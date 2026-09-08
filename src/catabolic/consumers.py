# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Verified publication generations and fenced, coalesced consumer deliveries."""

import hashlib
import json
import sqlite3
import time
from uuid import uuid4

from .app import Application
from .consumer_adapters import ConsumerError, validate_remote, within
from .domain import CatabolicError
from .filesystem import root_handle
from .reconcile import Reconciler
from .store import Store, encode

LEASE_SECONDS = 180
MAX_ATTEMPTS = 5


def group_id(profile, application, server_id, library):
    return hashlib.sha256(
        encode(
            [profile, application, server_id, library["id"], library["uuid"]]
        ).encode()
    ).hexdigest()


def connection(app, identifier):
    rows = app.store.rows(
        "SELECT * FROM consumer_connections WHERE profile=? AND id=?",
        (app.profile, identifier),
    )
    if not rows:
        raise ConsumerError("unknown_connection")
    return rows[0]


def binding(app, identifier):
    rows = app.store.rows(
        "SELECT * FROM consumer_bindings WHERE profile=? AND id=?",
        (app.profile, identifier),
    )
    if not rows:
        raise ConsumerError("unknown_binding")
    return rows[0]


def record_change(db, profile, catalog, relative):
    """Atomic with ownership commit/journal retirement; never called for markers."""
    local = db.execute(
        "SELECT * FROM bindings WHERE profile=? AND kind='output' AND owner=?",
        (profile, catalog),
    ).fetchone()
    if local is None:
        return
    db.execute(
        "INSERT INTO publication_generations(profile,catalog,local_binding,generation) VALUES (?,?,?,1) ON CONFLICT(profile,catalog) DO UPDATE SET generation=publication_generations.generation+1,local_binding=excluded.local_binding",
        (profile, catalog, encode(dict(local))),
    )
    for row in db.execute(
        "SELECT id,subtree,debounce FROM consumer_bindings WHERE profile=? AND catalog=? AND enabled=1",
        (profile, catalog),
    ).fetchall():
        if within(relative, row["subtree"]):
            db.execute(
                "UPDATE consumer_bindings SET generation=generation+1,due_at=?,indexing=NULL WHERE profile=? AND id=?",
                (time.time() + row["debounce"], profile, row["id"]),
            )


def local_valid(app, bindings):
    catalogs = set()
    for row in bindings:
        current = app.binding("output", row["catalog"])
        if encode(current) != row["local_binding"]:
            raise ConsumerError("output_binding_changed")
        with root_handle(current):
            pass
        catalogs.add(row["catalog"])
    for catalog in catalogs:
        if not Reconciler(app).verify(catalog)["healthy"]:
            raise ConsumerError("publication_unhealthy")


def publication_verified(app, db, catalog):
    """One human publication event per catalog batch, independent of consumers."""
    rows = app.store.rows(
        "SELECT * FROM publication_generations WHERE profile=? AND catalog=? AND generation>verified",
        (app.profile, catalog),
    )
    if not rows or json.loads(rows[0]["local_binding"]) != app.binding(
        "output", catalog
    ):
        return False
    row = rows[0]
    db.execute(
        "UPDATE publication_generations SET verified=generation,verified_at=CURRENT_TIMESTAMP WHERE profile=? AND catalog=?",
        (app.profile, catalog),
    )
    from .notifications import emit

    emit(
        db,
        app.profile,
        "projection_updated",
        "info",
        catalog,
        f"publication:{catalog}:{row['generation']}",
    )
    return True


def recover_publications(database, profile, limit=100):
    """Recover ownership committed before notification scheduling, without I/O to servers."""
    with Store(database, writable=True) as store:
        app = Application(store, profile)
        rows = store.rows(
            "SELECT * FROM publication_generations WHERE profile=? AND generation>verified ORDER BY last_checked,catalog LIMIT ?",
            (profile, limit),
        )
        for row in rows:
            with store.transaction() as db:
                db.execute(
                    "UPDATE publication_generations SET last_checked=? WHERE profile=? AND catalog=?",
                    (time.time(), profile, row["catalog"]),
                )
            try:
                if (
                    json.loads(row["local_binding"])
                    != app.binding("output", row["catalog"])
                    or not Reconciler(app).verify(row["catalog"])["healthy"]
                ):
                    continue
            except (CatabolicError, OSError):
                continue
            with store.transaction() as db:
                publication_verified(app, db, row["catalog"])


