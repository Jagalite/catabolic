# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Compatible full-source observation demand and fenced scanner publication."""

import hashlib
import json
import os
import time
from uuid import uuid4

from .domain import CatabolicError
from .source_trust import source_policy
from .store import encode


def compatibility(app, source, exclusions=()):
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
                "method": "full-inventory-v1",
            }
        ).encode()
    ).hexdigest()


def dirty(app, source):
    with app.store.transaction() as db:
        db.execute(
            "INSERT INTO observation_sources VALUES (?,?,1) ON CONFLICT(profile,source) DO UPDATE SET dirty_generation=dirty_generation+1",
            (app.profile, source),
        )


def request(app, source, *, max_age=0, after=0, exclusions=(), reuse_completed=True):
    now = time.time()
    key = compatibility(app, source, exclusions)
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
            if (
                reuse_completed
                and row["state"] in ("complete", "unavailable")
                and row["started_at"] >= max(after, now - max_age)
            ):
                return {**row, "reuse": "completed"}
            if row["state"] in ("queued", "running") and (
                row["state"] == "queued"
                or (
                    row["lease_until"] > now
                    and row["started_at"] >= max(after, now - max_age)
                )
            ):
                return {**row, "reuse": "inflight"}
        generation = db.execute(
            "SELECT coalesce(max(generation),0)+1 FROM observation_jobs WHERE profile=? AND source=?",
            (app.profile, source),
        ).fetchone()[0]
        identifier = str(uuid4())
        db.execute(
            "INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at) VALUES (?,?,?,?,?,?,?,'queued',?)",
            (
                identifier,
                app.profile,
                source,
                key,
                encode(list(exclusions)),
                watermark,
                generation,
                now,
            ),
        )
    return {
        **app.store.rows("SELECT * FROM observation_jobs WHERE id=?", (identifier,))[0],
        "reuse": "created",
    }


def claim(app, job):
    now = time.time()
    with app.store.transaction() as db:
        from .fallback_probe import alive

        active = [
            r
            for r in app.store.rows(
                "SELECT id,profile,source,worker_pid FROM observation_jobs WHERE state='running'"
            )
            if alive(r["worker_pid"])
        ]
        if len(active) >= 4 or any(
            r["profile"] == app.profile and r["source"] == job["source"] for r in active
        ):
            raise CatabolicError("observation_capacity")
        changed = db.execute(
            "UPDATE observation_jobs SET state='running',started_at=?,lease_until=?,worker_pid=? WHERE id=? AND state='queued'",
            (now, now + 3600, os.getpid(), job["id"]),
        ).rowcount
        if not changed:
            raise CatabolicError("observation_already_claimed")
    return app.store.rows("SELECT * FROM observation_jobs WHERE id=?", (job["id"],))[0]


def validate(app, job):
    rows = app.store.rows("SELECT * FROM observation_jobs WHERE id=?", (job["id"],))
    latest = app.store.rows(
        "SELECT max(generation) AS generation FROM observation_jobs WHERE profile=? AND source=? AND state!='queued'",
        (app.profile, job["source"]),
    )[0]["generation"]
    if (
        not rows
        or rows[0]["state"] != "running"
        or rows[0]["lease_until"] <= time.time()
        or latest != job["generation"]
        or compatibility(app, job["source"], json.loads(job["exclusions"]))
        != job["compatibility"]
    ):
        raise CatabolicError("stale_observation_claim")


def finish(db, job, report):
    db.execute(
        "UPDATE observation_jobs SET state=?,completed_at=?,lease_until=0,scan_id=?,report=? WHERE id=?",
        (
            "complete" if report["complete"] else "unavailable",
            time.time(),
            report["scan_id"],
            encode(report),
            job["id"],
        ),
    )


def observe(app, source, *, max_age=60, after=0, isolate=False, timeout=30):
    job = request(app, source, max_age=max_age, after=after)
    if job["reuse"] == "completed":
        return {**job, "report": json.loads(job["report"])}
    if job["state"] == "running":
        if time.time() - job["started_at"] >= timeout:
            return {
                **job,
                "state": "unavailable",
                "report": {
                    "complete": False,
                    "availability": "unknown",
                    "inventory_retained": True,
                    "errors": ["observation_still_running"],
                },
            }
        return job
    if not isolate:
        report = app.scan(source, _observation=job)["scans"][0]
        return {
            **job,
            "state": "complete" if report["complete"] else "unavailable",
            "report": report,
        }
    import subprocess
    import sys
    import tempfile

    try:
        job = claim(app, job)
    except CatabolicError as exc:
        if str(exc) != "observation_capacity":
            raise
        return {
            **job,
            "state": "unavailable",
            "report": {
                "complete": False,
                "availability": "unknown",
                "inventory_retained": True,
                "errors": ["observation_capacity"],
            },
        }
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
            )
            try:
                child.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                child.kill()
                try:
                    child.wait(timeout=0.1)
                except subprocess.TimeoutExpired:
                    # The durable PID remains a capacity reservation. Never start
                    # another traversal of this source while it is still alive.
                    pass
        current = app.store.rows(
            "SELECT * FROM observation_jobs WHERE id=?", (job["id"],)
        )[0]
        if current["state"] not in ("complete", "unavailable"):
            report = {
                "complete": False,
                "scan_id": None,
                "location": source,
                "errors": ["scan_timeout_or_failure"],
                "availability": "unknown",
                "inventory_retained": True,
            }
            with app.store.transaction() as db:
                if child.poll() is not None:
                    finish(db, job, report)
            return {
                **current,
                "state": "unavailable",
                "report": report,
                "reuse": job.get("reuse", "created"),
            }
        return {**current, "report": json.loads(current["report"]), "reuse": "created"}


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
                app.scan(
                    job["source"],
                    exclude=json.loads(job["exclusions"]),
                    _observation=job,
                )
            break
        except CatabolicError as exc:
            if not is_catalog_busy(exc) or attempt == 99:
                raise
            time.sleep(0.05)
