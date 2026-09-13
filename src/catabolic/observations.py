# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Compatible full-source observation demand and fenced scanner publication."""

import hashlib
import json
import os
import time
from uuid import UUID, uuid4

from .domain import CatabolicError, relative_path
from .source_trust import source_policy
from .store import encode


def compatibility(app, source, exclusions=(), require_complete=False, execution=None):
    from .scan_policy import effective
    from .scan_policy import execution as defaults

    execution = execution or defaults(app, source)
    return hashlib.sha256(
        encode(
            {
                "binding": app.store.rows(
                    "SELECT * FROM bindings WHERE profile=? AND kind='source' AND owner=?",
                    (app.profile, source),
                ),
                "volume": app.store.rows(
                    "SELECT * FROM binding_volumes WHERE profile=? AND kind='source' AND owner=?",
                    (app.profile, source),
                ),
                "policy": source_policy(app.store, app.profile, source),
                "exclusions": list(exclusions),
                "method": "guarded-inventory-v2",
                "observation_policy": effective(app, source),
                "execution": execution,
                "require_complete": require_complete,
            }
        ).encode()
    ).hexdigest()


def dirty(app, source):
    with app.store.transaction() as db:
        db.execute(
            "INSERT INTO observation_sources VALUES (?,?,1) ON CONFLICT(profile,source) DO UPDATE SET dirty_generation=dirty_generation+1",
            (app.profile, source),
        )


def _request_job(
    app,
    source,
    *,
    minimum_start,
    exclusions=(),
    reuse_completed=True,
    require_complete=False,
    reuse_unavailable=True,
    completed_after=0,
    execution=None,
    parent_job=None,
):
    from .scan_policy import execution as defaults

    execution = execution or defaults(app, source)
    now = time.time()
    key = compatibility(app, source, exclusions, require_complete, execution)
    with app.store.transaction() as db:
        db.execute(
            "INSERT OR IGNORE INTO observation_sources VALUES (?,?,0)",
            (app.profile, source),
        )
        watermark = db.execute(
            "SELECT dirty_generation FROM observation_sources WHERE profile=? AND source=?",
            (app.profile, source),
        ).fetchone()[0]
        rows = db.execute(
            "SELECT * FROM observation_jobs WHERE profile=? AND source=? AND compatibility=? AND dirty_generation>=? ORDER BY generation DESC LIMIT 100",
            (app.profile, source, key, watermark),
        ).fetchall()
        for raw in rows:
            row = dict(raw)
            if row["state"] == "failed" and json.loads(row["report"] or "{}").get(
                "deferred"
            ):
                # Successful continuation resolves the blocker, but does not
                # turn this historical interval into fresh completed evidence.
                resolved = db.execute(
                    "WITH RECURSIVE descendants(id) AS (SELECT id FROM observation_jobs WHERE parent_job=? UNION ALL SELECT j.id FROM observation_jobs j JOIN descendants d ON j.parent_job=d.id) SELECT 1 FROM descendants d JOIN observation_jobs j ON j.id=d.id WHERE j.state='complete' LIMIT 1",
                    (row["id"],),
                ).fetchone()
                if not resolved:
                    return {
                        **row,
                        "reuse": "deferred",
                        "blocker": "explicit_continuation_required",
                    }
            if (
                reuse_completed
                and row["state"] in ("complete", "unavailable")
                and row["started_at"] >= max(minimum_start, completed_after)
                and row["parent_job"] is None
                and json.loads(row["execution"]).get("scopes", [""]) == [""]
                and (
                    row["state"] == "complete"
                    or (reuse_unavailable and not require_complete)
                )
            ):
                return {**row, "reuse": "completed"}
            if row["state"] in ("queued", "running") and (
                row["state"] == "queued"
                or (row["lease_until"] > now and row["started_at"] >= minimum_start)
            ):
                return {**row, "reuse": "inflight"}
        generation = db.execute(
            "SELECT coalesce(max(generation),0)+1 FROM observation_jobs WHERE profile=? AND source=?",
            (app.profile, source),
        ).fetchone()[0]
        identifier = str(uuid4())
        db.execute(
            "INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,execution,parent_job) VALUES (?,?,?,?,?,?,?,'queued',?,?,?)",
            (
                identifier,
                app.profile,
                source,
                key,
                encode(list(exclusions)),
                watermark,
                generation,
                now,
                encode(execution),
                parent_job,
            ),
        )
    return {
        **app.store.rows("SELECT * FROM observation_jobs WHERE id=?", (identifier,))[0],
        "reuse": "created",
    }


