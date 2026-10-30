# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native supervisor, isolated shared observation, processing and HTTP reactions."""

import json
import os
import sqlite3
import subprocess
import time
import unittest

from catabolic.app import Application
from catabolic.operations import Operations
from catabolic.store import Store
from tests import test_rust_http_mappings as http_base
from tests import test_rust_watchers as base
from tests.test_rust_migration import BINARY


class NativeWatcherRuntimeTest(unittest.TestCase):
    setUp = base.NativeWatchersTest.setUp

    def call(self, action, *, env=None, **request):
        completed = subprocess.run(
            [
                str(BINARY),
                "--db",
                str(self.database),
                "operation",
                "watcher",
                action,
                "--options",
                json.dumps(request),
            ],
            capture_output=True,
            text=True,
            timeout=30,
            env=os.environ | (env or {}),
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        return json.loads(completed.stdout)

    def test_parallel_supervisor_preserves_demand_and_finishes_all_callers(self):
        for index in range(4):
            name = f"audit{index}"
            self.call(
                "put",
                name=name,
                definition={
                    "plan": {"kind": "query", "query_id": self.query},
                    "reaction": "event",
                },
            )
            self.call("admit", name=name)
        result = self.call("tick", workers=4)
        deadline = time.monotonic() + 15
        while not result["complete"] and time.monotonic() < deadline:
            time.sleep(1.05)
            result = self.call("tick", workers=4)
        self.assertTrue(result["complete"], result)
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute(
                    "SELECT count(*) FROM watchers WHERE completed_generation>=requested_generation AND active_run IS NULL"
                ).fetchone()[0],
                4,
            )
            self.assertEqual(
                db.execute(
                    "SELECT count(*) FROM consumer_events WHERE event='watcher_changed'"
                ).fetchone()[0],
                4,
            )
            self.assertEqual(db.execute("SELECT count(*) FROM scans").fetchone()[0], 1)

    def test_supervisor_timeout_does_not_cancel_shared_observation(self):
        for index in range(70):
            (self.source / f"slow{index:03}.txt").write_text("header")
        self.call(
            "put",
            name="slow",
            definition={
                "plan": {"kind": "observation", "sources": ["media"]},
                "max_age_seconds": 0,
            },
        )
        self.call("admit", name="slow")
        result = self.call(
            "tick",
            workers=1,
            timeout_seconds=1,
            env={"CATABOLIC_TEST_SCAN_ENTRY_DELAY_MS": "40"},
        )
        self.assertFalse(result["complete"], result)
        with sqlite3.connect(self.database) as db:
            job, pid, state = db.execute(
                "SELECT id,worker_pid,state FROM observation_jobs ORDER BY generation DESC LIMIT 1"
            ).fetchone()
            self.assertEqual(state, "running")
            os.kill(pid, 0)
            self.assertIsNone(
                db.execute(
                    "SELECT active_run FROM watchers WHERE name='slow'"
                ).fetchone()[0]
            )
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            with sqlite3.connect(self.database) as db:
                state = db.execute(
                    "SELECT state FROM observation_jobs WHERE id=?", (job,)
                ).fetchone()[0]
            if state == "complete":
                break
            time.sleep(0.05)
        self.assertEqual(state, "complete")
        with sqlite3.connect(self.database) as db:
            db.execute("UPDATE watchers SET retry_at=0 WHERE name='slow'")
        result = self.call("tick", workers=1)
        self.assertTrue(result["complete"], result)
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute("SELECT count(*) FROM observation_jobs").fetchone()[0], 2
            )

    def test_processing_reaction_admits_pinned_analysis_once(self):
        with Store(self.database, writable=True) as store:
            app = Application(store)
            file_id = store.rows("SELECT id FROM files")[0]["id"]
            item = app.put_item("movie", {"fixture": "watcher"}, {"title": "Title"})[
                "id"
            ]
            app.media.associate(file_id, item)
            operation = Operations(app).put(
                "hash", {"kind": "analysis", "operation": "hash", "options": {}}
            )["id"]
        self.call(
            "put",
            name="analysis",
            definition={
                "plan": {"kind": "query", "query_id": self.query},
                "reaction": "processing",
                "operation_id": operation,
            },
        )
        first = self.call("run", name="analysis")
        self.assertTrue(first["complete"], first)
        second = self.call("run", name="analysis")
        self.assertTrue(second["complete"], second)
        self.assertEqual(
            first["reaction"]["jobs"][0]["input"],
            second["reaction"]["jobs"][0]["input"],
        )
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute("SELECT state,recipe_id FROM processing_jobs").fetchall(),
                [("queued", operation)],
            )
            self.assertEqual(
                db.execute("SELECT count(*) FROM processing_attempts").fetchone()[0], 0
            )

    def test_processing_capability_wait_releases_writer_and_preserves_new_demand(self):
        with Store(self.database, writable=True) as store:
            app = Application(store)
            file_id = store.rows("SELECT id FROM files")[0]["id"]
            item = app.put_item("movie", {"fixture": "wait"}, {"title": "Title"})["id"]
            app.media.associate(file_id, item)
            operation = Operations(app).put(
                "probe", {"kind": "analysis", "operation": "probe", "options": {}}
            )["id"]
        self.call(
            "put",
            name="wait",
            definition={
                "plan": {"kind": "query", "query_id": self.query},
                "reaction": "processing",
                "operation_id": operation,
            },
        )
        tools = self.root / "tools"
        tools.mkdir()
        ready = self.root / "ready"
        release = self.root / "release"
        probe = tools / "ffprobe"
        probe.write_text(
            '#!/bin/sh\n: > "$WATCHER_READY"\nwhile [ ! -f "$WATCHER_RELEASE" ]; do sleep 0.01; done\necho fixture-version\n'
        )
        probe.chmod(0o755)
        child = subprocess.Popen(
            [
                str(BINARY),
                "--db",
                str(self.database),
                "operation",
                "watcher",
                "run",
                "--options",
                json.dumps({"name": "wait"}),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=os.environ
            | {
                "PATH": str(tools) + os.pathsep + os.environ.get("PATH", ""),
                "WATCHER_READY": str(ready),
                "WATCHER_RELEASE": str(release),
            },
        )
        deadline = time.monotonic() + 10
        while not ready.exists() and time.monotonic() < deadline:
            self.assertIsNone(child.poll())
            time.sleep(0.01)
        try:
            self.assertTrue(ready.exists())
            requested = self.call("admit", name="wait")
            self.assertIsNone(child.poll())
        finally:
            release.touch()
        output, error = child.communicate(timeout=10)
        self.assertEqual(child.returncode, 0, error)
        self.assertTrue(json.loads(output)["complete"], output)
        row = self.call("get", name="wait")
        self.assertEqual(row["requested_generation"], requested["requested_generation"])
        self.assertGreater(row["requested_generation"], row["completed_generation"])
        self.assertTrue(self.call("due", name="wait")["due"])

    def test_replaced_run_during_tool_wait_cannot_complete_or_clear_new_demand(self):
        with Store(self.database, writable=True) as store:
            app = Application(store)
            file_id = store.rows("SELECT id FROM files")[0]["id"]
            item = app.put_item("movie", {"fixture": "replacement"}, {"title": "Title"})["id"]
            app.media.associate(file_id, item)
            operation = Operations(app).put(
                "probe", {"kind": "analysis", "operation": "probe", "options": {}}
            )["id"]
        self.call("put", name="replacement", definition={
            "plan": {"kind": "query", "query_id": self.query},
            "reaction": "processing", "operation_id": operation,
        })
        tools = self.root / "replacement-tools"
        tools.mkdir()
        ready, release = self.root / "replacement-ready", self.root / "replacement-release"
        probe = tools / "ffprobe"
        probe.write_text('#!/bin/sh\n: > "$WATCHER_READY"\nwhile [ ! -f "$WATCHER_RELEASE" ]; do sleep 0.01; done\necho fixture-version\n')
        probe.chmod(0o755)
        child = subprocess.Popen(
            [str(BINARY), "--db", str(self.database), "operation", "watcher", "run",
             "--options", json.dumps({"name": "replacement"})],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            env=os.environ | {"PATH": str(tools) + os.pathsep + os.environ.get("PATH", ""),
                              "WATCHER_READY": str(ready), "WATCHER_RELEASE": str(release)},
        )
        deadline = time.monotonic() + 10
        while not ready.exists() and time.monotonic() < deadline:
            self.assertIsNone(child.poll())
            time.sleep(0.01)
        try:
            self.assertTrue(ready.exists())
            with Store(self.database, writable=True) as store:
                with store.transaction() as db:
                    old, definition, generation = db.execute(
                        "SELECT active_run,definition_id,pending_generation FROM watchers WHERE name='replacement'"
                    ).fetchone()
                    self.assertIsNotNone(old)
                    db.execute("INSERT INTO watcher_runs(id,profile,watcher,definition_id,generation,trigger,scheduled_at,started_at,state) VALUES('new-run','default','replacement',?,?,'manual',0,0,'observing')", (definition, generation + 1))
                    db.execute("UPDATE watchers SET active_run='new-run',lease_until=?,pending_generation=?,requested_generation=? WHERE name='replacement'", (time.time() + 3600, generation + 1, generation + 1))
        finally:
            release.touch()
        output, error = child.communicate(timeout=30)
        self.assertEqual(child.returncode, 0, error)
        result = json.loads(output)
        self.assertFalse(result["complete"])
        self.assertEqual(result["error"], "stale_watcher_run")
        with sqlite3.connect(self.database) as db:
            active, pending, requested, completed = db.execute(
                "SELECT active_run,pending_generation,requested_generation,completed_generation FROM watchers WHERE name='replacement'"
            ).fetchone()
            new_state = db.execute("SELECT state FROM watcher_runs WHERE id='new-run'").fetchone()[0]
        self.assertEqual(active, "new-run")
        self.assertEqual((pending, requested), (generation + 1, generation + 1))
        self.assertLess(completed, pending)
        self.assertEqual(new_state, "observing")

    def test_processing_budget_failure_retains_retry_demand(self):
        with Store(self.database, writable=True) as store:
            app = Application(store)
            file_id = store.rows("SELECT id FROM files")[0]["id"]
            for number in range(2):
                item = app.put_item(
                    "movie", {"fixture": str(number)}, {"title": "Title"}
                )["id"]
                app.media.associate(file_id, item)
            operation = Operations(app).put(
                "hash", {"kind": "analysis", "operation": "hash", "options": {}}
            )["id"]
        self.call(
            "put",
            name="ambiguous",
            definition={
                "plan": {"kind": "query", "query_id": self.query},
                "reaction": "processing",
                "operation_id": operation,
            },
        )
        result = self.call("run", name="ambiguous")
        self.assertFalse(result["complete"])
        self.assertIn("unambiguous primary", result["error"])
        row = self.call("get", name="ambiguous")
        self.assertIsNotNone(row["pending_reaction"])
        self.assertGreater(row["retry_at"], 0)
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute("SELECT count(*) FROM processing_jobs").fetchone()[0], 0
            )


