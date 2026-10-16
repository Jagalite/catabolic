# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Exact caller-site expectation composition; no candidate runtime observations."""

import copy
import hashlib
import json
import os
import unittest
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
PARITY_ROOT = Path(os.environ.get("CATABOLIC_GRAMMAR_PARITY_ROOT", HERE / "parity"))
CONTRACT_ROOT = HERE / "parity"
CONTRACT = "native-graphql-parser-grammar-provenance-v1"
FILE = "graphql-parser-grammar-provenance-v1.json"
FILE_SHA256 = "f26fbefa0af6efbaa6b61cf2db0e0ff1016028ef5961ae48dd0d29392f63f5da"
SECTION_DIGESTS = {
    "fixture_bindings": "1a82377bd826f71eae677dff6d46b89d78a4ed489efcf4f1f9a33821c97cd24a",
    "policy_scope": "9a07be1b3451ed80accc0a7a1ea51d20a53d7c5fc8ea44c90247cd00197c41f8",
    "provenance": "4edfaabebabb4cefb3e7f47d6ea6a6b7dcb0750f1a34af16da63d71fa759d30e",
    "rows": "0629f9a25a009f8827fcdf70a5bfe96c16f5b2ec0891079bd6795edc92d7d5c5",
}


def digest(value):
    return hashlib.sha256(
        json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode()
    ).hexdigest()


def validate_contract(correction, parity_root=PARITY_ROOT):
    if (
        set(correction) != {"contract", *SECTION_DIGESTS}
        or correction["contract"] != CONTRACT
    ):
        raise ValueError("Unexpected grammar-provenance contract")
    for field, expected in SECTION_DIGESTS.items():
        if digest(correction[field]) != expected:
            raise ValueError(f"Grammar-provenance section changed: {field}")
    fixtures = {}
    for binding in correction["fixture_bindings"]:
        raw = (parity_root / binding["fixture"]).read_bytes()
        fixture = json.loads(raw)
        if (
            hashlib.sha256(raw).hexdigest() != binding["sha256"]
            or fixture["contract"] != binding["contract"]
            or digest(fixture["cases"]) != binding["raw_cases_sha256"]
            or len(fixture["cases"]) != binding["case_count"]
        ):
            raise ValueError(
                "Historical grammar fixture/input/envelope preimage changed"
            )
        fixtures[binding["fixture"]] = fixture
    for row in correction["rows"]:
        fixture = fixtures[row["fixture"]]
        case = fixture["cases"][row["index"]]
        checks = {
            "case_sha256": case,
            "args_sha256": case["args"],
            "reference_sha256": case["reference"],
            "legacy_sha256": case["baseline"]["legacy"],
            "companion_sha256": case["baseline"]["companion"],
            "frozen_parse_sha256": case.get("frozen_parse"),
        }
        if "raw_receipts_sha256" in row:
            checks["raw_receipts_sha256"] = case["raw_receipts"]
            checks["frozen_receipts_sha256"] = case["frozen_receipts"]
        if any(digest(value) != row[key] for key, value in checks.items()):
            raise ValueError("Complete grammar-case preimage changed")
        if (
            case["name"] != row["name"]
            or fixture["contract"] != row["fixture_contract"]
            or case["expectation"] != "baseline"
            or hashlib.sha256(case["document"].encode()).hexdigest()
            != row["document_sha256"]
            or len(case["document"].encode()) != row["document_bytes"]
            or case["reference"]["stdout"]["data"]["errors"] != [row["diagnostic"]]
            or (case["reference"] == case["baseline"]["legacy"])
            != row["historical_matching"]
            or row["promotion"] == row["historical_matching"]
            or not row[
                "ordinary_policy"
                if row["active_lane"] == "ordinary"
                else "before_limit_policy"
            ]
        ):
            raise ValueError("Grammar witness identity, lane or diagnostic changed")


def load_contract(parity_root=PARITY_ROOT, contract_root=CONTRACT_ROOT):
    raw = (contract_root / FILE).read_bytes()
    if hashlib.sha256(raw).hexdigest() != FILE_SHA256:
        raise ValueError("Grammar-provenance contract bytes changed")
    correction = json.loads(raw)
    validate_contract(correction, parity_root)
    return correction


def compose_cases(cases, contract, correction=None, parity_root=PARITY_ROOT):
    """Promote only exact verified rows after the independent stage07 lanes."""
    correction = load_contract(parity_root) if correction is None else correction
    validate_contract(correction, parity_root)
    matches = [b for b in correction["fixture_bindings"] if b["contract"] == contract]
    if len(matches) != 1:
        raise ValueError("Unknown grammar-provenance composition lane")
    binding = matches[0]
    if digest(cases) != binding["prior_cases_sha256"]:
        raise ValueError("Exact preceding grammar composition differs")
    result = copy.deepcopy(cases)
    for index in binding["promotion_indices"]:
        result[index]["expectation"] = "oracle"
    if digest(result) != binding["composed_cases_sha256"]:
        raise ValueError("Grammar post-composition inventory differs")
    return result


