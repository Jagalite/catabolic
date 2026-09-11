# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Bounded supervised watcher processes; no catalog session while waiting."""

import json
import os
import subprocess
import sys
import tempfile
import time

from .app import Application
from .database_io import acquire_writer_lock, database_path
from .store import Store
from .watchers import Watchers


def due(row, now):
    value = json.loads(row["definition"])
    explicit = row["requested_generation"] > row["completed_generation"]
    if not row["enabled"] and not explicit:
        return False
    if row["active_run"] and row["lease_until"] > now:
        return False
    if row["retry_at"] > now:
        return False
    if explicit or row["pending_reaction"] or row["retry_at"]:
        return True
    if value["schedule"]["kind"] != "manual" and row["next_due"] <= now:
        return True
    return bool(
        value["events"]
        and row["pending_generation"] > row["completed_generation"]
        and row["event_first"] is not None
        and now
        >= min(
            row["event_last"] + value["debounce_seconds"],
            row["event_first"] + value["maximum_delay_seconds"],
        )
    )


def tick(path, profile="default", *, limit=100, workers=4):
    with Store(path, writable=True) as store:
        rows = store.rows(
            "SELECT w.*,d.definition FROM watchers w JOIN watcher_definitions d ON d.id=w.definition_id WHERE w.profile=? AND (enabled=1 OR requested_generation>completed_generation) ORDER BY coalesce(last_run,''),next_due,name LIMIT 1000",
            (profile,),
        )
        names = [r["name"] for r in rows if due(r, time.time())][:limit]
    active, results = [], []
    while names or active:
        while names and len(active) < workers:
            watcher = names.pop(0)
            output = tempfile.TemporaryFile(mode="w+")
            process = subprocess.Popen(
                [
                    sys.executable,
                    "-m",
                    "catabolic.supervisor",
                    str(path),
                    profile,
                    watcher,
                ],
                stdout=output,
                stderr=output,
                text=True,
            )
            active.append((watcher, process, output, time.monotonic()))
        for watcher, process, output, started in list(active):
            if process.poll() is None and time.monotonic() - started > 120:
                process.kill()
            if process.poll() is not None:
                output.seek(0)
                data = output.read(8 * 1024 * 1024)
                output.close()
                results.append(
                    json.loads(data)
                    if process.returncode == 0
                    else {
                        "watcher": watcher,
                        "complete": False,
                        "error": data[-2000:] or "watcher process interrupted",
                    }
                )
                active.remove((watcher, process, output, started))
        if active:
            time.sleep(0.05)
    return {"runs": results, "complete": all(r["complete"] for r in results)}


def run(path, profile="default", *, once=False, native=False):
    path = database_path(path)
    descriptor = acquire_writer_lock(path.with_name(path.name + ".supervisor"))
    monitor = None
    try:
        from .source_events import startup

        with Store(path, writable=True) as store:
            app = Application(store, profile)
            recover(app)
            if not once or native:
                startup(app)
        if native:
            from .native_monitor import NativeMonitor

            with Store(path) as store:
                roots = {
                    r["owner"]: r["root"]
                    for r in store.rows(
                        "SELECT owner,root FROM bindings WHERE profile=? AND kind='source' AND owner NOT IN (SELECT location FROM generated_locations WHERE profile=?)",
                        (profile, profile),
                    )
                }
                excluded = [
                    r["root"]
                    for r in store.rows(
                        "SELECT root FROM bindings WHERE profile=? AND (kind!='source' OR owner IN (SELECT location FROM generated_locations WHERE profile=?))",
                        (profile, profile),
                    )
                ]
            monitor = NativeMonitor(roots, excluded)
            monitor.start()
        while True:
            if monitor:
                from .source_events import invalidate

                pending = monitor.drain()
                if pending:
                    with Store(path, writable=True) as store:
                        for source in pending:
                            invalidate(Application(store, profile), source)
            report = tick(path, profile)
            if once:
                return report
            time.sleep(1)
    finally:
        if monitor:
            monitor.close()
        os.close(descriptor)


def recover(app):
    """Only dead local processes lose their leases before expiry."""

    def alive(pid):
        if not pid:
            return True
        try:
            os.kill(pid, 0)
            return True
        except ProcessLookupError:
            return False
        except PermissionError:
            return True

    with app.store.transaction() as db:
        for row in app.store.rows(
            "SELECT w.name,w.active_run,r.worker_pid FROM watchers w JOIN watcher_runs r ON r.id=w.active_run WHERE w.profile=?",
            (app.profile,),
        ):
            if not alive(row["worker_pid"]):
                db.execute(
                    "UPDATE watcher_runs SET state='interrupted',completed_at=? WHERE id=?",
                    (time.time(), row["active_run"]),
                )
                db.execute(
                    "UPDATE watchers SET active_run=NULL,lease_until=0,retry_at=? WHERE profile=? AND name=?",
                    (time.time(), app.profile, row["name"]),
                )
        for row in app.store.rows(
            "SELECT id,worker_pid FROM observation_jobs WHERE profile=? AND state='running'",
            (app.profile,),
        ):
            if not alive(row["worker_pid"]):
                db.execute(
                    "UPDATE observation_jobs SET state='stale',lease_until=0,completed_at=? WHERE id=?",
                    (time.time(), row["id"]),
                )


if __name__ == "__main__":
    from .domain import CatabolicError

    for attempt in range(100):
        try:
            with Store(sys.argv[1], writable=True) as store:
                result = Watchers(Application(store, sys.argv[2])).run(
                    sys.argv[3], trigger="scheduled"
                )
            print(json.dumps(result))
            break
        except CatabolicError as exc:
            from .database_io import is_catalog_busy

            if not is_catalog_busy(exc) or attempt == 99:
                raise
            time.sleep(0.05)
