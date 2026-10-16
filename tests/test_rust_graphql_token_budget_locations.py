# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Exact token/EOF coordinates, preserving budget and precedence non-parity."""

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
from tests.test_rust_graphql_comment_token_budget import (
    COMMENT_CORRECTION_PATH,
    comment_control_replacements,
)
from tests.test_rust_graphql_parser_budget_precedence import (
    PRECEDENCE_CORRECTION_PATH,
    precedence_control_replacements,
    promote_precedence_cases,
)
from tests.test_rust_graphql_scoped_parser_diagnostics import (
    OWNERSHIP_PATH,
    SCOPED_PRECEDENCE_PATH,
    ownership_control_replacements,
    promote_scoped_cases,
    scoped_precedence_replacements,
)
from tests.test_rust_graphql_source_locations import (
    ORACLE,
    TOKEN_CORRECTION_PATH,
    token_control_replacements,
)
from tests.test_rust_graphql_string_diagnostics import (
    STRING_CORRECTION_PATH,
    string_control_replacements,
)

EOF_CORRECTION_PATH = fixture.ROOT / "tests/parity/graphql-eof-locations-v1.json"
EOF_CORRECTED_NAMES = [
    f"{newline}-4000-tokens-missing-close" for newline in ("lf", "cr", "crlf")
]


def eof_control_replacements(correction, historical_path=TOKEN_CORRECTION_PATH):
    """Replace only the three named EOF coordinates; keep the v1 record intact."""
    if correction["contract"] != "native-graphql-eof-locations-v1":
        raise ValueError("Unexpected EOF control contract")
    expected_provenance = {
        "reference_commit": "453fca983222c6665a48775eef67c489c96b527e",
        "python": "3.14.6",
        "graphql_core": "3.2.12",
        "historical_token_fixture_sha256": "1c81574203eff294b01a36cf5b5274ade0e7f854f2deae62528a1c255582eac8",
        "baseline_helpers_sha256": {
            "catabolic-native": "1d10087f6f965eb2389853bd3691ae48f4b86f643ff0577983f8b345666821b1",
            "catabolic-legacy-native": "04915a4678eeeacd58070c74fa11ad9f6273415d910e3ce6f6bc6b065ad17c89",
        },
    }
    if fixture.strict(correction["provenance"]) != fixture.strict(expected_provenance):
        raise ValueError("EOF provenance changed")
    if (
        hashlib.sha256(historical_path.read_bytes()).hexdigest()
        != expected_provenance["historical_token_fixture_sha256"]
    ):
        raise ValueError("Historical token-budget fixture changed")
    historical = json.loads(historical_path.read_text())
    token_control_replacements(historical)
    cases = {case["name"]: case for case in historical["cases"]}
    replacements = correction["historical_replacements"]
    if [case["historical_control"] for case in replacements] != EOF_CORRECTED_NAMES:
        raise ValueError("Only the three named EOF controls may be replaced")
    for replacement in replacements:
        name = replacement["historical_control"]
        original = cases[name]
        expected = dict(
            historical_control=name,
            document=original["document"],
            args=original["args"],
            expected=original["reference"],
        )
        if fixture.strict(replacement) != fixture.strict(expected):
            raise ValueError(f"Historical input or frozen EOF response changed: {name}")
        old, new = (
            copy.deepcopy(original["baseline"]["legacy"]),
            copy.deepcopy(original["reference"]),
        )
        old["stdout"]["data"]["errors"][0].pop("locations")
        new["stdout"]["data"]["errors"][0].pop("locations")
        if fixture.strict(old) != fixture.strict(new) or fixture.strict(
            original["baseline"]["legacy"]
        ) == fixture.strict(original["reference"]):
            raise ValueError(f"Replacement must change only EOF coordinates: {name}")
    return {case["historical_control"]: case for case in replacements}


