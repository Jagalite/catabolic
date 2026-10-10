# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Run unchanged reference read assertions through the native process adapter.

Python still owns fixture mutations and explicit secondary-profile oracle calls.
CLI presentation, mutation/application and transport tests belong to M4-M10.
"""

import json
import select
import subprocess
import unittest

from catabolic.domain import CatabolicError
from tests import test_queries, test_sql_query
from tests.test_rust_migration import BINARY
from tests.test_rust_queries import NativeQueryTest


class NativeReadAssertions(unittest.TestCase):
    close_worker = NativeQueryTest.close_worker
    ids = test_queries.QueryTest.ids

    def setUp(self):
        test_queries.QueryTest.setUp(self)
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)
        owner = self

        class Queries:
            def __getattr__(self, entity):
                def call(*args, **options):
                    if args:
                        options["item_id"] = args[0]
                    return owner.request(
                        "catalog", entity, "--options", json.dumps(options)
                    )

                return call

        self.query = Queries()
        self.app.files = self.query.files
        if getattr(self, "sql_assertions", False):
            self.store.close()

    def request(self, *arguments):
        self.worker.stdin.write(json.dumps(["--db", str(self.path), *arguments]) + "\n")
        self.worker.stdin.flush()
        self.assertTrue(
            select.select([self.worker.stdout], [], [], 120)[0], "native timeout"
        )
        reply = json.loads(self.worker.stdout.readline())
        if reply["status"]:
            raise CatabolicError(reply["error"])
        return reply["result"]

    def sql(self, statement=None, **options):
        arguments = ["query"]
        if statement is not None:
            arguments.append(statement)
        for key, value in options.items():
            if key == "describe":
                if value:
                    arguments.append("--schema")
            elif key in ("http", "stable"):
                if value:
                    arguments.append("--" + key)
            elif value is not None:
                arguments.extend(["--" + key.replace("_", "-"), str(value)])
        return self.request(*arguments)

    def cli(self, *args, expected=0, **options):
        if args[0] != "query":
            # Python is solely the fixture writer here.
            return test_sql_query.SQLQueryTest.cli(
                self, *args, expected=expected, **options
            )
        try:
            result = self.request(*args)
        except CatabolicError as error:
            self.assertEqual(expected, 2)
            return {"error": {"message": str(error)}}
        self.assertIn(expected, (0, 3))
        return result


class NativeCatalogAssertions(NativeReadAssertions):
    pass


class NativeSQLAssertions(NativeReadAssertions):
    sql_assertions = True

    def test_real_output_byte_budget(self):
        result = self.sql(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100) SELECT printf('%0500000d',x) FROM n"
        )
        self.assertTrue(result["truncated"])
        self.assertEqual(result["truncation_reason"], "max_result_bytes")
        self.assertLess(result["row_count"], 100)
        with self.assertRaises(CatabolicError):
            self.sql("SELECT zeroblob(2000000)")


CATALOG_ASSERTIONS = [
    name
    for name in vars(test_queries.QueryTest)
    if name.startswith("test_")
    and name
    not in {
        "test_public_cli_search_show_filters_and_errors",
        "test_queries_are_database_only_and_create_no_lock_or_backup",
    }
]
SQL_ASSERTIONS = [
    name
    for name in vars(test_sql_query.SQLQueryTest)
    if name.startswith("test_")
    and name
    not in {
        "test_agent_can_discover_view_and_table_columns",
        "test_output_byte_budget_and_sqlite_value_limit",
        "test_agent_cli_supports_sql_files_stdin_and_json_format",
        "test_agent_cli_reports_input_errors_without_prompts",
        "test_main_connection_is_read_only_even_before_authorizer",
        "test_human_table_escapes_control_characters_and_reports_truncation",
    }
]
for name in CATALOG_ASSERTIONS:
    setattr(NativeCatalogAssertions, name, getattr(test_queries.QueryTest, name))
for name in SQL_ASSERTIONS:
    setattr(NativeSQLAssertions, name, getattr(test_sql_query.SQLQueryTest, name))
