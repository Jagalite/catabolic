# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Detached native scans, shared callers, registry evidence and regex parity."""

import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from uuid import UUID, uuid5

from catabolic import scan_policy
from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.store import Store, encode
from tests.test_rust_migration import BINARY


class NativeObservationSchedulerTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        self.database = self.root / "catalog.db"
        Store.initialize(self.database)
        with Store(self.database, writable=True) as store:
            Application(store).bind("source", "media", str(self.source))

    def command(self, surface, action, **request):
        return [
            str(BINARY),
            "--db",
            str(self.database),
            "operation",
            surface,
            action,
            "--options",
            json.dumps(request),
        ]

    def call(self, surface, action, **request):
        deadline = time.monotonic() + 5
        while True:
            result = subprocess.run(
                self.command(surface, action, **request),
                capture_output=True,
                text=True,
                timeout=30,
            )
            if (
                result.returncode == 2
                and "another Catabolic writer" in result.stderr
                and time.monotonic() < deadline
            ):
                time.sleep(0.01)
                continue
            if result.returncode:
                raise CatabolicError(result.stderr)
            return json.loads(result.stdout)

    def slow_scan(self, **request):
        for index in range(100):
            (self.source / f"file{index:03}.txt").write_text("native evidence")
        args = (
            dict(
                source="media",
                requester="first",
                max_age=60,
                budgets={"batch_records": 1},
            )
            | request
        )
        child = subprocess.Popen(
            self.command("observe", "scan", **args),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=os.environ | {"CATABOLIC_TEST_SCAN_ENTRY_DELAY_MS": "20"},
        )
        self.addCleanup(lambda: child.poll() is None and child.kill())
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            with sqlite3.connect(self.database) as db:
                observed = db.execute(
                    "SELECT count(*) FROM observation_batches"
                ).fetchone()[0]
                running = db.execute(
                    "SELECT count(*) FROM observation_scopes WHERE state='running'"
                ).fetchone()[0]
            if observed >= 3 and running:
                return child, args
            self.assertIsNone(
                child.poll(), child.communicate() if child.poll() is not None else None
            )
            time.sleep(0.01)
        self.fail("native traversal did not publish bounded progress")

    def test_actual_traversal_releases_writer_and_coalesces_cancelled_caller(self):
        child, args = self.slow_scan()
        # A distinct native writer attaches while the original process is
        # visiting entries, rather than merely between completed scans.
        shared = self.call("observe", "scan", **(args | {"requester": "second"}))
        row = shared["observations"][0]
        self.assertEqual(row["state"], "running")
        self.assertIsNone(child.poll())
        self.call("observe", "cancel", request_id=row["request_id"])
        deadline = time.monotonic() + 5
        while True:
            try:
                with (
                    Store(self.database, writable=True) as store,
                    store.transaction() as db,
                ):
                    db.execute(
                        "INSERT INTO meta VALUES('scheduler-writer-progress','yes')"
                    )
                break
            except CatabolicError as error:
                if (
                    "another Catabolic writer" not in str(error)
                    or time.monotonic() >= deadline
                ):
                    raise
                time.sleep(0.01)
        output, error = child.communicate(timeout=15)
        self.assertEqual(child.returncode, 0, error)
        result = json.loads(output)
        self.assertTrue(result["complete"], result)
        self.assertEqual(result["observations"][0]["id"], row["id"])
        resumed = self.call("observe", "scan", request_id=row["request_id"])
        self.assertEqual(resumed["observations"][0]["request_state"], "cancelled")
        self.assertEqual(resumed["observations"][0]["job_state"], "complete")
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute("SELECT count(*) FROM observation_jobs").fetchone()[0], 1
            )
            self.assertEqual(
                db.execute(
                    "SELECT count(*) FROM observations WHERE status='present'"
                ).fetchone()[0],
                100,
            )

    def test_dirty_generation_during_traversal_fences_late_publication(self):
        child, _ = self.slow_scan()
        self.call("observe", "dirty", source="media")
        with sqlite3.connect(self.database) as db:
            before = db.execute("SELECT count(*) FROM observations").fetchone()[0]
        _, error = child.communicate(timeout=15)
        self.assertEqual(child.returncode, 2, error)
        self.assertIn("stale_observation_claim", error)
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute("SELECT count(*) FROM observations").fetchone()[0], before
            )
            self.assertEqual(
                db.execute("SELECT count(*) FROM scans WHERE complete=1").fetchone()[0],
                0,
            )
        self.call("observe", "recover")
        completed = self.call("observe", "scan", source="media")
        self.assertTrue(completed["complete"], completed)

    def test_catalog_replacement_is_not_acknowledged_after_detached_work(self):
        child, _ = self.slow_scan()
        replacement = self.root / "replacement.db"
        Store.initialize(replacement)
        os.replace(replacement, self.database)
        _, error = child.communicate(timeout=15)
        self.assertEqual(child.returncode, 2, error)
        self.assertIn("catalog replaced", error)
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute("SELECT count(*) FROM observation_jobs").fetchone()[0], 0
            )

    def test_nonempty_regex_and_scope_absence_match_python(self):
        names = [
            "Movie.Movie.mkv",
            "Movie.Other.mkv",
            "private.mkv",
            "café.txt",
            "_foo.txt",
            "E42.txt",
            "episode42.txt",
            "title.MKV",
        ]
        for name in names:
            (self.source / name).write_text("header")
        self.call("observe", "scan", source="media")
        template = self.root / "template.db"
        shutil.copyfile(self.database, template)
        policies = [
            {"include_regex": [r"(?i)\.mkv\Z"], "exclude_regex": [r"^private"]},
            {"include_regex": [r"^(?P<title>\w+)\.(?P=title)\.mkv$"]},
            {"include_regex": [r"(?<=episode)\d+(?=\.txt$)"]},
            {"include_regex": [r"^\w+\.txt$"], "exclude_regex": [r"^_"]},
        ]
        for policy in policies:
            shutil.copyfile(template, self.database)
            reference = self.root / "reference.db"
            shutil.copyfile(template, reference)
            native_policy = self.call(
                "observe", "scan_policy", source="media", policy=policy, apply=True
            )
            with Store(reference, writable=True) as store:
                app = Application(store)
                expected_policy = scan_policy.configure(
                    app, "media", policy, apply=True
                )
                expected = app.scan("media")
            self.assertEqual(native_policy, expected_policy)
            actual = self.call("observe", "scan", source="media")
            self.assertEqual(
                actual["scans"][0]["observed"], expected["scans"][0]["observed"]
            )
            for key in ("include_regex", "exclude_regex", "coverage_kind"):
                self.assertEqual(actual["scans"][0][key], expected["scans"][0][key])

            def observations(path):
                with sqlite3.connect(path) as db:
                    return db.execute(
                        "SELECT f.path,o.status FROM files f JOIN observations o ON f.id=o.file_id ORDER BY f.path"
                    ).fetchall()

            self.assertEqual(observations(self.database), observations(reference))

    def test_immutable_request_readmission_preserves_guarantees_and_policy_fence(self):
        (self.source / "one.txt").write_text("evidence")
        first = self.call(
            "observe", "scan", source="media", requester="stable", max_age=60
        )
        original = first["observations"][0]
        self.call("observe", "dirty", source="media")
        second = self.call("observe", "scan", request_id=original["request_id"])
        current = second["observations"][0]
        self.assertTrue(second["complete"], second)
        self.assertEqual(current["request_id"], original["request_id"])
        self.assertEqual(current["guarantees"], original["guarantees"])
        self.assertNotEqual(current["id"], original["id"])
        self.assertGreater(current["generation"], original["generation"])
        self.call(
            "observe",
            "scan_policy",
            source="media",
            policy={"exclude_regex": ["private"]},
            apply=True,
        )
        with self.assertRaisesRegex(CatabolicError, "policy changed"):
            self.call("observe", "scan", request_id=original["request_id"])
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute("SELECT count(*) FROM observation_requests").fetchone()[0], 1
            )

    def test_regex_exhaustion_preserves_prior_inventory(self):
        name = "a" * 120 + "!" + ".txt"
        (self.source / name).write_text("evidence")
        self.call("observe", "scan", source="media")
        self.call(
            "observe",
            "scan_policy",
            source="media",
            policy={"include_regex": [r"^(a+)\1+$"]},
            apply=True,
        )
        result = self.call("observe", "scan", source="media")
        # This qualified backtracking shape can either reject quickly or hit its
        # bounded allowance; neither result authorizes absence outside the filter.
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute("SELECT status FROM observations").fetchone()[0], "present"
            )
        if not result["complete"]:
            self.assertIn(
                result["scans"][0]["blocker"],
                ("regex_filter_timeout", "regex_finalization_budget"),
            )

    def test_generated_registry_only_and_explicit_missing(self):
        (self.source / "input.txt").write_text("input")
        self.call("observe", "scan", source="media")
        generated = self.root / "generated"
        generated.mkdir()
        binding = self.call(
            "artifact", "bind", location="generated", path=str(generated)
        )
        with Store(self.database, writable=True) as store, store.transaction() as db:
            source = db.execute(
                "SELECT id FROM files WHERE location='media'"
            ).fetchone()[0]
            db.execute(
                "INSERT INTO items(id,kind,metadata) VALUES('item','movie','{}')"
            )
            for index, path in enumerate(("ready.txt", "missing.txt")):
                identifier = str(
                    uuid5(UUID(store.database_id), encode(["generated", path]))
                )
                db.execute(
                    "INSERT INTO files VALUES(?,?,?)", (identifier, "generated", path)
                )
                db.execute(
                    "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state) VALUES(?,'default',?,'render','media','{}','{}',?,'complete')",
                    (f"job{index}", source, f"key{index}"),
                )
                db.execute(
                    "INSERT INTO processing_attempts(job_id,attempt,state,retryable,retry_after) VALUES(?,1,'complete',0,0)",
                    (f"job{index}",),
                )
                db.execute(
                    "INSERT INTO processing_artifacts(id,job_id,attempt,profile,location,path,temporary_path,binding,state,file_id,item_id,role,size,sha256) VALUES(?,?,1,'default','generated',?,?,?,'ready',?,'item','primary',6,?)",
                    (
                        f"artifact{index}",
                        f"job{index}",
                        path,
                        f"temp{index}",
                        encode(binding),
                        identifier,
                        "a" * 64,
                    ),
                )
        (generated / "ready.txt").write_text("output")
        (generated / "unregistered.txt").write_text("never invent inventory")
        reference = self.root / "reference.db"
        shutil.copyfile(self.database, reference)
        with Store(reference, writable=True) as store:
            expected = Application(store).scan("generated")
        actual = self.call("observe", "scan", source="generated")
        self.assertTrue(actual["complete"], actual)
        self.assertEqual(
            actual["scans"][0]["observed"], expected["scans"][0]["observed"]
        )
        self.assertEqual(actual["scans"][0]["coverage"], {"registry_complete": 1})
        with sqlite3.connect(self.database) as db:
            self.assertEqual(
                db.execute(
                    "SELECT f.path,o.status FROM files f JOIN observations o ON o.file_id=f.id WHERE f.location='generated' ORDER BY f.path"
                ).fetchall(),
                [("missing.txt", "missing"), ("ready.txt", "present")],
            )
            self.assertEqual(
                db.execute(
                    "SELECT count(*) FROM files WHERE path='unregistered.txt'"
                ).fetchone()[0],
                0,
            )


if __name__ == "__main__":
    unittest.main()
