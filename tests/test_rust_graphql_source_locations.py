# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen complete source-location pairs on disposable empty catalogs.

Uses the explicitly selected reference interpreter and native legacy/companion
artifacts. This does not qualify runtime or syntax-error locations generally.
"""

import concurrent.futures
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from tests import test_rust_legacy_parser as fixture

FIXTURE = json.loads(
    (fixture.ROOT / "tests/parity/graphql-source-locations.json").read_text()
)
CASES = FIXTURE["cases"]
ORACLE = "import sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"


class SourceLocationTest(unittest.TestCase):
    def test_frozen_complete_legacy_and_companion_responses(self):
        legacy = Path(os.environ["CATABOLIC_LEGACY_BINARY"])
        companion = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        self.assertTrue(legacy.is_file())
        self.assertTrue(companion.is_file())
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            reference, native = root / "reference.db", root / "native.db"
            env = fixture.environment(overrides={"HOME": str(root / "home")})
            oracle = [str(fixture.PYTHON), "-c", ORACLE, str(fixture.REFERENCE / "src")]

            def invoke(prefix, db, args):
                result = subprocess.run(
                    prefix + ["--db", str(db)] + args,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=90,
                )
                return dict(
                    code=result.returncode,
                    stderr=result.stderr,
                    stdout=json.loads(result.stdout),
                )

            initialized = subprocess.run(
                oracle + ["--db", str(reference), "init"],
                env=env,
                capture_output=True,
                text=True,
                timeout=90,
            )
            self.assertEqual(initialized.returncode, 0, initialized.stderr)
            shutil.copyfile(reference, native)
            before = hashlib.sha256(native.read_bytes()).hexdigest()

            def compare(case):
                args = [
                    "graphql",
                    case["document"],
                    "--variables",
                    json.dumps(case["variables"]),
                ]
                expected = dict(
                    code=case["exit_code"],
                    stderr=case["stderr"],
                    stdout=case["expected"],
                )
                results = {
                    "reference": invoke(oracle, reference, ["--machine", *args]),
                    "legacy": invoke([str(legacy)], native, ["--machine", *args]),
                    "companion": invoke([str(companion)], native, ["--json", *args]),
                }
                return dict(
                    name=case["name"],
                    results=results,
                    match=(
                        results["reference"] == results["legacy"] == expected
                        and results["companion"]
                        == dict(code=0, stderr="", stdout=case["expected"]["data"])
                    ),
                )

            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
                results = list(executor.map(compare, CASES))
            if report := os.environ.get("CATABOLIC_SOURCE_LOCATION_REPORT"):
                Path(report).write_text(json.dumps(results, indent=2) + "\n")
            self.assertFalse(
                [r for r in results if not r["match"]],
                json.dumps([r for r in results if not r["match"]], indent=2),
            )
            # These preserve the original native result; syntax diagnostics and
            # token-budget locations are separate paths, not new parity claims.
            for case in FIXTURE["unchanged_parser_and_budget_controls"]:
                args = ["--machine", "graphql", case["document"]]
                with self.subTest(unchanged=case["name"]):
                    self.assertEqual(invoke(oracle, reference, args), case["reference"])
                    self.assertEqual(
                        invoke([str(legacy)], native, args), case["native"]
                    )
            self.assertEqual(hashlib.sha256(native.read_bytes()).hexdigest(), before)
            print(f"Frozen source-location CLI pairs: {len(results)}")


if __name__ == "__main__":
    unittest.main()
