# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Two exact syntax/budget promotions and bounded complete-envelope controls."""

import copy
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from tests import test_rust_legacy_parser as fixture
from tests.test_rust_graphql_scoped_parser_diagnostics import (
    SCOPED_PRECEDENCE_PATH,
    promote_scoped_cases,
    scoped_precedence_replacements,
)
from tests.test_rust_graphql_source_locations import ORACLE, TOKEN_CORRECTION_PATH

PRECEDENCE_CORRECTION_PATH = (
    fixture.ROOT / "tests/parity/graphql-parser-budget-precedence-v1.json"
)
PRECEDENCE_CORRECTED_NAMES = [
    "earlier-missing-value-over-limit",
    "earlier-invalid-character-over-limit",
]
PROVENANCE_SHA256 = "545c08d431b5ed1c2fdec15b0fb43d56d36a5dd446510f43b2c2e5ca5860d276"
CASES_SHA256 = "c58cb60a42f5fbd158bfa9380a06e40982a696483c13f52a8c0d198c17664f09"
HISTORICAL_SHA256 = "1c81574203eff294b01a36cf5b5274ade0e7f854f2deae62528a1c255582eac8"


def digest(value):
    return hashlib.sha256(
        json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode()
    ).hexdigest()


def precedence_control_replacements(correction, historical_path=TOKEN_CORRECTION_PATH):
    """Fail closed on exact membership, original inputs and complete envelopes."""
    if correction["contract"] != "native-graphql-parser-budget-precedence-v1":
        raise ValueError("Unexpected parser-budget precedence contract")
    if digest(correction["provenance"]) != PROVENANCE_SHA256:
        raise ValueError("Parser-budget source/oracle provenance changed")
    if hashlib.sha256(historical_path.read_bytes()).hexdigest() != HISTORICAL_SHA256:
        raise ValueError("Historical token-budget fixture changed")
    historical = {
        c["name"]: c for c in json.loads(historical_path.read_text())["cases"]
    }
    replacements = correction["historical_replacements"]
    if [c["historical_control"] for c in replacements] != PRECEDENCE_CORRECTED_NAMES:
        raise ValueError("Only the two exact precedence controls may be replaced")
    for replacement in replacements:
        original = historical[replacement["historical_control"]]
        expected = dict(
            historical_control=original["name"],
            document=original["document"],
            args=original["args"],
            expected=original["reference"],
            baseline=original["baseline"],
        )
        if fixture.strict(replacement) != fixture.strict(expected):
            raise ValueError("Historical precedence input or complete response changed")
        if fixture.strict(original["reference"]) == fixture.strict(
            original["baseline"]["legacy"]
        ):
            raise ValueError("Historical precedence mismatch was relabeled")
    if digest(correction["cases"]) != CASES_SHA256:
        raise ValueError(
            "Frozen precedence case membership or complete envelopes changed"
        )
    return {c["historical_control"]: c for c in replacements}


def promote_precedence_cases(cases, replacements):
    """Compose the same exact corrections into older immutable control suites."""
    promoted = []
    for case in cases:
        if replacement := replacements.get(case["name"]):
            for key, target in (
                ("document", "document"),
                ("args", "args"),
                ("reference", "expected"),
                ("baseline", "baseline"),
            ):
                if fixture.strict(case[key]) != fixture.strict(replacement[target]):
                    raise ValueError(
                        "Historical precedence control differs across suites"
                    )
            case = dict(case, expectation="oracle")
        promoted.append(case)
    return promoted


class ParserBudgetPrecedenceFixtureTest(unittest.TestCase):
    def test_exact_historical_replacements_and_preserved_residuals(self):
        contract = json.loads(PRECEDENCE_CORRECTION_PATH.read_text())
        self.assertEqual(
            list(precedence_control_replacements(contract)), PRECEDENCE_CORRECTED_NAMES
        )
        self.assertEqual(len(contract["cases"]), 68)
        residual = next(
            c for c in contract["cases"] if c["name"] == "dangling-backslash-residual"
        )
        self.assertEqual(residual["expectation"], "baseline")
        self.assertEqual(residual["reference"]["code"], 1)
        self.assertTrue(
            residual["reference"]["stderr"].endswith(
                "IndexError: string index out of range\n"
            )
        )

    def test_scope_inputs_provenance_and_complete_envelopes_fail_closed(self):
        original = json.loads(PRECEDENCE_CORRECTION_PATH.read_text())
        mutations = [
            lambda c: c.update(contract="unversioned"),
            lambda c: c.update(historical_replacements=[]),
            lambda c: c["historical_replacements"].reverse(),
            lambda c: c["historical_replacements"].append(
                c["historical_replacements"][0]
            ),
            lambda c: c["historical_replacements"][0].update(
                historical_control="earlier-missing-value-under-limit"
            ),
            lambda c: c["historical_replacements"][0].update(document="{profile}"),
            lambda c: c["historical_replacements"][0].update(
                args=["--timeout-ms", "0"]
            ),
            lambda c: c["historical_replacements"][0]["expected"].update(code=0),
            lambda c: c["historical_replacements"][0]["expected"].update(
                stderr="changed"
            ),
            lambda c: c["historical_replacements"][0]["expected"]["stdout"].update(
                exit_code=True
            ),
            lambda c: c["historical_replacements"][0]["expected"]["stdout"]["data"][
                "errors"
            ][0].update(locations=[]),
            lambda c: c["historical_replacements"][0]["baseline"]["legacy"].update(
                stderr="changed"
            ),
            lambda c: c["cases"].pop(),
            lambda c: c["cases"].reverse(),
            lambda c: c["cases"][0].update(document="{profile}"),
            lambda c: c["cases"][0].update(expectation="baseline"),
            lambda c: c["cases"][0]["reference"].update(code=0),
            lambda c: c["cases"][0]["baseline"]["companion"].update(stderr="changed"),
        ]
        mutations.extend(
            lambda c, key=key: c["provenance"].update({key: "changed"})
            for key in original["provenance"]
        )
        for index, mutate in enumerate(mutations):
            correction = copy.deepcopy(original)
            mutate(correction)
            with self.subTest(mutation=index), self.assertRaises(ValueError):
                precedence_control_replacements(correction)
        with tempfile.TemporaryDirectory() as temporary:
            changed = Path(temporary) / "historical.json"
            changed.write_bytes(TOKEN_CORRECTION_PATH.read_bytes() + b"\n")
            with self.assertRaises(ValueError):
                precedence_control_replacements(original, changed)


