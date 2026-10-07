# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Pinned, explicitly approved JSON HTTP operations; no remote schema loading."""

import json
import re
from uuid import uuid4

from .access import AccessError, authenticate
from .callback_urls import origin
from .consumer_adapters import ConsumerError, credential_ref, text
from .destination_mappings import digest
from .store import encode

METHODS = {"post", "put", "patch"}


def fail(code):
    raise ConsumerError("http_operation_" + code)


def validator(schema):
    try:
        from jsonschema import Draft202012Validator
    except ImportError:
        fail("install_openapi_extra")
    try:
        Draft202012Validator.check_schema(schema)
        return Draft202012Validator(schema)
    except Exception:
        fail("invalid_schema")


def resolve(document, value, *, trail=(), budget=None, depth=0):
    budget = [4096] if budget is None else budget
    budget[0] -= 1
    if budget[0] < 0 or depth > 32:
        fail("schema_budget")
    if isinstance(value, list):
        return [
            resolve(document, v, trail=trail, budget=budget, depth=depth + 1)
            for v in value
        ]
    if not isinstance(value, dict):
        return value
    if any(k in value for k in ("$dynamicRef", "$recursiveRef", "$id")):
        fail("unsupported_reference")
    if "pattern" in value or "patternProperties" in value:
        fail("unsupported_regex_schema")
    if "$schema" in value and value["$schema"] not in (
        "https://json-schema.org/draft/2020-12/schema",
        "https://spec.openapis.org/oas/3.1/dialect/base",
    ):
        fail("unsupported_schema_dialect")
    if "$ref" in value:
        ref = value["$ref"]
        if not isinstance(ref, str) or not ref.startswith("#/") or ref in trail:
            fail("external_or_recursive_reference")
        if set(value) - {"$ref", "description", "summary"}:
            fail("reference_siblings")
        target = document
        try:
            for part in ref[2:].split("/"):
                target = target[part.replace("~1", "/").replace("~0", "~")]
        except (KeyError, TypeError):
            fail("unknown_reference")
        return resolve(
            document, target, trail=(*trail, ref), budget=budget, depth=depth + 1
        )
    return {
        k: resolve(document, v, trail=trail, budget=budget, depth=depth + 1)
        for k, v in value.items()
    }


