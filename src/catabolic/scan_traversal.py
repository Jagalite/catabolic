# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Bounded iterative implementation of filesystem.walk_files."""

import errno
import json
import os
import shutil
import sqlite3
import stat
import tempfile
import time

from .domain import CatabolicError, relative_path
from .scan_policy import DEFAULTS


class ScanResourceStop(RuntimeError):
    """Operation-level exhaustion; never catch as a per-file traversal error."""


def storage_exhausted(exc):
    return (isinstance(exc, OSError) and exc.errno in (errno.ENOSPC, errno.EDQUOT)) or (
        isinstance(exc, sqlite3.OperationalError)
        and (
            getattr(exc, "sqlite_errorcode", None) == sqlite3.SQLITE_FULL
            or str(exc) == "database or disk is full"
        )
    )


def reopen(root_fd, path, expected=None):
    from .filesystem import DIRECTORY_FLAGS

    fd = os.dup(root_fd)
    root_device = os.fstat(root_fd).st_dev
    try:
        for part in path.split("/") if path else ():
            child = os.open(part, DIRECTORY_FLAGS, dir_fd=fd)
            os.close(fd)
            fd = child
            if os.fstat(fd).st_dev != root_device:
                raise CatabolicError(
                    "nested filesystem boundary; register this mount as a separate location"
                )
        st = os.fstat(fd)
        if expected and (st.st_dev, st.st_ino) != tuple(expected):
            raise CatabolicError("directory replaced during scan")
        return fd
    except BaseException:
        os.close(fd)
        raise


