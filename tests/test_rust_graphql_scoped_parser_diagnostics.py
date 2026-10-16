# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent exact admission and ordinary-ownership promotions.

Historical fixtures remain byte-identical. The two contracts retain complete
frozen-oracle and archived-precommit02 baseline envelopes. They are expectations
for future source-bound qualification, not evidence of a current native pass.
The source-only fixture class never runs a product process. The runtime class
uses explicitly selected artifacts and fresh empty disposable databases.
"""

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
from tests.test_rust_graphql_source_locations import ORACLE

PARITY_ROOT = fixture.ROOT / "tests/parity"
SCOPED_PRECEDENCE_PATH = PARITY_ROOT / "graphql-parser-budget-precedence-v2.json"
OWNERSHIP_PATH = PARITY_ROOT / "graphql-parser-diagnostic-ownership-v1.json"
RESIDUAL_CONTRACT = "native-graphql-parser-captured-residual-v1"
RESIDUAL_FILE = "graphql-parser-captured-residual-v1.json"
RESIDUAL_FILE_SHA256 = (
    "ea2947c6a3fd2bffae955a5b403bc2eef21ef6c8a24e4e5095adad76ae58c70f"
)
RESIDUAL_PAYLOAD_SHA256 = (
    "e7a90b852018c51fab431a57b6fb7018c1a748d352d8312b98c853b9a4cd08c1"
)
SCOPED_PRECEDENCE_NAMES = [
    "empty-argument-list-outside-scope",
    "missing-colon-outside-scope",
    "invalid-number-before-missing-outside-scope",
    "invalid-exclamation-outside-scope",
    "invalid-unicode-outside-scope",
    "malformed-string-before-missing-outside-scope",
]
OWNERSHIP_NAMES = [
    "earlier-invalid-character-before-string",
    "earlier-missing-colon-before-string",
    "earlier-invalid-number-before-string",
]
# Pins are derived from independently captured complete envelopes, never native
# outputs from the proposed implementation. Case digests include all inputs,
# classifications, frozen errors, archived native baselines and process codes.
CONTRACTS = {
    "native-graphql-parser-budget-precedence-v2": {
        "path": SCOPED_PRECEDENCE_PATH,
        "names": SCOPED_PRECEDENCE_NAMES,
        "historical": "graphql-parser-budget-precedence-v1.json",
        "historical_sha256": "7a3a4f42a5c51b5a6e6f083d6ad6304ffc7c232fcf32449a738dcc46263df85b",
        "provenance_sha256": "42d1188f65eb22bd72880d02054848c75d5a8906164f5a6ad421fd2f3863e5cb",
        "cases_sha256": "d4a5859d834279f39bafec73ad510a7e81c85c3e9688c4ef3dc2ee4cfc105675",
        "case_count": 150,
    },
    "native-graphql-parser-diagnostic-ownership-v1": {
        "path": OWNERSHIP_PATH,
        "names": OWNERSHIP_NAMES,
        "historical": "graphql-string-diagnostics-v1.json",
        "historical_sha256": "44b93b479b396a001d8835546f686c4a2da80ab8ff39694e12a3008d70de30b3",
        "provenance_sha256": "64e8aef076246cd2953239ec9fb74f088056319753b4d2144969380a176f7d17",
        "cases_sha256": "16c8f63734af3d4e4a20e74e088086a846ce06dd26c24c1ebb4917f9c1d7dc29",
        "case_count": 38,
    },
}


def digest(value):
    return hashlib.sha256(
        json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode()
    ).hexdigest()


def validate_captured_residual_contract(contract):
    """Bind exact observed rows and independent policy lanes, never error text."""
    if (
        contract["contract"] != RESIDUAL_CONTRACT
        or digest(contract) != RESIDUAL_PAYLOAD_SHA256
    ):
        raise ValueError("Captured residual scope, input, evidence or policy changed")
    return contract


def load_captured_residual_contract():
    raw = (Path(__file__).resolve().parent / "parity" / RESIDUAL_FILE).read_bytes()
    if hashlib.sha256(raw).hexdigest() != RESIDUAL_FILE_SHA256:
        raise ValueError("Captured residual fixture bytes changed")
    return validate_captured_residual_contract(json.loads(raw))


class CapturedResidualFixtureTest(unittest.TestCase):
    def test_exact_156_captured_rows_and_13_new_admission_tuples(self):
        contract = load_captured_residual_contract()
        cases = contract["cases"]
        self.assertEqual(len(cases), 156)
        self.assertEqual(len({c["case_id"] for c in cases}), 156)
        self.assertEqual(sum(len(c["occurrences"]) for c in cases), 162)
        self.assertEqual(sum(c["new_admission_policy"] for c in cases), 64)
        self.assertEqual(
            len({digest(c["tuple"]) for c in cases if c["new_admission_policy"]}),
            13,
        )
        self.assertEqual(
            [
                c["case_index"]
                for c in cases
                if c["earlier_grammar_masked_by_later_token_budget"]
            ],
            list(range(332, 393, 4)),
        )
        for case in cases:
            with self.subTest(case=case["name"]):
                self.assertEqual(
                    hashlib.sha256(case["document"].encode()).hexdigest(),
                    case["document_sha256"],
                )
                self.assertEqual(case["expectation"], "oracle")
                self.assertTrue(case["ordinary_policy"])
                self.assertNotEqual(case["reference"], case["baseline"]["legacy"])
                self.assertEqual(
                    case["frozen_parse"]["stdout"]["formatted"],
                    case["reference"]["stdout"]["data"]["errors"][0],
                )
                self.assertEqual(
                    (
                        case["baseline"]["companion"]["code"],
                        case["baseline"]["companion"]["stderr"],
                    ),
                    (0, ""),
                )
                self.assertEqual(
                    case["before_limit_policy"],
                    case["new_admission_policy"]
                    or case["tuple"]["site"] == "ExecutableDocument"
                    and case["tuple"]["found"] == "Spread",
                )

    def test_rejects_changed_inputs_envelopes_evidence_and_lane_widening(self):
        original = load_captured_residual_contract()
        changes = [
            lambda c: c["cases"].pop(),
            lambda c: c["cases"].reverse(),
            lambda c: c["cases"][0].update(document="{profile}"),
            lambda c: c["cases"][0].update(args=["--variables", "[]"]),
            lambda c: c["cases"][0].update(reference={}),
            lambda c: c["cases"][0].update(baseline={}),
            lambda c: c["cases"][0].update(evidence={}),
            lambda c: c["cases"][0].update(tuple={}),
            lambda c: c["cases"][0].update(before_limit_policy=True),
            lambda c: c["provenance"].update(source_map="changed"),
        ]
        for index, change in enumerate(changes):
            changed = copy.deepcopy(original)
            change(changed)
            with self.subTest(index=index), self.assertRaises(ValueError):
                validate_captured_residual_contract(changed)

    def test_existing_runtime_consumer_keeps_fresh_exact_rows(self):
        first, records = compose_runtime_contract(RESIDUAL_CONTRACT)
        expected = copy.deepcopy(first)
        self.assertEqual(len(records), 156)
        first["cases"][0]["document"] = "changed returned value"
        fresh, fresh_records = compose_runtime_contract(RESIDUAL_CONTRACT)
        self.assertEqual(fresh, expected)
        self.assertEqual(fresh_records, records)


def scoped_control_replacements(correction, contract, parity_root=PARITY_ROOT):
    """Reject any inventory, input, oracle, old-baseline or provenance drift."""
    spec = CONTRACTS[contract]
    if set(correction) != {
        "contract",
        "provenance",
        "historical_replacements",
        "cases",
    }:
        raise ValueError("Unexpected scoped parser contract fields")
    if correction["contract"] != contract:
        raise ValueError("Unexpected scoped parser correction contract")
    if digest(correction["provenance"]) != spec["provenance_sha256"]:
        raise ValueError("Scoped parser source/oracle provenance changed")
    historical_path = parity_root / spec["historical"]
    if (
        hashlib.sha256(historical_path.read_bytes()).hexdigest()
        != spec["historical_sha256"]
    ):
        raise ValueError("Preserved historical parser fixture changed")
    historical = {
        c["name"]: c for c in json.loads(historical_path.read_text())["cases"]
    }
    replacements = correction["historical_replacements"]
    if [c["historical_control"] for c in replacements] != spec["names"]:
        raise ValueError("Only the independently named scoped replacements are allowed")
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
            raise ValueError(
                "Historical input or complete oracle/baseline envelope changed"
            )
        if fixture.strict(original["reference"]) == fixture.strict(
            original["baseline"]["legacy"]
        ):
            raise ValueError("An already matching historical case was relabeled")
    if digest(correction["cases"]) != spec["cases_sha256"]:
        raise ValueError("Scoped case inventory or complete captured envelopes changed")
    return {c["historical_control"]: c for c in replacements}


def scoped_precedence_replacements(correction, parity_root=PARITY_ROOT):
    return scoped_control_replacements(
        correction, "native-graphql-parser-budget-precedence-v2", parity_root
    )


def ownership_control_replacements(correction, parity_root=PARITY_ROOT):
    return scoped_control_replacements(
        correction, "native-graphql-parser-diagnostic-ownership-v1", parity_root
    )


def promote_scoped_cases(cases, replacements):
    """Compose validated exact corrections without mutating historical captures."""
    promoted = []
    for case in cases:
        if replacement := replacements.get(case["name"]):
            for source, target in (
                ("document", "document"),
                ("args", "args"),
                ("reference", "expected"),
                ("baseline", "baseline"),
            ):
                if fixture.strict(case[source]) != fixture.strict(replacement[target]):
                    raise ValueError("Historical scoped control differs across suites")
            case = dict(case, expectation="oracle")
        promoted.append(case)
    return promoted


class ScopedParserDiagnosticFixtureTest(unittest.TestCase):
    def test_independent_exact_historical_membership(self):
        self.assertFalse(set(SCOPED_PRECEDENCE_NAMES) & set(OWNERSHIP_NAMES))
        for name, spec in CONTRACTS.items():
            with self.subTest(contract=name):
                correction = json.loads(spec["path"].read_text())
                replacements = scoped_control_replacements(correction, name)
                self.assertEqual(list(replacements), spec["names"])
                self.assertEqual(len(correction["cases"]), spec["case_count"])
                self.assertEqual(
                    len({c["name"] for c in correction["cases"]}), spec["case_count"]
                )
                for case in correction["cases"]:
                    self.assertIn(case["reference"]["code"], (0, 2))
                    self.assertLessEqual(len(case["document"].encode()), 131072)
                # The historical witnesses have exact recaptured archived baselines.
                for case in correction["cases"][: len(spec["names"])]:
                    replacement = replacements[case["name"]]
                    self.assertEqual(case["expectation"], "oracle")
                    self.assertEqual(
                        fixture.strict(case["reference"]),
                        fixture.strict(replacement["expected"]),
                    )
                    self.assertEqual(
                        fixture.strict(case["baseline"]),
                        fixture.strict(replacement["baseline"]),
                    )

    def test_composition_preserves_history_and_excluded_residuals(self):
        for name, spec in CONTRACTS.items():
            correction = json.loads(spec["path"].read_text())
            replacements = scoped_control_replacements(correction, name)
            cases = json.loads((PARITY_ROOT / spec["historical"]).read_text())["cases"]
            untouched = copy.deepcopy(cases)
            promoted = promote_scoped_cases(cases, replacements)
            self.assertEqual(cases, untouched)
            changed = []
            for old, new in zip(cases, promoted, strict=True):
                if old["name"] in replacements:
                    self.assertEqual(new, dict(old, expectation="oracle"))
                    changed.append(old["name"])
                else:
                    self.assertEqual(old, new)
            self.assertEqual(changed, spec["names"])
        # Existing execution coverage and crash/recursion envelopes are preserved;
        # no exclusion here is executed or promoted by this source-only test.
        old = json.loads(
            (PARITY_ROOT / CONTRACTS[next(iter(CONTRACTS))]["historical"]).read_text()
        )
        excluded = {
            c["name"]: c
            for c in old["cases"]
            if c["name"]
            in {
                "dangling-backslash-residual",
                "deep-input-budget-before-invalid",
                "deep-selection-budget-before-invalid",
                "deep-type-budget-before-missing",
            }
        }
        self.assertEqual(len(excluded), 4)
        self.assertTrue(all(c["expectation"] == "baseline" for c in excluded.values()))
        self.assertEqual(
            excluded["dangling-backslash-residual"]["reference"]["code"], 1
        )

    def test_scope_provenance_inputs_and_envelopes_fail_closed(self):
        for name, spec in CONTRACTS.items():
            original = json.loads(spec["path"].read_text())
            mutations = [
                lambda c: c.update(contract="unversioned"),
                lambda c: c.update(extra=True),
                lambda c: c.update(historical_replacements=[]),
                lambda c: c["historical_replacements"].reverse(),
                lambda c: c["historical_replacements"].append(
                    c["historical_replacements"][0]
                ),
                lambda c: c["historical_replacements"][0].update(
                    historical_control="out-of-scope"
                ),
                lambda c: c["historical_replacements"][0].update(document="{profile}"),
                lambda c: c["historical_replacements"][0].update(
                    args=["--timeout-ms", "0"]
                ),
                lambda c: c["historical_replacements"][0]["expected"].update(code=True),
                lambda c: c["historical_replacements"][0]["expected"].update(
                    stderr="changed"
                ),
                lambda c: c["historical_replacements"][0]["expected"]["stdout"]["data"][
                    "errors"
                ][0].update(locations=[]),
                lambda c: c["historical_replacements"][0]["baseline"]["legacy"].update(
                    stderr="changed"
                ),
                lambda c: c["historical_replacements"][0]["baseline"][
                    "companion"
                ].update(code=2),
                lambda c: c["cases"].pop(),
                lambda c: c["cases"].reverse(),
                lambda c: c["cases"][0].update(document="{profile}"),
                lambda c: c["cases"][0].update(args=["--variables", "[]"]),
                lambda c: c["cases"][0].update(expectation="baseline"),
                lambda c: c["cases"][0]["reference"].update(stderr="changed"),
                lambda c: c["cases"][0]["baseline"]["legacy"].update(code=0),
                lambda c: c["cases"][0]["baseline"]["companion"].update(stdout={}),
            ]
            mutations.extend(
                lambda c, key=key: c["provenance"].update({key: "changed"})
                for key in original["provenance"]
            )
            for index, mutate in enumerate(mutations):
                changed = copy.deepcopy(original)
                mutate(changed)
                with (
                    self.subTest(contract=name, mutation=index),
                    self.assertRaises(ValueError),
                ):
                    scoped_control_replacements(changed, name)
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / spec["historical"]).write_bytes(
                    (PARITY_ROOT / spec["historical"]).read_bytes() + b"\n"
                )
                with self.assertRaises(ValueError):
                    scoped_control_replacements(original, name, root)

    def test_cross_suite_composition_rejects_input_and_envelope_drift(self):
        for name, spec in CONTRACTS.items():
            original = json.loads(spec["path"].read_text())
            replacements = scoped_control_replacements(original, name)
            cases = json.loads((PARITY_ROOT / spec["historical"]).read_text())["cases"]
            case = next(c for c in cases if c["name"] == spec["names"][0])
            for field, value in [
                ("document", "{profile}"),
                ("args", ["--variables", "[]"]),
                ("reference", {}),
                ("baseline", {}),
            ]:
                changed = dict(case, **{field: value})
                with (
                    self.subTest(contract=name, field=field),
                    self.assertRaises(ValueError),
                ):
                    promote_scoped_cases([changed], replacements)


def compose_runtime_contract(contract_name):
    """Build exact pending expectations without opening databases or processes."""
    if contract_name == RESIDUAL_CONTRACT:
        contract = load_captured_residual_contract()
        return contract, [
            dict(
                contract=RESIDUAL_CONTRACT,
                historical_control=case["name"],
                historical_pointer=f"/cases/{index}",
                captured_preimage_sha256=digest(case),
            )
            for index, case in enumerate(contract["cases"])
        ]
    captured = contract_name == "native-graphql-parser-lexical-capture-v1"
    if captured:
        from tests import test_rust_graphql_lexical_capture as captured_lexical

        contract = captured_lexical.load_contract()
        contract["cases"] = captured_lexical.compose_cases(contract["cases"], contract)
    elif contract_name in CONTRACTS:
        spec = CONTRACTS[contract_name]
        contract = json.loads(spec["path"].read_text())
        scoped_control_replacements(contract, contract_name)
    else:
        from tests import test_rust_graphql_boundary_ownership as boundary

        contract = boundary.load_contract(contract_name)
    composed_replacements = (
        [
            dict(
                contract=contract_name,
                historical_control=target["name"],
                historical_pointer=f"/cases/{target['index']}",
                captured_preimage_sha256=target["case_sha256"],
            )
            for target in contract["promotions"]
        ]
        if captured
        else []
    )
    if contract_name == "native-graphql-parser-diagnostic-ownership-v1":
        from tests import test_rust_graphql_boundary_ownership as boundary

        contract["cases"] = boundary.promote_ownership_cases(contract["cases"])
        composed_replacements = [
            dict(
                contract=boundary.OWNERSHIP_CONTRACT,
                historical_control=name,
                historical_pointer=f"/cases/{index}",
            )
            for index, name in zip(
                boundary.REPLACEMENT_INDICES,
                boundary.REPLACEMENT_NAMES,
                strict=True,
            )
        ]
    if contract_name == "native-graphql-parser-budget-precedence-v2":
        # Runtime-only import avoids a cycle in the independent source-only
        # fixture validators and preserves both sealed stage01 contracts.
        from tests import test_rust_graphql_unsupported_scalar_precedence as scalar

        contract["cases"] = scalar.promote_stage01_cases(contract["cases"])
        composed_replacements = [
            dict(
                contract=scalar.CONTRACT,
                historical_control=scalar.SLASH_NAME,
                historical_pointer="/cases/120",
            )
        ]
    if not captured:
        from tests import test_rust_graphql_lexical_ownership as lexical

        contract["cases"] = lexical.compose_cases(
            contract["cases"], contract_name, stage04_composed=True
        )
        composed_replacements.extend(lexical.composition_records(contract_name))
        from tests import test_rust_graphql_field_value_tokens as field_tokens

        if contract_name in field_tokens.INDICES:
            contract["cases"] = field_tokens.compose_cases(
                contract["cases"], contract_name
            )
            composed_replacements.extend(
                field_tokens.composition_records(contract_name)
            )
        from tests import test_rust_graphql_directive_value_closers as directive_closers

        if contract_name in directive_closers.INDICES:
            contract["cases"] = directive_closers.compose_cases(
                contract["cases"], contract_name
            )
            composed_replacements.extend(
                directive_closers.composition_records(contract_name)
            )
    from tests import test_rust_graphql_final_composition as combined

    contract["cases"] = combined.compose_cases(contract["cases"], contract_name)
    composed_replacements.extend(combined.composition_records(contract_name))
    return contract, composed_replacements


class ScopedParserDiagnosticTest(unittest.TestCase):
    def replay(self, contract_name, report_key):
        contract, composed_replacements = compose_runtime_contract(contract_name)
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
        with tempfile.TemporaryDirectory(
            prefix="scoped-parser-diagnostics-"
        ) as temporary:
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
            for case in contract["cases"]:
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
            if report := os.environ.get(report_key):
                Path(report).write_text(
                    json.dumps(
                        dict(
                            contract=contract_name,
                            composed_replacements=composed_replacements,
                            cases=results,
                            database_sha256_before=before,
                            database_sha256_after=after,
                        ),
                        indent=2,
                    )
                    + "\n"
                )
            print(f"{contract_name} complete-process-envelope cases: {len(results)}")

    def test_six_admission_promotions_and_bounded_controls(self):
        self.replay(
            "native-graphql-parser-budget-precedence-v2",
            "CATABOLIC_SCOPED_PARSER_PRECEDENCE_REPORT",
        )

    def test_three_ownership_promotions_and_bounded_controls(self):
        self.replay(
            "native-graphql-parser-diagnostic-ownership-v1",
            "CATABOLIC_SCOPED_PARSER_OWNERSHIP_REPORT",
        )

    def test_156_exact_captured_residuals(self):
        self.replay(RESIDUAL_CONTRACT, "CATABOLIC_CAPTURED_RESIDUAL_REPORT")


if __name__ == "__main__":
    unittest.main()
