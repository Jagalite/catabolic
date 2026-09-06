import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from graphql import get_introspection_query

from catabolic.app import Application
from catabolic.cli import main
from catabolic.domain import CatabolicError
from catabolic.graphql_query import execute_graphql
from catabolic.migration import SCHEMA_VERSION
from catabolic.store import Store
from tests.test_media_model import populate_media


class GraphQLTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        with Store(self.path, writable=True) as store:
            self.app = Application(store)
            self.files, _ = populate_media(self.app, self.root)
            self.app.add_profile("offline")

    def query(self, document, **kwargs):
        return execute_graphql(self.path, document, **kwargs)

    def test_nested_query_variables_fragments_aliases_and_readonly(self):
        before = self.path.read_bytes()
        result = self.query(
            """query Album($id: ID!) {
          album: item(id: $id) { ...Core metadata
            relationships(direction: INCOMING, kind: "part_of") {
              nodes { position source { ...Core associations { nodes { role file { path sourcePath size mtimeNs status } } } } }
              pageInfo { hasNextPage endCursor }
            }
          }
        } fragment Core on Item { id kind title identities { namespace value } }""",
            variables={"id": "album"},
        )
        self.assertNotIn("errors", result, result)
        rows = result["data"]["album"]["relationships"]["nodes"]
        self.assertEqual([row["position"] for row in rows], [1, 2])
        file = rows[0]["source"]["associations"]["nodes"][0]["file"]
        self.assertEqual(file["path"], "track01.flac")
        self.assertEqual(file["status"], "PRESENT")
        self.assertIsInstance(file["size"], str)
        self.assertGreater(int(file["mtimeNs"]), 2**53)
        self.assertEqual(self.path.read_bytes(), before)

    def test_pages_cursors_and_filters(self):
        document = """query($after: String) { items(first: 1, kind: "track", sort: TITLE, after: $after) { nodes { id } pageInfo { endCursor hasNextPage } } }"""
        first = self.query(document)["data"]["items"]
        self.assertEqual(first["nodes"], [{"id": "track1"}])
        self.assertTrue(first["pageInfo"]["hasNextPage"])
        second = self.query(
            document, variables={"after": first["pageInfo"]["endCursor"]}
        )["data"]["items"]
        self.assertEqual(second["nodes"], [{"id": "track2"}])
        self.assertFalse(second["pageInfo"]["hasNextPage"])
        changed = self.query(
            document.replace('"track"', '"photo"'),
            variables={"after": first["pageInfo"]["endCursor"]},
        )
        self.assertIn("errors", changed)
        filtered = self.query(
            '{ items(metadata: ["title=\\"album\\""]) { nodes { id } } }'
        )
        self.assertEqual(filtered["data"]["items"]["nodes"], [{"id": "album"}])

    def test_profile_availability_and_catalog_projection(self):
        document = "{ profile files(first: 1) { nodes { id status sourcePath size associations { nodes { item { id } } } } } catalogs { nodes { id root mappings(first: 1) { nodes { outputPath item { id } file { path } } } } } }"
        result = self.query(document, profile="offline")
        self.assertNotIn("errors", result, result)
        self.assertEqual(result["data"]["profile"], "offline")
        file = result["data"]["files"]["nodes"][0]
        self.assertEqual(file["status"], "UNKNOWN")
        self.assertIsNone(file["size"])
        self.assertIsNone(file["sourcePath"])
        self.assertIsNone(result["data"]["catalogs"]["nodes"][0]["root"])
        with self.assertRaisesRegex(CatabolicError, "profile"):
            self.query("{ profile }", profile="nope")

    def test_schema_introspection_and_operation_selection(self):
        result = self.query(get_introspection_query())
        self.assertNotIn("errors", result, result)
        self.assertIsNone(result["data"]["__schema"]["mutationType"])
        self.assertIsNone(result["data"]["__schema"]["subscriptionType"])
        with patch("catabolic.graphql_query.MAX_FIELDS", 2):
            limited = self.query(get_introspection_query())
            self.assertEqual(
                limited["errors"][0]["extensions"]["code"], "LIMIT_EXCEEDED"
            )
        # Limits and resolver contexts must not leak through shared schema types.
        self.assertNotIn("errors", self.query(get_introspection_query()))
        self.assertIn(
            "errors", self.query("query A { profile } query B { schemaVersion }")
        )
        self.assertEqual(
            self.query(
                "query A { profile } query B { schemaVersion }", operation_name="B"
            )["data"],
            {"schemaVersion": SCHEMA_VERSION},
        )

    def test_rejects_writes_invalid_fields_variables_and_cursors(self):
        before = self.path.read_bytes()
        for document, variables in (
            ('mutation { deleteItem(id: "movie") }', None),
            ("subscription { items { nodes { id } } }", None),
            ("{ nonexistent }", None),
            ("query($n: Int!) { items(first: $n) { nodes { id } } }", {"n": "5"}),
            ("{ items(first: 1001) { nodes { id } } }", None),
            ('{ files(after: "bogus") { nodes { id } } }', None),
            ("{ item { id } }", None),
            ('{ item(id: "movie", identity: "x=y") { id } }', None),
            ("{ ...A } fragment A on Query { ...A }", None),
        ):
            with self.subTest(document=document):
                self.assertIn("errors", self.query(document, variables=variables))
        self.assertEqual(before, self.path.read_bytes())
        self.assertIsNone(self.query('{ item(id: "absent") { id } }')["data"]["item"])

    def test_query_limits(self):
        nested = "id"
        for _ in range(12):
            nested = "relationships { nodes { source { " + nested + " } } }"
        self.assertIn("errors", self.query('{ item(id: "movie") { ' + nested + " } }"))
        for budget, value, document in (
            ("MAX_NODES", 1, "{ items { nodes { id } } }"),
            ("MAX_FIELDS", 2, "{ items { nodes { id kind } } }"),
            ("MAX_RESULT_BYTES", 20, "{ items { nodes { metadata } } }"),
        ):
            with (
                self.subTest(budget=budget),
                patch("catabolic.graphql_query." + budget, value),
            ):
                result = self.query(document)
                self.assertIsNone(result["data"])
                self.assertEqual(
                    result["errors"][0]["extensions"]["code"], "LIMIT_EXCEEDED"
                )
        with patch("catabolic.graphql_query.time.monotonic", side_effect=[0, 1]):
            self.assertIn("errors", self.query("{ profile }", timeout_ms=1))
        for variables in ([], {"bad": float("nan")}):
            with self.assertRaises((CatabolicError, ValueError)):
                self.query("{ profile }", variables=variables)

    def test_cli_file_stdin_json_errors_and_schema_without_database(self):
        path = self.root / "query.graphql"
        path.write_text("query Q($id: ID!) { item(id: $id) { id } }")
        base = ["--db", str(self.path), "graphql"]
        for args, input_text, expected in (
            ([*base, "--file", str(path), "--variables", '{"id":"album"}'], "", 0),
            ([*base, "--file", "-"], "{ profile }", 0),
            ([*base, "{ bad }"], "", 2),
            (["graphql", "--schema"], "", 0),
        ):
            out, err = io.StringIO(), io.StringIO()
            with (
                contextlib.redirect_stdout(out),
                contextlib.redirect_stderr(err),
                patch("sys.stdin", io.StringIO(input_text)),
            ):
                self.assertEqual(main(args), expected, err.getvalue())
            self.assertIsInstance(json.loads(out.getvalue()), dict)
            self.assertEqual(err.getvalue(), "")
