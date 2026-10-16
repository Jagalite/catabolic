# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Exact immutable ordinary and required-field-value admission contracts.

Fixture tests inspect captured bytes only. Runtime tests are discovered
separately and require explicitly selected source-bound native artifacts.
The archived native envelopes are evidence of the old stage02 cohort only.
"""

import copy
import hashlib
import json
import tempfile
import unittest
from collections import Counter
from pathlib import Path

from tests import test_rust_graphql_scoped_parser_diagnostics as scoped
from tests import test_rust_legacy_parser as fixture

PARITY_ROOT = Path(__file__).resolve().parent / "parity"
OWNERSHIP_CONTRACT = "native-graphql-parser-boundary-ownership-v1"
ADMISSION_CONTRACT = "native-graphql-parser-required-value-precedence-v1"
HISTORICAL_CONTRACT = "native-graphql-parser-diagnostic-ownership-v1"
HISTORICAL_FILE = "graphql-parser-diagnostic-ownership-v1.json"
HISTORICAL_SHA256 = "11df0b04b32fa187d09c57623bf8ddf45f5ecfc7bee101853197d44810f78444"
REPLACEMENT_INDICES = [18, 19, 20, 24]
REPLACEMENT_NAMES = [
    "unowned-noninitial-question",
    "unowned-initial-unicode",
    "unowned-empty-arguments",
    "unowned-unsupported-slash",
]
CONTRACTS = {
    OWNERSHIP_CONTRACT: {
        "file": "graphql-parser-boundary-ownership-v1.json",
        "case_count": 61,
        "preflight_count": 56,
        "digests": {
            "provenance": "37c7ba67609b15d843dc1543db32aa267bc40f09345d1ebe4537888dc71e11d5",
            "ownership_replacements": "8067b1664d99a3ebd603afd0c385df26fc97ddd8449ad78b29628a218a8c465b",
            "frozen_source_controls": "54b946db76363b5b708bfabfb4e247cb86a8f30f693cf16560de451f5079a1b2",
            "cases": "7879baa36b670ab27a6e155e315972bcca880f546e9d660b44c06890cc0cf240",
        },
    },
    ADMISSION_CONTRACT: {
        "file": "graphql-parser-required-value-precedence-v1.json",
        "case_count": 50,
        "preflight_count": 50,
        "digests": {
            "provenance": "da6f8f0713b182dd4968830f67b52c1faa32da67fcb15b40f023f13a71309105",
            "cases": "9e3b6c2d42041826ef7a4bd7fc42e0a5843fe512b7d2c6dc38715771d9f22bb3",
        },
    },
}


def boundary_control_replacements(correction, contract, parity_root=PARITY_ROOT):
    """Fail closed on lane, provenance, inventory and every captured preimage."""
    if contract not in CONTRACTS:
        raise ValueError("Unknown boundary contract")
    spec = CONTRACTS[contract]
    if set(correction) != {"contract", *spec["digests"]}:
        raise ValueError("Unexpected boundary contract fields")
    if correction["contract"] != contract:
        raise ValueError("Unexpected boundary contract identity")
    for field, expected in spec["digests"].items():
        if scoped.digest(correction[field]) != expected:
            raise ValueError(f"Boundary {field} provenance or exact captures changed")
    historical_path = parity_root / HISTORICAL_FILE
    if hashlib.sha256(historical_path.read_bytes()).hexdigest() != HISTORICAL_SHA256:
        raise ValueError("Historical ownership fixture bytes changed")
    historical = json.loads(historical_path.read_text())
    scoped.ownership_control_replacements(historical, parity_root)
    if contract == ADMISSION_CONTRACT:
        return {}
    replacements = correction["ownership_replacements"]
    if [r["historical_control"] for r in replacements] != REPLACEMENT_NAMES:
        raise ValueError("Only the four independent ownership replacements are allowed")
    checked = {}
    for index, replacement in zip(REPLACEMENT_INDICES, replacements, strict=True):
        original = historical["cases"][index]
        expected = dict(
            historical_pointer=f"/cases/{index}",
            historical_control=original["name"],
            document=original["document"],
            args=original["args"],
            expected=original["reference"],
            baseline=original["baseline"],
        )
        if fixture.strict(replacement) != fixture.strict(expected):
            raise ValueError(
                "Ownership input, oracle or native-baseline preimage changed"
            )
        if original["expectation"] != "baseline" or fixture.strict(
            original["reference"]
        ) == fixture.strict(original["baseline"]["legacy"]):
            raise ValueError("A historical matching row cannot be relabeled")
        checked[original["name"]] = replacement
    return checked


def load_contract(contract, parity_root=PARITY_ROOT):
    correction = json.loads((parity_root / CONTRACTS[contract]["file"]).read_text())
    boundary_control_replacements(correction, contract, parity_root)
    return correction


def promote_ownership_cases(cases, correction=None, parity_root=PARITY_ROOT):
    """Compose four exact changes; never mutate or reclassify other old rows."""
    if correction is None:
        correction = load_contract(OWNERSHIP_CONTRACT, parity_root)
    replacements = boundary_control_replacements(
        correction, OWNERSHIP_CONTRACT, parity_root
    )
    if scoped.digest(cases) != scoped.CONTRACTS[HISTORICAL_CONTRACT]["cases_sha256"]:
        raise ValueError("Historical ownership inventory differs before composition")
    return scoped.promote_scoped_cases(cases, replacements)


class BoundaryOwnershipFixtureTest(unittest.TestCase):
    def test_exact_independent_inventory_and_capture_envelopes(self):
        names = set()
        for contract, spec in CONTRACTS.items():
            correction = load_contract(contract)
            cases = correction["cases"]
            self.assertEqual(len(cases), spec["case_count"])
            self.assertEqual(
                sum(c["preflight"] for c in cases), spec["preflight_count"]
            )
            self.assertEqual(len({c["name"] for c in cases}), len(cases))
            self.assertFalse(names & {c["name"] for c in cases})
            names.update(c["name"] for c in cases)
            for case in cases:
                self.assertEqual(case["args"], [])
                self.assertLessEqual(len(case["document"].encode()), 131072)
                self.assertEqual(
                    hashlib.sha256(case["document"].encode()).hexdigest(),
                    case["document_sha256"],
                )
                self.assertIn(case["expectation"], ("oracle", "baseline"))
                self.assertEqual(set(case["baseline"]), {"legacy", "companion"})
                self.assertEqual(case["reference"]["stderr"], "")
                self.assertEqual(
                    case["preflight"], not case["frozen_parse"]["accepted"]
                )
                if case["preflight"]:
                    self.assertEqual(case["reference"]["code"], 2)
                    self.assertEqual(
                        case["reference"]["stdout"]["data"]["errors"],
                        [case["frozen_parse"]["formatted"]],
                    )
        self.assertEqual(len(names), 111)

    def test_four_preimages_compose_without_mutating_historical_rows(self):
        path = PARITY_ROOT / HISTORICAL_FILE
        old_bytes = path.read_bytes()
        historical = json.loads(old_bytes)
        original = copy.deepcopy(historical["cases"])
        promoted = promote_ownership_cases(original)
        self.assertEqual(historical["cases"], original)
        self.assertEqual(path.read_bytes(), old_bytes)
        changed = []
        for index, (old, new) in enumerate(zip(original, promoted, strict=True)):
            if old != new:
                changed.append(index)
                self.assertEqual(new, dict(old, expectation="oracle"))
            else:
                self.assertEqual(old, new)
        self.assertEqual(changed, REPLACEMENT_INDICES)
        self.assertEqual(Counter(c["expectation"] for c in original)["baseline"], 11)
        self.assertEqual(Counter(c["expectation"] for c in promoted)["baseline"], 7)

    def test_other_twenty_four_composed_baselines_keep_original_classification(self):
        from tests import test_rust_graphql_unsupported_scalar_precedence as scalar

        ownership = json.loads(scoped.OWNERSHIP_PATH.read_text())["cases"]
        precedence = json.loads(scoped.SCOPED_PRECEDENCE_PATH.read_text())["cases"]
        scalar_cases = json.loads(scalar.SCALAR_PATH.read_text())["cases"]
        before = scalar.promote_stage01_cases(precedence) + ownership + scalar_cases
        after = (
            scalar.promote_stage01_cases(precedence)
            + promote_ownership_cases(ownership)
            + scalar_cases
        )
        old_baselines = [c for c in before if c["expectation"] == "baseline"]
        new_baselines = [c for c in after if c["expectation"] == "baseline"]
        self.assertEqual(len(old_baselines), 28)
        self.assertEqual(len(new_baselines), 24)
        self.assertEqual(
            new_baselines,
            [c for c in old_baselines if c["name"] not in REPLACEMENT_NAMES],
        )
        matching = [
            c
            for c in new_baselines
            if fixture.strict(c["reference"]) == fixture.strict(c["baseline"]["legacy"])
        ]
        mismatching = [
            c
            for c in new_baselines
            if fixture.strict(c["reference"]) != fixture.strict(c["baseline"]["legacy"])
        ]
        self.assertEqual(len(mismatching), 20)
        self.assertEqual(len(matching), 4)
        self.assertEqual(
            {c["name"] for c in matching},
            {
                "invalid-timeout-admission",
                "invalid-variables-admission",
                "unowned-variable-directive-empty",
                "unowned-variable-directive-colon",
            },
        )

    def test_directive_scope_and_existing_exceptions_are_separate(self):
        cases = {c["name"]: c for c in load_contract(OWNERSHIP_CONTRACT)["cases"]}
        for name in (
            "directive-field-empty",
            "directive-field-colon",
            "directive-field-root-bracket",
            "directive-field-list-paren",
            "directive-field-object-brace",
            "field-then-directive",
        ):
            self.assertEqual(cases[name]["expectation"], "baseline")
        for name in (
            "neighbor-directive-percent",
            "directive-field-existing-root-paren",
            "directive-variable-list-paren",
            "directive-then-field",
            "variable-root-bracket",
        ):
            self.assertEqual(cases[name]["expectation"], "oracle")
        for name in (
            "directive-field-existing-root-paren",
            "directive-variable-list-paren",
        ):
            self.assertEqual(
                cases[name]["reference"], cases[name]["baseline"]["legacy"]
            )

    def test_required_value_precedence_and_exact_token_coordinate_boundaries(self):
        cases = {c["name"]: c for c in load_contract(ADMISSION_CONTRACT)["cases"]}
        for suffix in ("root-close-bracket", "list-close-paren", "object-close-brace"):
            case = cases["overbudget-" + suffix]
            self.assertEqual(case["expectation"], "oracle")
            self.assertNotEqual(case["reference"], case["baseline"]["legacy"])
        for newline in ("lf", "cr", "crlf"):
            for count in (3999, 4000, 4001):
                for position in ("root", "list", "object"):
                    case = cases[f"limit-{newline}-{count}-{position}"]
                    self.assertEqual(case["expectation"], "oracle")
                    self.assertEqual(
                        "more than 4000 tokens" in case["frozen_parse"]["message"],
                        count == 4001,
                    )
            self.assertEqual(
                cases[f"overbudget-coordinate-{newline}"]["frozen_parse"]["locations"],
                [{"line": 2, "column": 27}],
            )
        self.assertIn(
            "more than 4000 tokens",
            cases["already-overbudget-closer"]["frozen_parse"]["message"],
        )
        self.assertEqual(
            Counter(c["expectation"] for c in cases.values()),
            {"oracle": 36, "baseline": 14},
        )

    def test_newly_owned_scalar_assertions_have_exact_frozen_evidence(self):
        rows = load_contract(OWNERSHIP_CONTRACT)["frozen_source_controls"]
        self.assertEqual(len(rows), 26)
        self.assertEqual(len({r["document"] for r in rows}), 26)
        self.assertIn("\x00", {r["document"] for r in rows})
        for row in rows:
            self.assertEqual(
                hashlib.sha256(row["document"].encode()).hexdigest(),
                row["document_sha256"],
            )
            self.assertFalse(row["frozen"]["accepted"])
            self.assertEqual(row["frozen"]["positions"], [0])
            self.assertEqual(row["frozen"]["locations"], [{"line": 1, "column": 1}])
            self.assertTrue(
                row["frozen"]["message"].startswith(
                    "Syntax Error: Unexpected character:"
                )
            )

    def test_identity_provenance_inputs_and_envelopes_fail_closed(self):
        for contract in CONTRACTS:
            original = load_contract(contract)
            mutations = [
                lambda c: c.update(contract="unversioned"),
                lambda c: c.update(extra=True),
                lambda c: c["cases"].pop(),
                lambda c: c["cases"].reverse(),
                lambda c: c["cases"][0].update(document="{profile}"),
                lambda c: c["cases"][0].update(expectation="baseline"),
                lambda c: c["cases"][0].update(preflight=False),
                lambda c: c["cases"][0]["reference"].update(code=True),
                lambda c: c["cases"][0]["reference"].update(stderr="changed"),
                lambda c: c["cases"][0]["baseline"]["legacy"].update(code=0),
                lambda c: c["cases"][0]["baseline"]["companion"].update(stdout={}),
                lambda c: c["cases"][0]["frozen_parse"].update(positions=[]),
            ]
            mutations.extend(
                lambda c, key=key: c["provenance"].update({key: "changed"})
                for key in original["provenance"]
            )
            if contract == OWNERSHIP_CONTRACT:
                mutations.extend(
                    [
                        lambda c: c["ownership_replacements"].pop(),
                        lambda c: c["ownership_replacements"].reverse(),
                        lambda c: c["ownership_replacements"][0].update(
                            historical_pointer="/cases/21"
                        ),
                        lambda c: c["ownership_replacements"][0].update(
                            document="{profile}"
                        ),
                        lambda c: c["ownership_replacements"][0].update(
                            args=["--variables", "[]"]
                        ),
                        lambda c: c["ownership_replacements"][0]["expected"].update(
                            code=True
                        ),
                        lambda c: c["ownership_replacements"][0]["baseline"][
                            "legacy"
                        ].update(stderr="changed"),
                        lambda c: c["frozen_source_controls"].pop(),
                    ]
                )
            for index, mutation in enumerate(mutations):
                changed = copy.deepcopy(original)
                mutation(changed)
                with (
                    self.subTest(contract=contract, mutation=index),
                    self.assertRaises(ValueError),
                ):
                    boundary_control_replacements(changed, contract)
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / HISTORICAL_FILE).write_bytes(
                    (PARITY_ROOT / HISTORICAL_FILE).read_bytes() + b"\n"
                )
                with self.assertRaises(ValueError):
                    boundary_control_replacements(original, contract, root)

    def test_composition_rejects_missing_duplicate_reordered_or_changed_rows(self):
        original = json.loads(scoped.OWNERSHIP_PATH.read_text())["cases"]
        for index in REPLACEMENT_INDICES:
            for field, value in (
                ("name", "other"),
                ("document", "{profile}"),
                ("args", ["--variables", "[]"]),
                ("reference", {}),
                ("baseline", {}),
                ("expectation", "oracle"),
            ):
                changed = copy.deepcopy(original)
                changed[index][field] = value
                with (
                    self.subTest(index=index, field=field),
                    self.assertRaises(ValueError),
                ):
                    promote_ownership_cases(changed)
        for changed in (
            original[:-1],
            original + [original[18]],
            list(reversed(original)),
        ):
            with self.assertRaises(ValueError):
                promote_ownership_cases(changed)


class BoundaryOwnershipTest(unittest.TestCase):
    def test_ordinary_boundary_complete_process_envelopes(self):
        scoped.ScopedParserDiagnosticTest.replay(
            self, OWNERSHIP_CONTRACT, "CATABOLIC_BOUNDARY_OWNERSHIP_REPORT"
        )

    def test_required_field_value_admission_complete_process_envelopes(self):
        scoped.ScopedParserDiagnosticTest.replay(
            self, ADMISSION_CONTRACT, "CATABOLIC_REQUIRED_VALUE_PRECEDENCE_REPORT"
        )


if __name__ == "__main__":
    unittest.main()
