# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import json
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.layouts import PRESETS, Layouts, validate_layout
from catabolic.reconcile import Reconciler
from catabolic.store import Store
from tests.test_media_model import populate_media


def sql_selection(sql, **kwargs):
    return {"language": "sql", "query": sql, **kwargs}


GRAPHQL_TRACKS = """query Tracks($after: String, $kind: String!) {
  chosen: items(kind: $kind, first: 1, after: $after) {
    nodes { id }
    pageInfo { hasNextPage endCursor }
  }
}"""


class QueryLayoutTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.files, _ = populate_media(self.app, self.root)
        (self.root / "query").mkdir()
        self.app.bind("output", "query", str(self.root / "query"))
        self.layouts = Layouts(self.app)

    def put(self, selection):
        definition = copy.deepcopy(PRESETS["flat"])
        definition["selection"] = selection
        return self.layouts.put("query", definition)

    def plan(self, **kwargs):
        return self.layouts.run("query", "query", **kwargs)

    def state(self):
        return (
            self.store.rows("SELECT * FROM mappings ORDER BY id"),
            self.store.rows("SELECT * FROM meta ORDER BY key"),
        )

    def test_sql_items_preview_apply_refresh_and_manual_preservation(self):
        manual = self.app.put_mapping(
            "query", self.files["book.epub"], "edition", "Manual/Book.epub"
        )
        self.put(
            sql_selection(
                "SELECT item_id FROM catalog_items WHERE kind=:kind AND title LIKE :title",
                params={"kind": "track", "title": "%"},
            )
        )
        before = self.path.read_bytes()
        with Store(self.path) as reader:
            plan = Layouts(Application(reader)).run("query", "query")
        self.assertEqual(before, self.path.read_bytes())
        self.assertEqual(plan["desired_count"], 2)
        self.assertEqual(list((self.root / "query").iterdir()), [])
        self.assertTrue(self.plan(apply=True)["applied"])
        self.assertTrue(Reconciler(self.app).apply("query")["healthy"])
        old = self.root / "query/track/track1/track01.flac"
        stable = self.root / "query/track/track2/track02.flac"
        stable_inode = stable.lstat().st_ino
        self.put(
            sql_selection(
                "SELECT item_id FROM catalog_items WHERE kind=:kind AND title LIKE :title",
                params={"kind": "track", "title": "track2"},
            )
        )
        update = self.plan(apply=True)
        self.assertEqual(update["change_count"], 1)
        self.assertEqual(update["changes"][0]["action"], "disable")
        self.assertTrue(Reconciler(self.app).apply("query")["healthy"])
        self.assertFalse(old.exists())
        self.assertEqual(stable_inode, stable.lstat().st_ino)
        self.assertEqual(
            self.store.rows("SELECT active FROM mappings WHERE id=?", (manual["id"],))[
                0
            ]["active"],
            1,
        )
        self.assertTrue((self.root / "media/track01.flac").is_file())
        self.put(sql_selection("SELECT item_id FROM catalog_items WHERE kind='track'"))
        self.assertEqual(self.plan(apply=True)["changes"][0]["action"], "enable")
        self.assertEqual(self.plan(apply=True)["change_count"], 0)

    def test_empty_requires_explicit_opt_in_and_keeps_manual(self):
        self.put(sql_selection("SELECT item_id FROM catalog_items WHERE kind='track'"))
        self.plan(apply=True)
        manual = self.app.put_mapping(
            "query", self.files["book.epub"], "edition", "Manual/Book.epub"
        )
        self.put(sql_selection("SELECT item_id FROM catalog_items WHERE 0"))
        before = self.state()
        self.assertFalse(self.plan(apply=True)["safe"])
        self.assertEqual(before, self.state())
        cleared = self.plan(apply=True, allow_empty=True)
        self.assertTrue(cleared["applied"])
        self.assertEqual(cleared["change_count"], 2)
        self.assertEqual(
            self.store.rows(
                "SELECT id FROM mappings WHERE catalog='query' AND active=1"
            ),
            [{"id": manual["id"]}],
        )

    def test_sql_ids_duplicates_and_association_role_precision(self):
        for query, expected in (
            (
                "SELECT item_id FROM catalog_items WHERE item_id='movie' UNION ALL SELECT 'movie'",
                2,
            ),
            (
                "SELECT file_id FROM catalog_files WHERE profile=:profile AND source_relative_path='movie.mkv'",
                1,
            ),
            (
                "SELECT association_id FROM catalog_item_files WHERE profile=:profile AND role='subtitle' AND active=1",
                1,
            ),
        ):
            self.put(sql_selection(query))
            self.assertEqual(self.plan()["desired_count"], expected)
        self.assertEqual(self.plan()["changes"][0]["file_id"], self.files["movie.srt"])

    def test_query_errors_unknown_ids_and_truncation_preserve_state(self):
        self.put(sql_selection("SELECT item_id FROM catalog_items WHERE kind='track'"))
        self.plan(apply=True)
        for query in (
            "DELETE FROM mappings RETURNING item_id",
            "PRAGMA user_version",
            "SELECT load_extension('bad') AS item_id",
            "SELECT 'absent' AS item_id",
            "SELECT NULL AS file_id",
            "SELECT 7 AS item_id",
            "SELECT item_id,kind FROM catalog_items",
            "SELECT item_id AS wrong FROM catalog_items",
            "SELECT item_id FROM catalog_items; DELETE FROM mappings",
            "not SQL",
        ):
            with self.subTest(query=query):
                self.put(sql_selection(query))
                before = self.state()
                with self.assertRaises(CatabolicError):
                    self.plan(apply=True, allow_empty=True)
                self.assertEqual(before, self.state())
                self.assertFalse(
                    self.store.db.execute("PRAGMA query_only").fetchone()[0]
                )
        self.put(sql_selection("SELECT item_id FROM catalog_items WHERE kind='track'"))
        before = self.state()
        with (
            patch("catabolic.selection.MAX_SELECTION_IDS", 1),
            self.assertRaisesRegex(CatabolicError, "truncated"),
        ):
            self.plan(apply=True, allow_empty=True)
        self.assertEqual(before, self.state())
        self.assertEqual(self.plan(apply=True)["change_count"], 0)

    def test_disabled_associations_refused(self):
        association = self.app.media.associate(
            self.files["notes.txt"], "movie", role="extra"
        )
        self.app.media.disable_association(association["id"])
        self.put(
            sql_selection(
                "SELECT :id AS association_id", params={"id": association["id"]}
            )
        )
        with self.assertRaisesRegex(CatabolicError, "disabled"):
            self.plan(apply=True)

    def test_sql_timeout_preserves_mappings_and_restores_connection(self):
        self.put(sql_selection("SELECT item_id FROM catalog_items WHERE kind='track'"))
        self.plan(apply=True)
        self.put(
            sql_selection(
                "WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM numbers) SELECT CAST(sum(n) AS TEXT) AS item_id FROM numbers",
                timeout_ms=1,
            )
        )
        before = self.state()
        limit = self.store.db.getlimit(sqlite3.SQLITE_LIMIT_LENGTH)
        with self.assertRaisesRegex(CatabolicError, "timed out"):
            self.plan(apply=True, allow_empty=True)
        self.assertEqual(before, self.state())
        self.assertEqual(limit, self.store.db.getlimit(sqlite3.SQLITE_LIMIT_LENGTH))
        self.assertEqual(self.store.db.execute("PRAGMA query_only").fetchone()[0], 0)
        self.put(sql_selection("SELECT item_id FROM catalog_items WHERE kind='track'"))
        self.assertEqual(self.plan(apply=True)["change_count"], 0)

    def test_graphql_follows_every_page_and_filters_files_and_associations(self):
        self.put(
            {
                "language": "graphql",
                "query": GRAPHQL_TRACKS,
                "variables": {"kind": "track"},
            }
        )
        result = self.plan(apply=True)
        self.assertTrue(result["applied"])
        self.assertEqual(result["desired_count"], 2)
        self.assertEqual(result["selection"]["pages"], 2)
        self.assertEqual(result["selection"]["selected_ids"], 2)
        self.assertEqual(self.plan(apply=True)["change_count"], 0)
        for collection, filters, expected in (
            ("files", 'search: "movie.mkv"', 1),
            ("associations", 'role: "subtitle"', 1),
        ):
            self.put(
                {
                    "language": "graphql",
                    "query": "query($after: String) { "
                    + collection
                    + "("
                    + filters
                    + ", first: 1, after: $after) { nodes { id } pageInfo { hasNextPage endCursor } } }",
                }
            )
            self.assertEqual(self.plan()["desired_count"], expected)

    def test_graphql_errors_and_incomplete_pages_never_clear(self):
        self.put(
            {
                "language": "graphql",
                "query": GRAPHQL_TRACKS,
                "variables": {"kind": "track"},
            }
        )
        self.plan(apply=True)
        before = self.state()
        with (
            patch("catabolic.selection.MAX_SELECTION_IDS", 1),
            self.assertRaisesRegex(CatabolicError, "limit"),
        ):
            self.plan(apply=True)
        self.assertEqual(before, self.state())
        for response in (
            {
                "data": {"chosen": {"nodes": []}},
                "errors": [{"message": "failed later page"}],
            },
            {
                "data": {
                    "chosen": {
                        "nodes": [],
                        "pageInfo": {"hasNextPage": True, "endCursor": "same"},
                    }
                }
            },
            {
                "data": {
                    "chosen": {
                        "nodes": [{"id": "track1"}],
                        "pageInfo": {"hasNextPage": True, "endCursor": "same"},
                    }
                }
            },
        ):
            with (
                patch("catabolic.graphql_query.execute_graphql", return_value=response),
                self.assertRaises(CatabolicError),
            ):
                self.plan(apply=True, allow_empty=True)
            self.assertEqual(before, self.state())

    def test_profile_is_saved_not_inherited_from_refresh_machine(self):
        self.app.add_profile("offline")
        self.put(
            sql_selection(
                "SELECT file_id FROM catalog_files WHERE profile=:profile AND status='present'"
            )
        )
        offline = Layouts(Application(self.store, "offline")).run("query", "query")
        self.assertEqual(offline["selection"]["profile"], "default")
        self.assertEqual(offline["desired_count"], 14)
        self.put(
            sql_selection(
                "SELECT file_id FROM catalog_files WHERE profile=:profile AND status='present'",
                profile="offline",
            )
        )
        self.assertEqual(self.plan()["desired_count"], 0)

    def test_readonly_query_uses_planner_snapshot_during_concurrent_change(self):
        self.store.db.execute("PRAGMA journal_mode=WAL")
        for language in ("sql", "graphql"):
            with self.subTest(language=language):
                self.app.put_item("track", {}, {"title": "Before"}, "track1")
                self.put(
                    sql_selection(
                        "SELECT item_id FROM catalog_items WHERE title='Before'"
                    )
                    if language == "sql"
                    else {
                        "language": "graphql",
                        "query": 'query($after: String) { items(search: "Before", after: $after) { nodes { id } pageInfo { hasNextPage endCursor } } }',
                    }
                )
                with Store(self.path) as reader:
                    app = Application(reader)
                    self.app.put_item("track", {}, {"title": "After"}, "track1")
                    result = Layouts(app).run("query", "query")
                    self.assertEqual(result["desired_count"], 1)
                self.assertEqual(self.plan()["desired_count"], 0)

    def test_selector_write_failure_rolls_back_mapping_changes(self):
        self.put(sql_selection("SELECT item_id FROM catalog_items WHERE kind='track'"))
        before = self.state()
        with self.store.transaction() as db:
            db.execute(
                "CREATE TRIGGER fail_selection BEFORE INSERT ON mappings BEGIN SELECT RAISE(ABORT, 'injected'); END"
            )
        with self.assertRaisesRegex(sqlite3.IntegrityError, "injected"):
            self.plan(apply=True)
        self.assertEqual(before, self.state())

    def test_invalid_selection_contract(self):
        for selection in (
            {"language": "python", "query": "x"},
            sql_selection("SELECT 1", params={"profile": "other"}),
            sql_selection("SELECT 1", timeout_ms=0),
            {"language": "graphql", "query": "{ items { nodes { id } } }"},
            {
                "language": "graphql",
                "query": GRAPHQL_TRACKS,
                "variables": {"after": "partial"},
            },
            {
                "language": "graphql",
                "query": GRAPHQL_TRACKS.replace("nodes { id }", "nodes { id: title }"),
            },
        ):
            with self.subTest(selection=selection), self.assertRaises(CatabolicError):
                validate_layout({**PRESETS["flat"], "selection": selection})

    def test_cli_saves_queries_and_refreshes(self):
        sql = self.root / "selection.sql"
        sql.write_text("SELECT item_id FROM catalog_items WHERE kind=:kind")
        gql = self.root / "selection.graphql"
        gql.write_text(GRAPHQL_TRACKS)
        prefix = [
            sys.executable,
            "-m",
            "catabolic",
            "--db",
            str(self.path),
            "--json",
            "layout",
        ]
        self.store.close()
        for args in (
            [
                "put",
                "query",
                "--preset",
                "flat",
                "--select-sql",
                str(sql),
                "--params",
                '{"kind":"track"}',
            ],
            ["preview", "query", "--catalog", "query"],
            ["apply", "query", "--catalog", "query"],
            [
                "put",
                "query",
                "--preset",
                "flat",
                "--select-graphql",
                str(gql),
                "--variables",
                '{"kind":"track"}',
            ],
            ["apply", "query", "--catalog", "query"],
        ):
            result = subprocess.run([*prefix, *args], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIsInstance(json.loads(result.stdout), dict)
