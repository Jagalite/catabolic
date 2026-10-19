# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Exact metadata composition only; historical captures are not candidate results."""

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
CONTRACT_ROOT = HERE / "parity"
CONTRACT = "native-graphql-parser-directive-value-closers-v1"
FILE = "graphql-parser-directive-value-closers-v1.json"
FILE_SHA256 = "6eb2349be6e964b3a9443996279426bb524ac561866e3cfac1eb3b3b553210bb"
SECTION_DIGESTS = {
    "provenance": "73b97a20e8b03b79cb5f6c05b6d592ea10871722c78de19e04465aa24aa29688",
    "fixture_bindings": "bd964cd97b00ccbd2dfd784ff83b5fa516b662a61ca18b3b9a831565121a45d5",
    "replacements": "70e3ceac019c41a04b4c5121501a0fe6ee44ad0f74fcc73f2ce3c56eca4ae5a1",
}
INDICES = {
    "native-graphql-parser-boundary-ownership-v1": [12, 13, 14, 53],
    "native-graphql-parser-required-value-precedence-v1": [35, 36, 37],
}


def digest(value):
    return hashlib.sha256(
        json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode()
    ).hexdigest()


def validate_contract(correction, parity_root=PARITY_ROOT):
    if set(correction) != {"contract", *SECTION_DIGESTS}:
        raise ValueError("Unexpected directive-closer contract fields")
    if correction["contract"] != CONTRACT:
        raise ValueError("Unexpected directive-closer contract identity")
    for field, expected in SECTION_DIGESTS.items():
        if digest(correction[field]) != expected:
            raise ValueError(
                f"Directive-closer {field} provenance or preimages changed"
            )
    provenance = correction["provenance"]
    if (
        hashlib.sha256(
            (parity_root / provenance["prior_fixture"]).read_bytes()
        ).hexdigest()
        != provenance["prior_fixture_sha256"]
    ):
        raise ValueError("Prior field-token contract bytes changed")
    fixtures = {}
    for binding in correction["fixture_bindings"]:
        raw = (parity_root / binding["fixture"]).read_bytes()
        if hashlib.sha256(raw).hexdigest() != binding["sha256"]:
            raise ValueError("Historical directive-closer fixture bytes changed")
        fixture = json.loads(raw)
        if (
            fixture["contract"] != binding["contract"]
            or len(fixture["cases"]) != binding["case_count"]
            or digest(fixture["cases"]) != binding["cases_sha256"]
            or digest(fixture["provenance"]) != binding["provenance_sha256"]
        ):
            raise ValueError("Historical directive-closer inventory changed")
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
            raise ValueError("Historical directive-closer input or envelope changed")
        for value, key in (
            (original, "case_sha256"),
            (original["reference"], "reference_sha256"),
            (original["baseline"]["legacy"], "legacy_sha256"),
            (original["baseline"]["companion"], "companion_sha256"),
            (original["frozen_parse"], "frozen_parse_sha256"),
        ):
            if digest(value) != row[key]:
                raise ValueError("Directive-closer preimage hash differs")
        if (
            hashlib.sha256(original["document"].encode()).hexdigest()
            != row["document_sha256"]
            or len(original["document"].encode()) != row["document_bytes"]
            or original["reference"]["stdout"]["data"]["errors"]
            != [original["frozen_parse"]["formatted"]]
        ):
            raise ValueError("Directive-closer document/frozen parse differs")


def load_contract(parity_root=PARITY_ROOT, contract_root=CONTRACT_ROOT):
    raw = (contract_root / FILE).read_bytes()
    if hashlib.sha256(raw).hexdigest() != FILE_SHA256:
        raise ValueError("Directive-closer contract bytes changed")
    correction = json.loads(raw)
    validate_contract(correction, parity_root)
    return correction


