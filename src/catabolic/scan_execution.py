# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Fenced incremental publication for the admitted observation executor."""

import json
import os
import shutil
import sqlite3
import tempfile
import time
from uuid import UUID, uuid4, uuid5

from .domain import CatabolicError
from .filesystem import root_handle
from .scan_traversal import reopen, storage_exhausted
from .store import Store, encode


class PublicationStopped(RuntimeError):
    pass


def writer(path):
    from .database_io import CatalogBusy

    deadline = time.monotonic() + 5
    while True:
        try:
            return Store(path, writable=True)
        except CatalogBusy:
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.02)


class Publisher:
    def __init__(self, app, job, binding):
        self.app, self.job, self.binding = app, job, binding
        self.budgets = json.loads(job["execution"])["budgets"]
        self.entries, self.scopes, self.bytes = [], {}, 0
        self.last = time.monotonic()
        self.blocker = None
        self.statistics = {}
        self.root_identity = None
        self.snapshot = sqlite3.connect("")
        self.snapshot.row_factory = sqlite3.Row
        self.snapshot.execute("CREATE TABLE scopes(path,state,detail)")
        self.snapshot.executemany(
            "INSERT INTO scopes VALUES (?,?,?)",
            app.store.db.execute(
                "SELECT path,CASE WHEN state='running' THEN 'pending' ELSE state END,detail FROM observation_scopes WHERE job_id=?",
                (job["id"],),
            ),
        )
        self.snapshot.commit()
        self.sequence = app.store.rows(
            "SELECT coalesce(max(sequence),0) AS n FROM observation_batches WHERE job_id=?",
            (job["id"],),
        )[0]["n"]

    @property
    def initial_scopes(self):
        for row in self.snapshot.execute("SELECT * FROM scopes"):
            yield {**dict(row), "detail": json.loads(row["detail"])}

    def __call__(self, entry):
        self.entries.append(entry)
        self.bytes += len(encode(entry).encode())
        self.checkpoint()

    def directory(self, path, state, detail):
        self.scopes[path] = (state, detail)
        self.bytes += len(path.encode()) + len(encode(detail).encode())
        if state == "running":
            self.flush()
        else:
            self.checkpoint()

    def checkpoint(self):
        if (
            len(self.entries) + len(self.scopes) >= self.budgets["batch_records"]
            or self.bytes >= self.budgets["batch_bytes"]
            or time.monotonic() - self.last >= self.budgets["batch_seconds"]
        ):
            self.flush()

    def stop(self, reason):
        self.blocker = reason
        self.entries.clear()
        self.scopes.clear()
        self.bytes = 0

    def flush(self):
        try:
            self._flush()
        except (OSError, CatabolicError, sqlite3.Error) as exc:
            raise PublicationStopped(str(exc)) from exc

    def _flush(self):
        from . import observations
        from .app import Application
        from .catalog_refresh import enqueue

        # The traversal owner's Store is detached; this short writer validates
        # the same catalog identity and execution fence on every checkpoint.
        with writer(self.app.store.path) as store:
            if store.database_id != self.app.store.database_id:
                raise CatabolicError("catalog replaced during scan")
            app = Application(store, self.app.profile)
            batch_binding = app.binding("source", self.job["source"])
            with store.detached():
                with root_handle(batch_binding) as fd:
                    if self.root_identity != (os.fstat(fd).st_dev, os.fstat(fd).st_ino):
                        raise CatabolicError("source root changed during scan")
                    from .filesystem import source_stat

                    for entry in self.entries:
                        if entry.get("status") == "missing":
                            try:
                                st = source_stat(fd, entry["path"])
                            except FileNotFoundError:
                                pass
                            else:
                                entry.update(
                                    status="present",
                                    size=st.st_size,
                                    mtime_ns=st.st_mtime_ns,
                                    device=st.st_dev,
                                    inode=st.st_ino,
                                )
                    # Directory completion is an interval assertion. Revalidate it
                    # immediately before absence finalization, independently of children.
                    for path, (state, detail) in list(self.scopes.items()):
                        if state == "complete":
                            try:
                                current = reopen(fd, path, detail.get("identity"))
                                try:
                                    st = os.fstat(current)
                                    if (st.st_mtime_ns, st.st_ctime_ns) != (
                                        detail["mtime_ns"],
                                        detail["ctime_ns"],
                                    ):
                                        raise CatabolicError(
                                            "directory_changed_before_publication"
                                        )
                                finally:
                                    os.close(current)
                            except (OSError, CatabolicError) as exc:
                                self.scopes[path] = (
                                    "failed",
                                    {**detail, "reason": str(exc)},
                                )
            with store.transaction() as db:
                observations.renew(app, self.job)
                next_sequence = self.sequence + 1
                if db.execute(
                    "SELECT 1 FROM observation_batches WHERE job_id=? AND sequence=?",
                    (self.job["id"], next_sequence),
                ).fetchone():
                    raise CatabolicError("duplicate_observation_batch")
                changed = False
                for path, (state, _) in self.scopes.items():
                    if state == "running":
                        prefix = path + "/" if path else ""
                        db.execute(
                            "DELETE FROM observation_seen WHERE job_id=? AND file_id IN (SELECT id FROM files WHERE location=? AND path>=? AND path<? AND instr(substr(path,?),'/')=0)",
                            (
                                self.job["id"],
                                self.job["source"],
                                prefix,
                                prefix + chr(0x10FFFF),
                                len(prefix) + 1,
                            ),
                        )
                for entry in self.entries:
                    identifier = str(
                        uuid5(
                            UUID(store.database_id),
                            encode([self.job["source"], entry["path"]]),
                        )
                    )
                    old = db.execute(
                        "SELECT * FROM observations WHERE profile=? AND file_id=?",
                        (app.profile, identifier),
                    ).fetchone()
                    status = entry.get("status", "present")
                    changed |= (
                        old is None
                        or old["status"] != status
                        or any(
                            old[k] != entry[k]
                            for k in ("size", "mtime_ns", "device", "inode")
                        )
                    )
                    db.execute(
                        "INSERT INTO files VALUES (?,?,?) ON CONFLICT(location,path) DO NOTHING",
                        (identifier, self.job["source"], entry["path"]),
                    )
                    db.execute(
                        "INSERT INTO observations VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(profile,file_id) DO UPDATE SET size=excluded.size,mtime_ns=excluded.mtime_ns,device=excluded.device,inode=excluded.inode,status=excluded.status,scan_id=excluded.scan_id",
                        (
                            app.profile,
                            identifier,
                            entry["size"],
                            entry["mtime_ns"],
                            entry["device"],
                            entry["inode"],
                            status,
                            self.job["scan_id"],
                        ),
                    )
                    if status == "present":
                        db.execute(
                            "INSERT OR IGNORE INTO observation_seen VALUES (?,?)",
                            (self.job["id"], identifier),
                        )
                for path, (state, detail) in self.scopes.items():
                    db.execute(
                        "INSERT INTO observation_scopes VALUES (?,?,?,?,?) ON CONFLICT(job_id,path) DO UPDATE SET state=excluded.state,detail=excluded.detail,updated_at=excluded.updated_at",
                        (self.job["id"], path, state, encode(detail), time.time()),
                    )
                    if state == "complete":
                        # Immediate files, plus descendants of a child confirmed absent
                        # by this enumeration. Present/pending child scopes protect descendants.
                        prefix = path + "/" if path else ""
                        exclusions = json.loads(self.job["exclusions"])
                        sql = "".join(
                            " AND NOT (f.path=? OR substr(f.path,1,length(?)+1)=?||'/')"
                            for _ in exclusions
                        )
                        args = [v for p in exclusions for v in (p, p, p)]
                        removed = db.execute(
                            "INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) SELECT ?,f.id,0,0,0,0,'missing',? FROM files f WHERE f.location=? AND f.path>=? AND f.path<? AND (instr(substr(f.path,?),'/')=0 OR NOT EXISTS(SELECT 1 FROM observation_scopes child WHERE child.job_id=? AND child.path=substr(f.path,1,?+instr(substr(f.path,?),'/')-1))) AND NOT EXISTS(SELECT 1 FROM observation_seen s WHERE s.job_id=? AND s.file_id=f.id)"
                            + sql
                            + " ON CONFLICT(profile,file_id) DO UPDATE SET status='missing',scan_id=excluded.scan_id WHERE observations.status!='missing'",
                            (
                                app.profile,
                                self.job["scan_id"],
                                self.job["source"],
                                prefix,
                                prefix + chr(0x10FFFF),
                                len(prefix) + 1,
                                self.job["id"],
                                len(prefix),
                                len(prefix) + 1,
                                self.job["id"],
                                *args,
                            ),
                        ).rowcount
                        changed |= bool(removed)
                db.execute(
                    "INSERT INTO observation_batches VALUES (?,?,?,?,?)",
                    (
                        self.job["id"],
                        next_sequence,
                        len(self.entries),
                        self.bytes,
                        time.time(),
                    ),
                )
                db.execute(
                    "UPDATE scans SET observed=(SELECT count(*) FROM observation_seen WHERE job_id=?) WHERE id=?",
                    (self.job["id"], self.job["scan_id"]),
                )
                if changed:
                    enqueue(db, app.profile, observation_job=self.job["id"])
        self.sequence += 1
        self.entries.clear()
        self.scopes.clear()
        self.bytes = 0
        self.last = time.monotonic()


