# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""First-use native query ownership regressions on disposable catalogs.

Run with the explicitly selected native artifact installed or staged by the
existing qualification flow. No legacy engine, build, or fallback is used here.
This separate unittest module does not change the baseline installed query lane.
"""

import os
import sys
import tempfile
import unittest
from contextlib import nullcontext
from pathlib import Path

# Successful execution retains the existing native envelope on the public facade.
PROFILE_RESULT = {
    "data": {"profile": "default"},
    "extensions": {"interfaceVersion": 1, "profile": "default", "records": 0},
}


class NativeQuerySetupOwnershipTest(unittest.TestCase):
    def setUp(self):
        root = Path(os.environ["CATABOLIC_COMPLETION_EXTENSION_ROOT"]).resolve()
        self.assertTrue((root / "catabolic_native/_native.abi3.so").is_file())
        previous_path = sys.path[:]
        self.addCleanup(setattr, sys, "path", previous_path)
        sys.path.insert(0, str(root))
        import catabolic_native
        from catabolic_native import _native

        self.assertEqual(
            Path(catabolic_native.__file__).resolve(),
            root / "catabolic_native/__init__.py",
        )
        self.assertEqual(
            Path(_native.__file__).resolve(), root / "catabolic_native/_native.abi3.so"
        )
        from catabolic_native import CatabolicError, Store
        from catabolic_native.graphql_query import execute_graphql
        from catabolic_native.sql_query import execute_sql

        self.Store, self.Error = Store, CatabolicError
        self.sql, self.graphql = execute_sql, execute_graphql
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "catalog.db"
        self.Store.initialize(self.path)

    def test_first_queries_preserve_temp_storage_and_owner(self):
        diagnostic = {
            "data": None,
            "errors": [
                {
                    "message": "Syntax Error: Expected Name, found ')'.",
                    "locations": [{"line": 1, "column": 8}],
                }
            ],
        }
        for temp_store in (0, 1, 2):
            for transaction in (False, True):
                for first in ("graphql", "sql"):
                    with self.subTest(
                        temp_store=temp_store, transaction=transaction, first=first
                    ):
                        with self.Store(self.path, writable=True) as store:
                            self.assertEqual(
                                store.db.execute("PRAGMA temp_store").fetchone()[0], 2
                            )
                            store.db.execute(f"PRAGMA temp_store={temp_store}")
                            store.db.execute("PRAGMA busy_timeout=1777")
                            scope = (
                                store.transaction()
                                if transaction
                                else nullcontext(store.db)
                            )
                            with scope as db:
                                db.execute("CREATE TEMP TABLE owner_marker(value)")
                                db.execute("INSERT INTO owner_marker VALUES(73)")
                                db.execute(
                                    "CREATE TEMP VIEW owner_marker_view AS SELECT value FROM owner_marker"
                                )
                                db.execute(
                                    "CREATE TEMP TRIGGER owner_marker_guard BEFORE INSERT ON owner_marker WHEN NEW.value<0 BEGIN SELECT RAISE(ABORT,'owner marker guard'); END"
                                )
                                changes = db.execute(
                                    "SELECT total_changes()"
                                ).fetchone()[0]

                                def unchanged(
                                    db=db,
                                    transaction=transaction,
                                    changes=changes,
                                    temp_store=temp_store,
                                ):
                                    self.assertEqual(db.in_transaction, transaction)
                                    self.assertEqual(
                                        db.execute(
                                            "SELECT value FROM owner_marker_view"
                                        ).fetchone()[0],
                                        73,
                                    )
                                    self.assertEqual(
                                        db.execute("SELECT total_changes()").fetchone()[
                                            0
                                        ],
                                        changes,
                                    )
                                    self.assertEqual(
                                        db.execute("PRAGMA temp_store").fetchone()[0],
                                        temp_store,
                                    )
                                    self.assertEqual(
                                        db.execute("PRAGMA query_only").fetchone()[0], 0
                                    )
                                    self.assertEqual(
                                        db.execute("PRAGMA busy_timeout").fetchone()[0],
                                        1777,
                                    )

                                self.assertEqual(
                                    self.graphql(self.path, "{items()}", _store=store),
                                    diagnostic,
                                )
                                unchanged()
                                actions = {
                                    "graphql": lambda: self.graphql(
                                        self.path, "{profile}", _store=store
                                    ),
                                    "sql": lambda: self.sql(
                                        self.path,
                                        "SELECT value FROM owner_marker_view ORDER BY value",
                                        _store=store,
                                    ),
                                }
                                for language in (first, "graphql", "sql"):
                                    result = actions[language]()
                                    if language == "graphql":
                                        self.assertEqual(result, PROFILE_RESULT)
                                    else:
                                        self.assertEqual(result["rows"], [[73]])
                                    unchanged()
                                with self.assertRaises(self.Error) as rejected:
                                    self.sql(
                                        self.path,
                                        "INSERT INTO owner_marker VALUES(99)",
                                        _store=store,
                                    )
                                self.assertIs(type(rejected.exception), self.Error)
                                self.assertEqual(
                                    str(rejected.exception),
                                    "read-only SQL rejected an operation or function: owner_marker",
                                )
                                unchanged()
                                bounded = self.sql(
                                    self.path,
                                    "WITH n(x) AS (VALUES(1),(2)) SELECT x FROM n ORDER BY x",
                                    max_rows=1,
                                    _store=store,
                                )
                                self.assertEqual(bounded["rows"], [[1]])
                                self.assertEqual(
                                    bounded["truncation_reason"], "max_rows"
                                )
                                self.assertIs(bounded["complete"], False)
                                unchanged()
                                with self.assertRaises(self.Error) as guarded:
                                    db.execute("INSERT INTO owner_marker VALUES(-1)")
                                self.assertEqual(
                                    str(guarded.exception), "owner marker guard"
                                )
                                db.execute("INSERT INTO owner_marker VALUES(79)")
                            self.assertFalse(store.db.in_transaction)
                            self.assertEqual(
                                store.db.execute(
                                    "SELECT count(*) FROM owner_marker_view"
                                ).fetchone()[0],
                                2,
                            )
                            self.assertEqual(
                                store.db.execute("PRAGMA temp_store").fetchone()[0],
                                temp_store,
                            )

    def test_rolled_back_first_query_setup_is_recreated(self):
        for temp_store in (0, 1, 2):
            with self.subTest(temp_store=temp_store):
                with self.Store(self.path, writable=True) as store:
                    store.db.execute(f"PRAGMA temp_store={temp_store}")
                    with self.assertRaisesRegex(ValueError, "rollback query setup"):
                        with store.transaction() as db:
                            db.execute("CREATE TEMP TABLE rollback_marker(value)")
                            db.execute("INSERT INTO rollback_marker VALUES(73)")
                            self.assertEqual(
                                self.graphql(self.path, "{profile}", _store=store),
                                PROFILE_RESULT,
                            )
                            raise ValueError("rollback query setup")
                    self.assertFalse(store.db.in_transaction)
                    self.assertEqual(
                        store.db.execute(
                            "SELECT count(*) FROM temp.sqlite_schema"
                        ).fetchone()[0],
                        0,
                    )
                    self.assertEqual(
                        self.sql(
                            self.path,
                            "SELECT count(*) FROM catalog_items",
                            _store=store,
                        )["rows"],
                        [[0]],
                    )
                    self.assertEqual(
                        self.graphql(self.path, "{profile}", _store=store),
                        PROFILE_RESULT,
                    )
                    self.assertEqual(
                        store.db.execute("PRAGMA temp_store").fetchone()[0], temp_store
                    )
                    self.assertFalse(store.db.in_transaction)


if __name__ == "__main__":
    unittest.main()
