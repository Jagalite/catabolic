# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen/native grouped GraphQL execution through both standalone CLI surfaces.

Python 3.14.6/frozen 453fca9 is an independent oracle only. Disposable populated
catalogs are copied for each owner; no wheel, wrapper or real service is used.
"""

import concurrent.futures
import hashlib
import json
import os
import subprocess
import unittest
from pathlib import Path

from tests import test_rust_legacy_inbox as fixture

FIXTURE = json.loads(
    (fixture.ROOT / "tests/parity/graphql-runtime-completion.json").read_text()
)


class RuntimeCompletionTest(unittest.TestCase):
    setUp = fixture.NativeLegacyInboxTest.setUp
    invoke = fixture.NativeLegacyInboxTest.invoke

    def assert_exact(self, actual, expected):
        # Preserve scalar kinds; True == 1 must not hide a completion regression.
        self.assertEqual(
            fixture.parser_fixture.strict(actual),
            fixture.parser_fixture.strict(expected),
        )

    def test_complete_frozen_legacy_companion_responses_and_conflict_controls(self):
        companion = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        self.assertTrue(companion.is_file())
        from scripts.qualify_native_mutations import oracle_snapshot

        before_reference = oracle_snapshot(
            fixture.parser_fixture.REFERENCE, fixture.ROOT
        )
        self.assert_exact(
            before_reference["reference_commit"],
            FIXTURE["provenance"]["reference_commit"],
        )
        version = subprocess.run(
            [
                str(fixture.parser_fixture.PYTHON),
                "-c",
                "import sys,graphql;print(sys.version.split()[0]);print(graphql.__version__)",
            ],
            capture_output=True,
            text=True,
            env=self.env,
            check=True,
        )
        self.assert_exact(
            version.stdout.splitlines(),
            [FIXTURE["provenance"]["python"], FIXTURE["provenance"]["graphql_core"]],
        )
        self.assert_exact(version.stderr, "")
        paths = [self.reference, self.native]
        before = {
            str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths
        }

        def invoke(case):
            arguments = [
                "graphql",
                case["document"],
                "--variables",
                json.dumps(case["variables"]),
            ]
            reference, native = (
                self.invoke(True, ["--machine", *arguments]),
                self.invoke(False, ["--machine", *arguments]),
            )
            companion_run = subprocess.run(
                [str(companion), "--db", str(self.native), "--json", *arguments],
                env=self.env,
                capture_output=True,
                text=True,
                timeout=90,
            )
            results = {
                name: dict(
                    code=run.returncode,
                    stderr=run.stderr,
                    stdout=json.loads(run.stdout),
                )
                for name, run in [
                    ("reference", reference),
                    ("legacy", native),
                    ("companion", companion_run),
                ]
            }
            return dict(name=case["name"], results=results)

        all_cases = FIXTURE["cases"] + FIXTURE["validation_controls"]
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
            results = list(executor.map(invoke, all_cases))
        if report := os.environ.get("CATABOLIC_GRAPHQL_RUNTIME_REPORT"):
            Path(report).write_text(
                json.dumps(
                    dict(provenance=FIXTURE["provenance"], results=results), indent=2
                )
                + "\n"
            )
        for case, result in zip(all_cases, results, strict=True):
            with self.subTest(case=case["name"]):
                expected = dict(
                    code=case["exit_code"],
                    stderr=case["stderr"],
                    stdout=case["expected"],
                )
                self.assert_exact(result["results"]["reference"], expected)
                self.assert_exact(result["results"]["legacy"], expected)
                if case in FIXTURE["validation_controls"]:
                    # Historical generic rejections stay immutable in the fixture.
                    self.assertNotEqual(case["native"], expected)
                    self.assertIsNone(expected["stdout"]["data"]["data"])
                    self.assertNotIn("extensions", expected["stdout"]["data"])
                self.assert_exact(
                    result["results"]["companion"],
                    dict(code=0, stderr="", stdout=expected["stdout"]["data"]),
                )
        self.assert_exact(
            {
                str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in paths
            },
            before,
        )
        self.assert_exact(
            oracle_snapshot(fixture.parser_fixture.REFERENCE, fixture.ROOT),
            before_reference,
        )
        print(
            f"Frozen runtime completion CLI pairs: {len(FIXTURE['cases'])}; strict-validation controls: {len(FIXTURE['validation_controls'])}"
        )

    def test_explicit_recorded_scalar_output_differences(self):
        import sqlite3
        from contextlib import closing

        controls = json.loads(
            (
                fixture.ROOT / "tests/parity/graphql-runtime-scalar-remaining.json"
            ).read_text()
        )
        companion = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        for case in controls["cases"]:
            for path in [self.reference, self.native]:
                with closing(sqlite3.connect(path)) as db, db:
                    db.execute("DELETE FROM item_relationships")
                    db.execute(
                        "INSERT INTO item_relationships(id,source_id,target_id,kind,position) VALUES('r','a','b','related',?)",
                        (case["position_value"],),
                    )
            args = ["graphql", case["document"]]
            for reference, key in [(True, "reference"), (False, "native")]:
                run = self.invoke(reference, ["--machine", *args])
                self.assert_exact(
                    dict(
                        code=run.returncode,
                        stderr=run.stderr,
                        stdout=json.loads(run.stdout),
                    ),
                    case[key],
                )
            run = subprocess.run(
                [str(companion), "--db", str(self.native), "--json", *args],
                env=self.env,
                capture_output=True,
                text=True,
                timeout=90,
            )
            self.assert_exact(run.returncode, 0)
            self.assert_exact(run.stderr, "")
            self.assert_exact(json.loads(run.stdout), case["native"]["stdout"]["data"])
            self.assertNotEqual(case["native"], case["reference"])
            self.assertNotEqual(case["baseline"], case["reference"])

    def test_opaque_json_numbers_preserve_exact_frozen_results(self):
        import sqlite3
        from contextlib import closing

        controls = json.loads(
            (
                fixture.ROOT / "tests/parity/graphql-runtime-json-completion.json"
            ).read_text()
        )
        companion = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        for case in controls["cases"]:
            with self.subTest(case=case["name"]):
                for path in [self.reference, self.native]:
                    with closing(sqlite3.connect(path)) as db, db:
                        db.execute(
                            "UPDATE items SET metadata=? WHERE id='a'",
                            (case["metadata_json"],),
                        )
                before = {
                    str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                    for path in [self.reference, self.native]
                }
                args = ["graphql", case["document"]]
                for reference in [True, False]:
                    run = self.invoke(reference, ["--machine", *args])
                    self.assert_exact(
                        dict(
                            code=run.returncode,
                            stderr=run.stderr,
                            stdout=json.loads(run.stdout),
                        ),
                        case["expected"],
                    )
                run = subprocess.run(
                    [str(companion), "--db", str(self.native), "--json", *args],
                    env=self.env,
                    capture_output=True,
                    text=True,
                    timeout=90,
                )
                self.assert_exact(
                    dict(
                        code=run.returncode,
                        stderr=run.stderr,
                        stdout=json.loads(run.stdout),
                    ),
                    dict(code=0, stderr="", stdout=case["expected"]["stdout"]["data"]),
                )
                self.assert_exact(
                    {
                        str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                        for path in [self.reference, self.native]
                    },
                    before,
                )


if __name__ == "__main__":
    unittest.main()
