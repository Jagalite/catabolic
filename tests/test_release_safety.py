# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Policy boundaries and live filesystem changes against disposable catalogs."""

import errno
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.domain import CatabolicError
from catabolic.processing import Processing, process
from catabolic.reconcile import Reconciler
from catabolic.store import Store
from tests import test_application, test_enrichment


class RemovalLimitsTest(unittest.TestCase):
    setUp = test_application.CatalogTest.setUp

    def test_limit_blocks_entire_plan_before_creating_any_output(self):
        self.reconciler = Reconciler(self.app)
        self.reconciler.apply()
        self.app.disable_mapping(self.mapping["id"])
        self.app.put_mapping("global", self.file_id, self.item_id, "New/Example.mkv")
        before = self.database.read_bytes()
        result = self.reconciler.apply(max_removals=0)
        self.assertFalse(result["safe"])
        self.assertEqual(result["applied"], [])
        self.assertTrue(self.link.exists())
        self.assertFalse((self.output / "New").exists())
        self.assertEqual(self.database.read_bytes(), before)
        self.assertEqual(result["removal_budget"]["percent"], 100)
        self.assertFalse(self.reconciler.preview(max_removal_percent=99)["safe"])
        self.assertTrue(
            self.reconciler.apply(max_removals=1, max_removal_percent=100)["healthy"]
        )

    def test_limits_validate_nan_negative_bool_and_allow_exact_boundary(self):
        r = Reconciler(self.app)
        for kwargs in (
            {"max_removals": -1},
            {"max_removals": True},
            {"max_removal_percent": float("nan")},
            {"max_removal_percent": 101},
        ):
            with self.assertRaises(CatabolicError):
                r.preview(**kwargs)
        self.assertTrue(r.apply(max_removals=0, max_removal_percent=0)["healthy"])

    def test_hardlink_retirement_is_counted_and_final_reference_stays_protected(self):
        self.app.bind("output", "global", link_mode="hardlink")
        r = Reconciler(self.app)
        r.apply()
        self.app.disable_mapping(self.mapping["id"])
        self.assertFalse(r.apply(max_removals=0)["safe"])
        self.assertTrue(os.path.samefile(self.link, self.media))
        self.media.unlink()
        self.app.scan()
        self.assertFalse(r.apply(max_removals=100)["safe"])
        self.assertEqual(self.link.stat().st_nlink, 1)

    def test_all_catalogs_share_one_budget(self):
        other = self.root / "other"
        other.mkdir()
        self.app.bind("output", "other", str(other))
        second = self.app.put_mapping(
            "other", self.file_id, self.item_id, "Example.mkv"
        )
        r = Reconciler(self.app)
        r.apply(None)
        self.app.disable_mapping(self.mapping["id"])
        self.app.disable_mapping(second["id"])
        result = r.apply(None, max_removals=1)
        self.assertFalse(result["safe"])
        self.assertEqual(result["removal_budget"]["owned_entries"], 2)
        self.assertTrue(self.link.exists())
        self.assertTrue((other / "Example.mkv").exists())