class NativeWatcherHTTPReactionTest(unittest.TestCase):
    setUp = http_base.NativeHTTPMappingTest.setUp
    native = http_base.NativeHTTPMappingTest.native
    title = http_base.NativeHTTPMappingTest.title

    def test_http_reaction_atomically_admits_and_reuses_delivery_without_sending(self):
        definition = {
            "plan": {"kind": "query", "query_id": self.definition["query"]},
            "reaction": "http",
            "mapping": self.definition,
        }
        self.native("watcher", "put", {"name": "mapped", "definition": definition})
        first = self.native("watcher", "run", {"name": "mapped"})
        self.assertTrue(first["complete"], first)
        self.assertEqual(first["reaction"]["queued"], 1)
        self.assertFalse(first["reaction"]["delivery_acknowledged"])
        second = self.native("watcher", "run", {"name": "mapped"})
        self.assertEqual(second["reaction"]["queued"], 0)
        self.assertEqual(self.received, [])
        self.title("Changed")
        third = self.native("watcher", "run", {"name": "mapped"})
        self.assertEqual(third["reaction"]["queued"], 1)
        with Store(self.database) as store:
            self.assertEqual(
                [
                    r["state"]
                    for r in store.rows(
                        "SELECT state FROM notification_deliveries ORDER BY rowid"
                    )
                ],
                ["cancelled", "pending"],
            )

    def test_http_budget_failure_does_not_commit_baseline_or_delivery(self):
        definition = {
            "plan": {"kind": "query", "query_id": self.definition["query"]},
            "reaction": "http",
            "mapping": self.definition,
            "max_changes": 0,
        }
        self.native("watcher", "put", {"name": "blocked", "definition": definition})
        result = self.native("watcher", "run", {"name": "blocked"})
        self.assertFalse(result["complete"])
        self.assertIn("change_budget", result["error"])
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT baseline,pending_reaction FROM watchers"),
                [{"baseline": None, "pending_reaction": None}],
            )
            self.assertEqual(store.rows("SELECT * FROM notification_deliveries"), [])


if __name__ == "__main__":
    unittest.main()