def published(app, verification, result):
    """Shared post-verification boundary. Delivery runs only after Store closes."""
    if app.store.schema_version < 17:
        return
    changed = False
    with app.store.transaction() as db:
        for report in verification["catalogs"]:
            if not report["healthy"]:
                continue
            changed = publication_verified(app, db, report["catalog"]) or changed
            rows = app.store.rows(
                "SELECT * FROM consumer_bindings WHERE profile=? AND catalog=? AND enabled=1 AND generation>verified",
                (app.profile, report["catalog"]),
            )
            for row in rows:
                if (
                    encode(app.binding("output", row["catalog"]))
                    != row["local_binding"]
                ):
                    continue
                db.execute(
                    "UPDATE consumer_bindings SET verified=generation,verified_at=CURRENT_TIMESTAMP,error=NULL WHERE profile=? AND id=?",
                    (app.profile, row["id"]),
                )
    if app.store.rows(
        "SELECT 1 FROM consumer_bindings WHERE profile=? AND enabled=1 LIMIT 1",
        (app.profile,),
    ):
        unresolved = app.store.rows(
            "SELECT count(*) AS n FROM consumer_bindings WHERE profile=? AND enabled=1 AND generation>acknowledged",
            (app.profile,),
        )[0]["n"]
        result["consumers"] = {
            "scope": "profile",
            "complete": unresolved == 0,
            "unresolved_bindings": unresolved,
            "processed": 0,
            "state": "pending_worker" if unresolved else "idle",
        }
    automatic = app.store.rows(
        "SELECT 1 FROM consumer_bindings WHERE profile=? AND automatic=1 AND enabled=1 AND generation>acknowledged LIMIT 1",
        (app.profile,),
    )
    if automatic:
        outcomes = result.setdefault(
            "consumers", {"complete": False, "state": "drain_at_command_close"}
        )
        name = "consumer-drain:" + app.profile
        # All publication batches in a command share one bounded drain.
        if name not in app.store.after_close:
            destinations = []
            path, profile = app.store.path, app.profile

            def deliver():
                try:
                    report = drain(path, profile, limit=4, automatic=True)
                except (CatabolicError, OSError, sqlite3.Error, ValueError):
                    report = {
                        "complete": False,
                        "error": {"code": "worker_unavailable", "safe_to_retry": True},
                    }
                for outcome in destinations:
                    outcome.clear()
                    outcome.update(report)

            deliver.destinations = destinations
            app.store.after_close[name] = deliver
        app.store.after_close[name].destinations.append(outcomes)
    if changed:
        from .notifications import defer_drain

        defer_drain(app, result)


def group_bindings(app, group):
    return app.store.rows(
        "SELECT * FROM consumer_bindings WHERE profile=? AND group_id=? AND enabled=1 ORDER BY id",
        (app.profile, group),
    )