def request(
    app,
    source,
    *,
    max_age=0,
    after=0,
    exclusions=(),
    reuse_completed=True,
    require_complete=False,
    requester=None,
    extended=False,
    budgets=None,
    scopes=None,
    parent_job=None,
    continuation_scopes=None,
):
    """Admit immutable guarantees once; an idempotency key resumes the same demand."""
    if max_age < 0 or after < 0:
        raise CatabolicError("invalid_observation_freshness")
    if not app.store.rows("SELECT id FROM locations WHERE id=?", (source,)):
        raise CatabolicError(f"unknown location: {source}")
    from .scan_policy import effective
    from .scan_policy import execution as execution_policy

    exclusions = sorted(
        {relative_path(p) for p in exclusions}
        | set(effective(app, source)["exclusions"])
    )
    execution = execution_policy(
        app, source, extended=extended, budgets=budgets, scopes=scopes
    )
    if parent_job:
        execution["continuation_of"] = parent_job
        execution["continuation_scopes"] = continuation_scopes
    if requester is not None:
        existing = app.store.rows(
            "SELECT id FROM observation_requests WHERE profile=? AND source=? AND requester=?",
            (app.profile, source, requester),
        )
        if existing:
            result = resume(app, existing[0]["id"])
            if any(
                result["guarantees"][key] != value
                for key, value in (
                    ("exclusions", exclusions),
                    ("require_complete", require_complete),
                    ("execution", execution),
                    ("max_age", max_age),
                    ("reuse_completed", reuse_completed),
                )
            ):
                raise CatabolicError("observation_request_guarantees_changed")
            return result
    now = time.time()
    guarantees = dict(
        minimum_start=max(after, now - max_age) if reuse_completed else after,
        after=after,
        max_age=max_age,
        exclusions=exclusions,
        reuse_completed=reuse_completed,
        require_complete=require_complete,
        coverage="full_source",
        method="guarded-inventory-v2",
        execution=execution,
        parent_job=parent_job,
    )
    recover(app)
    job = _request_job(
        app,
        source,
        execution=execution,
        parent_job=parent_job,
        **{
            k: guarantees[k]
            for k in (
                "minimum_start",
                "exclusions",
                "reuse_completed",
                "require_complete",
            )
        },
    )
    identifier = str(uuid4())
    with app.store.transaction() as db:
        db.execute(
            "INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,created_at) VALUES (?,?,?,?,?,?,?)",
            (
                identifier,
                app.profile,
                source,
                requester,
                encode(guarantees),
                job["id"],
                now,
            ),
        )
    return {**job, "request_id": identifier, "guarantees": guarantees}


