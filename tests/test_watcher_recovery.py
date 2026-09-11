# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import importlib.util
import time
import unittest
from unittest.mock import patch

from catabolic import observations
from catabolic.domain import CatabolicError
from catabolic.watchers import Watchers
from tests import test_watchers as fixtures


class RecoveryTest(unittest.TestCase):
    setUp = fixtures.WatcherTest.setUp
    definition = fixtures.WatcherTest.definition

    def test_tick_recovers_dead_lease_without_restart(self):
        from catabolic.supervisor import tick

        watchers = Watchers(self.app)
        watchers.put("audit", self.definition())
        with patch("catabolic.plans.evaluate", side_effect=SystemExit("crash")):
            with self.assertRaises(SystemExit):
                watchers.run("audit")
        with self.store.transaction() as db:
            db.execute("UPDATE watcher_runs SET worker_pid=2147483000")
        with self.store.detached():
            self.assertTrue(tick(self.database)["complete"])
        self.assertIsNone(watchers.get("audit")["active_run"])
        self.assertEqual(
            {r["state"] for r in watchers.history("audit")}, {"interrupted", "complete"}
        )

    def test_live_pending_lease_is_not_reported_complete(self):
        from catabolic.supervisor import tick

        watchers = Watchers(self.app)
        watchers.put("audit", self.definition())
        with patch("catabolic.plans.evaluate", side_effect=SystemExit("pause")):
            with self.assertRaises(SystemExit):
                watchers.run("audit")
        with self.store.detached():
            result = tick(self.database)
        self.assertFalse(result["complete"])
        self.assertEqual(result["pending"], 1)
        self.assertEqual(result["runs"], [])

    def test_recovery_cannot_clear_replacement_run(self):
        from catabolic.supervisor import recover_run

        watchers = Watchers(self.app)
        watchers.put("audit", self.definition())
        with patch("catabolic.plans.evaluate", side_effect=SystemExit("pause")):
            with self.assertRaises(SystemExit):
                watchers.run("audit")
        row = watchers.get("audit")
        self.assertFalse(recover_run(self.app, "audit", "old-run", 2147483000))
        self.assertEqual(watchers.get("audit")["active_run"], row["active_run"])

    def test_parent_retries_real_writer_contention_and_preserves_hints(self):
        from unittest.mock import MagicMock

        from catabolic import supervisor
        from catabolic.source_events import invalidate
        from catabolic.store import Store

        source = self.store.rows("SELECT id FROM locations")[0]["id"]
        monitor = MagicMock()
        monitor.drain.side_effect = [[source], [], []]
        blocker = None
        attempts = []

        def drain():
            nonlocal blocker
            if not attempts:
                blocker = Store(self.database, writable=True)
                attempts.append("blocked")
                invalidation.reset_mock()
                return [source]
            return []

        def sleep(seconds):
            nonlocal blocker
            if blocker is not None:
                blocker.close()
                blocker = None
            else:
                raise SystemExit("finished")

        monitor.drain.side_effect = drain
        with (
            self.store.detached(),
            patch("catabolic.native_monitor.NativeMonitor", return_value=monitor),
            patch("catabolic.supervisor.time.sleep", side_effect=sleep),
            patch(
                "catabolic.source_events.invalidate", wraps=invalidate
            ) as invalidation,
        ):
            with self.assertRaisesRegex(SystemExit, "finished"):
                supervisor.run(self.database, native=True)
            self.assertTrue(
                any(call.args[1] == source for call in invalidation.call_args_list)
            )
        monitor.close.assert_called_once()

    def test_parent_retries_contention_inside_tick(self):
        from catabolic import supervisor
        from catabolic.store import Store

        original = supervisor.tick
        calls = []

        def competing_tick(*args, **kwargs):
            calls.append(1)
            if len(calls) == 1:
                with Store(self.database, writable=True):
                    return original(*args, **kwargs)
            return original(*args, **kwargs)

        def sleep(seconds):
            if len(calls) == 2:
                raise SystemExit("finished")

        with (
            self.store.detached(),
            patch("catabolic.supervisor.tick", side_effect=competing_tick),
            patch("catabolic.supervisor.time.sleep", side_effect=sleep),
        ):
            with self.assertRaisesRegex(SystemExit, "finished"):
                supervisor.run(self.database)
        self.assertEqual(len(calls), 2)

    def test_parent_does_not_swallow_unrelated_database_errors(self):
        import sqlite3

        from catabolic.supervisor import run

        with (
            self.store.detached(),
            patch(
                "catabolic.supervisor.tick",
                side_effect=sqlite3.OperationalError("unrelated"),
            ),
        ):
            with self.assertRaisesRegex(sqlite3.OperationalError, "unrelated"):
                run(self.database)

    def test_abrupt_child_and_timeout_are_reaped(self):
        import subprocess
        import sys
        from pathlib import Path

        from catabolic import supervisor

        watchers = Watchers(self.app)
        watchers.put("audit", self.definition())
        real_popen = subprocess.Popen
        for fault in ("exit", "timeout"):
            watchers.admit("audit")
            marker = self.root / (fault + ".ready")
            script = """
import os,sys,time
from pathlib import Path
from unittest.mock import patch
from catabolic.store import Store
from catabolic.app import Application
from catabolic.watchers import Watchers
def fail(*args,**kwargs):
 Path(sys.argv[2]).touch()
 if sys.argv[3]=='exit': os._exit(17)
 time.sleep(60)
with Store(sys.argv[1],writable=True) as store:
 with patch('catabolic.plans.evaluate',side_effect=fail):
  Watchers(Application(store)).run('audit')
"""

            def launch(args, *, script=script, marker=marker, fault=fault, **kwargs):
                child = real_popen(
                    [
                        sys.executable,
                        "-c",
                        script,
                        str(self.database),
                        str(marker),
                        fault,
                    ],
                    **kwargs,
                )
                deadline = time.monotonic() + 10
                while (
                    not Path(marker).exists()
                    and child.poll() is None
                    and time.monotonic() < deadline
                ):
                    time.sleep(0.01)
                self.assertTrue(marker.exists())
                return child

            with (
                self.store.detached(),
                patch("catabolic.supervisor.subprocess.Popen", side_effect=launch),
                patch("catabolic.supervisor.CHILD_TIMEOUT_SECONDS", 0),
            ):
                result = supervisor.tick(self.database)
            self.assertFalse(result["complete"])
            row = watchers.get("audit")
            self.assertIsNone(row["active_run"])
            self.assertEqual(row["lease_until"], 0)
            self.assertEqual(watchers.history("audit")[0]["state"], "interrupted")

    def test_dead_worker_and_scan_are_recovered(self):
        from catabolic.supervisor import recover

        watchers = Watchers(self.app)
        watchers.put("audit", self.definition())
        with patch("catabolic.plans.evaluate", side_effect=SystemExit("crash")):
            with self.assertRaises(SystemExit):
                watchers.run("audit")
        source = self.store.rows("SELECT id FROM locations")[0]["id"]
        observations.dirty(self.app, source)
        observations.claim(self.app, observations.request(self.app, source))
        with self.store.transaction() as db:
            db.execute("UPDATE watcher_runs SET worker_pid=2147483000")
            db.execute(
                "UPDATE observation_jobs SET worker_pid=2147483000 WHERE state='running'"
            )
        recover(self.app)
        self.assertIsNone(watchers.get("audit")["active_run"])
        self.assertEqual(
            self.store.rows("SELECT * FROM observation_jobs WHERE state='running'"), []
        )
        self.assertTrue(watchers.run("audit")["complete"])

    def test_source_scope_and_barrier_are_not_conflated(self):
        source = self.store.rows("SELECT id FROM locations")[0]["id"]
        observations.dirty(self.app, source)
        first = observations.claim(
            self.app, observations.request(self.app, source, max_age=60)
        )
        same = observations.request(self.app, source, max_age=60)
        self.assertEqual(first["id"], same["id"])
        narrow = observations.request(
            self.app, source, max_age=60, exclusions=("private",)
        )
        self.assertNotEqual(first["id"], narrow["id"])
        forced = observations.request(
            self.app, source, max_age=60, after=first["started_at"] + 1
        )
        self.assertNotEqual(first["id"], forced["id"])
        with self.assertRaisesRegex(CatabolicError, "scope"):
            self.app.scan(source, _observation=narrow)

    def test_incomplete_query_retains_baseline(self):
        watchers = Watchers(self.app)
        watchers.put("audit", self.definition())
        self.assertTrue(watchers.run("audit")["complete"])
        baseline = watchers.get("audit")["baseline"]
        with patch(
            "catabolic.plans.evaluate",
            return_value={"kind": "selection", "value": {}, "complete": False},
        ):
            self.assertFalse(watchers.run("audit")["complete"])
        self.assertEqual(watchers.get("audit")["baseline"], baseline)

    @unittest.skipUnless(
        importlib.util.find_spec("watchdog"), "optional native dependency"
    )
    def test_native_adapter_marks_shared_source_dirty(self):
        from catabolic.native_monitor import NativeMonitor

        monitor = NativeMonitor({"source": str(self.source)})
        monitor.start()
        try:
            monitor.drain()
            (self.source / "native-fixture.bin").write_bytes(b"fixture")
            deadline = time.monotonic() + 5
            pending = []
            while time.monotonic() < deadline and not pending:
                time.sleep(0.05)
                pending = monitor.drain()
            self.assertIn("source", pending)
        finally:
            monitor.close()