class TokenBudgetLocationTest(unittest.TestCase):
    def test_exact_eof_replacements_fail_closed(self):
        original = json.loads(EOF_CORRECTION_PATH.read_text())
        self.assertEqual(list(eof_control_replacements(original)), EOF_CORRECTED_NAMES)
        mutations = [
            lambda c: c.update(contract="unversioned"),
            lambda c: c.update(
                historical_replacements=c["historical_replacements"][:-1]
            ),
            lambda c: c["historical_replacements"].append(
                c["historical_replacements"][0]
            ),
            lambda c: c["historical_replacements"].reverse(),
            lambda c: c["historical_replacements"][0].update(
                historical_control="earlier-missing-value-over-limit"
            ),
            lambda c: c["historical_replacements"][0].update(document="{profile}"),
            lambda c: c["historical_replacements"][0].update(
                args=["--timeout-ms", "0"]
            ),
            lambda c: c["historical_replacements"][0]["expected"].update(code=0),
            lambda c: c["historical_replacements"][0]["expected"].update(
                stderr="changed"
            ),
            lambda c: c["historical_replacements"][0]["expected"]["stdout"].update(
                exit_code=True
            ),
            lambda c: c["historical_replacements"][0]["expected"]["stdout"]["data"][
                "errors"
            ][0].update(locations=[]),
        ]
        mutations.extend(
            lambda c, key=key: c["provenance"].update({key: "changed"})
            for key in original["provenance"]
        )
        for index, mutate in enumerate(mutations):
            correction = copy.deepcopy(original)
            mutate(correction)
            with self.subTest(mutation=index), self.assertRaises(ValueError):
                eof_control_replacements(correction)
        with tempfile.TemporaryDirectory() as temporary:
            changed = Path(temporary) / "historical.json"
            changed.write_bytes(TOKEN_CORRECTION_PATH.read_bytes() + b"\n")
            with self.assertRaises(ValueError):
                eof_control_replacements(original, changed)

    def test_exact_versioned_historical_replacements(self):
        replacements = token_control_replacements(
            json.loads(TOKEN_CORRECTION_PATH.read_text())
        )
        self.assertEqual(
            list(replacements),
            [f"{newline}-token-budget-unchanged" for newline in ("lf", "cr", "crlf")],
        )

    def test_replacement_scope_inputs_provenance_and_envelopes_fail_closed(self):
        original = json.loads(TOKEN_CORRECTION_PATH.read_text())
        mutations = [
            lambda c: c.update(contract="unversioned"),
            lambda c: c.update(
                historical_replacements=c["historical_replacements"][:-1]
            ),
            lambda c: c["historical_replacements"].append(
                c["historical_replacements"][0]
            ),
            lambda c: c["historical_replacements"].reverse(),
            lambda c: c["historical_replacements"][0].update(
                historical_control="lf-syntax-admission-unchanged"
            ),
            lambda c: c["historical_replacements"][0].update(document="{profile}"),
            lambda c: c["historical_replacements"][0]["expected"].update(code=0),
            lambda c: c["historical_replacements"][0]["expected"].update(
                stderr="changed"
            ),
            lambda c: c["historical_replacements"][0]["expected"]["stdout"].update(
                exit_code=True
            ),
            lambda c: c["historical_replacements"][0]["expected"]["stdout"]["data"][
                "errors"
            ][0].update(locations=[]),
        ]
        mutations.extend(
            lambda c, key=key: c["provenance"].update({key: "changed"})
            for key in [
                "reference_commit",
                "python",
                "graphql_core",
                "historical_source_location_fixture_sha256",
                "syntax_controls_fixture_sha256",
                "baseline_commit",
                "baseline_helpers_sha256",
            ]
        )
        for index, mutate in enumerate(mutations):
            correction = copy.deepcopy(original)
            mutate(correction)
            with self.subTest(mutation=index), self.assertRaises(ValueError):
                token_control_replacements(correction)

    def test_frozen_boundaries_and_preserved_precedence_envelopes(self):
        contract = json.loads(TOKEN_CORRECTION_PATH.read_text())
        token_control_replacements(contract)
        names = [
            f"{newline}-{suffix}"
            for newline in ("lf", "cr", "crlf")
            for suffix in (
                "3999-tokens",
                "4000-tokens",
                "4001-tokens",
                "over-limit-indented",
                "4000-tokens-missing-close",
            )
        ]
        names += [
            "earlier-missing-value-under-limit",
            "earlier-missing-value-over-limit",
            "earlier-invalid-character-over-limit",
            "comments-not-counted-native-boundary",
            "unterminated-string-not-expanded",
            "invalid-variables-before-token-budget",
            "invalid-timeout-before-token-budget",
        ]
        self.assertEqual([case["name"] for case in contract["cases"]], names)
        eof = json.loads(EOF_CORRECTION_PATH.read_text())
        eof_replacements = eof_control_replacements(eof)
        comment_contract = json.loads(COMMENT_CORRECTION_PATH.read_text())
        comment_replacements = comment_control_replacements(comment_contract)
        string_replacements = string_control_replacements(
            json.loads(STRING_CORRECTION_PATH.read_text())
        )
        self.assertFalse(eof_replacements.keys() & comment_replacements.keys())
        self.assertFalse(eof_replacements.keys() & string_replacements.keys())
        self.assertFalse(comment_replacements.keys() & string_replacements.keys())
        replacements = {
            **eof_replacements,
            **comment_replacements,
            **string_replacements,
        }
        precedence_replacements = precedence_control_replacements(
            json.loads(PRECEDENCE_CORRECTION_PATH.read_text())
        )
        self.assertFalse(replacements.keys() & precedence_replacements.keys())
        # Each versioned fixture admits only its exact historical replacement.
        # Compose their disjoint corrections without accepting other differences.
        cases = [
            dict(case, expectation="oracle") if case["name"] in replacements else case
            for case in contract["cases"]
        ]
        cases = promote_precedence_cases(cases, precedence_replacements)
        scoped = scoped_precedence_replacements(
            json.loads(SCOPED_PRECEDENCE_PATH.read_text())
        )
        ownership = ownership_control_replacements(
            json.loads(OWNERSHIP_PATH.read_text())
        )
        self.assertFalse(scoped.keys() & ownership.keys())
        cases = promote_scoped_cases(cases, {**scoped, **ownership})
        self.assertEqual(len(eof["cases"]), 42)
        self.assertEqual(len({case["name"] for case in eof["cases"]}), 42)
        self.assertTrue(all(case["expectation"] == "oracle" for case in eof["cases"]))
        cases.extend(eof["cases"])
        cases.extend(comment_contract["cases"])
        self.assertEqual(len(cases), len({case["name"] for case in cases}))
        legacy, companion = [
            Path(os.environ[key])
            for key in ("CATABOLIC_LEGACY_BINARY", "CATABOLIC_NATIVE_BINARY")
        ]
        for path in (legacy, companion, fixture.PYTHON):
            self.assertTrue(path.is_file(), str(path))
        from scripts.qualify_native_mutations import oracle_snapshot

        self.assertEqual(
            oracle_snapshot(fixture.REFERENCE, fixture.ROOT)["reference_commit"],
            contract["provenance"]["reference_commit"],
        )
        with tempfile.TemporaryDirectory(prefix="token-locations-") as temporary:
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
            for case in cases:
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
            if report := os.environ.get("CATABOLIC_TOKEN_BUDGET_REPORT"):
                Path(report).write_text(
                    json.dumps(
                        dict(
                            cases=results,
                            database_sha256_before=before,
                            database_sha256_after=after,
                        ),
                        indent=2,
                    )
                    + "\n"
                )
            print(
                f"Token-budget boundary/precedence complete-envelope cases: {len(results)}"
            )


if __name__ == "__main__":
    unittest.main()