def resume(app, request_id):
    rows = app.store.rows(
        "SELECT * FROM observation_requests WHERE id=? AND profile=?",
        (request_id, app.profile),
    )
    if not rows:
        raise CatabolicError("unknown_observation_request")
    demand = rows[0]
    guarantees = json.loads(demand["guarantees"])
    recover(app)
    job = app.store.rows(
        "SELECT * FROM observation_jobs WHERE id=?", (demand["job_id"],)
    )[0]
    if demand["state"] == "cancelled":
        return _result(app, {**job, "request_id": request_id, "guarantees": guarantees})
    from .scan_policy import effective

    if (
        guarantees.get("execution", {}).get("policy_revision", 0)
        != effective(app, demand["source"])["revision"]
    ):
        raise CatabolicError("observation policy changed; request a new scan")
    watermark = app.store.rows(
        "SELECT dirty_generation FROM observation_sources WHERE profile=? AND source=?",
        (app.profile, demand["source"]),
    )[0]["dirty_generation"]
    compatible = (
        job["compatibility"]
        == compatibility(
            app,
            demand["source"],
            guarantees["exclusions"],
            guarantees["require_complete"],
            guarantees.get("execution"),
        )
        and job["dirty_generation"] >= watermark
    )
    if (
        compatible
        and (
            job["state"] == "stale"
            or (
                job["state"] == "failed"
                and json.loads(job["report"] or "{}").get("resume_progress")
            )
        )
        and job["scan_id"]
        and not json.loads(job["report"] or "{}").get("deferred")
    ):
        with app.store.transaction() as db:
            db.execute(
                "UPDATE observation_jobs SET state='queued',lease_until=0 WHERE id=? AND state IN ('stale','failed')",
                (job["id"],),
            )
        job["state"] = "queued"
    deferred = bool(json.loads(job["report"] or "{}").get("deferred"))
    if not compatible or (
        job["state"] in ("failed", "stale", "unavailable") and not deferred
    ):
        job = _request_job(
            app,
            demand["source"],
            execution=guarantees.get("execution"),
            parent_job=guarantees.get("parent_job"),
            reuse_unavailable=False,
            completed_after=job["started_at"] or job["requested_at"],
            **{
                k: guarantees[k]
                for k in (
                    "minimum_start",
                    "exclusions",
                    "reuse_completed",
                    "require_complete",
                )
            },
        )
        with app.store.transaction() as db:
            db.execute(
                "UPDATE observation_requests SET job_id=? WHERE id=?",
                (job["id"], request_id),
            )
    return {
        **job,
        "reuse": job.get("reuse", "resumed"),
        "request_id": request_id,
        "guarantees": guarantees,
    }


def cancel(app, request_id):
    """Cancel only this demand. Shared traversal and other requesters survive."""
    with app.store.transaction() as db:
        changed = db.execute(
            "UPDATE observation_requests SET state='cancelled' WHERE id=? AND profile=?",
            (request_id, app.profile),
        ).rowcount
    if not changed:
        raise CatabolicError("unknown_observation_request")
    return {"request_id": request_id, "state": "cancelled"}


def _guard_path(app, job):
    # Only canonical UUID job IDs can name a lock beside the database.
    identifier = str(UUID(job["id"]))
    return app.store.path.with_name(app.store.path.name + ".observation-" + identifier)


def _release_guard(app, job):
    close = app.store.after_close.pop("observation:" + job["id"], None)
    if close is not None:
        close()


def recover(app):
    from .database_io import CatalogBusy, acquire_writer_lock
    from .fallback_probe import alive

    with app.store.transaction() as db:
        for row in app.store.rows(
            "SELECT id,worker_pid,guarded FROM observation_jobs WHERE state='running'",
        ):
            if row["guarded"]:
                try:
                    fd = acquire_writer_lock(_guard_path(app, row))
                except CatalogBusy:
                    # This includes inherited child claims during PID handoff.
                    continue
                else:
                    os.close(fd)
            elif alive(row["worker_pid"]):
                continue
            db.execute(
                "UPDATE observation_jobs SET state='stale',lease_until=0,completed_at=? WHERE id=? AND state='running' AND worker_pid=?",
                (time.time(), row["id"], row["worker_pid"]),
            )