def compose_cases(cases, contract, correction=None, parity_root=PARITY_ROOT):
    """Change seven exact labels only after exact stage06 composition."""
    if contract not in INDICES:
        raise ValueError("Unknown directive-closer composition lane")
    if correction is None:
        correction = load_contract(parity_root)
    validate_contract(correction, parity_root)
    binding = next(
        b for b in correction["fixture_bindings"] if b["contract"] == contract
    )
    if digest(cases) != binding["stage06_cases_sha256"]:
        raise ValueError("Exact stage06 directive-closer inventory differs")
    composed = copy.deepcopy(cases)
    for index in INDICES[contract]:
        composed[index]["expectation"] = "oracle"
    if digest(composed) != binding["stage07_cases_sha256"]:
        raise ValueError("Exact stage07 directive-closer inventory differs")
    return composed


def composition_records(contract, correction=None):
    if contract not in INDICES:
        raise ValueError("Unknown directive-closer composition lane")
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


class DirectiveValueCloserFixtureTest(unittest.TestCase):
    def setUp(self):
        self.correction = load_contract()

    def stage06(self, binding):
        original = json.loads((PARITY_ROOT / binding["fixture"]).read_bytes())["cases"]
        cases = copy.deepcopy(original)
        for index in binding["prior_promotion_indices"]:
            self.assertEqual(cases[index]["expectation"], "baseline")
            cases[index]["expectation"] = "oracle"
        self.assertEqual(digest(cases), binding["stage06_cases_sha256"])
        return original, cases

    def test_exact_seven_preimages_and_policy_scope(self):
        rows = self.correction["replacements"]
        self.assertEqual(len(rows), 7)
        self.assertEqual(
            Counter(r["historical_active_policy"] for r in rows),
            {"ordinary": 4, "before_token_limit": 3},
        )
        self.assertEqual(
            Counter((r["position"], r["closer"]) for r in rows),
            {("Root", "]"): 2, ("ListElement", ")"): 3, ("ObjectMember", "}"): 2},
        )
        for contract, indices in INDICES.items():
            self.assertEqual(
                [r["pointer"] for r in rows if r["fixture_contract"] == contract],
                [f"/cases/{i}" for i in indices],
            )
        for row in rows:
            self.assertEqual(row["context"], "Argument(FieldDirective)")
            self.assertTrue(row["ordinary_policy"])
            self.assertTrue(row["before_limit_policy"])
            case = row["preimage"]
            self.assertEqual(set(case["reference"]), {"code", "stdout", "stderr"})
            for lane in ("legacy", "companion"):
                self.assertEqual(
                    set(case["baseline"][lane]), {"code", "stdout", "stderr"}
                )

    def test_composition_preserves_all_other_rows_and_prior_promotions(self):
        changed = 0
        for binding in self.correction["fixture_bindings"]:
            original, prior = self.stage06(binding)
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
            result[0]["document"] = "changed return value"
            self.assertEqual(prior, untouched)
        self.assertEqual(changed, 7)

    def test_scope_provenance_order_and_identity_fail_closed(self):
        mutations = [
            lambda c: c.update(contract="unversioned"),
            lambda c: c.update(extra=True),
            lambda c: c["provenance"].update(runtime_executed=True),
            lambda c: c["fixture_bindings"][0].update(case_count=True),
            lambda c: c["replacements"].reverse(),
            lambda c: c["replacements"].pop(),
            lambda c: c["replacements"].append(c["replacements"][0]),
        ]
        for field, value in (
            ("fixture", "other.json"),
            ("pointer", "/cases/0"),
            ("name", "other"),
            ("context", "Argument(Directive)"),
            ("position", "ObjectMember"),
            ("closer", "}"),
            ("ordinary_policy", False),
            ("before_limit_policy", False),
        ):
            mutations.append(
                lambda c, f=field, v=value: c["replacements"][0].update({f: v})
            )
        for mutate in mutations:
            changed = copy.deepcopy(self.correction)
            mutate(changed)
            with self.assertRaises(ValueError):
                validate_contract(changed)

    def test_complete_preimage_and_envelope_mutations_fail_closed(self):
        for index in range(7):
            for field, value in (
                ("document", "{profile}"),
                ("args", ["--timeout-ms", "0"]),
                ("name", "other"),
                ("expectation", "oracle"),
                ("frozen_parse", {}),
            ):
                changed = copy.deepcopy(self.correction)
                changed["replacements"][index]["preimage"][field] = value
                with (
                    self.subTest(index=index, field=field),
                    self.assertRaises(ValueError),
                ):
                    validate_contract(changed)
            for lane in ("reference", "legacy", "companion"):
                for field, value in (
                    ("code", True),
                    ("stdout", {}),
                    ("stderr", "changed"),
                ):
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

    def test_raw_partial_repeated_and_malformed_composition_fail_closed(self):
        for binding in self.correction["fixture_bindings"]:
            original, prior = self.stage06(binding)
            contract = binding["contract"]
            result = compose_cases(prior, contract, self.correction)
            for changed in (
                original,
                prior[:-1],
                prior + [prior[0]],
                list(reversed(prior)),
                result,
            ):
                with self.assertRaises(ValueError):
                    compose_cases(changed, contract, self.correction)
            for index in INDICES[contract] + binding["prior_promotion_indices"] + [0]:
                for field, value in (
                    ("document", "other"),
                    ("args", ["--variables", "[]"]),
                    ("reference", {}),
                    ("baseline", {}),
                ):
                    changed = copy.deepcopy(prior)
                    changed[index][field] = value
                    with self.assertRaises(ValueError):
                        compose_cases(changed, contract, self.correction)
            with self.assertRaises(ValueError):
                compose_cases(prior, "unknown", self.correction)

    def test_raw_contract_historical_and_prior_byte_drift_fail_closed(self):
        # Memory-backed file views avoid copying any source tree or fixtures.
        class BytesFile:
            def __init__(self, raw):
                self.raw = raw

            def read_bytes(self):
                return self.raw

        class BytesRoot:
            def __init__(self, root, changed):
                self.root = root
                self.changed = changed

            def __truediv__(self, name):
                raw = (self.root / name).read_bytes()
                return BytesFile(raw + b" " if name == self.changed else raw)

        with self.assertRaises(ValueError):
            load_contract(contract_root=BytesRoot(CONTRACT_ROOT, FILE))
        names = [b["fixture"] for b in self.correction["fixture_bindings"]]
        names.append(self.correction["provenance"]["prior_fixture"])
        for name in names:
            with self.subTest(file=name), self.assertRaises(ValueError):
                validate_contract(self.correction, BytesRoot(PARITY_ROOT, name))

    def test_stage06_full_inventory_retains_exact_prior_labels(self):
        lexical = json.loads(
            (PARITY_ROOT / "graphql-parser-lexical-ownership-v1.json").read_bytes()
        )
        field_tokens = json.loads(
            (PARITY_ROOT / "graphql-parser-field-value-tokens-v1.json").read_bytes()
        )
        promoted = residual = matching = 0
        for binding in lexical["fixture_bindings"]:
            fixture = json.loads((PARITY_ROOT / binding["fixture"]).read_bytes())
            self.assertEqual(
                hashlib.sha256(
                    (PARITY_ROOT / binding["fixture"]).read_bytes()
                ).hexdigest(),
                binding["sha256"],
            )
            cases = copy.deepcopy(fixture["cases"])
            for row in lexical["preserved_promotions"] + lexical["replacements"]:
                if row["fixture"] == binding["fixture"]:
                    cases[int(row["pointer"].split("/")[-1])]["expectation"] = "oracle"
            self.assertEqual(digest(cases), binding["stage05_cases_sha256"])
            for row in field_tokens["replacements"]:
                if row["fixture"] == binding["fixture"]:
                    cases[int(row["pointer"].split("/")[-1])]["expectation"] = "oracle"
            if fixture["contract"] in INDICES:
                cases = compose_cases(cases, fixture["contract"], self.correction)
                promoted += len(INDICES[fixture["contract"]])
            for case in cases:
                if case["expectation"] == "baseline":
                    equal = digest(case["reference"]) == digest(
                        case["baseline"]["legacy"]
                    )
                    matching += equal
                    residual += not equal
        self.assertEqual((promoted, residual, matching), (7, 27, 6))


if __name__ == "__main__":
    # Executed controls can read fixture JSON and use stdlib only. This blocks
    # product/oracle imports and all process creation, including indirect calls.
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
