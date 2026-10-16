# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fail-closed union of independently sealed exact stage07 promotions."""

import copy
import hashlib
import json
import unittest
from pathlib import Path
from unittest.mock import patch

from tests import test_rust_graphql_executable_punctuation as executable
from tests import test_rust_graphql_grammar_provenance as grammar

CONTRACT = "native-graphql-parser-final-composition-v1"
FILE = "graphql-parser-final-composition-v1.json"
FILE_SHA256 = "169a8d55457b33d9de89a8d332593470e5cfb2038ca882348e763899ff4f1bd6"
PAYLOAD_SHA256 = "39c6163d5f8c3565127f5c6484904c3bbd544dd6e59ce6281cb48e616ad72f86"
CONTRACT_ROOT = Path(__file__).resolve().parent / "parity"


def digest(value):
    return grammar.digest(value)


def validate_contract(correction):
    if digest(correction) != PAYLOAD_SHA256:
        raise ValueError(
            "Final composition scope, exact preimages or postimages changed"
        )
    g = grammar.load_contract()
    e = executable.load_contract()
    if correction["upstream_contracts"] != {
        "grammar": grammar.FILE_SHA256,
        "executable": executable.FILE_SHA256,
    }:
        raise ValueError("Sealed upstream contract identity changed")
    for binding in correction["fixture_bindings"]:
        gb = next(
            b for b in g["fixture_bindings"] if b["contract"] == binding["contract"]
        )
        eb = next(
            (b for b in e["fixture_bindings"] if b["contract"] == binding["contract"]),
            None,
        )
        if (
            binding["fixture"] != gb["fixture"]
            or binding["fixture_sha256"] != gb["sha256"]
            or binding["prior_cases_sha256"] != gb["prior_cases_sha256"]
            or binding["grammar_cases_sha256"] != gb["composed_cases_sha256"]
            or binding["grammar_indices"] != gb["promotion_indices"]
            or binding["prior_promotion_indices"] != gb["prior_promotion_indices"]
            or binding["case_count"] != gb["case_count"]
        ):
            raise ValueError("Exact grammar composition binding changed")
        if eb:
            if (
                binding["fixture_sha256"] != eb["sha256"]
                or binding["prior_cases_sha256"] != eb["stage07_cases_sha256"]
                or binding["executable_cases_sha256"] != eb["stage08_cases_sha256"]
                or binding["executable_indices"] != eb["replacement_indices"]
            ):
                raise ValueError("Exact common executable preimage changed")
        elif (
            binding["executable_indices"]
            or binding["executable_cases_sha256"] is not None
        ):
            raise ValueError("Captured lane cannot enter executable composition")
        if set(binding["grammar_indices"]) & set(binding["executable_indices"]):
            raise ValueError("Independent promotion sets overlap")
    for control in correction["test_reconciliations"]:
        for witness in control["witnesses"]:
            row = next(
                r
                for r in g["rows"]
                if r["fixture"] == witness["fixture"] and r["index"] == witness["index"]
            )
            case = json.loads((grammar.PARITY_ROOT / row["fixture"]).read_bytes())[
                "cases"
            ][row["index"]]
            if (
                digest(row) != witness["row_sha256"]
                or digest(case) != witness["case_sha256"]
                or not case["document"].startswith(control["prefix"])
                or row["active_lane"] != witness["lane"]
                or row["tuple"] != control["tuple"]
                or row["diagnostic"] != control["diagnostic"]
                or not row["ordinary_policy"]
                or not row["before_limit_policy"]
            ):
                raise ValueError(
                    "Old test reconciliation lacks its exact independent witness"
                )
    return g, e


def load_contract():
    raw = (CONTRACT_ROOT / FILE).read_bytes()
    if hashlib.sha256(raw).hexdigest() != FILE_SHA256:
        raise ValueError("Final composition contract bytes changed")
    result = json.loads(raw)
    validate_contract(result)
    return result