def claim(app, job):
    now = time.time()
    recover(app)
    acquired = False
    try:
        with app.store.transaction() as db:
            barriers = app.store.rows(
                "SELECT guarantees FROM observation_requests WHERE job_id=? AND state='active'",
                (job["id"],),
            )
            if any(
                json.loads(r["guarantees"])["minimum_start"] > now for r in barriers
            ):
                raise CatabolicError("observation_barrier")

            active = app.store.rows(
                "SELECT id,profile,source,worker_pid FROM observation_jobs WHERE state='running'"
            )
            if len(active) >= 4 or any(
                r["profile"] == app.profile and r["source"] == job["source"]
                for r in active
            ):
                raise CatabolicError("observation_capacity")
            from .database_io import acquire_writer_lock

            if not app.store.rows(
                "SELECT id FROM observation_jobs WHERE id=? AND state='queued'",
                (job["id"],),
            ):
                raise CatabolicError("observation_already_claimed")
            fd = acquire_writer_lock(_guard_path(app, job))

            def close():
                os.close(fd)

            close.fd = fd
            app.store.after_close["observation:" + job["id"]] = close
            acquired = True
            changed = db.execute(
                "UPDATE observation_jobs SET state='running',guarded=1,started_at=CASE WHEN scan_id IS NULL THEN ? ELSE started_at END,lease_until=?,worker_pid=?,claim_token=? WHERE id=? AND state='queued'",
                (now, now + 3600, os.getpid(), str(uuid4()), job["id"]),
            ).rowcount
            if not changed:
                _release_guard(app, job)
                raise CatabolicError("observation_already_claimed")
        return app.store.rows(
            "SELECT * FROM observation_jobs WHERE id=?", (job["id"],)
        )[0]

    except BaseException:
        if acquired:
            _release_guard(app, job)
        raise


def validate(app, job):
    rows = app.store.rows("SELECT * FROM observation_jobs WHERE id=?", (job["id"],))
    if (
        not rows
        or rows[0]["state"] != "running"
        or rows[0]["lease_until"] <= time.time()
        or job["profile"] != app.profile
        or any(
            rows[0][key] != job[key]
            for key in (
                "profile",
                "source",
                "compatibility",
                "exclusions",
                "generation",
                "worker_pid",
                "started_at",
                "claim_token",
            )
        )
        or not any(
            compatibility(
                app,
                job["source"],
                json.loads(job["exclusions"]),
                strict,
                json.loads(job["execution"]),
            )
            == job["compatibility"]
            for strict in (False, True)
        )
    ):
        raise CatabolicError("stale_observation_claim")


def renew(app, job):
    """Renew only the exact live owner and fence; an expired lease is not authority."""
    validate(app, job)
    with app.store.transaction() as db:
        changed = db.execute(
            "UPDATE observation_jobs SET lease_until=? WHERE id=? AND state='running' AND claim_token=? AND worker_pid=?",
            (time.time() + 3600, job["id"], job["claim_token"], job["worker_pid"]),
        ).rowcount
        if changed != 1:
            raise CatabolicError("stale_observation_claim")


def continue_request(app, request_id, *, scopes=None, extended=False, budgets=None):
    """An explicit amended request; committed scope history stays inspectable."""
    with app.store.transaction():
        return _continue_request(
            app, request_id, scopes=scopes, extended=extended, budgets=budgets
        )


