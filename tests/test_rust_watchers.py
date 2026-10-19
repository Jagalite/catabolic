# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native watcher lifecycle and frozen numeric-cron differential scenarios."""

import shutil
import sqlite3
import tempfile
import time
import unittest
from datetime import datetime
from pathlib import Path

from catabolic import source_events
from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.saved_queries import Queries
from catabolic.store import Store, encode
from catabolic.supervisor import due
from catabolic.watcher_schedule import fields, next_due
from catabolic.watchers import Watchers
from tests.test_rust_migration import BINARY
from tests.test_rust_observation_filesystem import native


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeWatchersTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.database = self.root / "native.sqlite3"
        self.reference = self.root / "python.sqlite3"
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "one.mkv").write_bytes(b"media sample")
        Store.initialize(self.database)
        with Store(self.database, writable=True) as store:
            app = Application(store)
            app.bind("source", "media", str(self.source))
            app.scan("media")
            self.query = Queries(store).put(
                "audit",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM files",
                    }
                },
            )["id"]
        shutil.copyfile(self.database, self.reference)

    def call(self, action, **args):
        return native(self.database, "watcher", action, **args)

    def comparable(self, row):
        row = dict(row)
        row.pop("definition_id", None)
        return row

    def test_defaults_digest_idempotency_and_disabled_revision_history(self):
        definitions = [
            {"plan": {"kind": "query", "query_id": self.query}},
            {
                "plan": {
                    "kind": "observation",
                    "sources": ["media", "media"],
                    "exclusions": ["private", "private"],
                }
            },
            {
                "plan": {"kind": "query", "query_id": self.query},
                "reaction": "event",
                "events": True,
                "schedule": {"kind": "interval", "seconds": 90},
            },
        ]
        for index, definition in enumerate(definitions):
            name = "watch" + str(index)
            with Store(self.reference, writable=True) as store:
                expected = Watchers(Application(store)).put(name, definition)
            actual = self.call("put", name=name, definition=definition)
            self.assertEqual(self.comparable(actual), self.comparable(expected))
            self.assertEqual(self.call("put", name=name, definition=definition), actual)
            enabled = self.call("enable", name=name)
            self.assertEqual(enabled["enabled"], 1)
            with self.assertRaisesRegex(CatabolicError, "disable and finish"):
                self.call("put", name=name, definition=definition)
            self.call("disable", name=name)
            revised = dict(definition, debounce_seconds=9)
            self.assertEqual(
                self.call("put", name=name, definition=revised)["revision"], 2
            )

    def test_report_event_runs_share_current_inventory_and_compare_baselines(self):
        with Store(self.reference, writable=True) as store:
            source_events.invalidate(Application(store), "media")
        native(self.database, "observe", "dirty", source="media")
        counts = []
        for index in range(4):
            definition = {
                "plan": {"kind": "query", "query_id": self.query},
                "reaction": "event",
            }
            name = "audit" + str(index)
            with Store(self.reference, writable=True) as store:
                watchers = Watchers(Application(store))
                watchers.put(name, definition)
                expected = watchers.run(name)
            self.call("put", name=name, definition=definition)
            actual = self.call("run", name=name)
            self.assertTrue(actual["complete"], actual)
            self.assertEqual(actual["evaluation"], expected["evaluation"])
            self.assertEqual(actual["reaction"], expected["reaction"])
            second = self.call("run", name=name)
            self.assertFalse(second["evaluation"]["changed"])
        for path in (self.reference, self.database):
            with sqlite3.connect(path) as db:
                counts.append(db.execute("SELECT count(*) FROM scans").fetchone()[0])
                self.assertEqual(
                    db.execute(
                        "SELECT count(*) FROM consumer_events WHERE event='watcher_changed'"
                    ).fetchone()[0],
                    4,
                )
        self.assertEqual(counts, [2, 2])

    def test_observation_only_run_has_no_baseline_or_reaction(self):
        definition = {
            "plan": {"kind": "observation", "sources": ["media"]},
            "require_complete_inventory": True,
        }
        self.call("put", name="inventory", definition=definition)
        result = self.call("run", name="inventory")
        self.assertTrue(result["complete"], result)
        self.assertIsNone(result["reaction"])
        row = self.call("get", name="inventory")
        self.assertIsNone(row["baseline"])
        self.assertIsNone(row["active_run"])
        self.assertEqual(row["completed_generation"], 1)

    def test_manual_admission_debounce_retry_and_lease_schedule_match_python(self):
        definition = {"plan": {"kind": "query", "query_id": self.query}, "events": True}
        self.call("put", name="due", definition=definition)
        self.call("enable", name="due", now=1000)
        base = self.call("get", name="due")
        cases = [
            {},
            {"requested_generation": 1},
            {"enabled": 0, "requested_generation": 1},
            {"active_run": "active", "lease_until": 1200, "requested_generation": 1},
            {"retry_at": 1100, "requested_generation": 1},
            {"retry_at": 900},
            {"pending_generation": 1, "event_first": 990, "event_last": 999},
            {"pending_generation": 1, "event_first": 930, "event_last": 999},
        ]
        for changes in cases:
            row = {**base, **changes}
            with sqlite3.connect(self.database) as db:
                keys = [
                    "enabled",
                    "requested_generation",
                    "active_run",
                    "lease_until",
                    "retry_at",
                    "pending_generation",
                    "event_first",
                    "event_last",
                ]
                db.execute(
                    "UPDATE watchers SET "
                    + ",".join(k + "=?" for k in keys)
                    + " WHERE name='due'",
                    [row[k] for k in keys],
                )
            expected = due({**row, "definition": encode(row["definition"])}, 1000)
            self.assertEqual(self.call("due", name="due", now=1000)["due"], expected)

    def test_cron_numeric_and_dst_fold_gap_match_python(self):
        cases = [
            ("30 2 * * *", "America/New_York", "2026-03-08T06:00:00+00:00"),
            ("30 1 * * *", "America/New_York", "2026-11-01T05:31:00+00:00"),
            ("*/15 2-4 * * 1,3,5", "UTC", "2026-10-11T17:00:00+00:00"),
            ("5 0 1 * 1", "Asia/Kathmandu", "2026-10-11T17:00:00+00:00"),
            ("30 2 * * *", "Europe/London", "2026-03-28T23:00:00+00:00"),
            ("30 2 * * *", "America/Asuncion", "2026-06-28T23:00:00+00:00"),
            ("30 2 * * *", "America/Coyhaique", "2026-06-28T23:00:00+00:00"),
            ("30 2 * * *", "Africa/Casablanca", "2026-03-28T23:00:00+00:00"),
        ]
        for cron, zone, stamp in cases:
            schedule = {"kind": "cron", "cron": cron, "timezone": zone}
            now = datetime.fromisoformat(stamp).timestamp()
            self.assertEqual(
                self.call("schedule", schedule=schedule, now=now)["next_due"],
                next_due(schedule, now),
            )
        for cron in (
            "* * * * */0",
            "1 2 3",
            "60 * * * *",
            "* * * JAN *",
            "3-1 * * * *",
        ):
            with self.assertRaises(CatabolicError):
                fields(cron)
            with self.assertRaises(CatabolicError):
                self.call(
                    "schedule",
                    schedule={"kind": "cron", "cron": cron, "timezone": "UTC"},
                    now=time.time(),
                )

    def test_schedule_rejects_unsafe_zone_paths_and_extreme_clocks(self):
        for zone in ("../etc/passwd", "/etc/passwd", "America//New_York", ""):
            with self.assertRaises(CatabolicError):
                self.call(
                    "schedule",
                    schedule={"kind": "cron", "cron": "* * * * *", "timezone": zone},
                    now=1,
                )
        with self.assertRaisesRegex(CatabolicError, "out of range"):
            self.call(
                "schedule",
                schedule={"kind": "cron", "cron": "* * * * *", "timezone": "UTC"},
                now=1e308,
            )

    def test_dead_pid_recovery_preserves_run_fence(self):
        definition = {"plan": {"kind": "query", "query_id": self.query}}
        row = self.call("put", name="recover", definition=definition)
        with sqlite3.connect(self.database) as db:
            db.execute(
                "INSERT INTO watcher_runs(id,profile,watcher,definition_id,generation,trigger,scheduled_at,started_at,state,worker_pid) VALUES('dead','default','recover',?,1,'manual',0,0,'observing',2147483647)",
                (row["definition_id"],),
            )
            db.execute(
                "UPDATE watchers SET active_run='dead',lease_until=9999999999 WHERE name='recover'"
            )
        self.assertEqual(self.call("recover", now=1000)["recovered"], ["dead"])
        row = self.call("get", name="recover")
        self.assertIsNone(row["active_run"])
        self.assertEqual(row["retry_at"], 1000)
        self.assertEqual(
            self.call("history", name="recover")[0]["state"], "interrupted"
        )
