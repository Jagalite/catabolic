# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Profile-local logical projections using the existing journaled reconciler."""

import hashlib
import json
import time
import unicodedata
from pathlib import PurePosixPath

from .domain import CatabolicError
from .fallback_policies import Policies
from .fallback_resolution import Resolver, require_epoch
from .layouts import Layouts, _layout_contexts, render_candidate
from .publication_lock import serialized
from .saved_queries import Queries
from .store import encode


def binding(store, profile, catalog):
    if store.schema_version < 23:
        return None
    rows = store.rows(
        "SELECT * FROM fallback_bindings WHERE profile=? AND catalog=?",
        (profile, catalog),
    )
    return rows[0] if rows else None


def effective_mappings(app, catalog):
    if binding(app.store, app.profile, catalog):
        return app.store.rows(
            "SELECT e.id,e.catalog,e.file_id,e.item_id,e.path,e.active,f.location,f.path AS source_path,o.status,o.size,o.mtime_ns,o.device,o.inode,e.revision,e.generation,e.policy_id,e.tier,e.role,e.part,e.variant FROM fallback_entries e JOIN files f ON f.id=e.file_id LEFT JOIN observations o ON o.file_id=f.id AND o.profile=e.profile WHERE e.profile=? AND e.catalog=? AND e.active=1 AND e.path IS NOT NULL ORDER BY e.path",
            (app.profile, catalog),
        )
    return app.store.rows(
        "SELECT m.*,f.location,f.path AS source_path,o.status,o.size,o.mtime_ns,o.device,o.inode FROM mappings m JOIN files f ON f.id=m.file_id LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? WHERE m.catalog=? AND m.active=1 ORDER BY m.path",
        (app.profile, catalog),
    )


