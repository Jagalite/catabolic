# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native subprocess qualification against the retained Python reference."""

import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path

from catabolic.migration import data_snapshot, inspect_database, upgrade_database
from catabolic.store import Store, encode

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/debug/catabolic-native"
FIXTURES = ROOT / ".local-tests/rust-migration/m0-completion/legacy-fixtures"


@unittest.skipUnless(
    BINARY.is_file(), "build catabolic-cli before native qualification"
)
class NativeStorageTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.path = self.root / "native.sqlite3"

    def native(self, *args, ok=True):
        run = subprocess.run(
            [str(BINARY), "--db", str(self.path), "--json", *map(str, args)],
            capture_output=True,
            text=True,
            timeout=120,
        )
        if ok:
            self.assertEqual(run.returncode, 0, run.stderr)
            return json.loads(run.stdout)
        self.assertEqual(run.returncode, 2, run.stdout)
        return run.stderr

    def test_all_32_retained_historical_fixtures(self):
        self.assertTrue(FIXTURES.is_dir(), "generate frozen historical fixtures first")
        for version in range(1, 33):
            with self.subTest(schema=version):
                self.path.unlink(missing_ok=True)
                before = FIXTURES / f"schema-{version:02d}/before.sqlite3"
                shutil.copyfile(before, self.path)
                python_path = self.root / f"python-{version}.sqlite3"
                shutil.copyfile(before, python_path)
                expected_info = inspect_database(python_path, full=True)
                self.assertEqual(
                    self.native("db", "inspect", "--full")["history"],
                    expected_info["history"],
                )
                result = self.native("db", "upgrade")
                upgrade_database(python_path)
                with (
                    sqlite3.connect(self.path) as native,
                    sqlite3.connect(python_path) as reference,
                ):
                    self.assertEqual(data_snapshot(native), data_snapshot(reference))
                    self.assertEqual(
                        native.execute("PRAGMA foreign_key_check").fetchall(), []
                    )
                    self.assertEqual(
                        native.execute("PRAGMA integrity_check").fetchall(), [("ok",)]
                    )
                # Python opens the Rust-upgraded result without migration.
                with Store(self.path) as store:
                    self.assertEqual(store.schema_version, 32)
                if version < 32:
                    backup = Path(result["backup"])
                    self.assertEqual(
                        inspect_database(backup, full=True)["schema"], version
                    )
                    manifest = json.loads((backup.parent / "manifest.json").read_text())
                    self.assertEqual(manifest["status"], "committed")
                    self.assertEqual(
                        manifest["sha256"],
                        hashlib.sha256(backup.read_bytes()).hexdigest(),
                    )

    def test_native_initialization_and_frozen_codecs(self):
        result = self.native("init")
        self.assertEqual(result["schema"], 32)
        inspect_database(self.path, full=True)
        samples = [
            {"z": "Café😀", "a": [True, None, 2**63 - 1]},
            [1e-7, 1e-5, 1e20, -0.0, 1.0, -1971969108.7328339, "\u0001\n\t\u007f"],
            {"nested": {"β": "é", "a": []}},
        ]
        for sample in samples:
            self.assertEqual(
                self.native("encode", json.dumps(sample))["encoded"], encode(sample)
            )
        self.assertIn("File exists", self.native("init", ok=False))

    def test_reads_and_dry_run_do_not_change_database(self):
        shutil.copyfile(FIXTURES / "schema-14/before.sqlite3", self.path)
        before = self.path.read_bytes()
        self.native("db", "inspect", "--full")
        self.native("db", "snapshot")
        self.native("db", "upgrade", "--dry-run")
        self.assertEqual(self.path.read_bytes(), before)
        self.assertFalse(Path(str(self.path) + ".lock").exists())
        self.assertFalse(Path(str(self.path) + ".backups").exists())

    def test_writer_lock_is_shared_with_python(self):
        self.native("init")
        with Store(self.path, writable=True):
            self.assertIn(
                "another Catabolic writer", self.native("db", "upgrade", ok=False)
            )

    def test_invalid_history_structure_and_identity_refuse_unchanged(self):
        for sql, message in [
            (
                "UPDATE schema_migrations SET checksum=printf('%064d',0) WHERE version=1",
                "history is damaged",
            ),
            (
                "DELETE FROM schema_migrations WHERE version=2",
                "incomplete or inconsistent",
            ),
            (
                "UPDATE meta SET value='not-a-uuid' WHERE key='database_id'",
                "identity is missing or invalid",
            ),
            ("CREATE TABLE unexpected(x)", "structure does not match"),
        ]:
            with self.subTest(sql=sql):
                self.path.unlink(missing_ok=True)
                shutil.copyfile(FIXTURES / "schema-14/before.sqlite3", self.path)
                with sqlite3.connect(self.path) as db:
                    db.execute(sql)
                before = self.path.read_bytes()
                self.assertIn(message, self.native("db", "upgrade", ok=False))
                self.assertEqual(self.path.read_bytes(), before)

    def test_symlink_and_hardlink_database_refusal(self):
        original = self.root / "original.sqlite3"
        shutil.copyfile(FIXTURES / "schema-32/before.sqlite3", original)
        self.path.symlink_to(original)
        self.assertIn("symlink", self.native("db", "inspect", ok=False))
        self.path.unlink()
        os.link(original, self.path)
        self.assertIn("hard-linked", self.native("db", "inspect", ok=False))

    def test_failures_before_commit_preserve_original(self):
        for point in [
            "upgrade:after_backup:14",
            "upgrade:after_rehearsal:14",
            "rehearsal:before_migration:15",
            "rehearsal:after_migration:25",
            "rehearsal:before_commit:32",
            "upgrade:before_migration:15",
            "upgrade:after_migration:25",
            "upgrade:before_commit:32",
        ]:
            with self.subTest(point=point):
                self.path.unlink(missing_ok=True)
                shutil.copyfile(FIXTURES / "schema-14/before.sqlite3", self.path)
                with sqlite3.connect(self.path) as db:
                    before = data_snapshot(db)
                self.assertIn(
                    "injected migration failure",
                    self.native(
                        "db", "upgrade", "--migration-failpoint", point, ok=False
                    ),
                )
                with sqlite3.connect(self.path) as db:
                    self.assertEqual(
                        db.execute("PRAGMA user_version").fetchone()[0], 14
                    )
                    self.assertEqual(data_snapshot(db), before)