def claim(path, profile, *, automatic=False):
    with Store(path, writable=True) as store:
        app = Application(store, profile)
        groups = store.rows(
            """SELECT d.* FROM consumer_deliveries d WHERE d.profile=?
        AND d.state IN ('pending','accepted','retry','leased','busy') AND d.due_at<=?
        AND (d.lease_until IS NULL OR d.lease_until<=?) AND EXISTS (SELECT 1 FROM consumer_bindings b WHERE b.profile=d.profile AND b.group_id=d.id AND b.enabled=1 AND b.generation>b.acknowledged) ORDER BY d.due_at,d.id LIMIT 100""",
            (profile, time.time(), time.time()),
        )
        for group in groups:
            rows = group_bindings(app, group["id"])
            pending = [r for r in rows if r["generation"] > r["acknowledged"]]
            if not pending or automatic and not any(r["automatic"] for r in pending):
                continue
            if any(r["due_at"] > time.time() for r in rows):
                continue
            try:
                evidence = [
                    {
                        k: json.loads(row["library"]).get(k)
                        for k in ("id", "uuid", "type", "roots", "created_at")
                    }
                    for row in rows
                ]
                if any(value != evidence[0] for value in evidence):
                    raise ConsumerError("library_binding_evidence_mismatch")
                local_valid(app, rows)
            except (CatabolicError, OSError) as exc:
                repair = isinstance(exc, ConsumerError) and not exc.safe_to_retry
                code = (
                    exc.code
                    if isinstance(exc, ConsumerError)
                    else "publication_unhealthy"
                )
                with store.transaction() as db:
                    db.execute(
                        "UPDATE consumer_deliveries SET state=CASE WHEN ? THEN 'repair' ELSE state END,error=?,due_at=? WHERE profile=? AND id=?",
                        (repair, code, time.time() + 30, profile, group["id"]),
                    )
                continue
            if group["attempts"] >= MAX_ATTEMPTS:
                with store.transaction() as db:
                    db.execute(
                        "UPDATE consumer_deliveries SET state='exhausted',lease_token=NULL,lease_until=NULL WHERE profile=? AND id=?",
                        (profile, group["id"]),
                    )
                continue
            # Recover a crash after ownership commit but before post-sync scheduling.
            with store.transaction() as db:
                for r in rows:
                    if r["generation"] > r["verified"]:
                        db.execute(
                            "UPDATE consumer_bindings SET verified=generation,verified_at=CURRENT_TIMESTAMP WHERE profile=? AND id=?",
                            (profile, r["id"]),
                        )
                for catalog in {r["catalog"] for r in rows}:
                    publication_verified(app, db, catalog)
            token = str(uuid4())
            snapshot = [
                {
                    "id": r["id"],
                    "revision": r["revision"],
                    "generation": r["generation"],
                    "connection_revision": connection(app, r["connection_id"])[
                        "revision"
                    ],
                }
                for r in rows
            ]
            with store.transaction() as db:
                # Retire lost claims; history still distinguishes uncertain sends.
                db.execute(
                    "UPDATE consumer_attempts SET state='lease_expired',finished_at=CURRENT_TIMESTAMP WHERE profile=? AND group_id=? AND state='leased'",
                    (profile, group["id"]),
                )
                db.execute(
                    "UPDATE consumer_deliveries SET state='leased',lease_token=?,lease_until=?,attempts=attempts+1 WHERE profile=? AND id=?",
                    (token, time.time() + LEASE_SECONDS, profile, group["id"]),
                )
                cursor = db.execute(
                    "INSERT INTO consumer_attempts(profile,group_id,token,snapshot,state) VALUES (?,?,?,?,'leased')",
                    (profile, group["id"], token, encode(snapshot)),
                )
            return {
                "group_id": group["id"],
                "token": token,
                "attempt_id": cursor.lastrowid,
                "snapshot": snapshot,
                "connection": connection(app, rows[0]["connection_id"]),
                "library": json.loads(rows[0]["library"]),
            }
    return None


def fence(app, claimed, *, before_send=False):
    rows = app.store.rows(
        "SELECT * FROM consumer_deliveries WHERE profile=? AND id=? AND lease_token=? AND lease_until>? AND state='leased'",
        (app.profile, claimed["group_id"], claimed["token"], time.time()),
    )
    if not rows:
        raise ConsumerError("stale_claim")
    current = group_bindings(app, claimed["group_id"])
    versions = {r["id"]: r for r in current}
    if set(versions) != {r["id"] for r in claimed["snapshot"]}:
        raise ConsumerError("binding_changed")
    for old in claimed["snapshot"]:
        r = versions[old["id"]]
        if (
            connection(app, r["connection_id"])["revision"]
            != old["connection_revision"]
        ):
            raise ConsumerError("binding_changed")
        if r["revision"] != old["revision"]:
            raise ConsumerError("binding_changed")
        if before_send and r["generation"] != old["generation"]:
            raise ConsumerError("publication_changed")
    if (
        connection(app, current[0]["connection_id"])["revision"]
        != claimed["connection"]["revision"]
    ):
        raise ConsumerError("binding_changed")
    return rows[0], current


