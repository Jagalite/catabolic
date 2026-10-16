# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Owned generator connections close on success, failure and interruption."""

import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import native_catalog_curation_schema as curation
from scripts import native_catalog_schema as foundation
from scripts import native_catalog_search_schema as search


class TrackedConnection(sqlite3.Connection):
    closed = False

    def close(self):
        super().close()
        self.closed = True


class NativeCatalogSchemaConnections(unittest.TestCase):
    def setUp(self):
        self.connections = []
        connect = sqlite3.connect

        def tracked(*args, **kwargs):
            db = connect(*args, factory=TrackedConnection, **kwargs)
            self.connections.append(db)
            self.addCleanup(db.close)
            return db

        patcher = mock.patch.object(sqlite3, "connect", side_effect=tracked)
        patcher.start()
        self.addCleanup(patcher.stop)

    def assert_closed(self, expected):
        self.assertEqual(len(self.connections), expected)
        for db in self.connections:
            self.assertTrue(db.closed, "generator retained an owned connection")
            with self.assertRaises(sqlite3.ProgrammingError):
                db.execute("SELECT 1")

    def test_legacy_factory_transfers_successful_connection_to_caller(self):
        db = foundation.legacy_db()
        self.assertFalse(db.closed)
        self.assertEqual(db.execute("PRAGMA foreign_keys").fetchone(), (1,))
        foundation.tables(db)
        foundation.objects(db)
        self.assertFalse(db.closed, "borrowed inspection must not close its input")
        db.close()
        self.assert_closed(1)

    def test_factory_closes_when_input_selection_fails(self):
        with mock.patch.object(
            foundation, "legacy_paths", side_effect=ValueError("selection failure")
        ):
            with self.assertRaisesRegex(ValueError, "selection failure"):
                foundation.legacy_db()
        self.assert_closed(1)

    def test_factory_closes_when_interrupted(self):
        with mock.patch.object(
            foundation, "legacy_paths", side_effect=KeyboardInterrupt
        ):
            with self.assertRaises(KeyboardInterrupt):
                foundation.legacy_db()
        self.assert_closed(1)

    def test_factory_closes_when_migration_is_invalid(self):
        with tempfile.TemporaryDirectory() as directory:
            migration = Path(directory) / "invalid.sql"
            migration.write_text("this is invalid SQL", encoding="utf-8")
            with mock.patch.object(
                foundation, "legacy_paths", return_value=[migration]
            ):
                with self.assertRaises(sqlite3.OperationalError):
                    foundation.legacy_db()
        self.assert_closed(1)

    def test_schema33_success_closes_both_connections(self):
        foundation.generate()
        self.assert_closed(2)

    def test_schema33_closes_after_first_connection_failure(self):
        with mock.patch.object(foundation, "objects", side_effect=ValueError("first")):
            with self.assertRaisesRegex(ValueError, "first"):
                foundation.generate()
        self.assert_closed(1)

    def test_schema33_closes_after_validation_connection_failure(self):
        original = foundation.script
        count = 0

        def fail_second(*args):
            nonlocal count
            count += 1
            if count == 2:
                raise ValueError("validation")
            return original(*args)

        with mock.patch.object(foundation, "script", side_effect=fail_second):
            with self.assertRaisesRegex(ValueError, "validation"):
                foundation.generate()
        self.assert_closed(2)

    def test_generators_close_after_interruption(self):
        for module in (foundation, curation, search):
            with self.subTest(module=module.__name__):
                before = len(self.connections)
                with mock.patch.object(
                    foundation, "objects", side_effect=KeyboardInterrupt
                ):
                    with self.assertRaises(KeyboardInterrupt):
                        module.generate()
                self.assert_closed(before + 1)

    def test_schema34_success_closes_connection(self):
        curation.generate()
        self.assert_closed(1)

    def test_schema35_success_closes_connection(self):
        search.generate()
        self.assert_closed(1)

    def test_additive_generators_close_after_inspection_failure(self):
        for module in (curation, search):
            with self.subTest(module=module.__name__):
                before = len(self.connections)
                with mock.patch.object(
                    foundation, "tables", side_effect=ValueError("inspection")
                ):
                    with self.assertRaisesRegex(ValueError, "inspection"):
                        module.generate()
                self.assert_closed(before + 1)


if __name__ == "__main__":
    unittest.main()
