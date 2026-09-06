"""Complete, read-only query selections for saved symlink layouts."""

import time

from .domain import CatabolicError, name
from .sql_query import MAX_SQL_BYTES, execute_sql, parameters
from .store import encode

MAX_SELECTION_IDS = 10000


def graphql_contract(query):
    """Accept one pageable catalog collection with an unambiguous ID contract."""
    from graphql import GraphQLError, parse
    from graphql.language import FieldNode, OperationDefinitionNode, VariableNode

    from .graphql_query import MAX_DOCUMENT_BYTES, check_document

    if not isinstance(query, str) or len(query.encode()) > MAX_DOCUMENT_BYTES:
        raise CatabolicError("GraphQL selection exceeds the document limit")
    try:
        ast = parse(query, max_tokens=4000)
        check_document(ast)
    except (GraphQLError, RecursionError) as exc:
        raise CatabolicError(f"invalid GraphQL selection: {exc}") from exc
    if len(ast.definitions) != 1 or not isinstance(
        ast.definitions[0], OperationDefinitionNode
    ):
        raise CatabolicError(
            "GraphQL selection requires one query operation without fragments"
        )
    operation = ast.definitions[0]
    roots = operation.selection_set.selections
    if (
        len(roots) != 1
        or not isinstance(roots[0], FieldNode)
        or roots[0].name.value not in ("items", "files", "associations")
    ):
        raise CatabolicError(
            "GraphQL selection must query one items, files, or associations collection"
        )
    root = roots[0]
    after = [arg.value for arg in root.arguments if arg.name.value == "after"]
    if (
        not after
        or not isinstance(after[0], VariableNode)
        or after[0].name.value != "after"
    ):
        raise CatabolicError(
            "GraphQL selection must declare $after: String and pass after: $after"
        )
    if root.directives or operation.directives:
        raise CatabolicError("GraphQL selection directives are not supported")

    def children(node, expected):
        fields = node.selection_set.selections if node.selection_set else ()
        if len(fields) != len(expected) or any(
            not isinstance(field, FieldNode) or field.alias or field.directives
            for field in fields
        ):
            raise CatabolicError(
                "GraphQL selection must return nodes { id } and pageInfo { hasNextPage endCursor }"
            )
        result = {field.name.value: field for field in fields}
        if set(result) != set(expected):
            raise CatabolicError(
                "GraphQL selection must return nodes { id } and pageInfo { hasNextPage endCursor }"
            )
        return result

    fields = children(root, ("nodes", "pageInfo"))
    children(fields["nodes"], ("id",))
    children(fields["pageInfo"], ("hasNextPage", "endCursor"))
    entity = {"items": "item_id", "files": "file_id", "associations": "association_id"}[
        root.name.value
    ]
    return entity, root.alias.value if root.alias else root.name.value


def validate_selection(selection):
    if not isinstance(selection, dict) or set(selection) - {
        "language",
        "query",
        "params",
        "variables",
        "profile",
        "timeout_ms",
    }:
        raise CatabolicError(
            "selection supports language, query, params/variables, profile, and timeout_ms"
        )
    language = selection.get("language")
    if language not in ("sql", "graphql"):
        raise CatabolicError("selection language must be sql or graphql")
    profile = selection.get("profile", "default")
    if not isinstance(profile, str):
        raise CatabolicError("selection profile must be a name")
    name(profile)
    timeout = selection.get("timeout_ms", 5000)
    if type(timeout) is not int or not 1 <= timeout <= 60000:
        raise CatabolicError("selection timeout_ms must be between 1 and 60000")
    query = selection.get("query")
    if (
        not isinstance(query, str)
        or not query.strip()
        or len(query.encode()) > MAX_SQL_BYTES
    ):
        raise CatabolicError("selection query must be nonempty and at most 1 MiB")
    if language == "sql":
        if "variables" in selection:
            raise CatabolicError("SQL selection uses params, not variables")
        parameters(encode(selection.get("params", {})))
    else:
        if "params" in selection:
            raise CatabolicError("GraphQL selection uses variables, not params")
        graphql_contract(query)
        variables = selection.get("variables", {})
        if not isinstance(variables, dict) or "after" in variables:
            raise CatabolicError(
                "GraphQL variables must be an object; after is reserved for complete pagination"
            )
    encode(selection)
    return selection


