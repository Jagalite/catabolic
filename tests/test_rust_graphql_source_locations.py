# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen complete source-location pairs on disposable empty catalogs.

Uses the explicitly selected reference interpreter and native legacy/companion
artifacts. Three versioned missing-argument syntax assertions supplement the
original static-location matrix; separately versioned token-budget coordinates
retain lexical admission. Runtime and general syntax parity are unclaimed.
"""

import concurrent.futures
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

FIXTURE = json.loads(
    (fixture.ROOT / "tests/parity/graphql-source-locations.json").read_text()
)
CASES = FIXTURE["cases"]
CORRECTION_PATH = (
    fixture.ROOT / "tests/parity/graphql-source-location-syntax-controls-v1.json"
)
CORRECTED_NAMES = [
    "lf-syntax-admission-unchanged",
    "cr-syntax-admission-unchanged",
    "crlf-syntax-admission-unchanged",
]
ORACLE = "import sys;sys.path.insert(0,sys.argv.pop(1));from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"


def control_replacements(correction, parity_root=fixture.ROOT / "tests/parity"):
    """Reject drift; never silently promote another historical difference."""
    if correction["contract"] != "native-graphql-source-location-syntax-controls-v1":
        raise ValueError("Unexpected source-location control contract")
    provenance = correction["provenance"]
    for key in ("reference_commit", "python", "graphql_core"):
        if provenance[key] != FIXTURE["provenance"][key]:
            raise ValueError(f"Frozen oracle provenance changed: {key}")
    for filename, key in [
        ("graphql-source-locations.json", "historical_source_location_fixture_sha256"),
        ("graphql-missing-argument-cases.json", "missing_argument_fixture_sha256"),
    ]:
        if (
            hashlib.sha256((parity_root / filename).read_bytes()).hexdigest()
            != provenance[key]
        ):
            raise ValueError(f"Preserved fixture changed: {filename}")
    replacements = correction["cases"]
    if [case["historical_control"] for case in replacements] != CORRECTED_NAMES:
        raise ValueError("Only the three named syntax controls may be replaced")
    historical = {
        case["name"]: case for case in FIXTURE["unchanged_parser_and_budget_controls"]
    }
    for replacement in replacements:
        name = replacement["historical_control"]
        original = historical[name]
        expected = dict(
            historical_control=name,
            document=original["document"],
            expected=original["reference"],
        )
        if fixture.strict(replacement) != fixture.strict(expected):
            raise ValueError(f"Historical input or frozen response changed: {name}")
        if fixture.strict(original["native"]) == fixture.strict(original["reference"]):
            raise ValueError(f"Historical mismatch was relabeled: {name}")
    return {case["historical_control"]: case for case in replacements}


TOKEN_CORRECTION_PATH = (
    fixture.ROOT / "tests/parity/graphql-token-budget-locations-v1.json"
)
TOKEN_CORRECTED_NAMES = [
    f"{newline}-token-budget-unchanged" for newline in ("lf", "cr", "crlf")
]


def token_control_replacements(correction, parity_root=fixture.ROOT / "tests/parity"):
    """Only replace coordinates for the three exact historical budget controls."""
    if correction["contract"] != "native-graphql-token-budget-locations-v1":
        raise ValueError("Unexpected token-budget control contract")
    provenance = correction["provenance"]
    for key in ("reference_commit", "python", "graphql_core"):
        if provenance[key] != FIXTURE["provenance"][key]:
            raise ValueError(f"Frozen oracle provenance changed: {key}")
    for filename, key in [
        ("graphql-source-locations.json", "historical_source_location_fixture_sha256"),
        (
            "graphql-source-location-syntax-controls-v1.json",
            "syntax_controls_fixture_sha256",
        ),
    ]:
        if (
            hashlib.sha256((parity_root / filename).read_bytes()).hexdigest()
            != provenance[key]
        ):
            raise ValueError(f"Preserved fixture changed: {filename}")
    if provenance["baseline_commit"] != "00950e792669fd401fbb59a3a0956b40a8668cc1":
        raise ValueError("Token-budget baseline commit changed")
    if provenance["baseline_helpers_sha256"] != {
        "catabolic-native-qualified": "e2635bf64c8681bd6452a94168f6444d3f91d49304fa1611ad045c1319c3cb78",
        "catabolic-legacy-native-qualified": "d1439db4f9bacf34d4e7adff7721594d6901d7a7071f9537bfbf717bc448ad04",
    }:
        raise ValueError("Token-budget baseline helpers changed")
    replacements = correction["historical_replacements"]
    if [case["historical_control"] for case in replacements] != TOKEN_CORRECTED_NAMES:
        raise ValueError("Only the three named token-budget controls may be replaced")
    historical = {
        case["name"]: case for case in FIXTURE["unchanged_parser_and_budget_controls"]
    }
    for replacement in replacements:
        name = replacement["historical_control"]
        original = historical[name]
        expected = dict(
            historical_control=name,
            document=original["document"],
            expected=original["reference"],
        )
        if fixture.strict(replacement) != fixture.strict(expected):
            raise ValueError(f"Historical input or frozen response changed: {name}")
        old, new = (
            copy.deepcopy(original["native"]),
            copy.deepcopy(original["reference"]),
        )
        old["stdout"]["data"]["errors"][0].pop("locations")
        new["stdout"]["data"]["errors"][0].pop("locations")
        if fixture.strict(old) != fixture.strict(new) or fixture.strict(
            original["native"]
        ) == fixture.strict(original["reference"]):
            raise ValueError(
                f"Replacement must change only historical coordinates: {name}"
            )
    return {case["historical_control"]: case for case in replacements}


class SourceLocationFixtureTest(unittest.TestCase):
    def test_versioned_controls_preserve_original_fixture_and_budget_differences(self):
        replacements = control_replacements(json.loads(CORRECTION_PATH.read_text()))
        self.assertEqual(len(CASES), 58)
        remaining = [
            case
            for case in FIXTURE["unchanged_parser_and_budget_controls"]
            if case["name"] not in replacements
        ]
        self.assertEqual(
            [case["name"] for case in remaining],
            [f"{newline}-token-budget-unchanged" for newline in ("lf", "cr", "crlf")],
        )
        for case in remaining:
            self.assertNotEqual(
                fixture.strict(case["native"]), fixture.strict(case["reference"])
            )

    def test_missing_duplicate_reordered_or_expanded_replacements_are_rejected(self):
        original = json.loads(CORRECTION_PATH.read_text())
        for cases in (
            original["cases"][:-1],
            original["cases"] + [original["cases"][0]],
            list(reversed(original["cases"])),
            [
                dict(
                    original["cases"][0], historical_control="lf-token-budget-unchanged"
                ),
                *original["cases"][1:],
            ],
        ):
            with self.subTest(cases=cases), self.assertRaises(ValueError):
                control_replacements(dict(original, cases=cases))

    def test_changed_inputs_complete_envelopes_and_provenance_are_rejected(self):
        original = json.loads(CORRECTION_PATH.read_text())
        mutations = [
            lambda c: c.update(contract="unversioned"),
            lambda c: c["cases"][0].update(document="{profile}"),
            lambda c: c["cases"][0]["expected"].update(code=0),
            lambda c: c["cases"][0]["expected"].update(stderr="changed"),
            lambda c: c["cases"][0]["expected"]["stdout"].update(exit_code=True),
            lambda c: c["cases"][0]["expected"]["stdout"]["data"]["errors"][0].update(
                locations=[]
            ),
        ]
        mutations.extend(
            lambda c, key=key: c["provenance"].update({key: "changed"})
            for key in (
                "reference_commit",
                "python",
                "graphql_core",
                "historical_source_location_fixture_sha256",
                "missing_argument_fixture_sha256",
            )
        )
        for index, mutate in enumerate(mutations):
            correction = copy.deepcopy(original)
            mutate(correction)
            with self.subTest(mutation=index), self.assertRaises(ValueError):
                control_replacements(correction)


class SourceLocationTest(unittest.TestCase):
    def test_frozen_complete_legacy_and_companion_responses(self):
        replacements = control_replacements(json.loads(CORRECTION_PATH.read_text()))
        token_replacements = token_control_replacements(
            json.loads(TOKEN_CORRECTION_PATH.read_text())
        )
        legacy = Path(os.environ["CATABOLIC_LEGACY_BINARY"])
        companion = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        self.assertTrue(legacy.is_file())
        self.assertTrue(companion.is_file())
        self.assertTrue(fixture.PYTHON.is_file())
        from scripts.qualify_native_mutations import oracle_snapshot

        snapshot = oracle_snapshot(fixture.REFERENCE, fixture.ROOT)
        self.assertEqual(
            snapshot["reference_commit"], FIXTURE["provenance"]["reference_commit"]
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            reference, native = root / "reference.db", root / "native.db"
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
            self.assertEqual(version.returncode, 0, version.stderr)
            self.assertEqual(version.stderr, "")
            self.assertEqual(
                version.stdout.splitlines(),
                [
                    FIXTURE["provenance"]["python"],
                    FIXTURE["provenance"]["graphql_core"],
                ],
            )

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
            before = {
                path: hashlib.sha256(path.read_bytes()).hexdigest()
                for path in (reference, native)
            }

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
            # Preserve every historical record. The separate v1 contract asserts
            # current syntax parity; a separate contract repairs only the three
            # token-budget coordinates without modifying historical captures.
            controls = []
            for case in FIXTURE["unchanged_parser_and_budget_controls"]:
                args = ["graphql", case["document"]]
                replacement = replacements.get(case["name"]) or token_replacements.get(
                    case["name"]
                )
                expected_native = (
                    replacement["expected"] if replacement else case["native"]
                )
                expected = dict(
                    reference=case["reference"],
                    legacy=expected_native,
                    companion=dict(
                        code=0, stderr="", stdout=expected_native["stdout"]["data"]
                    ),
                )
                actual = dict(
                    reference=invoke(oracle, reference, ["--machine", *args]),
                    legacy=invoke([str(legacy)], native, ["--machine", *args]),
                    companion=invoke([str(companion)], native, ["--json", *args]),
                )
                controls.append(
                    dict(
                        name=case["name"],
                        current_contract=(
                            "native-graphql-source-location-syntax-controls-v1"
                            if case["name"] in replacements
                            else "native-graphql-token-budget-locations-v1"
                        ),
                        historical_reference=case["reference"],
                        historical_native=case["native"],
                        results=actual,
                        match=fixture.strict(actual) == fixture.strict(expected),
                    )
                )
                with self.subTest(control=case["name"]):
                    self.assertEqual(fixture.strict(actual), fixture.strict(expected))
            if report := os.environ.get("CATABOLIC_SOURCE_LOCATION_CONTROLS_REPORT"):
                Path(report).write_text(json.dumps(controls, indent=2) + "\n")
            self.assertEqual(
                {
                    path: hashlib.sha256(path.read_bytes()).hexdigest()
                    for path in before
                },
                before,
            )
            print(
                f"Frozen source-location CLI pairs: {len(results)}; "
                f"versioned syntax controls: {len(replacements)}; "
                f"versioned token-budget controls: {len(token_replacements)}"
            )


if __name__ == "__main__":
    unittest.main()
