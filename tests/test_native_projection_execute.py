# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native `projection execute` against frozen Python (track C5).

Same sequential harness as the folder mapping suite: each command runs on
identical catalogs at the same paths, frozen first, then the catalog and the
output contents are restored and native runs.
"""

import shutil
import sqlite3
import unittest
from contextlib import closing

from tests import test_native_publication_mapping_folder as folder

PROJECTION = r"""
from catabolic.store import Store
from catabolic.app import Application
from catabolic.layouts import Layouts
from catabolic.projections import Projections
from catabolic.targets import definitions
import sys
database, query = sys.argv[1:3]
with Store(database, writable=True) as store:
    app = Application(store)
    Layouts(app).put('books', definitions()['komga'])
    Projections(app).put('global', query, 'books')
"""


class NativeProjectionExecuteTest(folder.FolderHarness):
    def setUp(self):
        super().setUp()
        result = self.python(PROJECTION, str(self.db[True]), self.definition["query"])
        self.assertEqual(result.returncode, 0, result.stderr)
        shutil.copyfile(self.db[True], self.db[False])

    def execute(self, *extra):
        return ["--machine", "projection", "execute", "global", *extra]

    def test_execute_publishes_and_repeats_like_frozen(self):
        result = self.assertSame(self.sequential(self.execute()))
        self.assertTrue(result["applied"], result)
        self.assertTrue(result["complete"], result)
        self.assertEqual(result["execution"]["applied_count"], 4)  # prepare + 3
        self.assertEqual(sum(1 for _, target in self.tree() if target), 3)
        result = self.assertSame(self.sequential(self.execute()))
        self.assertEqual(result["execution"]["applied_count"], 0)

    def test_limit_truncates_details_like_frozen(self):
        result = self.assertSame(self.sequential(self.execute("--limit", "1")))
        self.assertTrue(result["execution"]["details_truncated"], result)

    def test_unsafe_plan_and_admission_errors_match_frozen(self):
        # A missing source blocks the plan: nothing commits or publishes.
        (self.source / "book1.epub").unlink()
        result = self.assertSame(self.sequential(self.execute()))
        self.assertFalse(result["applied"], result)
        self.assertEqual(self.tree(), [])
        for args in (
            self.execute("--limit", "0"),
            ["--machine", "projection", "execute", "missing"],
        ):
            with self.subTest(args=args[3:]):
                self.assertSame(self.sequential(args))

    def test_preview_still_rolls_back_like_frozen(self):
        before = self.rows(False, "mappings", "id")
        result = self.assertSame(
            self.sequential(["--machine", "projection", "preview", "global"])
        )
        self.assertFalse(result["layout"]["applied"], result)
        self.assertEqual(self.rows(False, "mappings", "id"), before)
        with closing(sqlite3.connect(self.db[False])) as db:
            self.assertEqual(
                db.execute("SELECT count(*) FROM journal").fetchone(), (0,)
            )


if __name__ == "__main__":
    unittest.main()
