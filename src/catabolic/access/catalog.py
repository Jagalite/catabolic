# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Authorized catalog views and metadata shared by REST and GraphQL."""

import base64
import hashlib
import hmac
import json

from ..store import encode
from . import AccessError


class Catalog:
    def __init__(self, access):
        self.access, self.store = access, access.store
        self.install()

    def install(self):
        a, db = self.access, self.store.db
        from ..sql_query import _setup

        _setup(db)
        db.execute("PRAGMA query_only=OFF")
        # Authorization is applied beneath filtering, joins, pagination and counts.
        for name in ("items", "files"):
            db.execute(f"CREATE TEMP TABLE api_visible_{name}(id TEXT PRIMARY KEY)")
        items = set()
        files = set()
        for grant in a.matching("metadata:read"):
            if grant.get("operator"):
                items.update(
                    r["id"]
                    for r in self.store.rows("SELECT id FROM main.items LIMIT 100001")
                )
                files.update(
                    r["id"]
                    for r in self.store.rows("SELECT id FROM main.files LIMIT 100001")
                )
            else:
                items.update(a.members(grant))
                files.update(grant.get("file_ids", []))
                if grant.get("derivatives"):
                    for item in a.members(grant):
                        files.update(
                            r["file_id"]
                            for r in self.store.rows(
                                "SELECT file_id FROM main.media_outputs WHERE source_item_id=? AND profile=?",
                                (item, a.profile),
                            )
                        )
        if len(items) > 100000 or len(files) > 100000:
            raise AccessError("authorization_scope_too_large", 422)
        db.executemany(
            "INSERT INTO api_visible_items VALUES (?)", [(i,) for i in items]
        )
        db.executemany(
            "INSERT INTO api_visible_files VALUES (?)",
            [(i,) for i in files if a.file(i)],
        )
        db.create_function(
            "api_metadata",
            2,
            lambda identifier, value: encode(a.metadata(identifier, json.loads(value))),
        )
        db.execute(
            "CREATE TEMP VIEW items AS SELECT id,kind,api_metadata(id,metadata) AS metadata FROM main.items WHERE id IN (SELECT id FROM api_visible_items)"
        )
        operator = a.operator("metadata:read")
        db.execute(
            "CREATE TEMP VIEW files AS SELECT id,"
            + ("location,path" if operator else "'' AS location,'' AS path")
            + " FROM main.files WHERE id IN (SELECT id FROM api_visible_files)"
        )
        db.execute(
            "CREATE TEMP VIEW item_files AS SELECT * FROM main.item_files WHERE file_id IN (SELECT id FROM api_visible_files) AND item_id IN (SELECT id FROM api_visible_items)"
        )
        db.execute(
            "CREATE TEMP VIEW item_relationships AS SELECT * FROM main.item_relationships WHERE source_id IN (SELECT id FROM api_visible_items) AND target_id IN (SELECT id FROM api_visible_items)"
        )
        db.execute("CREATE TEMP TABLE api_profile(id TEXT PRIMARY KEY)")
        db.execute("INSERT INTO api_profile VALUES (?)", (a.profile,))
        db.execute(
            "CREATE TEMP VIEW media_outputs AS SELECT * FROM main.media_outputs WHERE profile IN (SELECT id FROM api_profile) AND file_id IN (SELECT id FROM api_visible_files)"
        )
        db.execute(
            "CREATE TEMP VIEW identities AS SELECT * FROM main.identities WHERE item_id IN (SELECT id FROM api_visible_items)"
        )
        projections = {
            p for g in a.matching("metadata:read") for p in g.get("projection_ids", [])
        }
        db.execute("CREATE TEMP TABLE api_visible_catalogs(id TEXT PRIMARY KEY)")
        if operator:
            projections.update(
                r["id"]
                for r in self.store.rows("SELECT id FROM main.catalogs LIMIT 100001")
            )
        db.executemany(
            "INSERT INTO api_visible_catalogs VALUES (?)", [(p,) for p in projections]
        )
        db.execute(
            "CREATE TEMP VIEW catalogs AS SELECT * FROM main.catalogs WHERE id IN (SELECT id FROM api_visible_catalogs)"
        )
        db.execute(
            "CREATE TEMP VIEW mappings AS SELECT id,catalog,file_id,item_id,path,active FROM (SELECT m.* FROM main.mappings m WHERE NOT EXISTS (SELECT 1 FROM main.fallback_bindings b WHERE b.catalog=m.catalog AND b.profile IN (SELECT id FROM api_profile)) UNION ALL SELECT id,catalog,file_id,item_id,path,active FROM main.fallback_entries WHERE profile IN (SELECT id FROM api_profile) AND path IS NOT NULL) WHERE catalog IN (SELECT id FROM api_visible_catalogs) AND item_id IN (SELECT id FROM api_visible_items) AND file_id IN (SELECT id FROM api_visible_files)"
        )

        from ..component_sql import OCCURRENCES_SQL

        ids = {
            i for g in a.matching("component:read") for i in g.get("occurrence_ids", [])
        }
        if len(ids) > 100000:
            raise AccessError("authorization_scope_too_large", 422)
        db.execute("CREATE TEMP TABLE api_visible_components(id TEXT PRIMARY KEY)")
        db.executemany(
            "INSERT INTO api_visible_components VALUES (?)", [(i,) for i in ids]
        )
        db.execute("DROP VIEW catalog_component_occurrences")
        db.execute(
            "CREATE TEMP VIEW catalog_component_occurrences AS SELECT c.* FROM ("
            + OCCURRENCES_SQL
            + ") c WHERE c.profile IN (SELECT id FROM api_profile) AND "
            + (
                "1"
                if operator
                else "c.occurrence_id IN (SELECT id FROM api_visible_components)"
            )
        )
        db.execute("DROP VIEW catalog_components")
        db.execute(
            "CREATE TEMP VIEW catalog_components AS SELECT component_id,kind,profile,json_group_array(DISTINCT item_id) AS item_ids FROM catalog_component_occurrences GROUP BY component_id,kind,profile"
        )

    def cursor(self, scope, after):
        body = encode([self.access.fingerprint, scope, after]).encode()
        signature = hmac.new(
            self.access.token["digest"].encode(), body, hashlib.sha256
        ).digest()
        return base64.urlsafe_b64encode(signature + body).decode()

    def after(self, scope, cursor):
        if not cursor:
            return ""
        try:
            if len(cursor) > 4096:
                raise ValueError
            raw = base64.urlsafe_b64decode(cursor)
            signature, body = raw[:32], raw[32:]
            if not hmac.compare_digest(
                signature,
                hmac.new(
                    self.access.token["digest"].encode(), body, hashlib.sha256
                ).digest(),
            ):
                raise ValueError
            fingerprint, saved_scope, after = json.loads(body)
            if (
                fingerprint != self.access.fingerprint
                or scope != saved_scope
                or not isinstance(after, str)
            ):
                raise ValueError
            return after
        except (ValueError, TypeError):
            raise AccessError("invalid_cursor", 400) from None

    def listing(self, resource, limit=100, cursor=None, *, item_id=None):
        if (
            resource not in ("items", "files", "renditions")
            or type(limit) is not int
            or not 1 <= limit <= 1000
        ):
            raise AccessError("invalid_page", 400)
        self.access.require("metadata:read")
        scope = [resource, item_id]
        after = self.after(scope, cursor)
        table = "media_outputs" if resource == "renditions" else resource
        extra, params = "", [after]
        if resource == "renditions":
            extra += " AND profile=?"
            params.append(self.access.profile)
        if item_id:
            extra += " AND id IN (SELECT file_id FROM item_files WHERE item_id=? AND active=1)"
            params.append(item_id)
        rows = self.store.rows(
            f"SELECT * FROM {table} WHERE id>?{extra} ORDER BY id LIMIT ?",
            (*params, limit + 1),
        )
        more = len(rows) > limit
        rows = rows[:limit]
        return {
            "data": [self.present(resource, r) for r in rows],
            "page_complete": True,
            "has_more": more,
            "selection_complete": not more,
            "next_cursor": self.cursor(scope, rows[-1]["id"]) if more else None,
        }

    def present(self, resource, row):
        if resource == "items":
            return {**row, "metadata": json.loads(row["metadata"])}
        if resource == "files":
            from ..content_access import revision_of

            observation = self.store.rows(
                "SELECT size,mtime_ns,status FROM observations WHERE profile=? AND file_id=?",
                (self.access.profile, row["id"]),
            )
            value = {
                "id": row["id"],
                "revision": revision_of(self.store, self.access.profile, row["id"]),
            }
            if observation:
                value.update(
                    {
                        k: str(v) if k in ("size", "mtime_ns") else v
                        for k, v in observation[0].items()
                    }
                )
            if self.access.operator("metadata:read"):
                value.update(location=row["location"], path=row["path"])
            return value
        value = {k: row[k] for k in ("id", "file_id", "definition_id", "origin")}
        from ..content_access import revision_of

        value["revision"] = revision_of(self.store, self.access.profile, row["file_id"])
        return value

    def get(self, resource, identifier):
        table = {"items": "items", "files": "files", "renditions": "media_outputs"}[
            resource
        ]
        rows = self.store.rows(f"SELECT * FROM {table} WHERE id=?", (identifier,))
        if not rows:
            raise AccessError("not_found", 404)
        return self.present(resource, rows[0])
