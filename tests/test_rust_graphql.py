# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""GraphQL differential scenarios using the real native process and snapshot."""

import json
import subprocess
import unittest

from catabolic.graphql_query import execute_graphql
from tests import test_graphql_query as fixtures
from tests import test_rust_queries as native_fixtures
from tests.test_rust_migration import BINARY


def reference_query(*args, **kwargs):
    # Compare the CLI's JSON contract; the Python oracle retains tuples in its
    # JSON scalar internally, while every shipped JSON transport emits arrays.
    return json.loads(json.dumps(execute_graphql(*args, **kwargs), allow_nan=False))


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeGraphQLTest(unittest.TestCase):
    native = native_fixtures.NativeQueryTest.native
    close_worker = native_fixtures.NativeQueryTest.close_worker

    def setUp(self):
        fixtures.GraphQLTest.setUp(self)
        self.worker = subprocess.Popen(
            [str(BINARY), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.close_worker)

    def query(self, document, **options):
        arguments = [document]
        if "variables" in options:
            arguments.extend(["--variables", json.dumps(options["variables"])])
        if "operation_name" in options:
            arguments.extend(["--operation-name", options["operation_name"]])
        return self.native(
            "graphql", *arguments, profile=options.get("profile", "default")
        )

    def test_nested_aliases_fragments_variables_and_read_only(self):
        document = """query Album($id: ID!) { album: item(id: $id) {
            ...Core metadata relationships(direction: INCOMING, kind: "part_of") {
                nodes { position source { ...Core associations { nodes {
                    role file { path sourcePath size mtimeNs status facts }
                } } } } pageInfo { hasNextPage endCursor }
            }
        } } fragment Core on Item { id kind title identities { namespace value } }"""
        before = self.path.read_bytes()
        options = {"variables": {"id": "album"}}
        self.assertEqual(
            self.query(document, **options),
            reference_query(self.path, document, **options),
        )
        self.assertEqual(before, self.path.read_bytes())

    def test_pages_filters_cursor_exchange_and_offline_profile(self):
        document = """query($after: String) { items(first: 1, kind: "track", sort: TITLE, after: $after) {
            nodes { id workflow { status requestedStatus revision } } pageInfo { endCursor hasNextPage }
        } }"""
        first = reference_query(self.path, document)
        self.assertEqual(self.query(document), first)
        variables = {"after": first["data"]["items"]["pageInfo"]["endCursor"]}
        self.assertEqual(
            self.query(document, variables=variables),
            reference_query(self.path, document, variables=variables),
        )
        offline = "{ profile files(first: 1) { nodes { id status sourcePath size } } catalogs { nodes { id root linkMode } } }"
        self.assertEqual(
            self.query(offline, profile="offline"),
            reference_query(self.path, offline, profile="offline"),
        )

    def test_all_read_only_page_fields(self):
        document = """{ profile schemaVersion mediaTypes
            items { nodes { id kind title year metadata } pageInfo { hasNextPage endCursor } }
            files { nodes { id location path status size mtimeNs } pageInfo { hasNextPage endCursor } }
            associations { nodes { id role metadata status itemId fileId } pageInfo { hasNextPage endCursor } }
            relationships { nodes { id kind position metadata sourceId targetId } pageInfo { hasNextPage endCursor } }
            mappings { nodes { id path catalog active itemId fileId } pageInfo { hasNextPage endCursor } }
            tags { nodes { id name aliases parentIds } pageInfo { hasNextPage endCursor } }
            taggings { nodes { id active subjectId } pageInfo { hasNextPage endCursor } }
            catalogs { nodes { id root linkMode } pageInfo { hasNextPage endCursor } }
            savedQueries { nodes pageInfo { hasNextPage endCursor } }
            operations { nodes pageInfo { hasNextPage endCursor } }
            projections { nodes pageInfo { hasNextPage endCursor } }
            artifacts { nodes pageInfo { hasNextPage endCursor } }
            recipes { nodes pageInfo { hasNextPage endCursor } }
            renditions { nodes pageInfo { hasNextPage endCursor } }
            outputDefinitions { nodes pageInfo { hasNextPage endCursor } }
            jobs { nodes pageInfo { hasNextPage endCursor } }
            proposals { nodes pageInfo { hasNextPage endCursor } }
            components { nodes { id kind itemIds } pageInfo { hasNextPage endCursor } }
            componentOccurrences { nodes { id kind current } pageInfo { hasNextPage endCursor } }
            worklog(item: "album") { nodes pageInfo { hasNextPage endCursor } }
            workflowChecks(item: "album") { nodes pageInfo { hasNextPage endCursor } }
            fallbackResolutions(catalog: "global") { nodes { id } pageInfo { hasNextPage endCursor } }
            projection(catalog: "global")
        }"""
        self.assertEqual(self.query(document), reference_query(self.path, document))

    def test_reject_writes_cycles_bad_variables_and_foreign_cursors(self):
        before = self.path.read_bytes()
        for document, variables in [
            ('mutation { deleteItem(id:"movie") }', {}),
            ("subscription { profile }", {}),
            ("{ ...A } fragment A on Query { ...A }", {}),
            ('{ files(after:"bogus") { nodes { id } } }', {}),
            ("query($n:Int!) { items(first:$n) { nodes { id } } }", {"n": "5"}),
            ("{ items(first:1001) { nodes { id } } }", {}),
            ("{ nonexistent }", {}),
        ]:
            with self.subTest(document=document):
                self.assertIn("errors", self.query(document, variables=variables))
        self.assertEqual(before, self.path.read_bytes())

    def test_schema_and_operation_selection(self):
        self.assertEqual(
            self.query(
                "query A { profile } query B { schemaVersion }", operation_name="B"
            ),
            reference_query(
                self.path,
                "query A { profile } query B { schemaVersion }",
                operation_name="B",
            ),
        )
        document = "{ __schema { mutationType { name } subscriptionType { name } queryType { name } } }"
        self.assertEqual(self.query(document), reference_query(self.path, document))

    def test_full_introspection_contract(self):
        from graphql import get_introspection_query

        document = get_introspection_query()
        expected = reference_query(self.path, document)
        actual = self.query(document)
        # Retain the first differences as a qualification artifact for diagnosis.
        from pathlib import Path

        output = (
            Path(__file__).resolve().parents[1] / ".local-tests/rust-migration/m2-m3"
        )
        (output / "introspection-python.json").write_text(
            json.dumps(expected, indent=2)
        )
        (output / "introspection-native.json").write_text(json.dumps(actual, indent=2))
        self.assertEqual(actual, expected)

    def test_selected_error_contracts(self):
        cases = [
            ("{ unknownField }", {}),
            ("{ items(first:0){nodes{id}} }", {}),
            ("query($id: ID!) {item(id:$id){id}}", {}),
            ("{item(id:42){id}}", {}),
            ("mutation { noop }", {}),
            (
                "query($n: Int!) { items(first: $n) { nodes { id } } }",
                {"variables": {"n": "5"}},
            ),
            ('{ files(after: "bogus") { nodes { id } } }', {}),
            ("{ item { id } }", {}),
            ('{ item(id: "movie", identity: "x=y") { id } }', {}),
        ]
        results = []
        for document, options in cases:
            expected = reference_query(self.path, document, **options)
            actual = self.query(document, **options)
            results.append({"query": document, "python": expected, "native": actual})
        from pathlib import Path

        output = (
            Path(__file__).resolve().parents[1]
            / ".local-tests/rust-migration/m2-m3/graphql-errors.json"
        )
        output.write_text(json.dumps(results, indent=2))
        for result in results:
            self.assertEqual(result["native"], result["python"], result["query"])

    def test_token_limit_and_unused_fragment_cycle(self):
        for document in (
            "{ " + " ".join(f"a{i}:profile" for i in range(1400)) + " }",
            "{profile} fragment A on Query {...A}",
        ):
            self.assertEqual(self.query(document), reference_query(self.path, document))

    def test_populated_definitions_jobs_and_projection_pages(self):
        from catabolic.app import Application
        from catabolic.layouts import PRESETS, Layouts
        from catabolic.operations import Operations
        from catabolic.processing import Processing
        from catabolic.projections import Projections
        from catabolic.saved_queries import Queries
        from catabolic.store import Store

        with Store(self.path, writable=True) as store:
            app = Application(store)
            queries = Queries(store)
            saved = queries.put(
                "album-query",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS item_id FROM items",
                    }
                },
            )
            queries.put(
                "large-query",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS item_id FROM items /*"
                        + "x" * 9000
                        + "*/",
                    }
                },
            )
            Layouts(app).put("flat", PRESETS["flat"])
            Projections(app).put("global", saved["id"], "flat")
            operation = Operations(app).put(
                "probe-test", {"kind": "analysis", "operation": "probe"}
            )
            file_id = store.rows("SELECT id FROM files ORDER BY id LIMIT 1")[0]["id"]
            queued = Processing(app).enqueue(
                "probe", file_ids=[file_id], recipe_id=operation["id"]
            )
            self.assertFalse(queued["errors"])
        document = """{ savedQueries(first:1){nodes pageInfo{hasNextPage endCursor}}
            operations{nodes pageInfo{hasNextPage endCursor}} recipes{nodes pageInfo{hasNextPage endCursor}}
            jobs{nodes pageInfo{hasNextPage endCursor}} projections{nodes pageInfo{hasNextPage endCursor}}
            projection(catalog:"global") }"""
        expected = reference_query(self.path, document)
        self.maxDiff = None
        self.assertEqual(self.query(document), expected)
        cursor = expected["data"]["savedQueries"]["pageInfo"]["endCursor"]
        document = "query($after:String){savedQueries(first:1,after:$after){nodes pageInfo{hasNextPage endCursor}}}"
        self.assertEqual(
            self.query(document, variables={"after": cursor}),
            reference_query(self.path, document, variables={"after": cursor}),
        )

    def test_typo_suggestions_and_selected_operation_variables(self):
        for document, options in [
            ("{taggings{nodes{tag}}}", {}),
            ("{items{nodes{titel}}}", {}),
            (
                "query A($n:Int!){items(first:$n){nodes{id}}} query B{profile}",
                {"variables": {"n": "bad"}, "operation_name": "B"},
            ),
            ("query($n:Int!){items(first:$n){nodes{id}}}", {"variables": {"n": 1.0}}),
        ]:
            self.assertEqual(
                self.query(document, **options),
                reference_query(self.path, document, **options),
            )

    def test_syntax_literals_and_field_validation_contracts(self):
        for document in (
            "{",
            "query { }",
            "{profile",
            '{items(first:"bad"){nodes{id}}}',
            "{items(sort:BAD){nodes{id}}}",
            "{profile{id}}",
            "{items}",
            "{profile(unknown:1)}",
            "{items(frst:1){nodes{id}}}",
            "{items(first:2147483648){nodes{id}}}",
            "query A{profile} query B{profile}",
            "{...Nope}",
            "query($n:Int){items(first:$n){nodes{id}}}",
        ):
            with self.subTest(document=document):
                self.assertEqual(
                    self.query(document), reference_query(self.path, document)
                )

    def test_populated_workflow_checks_worklog_and_work_inbox(self):
        from catabolic.app import Application
        from catabolic.item_workflow import ItemWorkflow
        from catabolic.store import Store

        with Store(self.path, writable=True) as store:
            workflow = ItemWorkflow(Application(store))
            workflow.note("book", "First")
            workflow.note("book", "Second")
            workflow.set_status("book", "complete")
            workflow.require("book", "review", "Review again")
        for document in (
            "{items(curationStatus:NEEDS_ATTENTION){nodes{id workflow{status requestedStatus revision}workflowChecks{nodes}worklog(first:1){nodes pageInfo{hasNextPage endCursor}}}}}",
            "{workInbox(first:1){nodes pageInfo{hasNextPage endCursor}}}",
            "{workInbox(includeInactive:true){nodes pageInfo{hasNextPage endCursor}}}",
        ):
            with self.subTest(document=document):
                self.maxDiff = None
                self.assertEqual(
                    self.query(document), reference_query(self.path, document)
                )
