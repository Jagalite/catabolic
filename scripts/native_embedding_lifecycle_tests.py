#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent installed-wheel regressions, using disposable catalogs only."""

import os
import signal
import tempfile
import threading
import time
import unittest
from pathlib import Path

from catabolic_native import Application, CatabolicError, Store

LONG_INSERT = "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<100000000) INSERT INTO review_rows SELECT x FROM n"


class ReviewLifecycle(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        path = Path(temporary.name) / "catalog.db"
        Store.initialize(path)
        self.store = Store(path, writable=True)
        self.addCleanup(self.store.close)
        self.store.db.execute("CREATE TABLE review_rows(n)")

    def interrupt(self):
        thread = threading.Thread(
            target=lambda: (time.sleep(0.1), os.kill(os.getpid(), signal.SIGINT))
        )
        thread.start()
        self.addCleanup(thread.join, 5)

    def test_write_sigint_preserves_keyboard_interrupt(self):
        self.interrupt()
        with self.assertRaises(KeyboardInterrupt):
            with self.store.transaction() as db:
                db.execute(LONG_INSERT)
        self.assertFalse(self.store.db.in_transaction)
        self.assertEqual(self.store.rows("SELECT * FROM review_rows"), [])

    def test_nested_write_sigint_preserves_keyboard_interrupt(self):
        self.interrupt()
        with self.assertRaises(KeyboardInterrupt):
            with self.store.transaction():
                with self.store.transaction() as inner:
                    inner.execute(LONG_INSERT)
        self.assertFalse(self.store.db.in_transaction)
        self.assertEqual(self.store.rows("SELECT * FROM review_rows"), [])

    def test_nested_read_sigint_rolls_back_only_inner_scope(self):
        with self.store.transaction() as outer:
            outer.execute("INSERT INTO review_rows VALUES (1)")
            self.interrupt()
            with self.assertRaises(KeyboardInterrupt):
                with self.store.transaction() as inner:
                    inner.execute("INSERT INTO review_rows VALUES (2)")
                    inner.execute(
                        "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n"
                    )
            self.assertTrue(outer.in_transaction)
            outer.execute("INSERT INTO review_rows VALUES (3)")
        self.assertEqual(
            self.store.rows("SELECT * FROM review_rows ORDER BY n"),
            [{"n": 1}, {"n": 3}],
        )

    def test_caught_sqlite_rollback_retires_handle(self):
        with self.assertRaisesRegex(ValueError, "outer failure"):
            with self.store.transaction() as db:
                with self.assertRaises(CatabolicError):
                    db.execute("INSERT OR ROLLBACK INTO profiles VALUES ('default')")
                with self.assertRaises(CatabolicError):
                    db.execute("INSERT INTO review_rows VALUES (123)")
                raise ValueError("outer failure")
        self.assertEqual(self.store.rows("SELECT * FROM review_rows"), [])

    def test_caught_sqlite_rollback_blocks_service_write(self):
        app = Application(self.store)
        with self.assertRaisesRegex(ValueError, "outer failure"):
            with self.store.transaction() as db:
                with self.assertRaises(CatabolicError):
                    db.execute("INSERT OR ROLLBACK INTO profiles VALUES ('default')")
                with self.assertRaises(CatabolicError):
                    app.add_profile("should-not-commit")
                raise ValueError("outer failure")
        self.assertEqual(
            self.store.rows("SELECT id FROM profiles WHERE id='should-not-commit'"), []
        )

    def test_swallowed_nested_rollback_cannot_commit_outer(self):
        with self.assertRaises(CatabolicError):
            with self.store.transaction() as outer:
                try:
                    with self.store.transaction() as inner:
                        inner.execute(
                            "INSERT OR ROLLBACK INTO profiles VALUES ('default')"
                        )
                except CatabolicError:
                    pass
                with self.assertRaises(CatabolicError):
                    outer.execute("INSERT INTO review_rows VALUES (456)")
        self.assertEqual(self.store.rows("SELECT * FROM review_rows"), [])

    def test_ordinary_statement_error_does_not_retire_valid_scope(self):
        with self.store.transaction() as db:
            with self.assertRaises(CatabolicError):
                db.execute("INSERT INTO profiles VALUES ('default')")
            db.execute("INSERT INTO review_rows VALUES (789)")
        self.assertEqual(self.store.rows("SELECT * FROM review_rows"), [{"n": 789}])


if __name__ == "__main__":
    unittest.main(verbosity=2)
