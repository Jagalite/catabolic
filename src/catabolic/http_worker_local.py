# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit supervised local API worker; normal artifacts own execution."""

import os
import sqlite3
import time

from . import rendering
from .access import AccessError, authenticate
from .api_events import prune
from .app import Application
from .artifacts import Artifacts
from .database_io import acquire_writer_lock, database_path, is_catalog_busy
from .domain import CatabolicError
from .execution_claims import active
from .rendition_requests import status
from .store import Store, encode


def run(database, profile="default", *, once=False, interval=2):
    path = database_path(database)
    lock = acquire_writer_lock(path.with_name(path.name + ".api-worker"))
    try:
        capabilities = rendering.capabilities()
        while True:
            try:
                result = tick(path, profile, capabilities)
            except (CatabolicError, sqlite3.Error) as exc:
                if not is_catalog_busy(exc):
                    raise
                result = {
                    "processed": 0,
                    "complete": False,
                    "blockers": ["catalog_busy"],
                }
            if once:
                return result
            time.sleep(interval)
    finally:
        try:
            with Store(path, writable=True) as store:
                with store.transaction() as db:
                    db.execute(
                        "DELETE FROM api_worker_status WHERE profile=? AND worker_pid=?",
                        (profile, os.getpid()),
                    )
        except (CatabolicError, sqlite3.Error) as exc:
            if not is_catalog_busy(exc):
                raise
        finally:
            os.close(lock)


def tick(path, profile, capabilities):
    with Store(path, writable=True) as store:
        with store.transaction() as db:
            db.execute(
                "INSERT INTO api_worker_status VALUES (?,?,?,?) ON CONFLICT(profile) DO UPDATE SET worker_pid=excluded.worker_pid,capabilities=excluded.capabilities,updated_at=excluded.updated_at",
                (profile, os.getpid(), encode(capabilities), time.time()),
            )
        prune(store)
        rows = store.rows(
            "SELECT r.* FROM api_requests r JOIN processing_jobs j ON j.id=r.job_id WHERE r.profile=? AND r.state IN ('queued','running','validating','blocked','cancelled') AND j.state IN ('queued','running') ORDER BY r.created_at,r.id LIMIT 100",
            (profile,),
        )
        selected = None
        for row in rows:
            try:
                access = authenticate(store, token_id=row["token_id"])
                report = status(access, row["id"])
                if report["state"] == "stale":
                    with store.transaction() as db:
                        changed = db.execute(
                            "UPDATE processing_jobs SET state='changed' WHERE id=? AND state='queued'",
                            (row["job_id"],),
                        ).rowcount
                    if changed:
                        continue
                enabled = store.rows(
                    "SELECT 1 FROM api_operations WHERE id=json_extract(?,'$.operation_id') AND enabled=1",
                    (row["body"],),
                )
                if not enabled:
                    raise AccessError("operation_disabled")
            except AccessError:
                with store.transaction() as db:
                    db.execute(
                        "UPDATE api_requests SET state='blocked' WHERE id=? AND state!='cancelled'",
                        (row["id"],),
                    )
                continue
            selected = row["job_id"]
            break
        result = {"processed": 0}
        if selected and not any(
            active(store, r["job_id"])
            for r in store.rows("SELECT job_id FROM execution_claims")
        ):
            artifacts = Artifacts(Application(store, profile))
            artifacts.recover()
            with store.transaction() as db:
                db.execute(
                    "UPDATE processing_jobs SET state='queued' WHERE id=? AND state='running'",
                    (selected,),
                )
            result = artifacts.run(limit=1, job_ids=[selected])
    return result
