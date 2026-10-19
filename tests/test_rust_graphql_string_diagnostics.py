# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Versioned lexical-string repairs; immutable original captures and residuals."""

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
from tests.test_rust_graphql_parser_budget_precedence import (
    PRECEDENCE_CORRECTION_PATH,
    precedence_control_replacements,
    promote_precedence_cases,
)
from tests.test_rust_graphql_scoped_parser_diagnostics import (
    OWNERSHIP_PATH,
    ownership_control_replacements,
    promote_scoped_cases,
)
from tests.test_rust_graphql_source_locations import ORACLE

STRING_CORRECTION_PATH = (
    fixture.ROOT / "tests/parity/graphql-string-diagnostics-v1.json"
)
STRING_CORRECTED_NAMES = ["unterminated-string-not-expanded"]
# Filled from independently captured oracle/baseline process envelopes.
PROVENANCE_SHA256 = "e3afc8db27af3923e3505682a757df6f2bd4588af44a0221c591cd23626ef8ff"
CASES_SHA256 = "9b4f556b79001b5f4f852cba6f1c38e66fde06b68184b1f6d55cd764bce815c3"


def digest(value):
    return hashlib.sha256(
        json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode()
    ).hexdigest()


def string_control_replacements(correction, parity_root=fixture.ROOT / "tests/parity"):
    """Admit one exact historical repair, without relabeling other residuals."""
    if correction["contract"] != "native-graphql-string-diagnostics-v1":
        raise ValueError("Unexpected string-diagnostic contract")
    if digest(correction["provenance"]) != PROVENANCE_SHA256:
        raise ValueError("String-diagnostic source/oracle provenance changed")
    path = parity_root / "graphql-token-budget-locations-v1.json"
    if (
        hashlib.sha256(path.read_bytes()).hexdigest()
        != correction["provenance"]["historical_token_budget_fixture_sha256"]
    ):
        raise ValueError("Preserved token-budget captures changed")
    replacements = correction["historical_replacements"]
    if [c["historical_control"] for c in replacements] != STRING_CORRECTED_NAMES:
        raise ValueError("Only the named unterminated-string control may be replaced")
    historical = {c["name"]: c for c in json.loads(path.read_text())["cases"]}
    for replacement in replacements:
        original = historical[replacement["historical_control"]]
        expected = dict(
            historical_control=original["name"],
            document=original["document"],
            args=original["args"],
            expected=original["reference"],
        )
        if fixture.strict(replacement) != fixture.strict(expected):
            raise ValueError("Historical string input or complete response changed")
        if fixture.strict(original["reference"]) == fixture.strict(
            original["baseline"]["legacy"]
        ):
            raise ValueError("Historical string mismatch was relabeled")
    if digest(correction["cases"]) != CASES_SHA256:
        raise ValueError("Frozen string cases, membership or process envelopes changed")
    return {c["historical_control"]: c for c in replacements}


class StringDiagnosticFixtureTest(unittest.TestCase):
    def test_exact_historical_replacement_and_preserved_oracle_crash(self):
        contract = json.loads(STRING_CORRECTION_PATH.read_text())
        self.assertEqual(
            list(string_control_replacements(contract)), STRING_CORRECTED_NAMES
        )
        self.assertEqual(len(contract["cases"]), 56)
        case = next(
            c for c in contract["cases"] if c["name"] == "ordinary-backslash-eof"
        )
        self.assertEqual(case["expectation"], "baseline")
        self.assertEqual(case["reference"]["code"], 1)
        self.assertEqual(case["reference"]["stdout"], "")
        self.assertTrue(
            case["reference"]["stderr"].endswith(
                "IndexError: string index out of range\n"
            )
        )
        self.assertEqual(case["baseline"]["legacy"]["code"], 2)

    def test_scope_inputs_provenance_and_complete_envelopes_fail_closed(self):
        original = json.loads(STRING_CORRECTION_PATH.read_text())
        mutations = [
            lambda c: c.update(contract="unversioned"),
            lambda c: c.update(historical_replacements=[]),
            lambda c: c["historical_replacements"].append(
                c["historical_replacements"][0]
            ),
            lambda c: c["historical_replacements"][0].update(
                historical_control="earlier-missing-value-over-limit"
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
            lambda c: c["cases"].pop(),
            lambda c: c["cases"].reverse(),
            lambda c: c["cases"][0].update(expectation="baseline"),
            lambda c: c["cases"][0]["baseline"]["legacy"].update(stderr="changed"),
        ]
        mutations.extend(
            lambda c, key=key: c["provenance"].update({key: "changed"})
            for key in original["provenance"]
        )
        for index, mutate in enumerate(mutations):
            correction = copy.deepcopy(original)
            mutate(correction)
            with self.subTest(mutation=index), self.assertRaises(ValueError):
                string_control_replacements(correction)


class StringDiagnosticTest(unittest.TestCase):
    def test_frozen_complete_legacy_companion_and_oracle_envelopes(self):
        contract = json.loads(STRING_CORRECTION_PATH.read_text())
        string_control_replacements(contract)
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
        with tempfile.TemporaryDirectory(prefix="string-diagnostics-") as temporary:
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
            replacements = precedence_control_replacements(
                json.loads(PRECEDENCE_CORRECTION_PATH.read_text())
            )
            cases = promote_precedence_cases(contract["cases"], replacements)
            ownership = ownership_control_replacements(
                json.loads(OWNERSHIP_PATH.read_text())
            )
            for case in promote_scoped_cases(cases, ownership):
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
            if report := os.environ.get("CATABOLIC_STRING_DIAGNOSTICS_REPORT"):
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
            print(f"String diagnostic complete-process-envelope cases: {len(results)}")


if __name__ == "__main__":
    unittest.main()