def acknowledge(path, profile, claimed, error=None):
    with Store(path, writable=True) as store:
        app = Application(store, profile)
        try:
            group, current = fence(app, claimed)
        except ConsumerError:
            # Never acknowledge a replaced/disabled binding or a newer lease.
            with store.transaction() as db:
                db.execute(
                    "UPDATE consumer_attempts SET state='stale_acknowledgement',finished_at=CURRENT_TIMESTAMP WHERE id=? AND token=?",
                    (claimed["attempt_id"], claimed["token"]),
                )
                db.execute(
                    "UPDATE consumer_deliveries SET state='pending',lease_token=NULL,lease_until=NULL WHERE profile=? AND id=? AND lease_token=?",
                    (profile, claimed["group_id"], claimed["token"]),
                )
            return {"state": "stale_acknowledgement", "complete": False}
        with store.transaction() as db:
            if error is None:
                for r in claimed["snapshot"]:
                    db.execute(
                        "UPDATE consumer_bindings SET acknowledged=max(acknowledged,?),error=NULL WHERE profile=? AND id=? AND revision=?",
                        (r["generation"], profile, r["id"], r["revision"]),
                    )
                state, due = "accepted", 0
                from .notifications import emit

                emit(
                    db,
                    profile,
                    "scan_requested",
                    "info",
                    claimed["group_id"],
                    "scan:" + claimed["token"],
                )
            else:
                state = (
                    "pending"
                    if error.code == "publication_changed"
                    else "busy"
                    if error.code == "busy"
                    else "retry"
                    if error.safe_to_retry and group["attempts"] < MAX_ATTEMPTS
                    else "exhausted"
                    if error.safe_to_retry
                    else "repair"
                )
                due = time.time() + max(
                    error.retry_after, min(3600, 5 * 2 ** min(group["attempts"], 10))
                )
                if error.code in ("busy", "publication_changed"):
                    due = time.time() + (30 if error.code == "busy" else 1)
                    db.execute(
                        "UPDATE consumer_deliveries SET attempts=max(0,attempts-1) WHERE profile=? AND id=?",
                        (profile, claimed["group_id"]),
                    )
                from .notifications import emit

                if state in ("repair", "exhausted"):
                    emit(
                        db,
                        profile,
                        "consumer_needs_repair" if state == "repair" else "scan_failed",
                        "error",
                        claimed["group_id"],
                        state + ":" + claimed["token"],
                    )
            db.execute(
                "UPDATE consumer_deliveries SET state=?,due_at=?,lease_token=NULL,lease_until=NULL,error=?,accepted_at=CASE WHEN ?='accepted' THEN CURRENT_TIMESTAMP ELSE accepted_at END,attempts=CASE WHEN ?='accepted' THEN 0 ELSE attempts END WHERE profile=? AND id=?",
                (
                    state,
                    due,
                    error.code if error else None,
                    state,
                    state,
                    profile,
                    claimed["group_id"],
                ),
            )
            db.execute(
                "UPDATE consumer_attempts SET state=?,error=?,finished_at=CURRENT_TIMESTAMP WHERE id=?",
                (state, error.code if error else None, claimed["attempt_id"]),
            )
        return {
            "state": state,
            "complete": error is None,
            "error": error.result() if error else None,
            "scan_request_accepted": error is None,
            "indexing_verified": False,
        }


def deliver(path, profile, claimed):
    try:
        a, library = validate_remote(claimed["connection"], claimed["library"])
        if library.get("scanning") is True:
            raise ConsumerError("busy")
        # Recheck edits, newer generations and storage after discovery I/O.
        with Store(path, writable=True) as store:
            app = Application(store, profile)
            _, rows = fence(app, claimed, before_send=True)
            local_valid(app, rows)
        a.scan(library)
        error = None
    except ConsumerError as exc:
        error = exc
    except (CatabolicError, OSError, ValueError, TypeError, KeyError):
        error = ConsumerError("temporarily_failed")
    return acknowledge(path, profile, claimed, error)


def status(app, limit=100):
    rows = app.store.rows(
        """SELECT b.*,d.state AS delivery_state,d.attempts,d.error AS delivery_error,d.lease_until,d.accepted_at
    FROM consumer_bindings b JOIN consumer_deliveries d ON d.profile=b.profile AND d.id=b.group_id
    WHERE b.profile=? ORDER BY b.id LIMIT ?""",
        (app.profile, limit + 1),
    )
    for r in rows:
        r["library"] = json.loads(r["library"])
        r["local_binding"] = json.loads(r["local_binding"])
        r["indexing"] = json.loads(r["indexing"]) if r["indexing"] else None
        r["state"] = (
            "disabled"
            if not r["enabled"]
            else "unverified"
            if r["generation"] > r["verified"]
            else r["delivery_state"]
            if r["generation"] > r["acknowledged"]
            else "accepted"
            if r["acknowledged"]
            else "idle"
        )
    unresolved = app.store.rows(
        "SELECT count(*) AS n FROM consumer_bindings WHERE profile=? AND enabled=1 AND generation>acknowledged",
        (app.profile,),
    )[0]["n"]
    return {
        "bindings": rows[:limit],
        "truncated": len(rows) > limit,
        "unresolved_bindings": unresolved,
        "complete": unresolved == 0,
    }


def drain(path, profile="default", *, limit=10, automatic=False):
    if type(limit) is not int or not 1 <= limit <= 100:
        raise ConsumerError("invalid_configuration")
    results = []
    for _ in range(limit):
        claimed = claim(path, profile, automatic=automatic)
        if claimed is None:
            break
        results.append(
            {"group_id": claimed["group_id"], **deliver(path, profile, claimed)}
        )
    with Store(path) as store:
        report = status(Application(store, profile))
    return {
        "deliveries": results,
        "processed": len(results),
        "unresolved_bindings": report["unresolved_bindings"],
        "complete": report["complete"],
    }
