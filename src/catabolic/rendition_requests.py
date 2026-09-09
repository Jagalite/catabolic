# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Caller demand references normal jobs; owner-approved recipes choose destinations."""

import json
import os
from uuid import uuid4

from .access import AccessError
from .app import Application
from .artifacts import Artifacts
from .content_access import revision_of, snapshot
from .filesystem import root_handle
from .processing import Processing
from .source_access import validated_source
from .store import encode


def admit(access, body, key, capabilities, *, max_requests=10, max_global=100):
    store = access.store
    access.require("processing:request")
    if (
        not isinstance(key, str)
        or not 1 <= len(key) <= 128
        or set(body) != {"item_id", "source_file_id", "source_revision", "operation_id"}
        or any(not isinstance(v, str) or not 1 <= len(v) <= 256 for v in body.values())
    ):
        raise AccessError("invalid_rendition_request", 400)
    if not any(
        g.get("operator")
        or body["item_id"] in access.members(g)
        and body["operation_id"] in g.get("operation_ids", [])
        for g in access.matching("processing:request")
    ):
        raise AccessError()
    if not store.rows(
        "SELECT 1 FROM item_files WHERE item_id=? AND file_id=? AND active=1 AND role IN ('primary','source')",
        (body["item_id"], body["source_file_id"]),
    ):
        raise AccessError("source_not_associated", 409)
    if (
        revision_of(store, access.profile, body["source_file_id"])
        != body["source_revision"]
    ):
        raise AccessError("revision_mismatch", 409)
    with validated_source(snapshot(store, access.profile, body["source_file_id"])):
        pass
    rows = store.rows(
        "SELECT * FROM api_operations WHERE id=? AND profile=? AND enabled=1",
        (body["operation_id"], access.profile),
    )
    if not rows:
        raise AccessError("operation_not_approved", 403)
    operation = rows[0]
    existing = store.rows(
        "SELECT * FROM api_requests WHERE principal=? AND idempotency_key=?",
        (access.principal, key),
    )
    if existing:
        if existing[0]["body"] != encode(body):
            raise AccessError("idempotency_conflict", 409)
        return status(access, existing[0]["id"])
    app = Application(store, access.profile)
    artifacts = Artifacts(app)
    recipe = artifacts.get_recipe(operation["recipe_id"])["definition"]
    destination = app.binding("source", operation["location"])
    with root_handle(destination) as fd:
        device = os.fstat(fd).st_dev
        fs = os.fstatvfs(fd)
        available = fs.f_bavail * fs.f_frsize
    budget = recipe["max_output_bytes"]
    with store.transaction() as db:
        pending = store.rows(
            "SELECT r.principal,count(*) AS count FROM api_requests r JOIN processing_jobs j ON j.id=r.job_id WHERE j.state IN ('queued','running') GROUP BY r.principal"
        )
        if (
            sum(r["count"] for r in pending) >= max_global
            or sum(r["count"] for r in pending if r["principal"] == access.principal)
            >= max_requests
        ):
            raise AccessError("request_limit", 429)
        queued = artifacts.enqueue(
            body["source_file_id"],
            operation["recipe_id"],
            operation["location"],
            body["item_id"],
            _capabilities=capabilities,
            _full_cache_check=False,
        )
        job = db.execute(
            "SELECT state FROM processing_jobs WHERE id=?", (queued["job_id"],)
        ).fetchone()
        state = "ready" if job["state"] == "complete" else job["state"]
        if (
            state != "ready"
            and not db.execute(
                "SELECT 1 FROM api_reservations WHERE job_id=?", (queued["job_id"],)
            ).fetchone()
        ):
            reserved = db.execute(
                "SELECT coalesce(sum(bytes),0) FROM api_reservations WHERE device=?",
                (device,),
            ).fetchone()[0]
            if available - reserved - budget < recipe["reserve_bytes"]:
                raise AccessError("storage_reservation_unavailable", 409)
            db.execute(
                "INSERT INTO api_reservations VALUES (?,?,?)",
                (queued["job_id"], device, budget),
            )
        identifier = str(uuid4())
        db.execute(
            "INSERT INTO api_requests(id,profile,principal,token_id,idempotency_key,body,job_id,state,reserved_bytes,reservation_device) VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                identifier,
                access.profile,
                access.principal,
                access.token["id"],
                key,
                encode(body),
                queued["job_id"],
                state,
                0 if state == "ready" else budget,
                device,
            ),
        )
    return status(access, identifier)


