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
