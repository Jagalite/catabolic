"""Application use cases, independent of terminal parsing and presentation."""

from __future__ import annotations

import os
from pathlib import Path
from uuid import UUID, uuid4, uuid5

from .domain import CatabolicError, name, relative_path
from .filesystem import DIRECTORY_FLAGS, open_directory, root_handle, walk_files
from .media import media_kind
from .media_catalog import MediaCatalog
from .query import CatalogQuery
from .store import Store, encode


class Application:
    def __init__(self, store: Store, profile: str = "default"):
        self.store = store
        self.profile = profile
        if not store.rows("SELECT id FROM profiles WHERE id=?", (profile,)):
            raise CatabolicError(f"unknown profile: {profile}")
        self.queries = CatalogQuery(store, profile)
        self.media = MediaCatalog(self)

    def status(self) -> dict:
        return {
            "database": str(self.store.path),
            "database_id": self.store.database_id,
            "profile": self.profile,
            "pending_operations": self.store.rows(
                "SELECT * FROM journal WHERE profile=? ORDER BY rowid", (self.profile,)
            ),
            "counts": {
                table: self.store.db.execute(
                    f"SELECT count(*) FROM {table}"
                ).fetchone()[0]
                for table in (
                    "locations",
                    "catalogs",
                    "files",
                    "items",
                    "mappings",
                    "journal",
                )
            },
            "bindings": self.store.rows(
                "SELECT * FROM bindings WHERE profile=? ORDER BY kind,owner",
                (self.profile,),
            ),
        }

    def require_recovered(self):
        if self.store.rows("SELECT id FROM journal LIMIT 1"):
            raise CatabolicError(
                "recover pending operations before changing desired catalog state"
            )

    def add_profile(self, profile: str) -> dict:
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO profiles(id) VALUES (?) ON CONFLICT DO NOTHING",
                (name(profile),),
            )
        return {"profile": profile}

    def _validate_binding_path(self, kind: str, owner: str, path: Path):
        db_path = self.store.path.resolve()
        if db_path.is_relative_to(path):
            raise CatabolicError("a source or output cannot contain the database")
        for binding in self.store.rows("SELECT * FROM bindings"):
            if (binding["profile"], binding["kind"], binding["owner"]) == (
                self.profile,
                kind,
                owner,
            ):
                continue
            other = Path(binding["root"])
            overlaps = path.is_relative_to(other) or other.is_relative_to(path)
            if overlaps and (
                kind == "output"
                or binding["kind"] == "output"
                or binding["profile"] == self.profile
            ):
                raise CatabolicError(
                    f"root overlaps registered {binding['kind']}: {other}"
                )

    def _guard_default_parent(self, fd: int):
        # Compare inode identities too: case aliases can defeat path spelling
        # checks on macOS. Never create a directory inside a registered tree.
        roots = {
            (row["device"], row["inode"]): row
            for row in self.store.rows("SELECT * FROM bindings")
        }
        current = os.dup(fd)
        try:
            while True:
                st = os.fstat(current)
                identity = (st.st_dev, st.st_ino)
                if identity in roots:
                    row = roots[identity]
                    raise CatabolicError(
                        f"default output overlaps registered {row['kind']}: {row['root']}"
                    )
                parent = os.open("..", DIRECTORY_FLAGS, dir_fd=current)
                parent_st = os.fstat(parent)
                os.close(current)
                current = parent
                if (parent_st.st_dev, parent_st.st_ino) == identity:
                    break
        finally:
            os.close(current)

    def _default_output_root(self, owner: str) -> Path:
        if self.store.lock_fd is None:
            raise CatabolicError("default output creation requires a writable catalog")
        base = Path.cwd()
        path = base / "catabolic" / owner
        # Check registered trees before creating any directories. Directory-relative
        # creation rejects symlink parents rather than following them into media.
        self._validate_binding_path("output", owner, path)
        fd = open_directory(base)
        try:
            self._guard_default_parent(fd)
            for component in ("catabolic", owner):
                try:
                    os.mkdir(component, mode=0o755, dir_fd=fd)
                    os.fsync(fd)
                except FileExistsError:
                    pass
                child = os.open(component, DIRECTORY_FLAGS, dir_fd=fd)
                os.close(fd)
                fd = child
                self._guard_default_parent(fd)
            with os.scandir(fd) as entries:
                if next(entries, None) is not None:
                    raise CatabolicError(
                        "default output already contains files; choose an empty directory or bind --root explicitly"
                    )
        finally:
            os.close(fd)
        return path

    def bind(self, kind: str, owner: str, root: str | None = None) -> dict:
        self.require_recovered()
        if kind not in ("source", "output"):
            raise CatabolicError("binding kind must be source or output")
        name(owner)
        if root is None:
            if kind != "output":
                raise CatabolicError("source bindings require an explicit root")
            existing = self.store.rows(
                "SELECT * FROM bindings WHERE profile=? AND kind='output' AND owner=?",
                (self.profile, owner),
            )
            if existing:
                # Omission never relocates or silently repairs an existing binding.
                with root_handle(existing[0]):
                    pass
                return existing[0]
            path = self._default_output_root(owner)
        else:
            path = Path(root).resolve(strict=True)
        fd = open_directory(path)
        try:
            st = os.fstat(fd)
        finally:
            os.close(fd)
        self._validate_binding_path(kind, owner, path)
        old = self.store.rows(
            "SELECT * FROM bindings WHERE profile=? AND kind=? AND owner=?",
            (self.profile, kind, owner),
        )
        if old and (old[0]["root"], old[0]["device"], old[0]["inode"]) == (
            str(path),
            st.st_dev,
            st.st_ino,
        ):
            return old[0]
        if old:
            if self.store.rows(
                "SELECT id FROM journal WHERE profile=?", (self.profile,)
            ):
                raise CatabolicError(
                    "recover pending filesystem operations before changing bindings"
                )
            if kind == "output" and self.store.rows(
                "SELECT path FROM owned_links WHERE profile=? AND catalog=?",
                (self.profile, owner),
            ):
                raise CatabolicError("cannot rebind an output while it has owned links")
        with self.store.transaction() as db:
            table = "locations" if kind == "source" else "catalogs"
            db.execute(
                f"INSERT INTO {table}(id) VALUES (?) ON CONFLICT DO NOTHING", (owner,)
            )
            db.execute(
                "INSERT INTO bindings VALUES (?,?,?,?,?,?) ON CONFLICT(profile,kind,owner) DO UPDATE SET root=excluded.root,device=excluded.device,inode=excluded.inode",
                (self.profile, kind, owner, str(path), st.st_dev, st.st_ino),
            )
            if kind == "source":
                db.execute(
                    "DELETE FROM observations WHERE profile=? AND file_id IN (SELECT id FROM files WHERE location=?)",
                    (self.profile, owner),
                )
        return self.binding(kind, owner)

    def binding(self, kind: str, owner: str) -> dict:
        rows = self.store.rows(
            "SELECT * FROM bindings WHERE profile=? AND kind=? AND owner=?",
            (self.profile, kind, owner),
        )
        if not rows:
            raise CatabolicError(f"unbound {kind} {owner} in profile {self.profile}")
        return rows[0]

    def scan(
        self, location: str | None = None, *, exclude: list[str] | None = None
    ) -> dict:
        excluded = tuple(sorted({relative_path(path) for path in (exclude or [])}))
        locations = (
            [location]
            if location
            else [
                row["id"]
                for row in self.store.rows("SELECT id FROM locations ORDER BY id")
            ]
        )
        reports = []
        for source in locations:
            observed: list[dict] = []
            errors: list[str] = []
            try:
                binding = self.binding("source", source)
                with root_handle(binding) as fd:
                    observed, errors = walk_files(fd, exclude=excluded)
                    # Reopen by name to detect a mount or root replaced during traversal.
                    with root_handle(binding):
                        pass
            except (OSError, CatabolicError) as exc:
                errors.append(str(exc))
            scan_id = str(uuid4())
            complete = not errors
            with self.store.transaction() as db:
                # Unknown IDs are input errors, not failed scan records.
                if not db.execute(
                    "SELECT id FROM locations WHERE id=?", (source,)
                ).fetchone():
                    raise CatabolicError(f"unknown location: {source}")
                db.execute(
                    "INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES (?,?,?,?,?,?)",
                    (
                        scan_id,
                        self.profile,
                        source,
                        int(complete),
                        len(observed),
                        encode(errors),
                    ),
                )
                db.execute(
                    "INSERT INTO meta(key,value) VALUES (?,?)",
                    (f"scan:{scan_id}:scope", encode({"exclude": excluded})),
                )
                if complete:
                    scope_sql = "".join(
                        " AND NOT (path=? OR substr(path,1,length(?)+1)=? || '/')"
                        for _ in excluded
                    )
                    scope_args = tuple(
                        value for path in excluded for value in (path, path, path)
                    )
                    db.execute(
                        """INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id)
                        SELECT ?,id,0,0,0,0,'missing',? FROM files WHERE location=?"""
                        + scope_sql
                        + " ON CONFLICT(profile,file_id) DO UPDATE SET status='missing',scan_id=excluded.scan_id",
                        (self.profile, scan_id, source, *scope_args),
                    )
                    for entry in observed:
                        file_id = str(
                            uuid5(
                                UUID(self.store.database_id),
                                encode([source, entry["path"]]),
                            )
                        )
                        db.execute(
                            "INSERT INTO files VALUES (?,?,?) ON CONFLICT(location,path) DO NOTHING",
                            (file_id, source, entry["path"]),
                        )
                        db.execute(
                            "INSERT INTO observations VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(profile,file_id) DO UPDATE SET size=excluded.size,mtime_ns=excluded.mtime_ns,device=excluded.device,inode=excluded.inode,status=excluded.status,scan_id=excluded.scan_id",
                            (
                                self.profile,
                                file_id,
                                entry["size"],
                                entry["mtime_ns"],
                                entry["device"],
                                entry["inode"],
                                "present",
                                scan_id,
                            ),
                        )
            reports.append(
                {
                    "scan_id": scan_id,
                    "location": source,
                    "complete": complete,
                    "observed": len(observed),
                    "published": len(observed) if complete else 0,
                    "errors": errors,
                    "excluded": list(excluded),
                }
            )
        return {
            "complete": all(report["complete"] for report in reports),
            "scans": reports,
        }

    def files(self, **filters) -> dict:
        return self.queries.files(**filters)

    def put_item(
        self,
        kind: str,
        identities: dict[str, str],
        metadata: dict,
        item_id: str | None = None,
    ) -> dict:
        media_kind(kind)
        if not isinstance(metadata, dict):
            raise CatabolicError("item metadata must be a JSON object")
        if not identities and item_id is None:
            raise CatabolicError(
                "supply an authoritative identity or an explicit item ID"
            )
        for namespace, value in identities.items():
            name(namespace)
            if not isinstance(value, str) or not value.strip():
                raise CatabolicError("identity values must be nonempty strings")
        with self.store.transaction() as db:
            matches = {
                row[0]
                for namespace, value in identities.items()
                for row in db.execute(
                    "SELECT item_id FROM identities WHERE namespace=? AND value=?",
                    (namespace, value),
                )
            }
            if len(matches) > 1 or (item_id and matches and item_id not in matches):
                raise CatabolicError(
                    "supplied identities refer to different media items"
                )
            actual_id = item_id or next(iter(matches), str(uuid4()))
            existing = db.execute(
                "SELECT * FROM items WHERE id=?", (actual_id,)
            ).fetchone()
            if existing and existing["kind"] != kind:
                raise CatabolicError("an existing item's kind cannot change")
            db.execute(
                "INSERT INTO items VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET metadata=excluded.metadata",
                (actual_id, kind, encode(metadata)),
            )
            for namespace, value in identities.items():
                db.execute(
                    "INSERT INTO identities VALUES (?,?,?) ON CONFLICT(namespace,value) DO NOTHING",
                    (namespace, value, actual_id),
                )
        return {
            "id": actual_id,
            "kind": kind,
            "metadata": metadata,
            "identities": self.store.rows(
                "SELECT namespace,value FROM identities WHERE item_id=? ORDER BY namespace,value",
                (actual_id,),
            ),
        }

    def put_mapping(self, catalog: str, file_id: str, item_id: str, path: str) -> dict:
        self.require_recovered()
        path = relative_path(path)
        mapping_id = self.mapping_id(catalog, file_id, item_id, path)
        with self.store.transaction() as db:
            for table, key in (
                ("catalogs", catalog),
                ("files", file_id),
                ("items", item_id),
            ):
                if not db.execute(
                    f"SELECT id FROM {table} WHERE id=?", (key,)
                ).fetchone():
                    raise CatabolicError(f"unknown {table} ID: {key}")
            for row in db.execute(
                "SELECT id,path FROM mappings WHERE catalog=? AND active=1", (catalog,)
            ):
                if row["id"] != mapping_id and (
                    row["path"] == path
                    or row["path"].startswith(path + "/")
                    or path.startswith(row["path"] + "/")
                ):
                    raise CatabolicError(
                        f"destination conflicts with active mapping: {row['path']}"
                    )
            db.execute(
                "INSERT INTO mappings VALUES (?,?,?,?,?,1) ON CONFLICT(id) DO UPDATE SET active=1",
                (mapping_id, catalog, file_id, item_id, path),
            )
            if not db.execute(
                "SELECT 1 FROM item_files WHERE file_id=? AND item_id=? AND active=1",
                (file_id, item_id),
            ).fetchone():
                self.media.associate_in_transaction(
                    db, file_id, item_id, origin="mapping"
                )
        return self.store.rows("SELECT * FROM mappings WHERE id=?", (mapping_id,))[0]

    def mapping_id(self, catalog, file_id, item_id, path):
        return str(
            uuid5(
                UUID(self.store.database_id), encode([catalog, file_id, item_id, path])
            )
        )

    def disable_mapping(self, mapping_id: str) -> dict:
        self.require_recovered()
        with self.store.transaction() as db:
            if not db.execute(
                "SELECT id FROM mappings WHERE id=?", (mapping_id,)
            ).fetchone():
                raise CatabolicError(f"unknown mapping: {mapping_id}")
            db.execute("UPDATE mappings SET active=0 WHERE id=?", (mapping_id,))
        return {"id": mapping_id, "active": False}
