# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Complete query results mapped to immutable requests in the notification queue."""

import json
import os
import time
from urllib.parse import quote, urlencode
from uuid import uuid4

from .consumer_adapters import ConsumerError, text
from .destination_mappings import digest, expression, fail
from .evaluation import EvaluationSession
from .openapi_operations import authorize, get, require_operator, validator
from .saved_queries import Queries
from .store import Store, encode

MAX_REQUESTS = 100
MAX_BODY = 65536


def validate_definition(store, profile, definition, *, access=None):
    if (
        not isinstance(definition, dict)
        or set(definition)
        - {
            "version",
            "id",
            "query",
            "operation",
            "key",
            "parameters",
            "body",
            "rows_path",
        }
        or not {"version", "id", "query", "operation", "key"} <= set(definition)
        or type(definition["version"]) is not int
        or definition["version"] != 3
    ):
        fail("invalid_http_definition")
    for field in ("id", "query", "operation"):
        text(definition[field], 255)
    if access:
        require_operator(access)
    operation = get(store, profile, definition["operation"], access=access)
    if not operation["enabled"]:
        fail("operation_disabled")
    query = Queries(store, profile).get(definition["query"])["definition"]
    if query["mode"] not in ("rows", "document"):
        fail("http_requires_rows_or_document")
    path = definition.get("rows_path")
    if query["mode"] == "document" and (
        not isinstance(path, list)
        or not 1 <= len(path) <= 16
        or any(not isinstance(p, str) or not p for p in path)
    ):
        fail("document_requires_connection_path")
    if query["mode"] == "rows" and path is not None:
        fail("rows_path_requires_document")
    parameters = definition.get("parameters", {})
    if (
        not isinstance(parameters, dict)
        or set(parameters) - {"path", "query"}
        or any(not isinstance(v, dict) for v in parameters.values())
    ):
        fail("invalid_parameters")
    saved = store.rows(
        "SELECT digest FROM http_mapping_definitions WHERE profile=? AND id=?",
        (profile, definition["id"]),
    )
    if saved and saved[0]["digest"] != digest(definition):
        fail("definition_changed_use_new_mapping_id")
    return operation, query


def mapped(spec, row, depth=0):
    if depth > 16:
        fail("body_depth")
    if isinstance(spec, dict):
        if "column" in spec or "constant" in spec:
            return expression(spec, row)
        return {k: mapped(v, row, depth + 1) for k, v in spec.items()}
    if isinstance(spec, list):
        return [mapped(v, row, depth + 1) for v in spec]
    fail("body_requires_expressions")


def complete_rows(value, query, definition):
    if value.get("errors") or value.get("complete") is False or value.get("truncated"):
        fail("incomplete_query")
    if query["mode"] == "rows":
        if value.get("complete") is not True or value.get("truncated") is not False:
            fail("incomplete_query")
        columns = value["columns"]
        if len(set(columns)) != len(columns):
            fail("duplicate_columns")
        return [dict(zip(columns, row, strict=True)) for row in value["rows"]]
    node = value.get("data")
    try:
        for field in definition["rows_path"]:
            node = node[field]
    except (KeyError, TypeError):
        fail("invalid_connection_path")
    if (
        not isinstance(node, dict)
        or not isinstance(node.get("nodes"), list)
        or not isinstance(node.get("pageInfo"), dict)
        or node["pageInfo"].get("hasNextPage") is not False
    ):
        fail("incomplete_connection")

    def check_nested(value):
        if isinstance(value, dict):
            if "nodes" in value and (
                not isinstance(value.get("pageInfo"), dict)
                or value["pageInfo"].get("hasNextPage") is not False
            ):
                fail("incomplete_nested_connection")
            for child in value.values():
                check_nested(child)
        elif isinstance(value, list):
            for child in value:
                check_nested(child)

    check_nested(node)
    if any(not isinstance(row, dict) for row in node["nodes"]):
        fail("invalid_document_rows")
    return node["nodes"]