def compose_cases(cases, contract, correction=None):
    """Apply both sealed composers to the same exact preimage, never in sequence."""
    correction = load_contract() if correction is None else correction
    g, e = validate_contract(correction)
    matches = [b for b in correction["fixture_bindings"] if b["contract"] == contract]
    if len(matches) != 1:
        raise ValueError("Unknown final composition lane")
    binding = matches[0]
    if digest(cases) != binding["prior_cases_sha256"]:
        raise ValueError("Exact common preimage differs or was already composed")
    grammar_result = grammar.compose_cases(cases, contract, g)
    executable_result = (
        executable.compose_cases(cases, contract, e)
        if binding["executable_cases_sha256"] is not None
        else copy.deepcopy(cases)
    )
    result = copy.deepcopy(cases)
    for index, (before, grammar_case, executable_case) in enumerate(
        zip(cases, grammar_result, executable_result, strict=True)
    ):
        gi = index in binding["grammar_indices"]
        ei = index in binding["executable_indices"]
        if (
            grammar_case != (dict(before, expectation="oracle") if gi else before)
            or executable_case != (dict(before, expectation="oracle") if ei else before)
            or (gi or ei)
            and before["expectation"] != "baseline"
        ):
            raise ValueError(
                "Independent composer changed an unapproved input or envelope"
            )
        if gi or ei:
            result[index]["expectation"] = "oracle"
    if digest(result) != binding["final_cases_sha256"]:
        raise ValueError("Exact final postimage differs")
    return result


def composition_records(contract, correction=None):
    correction = load_contract() if correction is None else correction
    g, e = validate_contract(correction)
    matches = [b for b in correction["fixture_bindings"] if b["contract"] == contract]
    if len(matches) != 1:
        raise ValueError("Unknown final composition record lane")
    binding = matches[0]
    records = grammar.composition_records(contract, g)
    if binding["executable_cases_sha256"] is not None:
        records.extend(executable.composition_records(contract, e))
    if len(records) != len(binding["grammar_indices"]) + len(
        binding["executable_indices"]
    ):
        raise ValueError("Final promotion record inventory differs")
    return records


