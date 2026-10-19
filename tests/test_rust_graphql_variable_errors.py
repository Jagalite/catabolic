# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Finite legacy variable-JSON diagnostics and unchanged value/order controls."""

import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from tests import test_rust_legacy_parser as fixture

ORACLE = "import sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"


class GraphQLVariableErrorsTest(unittest.TestCase):
    def test_legacy_variable_diagnostics_match_frozen_cli(self):
        legacy = Path(os.environ["CATABOLIC_LEGACY_BINARY"])
        from scripts.qualify_native_mutations import oracle_snapshot

        oracle_before = oracle_snapshot(fixture.REFERENCE, fixture.ROOT)
        self.assertEqual(oracle_before["reference_commit"], fixture.FROZEN_COMMIT)
        artifact_before = hashlib.sha256(legacy.read_bytes()).hexdigest()
        malformed = [
            ("empty", ""),
            ("whitespace", " \t\r\n"),
            ("open-object", "{"),
            ("unquoted-key", "{profile}"),
            ("single-quotes", "{'x':1}"),
            ("missing-colon", '{"x" 1}'),
            ("eof-after-key", '{"x"'),
            ("eof-after-colon", '{"x":'),
            ("missing-value", '{"x":}'),
            ("missing-comma", '{"x":1 "y":2}'),
            ("object-trailing-comma", '{"x":1, \n}'),
            ("array-trailing-comma", "[1, \n]"),
            ("open-array", "["),
            ("array-missing-comma", "[true false]"),
            ("array-double-comma", "[1,,2]"),
            ("unterminated-string", '{"x":"abc'),
            ("trailing-backslash", '"abc\\'),
            ("bad-escape", '"a\\q"'),
            ("short-unicode", '"a\\u123"'),
            ("bad-unicode", '"a\\uzzzz"'),
            ("control-character", '"a\nb"'),
            ("bad-literal", "tru"),
            ("leading-zero", "01"),
            ("bad-number", "-"),
            ("incomplete-exponent", "1e+"),
            ("incomplete-fraction", "1."),
            ("extra-data", "{} true"),
            ("bom", "\ufeff{}"),
            ("non-json-whitespace", "\u00a0{}"),
            ("unicode-multiline", '{"é😀":1,\n "x": [true false]}'),
        ]
        controls = [
            ("omitted", None, "{profile}"),
            ("null", "null", "{profile}"),
            ("object", "{}", "{profile}"),
            ("integer", "1", "{profile}"),
            ("float", "1.25", "{profile}"),
            ("boolean", "false", "{profile}"),
            ("string", '"x"', "{profile}"),
            ("array", "[]", "{profile}"),
            (
                "duplicate-keys",
                '{"x":true,"x":false}',
                "query($x:Boolean!){profile @skip(if:$x)}",
            ),
            (
                "escaped-duplicate",
                '{"x":true,"\\u0078":false}',
                "query($x:Boolean!){profile @skip(if:$x)}",
            ),
            ("huge-unused-integer", '{"x":1237940039285380274899124225}', "{profile}"),
            ("numeric-lexemes", '{"x":1.00000000000000001e+20,"z":-0.0}', "{profile}"),
            (
                "huge-id",
                '{"id":1237940039285380274899124225}',
                "query($id:ID!){item(id:$id){id}}",
            ),
            (
                "integral-float",
                '{"n":1.000e+0}',
                "query($n:Int!){items(first:$n){nodes{id}}}",
            ),
            (
                "nonintegral-float",
                '{"n":1.0000000000000002}',
                "query($n:Int!){items(first:$n){nodes{id}}}",
            ),
            ("unicode-strings", '{"x":"é😀","y":"\\u00e9"}', "{profile}"),
            ("literal-sentinel", '{"$serde_json::private::Number":"123"}', "{profile}"),
            (
                "coercion-order",
                '{"f":{"zzz":1,"aaa":2,"zzz":3}}',
                "query($f:RenditionFilter){items(hasRendition:$f){nodes{id}}}",
            ),
        ]
        rows = []
        with tempfile.TemporaryDirectory(prefix="graphql-variable-errors-") as scratch:
            root = Path(scratch)
            env = fixture.environment(
                overrides={
                    "HOME": str(root / "home"),
                    "PYTHONPYCACHEPREFIX": str(root / "pycache"),
                }
            )
            oracle = [str(fixture.PYTHON), "-c", ORACLE, str(fixture.REFERENCE / "src")]
            reference, native = root / "reference.db", root / "native.db"
            initialized = subprocess.run(
                oracle + ["--db", str(reference), "init"],
                capture_output=True,
                env=env,
                timeout=90,
            )
            self.assertEqual(initialized.returncode, 0, initialized.stderr)
            shutil.copyfile(reference, native)
            before = [
                hashlib.sha256(path.read_bytes()).hexdigest()
                for path in (reference, native)
            ]

            def check(name, tail, mode=(), data=b"", database=True):
                pair = []
                for prefix, db in ((oracle, reference), ([str(legacy)], native)):
                    arguments = (
                        (["--db", str(db)] if database else [])
                        + list(mode)
                        + ["graphql"]
                        + tail
                    )
                    result = subprocess.run(
                        prefix + arguments,
                        input=data,
                        capture_output=True,
                        cwd=root,
                        env=env,
                        timeout=90,
                    )
                    pair.append(
                        {
                            "code": result.returncode,
                            "stdout": result.stdout.decode(),
                            "stderr": result.stderr.decode(),
                        }
                    )
                # Exact stdout/stderr bytes, exit code, and JSON error identity.
                rows.append(
                    {
                        "name": name,
                        "mode": list(mode),
                        "reference": pair[0],
                        "native": pair[1],
                        "match": pair[0] == pair[1],
                    }
                )

            for name, raw in malformed:
                for mode in ([], ["--json"], ["--machine"]):
                    check(name, ["{profile}", "--variables", raw], mode)
            for name, raw, document in controls:
                for mode in ([], ["--json"], ["--machine"]):
                    check(
                        name,
                        [document] + (["--variables", raw] if raw is not None else []),
                        mode,
                    )
            query = root / "query.graphql"
            query.write_text("{profile}")
            for source in (str(query), "-"):
                check(
                    "file-admitted-variables",
                    ["--file", source, "--variables", "{"],
                    ["--machine"],
                    b"{profile}" if source == "-" else b"",
                )
                oversized = b" " * 131073
                if source != "-":
                    query.write_bytes(oversized)
                check(
                    "file-limit-before-variables",
                    ["--file", source, "--variables", "{"],
                    ["--machine"],
                    oversized if source == "-" else b"",
                )
            check(
                "timeout-before-variables",
                ["{profile}", "--variables", "{", "--timeout-ms", "0"],
                ["--machine"],
            )
            check(
                "timeout-negative-before-variables",
                ["{profile}", "--variables", "{", "--timeout-ms=-1"],
                ["--machine"],
            )
            check(
                "document-before-variables",
                ["{profile}", "--variables", "{", "--schema"],
                ["--machine"],
            )
            check(
                "schema-before-variables",
                ["--schema", "--variables", "{"],
                ["--machine"],
            )
            check(
                "database-before-variables",
                ["{profile}", "--variables", "{"],
                ["--machine"],
                database=False,
            )
            self.assertEqual(
                [
                    hashlib.sha256(path.read_bytes()).hexdigest()
                    for path in (reference, native)
                ],
                before,
            )
        self.assertEqual(
            hashlib.sha256(legacy.read_bytes()).hexdigest(), artifact_before
        )
        self.assertEqual(
            oracle_snapshot(fixture.REFERENCE, fixture.ROOT), oracle_before
        )
        if report := os.environ.get("CATABOLIC_GRAPHQL_VARIABLE_ERRORS_REPORT"):
            Path(report).write_text(
                json.dumps(rows, indent=2, ensure_ascii=False) + "\n"
            )
        mismatch = [row for row in rows if not row["match"]]
        print(
            f"Frozen legacy GraphQL variable pairs: {len(rows)}, mismatches: {len(mismatch)}"
        )
        self.assertFalse(mismatch, json.dumps(mismatch, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    unittest.main()