def composition_records(contract, correction=None):
    correction = load_contract() if correction is None else correction
    validate_contract(correction)
    if contract not in {b["contract"] for b in correction["fixture_bindings"]}:
        raise ValueError("Unknown grammar-provenance composition lane")
    return [
        dict(
            contract=CONTRACT,
            historical_control=r["name"],
            historical_pointer=r["pointer"],
            captured_preimage_sha256=r["case_sha256"],
        )
        for r in correction["rows"]
        if r["fixture_contract"] == contract and r["promotion"]
    ]


class GrammarProvenanceFixtureTest(unittest.TestCase):
    def setUp(self):
        self.contract = load_contract()

    def test_exact_24_historical_and_40_capture_promotions(self):
        rows = self.contract["rows"]
        self.assertEqual(
            Counter((r["origin"], r["promotion"]) for r in rows),
            {
                ("historical", True): 24,
                ("historical", False): 3,
                ("lexical-capture", True): 40,
                ("lexical-capture", False): 8,
            },
        )
        for binding in self.contract["fixture_bindings"]:
            original = json.loads((PARITY_ROOT / binding["fixture"]).read_bytes())[
                "cases"
            ]
            prior = copy.deepcopy(original)
            for index in binding["prior_promotion_indices"]:
                prior[index]["expectation"] = "oracle"
            before = copy.deepcopy(prior)
            result = compose_cases(prior, binding["contract"], self.contract)
            self.assertEqual(prior, before)
            for index, (old, new) in enumerate(zip(prior, result, strict=True)):
                self.assertEqual(
                    new,
                    dict(old, expectation="oracle")
                    if index in binding["promotion_indices"]
                    else old,
                )
            self.assertEqual(
                len(composition_records(binding["contract"], self.contract)),
                len(binding["promotion_indices"]),
            )

    def test_independent_policies_and_evidence_lanes(self):
        rows = self.contract["rows"]
        grouped = {}
        for r in rows:
            grouped.setdefault(digest(r["tuple"]), []).append(r)
            self.assertTrue(r["ordinary_policy"])
            self.assertEqual(
                r["before_limit_policy"], r["tuple"]["site"] != "VariableReferenceName"
            )
            self.assertEqual(r["candidate_runtime"], "unexecuted")
        self.assertEqual(len(grouped), 10)
        for group in grouped.values():
            witnessed = {r["active_lane"] for r in group}
            self.assertEqual(
                witnessed,
                {"ordinary"}
                if group[0]["tuple"]["site"] == "VariableReferenceName"
                else {"ordinary", "before_token_limit"},
            )
        capture = [r for r in rows if r["origin"] == "lexical-capture"]
        self.assertEqual(
            Counter(r["active_lane"] for r in capture),
            {"ordinary": 24, "before_token_limit": 24},
        )
        self.assertEqual(
            Counter(r["active_lane"] for r in capture if r["promotion"]),
            {"ordinary": 16, "before_token_limit": 24},
        )
        variable = [r for r in rows if r["legacy_variable_flag"]]
        self.assertEqual(
            Counter((r["active_lane"], r["promotion"]) for r in variable),
            {("ordinary", False): 2, ("before_token_limit", True): 2},
        )

    def test_unknown_wrong_order_repeat_and_mutations_fail_closed(self):
        for binding in self.contract["fixture_bindings"]:
            raw = json.loads((PARITY_ROOT / binding["fixture"]).read_bytes())["cases"]
            prior = copy.deepcopy(raw)
            for index in binding["prior_promotion_indices"]:
                prior[index]["expectation"] = "oracle"
            composed = compose_cases(prior, binding["contract"], self.contract)
            for wrong in (raw, composed, prior[:-1]):
                with self.assertRaises(ValueError):
                    compose_cases(wrong, binding["contract"], self.contract)
            for key, value in [
                ("document", "changed"),
                ("args", ["changed"]),
                ("name", "changed"),
                ("reference", {}),
                ("baseline", {}),
            ]:
                changed = copy.deepcopy(prior)
                changed[binding["promotion_indices"][0]][key] = value
                with self.assertRaises(ValueError):
                    compose_cases(changed, binding["contract"], self.contract)
        with self.assertRaises(ValueError):
            compose_cases([], "unknown", self.contract)

    def test_contract_policy_preimage_and_envelope_mutations_fail_closed(self):
        for section in SECTION_DIGESTS:
            changed = copy.deepcopy(self.contract)
            changed[section] = {}
            with self.assertRaises(ValueError):
                validate_contract(changed)
        for key, value in [
            ("promotion", False),
            ("before_limit_policy", False),
            ("active_lane", "ordinary"),
            ("reference_sha256", "0" * 64),
            ("companion_sha256", "0" * 64),
        ]:
            changed = copy.deepcopy(self.contract)
            changed["rows"][0][key] = value
            with self.assertRaises(ValueError):
                validate_contract(changed)

    def test_complete_original_raw_receipts_remain_verified(self):
        from tests import test_rust_graphql_lexical_capture as capture

        self.assertEqual(capture.verify_raw_receipts(capture.load_contract()), 4974)


if __name__ == "__main__":
    unittest.main()
