# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Live reconciliation and durable recovery of filesystem operations."""

from __future__ import annotations

import math
import os
import stat
from pathlib import Path
from uuid import uuid4

from .app import Application
from .domain import Action, CatabolicError, source_health
from .filesystem import (
    claim_output,
    link_state,
    owner_state,
    parent_handle,
    root_handle,
    source_stat,
)
from .publication_lock import serialized

ACTION_ORDER = {
    "hard_create": 3,
    "hard_replace": 4,
    "hard_remove": 5,
    "hard_forget": 6,
    "prepare": 0,
    "blocked_source": 1,
    "unchanged": 2,
    "create": 3,
    "replace": 4,
    "remove": 5,
    "forget": 6,
}


class StaleFallbackIntent(CatabolicError):
    """Complete catalog evaluation proves an admitted fallback intent obsolete."""


class Reconciler:
    def __init__(self, app: Application, *, notify_consumers=True):
        self.app = app
        self.store = app.store
        self.profile = app.profile
        self.notify_consumers = notify_consumers

    def owner(self, catalog: str) -> dict:
        return {
            "format": 1,
            "database_id": self.store.database_id,
            "profile": self.profile,
            "catalog": catalog,
        }

    def catalogs(self, catalog: str | None) -> list[str]:
        if catalog is not None:
            if not self.store.rows("SELECT id FROM catalogs WHERE id=?", (catalog,)):
                raise CatabolicError(f"unknown catalog: {catalog}")
            return [catalog]
        return [
            row["id"] for row in self.store.rows("SELECT id FROM catalogs ORDER BY id")
        ]

    def preview(
        self,
        catalog: str | None = "global",
        *,
        max_removals=None,
        max_removal_percent=None,
    ) -> dict:
        if max_removals is not None and (
            type(max_removals) is not int or max_removals < 0
        ):
            raise CatabolicError("max removals must be a nonnegative integer")
        if max_removal_percent is not None and (
            type(max_removal_percent) not in (int, float)
            or not math.isfinite(max_removal_percent)
            or not 0 <= max_removal_percent <= 100
        ):
            raise CatabolicError(
                "max removal percent must be finite and between 0 and 100"
            )
        actions: list[Action] = []
        blockers: list[dict] = []
        for selected in self.catalogs(catalog):
            try:
                if self.store.rows(
                    "SELECT id FROM journal WHERE profile=? AND catalog=?",
                    (self.profile, selected),
                ):
                    raise CatabolicError(
                        "pending operations require recover before a fresh preview"
                    )
                actions.extend(self._catalog_delta(selected))
            except (OSError, CatabolicError) as exc:
                blockers.append({"catalog": selected, "reason": str(exc)})
        actions.sort(
            key=lambda action: (ACTION_ORDER[action.kind], action.catalog, action.path)
        )
        removals = sum(a.kind in ("remove", "hard_remove") for a in actions)
        # Count existing owned output entries, including entries now absent. New
        # creations must not dilute a removal percentage. Retained data is separate.
        owned = sum(
            self.store.db.execute(
                "SELECT count(*) FROM "
                + (
                    "owned_hardlinks"
                    if self.app.link_mode(selected) == "hardlink"
                    else "owned_links"
                )
                + " WHERE profile=? AND catalog=?",
                (self.profile, selected),
            ).fetchone()[0]
            for selected in self.catalogs(catalog)
        )
        percent = 100 * removals / owned if owned else 0
        if (max_removals is not None and removals > max_removals) or (
            max_removal_percent is not None and percent > max_removal_percent
        ):
            blockers.append(
                {
                    "catalog": catalog,
                    "reason": "bulk-removal limit exceeded; review the full plan before increasing the explicit limit",
                }
            )
        from .hardlinks import WARNING

        hard_catalogs = [
            selected
            for selected in self.catalogs(catalog)
            if self.app.link_mode(selected) == "hardlink"
        ]
        return {
            **(
                {"warnings": [WARNING], "hardlink_catalogs": hard_catalogs}
                if hard_catalogs
                else {}
            ),
            "safe": not blockers,
            "removal_budget": {
                "removals": removals,
                "owned_entries": owned,
                "percent": percent,
                "max_removals": max_removals,
                "max_removal_percent": max_removal_percent,
            },
            "actions": [action.to_dict() for action in actions],
            "blockers": blockers,
        }

    def _catalog_delta(self, catalog: str) -> list[Action]:
        if self.app.link_mode(catalog) == "hardlink":
            from .hardlinks import Hardlinks

            return Hardlinks(self).delta(catalog)
        output = self.app.binding("output", catalog)
        actions = []
        from .fallback_projection import binding as fallback_binding
        from .fallback_projection import effective_mappings

        if fallback_binding(self.store, self.profile, catalog) and self.store.rows(
            "SELECT 1 FROM fallback_entries WHERE profile=? AND catalog=? AND active=1 AND state IN ('blocked','unresolved') LIMIT 1",
            (self.profile, catalog),
        ):
            raise CatabolicError(
                "fallback membership unresolved; owned output retained"
            )
        mappings = effective_mappings(self.app, catalog)
        owned = {
            row["path"]: row["target"]
            for row in self.store.rows(
                "SELECT path,target FROM owned_links WHERE profile=? AND catalog=?",
                (self.profile, catalog),
            )
        }
        with root_handle(output) as out_fd:
            ownership = owner_state(out_fd, self.owner(catalog))
            if ownership == "unowned":
                raise CatabolicError(
                    "output is not empty and has no matching ownership marker"
                )
            if ownership == "empty":
                if owned:
                    raise CatabolicError(
                        "ownership marker disappeared from a previously managed output"
                    )
                actions.append(Action("prepare", catalog, ""))
            desired = {}
            for mapping in mappings:
                target, reason = self._source_target(mapping, output)
                if reason:
                    actions.append(
                        Action(
                            "blocked_source", catalog, mapping["path"], reason=reason
                        )
                    )
                else:
                    desired[mapping["path"]] = target
            for path, target in desired.items():
                state, actual = link_state(out_fd, path)
                previous = owned.get(path)
                if state == "absent":
                    actions.append(Action("create", catalog, path, target, previous))
                elif state != "link" or previous is None or actual != previous:
                    raise CatabolicError(
                        f"destination collision or externally changed owned link: {path}"
                    )
                elif actual == target:
                    actions.append(Action("unchanged", catalog, path, target, previous))
                else:
                    actions.append(Action("replace", catalog, path, target, previous))
            for path, target in owned.items():
                if path in desired:
                    continue
                state, actual = link_state(out_fd, path)
                if state == "absent":
                    actions.append(Action("forget", catalog, path, previous=target))
                elif state == "link" and actual == target:
                    actions.append(Action("remove", catalog, path, previous=target))
                else:
                    raise CatabolicError(f"owned path changed externally: {path}")
        return sorted(
            actions, key=lambda action: (ACTION_ORDER[action.kind], action.path)
        )

    @serialized
    def apply(
        self,
        catalog: str | None = "global",
        *,
        after_filesystem=None,
        max_changes=None,
        max_removals=None,
        max_removal_percent=None,
    ) -> dict:
        preview = self.preview(
            catalog, max_removals=max_removals, max_removal_percent=max_removal_percent
        )
        if not preview["safe"]:
            return {**preview, "applied": [], "healthy": False}
        if max_changes is not None:
            if type(max_changes) is not int or max_changes < 0:
                raise CatabolicError("max changes must be a nonnegative integer")
            mutations = sum(
                action["kind"] not in ("unchanged", "blocked_source", "prepare")
                for action in preview["actions"]
            )
            if mutations > max_changes:
                raise CatabolicError("fallback_change_or_removal_budget")
        from .projection_stats import begin_execution, finish_execution

        statistics = (
            begin_execution(self.app, self.catalogs(catalog), preview["actions"])
            if self.store.schema_version >= 16
            else None
        )
        applied = []
        for raw in preview["actions"]:
            action = Action(**raw)
            if action.kind in ("unchanged", "blocked_source"):
                continue
            # The initial preflight covers the full selected scope. Execution
            # revalidates this operation's source, ownership, and destination.
            operation = {
                "id": str(uuid4()),
                "profile": self.profile,
                **action.to_dict(),
            }
            with self.store.transaction() as db:
                db.execute(
                    "INSERT INTO journal(id,profile,catalog,path,kind,target,previous) VALUES (?,?,?,?,?,?,?)",
                    tuple(
                        operation[key]
                        for key in (
                            "id",
                            "profile",
                            "catalog",
                            "path",
                            "kind",
                            "target",
                            "previous",
                        )
                    ),
                )
            self._execute(operation, after_filesystem=after_filesystem)
            applied.append(action.to_dict())
        verified = self.verify(catalog)
        if statistics is not None:
            finish_execution(self.app, *statistics, applied, verified)
        events = []
        if self.store.schema_version >= 6 and self.notify_consumers:
            from .network_adapters import Refresh

            events = Refresh(self.app).after_sync(
                {"healthy": verified["healthy"]}, catalog
            )
        result = {
            "safe": True,
            **({"refresh_events": events} if events else {}),
            **({"warnings": preview["warnings"]} if "warnings" in preview else {}),
            "applied": applied,
            "verification": verified,
            "healthy": verified["healthy"],
        }
        from .consumers import published

        published(self.app, verified, result)
        if applied:
            self._record_fallback_publication(verified)
        return result

    @serialized
    def recover(
        self,
        catalog: str | None = "global",
        *,
        after_filesystem=None,
        cancel_unapplied=False,
    ) -> dict:
        recovered = []
        cancelled = []
        for selected in self.catalogs(catalog):
            for operation in self.store.rows(
                "SELECT * FROM journal WHERE profile=? AND catalog=? ORDER BY rowid",
                (self.profile, selected),
            ):
                if cancel_unapplied and operation["kind"].startswith("hard_"):
                    from .hardlinks import Hardlinks

                    if Hardlinks(self).cancel_obsolete(operation, force=True):
                        cancelled.append(operation["id"])
                        continue
                if self._cancel_obsolete(operation):
                    cancelled.append(operation["id"])
                    continue
                self._execute(operation, after_filesystem=after_filesystem)
                recovered.append(operation["id"])
        result = {
            "recovered": recovered,
            "cancelled": cancelled,
            "next": "run sync --dry-run to inspect current desired state",
        }
        if self.store.schema_version >= 17:
            from .consumers import published

            verified = self.verify(catalog)
            published(self.app, verified, result)
            if recovered:
                self._record_fallback_publication(verified)
        return result

    def _record_fallback_publication(self, verified):
        if self.store.schema_version < 23:
            return
        with self.store.transaction() as db:
            for report in verified["catalogs"]:
                if (
                    report["healthy"]
                    and not db.execute(
                        "SELECT 1 FROM journal WHERE profile=? AND catalog=?",
                        (self.profile, report["catalog"]),
                    ).fetchone()
                ):
                    db.execute(
                        "UPDATE fallback_entries SET published_generation=generation WHERE profile=? AND catalog=? AND state NOT IN ('blocked','unresolved')",
                        (self.profile, report["catalog"]),
                    )

    def _cancel_obsolete(self, operation: dict) -> bool:
        """Cancel unapplied intent only after a successful live revalidation."""
        kind = operation["kind"]
        if kind.startswith("hard_"):
            from .hardlinks import Hardlinks

            return Hardlinks(self).cancel_obsolete(operation)
        if kind in ("prepare", "forget"):
            return False
        path = operation["path"]
        catalog = operation["catalog"]
        with root_handle(self.app.binding("output", catalog)) as fd:
            initial = (
                ("absent", None)
                if kind == "create"
                else ("link", operation["previous"])
            )
            if link_state(fd, path) != initial:
                return False
            try:
                current = self._catalog_delta(catalog)
            except StaleFallbackIntent:
                # A complete selection rejected this batch. Cancel only intent
                # whose filesystem action has not happened and remains ours.
                if owner_state(fd, self.owner(catalog)) != "owned":
                    raise CatabolicError(
                        "output ownership missing during cancellation"
                    ) from None
                owned = self.store.rows(
                    "SELECT target FROM owned_links WHERE profile=? AND catalog=? AND path=?",
                    (self.profile, catalog, path),
                )
                if (owned[0]["target"] if owned else None) != operation["previous"]:
                    raise CatabolicError(
                        "recorded ownership changed during cancellation"
                    ) from None
                current = []
            if any(
                action.kind == kind
                and action.path == path
                and action.target == operation["target"]
                and action.previous == operation["previous"]
                for action in current
            ):
                return False
            if kind == "replace":
                with parent_handle(fd, path) as (parent, _):
                    temporary = f".catabolic-link-{operation['id']}"
                    try:
                        current_temp = os.stat(
                            temporary, dir_fd=parent, follow_symlinks=False
                        )
                    except FileNotFoundError:
                        pass
                    else:
                        if (
                            not stat.S_ISLNK(current_temp.st_mode)
                            or os.readlink(temporary, dir_fd=parent)
                            != operation["target"]
                        ):
                            raise CatabolicError(
                                "replacement recovery link changed externally"
                            )
                        os.unlink(temporary, dir_fd=parent)
                        os.fsync(parent)
        with self.store.transaction() as db:
            db.execute("DELETE FROM journal WHERE id=?", (operation["id"],))
        return True

    def _execute(self, operation: dict, *, after_filesystem=None):
        if operation["kind"].startswith("hard_"):
            from .hardlinks import Hardlinks

            return Hardlinks(self).execute(operation, after_filesystem=after_filesystem)
        catalog = operation["catalog"]
        output = self.app.binding("output", catalog)
        kind = operation["kind"]
        path = operation["path"]
        target = operation["target"]
        previous = operation["previous"]
        with root_handle(output) as fd:
            if kind == "prepare":
                claim_output(fd, self.owner(catalog), operation["id"])
            else:
                if owner_state(fd, self.owner(catalog)) != "owned":
                    raise CatabolicError(
                        "output ownership missing during operation recovery"
                    )
                state, actual = link_state(fd, path)
                if kind in ("create", "replace"):
                    if (state, actual) != ("link", target):
                        self._validate_target(catalog, path, target)
                        if kind == "create" and state != "absent":
                            raise CatabolicError(
                                f"creation destination changed externally: {path}"
                            )
                        if kind == "replace" and (state, actual) != ("link", previous):
                            raise CatabolicError(
                                f"replacement destination changed externally: {path}"
                            )
                        with parent_handle(fd, path, create=True) as (parent, leaf):
                            if kind == "create":
                                os.symlink(target, leaf, dir_fd=parent)
                            else:
                                temporary = f".catabolic-link-{operation['id']}"
                                try:
                                    os.symlink(target, temporary, dir_fd=parent)
                                    os.fsync(parent)
                                except FileExistsError:
                                    temp_stat = os.stat(
                                        temporary, dir_fd=parent, follow_symlinks=False
                                    )
                                    if (
                                        not stat.S_ISLNK(temp_stat.st_mode)
                                        or os.readlink(temporary, dir_fd=parent)
                                        != target
                                    ):
                                        raise CatabolicError(
                                            "replacement recovery link changed externally"
                                        ) from None
                                # Outputs are exclusively managed by Catabolic.
                                # Recheck immediately before atomic replacement.
                                leaf_stat = os.stat(
                                    leaf, dir_fd=parent, follow_symlinks=False
                                )
                                if (
                                    not stat.S_ISLNK(leaf_stat.st_mode)
                                    or os.readlink(leaf, dir_fd=parent) != previous
                                ):
                                    raise CatabolicError(
                                        f"owned link changed before replacement: {path}"
                                    )
                                os.replace(
                                    temporary,
                                    leaf,
                                    src_dir_fd=parent,
                                    dst_dir_fd=parent,
                                )
                            os.fsync(parent)
                elif kind in ("remove", "forget"):
                    if state != "absent":
                        if kind == "forget" or (state, actual) != ("link", previous):
                            raise CatabolicError(
                                f"removal destination changed externally: {path}"
                            )
                        self._validate_removal(catalog, path)
                        with parent_handle(fd, path) as (parent, leaf):
                            current = os.stat(
                                leaf, dir_fd=parent, follow_symlinks=False
                            )
                            if (
                                not stat.S_ISLNK(current.st_mode)
                                or os.readlink(leaf, dir_fd=parent) != previous
                            ):
                                raise CatabolicError(
                                    f"owned link changed before removal: {path}"
                                )
                            os.unlink(leaf, dir_fd=parent)
                            os.fsync(parent)
                else:
                    raise CatabolicError(f"unknown journal operation: {kind}")
            if after_filesystem:
                after_filesystem(operation)
        with self.store.transaction() as db:
            if kind in ("create", "replace"):
                db.execute(
                    "INSERT INTO owned_links VALUES (?,?,?,?) ON CONFLICT(profile,catalog,path) DO UPDATE SET target=excluded.target",
                    (self.profile, catalog, path, target),
                )
            elif kind in ("remove", "forget"):
                db.execute(
                    "DELETE FROM owned_links WHERE profile=? AND catalog=? AND path=?",
                    (self.profile, catalog, path),
                )
            if (
                self.store.schema_version >= 6
                and self.notify_consumers
                and operation["kind"]
                in (
                    "create",
                    "replace",
                    "remove",
                )
            ):
                from .network_adapters import record_output_change

                record_output_change(db, self.profile, operation["catalog"])
            db.execute("DELETE FROM journal WHERE id=?", (operation["id"],))
            if self.store.schema_version >= 17 and kind in (
                "create",
                "replace",
                "remove",
            ):
                from .consumers import record_change

                record_change(db, self.profile, catalog, path)

    def _source_target(
        self, mapping: dict, output: dict
    ) -> tuple[str | None, str | None]:
        if "revision" in mapping:
            from .content_access import revision_of
            from .curation import occurrence
            from .fallback_probe import probe
            from .fallback_resolution import epoch, require_epoch

            current_epoch = epoch(self.store)
            current = self.store.rows(
                "SELECT file_id,revision,generation,active,path FROM fallback_entries WHERE id=? AND profile=?",
                (mapping["id"], self.profile),
            )
            if not current or any(
                current[0][key] != mapping[key]
                for key in ("file_id", "revision", "generation", "active", "path")
            ):
                raise CatabolicError("stale_resolution_generation")
            if (
                revision_of(self.store, self.profile, mapping["file_id"])
                != mapping["revision"]
            ):
                return None, "fallback_revision_changed"
            import json

            from .fallback_policies import Policies
            from .fallback_projection import binding
            from .fallback_resolution import Resolver
            from .saved_queries import Queries

            config = binding(self.store, self.profile, mapping["catalog"])
            if not config or config["policy_id"] != mapping["policy_id"]:
                raise StaleFallbackIntent("stale_resolution_policy")
            policy = Policies(self.store, self.profile).get(mapping["policy_id"])[
                "definition"
            ]
            cache = getattr(self, "_fallback_queries", {})

            def selection(identifier):
                key = (current_epoch, identifier)
                if key not in cache:
                    cache[key] = Queries(self.store, self.profile).select(identifier)
                entity, ids, report = cache[key]
                if not report["complete"]:
                    raise CatabolicError("incomplete_publication_selection")
                return entity, ids

            self._fallback_queries = cache
            entity, members = selection(config["query_id"])
            if entity != "item_id" or mapping["item_id"] not in members:
                raise StaleFallbackIntent("stale_resolution_membership")
            entity, ids = selection(policy["fallbacks"][mapping["tier"]]["query_id"])
            if policy.get("package"):
                from .component_packages import publication_capture

                accepted = publication_capture(self, mapping, policy, entity, ids)
            else:
                associations = self.store.rows(
                    "SELECT a.*,f.location,f.path FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.item_id=? AND a.file_id=? AND a.role=? AND a.part IS ?",
                    (
                        mapping["item_id"],
                        mapping["file_id"],
                        mapping["role"],
                        mapping["part"],
                    ),
                )
                candidate = next(
                    (
                        a
                        for a in associations
                        if a[
                            {
                                "file_id": "file_id",
                                "item_id": "item_id",
                                "association_id": "id",
                            }[entity]
                        ]
                        in ids
                        and json.loads(a["metadata"]).get("variant", "")
                        == mapping["variant"]
                    ),
                    None,
                )
                if not candidate:
                    raise StaleFallbackIntent("stale_resolution_candidate")
                resolver = Resolver(self.app)
                resolver.primary_choices = {
                    r["item_id"]: r["file_id"]
                    for r in self.store.rows(
                        "SELECT item_id,file_id FROM fallback_entries WHERE profile=? AND catalog=? AND active=1 AND role='primary'",
                        (self.profile, mapping["catalog"]),
                    )
                }
                accepted = resolver.capture(candidate, policy, mapping["catalog"])
            if not accepted:
                raise StaleFallbackIntent("stale_resolution_evidence")
            snapshot = occurrence(self.store, self.profile, mapping["file_id"])
            if not self.store.writable and self.store.db.in_transaction:
                self.store.db.rollback()
            with self.store.detached():
                result = probe(
                    self.store.path,
                    accepted["checks"],
                    policy["budgets"]["probe_timeout_ms"],
                )
            require_epoch(self.store, current_epoch)
            if not result["usable"]:
                return None, result["reason"]
            return os.path.relpath(
                Path(snapshot["root"]) / snapshot["path"],
                (Path(output["root"]) / mapping["path"]).parent,
            ), None
        binding = self.app.binding("source", mapping["location"])
        with root_handle(binding) as source_fd:
            if mapping["status"] is None:
                raise CatabolicError(
                    f"source needs a complete scan: {mapping['source_path']}"
                )
            try:
                current = source_stat(source_fd, mapping["source_path"])
            except FileNotFoundError:
                if mapping["status"] != "missing":
                    raise CatabolicError(
                        f"unconfirmed missing source: {mapping['source_path']}"
                    ) from None
                return None, "confirmed_missing"
            if mapping["status"] == "missing":
                raise CatabolicError(
                    f"source returned; scan before synchronization: {mapping['source_path']}"
                )
            if (
                current.st_size,
                current.st_mtime_ns,
                current.st_dev,
                current.st_ino,
            ) != (
                mapping["size"],
                mapping["mtime_ns"],
                mapping["device"],
                mapping["inode"],
            ):
                raise CatabolicError(
                    f"source changed; scan before synchronization: {mapping['source_path']}"
                )
            reason = source_health(current.st_size, mapping["source_path"])
            if reason:
                return None, reason
        target = os.path.relpath(
            Path(binding["root"]) / mapping["source_path"],
            (Path(output["root"]) / mapping["path"]).parent,
        )
        return target, None

    def _active_mapping(self, catalog: str, path: str) -> dict | None:
        from .fallback_projection import effective_mappings

        return next(
            (r for r in effective_mappings(self.app, catalog) if r["path"] == path),
            None,
        )

    def _validate_target(self, catalog: str, path: str, target: str):
        mapping = self._active_mapping(catalog, path)
        if mapping is not None:
            current, _ = self._source_target(
                mapping, self.app.binding("output", catalog)
            )
            if current == target:
                return
        raise CatabolicError(
            "pending link target is no longer desired; inspect catalog state"
        )

    def _validate_removal(self, catalog: str, path: str):
        mapping = self._active_mapping(catalog, path)
        if mapping is None:
            return
        current, reason = self._source_target(
            mapping, self.app.binding("output", catalog)
        )
        if current is not None or reason is None:
            raise CatabolicError(
                "pending removal is no longer desired; inspect catalog state"
            )

    def verify(self, catalog: str | None = "global") -> dict:
        reports = []
        for selected in self.catalogs(catalog):
            if self.app.link_mode(selected) == "hardlink":
                from .hardlinks import Hardlinks

                reports.append(Hardlinks(self).verify(selected))
                continue
            issues = []
            verified = 0
            source_evidence = {}
            try:
                output = self.app.binding("output", selected)
                with root_handle(output) as fd:
                    if owner_state(fd, self.owner(selected)) != "owned":
                        raise CatabolicError("output ownership is missing")
                    if self.store.rows(
                        "SELECT id FROM journal WHERE profile=? AND catalog=?",
                        (self.profile, selected),
                    ):
                        issues.append({"reason": "pending operations require recovery"})
                    owned = {
                        row["path"]: row["target"]
                        for row in self.store.rows(
                            "SELECT path,target FROM owned_links WHERE profile=? AND catalog=?",
                            (self.profile, selected),
                        )
                    }
                    active_paths = set()
                    from .fallback_projection import effective_mappings

                    for mapping in effective_mappings(self.app, selected):
                        path = mapping["path"]
                        active_paths.add(path)
                        try:
                            source = self.app.binding("source", mapping["location"])
                            source_evidence[mapping["location"]] = source.evidence
                            with root_handle(source) as source_fd:
                                current = source_stat(source_fd, mapping["source_path"])
                            if mapping["status"] != "present":
                                raise CatabolicError("source needs a complete scan")
                            if (
                                current.st_size,
                                current.st_mtime_ns,
                                current.st_dev,
                                current.st_ino,
                            ) != (
                                mapping["size"],
                                mapping["mtime_ns"],
                                mapping["device"],
                                mapping["inode"],
                            ):
                                raise CatabolicError("source changed since scan")
                            reason = source_health(
                                current.st_size, mapping["source_path"]
                            )
                            if reason:
                                raise CatabolicError(reason)
                            expected = os.path.relpath(
                                Path(source["root"]) / mapping["source_path"],
                                (Path(output["root"]) / path).parent,
                            )
                            if owned.get(path) != expected or link_state(fd, path) != (
                                "link",
                                expected,
                            ):
                                raise CatabolicError(
                                    "link or ownership does not match the selected source"
                                )
                            verified += 1
                        except (OSError, CatabolicError) as exc:
                            issues.append({"path": path, "reason": str(exc)})
                    issues.extend(
                        {
                            "path": path,
                            "reason": "obsolete owned link needs synchronization",
                        }
                        for path in owned
                        if path not in active_paths
                    )
            except (OSError, CatabolicError) as exc:
                issues.append({"reason": str(exc)})
            reports.append(
                {
                    "catalog": selected,
                    "healthy": not issues,
                    "source_validation": source_evidence,
                    "identity_verified": not issues
                    and all(
                        v.get("identity_verified", False)
                        for v in source_evidence.values()
                    ),
                    "verified_links": verified,
                    "issues": issues,
                }
            )
        return {
            "healthy": all(report["healthy"] for report in reports),
            "catalogs": reports,
        }
