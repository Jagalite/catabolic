# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""One-shot observation admission against disposable catalogs and real traversal."""

import sqlite3
import subprocess
import sys
import time
import unittest
from unittest.mock import patch

from catabolic import observations
from catabolic.app import Application, walk_files
from catabolic.domain import CatabolicError
from catabolic.store import Store
from catabolic.watchers import Watchers
from tests import test_application as fixtures


class ObservationServiceTest(unittest.TestCase):
    setUp = fixtures.CatalogTest.setUp

    def count(self):
        return self.store.rows("SELECT count(*) AS n FROM scans")[0]["n"]

    def test_manual_and_scheduled_demands_share_traversal(self):
        before = self.count()
        manual = observations.request(
            self.app, "media", after=time.time(), reuse_completed=False
        )
        scheduled = observations.request(self.app, "media", max_age=60)
        self.assertEqual(manual["id"], scheduled["id"])
        self.assertNotEqual(manual["request_id"], scheduled["request_id"])
        self.assertEqual(scheduled["reuse"], "inflight")
        result = observations.observe(
            self.app, "media", request_id=manual["request_id"]
        )
        self.assertEqual(result["state"], "complete")
        other = observations.observe(
            self.app, "media", request_id=scheduled["request_id"]
        )
        self.assertEqual(other["scan_id"], result["scan_id"])
        self.assertEqual(self.count() - before, 1)

    def test_completed_reuse_and_manual_scan_now(self):
        before = self.count()
        reused = observations.observe(self.app, "media", max_age=60)
        self.assertEqual(reused["reuse"], "completed")
        self.assertEqual(self.count(), before)
        result = self.app.scan("media")
        self.assertTrue(result["complete"])
        self.assertEqual(self.count(), before + 1)
        self.assertGreaterEqual(
            result["observations"][0]["started_at"],
            result["observations"][0]["guarantees"]["after"],
        )

    def test_strict_barrier_queues_followup_without_competing_traversal(self):
        first = observations.request(
            self.app, "media", after=time.time(), reuse_completed=False
        )
        running = observations.claim(self.app, first)
        strict = observations.request(
            self.app,
            "media",
            after=running["started_at"] + 0.001,
            reuse_completed=False,
        )
        self.assertNotEqual(first["id"], strict["id"])
        pending = observations.execute(self.app, strict)
        self.assertEqual(pending["state"], "queued")
        self.assertIn(
            pending["blocker"], ("observation_capacity", "observation_barrier")
        )
        self.app._execute_observation(running)
        result = observations.observe(
            self.app, "media", request_id=strict["request_id"], wait=True
        )
        self.assertEqual(result["state"], "complete")
        self.assertGreaterEqual(result["started_at"], strict["guarantees"]["after"])

    def test_retry_preserves_barrier_and_restart_identity(self):
        request = observations.request(
            self.app, "media", max_age=0, after=time.time(), requester="stable-demand"
        )
        with self.store.transaction() as db:
            db.execute(
                "UPDATE observation_jobs SET state='failed' WHERE id=?",
                (request["id"],),
            )
        with self.store.detached():
            with Store(self.database, writable=True) as store:
                resumed = observations.request(
                    Application(store),
                    "media",
                    max_age=0,
                    after=time.time() + 100,
                    requester="stable-demand",
                )
                self.assertEqual(resumed["request_id"], request["request_id"])
                self.assertEqual(resumed["guarantees"], request["guarantees"])
                self.assertNotEqual(resumed["id"], request["id"])
                self.assertEqual(
                    observations.execute(Application(store), resumed)["state"],
                    "complete",
                )

    def test_scope_policy_binding_and_completeness_are_separate(self):
        first = observations.request(self.app, "media", max_age=60)
        excluded = observations.request(
            self.app, "media", max_age=60, exclusions=["private"]
        )
        strict = observations.request(
            self.app, "media", max_age=60, require_complete=True
        )
        self.assertEqual(len({first["id"], excluded["id"], strict["id"]}), 3)
        original = observations.compatibility(self.app, "media")
        with patch(
            "catabolic.observations.source_policy", return_value={"revision": 999}
        ):
            self.assertNotEqual(original, observations.compatibility(self.app, "media"))
        with self.store.transaction() as db:
            db.execute(
                "UPDATE bindings SET inode=inode+1 WHERE kind='source' AND owner='media'"
            )
        self.assertNotEqual(original, observations.compatibility(self.app, "media"))

    def test_event_during_traversal_remains_pending(self):
        observations.dirty(self.app, "media")
        request = observations.request(self.app, "media", max_age=60)

        def changed(*args, **kwargs):
            with Store(self.database, writable=True) as store:
                observations.dirty(Application(store), "media")
            return walk_files(*args, **kwargs)

        with patch("catabolic.app.walk_files", side_effect=changed):
            result = observations.execute(self.app, request)
        self.assertEqual(result["state"], "complete")
        fresh = observations.request(self.app, "media", max_age=60)
        self.assertGreater(fresh["dirty_generation"], result["dirty_generation"])
        self.assertNotEqual(fresh["id"], result["id"])

    def test_cancelling_one_request_preserves_other_request(self):
        first = observations.request(
            self.app, "media", after=time.time(), reuse_completed=False
        )
        second = observations.request(self.app, "media", max_age=60)
        observations.cancel(self.app, first["request_id"])
        self.assertEqual(
            observations.resume(self.app, first["request_id"])["state"], "cancelled"
        )
        self.assertEqual(
            observations.observe(self.app, "media", request_id=second["request_id"])[
                "state"
            ],
            "complete",
        )

    def test_offline_and_failed_inventory_are_retained(self):
        self.source.rename(self.root / "offline")
        report = self.app.scan("media")
        self.assertFalse(report["complete"])
        self.assertEqual(report["observations"][0]["state"], "unavailable")
        self.assertEqual(self.app.files()["files"][0]["status"], "present")
        (self.root / "offline").rename(self.source)
        with patch(
            "catabolic.app.walk_files", return_value=([], ["permission denied"])
        ):
            report = self.app.scan("media")
        self.assertEqual(report["observations"][0]["state"], "failed")
        self.assertEqual(self.app.files()["files"][0]["status"], "present")

    def test_dead_worker_recovers_before_lease_and_retries(self):
        request = observations.request(
            self.app, "media", after=time.time(), reuse_completed=False
        )
        observations.claim(self.app, request)
        with self.store.transaction() as db:
            db.execute(
                "UPDATE observation_jobs SET worker_pid=2147483000 WHERE id=?",
                (request["id"],),
            )
        observations._release_guard(self.app, request)  # Simulate process exit.
        result = observations.observe(
            self.app, "media", request_id=request["request_id"]
        )
        self.assertEqual(result["state"], "complete")
        self.assertNotEqual(result["id"], request["id"])
        self.assertEqual(result["guarantees"], request["guarantees"])

    def test_scan_only_watcher_has_no_query_reaction_or_processing(self):
        watchers = Watchers(self.app)
        value = watchers.put(
            "inventory", {"plan": {"kind": "observation", "sources": ["media"]}}
        )
        self.assertIsNone(value["definition"]["reaction"])
        self.assertEqual(value["enabled"], 0)
        before = self.count()
        with (
            patch(
                "catabolic.plans.evaluate",
                side_effect=AssertionError("query forbidden"),
            ),
            patch(
                "catabolic.watcher_processing.admit",
                side_effect=AssertionError("processing forbidden"),
            ),
        ):
            result = watchers.run("inventory")
        self.assertTrue(result["complete"], result)
        self.assertEqual(self.count(), before)
        self.assertIsNone(watchers.get("inventory")["pending_reaction"])
        self.assertIsNone(watchers.get("inventory")["baseline"])
        for table in ("processing_jobs", "journal"):
            self.assertEqual(
                self.store.rows(f"SELECT count(*) AS n FROM {table}")[0]["n"], 0
            )

    def test_scan_only_watcher_retry_reuses_original_demand(self):
        watchers = Watchers(self.app)
        watchers.put(
            "inventory",
            {
                "plan": {"kind": "observation", "sources": ["media"]},
                "observe_after_request": True,
            },
        )
        with patch(
            "catabolic.observations.execute", side_effect=lambda app, job, **kw: job
        ):
            self.assertFalse(watchers.run("inventory")["complete"])
        before = self.store.rows(
            "SELECT * FROM observation_requests WHERE requester LIKE 'watcher:%'"
        )[0]
        self.assertTrue(watchers.run("inventory")["complete"])
        after = self.store.rows(
            "SELECT * FROM observation_requests WHERE requester LIKE 'watcher:%'"
        )
        self.assertEqual(len(after), 1)
        self.assertEqual(before["guarantees"], after[0]["guarantees"])

    def test_resume_cli_wrapper_keeps_source_scope_and_barrier(self):
        request = observations.request(
            self.app,
            "media",
            after=time.time(),
            exclusions=["private"],
            reuse_completed=False,
        )
        result = self.app.scan(request_id=request["request_id"])
        self.assertTrue(result["complete"])
        self.assertEqual(result["scans"][0]["excluded"], ["private"])
        self.assertEqual(result["observations"][0]["guarantees"], request["guarantees"])
        with self.assertRaises(CatabolicError):
            self.app.scan("media", request_id=request["request_id"])
        with self.assertRaises(CatabolicError):
            observations.observe(
                Application(self.store, "other"),
                "media",
                request_id=request["request_id"],
            )

    def test_watcher_joins_a_real_manual_traversal(self):
        watchers = Watchers(self.app)
        watchers.put(
            "inventory", {"plan": {"kind": "observation", "sources": ["media"]}}
        )
        before = self.count()

        def join(*args, **kwargs):
            with Store(self.database, writable=True) as store:
                pending = Watchers(Application(store)).run("inventory")
                self.assertFalse(pending["complete"])
                self.assertEqual(pending["error"], "observation_pending")
            return walk_files(*args, **kwargs)

        with patch("catabolic.app.walk_files", side_effect=join):
            manual = self.app.scan("media")
        scheduled = watchers.run("inventory")
        self.assertTrue(scheduled["complete"])
        self.assertEqual(
            manual["observations"][0]["id"], scheduled["observations"][0]["id"]
        )
        self.assertEqual(self.count(), before + 1)

    def test_cancellation_during_scan_does_not_discard_shared_publication(self):
        first = observations.request(
            self.app, "media", after=time.time(), reuse_completed=False
        )
        second = observations.request(self.app, "media", max_age=60)

        def cancel(*args, **kwargs):
            with Store(self.database, writable=True) as store:
                observations.cancel(Application(store), first["request_id"])
            return walk_files(*args, **kwargs)

        with patch("catabolic.app.walk_files", side_effect=cancel):
            result = observations.execute(self.app, first)
        self.assertEqual(result["state"], "cancelled")
        self.assertEqual(result["job_state"], "complete")
        self.assertEqual(
            observations.observe(self.app, "media", request_id=second["request_id"])[
                "state"
            ],
            "complete",
        )

    def test_writer_contention_during_traversal_reconnects_and_publishes(self):
        ready = self.root / "writer-ready"
        children = []

        def concurrent_writer(*args, **kwargs):
            child = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    "import sys,time; from pathlib import Path; from catabolic.store import Store; "
                    "s=Store(sys.argv[1],writable=True); Path(sys.argv[2]).touch(); time.sleep(.2); s.close()",
                    str(self.database),
                    str(ready),
                ]
            )
            children.append(child)
            deadline = time.monotonic() + 5
            while not ready.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(ready.exists())
            return walk_files(*args, **kwargs)

        try:
            with patch("catabolic.app.walk_files", side_effect=concurrent_writer):
                self.assertTrue(self.app.scan("media")["complete"])
        finally:
            for child in children:
                child.wait(timeout=5)

    def test_source_event_transaction_does_not_lose_dirty_hint(self):
        from catabolic.source_events import invalidate

        before = self.store.rows(
            "SELECT dirty_generation AS n FROM observation_sources WHERE source='media'"
        )[0]["n"]
        with patch(
            "catabolic.source_events.dirty_watchers", side_effect=CatabolicError("busy")
        ):
            with self.assertRaises(CatabolicError):
                invalidate(self.app, "media")
        self.assertEqual(
            self.store.rows(
                "SELECT dirty_generation AS n FROM observation_sources WHERE source='media'"
            )[0]["n"],
            before,
        )
        invalidate(self.app, "media")
        self.assertEqual(
            self.store.rows(
                "SELECT dirty_generation AS n FROM observation_sources WHERE source='media'"
            )[0]["n"],
            before + 1,
        )

    def test_incompatible_queued_scopes_can_execute_in_either_order(self):
        first = observations.request(
            self.app, "media", exclusions=["a"], reuse_completed=False
        )
        second = observations.request(
            self.app, "media", exclusions=["b"], reuse_completed=False
        )
        self.assertEqual(observations.execute(self.app, second)["state"], "complete")
        self.assertEqual(observations.execute(self.app, first)["state"], "complete")

    def test_failed_worker_launch_does_not_pin_parent_capacity(self):
        request = observations.request(
            self.app, "media", after=time.time(), reuse_completed=False
        )
        with patch("subprocess.Popen", side_effect=OSError("launch failed")):
            with self.assertRaises(OSError):
                observations.execute(self.app, request, isolate=True)
        self.assertEqual(
            self.store.rows(
                "SELECT state FROM observation_jobs WHERE id=?", (request["id"],)
            )[0]["state"],
            "failed",
        )
        self.assertEqual(
            observations.observe(self.app, "media", request_id=request["request_id"])[
                "state"
            ],
            "complete",
        )

    def test_cancelled_direct_executor_leaves_other_demand_retryable(self):
        first = observations.request(
            self.app, "media", after=time.time(), reuse_completed=False
        )
        second = observations.request(self.app, "media", max_age=60)
        with patch("catabolic.app.walk_files", side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                observations.execute(self.app, first)
        result = observations.observe(
            self.app, "media", request_id=second["request_id"]
        )
        self.assertEqual(result["state"], "complete")

    def test_reconnect_exhaustion_preserves_error_and_releases_live_claim(self):
        from catabolic.database_io import CatalogBusy

        request = observations.request(self.app, "media", after=time.time())
        with patch(
            "catabolic.store.acquire_writer_lock", side_effect=CatalogBusy("busy")
        ):
            with self.assertRaisesRegex(CatalogBusy, "busy"):
                observations.execute(self.app, request)
        # A different session can recover without waiting for this process to die.
        with Store(self.database, writable=True) as store:
            result = observations.observe(
                Application(store), "media", request_id=request["request_id"]
            )
            self.assertEqual(result["state"], "complete")
            self.assertEqual(result["id"], request["id"])
            self.assertEqual(result["guarantees"], request["guarantees"])

    def test_retry_reuses_compatible_success_but_not_unavailable_evidence(self):
        request = observations.request(self.app, "media", after=time.time(), max_age=60)
        with patch(
            "catabolic.app.walk_files", return_value=([], ["temporary failure"])
        ):
            self.assertEqual(observations.execute(self.app, request)["state"], "failed")
        success = self.app.scan("media")["observations"][0]
        count = self.count()
        result = observations.observe(
            self.app, "media", request_id=request["request_id"]
        )
        self.assertEqual(result["id"], success["id"])
        self.assertEqual(result["reuse"], "completed")
        self.assertEqual(self.count(), count)
        offline = observations.request(self.app, "media", after=time.time())
        self.source.rename(self.root / "offline")
        self.assertEqual(
            observations.execute(self.app, offline)["state"], "unavailable"
        )
        self.assertNotEqual(
            observations.resume(self.app, offline["request_id"])["id"], offline["id"]
        )

    def test_cancelled_complete_request_keeps_object_reports(self):
        request = observations.request(self.app, "media", after=time.time())
        observations.execute(self.app, request)
        observations.cancel(self.app, request["request_id"])
        for result in (
            observations.resume(self.app, request["request_id"]),
            observations.execute(self.app, request),
        ):
            self.assertEqual(result["state"], "cancelled")
            self.assertEqual(result["job_state"], "complete")
            self.assertIsInstance(result["report"], dict)
        replay = self.app.scan(request_id=request["request_id"])
        self.assertFalse(replay["complete"])
        self.assertIsInstance(replay["scans"][0], dict)

    def test_guard_holds_claim_even_during_dead_parent_pid_handoff(self):
        request = observations.request(self.app, "media", after=time.time())
        observations.claim(self.app, request)
        with self.store.transaction() as db:
            db.execute(
                "UPDATE observation_jobs SET worker_pid=2147483000 WHERE id=?",
                (request["id"],),
            )
        observations.recover(self.app)
        self.assertEqual(
            self.store.rows(
                "SELECT state FROM observation_jobs WHERE id=?", (request["id"],)
            )[0]["state"],
            "running",
        )
        following = observations.request(self.app, "media", after=time.time())
        self.assertEqual(
            observations.execute(self.app, following)["blocker"], "observation_capacity"
        )
        observations._release_guard(self.app, request)
        self.assertEqual(
            observations.observe(self.app, "media", request_id=following["request_id"])[
                "state"
            ],
            "complete",
        )

    def test_claim_transaction_failure_releases_guard(self):
        request = observations.request(self.app, "media", after=time.time())
        with self.store.transaction() as db:
            db.execute(
                "CREATE TEMP TRIGGER reject_claim BEFORE UPDATE OF state ON observation_jobs WHEN NEW.state='running' BEGIN SELECT RAISE(ABORT,'claim failed'); END"
            )
        with self.assertRaisesRegex(sqlite3.IntegrityError, "claim failed"):
            observations.execute(self.app, request)
        with self.store.transaction() as db:
            db.execute("DROP TRIGGER reject_claim")
        self.assertEqual(
            observations.observe(self.app, "media", request_id=request["request_id"])[
                "state"
            ],
            "complete",
        )

    def test_isolated_worker_inherits_guard_after_requester_stops_waiting(self):
        request = observations.request(self.app, "media", after=time.time())
        spawn = subprocess.Popen
        children = []

        def delayed_worker(*args, **kwargs):
            child = spawn(
                [sys.executable, "-c", "import time; time.sleep(60)"], **kwargs
            )
            children.append(child)
            return child

        try:
            with patch("subprocess.Popen", side_effect=delayed_worker):
                result = observations.execute(
                    self.app, request, isolate=True, timeout=0.05
                )
            self.assertEqual(result["state"], "running")
            self.assertNotIn("observation:" + request["id"], self.store.after_close)
            observations.recover(self.app)
            self.assertEqual(
                self.store.rows(
                    "SELECT state FROM observation_jobs WHERE id=?", (request["id"],)
                )[0]["state"],
                "running",
            )
        finally:
            for child in children:
                child.kill()
                child.wait(timeout=5)
        result = observations.observe(
            self.app, "media", request_id=request["request_id"]
        )
        self.assertEqual(result["state"], "complete")
        self.assertNotEqual(result["id"], request["id"])

    def test_retry_does_not_resurrect_success_before_newer_unavailability(self):
        fresh = observations.request(self.app, "media", after=time.time())
        self.source.rename(self.root / "offline")
        observations.execute(self.app, fresh)
        request = observations.request(self.app, "media", max_age=60)
        self.assertEqual(request["state"], "unavailable")
        self.assertEqual(
            observations.resume(self.app, request["request_id"])["state"], "queued"
        )

    def test_observation_plan_rejects_reaction(self):
        with self.assertRaises(ValueError):
            Watchers(self.app).put(
                "bad",
                {
                    "plan": {"kind": "observation", "sources": ["media"]},
                    "reaction": "processing",
                    "operation_id": "anything",
                },
            )
