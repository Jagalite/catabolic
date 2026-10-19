# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Versioned, frozen comment-token accounting and complete-envelope evidence."""

import copy
import hashlib
import json
import unittest

from tests import test_rust_legacy_parser as fixture
from tests.test_rust_graphql_source_locations import TOKEN_CORRECTION_PATH

COMMENT_CORRECTION_PATH = (
    fixture.ROOT / "tests/parity/graphql-comment-token-budget-v1.json"
)
COMMENT_CORRECTED_NAME = "comments-not-counted-native-boundary"
HISTORICAL_SHA256 = "1c81574203eff294b01a36cf5b5274ade0e7f854f2deae62528a1c255582eac8"
COMMENT_CASES_SHA256 = (
    "dd4231cef973197ef9043b151b2a0c6c9e9ef2558aaebe292b8f0693e6459bb2"
)


def comment_case_specs():
    """Exact 3999/4000/4001 combined-token inputs, without a token-list oracle."""
    cases = []

    def add(name, document, tokens):
        cases.append(dict(name=name, document=document, args=[], token_count=tokens))

    for label, nl in (("lf", "\n"), ("cr", "\r"), ("crlf", "\r\n")):
        for total in (3999, 4000, 4001):
            comments = total - 3
            run = ("# comment" + nl) * comments
            add(
                f"{label}-trailing-{total}-eof",
                "{profile}" + nl + run[: -len(nl)],
                total,
            )
            add(f"{label}-trailing-{total}-newline", "{profile}" + nl + run, total)
            add(f"{label}-leading-{total}", run + "{profile}", total)
            add(f"{label}-middle-{total}", "{profile" + nl + run + "  }", total)
            mixed = "{" + nl + ("profile # mixed" + nl) * 1998
            mixed += ("# last" + nl) * (total - 3998) + "}"
            add(f"{label}-mixed-{total}", mixed, total)
            unicode_run = ('#雪😀\u2028 # {{ " text' + nl) * comments
            add(f"{label}-unicode-{total}", "{profile}" + nl + unicode_run, total)
        add(
            f"{label}-delayed-next-token-after-overrun",
            "{profile" + nl + ("#雪😀" + nl) * 4001 + "   }",
            4004,
        )
        add(
            f"{label}-delayed-eof-after-overrun",
            "{profile}" + nl + ("#雪😀" + nl) * 4001 + " ,\ufeff  ",
            4004,
        )
        for ending, suffix in (("eof", ""), ("newline", nl)):
            add(
                f"{label}-comments-only-over-limit-{ending}",
                ("#" + nl) * 4000 + "#雪😀" + suffix,
                4001,
            )
    for label, value in (
        ("quoted-hash", '"#雪😀 { } # text"'),
        ("escaped-quoted-hash", '"\\"#\\" \\\\ # text"'),
        ("block-hash", '"""# first\n#雪😀 second\r\n# third"""'),
        ("escaped-block-hash", '"""# first \\""" # second"""'),
    ):
        for total in (3999, 4000, 4001):
            add(
                f"{label}-{total}",
                "{item(id:" + value + "){id}}\n" + "# comment\n" * (total - 11),
                total,
            )
    add("quoted-4001-hashes-one-token", '{item(id:"' + "# " * 4001 + '"){id}}', 11)
    add(
        "block-4001-hash-lines-one-token",
        '{item(id:"""' + "# text\n" * 4001 + '"""){id}}',
        11,
    )
    return cases


