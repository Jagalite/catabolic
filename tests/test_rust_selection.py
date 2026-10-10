# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native complete selections and saved-query composition against the reference."""

import json

from catabolic.saved_queries import Queries
from catabolic.selection import select_ids
from catabolic.store import Store
from tests import test_rust_queries as runner


class NativeSelectionTest(runner.NativeQueryTest):
    # Reuse the snapshot fixture and worker, without inheriting unrelated test cases.
    test_all_views_and_discovery = None

    def test_sql_complete_and_paged_ids(self):
        before = self.path.read_bytes()
        for selection in (
            {"language": "sql", "query": "SELECT id AS item_id FROM items"},
            {
                "language": "sql",
                "query": "SELECT id AS item_id FROM items",
                "page_size": 2,
            },
            {
                "language": "sql",
                "query": "SELECT id AS item_id FROM items UNION ALL SELECT id AS item_id FROM items",
                "page_size": 1,
            },
            {
                "language": "sql",
                "query": "SELECT file_id FROM item_files WHERE active=1",
                "page_size": 2,
                "max_ids": 100000,
            },
        ):
            with self.subTest(selection=selection), Store(self.path) as store:
                entity, ids, report = select_ids(store, selection)
                actual = self.native("select", json.dumps(selection))
                self.assertEqual(
                    actual, {"entity": entity, "ids": sorted(ids), "report": report}
                )
        self.assertEqual(before, self.path.read_bytes())

    def test_graphql_complete_and_aliased_pages(self):
        for field in ("items", "files", "associations"):
            selection = {
                "language": "graphql",
                "query": f"query($after:String) {{ picked:{field}(first:2,after:$after) {{ nodes {{ id }} pageInfo {{ hasNextPage endCursor }} }} }}",
            }
            with self.subTest(field=field), Store(self.path) as store:
                entity, ids, report = select_ids(store, selection)
                self.assertEqual(
                    self.native("select", json.dumps(selection)),
                    {"entity": entity, "ids": sorted(ids), "report": report},
                )

    def test_saved_modes_composition_and_limits(self):
        with Store(self.path, writable=True) as store:
            queries = Queries(store)
            first = queries.put(
                "first",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS item_id FROM items WHERE kind='movie'",
                    }
                },
            )["id"]
            second = queries.put(
                "second",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS item_id FROM items WHERE id IN ('a','c')",
                    }
                },
            )["id"]
            identifiers = [first, second]
            for combine in ("union", "intersection", "difference"):
                identifiers.append(
                    queries.put(
                        combine, {"combine": combine, "queries": [first, second]}
                    )["id"]
                )
            identifiers.append(
                queries.put(
                    "rows",
                    {
                        "mode": "rows",
                        "selection": {
                            "language": "sql",
                            "query": "SELECT id FROM items ORDER BY id",
                        },
                    },
                )["id"]
            )
            identifiers.append(
                queries.put(
                    "document",
                    {
                        "mode": "document",
                        "selection": {
                            "language": "graphql",
                            "query": "{ items(first:2) { nodes { id } } }",
                        },
                    },
                )["id"]
            )
        before = self.path.read_bytes()
        with Store(self.path) as store:
            for identifier in identifiers:
                with self.subTest(identifier=identifier):
                    expected = json.loads(
                        json.dumps(Queries(store).run(identifier, limit=2))
                    )
                    self.assertEqual(
                        self.native("saved-query", identifier, "--max-rows", 2),
                        expected,
                    )
        self.assertEqual(before, self.path.read_bytes())

    def test_selection_failures_refuse_partial_results(self):
        from catabolic.domain import CatabolicError

        for selection in (
            {
                "language": "sql",
                "query": "SELECT id AS item_id FROM items",
                "max_ids": 1,
            },
            {
                "language": "sql",
                "query": "SELECT id AS item_id FROM items",
                "page_size": 1,
                "max_ids": 1,
            },
            {"language": "sql", "query": "SELECT 1 AS item_id"},
            {"language": "sql", "query": "SELECT id AS wrong FROM items"},
            {"language": "sql", "query": "SELECT random() AS item_id"},
            {"language": "graphql", "query": "{ items { nodes { id } } }"},
            {
                "language": "graphql",
                "query": "query($after:String){ items(first:1,after:$after){nodes{id} pageInfo{hasNextPage endCursor}}}",
                "variables": {"after": None},
            },
        ):
            with self.subTest(selection=selection), Store(self.path) as store:
                with self.assertRaises(CatabolicError) as error:
                    select_ids(store, selection)
                actual = self.native("select", json.dumps(selection), ok=False)
                self.assertEqual(actual, "catabolic: " + str(error.exception))


# Only the four selection tests belong to this module's qualification.
for _name in list(vars(runner.NativeQueryTest)):
    if _name.startswith("test_"):
        setattr(NativeSelectionTest, _name, None)
