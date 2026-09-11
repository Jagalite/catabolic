# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Operator-only watcher definitions/status; execution is durably admitted."""

from fastapi import Request
from pydantic import JsonValue

from ..access import AccessError
from ..app import Application
from ..watcher_models import WatcherDefinition
from ..watchers import Watchers
from .models import Model


class WatcherStatus(Model):
    name: str
    definition_id: str
    enabled: bool
    next_due: float
    pending_generation: int
    completed_generation: int
    active_run: str | None


class WatcherPage(Model):
    watchers: list[WatcherStatus]


class WatcherDetail(WatcherStatus):
    definition: WatcherDefinition
    revision: int
    digest: str
    retry_at: float
    pending_reaction: str | None
    last_run: str | None


class WatcherControl(Model):
    enabled: bool
    transfer: bool = False


class WatcherRun(Model):
    id: str
    definition_id: str
    generation: int
    trigger: str
    scheduled_at: float
    started_at: float
    completed_at: float | None
    state: str
    result: JsonValue
    error: str | None


class WatcherHistory(Model):
    runs: list[WatcherRun]


class WatcherAdmission(Model):
    name: str
    pending_generation: int
    admitted: bool


def fields(model, value):
    return {
        key: bool(value[key]) if key == "enabled" else value[key]
        for key in model.model_fields
    }


def install_routes(app, session, mutation):
    def service(access):
        if not access.operator("operator:write"):
            raise AccessError("forbidden", 403)
        return Watchers(Application(access.store, access.profile))

    @app.get(
        "/v1/operator/watchers",
        operation_id="list_watchers",
        response_model=WatcherPage,
    )
    def listing(request: Request):
        with session(request) as access:
            return {
                "watchers": [fields(WatcherStatus, r) for r in service(access).list()]
            }

    @app.put(
        "/v1/operator/watchers/{name}",
        operation_id="put_watcher",
        response_model=WatcherDetail,
    )
    def put(name: str, body: WatcherDefinition, request: Request):
        mutation()
        with session(request, write=True) as access:
            return fields(WatcherDetail, service(access).put(name, body.model_dump()))

    @app.get(
        "/v1/operator/watchers/{name}",
        operation_id="get_watcher",
        response_model=WatcherDetail,
    )
    def get(name: str, request: Request):
        with session(request) as access:
            return fields(WatcherDetail, service(access).get(name))

    @app.post(
        "/v1/operator/watchers/{name}/enabled",
        operation_id="set_watcher_enabled",
        response_model=WatcherDetail,
    )
    def enable(name: str, body: WatcherControl, request: Request):
        mutation()
        with session(request, write=True) as access:
            return fields(
                WatcherDetail,
                service(access).enable(name, body.enabled, transfer=body.transfer),
            )

    @app.post(
        "/v1/operator/watchers/{name}/runs",
        operation_id="admit_watcher_run",
        response_model=WatcherAdmission,
        status_code=202,
    )
    def run(name: str, request: Request):
        mutation()
        with session(request, write=True) as access:
            watchers = service(access)
            row = watchers.admit(name)
            return {
                "name": name,
                "pending_generation": row["pending_generation"],
                "admitted": True,
            }

    @app.get(
        "/v1/operator/watchers/{name}/history",
        operation_id="get_watcher_history",
        response_model=WatcherHistory,
    )
    def history(name: str, request: Request):
        import json

        with session(request) as access:
            rows = service(access).history(name)
            return {
                "runs": [
                    fields(
                        WatcherRun,
                        {
                            **r,
                            "result": json.loads(r["result"]) if r["result"] else None,
                        },
                    )
                    for r in rows
                ]
            }