def compile_operation(document, operation_id, base_url, environment=None):
    if not isinstance(document, dict) or not str(
        document.get("openapi", "")
    ).startswith("3.1."):
        fail("requires_openapi_3_1")
    if document.get(
        "jsonSchemaDialect", "https://spec.openapis.org/oas/3.1/dialect/base"
    ) not in (
        "https://spec.openapis.org/oas/3.1/dialect/base",
        "https://json-schema.org/draft/2020-12/schema",
    ):
        fail("unsupported_schema_dialect")
    if len(encode(document).encode()) > 1024 * 1024:
        fail("spec_too_large")
    text(operation_id, 255)
    try:
        origin(base_url)
    except ValueError:
        fail("invalid_base_url")
    if any(c in base_url for c in "?#{}"):
        fail("invalid_base_url")
    matches = [
        (path, method, path_item, op)
        for path, path_item in document.get("paths", {}).items()
        if isinstance(path_item, dict)
        for method, op in path_item.items()
        if isinstance(op, dict) and op.get("operationId") == operation_id
    ]
    if len(matches) != 1:
        fail("operation_not_unique")
    path, method, path_item, raw = matches[0]
    if (
        method not in METHODS
        or not path.startswith("/")
        or path.startswith("//")
        or any(c in path for c in "?#\\")
        or any(ord(c) <= 32 for c in path)
    ):
        fail("unsupported_method_or_path")
    op = {
        **raw,
        "parameters": resolve(document, raw.get("parameters", [])),
        "requestBody": resolve(document, raw.get("requestBody", {})),
    }
    parameters = {}
    for parameter in resolve(document, path_item.get("parameters", [])) + op.get(
        "parameters", []
    ):
        where, name = parameter.get("in"), parameter.get("name")
        if (
            where not in ("path", "query")
            or not isinstance(name, str)
            or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.-]*", name)
        ):
            fail("unsupported_parameter")
        schema = parameter.get("schema")
        if not isinstance(schema, dict) or schema.get("type") not in (
            "string",
            "integer",
            "number",
            "boolean",
        ):
            fail("requires_scalar_parameter")
        if parameter.get("style", "simple" if where == "path" else "form") != (
            "simple" if where == "path" else "form"
        ) or parameter.get("allowReserved"):
            fail("unsupported_parameter_encoding")
        validator(schema)
        parameters[where + ":" + name] = {
            "schema": schema,
            "required": where == "path" or parameter.get("required", False),
        }
    names = re.findall(r"\{([^{}]+)\}", path)
    if (
        set(names) != {k[5:] for k in parameters if k.startswith("path:")}
        or "{" in re.sub(r"\{[^{}]+\}", "", path)
        or "}" in re.sub(r"\{[^{}]+\}", "", path)
    ):
        fail("path_parameters_mismatch")
    request = op.get("requestBody", {})
    if request:
        content = request.get("content", {})
        if "application/json" not in content:
            fail("requires_json_body")
        schema = content["application/json"].get("schema", {})
        validator(schema)
    else:
        schema = None
    security = op.get("security", document.get("security", []))
    auth = None
    if security:
        if (
            len(security) != 1
            or not isinstance(security[0], dict)
            or len(security[0]) != 1
        ):
            fail("requires_single_security_scheme")
        name, scopes = next(iter(security[0].items()))
        if scopes:
            fail("unsupported_security_scopes")
        scheme = resolve(
            document,
            document.get("components", {}).get("securitySchemes", {}).get(name, {}),
        )
        if (
            scheme.get("type") == "http"
            and scheme.get("scheme", "").lower() == "bearer"
        ):
            auth = {"header": "Authorization", "prefix": "Bearer "}
        elif scheme.get("type") == "apiKey" and scheme.get("in") == "header":
            header = scheme.get("name", "")
            if not re.fullmatch(
                r"[A-Za-z][A-Za-z0-9-]{0,99}", header
            ) or header.lower() in {
                "host",
                "content-length",
                "content-type",
                "transfer-encoding",
                "connection",
                "idempotency-key",
                "user-agent",
            }:
                fail("unsafe_auth_header")
            auth = {"header": header, "prefix": ""}
        else:
            fail("unsupported_security_scheme")
        credential_ref(environment)
        auth["environment"] = environment
    elif environment is not None:
        fail("credential_without_security_scheme")
    responses = {}
    for status, response in raw.get("responses", {}).items():
        if (
            status != "default"
            and status != "2XX"
            and not re.fullmatch(r"2[0-9]{2}", status)
        ):
            continue
        response = resolve(document, response)
        content = response.get("content", {})
        if content:
            if set(content) != {"application/json"}:
                fail("requires_json_response")
            response_schema = content["application/json"].get("schema", {})
            validator(response_schema)
            responses[status] = {"schema": response_schema, "has_body": True}
        else:
            responses[status] = {"has_body": False}
    if not responses:
        fail("requires_success_response")
    if len(encode(responses).encode()) > 65536:
        fail("response_schema_budget")
    return {
        "operation_id": operation_id,
        "spec_digest": digest(document),
        "base_url": base_url.rstrip("/"),
        "method": method.upper(),
        "path": path,
        "parameters": parameters,
        "body_schema": schema,
        "body_required": request.get("required", False),
        "auth": auth,
        "responses": responses,
    }


def require_operator(access):
    if not access.operator("webhooks:manage") or not access.operator("sql:read"):
        raise AccessError()