def comment_control_replacements(correction, historical_path=TOKEN_CORRECTION_PATH):
    """Only promote the one recorded comment gap; retain every other baseline."""
    if correction["contract"] != "native-graphql-comment-token-budget-v1":
        raise ValueError("Unexpected comment-token contract")
    if hashlib.sha256(historical_path.read_bytes()).hexdigest() != HISTORICAL_SHA256:
        raise ValueError("Historical token-budget fixture changed")
    historical = json.loads(historical_path.read_text())
    expected_provenance = {
        "reference_commit": "453fca983222c6665a48775eef67c489c96b527e",
        "python": "3.14.6",
        "graphql_core": "3.2.12",
        "historical_token_budget_fixture_sha256": HISTORICAL_SHA256,
        "composed_source_files": 1564,
        "composed_source_sha256": "19d16731702867d62c295eed8e5d9a4144b6206ab3cf24c946263965c371f6d5",
        "composed_helpers_sha256": {
            "catabolic-native": "e8b3caafca409fad99cf45fb96f13d107d92389227b58adbacfbbb5c60f398b1",
            "catabolic-legacy-native": "566ceb9b9c9a3050d5ca83192a173423ffc706fe868b2d6140769c3a2bf130ef",
            "legacy_parse": "797d90d9141d3383290c3226cc67c6d669fcf3f3221d12a31165d52bbbdca0a5",
        },
    }
    if fixture.strict(correction["provenance"]) != fixture.strict(expected_provenance):
        raise ValueError("Comment-token provenance changed")
    old = next(c for c in historical["cases"] if c["name"] == COMMENT_CORRECTED_NAME)
    expected = dict(
        historical_control=old["name"],
        document=old["document"],
        args=old["args"],
        expected=old["reference"],
        baseline=old["baseline"],
    )
    if fixture.strict(correction["historical_replacements"]) != fixture.strict(
        [expected]
    ):
        raise ValueError("Only the exact historical comment gap may be replaced")
    cases = correction["cases"]
    specs = [
        {key: c[key] for key in ("name", "document", "args", "token_count")}
        for c in cases
    ]
    if fixture.strict(specs) != fixture.strict(comment_case_specs()):
        raise ValueError("Comment-token cases changed")
    digest = hashlib.sha256(
        json.dumps(
            cases, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode()
    ).hexdigest()
    if digest != COMMENT_CASES_SHA256 or any(
        c["expectation"] != "oracle" for c in cases
    ):
        raise ValueError("Frozen comment-token complete envelopes changed")
    return {COMMENT_CORRECTED_NAME: expected}


class CommentTokenBudgetFixtureTest(unittest.TestCase):
    def test_exact_historical_replacement_and_regression_cases(self):
        contract = json.loads(COMMENT_CORRECTION_PATH.read_text())
        self.assertEqual(
            list(comment_control_replacements(contract)), [COMMENT_CORRECTED_NAME]
        )
        self.assertEqual(len(contract["cases"]), 80)
        self.assertEqual(
            {c["token_count"] for c in contract["cases"]}, {11, 3999, 4000, 4001, 4004}
        )

    def test_provenance_scope_inputs_and_complete_envelopes_fail_closed(self):
        original = json.loads(COMMENT_CORRECTION_PATH.read_text())
        mutations = [
            lambda c: c.update(contract="unversioned"),
            lambda c: c["provenance"].update(python="changed"),
            lambda c: c["historical_replacements"].clear(),
            lambda c: c["historical_replacements"].append(
                c["historical_replacements"][0]
            ),
            lambda c: c["historical_replacements"][0].update(
                historical_control="earlier-missing-value-over-limit"
            ),
            lambda c: c["historical_replacements"][0].update(document="{profile}"),
            lambda c: c["historical_replacements"][0]["expected"].update(code=0),
            lambda c: c["historical_replacements"][0]["baseline"]["legacy"].update(
                stderr="changed"
            ),
            lambda c: c["cases"].pop(),
            lambda c: c["cases"].reverse(),
            lambda c: c["cases"][0].update(document="{profile}"),
            lambda c: c["cases"][0].update(token_count=4000),
            lambda c: c["cases"][0].update(expectation="baseline"),
            lambda c: c["cases"][0]["reference"].update(stderr="changed"),
            lambda c: c["cases"][0]["reference"]["stdout"].update(exit_code=False),
            lambda c: c["cases"][0]["baseline"]["companion"].update(code=1),
        ]
        for index, mutate in enumerate(mutations):
            correction = copy.deepcopy(original)
            mutate(correction)
            with self.subTest(mutation=index), self.assertRaises(ValueError):
                comment_control_replacements(correction)


if __name__ == "__main__":
    unittest.main()