def _continue_request(app, request_id, *, scopes, extended, budgets):
    rows = app.store.rows(
        "SELECT r.*,j.state AS job_state,j.execution,j.compatibility,j.exclusions,j.dirty_generation FROM observation_requests r JOIN observation_jobs j ON j.id=r.job_id WHERE r.id=? AND r.profile=?",
        (request_id, app.profile),
    )
    if not rows:
        raise CatabolicError("unknown_observation_request")
    old = rows[0]
    if old["job_state"] in ("queued", "running"):
        raise CatabolicError("observation still active; resume the existing request")
    guarantees = json.loads(old["guarantees"])
    execution = json.loads(old["execution"])
    if (
        compatibility(
            app,
            old["source"],
            json.loads(old["exclusions"]),
            guarantees["require_complete"],
            execution,
        )
        != old["compatibility"]
    ):
        raise CatabolicError(
            "observation policy or binding changed; request a new scan"
        )
    watermark = app.store.rows(
        "SELECT dirty_generation FROM observation_sources WHERE profile=? AND source=?",
        (app.profile, old["source"]),
    )
    if watermark and watermark[0]["dirty_generation"] > old["dirty_generation"]:
        raise CatabolicError("source changed since observation; request a new scan")
    # Continue all outstanding scopes by default, retaining completed coverage.
    chosen = None if scopes is None else {relative_path(p) if p else "" for p in scopes}
    if chosen is not None:
        for path in chosen:
            if not app.store.rows(
                "SELECT 1 FROM observation_scopes WHERE job_id=? AND path=? AND state NOT IN ('complete','excluded')",
                (old["job_id"], path),
            ):
                raise CatabolicError("scope is not outstanding")
    result = request(
        app,
        old["source"],
        after=guarantees["minimum_start"],
        reuse_completed=False,
        exclusions=guarantees["exclusions"],
        require_complete=guarantees["require_complete"],
        extended=extended,
        budgets=budgets,
        scopes=execution["scopes"],
        parent_job=old["job_id"],
        continuation_scopes=sorted(chosen) if chosen is not None else None,
    )
    with app.store.transaction() as db:
        db.execute(
            "INSERT OR IGNORE INTO meta SELECT ?,value FROM meta WHERE key=?",
            (f"observation:{result['id']}:root", f"observation:{old['job_id']}:root"),
        )
        db.execute(
            "INSERT OR IGNORE INTO observation_scopes SELECT ?,path,CASE WHEN state NOT IN ('complete','excluded') THEN 'pending' ELSE state END,detail,updated_at FROM observation_scopes WHERE job_id=?",
            (result["id"], old["job_id"]),
        )
        if chosen is not None:
            for row in db.execute(
                "SELECT path FROM observation_scopes WHERE job_id=? AND state='pending'",
                (result["id"],),
            ).fetchall():
                if not any(
                    row["path"] == p or row["path"].startswith(p + "/") for p in chosen
                ):
                    db.execute(
                        "UPDATE observation_scopes SET state='deferred',detail=? WHERE job_id=? AND path=?",
                        (
                            encode({"reason": "not_selected_for_continuation"}),
                            result["id"],
                            row["path"],
                        ),
                    )
    return result


def finish(db, job, report):
    db.execute(
        "UPDATE observation_jobs SET state=?,completed_at=?,lease_until=0,scan_id=?,report=? WHERE id=?",
        (
            "complete"
            if report["complete"]
            else (
                "unavailable"
                if report.get("availability") == "unavailable"
                else "failed"
            ),
            time.time(),
            report["scan_id"],
            encode(report),
            job["id"],
        ),
    )


def scan_report(evidence):
    """Legacy scan shape, including honest pending/failed outcomes."""
    return evidence.get("report") or {
        "scan_id": None,
        "location": evidence["source"],
        "complete": False,
        "observed": 0,
        "published": 0,
        "excluded": json.loads(evidence["exclusions"]),
        "availability": "unknown",
        "inventory_retained": True,
        "errors": [evidence.get("blocker", "observation_" + evidence["state"])],
    }


