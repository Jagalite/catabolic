# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Bounded, read-only GraphQL over the catalog's recorded state."""

import json
import re
import sqlite3
import time
from contextlib import nullcontext
from pathlib import PurePosixPath

from graphql import GraphQLError, build_schema, execute_sync, parse, validate
from graphql.language import FieldNode, FragmentSpreadNode, OperationDefinitionNode

from .app import Application
from .domain import CatabolicError
from .media import describe_types
from .migration import SCHEMA_VERSION
from .query import identity_pair
from .store import Store, encode

MAX_DOCUMENT_BYTES = 131072
MAX_RESULT_BYTES = 8 * 1024 * 1024
MAX_NODES = 10000
MAX_FIELDS = 20000
MAX_DEPTH = 20

SDL = '''
scalar JSON
"""A decimal string, preserving integers larger than JavaScript's safe range."""
scalar BigInt
enum Active { ACTIVE DISABLED ALL }
enum Direction { INCOMING OUTGOING BOTH }
enum Availability { PRESENT MISSING UNKNOWN }
enum ItemSort { ID TITLE YEAR }
enum FileSort { ID PATH SIZE MTIME }
enum MappingSort { ID PATH CATALOG }
type PageInfo { endCursor: String hasNextPage: Boolean! }
type Identity { namespace: String! value: String! }
type Tag { id: ID! name: String! description: String! aliases: [String!]! parentIds: [ID!]! }
type Tagging { id: ID! subjectType: String! subjectId: ID! tagId: ID! tagName: String! source: String! confidence: Float note: String! active: Boolean! createdAt: String! updatedAt: String! }
type TagPage { nodes: [Tag!]! pageInfo: PageInfo! }
type TaggingPage { nodes: [Tagging!]! pageInfo: PageInfo! }
type Item {
  id: ID! kind: String! title: String year: Int metadata: JSON! identities: [Identity!]!
  taggings(source: String, active: Active = ACTIVE, first: Int! = 100, after: String): TaggingPage
  associations(role: String, active: Active = ACTIVE, first: Int! = 100, after: String): AssociationPage
  relationships(direction: Direction = BOTH, kind: String, active: Active = ACTIVE, first: Int! = 100, after: String): RelationshipPage
  mappings(catalog: String = "global", allCatalogs: Boolean! = false, active: Active = ALL, first: Int! = 100, after: String): MappingPage
}
type File {
  facts: JSON!
  id: ID! location: String! path: String! sourcePath: String
  size: BigInt mtimeNs: BigInt status: Availability! scanId: ID observedAt: String
  taggings(source: String, active: Active = ACTIVE, first: Int! = 100, after: String): TaggingPage
  associations(role: String, active: Active = ACTIVE, first: Int! = 100, after: String): AssociationPage
  mappings(catalog: String = "global", allCatalogs: Boolean! = false, active: Active = ALL, first: Int! = 100, after: String): MappingPage
}
type Association {
  id: ID! fileId: ID! itemId: ID! role: String! part: Int metadata: JSON! origin: String! active: Boolean!
  sourcePath: String size: BigInt status: Availability! item: Item file: File
}
type Relationship {
  id: ID! sourceId: ID! targetId: ID! kind: String! position: Int metadata: JSON! active: Boolean!
  source: Item target: Item
}
type Mapping {
  id: ID! catalog: String! path: String! outputPath: String active: Boolean! itemId: ID! fileId: ID!
  item: Item file: File
}
type Catalog {
  id: ID! root: String linkMode: String!
  mappings(active: Active = ALL, first: Int! = 100, after: String): MappingPage
}
type ItemPage { nodes: [Item!]! pageInfo: PageInfo! }
type FilePage { nodes: [File!]! pageInfo: PageInfo! }
type AssociationPage { nodes: [Association!]! pageInfo: PageInfo! }
type RelationshipPage { nodes: [Relationship!]! pageInfo: PageInfo! }
type MappingPage { nodes: [Mapping!]! pageInfo: PageInfo! }
type CatalogPage { nodes: [Catalog!]! pageInfo: PageInfo! }
type EvidencePage { nodes: [JSON!]! pageInfo: PageInfo! }
type Query {
  job(id: ID!): JSON
  proposal(id: ID!): JSON
  jobs(state: String, file: ID, first: Int! = 100, after: String): EvidencePage
  proposals(state: String, file: ID, first: Int! = 100, after: String): EvidencePage
  contentSearch(text: String!, first: Int! = 100, after: Int! = 0): JSON!
  profile: String! schemaVersion: Int! mediaTypes: JSON!
  tags(search: String, namespace: String, parent: String, child: String, first: Int! = 100, after: String): TagPage
  taggings(tag: String, item: ID, file: ID, source: String, active: Active = ACTIVE, first: Int! = 100, after: String): TaggingPage
  items(search: String, kind: String, year: Int, identity: String, metadata: [String!], catalog: String, tags: [String!], anyTags: [String!], notTags: [String!], descendants: Boolean! = false, sort: ItemSort = ID, descending: Boolean! = false, first: Int! = 100, after: String): ItemPage
  item(id: ID, identity: String): Item
  files(search: String, location: String, status: Availability, unidentified: Boolean! = false, unmapped: Boolean! = false, catalog: String = "global", item: ID, kind: String, year: Int, identity: String, tags: [String!], anyTags: [String!], notTags: [String!], descendants: Boolean! = false, sort: FileSort = ID, descending: Boolean! = false, first: Int! = 100, after: String): FilePage
  file(id: ID!): File
  associations(item: ID, file: ID, role: String, active: Active = ACTIVE, first: Int! = 100, after: String): AssociationPage
  relationships(item: ID, direction: Direction = BOTH, kind: String, active: Active = ACTIVE, first: Int! = 100, after: String): RelationshipPage
  mappings(catalog: String = "global", allCatalogs: Boolean! = false, item: ID, file: ID, active: Active = ALL, search: String, sort: MappingSort = PATH, descending: Boolean! = false, first: Int! = 100, after: String): MappingPage
  catalogs(first: Int! = 100, after: String): CatalogPage
}
'''


