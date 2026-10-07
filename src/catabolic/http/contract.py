# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Versioned public transport contract, including non-JSON representations."""

from .models import Problem, RequestCallbackEvent, WebhookEvent

VERSION = "1.7.0"

# Descriptions are part of the published artifact, alongside stable operation IDs.
GROUPS = {
    "Identity": {"get_identity", "get_capabilities", "get_openapi"},
    "Queries": {"execute_sql", "execute_graphql", "run_saved_query"},
    "Rendition requests": {
        "create_rendition_request",
        "get_rendition_request",
        "cancel_rendition_request",
        "retry_rendition_request",
        "get_request_callback",
        "list_request_callback_deliveries",
        "retry_request_callback",
        "disable_request_callback",
    },
    "Content": {"create_content_ticket", "revoke_content_ticket", "resolve_item"},
    "Events": {
        "list_events",
        "create_webhook",
        "list_webhooks",
        "get_webhook",
        "list_webhook_deliveries",
        "disable_webhook",
        "retry_webhook",
    },
    "Playback": {
        "create_playback_session",
        "get_playback_session",
        "cancel_playback_session",
        "get_playback_playlist",
        "head_playback_playlist",
        "get_playback_segment",
        "head_playback_segment",
    },
    "Operator": {
        "manage_definition",
        "approve_http_operation",
        "list_http_operations",
        "get_http_operation",
        "disable_http_operation",
        "retry_http_operation",
        "list_http_mapping_deliveries",
        "preview_http_mapping",
        "apply_http_mapping",
    },
}
DESCRIPTIONS = {
    "approve_http_operation": "Pin a selected OpenAPI 3.1 JSON operation and explicitly chosen base URL. Requires operator webhooks:manage and sql:read. Credential references are environment names; no network requests occur during approval.",
    "list_http_operations": "Page through visible approved HTTP operation revisions using limit and after. Operations owned by other API principals are omitted.",
    "get_http_operation": "Inspect an approved HTTP operation revision, resolved request schemas and credential reference. Secret environment values are never returned.",
    "disable_http_operation": "Disable an approved operation and cancel its unfinished mapped deliveries. Already dispatched requests cannot be recalled.",
    "retry_http_operation": "Reauthorize and retry unfinished mapped deliveries with the current operator token. Live leases and disabled operations are rejected.",
    "list_http_mapping_deliveries": "Page through the operation's mapped delivery acknowledgments, attempts and sanitized errors. A completed delivery is not proof of remote processing completion.",
    "preview_http_mapping": "Evaluate a complete saved SQL rows query or bounded GraphQL connection and validate mapped requests without dispatch. Requires operator webhooks:manage and sql:read; normal query disclosure restrictions still apply.",
    "apply_http_mapping": "Reevaluate and compare the reviewed plan, then atomically queue changed requests. Enforces max_changes; unchanged keys do not enqueue, missing keys do not delete remote data, and newer payloads supersede older unsent payloads.",
    "get_request_callback": "Inspect this caller's rendition callback without disclosing its receiver URL. Requires current access to the request.",
    "list_request_callback_deliveries": "Page through this caller's callback deliveries using limit and after. Requires current request access.",
    "retry_request_callback": "Reauthorize the callback with the current token and retry unfinished deliveries without rerendering. Rechecks the approved receiver origin; live leases and disabled callbacks return 409.",
    "disable_request_callback": "Disable this caller's callback and cancel unfinished deliveries without cancelling its rendition request.",
    "create_webhook": "Register an HTTP/HTTPS destination for profile events. Requires operator webhooks:manage. URLs are stored privately and never returned. The API worker delivers queued notifications.",
    "list_webhooks": "Page through this operator principal's webhook registrations using limit and after. Receiver URLs are omitted.",
    "get_webhook": "Inspect an owned webhook registration without revealing its receiver URL.",
    "list_webhook_deliveries": "Page through delivery state, attempts and sanitized errors for an owned webhook using limit and after.",
    "disable_webhook": "Disable an owned webhook and cancel unfinished deliveries. Already dispatched requests cannot be recalled.",
    "retry_webhook": "Retry unfinished non-cancelled deliveries and bind delivery authorization to the current operator token.",
    "create_playback_session": "Request an approved H.264 playback profile. Reuse a current completed rendition or shared HLS encode. Requires an idempotency key; pending sessions return 202 with Location. Encoding runs in the supervised API worker.",
    "get_playback_session": "Principal-bound playback status, expiry and available segment duration. Clients poll during startup and play the growing playlist once available. Seeking is limited to produced segments.",
    "cancel_playback_session": "Cancel this caller's playback session. The worker retires encoding when no authorized unexpired sessions remain.",
    "get_playback_playlist": "Authenticated growing HLS EVENT playlist. References only completed segments; returns 409 before the first segment. Each referenced request requires Bearer authentication.",
    "head_playback_playlist": "Headers for the current authorized HLS playlist, without its body.",
    "get_playback_segment": "Retrieve one closed MPEG-TS segment named by this session's playlist. Temporary and unlisted files are never served. Whole-segment delivery only; ranges return 416.",
    "head_playback_segment": "Headers for one current authorized completed HLS segment, without its body.",
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
    "create_rendition_request": "Ensure an approved durable rendition exists. Requires an idempotency key; conflicting reuse returns 409. Returns 200 when ready, otherwise 202 with a status Location. Optional callback_url receives principal-bound terminal request events; its origin must be approved in the processing grant or by operator webhooks:manage.",
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
                "Playback",
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
        for model in (WebhookEvent, RequestCallbackEvent):
            schemas[model.__name__] = model.model_json_schema()

        def outbound(model, operation_id):
            return {
                "post": {
                    "operationId": operation_id,
                    "description": "Catabolic sends this JSON POST. Any 2xx acknowledges receipt. Delivery is at least once; deduplicate the event id. No Catabolic bearer credential is forwarded.",
                    "security": [],
                    "parameters": [
                        {
                            "name": "Idempotency-Key",
                            "in": "header",
                            "required": True,
                            "schema": {"type": "string"},
                            "description": "Stable event id, identical to the body id.",
                        }
                    ],
                    "requestBody": {
                        "required": True,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "$ref": "#/components/schemas/" + model.__name__
                                }
                            }
                        },
                    },
                    "responses": {
                        "2XX": {"description": "Event accepted."},
                        "default": {
                            "description": "Delivery is retried within the bounded retry policy. Redirects are not followed."
                        },
                    },
                }
            }

        schema["webhooks"] = {
            "catalogEvent": outbound(WebhookEvent, "receive_catalog_event")
        }
        schema["paths"]["/v1/rendition-requests"]["post"]["callbacks"] = {
            "renditionState": {
                "{$request.body#/callback_url}": outbound(
                    RequestCallbackEvent, "receive_rendition_callback"
                )
            }
        }

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
                    410,
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
                if "/playback-sessions/" in path and path.endswith("/content"):
                    # HLS segments are whole-file delivery, not range content or ticket URLs.
                    responses.pop("206", None)
                    responses.pop("304", None)
                    responses["200"]["content"] = (
                        {
                            "video/mp2t": {
                                "schema": {"type": "string", "format": "binary"}
                            }
                        }
                        if method == "get"
                        else {}
                    )
                    operation["security"] = [{"BearerAuth": []}]
                    operation["description"] = DESCRIPTIONS[identifier]
                    operation["tags"] = ["Playback"]
                    responses["200"]["description"] = (
                        "Completed HLS segment bytes (HEAD: headers only)."
                    )
                    responses["200"]["headers"] = {
                        name: {"schema": {"type": "string"}}
                        for name in ("Content-Length", "Content-Type", "Cache-Control")
                    }
                    operation["parameters"] = [
                        p
                        for p in operation.get("parameters", [])
                        if p["name"]
                        not in ("Range", "If-Match", "If-None-Match", "If-Range")
                    ]
                    if method == "head":
                        responses["200"].pop("content", None)
                if path.endswith("/index.m3u8"):
                    if method == "get":
                        responses["200"]["content"] = {
                            "application/vnd.apple.mpegurl": {
                                "schema": {"type": "string"}
                            }
                        }
                    else:
                        responses["200"].pop("content", None)
                if path in ("/v1/rendition-requests", "/v1/playback-sessions"):
                    for code in ("200", "202"):
                        responses[code]["headers"] = {
                            "Location": {"schema": {"type": "string"}}
                        }
        return schema

    app.openapi = contract
