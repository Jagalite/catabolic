# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Six exact field-token promotions over sealed stage05 composition.

These tests examine historical captures and metadata only. Native execution,
new EOF admission expectations and broader capture remain separate obligations.
"""

import copy
import hashlib
import json
import shutil
import tempfile
import unittest
from collections import Counter
from pathlib import Path

from tests import test_rust_graphql_lexical_ownership as lexical

PARITY_ROOT = Path(__file__).resolve().parent / "parity"
CONTRACT = "native-graphql-parser-field-value-tokens-v1"
FILE = "graphql-parser-field-value-tokens-v1.json"
FILE_SHA256 = "89793e8e4e19227c61dec263b77ff1016531e850d2a41940d473f7084e742184"
SECTION_DIGESTS = {
    "provenance": "ea2a3f06046d6c170947aaaa25cdbd97becda39d972f4e8db52940a1fd69737c",
    "fixture_bindings": "268397e958be2f5fec21e6a090fc483f0f71d8c0d6a3ff82341522ce1717ab96",
    "replacements": "b314cc744f4ba9f5be11a5178bc1a4b1cfbcfe74df8ca1ec54fda0602c1f2d79",
}
INDICES = {
    "native-graphql-parser-boundary-ownership-v1": [36, 37, 40, 56, 57],
    "native-graphql-parser-required-value-precedence-v1": [46],
}


def validate_contract(correction, parity_root=PARITY_ROOT):
    if set(correction) != {"contract", *SECTION_DIGESTS}:
        raise ValueError("Unexpected field-token contract fields")
    if correction["contract"] != CONTRACT:
        raise ValueError("Unexpected field-token contract identity")
    for field, expected in SECTION_DIGESTS.items():
        if lexical.digest(correction[field]) != expected:
            raise ValueError(f"Field-token {field} provenance or preimages changed")
    provenance = correction["provenance"]
    if (
        hashlib.sha256(
            (parity_root / provenance["prior_fixture"]).read_bytes()
        ).hexdigest()
        != provenance["prior_fixture_sha256"]
    ):
        raise ValueError("Prior lexical contract bytes changed")
    fixtures = {}
    for binding in correction["fixture_bindings"]:
        raw = (parity_root / binding["fixture"]).read_bytes()
        if hashlib.sha256(raw).hexdigest() != binding["sha256"]:
            raise ValueError("Historical field-token fixture bytes changed")
        fixture = json.loads(raw)
        if (
            fixture["contract"] != binding["contract"]
            or len(fixture["cases"]) != binding["case_count"]
            or lexical.digest(fixture["cases"]) != binding["cases_sha256"]
            or lexical.digest(fixture["provenance"]) != binding["provenance_sha256"]
        ):
            raise ValueError("Historical field-token inventory changed")
        fixtures[binding["fixture"]] = fixture
    for replacement in correction["replacements"]:
        fixture = fixtures[replacement["fixture"]]
        original = lexical.pointer(fixture, replacement["pointer"])
        if (
            fixture["contract"] != replacement["fixture_contract"]
            or original["name"] != replacement["name"]
            or lexical.digest(original) != lexical.digest(replacement["preimage"])
            or original["expectation"] != "baseline"
            or lexical.digest(original["reference"])
            == lexical.digest(original["baseline"]["legacy"])
        ):
            raise ValueError("Historical field-token input or envelope changed")
        for value, key in (
            (original, "case_sha256"),
            (original["reference"], "reference_sha256"),
            (original["baseline"]["legacy"], "legacy_sha256"),
            (original["baseline"]["companion"], "companion_sha256"),
        ):
            if lexical.digest(value) != replacement[key]:
                raise ValueError("Field-token preimage hash differs")
        if (
            hashlib.sha256(original["document"].encode()).hexdigest()
            != replacement["document_sha256"]
        ):
            raise ValueError("Field-token document bytes differ")


def load_contract(parity_root=PARITY_ROOT):
    raw = (parity_root / FILE).read_bytes()
    if hashlib.sha256(raw).hexdigest() != FILE_SHA256:
        raise ValueError("Field-token contract bytes changed")
    correction = json.loads(raw)
    validate_contract(correction, parity_root)
    return correction


def compose_cases(cases, contract, correction=None, parity_root=PARITY_ROOT):
    """Accept only exact stage05 inventories in the two affected lanes."""
    if contract not in INDICES:
        raise ValueError("Unknown field-token composition lane")
    if correction is None:
        correction = load_contract(parity_root)
    validate_contract(correction, parity_root)
    binding = next(
        b for b in correction["fixture_bindings"] if b["contract"] == contract
    )
    if lexical.digest(cases) != binding["stage05_cases_sha256"]:
        raise ValueError("Exact stage05 field-token inventory differs")
    composed = copy.deepcopy(cases)
    for index in INDICES[contract]:
        composed[index]["expectation"] = "oracle"
    if lexical.digest(composed) != binding["stage06_cases_sha256"]:
        raise ValueError("Exact stage06 field-token inventory differs")
    return composed


def composition_records(contract, correction=None):
    if contract not in INDICES:
        raise ValueError("Unknown field-token composition lane")
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


class FieldValueTokenFixtureTest(unittest.TestCase):
    def setUp(self):
        self.correction = load_contract()

    def test_exact_six_captures_and_independent_policy_scope(self):
        rows = self.correction["replacements"]
        self.assertEqual(len(rows), 6)
        self.assertEqual(Counter(r["token"] for r in rows), {"Bang": 4, "Eof": 2})
        self.assertEqual(
            Counter(r["historical_active_policy"] for r in rows),
            {"ordinary": 5, "before_token_limit": 1},
        )
        for contract, indices in INDICES.items():
            self.assertEqual(
                [r["pointer"] for r in rows if r["fixture_contract"] == contract],
                [f"/cases/{index}" for index in indices],
            )
        for row in rows:
            self.assertTrue(row["ordinary_policy"])
            self.assertEqual(row["before_limit_policy"], row["token"] == "Bang")
            self.assertEqual(row["context"], "Argument(Field)")
            if row["token"] == "Eof":
                self.assertIn(row["position"], {"Root", "ListElement"})
            self.assertNotEqual(
                row["preimage"]["reference"], row["preimage"]["baseline"]["legacy"]
            )

    def test_composition_preserves_old_promotions_and_every_other_byte(self):
        promoted, residual, matching = 0, 0, 0
        prior = lexical.load_contract()
        for binding in prior["fixture_bindings"]:
            raw = (PARITY_ROOT / binding["fixture"]).read_bytes()
            original = json.loads(raw)["cases"]
            contract = binding["contract"]
            stage05 = lexical.compose_cases(original, contract)
            unchanged = copy.deepcopy(stage05)
            result = (
                compose_cases(stage05, contract, self.correction)
                if contract in INDICES
                else stage05
            )
            for index, (before, after) in enumerate(zip(stage05, result, strict=True)):
                changed = index in INDICES.get(contract, [])
                self.assertEqual(
                    after, dict(before, expectation="oracle") if changed else before
                )
                promoted += changed
                if after["expectation"] == "baseline":
                    equal = lexical.digest(after["reference"]) == lexical.digest(
                        after["baseline"]["legacy"]
                    )
                    matching += equal
                    residual += not equal
            self.assertEqual(stage05, unchanged)
            self.assertEqual((PARITY_ROOT / binding["fixture"]).read_bytes(), raw)
            if contract in INDICES:
                result[0]["document"] = "changed result"
                self.assertEqual(stage05, unchanged)
        self.assertEqual((promoted, residual, matching), (6, 34, 6))

    def test_contract_identity_scope_and_order_mutations_fail_closed(self):
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
            ("token", "Other"),
            ("position", "ObjectMember"),
            ("context", "Argument(Directive)"),
            ("before_limit_policy", True),
        ):
            mutations.append(
                lambda c, f=field, v=value: c["replacements"][0].update({f: v})
            )
        for mutate in mutations:
            changed = copy.deepcopy(self.correction)
            mutate(changed)
            with self.assertRaises(ValueError):
                validate_contract(changed)

    def test_each_full_preimage_and_complete_envelope_fails_closed(self):
        for index in range(6):
            for field, value in (
                ("document", "{profile}"),
                ("args", ["--timeout-ms", "0"]),
                ("name", "other"),
                ("expectation", "oracle"),
                ("reference", {}),
                ("baseline", {}),
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

    def test_only_exact_stage05_inventory_can_be_composed_once(self):
        for binding in self.correction["fixture_bindings"]:
            raw = json.loads((PARITY_ROOT / binding["fixture"]).read_text())["cases"]
            contract = binding["contract"]
            prior = lexical.compose_cases(raw, contract)
            composed = compose_cases(prior, contract, self.correction)
            for changed in (
                raw,
                prior[:-1],
                prior + [prior[0]],
                list(reversed(prior)),
                composed,
            ):
                with self.assertRaises(ValueError):
                    compose_cases(changed, contract, self.correction)
            for index in INDICES[contract] + [0]:
                for field, value in (
                    ("document", "other"),
                    ("expectation", "changed"),
                    ("args", ["--variables", "[]"]),
                    ("reference", {}),
                    ("baseline", {}),
                ):
                    changed = copy.deepcopy(prior)
                    changed[index][field] = value
                    with (
                        self.subTest(index=index, field=field),
                        self.assertRaises(ValueError),
                    ):
                        compose_cases(changed, contract, self.correction)
        with self.assertRaises(ValueError):
            compose_cases([], "unknown", self.correction)

    def test_historical_and_new_fixture_byte_drift_fails_closed(self):
        files = [b["fixture"] for b in self.correction["fixture_bindings"]]
        files += [self.correction["provenance"]["prior_fixture"], FILE]
        with tempfile.TemporaryDirectory(prefix="field-token-metadata-") as temporary:
            root = Path(temporary)
            for name in files:
                shutil.copyfile(PARITY_ROOT / name, root / name)
            for name in files:
                original = (root / name).read_bytes()
                (root / name).write_bytes(original + b"\n")
                with self.subTest(name=name), self.assertRaises(ValueError):
                    load_contract(root)
                (root / name).write_bytes(original)
            validate_contract(self.correction, root)


if __name__ == "__main__":
    unittest.main()
