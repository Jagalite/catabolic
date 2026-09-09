# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Authorized durable replay; request resources remain authoritative."""

import secrets
import time

from .access import AccessError
from .rendition_requests import status


def page(access, cursor=None, limit=100):
    access.require("events:read")
    offset = 0
    if cursor:
        rows = access.store.rows(
            "SELECT ids FROM api_snapshots WHERE id=? AND principal=? AND scope='events' AND expires>?",
            (cursor, access.principal, time.time()),
        )
        if not rows:
            raise AccessError("resync_required", 409)
        offset = int(rows[0]["ids"])
    oldest = access.store.db.execute("SELECT min(id) FROM api_events").fetchone()[0]
    sequence = access.store.db.execute(
        "SELECT seq FROM sqlite_sequence WHERE name='api_events'"
    ).fetchone()
    retention_floor = (
        oldest - 1 if oldest is not None else (sequence[0] if sequence else 0)
    )
    if offset and offset < retention_floor:
        raise AccessError("resync_required", 409)
    rows = access.store.rows(
        "SELECT * FROM api_events WHERE id>? ORDER BY id LIMIT ?", (offset, limit)
    )
    events = []
    for event in rows:
        offset = event["id"]
        requests = access.store.rows(
            "SELECT id FROM api_requests WHERE principal=? AND "
            + ("job_id" if event["resource_type"] == "job" else "id")
            + "=?",
            (access.principal, event["resource_id"]),
        )
        for request in requests:
            try:
                current = status(access, request["id"])
            except AccessError:
                continue
            events.append(
                {
                    "type": "request.changed",
                    "request_id": request["id"],
                    "state": current["state"],
                }
            )
    next_cursor = secrets.token_urlsafe(24)
    with access.store.transaction() as db:
        db.execute(
            "INSERT INTO api_snapshots VALUES (?,?,?,?,?)",
            (next_cursor, access.principal, "events", str(offset), time.time() + 3600),
        )
    return {"events": events, "cursor": next_cursor, "page_complete": True}


def prune(store, keep_seconds=86400):
    with store.transaction() as db:
        db.execute(
            "DELETE FROM api_events WHERE id IN (SELECT id FROM api_events WHERE created_at<? LIMIT 1000)",
            (time.time() - keep_seconds,),
        )
        db.execute(
            "DELETE FROM api_snapshots WHERE id IN (SELECT id FROM api_snapshots WHERE expires<? LIMIT 1000)",
            (time.time(),),
        )
        db.execute(
            "DELETE FROM api_tickets WHERE id IN (SELECT id FROM api_tickets WHERE expires<? LIMIT 1000)",
            (time.time(),),
        )
