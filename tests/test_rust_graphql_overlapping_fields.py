# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen overlap contracts through standalone legacy and companion executables.

The frozen Python checkout is a test oracle only. Historical witnesses are kept
verbatim and accepted groups are compared as complete responses, including
record budgets and scalar kinds. No wrapper or installed Python runtime is used.
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
    (fixture.ROOT / "tests/parity/graphql-overlapping-fields.json").read_text()
)
ORACLE = "import sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"


class OverlappingFieldsTest(unittest.TestCase):
    def test_historical_inputs_and_failures_are_preserved(self):
        cases = {case["name"]: case for case in FIXTURE["cases"]}
        for filename, key in [
            ("graphql-document-validation.json", "remaining_overlap_controls"),
            ("graphql-runtime-completion.json", "validation_controls"),
        ]:
            original = json.loads(
                (fixture.ROOT / "tests/parity" / filename).read_text()
            )
            for old in original[key]:
                new = cases["historical-" + old["name"]]
                for field in (
                    "document",
                    "variables",
                    "expected",
                    "exit_code",
                    "stderr",
                ):
                    self.assertEqual(
                        fixture.strict(new[field]), fixture.strict(old[field])
                    )
                self.assertEqual(new["pre_fix_observed"], old["native"])
                self.assertNotEqual(new["pre_fix_observed"]["stdout"], new["expected"])

        parser = cases["historical-parser-precedence-8"]
        self.assertEqual(parser["document"], parser["historical_probe"]["document"])
        self.assertEqual(parser["variables"], parser["historical_probe"]["variables"])
        self.assertEqual(
            parser["expected"], parser["historical_probe"]["reference"]["stdout"]
        )
        self.assertEqual(
            parser["pre_fix_observed"], parser["historical_probe"]["legacy"]
        )

        sibling = cases["historical-conflicting-types"]
        composed = sibling["composed_pre_fix_observed"]["results"]
        self.assertEqual(composed["legacy"]["stdout"], sibling["expected"])
        self.assertEqual(composed["companion"]["stdout"], sibling["expected"]["data"])
        self.assertNotEqual(composed["legacy"], sibling["pre_fix_observed"])

    def test_frozen_legacy_companion_complete_responses(self):
        from scripts.qualify_native_mutations import oracle_snapshot

        legacy = Path(os.environ["CATABOLIC_LEGACY_BINARY"])
        companion = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        before_source = oracle_snapshot(fixture.REFERENCE, fixture.ROOT)
        self.assertEqual(
            before_source["reference_commit"], FIXTURE["provenance"]["reference_commit"]
        )
        with tempfile.TemporaryDirectory(prefix="graphql-overlap-") as directory:
            root = Path(directory)
            reference, native = root / "reference.db", root / "native.db"
            env = fixture.environment(
                overrides={
                    "HOME": str(root / "home"),
                    "PYTHONPYCACHEPREFIX": str(root / "pycache"),
                }
            )
            oracle = [str(fixture.PYTHON), "-c", ORACLE, str(fixture.REFERENCE / "src")]
            version = subprocess.run(
                [
                    str(fixture.PYTHON),
                    "-c",
                    "import sys,graphql;print(sys.version.split()[0]);print(graphql.__version__)",
                ],
                env=env,
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertEqual(
                version.stdout.splitlines(),
                [
                    FIXTURE["provenance"]["python"],
                    FIXTURE["provenance"]["graphql_core"],
                ],
            )
            self.assertEqual(version.stderr, "")
            initialized = subprocess.run(
                oracle + ["--db", str(reference), "init"],
                env=env,
                capture_output=True,
                text=True,
                timeout=90,
            )
            self.assertEqual(initialized.returncode, 0, initialized.stderr)
            self.assertEqual(initialized.stderr, "")
            shutil.copyfile(reference, native)
            before = {
                str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in (reference, native)
            }

            def invoke(case):
                args = [
                    "graphql",
                    case["document"],
                    "--variables",
                    json.dumps(case["variables"]),
                ]
                results = {}
                for name, prefix, db, mode in [
                    ("reference", oracle, reference, "--machine"),
                    ("legacy", [str(legacy)], native, "--machine"),
                    ("companion", [str(companion)], native, "--json"),
                ]:
                    run = subprocess.run(
                        prefix + ["--db", str(db), mode, *args],
                        env=env,
                        capture_output=True,
                        text=True,
                        timeout=90,
                    )
                    results[name] = dict(
                        code=run.returncode,
                        stderr=run.stderr,
                        stdout=json.loads(run.stdout),
                    )
                expected = dict(
                    code=case["exit_code"],
                    stderr=case["stderr"],
                    stdout=case["expected"],
                )
                companion_expected = dict(
                    code=0, stderr="", stdout=case["expected"]["data"]
                )
                return dict(
                    name=case["name"],
                    results=results,
                    match=fixture.strict(results["reference"])
                    == fixture.strict(expected)
                    and fixture.strict(results["legacy"]) == fixture.strict(expected)
                    and fixture.strict(results["companion"])
                    == fixture.strict(companion_expected),
                )

            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
                results = list(executor.map(invoke, FIXTURE["cases"]))
            if report := os.environ.get("CATABOLIC_GRAPHQL_OVERLAP_REPORT"):
                Path(report).write_text(
                    json.dumps(
                        dict(
                            provenance=FIXTURE["provenance"],
                            artifacts={
                                str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                                for path in (legacy, companion)
                            },
                            results=results,
                        ),
                        indent=2,
                    )
                    + "\n"
                )
            failures = [result for result in results if not result["match"]]
            self.assertFalse(failures, json.dumps(failures, indent=2))
            self.assertEqual(
                {
                    str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                    for path in (reference, native)
                },
                before,
            )
        self.assertEqual(
            oracle_snapshot(fixture.REFERENCE, fixture.ROOT), before_source
        )
        print(f"Frozen overlapping-field CLI pairs: {len(FIXTURE['cases'])}")


if __name__ == "__main__":
    unittest.main()
