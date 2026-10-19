# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Exact metadata composition; historical captures are not candidate results."""

import copy
import hashlib
import json
import os
import sys
import unittest
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
PARITY_ROOT = Path(os.environ.get("CATABOLIC_IMMUTABLE_PARITY_ROOT", HERE / "parity"))
PRIOR_ROOT = Path(os.environ.get("CATABOLIC_STAGE07_PARITY_ROOT", PARITY_ROOT))
CONTRACT_ROOT = HERE / "parity"
CONTRACT = "native-graphql-parser-executable-punctuation-v1"
FILE = "graphql-parser-executable-punctuation-v1.json"
FILE_SHA256 = "4e23178350028f767d3b0e316b77f130a9db2b8d65adb388265c35908448d49d"
SECTION_DIGESTS = {
    "provenance": "ef1b787795613efa366ada1b6f7e3a02717b7c617acd93a3f090cfdc9458dd88",
    "fixture_bindings": "d6003dc080cec540d0836b8326693f77a2a008817451813dc98e2ad2f2b58eac",
    "replacements": "45ea8bfb1ade0fedb16a8cfa101112864c0edcfe6ed7c87d16570609f5989c2e",
}
INDICES = {
    "native-graphql-parser-budget-precedence-v2": [],
    "native-graphql-parser-diagnostic-ownership-v1": [21],
    "native-graphql-unsupported-scalar-precedence-v1": [25],
    "native-graphql-parser-boundary-ownership-v1": [18],
    "native-graphql-parser-required-value-precedence-v1": [],
}


def digest(value):
    return hashlib.sha256(
        json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode()
    ).hexdigest()


def prior_bytes(name, parity_root=PARITY_ROOT, prior_root=PRIOR_ROOT):
    root = (
        prior_root
        if name == "graphql-parser-directive-value-closers-v1.json"
        else parity_root
    )
    return (root / name).read_bytes()


def validate_contract(correction, parity_root=PARITY_ROOT, prior_root=PRIOR_ROOT):
    if (
        set(correction) != {"contract", *SECTION_DIGESTS}
        or correction["contract"] != CONTRACT
    ):
        raise ValueError(
            "Unexpected executable-punctuation contract identity or fields"
        )
    for field, expected in SECTION_DIGESTS.items():
        if digest(correction[field]) != expected:
            raise ValueError(
                "Executable-punctuation scope, provenance or preimages changed"
            )
    for prior in correction["provenance"]["prior_contracts"]:
        if (
            hashlib.sha256(
                prior_bytes(prior["fixture"], parity_root, prior_root)
            ).hexdigest()
            != prior["sha256"]
        ):
            raise ValueError("Prior composition contract bytes changed")
    fixtures = {}
    for binding in correction["fixture_bindings"]:
        raw = (parity_root / binding["fixture"]).read_bytes()
        if hashlib.sha256(raw).hexdigest() != binding["sha256"]:
            raise ValueError("Historical fixture bytes changed")
        fixture = json.loads(raw)
        if (
            fixture["contract"] != binding["contract"]
            or len(fixture["cases"]) != binding["case_count"]
            or digest(fixture["cases"]) != binding["cases_sha256"]
            or digest(fixture["provenance"]) != binding["provenance_sha256"]
        ):
            raise ValueError("Historical fixture inventory or provenance changed")
        fixtures[binding["fixture"]] = fixture
    for row in correction["replacements"]:
        fixture = fixtures[row["fixture"]]
        original = fixture["cases"][int(row["pointer"].split("/")[-1])]
        if (
            fixture["contract"] != row["fixture_contract"]
            or original["name"] != row["name"]
            or digest(original) != digest(row["preimage"])
            or original["expectation"] != "baseline"
            or digest(original["reference"]) == digest(original["baseline"]["legacy"])
        ):
            raise ValueError(
                "Historical executable-punctuation input or envelope changed"
            )
        for value, key in [
            (original, "case_sha256"),
            (original["reference"], "reference_sha256"),
            (original["baseline"]["legacy"], "legacy_sha256"),
            (original["baseline"]["companion"], "companion_sha256"),
        ]:
            if digest(value) != row[key]:
                raise ValueError(
                    "Complete executable-punctuation preimage hash differs"
                )
        expected_parse = (
            digest(original["frozen_parse"]) if "frozen_parse" in original else None
        )
        if row["frozen_parse_sha256"] != expected_parse:
            raise ValueError("Existing frozen parse presence or value changed")
        if "frozen_parse" in original and original["reference"]["stdout"]["data"][
            "errors"
        ] != [original["frozen_parse"]["formatted"]]:
            raise ValueError("Existing frozen parse and reference differ")
        if (
            hashlib.sha256(original["document"].encode()).hexdigest()
            != row["document_sha256"]
            or len(original["document"].encode()) != row["document_bytes"]
        ):
            raise ValueError("Exact source bytes changed")


