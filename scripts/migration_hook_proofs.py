#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Independent fault and encoding checks for the pinned Python migration hooks."""

import argparse
import hashlib
import json
import sqlite3
import sys
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from scripts.migration_contracts import verify_source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    args = parser.parse_args()
    source = args.source.resolve()
    verify_source(source, json.loads(Path("tests/parity/inventory.json").read_text()))
    sys.path[:0] = [str(source), str(source / "src")]
    from catabolic.domain import CatabolicError
    from catabolic.migration import (
        data_snapshot,
        execute_migration,
        load_migrations,
        upgrade_database,
    )
    from tests.test_migrations import create_legacy

    class Hooks(unittest.TestCase):
        def setUp(self):
            self.db = sqlite3.connect(":memory:")
            self.addCleanup(self.db.close)
            self.db.execute("PRAGMA foreign_keys=ON")
            self.db.execute("BEGIN")
            self.steps = load_migrations()

        def test_unicode_profile_precedence_and_digest(self):
            raw = '{"selection":{"profile":"chosen","query":"Café 宇宙"},"version":1}'
            sql = (
                "CREATE TABLE proof AS SELECT catabolic_query_v1('"
                + raw
                + "','fallback') value;"
            )
            execute_migration(self.db, replace(self.steps[14], sql=sql))
            value = self.db.execute("SELECT value FROM proof").fetchone()[0]
            expected = '{"selection": {"profile": "chosen", "query": "Café 宇宙"}, "version": 1}'
            self.assertEqual(value, expected)
            execute_migration(
                self.db,
                replace(
                    self.steps[14],
                    sql="CREATE TABLE digest AS SELECT catabolic_sha256_v1(value) value FROM proof;",
                ),
            )
            self.assertEqual(
                self.db.execute("SELECT value FROM digest").fetchone()[0],
                hashlib.sha256(expected.encode("utf-8")).hexdigest(),
            )
            with self.assertRaises(sqlite3.OperationalError):
                self.db.execute("SELECT catabolic_query_v1('{}','fallback')")

        def test_nonfinite_refusal_removes_udfs_and_authorizer(self):
            sql = "SELECT catabolic_query_v1('{\"selection\":{},\"value\":NaN}','default');"
            with self.assertRaises(CatabolicError):
                execute_migration(self.db, replace(self.steps[14], sql=sql))
            for expression in (
                "catabolic_query_v1('{}','default')",
                "catabolic_sha256_v1('value')",
            ):
                with self.assertRaises(sqlite3.OperationalError):
                    self.db.execute("SELECT " + expression)
            self.assertEqual(self.db.execute("PRAGMA user_version").fetchone()[0], 0)

        def test_rebuild_checks_final_foreign_keys_before_reset(self):
            self.db.execute("CREATE TABLE parent(id PRIMARY KEY)")
            self.db.execute("CREATE TABLE child(id REFERENCES parent(id))")
            self.db.commit()
            self.db.execute("BEGIN")
            with self.assertRaisesRegex(CatabolicError, "violated a foreign key"):
                execute_migration(
                    self.db,
                    replace(self.steps[15], sql="INSERT INTO child VALUES (42);"),
                )
            self.assertTrue(self.db.in_transaction)
            self.db.rollback()
            self.assertEqual(self.db.execute("SELECT * FROM child").fetchall(), [])
            self.assertEqual(self.db.execute("PRAGMA foreign_key_check").fetchall(), [])

        def test_backfill_failure_preserves_version24_data(self):
            with tempfile.TemporaryDirectory() as directory:
                path = create_legacy(Path(directory))
                upgrade_database(path, migrations=self.steps[:24])
                with sqlite3.connect(path) as db:
                    before = data_snapshot(db)

                from catabolic.components import backfill

                injected = []

                def fail_after_write(db):
                    backfill(db)
                    # Let schema validation and rehearsal pass; fail the original
                    # database's apply phase, after a real transactional write.
                    filename = db.execute("PRAGMA database_list").fetchone()[2]
                    if filename and Path(filename).resolve() == path.resolve():
                        self.assertTrue(db.in_transaction)
                        db.execute(
                            "INSERT INTO meta VALUES ('injected','must roll back')"
                        )
                        injected.append(True)
                        raise RuntimeError("injected backfill failure")

                with patch(
                    "catabolic.components.backfill", side_effect=fail_after_write
                ):
                    with self.assertRaisesRegex(
                        (CatabolicError, RuntimeError), "injected backfill failure"
                    ):
                        upgrade_database(path)
                self.assertEqual(injected, [True])
                with sqlite3.connect(path) as db:
                    self.assertEqual(data_snapshot(db), before)
                    self.assertEqual(
                        db.execute("PRAGMA user_version").fetchone()[0], 24
                    )
                    self.assertEqual(
                        db.execute("PRAGMA integrity_check").fetchall(), [("ok",)]
                    )
                    self.assertEqual(
                        db.execute("PRAGMA foreign_key_check").fetchall(), []
                    )

    result = unittest.TextTestRunner(verbosity=2).run(
        unittest.defaultTestLoader.loadTestsFromTestCase(Hooks)
    )
    return int(not result.wasSuccessful())


if __name__ == "__main__":
    raise SystemExit(main())
