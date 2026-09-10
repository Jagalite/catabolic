# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Optional human summaries; per-destination leases and independent retries."""

import hashlib
import json
import os
import sys
import time
from uuid import uuid4

from .consumer_adapters import ConsumerError, credential_ref, text
from .process_runner import CommandFailure, command_output
from .store import Store, encode

EVENTS = {
    "fallback_selected",
    "fallback_unresolved",
    "projection_updated",
    "scan_requested",
    "scan_failed",
    "consumer_needs_repair",
}
SEVERITIES = {"info": 0, "warning": 1, "error": 2}


def configure(
    app, identifier, environment, subscriptions, tags, severity, *, apply=False
):
    text(identifier, 255)
    credential_ref(environment)
    if (
        not isinstance(subscriptions, list)
        or not subscriptions
        or set(subscriptions) - EVENTS
        or severity not in SEVERITIES
        or not isinstance(tags, list)
        or len(tags) > 20
    ):
        raise ConsumerError("invalid_notification_policy")
    for tag in tags:
        text(tag, 64)
    previous = app.store.rows(
        "SELECT * FROM notification_destinations WHERE profile=? AND id=?",
        (app.profile, identifier),
    )
    unchanged = previous and all(
        previous[0][key] == value
        for key, value in {
            "credential_env": environment,
            "subscriptions": encode(subscriptions),
            "tags": encode(tags),
            "min_severity": severity,
            "enabled": 1,
        }.items()
    )
    if apply and not unchanged:
        with app.store.transaction() as db:
            db.execute(
                """INSERT INTO notification_destinations(profile,id,credential_env,subscriptions,tags,min_severity,enabled) VALUES (?,?,?,?,?,?,1)
            ON CONFLICT(profile,id) DO UPDATE SET credential_env=excluded.credential_env,subscriptions=excluded.subscriptions,tags=excluded.tags,min_severity=excluded.min_severity,enabled=1,revision=notification_destinations.revision+1""",
                (
                    app.profile,
                    identifier,
                    environment,
                    encode(subscriptions),
                    encode(tags),
                    severity,
                ),
            )
            db.execute(
                "UPDATE notification_deliveries SET state='cancelled',lease_token=NULL,lease_until=NULL WHERE profile=? AND destination_id=? AND state!='complete'",
                (app.profile, identifier),
            )
    return {
        "id": identifier,
        "credential_env": environment,
        "subscriptions": subscriptions,
        "tags": tags,
        "min_severity": severity,
        "applied": apply,
        "complete": True,
    }


def emit(db, profile, event, severity, subject, identity):
    event_id = hashlib.sha256(encode([profile, event, identity]).encode()).hexdigest()
    inserted = db.execute(
        "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,?,?,?) ON CONFLICT DO NOTHING",
        (event_id, profile, event, severity, subject),
    ).rowcount
    if not inserted:
        return
    for row in db.execute(
        "SELECT * FROM notification_destinations WHERE profile=? AND enabled=1",
        (profile,),
    ).fetchall():
        if (
            event in json.loads(row["subscriptions"])
            and SEVERITIES[severity] >= SEVERITIES[row["min_severity"]]
        ):
            db.execute(
                "INSERT INTO notification_deliveries(event_id,profile,destination_id) VALUES (?,?,?) ON CONFLICT DO NOTHING",
                (event_id, profile, row["id"]),
            )


def send(environment, event, severity):
    value = os.environ.get(environment)
    if not value:
        return "credential_unavailable"
    payload = encode({"url": value, "event": event, "severity": severity}).encode()
    if len(payload) > 16384:
        return "invalid_destination"
    try:
        raw = command_output(
            [sys.executable, "-m", "catabolic.notification_worker"],
            input_bytes=payload,
            timeout=30,
            maximum=4096,
        )
        return json.loads(raw)["status"]
    except (CommandFailure, OSError, ValueError, KeyError):
        return "temporarily_failed"


def pending_publications(store, profile):
    subscribed = store.rows(
        "SELECT 1 FROM notification_destinations d WHERE profile=? AND enabled=1 AND min_severity='info' AND EXISTS(SELECT 1 FROM json_each(d.subscriptions) WHERE value='projection_updated') LIMIT 1",
        (profile,),
    )
    if not subscribed:
        return 0
    return store.rows(
        "SELECT count(*) AS n FROM publication_generations WHERE profile=? AND generation>verified",
        (profile,),
    )[0]["n"]


