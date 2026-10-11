# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen accepted-literal diagnostic pairs, using disposable empty catalogs.

Compare the complete legacy CLI response, exit code, and stderr, and separately
check the durable captured errors. No parser-parity claim or real service use.
"""

import concurrent.futures
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from tests import test_rust_legacy_parser as parser_fixture

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(
    os.environ.get(
        "CATABOLIC_LEGACY_BINARY", ROOT / "target/debug/catabolic-legacy-native"
    )
)
ORACLE = "import sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"


class NativeGraphqlLiteralDiagnosticsTest(unittest.TestCase):
    def test_frozen_literal_diagnostic_pairs(self):
        self.assertTrue(BINARY.is_file(), "build the explicit native legacy CLI")
        cases = json.loads(
            (ROOT / "tests/parity/graphql-literal-diagnostics.json").read_text()
        )["cases"]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            reference, native = root / "reference.db", root / "native.db"
            env = parser_fixture.environment()
            env["HOME"] = str(root / "home")

            def invoke(is_reference, args):
                prefix = (
                    [
                        str(parser_fixture.PYTHON),
                        "-c",
                        ORACLE,
                        str(parser_fixture.REFERENCE / "src"),
                    ]
                    if is_reference
                    else [str(BINARY)]
                )
                result = subprocess.run(
                    prefix
                    + ["--db", str(reference if is_reference else native)]
                    + args,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=90,
                )
                return {
                    "code": result.returncode,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                }

            initialized = invoke(True, ["init"])
            self.assertEqual(initialized["code"], 0, initialized)
            shutil.copyfile(reference, native)

            def compare(case):
                args = ["--machine", "graphql", case["document"], "--variables", "{}"]
                expected, observed = invoke(True, args), invoke(False, args)
                # JSON property order is not a semantic difference; error order,
                # spelling, locations and the complete command envelope are.
                for response in (expected, observed):
                    response["stdout"] = json.loads(response["stdout"])
                return {
                    "name": case["name"],
                    "document": case["document"],
                    "expected": expected,
                    "observed": observed,
                    "match": expected == observed,
                    "fixture_match": expected["stdout"]["data"] == case["expected"],
                }

            with concurrent.futures.ThreadPoolExecutor(max_workers=6) as executor:
                results = list(executor.map(compare, cases))
            report = os.environ.get("CATABOLIC_GRAPHQL_LITERAL_REPORT")
            if report:
                Path(report).write_text(
                    json.dumps(results, ensure_ascii=False, indent=2) + "\n"
                )
            failures = [
                result
                for result in results
                if not result["match"] or not result["fixture_match"]
            ]
            self.assertFalse(
                failures, json.dumps(failures, ensure_ascii=False, indent=2)
            )
            print(f"Frozen GraphQL literal diagnostic pairs: {len(results)}")


if __name__ == "__main__":
    unittest.main()
