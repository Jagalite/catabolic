# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Package-root GraphQL argument forwarding, independent of extension builds.

These source-level regressions verify the real facade's serialized argument.
They do not qualify native execution, HTTP validation, or an installed wheel.
"""

import ast
import json
import types
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FACADE = ROOT / "crates/catabolic-python/python/catabolic_native/__init__.py"


class GraphQLTypedPresenceReview(unittest.TestCase):
    def setUp(self):
        tree = ast.parse(FACADE.read_text())
        function = next(
            node
            for node in tree.body
            if isinstance(node, ast.FunctionDef) and node.name == "execute_graphql"
        )
        self.calls = []

        def execute_graphql(*args):
            self.calls.append(args)
            return '{"data": null}'

        namespace = {
            "json": json,
            "_native": types.SimpleNamespace(execute_graphql=execute_graphql),
        }
        exec(
            compile(ast.Module(body=[function], type_ignores=[]), str(FACADE), "exec"),
            namespace,
        )
        self.execute = namespace["execute_graphql"]

    def test_only_omitted_none_and_empty_object_forward_empty_object(self):
        for arguments in ({}, {"variables": None}, {"variables": {}}):
            with self.subTest(arguments=arguments):
                result = self.execute("disposable.sqlite3", "{profile}", **arguments)
                self.assertEqual(result, {"data": None})
                self.assertEqual(self.calls[-1][3], "{}")

    def test_falsey_non_objects_are_not_erased_before_native_validation(self):
        for value, expected in (([], "[]"), (False, "false"), (0, "0"), ("", '""')):
            with self.subTest(value=value):
                self.execute("disposable.sqlite3", "{profile}", variables=value)
                self.assertEqual(self.calls[-1][3], expected)

    def test_nested_order_exact_integer_and_call_options_are_preserved(self):
        variables = {
            "f": {"zzz": {"y": 1, "b": 2}, "aaa": 3},
            "id": 10**400 + 17,
        }
        self.execute(
            Path("disposable.sqlite3"),
            "query Named {profile}",
            variables=variables,
            profile="fixture",
            operation_name="Named",
            timeout_ms=37,
        )
        self.assertEqual(
            self.calls[-1],
            (
                "disposable.sqlite3",
                "query Named {profile}",
                "fixture",
                json.dumps(variables, allow_nan=False),
                "Named",
                37,
            ),
        )


if __name__ == "__main__":
    unittest.main()
