# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent Linux native-adapter worker-pulse and cancellation evidence."""

import os
import signal
import sqlite3
import subprocess
import time
import unittest
from contextlib import closing
from pathlib import Path

from tests import test_rust_watchers as base
from tests.test_rust_migration import BINARY

LEGACY = Path(os.environ.get("CATABOLIC_LEGACY_BINARY", ""))


@unittest.skipUnless(
    BINARY.is_file() and LEGACY.is_file(), "set both frozen native executable paths"
)
class IndependentNativeEventSupervisorTest(unittest.TestCase):
    setUp = base.NativeWatchersTest.setUp
    call = base.NativeWatchersTest.call

    def configure(self):
        self.call(
            "put",
            name="inventory",
            definition={
                "plan": {"kind": "observation", "sources": ["media"]},
                "events": True,
                "debounce_seconds": 0,
                "retry_seconds": 1,
                "max_age_seconds": 0,
            },
        )
        self.call("enable", name="inventory")

    def start(self):
        for index in range(60):
            (self.source / f"slow-{index:03}.txt").write_bytes(b"initial")
        self.configure()
        return subprocess.Popen(
            [
                str(LEGACY),
                "--db",
                str(self.database),
                "--json",
                "supervise",
                "--native",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=os.environ
            | {
                "CATABOLIC_NATIVE_BINARY": str(BINARY),
                "CATABOLIC_TEST_SCAN_ENTRY_DELAY_MS": "60",
            },
        )

    def wait(self, predicate, seconds=20):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            with closing(sqlite3.connect(self.database)) as db:
                result = predicate(db)
            if result:
                return result
            time.sleep(0.02)
        self.fail(
            "the independent native-event fixture did not reach the expected state"
        )

    def await_shared_workers(self):
        # A shared observation owner may outlive this watcher waiter. Keep
        # disposable inputs until that work finishes, including test failures.
        self.wait(
            lambda db: (
                db.execute(
                    "SELECT count(*) FROM observation_jobs WHERE state='running'"
                ).fetchone()[0]
                == 0
            )
        )

    def stop(self, process):
        if process.poll() is None:
            process.send_signal(signal.SIGINT)
        output, error = process.communicate(timeout=10)
        self.assertNotEqual(process.returncode, 0, (output, error))
        self.assertIn("interrupted", output + error)

    def test_worker_pulse_records_new_generation_before_running_scan_finishes(self):
        process = self.start()
        try:
            job, initial_generation = self.wait(
                lambda db: db.execute(
                    "SELECT id,dirty_generation FROM observation_jobs WHERE state='running'"
                ).fetchone()
            )
            with closing(sqlite3.connect(self.database)) as db:
                initial_pending = db.execute(
                    "SELECT pending_generation FROM watchers WHERE name='inventory'"
                ).fetchone()[0]
            (self.source / "late-independent.txt").write_bytes(
                b"new hint during child wait"
            )
            newer = self.wait(
                lambda db: db.execute(
                    "SELECT s.dirty_generation FROM observation_sources s "
                    "JOIN observation_jobs j ON j.id=? "
                    "JOIN watchers w ON w.name='inventory' "
                    "WHERE s.source='media' AND j.state='running' "
                    "AND s.dirty_generation>? AND w.pending_generation>? "
                    "AND w.active_run IS NOT NULL",
                    (job, initial_generation, initial_pending),
                ).fetchone()
            )[0]
            self.wait(
                lambda db: db.execute(
                    "SELECT 1 FROM watchers w JOIN observation_jobs j ON j.source='media' "
                    "WHERE w.name='inventory' AND w.completed_generation>=w.pending_generation "
                    "AND w.active_run IS NULL AND j.state='complete' AND j.dirty_generation>=?",
                    (newer,),
                ).fetchone()
            )
            with closing(sqlite3.connect(self.database)) as db:
                self.assertEqual(
                    db.execute(
                        "SELECT count(*) FROM files f JOIN observations o ON f.id=o.file_id "
                        "WHERE f.path='late-independent.txt' AND o.status='present'"
                    ).fetchone()[0],
                    1,
                )
        finally:
            self.stop(process)
            self.await_shared_workers()

    def test_cancellation_reaps_watcher_scope_without_cancelling_durable_demand(self):
        process = self.start()
        try:
            watcher_pid = self.wait(
                lambda db: db.execute(
                    "SELECT r.worker_pid FROM watcher_runs r JOIN watchers w "
                    "ON w.active_run=r.id JOIN observation_jobs j ON j.state='running' "
                    "WHERE r.worker_pid IS NOT NULL"
                ).fetchone()
            )[0]
            with closing(sqlite3.connect(self.database)) as db:
                requests = db.execute("SELECT id FROM observation_requests").fetchall()
                self.assertTrue(requests)
            self.stop(process)
            with self.assertRaises(ProcessLookupError):
                os.kill(watcher_pid, 0)
            with closing(sqlite3.connect(self.database)) as db:
                current = dict(db.execute("SELECT id,state FROM observation_requests"))
                for (request,) in requests:
                    self.assertIn(request, current)
                    self.assertNotEqual(current[request], "cancelled")
        finally:
            if process.poll() is None:
                self.stop(process)
            self.await_shared_workers()
