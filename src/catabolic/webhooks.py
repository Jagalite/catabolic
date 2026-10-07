# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""HTTP-managed destinations and principal-bound request callbacks."""

import json
from uuid import uuid4

from .access import AccessError, authenticate
from .app import Application
from .notifications import EVENTS, configure

TERMINAL = {"ready", "failed", "cancelled", "stale", "blocked"}


def origin(value):
    from .callback_urls import origin as validate

    try:
        return validate(value)
    except ValueError:
        raise AccessError("invalid_callback_url", 400) from None


def allow_callback(access, url):
    target = origin(url)
    if access.operator("webhooks:manage"):
        return
    if any(
        target == origin(allowed)
        for grant in access.matching("processing:request")
        for allowed in grant.get("callback_origins", [])
    ):
        return
    raise AccessError("callback_origin_not_allowed", 403)


def require_operator(access):
    if not access.operator("webhooks:manage"):
        raise AccessError()


def register(access, url, events, *, request_id=None):
    if request_id is None:
        require_operator(access)
        origin(url)
        if not events or set(events) - EVENTS:
            raise AccessError("invalid_webhook_events", 400)
    else:
        allow_callback(access, url)
    if (
        access.store.rows(
            """SELECT count(*) AS n FROM api_webhooks w
            JOIN notification_destinations d ON d.profile=w.profile AND d.id=w.destination_id
            WHERE w.principal=? AND d.enabled=1 AND (
              w.request_id IS NULL OR EXISTS(SELECT 1 FROM api_requests r WHERE r.id=w.request_id AND r.state IN ('queued','running','validating'))
              OR EXISTS(SELECT 1 FROM notification_deliveries n WHERE n.profile=w.profile AND n.destination_id=w.destination_id AND n.state NOT IN ('complete','cancelled')))
            """,
            (access.principal,),
        )[0]["n"]
        >= 1000
    ):
        raise AccessError("webhook_limit", 429)
    identifier = "http:" + str(uuid4())
    with access.store.transaction() as db:
        if request_id is None:
            configure(
                Application(access.store, access.profile),
                identifier,
                "CATABOLIC_HTTP_WEBHOOK",
                events,
                [],
                "info",
                apply=True,
            )
        else:
            # Request events are routed only to the associated callback, never
            # through profile-wide subscriptions or another caller's shared job.
            db.execute(
                "INSERT INTO notification_destinations(profile,id,credential_env,subscriptions,tags,min_severity) VALUES (?,?,?,'[]','[]','info')",
                (access.profile, identifier, "CATABOLIC_HTTP_WEBHOOK"),
            )
        db.execute(
            "INSERT INTO api_webhooks VALUES (?,?,?,?,?,?)",
            (
                access.profile,
                identifier,
                access.principal,
                access.token["id"],
                url,
                request_id,
            ),
        )
        if request_id is not None:
            row = db.execute(
                "SELECT state FROM api_requests WHERE id=?", (request_id,)
            ).fetchone()
            if row["state"] in TERMINAL:
                event_id = "request:" + str(uuid4())
                db.execute(
                    "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,?,?,?)",
                    (
                        event_id,
                        access.profile,
                        "request_" + row["state"],
                        "error" if row["state"] == "failed" else "info",
                        request_id,
                    ),
                )
                db.execute(
                    "INSERT INTO notification_deliveries(event_id,profile,destination_id) VALUES (?,?,?)",
                    (event_id, access.profile, identifier),
                )
    return inspect(access, identifier, request_scope=request_id is not None)


def inspect(access, identifier, *, request_scope=False):
    if not request_scope:
        require_operator(access)
    rows = access.store.rows(
        "SELECT d.id,d.enabled,d.subscriptions,w.request_id FROM notification_destinations d JOIN api_webhooks w ON w.profile=d.profile AND w.destination_id=d.id WHERE d.profile=? AND d.id=? AND w.principal=?",
        (access.profile, identifier, access.principal),
    )
    if not rows:
        raise AccessError("not_found", 404)
    row = rows[0]
    return {
        "id": row["id"],
        "enabled": bool(row["enabled"]),
        "events": sorted("request_" + state for state in TERMINAL)
        if row["request_id"]
        else json.loads(row["subscriptions"]),
        "request_id": row["request_id"],
    }


def listing(access, *, limit=100, after=""):
    require_operator(access)
    rows = access.store.rows(
        "SELECT destination_id FROM api_webhooks WHERE profile=? AND principal=? AND destination_id>? ORDER BY destination_id LIMIT ?",
        (access.profile, access.principal, after, limit + 1),
    )
    return {
        "webhooks": [inspect(access, row["destination_id"]) for row in rows[:limit]],
        "next_cursor": rows[limit - 1]["destination_id"] if len(rows) > limit else None,
    }


def deliveries(access, identifier, *, limit=100, after="", request_scope=False):
    inspect(access, identifier, request_scope=request_scope)
    rows = access.store.rows(
        "SELECT event_id,state,attempts,error FROM notification_deliveries WHERE profile=? AND destination_id=? AND event_id>? ORDER BY event_id LIMIT ?",
        (access.profile, identifier, after, limit + 1),
    )
    return {
        "deliveries": rows[:limit],
        "next_cursor": rows[limit - 1]["event_id"] if len(rows) > limit else None,
    }


def request_callback(access, identifier):
    from .rendition_requests import status

    status(access, identifier)
    rows = access.store.rows(
        "SELECT destination_id FROM api_webhooks WHERE profile=? AND principal=? AND request_id=?",
        (access.profile, access.principal, identifier),
    )
    if not rows:
        raise AccessError("not_found", 404)
    return rows[0]["destination_id"]


def control(access, identifier, operation, *, request_scope=False):
    destination = inspect(access, identifier, request_scope=request_scope)
    if operation == "retry":
        if not destination["enabled"]:
            raise AccessError("webhook_disabled", 409)
        if request_scope:
            row = access.store.rows(
                "SELECT url FROM api_webhooks WHERE profile=? AND destination_id=?",
                (access.profile, identifier),
            )[0]
            allow_callback(access, row["url"])
    with access.store.transaction() as db:
        if operation == "disable":
            db.execute(
                "UPDATE notification_destinations SET enabled=0,revision=revision+1 WHERE profile=? AND id=?",
                (access.profile, identifier),
            )
            db.execute(
                "UPDATE notification_deliveries SET state='cancelled',lease_token=NULL,lease_until=NULL WHERE profile=? AND destination_id=? AND state!='complete'",
                (access.profile, identifier),
            )
        elif operation == "retry":
            from .notifications import retry_delivery

            retry_delivery(db, access.profile, identifier)
            db.execute(
                "UPDATE api_webhooks SET token_id=? WHERE profile=? AND destination_id=?",
                (access.token["id"], access.profile, identifier),
            )
    return inspect(access, identifier, request_scope=request_scope)


def delivery_url(store, profile, identifier):
    rows = store.rows(
        "SELECT * FROM api_webhooks WHERE profile=? AND destination_id=?",
        (profile, identifier),
    )
    if not rows:
        return None
    row = rows[0]
    access = authenticate(store, token_id=row["token_id"])
    if row["request_id"] is None:
        require_operator(access)
    else:
        from .rendition_requests import status

        status(access, row["request_id"])
        allow_callback(access, row["url"])
    return row["url"]
