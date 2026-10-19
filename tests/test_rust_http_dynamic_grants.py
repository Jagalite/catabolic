# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Dynamic HTTP grant qualification against frozen 1.7 behavior.

Put frozen 453fca983222c6665a48775eef67c489c96b527e source first on
PYTHONPATH. Python creates disposable fixtures and serves the independent
oracle database; candidate requests use the opt-in native loopback listener.
Approved saved selections preserve frozen global-selection-then-intersection
semantics; direct GraphQL and saved documents retain their scoped disclosure.
"""

import json
import os
import shutil
import unittest

from catabolic.access import grant_put
from catabolic.app import Application
from catabolic.saved_queries import Queries
from catabolic.store import Store, encode
from tests import test_http_backend as reference
from tests import test_rust_http_transport as transport


class NativeHTTPDynamicGrantsTest(unittest.TestCase):
    start_native_server = transport.NativeHTTPTransportTest.start_native_server
    stop_server = transport.NativeHTTPTransportTest.stop_server

    def setUp(self):
        self.assertTrue(transport.BINARY.is_file(), "build catabolic-http-experimental")
        reference.HTTPTest.setUp(self)
        self.client.close()
        with Store(self.database, writable=True) as store:
            app = Application(store)
            self.second = app.put_item(
                "movie",
                {"test": "second"},
                {
                    "title": "Allowed second",
                    "year": 2025,
                    "private_note": "DO_NOT_DISCLOSE",
                },
            )["id"]
            self.literal = app.put_item(
                "movie",
                {"test": "literal"},
                {"title": "Literal item", "private_note": "PRIVATE-LITERAL"},
            )["id"]
            queries = Queries(store)
            self.queries = {}

            def put(name, definition):
                result = queries.put(name, definition)
                self.queries[name] = result["id"]
                return result["id"]

            sql = put(
                "sql-public",
                self.sql(
                    "SELECT id AS item_id FROM items WHERE json_extract(metadata,'$.private_note')=:marker",
                    {"marker": "DO_NOT_DISCLOSE"},
                ),
            )
            first = put(
                "sql-first",
                self.sql(
                    "SELECT id AS item_id FROM items WHERE id=:id", {"id": self.item}
                ),
            )
            graphql = put("graphql-public", self.graphql(marker="DO_NOT_DISCLOSE"))
            all_items = put("graphql-all", self.graphql())
            union = put("union", {"combine": "union", "queries": [first, graphql]})
            intersection = put(
                "intersection",
                {"combine": "intersection", "queries": [all_items, union]},
            )
            put(
                "difference",
                {"combine": "difference", "queries": [intersection, first]},
            )
            put("mixed", {"combine": "union", "queries": [sql, graphql]})
            paged = self.sql(
                "SELECT id AS item_id FROM items WHERE json_extract(metadata,'$.private_note')=:marker",
                {"marker": "DO_NOT_DISCLOSE"},
            )
            paged["selection"]["page_size"] = 1
            put("sql-paged", paged)
            put(
                "rows-public",
                {
                    "mode": "rows",
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS item_id FROM items ORDER BY id",
                    },
                },
            )
            put(
                "document-public",
                {
                    "mode": "document",
                    "selection": {
                        "language": "graphql",
                        "query": "{ items { nodes { id metadata } } }",
                    },
                },
            )
            self.original = queries.get(sql)["definition"]

    @staticmethod
    def sql(query, params=None):
        return {
            "mode": "selection",
            "selection": {"language": "sql", "query": query, "params": params or {}},
        }

    @staticmethod
    def graphql(marker=None):
        variables = {}
        declaration, arguments = "$after:String", "first:1,after:$after"
        if marker is not None:
            declaration += ",$marker:String!"
            arguments += ",metadata:[$marker]"
            variables["marker"] = "private_note=" + json.dumps(marker)
        return {
            "mode": "selection",
            "selection": {
                "language": "graphql",
                # Root aliases are valid; nested aliases would break ID contracts.
                "query": "query("
                + declaration
                + "){selected:items("
                + arguments
                + "){nodes{id} pageInfo{hasNextPage endCursor}}}",
                "variables": variables,
            },
        }

    def start_http(self, query="sql-public", *, literals=(), reports=()):
        from fastapi.testclient import TestClient

        from catabolic.http.app import create_app

        self.definition = {
            "actions": ["metadata:read", "content:read"],
            "query_id": self.queries[query],
            "item_ids": list(literals),
            "file_ids": [self.file],
            "metadata_fields": ["title", "year"],
            "report_ids": [self.queries[name] for name in reports],
        }
        with Store(self.database, writable=True) as store:
            grant_put(store, "site", self.definition, self.grant)
        self.oracle_database = self.root / "frozen-http.db"
        shutil.copyfile(self.database, self.oracle_database)
        self.assertNotEqual(self.database, self.oracle_database)
        self.assertEqual(self.database.read_bytes(), self.oracle_database.read_bytes())
        self.oracle = TestClient(create_app(self.oracle_database, read_only=False))
        self.addCleanup(self.oracle.close)
        self.start_native_server(env=os.environ.copy())

    def pair(self, method, path, **kwargs):
        kwargs.setdefault("headers", self.headers)
        expected = self.oracle.request(method, path, **kwargs)
        actual = self.client.request(method, path, **kwargs)
        self.assertEqual(
            actual.status_code, expected.status_code, (actual.text, expected.text)
        )
        if "json" in expected.headers.get("content-type", ""):
            left, right = actual.json(), expected.json()
            for value in (left, right):
                if "request_id" in value:
                    self.assertTrue(value.pop("request_id"))
            self.assertEqual(left, right)
        else:
            self.assertEqual(actual.content, expected.content)
        return actual

    def update_grant(self, **changes):
        self.definition.update(changes)
        for path in (self.database, self.oracle_database):
            with Store(path, writable=True) as store:
                grant_put(store, "site", self.definition, self.grant)

    def replace_query(self, value, name="sql-public"):
        # Simulate a corrupted/imported stored definition after a valid grant
        # was issued. The administration API correctly rejects these up front.
        for path in (self.database, self.oracle_database):
            with Store(path, writable=True) as store:
                with store.transaction() as db:
                    db.execute(
                        "UPDATE saved_queries SET definition=? WHERE id=?",
                        (encode(value), self.queries[name]),
                    )

    def assert_private(self, response, *, hidden=True):
        for value in (
            "DO_NOT_DISCLOSE",
            "PRIVATE-LITERAL",
            str(self.source),
            "hidden.bin",
            self.credential["token"],
        ):
            self.assertNotIn(value, response.text)
        if hidden:
            self.assertNotIn(self.hidden, response.text)

    def assert_items(self, expected):
        result = self.pair("GET", "/v1/items")
        self.assertEqual(result.status_code, 200, result.text)
        self.assertEqual([row["id"] for row in result.json()["data"]], sorted(expected))
        self.assert_private(result)
        return result

    def test_sql_membership_reads_administrator_selection_but_masks_response(self):
        self.start_http()
        self.assert_items([self.item, self.second])
        result = self.pair("GET", "/v1/items/" + self.item)
        self.assertEqual(result.status_code, 200, result.text)
        self.assert_private(result)
        self.assertEqual(self.pair("GET", "/v1/items/" + self.hidden).status_code, 404)
        self.assertEqual(
            self.pair(
                "POST", "/v1/query/sql", json={"query": "SELECT id FROM items"}
            ).status_code,
            403,
        )

    def test_graphql_membership_bootstraps_complete_aliased_paged_ids(self):
        self.start_http("graphql-public")
        self.assert_items([self.item, self.second])
        result = self.pair(
            "POST",
            "/v1/query/graphql",
            json={
                "query": "{ items { nodes { id title metadata associations { nodes { file { id size } } } } } }"
            },
        )
        self.assertEqual(result.status_code, 200, result.text)
        self.assertNotIn("errors", result.json())
        self.assert_private(result)
        self.assertEqual(
            self.pair("GET", "/v1/files/" + self.hidden_file).status_code, 404
        )
        path = self.pair(
            "POST",
            "/v1/query/graphql",
            json={"query": "{ files { nodes { id sourcePath } } }"},
        )
        self.assertIn("FORBIDDEN", path.text)
        self.assert_private(path)

    def test_literal_membership_unions_with_dynamic_ids(self):
        self.start_http("graphql-public", literals=[self.literal])
        self.assert_items([self.item, self.second, self.literal])

    def test_nested_union_intersection_difference_and_mixed_families(self):
        self.start_http("union")
        for name, expected in (
            ("union", [self.item, self.second]),
            ("intersection", [self.item, self.second]),
            ("difference", [self.second]),
            ("mixed", [self.item, self.second]),
            ("sql-paged", [self.item, self.second]),
        ):
            with self.subTest(query=name):
                self.update_grant(query_id=self.queries[name])
                self.assert_items(expected)

    def test_rest_and_saved_query_paging_preserve_complete_membership(self):
        self.start_http("graphql-public", reports=["graphql-all"])
        for endpoint, key in (
            ("/v1/items", "data"),
            ("/v1/queries/" + self.queries["graphql-all"] + "/runs", "ids"),
        ):
            with self.subTest(endpoint=endpoint):
                cursor, collected = None, []
                for index in range(2):
                    if key == "data":
                        result = self.pair(
                            "GET",
                            endpoint,
                            params={
                                "limit": 1,
                                **({"cursor": cursor} if cursor else {}),
                            },
                        )
                        collected.extend(row["id"] for row in result.json()[key])
                    else:
                        result = self.pair(
                            "POST",
                            endpoint,
                            json={"limit": 1, **({"cursor": cursor} if cursor else {})},
                        )
                        collected.extend(result.json()[key])
                    self.assertEqual(result.status_code, 200, result.text)
                    self.assert_private(result)
                    cursor = result.json()["next_cursor"]
                    self.assertEqual(cursor is not None, index == 0)
                self.assertEqual(collected, sorted([self.item, self.second]))

    def test_live_metadata_changes_recompute_both_sql_and_graphql_membership(self):
        self.start_http()
        for name in ("sql-public", "graphql-public"):
            with self.subTest(query=name):
                self.update_grant(query_id=self.queries[name])
                for include in (False, True):
                    for path in (self.database, self.oracle_database):
                        with Store(path, writable=True) as store:
                            Application(store).put_item(
                                "movie",
                                {},
                                {
                                    "title": "Allowed",
                                    "private_note": "DO_NOT_DISCLOSE"
                                    if include
                                    else "PRIVATE-REMOVED",
                                },
                                self.item,
                            )
                    self.assert_items(
                        [self.item, self.second] if include else [self.second]
                    )

    def test_live_grant_revision_invalidates_cursor_and_changes_metadata_fields(self):
        self.start_http("graphql-public")
        page = self.pair("GET", "/v1/items", params={"limit": 1})
        self.assertIsNotNone(page.json()["next_cursor"])
        self.update_grant(query_id=self.queries["sql-first"], metadata_fields=["year"])
        self.assertEqual(
            self.pair(
                "GET", "/v1/items", params={"cursor": page.json()["next_cursor"]}
            ).status_code,
            400,
        )
        result = self.assert_items([self.item])
        self.assertNotIn("Allowed", result.text)
        self.assertEqual(self.pair("GET", "/v1/items/" + self.second).status_code, 404)

    def test_approved_saved_selection_preserves_frozen_semantics_without_document_leak(
        self,
    ):
        self.start_http("graphql-public", reports=["graphql-public", "document-public"])
        self.assert_items([self.item, self.second])
        # In both engines a client-facing GraphQL filter sees only allowed
        # metadata even though the administrator's membership query sees more.
        result = self.pair(
            "POST",
            "/v1/query/graphql",
            json={
                "query": '{ items(metadata:["private_note=\\"DO_NOT_DISCLOSE\\""]) { nodes { id } } }'
            },
        )
        self.assertEqual(result.json()["data"]["items"]["nodes"], [])
        document = self.pair(
            "POST", "/v1/queries/" + self.queries["document-public"] + "/runs", json={}
        )
        self.assert_private(document)
        self.assertNotIn("errors", document.json())
        endpoint = "/v1/queries/" + self.queries["graphql-public"] + "/runs"
        expected = self.oracle.post(endpoint, headers=self.headers, json={})
        actual = self.client.post(endpoint, headers=self.headers, json={})
        self.assertEqual(expected.status_code, 200, expected.text)
        self.assertEqual(actual.status_code, 200, actual.text)
        # Approved saved selections use administrator-authored global
        # predicates, then intersect admitted IDs. This does not authorize
        # revealing their private metadata to a direct/document GraphQL query.
        self.assertEqual(expected.json()["ids"], sorted([self.item, self.second]))
        self.assertEqual(actual.json(), expected.json())
        self.assertTrue(actual.json()["selection_complete"])
        self.assert_private(actual)

    def test_invalid_grant_selections_fail_closed_even_with_literal_ids(self):
        self.start_http(literals=[self.literal])
        query_id = self.queries["sql-public"]
        invalid = {
            "cycle": {
                "mode": "selection",
                "combine": "union",
                "queries": [query_id, query_id],
            },
            "unknown-child": {
                "mode": "selection",
                "combine": "union",
                "queries": [self.queries["sql-first"], "missing-query"],
            },
            "wrong-entity": self.sql("SELECT id AS file_id FROM files"),
            "wrong-declared-entity": {**self.original, "entity": "file_id"},
            "rows": {
                "mode": "rows",
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items",
                },
            },
            "document": {
                "mode": "document",
                "selection": {
                    "language": "graphql",
                    "query": "{ items { nodes { id metadata } } }",
                },
            },
            "composition-rows": {
                "mode": "selection",
                "combine": "union",
                "queries": [self.queries["sql-first"], self.queries["rows-public"]],
            },
            "composition-document": {
                "mode": "selection",
                "combine": "union",
                "queries": [self.queries["sql-first"], self.queries["document-public"]],
            },
            "sql-incomplete": {
                **self.original,
                "selection": {**self.original["selection"], "max_ids": 1},
            },
            "sql-paged-overbudget": {
                **self.original,
                "max_ids": 1,
                "selection": {
                    **self.original["selection"],
                    "page_size": 1,
                    "max_ids": 1,
                },
            },
            "graphql-overbudget": {**self.graphql(), "max_ids": 1},
            "incomplete-page-contract": {
                "mode": "selection",
                "selection": {
                    "language": "graphql",
                    "query": "query($after:String){items(after:$after){nodes{id}}}",
                },
            },
            "document-shaped-selection": {
                "mode": "selection",
                "selection": {
                    "language": "graphql",
                    "query": "query($after:String){items(after:$after){nodes{id metadata} pageInfo{hasNextPage endCursor}}}",
                },
            },
        }
        for name, definition in invalid.items():
            with self.subTest(case=name):
                self.replace_query(definition)
                result = self.pair("GET", "/v1/items")
                self.assertGreaterEqual(result.status_code, 400, result.text)
                self.assert_private(result)
                self.assertNotIn(self.literal, result.text)
        self.replace_query(self.original)
        self.assert_items([self.item, self.second, self.literal])

    def test_private_sql_aliases_and_mutation_cannot_bootstrap_membership(self):
        self.start_http(literals=[self.literal])
        for query in (
            "SELECT digest AS item_id FROM api_tokens",
            "SELECT t.digest AS item_id FROM api_tokens AS t",
            "WITH leaked AS (SELECT digest FROM api_tokens) SELECT digest AS item_id FROM leaked",
            "SELECT token AS item_id FROM execution_claims",
            "SELECT id AS item_id FROM main.api_tokens",
            "DELETE FROM items RETURNING id AS item_id",
        ):
            with self.subTest(query=query):
                self.replace_query(self.sql(query))
                result = self.pair("GET", "/v1/items")
                self.assertGreaterEqual(result.status_code, 400, result.text)
                self.assert_private(result)
                self.assertNotIn(self.literal, result.text)
        self.replace_query(self.original)
        self.assert_items([self.item, self.second, self.literal])
        for path in (self.database, self.oracle_database):
            with Store(path) as store:
                self.assertEqual(
                    store.rows("SELECT count(*) AS n FROM items")[0]["n"], 4
                )


if __name__ == "__main__":
    unittest.main()
