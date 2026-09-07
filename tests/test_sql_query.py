# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import hashlib
import json
import sqlite3
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.domain import CatabolicError
from catabolic.sql_query import execute_sql, render_table
from catabolic.store import Store
from tests import test_queries as fixtures


class SQLQueryTest(unittest.TestCase):
    def setUp(self):
        fixtures.QueryTest.setUp(self)
        self.store.close()

    def sql(self, statement=None, **options):
        return execute_sql(self.path, statement, **options)

    def cli(self, *args, expected=0, stdin=None, json_output=True):
        result = subprocess.run(
            [
                sys.executable,
                "-m",
                "catabolic",
                "--db",
                str(self.path),
                *(["--json"] if json_output else []),
                *args,
            ],
            input=stdin,
            capture_output=True,
            text=True,
            timeout=20,
        )
        self.assertEqual(result.returncode, expected, result.stderr or result.stdout)
        if json_output:
            if expected in (0, 3):
                self.assertEqual(result.stderr, "")
                return json.loads(result.stdout)
            self.assertEqual(result.stdout, "")
            return json.loads(result.stderr)
        return result

    def test_agent_can_discover_view_and_table_columns(self):
        result = self.cli("query", "--schema")
        self.assertEqual(result["interface_version"], 1)
        views = {row["name"]: row for row in result["views"]}
        self.assertEqual(
            set(views),
            {
                "catalog_items",
                "catalog_item_workflow",
                "catalog_workflow_checks",
                "catalog_item_requirements",
                "catalog_item_worklog",
                "catalog_identities",
                "catalog_files",
                "catalog_entries",
                "catalog_item_files",
                "catalog_relationships",
                "catalog_outputs",
                "catalog_hardlinks",
                "catalog_retained_hardlinks",
                "catalog_jobs",
                "catalog_artifacts",
                "catalog_recipes",
                "catalog_rules",
                "catalog_rule_jobs",
                "catalog_renditions",
                "catalog_output_definitions",
                "catalog_generated_locations",
                "catalog_job_attempts",
                "catalog_proposals",
                "catalog_decisions",
                "catalog_expected",
                "catalog_checksums",
                "catalog_text",
                "catalog_refresh",
                "catalog_facts",
                "catalog_tags",
                "catalog_tag_names",
                "catalog_tag_parents",
                "catalog_taggings",
            },
        )
        self.assertIn(
            "mapping_id",
            [column["name"] for column in views["catalog_entries"]["columns"]],
        )
        self.assertIn("items", [row["name"] for row in result["tables"]])
        self.assertIn("profile", result["parameters"])

    def test_aggregate_cte_and_joins_over_documented_views(self):
        result = self.sql("""WITH missing AS (
            SELECT location FROM catalog_files WHERE profile=:profile AND status='missing'
        ) SELECT location,count(*) AS missing_files FROM missing GROUP BY location""")
        self.assertEqual(result["columns"], ["location", "missing_files"])
        self.assertEqual(result["rows"], [["drive-a", 1]])
        result = self.sql(
            """SELECT DISTINCT e.item_id,e.file_id,e.source_path
            FROM catalog_entries e JOIN catalog_identities i ON i.item_id=e.item_id
            WHERE e.profile=:profile AND e.active=1 AND i.namespace=:namespace AND i.value=:identity
            ORDER BY e.file_id""",
            params='{"namespace":"tmdb.movie","identity":"1"}',
        )
        self.assertEqual(result["row_count"], 2)
        self.assertTrue(all(row[0] == "a" for row in result["rows"]))

    def test_views_include_all_profiles_but_parameter_selects_current(self):
        self.assertEqual(self.sql("SELECT count(*) FROM catalog_files")["rows"], [[12]])
        result = self.sql(
            "SELECT DISTINCT profile,status,source_path FROM catalog_files WHERE profile=:profile",
            profile="laptop",
        )
        self.assertEqual(result["rows"], [["laptop", "unknown", None]])
        self.assertEqual(
            self.sql(
                "SELECT count(*) FROM catalog_entries WHERE profile=:profile AND active=0"
            )["rows"],
            [[1]],
        )
        self.assertEqual(
            self.sql(
                "SELECT count(*) FROM catalog_entries WHERE profile=:profile AND active=1"
            )["rows"],
            [[6]],
        )
        self.assertEqual(
            self.sql(
                "SELECT item_id,title,year FROM catalog_items WHERE item_id='orphan'"
            )["rows"],
            [["orphan", None, None]],
        )
        self.assertEqual(
            self.sql("SELECT year FROM catalog_items WHERE item_id='d'")["rows"],
            [[None]],
        )

    def test_count_compound_tag_view_preserves_read_only_authorization(self):
        self.cli("tag", "put", "qa:test")
        self.cli("tag", "add", "qa:test", "--item", "a")
        self.assertEqual(
            self.sql("SELECT count(*) FROM catalog_taggings")["rows"], [[1]]
        )
        for statement in (
            "DELETE FROM item_tags",
            "WITH catalog_taggings AS (SELECT load_extension('untrusted')) SELECT count(*) FROM catalog_taggings",
        ):
            with self.assertRaises(CatabolicError):
                self.sql(statement)

    def test_view_paths_are_derived_without_probing_media(self):
        (self.root / "drive-a").rename(self.root / "offline")
        result = self.sql(
            "SELECT source_path,output_path,status,recorded_link_target FROM catalog_entries WHERE profile=:profile AND item_id='b'"
        )
        self.assertEqual(
            result["rows"],
            [
                [
                    str(self.root / "drive-a/missing.mkv"),
                    str(self.root / "global/Movies/B.mkv"),
                    "missing",
                    None,
                ]
            ],
        )

    def test_named_parameters_are_values_not_sql(self):
        attack = "a' OR 1=1 --"
        result = self.cli(
            "query",
            "SELECT item_id FROM catalog_items WHERE item_id=:item",
            "--params",
            json.dumps({"item": attack}),
        )
        self.assertEqual(result["rows"], [])
        result = self.sql(
            "SELECT :text,:number,:truth,:nothing,:profile",
            params='{"text":"Café","number":1.5,"truth":true,"nothing":null}',
        )
        self.assertEqual(result["rows"], [["Café", 1.5, 1, None, "default"]])
        self.assertEqual(
            self.sql(
                "SELECT item_id FROM catalog_items WHERE instr(casefold(title),casefold(:term))>0 ORDER BY item_id",
                params='{"term":"STRASSE%_CAFÉ"}',
            )["rows"],
            [["a"], ["b"]],
        )

    def test_invalid_bindings_and_limits_are_actionable_errors(self):
        for params in (
            "[]",
            "null",
            '{"profile":"laptop"}',
            '{"x":[]}',
            '{"x":{}}',
            '{"x":NaN}',
            '{"x":1e999}',
            '{"x":9223372036854775808}',
            '{"bad-name":1}',
            "invalid",
        ):
            with self.subTest(params=params), self.assertRaises(CatabolicError):
                self.sql("SELECT :x", params=params)
        for options in (
            {"max_rows": 0},
            {"max_rows": 10001},
            {"timeout_ms": 0},
            {"timeout_ms": 60001},
            {"profile": "absent"},
        ):
            with self.subTest(options=options), self.assertRaises(CatabolicError):
                self.sql("SELECT 1", **options)
        with self.assertRaisesRegex(CatabolicError, "binding"):
            self.sql("SELECT :missing")

    def test_result_arrays_preserve_duplicate_aliases_empty_results_and_types(self):
        result = self.sql(
            "SELECT 1 AS value, 2 AS value, NULL AS empty, x'00ff' AS blob, 1e999 AS huge"
        )
        self.assertEqual(result["columns"], ["value", "value", "empty", "blob", "huge"])
        self.assertEqual(
            result["rows"], [[1, 2, None, {"$blob": "00ff"}, {"$float": "Infinity"}]]
        )
        self.assertEqual(self.sql("SELECT -1e999")["rows"], [[{"$float": "-Infinity"}]])
        empty = self.sql("SELECT item_id,title FROM catalog_items WHERE 0")
        self.assertEqual(empty["columns"], ["item_id", "title"])
        self.assertEqual(empty["rows"], [])
        self.assertTrue(empty["complete"])
        json.dumps(result, allow_nan=False)

    def test_row_truncation_is_explicit_and_sql_limit_is_complete(self):
        result = self.cli(
            "query",
            "SELECT item_id FROM catalog_items ORDER BY item_id",
            "--max-rows",
            "2",
            expected=3,
        )
        self.assertEqual(result["rows"], [["a"], ["b"]])
        self.assertEqual(result["truncation_reason"], "max_rows")
        self.assertFalse(result["complete"])
        result = self.sql(
            "SELECT item_id FROM catalog_items ORDER BY item_id LIMIT 2", max_rows=2
        )
        self.assertTrue(result["complete"])
        self.assertFalse(result["truncated"])
        next_page = self.sql(
            "SELECT item_id FROM catalog_items WHERE item_id>:after ORDER BY item_id LIMIT 2",
            params='{"after":"b"}',
        )
        self.assertEqual(next_page["rows"], [["c"], ["d"]])

    def test_output_byte_budget_and_sqlite_value_limit(self):
        with patch("catabolic.sql_query.MAX_RESULT_BYTES", 150):
            result = self.sql(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100) SELECT printf('%020d',x) AS number FROM n"
            )
        self.assertTrue(result["truncated"])
        self.assertEqual(result["truncation_reason"], "max_result_bytes")
        self.assertLess(result["row_count"], 100)
        with self.assertRaises(CatabolicError):
            self.sql("SELECT zeroblob(2000000)")
        with self.assertRaises(CatabolicError):
            self.sql("SELECT '" + "x" * (1024 * 1024) + "'")

    def test_runaway_recursive_query_times_out_without_partial_output(self):
        result = self.cli(
            "query",
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n) SELECT sum(x) FROM n",
            "--timeout-ms",
            "10",
            expected=2,
        )
        self.assertIn("timed out", result["error"]["message"])
        self.assertEqual(self.sql("SELECT count(*) FROM catalog_items")["rows"], [[5]])

    def test_engine_rejects_writes_ddl_transactions_pragmas_and_external_access(self):
        destination = self.root / "must-not-exist.sqlite3"
        statements = (
            "UPDATE items SET metadata='{}'",
            "DELETE FROM mappings",
            "INSERT INTO profiles VALUES ('attacker')",
            "DROP TABLE items",
            "ALTER TABLE items ADD COLUMN surprise TEXT",
            "CREATE TABLE surprise(x)",
            "CREATE TEMP TABLE surprise(x)",
            "CREATE TEMP VIEW surprise AS SELECT 1",
            "CREATE INDEX surprise ON items(kind)",
            "BEGIN",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT surprise",
            "PRAGMA query_only=OFF",
            "PRAGMA user_version=99",
            "PRAGMA writable_schema=ON",
            "PRAGMA journal_mode=WAL",
            "VACUUM",
            "VACUUM INTO :destination",
            "ATTACH DATABASE :destination AS extra",
            "ATTACH ':memory:' AS extra",
            "DETACH main",
            "SELECT load_extension(:destination)",
            "SELECT writefile(:destination,'oops')",
            "SELECT readfile(:destination)",
            "SELECT fts3_tokenizer('simple')",
            "WITH x AS (SELECT 1) DELETE FROM items RETURNING id",
            "SELECT * FROM pragma_table_info('items')",
        )
        before = self.path.read_bytes()
        for statement in statements:
            with self.subTest(statement=statement), self.assertRaises(CatabolicError):
                self.sql(
                    statement, params=json.dumps({"destination": str(destination)})
                )
        self.assertEqual(self.path.read_bytes(), before)
        self.assertFalse(destination.exists())

    def test_multiple_statements_and_invalid_sql_do_not_execute(self):
        for sql in (
            "SELECT 1; DELETE FROM items;",
            "SELECT 1; SELECT 2;",
            "SELECT FROM",
            "-- comment",
            "",
            "SELECT\x001",
        ):
            with self.subTest(sql=sql), self.assertRaises(CatabolicError):
                self.sql(sql)
        self.assertEqual(
            self.sql(
                "/* leading comment */ SELECT ';not another statement;' AS text; -- trailing"
            )["rows"],
            [[";not another statement;"]],
        )

    def test_query_and_schema_leave_database_and_filesystem_unchanged(self):
        Path(str(self.path) + ".lock").unlink()
        before = hashlib.sha256(self.path.read_bytes()).hexdigest()
        paths = set(self.root.rglob("*"))
        self.sql(describe=True)
        self.sql(
            "SELECT * FROM catalog_entries WHERE profile=:profile ORDER BY mapping_id"
        )
        self.sql("SELECT name FROM sqlite_schema ORDER BY name")
        self.assertEqual(hashlib.sha256(self.path.read_bytes()).hexdigest(), before)
        self.assertEqual(set(self.root.rglob("*")), paths)
        with Store(self.path) as store:
            self.assertEqual(
                store.db.execute("SELECT count(*) FROM sqlite_temp_master").fetchone()[
                    0
                ],
                0,
            )

    def test_query_can_run_while_catabolic_holds_writer_lock(self):
        with Store(self.path, writable=True):
            self.assertEqual(
                self.cli("query", "SELECT count(*) FROM catalog_items")["rows"], [[5]]
            )

    def test_agent_cli_supports_sql_files_stdin_and_json_format(self):
        sql_file = self.root / "report.sql"
        sql_file.write_text(
            "SELECT item_id,title FROM catalog_items WHERE item_id=:item",
            encoding="utf-8",
        )
        result = self.cli("query", "--file", str(sql_file), "--params", '{"item":"a"}')
        self.assertEqual(result["rows"], [["a", "Straße%_Café"]])
        self.assertEqual(
            self.cli("query", "--file", "-", stdin="SELECT :profile")["rows"],
            [["default"]],
        )
        result = self.cli(
            "query", "SELECT 42 AS answer", "--format", "json", json_output=False
        )
        self.assertEqual(json.loads(result.stdout)["rows"], [[42]])
        failed = self.cli(
            "query",
            "DELETE FROM items",
            "--format",
            "json",
            expected=2,
            json_output=False,
        )
        self.assertEqual(failed.stdout, "")
        self.assertIn("error", json.loads(failed.stderr))
        self.assertEqual(
            self.cli("--profile", "laptop", "query", "SELECT :profile")["rows"],
            [["laptop"]],
        )

    def test_agent_cli_reports_input_errors_without_prompts(self):
        for args in (
            ("query",),
            ("query", "SELECT 1", "--schema"),
            ("query", "--schema", "--params", "{}"),
            ("query", "SELECT 1", "--file", "-"),
            ("query", "--file", str(self.root / "absent.sql")),
            ("query", "--file", "-"),
        ):
            with self.subTest(args=args):
                self.assertIn("error", self.cli(*args, expected=2, stdin=""))

    def test_human_table_escapes_control_characters_and_reports_truncation(self):
        result = self.sql("SELECT char(27)||char(10)||'Café' AS text, NULL AS empty")
        table = render_table(result)
        self.assertNotIn("\x1b", table)
        self.assertIn("\\n", table)
        self.assertIn("NULL", table)
        truncated = self.cli(
            "query",
            "SELECT item_id FROM catalog_items",
            "--max-rows",
            "1",
            expected=3,
            json_output=False,
        )
        self.assertIn("truncated", truncated.stdout)
        empty = self.cli("query", "SELECT id FROM items WHERE 0", json_output=False)
        self.assertIn("0 row(s)", empty.stdout)

    def test_main_connection_is_read_only_even_before_authorizer(self):
        from catabolic import sql_query

        original_setup = sql_query._setup

        def check(db):
            with self.assertRaises(sqlite3.OperationalError):
                db.execute("DELETE FROM items")
            original_setup(db)
            self.assertEqual(db.execute("PRAGMA query_only").fetchone()[0], 1)

        with patch("catabolic.sql_query._setup", side_effect=check):
            self.assertEqual(
                self.sql("SELECT count(*) FROM catalog_items")["rows"], [[5]]
            )