def schema_description():
    return {
        "interface_version": 1,
        "schema": SDL,
        "limits": {
            "document_bytes": MAX_DOCUMENT_BYTES,
            "page_size": 1000,
            "returned_records": MAX_NODES,
            "resolved_fields": MAX_FIELDS,
            "depth": MAX_DEPTH,
            "result_bytes": MAX_RESULT_BYTES,
            "default_timeout_ms": 5000,
        },
    }


def check_document(document):
    """Bound expanded selections before validation, including fragment reuse."""
    fragments = {
        node.name.value: node
        for node in document.definitions
        if node.kind == "fragment_definition"
    }
    count = 0

    def visit(selection, depth, seen):
        nonlocal count
        if depth > MAX_DEPTH:
            raise GraphQLError(f"query depth exceeds {MAX_DEPTH}")
        if selection is None:
            return
        for node in selection.selections:
            count += 1
            if count > 2000:
                raise GraphQLError("query exceeds 2000 expanded selections")
            if isinstance(node, FragmentSpreadNode):
                key = node.name.value
                if key in seen:
                    raise GraphQLError("fragment cycle is not allowed")
                if key in fragments:
                    visit(fragments[key].selection_set, depth, seen | {key})
            else:
                visit(node.selection_set, depth + isinstance(node, FieldNode), seen)

    for node in document.definitions:
        if isinstance(node, OperationDefinitionNode):
            if node.operation.value != "query":
                raise GraphQLError("only read-only query operations are supported")
        visit(getattr(node, "selection_set", None), 0, set())