def select_ids(store, selection):
    """Evaluate in the caller's snapshot; errors and truncation never mean empty."""
    validate_selection(selection)
    profile = selection.get("profile", "default")
    timeout_ms = selection.get("timeout_ms", 5000)
    query = selection["query"]
    ids, rows_count, pages = set(), 0, 1
    if selection["language"] == "sql":
        result = execute_sql(
            store.path,
            query,
            profile=profile,
            params=encode(selection.get("params", {})),
            max_rows=MAX_SELECTION_IDS,
            timeout_ms=timeout_ms,
            _store=store,
        )
        if not result["complete"]:
            raise CatabolicError(
                "selection query was truncated; no mappings were changed"
            )
        if len(result["columns"]) != 1 or result["columns"][0] not in (
            "association_id",
            "item_id",
            "file_id",
        ):
            raise CatabolicError(
                "selection SQL must return exactly one column: association_id, item_id, or file_id"
            )
        entity = result["columns"][0]
        values = [row[0] for row in result["rows"]]
        rows_count = len(values)
        if any(not isinstance(value, str) or not value for value in values):
            raise CatabolicError("selection IDs must be nonempty strings")
        ids.update(values)
    else:
        from .graphql_query import execute_graphql

        entity, field = graphql_contract(query)
        cursor, visited, pages = None, set(), 0
        deadline = time.monotonic() + timeout_ms / 1000
        while True:
            remaining = int((deadline - time.monotonic()) * 1000)
            if remaining < 1:
                raise CatabolicError("selection GraphQL execution timed out")
            result = execute_graphql(
                store.path,
                query,
                profile=profile,
                variables={**selection.get("variables", {}), "after": cursor},
                timeout_ms=remaining,
                _store=store,
            )
            if result.get("errors"):
                raise CatabolicError(
                    "selection GraphQL failed: " + result["errors"][0]["message"]
                )
            page = (result.get("data") or {}).get(field)
            if (
                not isinstance(page, dict)
                or not isinstance(page.get("nodes"), list)
                or not isinstance(page.get("pageInfo"), dict)
            ):
                raise CatabolicError("selection GraphQL returned an incomplete page")
            values = [
                row.get("id") if isinstance(row, dict) else None
                for row in page["nodes"]
            ]
            if any(not isinstance(value, str) or not value for value in values):
                raise CatabolicError("selection IDs must be nonempty strings")
            rows_count += len(values)
            pages += 1
            if rows_count > MAX_SELECTION_IDS or pages > MAX_SELECTION_IDS:
                raise CatabolicError(
                    "selection GraphQL exceeds the complete selection limit"
                )
            ids.update(values)
            info = page["pageInfo"]
            if info.get("hasNextPage") is False:
                if info.get("endCursor") is not None:
                    raise CatabolicError(
                        "selection GraphQL returned inconsistent pagination"
                    )
                break
            cursor = info.get("endCursor")
            if (
                info.get("hasNextPage") is not True
                or not isinstance(cursor, str)
                or not cursor
                or cursor in visited
                or not values
            ):
                raise CatabolicError(
                    "selection GraphQL returned incomplete or repeated pagination"
                )
            visited.add(cursor)
    return (
        entity,
        ids,
        {
            "language": selection["language"],
            "profile": profile,
            "entity": entity,
            "selected_ids": len(ids),
            "returned_rows": rows_count,
            "pages": pages,
            "complete": True,
        },
    )


def selected_associations(store, selection):
    entity, identifiers, report = select_ids(store, selection)
    table = {"item_id": "items", "file_id": "files", "association_id": "item_files"}[
        entity
    ]
    found, rows = set(), {}
    values = sorted(identifiers)
    for start in range(0, len(values), 500):
        batch = values[start : start + 500]
        marks = ",".join("?" for _ in batch)
        found.update(
            row["id"]
            for row in store.rows(
                f"SELECT id FROM {table} WHERE id IN ({marks})", tuple(batch)
            )
        )
        column = "id" if entity == "association_id" else entity
        for row in store.rows(
            f"SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.active=1 AND a.{column} IN ({marks})",
            tuple(batch),
        ):
            rows[row["id"]] = row
        if len(rows) > 100000:
            raise CatabolicError(
                "query selection expands beyond 100000 active associations"
            )
    if found != identifiers:
        raise CatabolicError("selection returned unknown IDs; no mappings were changed")
    if entity == "association_id" and rows.keys() != identifiers:
        raise CatabolicError(
            "selection returned disabled associations; filter active=1"
        )
    report["selected_associations"] = len(rows)
    return [rows[key] for key in sorted(rows)], report