class ParserBudgetPrecedenceTest(unittest.TestCase):
    def test_frozen_complete_legacy_companion_and_oracle_envelopes(self):
        contract = json.loads(PRECEDENCE_CORRECTION_PATH.read_text())
        precedence_control_replacements(contract)
        legacy = Path(os.environ["CATABOLIC_LEGACY_BINARY"])
        companion = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        for path in (legacy, companion, fixture.PYTHON):
            self.assertTrue(path.is_file(), str(path))
        from scripts.qualify_native_mutations import oracle_snapshot

        snapshot = oracle_snapshot(fixture.REFERENCE, fixture.ROOT)
        for key in ("reference_commit", "inventory_sha256", "resource_sha256"):
            self.assertEqual(
                snapshot[key], contract["provenance"]["oracle_snapshot"][key]
            )
        with tempfile.TemporaryDirectory(prefix="parser-precedence-") as temporary:
            root = Path(temporary)
            env = fixture.environment(
                overrides={"HOME": str(root / "home"), "PYTHONDONTWRITEBYTECODE": "1"}
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
            self.assertEqual(
                (version.returncode, version.stdout, version.stderr),
                (0, "3.14.6\n3.2.12\n", ""),
            )
            reference, native = root / "reference.db", root / "native.db"
            initialized = subprocess.run(
                oracle + ["--db", str(reference), "init"],
                env=env,
                capture_output=True,
                timeout=90,
            )
            self.assertEqual((initialized.returncode, initialized.stderr), (0, b""))
            shutil.copyfile(reference, native)
            before = {
                p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                for p in (reference, native)
            }
            results = []
            scoped = scoped_precedence_replacements(
                json.loads(SCOPED_PRECEDENCE_PATH.read_text())
            )
            for case in promote_scoped_cases(contract["cases"], scoped):
                actual = {}
                for kind, prefix, db, flag in [
                    ("reference", oracle, reference, "--machine"),
                    ("legacy", [str(legacy)], native, "--machine"),
                    ("companion", [str(companion)], native, "--json"),
                ]:
                    process = subprocess.run(
                        prefix
                        + [
                            "--db",
                            str(db),
                            flag,
                            "graphql",
                            case["document"],
                            *case["args"],
                        ],
                        env=env,
                        capture_output=True,
                        text=True,
                        timeout=90,
                    )
                    try:
                        stdout = json.loads(process.stdout)
                    except json.JSONDecodeError:
                        stdout = process.stdout
                    actual[kind] = dict(
                        code=process.returncode, stdout=stdout, stderr=process.stderr
                    )
                if case["expectation"] == "oracle":
                    expected = dict(
                        reference=case["reference"],
                        legacy=case["reference"],
                        companion=dict(
                            code=0,
                            stderr="",
                            stdout=case["reference"]["stdout"]["data"],
                        ),
                    )
                else:
                    self.assertEqual(case["expectation"], "baseline")
                    expected = dict(reference=case["reference"], **case["baseline"])
                match = fixture.strict(actual) == fixture.strict(expected)
                results.append(
                    dict(
                        name=case["name"],
                        expectation=case["expectation"],
                        results=actual,
                        match=match,
                    )
                )
                with self.subTest(case=case["name"]):
                    self.assertEqual(fixture.strict(actual), fixture.strict(expected))
            after = {
                p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                for p in (reference, native)
            }
            self.assertEqual(before, after)
            if report := os.environ.get("CATABOLIC_PARSER_BUDGET_PRECEDENCE_REPORT"):
                Path(report).write_text(
                    json.dumps(
                        dict(
                            cases=results,
                            database_sha256_before=before,
                            database_sha256_after=after,
                        ),
                        indent=2,
                    )
                    + "\n"
                )
            print(
                f"Parser-budget precedence complete-process-envelope cases: {len(results)}"
            )


if __name__ == "__main__":
    unittest.main()
