# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Metadata-only exact-preimage composition; no product/oracle imports or runs.

Captured envelopes are historical evidence, not stage04 or stage05 runtime
results. The added lexical branches await separately authorized fresh capture.
Existing process and owner suites consume these exact composed expectations.
"""

import copy
import hashlib
import json
import shutil
import tempfile
import unittest
from collections import Counter
from pathlib import Path

PARITY_ROOT = Path(__file__).resolve().parent / "parity"
CONTRACT = "native-graphql-parser-lexical-ownership-v1"
FILE = "graphql-parser-lexical-ownership-v1.json"
FILE_SHA256 = "e23562386198aff15ba45cc16a22e4c8281a3f0011815b57f8faadbc6568ffae"
SECTION_DIGESTS = {
    "provenance": "a92ce50ea8463255ea5971d1beab73e4c84c66c3d7e8881f151ab3eeab389c5d",
    "fixture_bindings": "09610d4ee852f570c07eadb8b129141e3ec20b8489f30eb5fa9560b1d5d682e5",
    "preserved_promotions": "0840c380b24b3d07f812e813ff6ab1cef94b2440d157c88abf6bfe99053b1c4f",
    "replacements": "b442a5d75a0e9cd8b4b8e5471040c4f46babd75419809e5a6507a2e0959a303e",
}
INDICES = {
    "native-graphql-parser-budget-precedence-v2": [117, 118, 119],
    "native-graphql-parser-diagnostic-ownership-v1": [22, 23],
    "native-graphql-unsupported-scalar-precedence-v1": [22, 23, 29],
    "native-graphql-parser-boundary-ownership-v1": [19, 22, 27],
    "native-graphql-parser-required-value-precedence-v1": [40, 43],
}
PRESERVED_INDICES = {
    "native-graphql-parser-budget-precedence-v2": [120],
    "native-graphql-parser-diagnostic-ownership-v1": [18, 19, 20, 24],
}


def digest(value):
    """JSON canonical identity distinguishes booleans, numbers and strings."""
    return hashlib.sha256(
        json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode()
    ).hexdigest()


def pointer(value, path):
    for component in path.removeprefix("/").split("/"):
        value = value[int(component)] if isinstance(value, list) else value[component]
    return value


def validate_contract(correction, parity_root=PARITY_ROOT):
    """Require sealed inventory, provenance and every full historical preimage."""
    if set(correction) != {"contract", *SECTION_DIGESTS}:
        raise ValueError("Unexpected lexical contract fields")
    if correction["contract"] != CONTRACT:
        raise ValueError("Unexpected lexical contract identity")
    for field, expected in SECTION_DIGESTS.items():
        if digest(correction[field]) != expected:
            raise ValueError(f"Lexical {field} provenance or exact preimages changed")
    fixtures = {}
    for binding in correction["fixture_bindings"]:
        raw = (parity_root / binding["fixture"]).read_bytes()
        if hashlib.sha256(raw).hexdigest() != binding["sha256"]:
            raise ValueError("Historical lexical fixture bytes changed")
        fixture = json.loads(raw)
        if (
            fixture["contract"] != binding["contract"]
            or len(fixture["cases"]) != binding["case_count"]
            or digest(fixture["cases"]) != binding["cases_sha256"]
        ):
            raise ValueError("Historical lexical inventory changed")
        fixtures[binding["fixture"]] = fixture
    for group in ("preserved_promotions", "replacements"):
        for replacement in correction[group]:
            fixture = fixtures[replacement["fixture"]]
            original = pointer(fixture, replacement["pointer"])
            if (
                fixture["contract"] != replacement["fixture_contract"]
                or original["name"] != replacement["name"]
                or digest(original) != digest(replacement["preimage"])
                or original["expectation"] != "baseline"
                or digest(original["reference"])
                == digest(original["baseline"]["legacy"])
            ):
                raise ValueError(
                    "Historical lexical input or complete envelope changed"
                )
            if group == "preserved_promotions":
                prior = pointer(
                    fixtures[replacement["prior_fixture"]], replacement["prior_pointer"]
                )
                if digest(prior) != digest(replacement["prior_preimage"]):
                    raise ValueError("Prior promotion preimage changed")
                for old, new in (
                    ("document", "document"),
                    ("args", "args"),
                    ("reference", "expected"),
                    ("baseline", "baseline"),
                ):
                    if digest(original[old]) != digest(prior[new]):
                        raise ValueError("Prior promotion differs across fixtures")
            else:
                for value, key in (
                    (original, "case_sha256"),
                    (original["reference"], "reference_sha256"),
                    (original["baseline"]["legacy"], "legacy_sha256"),
                    (original["baseline"]["companion"], "companion_sha256"),
                ):
                    if digest(value) != replacement[key]:
                        raise ValueError("Lexical preimage hash differs")
                if (
                    hashlib.sha256(original["document"].encode()).hexdigest()
                    != (replacement["document_sha256"])
                ):
                    raise ValueError("Lexical document bytes differ")
    return fixtures


def load_contract(parity_root=PARITY_ROOT):
    raw = (parity_root / FILE).read_bytes()
    if hashlib.sha256(raw).hexdigest() != FILE_SHA256:
        raise ValueError("Lexical contract bytes changed")
    correction = json.loads(raw)
    validate_contract(correction, parity_root)
    return correction


def compose_cases(
    cases, contract, correction=None, parity_root=PARITY_ROOT, *, stage04_composed=False
):
    """Apply only 13 new labels, retaining the five prior exact promotions.

    The caller explicitly selects raw or stage04-composed input. A stage05 list,
    unknown lane, partial composition or any other input drift is rejected.
    """
    if contract not in INDICES:
        raise ValueError("Unknown lexical composition lane")
    if correction is None:
        correction = load_contract(parity_root)
    validate_contract(correction, parity_root)
    binding = next(
        b for b in correction["fixture_bindings"] if b["contract"] == contract
    )
    preimage_key = "stage04_cases_sha256" if stage04_composed else "cases_sha256"
    if digest(cases) != binding[preimage_key]:
        raise ValueError("Exact pre-composition inventory differs")
    composed = copy.deepcopy(cases)
    for index in PRESERVED_INDICES.get(contract, []) + INDICES[contract]:
        composed[index]["expectation"] = "oracle"
    if digest(composed) != binding["stage05_cases_sha256"]:
        raise ValueError("Exact post-composition inventory differs")
    return composed


def composition_records(contract, correction=None):
    if contract not in INDICES:
        raise ValueError("Unknown lexical composition lane")
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


class LexicalOwnershipFixtureTest(unittest.TestCase):
    def setUp(self):
        self.correction = load_contract()

    def test_exact_thirteen_preimages_and_five_preserved_promotions(self):
        self.assertEqual(len(self.correction["replacements"]), 13)
        self.assertEqual(len(self.correction["preserved_promotions"]), 5)
        for contract, indices in INDICES.items():
            self.assertEqual(
                [
                    r["pointer"]
                    for r in self.correction["replacements"]
                    if r["fixture_contract"] == contract
                ],
                [f"/cases/{index}" for index in indices],
            )
        self.assertEqual(
            Counter(r["reason"] for r in self.correction["replacements"]),
            {
                "numeric lexical reason": 8,
                "single-quote lexical reason": 4,
                "unicode lexical reason": 1,
            },
        )
        self.assertEqual(
            Counter(
                r["historical_active_policy"] for r in self.correction["replacements"]
            ),
            {"ordinary": 5, "before_token_limit": 8},
        )

    def test_composition_preserves_every_unrelated_preimage_and_input_bytes(self):
        residual, matching, newly_promoted = [], [], 0
        for binding in self.correction["fixture_bindings"]:
            path = PARITY_ROOT / binding["fixture"]
            raw = path.read_bytes()
            cases = json.loads(raw)["cases"]
            untouched = copy.deepcopy(cases)
            contract = binding["contract"]
            prior = copy.deepcopy(cases)
            for index in PRESERVED_INDICES.get(contract, []):
                prior[index]["expectation"] = "oracle"
            result = compose_cases(cases, contract, self.correction)
            self.assertEqual(
                result,
                compose_cases(prior, contract, self.correction, stage04_composed=True),
            )
            expected_changes = sorted(
                PRESERVED_INDICES.get(contract, []) + INDICES[contract]
            )
            self.assertEqual(
                [
                    i
                    for i, (old, new) in enumerate(zip(cases, result, strict=True))
                    if old != new
                ],
                expected_changes,
            )
            for i, (old, new) in enumerate(zip(cases, result, strict=True)):
                self.assertEqual(
                    new,
                    dict(old, expectation="oracle") if i in expected_changes else old,
                )
                if prior[i] != new:
                    newly_promoted += 1
                if new["expectation"] == "baseline":
                    (
                        matching
                        if digest(new["reference"]) == digest(new["baseline"]["legacy"])
                        else residual
                    ).append(new)
            self.assertEqual(cases, untouched)
            self.assertEqual(path.read_bytes(), raw)
            result[0]["document"] = "mutated result"
            self.assertEqual(cases, untouched)
        self.assertEqual((newly_promoted, len(residual), len(matching)), (13, 40, 6))
        self.assertEqual(
            {c["name"] for c in matching},
            {
                "invalid-timeout-admission",
                "invalid-variables-admission",
                "unowned-variable-directive-empty",
                "unowned-variable-directive-colon",
                "earlier-empty-before-percent",
                "overbudget-earlier-bang-before-bracket",
            },
        )

    def test_contract_identity_provenance_and_inventory_mutations_fail_closed(self):
        mutations = [
            lambda c: c.update(contract="unversioned"),
            lambda c: c.update(extra=True),
            lambda c: c["provenance"].update(source_map_sha256="0" * 64),
            lambda c: c["fixture_bindings"][0].update(sha256="0" * 64),
            lambda c: c["fixture_bindings"][0].update(case_count=True),
        ]
        for group in ("replacements", "preserved_promotions"):
            for mutation in (
                lambda c, group=group: c[group].pop(),
                lambda c, group=group: c[group].reverse(),
                lambda c, group=group: c[group].append(c[group][0]),
                lambda c, group=group: c[group][0].update(pointer="/cases/0"),
                lambda c, group=group: c[group][0].update(fixture="other.json"),
                lambda c, group=group: c[group][0].update(name="other"),
            ):
                mutations.append(mutation)
        for mutate in mutations:
            changed = copy.deepcopy(self.correction)
            mutate(changed)
            with self.assertRaises(ValueError):
                validate_contract(changed)

    def test_each_full_preimage_field_and_process_envelope_fails_closed(self):
        for group in ("replacements", "preserved_promotions"):
            for index in range(len(self.correction[group])):
                for field, value in (
                    ("document", "{profile}"),
                    ("args", ["--timeout-ms", "0"]),
                    ("name", "other"),
                    ("expectation", "oracle"),
                    ("reference", {}),
                    ("baseline", {}),
                ):
                    changed = copy.deepcopy(self.correction)
                    changed[group][index]["preimage"][field] = value
                    with (
                        self.subTest(group=group, index=index, field=field),
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
                        case = changed[group][index]["preimage"]
                        envelope = (
                            case["reference"]
                            if lane == "reference"
                            else case["baseline"][lane]
                        )
                        envelope[field] = value
                        with (
                            self.subTest(
                                group=group, index=index, lane=lane, field=field
                            ),
                            self.assertRaises(ValueError),
                        ):
                            validate_contract(changed)

    def test_incoming_inventory_scope_and_duplicate_composition_fail_closed(self):
        for binding in self.correction["fixture_bindings"]:
            cases = json.loads((PARITY_ROOT / binding["fixture"]).read_text())["cases"]
            contract = binding["contract"]
            for changed in (cases[:-1], cases + [cases[0]], list(reversed(cases))):
                with self.assertRaises(ValueError):
                    compose_cases(changed, contract, self.correction)
            for index in INDICES[contract] + [0]:
                for field, value in (
                    ("name", "other"),
                    ("document", "{profile}"),
                    ("args", [] if cases[index]["args"] else ["--variables", "[]"]),
                    ("reference", {}),
                    ("baseline", {}),
                    ("expectation", "changed"),
                ):
                    changed = copy.deepcopy(cases)
                    changed[index][field] = value
                    with (
                        self.subTest(contract=contract, index=index, field=field),
                        self.assertRaises(ValueError),
                    ):
                        compose_cases(changed, contract, self.correction)
            composed = compose_cases(cases, contract, self.correction)
            for stage04_composed in (False, True):
                with self.assertRaises(ValueError):
                    compose_cases(
                        composed,
                        contract,
                        self.correction,
                        stage04_composed=stage04_composed,
                    )
        with self.assertRaises(ValueError):
            compose_cases([], "unknown", self.correction)

    def test_old_and_new_fixture_bytes_cannot_be_changed_even_semantically(self):
        files = [b["fixture"] for b in self.correction["fixture_bindings"]] + [FILE]
        with tempfile.TemporaryDirectory(
            prefix="lexical-fixture-metadata-"
        ) as temporary:
            root = Path(temporary)
            for name in files:
                shutil.copyfile(PARITY_ROOT / name, root / name)
            for name in files:
                original = (root / name).read_bytes()
                (root / name).write_bytes(original + b"\n")
                with self.subTest(file=name), self.assertRaises(ValueError):
                    load_contract(root)
                (root / name).write_bytes(original)
            validate_contract(self.correction, root)


if __name__ == "__main__":
    unittest.main()
