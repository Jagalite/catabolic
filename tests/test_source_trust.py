# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import contextlib
import io
import json
import os
import unittest
from unittest.mock import patch

from catabolic import app as app_module
from catabolic.app import Application
from catabolic.cli import main
from catabolic.domain import CatabolicError
from catabolic.filesystem import root_handle
from catabolic.reconcile import Reconciler
from catabolic.remount import repair
from catabolic.source_trust import configure
from catabolic.store import Store
from tests import test_remount
from tests.test_consumer_publication import generations


class SourceTrustTest(unittest.TestCase):
    setUp = test_remount.RemountTest.setUp

    def replace_source(self):
        (self.root / "source").rename(self.root / "previous-source")
        (self.root / "source").mkdir()
        (self.root / "source/movie.mkv").write_bytes(b"replacement media fixture")
        inode = (self.root / "source").stat().st_ino
        for name in (
            "catabolic.volume_identity.volume_uuid",
            "catabolic.remount.volume_uuid",
        ):
            mock = patch(
                name,
                side_effect=lambda fd: (
                    "replacement-volume"
                    if os.fstat(fd).st_ino == inode
                    else "fixture-volume"
                ),
            )
            mock.start()
            self.addCleanup(mock.stop)

    def test_explicit_replacement_adoption_updates_versions_and_preserves_links(self):
        self.replace_source()
        before = generations(self.app)
        with self.assertRaises(CatabolicError):
            repair(self.app)
        preview = repair(self.app, trust_sources=["source"])
        self.assertFalse(preview["applied"])
        with self.assertRaisesRegex(CatabolicError, "preview changed"):
            repair(
                self.app, apply=True, trust_sources=["source"], expected_plan="wrong"
            )
        done = repair(
            self.app,
            apply=True,
            trust_sources=["source"],
            expected_plan=preview["plan_id"],
        )
        self.assertTrue(done["verification"]["healthy"])
        after = generations(self.app)
        self.assertGreater(after["generation"], before["generation"])
        self.assertEqual(after["acknowledged"], before["acknowledged"])
        self.assertEqual((self.root / "output/Movie.mkv").lstat(), self.before_link)
        with root_handle(self.app.binding("source", "source")):
            pass
        self.assertFalse(self.app.binding("source", "source").trust_path)
        self.assertEqual(self.store.rows("SELECT * FROM source_identity_policies"), [])

    def test_persistent_path_policy_requires_scan_and_can_be_disabled(self):
        self.replace_source()
        preview = configure(self.app, "source", "path")
        self.assertFalse(preview["applied"])
        with (
            self.assertRaises(CatabolicError),
            root_handle(self.app.binding("source", "source")),
        ):
            pass
        configure(self.app, "source", "path", apply=True)
        with root_handle(self.app.binding("source", "source")):
            pass
        self.assertFalse(Reconciler(self.app).verify()["healthy"])
        self.app.scan("source")
        self.assertTrue(Reconciler(self.app).verify()["healthy"])
        configure(self.app, "source", "strict", apply=True)
        with (
            self.assertRaises(CatabolicError),
            root_handle(self.app.binding("source", "source")),
        ):
            pass

    def test_path_policy_still_detects_root_replacement_mid_scan(self):
        configure(self.app, "source", "path", apply=True)
        original = app_module.walk_files

        def swap(fd, **kwargs):
            result = original(fd, **kwargs)
            (self.root / "source").rename(self.root / "old-source")
            (self.root / "source").mkdir()
            return result

        with patch("catabolic.app.walk_files", side_effect=swap):
            result = self.app.scan("source")
        self.assertFalse(result["scans"][0]["complete"])

    def test_source_trust_never_relaxes_output_or_missing_file_checks(self):
        configure(self.app, "source", "path", apply=True)
        (self.root / "source/movie.mkv").unlink()
        with self.assertRaises(OSError):
            repair(self.app, trust_sources=["source"])
        self.assertFalse(Reconciler(self.app).verify()["healthy"])
        with self.assertRaisesRegex(CatabolicError, "unknown source"):
            repair(self.app, trust_sources=["global"])

    def test_interruption_rolls_back_trusted_versions_and_generation(self):
        self.replace_source()
        old = generations(self.app)
        bindings = self.store.rows("SELECT * FROM bindings")
        preview = repair(self.app, trust_sources=["source"])
        with patch.object(Reconciler, "verify", side_effect=RuntimeError("crash")):
            with self.assertRaises(RuntimeError):
                repair(
                    self.app,
                    apply=True,
                    trust_sources=["source"],
                    expected_plan=preview["plan_id"],
                )
        self.assertEqual(self.store.rows("SELECT * FROM bindings"), bindings)
        self.assertEqual(generations(self.app), old)
        self.assertEqual(self.store.rows("SELECT * FROM remount_guard"), [])

    def test_machine_cli_policy_and_default_strict(self):
        self.store.close()

        def command(*args):
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                code = main(["--db", str(self.path), "--machine", "location", *args])
            self.assertEqual(code, 0, out.getvalue())
            value = json.loads(out.getvalue())
            self.assertEqual(value["interface_version"], 1)
            return value["data"]

        self.assertFalse(command("trust", "source", "--identity", "path")["applied"])
        self.assertTrue(
            command("trust", "source", "--identity", "path", "--apply")["applied"]
        )
        with Store(self.path) as store:
            self.assertTrue(Application(store).binding("source", "source").trust_path)
        self.assertTrue(
            command("trust", "source", "--identity", "strict", "--apply")["applied"]
        )

    def test_precise_override_history_and_scan_evidence(self):
        before = configure(self.app, "source")
        self.assertEqual(
            before["settings"], dict.fromkeys(("uuid", "device", "inode"), "check")
        )
        changed = configure(self.app, "source", overrides={"uuid": "skip"}, apply=True)
        self.assertEqual(changed["revision"], 1)
        self.assertEqual(changed["settings"]["device"], "check")
        scan = self.app.scan("source")["scans"][0]
        self.assertTrue(scan["complete"])
        self.assertFalse(scan["validation"]["identity_verified"])
        self.assertEqual(scan["validation"]["checks"]["uuid"], "skipped_owner_policy")
        historical = self.store.rows(
            "SELECT value FROM meta WHERE key=?",
            (f"scan:{scan['scan_id']}:validation",),
        )
        configure(self.app, "source", "strict", apply=True)
        self.assertEqual(
            historical,
            self.store.rows(
                "SELECT value FROM meta WHERE key=?",
                (f"scan:{scan['scan_id']}:validation",),
            ),
        )
        self.assertEqual(len(configure(self.app, "source")["history"]), 2)
        self.assertEqual(
            configure(self.app, "source", "strict", apply=True)["revision"], 2
        )

    def test_preset_precedence_and_invalid_atomicity(self):
        report = configure(
            self.app, "source", "path", overrides={"inode": "check"}, apply=True
        )
        self.assertEqual(
            report["settings"], {"uuid": "skip", "device": "skip", "inode": "check"}
        )
        self.replace_source()
        with (
            self.assertRaises(CatabolicError),
            root_handle(self.app.binding("source", "source")),
        ):
            pass
        with self.assertRaises(CatabolicError):
            configure(self.app, "source", overrides={"everything": "skip"}, apply=True)
        self.assertEqual(configure(self.app, "source")["revision"], 1)

    def test_healthy_publication_does_not_claim_skipped_identity(self):
        configure(self.app, "source", "path", apply=True)
        report = Reconciler(self.app).verify()
        self.assertTrue(report["healthy"])
        self.assertFalse(report["catalogs"][0]["identity_verified"])
        (self.root / "source/movie.mkv").unlink()
        self.assertFalse(Reconciler(self.app).verify()["healthy"])

    def test_policy_change_fences_pending_processing_and_query_evidence(self):
        from catabolic.processing import Processing
        from catabolic.sql_query import execute_sql

        processing = Processing(self.app)
        processing.enqueue("hash", location="source")
        configure(self.app, "source", "path", apply=True)
        processing.run(_refresh=False)
        self.assertEqual(
            self.store.rows("SELECT state FROM processing_jobs")[0]["state"], "changed"
        )
        scan = self.app.scan("source")["scans"][0]
        query = execute_sql(
            self.path,
            "SELECT complete,identity_verified FROM catalog_scan_validation WHERE scan_id='"
            + scan["scan_id"]
            + "'",
        )
        self.assertEqual(query["rows"], [[1, 0]])
        self.assertEqual(self.store.rows("SELECT * FROM file_facts"), [])

    def test_populated_schema19_preserves_policy_without_inventing_evidence(self):
        import sqlite3

        from catabolic.migration import load_migrations, upgrade_database

        path = test_remount.create_legacy(self.root)
        upgrade_database(path, migrations=load_migrations()[:19])
        with contextlib.closing(sqlite3.connect(path)) as db, db:
            profile = db.execute("SELECT id FROM profiles LIMIT 1").fetchone()[0]
            location = db.execute("SELECT id FROM locations LIMIT 1").fetchone()[0]
            db.execute(
                "INSERT INTO source_identity_policies VALUES (?,?,?)",
                (profile, location, "path"),
            )
        upgrade_database(path)
        with Store(path) as store:
            self.assertEqual(
                store.rows(
                    "SELECT identity_policy,revision FROM source_identity_policies"
                ),
                [{"identity_policy": "path", "revision": 1}],
            )
            self.assertIn(
                "migration_baseline",
                store.rows("SELECT settings FROM source_policy_history")[0]["settings"],
            )
            self.assertEqual(
                store.rows("SELECT * FROM meta WHERE key LIKE 'scan:%:validation'"), []
            )
