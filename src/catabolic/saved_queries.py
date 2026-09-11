# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Immutable query contracts shared by rules and projections, not an executor."""

import hashlib
import json
import time
from uuid import uuid4

from .curation import bounded_rows, page_limit
from .domain import CatabolicError, name
from .store import encode

ENTITIES = ("item_id", "file_id", "association_id", "component_id", "occurrence_id")


class Queries:
    def __init__(self, store, profile="default"):
        self.store, self.profile = store, profile

    def get(self, identifier):
        rows = self.store.rows(
            "SELECT * FROM saved_queries WHERE id=? AND profile=?",
            (identifier, self.profile),
        )
        if not rows:
            raise CatabolicError("unknown query revision in this profile")
        return {**rows[0], "definition": json.loads(rows[0]["definition"])}

    def list(self, limit=100, after=""):
        page_limit(limit)
        rows, more = bounded_rows(
            self.store,
            "SELECT id,profile,name,revision,digest,created_at,CASE WHEN length(definition)<=8192 THEN definition END AS definition,length(definition)>8192 AS definition_omitted FROM saved_queries WHERE profile=? AND id>? ORDER BY id LIMIT ?",
            (self.profile, after, limit + 1),
            limit,
        )
        return {
            "queries": [
                {
                    **r,
                    "definition": json.loads(r["definition"])
                    if r["definition"]
                    else None,
                }
                for r in rows
            ],
            "next_after": rows[-1]["id"] if more else None,
        }

    def validate(self, value):
        from .selection import validate_selection

        if not isinstance(value, dict) or set(value) - {
            "version",
            "mode",
            "selection",
            "combine",
            "queries",
            "entity",
            "max_ids",
            "timeout_ms",
        }:
            raise CatabolicError("invalid query definition fields")
        value = {"version": 1, "mode": "selection", **value}
        if (
            type(value["version"]) is not int
            or value["version"] != 1
            or value["mode"] not in ("selection", "rows", "document")
        ):
            raise CatabolicError(
                "query requires version 1 and mode selection, rows or document"
            )
        if value.get("entity") not in (None, *ENTITIES):
            raise CatabolicError(
                "query selection entity must be item_id, file_id, association_id, component_id or occurrence_id"
            )
        inline = value.get("selection")
        if isinstance(inline, dict):
            value.setdefault("max_ids", inline.get("max_ids", 10000))
            value.setdefault("timeout_ms", inline.get("timeout_ms", 5000))
        for key, default, maximum in [
            ("max_ids", 10000, 100000),
            ("timeout_ms", 5000, 60000),
        ]:
            if (
                type(value.get(key, default)) is not int
                or not 1 <= value.get(key, default) <= maximum
            ):
                raise CatabolicError("invalid query " + key)
        if "combine" in value:
            if (
                value["mode"] != "selection"
                or "selection" in value
                or value["combine"] not in ("union", "intersection", "difference")
            ):
                raise CatabolicError(
                    "combine requires a selection union, intersection or difference"
                )
            refs = value.get("queries")
            if (
                not isinstance(refs, list)
                or not 2 <= len(refs) <= 16
                or any(not isinstance(r, str) for r in refs)
            ):
                raise CatabolicError("composition requires 2..16 query revision IDs")
            for ref in refs:
                if self.get(ref)["definition"]["mode"] != "selection":
                    raise CatabolicError("only typed selections can be composed")
        else:
            if "queries" in value or not isinstance(value.get("selection"), dict):
                raise CatabolicError("query requires selection or composition")
            selection = {"profile": self.profile, **value["selection"]}
            if selection["profile"] != self.profile or "query_id" in selection:
                raise CatabolicError(
                    "query body must use its profile and inline SQL/GraphQL"
                )
            if value["mode"] == "selection":
                validate_selection(selection)
            else:
                from .sql_query import MAX_SQL_BYTES, parameters

                expected = "sql" if value["mode"] == "rows" else "graphql"
                if selection.get("language") != expected or set(selection) - {
                    "language",
                    "profile",
                    "query",
                    "params",
                    "variables",
                    "timeout_ms",
                }:
                    raise CatabolicError("rows uses SQL; document uses GraphQL")
                query = selection.get("query")
                if (
                    not isinstance(query, str)
                    or not query.strip()
                    or len(query.encode()) > MAX_SQL_BYTES
                ):
                    raise CatabolicError("query text must be nonempty and bounded")
                if expected == "sql":
                    parameters(encode(selection.get("params", {})))
                elif not isinstance(selection.get("variables", {}), dict):
                    raise CatabolicError("GraphQL variables must be an object")
            value["selection"] = selection
        if len(encode(value).encode()) > 1024 * 1024:
            raise CatabolicError("query definition exceeds 1 MiB")
        return value

    def put(self, query_name, value):
        name(query_name)
        value = self.validate(value)
        serialized = encode(value)
        digest = hashlib.sha256(serialized.encode()).hexdigest()
        with self.store.transaction() as db:
            old = db.execute(
                "SELECT id FROM saved_queries WHERE profile=? AND name=? AND digest=?",
                (self.profile, query_name, digest),
            ).fetchone()
            if old:
                identifier = old[0]
            else:
                revision = db.execute(
                    "SELECT coalesce(max(revision),0)+1 FROM saved_queries WHERE profile=? AND name=?",
                    (self.profile, query_name),
                ).fetchone()[0]
                identifier = str(uuid4())
                db.execute(
                    "INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES (?,?,?,?,?,?)",
                    (
                        identifier,
                        self.profile,
                        query_name,
                        revision,
                        serialized,
                        digest,
                    ),
                )
                db.executemany(
                    "INSERT INTO query_dependencies VALUES (?,?)",
                    [(identifier, ref) for ref in set(value.get("queries", []))],
                )
        return self.get(identifier)

    def select(self, identifier, *, _http=False, session=None):
        from .evaluation import EvaluationSession

        definition = self.get(identifier)["definition"]
        session = session or EvaluationSession(
            self.store,
            self.profile,
            http=_http,
            timeout_ms=definition.get("timeout_ms", 5000),
            max_ids=definition.get("max_ids", 10000),
        )
        session.check(self.store, self.profile)
        cached = identifier in session.context["cache"]
        result = self._select(identifier, session.context, ())
        if not cached:
            session.account([result[0], sorted(result[1]), result[2]])
        return result

    def _select(self, identifier, context, ancestors):
        from .selection import select_ids

        if identifier in ancestors or len(ancestors) >= 16:
            raise CatabolicError("query composition cycle or depth limit")
        if identifier in context["cache"]:
            return context["cache"][identifier]
        context["nodes"] += 1
        if context["nodes"] > 64:
            raise CatabolicError("query composition exceeds 64 definitions")
        remaining = int((context["deadline"] - time.monotonic()) * 1000)
        if remaining < 1:
            raise CatabolicError("query composition timed out")
        definition = self.get(identifier)["definition"]
        if definition["mode"] != "selection":
            raise CatabolicError(
                "rule/projection requires a complete ID selection, not rows/document"
            )
        if "combine" in definition:
            children = [
                self._select(ref, context, (*ancestors, identifier))
                for ref in definition["queries"]
            ]
            entity = children[0][0]
            if any(child[0] != entity for child in children):
                raise CatabolicError("composed queries must select the same entity")
            ids = set(children[0][1])
            for child in children[1:]:
                if definition["combine"] == "union":
                    ids.update(child[1])
                elif definition["combine"] == "intersection":
                    ids.intersection_update(child[1])
                else:
                    ids.difference_update(child[1])
            report = {
                "complete": True,
                "profile": self.profile,
                "entity": entity,
                "selected_ids": len(ids),
                "language": "composition",
                "components": [
                    {"query_id": c[2]["query_id"], "selected_ids": len(c[1])}
                    for c in children
                ],
            }
        else:
            selection = {
                **definition["selection"],
                "timeout_ms": min(
                    remaining,
                    definition.get("timeout_ms", 5000),
                    definition["selection"].get("timeout_ms", 5000),
                ),
            }
            entity, ids, report = select_ids(
                self.store,
                selection,
                _http=context["http"],
                access=context.get("access"),
            )
            context["count"] += len(ids)
        if context["count"] > context["maximum"] or len(ids) > definition.get(
            "max_ids", 10000
        ):
            raise CatabolicError("complete query selection exceeds its ID budget")
        if definition.get("entity") not in (None, entity):
            raise CatabolicError(
                "query returned a different entity from its declared contract"
            )
        result = entity, ids, {**report, "query_id": identifier}
        if time.monotonic() >= context["deadline"]:
            raise CatabolicError("query composition timed out")
        context["cache"][identifier] = result
        return result

    def run(self, identifier, limit=1000, *, session=None):
        page_limit(limit)
        query = self.get(identifier)
        value = query["definition"]
        timeout = value.get("timeout_ms", 5000)
        if session is not None:
            session.check(self.store, self.profile)
            timeout = min(
                timeout,
                max(1, int((session.context["deadline"] - time.monotonic()) * 1000)),
            )
        if value["mode"] == "selection":
            entity, ids, report = self.select(identifier, session=session)
            return {
                **report,
                "ids": sorted(ids)[:limit],
                "details_truncated": len(ids) > limit,
                "entity": entity,
                "selection_complete": True,
            }
        selection = value["selection"]
        access = session.access if session is not None else None
        http = session.context["http"] if session is not None else False
        if value["mode"] == "rows":
            from .sql_query import execute_sql

            if access is not None and not (
                access.operator("sql:read")
                or any(
                    identifier in grant.get("report_ids", [])
                    for grant in access.matching("metadata:read")
                )
            ):
                raise CatabolicError(
                    "SQL report requires operator or approved report authority"
                )
            result = execute_sql(
                self.store.path,
                selection["query"],
                profile=self.profile,
                params=encode(selection.get("params", {})),
                max_rows=limit,
                timeout_ms=timeout,
                _store=self.store,
                _http=http,
            )
        else:
            from .graphql_query import execute_graphql

            factory = None
            if access is not None:
                from .access.graphql import AuthorizedContext

                access.require("metadata:read")

                def factory(st, pr, deadline):
                    return AuthorizedContext(st, pr, deadline, access)
            elif http:
                raise CatabolicError(
                    "GraphQL HTTP evaluation requires authorization context"
                )
            result = execute_graphql(
                self.store.path,
                selection["query"],
                profile=self.profile,
                variables=selection.get("variables", {}),
                timeout_ms=timeout,
                _store=self.store,
                _context_factory=factory,
            )
        if session is not None:
            session.check(self.store, self.profile)
            session.account(result)
        return result
