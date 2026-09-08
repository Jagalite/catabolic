# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Read-only catalog queries. Availability is recorded evidence, not a live probe."""

from __future__ import annotations

import base64
import hashlib
import json
from pathlib import PurePosixPath

from .domain import CatabolicError, name
from .item_workflow import STATUSES, SUMMARY_SQL
from .media import RELATIONS, ROLES, media_kind, vocabulary
from .store import Store, encode
from .tagging import resolve_tag, tag_filters, tag_name


def identity_pair(value: str) -> tuple[str, str]:
    namespace, separator, identity = value.partition("=")
    if not separator or not identity.strip():
        raise CatabolicError("identity must be NAMESPACE=VALUE")
    name(namespace)
    return namespace, identity


def _metadata(raw, key):
    value = json.loads(raw)
    return value.get(key) if isinstance(value, dict) else None


def _title(raw):
    title = _metadata(raw, "title")
    return title.casefold() if isinstance(title, str) else ""


def _year(raw):
    year = _metadata(raw, "year")
    return year if type(year) is int and 1 <= year <= 9999 else -1


class CatalogQuery:
    def __init__(self, store: Store, profile: str):
        self.store = store
        self.profile = profile
        db = store.db
        db.create_function(
            "contains_text",
            2,
            lambda value, needle: needle.casefold() in (value or "").casefold(),
            deterministic=True,
        )
        db.create_function("title_key", 1, _title, deterministic=True)
        db.create_function("year_key", 1, _year, deterministic=True)
        db.create_function(
            "metadata_matches",
            3,
            lambda raw, key, expected: (
                key in json.loads(raw) and encode(_metadata(raw, key)) == expected
            ),
            deterministic=True,
        )

    def _exists(self, table: str, value: str | None):
        # Table names only come from fixed call sites, never CLI input.
        if value is not None and not self.store.rows(
            f"SELECT id FROM {table} WHERE id=?", (value,)
        ):
            raise CatabolicError(f"unknown {table}: {value}")

    def _page(
        self,
        key,
        select,
        source,
        conditions,
        parameters,
        sorts,
        sort,
        descending,
        limit,
        cursor,
        context,
    ):
        if type(limit) is not int or not 1 <= limit <= 1000:
            raise CatabolicError("limit must be between 1 and 1000")
        if sort not in sorts:
            raise CatabolicError(f"sort must be one of: {', '.join(sorts)}")
        fingerprint = hashlib.sha256(
            encode(
                [
                    self.store.database_id,
                    self.profile,
                    key,
                    context,
                    sort,
                    descending,
                ]
            ).encode()
        ).hexdigest()
        after = ""
        values = list(parameters)
        direction = "DESC" if descending else "ASC"
        comparison = "<" if descending else ">"
        if cursor:
            try:
                if len(cursor) > 16384:
                    raise ValueError()
                token = json.loads(
                    base64.b64decode(cursor, altchars=b"-_", validate=True)
                )
                if (
                    not isinstance(token, list)
                    or len(token) != 4
                    or token[:2] != [1, fingerprint]
                    or type(token[2]) not in (str, int)
                    or not isinstance(token[3], str)
                ):
                    raise ValueError()
                after = (
                    f"WHERE (_sort {comparison} ? OR (_sort = ? AND id {comparison} ?))"
                )
                values.extend((token[2], token[2], token[3]))
            except (ValueError, TypeError, UnicodeError) as exc:
                raise CatabolicError(
                    "invalid cursor or cursor belongs to another query"
                ) from exc
        values.append(limit + 1)
        where = " AND ".join(conditions) or "1"
        rows = self.store.rows(
            f"""WITH matches AS (
                SELECT {select}, {sorts[sort]} AS _sort FROM {source} WHERE {where}
            ) SELECT * FROM matches {after}
            ORDER BY _sort {direction}, id {direction} LIMIT ?""",
            tuple(values),
        )
        more = len(rows) > limit
        rows = rows[:limit]
        next_cursor = (
            base64.urlsafe_b64encode(
                encode(
                    [
                        1,
                        fingerprint,
                        rows[-1]["_sort"],
                        rows[-1]["id"],
                    ]
                ).encode()
            ).decode()
            if more
            else None
        )
        for row in rows:
            del row["_sort"]
        return {key: rows, "next_cursor": next_cursor}

    def _item_filters(
        self, *, search=None, kind=None, year=None, identity=None, metadata=()
    ):
        clauses, values = [], []
        if search is not None:
            clauses.append("contains_text(title_key(i.metadata), ?)")
            values.append(search)
        if kind is not None:
            media_kind(kind)
            clauses.append("i.kind=?")
            values.append(kind)
        if year is not None:
            if type(year) is not int or not 1 <= year <= 9999:
                raise CatabolicError("year must be between 1 and 9999")
            clauses.append("year_key(i.metadata)=?")
            values.append(year)
        if identity is not None:
            clauses.append(
                "i.id IN (SELECT x.item_id FROM identities x WHERE x.namespace=? AND x.value=?)"
            )
            values.extend(identity_pair(identity))
        for expression in metadata:
            key, separator, raw = expression.partition("=")
            try:
                if not separator or not key:
                    raise ValueError()
                value = encode(json.loads(raw))
            except (ValueError, TypeError) as exc:
                raise CatabolicError("metadata filter must be KEY=JSON_VALUE") from exc
            clauses.append("metadata_matches(i.metadata, ?, ?)")
            values.extend((key, value))
        return clauses, values

    def files(
        self,
        *,
        catalog="global",
        unmapped=False,
        unidentified=False,
        search=None,
        location=None,
        status=None,
        item=None,
        kind=None,
        year=None,
        identity=None,
        tags=(),
        any_tags=(),
        not_tags=(),
        descendants=False,
        sort="id",
        descending=False,
        limit=100,
        cursor=None,
    ):
        self._exists("catalogs", catalog)
        self._exists("locations", location)
        self._exists("items", item)
        clauses, values = [], [self.profile]
        for expression, value in (
            ("contains_text(f.path, ?)", search),
            ("f.location=?", location),
        ):
            if value is not None:
                clauses.append(expression)
                values.append(value)
        if status is not None:
            if status not in ("present", "missing", "unknown"):
                raise CatabolicError("status must be present, missing, or unknown")
            clauses.append("coalesce(o.status,'unknown')=?")
            values.append(status)
        if unmapped:
            clauses.append(
                "f.id NOT IN (SELECT m.file_id FROM mappings m WHERE m.catalog=? AND m.active=1)"
            )
            values.append(catalog)
        if unidentified:
            clauses.append(
                "f.id NOT IN (SELECT a.file_id FROM item_files a WHERE a.active=1)"
            )
        item_clauses, item_values = self._item_filters(
            kind=kind, year=year, identity=identity
        )
        if item is not None:
            item_clauses.append("i.id=?")
            item_values.append(item)
        if item_clauses:
            clauses.append(
                "f.id IN (SELECT a.file_id FROM item_files a JOIN items i ON i.id=a.item_id WHERE a.active=1 AND "
                + " AND ".join(item_clauses)
                + ")"
            )
            values.extend(item_values)
        tag_clauses, tag_values = tag_filters(
            self.store.db,
            "file",
            tags=tags,
            any_tags=any_tags,
            not_tags=not_tags,
            descendants=descendants,
        )
        clauses.extend(tag_clauses)
        values.extend(tag_values)
        return self._page(
            "files",
            "f.*,o.size,o.mtime_ns,coalesce(o.status,'unknown') AS status,o.scan_id,s.finished_at AS observed_at",
            "files f LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? LEFT JOIN scans s ON s.id=o.scan_id",
            clauses,
            values,
            {
                "id": "f.id",
                "path": "f.path",
                "size": "coalesce(o.size,-1)",
                "mtime": "coalesce(o.mtime_ns,-1)",
            },
            sort,
            descending,
            limit,
            cursor,
            [
                catalog,
                unmapped,
                unidentified,
                search,
                location,
                status,
                item,
                kind,
                year,
                identity,
                tags,
                any_tags,
                not_tags,
                descendants,
            ],
        )

    def _decorate_items(self, rows):
        if not rows:
            return
        by_id = {row["id"]: row for row in rows}
        for row in rows:
            row["metadata"] = json.loads(row["metadata"])
            row["identities"] = []
        # Keep below older SQLite builds' 999 bound-parameter limit.
        ids = list(by_id)
        for start in range(0, len(ids), 500):
            batch = ids[start : start + 500]
            placeholders = ",".join("?" for _ in batch)
            for identity in self.store.rows(
                f"SELECT item_id,namespace,value FROM identities WHERE item_id IN ({placeholders}) ORDER BY namespace,value",
                tuple(batch),
            ):
                by_id[identity.pop("item_id")]["identities"].append(identity)
            for workflow in self.store.rows(
                f"SELECT * FROM ({SUMMARY_SQL}) WHERE profile=? AND item_id IN ({placeholders})",
                (self.profile, *batch),
            ):
                by_id[workflow.pop("item_id")]["workflow"] = workflow

    def items(
        self,
        *,
        search=None,
        kind=None,
        year=None,
        identity=None,
        metadata=(),
        catalog=None,
        curation_status=None,
        has_rendition=None,
        missing_rendition=None,
        tags=(),
        any_tags=(),
        not_tags=(),
        descendants=False,
        sort="id",
        descending=False,
        limit=100,
        cursor=None,
    ):
        self._exists("catalogs", catalog)
        if curation_status is not None and curation_status not in (
            *STATUSES,
            "needs_attention",
        ):
            raise CatabolicError("invalid curation status")
        clauses, values = self._item_filters(
            search=search, kind=kind, year=year, identity=identity, metadata=metadata
        )
        if curation_status is not None:
            clauses.append(
                f"i.id IN (SELECT item_id FROM ({SUMMARY_SQL}) WHERE profile=? AND status=?)"
            )
            values.extend((self.profile, curation_status))
        from .catalog_state import RENDITION_STATE_SQL, rendition_condition

        for wanted, negate in ((has_rendition, False), (missing_rendition, True)):
            if wanted is not None:
                conditions, params = rendition_condition(wanted)
                clauses.append(
                    ("NOT " if negate else "")
                    + f"EXISTS (SELECT 1 FROM ({RENDITION_STATE_SQL}) r WHERE r.profile=? AND r.source_item_id=i.id"
                    + (" AND " + " AND ".join(conditions) if conditions else "")
                    + ")"
                )
                values.extend((self.profile, *params))
        if catalog is not None:
            clauses.append(
                "i.id IN (SELECT m.item_id FROM mappings m WHERE m.catalog=? AND m.active=1)"
            )
            values.append(catalog)
        tag_clauses, tag_values = tag_filters(
            self.store.db,
            "item",
            tags=tags,
            any_tags=any_tags,
            not_tags=not_tags,
            descendants=descendants,
        )
        clauses.extend(tag_clauses)
        values.extend(tag_values)
        result = self._page(
            "items",
            "i.*",
            "items i",
            clauses,
            values,
            {
                "id": "i.id",
                "title": "title_key(i.metadata)",
                "year": "year_key(i.metadata)",
            },
            sort,
            descending,
            limit,
            cursor,
            [
                search,
                kind,
                year,
                identity,
                list(metadata),
                catalog,
                curation_status,
                has_rendition,
                missing_rendition,
                tags,
                any_tags,
                not_tags,
                descendants,
            ],
        )
        self._decorate_items(result["items"])
        return result

    def tags(
        self,
        *,
        search=None,
        namespace=None,
        parent=None,
        child=None,
        limit=100,
        cursor=None,
    ):
        clauses, values = [], []
        if search is not None:
            clauses.append(
                "EXISTS (SELECT 1 FROM tag_names n WHERE n.tag_id=t.id AND contains_text(n.name,?))"
            )
            values.append(search)
        if namespace is not None:
            prefix = tag_name(namespace + ":placeholder").partition(":")[0] + ":"
            clauses.append("substr(t.name,1,?)=?")
            values.extend((len(prefix), prefix))
        for reference, column, other in (
            (parent, "child_id", "parent_id"),
            (child, "parent_id", "child_id"),
        ):
            if reference is not None:
                clauses.append(
                    f"t.id IN (SELECT {column} FROM tag_parents WHERE {other}=?)"
                )
                values.append(resolve_tag(self.store.db, reference))
        result = self._page(
            "tags",
            "t.*",
            "tags t",
            clauses,
            values,
            {"name": "t.name"},
            "name",
            False,
            limit,
            cursor,
            [search, namespace, parent, child],
        )
        by_id = {row["id"]: row for row in result["tags"]}
        for row in by_id.values():
            row["aliases"], row["parent_ids"] = [], []
        ids = list(by_id)
        for offset in range(0, len(ids), 500):
            batch = ids[offset : offset + 500]
            marks = ",".join("?" for _ in batch)
            for row in self.store.rows(
                f"SELECT name,tag_id FROM tag_names WHERE tag_id IN ({marks}) ORDER BY name",
                tuple(batch),
            ):
                if row["name"] != by_id[row["tag_id"]]["name"]:
                    by_id[row["tag_id"]]["aliases"].append(row["name"])
            for row in self.store.rows(
                f"SELECT child_id,parent_id FROM tag_parents WHERE child_id IN ({marks}) ORDER BY parent_id",
                tuple(batch),
            ):
                by_id[row["child_id"]]["parent_ids"].append(row["parent_id"])
        return result

    def taggings(
        self,
        *,
        tag=None,
        item=None,
        file=None,
        source=None,
        active="active",
        limit=100,
        cursor=None,
    ):
        if item is not None and file is not None:
            raise CatabolicError("choose item or file for an assignment query")
        self._exists("items", item)
        self._exists("files", file)
        if active not in ("active", "disabled", "all"):
            raise CatabolicError("active must be active, disabled, or all")
        clauses, values = [], []
        if tag is not None:
            clauses.append("a.tag_id=?")
            values.append(resolve_tag(self.store.db, tag))
        if active != "all":
            clauses.append("a.active=?")
            values.append(int(active == "active"))
        if source is not None:
            clauses.append("a.source=?")
            values.append(source)
        for subject, identifier in (("item", item), ("file", file)):
            if identifier is not None:
                clauses.append("a.subject_type=? AND a.subject_id=?")
                values.extend((subject, identifier))
        source_sql = """(SELECT id,'item' AS subject_type,item_id AS subject_id,tag_id,source,confidence,note,active,created_at,updated_at FROM item_tags
            UNION ALL SELECT id,'file',file_id,tag_id,source,confidence,note,active,created_at,updated_at FROM file_tags) a JOIN tags t ON t.id=a.tag_id"""
        result = self._page(
            "taggings",
            "a.*,t.name AS tag_name",
            source_sql,
            clauses,
            values,
            {"id": "a.id"},
            "id",
            False,
            limit,
            cursor,
            [tag, item, file, source, active],
        )
        for row in result["taggings"]:
            row["active"] = bool(row["active"])
        return result

    def mappings(
        self,
        *,
        catalog="global",
        item=None,
        file=None,
        active="all",
        search=None,
        sort="path",
        descending=False,
        limit=100,
        cursor=None,
    ):
        self._exists("catalogs", catalog)
        self._exists("items", item)
        self._exists("files", file)
        if active not in ("all", "active", "disabled"):
            raise CatabolicError("active must be all, active, or disabled")
        clauses, values = [], []
        for column, value in (
            ("catalog", catalog),
            ("item_id", item),
            ("file_id", file),
        ):
            if value is not None:
                clauses.append(f"m.{column}=?")
                values.append(value)
        if active != "all":
            clauses.append("m.active=?")
            values.append(int(active == "active"))
        if search is not None:
            clauses.append("contains_text(m.path, ?)")
            values.append(search)
        return self._page(
            "mappings",
            "m.*",
            "mappings m",
            clauses,
            values,
            {"id": "m.id", "path": "m.path", "catalog": "m.catalog"},
            sort,
            descending,
            limit,
            cursor,
            [catalog, item, file, active, search],
        )

    def associations(
        self,
        *,
        item=None,
        file=None,
        role=None,
        active="active",
        limit=100,
        cursor=None,
    ):
        self._exists("items", item)
        self._exists("files", file)
        if role is not None:
            vocabulary(role, ROLES, "file role")
        if active not in ("all", "active", "disabled"):
            raise CatabolicError("active must be all, active, or disabled")
        clauses, values = [], [self.profile, self.profile]
        for column, value in (("item_id", item), ("file_id", file), ("role", role)):
            if value is not None:
                clauses.append(f"a.{column}=?")
                values.append(value)
        if active != "all":
            clauses.append("a.active=?")
            values.append(int(active == "active"))
        result = self._page(
            "associations",
            "a.*,f.location,f.path AS source_relative_path,b.root AS source_root, CASE WHEN b.root IS NOT NULL THEN rtrim(b.root,'/') || '/' || f.path END AS source_path,o.size,coalesce(o.status,'unknown') AS status",
            "item_files a JOIN files f ON f.id=a.file_id LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? LEFT JOIN bindings b ON b.profile=? AND b.kind='source' AND b.owner=f.location",
            clauses,
            values,
            {"part": "coalesce(a.part,0)"},
            "part",
            False,
            limit,
            cursor,
            [item, file, role, active],
        )
        for row in result["associations"]:
            row["metadata"] = json.loads(row["metadata"])
        return result

    def relationships(
        self,
        *,
        item=None,
        direction="both",
        kind=None,
        active="active",
        limit=100,
        cursor=None,
    ):
        self._exists("items", item)
        if kind is not None:
            vocabulary(kind, RELATIONS, "relationship kind")
        if direction not in ("both", "outgoing", "incoming"):
            raise CatabolicError("direction must be both, outgoing, or incoming")
        if active not in ("all", "active", "disabled"):
            raise CatabolicError("active must be all, active, or disabled")
        clauses, values = [], []
        if item is not None:
            columns = (
                ("source_id", "target_id")
                if direction == "both"
                else ("source_id",)
                if direction == "outgoing"
                else ("target_id",)
            )
            clauses.append(
                "(" + " OR ".join(f"r.{column}=?" for column in columns) + ")"
            )
            values.extend([item] * len(columns))
        if kind is not None:
            clauses.append("r.kind=?")
            values.append(kind)
        if active != "all":
            clauses.append("r.active=?")
            values.append(int(active == "active"))
        result = self._page(
            "relationships",
            "r.*,s.kind AS source_kind,t.kind AS target_kind",
            "item_relationships r JOIN items s ON s.id=r.source_id JOIN items t ON t.id=r.target_id",
            clauses,
            values,
            {"position": "coalesce(r.position,0)"},
            "position",
            False,
            limit,
            cursor,
            [item, direction, kind, active],
        )
        for row in result["relationships"]:
            row["metadata"] = json.loads(row["metadata"])
        return result

    def item(
        self,
        item_id=None,
        *,
        identity=None,
        catalog=None,
        include_disabled=False,
        limit=100,
        cursor=None,
    ):
        if (item_id is None) == (identity is None):
            raise CatabolicError("supply either an item ID or --identity")
        if identity is not None:
            found = self.store.rows(
                "SELECT item_id FROM identities WHERE namespace=? AND value=?",
                identity_pair(identity),
            )
            if not found:
                raise CatabolicError(f"unknown identity: {identity}")
            item_id = found[0]["item_id"]
        self._exists("items", item_id)
        self._exists("catalogs", catalog)
        rows = self.store.rows("SELECT * FROM items WHERE id=?", (item_id,))
        self._decorate_items(rows)
        clauses, values = ["m.item_id=?"], [self.profile] * 4 + [item_id]
        if not include_disabled:
            clauses.append("m.active=1")
        if catalog is not None:
            clauses.append("m.catalog=?")
            values.append(catalog)
        result = self._page(
            "occurrences",
            "m.*, f.location, f.path AS source_relative_path, o.size, o.mtime_ns, coalesce(o.status,'unknown') AS status, o.scan_id, s.finished_at AS observed_at, sb.root AS source_root, ob.root AS output_root, ol.target AS recorded_link_target",
            """mappings m JOIN files f ON f.id=m.file_id
            LEFT JOIN observations o ON o.file_id=f.id AND o.profile=?
            LEFT JOIN scans s ON s.id=o.scan_id
            LEFT JOIN bindings sb ON sb.kind='source' AND sb.owner=f.location AND sb.profile=?
            LEFT JOIN bindings ob ON ob.kind='output' AND ob.owner=m.catalog AND ob.profile=?
            LEFT JOIN owned_links ol ON ol.catalog=m.catalog AND ol.path=m.path AND ol.profile=?""",
            clauses,
            values,
            {"id": "m.id"},
            "id",
            False,
            limit,
            cursor,
            [item_id, catalog, include_disabled],
        )
        for row in result["occurrences"]:
            row["source_path"] = (
                str(PurePosixPath(row["source_root"]) / row["source_relative_path"])
                if row["source_root"]
                else None
            )
            row["output_path"] = (
                str(PurePosixPath(row["output_root"]) / row["path"])
                if row["output_root"]
                else None
            )
        return {
            "item": rows[0],
            "profile": self.profile,
            **result,
            "file_associations": self.associations(
                item=item_id,
                active="all" if include_disabled else "active",
                limit=limit,
            ),
            "relationships": self.relationships(
                item=item_id,
                active="all" if include_disabled else "active",
                limit=limit,
            ),
        }
