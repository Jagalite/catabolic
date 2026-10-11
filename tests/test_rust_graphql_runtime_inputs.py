# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent frozen pairs for runtime null arguments and selection directives.

Checks complete responses, including aliases, sibling execution, null boundaries,
locations, records and paths. All catalogs are disposable copies.
"""

import json
import unittest

from tests import test_rust_legacy_inbox as fixture


def runtime_cases():
    cases = []

    def add(name, document, variables):
        cases.append((name, document, variables))

    for declaration in ["Int", "Int=null", "Int=2"]:
        for suffix in ["", " @skip(if:true)", " @include(if:false)"]:
            add(
                f"first-{declaration}-{suffix}",
                f"query($v:{declaration}){{before:profile bad:items(first:$v){suffix}{{nodes{{id}}}} after:schemaVersion}}",
                {"v": None},
            )
        for item in ["a", "absent"]:
            add(
                f"nested-first-{declaration}-{item}",
                f'query($v:{declaration}){{before:profile item(id:"{item}"){{id bad:worklog(first:$v){{nodes}} title}} after:schemaVersion}}',
                {"v": None},
            )
    add(
        "first-default-null-absent",
        "query($v:Int=null){before:profile bad:items(first:$v){nodes{id}} after:schemaVersion}",
        {},
    )
    add(
        "schema-order",
        "query($v:Int,$b:Boolean){items(first:$v,descendants:$b){nodes{id}}}",
        {"v": None, "b": None},
    )
    for expression in ["$v", "[$v]", '["valid",$v]']:
        add(
            "list-" + expression,
            f'query($v:String=""){{before:profile items(tags:{expression}){{nodes{{id}}}} after:schemaVersion}}',
            {"v": None},
        )
    for directives in [
        "@skip(if:$b)",
        "@include(if:$b)",
        "@skip(if:true) @include(if:$b)",
        "@include(if:$b) @skip(if:true)",
        "@skip(if:false) @include(if:$b)",
        "@include(if:false) @skip(if:$b)",
    ]:
        for selection in [
            f"bad:profile {directives}",
            f"... on Query {directives} {{profile}}",
            f"...Root {directives}",
        ]:
            tail = (
                " fragment Root on Query{profile}"
                if selection.startswith("...Root")
                else ""
            )
            add(
                "root-directive-" + selection,
                f"query($b:Boolean=false){{before:schemaVersion {selection} after:profile}}"
                + tail,
                {"b": None},
            )
        for item in ["a", "absent"]:
            add(
                f"object-directive-{item}-{directives}",
                f'query($b:Boolean=false){{before:profile item(id:"{item}"){{left:id title {directives} right:kind}} after:schemaVersion}}',
                {"b": None},
            )
        for first in [1, 100]:
            add(
                f"list-directive-{first}-{directives}",
                f"query($b:Boolean=false){{before:profile items(first:{first}){{nodes{{left:id title {directives} right:kind}} pageInfo{{hasNextPage}}}} after:schemaVersion}}",
                {"b": None},
            )
        add(
            "empty-list-directive-" + directives,
            f'query($b:Boolean=false){{items(search:"no-matching-title"){{nodes{{id title {directives}}}}} after:profile}}',
            {"b": None},
        )
        add(
            "skipped-parent-directive-" + directives,
            f'query($b:Boolean=false){{item(id:"a") @skip(if:true){{id title {directives}}} after:profile}}',
            {"b": None},
        )
    add(
        "fragment-argument",
        'query($n:Int){a:item(id:"a"){...Parts} b:item(id:"absent"){...Parts} after:profile} fragment Parts on Item{id worklog(first:$n){nodes}}',
        {"n": None},
    )
    add(
        "fragment-directive",
        'query($b:Boolean=false){a:item(id:"a"){...Parts} b:item(id:"absent"){...Parts} after:profile} fragment Parts on Item{id title @skip(if:$b)}',
        {"b": None},
    )
    add(
        "non-null-root-bubble",
        'query($v:String="x"){before:profile contentSearch(text:$v) after:schemaVersion}',
        {"v": None},
    )
    add("root-all-skipped-literal", "{profile @skip(if:true)}", {})
    add(
        "root-all-skipped-null-argument",
        "query($n:Int){items(first:$n) @skip(if:true){nodes{id}}}",
        {"n": None},
    )
    add(
        "root-all-skipped-null-include",
        "query($b:Boolean=false){profile @skip(if:true) @include(if:$b)}",
        {"b": None},
    )
    add(
        "root-all-skipped-fragment",
        "{... F} fragment F on Query{profile @skip(if:true)}",
        {},
    )
    add("root-all-skipped-inline", "{... on Query @include(if:false){profile}}", {})
    return cases


class NativeGraphqlRuntimeInputTest(unittest.TestCase):
    setUp = fixture.NativeLegacyInboxTest.setUp
    invoke = fixture.NativeLegacyInboxTest.invoke

    def compare(self, args):
        reference, native = self.invoke(True, args), self.invoke(False, args)
        self.assertEqual(
            native.returncode,
            reference.returncode,
            (args, reference.stdout, native.stdout),
        )
        self.assertEqual(native.stderr, reference.stderr)
        self.assertEqual(
            json.loads(native.stdout),
            json.loads(reference.stdout),
            (args, reference.stdout, native.stdout),
        )

    def test_legacy_variables_presence_and_null(self):
        for document in [
            "{profile}",
            "query($n:Int){items(first:$n){nodes{id}}}",
            "query($n:Int!){items(first:$n){nodes{id}}}",
        ]:
            for tail in [
                [],
                ["--variables", "{}"],
                ["--variables", "null"],
                ["--variables", " \n null\t"],
                ["--variables", "[]"],
            ]:
                with self.subTest(document=document, tail=tail):
                    self.compare(["--machine", "graphql", document] + tail)

    def test_explicit_remaining_gap_vectors(self):
        # This verifies the known differences, not parity. Guarded runtime
        # vectors must stay exactly pre-fix and must never report success.
        path = fixture.ROOT / "tests/parity/graphql-review-remaining-gaps.json"
        for case in json.loads(path.read_text())["cases"]:
            with self.subTest(case=case["name"]):
                args = [
                    "--machine",
                    "graphql",
                    case["document"],
                    "--variables",
                    json.dumps(case["variables"]),
                ]
                reference, native = self.invoke(True, args), self.invoke(False, args)
                self.assertEqual(json.loads(reference.stdout), case["expected"])
                self.assertEqual(json.loads(native.stdout), case["observed"])
                self.assertNotEqual(case["expected"], case["observed"])
                if case["category"].startswith("guarded-"):
                    self.assertEqual(case["observed"], case["pre_fix_observed"])
                    self.assertNotEqual(native.returncode, 0)
                    self.assertIsNone(case["observed"]["data"]["data"])
                    self.assertNotIn("extensions", case["observed"]["data"])

    def test_runtime_null_matrix(self):
        for name, document, variables in runtime_cases():
            with self.subTest(case=name):
                self.compare(
                    [
                        "--machine",
                        "graphql",
                        document,
                        "--variables",
                        json.dumps(variables),
                    ]
                )


if __name__ == "__main__":
    unittest.main()