def walk(root_fd, *, exclude=(), sink=None):
    limits = getattr(sink, "budgets", DEFAULTS)
    observed, errors = [], []
    started = time.monotonic()
    total = 0
    initial = getattr(
        sink, "initial_scopes", [{"path": "", "state": "pending", "detail": {}}]
    )
    with tempfile.TemporaryDirectory(prefix="catabolic-scan-") as temporary:
        queue = sqlite3.connect(os.path.join(temporary, "directories.sqlite3"))
        queue.execute("PRAGMA cache_size=-1024")
        queue.execute(
            "CREATE TABLE queue(path TEXT PRIMARY KEY,state TEXT,detail TEXT)"
        )
        queue.executemany(
            "INSERT OR IGNORE INTO queue VALUES (?,?,?)",
            ((r["path"], r["state"], json.dumps(r["detail"])) for r in initial),
        )
        queue.commit()
        queue.execute("CREATE INDEX pending ON queue(state)")
        directory_count = queue.execute("SELECT count(*) FROM queue").fetchone()[0]

        def notify(path, state, detail):
            if state in ("failed", "deferred") and len(errors) < 1000:
                errors.append(
                    (
                        f"{path or '.'}: {detail['reason'].replace('_', ' ')}"
                        + (
                            f" ({detail['last_error']})"
                            if detail.get("last_error")
                            else ""
                        )
                    )[:4096]
                )
            if hasattr(sink, "directory"):
                sink.directory(path, state, detail)

        def resources():
            free = shutil.disk_usage(temporary).free
            pages = queue.execute("PRAGMA page_count").fetchone()[0]
            size = pages * queue.execute("PRAGMA page_size").fetchone()[0]
            if hasattr(sink, "statistics"):
                sink.statistics = dict(
                    entries_visited=total,
                    directory_records=directory_count,
                    temporary_bytes=size,
                    temporary_free_bytes=free,
                    elapsed_seconds=time.monotonic() - started,
                )
            if free < limits["minimum_free_bytes"]:
                raise ScanResourceStop("temporary_storage_low")
            if size > limits["temporary_bytes"]:
                raise ScanResourceStop("temporary_storage_budget")
            if time.monotonic() - started >= limits["total_seconds"]:
                raise ScanResourceStop("total_time_budget")
            if total > limits["total_entries"]:
                raise ScanResourceStop("total_entry_budget")
            if hasattr(sink, "checkpoint"):
                sink.checkpoint()

        try:
            while True:
                resources()
                row = queue.execute(
                    "SELECT path,detail FROM queue WHERE state='pending' ORDER BY rowid LIMIT 1"
                ).fetchone()
                if row is None:
                    break
                path, detail = row[0], json.loads(row[1])
                queue.execute("UPDATE queue SET state='running' WHERE path=?", (path,))
                depth = len(path.split("/")) if path else 0
                if any(path == e or path.startswith(e + "/") for e in exclude):
                    notify(path, "excluded", {"reason": "configured_exclusion"})
                    continue
                if depth > limits["depth"]:
                    notify(
                        path,
                        "deferred",
                        {
                            **detail,
                            "reason": "depth_budget",
                            "depth": depth,
                            "limit": limits["depth"],
                        },
                    )
                    continue
                fd = None
                count, failures = 0, 0
                began = time.monotonic()
                status, reason = "complete", None
                try:
                    fd = reopen(root_fd, path, detail.get("identity"))
                    before = os.fstat(fd)
                    detail = {
                        "identity": [before.st_dev, before.st_ino],
                        "mtime_ns": before.st_mtime_ns,
                        "ctime_ns": before.st_ctime_ns,
                    }
                    notify(path, "running", detail)
                    with os.scandir(fd) as entries:
                        for entry in entries:
                            count += 1
                            total += 1
                            now = time.monotonic()
                            if total > limits["total_entries"]:
                                raise ScanResourceStop("total_entry_budget")
                            if now - started >= limits["total_seconds"]:
                                raise ScanResourceStop("total_time_budget")
                            if (
                                count > limits["directory_entries"]
                                or now - began >= limits["directory_seconds"]
                            ):
                                status, reason = (
                                    "deferred",
                                    "directory_entry_budget"
                                    if count > limits["directory_entries"]
                                    else "directory_time_budget",
                                )
                                break
                            if count % 100 == 1:
                                resources()
                            child_path = f"{path}/{entry.name}" if path else entry.name
                            if child_path in exclude:
                                notify(
                                    child_path,
                                    "excluded",
                                    {"reason": "configured_exclusion"},
                                )
                                continue
                            observation = None
                            tick = time.monotonic()
                            try:
                                relative_path(child_path)
                                st = os.stat(
                                    entry.name, dir_fd=fd, follow_symlinks=False
                                )
                                latency = time.monotonic() - tick
                                if stat.S_ISDIR(st.st_mode):
                                    child_detail = {"identity": [st.st_dev, st.st_ino]}
                                    if st.st_dev != before.st_dev:
                                        notify(
                                            child_path,
                                            "failed",
                                            {
                                                **child_detail,
                                                "reason": "filesystem_boundary",
                                            },
                                        )
                                    else:
                                        if (
                                            directory_count
                                            >= limits["pending_directories"]
                                        ):
                                            raise ScanResourceStop(
                                                "directory_queue_budget"
                                            )
                                        inserted = queue.execute(
                                            "INSERT OR IGNORE INTO queue VALUES (?,'pending',?)",
                                            (child_path, json.dumps(child_detail)),
                                        ).rowcount
                                        if inserted:
                                            directory_count += 1
                                            notify(child_path, "pending", child_detail)
                                elif stat.S_ISREG(st.st_mode):
                                    observation = dict(
                                        path=child_path,
                                        size=st.st_size,
                                        mtime_ns=st.st_mtime_ns,
                                        device=st.st_dev,
                                        inode=st.st_ino,
                                    )
                            except (OSError, CatabolicError) as exc:
                                latency = time.monotonic() - tick
                                failures += 1
                                status, reason = "failed", str(exc)[:1000]
                            # Sink failures, including resource exhaustion, escape the per-file handler.
                            if observation is not None:
                                (sink if sink is not None else observed.append)(
                                    observation
                                )
                            if latency >= limits["metadata_seconds"]:
                                status, reason = "deferred", "metadata_latency_budget"
                                detail["metadata_seconds"] = latency
                                break
                            if failures >= limits["errors"]:
                                detail["last_error"] = reason
                                status, reason = "deferred", "directory_error_budget"
                                break
                    after = os.fstat(fd)
                    if (before.st_mtime_ns, before.st_ctime_ns) != (
                        after.st_mtime_ns,
                        after.st_ctime_ns,
                    ):
                        status, reason = "deferred", "directory_changed_during_scan"
                except (OSError, CatabolicError) as exc:
                    status, reason = "failed", str(exc)[:1000]
                finally:
                    if fd is not None:
                        os.close(fd)
                detail.update(
                    entries=count, seconds=time.monotonic() - began, errors=failures
                )
                if reason:
                    detail["reason"] = reason
                    budget_key = {
                        "directory_entry_budget": "directory_entries",
                        "directory_time_budget": "directory_seconds",
                        "metadata_latency_budget": "metadata_seconds",
                        "directory_error_budget": "errors",
                    }.get(reason)
                    if budget_key:
                        detail["limit"] = limits[budget_key]
                notify(path, status, detail)
                queue.execute("UPDATE queue SET state=? WHERE path=?", (status, path))
                queue.commit()
                if hasattr(sink, "flush"):
                    sink.flush()
        except (ScanResourceStop, sqlite3.OperationalError) as exc:
            # One source-level blocker, retaining the disk-backed pending scopes.
            if hasattr(sink, "stop"):
                sink.stop(
                    "temporary_storage_low" if storage_exhausted(exc) else str(exc)
                )
            errors.append(str(exc))
        finally:
            queue.close()
    return observed, errors
