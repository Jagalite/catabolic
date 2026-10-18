# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Paired local mapping history and read-only HTTP recovery; no delivery worker.

Every catalog is a disposable private copy. SQL fixtures add event evidence, not
real destinations or credentials; no publication/recovery mutation owner runs.
"""

import fcntl
import json
import shutil
import signal
import sqlite3
import subprocess
import time
import unittest
from contextlib import closing
from pathlib import Path

from tests import test_rust_legacy_projection as projection


class NativeLegacyMappingHistoryTest(unittest.TestCase):
    setUp = projection.NativeLegacyProjectionTest.setUp
    command = projection.NativeLegacyProjectionTest.command
    run_cli = projection.NativeLegacyProjectionTest.run_cli
    compare = projection.NativeLegacyProjectionTest.compare
    snapshot = projection.NativeLegacyProjectionTest.snapshot

    def seed(self, *, http=3, publication=2, states=None, collision=False):
        states = states or ["pending", "complete", "cancelled"]
        with closing(sqlite3.connect(self.db[True])) as db:
            # Local-owner history has no HTTP grants, operation filter or token.
            db.execute("INSERT INTO profiles(id) VALUES ('other')")
            for profile in ("default", "other"):
                for i in range(http):
                    event = f"{profile}-http-{http - i:04}"
                    db.execute(
                        "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,'mapping_http','info','fixture')",
                        (event, profile),
                    )
                    db.execute(
                        "INSERT INTO http_mapping_deliveries(event_id,operation_id,mapping_id,key,plan_id,request) VALUES (?,?,?,?,?,'{}')",
                        (
                            event,
                            self.definition["operation"],
                            f"owner-{i % 2}",
                            f"key-{i}",
                            f"plan-{i}",
                        ),
                    )
                    db.execute(
                        "INSERT INTO notification_deliveries(event_id,profile,destination_id,state,attempts,error) VALUES (?,?,'fixture',?,?,?)",
                        (
                            event,
                            profile,
                            states[i % len(states)],
                            i,
                            "fixture" if i else None,
                        ),
                    )
                # Publication rows are later than HTTP rows but still come first
                # in the aggregate. IDs deliberately oppose insertion order.
                for i in range(publication):
                    payload = {
                        "owner": "publication-owner",
                        "key": f"publication-key-{i}",
                        "state": "uncertain",
                        "plan_id": f"publication-plan-{i}",
                        "version": 1 + i % 2,
                        "before": {
                            "z": "Ångström",
                            "a": {"state": "old", "id": "nested"},
                        },
                        "after": {"z": "new", "a": {"state": "new", "id": "nested"}},
                    }
                    db.execute(
                        "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,'mapping_publish','warning',?)",
                        (
                            f"{profile}-publication-{publication - i:04}",
                            profile,
                            json.dumps(payload, sort_keys=True),
                        ),
                    )
            db.execute(
                "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES ('ignored','default','other','info','not JSON')"
            )
            if collision:
                db.execute(
                    "UPDATE consumer_events SET event='mapping_publish',subject=? WHERE id='default-http-0001'",
                    (
                        json.dumps(
                            {
                                "owner": "collision",
                                "key": "same",
                                "state": "verified",
                                "plan_id": "collision",
                                "version": 3,
                            }
                        ),
                    ),
                )
            db.commit()
        shutil.copyfile(self.db[True], self.db[False])

    def assert_unchanged(self, before):
        for flag in (True, False):
            self.assertEqual(before[flag], self.snapshot(flag))

    def test_empty_history_unknown_events_and_profiles(self):
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        for mode in ([], ["--json"], ["--machine"]):
            for args in (
                ["projection", "mapping-events"],
                ["projection", "mapping-events", "--event", ""],
                ["projection", "mapping-events", "--event", "absent"],
                ["projection", "mapping-recover", "absent"],
                ["projection", "mapping-recover", "absent", "--apply"],
                ["--profile", "unknown", "projection", "mapping-events"],
            ):
                self.compare(mode + args)
        self.assert_unchanged(before)

    def test_combined_local_authority_profile_order_and_complete_events(self):
        self.seed()
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        for mode in ([], ["--json"], ["--machine"]):
            for profile in ("default", "other", "unknown"):
                self.compare(
                    mode + ["--profile", profile, "projection", "mapping-events"]
                )
            for event in (
                "default-publication-0002",
                "default-publication-0001",
                "default-http-0003",
                "default-http-0002",
                "other-http-0001",
            ):
                self.compare(mode + ["projection", "mapping-events", "--event", event])
        report = json.loads(
            self.run_cli(False, ["--json", "projection", "mapping-events"]).stdout
        )
        self.assertEqual(
            [e["id"] for e in report["events"]],
            [
                "default-publication-0002",
                "default-publication-0001",
                "default-http-0003",
                "default-http-0002",
                "default-http-0001",
            ],
        )
        self.assertFalse(report["truncated"])
        self.assert_unchanged(before)

    def test_last_hundred_and_http_thousand_cap_before_event_lookup(self):
        self.seed(http=1005, publication=103)
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        for mode in ([], ["--json"], ["--machine"]):
            for args in (
                ["projection", "mapping-events"],
                ["projection", "mapping-events", "--event", "default-publication-0103"],
                ["projection", "mapping-events", "--event", "default-http-1005"],
                ["projection", "mapping-events", "--event", "default-http-1000"],
                ["projection", "mapping-recover", "default-http-1005"],
                ["projection", "mapping-recover", "default-http-1000"],
            ):
                self.compare(mode + args)
        report = json.loads(
            self.run_cli(False, ["--json", "projection", "mapping-events"]).stdout
        )
        self.assertTrue(report["truncated"])
        self.assertEqual(len(report["events"]), 100)
        self.assertEqual(report["events"][0]["id"], "default-http-0100")
        self.assertEqual(report["events"][-1]["id"], "default-http-0001")
        self.assert_unchanged(before)

    def test_publication_only_last_hundred(self):
        self.seed(http=0, publication=101)
        for mode in ([], ["--json"], ["--machine"]):
            self.compare(mode + ["projection", "mapping-events"])
        report = json.loads(
            self.run_cli(False, ["--json", "projection", "mapping-events"]).stdout
        )
        self.assertTrue(report["truncated"])
        self.assertEqual(report["events"][0]["id"], "default-publication-0100")

    def test_http_queue_states_options_status_and_apply_refusal(self):
        states = [
            "pending",
            "complete",
            "cancelled",
            "running",
            "failed",
            "needs_repair",
        ]
        self.seed(http=len(states), publication=0, states=states)
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        for mode in ([], ["--json"], ["--machine"]):
            for i, state in enumerate(states):
                event = f"default-http-{len(states) - i:04}"
                for flags in (
                    [],
                    ["--resolution", "submitted", "--expected-plan", "ignored"],
                    ["--apply"],
                    [
                        "--apply",
                        "--resolution",
                        "not_applied",
                        "--expected-plan",
                        "ignored",
                    ],
                ):
                    old, new = self.compare(
                        mode + ["projection", "mapping-recover", event] + flags
                    )
                    self.assertEqual(
                        new.returncode,
                        2 if "--apply" in flags else 0 if state == "complete" else 3,
                    )
        self.assert_unchanged(before)

    def test_apply_metadata_lock_precedence_and_readonly_catalog_authority(self):
        self.seed()
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        locks = []
        for flag in (True, False):
            for suffix in (".lock", ".plex-metadata.lock"):
                lock = open(str(self.db[flag]) + suffix, "a+")
                self.addCleanup(lock.close)
                locks.append(lock)
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        for mode in ([], ["--json"], ["--machine"]):
            self.compare(mode + ["projection", "mapping-events"])
            self.compare(mode + ["projection", "mapping-recover", "default-http-0001"])
            self.compare(mode + ["projection", "mapping-recover", "missing", "--apply"])
        # The catalog writer lock alone must not block even a refused --apply.
        for lock in locks[1::2]:
            lock.close()
        for mode in ([], ["--json"], ["--machine"]):
            self.compare(
                mode + ["projection", "mapping-recover", "default-http-0001", "--apply"]
            )
            self.compare(mode + ["projection", "mapping-recover", "missing", "--apply"])
        for flag in (True, False):
            with open(str(self.db[flag]) + ".plex-metadata.lock", "r+") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.assert_unchanged(before)

    def test_duplicate_aggregate_event_first_lookup_but_recovery_rejected(self):
        self.seed(http=1, publication=0, collision=True)
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        for mode in ([], ["--json"], ["--machine"]):
            self.compare(mode + ["projection", "mapping-events"])
            # Version-three publication payloads aren't standard HTTP rows.
            self.compare(mode + ["projection", "mapping-recover", "default-http-0001"])
            self.compare(
                mode + ["projection", "mapping-recover", "default-http-0001", "--apply"]
            )
        result = self.run_cli(
            False,
            ["--json", "projection", "mapping-events", "--event", "default-http-0001"],
        )
        self.assertEqual(json.loads(result.stdout)["owner"], "collision")
        self.assert_unchanged(before)

    def test_publication_version_two_without_identity_refuses_explicitly(self):
        self.seed(http=0)
        before = self.snapshot(False)
        for version in (2,):
            event = f"default-publication-{3 - version:04}"
            for flags in (
                [],
                ["--apply"],
                ["--resolution", "submitted", "--expected-plan", "anything"],
            ):
                result = self.run_cli(
                    False, ["--machine", "projection", "mapping-recover", event] + flags
                )
                # These fixture events carry no database_id. Frozen recovery
                # raises KeyError; native refuses them as a structured,
                # documented safety difference, with nothing written.
                self.assertEqual(result.returncode, 2, result.stdout)
                self.assertIn("mapping_event_database_changed", result.stdout)
        self.assertEqual(before, self.snapshot(False))

    def test_missing_database_admission(self):
        for mode in ([], ["--json"], ["--machine"]):
            for args in (
                ["projection", "mapping-events"],
                ["projection", "mapping-recover", "missing"],
                ["projection", "mapping-recover", "missing", "--apply"],
            ):
                self.compare(mode + args, database=False)

    def test_arbitrary_stored_subject_order_is_preserved(self):
        self.seed(http=0, publication=1)
        for flag in (True, False):
            with closing(sqlite3.connect(self.db[flag])) as db:
                db.execute(
                    "UPDATE consumer_events SET subject=? WHERE id='default-publication-0001'",
                    (
                        json.dumps(
                            {
                                "version": 1,
                                "owner": "custom",
                                "key": "k",
                                "state": "uncertain",
                                "plan_id": "p",
                                "extra": {"z": 1, "a": 2},
                            }
                        ),
                    ),
                )
                db.commit()
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        for mode in ([], ["--json"], ["--machine"]):
            self.compare(
                mode
                + [
                    "projection",
                    "mapping-events",
                    "--event",
                    "default-publication-0001",
                ]
            )
        self.assert_unchanged(before)

    def test_readonly_interruption_catalog_and_lock_cleanup(self):
        self.seed(http=1, publication=0)
        # Enough legitimate publication evidence keeps this ordinary read in
        # flight while the parent observes its metadata lock. The Rust owner
        # test independently exercises cancellation inside SQLite's progress
        # callback; this subprocess test covers real-signal transport/cleanup.
        with closing(sqlite3.connect(self.db[True])) as db:
            db.execute(
                "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<99999) INSERT INTO consumer_events(id,profile,event,severity,subject) SELECT 'slow-'||x,'default','mapping_publish','info',? FROM n",
                (
                    json.dumps(
                        {
                            "owner": "fixture",
                            "key": "k",
                            "state": "verified",
                            "plan_id": "p",
                            "version": 1,
                        }
                    ),
                ),
            )
            db.commit()
        shutil.copyfile(self.db[True], self.db[False])
        before = {flag: self.snapshot(flag) for flag in (True, False)}
        for mode in ([], ["--machine"]):
            outputs = []
            for flag in (True, False):
                process = subprocess.Popen(
                    self.command(
                        flag,
                        mode + ["projection", "mapping-recover", "missing", "--apply"],
                    ),
                    env=self.env,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                self.addCleanup(
                    lambda p=process: p.kill() if p.poll() is None else None
                )
                path = Path(str(self.db[flag]) + ".plex-metadata.lock")
                deadline = time.monotonic() + 10
                held = False
                while time.monotonic() < deadline and process.poll() is None:
                    if path.exists():
                        with path.open("r+") as lock:
                            try:
                                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                                fcntl.flock(lock, fcntl.LOCK_UN)
                            except BlockingIOError:
                                held = True
                                break
                    time.sleep(0.01)
                self.assertTrue(
                    held,
                    process.communicate(timeout=5)
                    if process.poll() is not None
                    else "metadata lock not held",
                )
                process.send_signal(signal.SIGINT)
                out, err = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 130, (out, err))
                outputs.append((out, err))
                with path.open("r+") as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.assertEqual(outputs[1], outputs[0])
        self.assert_unchanged(before)


if __name__ == "__main__":
    unittest.main()