def load_contract(
    parity_root=PARITY_ROOT, prior_root=PRIOR_ROOT, contract_root=CONTRACT_ROOT
):
    raw = (contract_root / FILE).read_bytes()
    if hashlib.sha256(raw).hexdigest() != FILE_SHA256:
        raise ValueError("Executable-punctuation contract bytes changed")
    correction = json.loads(raw)
    validate_contract(correction, parity_root, prior_root)
    return correction


def compose_cases(
    cases, contract, correction=None, parity_root=PARITY_ROOT, prior_root=PRIOR_ROOT
):
    """Change three exact labels only after exact stage07 composition."""
    if contract not in INDICES:
        raise ValueError("Unknown executable-punctuation composition lane")
    if correction is None:
        correction = load_contract(parity_root, prior_root)
    validate_contract(correction, parity_root, prior_root)
    binding = next(
        b for b in correction["fixture_bindings"] if b["contract"] == contract
    )
    if digest(cases) != binding["stage07_cases_sha256"]:
        raise ValueError("Exact stage07 executable-punctuation inventory differs")
    composed = copy.deepcopy(cases)
    for index in INDICES[contract]:
        composed[index]["expectation"] = "oracle"
    if digest(composed) != binding["stage08_cases_sha256"]:
        raise ValueError("Exact stage08 executable-punctuation inventory differs")
    return composed


def composition_records(contract, correction=None):
    if contract not in INDICES:
        raise ValueError("Unknown executable-punctuation composition lane")
    if correction is None:
        correction = load_contract()
    validate_contract(correction)
    return [
        dict(
            contract=CONTRACT,
            historical_control=r["name"],
            historical_pointer=r["pointer"],
        )
        for r in correction["replacements"]
        if r["fixture_contract"] == contract
    ]