def _result(app, job):
    current = app.store.rows("SELECT * FROM observation_jobs WHERE id=?", (job["id"],))[
        0
    ]
    result = {
        **job,
        **current,
        "report": json.loads(current["report"]) if current["report"] else None,
        "coverage": {
            "source": current["source"],
            "exclusions": json.loads(current["exclusions"]),
            "method": "guarded-inventory-v2"
            if json.loads(current["execution"] or "{}")
            else "full-inventory-v1",
            "execution": json.loads(current["execution"] or "{}"),
            "dirty_generation": current["dirty_generation"],
        },
    }
    if result["report"] is None:
        result["report"] = scan_report(result)
        if current["scan_id"]:
            result["report"].update(
                app.store.rows(
                    "SELECT id AS scan_id,observed,observed AS published FROM scans WHERE id=?",
                    (current["scan_id"],),
                )[0]
            )
            result["report"]["progress"] = app.store.rows(
                "SELECT count(*) AS batches,coalesce(sum(bytes),0) AS committed_bytes,max(committed_at) AS last_progress_at FROM observation_batches WHERE job_id=?",
                (current["id"],),
            )[0]
            result["report"]["coverage"] = {
                r["state"]: r["n"]
                for r in app.store.rows(
                    "SELECT state,count(*) AS n FROM observation_scopes WHERE job_id=? GROUP BY state",
                    (current["id"],),
                )
            }
    result["job_state"] = current["state"]
    result["retry_state"] = (
        "pending"
        if current["state"] in ("queued", "running")
        else "retryable"
        if current["state"] in ("stale", "failed", "unavailable")
        else None
    )
    if result["report"].get("deferred"):
        result["retry_state"] = "explicit_continuation_required"
    if job.get("request_id"):
        result["request_state"] = app.store.rows(
            "SELECT state FROM observation_requests WHERE id=? AND profile=?",
            (job["request_id"], app.profile),
        )[0]["state"]
        if result["request_state"] == "cancelled":
            result["state"] = "cancelled"
    return result


def execute(app, job, *, isolate=False, timeout=30):
    """Execute admitted work only. Never calls the public scan/request API."""
    if job.get("request_id"):
        demand = app.store.rows(
            "SELECT state FROM observation_requests WHERE id=? AND profile=?",
            (job["request_id"], app.profile),
        )
        if not demand:
            raise CatabolicError("unknown_observation_request")
        if demand[0]["state"] == "cancelled":
            return _result(app, job)
    if job["profile"] != app.profile:
        raise CatabolicError("foreign_observation_request")
    if job["state"] != "queued":
        return _result(app, job)
    if time.time() < job.get("guarantees", {}).get("minimum_start", 0):
        return {**job, "blocker": "observation_barrier"}
    try:
        claimed = claim(app, job)
    except CatabolicError as exc:
        if str(exc) not in (
            "observation_capacity",
            "observation_already_claimed",
            "observation_barrier",
        ):
            raise
        return {**_result(app, job), "blocker": str(exc)}
    claimed = {**job, **claimed}
    try:
        return _execute_claimed(app, claimed, isolate=isolate, timeout=timeout)
    finally:
        _release_guard(app, claimed)


def _execute_claimed(app, claimed, *, isolate, timeout):
    job = claimed
    if not isolate:
        try:
            app._execute_observation(claimed)
        except BaseException as exc:
            if app.store.db is None:
                raise
            with app.store.transaction() as db:
                db.execute(
                    "UPDATE observation_jobs SET state='failed',completed_at=?,lease_until=0,report=? WHERE id=? AND state='running' AND worker_pid=?",
                    (
                        time.time(),
                        encode(
                            {
                                "complete": False,
                                "scan_id": None,
                                "location": job["source"],
                                "availability": "unknown",
                                "inventory_retained": True,
                                "errors": [str(exc)],
                                "resume_progress": True,
                            }
                        ),
                        job["id"],
                        os.getpid(),
                    ),
                )
            raise
        return _result(app, claimed)
    import subprocess
    import sys
    import tempfile

    child = None
    try:
        with tempfile.TemporaryFile() as output:
            with app.store.detached():
                child = subprocess.Popen(
                    [
                        sys.executable,
                        "-m",
                        "catabolic.observations",
                        str(app.store.path),
                        app.profile,
                        job["id"],
                    ],
                    stdout=output,
                    stderr=output,
                    pass_fds=(app.store.after_close["observation:" + job["id"]].fd,),
                )
                # Timeout bounds this requester's wait, not the shared worker's lifetime.
                try:
                    child.wait(timeout=timeout)
                except subprocess.TimeoutExpired:
                    pass
            # If the child has not opened the catalog yet, transfer the reservation
            # here too. A failed launch must never leave a live parent's PID pinned.
            with app.store.transaction() as db:
                db.execute(
                    "UPDATE observation_jobs SET worker_pid=? WHERE id=? AND state='running' AND worker_pid=?",
                    (child.pid, job["id"], os.getpid()),
                )
            # Retain Popen to reap without cancelling useful shared work.
            if child.poll() is None:
                _children.append(child)
            elif child.returncode:
                recover(app)
            return _result(app, claimed)

    except BaseException as exc:
        if app.store.db is not None and child is not None:
            with app.store.transaction() as db:
                db.execute(
                    "UPDATE observation_jobs SET worker_pid=? WHERE id=? AND state='running' AND worker_pid=?",
                    (child.pid, job["id"], os.getpid()),
                )
            _children.append(child)
            recover(app)
        elif app.store.db is not None:
            with app.store.transaction() as db:
                db.execute(
                    "UPDATE observation_jobs SET state='failed',lease_until=0,completed_at=?,report=? WHERE id=? AND state='running' AND worker_pid=?",
                    (
                        time.time(),
                        encode(
                            {
                                "complete": False,
                                "scan_id": None,
                                "availability": "unknown",
                                "inventory_retained": True,
                                "errors": [str(exc)],
                            }
                        ),
                        job["id"],
                        os.getpid(),
                    ),
                )
        raise