class FinalCompositionFixtureTest(unittest.TestCase):
    def setUp(self):
        self.correction = load_contract()

    def prior(self, binding):
        original = json.loads((grammar.PARITY_ROOT / binding["fixture"]).read_bytes())[
            "cases"
        ]
        result = copy.deepcopy(original)
        for index in binding["prior_promotion_indices"]:
            result[index]["expectation"] = "oracle"
        self.assertEqual(digest(result), binding["prior_cases_sha256"])
        return result

    def test_exact_common_preimages_disjoint_27_historical_and_40_captured_labels(self):
        historical = captured = 0
        for binding in self.correction["fixture_bindings"]:
            prior = self.prior(binding)
            expected = copy.deepcopy(prior)
            indices = binding["grammar_indices"] + binding["executable_indices"]
            self.assertEqual(len(set(indices)), len(indices))
            for index in indices:
                expected[index]["expectation"] = "oracle"
            result = compose_cases(prior, binding["contract"], self.correction)
            self.assertEqual(result, expected)
            self.assertEqual(
                len(composition_records(binding["contract"])), len(indices)
            )
            if binding["executable_cases_sha256"] is None:
                captured += len(indices)
                self.assertEqual(len(binding["prior_promotion_indices"]), 104)
            else:
                historical += len(indices)
        self.assertEqual((historical, captured), (27, 40))

    def test_all_old_inputs_envelopes_and_matching_labels_stay_unchanged(self):
        for binding in self.correction["fixture_bindings"]:
            prior = self.prior(binding)
            untouched = copy.deepcopy(prior)
            result = compose_cases(prior, binding["contract"], self.correction)
            for before, after in zip(prior, result, strict=True):
                self.assertEqual(
                    {k: v for k, v in before.items() if k != "expectation"},
                    {k: v for k, v in after.items() if k != "expectation"},
                )
                if (
                    before["expectation"] == "oracle"
                    or before["reference"] == before["baseline"]["legacy"]
                ):
                    self.assertEqual(after, before)
            result[0]["document"] = "mutated returned value"
            self.assertEqual(prior, untouched)

    def test_rejects_wrong_order_partial_repeated_and_unknown_lanes(self):
        for binding in self.correction["fixture_bindings"]:
            prior = self.prior(binding)
            final = compose_cases(prior, binding["contract"], self.correction)
            wrong = [
                prior[:-1],
                list(reversed(prior)),
                final,
                grammar.compose_cases(prior, binding["contract"]),
            ]
            if binding["executable_indices"]:
                wrong.append(executable.compose_cases(prior, binding["contract"]))
            for cases in wrong:
                with self.assertRaises(ValueError):
                    compose_cases(cases, binding["contract"], self.correction)
        with self.assertRaises(ValueError):
            compose_cases([], "unknown", self.correction)
        with self.assertRaises(ValueError):
            composition_records("unknown", self.correction)

    def test_rejects_mutated_inputs_args_labels_and_complete_envelopes(self):
        for binding in self.correction["fixture_bindings"]:
            prior = self.prior(binding)
            for key, value in [
                ("document", "changed"),
                ("args", ["changed"]),
                ("name", "changed"),
                ("reference", {}),
                ("baseline", {}),
                ("expectation", "oracle"),
            ]:
                changed = copy.deepcopy(prior)
                changed[binding["grammar_indices"][0]][key] = value
                with self.assertRaises(ValueError):
                    compose_cases(changed, binding["contract"], self.correction)

    def test_rejects_mutated_scope_policy_preimages_postimages_and_extra_fields(self):
        for field in self.correction:
            changed = copy.deepcopy(self.correction)
            changed[field] = None
            with self.assertRaises(ValueError):
                validate_contract(changed)
        for key in self.correction["fixture_bindings"][0]:
            changed = copy.deepcopy(self.correction)
            changed["fixture_bindings"][0][key] = None
            with self.assertRaises(ValueError):
                validate_contract(changed)
        changed = dict(self.correction, extra=True)
        with self.assertRaises(ValueError):
            validate_contract(changed)

    def test_captured_lane_never_calls_executable_composer(self):
        binding = next(
            b
            for b in self.correction["fixture_bindings"]
            if b["executable_cases_sha256"] is None
        )
        with patch.object(
            executable, "compose_cases", side_effect=AssertionError("wrong lane")
        ):
            result = compose_cases(
                self.prior(binding), binding["contract"], self.correction
            )
        self.assertEqual(digest(result), binding["final_cases_sha256"])

    def test_both_composers_receive_unchanged_common_preimage(self):
        binding = next(
            b for b in self.correction["fixture_bindings"] if b["executable_indices"]
        )
        prior = self.prior(binding)
        with (
            patch.object(grammar, "compose_cases", wraps=grammar.compose_cases) as g,
            patch.object(
                executable, "compose_cases", wraps=executable.compose_cases
            ) as e,
        ):
            compose_cases(prior, binding["contract"], self.correction)
        self.assertEqual(digest(g.call_args.args[0]), binding["prior_cases_sha256"])
        self.assertEqual(digest(e.call_args.args[0]), binding["prior_cases_sha256"])

    def test_rejects_unapproved_delta_even_from_an_independent_composer(self):
        binding = next(
            b for b in self.correction["fixture_bindings"] if b["executable_indices"]
        )
        prior = self.prior(binding)
        wrong = executable.compose_cases(prior, binding["contract"])
        wrong[0]["reference"] = {}
        with (
            patch.object(executable, "compose_cases", return_value=wrong),
            self.assertRaises(ValueError),
        ):
            compose_cases(prior, binding["contract"], self.correction)

    def test_standalone_scalar_runtime_helper_composes_both_exact_rows(self):
        from tests import test_rust_graphql_unsupported_scalar_precedence as scalar

        result, records = scalar.compose_runtime_contract()
        binding = next(
            b
            for b in self.correction["fixture_bindings"]
            if b["contract"] == scalar.CONTRACT
        )
        self.assertEqual(binding["grammar_indices"], [21])
        self.assertEqual(binding["executable_indices"], [25])
        self.assertEqual(digest(result["cases"]), binding["final_cases_sha256"])
        for index in [21, 25]:
            self.assertEqual(result["cases"][index]["expectation"], "oracle")
        final_records = [
            r
            for r in records
            if r["contract"] in {grammar.CONTRACT, executable.CONTRACT}
        ]
        self.assertEqual(
            [r["historical_pointer"] for r in final_records], ["/cases/21", "/cases/25"]
        )

    def test_old_test_reconciliations_require_exact_independent_witnesses(self):
        controls = self.correction["test_reconciliations"]
        self.assertEqual(
            [r["prefix"] for r in controls],
            [
                "{}",
                "{!}",
                "{items(:1)}",
                "{items(first:{name 1})}",
                "{profile @skip()}",
                "{profile @skip(if true)}",
            ],
        )
        for control in controls:
            self.assertEqual(
                [w["lane"] for w in control["witnesses"]],
                ["ordinary", "before_token_limit"],
            )
        for index in range(len(controls)):
            for key in controls[index]:
                changed = copy.deepcopy(self.correction)
                changed["test_reconciliations"][index][key] = None
                with self.assertRaises(ValueError):
                    validate_contract(changed)

    def test_complete_4974_original_capture_receipts_are_retained(self):
        from tests import test_rust_graphql_lexical_capture as capture

        self.assertEqual(capture.verify_raw_receipts(capture.load_contract()), 4974)


if __name__ == "__main__":
    unittest.main()
