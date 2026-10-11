# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native kernel hints integrated with actual detached watcher workers."""

import json
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

LEGACY = Path(
    os.environ.get(
        "CATABOLIC_LEGACY_BINARY", BINARY.with_name("catabolic-legacy-native")
    )
)


@unittest.skipUnless(BINARY.is_file() and LEGACY.is_file(), "build native binaries")
class NativeEventSupervisorTest(unittest.TestCase):
    setUp = base.NativeWatchersTest.setUp
    call = base.NativeWatchersTest.call

    def configure(self, **extra):
        self.call(
            "put",
            name="inventory",
            definition={
                "plan": {"kind": "observation", "sources": ["media"]},
                "events": True,
                "debounce_seconds": 0,
                "retry_seconds": 1,
                **extra,
            },
        )
        self.call("enable", name="inventory")

    def supervisor(self, *, once=False, env=None):
        command = [
            str(LEGACY),
            "--db",
            str(self.database),
            "--json",
            "supervise",
            "--native",
        ]
        if once:
            command.append("--once")
        return subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=os.environ | {"CATABOLIC_NATIVE_BINARY": str(BINARY)} | (env or {}),
        )

    def wait_for(self, predicate, *, seconds=15):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            with closing(sqlite3.connect(self.database)) as db:
                result = predicate(db)
            if result:
                return result
            time.sleep(0.05)
        self.fail("native event supervisor did not reach expected durable state")

    def stop(self, process):
        if process.poll() is None:
            process.send_signal(signal.SIGINT)
        output, error = process.communicate(timeout=10)
        self.assertNotEqual(process.returncode, 0, (output, error))
        self.assertIn("interrupted", error + output)

    def test_native_once_uses_kernel_adapter_and_existing_reducers(self):
        self.configure()
        with closing(sqlite3.connect(self.database)) as db:
            before = db.execute(
                "SELECT coalesce((SELECT dirty_generation FROM observation_sources WHERE source='media'),0)"
            ).fetchone()[0]
        process = self.supervisor(once=True)
        output, error = process.communicate(timeout=15)
        self.assertEqual(process.returncode, 0, error)
        result = json.loads(output)
        self.assertTrue(result["complete"], result)
        with closing(sqlite3.connect(self.database)) as db:
            self.assertEqual(
                db.execute(
                    "SELECT dirty_generation FROM observation_sources WHERE source='media'"
                ).fetchone()[0],
                before + 2,
            )
        self.assertEqual(set(result), {"complete", "pending", "runs"})
        diagnostics = self.call("tick", native=True, include_monitoring=True)
        self.assertEqual(diagnostics["monitoring"]["coverage"], "hints_only")
        self.assertEqual(len(result["runs"]), 1)
        self.assertTrue(result["runs"][0]["complete"], result)

    def test_continuous_native_events_reconcile_created_subtree_and_cancel(self):
        self.configure()
        process = self.supervisor()
        try:
            self.wait_for(
                lambda db: db.execute(
                    "SELECT count(*) FROM watcher_runs WHERE state='complete'"
                ).fetchone()[0]
            )
            (self.source / "new").mkdir()
            (self.source / "new" / "two.mkv").write_bytes(b"new kernel event")
            self.wait_for(
                lambda db: db.execute(
                    "SELECT count(*) FROM files f JOIN observations o ON f.id=o.file_id WHERE f.path='new/two.mkv' AND o.status='present'"
                ).fetchone()[0]
            )
            # A subsequent content change must use the newly registered subtree.
            (self.source / "new" / "two.mkv").write_bytes(b"updated kernel event bytes")
            self.wait_for(
                lambda db: db.execute(
                    "SELECT count(*) FROM files f JOIN observations o ON f.id=o.file_id WHERE f.path='new/two.mkv' AND o.size=?",
                    (len(b"updated kernel event bytes"),),
                ).fetchone()[0]
            )
            with closing(sqlite3.connect(self.database)) as db:
                self.assertEqual(
                    db.execute(
                        "SELECT count(*) FROM observation_requests WHERE state='cancelled'"
                    ).fetchone()[0],
                    0,
                )
        finally:
            self.stop(process)

    def test_event_during_active_scan_preserves_newer_demand(self):
        for index in range(100):
            (self.source / f"slow{index:03}.txt").write_text("initial bytes")
        self.configure(max_age_seconds=0)
        process = self.supervisor(env={"CATABOLIC_TEST_SCAN_ENTRY_DELAY_MS": "20"})
        try:
            self.wait_for(
                lambda db: db.execute(
                    "SELECT count(*) FROM observation_jobs WHERE state='running'"
                ).fetchone()[0]
            )
            with closing(sqlite3.connect(self.database)) as db:
                before = db.execute(
                    "SELECT dirty_generation FROM observation_sources WHERE source='media'"
                ).fetchone()[0]
            (self.source / "late.mkv").write_bytes(b"late notification")
            self.wait_for(
                lambda db: (
                    db.execute(
                        "SELECT dirty_generation FROM observation_sources WHERE source='media'"
                    ).fetchone()[0]
                    > before
                )
            )
            self.wait_for(
                lambda db: db.execute(
                    "SELECT count(*) FROM watchers WHERE completed_generation>=pending_generation AND active_run IS NULL"
                ).fetchone()[0],
                seconds=20,
            )
            with closing(sqlite3.connect(self.database)) as db:
                self.assertEqual(
                    db.execute(
                        "SELECT count(*) FROM files f JOIN observations o ON f.id=o.file_id WHERE f.path='late.mkv' AND o.status='present'"
                    ).fetchone()[0],
                    1,
                )
                generation = db.execute(
                    "SELECT dirty_generation FROM observation_sources WHERE source='media'"
                ).fetchone()[0]
                self.assertTrue(
                    db.execute(
                        "SELECT count(*) FROM observation_jobs WHERE state='complete' AND dirty_generation=?",
                        (generation,),
                    ).fetchone()[0]
                )
        finally:
            self.stop(process)

    def test_startup_debounce_remains_pending_in_native_once(self):
        self.configure(debounce_seconds=5)
        result = self.call("tick", native=True)
        self.assertFalse(result["complete"], result)
        self.assertEqual(result["runs"], [])
        self.assertEqual(result["pending"], 1)