_children = []


def observe(
    app,
    source,
    *,
    max_age=60,
    after=0,
    exclusions=(),
    reuse_completed=True,
    require_complete=False,
    requester=None,
    request_id=None,
    isolate=False,
    timeout=30,
    wait=False,
    extended=False,
    budgets=None,
    scopes=None,
):
    """One-shot service: admit/resume, reuse/join, execute, optionally wait."""
    _children[:] = [child for child in _children if child.poll() is None]
    job = (
        resume(app, request_id)
        if request_id
        else request(
            app,
            source,
            max_age=max_age,
            after=after,
            exclusions=exclusions,
            reuse_completed=reuse_completed,
            require_complete=require_complete,
            requester=requester,
            extended=extended,
            budgets=budgets,
            scopes=scopes,
        )
    )
    if job["source"] != source:
        raise CatabolicError("observation_request_source_mismatch")
    if job["state"] == "cancelled":
        return job
    deadline = time.monotonic() + timeout
    while True:
        result = execute(
            app, job, isolate=isolate, timeout=max(0.01, deadline - time.monotonic())
        )
        if (
            result["state"] not in ("queued", "running")
            or not wait
            or time.monotonic() >= deadline
        ):
            return result
        with app.store.detached():
            time.sleep(0.05)
        job = resume(app, job["request_id"])


def inventory_signature(app, source):
    digest = hashlib.sha256()
    for row in app.store.db.execute(
        "SELECT f.id,o.size,o.mtime_ns,o.device,o.inode FROM files f JOIN observations o ON o.file_id=f.id WHERE o.profile=? AND f.location=? AND o.status='present' ORDER BY f.id",
        (app.profile, source),
    ):
        digest.update(encode(list(row)).encode())
    return digest.hexdigest()


if __name__ == "__main__":
    import sys

    from .app import Application
    from .database_io import is_catalog_busy
    from .store import Store

    for attempt in range(100):
        try:
            with Store(sys.argv[1], writable=True) as store:
                app = Application(store, sys.argv[2])
                job = store.rows(
                    "SELECT * FROM observation_jobs WHERE id=?", (sys.argv[3],)
                )[0]
                validate(app, job)
                with store.transaction() as db:
                    db.execute(
                        "UPDATE observation_jobs SET worker_pid=? WHERE id=?",
                        (os.getpid(), job["id"]),
                    )
                job["worker_pid"] = os.getpid()
                app._execute_observation(job)
            break
        except CatabolicError as exc:
            if not is_catalog_busy(exc) or attempt == 99:
                raise
            time.sleep(0.05)
