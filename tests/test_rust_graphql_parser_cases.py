# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Complete frozen parser-case responses, with explicit non-parity controls.

The frozen Python 3.14.6 checkout is an independent test oracle only. Run against
explicit native legacy and companion artifacts; no service or network is used.
Unicode-seeded catalogs prove decoded values, rather than merely successful
parsing. Empty catalogs preserve the original three failing witnesses verbatim.
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
    (fixture.ROOT / "tests/parity/graphql-parser-cases.json").read_text()
)
CASES = FIXTURE["cases"]
ORACLE = "import sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"


class ParserCasesTest(unittest.TestCase):
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
        with tempfile.TemporaryDirectory(prefix="graphql-parser-cases-") as temporary:
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
                native = case["native"] if control else expected
                companion_expected = dict(
                    code=0, stderr="", stdout=native["stdout"]["data"]
                )
                return dict(
                    name=case["name"],
                    category="remaining-control" if control else "parity",
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
                        FIXTURE["remaining_parser_controls"],
                    )
                )
            if report := os.environ.get("CATABOLIC_GRAPHQL_PARSER_CASES_REPORT"):
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
                f"Frozen parser CLI pairs: {len(results)}; "
                f"remaining non-parity controls: {len(controls)}"
            )


if __name__ == "__main__":
    unittest.main()
