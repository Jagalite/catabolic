# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Policy-aware logical requests; byte routes remain strictly concrete."""

import json
from typing import Annotated

from fastapi import Header, Query, Request, Response

from ..access import AccessError, authenticate
from ..app import Application
from ..fallback_policies import Policies
from ..fallback_resolution import Resolver
from ..rendition_requests import admit, status
from ..store import encode
from .models import DemandStatus, LogicalDemand, Model, Resolve


class PolicySummary(Model):
    id: str
    name: str
    revision: int
    tiers: list[str]


class ResolutionRecord(Model):
    id: str
    item_id: str
    file_id: str | None
    revision: str | None
    policy_id: str
    state: str
    tier: int | None
    generation: int
    published_generation: int


class ResolutionPage(Model):
    data: list[ResolutionRecord]
    page_complete: bool
    has_more: bool
    next_cursor: str | None


def require_policy(access, identifier, action="metadata:read"):
    if not any(
        g.get("operator") or identifier in g.get("fallback_policy_ids", [])
        for g in access.matching(action)
    ):
        raise AccessError("not_found", 404)
    try:
        return Policies(access.store, access.profile).get(identifier)
    except Exception as exc:
        from ..domain import CatabolicError

        if isinstance(exc, CatabolicError):
            raise AccessError("not_found", 404) from None
        raise


def logical_resolve(access, item_id, body, operation_id=None):
    action = "processing:request" if operation_id else "metadata:read"
    require_policy(access, body.fallback_policy_id, action)
    if not operation_id and not access.item(item_id):
        raise AccessError("not_found", 404)
    result = Resolver(
        Application(access.store, access.profile), access, operation_id
    ).resolve(
        [
            {
                "item_id": item_id,
                "role": body.role,
                "part": body.part,
                "variant": body.variant,
            }
        ],
        body.fallback_policy_id,
        catalog="http:" + access.principal,
    )
    current = authenticate(access.store, token_id=access.token["id"])
    require_policy(current, body.fallback_policy_id, action)
    # Do not expose query membership, private file IDs or rejected candidates.
    decision = result["decisions"][0]
    decision["reasons"] = [] if decision["file_id"] else ["no_eligible_content"]
    return decision


def install_routes(app, session, mutation):
    @app.get(
        "/v1/fallback-policies/{identifier}",
        operation_id="get_fallback_policy",
        response_model=PolicySummary,
    )
    def policy(identifier: str, request: Request):
        with session(request) as access:
            row = require_policy(access, identifier)
            return {
                "id": row["id"],
                "name": row["name"],
                "revision": row["revision"],
                "tiers": [t["name"] for t in row["definition"]["fallbacks"]],
            }

    @app.get(
        "/v1/projections/{identifier}/resolutions",
        operation_id="list_projection_resolutions",
        response_model=ResolutionPage,
    )
    def resolutions(
        identifier: str,
        request: Request,
        limit: Annotated[int, Query(ge=1, le=1000)] = 100,
        cursor: str | None = None,
    ):
        from ..access.catalog import Catalog
        from ..fallback_records import page

        with session(request) as access:
            catalog = Catalog(access)
            scope = ["fallback", identifier]
            result = page(
                access.store,
                access.profile,
                identifier,
                first=limit,
                after=catalog.after(scope, cursor),
                access=access,
            )
            more = result["pageInfo"]["hasNextPage"]
            return {
                "data": result["nodes"],
                "page_complete": True,
                "has_more": more,
                "next_cursor": catalog.cursor(scope, result["pageInfo"]["endCursor"])
                if more
                else None,
            }

    @app.post(
        "/v1/rendition-requests/logical",
        operation_id="create_logical_rendition_request",
        response_model=DemandStatus,
        responses={202: {"model": DemandStatus}},
    )
    def logical_demand(
        body: LogicalDemand,
        request: Request,
        response: Response,
        idempotency_key: Annotated[str, Header(alias="Idempotency-Key")],
    ):
        mutation()
        with session(request, write=True) as access:
            policy = require_policy(
                access, body.fallback_policy_id, "processing:request"
            )
            logical_body = encode(body.model_dump())
            old = access.store.rows(
                "SELECT * FROM api_logical_demands WHERE principal=? AND idempotency_key=?",
                (access.principal, idempotency_key),
            )
            if old:
                if old[0]["body"] != logical_body:
                    raise AccessError("idempotency_conflict", 409)
                result = status(access, old[0]["request_id"])
            else:
                workers = access.store.rows(
                    "SELECT * FROM api_worker_status WHERE profile=?", (access.profile,)
                )
                if not workers:
                    raise AccessError("worker_unavailable", 409)
                decision = logical_resolve(
                    access,
                    body.item_id,
                    Resolve(
                        fallback_policy_id=body.fallback_policy_id,
                        role=body.role,
                        part=body.part,
                        variant=body.variant,
                    ),
                    body.operation_id,
                )
                if not decision["file_id"]:
                    raise AccessError("no_eligible_processing_source", 409)
                concrete = {
                    "item_id": body.item_id,
                    "source_file_id": decision["file_id"],
                    "source_revision": decision["revision"],
                    "operation_id": body.operation_id,
                }
                with access.store.transaction() as db:
                    result = admit(
                        access,
                        concrete,
                        idempotency_key,
                        json.loads(workers[0]["capabilities"]),
                        _source_lineage_mode=policy["definition"][
                            "lineage_requirement"
                        ],
                    )
                    db.execute(
                        "INSERT INTO api_logical_demands VALUES (?,?,?,?)",
                        (
                            access.principal,
                            idempotency_key,
                            logical_body,
                            result["request_id"],
                        ),
                    )
            response.status_code = 200 if result["state"] == "ready" else 202
            response.headers["Location"] = result["status_url"]
            return result
