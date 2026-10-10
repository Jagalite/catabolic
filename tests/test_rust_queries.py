# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Differential checks for native query values, cursors and disclosure bounds."""

import json
import select
import subprocess
import unittest

from catabolic.app import Application
from catabolic.sql_query import VIEWS, execute_sql
from catabolic.store import Store
from tests import test_queries as fixtures
from tests.test_rust_migration import BINARY


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeQueryTest(unittest.TestCase):
    def setUp(self):
        fixtures.QueryTest.setUp(self)
        self.store.close()
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)

    def close_worker(self):
        self.worker.stdin.close()
        try:
            self.worker.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.worker.kill()
            self.worker.wait()
        self.worker.stdout.close()
        self.worker.stderr.close()

    def native(self, command, *args, ok=True, profile="default"):
        self.worker.stdin.write(
            json.dumps(
                ["--db", str(self.path), "--profile", profile, command, *map(str, args)]
            )
            + "\n"
        )
        self.worker.stdin.flush()
        self.assertTrue(
            select.select([self.worker.stdout], [], [], 120)[0],
            "native request timed out",
        )
        line = self.worker.stdout.readline()
        self.assertTrue(
            line,
            "native worker terminated: "
            + (self.worker.stderr.read() if self.worker.poll() is not None else ""),
        )
        reply = json.loads(line)
        self.assertEqual(reply["status"], 0 if ok else 2, reply)
        return reply["result"] if ok else reply["error"]

    def test_all_catalog_views_and_column_contracts(self):
        before = self.path.read_bytes()
        for view in VIEWS:
            with self.subTest(view=view):
                sql = f"SELECT * FROM {view} LIMIT 100"
                self.assertEqual(self.native("query", sql), execute_sql(self.path, sql))
        native = self.native("query", "--schema")
        reference = execute_sql(self.path, describe=True)
        # SQLite's build/version is reported, not rewritten to resemble Python.
        native.pop("sqlite_version")
        reference.pop("sqlite_version")
        self.assertEqual(native, reference)
        self.assertEqual(before, self.path.read_bytes())

    def test_sql_types_aliases_parameters_and_row_budget(self):
        for sql, params, limit in [
            ("SELECT 1 AS x,2 AS x,NULL AS nil,X'00ff' AS b", None, 1000),
            (
                "SELECT casefold('StraßeCAFÉ'),catalog_year('{\"year\":true}'),catalog_title('{\"title\":4}')",
                None,
                1000,
            ),
            (
                "SELECT :x,:profile,:yes,:nil",
                '{"x":9223372036854775807,"yes":true,"nil":null}',
                1000,
            ),
            ("SELECT id FROM items ORDER BY id", None, 2),
            ("SELECT -0.0,1e999", None, 1000),
        ]:
            with self.subTest(sql=sql):
                arguments = [sql, "--max-rows", str(limit)]
                if params is not None:
                    arguments.extend(["--params", params])
                self.assertEqual(
                    self.native("query", *arguments),
                    execute_sql(self.path, sql, params=params, max_rows=limit),
                )

    def test_sql_attacks_and_http_private_tables(self):
        before = self.path.read_bytes()
        for sql in [
            "DELETE FROM items",
            "CREATE TABLE nope(x)",
            "ATTACH ':memory:' AS attached",
            "PRAGMA writable_schema=ON",
            "SELECT load_extension('bad')",
            "BEGIN",
        ]:
            with self.subTest(sql=sql):
                self.assertIn(
                    "read-only SQL rejected", self.native("query", sql, ok=False)
                )
        for table in [
            "api_tokens",
            "execution_claims",
            "watchers",
            "schema_migrations",
            "sqlite_schema",
        ]:
            with self.subTest(table=table):
                # schema_migrations is not private in the reference operator SQL surface.
                if table == "schema_migrations":
                    continue
                self.assertIn(
                    "read-only SQL rejected",
                    self.native("query", f"SELECT * FROM {table}", "--http", ok=False),
                )
        self.assertEqual(before, self.path.read_bytes())
        self.assertEqual(
            self.native("query", "SELECT 9223372036854775807", "--http"),
            execute_sql(self.path, "SELECT 9223372036854775807", _http=True),
        )
        self.assertIn(
            "read-only SQL rejected",
            self.native("query", "SELECT random()", "--stable", ok=False),
        )
        self.assertIn(
            "timed out",
            self.native(
                "query",
                "WITH RECURSIVE x(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM x) SELECT sum(n) FROM x",
                "--timeout-ms",
                "1",
                ok=False,
            ),
        )

    def test_catalog_filters_and_cross_runtime_cursors(self):
        with Store(self.path) as store:
            query = Application(store).queries
            cases = {
                "items": [
                    {},
                    {"search": "strasse%_café", "kind": "movie", "year": 2021},
                    {"metadata": ["reviewed=true"]},
                    {"metadata": ["reviewed=1"]},
                    {"sort": "title", "descending": True, "limit": 1},
                    {"identity": "imdb=tt1"},
                ],
                "files": [
                    {},
                    {"status": "missing"},
                    {"unmapped": True, "catalog": "favorites"},
                    {"unidentified": True},
                    {"kind": "movie", "identity": "tmdb.movie=1"},
                    {"sort": "size", "limit": 2},
                ],
                "mappings": [
                    {},
                    {"catalog": None, "active": "all"},
                    {"item": "a", "active": "active", "limit": 1},
                ],
                "associations": [{}, {"item": "a", "limit": 1}],
                "relationships": [{}],
                "item": [
                    {"item_id": "a"},
                    {"identity": "tmdb.movie=1", "include_disabled": True},
                ],
            }
            for entity, variants in cases.items():
                for options in variants:
                    with self.subTest(entity=entity, options=options):
                        expected = getattr(query, entity)(**options)
                        actual = self.native(
                            "catalog", entity, "--options", json.dumps(options)
                        )
                        self.assertEqual(actual, expected)
                        if entity != "item" and expected["next_cursor"]:
                            options = dict(options, cursor=expected["next_cursor"])
                            self.assertEqual(
                                self.native(
                                    "catalog", entity, "--options", json.dumps(options)
                                ),
                                getattr(query, entity)(**options),
                            )

    def test_sql_error_messages_match_reference(self):
        from catabolic.domain import CatabolicError

        for sql in (
            "SELECT FROM",
            "SELECT 1; SELECT 2",
            "SELECT nonexistent FROM items",
            "SELECT\x001",
            "-- comment",
            "SELECT :x",
            "SELECT ?",
        ):
            with self.subTest(sql=sql):
                with self.assertRaises(CatabolicError) as error:
                    execute_sql(self.path, sql)
                actual = self.native("query", sql, ok=False)
                self.assertEqual(
                    actual.removeprefix("catabolic: "), str(error.exception)
                )

    def test_item_decoration_crosses_batch_boundaries(self):
        from catabolic.store import encode

        with Store(self.path, writable=True) as store, store.transaction() as db:
            records = [
                (
                    f"bulk-{i:04d}",
                    "movie",
                    encode({"title": f"Bulk {i:04d}", "year": 2000 + i % 25}),
                )
                for i in range(620)
            ]
            db.executemany("INSERT INTO items(id,kind,metadata) VALUES(?,?,?)", records)
            db.executemany(
                "INSERT INTO identities(item_id,namespace,value) VALUES(?,?,?)",
                [
                    (identifier, namespace, identifier)
                    for identifier, _, _ in records
                    for namespace in ("fixture.a", "fixture.b")
                ],
            )
        before = self.path.read_bytes()
        with Store(self.path) as store:
            expected = Application(store).queries.items(limit=1000, sort="title")
        self.assertEqual(
            self.native(
                "catalog",
                "items",
                "--options",
                json.dumps({"limit": 1000, "sort": "title"}),
            ),
            expected,
        )
        self.assertEqual(before, self.path.read_bytes())
