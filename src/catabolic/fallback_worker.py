# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit supervised fallback health checks, independent of all-source scans."""

import os
import sqlite3
import time

from .app import Application
from .database_io import acquire_writer_lock, database_path, is_catalog_busy
from .domain import CatabolicError
from .fallback_projection import FallbackProjection, binding
from .reconcile import Reconciler
from .store import Store


def configure(app, catalog, *, enabled, interval=60, max_changes=100, max_removals=0):
    if not binding(app.store, app.profile, catalog):
        raise CatabolicError("bind an explicit fallback projection first")
    if (
        not 1 <= interval <= 86400
        or not 1 <= max_changes <= 10000
        or not 0 <= max_removals <= 10000
    ):
        raise CatabolicError("invalid fallback maintenance limits")
    with app.store.transaction() as db:
        db.execute(
            "UPDATE fallback_bindings SET enabled=?,interval_seconds=?,max_changes=?,max_removals=?,next_attempt=0 WHERE profile=? AND catalog=?",
            (int(enabled), interval, max_changes, max_removals, app.profile, catalog),
        )
        db.execute(
            "INSERT INTO catalog_refresh_settings VALUES (?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET enabled=excluded.enabled,max_removals=excluded.max_removals",
            (app.profile, catalog, int(enabled), max_removals),
        )
    return binding(app.store, app.profile, catalog)


def tick(database, profile="default", *, limit=10):
    results = []
    with Store(database, writable=True) as store:
        app = Application(store, profile)
        rows = store.rows(
            "SELECT * FROM fallback_bindings WHERE profile=? AND enabled=1 AND next_attempt<=? ORDER BY next_attempt,catalog LIMIT ?",
            (profile, time.time(), limit),
        )
        for config in rows:
            catalog = config["catalog"]
            if store.rows(
                "SELECT 1 FROM watcher_owners WHERE profile=? AND catalog=?",
                (profile, catalog),
            ):
                continue
            try:
                if store.rows(
                    "SELECT 1 FROM journal WHERE profile=? AND catalog=?",
                    (profile, catalog),
                ):
                    Reconciler(app).recover(catalog)
                result = FallbackProjection(app).run(
                    catalog,
                    apply=True,
                    automatic=True,
                    max_changes=config["max_changes"],
                    max_removals=config["max_removals"],
                )
                results.append(
                    {
                        "catalog": catalog,
                        "complete": result.get("complete", False),
                        "states": [d["state"] for d in result["desired"]],
                    }
                )
            except (CatabolicError, OSError) as exc:
                results.append(
                    {"catalog": catalog, "complete": False, "error": str(exc)}
                )
            with store.transaction() as db:
                db.execute(
                    "UPDATE fallback_bindings SET next_attempt=? WHERE profile=? AND catalog=?",
                    (time.time() + config["interval_seconds"], profile, catalog),
                )

    return {
        "processed": len(results),
        "complete": all(r["complete"] for r in results),
        "projections": results,
    }


def run(database, profile="default", *, once=False, interval=1):
    path = database_path(database)
    lock = acquire_writer_lock(path.with_name(path.name + ".fallback-worker"))
    try:
        while True:
            try:
                result = tick(path, profile)
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
        os.close(lock)
