# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Metadata-only regression controls for the deliberately composed consumers."""

import copy
import json
import unittest
from unittest.mock import patch

from tests import test_rust_graphql_directive_value_closers as closers
from tests import test_rust_graphql_field_value_tokens as field_tokens
from tests import test_rust_graphql_final_composition as combined
from tests import test_rust_graphql_grammar_provenance as grammar
from tests import test_rust_graphql_lexical_capture as capture
from tests import test_rust_graphql_lexical_ownership as lexical
from tests import test_rust_graphql_scoped_parser_diagnostics as scoped
from tests import test_rust_graphql_unsupported_scalar_precedence as scalar


class ComposedStage07FixtureTest(unittest.TestCase):
    def test_captured_lane_preserves_104_lexical_and_adds_40_exact_grammar_rows(self):
        original = capture.load_contract()
        expected = capture.compose_cases(original["cases"], original)
        expected = grammar.compose_cases(expected, capture.CONTRACT)
        result, records = scoped.compose_runtime_contract(capture.CONTRACT)
        self.assertEqual(result["cases"], expected)
        self.assertEqual(len(records), 144)
        self.assertEqual(
            [r["historical_control"] for r in records],
            [t["name"] for t in original["promotions"]]
            + [
                r["historical_control"]
                for r in grammar.composition_records(capture.CONTRACT)
            ],
        )
        for key in original.keys() - {"cases"}:
            self.assertEqual(result[key], original[key])
        negatives = [
            c
            for c in result["cases"]
            if c["expectation"] == "baseline"
            and c["reference"] != c["baseline"]["legacy"]
        ]
        self.assertEqual(len(negatives), 0)
        self.assertTrue(
            all(c["group"] == "unowned first failure must stop" for c in negatives)
        )
        self.assertEqual(original, capture.load_contract())

    def test_historical_lanes_follow_exact_order_and_retain_prior_promotions(self):
        prior = lexical.load_contract()
        total_field = total_closers = residual = matching = 0
        for binding in prior["fixture_bindings"]:
            original = json.loads(
                (lexical.PARITY_ROOT / binding["fixture"]).read_bytes()
            )
            name = binding["contract"]
            expected = lexical.compose_cases(original["cases"], name)
            stage05 = copy.deepcopy(expected)
            if name in field_tokens.INDICES:
                expected = field_tokens.compose_cases(expected, name)
                total_field += len(field_tokens.INDICES[name])
            if name in closers.INDICES:
                expected = closers.compose_cases(expected, name)
                total_closers += len(closers.INDICES[name])
            expected = combined.compose_cases(expected, name)
            if name == "native-graphql-unsupported-scalar-precedence-v1":
                # This sealed lane retains its separate existing runtime consumer.
                result, records = scalar.compose_runtime_contract()
                self.assertEqual(result["cases"], expected)
            else:
                result, records = scoped.compose_runtime_contract(name)
                self.assertEqual(result["cases"], expected)
                if name in field_tokens.INDICES:
                    self.assertEqual(
                        sum(r["contract"] == field_tokens.CONTRACT for r in records),
                        len(field_tokens.INDICES[name]),
                    )
                if name in closers.INDICES:
                    self.assertEqual(
                        sum(r["contract"] == closers.CONTRACT for r in records),
                        len(closers.INDICES[name]),
                    )
            for before, after in zip(stage05, result["cases"], strict=True):
                if before["expectation"] == "oracle":
                    self.assertEqual(after, before)
                else:
                    self.assertEqual(dict(after, expectation="baseline"), before)
                if after["expectation"] == "baseline":
                    equal = after["reference"] == after["baseline"]["legacy"]
                    matching += equal
                    residual += not equal
            self.assertEqual(
                json.loads((lexical.PARITY_ROOT / binding["fixture"]).read_bytes()),
                original,
            )
        self.assertEqual((total_field, total_closers, residual, matching), (6, 7, 0, 6))

    def test_captured_lane_never_enters_historical_composers(self):
        with (
            patch.object(
                field_tokens, "compose_cases", side_effect=AssertionError("wrong lane")
            ),
            patch.object(
                closers, "compose_cases", side_effect=AssertionError("wrong lane")
            ),
            patch.object(
                lexical, "compose_cases", side_effect=AssertionError("wrong lane")
            ),
        ):
            result, records = scoped.compose_runtime_contract(capture.CONTRACT)
        self.assertEqual((len(result["cases"]), len(records)), (197, 144))

    def test_returned_mutations_do_not_change_fresh_composition(self):
        for name in [capture.CONTRACT, *field_tokens.INDICES]:
            first = scoped.compose_runtime_contract(name)
            expected = copy.deepcopy(first)
            first[0]["cases"][0]["document"] = "mutated returned value"
            first[1].clear()
            self.assertEqual(scoped.compose_runtime_contract(name), expected)

    def test_wrong_order_and_repeat_composition_fail_closed(self):
        prior = lexical.load_contract()
        for binding in prior["fixture_bindings"]:
            name = binding["contract"]
            if name not in field_tokens.INDICES:
                continue
            raw = json.loads((lexical.PARITY_ROOT / binding["fixture"]).read_bytes())[
                "cases"
            ]
            stage05 = lexical.compose_cases(raw, name)
            stage06 = field_tokens.compose_cases(stage05, name)
            stage07 = closers.compose_cases(stage06, name)
            for wrong in (raw, stage05, stage07):
                with self.assertRaises(ValueError):
                    closers.compose_cases(wrong, name)
            with self.assertRaises(ValueError):
                field_tokens.compose_cases(stage07, name)


if __name__ == "__main__":
    unittest.main()
