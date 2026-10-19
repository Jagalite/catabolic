# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Input coercion and validation differentials against frozen graphql-core.

All requests use disposable catalog copies. Historical known-gap fixtures remain
unchanged; passing them here is evidence for this implementation, not a blanket
GraphQL parity assertion. No runtime Python business logic is introduced.
"""

import json
import sqlite3
import unittest
from contextlib import closing

from tests import test_rust_legacy_inbox as inbox_fixture

ROOT = inbox_fixture.ROOT


class NativeGraphqlCoercionTest(unittest.TestCase):
    paired_cases = 0

    @classmethod
    def tearDownClass(cls):
        print(f"Frozen GraphQL differential pairs: {cls.paired_cases}")

    setUp = inbox_fixture.NativeLegacyInboxTest.setUp
    invoke = inbox_fixture.NativeLegacyInboxTest.invoke

    def compare(self, args):
        oracle, native = self.invoke(True, args), self.invoke(False, args)
        context = (args, oracle.stdout, oracle.stderr, native.stdout, native.stderr)
        self.assertEqual(native.returncode, oracle.returncode, context)
        self.assertEqual(native.stderr, oracle.stderr, context)
        if oracle.returncode:
            self.assertEqual(native.stdout, oracle.stdout, context)
        else:
            self.assertEqual(
                json.loads(native.stdout), json.loads(oracle.stdout), context
            )
        type(self).paired_cases += 1
        return json.loads(native.stdout)

    def graphql(self, document, variables=None, selected=None):
        args = [
            "--machine",
            "graphql",
            document,
            "--variables",
            json.dumps(variables or {}, ensure_ascii=False),
        ]
        if selected is not None:
            args += ["--operation-name", selected]
        return self.compare(args)

    def test_historical_gap_vectors(self):
        for name in [
            "graphql-composite-coercion-known-gaps.json",
            "graphql-document-validation-known-gaps.json",
        ]:
            for case in json.loads((ROOT / "tests/parity" / name).read_text())["cases"]:
                with self.subTest(case=case["name"]):
                    value = self.graphql(
                        case["document"], case["variables"], case.get("operation_name")
                    )
                    self.assertEqual(value, case["expected"])

    def test_lists_single_values_nullability_and_paths(self):
        document = (
            "query($n:Int!,$tags:[String!]){items(first:$n,tags:$tags){nodes{id}}}"
        )
        for value in [
            [],
            None,
            "one",
            ["one"],
            ["one", "two"],
            1,
            True,
            {},
            [1, None, False],
            [[1], {"x": True}],
            {"z": [1, None], "a": False},
        ]:
            with self.subTest(value=value):
                self.graphql(document, {"n": 1, "tags": value})
                self.graphql(document, {"n": 2147483648, "tags": value})
        for variables in [{}, {"tags": None}, {"tags": [None]}, {"tags": "x"}]:
            self.graphql(
                "query($tags:[String!]!){items(tags:$tags){nodes{id}}}", variables
            )
        self.graphql('query($tags:[String!]="single"){items(tags:$tags){nodes{id}}}')
        self.graphql(
            'query($tags:[String!]=["one","two"]){items(tags:$tags){nodes{id}}}'
        )

    def test_enums_values_suggestions_and_scalar_aggregation(self):
        for scalar in [
            "ID",
            "TITLE",
            "YEAR",
            "title",
            "TITEL",
            "YEER",
            "I",
            "bogus",
            "",
            1,
            False,
            [],
            {},
            ["ID"],
        ]:
            self.graphql(
                "query($n:Int!,$sort:ItemSort){items(first:$n,sort:$sort){nodes{id}}}",
                {"n": 2147483648, "sort": scalar},
            )
        for type_, field, values in [
            ("Active", "associations", ["active", "DISABELD", "ALL"]),
            ("Availability", "files", ["PRESNET", "missing", "UNKNOWN"]),
            ("Direction", "relationships", ["incoming", "BOT", "OUTGOING"]),
            ("EntryStatus", "items", ["NEEDS_ATENTION", "complete", "PENDING"]),
        ]:
            argument = {
                "Active": "active",
                "Availability": "status",
                "Direction": "direction",
                "EntryStatus": "curationStatus",
            }[type_]
            for value in values:
                self.graphql(
                    f"query($v:{type_}){{{field}({argument}:$v){{nodes{{id}}}}}}",
                    {"v": value},
                )
        self.graphql("query($v:ItemSort=TITLE){items(sort:$v){nodes{id}}}")

    def test_input_object_fields_mappings_defaults_and_inspect(self):
        document = "query($n:Int!,$f:RenditionFilter){items(first:$n,hasRendition:$f){nodes{id}}}"
        for value in [
            {},
            None,
            [],
            "x",
            False,
            1,
            {"current": "yes"},
            {"purpose": 1, "recipe": False, "definition": [], "current": "yes"},
            {"curent": True},
            {"recipee": "x", "purpos": "transcode"},
            {"zzz": 1, "aaa": 2, "current": False},
            {"current": [None, True, False, {"z": [1], "a": 2}]},
            {"recipe": {"z": {"a": {"b": 1}}, "a": []}},
        ]:
            with self.subTest(value=value):
                self.graphql(document, {"n": 2147483648, "f": value})
        self.graphql(
            "query($f:RenditionFilter={current:true,recipe:123}){items(hasRendition:$f){nodes{id}}}"
        )
        self.graphql("query($f:RenditionFilter={}){items(hasRendition:$f){nodes{id}}}")

    def test_scalar_inspection_limits_and_types(self):
        for value in [
            "x" * 239,
            "x" * 240,
            "x" * 400,
            "雪" * 400,
            "single'",
            'double"',
            "both'\"",
            "\\\n\r\t\x00\x08\x1f\x7f\u00a0\u200b\u2028\ue000",
            "a\u0301",
            [None, True, False],
            list(range(10)),
            list(range(11)),
            list(range(25)),
            {str(i): i for i in range(12)},
            [[1, [2, [3]]]],
            {"x": {"y": {"z": True}}},
            1e-7,
            -0.0,
            1e100,
            10**400,
            -(10**400),
        ]:
            with self.subTest(value=value):
                self.graphql("query($n:Int!){items(first:$n){nodes{id}}}", {"n": value})
                self.graphql("query($b:Boolean!){profile @skip(if:$b)}", {"b": value})

    def test_composite_error_cap_keeps_definition_then_field_order(self):
        for count in [49, 50, 51, 70]:
            self.graphql(
                "query($t:[String!],$n:Int!){items(tags:$t,first:$n){nodes{id}}}",
                {"t": [None] * count, "n": 2147483648},
            )
            self.graphql(
                "query($n:Int!,$t:[String!]){items(tags:$t,first:$n){nodes{id}}}",
                {"t": [None] * count, "n": 2147483648},
            )
        for count in [49, 50, 51]:
            self.graphql(
                "query($f:RenditionFilter){items(hasRendition:$f){nodes{id}}}",
                {"f": {f"f{i}": i for i in range(count)}},
            )

    def test_selected_operation_and_validation_precedence(self):
        document = "query A($n:Int!,$t:[String!],$f:RenditionFilter){items(first:$n,tags:$t,hasRendition:$f){nodes{id}}} query B{profile}"
        for selected in ["A", "B", "Missing", "", None]:
            self.graphql(
                document, {"n": 2147483648, "t": [None], "f": {"current": 1}}, selected
            )
        for query in [
            "query($n:Int!){notAField}",
            "query Named($n:Int!){notAField}",
            "query A($n:Int!){profile} query B($m:Int!){notAField}",
            "query($n:Int!,$f:RenditionFilter){notAField items(first:$n,hasRendition:$f){nodes{id}}}",
            "query($n:Int!){profile @skip(if:true)}",
            "query($n:Int!){items(first:$n){nodes{id}}}",
        ]:
            for selected in [None, "Missing"]:
                self.graphql(query, {"n": 2147483648, "f": {"current": 1}}, selected)

    def test_raw_json_order_duplicates_and_numeric_lexemes(self):
        for database in (self.reference, self.native):
            with closing(sqlite3.connect(database)) as db, db:
                db.executemany(
                    "INSERT INTO items(id,kind,metadata) VALUES(?,'movie','{}')",
                    [("0",), ("1",)],
                )
        for raw, expected in [
            ('{"id":-0}', "0"),
            ('{"id":-0.0}', "0"),
            ('{"id":1e0}', "1"),
        ]:
            result = self.compare(
                [
                    "--machine",
                    "graphql",
                    "query($id:ID!){item(id:$id){id}}",
                    "--variables",
                    raw,
                ]
            )
            self.assertEqual(result["data"]["data"]["item"]["id"], expected)
        for raw in [
            '{"f":{"zzz":1,"aaa":2,"zzz":3}}',
            '{"f":{"$serde_json::private::Number":"123"}}',
        ]:
            self.compare(
                [
                    "--machine",
                    "graphql",
                    "query($f:RenditionFilter){items(hasRendition:$f){nodes{id}}}",
                    "--variables",
                    raw,
                ]
            )

    def test_huge_literal_defaults_preserve_integer_kind(self):
        values = [2**90 + 1, -(2**90 + 1), 10**400]
        for database in (self.reference, self.native):
            with closing(sqlite3.connect(database)) as db, db:
                db.executemany(
                    "INSERT INTO items(id,kind,metadata) VALUES(?,'movie','{}')",
                    [(str(value),) for value in values],
                )
        for value in values:
            result = self.graphql(f"query($id:ID={value}){{item(id:$id){{id}}}}")
            self.assertEqual(result["data"]["data"]["item"]["id"], str(value))
            self.graphql(f"query($n:Int={value}){{items(first:$n){{nodes{{id}}}}}}")
            result = self.graphql(f"{{item(id:{value}){{id}}}}")
            self.assertEqual(result["data"]["data"]["item"]["id"], str(value))
            self.graphql(f"{{items(first:{value}){{nodes{{id}}}}}}")
        self.graphql("query($n:Int=1.0){items(first:$n){nodes{id}}}")
        self.graphql("query($id:ID=1.0){item(id:$id){id}}")

    def test_nested_id_coercion_and_huge_lexemes(self):
        for value in [
            2**90 + 1,
            -(2**90 + 1),
            10**400,
            -(10**400),
            1,
            0,
            1.0,
            -0.0,
            1e100,
        ]:
            self.graphql(
                "query($f:RenditionFilter){items(hasRendition:$f){nodes{id}}}",
                {"f": {"recipe": value}},
            )
        ids = [2**90 + 1, -(2**90 + 1), 10**400, -(10**400), 1, 0, int(1e100)]
        for database in (self.reference, self.native):
            with closing(sqlite3.connect(database)) as db, db:
                db.executemany(
                    "INSERT INTO items(id,kind,metadata) VALUES(?,'movie','{}')",
                    [(str(value),) for value in ids],
                )
        for value in ids + [1.0, -0.0, 1e100]:
            result = self.graphql("query($id:ID!){item(id:$id){id}}", {"id": value})
            self.assertEqual(result["data"]["data"]["item"]["id"], str(int(value)))


if __name__ == "__main__":
    unittest.main()