def put(
    store,
    profile,
    name,
    document,
    operation_id,
    base_url,
    environment=None,
    *,
    access=None,
):
    if access:
        require_operator(access)
    text(name, 255)
    try:
        definition = compile_operation(document, operation_id, base_url, environment)
    except (ValueError, TypeError, KeyError, AttributeError, RecursionError):
        fail("invalid_specification")
    fingerprint = digest(definition)
    prior = store.rows(
        "SELECT * FROM http_operations WHERE profile=? AND name=? AND digest=?",
        (profile, name, fingerprint),
    )
    if prior:
        get(store, profile, prior[0]["id"], access=access)
        return present(prior[0])
    identifier = str(uuid4())
    with store.transaction() as db:
        version = db.execute(
            "SELECT coalesce(max(revision),0)+1 FROM http_operations WHERE profile=? AND name=?",
            (profile, name),
        ).fetchone()[0]
        db.execute(
            "INSERT INTO http_operations VALUES (?,?,?,?,?,?,1,?,?)",
            (
                identifier,
                profile,
                name,
                version,
                encode(definition),
                fingerprint,
                access.principal if access else None,
                access.token["id"] if access else None,
            ),
        )
        db.execute(
            "INSERT INTO notification_destinations(profile,id,credential_env,subscriptions,tags,min_severity) VALUES (?,?,?,'[]','[]','info')",
            (profile, "operation:" + identifier, "CATABOLIC_HTTP_OPERATION"),
        )
    return present(
        store.rows("SELECT * FROM http_operations WHERE id=?", (identifier,))[0]
    )


def present(row):
    value = (
        json.loads(row["definition"])
        if isinstance(row["definition"], str)
        else row["definition"]
    )
    return {
        "id": row["id"],
        "name": row["name"],
        "revision": row["revision"],
        "enabled": bool(row["enabled"]),
        "operation_id": value["operation_id"],
        "spec_digest": value["spec_digest"],
        "method": value["method"],
    }


def get(store, profile, identifier, *, access=None):
    rows = store.rows(
        "SELECT * FROM http_operations WHERE profile=? AND id=?", (profile, identifier)
    )
    if not rows or (access and rows[0]["principal"] not in (None, access.principal)):
        raise AccessError("not_found", 404)
    return {**rows[0], "definition": json.loads(rows[0]["definition"])}


def disable(store, profile, identifier, *, access=None):
    if access:
        require_operator(access)
    get(store, profile, identifier, access=access)
    with store.transaction() as db:
        db.execute("UPDATE http_operations SET enabled=0 WHERE id=?", (identifier,))
        db.execute(
            "UPDATE notification_destinations SET enabled=0,revision=revision+1 WHERE profile=? AND id=?",
            (profile, "operation:" + identifier),
        )
        db.execute(
            "UPDATE notification_deliveries SET state='cancelled',lease_token=NULL,lease_until=NULL WHERE profile=? AND destination_id=? AND state!='complete'",
            (profile, "operation:" + identifier),
        )
    return {"id": identifier, "enabled": False}


def authorize(store, operation, token_id=None):
    if not operation["enabled"]:
        raise ConsumerError("invalid_destination")
    for identifier in {operation["token_id"], token_id} - {None}:
        access = authenticate(store, token_id=identifier)
        require_operator(access)


def retry(store, profile, identifier, *, access=None):
    from .notifications import retry_delivery

    if access:
        require_operator(access)
    operation = get(store, profile, identifier, access=access)
    if not operation["enabled"]:
        fail("disabled")
    token = access.token["id"] if access else None
    with store.transaction() as db:
        retry_delivery(db, profile, "operation:" + identifier)
        db.execute(
            "UPDATE http_operations SET token_id=? WHERE id=?", (token, identifier)
        )
        db.execute(
            "UPDATE http_mapping_deliveries SET token_id=? WHERE operation_id=? AND event_id IN (SELECT event_id FROM notification_deliveries WHERE profile=? AND state NOT IN ('complete','cancelled'))",
            (token, identifier, profile),
        )
    return {"id": identifier, "retry_scheduled": True}


def listing(store, profile, *, access=None, limit=100, after=""):
    if access:
        require_operator(access)
    rows = store.rows(
        "SELECT * FROM http_operations WHERE profile=? AND id>? AND (? IS NULL OR principal IS NULL OR principal=?) ORDER BY id LIMIT ?",
        (
            profile,
            after,
            access.principal if access else None,
            access.principal if access else None,
            limit + 1,
        ),
    )
    return {
        "operations": [present(row) for row in rows[:limit]],
        "next_cursor": rows[limit - 1]["id"] if len(rows) > limit else None,
    }