def status(access, identifier):
    rows = access.store.rows(
        "SELECT * FROM api_requests WHERE id=? AND principal=? AND profile=?",
        (identifier, access.principal, access.profile),
    )
    if not rows:
        raise AccessError("not_found", 404)
    row = rows[0]
    body = json.loads(row["body"])
    if not any(
        g.get("operator")
        or body["item_id"] in access.members(g)
        and body["operation_id"] in g.get("operation_ids", [])
        for g in access.matching("processing:request")
    ):
        raise AccessError("not_found", 404)
    state = row["state"]
    blockers = []
    if state == "blocked":
        blockers.append("authorization_or_operation_unavailable")
    result = None
    if state != "cancelled" and (
        revision_of(access.store, access.profile, body["source_file_id"])
        != body["source_revision"]
    ):
        state = "stale"
    elif state == "ready":
        outputs = access.store.rows(
            "SELECT a.file_id,o.id FROM processing_artifacts a JOIN media_outputs o ON o.artifact_id=a.id WHERE a.job_id=? AND a.state='ready'",
            (row["job_id"],),
        )
        if not outputs:
            state = "stale"
        else:
            output = outputs[0]
            revision = revision_of(access.store, access.profile, output["file_id"])
            if access.file(output["file_id"], "content:read", revision):
                result = {
                    "rendition_id": output["id"],
                    "file_id": output["file_id"],
                    "revision": revision,
                    "content_path": f"/v1/files/{output['file_id']}/content?revision={revision}",
                }
    return {
        "request_id": identifier,
        "state": state,
        "status_url": f"/v1/rendition-requests/{identifier}",
        "result": result,
        "progress": None,
        "blockers": ["result_not_authorized"]
        if state == "ready" and result is None
        else blockers,
    }


def cancel(access, identifier):
    status(access, identifier)
    with access.store.transaction() as db:
        db.execute(
            "UPDATE api_requests SET state='cancelled' WHERE id=?",
            (identifier,),
        )
    # A demand does not own the shared job. Leaving approved work running also
    # preserves demand from persistent rules and CLI users without API records.
    return status(access, identifier)


def retry(access, identifier):
    report = status(access, identifier)
    if report["state"] not in ("failed", "cancelled", "blocked"):
        raise AccessError("request_not_retryable", 409)
    row = access.store.rows("SELECT * FROM api_requests WHERE id=?", (identifier,))[0]
    job = access.store.rows(
        "SELECT * FROM processing_jobs WHERE id=?", (row["job_id"],)
    )[0]
    approved = access.store.rows(
        "SELECT * FROM api_operations WHERE id=? AND profile=? AND enabled=1",
        (json.loads(row["body"])["operation_id"], access.profile),
    )
    if not approved:
        raise AccessError("operation_not_approved", 403)
    app = Application(access.store, access.profile)
    recipe = Artifacts(app).get_recipe(approved[0]["recipe_id"])["definition"]
    with root_handle(app.binding("source", approved[0]["location"])) as fd:
        fs = os.fstatvfs(fd)
        device = os.fstat(fd).st_dev
    with access.store.transaction() as db:
        pending = access.store.rows(
            "SELECT r.principal,count(*) AS count FROM api_requests r JOIN processing_jobs j ON j.id=r.job_id WHERE r.id!=? AND j.state IN ('queued','running') GROUP BY r.principal",
            (identifier,),
        )
        if (
            sum(r["count"] for r in pending) >= 100
            or sum(r["count"] for r in pending if r["principal"] == access.principal)
            >= 10
        ):
            raise AccessError("request_limit", 429)
        if (
            job["state"] != "complete"
            and not db.execute(
                "SELECT 1 FROM api_reservations WHERE job_id=?", (job["id"],)
            ).fetchone()
        ):
            reserved = db.execute(
                "SELECT coalesce(sum(bytes),0) FROM api_reservations WHERE device=?",
                (device,),
            ).fetchone()[0]
            if (
                fs.f_bavail * fs.f_frsize - reserved - recipe["max_output_bytes"]
                < recipe["reserve_bytes"]
            ):
                raise AccessError("storage_reservation_unavailable", 409)
            db.execute(
                "INSERT INTO api_reservations VALUES (?,?,?)",
                (job["id"], device, recipe["max_output_bytes"]),
            )
        if job["state"] in ("failed", "timeout", "cancelled"):
            Processing(app).retry(job["id"])
        state = "ready" if job["state"] == "complete" else "queued"
        db.execute(
            "UPDATE api_requests SET state=?,reserved_bytes=?,reservation_device=? WHERE id=?",
            (
                state,
                0 if state == "ready" else recipe["max_output_bytes"],
                device,
                identifier,
            ),
        )
    return status(access, identifier)
