# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Isolated, globally slot-bounded source probes; no hashing or media execution."""

import json
import os
import subprocess
import sys
import time
from contextlib import contextmanager
from uuid import uuid4

from .domain import CatabolicError
from .source_access import validated_source
from .store import Store, encode

# Keep Popen objects until reaped, including timed-out helpers. A live PID also
# retains its durable slot across CLI exits; timeouts never authorize slot reuse.
_PENDING = []


def alive(pid):
    if pid is None:
        return False
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


@contextmanager
def _writer(database):
    from .database_io import is_catalog_busy

    deadline = time.monotonic() + 1
    while True:
        try:
            store = Store(database, writable=True)
            break
        except CatabolicError as exc:
            if not is_catalog_busy(exc) or time.monotonic() >= deadline:
                raise
            time.sleep(0.01)
    with store:
        yield store


def probe(database, snapshots, timeout_ms):
    for child in list(_PENDING):
        if child.poll() is not None:
            _PENDING.remove(child)
    token = str(uuid4())
    with _writer(database) as store:
        slot = next(
            (
                r
                for r in store.rows("SELECT * FROM fallback_probe_slots ORDER BY id")
                if not alive(r["pid"])
            ),
            None,
        )
        if slot is None:
            return {"usable": False, "reason": "probe_capacity"}
        with store.transaction() as db:
            db.execute(
                "UPDATE fallback_probe_slots SET owner=?,pid=? WHERE id=?",
                (token, os.getpid(), slot["id"]),
            )
    child = None
    try:
        child = subprocess.Popen(
            [sys.executable, "-m", "catabolic.fallback_probe"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        with _writer(database) as store:
            with store.transaction() as db:
                db.execute(
                    "UPDATE fallback_probe_slots SET pid=? WHERE id=? AND owner=?",
                    (child.pid, slot["id"], token),
                )
        try:
            stdout, _ = child.communicate(
                encode(snapshots).encode(), timeout=timeout_ms / 1000
            )
        except subprocess.TimeoutExpired:
            child.kill()
            try:
                child.communicate(timeout=0.05)
            except subprocess.TimeoutExpired:
                _PENDING.append(child)
            return {"usable": False, "reason": "probe_timeout"}
        if child.returncode != 0:
            return {"usable": False, "reason": "probe_failed"}
        result = json.loads(stdout)
        return result
    finally:
        # Input is sent only after the child PID is durable. If the parent dies
        # before that handoff, EOF makes the helper exit without touching sources.
        # Exceptions during handoff must also close/kill the waiting helper.
        if child is not None and child.poll() is None and child not in _PENDING:
            child.kill()
            try:
                child.communicate(timeout=0.05)
            except subprocess.TimeoutExpired:
                _PENDING.append(child)
        if child is None or child.poll() is not None:
            with _writer(database) as store:
                with store.transaction() as db:
                    db.execute(
                        "UPDATE fallback_probe_slots SET owner=NULL,pid=NULL WHERE id=? AND owner=?",
                        (slot["id"], token),
                    )


def check(snapshots):
    try:
        if not isinstance(snapshots, list) or not 1 <= len(snapshots) <= 65:
            raise CatabolicError("probe_snapshot_limit")
        for snapshot in snapshots:
            with validated_source(snapshot) as fd:
                if os.pread(fd, 16, 0) == b"SQLite format 3\x00":
                    raise CatabolicError("private_content")
        return {"usable": True, "reason": None}
    except (CatabolicError, OSError):
        return {"usable": False, "reason": "source_unavailable_or_changed"}


if __name__ == "__main__":
    print(encode(check(json.loads(sys.stdin.buffer.read(1024 * 1024)))))
