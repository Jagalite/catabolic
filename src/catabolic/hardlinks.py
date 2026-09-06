"""Hardlink reconciliation: regular-file ownership and non-destructive retirement.

Regular media files are never unlinked or overwritten here. Retirement uses an
atomic rename into an owned retention directory. A link-count check alone cannot
protect the last reference against concurrent removal of the source filename.
"""

import errno
import json
import os
import stat
from contextlib import contextmanager
from uuid import UUID

from .domain import Action, CatabolicError, relative_path
from .filesystem import (
    DIRECTORY_FLAGS,
    claim_output,
    owner_state,
    parent_handle,
    rename_noreplace,
    root_handle,
)
from .store import encode

RETAINED = ".catabolic-retained"
WARNING = "Hardlinks share source data and permissions. Retired links are retained, never automatically purged."


def identity(st):
    return (st.st_dev, st.st_ino)


def target_identity(raw):
    value = json.loads(raw)
    return (value["device"], value["inode"])


def state(root, path):
    try:
        with parent_handle(root, path) as (parent, leaf):
            st = os.stat(leaf, dir_fd=parent, follow_symlinks=False)
            return st
    except FileNotFoundError:
        return None


def matches(st, raw):
    return (
        st is not None
        and stat.S_ISREG(st.st_mode)
        and identity(st) == target_identity(raw)
    )


def checkpoint(stage, operation):
    """Fault-injection seam for crash and concurrent-change tests."""


def check_count(st, path):
    if st.st_nlink <= 1:
        raise CatabolicError(
            f"last hardlink protected: {path} is the final filesystem reference; keep this file or restore another hardlink to the same inode before retrying"
        )


