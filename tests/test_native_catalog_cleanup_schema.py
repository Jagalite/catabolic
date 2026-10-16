# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Portable schema36 reproduction only; no independent review or runtime claim."""

import hashlib
import json
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import native_catalog_cleanup_schema as cleanup

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = Path("scripts/native_catalog_cleanup_schema.py")


class NativeCatalogCleanupSchema(unittest.TestCase):
    def relocated(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name) / "source"
        for path in (
            SCRIPT,
            Path("scripts/native_catalog_schema.py"),
            cleanup.MANIFEST_PATH,
        ):
            target = root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / path, target)
        shutil.copytree(ROOT / cleanup.RESOURCES, root / cleanup.RESOURCES)
        return root

    def run_cli(self, root, *args):
        return subprocess.run(
            [sys.executable, "-I", "-B", str(root / SCRIPT), *map(str, args)],
            cwd=root.parent,
            text=True,
            capture_output=True,
            check=False,
        )

    def inventory(self, root):
        return {
            str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(root.rglob("*"))
            if path.is_file()
        }

    def test_exact_frozen_signature_and_manifest_are_reproduced(self):
        artifacts = cleanup.generate()
        self.assertEqual(
            set(artifacts), {cleanup.SIGNATURE_PATH, cleanup.MANIFEST_PATH}
        )
        for path, text in artifacts.items():
            self.assertEqual((ROOT / path).read_bytes(), text.encode("utf-8"))
        manifest = json.loads(artifacts[cleanup.MANIFEST_PATH])
        self.assertEqual(manifest["schema_version"], 36)
        self.assertEqual(manifest["prior_schema"], 35)
        self.assertEqual(manifest["new_objects"], 6)
        self.assertEqual(set(manifest["new_tables"]), {"native_execution_lifetimes"})
        self.assertEqual(len(manifest["input_sha256"]), 37)
        self.assertTrue(manifest["historical_ddl_preserved"])
        self.assertFalse(manifest["execution_lifetimes_backfilled"])
        self.assertIn("incomplete", manifest["independent_review"])

    def test_read_only_check_is_relocatable_and_cwd_independent(self):
        root = self.relocated()
        before = self.inventory(root)
        result = self.run_cli(root, "--check")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.inventory(root), before)

    def test_output_is_limited_to_two_derived_files_outside_source(self):
        root = self.relocated()
        before = self.inventory(root)
        output = root.parent / "derived"
        result = self.run_cli(root, "--output-dir", output)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            set(self.inventory(output)),
            {str(cleanup.SIGNATURE_PATH), str(cleanup.MANIFEST_PATH)},
        )
        for path in (cleanup.SIGNATURE_PATH, cleanup.MANIFEST_PATH):
            self.assertEqual((output / path).read_bytes(), (root / path).read_bytes())
        self.assertEqual(self.inventory(root), before)

    def test_output_refuses_source_paths_and_existing_directories(self):
        root = self.relocated()
        before = self.inventory(root)
        for output in (root, root / "new", root.parent):
            with self.subTest(output=output):
                result = self.run_cli(root, "--output-dir", output)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("new directory outside the source root", result.stderr)
        self.assertEqual(self.inventory(root), before)

    def test_fixed_inputs_refuse_even_whitespace_drift_before_database_open(self):
        root = self.relocated()
        for path in cleanup.FIXED_INPUTS:
            with self.subTest(path=path):
                original = (root / path).read_bytes()
                (root / path).write_bytes(original + b"\n")
                try:
                    with mock.patch.object(cleanup.foundation, "legacy_db") as factory:
                        with self.assertRaisesRegex(
                            ValueError, "immutable schema input"
                        ):
                            cleanup.generate(root)
                        factory.assert_not_called()
                finally:
                    (root / path).write_bytes(original)

    def test_future_migrations_are_never_consumed(self):
        root = self.relocated()
        future = root / cleanup.RESOURCES / "migrations/037_future.sql"
        future.write_text("invalid SQL must not execute", encoding="utf-8")
        self.assertEqual(cleanup.generate(root), cleanup.generate())

    def test_prior_signature_mismatch_closes_owned_connection(self):
        db = cleanup.foundation.legacy_db()
        self.addCleanup(db.close)
        with mock.patch.object(cleanup.foundation, "legacy_db", return_value=db):
            with mock.patch.object(cleanup.foundation, "objects", return_value=[]):
                with self.assertRaisesRegex(ValueError, "schema35 prerequisite"):
                    cleanup.generate()
        with self.assertRaises(sqlite3.ProgrammingError):
            db.execute("SELECT 1")

    def test_historical_ddl_must_be_preserved(self):
        original = cleanup.foundation.objects
        calls = 0

        def remove_prior_object(db):
            nonlocal calls
            calls += 1
            result = original(db)
            if calls == 2:
                result = [obj for obj in result if obj[1] != "items"]
            return result

        with mock.patch.object(
            cleanup.foundation, "objects", side_effect=remove_prior_object
        ):
            with self.assertRaisesRegex(ValueError, "historical DDL changed"):
                cleanup.generate()

    def test_interruption_closes_owned_connection(self):
        db = cleanup.foundation.legacy_db()
        self.addCleanup(db.close)
        with mock.patch.object(cleanup.foundation, "legacy_db", return_value=db):
            with mock.patch.object(
                cleanup.foundation, "objects", side_effect=KeyboardInterrupt
            ):
                with self.assertRaises(KeyboardInterrupt):
                    cleanup.generate()
        with self.assertRaises(sqlite3.ProgrammingError):
            db.execute("SELECT 1")


if __name__ == "__main__":
    unittest.main()