def request_for(operation, definition, row):
    parameters = {
        where + ":" + name: expression(spec, row)
        for where, values in definition.get("parameters", {}).items()
        for name, spec in values.items()
    }
    approved = operation["parameters"]
    if set(parameters) - set(approved) or any(
        p["required"] and k not in parameters for k, p in approved.items()
    ):
        fail("parameter_mismatch")
    path, query = operation["path"], []
    for key, value in parameters.items():
        if not validator(approved[key]["schema"]).is_valid(value):
            fail("parameter_schema_mismatch")
        value = encode(value) if not isinstance(value, str) else value
        if key.startswith("path:"):
            if (
                value in ("", ".", "..")
                or any(c in value for c in "/\\%")
                or any(ord(c) < 32 or ord(c) == 127 for c in value)
            ):
                fail("invalid_path_value")
            path = path.replace("{" + key[5:] + "}", quote(value, safe=""))
        else:
            query.append((key[6:], value))
    url = operation["base_url"] + path
    if query:
        url += "?" + urlencode(sorted(query))
    if len(url) > 4096:
        fail("url_too_large")
    body = mapped(definition["body"], row) if "body" in definition else None
    if len(encode(body).encode()) > MAX_BODY:
        fail("body_too_large")
    schema = operation["body_schema"]
    if "body" in definition:
        if schema is None or not validator(schema).is_valid(body):
            fail("body_schema_mismatch")
    elif operation["body_required"]:
        fail("body_required")
    return {
        "url": url,
        "method": operation["method"],
        "body": body,
        "has_body": "body" in definition,
    }


def prepare(store, profile, definition, *, access=None, evaluated=None):
    operation, query = validate_definition(store, profile, definition, access=access)
    if evaluated is None:
        evaluated = Queries(store, profile).run(
            definition["query"],
            limit=1000,
            session=EvaluationSession(store, profile, access=access)
            if access
            else None,
        )
    rows = complete_rows(evaluated, query, definition)
    if len(rows) > 1000:
        fail("too_many_rows")
    grouped = {}
    for row in rows:
        key = text(expression(definition["key"], row), 255)
        request = request_for(operation["definition"], definition, row)
        if key in grouped and grouped[key] != request:
            fail("conflicting_key_rows")
        grouped[key] = request
    if len(grouped) > MAX_REQUESTS:
        fail("too_many_targets")
    ledger = {
        r["key"]: r
        for r in store.rows(
            "SELECT key,request_digest,event_id FROM http_mapping_ledger WHERE profile=? AND mapping_id=? AND key IN (SELECT value FROM json_each(?))",
            (profile, definition["id"], encode(sorted(grouped))),
        )
    }
    planned = []
    for key, request in sorted(grouped.items()):
        fingerprint = digest([operation["digest"], request])
        previous = ledger.get(key)
        planned.append(
            {
                "key": key,
                "request": request,
                "request_digest": fingerprint,
                "previous": previous,
                "changed": previous is None
                or previous["request_digest"] != fingerprint,
            }
        )
    identity = {
        "database_id": store.database_id,
        "profile": profile,
        "definition": definition,
        "operation_digest": operation["digest"],
        "rows": planned,
    }
    if len(encode(identity).encode()) > 4 * 1024 * 1024:
        fail("plan_too_large")
    return {
        **identity,
        "plan_id": digest(identity),
        "mapping": definition["id"],
        "complete": True,
        "applied": False,
        "queued": 0,
        "delivery_acknowledged": False,
    }


