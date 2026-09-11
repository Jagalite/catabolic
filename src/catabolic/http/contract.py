# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Versioned public transport contract, including non-JSON representations."""

from .models import Problem

VERSION = "1.2.0"

# Descriptions are part of the published artifact, alongside stable operation IDs.
GROUPS = {
    "Identity": {"get_identity", "get_capabilities", "get_openapi"},
    "Queries": {"execute_sql", "execute_graphql", "run_saved_query"},
    "Rendition requests": {
        "create_rendition_request",
        "get_rendition_request",
        "cancel_rendition_request",
        "retry_rendition_request",
    },
    "Content": {"create_content_ticket", "revoke_content_ticket", "resolve_item"},
    "Events": {"list_events"},
    "Operator": {"manage_definition"},
}
DESCRIPTIONS = {
    "get_identity": "Effective principal, profile, permitted actions and credential expiry in Unix seconds.",
    "get_capabilities": "Caller-visible operations, configured limits and local worker readiness. Readiness is advisory; admission rechecks prerequisites.",
    "get_openapi": "Versioned OpenAPI contract. Contains no deployment-specific catalog data; authentication is required on this endpoint.",
    "execute_sql": "Operator-only bounded read-only SQL. Security tables are excluded. The columns/rows result preserves the core SQL completeness contract.",
    "execute_graphql": "Authorized read-only GraphQL. Data depends on the submitted document; execution errors use the standard GraphQL errors envelope.",
    "run_saved_query": "Execute an approved pinned query revision. Returns a selection page, SQL rows or a GraphQL document according to the saved definition.",
    "create_item_snapshot": "Create a principal-bound ID snapshot expiring after 15 minutes. Snapshot pages recheck current authorization.",
    "get_snapshot_page": "Browse a snapshot with a principal-bound cursor. Revoked resources are omitted; expired snapshots are not found.",
    "resolve_item": "Resolve an eligible existing revision and authenticated content path without scheduling processing.",
    "create_content_ticket": "Issue a revocable bearer ticket for one file revision. Its URL permits repeated GET/HEAD requests until expiry or the transfer budget is exhausted. Keep this URL secret.",
    "revoke_content_ticket": "Revoke an owned ticket. Already delivered bytes cannot be recalled.",
    "get_fallback_policy": "Inspect an approved immutable policy revision. Policy approval does not grant access to its candidates.",
    "list_projection_resolutions": "Authorized logical membership and selected revisions, with separate admitted and verified publication generations.",
    "create_logical_rendition_request": "Resolve an approved fallback policy before admitting a concrete source revision. Requires an idempotency key; retries retain the original chosen source.",
    "create_rendition_request": "Ensure an approved durable rendition exists. Requires an idempotency key; conflicting reuse returns 409. Returns 200 when ready, otherwise 202 with a status Location.",
    "get_rendition_request": "Current authorized caller demand, blockers and result. Unknown progress is null. Shared processing does not disclose other callers.",
    "cancel_rendition_request": "Cancel this caller's demand while preserving work still required by another request or persistent rule.",
    "retry_rendition_request": "Revalidate current source, permissions and operation before retrying failed or cancelled demand.",
    "list_events": "Poll authorized durable events, or request SSE with follow=true. Use the cursor or Last-Event-ID for replay; expired history requires resynchronization.",
    "manage_definition": "Operator-only query/rule/projection definition preview or application. Applying requires the reviewed plan digest; stale plans return 409.",
}