class ExecutablePunctuationFixtureTest(unittest.TestCase):
    def setUp(self):
        self.correction = load_contract()

    def stage07(self, binding):
        original = json.loads((PARITY_ROOT / binding["fixture"]).read_bytes())["cases"]
        cases = copy.deepcopy(original)
        for index in binding["prior_promotion_indices"]:
            self.assertEqual(cases[index]["expectation"], "baseline")
            cases[index]["expectation"] = "oracle"
        self.assertEqual(digest(cases), binding["stage07_cases_sha256"])
        return original, cases

    def test_exact_three_preimages_and_asymmetric_policy_scope(self):
        rows = self.correction["replacements"]
        self.assertEqual(len(rows), 3)
        self.assertEqual(
            Counter(r["historical_active_policy"] for r in rows),
            {"ordinary": 2, "before_token_limit": 1},
        )
        self.assertEqual(
            Counter(r["found"] for r in rows), {"Punctuation('!')": 2, "Spread": 1}
        )
        for row in rows:
            self.assertEqual(row["site"], "ExecutableDocument")
            self.assertEqual(row["expected"], "Unexpected")
            self.assertEqual(row["ordinary_policy"], row["found"] != "Spread")
            self.assertIs(row["before_limit_policy"], True)
            case = row["preimage"]
            self.assertEqual(set(case["reference"]), {"code", "stdout", "stderr"})
            for lane in ["legacy", "companion"]:
                self.assertEqual(
                    set(case["baseline"][lane]), {"code", "stdout", "stderr"}
                )
        self.assertEqual(sum(r["frozen_parse_sha256"] is not None for r in rows), 1)
        for binding in self.correction["fixture_bindings"]:
            self.assertEqual(
                binding["replacement_indices"], INDICES[binding["contract"]]
            )
            self.assertEqual(
                [
                    r["pointer"]
                    for r in rows
                    if r["fixture_contract"] == binding["contract"]
                ],
                [f"/cases/{i}" for i in INDICES[binding["contract"]]],
            )

    def test_composition_preserves_every_other_row_and_old_promotion(self):
        changed = 0
        for binding in self.correction["fixture_bindings"]:
            original, prior = self.stage07(binding)
            untouched = copy.deepcopy(prior)
            result = compose_cases(prior, binding["contract"], self.correction)
            for i, (before, after) in enumerate(zip(prior, result, strict=True)):
                promoted = i in INDICES[binding["contract"]]
                self.assertEqual(
                    after, dict(before, expectation="oracle") if promoted else before
                )
                changed += promoted
                if i in binding["prior_promotion_indices"]:
                    self.assertEqual(after["expectation"], "oracle")
                    self.assertEqual(original[i]["expectation"], "baseline")
            result[0]["document"] = "changed returned value"
            self.assertEqual(prior, untouched)
        self.assertEqual(changed, 3)

    def test_scope_provenance_order_and_identity_fail_closed(self):
        mutations = [
            lambda c: c.update(contract="other"),
            lambda c: c.update(extra=True),
            lambda c: c["provenance"].update(runtime_executed=True),
            lambda c: c["fixture_bindings"][0].update(case_count=True),
            lambda c: c["replacements"].reverse(),
            lambda c: c["replacements"].pop(),
            lambda c: c["replacements"].append(c["replacements"][0]),
        ]
        for field, value in [
            ("fixture", "other.json"),
            ("pointer", "/cases/0"),
            ("name", "other"),
            ("site", "Other"),
            ("expected", "Value"),
            ("found", "Punctuation('.')"),
            ("ordinary_policy", False),
            ("before_limit_policy", False),
        ]:
            mutations.append(
                lambda c, f=field, v=value: c["replacements"][0].update({f: v})
            )
        mutations.append(lambda c: c["replacements"][1].update(ordinary_policy=True))
        for mutate in mutations:
            changed = copy.deepcopy(self.correction)
            mutate(changed)
            with self.assertRaises(ValueError):
                validate_contract(changed)

    def test_complete_preimage_and_all_process_envelope_mutations_fail_closed(self):
        for index in range(3):
            for field, value in [
                ("document", "{profile}"),
                ("args", ["--timeout-ms", "0"]),
                ("name", "other"),
                ("expectation", "oracle"),
                ("frozen_parse", {}),
            ]:
                changed = copy.deepcopy(self.correction)
                changed["replacements"][index]["preimage"][field] = value
                with (
                    self.subTest(index=index, field=field),
                    self.assertRaises(ValueError),
                ):
                    validate_contract(changed)
            for lane in ["reference", "legacy", "companion"]:
                for field, value in [
                    ("code", True),
                    ("stdout", {}),
                    ("stderr", "changed"),
                ]:
                    changed = copy.deepcopy(self.correction)
                    case = changed["replacements"][index]["preimage"]
                    envelope = (
                        case["reference"]
                        if lane == "reference"
                        else case["baseline"][lane]
                    )
                    envelope[field] = value
                    with (
                        self.subTest(index=index, lane=lane, field=field),
                        self.assertRaises(ValueError),
                    ):
                        validate_contract(changed)

    def test_partial_reordered_mutated_and_repeated_target_composition_fail_closed(
        self,
    ):
        for binding in self.correction["fixture_bindings"]:
            original, prior = self.stage07(binding)
            contract = binding["contract"]
            result = compose_cases(prior, contract, self.correction)
            bad = [original, prior[:-1], prior + [prior[0]], list(reversed(prior))]
            if INDICES[contract]:
                bad.append(result)
            for changed in bad:
                with self.assertRaises(ValueError):
                    compose_cases(changed, contract, self.correction)
            for index in INDICES[contract] + binding["prior_promotion_indices"] + [0]:
                for field, value in [
                    ("document", "other"),
                    ("args", ["--variables", "[]"]),
                    ("reference", {}),
                    ("baseline", {}),
                ]:
                    changed = copy.deepcopy(prior)
                    changed[index][field] = value
                    with self.assertRaises(ValueError):
                        compose_cases(changed, contract, self.correction)
        with self.assertRaises(ValueError):
            compose_cases([], "unknown", self.correction)

    def test_all_historical_and_contract_raw_byte_drift_fail_closed(self):
        class BytesFile:
            def __init__(self, raw):
                self.raw = raw

            def read_bytes(self):
                return self.raw

        class BytesRoot:
            def __init__(self, root, changed):
                self.root, self.changed = root, changed

            def __truediv__(self, name):
                raw = (self.root / name).read_bytes()
                return BytesFile(raw + b" " if name == self.changed else raw)

        with self.assertRaises(ValueError):
            load_contract(contract_root=BytesRoot(CONTRACT_ROOT, FILE))
        for binding in self.correction["fixture_bindings"]:
            with self.assertRaises(ValueError):
                validate_contract(
                    self.correction, BytesRoot(PARITY_ROOT, binding["fixture"])
                )
        for prior in self.correction["provenance"]["prior_contracts"]:
            name = prior["fixture"]
            with self.assertRaises(ValueError):
                validate_contract(
                    self.correction,
                    BytesRoot(PARITY_ROOT, name),
                    BytesRoot(PRIOR_ROOT, name),
                )

    def test_five_lane_inventory_and_matching_bang_control_are_preserved(self):
        residual = matching = 0
        for binding in self.correction["fixture_bindings"]:
            original, before = self.stage07(binding)
            cases = copy.deepcopy(original)
            for prior in self.correction["provenance"]["prior_contracts"]:
                correction = json.loads(prior_bytes(prior["fixture"]))
                for row in (
                    correction.get("preserved_promotions", [])
                    + correction["replacements"]
                ):
                    if row["fixture"] == binding["fixture"]:
                        cases[int(row["pointer"].split("/")[-1])]["expectation"] = (
                            "oracle"
                        )
            self.assertEqual(cases, before)
            result = compose_cases(cases, binding["contract"], self.correction)
            if (
                binding["contract"]
                == "native-graphql-parser-required-value-precedence-v1"
            ):
                self.assertEqual(result[39], original[39])
                self.assertEqual(
                    result[39]["name"], "overbudget-earlier-bang-before-bracket"
                )
                self.assertEqual(
                    result[39]["reference"], result[39]["baseline"]["legacy"]
                )
            for case in result:
                if case["expectation"] == "baseline":
                    equal = digest(case["reference"]) == digest(
                        case["baseline"]["legacy"]
                    )
                    residual += not equal
                    matching += equal
        self.assertEqual((residual, matching), (24, 6))


if __name__ == "__main__":

    def guard(event, args):
        if event in {"subprocess.Popen", "os.system", "os.posix_spawn", "os.fork"}:
            raise RuntimeError("Metadata controls forbid process creation")
        if event == "import" and args[0].split(".")[0] in {
            "catabolic",
            "graphql",
            "kyoto",
        }:
            raise RuntimeError("Metadata controls forbid product/oracle imports")

    sys.addaudithook(guard)
    unittest.main(verbosity=2)