class CoordinationTest(unittest.TestCase):
    setUp = fixtures.WatcherTest.setUp
    bind = fixtures.WatcherTest.bind
    policy = fixtures.WatcherTest.policy

    def test_two_projection_watchers_make_progress_under_supervisor(self):
        from catabolic.fallback_projection import FallbackProjection, binding
        from catabolic.plans import projection_digest
        from catabolic.store import Store
        from catabolic.supervisor import tick

        self.bind("immediate")
        config = binding(self.store, "default", "global")
        output = self.root / "browser"
        output.mkdir()
        self.app.bind("output", "browser", str(output))
        FallbackProjection(self.app).bind(
            "browser", config["policy_id"], config["query_id"], config["layout"]
        )
        watchers = Watchers(self.app)
        for catalog in ("global", "browser"):
            watchers.put(
                catalog,
                {
                    "plan": {
                        "kind": "projection",
                        "catalog": catalog,
                        "binding_digest": projection_digest(self.app, catalog),
                    },
                    "reaction": "projection",
                    "schedule": {"kind": "interval", "seconds": 3600},
                },
            )
            watchers.enable(catalog)
        with self.store.transaction() as db:
            db.execute("UPDATE watchers SET next_due=0")
        for _ in range(5):
            with self.store.detached():
                tick(self.database)
                time.sleep(1.1)
                with Store(self.database) as check:
                    count = check.rows(
                        "SELECT count(DISTINCT watcher) n FROM watcher_runs WHERE state='complete'"
                    )[0]["n"]
            if count == 2:
                break
        self.assertEqual(
            count, 2, watchers.history("global") + watchers.history("browser")
        )

    def test_scan_timeout_retains_inventory_and_reports_uncertainty(self):
        import subprocess
        import sys

        source = self.store.rows("SELECT id FROM locations")[0]["id"]
        observations.dirty(self.app, source)
        before = self.store.rows("SELECT * FROM observations")
        spawn = subprocess.Popen

        def slow(*args, **kwargs):
            return spawn(
                [sys.executable, "-c", "import time; time.sleep(10)"], **kwargs
            )

        with patch("subprocess.Popen", side_effect=slow):
            result = observations.observe(self.app, source, isolate=True, timeout=0.05)
        self.assertEqual(result["report"]["availability"], "unknown")
        self.assertFalse(result["report"]["complete"])
        self.assertEqual(self.store.rows("SELECT * FROM observations"), before)
