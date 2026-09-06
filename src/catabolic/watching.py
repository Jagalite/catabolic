"""Portable polling trigger; writer locks are released between cycles."""

import hashlib
import time

from .app import Application
from .domain import CatabolicError
from .processing import Processing
from .store import Store, encode


def cycle(
    app,
    *,
    operation="probe",
    location=None,
    settle=30,
    workers=2,
    batch=1000,
    retry_transient=0,
    retry_delay=30,
):
    if type(settle) is not int or settle < 0 or settle > 86400:
        raise CatabolicError("settle must be 0..86400 seconds")
    if type(batch) is not int or not 1 <= batch <= 1000:
        raise CatabolicError("batch must be 1..1000")
    report = app.scan(location)
    now = time.time()
    enqueued = cached = 0
    errors = []
    for scan in report["scans"]:
        if not scan["complete"]:
            continue
        after = ""
        while True:
            rows = app.store.rows(
                """SELECT o.file_id,o.size,o.mtime_ns,o.device,o.inode,o.status,
                b.root,b.device AS root_device,b.inode AS root_inode FROM observations o
                JOIN files f ON f.id=o.file_id JOIN bindings b ON b.profile=o.profile AND b.kind='source' AND b.owner=f.location
                WHERE o.profile=? AND f.location=? AND o.file_id>? ORDER BY o.file_id LIMIT ?""",
                (app.profile, scan["location"], after, batch),
            )
            if not rows:
                break
            with app.store.transaction() as db:
                for row in rows:
                    revision = hashlib.sha256(encode(row).encode()).hexdigest()
                    db.execute(
                        """INSERT INTO watch_revisions VALUES (?,?,?,?) ON CONFLICT(profile,file_id) DO UPDATE SET
                        revision=excluded.revision,stable_since=CASE WHEN watch_revisions.revision=excluded.revision THEN watch_revisions.stable_since ELSE excluded.stable_since END""",
                        (app.profile, row["file_id"], revision, now),
                    )
            ids = [
                r["file_id"]
                for r in rows
                if r["status"] == "present"
                and app.store.db.execute(
                    "SELECT stable_since FROM watch_revisions WHERE profile=? AND file_id=?",
                    (app.profile, r["file_id"]),
                ).fetchone()[0]
                <= now - settle
            ]
            if ids:
                result = Processing(app).enqueue(operation, file_ids=ids)
                enqueued += len(result["queued"])
                cached += len(result["cached"])
                errors.extend(result["errors"][: max(0, 100 - len(errors))])
            after = rows[-1]["file_id"]
    processed = Processing(app).run(
        workers=workers,
        limit=batch,
        retry_transient=retry_transient,
        retry_delay=retry_delay,
    )
    return {
        "scan": report,
        "queued": enqueued,
        "cached": cached,
        "errors": errors,
        "processing": processed,
        "complete": report["complete"] and not errors and processed["complete"],
    }


def watch(
    path,
    profile,
    *,
    operation="probe",
    location=None,
    settle=30,
    workers=2,
    batch=1000,
    interval=30,
    cycles=1,
    progress=None,
    retry_transient=0,
    retry_delay=30,
):
    if type(interval) is not int or not 1 <= interval <= 86400:
        raise CatabolicError("interval must be 1..86400 seconds")
    if type(cycles) is not int or not 0 <= cycles <= 1000000:
        raise CatabolicError("cycles must be 0 (continuous) or 1..1000000")
    count = 0
    last = None
    while cycles == 0 or count < cycles:
        with Store(path, writable=True) as store:
            last = cycle(
                Application(store, profile),
                operation=operation,
                location=location,
                settle=settle,
                workers=workers,
                batch=batch,
                retry_transient=retry_transient,
                retry_delay=retry_delay,
            )
        count += 1
        if progress:
            progress({"cycle": count, **last})
        if cycles == 0 or count < cycles:
            time.sleep(interval)
    return {"cycles": count, "last": last, "complete": last["complete"]}
