# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Operator query mappings use the same admission and durable delivery services."""

from typing import Annotated

from fastapi import Query, Request

from .. import http_mappings, openapi_operations
from ..access import AccessError
from . import models


def install_routes(app, session, mutation):
    @app.get(
        "/v1/http-operations",
        operation_id="list_http_operations",
        response_model=models.HTTPOperationPage,
    )
    def listing(
        request: Request,
        limit: Annotated[int, Query(ge=1, le=1000)] = 100,
        after: str = "",
    ):
        with session(request) as access:
            return openapi_operations.listing(
                access.store, access.profile, access=access, limit=limit, after=after
            )

    @app.post(
        "/v1/http-operations",
        operation_id="approve_http_operation",
        response_model=models.HTTPOperationStatus,
        status_code=201,
    )
    def approve(body: models.HTTPOperationCreate, request: Request):
        mutation()
        with session(request, write=True) as access:
            return openapi_operations.put(
                access.store,
                access.profile,
                body.name,
                body.spec,
                body.operation_id,
                body.base_url,
                body.credential_env,
                access=access,
            )

    @app.get(
        "/v1/http-operations/{identifier}",
        operation_id="get_http_operation",
        response_model=models.HTTPOperationDetail,
    )
    def inspect(identifier: str, request: Request):
        with session(request) as access:
            openapi_operations.require_operator(access)
            value = openapi_operations.get(
                access.store, access.profile, identifier, access=access
            )
            return {
                **openapi_operations.present(value),
                "definition": value["definition"],
            }

    @app.post(
        "/v1/http-operations/{identifier}/disable",
        operation_id="disable_http_operation",
        response_model=models.HTTPOperationControl,
        response_model_exclude_unset=True,
    )
    def disable(identifier: str, request: Request):
        mutation()
        with session(request, write=True) as access:
            return openapi_operations.disable(
                access.store, access.profile, identifier, access=access
            )

    @app.post(
        "/v1/http-operations/{identifier}/retry",
        operation_id="retry_http_operation",
        response_model=models.HTTPOperationControl,
        response_model_exclude_unset=True,
    )
    def retry(identifier: str, request: Request):
        mutation()
        with session(request, write=True) as access:
            return openapi_operations.retry(
                access.store, access.profile, identifier, access=access
            )

    @app.get(
        "/v1/http-operations/{identifier}/deliveries",
        operation_id="list_http_mapping_deliveries",
        response_model=models.HTTPMappingHistory,
    )
    def history(
        identifier: str,
        request: Request,
        limit: Annotated[int, Query(ge=1, le=1000)] = 100,
        after: str = "",
    ):
        with session(request) as access:
            openapi_operations.require_operator(access)
            openapi_operations.get(
                access.store, access.profile, identifier, access=access
            )
            return http_mappings.history_page(
                access.store, access.profile, identifier, limit=limit, after=after
            )

    @app.post(
        "/v1/http-mappings/preview",
        operation_id="preview_http_mapping",
        response_model=models.HTTPMappingPlan,
    )
    def preview(body: models.HTTPMappingInput, request: Request):
        with session(request) as access:
            return http_mappings.prepare(
                access.store, access.profile, body.definition, access=access
            )

    @app.post(
        "/v1/http-mappings/apply",
        operation_id="apply_http_mapping",
        response_model=models.HTTPMappingPlan,
    )
    def apply(body: models.HTTPMappingInput, request: Request):
        mutation()
        if body.expected_plan is None:
            raise AccessError("expected_plan_required", 400)
        with session(request, write=True) as access:
            plan = http_mappings.prepare(
                access.store, access.profile, body.definition, access=access
            )
            return http_mappings.enqueue(
                access.store,
                access.profile,
                plan,
                expected_plan=body.expected_plan,
                max_changes=body.max_changes,
                token_id=access.token["id"],
            )
