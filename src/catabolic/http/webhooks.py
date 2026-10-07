# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Operator HTTP management of durable webhook subscriptions."""

from typing import Annotated

from fastapi import Query, Request

from .. import webhooks
from . import models


def install_routes(app, session, mutation):
    @app.post(
        "/v1/webhooks",
        operation_id="create_webhook",
        response_model=models.WebhookStatus,
        status_code=201,
    )
    def create(body: models.WebhookCreate, request: Request):
        mutation()
        with session(request, write=True) as access:
            return webhooks.register(access, body.url, body.events)

    @app.get(
        "/v1/webhooks", operation_id="list_webhooks", response_model=models.WebhookPage
    )
    def listing(
        request: Request,
        limit: Annotated[int, Query(ge=1, le=1000)] = 100,
        after: str = "",
    ):
        with session(request) as access:
            return webhooks.listing(access, limit=limit, after=after)

    @app.get(
        "/v1/webhooks/{identifier}",
        operation_id="get_webhook",
        response_model=models.WebhookStatus,
    )
    def inspect(identifier: str, request: Request):
        with session(request) as access:
            return webhooks.inspect(access, identifier)

    @app.get(
        "/v1/webhooks/{identifier}/deliveries",
        operation_id="list_webhook_deliveries",
        response_model=models.WebhookDeliveries,
    )
    def deliveries(
        identifier: str,
        request: Request,
        limit: Annotated[int, Query(ge=1, le=1000)] = 100,
        after: str = "",
    ):
        with session(request) as access:
            return webhooks.deliveries(access, identifier, limit=limit, after=after)

    @app.post(
        "/v1/webhooks/{identifier}/disable",
        operation_id="disable_webhook",
        response_model=models.WebhookStatus,
    )
    def disable(identifier: str, request: Request):
        mutation()
        with session(request, write=True) as access:
            return webhooks.control(access, identifier, "disable")

    @app.post(
        "/v1/webhooks/{identifier}/retry",
        operation_id="retry_webhook",
        response_model=models.WebhookStatus,
    )
    def retry(identifier: str, request: Request):
        mutation()
        with session(request, write=True) as access:
            return webhooks.control(access, identifier, "retry")

    @app.get(
        "/v1/rendition-requests/{identifier}/callback",
        operation_id="get_request_callback",
        response_model=models.WebhookStatus,
    )
    def callback(identifier: str, request: Request):
        with session(request) as access:
            destination = webhooks.request_callback(access, identifier)
            return webhooks.inspect(access, destination, request_scope=True)

    @app.get(
        "/v1/rendition-requests/{identifier}/callback/deliveries",
        operation_id="list_request_callback_deliveries",
        response_model=models.WebhookDeliveries,
    )
    def callback_deliveries(
        identifier: str,
        request: Request,
        limit: Annotated[int, Query(ge=1, le=1000)] = 100,
        after: str = "",
    ):
        with session(request) as access:
            destination = webhooks.request_callback(access, identifier)
            return webhooks.deliveries(
                access, destination, limit=limit, after=after, request_scope=True
            )

    @app.post(
        "/v1/rendition-requests/{identifier}/callback/retry",
        operation_id="retry_request_callback",
        response_model=models.WebhookStatus,
    )
    def callback_retry(identifier: str, request: Request):
        mutation()
        with session(request, write=True) as access:
            destination = webhooks.request_callback(access, identifier)
            return webhooks.control(access, destination, "retry", request_scope=True)

    @app.post(
        "/v1/rendition-requests/{identifier}/callback/disable",
        operation_id="disable_request_callback",
        response_model=models.WebhookStatus,
    )
    def callback_disable(identifier: str, request: Request):
        mutation()
        with session(request, write=True) as access:
            destination = webhooks.request_callback(access, identifier)
            return webhooks.control(access, destination, "disable", request_scope=True)
