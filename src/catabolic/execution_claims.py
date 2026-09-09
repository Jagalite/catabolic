# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Fenced local execution shared by CLI, maintenance and HTTP workers."""

import os
import secrets
import threading
import time
from contextlib import contextmanager

from .domain import CatabolicError
from .store import Store

LEASE_SECONDS = 60


def live(row):
    if not row or row["lease_until"] <= time.time():
        return False
    try:
        os.kill(row["worker_pid"], 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def active(store, job_id):
    rows = store.rows("SELECT * FROM execution_claims WHERE job_id=?", (job_id,))
    return live(rows[0]) if rows else False


def claim(store, job_id):
    with store.transaction() as db:
        old = db.execute(
            "SELECT * FROM execution_claims WHERE job_id=?", (job_id,)
        ).fetchone()
        if live(old):
            raise CatabolicError("job is already owned by a live worker")
        job = db.execute(
            "SELECT * FROM processing_jobs WHERE id=?", (job_id,)
        ).fetchone()
        if not job or job["state"] != "running":
            raise CatabolicError("only a running attempt can be claimed")
        value = dict(
            job_id=job_id,
            generation=(old["generation"] + 1 if old else 1),
            token=secrets.token_hex(32),
            worker_pid=os.getpid(),
            attempt=job["attempts"],
            lease_until=time.time() + LEASE_SECONDS,
            snapshot=job["snapshot"],
            options=job["options"],
        )
        db.execute(
            "INSERT OR REPLACE INTO execution_claims VALUES (:job_id,:generation,:token,:worker_pid,:attempt,:lease_until,:snapshot,:options)",
            value,
        )
    return value


def fence(store, value):
    rows = store.rows(
        "SELECT c.*,j.state,j.attempts,j.snapshot AS current_snapshot,j.options AS current_options FROM execution_claims c JOIN processing_jobs j ON j.id=c.job_id WHERE c.job_id=?",
        (value["job_id"],),
    )
    if (
        not rows
        or not live(rows[0])
        or any(
            rows[0][key] != value[key]
            for key in ("token", "generation", "attempt", "snapshot", "options")
        )
        or rows[0]["state"] != "running"
        or rows[0]["attempts"] != value["attempt"]
        or rows[0]["current_snapshot"] != value["snapshot"]
        or rows[0]["current_options"] != value["options"]
    ):
        raise CatabolicError("execution claim expired, cancelled or replaced")


def release(store, value):
    with store.transaction() as db:
        db.execute(
            "UPDATE execution_claims SET lease_until=0 WHERE job_id=? AND token=?",
            (value["job_id"], value["token"]),
        )


@contextmanager
def detached(store, values):
    """Heartbeat while slow tools run with no catalog session or writer lock."""
    stop = threading.Event()

    def heartbeat():
        while not stop.wait(5):
            try:
                with Store(store.path, writable=True) as session:
                    for value in values:
                        fence(session, value)
                    with session.transaction() as db:
                        for value in values:
                            db.execute(
                                "UPDATE execution_claims SET lease_until=? WHERE job_id=? AND token=?",
                                (
                                    time.time() + LEASE_SECONDS,
                                    value["job_id"],
                                    value["token"],
                                ),
                            )
            except (CatabolicError, OSError):
                # Lost claims cannot be revived. The final fence rejects their results.
                continue

    with store.detached():
        thread = threading.Thread(target=heartbeat, daemon=True)
        thread.start()
        try:
            yield
        finally:
            stop.set()
            thread.join()
    for value in values:
        fence(store, value)
    with store.transaction() as db:
        for value in values:
            db.execute(
                "UPDATE execution_claims SET lease_until=? WHERE job_id=? AND token=?",
                (time.time() + LEASE_SECONDS, value["job_id"], value["token"]),
            )