class Hardlinks:
    def __init__(self, reconciler):
        self.r = reconciler
        self.app, self.store, self.profile = (
            reconciler.app,
            reconciler.store,
            reconciler.profile,
        )

    def _target(self, mapping, output):
        _, reason = self.r._source_target(mapping, output)
        if reason:
            return None, reason
        if mapping["device"] != output["device"]:
            raise CatabolicError(
                f"cross-filesystem hardlink refused: source {mapping['location']}:{mapping['source_path']} and output {output['root']} are on different filesystems; choose a same-filesystem output or a symlink catalog (no automatic fallback)"
            )
        return encode(
            {
                key: mapping[key]
                for key in (
                    "file_id",
                    "location",
                    "source_path",
                    "device",
                    "inode",
                    "size",
                    "mtime_ns",
                )
            }
        ), None

    def _check_parents(self, root, path):
        """Validate all existing parents without creating folders during preview."""
        relative_path(path)
        expected = os.fstat(root).st_dev
        fd = os.dup(root)
        try:
            for part in path.split("/")[:-1]:
                try:
                    child = os.open(part, DIRECTORY_FLAGS, dir_fd=fd)
                except FileNotFoundError:
                    break
                os.close(fd)
                fd = child
                if os.fstat(fd).st_dev != expected:
                    raise CatabolicError(
                        f"cross-filesystem output parent refused: {path}"
                    )
        finally:
            os.close(fd)

    def delta(self, catalog):
        output = self.app.binding("output", catalog)
        owned = {
            row["path"]: row["target"]
            for row in self.store.rows(
                "SELECT path,target FROM owned_hardlinks WHERE profile=? AND catalog=?",
                (self.profile, catalog),
            )
        }
        if self.store.rows(
            "SELECT 1 FROM owned_links WHERE catalog=? LIMIT 1", (catalog,)
        ):
            raise CatabolicError(
                "hardlink catalog has incompatible symlink ownership records"
            )
        actions, desired = [], {}
        with root_handle(output) as fd:
            ownership = owner_state(fd, self.r.owner(catalog))
            if ownership == "unowned" or ownership == "empty" and owned:
                raise CatabolicError("hardlink output ownership is missing or changed")
            if ownership == "empty":
                actions.append(Action("prepare", catalog, ""))
            for mapping in self.store.rows(
                "SELECT m.*,f.location,f.path AS source_path,o.status,o.size,o.mtime_ns,o.device,o.inode FROM mappings m JOIN files f ON f.id=m.file_id LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? WHERE m.catalog=? AND m.active=1 ORDER BY m.path",
                (self.profile, catalog),
            ):
                self._check_parents(fd, mapping["path"])
                target, reason = self._target(mapping, output)
                if reason:
                    actions.append(
                        Action(
                            "blocked_source", catalog, mapping["path"], reason=reason
                        )
                    )
                else:
                    desired[mapping["path"]] = target
            for path, target in desired.items():
                actual, previous = state(fd, path), owned.get(path)
                if actual is None:
                    actions.append(
                        Action("hard_create", catalog, path, target, previous)
                    )
                elif previous is None or not matches(actual, previous):
                    raise CatabolicError(
                        f"unowned or externally changed regular-file destination: {path}"
                    )
                elif matches(actual, target):
                    actions.append(Action("unchanged", catalog, path, target, previous))
                else:
                    check_count(actual, path)
                    actions.append(
                        Action(
                            "hard_replace",
                            catalog,
                            path,
                            target,
                            previous,
                            "previous hardlink will be retained",
                        )
                    )
            for path, previous in owned.items():
                if path in desired:
                    continue
                self._check_parents(fd, path)
                actual = state(fd, path)
                if actual is None:
                    actions.append(
                        Action("hard_forget", catalog, path, previous=previous)
                    )
                elif matches(actual, previous):
                    check_count(actual, path)
                    actions.append(
                        Action(
                            "hard_remove",
                            catalog,
                            path,
                            previous=previous,
                            reason="hardlink will move into protected retention",
                        )
                    )
                else:
                    raise CatabolicError(f"owned hardlink changed externally: {path}")
            if any(
                action.kind in ("hard_replace", "hard_remove") for action in actions
            ):
                self._check_retention_root(fd, catalog)
        return actions

    def _check_retention_root(self, root, catalog):
        try:
            fd = os.open(RETAINED, DIRECTORY_FLAGS, dir_fd=root)
        except FileNotFoundError:
            return
        try:
            if os.fstat(fd).st_dev != os.fstat(root).st_dev:
                raise CatabolicError(
                    "retention directory crossed a filesystem boundary"
                )
            expected = {**self.r.owner(catalog), "purpose": "retained-hardlinks"}
            if owner_state(fd, expected) not in ("owned", "empty"):
                raise CatabolicError("retention directory contains unowned files")
        finally:
            os.close(fd)

    @contextmanager
    def _retention(self, root, operation, *, create=False):
        # Operation IDs become internal directory names, never arbitrary paths.
        identifier = str(UUID(operation["id"]))
        expected = {
            **self.r.owner(operation["catalog"]),
            "purpose": "retained-hardlinks",
        }
        fd = child = None
        try:
            if create:
                try:
                    os.mkdir(RETAINED, mode=0o700, dir_fd=root)
                    os.fsync(root)
                except FileExistsError:
                    pass
            fd = os.open(RETAINED, DIRECTORY_FLAGS, dir_fd=root)
            if os.fstat(fd).st_dev != os.fstat(root).st_dev:
                raise CatabolicError(
                    "retention directory must be on the output filesystem"
                )
            if create:
                claim_output(fd, expected, identifier)
            elif owner_state(fd, expected) != "owned":
                if set(os.listdir(fd)) <= {f".catabolic-claim-{identifier}"}:
                    raise FileNotFoundError("retention initialization is pending")
                raise CatabolicError("retention ownership is missing")
            if create:
                try:
                    os.mkdir(identifier, mode=0o700, dir_fd=fd)
                    os.fsync(fd)
                except FileExistsError:
                    pass
            child = os.open(identifier, DIRECTORY_FLAGS, dir_fd=fd)
            if os.fstat(child).st_dev != os.fstat(root).st_dev:
                raise CatabolicError(
                    "retention operation directory crossed a filesystem boundary"
                )
            yield child
        finally:
            if child is not None:
                os.close(child)
            if fd is not None:
                os.close(fd)

    def _retained(self, root, operation):
        try:
            with self._retention(root, operation) as fd:
                st = os.stat("data", dir_fd=fd, follow_symlinks=False)
        except FileNotFoundError:
            return False
        if not matches(st, operation["previous"]):
            raise CatabolicError(
                "retained hardlink changed externally; no data removed"
            )
        return True

    def _retain(self, root, operation):
        path, previous = operation["path"], operation["previous"]
        with self._retention(root, operation, create=True) as saved:
            try:
                st = os.stat("data", dir_fd=saved, follow_symlinks=False)
            except FileNotFoundError:
                st = None
            if st is not None:
                if not matches(st, previous):
                    raise CatabolicError(
                        "retention destination collision; no data overwritten"
                    )
                return
            with parent_handle(root, path) as (parent, leaf):
                st = os.stat(leaf, dir_fd=parent, follow_symlinks=False)
                if not matches(st, previous):
                    raise CatabolicError("hardlink changed before retirement")
                check_count(st, path)
                checkpoint("before_retention_rename", operation)
                # No unlink or replacement: even if another link disappears after
                # check_count, this rename preserves the data's remaining name.
                rename_noreplace(parent, leaf, saved, "data")
                os.fsync(saved)
                os.fsync(parent)
                if not matches(
                    os.stat("data", dir_fd=saved, follow_symlinks=False), previous
                ):
                    raise CatabolicError(
                        "destination changed concurrently; unexpected file preserved in retention"
                    )
        checkpoint("after_retention", operation)

    def _record_retention(self, db, operation):
        db.execute(
            "INSERT INTO retained_hardlinks(id,profile,catalog,path,original_path,target) VALUES (?,?,?,?,?,?) ON CONFLICT(id) DO NOTHING",
            (
                operation["id"],
                self.profile,
                operation["catalog"],
                f"{RETAINED}/{operation['id']}/data",
                operation["path"],
                operation["previous"],
            ),
        )

    def _create(self, root, operation):
        raw = operation["target"]
        target = json.loads(raw)
        mapping = self.r._active_mapping(operation["catalog"], operation["path"])
        if (
            mapping is None
            or self._target(mapping, self.app.binding("output", operation["catalog"]))[
                0
            ]
            != raw
        ):
            raise CatabolicError(
                "pending hardlink source is no longer desired; scan and recover"
            )
        with root_handle(self.app.binding("source", target["location"])) as source:
            with (
                parent_handle(source, target["source_path"]) as (src, src_name),
                parent_handle(root, operation["path"], create=True) as (dst, dst_name),
            ):
                opened = os.open(
                    src_name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=src
                )
                try:
                    observed = os.fstat(opened)
                    if not matches(observed, raw) or (
                        observed.st_size,
                        observed.st_mtime_ns,
                    ) != (target["size"], target["mtime_ns"]):
                        raise CatabolicError(
                            "source changed before hardlink creation; scan and recover"
                        )
                    if observed.st_dev != os.fstat(dst).st_dev:
                        raise CatabolicError(
                            "cross-filesystem hardlink refused at destination; no fallback"
                        )
                    checkpoint("before_link", operation)
                    try:
                        os.link(
                            src_name,
                            dst_name,
                            src_dir_fd=src,
                            dst_dir_fd=dst,
                            follow_symlinks=False,
                        )
                    except OSError as exc:
                        if exc.errno == errno.EXDEV:
                            raise CatabolicError(
                                "cross-filesystem hardlink rejected by filesystem (EXDEV); choose a symlink catalog; no fallback"
                            ) from exc
                        if exc.errno in (
                            errno.EPERM,
                            errno.EACCES,
                            errno.ENOTSUP,
                            errno.EROFS,
                        ):
                            raise CatabolicError(
                                f"filesystem or permissions rejected hardlink creation: {exc}; no fallback"
                            ) from exc
                        raise
                    os.fsync(dst)
                    current = os.stat(dst_name, dir_fd=dst, follow_symlinks=False)
                    if not matches(current, raw) or (
                        current.st_size,
                        current.st_mtime_ns,
                    ) != (target["size"], target["mtime_ns"]):
                        raise CatabolicError(
                            "source changed during hardlink creation; destination preserved for inspection"
                        )
                finally:
                    os.close(opened)
        checkpoint("after_link", operation)

    def execute(self, operation, *, after_filesystem=None):
        catalog, path, kind = operation["catalog"], operation["path"], operation["kind"]
        retained = False
        with root_handle(self.app.binding("output", catalog)) as root:
            if owner_state(root, self.r.owner(catalog)) != "owned":
                raise CatabolicError(
                    "hardlink output ownership missing during recovery"
                )
            self._check_parents(root, path)
            actual = state(root, path)
            if kind in ("hard_replace", "hard_remove"):
                retained = self._retained(root, operation)
                if not retained:
                    if actual is None and kind == "hard_remove":
                        # Someone already removed the output; there is nothing to unlink.
                        pass
                    else:
                        if not matches(actual, operation["previous"]):
                            raise CatabolicError(
                                "owned hardlink changed before retirement"
                            )
                        if kind == "hard_remove":
                            self.r._validate_removal(catalog, path)
                        else:
                            mapping = self.r._active_mapping(catalog, path)
                            if (
                                mapping is None
                                or self._target(
                                    mapping, self.app.binding("output", catalog)
                                )[0]
                                != operation["target"]
                            ):
                                raise CatabolicError(
                                    "pending hardlink replacement is no longer desired"
                                )
                        self._retain(root, operation)
                        retained = True
                actual = state(root, path)
            if kind in ("hard_create", "hard_replace"):
                if actual is None:
                    self._create(root, operation)
                elif not matches(actual, operation["target"]):
                    raise CatabolicError(
                        "hardlink creation destination changed externally"
                    )
            elif kind in ("hard_remove", "hard_forget"):
                if actual is not None:
                    raise CatabolicError(
                        "hardlink removal destination reappeared; no data overwritten"
                    )
            else:
                raise CatabolicError(f"unknown hardlink operation: {kind}")
            if after_filesystem:
                after_filesystem(operation)
        with self.store.transaction() as db:
            if retained:
                self._record_retention(db, operation)
            if kind in ("hard_create", "hard_replace"):
                db.execute(
                    "INSERT INTO owned_hardlinks VALUES (?,?,?,?) ON CONFLICT(profile,catalog,path) DO UPDATE SET target=excluded.target",
                    (self.profile, catalog, path, operation["target"]),
                )
            else:
                db.execute(
                    "DELETE FROM owned_hardlinks WHERE profile=? AND catalog=? AND path=?",
                    (self.profile, catalog, path),
                )
            if self.store.schema_version >= 6 and kind in (
                "hard_create",
                "hard_replace",
                "hard_remove",
            ):
                from .network_adapters import record_output_change

                record_output_change(db, self.profile, catalog)
            db.execute("DELETE FROM journal WHERE id=?", (operation["id"],))

    def cancel_obsolete(self, operation, *, force=False):
        """Cancel only intent whose original output is untouched or safely retained."""
        if operation["kind"] == "hard_forget":
            return False
        with root_handle(self.app.binding("output", operation["catalog"])) as root:
            if owner_state(root, self.r.owner(operation["catalog"])) != "owned":
                raise CatabolicError("hardlink output ownership missing")
            actual = state(root, operation["path"])
            retained = operation["kind"] in (
                "hard_replace",
                "hard_remove",
            ) and self._retained(root, operation)
            untouched = (
                actual is None
                if operation["kind"] == "hard_create"
                else matches(actual, operation["previous"]) and not retained
            )
            if not untouched and not (actual is None and retained):
                return False
            if not force:
                mapping = self.r._active_mapping(
                    operation["catalog"], operation["path"]
                )
                desired = (
                    self._target(
                        mapping, self.app.binding("output", operation["catalog"])
                    )[0]
                    if mapping
                    else None
                )
                obsolete = (
                    desired != operation["target"]
                    if operation["kind"] != "hard_remove"
                    else desired is not None
                )
                if not obsolete:
                    return False
        with self.store.transaction() as db:
            if retained:
                self._record_retention(db, operation)
                db.execute(
                    "DELETE FROM owned_hardlinks WHERE profile=? AND catalog=? AND path=?",
                    (self.profile, operation["catalog"], operation["path"]),
                )
            db.execute("DELETE FROM journal WHERE id=?", (operation["id"],))
        return True

    def verify(self, catalog):
        issues, verified = [], 0
        try:
            with root_handle(self.app.binding("output", catalog)) as root:
                if owner_state(root, self.r.owner(catalog)) != "owned":
                    raise CatabolicError("hardlink output ownership is missing")
            if self.store.rows(
                "SELECT 1 FROM journal WHERE profile=? AND catalog=?",
                (self.profile, catalog),
            ):
                raise CatabolicError("pending hardlink operations require recovery")
            for action in self.delta(catalog):
                if action.kind == "unchanged":
                    verified += 1
                else:
                    issues.append(
                        {
                            "path": action.path,
                            "reason": action.reason
                            or f"{action.kind} needs synchronization",
                        }
                    )
        except (OSError, CatabolicError) as exc:
            issues.append({"reason": str(exc)})
        count = self.store.db.execute(
            "SELECT count(*) FROM retained_hardlinks WHERE profile=? AND catalog=?",
            (self.profile, catalog),
        ).fetchone()[0]
        return {
            "catalog": catalog,
            "link_mode": "hardlink",
            "healthy": not issues,
            "verified_links": verified,
            "issues": issues,
            "retained_count": count,
            "warnings": [WARNING]
            + (
                [
                    f"{count} retired hardlinks remain in {RETAINED}; review with catalog retained"
                ]
                if count
                else []
            ),
        }
