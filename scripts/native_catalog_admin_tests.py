#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Supplemental installed tests for the opt-in read-only catalog admin seam.

Run outside the checkout with -I -B and CATABOLIC_EXPECTED_NATIVE_SCHEMA set to
the source-frozen target. The surrounding exact-wheel qualification must verify
the same installed payloads before and after this suite; these tests alone do
not issue an artifact, parity, migration, recovery, or release acceptance receipt.
"""

import hashlib
import os
import sqlite3
import sys
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

import catabolic_native as native
from catabolic_native import catalog_admin

if not __debug__ or not sys.flags.isolated or not sys.dont_write_bytecode:
    raise RuntimeError("catalog admin tests require isolated, unoptimized Python -I -B")
EXPECTED_SCHEMA = int(os.environ["CATABOLIC_EXPECTED_NATIVE_SCHEMA"])
if EXPECTED_SCHEMA <= 32:
    raise RuntimeError("explicit source-frozen native schema target required")


class InstalledCatalogAdmin(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="catabolic-admin-fixture-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "legacy.sqlite3"
        result = native.initialize_database(self.path)
        self.assertEqual(result["schema"], 32)

    def snapshot(self):
        return {
            str(path.relative_to(self.root)): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in self.root.rglob("*")
            if path.is_file()
        }

    def test_default_initializer_stays_legacy_and_inspection_never_upgrades(self):
        before = self.snapshot()
        for full in (False, True):
            value = catalog_admin.inspect_database(self.path, full=full)
            self.assertEqual(value["schema"], 32)
            self.assertEqual(value["latest_schema"], EXPECTED_SCHEMA)
            self.assertFalse(value["quiescence_verified"])
            self.assertTrue(value["requires_quiescence"])
            self.assertFalse(value["safe"])
        self.assertEqual(self.snapshot(), before)

    def test_upgrade_plan_is_deterministic_complete_and_read_only(self):
        before = self.snapshot()
        first = catalog_admin.plan_upgrade(self.path)
        self.assertEqual(first, catalog_admin.plan_upgrade(self.path))
        self.assertEqual(first["contract"], "catabolic-native-upgrade-v1")
        self.assertEqual(first["from_schema"], 32)
        self.assertEqual(first["to_schema"], EXPECTED_SCHEMA)
        self.assertEqual(len(first["migrations"]), EXPECTED_SCHEMA - 32)
        self.assertEqual(len(first["migration_resources"]), EXPECTED_SCHEMA)
        self.assertTrue(first["quiescence_required"])
        self.assertEqual(
            first["execution_journal_modes"], ["delete", "truncate", "persist"]
        )
        self.assertEqual(len(first["plan_fingerprint"]), 64)
        self.assertEqual(self.snapshot(), before)

    def test_missing_catalog_is_not_created(self):
        missing = self.root / "missing.sqlite3"
        before = self.snapshot()
        for call in (catalog_admin.inspect_database, catalog_admin.plan_upgrade):
            with self.subTest(call=call.__name__), self.assertRaises(RuntimeError):
                call(missing)
        self.assertFalse(missing.exists())
        self.assertEqual(self.snapshot(), before)

    def test_invalid_bytes_are_not_repaired(self):
        bad = self.root / "invalid.sqlite3"
        bad.write_bytes(b"disposable non-SQLite input")
        before = self.snapshot()
        for call in (catalog_admin.inspect_database, catalog_admin.plan_upgrade):
            with self.subTest(call=call.__name__), self.assertRaises(RuntimeError):
                call(bad)
        self.assertEqual(self.snapshot(), before)

    def test_unsupported_future_schema_is_not_rewritten(self):
        with closing(sqlite3.connect(self.path)) as database:
            database.execute(f"PRAGMA user_version={EXPECTED_SCHEMA + 1000}")
        before = self.snapshot()
        for call in (catalog_admin.inspect_database, catalog_admin.plan_upgrade):
            with self.subTest(call=call.__name__), self.assertRaises(RuntimeError):
                call(self.path)
        self.assertEqual(self.snapshot(), before)

    def test_optional_module_exposes_no_mutation_or_authority_entrypoint(self):
        public_functions = {
            name
            for name, value in vars(catalog_admin).items()
            if not name.startswith("_") and callable(value)
        }
        self.assertEqual(public_functions, {"inspect_database", "plan_upgrade"})


if __name__ == "__main__":
    unittest.main(verbosity=2)