class RetryAndArrivalTest(unittest.TestCase):
    setUp = test_enrichment.EnrichmentTest.setUp

    def test_transient_retries_are_delayed_bounded_durable_and_opt_in(self):
        job = self.p.enqueue("hash", file_ids=[self.file_id])["queued"][0]
        failure = {
            "state": "failed",
            "data": {},
            "error": "temporary I/O",
            "retryable": True,
        }
        with (
            patch("catabolic.processing.time.time", return_value=1000),
            patch("catabolic.processing.process", return_value=failure),
        ):
            self.assertEqual(self.p.run(retry_transient=2)["counts"], {"failed": 1})
        self.store.close()
        self.store = Store(self.database, writable=True)
        self.addCleanup(self.store.close)
        from catabolic.app import Application

        self.p = Processing(Application(self.store))
        with patch("catabolic.processing.time.time", return_value=1029):
            self.assertEqual(self.p.run(retry_transient=2)["processed"], 0)
        with (
            patch("catabolic.processing.time.time", return_value=1030),
            patch("catabolic.processing.process", return_value=failure),
        ):
            self.assertEqual(self.p.run()["processed"], 0)
            self.assertEqual(self.p.run(retry_transient=2)["retried"], 1)
        with patch("catabolic.processing.time.time", return_value=1089):
            self.assertEqual(self.p.run(retry_transient=2)["processed"], 0)
        with (
            patch("catabolic.processing.time.time", return_value=1090),
            patch("catabolic.processing.process", return_value=failure),
        ):
            self.assertEqual(self.p.run(retry_transient=2)["retried"], 1)
        with patch("catabolic.processing.time.time", return_value=9999):
            self.assertEqual(self.p.run(retry_transient=2)["processed"], 0)
        rows = self.p.attempts(job)["attempts"]
        self.assertEqual([r["attempt"] for r in rows], [1, 2, 3])
        self.assertEqual([r["retry_after"] for r in rows], [1030, 1090, 1210])
        page = self.p.attempts(job, limit=1)
        self.assertEqual(page["next_after"], 1)
        self.assertEqual(len(self.p.attempts(job, after=1)["attempts"]), 2)

    def test_successful_retry_preserves_facts_and_stale_retry_is_rejected(self):
        job = self.p.enqueue("hash", file_ids=[self.file_id])["queued"][0]
        with (
            patch("catabolic.processing.time.time", return_value=1000),
            patch(
                "catabolic.processing.process",
                return_value={
                    "state": "timeout",
                    "data": {},
                    "error": "timeout",
                    "retryable": True,
                },
            ),
        ):
            self.p.run()
        with patch("catabolic.processing.time.time", return_value=1030):
            self.assertEqual(self.p.run(retry_transient=1)["counts"], {"complete": 1})
        self.assertEqual(
            [r["state"] for r in self.p.attempts(job)["attempts"]],
            ["timeout", "complete"],
        )
        self.assertEqual(len(self.p.facts(self.file_id)["facts"]), 1)
        self.p.enqueue("hash", file_ids=[self.file_id], refresh=True)
        with (
            patch("catabolic.processing.time.time", return_value=2000),
            patch(
                "catabolic.processing.process",
                return_value={
                    "state": "failed",
                    "data": {},
                    "error": "io",
                    "retryable": True,
                },
            ),
        ):
            self.p.run()
        self.file.write_bytes(b"new content")
        with patch("catabolic.processing.time.time", return_value=2030):
            self.assertEqual(self.p.run(retry_transient=1)["counts"], {"changed": 1})
        self.assertEqual(len(self.p.facts(self.file_id)["facts"]), 1)

    def test_error_classification_does_not_retry_permissions_or_invalid_media(self):
        self.p.enqueue("hash", file_ids=[self.file_id])
        job = self.p.list()["jobs"][0]
        import threading

        for code, retryable in (
            (errno.EIO, True),
            (errno.EACCES, False),
            (errno.ENOENT, False),
        ):
            with patch(
                "catabolic.processing.validated_source",
                side_effect=OSError(code, "fixture"),
            ):
                self.assertEqual(
                    process(job, threading.Event())["retryable"], retryable
                )
        self.p.enqueue("text", file_ids=[self.file_id])
        self.p.run()
        with patch("catabolic.processing.time.time", return_value=99999999999):
            self.assertEqual(self.p.run(retry_transient=10)["processed"], 0)

    def test_preallocated_file_changed_during_read_never_publishes_hash(self):
        self.p.enqueue("hash", file_ids=[self.file_id])
        original = os.read
        changed = False

        def read(fd, count):
            nonlocal changed
            data = original(fd, count)
            if data and not changed:
                changed = True
                stamp = self.file.stat()
                with self.file.open("r+b") as writer:
                    writer.seek(44)
                    writer.write(b"XX")
                os.utime(self.file, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
            return data

        with patch("catabolic.processing.os.read", side_effect=read):
            self.assertEqual(self.p.run()["counts"], {"changed": 1})
        self.assertEqual(self.store.rows("SELECT * FROM content_baselines"), [])

    def test_atomic_download_rename_and_pause_require_fresh_stability_window(self):
        from catabolic.watching import cycle

        partial = self.source / "download.wav.part"
        partial.write_bytes(self.file.read_bytes())
        with patch("catabolic.watching.time.time", return_value=1000):
            cycle(self.app, operation="sniff", settle=30)
        final = self.source / "download.wav"
        partial.rename(final)
        with patch("catabolic.watching.time.time", return_value=1031):
            result = cycle(self.app, operation="sniff", settle=30)
        fid = next(
            r["id"] for r in self.app.files()["files"] if r["path"] == "download.wav"
        )
        self.assertEqual(self.p.list(file_id=fid)["jobs"], [])
        with final.open("ab") as writer:
            writer.write(b"continued after a pause")
        with patch("catabolic.watching.time.time", return_value=1062):
            cycle(self.app, operation="sniff", settle=30)
        self.assertEqual(self.p.list(file_id=fid)["jobs"], [])
        with patch("catabolic.watching.time.time", return_value=1093):
            result = cycle(self.app, operation="sniff", settle=30)
        self.assertTrue(result["complete"])
        self.assertEqual(len(self.p.list(file_id=fid)["jobs"]), 1)

    def test_permission_or_io_failure_during_scan_preserves_previous_observations(self):
        before = self.store.rows("SELECT * FROM observations")
        original = os.scandir

        def broken(fd):
            if isinstance(fd, int):
                raise OSError(errno.EIO, "fixture lost mount")
            return original(fd)

        with patch("catabolic.filesystem.os.scandir", side_effect=broken):
            self.assertFalse(self.app.scan()["complete"])
        self.assertEqual(self.store.rows("SELECT * FROM observations"), before)


class AttemptMigrationTest(unittest.TestCase):
    def test_schema_six_upgrade_preserves_populated_jobs_and_backup(self):
        from catabolic.migration import (
            SCHEMA_VERSION,
            load_migrations,
            upgrade_database,
            validate_preservation,
        )
        from tests.test_migrations import contents, create_legacy

        with tempfile.TemporaryDirectory() as directory:
            path = create_legacy(Path(directory))
            upgrade_database(path, migrations=load_migrations()[:6])
            import sqlite3
            from contextlib import closing

            with closing(sqlite3.connect(path)) as db:
                profile = db.execute("SELECT id FROM profiles LIMIT 1").fetchone()[0]
                fid, location = db.execute(
                    "SELECT id,location FROM files LIMIT 1"
                ).fetchone()
                db.execute(
                    "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,attempts,result) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                    (
                        "old-job",
                        profile,
                        fid,
                        "sniff",
                        location,
                        "{}",
                        "{}",
                        "key",
                        "complete",
                        1,
                        json.dumps({"mime": "video/test"}),
                    ),
                )
                db.commit()
            before = contents(path)
            result = upgrade_database(path)
            self.assertEqual(
                [s["version"] for s in result["applied"]],
                list(range(7, SCHEMA_VERSION + 1)),
            )
            self.assertEqual(contents(Path(result["backup"])), before)
            with Store(path) as store:
                validate_preservation(store.db, before)
                self.assertEqual(store.rows("SELECT * FROM processing_attempts"), [])
