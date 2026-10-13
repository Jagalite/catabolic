# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Complete frozen document-validation responses and residual overlap controls.

The frozen Python 3.14.6 checkout is an independent test oracle only. Run against
explicit native legacy and companion artifacts; no service or network is used.
The original three failing documents and variable values stay verbatim.
Historical overlap failures stay recorded; live overlap responses now match frozen.
"""

import concurrent.futures
import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from tests import test_rust_legacy_parser as fixture

FIXTURE = json.loads(
    (fixture.ROOT / "tests/parity/graphql-document-validation.json").read_text()
)
CASES = FIXTURE["cases"]
ORACLE = "import sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"


class DocumentValidationTest(unittest.TestCase):
    def test_original_witnesses_and_residual_controls_are_preserved(self):
        original = json.loads(
            (
                fixture.ROOT / "tests/parity/graphql-validation-remaining-gaps.json"
            ).read_text()
        )
        current = {case["name"]: case for case in CASES}
        for old in original["cases"]:
            new = current[old["name"]]
            for key in ("document", "variables", "expected"):
                self.assertEqual(new[key], old[key])
            self.assertEqual(new["pre_fix_observed"], old["observed"])
            self.assertNotEqual(new["expected"], new["pre_fix_observed"])
        for case in FIXTURE["remaining_overlap_controls"]:
            self.assertEqual(case["native"], case["pre_fix_observed"])
            self.assertEqual(case["native"]["code"], 2)
            self.assertIsNone(case["native"]["stdout"]["data"]["data"])
            self.assertNotIn("extensions", case["native"]["stdout"]["data"])
            self.assertNotEqual(case["native"]["stdout"], case["expected"])

    def test_document_byte_boundary_on_both_native_cli_surfaces(self):
        legacy = Path(os.environ["CATABOLIC_LEGACY_BINARY"])
        companion = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        with tempfile.TemporaryDirectory(
            prefix="graphql-validation-bytes-"
        ) as temporary:
            root = Path(temporary)
            reference, native = root / "reference.db", root / "native.db"
            env = fixture.environment(
                overrides={
                    "HOME": str(root / "home"),
                    "PYTHONPYCACHEPREFIX": str(root / "pycache"),
                }
            )
            oracle = [str(fixture.PYTHON), "-c", ORACLE, str(fixture.REFERENCE / "src")]
            initialized = subprocess.run(
                oracle + ["--db", str(reference), "init"],
                capture_output=True,
                text=True,
                env=env,
                timeout=90,
            )
            self.assertEqual(initialized.returncode, 0, initialized.stderr)
            shutil.copyfile(reference, native)
            before = {
                path: hashlib.sha256(path.read_bytes()).hexdigest()
                for path in (reference, native)
            }
            boundary = json.loads(
                (
                    fixture.ROOT / "tests/parity/graphql-file-input-remaining-gap.json"
                ).read_text()
            )
            base = boundary["document_prefix"]
            witness = base + " " * boundary["trailing_ascii_spaces"]
            self.assertEqual(len(witness.encode()), boundary["document_bytes"])
            self.assertEqual(
                hashlib.sha256(witness.encode()).hexdigest(),
                boundary["document_sha256"],
            )
            for size in (131072, 131073):
                document = base + " " * (size - len(base.encode()))
                results = []
                for prefix, db in ((oracle, reference), ([str(legacy)], native)):
                    result = subprocess.run(
                        prefix
                        + ["--db", str(db), "--machine", "graphql", "--file", "-"],
                        input=document,
                        capture_output=True,
                        text=True,
                        env=env,
                        timeout=90,
                    )
                    results.append(
                        dict(
                            code=result.returncode,
                            stderr=result.stderr,
                            stdout=json.loads(result.stdout),
                        )
                    )
                if size == 131072:
                    self.assertEqual(
                        fixture.strict(results[0]), fixture.strict(results[1])
                    )
                else:
                    # CLI file admission now matches; the historical response stays
                    # recorded separately from the companion engine boundary.
                    self.assertEqual(results[0], boundary["expected"])
                    self.assertEqual(results[1], boundary["expected"])
                # The companion stdio surface avoids the OS argv byte ceiling.
                request = ["--db", str(native), "--json", "graphql", document]
                result = subprocess.run(
                    [str(companion), "--stdio"],
                    input=json.dumps(request) + "\n",
                    capture_output=True,
                    text=True,
                    env=env,
                    timeout=90,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stderr, "")
                reply = json.loads(result.stdout)
                if size == 131072:
                    self.assertEqual(results[0]["stdout"], CASES[1]["expected"])
                    self.assertEqual(
                        reply, dict(status=0, result=CASES[1]["expected"]["data"])
                    )
                else:
                    self.assertEqual(
                        results[0]["stdout"]["data"]["error"]["message"],
                        "input exceeds 131072 bytes",
                    )
                    self.assertEqual(
                        reply,
                        dict(
                            status=2,
                            error="catabolic: query document must be at most 131072 bytes",
                        ),
                    )
            self.assertEqual(
                {
                    path: hashlib.sha256(path.read_bytes()).hexdigest()
                    for path in (reference, native)
                },
                before,
            )

    def test_frozen_complete_legacy_and_companion_responses(self):
        legacy = Path(os.environ["CATABOLIC_LEGACY_BINARY"])
        companion = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        for artifact in (legacy, companion, fixture.PYTHON):
            self.assertTrue(artifact.is_file(), str(artifact))
        from scripts.qualify_native_mutations import oracle_snapshot

        snapshot = oracle_snapshot(fixture.REFERENCE, fixture.ROOT)
        self.assertEqual(
            snapshot["reference_commit"], FIXTURE["provenance"]["reference_commit"]
        )
        with tempfile.TemporaryDirectory(
            prefix="graphql-document-validation-"
        ) as temporary:
            root = Path(temporary)
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
                    "import graphql,sys;print(sys.version.split()[0]);print(graphql.__version__)",
                ],
                env=env,
                capture_output=True,
                text=True,
                timeout=90,
            )
            self.assertEqual(version.returncode, 0, version.stderr)
            self.assertEqual(version.stderr, "")
            self.assertEqual(
                version.stdout.splitlines(),
                [
                    FIXTURE["provenance"]["python"],
                    FIXTURE["provenance"]["graphql_core"],
                ],
            )
            databases = {}
            for name, catalog in FIXTURE["catalogs"].items():
                reference, native = (
                    root / f"{name}-reference.db",
                    root / f"{name}-native.db",
                )
                initialized = subprocess.run(
                    oracle + ["--db", str(reference), "init"],
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=90,
                )
                self.assertEqual(initialized.returncode, 0, initialized.stderr)
                self.assertEqual(initialized.stderr, "")
                with closing(sqlite3.connect(reference)) as connection, connection:
                    connection.executemany(
                        "INSERT INTO items(id,kind,metadata) VALUES(?,'movie','{}')",
                        [(item_id,) for item_id in catalog["item_ids"]],
                    )
                shutil.copyfile(reference, native)
                databases[name] = dict(reference=reference, native=native)
            paths = [path for pair in databases.values() for path in pair.values()]
            before = {
                path: hashlib.sha256(path.read_bytes()).hexdigest() for path in paths
            }

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

            def compare(case, control=False):
                args = [
                    "graphql",
                    case["document"],
                    "--variables",
                    json.dumps(case["variables"]),
                ]
                db = databases[case["catalog"]]
                expected = dict(
                    code=case["exit_code"],
                    stderr=case["stderr"],
                    stdout=case["expected"],
                )
                results = {
                    "reference": invoke(oracle, db["reference"], ["--machine", *args]),
                    "legacy": invoke([str(legacy)], db["native"], ["--machine", *args]),
                    "companion": invoke(
                        [str(companion)], db["native"], ["--json", *args]
                    ),
                }
                native = expected
                companion_expected = dict(
                    code=0, stderr="", stdout=native["stdout"]["data"]
                )
                return dict(
                    name=case["name"],
                    category="resolved-overlap" if control else "parity",
                    results=results,
                    match=(
                        fixture.strict(results["reference"]) == fixture.strict(expected)
                        and fixture.strict(results["legacy"]) == fixture.strict(native)
                        and fixture.strict(results["companion"])
                        == fixture.strict(companion_expected)
                    ),
                )

            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
                results = list(executor.map(compare, CASES))
                controls = list(
                    executor.map(
                        lambda case: compare(case, control=True),
                        FIXTURE["remaining_overlap_controls"],
                    )
                )
            if report := os.environ.get("CATABOLIC_GRAPHQL_DOCUMENT_VALIDATION_REPORT"):
                Path(report).write_text(
                    json.dumps(
                        dict(parity=results, remaining_controls=controls), indent=2
                    )
                    + "\n"
                )
            mismatches = [
                result for result in results + controls if not result["match"]
            ]
            self.assertFalse(mismatches, json.dumps(mismatches, indent=2))
            self.assertEqual(
                {path: hashlib.sha256(path.read_bytes()).hexdigest() for path in paths},
                before,
            )
            print(
                f"Frozen document-validation CLI pairs: {len(results)}; "
                f"resolved overlap controls: {len(controls)}"
            )


if __name__ == "__main__":
    unittest.main()