def execute(app, job, walker):
    from . import observations

    observations.validate(app, job)
    binding = app.binding("source", job["source"])
    execution = json.loads(job["execution"])
    excluded = json.loads(job["exclusions"])
    with app.store.transaction() as db:
        if not job["scan_id"]:
            job["scan_id"] = str(uuid4())
            db.execute(
                "INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES (?,?,?,0,0,'[]')",
                (job["scan_id"], app.profile, job["source"]),
            )
            db.execute(
                "UPDATE observation_jobs SET scan_id=? WHERE id=?",
                (job["scan_id"], job["id"]),
            )
            db.execute(
                "INSERT INTO meta VALUES (?,?)",
                (
                    f"scan:{job['scan_id']}:scope",
                    encode({"exclude": excluded}),
                ),
            )
            for path in execution["scopes"]:
                db.execute(
                    "INSERT OR IGNORE INTO observation_scopes VALUES (?,?,'pending','{}',?)",
                    (job["id"], path, time.time()),
                )
    generated = app.store.rows(
        "SELECT 1 FROM generated_locations WHERE profile=? AND location=?",
        (app.profile, job["source"]),
    )
    publisher = Publisher(app, job, binding)
    if generated:
        publisher.snapshot.execute("CREATE TABLE artifacts(path TEXT)")
        registry = app.store.db.execute(
            "SELECT path FROM processing_artifacts WHERE profile=? AND location=? AND state='ready' ORDER BY path LIMIT ?",
            (app.profile, job["source"], publisher.budgets["total_entries"] + 1),
        )
        while rows := registry.fetchmany(publisher.budgets["batch_records"]):
            publisher.snapshot.executemany("INSERT INTO artifacts VALUES (?)", rows)
            publisher.snapshot.commit()
            size = (
                publisher.snapshot.execute("PRAGMA page_count").fetchone()[0]
                * publisher.snapshot.execute("PRAGMA page_size").fetchone()[0]
            )
            if size > publisher.budgets["temporary_bytes"]:
                publisher.stop("temporary_storage_budget")
                break
            if (
                shutil.disk_usage(tempfile.gettempdir()).free
                < publisher.budgets["minimum_free_bytes"]
            ):
                publisher.stop("temporary_storage_low")
                break
    errors = []
    try:
        with app.store.detached():
            try:
                with root_handle(binding) as fd:
                    publisher.root_identity = (os.fstat(fd).st_dev, os.fstat(fd).st_ino)
                    with writer(app.store.path) as identity_store:
                        identity_app = type(app)(identity_store, app.profile)
                        with identity_store.transaction() as db:
                            observations.renew(identity_app, job)
                            key = f"observation:{job['id']}:root"
                            pinned = db.execute(
                                "SELECT value FROM meta WHERE key=?", (key,)
                            ).fetchone()
                            if (
                                pinned
                                and tuple(json.loads(pinned[0]))
                                != publisher.root_identity
                            ):
                                error = CatabolicError(
                                    "source root changed during scan recovery"
                                )
                                raise PublicationStopped(str(error)) from error
                            db.execute(
                                "INSERT OR IGNORE INTO meta VALUES (?,?)",
                                (key, encode(publisher.root_identity)),
                            )
                    if publisher.blocker:
                        errors.append(publisher.blocker)
                    elif generated:
                        from .filesystem import source_stat

                        began = time.monotonic()
                        for index, row in enumerate(
                            publisher.snapshot.execute("SELECT path FROM artifacts")
                        ):
                            if index >= publisher.budgets["total_entries"]:
                                publisher.stop("total_entry_budget")
                                break
                            if (
                                time.monotonic() - began
                                >= publisher.budgets["total_seconds"]
                            ):
                                publisher.stop("total_time_budget")
                                break
                            path = row[0]
                            if any(
                                path == e or path.startswith(e + "/") for e in excluded
                            ):
                                continue
                            try:
                                st = source_stat(fd, path)
                            except FileNotFoundError:
                                publisher(
                                    dict(
                                        path=path,
                                        status="missing",
                                        size=0,
                                        mtime_ns=0,
                                        device=0,
                                        inode=0,
                                    )
                                )
                                continue
                            publisher(
                                dict(
                                    path=path,
                                    size=st.st_size,
                                    mtime_ns=st.st_mtime_ns,
                                    device=st.st_dev,
                                    inode=st.st_ino,
                                )
                            )
                        # Registry coverage is separate from directory absence proof.
                        if not publisher.blocker:
                            publisher.directory("", "registry_complete", {})
                    else:
                        observed, errors = walker(
                            fd, exclude=tuple(excluded), sink=publisher
                        )
                        for entry in observed:
                            publisher(entry)
                    if not publisher.blocker:
                        publisher.flush()
            except PublicationStopped as exc:
                if storage_exhausted(exc.__cause__):
                    errors.append(str(exc))
                    publisher.stop("temporary_storage_low")
                elif isinstance(
                    exc.__cause__, CatabolicError
                ) and "source root changed" in str(exc):
                    errors.append(str(exc))
                    publisher.blocker = "source_root_replaced"
                    binding.evidence.update(
                        allowed=False, identity_verified=False, availability="unknown"
                    )
                else:
                    raise exc.__cause__ from exc
            except (OSError, CatabolicError, sqlite3.OperationalError) as exc:
                errors.append(str(exc))
                if storage_exhausted(exc):
                    publisher.stop("temporary_storage_low")
                else:
                    publisher.blocker = str(exc)
    finally:
        publisher.snapshot.close()
    observations.renew(app, job)
    counts = {
        r["state"]: r["n"]
        for r in app.store.rows(
            "SELECT state,count(*) AS n FROM observation_scopes WHERE job_id=? GROUP BY state",
            (job["id"],),
        )
    }
    complete = (
        execution["scopes"] == [""]
        and not errors
        and not publisher.blocker
        and not any(
            counts.get(k, 0) for k in ("pending", "running", "failed", "deferred")
        )
    )
    outstanding = app.store.rows(
        "SELECT path,state,detail,updated_at FROM observation_scopes WHERE job_id=? AND state NOT IN ('complete','excluded') ORDER BY path LIMIT 101",
        (job["id"],),
    )
    for row in outstanding:
        row["detail"] = json.loads(row["detail"])
    report = dict(
        scan_id=job["scan_id"],
        location=job["source"],
        complete=complete,
        availability="unknown"
        if publisher.blocker == "source_root_replaced"
        else "available"
        if complete or counts.get("complete")
        else getattr(binding, "evidence", {}).get("availability", "unknown"),
        inventory_retained=not complete,
        excluded=excluded,
        errors=errors,
        coverage=counts,
        outstanding=outstanding[:100],
        outstanding_truncated=len(outstanding) > 100,
        blocker=publisher.blocker,
        budgets=publisher.budgets,
        batches=publisher.sequence,
        observed=app.store.rows(
            "SELECT observed FROM scans WHERE id=?", (job["scan_id"],)
        )[0]["observed"],
        validation=dict(getattr(binding, "evidence", {})),
    )
    report["published"] = report["observed"]
    report["progress"] = app.store.rows(
        "SELECT count(*) AS batches,sum(bytes) AS committed_bytes,max(committed_at) AS last_progress_at FROM observation_batches WHERE job_id=?",
        (job["id"],),
    )[0]
    report["progress"].update(publisher.statistics)
    report["progress"]["buffered_bytes"] = publisher.bytes
    report["requested_scopes"] = execution["scopes"]
    report["policy_revision"] = execution["policy_revision"]
    report["deferred"] = not complete and (
        publisher.blocker
        in (
            "temporary_storage_low",
            "temporary_storage_budget",
            "total_time_budget",
            "total_entry_budget",
            "directory_queue_budget",
        )
        or bool(counts.get("deferred"))
    )
    demands = app.store.rows(
        "SELECT id FROM observation_requests WHERE job_id=? AND profile=? AND state='active' ORDER BY created_at LIMIT 1",
        (job["id"], app.profile),
    )
    report["suggested_actions"] = (
        []
        if complete
        else [
            {
                "operation": "scan",
                "arguments": {
                    "continue_request": demands[0]["id"] if demands else None
                },
                "choices": [
                    "extended",
                    "selected_scopes",
                    "leave_deferred",
                    "repair_then_retry",
                ],
            },
            {
                "operation": "location scan-policy",
                "arguments": {"name": job["source"]},
                "preview_required": True,
            },
        ]
    )
    with app.store.transaction() as db:
        observations.validate(app, job)
        db.execute(
            "UPDATE scans SET complete=?,errors=?,finished_at=CURRENT_TIMESTAMP WHERE id=?",
            (int(complete), encode(errors), job["scan_id"]),
        )
        db.execute(
            "INSERT OR REPLACE INTO meta VALUES (?,?)",
            (f"scan:{job['scan_id']}:validation", encode(report["validation"])),
        )
        observations.finish(db, job, report)
    return report