def enqueue(store, profile, plan, *, expected_plan, max_changes=100, token_id=None):
    if plan["plan_id"] != expected_plan:
        fail("stale_plan_preview_again")
    changed = [r for r in plan["rows"] if r["changed"]]
    if len(changed) > max_changes:
        fail("change_budget_exceeded")
    definition = plan["definition"]
    with store.transaction() as db:
        db.execute(
            "INSERT INTO http_mapping_definitions VALUES (?,?,?,?) ON CONFLICT DO NOTHING",
            (profile, definition["id"], encode(definition), digest(definition)),
        )
        for row in changed:
            if row["previous"]:
                previous = db.execute(
                    "SELECT state,lease_until FROM notification_deliveries WHERE event_id=?",
                    (row["previous"]["event_id"],),
                ).fetchone()
                if (
                    previous
                    and previous["state"] == "leased"
                    and previous["lease_until"] > time.time()
                ):
                    fail("delivery_in_progress")
                db.execute(
                    "UPDATE notification_deliveries SET state='cancelled',lease_token=NULL,lease_until=NULL WHERE event_id=? AND state!='complete'",
                    (row["previous"]["event_id"],),
                )
            identifier = "mapping:" + str(uuid4())
            db.execute(
                "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,'mapping_http','info',?)",
                (identifier, profile, definition["id"]),
            )
            db.execute(
                "INSERT INTO http_mapping_deliveries VALUES (?,?,?,?,?,?,?)",
                (
                    identifier,
                    definition["operation"],
                    definition["id"],
                    row["key"],
                    plan["plan_id"],
                    encode(row["request"]),
                    token_id,
                ),
            )
            db.execute(
                "INSERT INTO notification_deliveries(event_id,profile,destination_id) VALUES (?,?,?)",
                (identifier, profile, "operation:" + definition["operation"]),
            )
            db.execute(
                "INSERT INTO http_mapping_ledger VALUES (?,?,?,?,?) ON CONFLICT(profile,mapping_id,key) DO UPDATE SET request_digest=excluded.request_digest,event_id=excluded.event_id",
                (
                    profile,
                    definition["id"],
                    row["key"],
                    row["request_digest"],
                    identifier,
                ),
            )
    return {**plan, "applied": True, "queued": len(changed)}


def run(
    database, profile, definition, *, apply=False, expected_plan=None, max_changes=100
):
    if apply and not expected_plan:
        fail("apply_requires_expected_plan")
    with Store(database, writable=apply) as store:
        plan = prepare(store, profile, definition)
        return (
            enqueue(
                store,
                profile,
                plan,
                expected_plan=expected_plan,
                max_changes=max_changes,
            )
            if apply
            else plan
        )


def delivery_request(store, profile, event_id):
    rows = store.rows(
        "SELECT * FROM http_mapping_deliveries WHERE event_id=?", (event_id,)
    )
    if not rows:
        return None
    row = rows[0]
    operation = get(store, profile, row["operation_id"])
    authorize(store, operation, row["token_id"])
    request = json.loads(row["request"])
    headers = {}
    auth = operation["definition"]["auth"]
    if auth:
        secret = os.environ.get(auth["environment"])
        if not secret:
            raise ConsumerError("credential_unavailable")
        if len(secret) > 4096 or any(ord(c) < 32 or ord(c) > 126 for c in secret):
            raise ConsumerError("invalid_destination")
        headers[auth["header"]] = auth["prefix"] + secret
    if "responses" not in operation["definition"]:
        raise ConsumerError("invalid_destination")
    return {
        **request,
        "headers": headers,
        "responses": operation["definition"]["responses"],
    }


def history(store, profile, operation_id=None):
    return store.rows(
        """SELECT m.event_id AS id,m.mapping_id AS owner,m.key,m.plan_id,n.state,n.attempts,n.error,3 AS version
    FROM http_mapping_deliveries m JOIN consumer_events e ON e.id=m.event_id
    JOIN notification_deliveries n ON n.event_id=m.event_id
    WHERE e.profile=? AND (? IS NULL OR m.operation_id=?) ORDER BY e.rowid DESC LIMIT 1000""",
        (profile, operation_id, operation_id),
    )


def history_page(store, profile, operation_id, *, limit=100, after=""):
    rows = store.rows(
        """SELECT m.event_id AS id,m.mapping_id AS owner,m.key,m.plan_id,n.state,n.attempts,n.error,3 AS version
        FROM http_mapping_deliveries m JOIN consumer_events e ON e.id=m.event_id
        JOIN notification_deliveries n ON n.event_id=m.event_id
        WHERE e.profile=? AND m.operation_id=? AND m.event_id>? ORDER BY m.event_id LIMIT ?""",
        (profile, operation_id, after, limit + 1),
    )
    return {
        "deliveries": rows[:limit],
        "next_cursor": rows[limit - 1]["id"] if len(rows) > limit else None,
    }
