# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Versioned metadata snapshots and guarded atomic manifest publication."""

import hashlib
import json
import os
import stat
import unicodedata
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from uuid import uuid4

from . import __version__
from .domain import CatabolicError
from .filesystem import DIRECTORY_FLAGS, open_directory, owner_state, root_handle
from .interchange.v3 import FORMAT, VERSION
from .interchange.validation import validate_document
from .layouts import Layouts
from .migration import SCHEMA_VERSION
from .reconcile import Reconciler
from .store import encode

FILENAME = ".catabolic-manifest.json"
MAX_RECORDS = 100000
MAX_BYTES = 128 * 1024 * 1024


def digest(content):
    return hashlib.sha256(encode(content).encode()).hexdigest()


def path_key(path):
    return unicodedata.normalize("NFC", str(path)).casefold()


def batches(values):
    values = sorted(values)
    for offset in range(0, len(values), 500):
        yield values[offset : offset + 500]


def bounded(rows, label):
    if len(rows) > MAX_RECORDS:
        raise CatabolicError(
            f"manifest exceeds {MAX_RECORDS} {label}; export a smaller catalog"
        )
    return rows


class Manifest:
    def __init__(self, app):
        self.app, self.store = app, app.store

    def build(self, catalog="global", *, extra=None):
        """Call inside a Store read snapshot or its writer transaction."""
        if not self.store.db.in_transaction:
            raise CatabolicError("manifest export requires a database snapshot")
        self.app.require_recovered()
        if not isinstance(extra if extra is not None else {}, dict):
            raise CatabolicError("extra metadata must be a JSON object")
        if extra is not None:
            encode(extra)
        Reconciler(self.app).catalogs(catalog)
        link_mode = self.app.link_mode(catalog)
        mappings = bounded(
            self.store.rows(
                "SELECT * FROM mappings WHERE catalog=? AND active=1 ORDER BY path,id LIMIT ?",
                (catalog, MAX_RECORDS + 1),
            ),
            "entries",
        )
        bindings = self.store.rows(
            "SELECT kind,owner,root FROM bindings WHERE profile=?", (self.app.profile,)
        )
        roots = {(row["kind"], row["owner"]): row["root"] for row in bindings}
        output_root = roots.get(("output", catalog))
        records = bounded(
            self.store.rows(
                "SELECT path,target FROM owned_links WHERE profile=? AND catalog=? ORDER BY path LIMIT ?",
                (self.app.profile, catalog, MAX_RECORDS + 1),
            ),
            "recorded links",
        )
        recorded = {row["path"]: row["target"] for row in records}
        files = {}
        for batch in batches({row["file_id"] for row in mappings}):
            marks = ",".join("?" for _ in batch)
            for row in self.store.rows(
                f"""SELECT f.*,o.size,o.mtime_ns,coalesce(o.status,'unknown') AS status,o.scan_id,s.finished_at AS observed_at
                FROM files f LEFT JOIN observations o ON o.file_id=f.id AND o.profile=?
                LEFT JOIN scans s ON s.id=o.scan_id WHERE f.id IN ({marks})""",
                (self.app.profile, *batch),
            ):
                root = roots.get(("source", row["location"]))
                row["source_root"] = root
                row["source_path"] = (
                    str(PurePosixPath(root) / row["path"]) if root else None
                )
                for field in ("size", "mtime_ns"):
                    row[field] = str(row[field]) if row[field] is not None else None
                files[row["id"]] = row
        associations = bounded(
            self.store.rows(
                """SELECT a.* FROM (SELECT DISTINCT item_id,file_id FROM mappings WHERE active=1 AND catalog=?) m
            JOIN item_files a ON a.item_id=m.item_id AND a.file_id=m.file_id
            ORDER BY a.id LIMIT ?""",
                (catalog, MAX_RECORDS + 1),
            ),
            "associations",
        )
        by_pair = defaultdict(list)
        owner = self.store.rows(
            "SELECT value FROM meta WHERE key=?", ("layout-catalog:" + catalog,)
        )
        admitted = (
            set(json.loads(owner[0]["value"]).get("rendition_associations", []))
            if owner
            else set()
        )
        associations = [
            row for row in associations if row["active"] or row["id"] in admitted
        ]
        for row in associations:
            row["metadata"] = json.loads(row["metadata"])
            # This is an active-catalog projection, not the global association flag.
            row["active"] = True
            by_pair[(row["item_id"], row["file_id"])].append(row["id"])
        # Include outgoing parents, editions, and contributors recursively, but
        # not unrelated incoming siblings or their source files.
        items, relationships = {}, {}
        frontier = {row["item_id"] for row in mappings}
        while frontier:
            if len(items) + len(frontier) > MAX_RECORDS:
                raise CatabolicError(
                    "manifest related-item closure exceeds the record limit"
                )
            next_ids = set()
            for batch in batches(frontier):
                marks = ",".join("?" for _ in batch)
                rows = self.store.rows(
                    f"SELECT * FROM items WHERE id IN ({marks})", tuple(batch)
                )
                if {row["id"] for row in rows} != set(batch):
                    raise CatabolicError("manifest references missing related items")
                self.app.queries._decorate_items(rows)
                items.update((row["id"], row) for row in rows)
                edges = self.store.rows(
                    f"SELECT * FROM item_relationships WHERE active=1 AND source_id IN ({marks}) ORDER BY id LIMIT ?",
                    (*batch, MAX_RECORDS + 1),
                )
                for edge in edges:
                    edge["metadata"] = json.loads(edge["metadata"])
                    edge["active"] = bool(edge["active"])
                    relationships[edge["id"]] = edge
                    next_ids.add(edge["target_id"])
                bounded(relationships, "relationships")
            frontier = next_ids - items.keys()
        layouts = Layouts(self.app)
        owner = layouts._read("layout-catalog:" + catalog)
        layout = None
        owned = set()
        if owner:
            definition = layouts.effective_definition(owner["layout"], catalog)
            owned = set(owner["mapping_ids"])
            current_hash = digest(definition)
            layout = {
                "name": owner["layout"],
                "current_definition": definition,
                "current_definition_sha256": current_hash,
                "last_applied_definition_sha256": owner["definition_sha256"],
                "definition_matches_last_apply": current_hash
                == owner["definition_sha256"],
            }
        entries = []
        for row in mappings:
            source = files[row["file_id"]]["source_path"]
            output = (
                str(PurePosixPath(output_root) / row["path"]) if output_root else None
            )
            expected = (
                os.path.relpath(source, str(PurePosixPath(output).parent))
                if source and output and link_mode == "symlink"
                else None
            )
            entries.append(
                {
                    "mapping_id": row["id"],
                    "path": row["path"],
                    "output_path": output,
                    "item_id": row["item_id"],
                    "file_id": row["file_id"],
                    "association_ids": by_pair[(row["item_id"], row["file_id"])],
                    "managed_by_layout": owner["layout"]
                    if row["id"] in owned
                    else None,
                    "expected_link_target": expected,
                    "recorded_link_target": recorded.get(row["path"]),
                    "recorded_link_matches_desired": recorded.get(row["path"])
                    == expected
                    if expected is not None
                    else None,
                }
            )
        content = {
            "database_id": self.store.database_id,
            "database_schema": SCHEMA_VERSION,
            "profile": self.app.profile,
            "catalog": {
                "id": catalog,
                "output_root": output_root,
                "link_mode": link_mode,
            },
            "state": "desired_catalog",
            "filesystem_verified": False,
            "layout": layout,
            "entries": entries,
            "items": [items[key] for key in sorted(items)],
            "files": [files[key] for key in sorted(files)],
            "associations": associations,
            "relationships": [relationships[key] for key in sorted(relationships)],
            "recorded_links": records,
            "extra": extra if extra is not None else {},
        }
        # Preserve provenance even for withdrawn assignments on included entities.
        taggings = []
        for subject, included in (("item", items), ("file", files)):
            for batch in batches(included):
                marks = ",".join("?" for _ in batch)
                rows = self.store.rows(
                    f"SELECT * FROM {subject}_tags WHERE {subject}_id IN ({marks}) ORDER BY id LIMIT ?",
                    (*batch, MAX_RECORDS + 1),
                )
                for row in rows:
                    row["subject_type"] = subject
                    row["subject_id"] = row.pop(subject + "_id")
                    row["active"] = bool(row["active"])
                taggings.extend(rows)
                bounded(taggings, "taggings")
        tags, names, parents = {}, [], []
        frontier = {row["tag_id"] for row in taggings}
        while frontier:
            bounded(set(tags) | frontier, "tag closure")
            next_ids = set()
            for batch in batches(frontier):
                marks = ",".join("?" for _ in batch)
                tags.update(
                    (row["id"], row)
                    for row in self.store.rows(
                        f"SELECT * FROM tags WHERE id IN ({marks})", tuple(batch)
                    )
                )
                names.extend(
                    self.store.rows(
                        f"SELECT * FROM tag_names WHERE tag_id IN ({marks}) LIMIT ?",
                        (*batch, MAX_RECORDS + 1),
                    )
                )
                edges = self.store.rows(
                    f"SELECT * FROM tag_parents WHERE child_id IN ({marks}) LIMIT ?",
                    (*batch, MAX_RECORDS + 1),
                )
                parents.extend(edges)
                next_ids.update(row["parent_id"] for row in edges)
                bounded(names, "tag names")
                bounded(parents, "tag parents")
            frontier = next_ids - tags.keys()
        content.update(
            tags=[tags[key] for key in sorted(tags)],
            tag_names=sorted(names, key=lambda r: r["name"]),
            tag_parents=sorted(parents, key=lambda r: (r["child_id"], r["parent_id"])),
            taggings=sorted(taggings, key=lambda r: r["id"]),
        )
        for table, key in (
            ("owned_hardlinks", "hardlinks"),
            ("retained_hardlinks", "retained_hardlinks"),
        ):
            records = bounded(
                self.store.rows(
                    f"SELECT * FROM {table} WHERE profile=? AND catalog=? ORDER BY path LIMIT ?",
                    (self.app.profile, catalog, MAX_RECORDS + 1),
                ),
                key,
            )
            content[key] = []
            for record in records:
                target = json.loads(record["target"])
                entry = {
                    "path": record["path"],
                    **{
                        key: target[key]
                        for key in ("file_id", "location", "source_path")
                    },
                    **{key: str(target[key]) for key in ("device", "inode")},
                }
                if table == "retained_hardlinks":
                    entry.update(
                        {
                            key: record[key]
                            for key in ("id", "original_path", "retained_at")
                        }
                    )
                content[key].append(entry)
        content["counts"] = {
            key: len(content[key])
            for key in (
                "entries",
                "items",
                "files",
                "associations",
                "relationships",
                "recorded_links",
                "tags",
                "tag_names",
                "tag_parents",
                "taggings",
                "hardlinks",
                "retained_hardlinks",
            )
        }
        result = {
            "format": FORMAT,
            "format_version": VERSION,
            "generator": {"name": "catabolic", "version": __version__},
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "content_sha256": digest(content),
            "content": content,
        }
        if len(encode(result).encode()) > MAX_BYTES:
            raise CatabolicError(
                "manifest exceeds the 128 MiB output limit; nothing was exported"
            )
        validate_document(result)
        return result

    def write(self, document, *, output=None, in_catalog=False, replace=False):
        if self.store.lock_fd is None or not self.store.db.in_transaction:
            raise CatabolicError(
                "manifest publication requires the catalog writer lock and snapshot"
            )
        validate_document(document)
        catalog = document["content"]["catalog"]["id"]
        if in_catalog:
            binding = self.app.binding("output", catalog)
            path = Path(binding["root"]) / FILENAME
            with root_handle(binding) as fd:
                self._check_owner(fd, catalog)
                return self._publish(fd, path, document, replace)
        if output is None:
            raise CatabolicError("select an output path or --in-catalog")
        path = Path(output).absolute()
        if ".." in path.parts:
            raise CatabolicError("manifest output cannot contain parent traversal")
        # Check every known source binding, not just this machine profile.
        for binding in self.store.rows("SELECT * FROM bindings"):
            root = Path(binding["root"])
            if path_key(path).startswith(path_key(root).rstrip("/") + "/"):
                if binding["kind"] == "source":
                    raise CatabolicError(
                        "manifest output cannot be inside a source tree"
                    )
                if (binding["profile"], binding["owner"]) != (
                    self.app.profile,
                    catalog,
                ) or path != root / FILENAME:
                    raise CatabolicError(
                        "inside an output catalog, use its reserved --in-catalog manifest"
                    )
                with root_handle(binding) as fd:
                    self._check_owner(fd, catalog)
                    return self._publish(fd, path, document, replace)
        if path_key(path.name).startswith(".catabolic"):
            raise CatabolicError(
                "reserved manifest names belong inside their output catalog"
            )
        # Never target SQLite files or its lock, journals, or backups.
        if (
            path_key(path) == path_key(self.store.path)
            or path_key(path).startswith(path_key(self.store.path) + ".")
            or path_key(path)
            in {
                path_key(self.store.path) + suffix
                for suffix in ("-wal", "-shm", "-journal")
            }
        ):
            raise CatabolicError("manifest output cannot replace database state")
        fd = open_directory(path.parent)
        try:
            return self._publish(fd, path, document, replace)
        finally:
            os.close(fd)

    def _check_owner(self, fd, catalog):
        if owner_state(fd, Reconciler(self.app).owner(catalog)) != "owned":
            raise CatabolicError("sync this catalog before writing its manifest")

    def _guard_ancestors(self, fd, leaf, catalog):
        """Recognize bound trees by inode as well as spelling (case aliases)."""
        bindings = self.store.rows("SELECT * FROM bindings")
        database_parent = self.store.path.parent.stat()
        database_identity = (database_parent.st_dev, database_parent.st_ino)
        current, depth = os.dup(fd), 0
        try:
            while True:
                status = os.fstat(current)
                identity = (status.st_dev, status.st_ino)
                if (
                    depth == 0
                    and identity == database_identity
                    and (
                        path_key(leaf) == path_key(self.store.path.name)
                        or path_key(leaf).startswith(
                            path_key(self.store.path.name) + "."
                        )
                        or path_key(leaf)
                        in {
                            path_key(self.store.path.name) + suffix
                            for suffix in ("-wal", "-shm", "-journal")
                        }
                    )
                ):
                    raise CatabolicError(
                        "manifest output cannot replace database state"
                    )
                for binding in bindings:
                    if identity != (binding["device"], binding["inode"]):
                        continue
                    if binding["kind"] == "source":
                        raise CatabolicError(
                            "manifest output cannot be inside a source tree"
                        )
                    if (
                        depth
                        or leaf != FILENAME
                        or (binding["profile"], binding["owner"])
                        != (self.app.profile, catalog)
                    ):
                        raise CatabolicError(
                            "inside an output catalog, use its reserved --in-catalog manifest"
                        )
                    self._check_owner(current, catalog)
                parent = os.open("..", DIRECTORY_FLAGS, dir_fd=current)
                parent_status = os.fstat(parent)
                os.close(current)
                current = parent
                if (parent_status.st_dev, parent_status.st_ino) == identity:
                    break
                depth += 1
        finally:
            os.close(current)

    def _existing(self, fd, leaf, document):
        try:
            source = os.open(
                leaf, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=fd
            )
        except FileNotFoundError:
            return None, False
        with os.fdopen(source, "rb") as stream:
            status = os.fstat(stream.fileno())
            if (
                not stat.S_ISREG(status.st_mode)
                or status.st_nlink != 1
                or status.st_size > MAX_BYTES
            ):
                raise CatabolicError(
                    "existing manifest must be a single-link regular file within the output limit"
                )
            try:
                existing = json.loads(stream.read(MAX_BYTES + 1))
                content = existing["content"]
                expected = document["content"]
                same_owner = (
                    content["database_id"],
                    content["profile"],
                    content["catalog"]["id"],
                ) == (
                    expected["database_id"],
                    expected["profile"],
                    expected["catalog"]["id"],
                )
                valid = (
                    existing["format"] == FORMAT
                    and existing["format_version"] in (1, 2, 3)
                    and digest(content) == existing["content_sha256"]
                )
            except (KeyError, ValueError, TypeError, RecursionError) as exc:
                raise CatabolicError(
                    "refusing to replace an unrelated or edited metadata file"
                ) from exc
            if valid:
                validate_document(existing)
            if not same_owner or not valid:
                raise CatabolicError(
                    "refusing to replace a foreign, edited, or unsupported manifest"
                )
            identity = (
                status.st_dev,
                status.st_ino,
                status.st_size,
                status.st_mtime_ns,
                status.st_ctime_ns,
            )
            return identity, existing["content_sha256"] == document["content_sha256"]

    def _publish(self, fd, path, document, replace):
        self._guard_ancestors(fd, path.name, document["content"]["catalog"]["id"])
        payload = (encode(document) + "\n").encode()
        if len(payload) > MAX_BYTES:
            raise CatabolicError("manifest exceeds the output limit")
        identity, unchanged = self._existing(fd, path.name, document)
        result = {
            "output": str(path),
            "content_sha256": document["content_sha256"],
            "counts": document["content"]["counts"],
            "written": False,
            "unchanged": unchanged,
        }
        if unchanged:
            return result
        if identity is not None and not replace:
            raise CatabolicError(
                "manifest changed; use --replace to refresh this owned export"
            )
        temporary = ".catabolic-manifest-" + uuid4().hex
        temp_fd = os.open(
            temporary,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
            0o600,
            dir_fd=fd,
        )
        try:
            with os.fdopen(temp_fd, "wb") as stream:
                stream.write(payload)
                stream.flush()
                os.fsync(stream.fileno())
            if identity is None:
                os.link(
                    temporary,
                    path.name,
                    src_dir_fd=fd,
                    dst_dir_fd=fd,
                    follow_symlinks=False,
                )
            else:
                current, _ = self._existing(fd, path.name, document)
                if current != identity:
                    raise CatabolicError(
                        "manifest changed during export; refusing replacement"
                    )
                os.replace(temporary, path.name, src_dir_fd=fd, dst_dir_fd=fd)
            os.fsync(fd)
            result["written"] = True
            return result
        finally:
            try:
                os.unlink(temporary, dir_fd=fd)
                os.fsync(fd)
            except FileNotFoundError:
                pass