class QueryContext:
    def __init__(self, store, profile, deadline):
        self.store, self.profile, self.deadline = store, profile, deadline
        self.queries = Application(store, profile).queries
        self.nodes = self.fields = self.bytes = 0
        self.aborted = None
        self.cache = {}

    def check(self):
        if time.monotonic() > self.deadline:
            self.aborted = "query execution deadline exceeded"
        if self.aborted:
            raise GraphQLError(self.aborted, extensions={"code": "LIMIT_EXCEEDED"})

    def charge(self, value):
        if value is not None:
            self.bytes += len(encode(value).encode())
            if self.bytes > MAX_RESULT_BYTES:
                self.aborted = "query output budget exceeded"
                self.check()
        return value

    def count_field(self):
        self.fields += 1
        if self.fields > MAX_FIELDS:
            self.aborted = "query field budget exceeded"
        self.check()

    def introspection_middleware(self, original, source, info, /, **args):
        if info.parent_type.name.startswith("__") or info.field_name.startswith("__"):
            self.count_field()
            value = original(source, info, **args)
            if isinstance(value, (list, tuple)):
                self.records(len(value))
            elif isinstance(value, str):
                self.charge(value)
            return value
        return original(source, info, **args)

    def records(self, count):
        self.nodes += count
        if self.nodes > MAX_NODES:
            self.aborted = "query record budget exceeded"
            self.check()

    def lookup(self, entity, identifier):
        self.records(1)
        key = (entity, identifier)
        if key not in self.cache:
            if entity == "item":
                rows = self.store.rows("SELECT * FROM items WHERE id=?", (identifier,))
                self.queries._decorate_items(rows)
            else:
                rows = self.store.rows(
                    """SELECT f.*,o.size,o.mtime_ns,coalesce(o.status,'unknown') AS status,
                    o.scan_id,s.finished_at AS observed_at FROM files f
                    LEFT JOIN observations o ON o.file_id=f.id AND o.profile=?
                    LEFT JOIN scans s ON s.id=o.scan_id WHERE f.id=?""",
                    (self.profile, identifier),
                )
            self.cache[key] = rows[0] if rows else None
        return self.cache[key]

    def bound_path(self, kind, owner, path=None):
        key = ("binding", kind, owner)
        if key not in self.cache:
            rows = self.store.rows(
                "SELECT root FROM bindings WHERE profile=? AND kind=? AND owner=?",
                (self.profile, kind, owner),
            )
            self.cache[key] = rows[0]["root"] if rows else None
        root = self.cache[key]
        return str(PurePosixPath(root) / path) if root and path else root

    def page(self, field, kwargs):
        args = {
            re.sub(r"(?<!^)(?=[A-Z])", "_", key).lower(): value
            for key, value in kwargs.items()
        }
        args["limit"] = args.pop("first", 100)
        args["cursor"] = args.pop("after", None)
        for key in ("active", "direction", "sort", "status"):
            if isinstance(args.get(key), str):
                args[key] = args[key].lower()
        if args.pop("all_catalogs", False):
            args["catalog"] = None
        if args.get("metadata") is None:
            args.pop("metadata", None)
        # Bound each request before querying; count cached pages again because
        # aliases still duplicate the returned records and their nested work.
        limit = args["limit"]
        if type(limit) is not int or not 1 <= limit <= 1000:
            raise CatabolicError("first must be between 1 and 1000")
        key = (field, encode(args))
        if key not in self.cache:
            if field in ("jobs", "proposals"):
                table = "processing_jobs" if field == "jobs" else "proposals"
                conditions, values = ["profile=?"], [self.profile]
                for key, column in (("state", "state"), ("file", "file_id")):
                    if args.get(key) is not None:
                        conditions.append(column + "=?")
                        values.append(args[key])
                columns = (
                    "id,profile,file_id,operation,state,attempts,error,created_at,finished_at"
                    if field == "jobs"
                    else "id,profile,file_id,kind,state,source,created_at"
                )
                result = self.queries._page(
                    field,
                    columns,
                    table,
                    conditions,
                    values,
                    {"id": "id"},
                    "id",
                    False,
                    limit,
                    args["cursor"],
                    [args.get("state"), args.get("file")],
                )
                for row in result[field]:
                    for key in ("snapshot", "options", "payload", "evidence", "result"):
                        if row.get(key) is not None:
                            row[key] = json.loads(row[key])
                self.charge(result)
            elif field == "catalogs":
                result = self.queries._page(
                    "catalogs",
                    "c.*",
                    "catalogs c",
                    [],
                    [],
                    {"id": "c.id"},
                    "id",
                    False,
                    limit,
                    args["cursor"],
                    [],
                )
            else:
                result = getattr(self.queries, field)(**args)
            self.cache[key] = result
        result = self.cache[key]
        self.records(len(result[field]))
        return {
            "nodes": result[field],
            "pageInfo": {
                "endCursor": result["next_cursor"],
                "hasNextPage": result["next_cursor"] is not None,
            },
        }

    def resolve(self, source, info, /, **args):
        self.count_field()
        field, parent = info.field_name, info.parent_type.name
        try:
            if parent == "Query":
                if field in ("profile", "schemaVersion", "mediaTypes"):
                    return self.charge(
                        {
                            "profile": self.profile,
                            "schemaVersion": SCHEMA_VERSION,
                            "mediaTypes": describe_types(),
                        }[field]
                    )
                if field in ("job", "proposal"):
                    from .curation import Curation
                    from .processing import Processing

                    adapter = Processing if field == "job" else Curation
                    self.records(1)
                    return self.charge(
                        adapter(Application(self.store, self.profile)).get(args["id"])
                    )
                if field == "contentSearch":
                    from .processing import Processing

                    result = Processing(Application(self.store, self.profile)).search(
                        args["text"], limit=args["first"], after=args["after"]
                    )
                    self.records(len(result["hits"]))
                    return self.charge(result)
                if field == "item":
                    if (args.get("id") is None) == (args.get("identity") is None):
                        raise CatabolicError("supply exactly one item id or identity")
                    if args.get("identity") is not None:
                        rows = self.store.rows(
                            "SELECT item_id FROM identities WHERE namespace=? AND value=?",
                            identity_pair(args["identity"]),
                        )
                        return self.lookup("item", rows[0]["item_id"]) if rows else None
                    return self.lookup("item", args["id"])
                if field == "file":
                    return self.lookup("file", args["id"])
                return self.page(field, args)
            if field in ("associations", "relationships", "mappings", "taggings"):
                args[{"Item": "item", "File": "file", "Catalog": "catalog"}[parent]] = (
                    source["id"]
                )
                return self.page(field, args)
            if field in ("item", "file", "source", "target") and parent in (
                "Association",
                "Mapping",
                "Relationship",
            ):
                entity = "file" if field == "file" else "item"
                return self.lookup(entity, source[field + "_id"])
            if parent == "Item" and field in ("title", "year"):
                value = source["metadata"].get(field)
                if field == "title":
                    return self.charge(value if isinstance(value, str) else None)
                return value if type(value) is int and 1 <= value <= 9999 else None
            if parent == "File" and field == "facts":
                from .processing import Processing

                result = Processing(Application(self.store, self.profile)).facts(
                    source["id"]
                )["facts"]
                self.records(len(result))
                return self.charge(result)
            if parent == "File" and field == "sourcePath":
                return self.charge(
                    self.bound_path("source", source["location"], source["path"])
                )
            if parent == "Mapping" and field == "outputPath":
                return self.charge(
                    self.bound_path("output", source["catalog"], source["path"])
                )
            if parent == "Catalog" and field == "linkMode":
                return self.queries.store.rows(
                    "SELECT coalesce((SELECT mode FROM catalog_link_modes WHERE catalog=?),'symlink') AS mode",
                    (source["id"],),
                )[0]["mode"]
            if parent == "Catalog" and field == "root":
                return self.charge(self.bound_path("output", source["id"]))
            snake = re.sub(r"(?<!^)(?=[A-Z])", "_", field).lower()
            value = source.get(field, source.get(snake))
            if field == "status":
                value = value.upper()
            if field == "active":
                value = bool(value)
            if field not in ("nodes", "pageInfo", "identities"):
                self.charge(value)
            return value
        except (CatabolicError, sqlite3.Error) as exc:
            self.check()
            raise GraphQLError(str(exc), extensions={"code": "QUERY_ERROR"}) from exc


