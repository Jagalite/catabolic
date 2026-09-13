# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import os
import sqlite3
import time
import unittest
from unittest.mock import patch

from catabolic import observations
from catabolic.domain import CatabolicError
from catabolic.scan_execution import Publisher
from catabolic.scan_policy import configure
from catabolic.scan_traversal import ScanResourceStop, walk
from catabolic.sql_query import execute_sql
from catabolic.store import Store
from tests import test_application as fixtures


class GuardedScanTest(unittest.TestCase):
    setUp = fixtures.CatalogTest.setUp

    def files(self):
        return self.store.rows(
            "SELECT f.path,o.status,o.scan_id FROM files f JOIN observations o ON o.file_id=f.id WHERE f.location='media' ORDER BY path"
        )

    def populate(self, folder, count):
        directory = self.source / folder
        directory.mkdir(exist_ok=True)
        for n in range(count):
            (directory / f"{n}.txt").write_text(str(n))
        return directory

    def test_continuation_rejects_new_dirty_generation(self):
        self.populate("costly", 8)
        self.populate("healthy", 1)
        first = self.app.scan("media", budgets={"directory_entries": 3})
        (self.source / "healthy" / "new.txt").write_text("new")
        observations.dirty(self.app, "media")
        with self.assertRaisesRegex(CatabolicError, "source changed"):
            self.app.scan(
                continue_id=first["observations"][0]["request_id"], extended=True
            )
        result = self.app.scan("media", extended=True)
        self.assertTrue(result["complete"])
        self.assertIn("healthy/new.txt", {row["path"] for row in self.files()})

    def test_recovery_rejects_replaced_root_with_path_trust(self):
        from catabolic.source_trust import configure as trust

        trust(self.app, "media", "path", apply=True)
        self.populate("healthy", 1)
        self.populate("later", 2)
        request = observations.request(self.app, "media", after=time.time())
        original = Publisher._flush

        def crash(publisher):
            done = publisher.scopes.get("healthy", (None,))[0] == "complete"
            original(publisher)
            if done:
                raise KeyboardInterrupt()

        with patch.object(Publisher, "_flush", crash):
            with self.assertRaises(KeyboardInterrupt):
                observations.execute(self.app, request)
        before = self.store.rows(
            "SELECT count(*) AS n FROM observation_batches WHERE job_id=?",
            (request["id"],),
        )[0]["n"]
        self.source.rename(self.root / "oldsource")
        self.source.mkdir()
        (self.root / "oldsource" / "later").rename(self.source / "later")
        (self.source / "new.txt").write_text("new")
        result = observations.observe(
            self.app, "media", request_id=request["request_id"]
        )
        self.assertNotEqual(result["state"], "complete")
        self.assertEqual(result["report"]["blocker"], "source_root_replaced")
        self.assertEqual(
            before,
            self.store.rows(
                "SELECT count(*) AS n FROM observation_batches WHERE job_id=?",
                (request["id"],),
            )[0]["n"],
        )

    def test_sqlite_full_stops_other_sources(self):
        other = self.root / "other"
        other.mkdir()
        self.app.bind("source", "other", str(other))
        connect = sqlite3.connect

        class Queue:
            def __init__(self, connection):
                self.connection = connection

            def __getattr__(self, name):
                return getattr(self.connection, name)

            def execute(self, sql, *args, **kwargs):
                if sql.startswith("PRAGMA page_count"):
                    raise sqlite3.OperationalError("database or disk is full")
                return self.connection.execute(sql, *args, **kwargs)

        def injected(path, *args, **kwargs):
            connection = connect(path, *args, **kwargs)
            return (
                Queue(connection)
                if str(path).endswith("directories.sqlite3")
                else connection
            )

        with patch("catabolic.scan_traversal.sqlite3.connect", injected):
            result = self.app.scan()
        self.assertEqual(result["unstarted_sources"], ["other"])
        self.assertEqual(result["scans"][0]["blocker"], "temporary_storage_low")
        self.assertTrue(result["scans"][0]["deferred"])
        self.assertEqual(self.files()[0]["status"], "present")

    def test_readers_see_batches_before_completion(self):
        self.populate("new", 8)
        seen = []
        original = Publisher._flush

        def flushed(publisher):
            original(publisher)
            with Store(self.database) as store:
                n = store.rows(
                    "SELECT count(*) AS n FROM observations WHERE scan_id=?",
                    (publisher.job["scan_id"],),
                )[0]["n"]
                complete = store.rows(
                    "SELECT complete FROM scans WHERE id=?", (publisher.job["scan_id"],)
                )[0]["complete"]
                if n:
                    seen.append((n, complete))
                    result = execute_sql(
                        self.database,
                        "SELECT count(*) FROM catalog_work_inbox WHERE category='identification'",
                        _store=store,
                    )
                    self.assertTrue(result["complete"])

        with patch.object(Publisher, "_flush", flushed):
            result = self.app.scan("media", budgets={"batch_records": 2})
        self.assertTrue(result["complete"])
        self.assertTrue(any(n > 1 and not complete for n, complete in seen))

    def test_unvisited_inventory_retained_and_independent_scope_progresses(self):
        self.populate("costly", 8)
        self.populate("healthy", 1)
        self.app.scan("media")
        (self.source / "costly" / "7.txt").unlink()
        (self.source / "healthy" / "new.txt").write_text("new")
        result = self.app.scan(
            "media", budgets={"directory_entries": 3, "batch_records": 2}
        )
        self.assertFalse(result["complete"])
        self.assertTrue(result["scans"][0]["deferred"])
        rows = {r["path"]: r["status"] for r in self.files()}
        self.assertEqual(rows["costly/7.txt"], "present")
        self.assertEqual(rows["healthy/new.txt"], "present")

    def test_partial_scope_absence_does_not_escape_directory(self):
        self.populate("costly", 8)
        self.populate("healthy", 1)
        self.app.scan("media")
        (self.source / "healthy" / "0.txt").unlink()
        (self.source / "costly" / "7.txt").unlink()
        self.app.scan("media", budgets={"directory_entries": 3})
        rows = {r["path"]: r["status"] for r in self.files()}
        self.assertEqual(rows["healthy/0.txt"], "missing")
        self.assertEqual(rows["costly/7.txt"], "present")

    def test_continue_retains_completed_scopes_and_barrier(self):
        self.populate("costly", 8)
        self.populate("healthy", 1)
        first = self.app.scan("media", budgets={"directory_entries": 3})
        request = first["observations"][0]
        repeats = self.app.scan("media", budgets={"directory_entries": 3})
        self.assertEqual(repeats["observations"][0]["id"], request["id"])
        visited = []
        original = Publisher.directory

        def directory(publisher, path, state, detail):
            if state == "running":
                visited.append(path)
            return original(publisher, path, state, detail)

        with patch.object(Publisher, "directory", directory):
            result = self.app.scan(continue_id=request["request_id"], extended=True)
        self.assertTrue(result["complete"])
        self.assertEqual(visited, ["costly"])
        self.assertEqual(result["observations"][0]["parent_job"], request["id"])
        self.assertEqual(
            result["observations"][0]["guarantees"]["minimum_start"],
            request["guarantees"]["minimum_start"],
        )

    def test_successful_continuation_does_not_resurrect_old_blocker(self):
        self.populate("costly", 8)
        first = self.app.scan("media", budgets={"directory_entries": 3})
        second = self.app.scan(
            continue_id=first["observations"][0]["request_id"],
            budgets={"directory_entries": 5},
        )
        third = self.app.scan(
            continue_id=second["observations"][0]["request_id"], extended=True
        )
        self.assertTrue(third["complete"])
        request = observations.request(
            self.app,
            "media",
            after=time.time(),
            reuse_completed=False,
            budgets={"directory_entries": 3},
        )
        self.assertEqual(request["state"], "queued")
        self.assertNotEqual(request["id"], first["observations"][0]["id"])

    def test_selected_continuation_keeps_other_scopes_deferred(self):
        self.populate("one", 6)
        self.populate("two", 6)
        first = self.app.scan("media", budgets={"directory_entries": 3})
        result = self.app.scan(
            continue_id=first["observations"][0]["request_id"],
            extended=True,
            scopes=["one"],
        )
        self.assertFalse(result["complete"])
        self.assertEqual(result["scans"][0]["coverage"]["deferred"], 1)

    def test_policy_is_shared_and_change_requires_new_request(self):
        self.populate("excluded", 2)
        configure(self.app, "media", {"exclusions": ["excluded"]}, apply=True)
        request = observations.request(self.app, "media", exclusions=[])
        self.assertEqual(request["guarantees"]["exclusions"], ["excluded"])
        result = observations.execute(self.app, request)
        self.assertEqual(result["state"], "complete")
        self.assertFalse(any(r["path"].startswith("excluded/") for r in self.files()))
        configure(self.app, "media", {"exclusions": []}, apply=True)
        with self.assertRaisesRegex(CatabolicError, "policy changed"):
            observations.resume(self.app, request["request_id"])

    def test_staging_resource_error_is_not_a_per_file_error(self):
        self.populate("files", 20)
        visits = []

        def sink(entry):
            visits.append(entry)
            raise ScanResourceStop("staging full")

        fd = os.open(self.source, os.O_RDONLY | os.O_DIRECTORY)
        try:
            _, errors = walk(fd, sink=sink)
        finally:
            os.close(fd)
        self.assertEqual(len(visits), 1)
        self.assertEqual(errors, ["staging full"])

    def test_temporary_space_stops_before_metadata_enumeration(self):
        from collections import namedtuple

        usage = namedtuple("usage", "total used free")(100, 100, 0)
        with patch("catabolic.scan_traversal.shutil.disk_usage", return_value=usage):
            result = self.app.scan("media")
        self.assertFalse(result["complete"])
        self.assertEqual(result["scans"][0]["blocker"], "temporary_storage_low")
        self.assertEqual(self.files()[0]["status"], "present")

    def test_slow_metadata_is_measured_and_deferred(self):
        self.populate("slow", 3)
        original = os.stat

        def slow(path, *args, **kwargs):
            if kwargs.get("dir_fd") is not None and path == "0.txt":
                time.sleep(0.01)
            return original(path, *args, **kwargs)

        with patch("catabolic.scan_traversal.os.stat", side_effect=slow):
            result = self.app.scan("media", budgets={"metadata_seconds": 0.005})
        self.assertFalse(result["complete"])
        self.assertTrue(
            any(
                r["detail"].get("reason") == "metadata_latency_budget"
                for r in result["scans"][0]["outstanding"]
            )
        )

    def test_deep_tree_uses_no_python_recursion(self):
        fd = os.open(self.source, os.O_RDONLY | os.O_DIRECTORY)
        for _ in range(90):
            os.mkdir("d", dir_fd=fd)
            child = os.open("d", os.O_RDONLY | os.O_DIRECTORY, dir_fd=fd)
            os.close(fd)
            fd = child
        os.close(fd)
        first = self.app.scan("media", budgets={"depth": 10})
        self.assertFalse(first["complete"])
        result = self.app.scan(
            continue_id=first["observations"][0]["request_id"], extended=True
        )
        self.assertTrue(result["complete"])
        # Run the same physical walker below the tree's depth in Python frames.
        import sys

        from catabolic.scan_policy import DEFAULTS

        class Sink:
            budgets = {**DEFAULTS, "depth": 200}

            def __call__(self, entry):
                pass

        fd = os.open(self.source, os.O_RDONLY | os.O_DIRECTORY)
        previous = sys.getrecursionlimit()
        try:
            sys.setrecursionlimit(70)
            _, errors = walk(fd, sink=Sink())
        finally:
            sys.setrecursionlimit(previous)
            os.close(fd)
        self.assertEqual(errors, [])

    def test_lost_ownership_rolls_back_next_batch(self):
        self.populate("new", 10)
        original = Publisher._flush
        changed = []

        def stolen(publisher):
            original(publisher)
            if publisher.entries:  # original clears entries
                return
            if not changed:
                with Store(self.database, writable=True) as store:
                    with store.transaction() as db:
                        db.execute(
                            "UPDATE observation_jobs SET claim_token='foreign' WHERE id=?",
                            (publisher.job["id"],),
                        )
                changed.append(True)

        with patch.object(Publisher, "_flush", stolen):
            with self.assertRaisesRegex(CatabolicError, "stale_observation_claim"):
                self.app.scan("media", budgets={"batch_records": 2})
        self.assertEqual(len(self.files()), 1)

    def test_root_replacement_stops_publication_even_with_path_policy(self):
        from catabolic.source_trust import configure as trust

        trust(self.app, "media", "path", apply=True)
        self.populate("new", 5)
        original = Publisher._flush
        replaced = []

        def swap(publisher):
            original(publisher)
            if not replaced:
                self.source.rename(self.root / "old_source")
                self.source.mkdir()
                replaced.append(True)

        with patch.object(Publisher, "_flush", swap):
            result = self.app.scan("media", budgets={"batch_records": 2})
            self.assertFalse(result["complete"])
            self.assertEqual(result["scans"][0]["blocker"], "source_root_replaced")
        self.assertEqual(len(self.files()), 1)

    def test_failed_batch_is_not_visible(self):
        self.populate("new", 5)
        original = Publisher._flush
        from catabolic import catalog_refresh

        with patch.object(
            catalog_refresh, "enqueue", side_effect=RuntimeError("rollback")
        ):
            with self.assertRaisesRegex(RuntimeError, "rollback"):
                self.app.scan("media", budgets={"batch_records": 1})
        self.assertEqual(len(self.files()), 1)
        self.assertIsNotNone(original)

    def test_crash_preserves_committed_discoveries_and_resumes_job(self):
        self.populate("new", 5)
        original = Publisher._flush
        request = observations.request(
            self.app, "media", after=time.time(), budgets={"batch_records": 1}
        )

        def crash(publisher):
            had_entry = bool(publisher.entries)
            original(publisher)
            if had_entry:
                raise KeyboardInterrupt()

        with patch.object(Publisher, "_flush", crash):
            with self.assertRaises(KeyboardInterrupt):
                observations.execute(self.app, request)
        before = self.store.rows(
            "SELECT count(*) AS n FROM observation_batches WHERE job_id=?",
            (request["id"],),
        )[0]["n"]
        result = observations.observe(
            self.app, "media", request_id=request["request_id"]
        )
        self.assertEqual(result["id"], request["id"])
        self.assertEqual(result["state"], "complete")
        self.assertGreater(result["report"]["batches"], before)

    def test_inbox_blocker_is_one_stable_source_entry(self):
        self.populate("one", 8)
        self.populate("two", 8)
        self.app.scan("media", budgets={"directory_entries": 3})

        def query():
            return execute_sql(
                self.database,
                "SELECT work_key FROM catalog_work_inbox WHERE category='observation'",
                _store=self.store,
            )["rows"]

        before = query()
        self.assertEqual(len(before), 1)
        self.app.scan("media", budgets={"directory_entries": 3})
        self.assertEqual(query(), before)

    def test_renewal_checks_exact_fence(self):
        request = observations.request(self.app, "media", after=time.time())
        job = observations.claim(self.app, request)
        with self.store.transaction() as db:
            db.execute(
                "UPDATE observation_jobs SET lease_until=? WHERE id=?",
                (time.time() + 1, job["id"]),
            )
        observations.renew(self.app, job)
        self.assertGreater(
            self.store.rows(
                "SELECT lease_until FROM observation_jobs WHERE id=?", (job["id"],)
            )[0]["lease_until"],
            time.time() + 3500,
        )
        with self.store.transaction() as db:
            db.execute(
                "UPDATE observation_jobs SET claim_token='foreign' WHERE id=?",
                (job["id"],),
            )
        with self.assertRaisesRegex(CatabolicError, "stale_observation_claim"):
            observations.renew(self.app, job)

    def test_noop_batches_do_not_dirty_watchers_or_sources(self):
        before = self.store.rows("SELECT * FROM observation_sources")
        with patch("catabolic.catalog_refresh.enqueue") as enqueue:
            self.app.scan("media", budgets={"batch_records": 1})
            enqueue.assert_not_called()
        self.assertEqual(self.store.rows("SELECT * FROM observation_sources"), before)

    def test_confirmed_absent_child_finalizes_only_its_subtree(self):
        self.populate("gone", 3)
        self.populate("unfinished", 8)
        self.app.scan("media")
        import shutil

        shutil.rmtree(self.source / "gone")
        (self.source / "unfinished" / "7.txt").unlink()
        report = self.app.scan("media", budgets={"directory_entries": 3})
        self.assertFalse(report["complete"])
        rows = {r["path"]: r["status"] for r in self.files()}
        self.assertEqual(rows["gone/0.txt"], "missing")
        self.assertEqual(rows["unfinished/7.txt"], "present")

    def test_entry_limit_stops_before_visiting_more_files(self):
        self.populate("many", 1000)
        original = os.stat
        calls = []

        def counted(path, *args, **kwargs):
            if kwargs.get("dir_fd") is not None:
                calls.append(path)
            return original(path, *args, **kwargs)

        with patch("catabolic.scan_traversal.os.stat", new=counted):
            report = self.app.scan(
                "media", budgets={"total_entries": 20, "batch_records": 2}
            )
        self.assertFalse(report["complete"])
        self.assertEqual(report["scans"][0]["blocker"], "total_entry_budget")
        self.assertLessEqual(len(calls), 20)
        self.assertGreater(report["scans"][0]["published"], 0)

    def test_query_watchers_honor_policy_and_own_batches_do_not_rescan(self):
        from catabolic.saved_queries import Queries
        from catabolic.watchers import Watchers

        self.populate("excluded", 2)
        self.populate("visible", 3)
        configure(self.app, "media", {"exclusions": ["excluded"]}, apply=True)
        query = Queries(self.store).put(
            "files",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS file_id FROM files",
                }
            },
        )["id"]
        watchers = Watchers(self.app)
        watchers.put(
            "audit",
            {
                "plan": {"kind": "query", "query_id": query},
                "events": True,
                "observe_after_request": True,
            },
        )
        watchers.enable("audit")
        report = watchers.run("audit")
        self.assertTrue(report["complete"], report)
        self.assertFalse(any(r["path"].startswith("excluded/") for r in self.files()))
        row = watchers.get("audit")
        self.assertEqual(row["pending_generation"], row["completed_generation"])

    def test_global_storage_pause_leaves_other_sources_unstarted(self):
        from collections import namedtuple

        other = self.root / "other"
        other.mkdir()
        (other / "file").write_text("x")
        self.app.bind("source", "second", str(other))
        usage = namedtuple("usage", "total used free")(100, 100, 0)
        with patch("catabolic.scan_traversal.shutil.disk_usage", return_value=usage):
            result = self.app.scan()
        self.assertEqual(result["unstarted_sources"], ["second"])
        self.assertEqual(len(result["observations"]), 1)

    def test_continuation_admission_distinguishes_selected_scopes_and_joins_duplicates(
        self,
    ):
        self.populate("one", 6)
        self.populate("two", 6)
        first = self.app.scan("media", budgets={"directory_entries": 3})
        request_id = first["observations"][0]["request_id"]
        one = observations.continue_request(
            self.app, request_id, scopes=["one"], extended=True
        )
        two = observations.continue_request(
            self.app, request_id, scopes=["two"], extended=True
        )
        duplicate = observations.continue_request(
            self.app, request_id, scopes=["one"], extended=True
        )
        self.assertNotEqual(one["id"], two["id"])
        self.assertEqual(one["id"], duplicate["id"])
        self.assertEqual(
            self.store.rows(
                "SELECT state FROM observation_scopes WHERE job_id=? AND path='two'",
                (one["id"],),
            )[0]["state"],
            "deferred",
        )

    def test_error_budget_waits_for_explicit_repair_continuation(self):
        self.populate("bad", 12)
        original = os.stat

        def denied(path, *args, **kwargs):
            if kwargs.get("dir_fd") is not None and str(path).endswith(".txt"):
                raise PermissionError("repair access")
            return original(path, *args, **kwargs)

        with patch("catabolic.scan_traversal.os.stat", new=denied):
            first = self.app.scan("media", budgets={"errors": 3})
            again = self.app.scan("media", budgets={"errors": 3})
        self.assertTrue(first["scans"][0]["deferred"])
        self.assertEqual(first["observations"][0]["id"], again["observations"][0]["id"])
        self.assertEqual(
            first["scans"][0]["outstanding"][0]["detail"]["reason"],
            "directory_error_budget",
        )
        self.assertTrue(
            self.app.scan(
                continue_id=first["observations"][0]["request_id"],
                budgets={"errors": 3},
            )["complete"]
        )

    def test_publication_delay_is_not_reported_as_metadata_latency(self):
        self.populate("directory", 2)
        original = Publisher._flush

        def delayed(publisher):
            time.sleep(0.05)
            return original(publisher)

        with patch.object(Publisher, "_flush", delayed):
            result = self.app.scan(
                "media", budgets={"batch_records": 1, "metadata_seconds": 0.02}
            )
        self.assertTrue(result["complete"], result)