class FallbackProjection:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    @serialized
    def bind(self, catalog, policy_id, query_id, layout):
        self.app.require_recovered()
        Policies(self.store, self.profile).get(policy_id)
        Layouts(self.app).get(layout)
        entity, _, report = Queries(self.store, self.profile).select(query_id)
        if entity != "item_id" or not report["complete"]:
            raise CatabolicError(
                "logical fallback membership requires a complete item_id selection; exact file selections are not converted"
            )
        if self.app.link_mode(catalog) != "symlink":
            raise CatabolicError("fallback publication supports symlinks only")
        if not binding(self.store, self.profile, catalog) and (
            self.store.rows(
                "SELECT 1 FROM mappings WHERE catalog=? AND active=1 LIMIT 1",
                (catalog,),
            )
            or self.store.rows(
                "SELECT 1 FROM owned_links WHERE profile=? AND catalog=? LIMIT 1",
                (self.profile, catalog),
            )
        ):
            raise CatabolicError(
                "existing exact mappings require an explicit separate logical projection; automatic adoption is forbidden"
            )
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO fallback_bindings(profile,catalog,policy_id,query_id,layout) VALUES (?,?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET policy_id=excluded.policy_id,query_id=excluded.query_id,layout=excluded.layout,next_attempt=0",
                (self.profile, catalog, policy_id, query_id, layout),
            )
        return binding(self.store, self.profile, catalog)

    def plan(self, catalog, *, manual_failback=False, session=None):
        config = binding(self.store, self.profile, catalog)
        if not config:
            raise CatabolicError("projection has no fallback binding")
        self.app.require_recovered()
        if self.app.link_mode(catalog) != "symlink":
            raise CatabolicError("fallback publication supports symlinks only")
        from .evaluation import EvaluationSession

        limits = Policies(self.store, self.profile).get(config["policy_id"])[
            "definition"
        ]["budgets"]
        session = session or EvaluationSession(
            self.store,
            self.profile,
            timeout_ms=limits["query_timeout_ms"],
            max_ids=limits["max_candidates"],
        )
        entity, ids, report = Queries(self.store, self.profile).select(
            config["query_id"], session=session
        )
        if entity != "item_id" or not report["complete"]:
            raise CatabolicError("incomplete_logical_membership")
        policy = Policies(self.store, self.profile).get(config["policy_id"])[
            "definition"
        ]
        if len(ids) > policy["budgets"]["max_entries"]:
            raise CatabolicError("logical_membership_budget")
        rows = self.store.rows(
            "SELECT item_id,role,part,metadata FROM item_files WHERE active=1 AND role IN (SELECT value FROM json_each(?)) AND item_id IN (SELECT value FROM json_each(?)) ORDER BY id LIMIT ?",
            (
                encode(policy["slot_roles"]),
                encode(sorted(ids)),
                policy["budgets"]["max_candidates"] + 1,
            ),
        )
        if len(rows) > policy["budgets"]["max_candidates"]:
            raise CatabolicError("logical_membership_budget")
        slots = {
            (
                r["item_id"],
                r["role"],
                r["part"],
                json.loads(r["metadata"]).get("variant", ""),
            )
            for r in rows
        }
        for item in ids - {r[0] for r in slots}:
            slots.add((item, "primary", None, ""))
        entries = [
            dict(zip(("item_id", "role", "part", "variant"), slot, strict=True))
            for slot in sorted(slots, key=encode)
        ]
        old = {
            r["id"]: r
            for r in self.store.rows(
                "SELECT * FROM fallback_entries WHERE profile=? AND catalog=?",
                (self.profile, catalog),
            )
        }
        if entries:
            resolver = Resolver(self.app, session=session)
            primary = [e for e in entries if e["role"] == "primary"]
            sidecars = [e for e in entries if e["role"] != "primary"]
            result = resolver.resolve(
                primary or entries,
                config["policy_id"],
                catalog=catalog,
                previous={
                    k: v
                    for k, v in old.items()
                    if v["policy_id"] == config["policy_id"]
                },
                manual_failback=manual_failback,
            )
            if primary and sidecars:
                resolver.primary_choices = {
                    d["item_id"]: d["file_id"]
                    for d in result["decisions"]
                    if d["state"].startswith("resolved")
                }
                extra = resolver.resolve(
                    sidecars,
                    config["policy_id"],
                    catalog=catalog,
                    previous=old,
                    manual_failback=manual_failback,
                )
                if result["checks"] + extra["checks"] > policy["budgets"]["max_checks"]:
                    raise CatabolicError("resolution_probe_budget")
                result["decisions"].extend(extra["decisions"])
                result["captures"].update(extra["captures"])
                result["health"].extend(extra["health"])
                result["checks"] += extra["checks"]
                result["sidecar_tiers"] = extra["tiers"]
        else:
            from .fallback_resolution import epoch

            result = {
                "policy_id": config["policy_id"],
                "epoch": epoch(self.store),
                "decisions": [],
                "captures": {},
                "health": [],
                "tiers": [],
                "checks": 0,
            }
        layout = Layouts(self.app).get(config["layout"])["definition"]
        associations = [
            {**r, "metadata": encode(r["metadata"])}
            for r in result["captures"].values()
        ]
        destinations = {}
        for association, selected, context in _layout_contexts(
            self.store, associations, layout["rules"]
        ):
            if selected is None:
                raise CatabolicError(
                    "fallback logical entry has no matching layout rule"
                )
            destinations[association["id"]] = render_candidate(
                self.store, self.profile, association, selected, context, layout
            )
        desired = []
        for decision in result["decisions"]:
            chosen = result["captures"].get(decision["entry_id"])
            desired.append(
                {
                    **decision,
                    "path": destinations[chosen["id"]]
                    if chosen and decision["state"].startswith("resolved")
                    else old.get(decision["entry_id"], {}).get("path"),
                }
            )
        layout_digest = hashlib.sha256(encode(layout).encode()).hexdigest()
        for decision in desired:
            previous = old.get(decision["entry_id"], {})
            if (
                decision["path"]
                and previous.get("path")
                and json.loads(previous.get("evidence", "{}")).get("layout_digest")
                == layout_digest
            ):
                decision["path"] = str(
                    PurePosixPath(previous["path"]).with_suffix(
                        PurePosixPath(decision["path"]).suffix
                    )
                )
            decision["evidence"]["layout_digest"] = layout_digest
        paths = {}
        for row in desired:
            if not row["path"]:
                continue
            path = unicodedata.normalize("NFC", row["path"]).casefold()
            if path in paths and paths[path] != row["entry_id"]:
                raise CatabolicError("fallback destination collision")
            paths[path] = row["entry_id"]
        if any(
            "/".join(path.split("/")[:i]) in paths
            for path in paths
            for i in range(1, len(path.split("/")))
        ):
            raise CatabolicError("fallback destination parent collision")
        result.update(
            catalog=catalog,
            generation=config["generation"],
            desired=desired,
            removed=sorted(
                {k for k, r in old.items() if r["active"]}
                - {d["entry_id"] for d in desired}
            ),
        )
        result["safe"] = all(d["state"].startswith("resolved") for d in desired)
        result["plan_id"] = hashlib.sha256(
            encode(
                {k: v for k, v in result.items() if k not in ("health", "plan_id")}
            ).encode()
        ).hexdigest()
        return result

    @serialized
    def run(
        self,
        catalog,
        *,
        apply=False,
        expected_plan=None,
        max_removals=0,
        max_changes=100,
        manual_failback=False,
        automatic=False,
    ):
        plan = self.plan(catalog, manual_failback=manual_failback)
        if apply and not automatic and expected_plan != plan["plan_id"]:
            raise CatabolicError("stale_or_missing_resolution_plan")
        if not apply:
            return self.present(plan)
        return self.apply_prepared(
            plan, max_removals=max_removals, max_changes=max_changes
        )

    @serialized
    def apply_prepared(self, plan, *, max_removals=0, max_changes=100):
        """Publish one evaluated plan, retaining epoch and journal admission fences."""
        from .reconcile import Reconciler

        catalog = plan["catalog"]
        require_epoch(self.store, plan["epoch"])
        config = binding(self.store, self.profile, catalog)
        if config["generation"] != plan["generation"]:
            raise CatabolicError("stale_resolution_generation")
        old = {
            r["id"]: r
            for r in self.store.rows(
                "SELECT * FROM fallback_entries WHERE profile=? AND catalog=?",
                (self.profile, catalog),
            )
        }
        changes = sum(
            any(
                old.get(d["entry_id"], {}).get(k) != d[v]
                for k, v in (
                    ("file_id", "file_id"),
                    ("path", "path"),
                    ("revision", "revision"),
                )
            )
            for d in plan["desired"]
        )
        removals = len(plan["removed"]) + sum(
            d["entry_id"] in old and old[d["entry_id"]]["path"] != d["path"]
            for d in plan["desired"]
        )
        if plan["safe"] and (changes > max_changes or removals > max_removals):
            raise CatabolicError("fallback_change_or_removal_budget")
        generation = config["generation"] + 1
        with self.store.transaction() as db:
            # Preview never reaches this block. Failed checks reset stability.
            db.execute(
                "DELETE FROM fallback_health WHERE profile=? AND catalog=?",
                (self.profile, catalog),
            )
            db.executemany(
                "INSERT INTO fallback_health VALUES (:profile,:catalog,:entry_id,:file_id,:revision,:healthy_since,:successes,:last_check)",
                plan["health"],
            )
            for decision in plan["desired"]:
                previous = old.get(decision["entry_id"], {})
                retain = not plan["safe"]
                selected = (
                    {
                        "file_id": previous.get("file_id"),
                        "revision": previous.get("revision"),
                        "tier": previous.get("tier"),
                        "tier_name": previous.get("tier_name"),
                        "path": previous.get("path"),
                    }
                    if retain
                    else {
                        "file_id": decision["file_id"],
                        "revision": decision["revision"],
                        "tier": decision["selected_tier"],
                        "tier_name": decision["selected_tier_name"],
                        "path": decision["path"],
                    }
                )
                stored_state = (
                    decision["state"]
                    if not retain or not decision["state"].startswith("resolved")
                    else "blocked"
                )
                stored_evidence = (
                    json.loads(previous.get("evidence", "{}"))
                    if retain
                    else decision["evidence"]
                )
                db.execute(
                    "INSERT INTO fallback_entries(id,profile,catalog,item_id,role,part,variant,policy_id,file_id,revision,tier,tier_name,path,state,generation,evidence) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET policy_id=excluded.policy_id,file_id=excluded.file_id,revision=excluded.revision,tier=excluded.tier,tier_name=excluded.tier_name,path=excluded.path,state=excluded.state,generation=excluded.generation,evidence=excluded.evidence,active=1",
                    (
                        decision["entry_id"],
                        self.profile,
                        catalog,
                        decision["item_id"],
                        decision["role"],
                        decision["part"],
                        decision["variant"],
                        plan["policy_id"],
                        *selected.values(),
                        stored_state,
                        generation,
                        encode(stored_evidence),
                    ),
                )
                if (
                    previous.get("file_id") != selected["file_id"]
                    or previous.get("state") != stored_state
                    or previous.get("path") != selected["path"]
                    or previous.get("revision") != selected["revision"]
                    or previous.get("policy_id") != plan["policy_id"]
                    or previous.get("tier") != selected["tier"]
                    or not previous.get("active")
                ):
                    db.execute(
                        "INSERT INTO fallback_history(profile,catalog,entry_id,generation,decision,created_at) VALUES (?,?,?,?,?,?)",
                        (
                            self.profile,
                            catalog,
                            decision["entry_id"],
                            generation,
                            encode(
                                {
                                    **decision,
                                    "phase": "planned" if plan["safe"] else "retained",
                                }
                            ),
                            time.time(),
                        ),
                    )
            if plan["safe"]:
                for removed in plan["removed"]:
                    db.execute(
                        "INSERT INTO fallback_history(profile,catalog,entry_id,generation,decision,created_at) VALUES (?,?,?,?,?,?)",
                        (
                            self.profile,
                            catalog,
                            removed,
                            generation,
                            encode(
                                {
                                    "entry_id": removed,
                                    "state": "removed",
                                    "phase": "planned",
                                    "policy_id": plan["policy_id"],
                                }
                            ),
                            time.time(),
                        ),
                    )
                db.executemany(
                    "UPDATE fallback_entries SET active=0,generation=? WHERE id=?",
                    [(generation, key) for key in plan["removed"]],
                )
            db.execute(
                "UPDATE fallback_bindings SET generation=? WHERE profile=? AND catalog=?",
                (generation, self.profile, catalog),
            )
        if not plan["safe"]:
            from .notifications import emit

            with self.store.transaction() as db:
                for decision in plan["desired"]:
                    if old.get(decision["entry_id"], {}).get("state") != (
                        decision["state"]
                        if not decision["state"].startswith("resolved")
                        else "blocked"
                    ):
                        emit(
                            db,
                            self.profile,
                            "fallback_unresolved",
                            "warning",
                            catalog,
                            [decision["entry_id"], generation],
                        )
            return {**self.present(plan), "applied": False, "membership_retained": True}
        reconciler = Reconciler(self.app)
        result = reconciler.apply(
            catalog, max_removals=max_removals, max_changes=max_changes
        )
        if result.get("healthy"):
            with self.store.transaction() as db:
                from .notifications import emit

                for decision in plan["desired"]:
                    if (
                        old.get(decision["entry_id"], {}).get("file_id")
                        != decision["file_id"]
                    ):
                        emit(
                            db,
                            self.profile,
                            "fallback_selected",
                            "info",
                            catalog,
                            [decision["entry_id"], generation],
                        )
                for decision in plan["desired"]:
                    previous = old.get(decision["entry_id"], {})
                    changed = any(
                        previous.get(k) != decision.get(v)
                        for k, v in (
                            ("file_id", "file_id"),
                            ("revision", "revision"),
                            ("path", "path"),
                            ("state", "state"),
                            ("policy_id", "policy_id"),
                            ("tier", "selected_tier"),
                        )
                    ) or not previous.get("active")
                    if (
                        changed
                        or not previous.get("published_generation")
                        or result.get("applied", 0)
                    ):
                        db.execute(
                            "UPDATE fallback_entries SET published_generation=generation WHERE id=?",
                            (decision["entry_id"],),
                        )
                    if changed:
                        db.execute(
                            "INSERT INTO fallback_history(profile,catalog,entry_id,generation,decision,created_at) VALUES (?,?,?,?,?,?)",
                            (
                                self.profile,
                                catalog,
                                decision["entry_id"],
                                generation,
                                encode({**decision, "phase": "verified"}),
                                time.time(),
                            ),
                        )
        return {
            **self.present(plan),
            "applied": True,
            "complete": result.get("healthy", False),
            "publication": result,
        }

    @staticmethod
    def present(plan):
        return {k: v for k, v in plan.items() if k not in ("captures", "health")}