def budget_introspection(original, source, info, /, **args):
    return info.context.introspection_middleware(original, source, info, **args)


def execute_graphql(
    path,
    document,
    *,
    profile="default",
    variables=None,
    operation_name=None,
    timeout_ms=5000,
    _store=None,
):
    if type(timeout_ms) is not int or not 1 <= timeout_ms <= 60000:
        raise CatabolicError("timeout-ms must be between 1 and 60000")
    if not isinstance(document, str) or len(document.encode()) > MAX_DOCUMENT_BYTES:
        raise CatabolicError(
            f"query document must be at most {MAX_DOCUMENT_BYTES} bytes"
        )
    if isinstance(variables, str):
        if len(variables.encode()) > MAX_DOCUMENT_BYTES:
            raise CatabolicError("variables exceed input budget")
        variables = json.loads(variables)
    if variables is not None and not isinstance(variables, dict):
        raise CatabolicError("variables must be a JSON object")
    if variables is not None and len(encode(variables).encode()) > MAX_DOCUMENT_BYTES:
        raise CatabolicError("variables exceed input budget")
    deadline = time.monotonic() + timeout_ms / 1000
    try:
        ast = parse(document, max_tokens=4000)
        check_document(ast)
        schema = build_schema(SDL)
        schema.type_map["JSON"].serialize = lambda value: value
        schema.type_map["BigInt"].serialize = lambda value: str(value)
        errors = validate(schema, ast, max_errors=20)
        if errors:
            return {"data": None, "errors": [error.formatted for error in errors]}
        with nullcontext(_store) if _store is not None else Store(path) as store:
            previous_query_only = store.db.execute("PRAGMA query_only").fetchone()[0]
            try:
                store.db.execute("PRAGMA query_only=ON")
                context = QueryContext(store, profile, deadline)
                store.db.set_progress_handler(
                    lambda: int(time.monotonic() > deadline), 1000
                )
                context.check()
                result = execute_sync(
                    schema,
                    ast,
                    context_value=context,
                    variable_values=variables,
                    operation_name=operation_name,
                    field_resolver=context.resolve,
                    middleware=[budget_introspection],
                )
                context.check()
                response = result.formatted
                response["extensions"] = {
                    "interfaceVersion": 1,
                    "profile": profile,
                    "records": context.nodes,
                }
                if len(encode(response).encode()) > MAX_RESULT_BYTES:
                    raise GraphQLError(
                        "query output budget exceeded",
                        extensions={"code": "LIMIT_EXCEEDED"},
                    )
                return response
            finally:
                store.db.set_progress_handler(None, 0)
                store.db.execute(f"PRAGMA query_only={previous_query_only}")

    except GraphQLError as exc:
        return {"data": None, "errors": [exc.formatted]}
    except RecursionError:
        return {
            "data": None,
            "errors": [
                {
                    "message": "query nesting exceeds execution limits",
                    "extensions": {"code": "LIMIT_EXCEEDED"},
                }
            ],
        }
