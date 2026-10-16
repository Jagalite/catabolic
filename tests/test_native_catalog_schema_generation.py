# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Packaging checks only; separate from frozen legacy qualification evidence."""

import ast
import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from scripts import native_catalog_schema as schema

ROOT = Path(__file__).resolve().parents[1]
EVIDENCE = ROOT / schema.EVIDENCE_PATH


class NativeCatalogSchemaGeneration(unittest.TestCase):
    def test_all_37_preparation_tests_are_preserved(self):
        provenance = json.loads((EVIDENCE / "source-provenance.json").read_text())
        tree = ast.parse((ROOT / "tests/test_native_catalog_schema.py").read_text())
        tests = [
            node.name
            for node in ast.walk(tree)
            if isinstance(node, ast.FunctionDef) and node.name.startswith("test_")
        ]
        self.assertEqual(provenance["preserved_schema_test_count"], 37)
        self.assertEqual(tests, provenance["preserved_schema_tests"])

    def test_legacy_selection_is_exactly_001_to_032(self):
        paths = schema.legacy_paths()
        self.assertEqual([int(path.name[:3]) for path in paths], list(range(1, 33)))
        self.assertTrue((ROOT / schema.SQL_PATH).is_file())
        self.assertNotIn(ROOT / schema.SQL_PATH, paths)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            migrations = root / schema.RESOURCE_PATH / "migrations"
            migrations.mkdir(parents=True)
            for path in paths:
                (migrations / path.name).touch()
            (migrations / "033_native_catalog.sql").write_text("INVALID SQL")
            (migrations / "999_future.sql").write_text("INVALID SQL")
            self.assertEqual(len(schema.legacy_paths(root)), 32)
            (migrations / paths[0].name).unlink()
            with self.assertRaisesRegex(ValueError, "exactly one frozen migration"):
                schema.legacy_paths(root)
            (migrations / paths[0].name).touch()
            (migrations / "001_duplicate.sql").touch()
            with self.assertRaisesRegex(ValueError, "exactly one frozen migration"):
                schema.legacy_paths(root)

    def test_original_preparation_resources_stay_byte_identical(self):
        provenance = json.loads((EVIDENCE / "source-provenance.json").read_text())
        for path, digest in provenance["legacy_resource_sha256"].items():
            with self.subTest(path=path):
                self.assertEqual(
                    hashlib.sha256((ROOT / path).read_bytes()).hexdigest(), digest
                )
        for filename, relative in (
            ("033_native_catalog.base.sql", schema.BASE_PATH),
            ("033_native_catalog.sql", schema.SQL_PATH),
            ("native-schema-33.json", schema.SIGNATURE_PATH),
        ):
            with self.subTest(path=relative):
                self.assertEqual(
                    hashlib.sha256((ROOT / relative).read_bytes()).hexdigest(),
                    provenance["prepared_source_sha256"][filename],
                )

    def test_generation_is_deterministic_and_matches_all_artifacts(self):
        first = schema.generate()
        second = schema.generate()
        self.assertEqual(first, second)
        self.assertEqual(
            set(first),
            {
                schema.SQL_PATH,
                schema.SIGNATURE_PATH,
                schema.INVENTORY_PATH,
                schema.MANIFEST_PATH,
            },
        )
        for relative, content in first.items():
            with self.subTest(path=relative):
                self.assertEqual(
                    (ROOT / relative).read_bytes(), content.encode("utf-8")
                )

    def test_cli_check_and_output_are_working_directory_independent(self):
        before = {
            relative: (ROOT / relative).read_bytes()
            for relative in (
                schema.SQL_PATH,
                schema.SIGNATURE_PATH,
                schema.INVENTORY_PATH,
                schema.MANIFEST_PATH,
            )
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = ROOT / "scripts/native_catalog_schema.py"
            checked = subprocess.run(
                [sys.executable, str(script), "--check"],
                cwd=root,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)
            generated = subprocess.run(
                [sys.executable, str(script), "--output-dir", str(root / "generated")],
                cwd=root,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(
                generated.returncode, 0, generated.stdout + generated.stderr
            )
            actual_files = {
                path.relative_to(root / "generated")
                for path in (root / "generated").rglob("*")
                if path.is_file()
            }
            self.assertEqual(actual_files, set(before))
            for relative, expected in before.items():
                self.assertEqual((root / "generated" / relative).read_bytes(), expected)
                self.assertEqual((ROOT / relative).read_bytes(), expected)

    def test_generator_is_relocatable_and_does_not_consume_033_as_legacy(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = ROOT / "scripts/native_catalog_schema.py"
            target = root / "scripts/native_catalog_schema.py"
            target.parent.mkdir()
            shutil.copyfile(source, target)
            base = root / schema.BASE_PATH
            base.parent.mkdir(parents=True)
            shutil.copyfile(ROOT / schema.BASE_PATH, base)
            for path in schema.legacy_paths():
                destination = root / path.relative_to(ROOT)
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, destination)
            (root / schema.SQL_PATH).write_text(
                "INVALID SQL: never consume migration 033"
            )
            result = subprocess.run(
                [sys.executable, str(target), "--output-dir", str(root / "generated")],
                cwd=root,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            # Rust ownership scans differ in this minimal copy; SQL/signature cannot.
            for relative in (schema.SQL_PATH, schema.SIGNATURE_PATH):
                self.assertEqual(
                    (root / "generated" / relative).read_bytes(),
                    (ROOT / relative).read_bytes(),
                )

    def test_review_block_is_preserved_without_claiming_clearance(self):
        provenance = json.loads((EVIDENCE / "source-provenance.json").read_text())
        review_path = ROOT / provenance["independent_review"]["record"]
        self.assertEqual(
            hashlib.sha256(review_path.read_bytes()).hexdigest(),
            provenance["independent_review"]["record_sha256"],
        )
        review = json.loads(review_path.read_text())
        self.assertEqual(review["review_status"], "incomplete")
        self.assertFalse(review["retry_or_reassignment"])
        self.assertIn("Agent errored: This content was flagged", review["result"])
        manifest = json.loads((ROOT / schema.MANIFEST_PATH).read_text())
        self.assertEqual(
            manifest["status"], "schema-only-regression-independent-review-incomplete"
        )
        self.assertIn(
            "not Rust-owner or release qualification", manifest["qualification_scope"]
        )
        self.assertFalse(provenance["frozen_legacy_ledger_modified"])
        self.assertFalse(provenance["production_sql_semantics_modified"])


if __name__ == "__main__":
    unittest.main()