def install_contract(app):
    default_openapi = app.openapi

    def contract():
        schema = default_openapi()
        schema["info"]["description"] = (
            "Discover authorized media, request approved durable renditions, and retrieve exact revisions using Catabolic's shared catalog and workers."
        )
        schema["info"]["license"] = {"name": "MIT", "identifier": "MIT"}
        schema["tags"] = [
            {"name": name}
            for name in (
                "Identity",
                "Queries",
                "Catalog",
                "Content",
                "Rendition requests",
                "Events",
                "Operator",
            )
        ]
        schemas = schema.setdefault("components", {}).setdefault("schemas", {})
        schemas["Problem"] = Problem.model_json_schema()
        schema["components"]["securitySchemes"] = {
            "BearerAuth": {"type": "http", "scheme": "bearer"},
            "ContentTicket": {"type": "apiKey", "in": "query", "name": "ticket"},
        }
        schema["security"] = [{"BearerAuth": []}]
        for path, methods in schema["paths"].items():
            for method, operation in methods.items():
                if method not in ("get", "head", "post"):
                    continue
                identifier = operation["operationId"]
                operation["summary"] = identifier.replace("_", " ").capitalize()
                operation["description"] = DESCRIPTIONS.get(
                    identifier,
                    "Return only authorized catalog records. Cursor pages are bounded and recheck current access; exact revision references do not grant content access.",
                )
                operation["tags"] = [
                    next(
                        (name for name, ids in GROUPS.items() if identifier in ids),
                        "Catalog",
                    )
                ]
                responses = operation.setdefault("responses", {})
                for code in (
                    400,
                    401,
                    403,
                    404,
                    409,
                    412,
                    413,
                    416,
                    422,
                    429,
                    500,
                    503,
                ):
                    responses[str(code)] = {
                        "description": "Problem Details; inspect code and remediation. HEAD responses have no body.",
                        "headers": {
                            "X-Request-ID": {"schema": {"type": "string"}},
                            **(
                                {"Retry-After": {"schema": {"type": "string"}}}
                                if code in (429, 503)
                                else {}
                            ),
                        },
                        **(
                            {
                                "content": {
                                    "application/problem+json": {
                                        "schema": {
                                            "$ref": "#/components/schemas/Problem"
                                        }
                                    }
                                }
                            }
                            if method != "head"
                            else {}
                        ),
                    }
                if path.endswith("/content"):
                    operation["tags"] = ["Content"]
                    operation["description"] = (
                        "Serve catalog-backed exact revision bytes with a bearer credential or narrow content ticket. Single ranges only; HEAD ignores Range. ETags are weak revision validators; in-place source mutation aborts the remaining transfer. Successful Content-Type reflects the media."
                    )
                    operation["security"] = [{"BearerAuth": []}, {"ContentTicket": []}]
                    headers = {
                        name: {"schema": {"type": "string"}}
                        for name in (
                            "Content-Length",
                            "Content-Type",
                            "Content-Range",
                            "ETag",
                            "Accept-Ranges",
                            "Cache-Control",
                            "Content-Disposition",
                        )
                    }
                    responses["200"] = {
                        "description": "Exact revision bytes (HEAD: headers only).",
                        "headers": headers,
                    }
                    if method == "get":
                        responses["200"]["content"] = {
                            "*/*": {"schema": {"type": "string", "format": "binary"}}
                        }
                        responses["206"] = {
                            **responses["200"],
                            "description": "Single byte range; Content-Range identifies the returned interval.",
                        }
                    responses["304"] = {
                        "description": "Not modified; no response body.",
                        "headers": headers,
                    }
                    parameters = operation.setdefault("parameters", [])
                    for name in ("Range", "If-Match", "If-None-Match", "If-Range"):
                        if not any(p["name"] == name for p in parameters):
                            parameters.append(
                                {
                                    "name": name,
                                    "in": "header",
                                    "required": False,
                                    "schema": {"type": "string"},
                                }
                            )
                else:
                    operation.setdefault("tags", ["Catalog"])
                if path == "/v1/events":
                    responses["200"]["content"]["text/event-stream"] = {
                        "schema": {"type": "string"},
                        "description": "follow=true: catalog events contain RequestChanged arrays; id is the replay cursor. resync-required carries a code.",
                    }
                    parameters = operation.setdefault("parameters", [])
                    if not any(p["name"] == "Last-Event-ID" for p in parameters):
                        parameters.append(
                            {
                                "name": "Last-Event-ID",
                                "in": "header",
                                "required": False,
                                "schema": {"type": "string"},
                            }
                        )
                if path == "/v1/rendition-requests":
                    for code in ("200", "202"):
                        responses[code]["headers"] = {
                            "Location": {"schema": {"type": "string"}}
                        }
        return schema

    app.openapi = contract
