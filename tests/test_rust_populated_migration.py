# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Reuse richer historical scenarios unchanged, comparing independent upgrades."""

import json
import shutil
import sqlite3
import subprocess
import unittest
from datetime import datetime, timezone
from unittest.mock import patch

from catabolic.migration import data_snapshot, upgrade_database
from tests import test_component_migration as components
from tests import test_programmable_migration as programmable
from tests.test_rust_migration import BINARY


def compare_upgrade(test, path, **kwargs):
    native = path.with_name(path.name + ".native")
    shutil.copyfile(path, native)
    started = datetime.now(timezone.utc).replace(tzinfo=None, microsecond=0)
    result = upgrade_database(path, **kwargs)
    process = subprocess.run(
        [str(BINARY), "--db", str(native), "db", "upgrade"],
        capture_output=True,
        text=True,
        timeout=120,
    )
    test.assertEqual(process.returncode, 0, process.stderr)
    test.assertEqual(json.loads(process.stdout)["schema"], result["schema"])
    with sqlite3.connect(path) as reference, sqlite3.connect(native) as actual:
        actual_snapshot, expected_snapshot = (
            data_snapshot(actual),
            data_snapshot(reference),
        )
        different = [
            table
            for table in expected_snapshot
            if actual_snapshot.get(table) != expected_snapshot[table]
        ]
        finished = datetime.now(timezone.utc).replace(tzinfo=None, microsecond=0)
        for table in different:
            # These tables are newly created by this upgrade. CURRENT_TIMESTAMP
            # is explicitly nondeterministic (plan section 9.1); all historical
            # columns, identities, JSON bytes and digests still compare exactly.
            test.assertIn(table, ("saved_queries", "component_occurrences"))
            columns = [r[1] for r in reference.execute(f'PRAGMA table_info("{table}")')]
            stamp = columns.index("created_at")
            projections = []
            for db in (actual, reference):
                records = db.execute(f'SELECT * FROM "{table}" ORDER BY id').fetchall()
                for record in records:
                    test.assertRegex(
                        record[stamp], r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$"
                    )
                    test.assertTrue(
                        started <= datetime.fromisoformat(record[stamp]) <= finished
                    )
                projections.append(
                    [record[:stamp] + record[stamp + 1 :] for record in records]
                )
            test.assertEqual(*projections, table)
    return result


class NativePopulatedComponentMigrationTest(unittest.TestCase):
    setUp = components.MigrationTest.setUp
    seed_legacy_inventory = components.MigrationTest.seed_legacy_inventory

    def test_populated_schema24_with_probe_and_sidecar(self):
        with patch.object(
            components,
            "upgrade_database",
            side_effect=lambda path: compare_upgrade(self, path),
        ):
            components.MigrationTest.test_populated_schema_24_adopts_probe_and_sidecar_without_rewriting_data(
                self
            )


class NativePopulatedProgrammableMigrationTest(unittest.TestCase):
    setUp = programmable.ProgrammableMigrationTest.setUp

    def test_populated_schema14_recipe_query_and_foreign_keys(self):
        with patch.object(
            programmable,
            "upgrade_database",
            side_effect=lambda path: compare_upgrade(self, path),
        ):
            programmable.ProgrammableMigrationTest.test_schema14_preserves_all_original_columns_ids_and_foreign_keys(
                self
            )
