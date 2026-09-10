# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Legacy boundaries that opt-in fallback integration must preserve or adapt."""

import copy
import os
import tempfile
import unittest
from pathlib import Path

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.layouts import PRESETS, Layouts
from catabolic.projections import Projections
from catabolic.reconcile import Reconciler
from catabolic.saved_queries import Queries
from catabolic.store import Store


class FallbackBaselineTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "main.bin").write_bytes(b"original media")
        (self.source / "backup.bin").write_bytes(b"original media")
        self.output = self.root / "output"
        self.output.mkdir()
        self.database = self.root / "catalog.db"
        Store.initialize(self.database)
        self.store = Store(self.database, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.app.bind("source", "source", str(self.source))
        self.app.bind("output", "global", str(self.output))
        self.app.scan()
        self.files = {
            row["path"]: row["id"] for row in self.store.rows("SELECT * FROM files")
        }
        self.item = self.app.put_item(
            "movie", {"fixture": "fallback"}, {"title": "Film"}
        )["id"]
        self.app.media.associate(self.files["main.bin"], self.item)
        self.app.media.associate(self.files["backup.bin"], self.item)

    def project_exact(self):
        query = Queries(self.store).put(
            "exact",
            {
                "mode": "selection",
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS file_id FROM files WHERE path='main.bin'",
                },
            },
        )
        Layouts(self.app).put("flat", copy.deepcopy(PRESETS["flat"]))
        Projections(self.app).put("global", query["id"], "flat")
        result = Projections(self.app).run("global", apply=True)
        self.assertTrue(result["complete"], result)
        return query

    def test_exact_file_membership_does_not_expand_to_backup(self):
        query = self.project_exact()
        entity, ids, report = Queries(self.store).select(query["id"])
        self.assertEqual(entity, "file_id")
        self.assertEqual(ids, {self.files["main.bin"]})
        self.assertTrue(report["complete"])
        self.assertEqual(
            [
                r["file_id"]
                for r in self.store.rows("SELECT * FROM mappings WHERE active=1")
            ],
            [self.files["main.bin"]],
        )

    def test_legacy_mapping_identity_depends_on_file_not_profile(self):
        main = self.app.mapping_id(
            "global", self.files["main.bin"], self.item, "Film.bin"
        )
        backup = self.app.mapping_id(
            "global", self.files["backup.bin"], self.item, "Film.bin"
        )
        self.assertNotEqual(main, backup)
        self.app.profile = "another-machine"
        self.assertEqual(
            main,
            self.app.mapping_id(
                "global", self.files["main.bin"], self.item, "Film.bin"
            ),
        )

    def test_blocked_legacy_delta_requires_projection_retention_gate(self):
        self.project_exact()
        before = self.store.rows("SELECT * FROM mappings ORDER BY id")
        published = next(path for path in self.output.rglob("*") if path.is_symlink())
        previous = os.readlink(published)
        (self.source / "main.bin").unlink()
        self.app.scan()
        plan = Reconciler(self.app).preview("global", max_removals=1)
        kinds = {action["kind"] for action in plan["actions"]}
        self.assertIn("blocked_source", kinds)
        self.assertIn("remove", kinds)
        projection = Projections(self.app).run("global", apply=True, max_removals=1)
        self.assertFalse(projection["applied"])
        self.assertFalse(projection["safe"])
        self.assertEqual(os.readlink(published), previous)
        self.assertEqual(self.store.rows("SELECT * FROM mappings ORDER BY id"), before)

    def test_slow_work_cannot_detach_an_open_projection_transaction(self):
        with self.store.transaction():
            with self.assertRaisesRegex(CatabolicError, "active transaction"):
                with self.store.detached():
                    self.fail("detached an open transaction")
        with self.store.detached():
            with Store(self.database, writable=True) as independent:
                self.assertIsNotNone(independent.lock_fd)
        self.assertIsNotNone(self.store.lock_fd)