def drain(database, profile="default", limit=10, tag=None):
    if not 1 <= limit <= 100:
        raise ConsumerError("invalid_configuration")
    from .consumers import recover_publications

    recover_publications(database, profile, limit)
    results = []
    for _ in range(limit):
        with Store(database, writable=True) as store:
            with store.transaction() as db:
                db.execute(
                    "UPDATE notification_deliveries SET state='exhausted',lease_token=NULL,lease_until=NULL WHERE profile=? AND state IN ('pending','retry','leased') AND attempts>=5 AND (lease_until IS NULL OR lease_until<=?)",
                    (profile, time.time()),
                )
            rows = store.rows(
                """SELECT n.*,d.credential_env,d.tags,d.revision AS destination_revision,e.event,e.severity FROM notification_deliveries n
            JOIN notification_destinations d ON d.profile=n.profile AND d.id=n.destination_id
            JOIN consumer_events e ON e.id=n.event_id WHERE n.profile=? AND d.enabled=1
            AND n.state IN ('pending','retry','leased') AND n.due_at<=? AND (n.lease_until IS NULL OR n.lease_until<=?) AND (? IS NULL OR EXISTS(SELECT 1 FROM json_each(d.tags) WHERE value=?)) ORDER BY e.created_at,n.event_id LIMIT 100""",
                (profile, time.time(), time.time(), tag, tag),
            )
            if not rows:
                break
            row, token = rows[0], str(uuid4())
            with store.transaction() as db:
                db.execute(
                    "UPDATE notification_deliveries SET state='leased',attempts=attempts+1,lease_token=?,lease_until=? WHERE profile=? AND event_id=? AND destination_id=?",
                    (
                        token,
                        time.time() + 60,
                        profile,
                        row["event_id"],
                        row["destination_id"],
                    ),
                )
        # Recheck the destination before I/O. Changes after dispatch cannot unsend it.
        with Store(database) as store:
            valid = store.rows(
                "SELECT 1 FROM notification_destinations WHERE profile=? AND id=? AND enabled=1 AND revision=?",
                (profile, row["destination_id"], row["destination_revision"]),
            )
        if not valid:
            continue
        outcome = send(row["credential_env"], row["event"], row["severity"])
        state = (
            "complete"
            if outcome == "complete"
            else "repair"
            if outcome
            in ("uninstalled", "credential_unavailable", "invalid_destination")
            else "exhausted"
            if row["attempts"] + 1 >= 5
            else "retry"
        )
        with Store(database, writable=True) as store, store.transaction() as db:
            updated = db.execute(
                "UPDATE notification_deliveries SET state=?,error=?,lease_token=NULL,lease_until=NULL,due_at=? WHERE profile=? AND event_id=? AND destination_id=? AND lease_token=? AND lease_until>? AND EXISTS (SELECT 1 FROM notification_destinations d WHERE d.profile=notification_deliveries.profile AND d.id=notification_deliveries.destination_id AND d.enabled=1 AND d.revision=?)",
                (
                    state,
                    None if outcome == "complete" else outcome,
                    time.time() + min(3600, 5 * 2 ** row["attempts"]),
                    profile,
                    row["event_id"],
                    row["destination_id"],
                    token,
                    time.time(),
                    row["destination_revision"],
                ),
            ).rowcount
            if not updated:
                state = "stale_acknowledgement"
        results.append(
            {
                "event_id": row["event_id"],
                "destination_id": row["destination_id"],
                "state": state,
            }
        )
    with Store(database) as store:
        pending = store.rows(
            "SELECT count(*) AS n FROM notification_deliveries n JOIN notification_destinations d ON d.profile=n.profile AND d.id=n.destination_id WHERE n.profile=? AND d.enabled=1 AND n.state NOT IN ('complete','cancelled')",
            (profile,),
        )[0]["n"]
        publications = pending_publications(store, profile)
    return {
        "deliveries": results,
        "unresolved": pending,
        "unverified_publications": publications,
        "complete": pending == 0 and publications == 0,
    }


def defer_drain(app, result):
    if not app.store.rows(
        "SELECT 1 FROM notification_destinations WHERE profile=? AND enabled=1 LIMIT 1",
        (app.profile,),
    ):
        return
    output = result.setdefault(
        "notifications", {"complete": False, "state": "drain_at_command_close"}
    )
    database, profile = app.store.path, app.profile

    name = "notifications:" + profile
    if name not in app.store.after_close:
        destinations = []

        def deliver():
            try:
                report = drain(database, profile, limit=4)
            except Exception:
                report = {"complete": False, "error": "notification_worker_unavailable"}
            for target in destinations:
                target.clear()
                target.update(report)

        deliver.destinations = destinations
        app.store.after_close[name] = deliver
    app.store.after_close[name].destinations.append(output)
