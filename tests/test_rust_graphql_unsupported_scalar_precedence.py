# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent unsupported-scalar admission controls, including NUL transport.

All expectations precede execution of the proposed Rust source. Historical
prior-six admission and new-three ownership inventories remain separately
versioned. The source-only fixture tests never launch a product process.
"""

import copy
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from collections import Counter
from pathlib import Path

from tests import test_rust_graphql_scoped_parser_diagnostics as scoped
from tests import test_rust_legacy_parser as fixture
from tests.test_rust_graphql_source_locations import ORACLE

CONTRACT = "native-graphql-unsupported-scalar-precedence-v1"
# Use this module's payload directory so a source-only overlay is testable while
# the original fixture module and historical roots still point at sealed stage01.
SCALAR_PATH = (
    Path(__file__).resolve().parent
    / "parity"
    / ("graphql-unsupported-scalar-precedence-v1.json")
)
PROVENANCE_SHA256 = "82944e046ef106e9d4962cd77d9bc2481c1a60f455aa340a88242f8d05e5b409"
CASES_SHA256 = "a88a45996fd3eecc4ba7fc7a125d0274b21fe39872a19fc38be0f7d520a62726"
REPLACEMENTS_SHA256 = "72d6280b525d2462e86415eb626e004f895e0fc1ebfbe5abee040e34a13bb66b"
SLASH_NAME = "unsupported-first-unsupported-slash"
STAGE01_FILE = "graphql-parser-budget-precedence-v2.json"
STAGE01_CONTRACT = "native-graphql-parser-budget-precedence-v2"
UNSUPPORTED_COMPANION = {
    "kind": "unsupported",
    "reason": "OS argv cannot contain U+0000; companion has no graphql --file transport",
}
INITIAL_SCALARS = (
    0x2F,
    0x2B,
    0x2E,
    0,
    0xB,
    0x7F,
    0x80,
    0xA0,
    0xE9,
    0x2028,
    0xD7FF,
    0xE000,
    0x10000,
    0x10FFFF,
)
CASE_NAMES = [f"initial-unsupported-u{point:04x}" for point in INITIAL_SCALARS] + [
    "lf-unicode-column-before-scalar",
    "cr-unicode-column-before-scalar",
    "crlf-unicode-column-before-scalar",
    "lf-scalar-after-3999-comments",
    "cr-scalar-after-4000-comments",
    "crlf-scalar-after-4001-comments",
    "successful-token-4001-before-scalar",
    "earlier-empty-selection-before-scalar",
    "earlier-apostrophe-before-scalar",
    "initial-apostrophe-control",
    "two-dot-fallback-control",
    "recognized-spread-control",
    "ordinary-string-contained-scalar",
    "comment-contained-scalar",
    "scalar-after-complete-number",
    "scalar-while-exponent-requires-digit",
    "skipped-bom-before-scalar",
]
BASELINE_NAMES = {
    "earlier-empty-selection-before-scalar",
    "earlier-apostrophe-before-scalar",
    "initial-apostrophe-control",
    "recognized-spread-control",
    "scalar-while-exponent-requires-digit",
}


def scalar_control_replacements(correction, parity_root=scoped.PARITY_ROOT):
    """Fail closed on the exact new inventory, transports and stage01 member."""
    if set(correction) != {"contract", "provenance", "stage01_replacements", "cases"}:
        raise ValueError("Unexpected unsupported-scalar contract fields")
    if correction["contract"] != CONTRACT:
        raise ValueError("Unexpected unsupported-scalar contract")
    if scoped.digest(correction["provenance"]) != PROVENANCE_SHA256:
        raise ValueError("Scalar source/oracle/transport provenance changed")
    if scoped.digest(correction["cases"]) != CASES_SHA256:
        raise ValueError("Scalar inventory, input, transport or envelopes changed")
    replacements = correction["stage01_replacements"]
    if scoped.digest(replacements) != REPLACEMENTS_SHA256:
        raise ValueError("Exact stage01 replacement changed")
    historical_path = parity_root / STAGE01_FILE
    if (
        hashlib.sha256(historical_path.read_bytes()).hexdigest()
        != (correction["provenance"]["stage01_fixture_sha256"])
    ):
        raise ValueError("Sealed stage01 fixture bytes changed")
    historical = json.loads(historical_path.read_text())
    scoped.scoped_control_replacements(historical, STAGE01_CONTRACT, parity_root)
    original = historical["cases"][120]
    expected = dict(
        historical_control=SLASH_NAME,
        document=original["document"],
        args=original["args"],
        expected=original["reference"],
        baseline=original["baseline"],
    )
    if original["name"] != SLASH_NAME or fixture.strict(replacements) != fixture.strict(
        [expected]
    ):
        raise ValueError("Stage01 /cases/120 input or complete envelopes changed")
    return {SLASH_NAME: replacements[0]}


def promote_stage01_cases(cases, correction=None, parity_root=scoped.PARITY_ROOT):
    """Replace only the exact versioned slash member without rewriting history."""
    if correction is None:
        correction = json.loads(SCALAR_PATH.read_text())
    replacements = scalar_control_replacements(correction, parity_root)
    if scoped.digest(cases) != scoped.CONTRACTS[STAGE01_CONTRACT]["cases_sha256"]:
        raise ValueError("Stage01 admission inventory differs before composition")
    return scoped.promote_scoped_cases(cases, replacements)


def expected_processes(case):
    """An unsupported transport is metadata, never an invented process result."""
    if case["expectation"] == "baseline":
        return dict(reference=case["reference"], **case["baseline"])
    if case["expectation"] != "oracle":
        raise ValueError("Unknown scalar expectation")
    expected = dict(reference=case["reference"], legacy=case["reference"])
    if case["transport"]["companion"]["kind"] != "unsupported":
        expected["companion"] = dict(
            code=0, stderr="", stdout=case["reference"]["stdout"]["data"]
        )
    return expected


def graphql_arguments(case, kind, directory):
    """Use only existing per-case transports; reject NUL argv explicitly."""
    transport = case["transport"][kind]
    if transport["kind"] == "unsupported":
        if kind != "companion" or transport != UNSUPPORTED_COMPANION:
            raise ValueError("Unknown unsupported transport")
        return None
    if transport["kind"] == "argv":
        if "\x00" in case["document"]:
            raise ValueError("OS argv cannot contain U+0000")
        return ["graphql", case["document"], *case["args"]]
    if transport["kind"] != "file" or kind == "companion":
        raise ValueError("Unknown or unavailable GraphQL transport")
    path = directory / f"{case['name']}.graphql"
    path.write_bytes(case["document"].encode())
    return ["graphql", "--file", str(path), *case["args"]]


class UnsupportedScalarPrecedenceFixtureTest(unittest.TestCase):
    def setUp(self):
        self.correction = json.loads(SCALAR_PATH.read_text())

    def test_exact_inventory_expectations_and_independent_membership(self):
        replacements = scalar_control_replacements(self.correction)
        cases = self.correction["cases"]
        self.assertEqual(list(replacements), [SLASH_NAME])
        self.assertEqual([case["name"] for case in cases], CASE_NAMES)
        self.assertEqual(len(set(CASE_NAMES)), 31)
        self.assertFalse(set(CASE_NAMES) & set(scoped.SCOPED_PRECEDENCE_NAMES))
        self.assertFalse(set(CASE_NAMES) & set(scoped.OWNERSHIP_NAMES))
        self.assertNotIn(SLASH_NAME, scoped.SCOPED_PRECEDENCE_NAMES)
        self.assertNotIn(SLASH_NAME, scoped.OWNERSHIP_NAMES)
        self.assertEqual(
            Counter(case["expectation"] for case in cases),
            {"oracle": 26, "baseline": 5},
        )
        self.assertEqual(
            {case["name"] for case in cases if case["expectation"] == "baseline"},
            BASELINE_NAMES,
        )
        self.assertEqual(
            sum(
                case["expectation"] == "oracle"
                and fixture.strict(case["reference"])
                != fixture.strict(case["baseline"]["legacy"])
                for case in cases
            ),
            23,
        )
        for case in cases:
            self.assertEqual(case["reference"]["code"], 2)
            self.assertLessEqual(len(case["document"].encode()), 131072)
            self.assertEqual(case["args"], [])
        for point, case in zip(INITIAL_SCALARS, cases[:14], strict=True):
            self.assertEqual(ord(case["document"][0]), point)

    def test_nul_transports_and_no_companion_outcome(self):
        nul = self.correction["cases"][3]
        self.assertEqual(nul["name"], "initial-unsupported-u0000")
        self.assertEqual(
            nul["transport"],
            dict(
                reference={"kind": "file"},
                legacy={"kind": "file"},
                companion=UNSUPPORTED_COMPANION,
            ),
        )
        self.assertEqual(set(nul["baseline"]), {"legacy"})
        self.assertEqual(set(expected_processes(nul)), {"reference", "legacy"})
        self.assertIn(
            "U+0000", nul["reference"]["stdout"]["data"]["errors"][0]["message"]
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.assertIsNone(graphql_arguments(nul, "companion", root))
            for kind in ("reference", "legacy"):
                args = graphql_arguments(nul, kind, root)
                self.assertEqual(args[:2], ["graphql", "--file"])
                self.assertEqual(Path(args[2]).read_bytes(), nul["document"].encode())
            changed = copy.deepcopy(nul)
            changed["transport"]["legacy"] = {"kind": "argv"}
            with self.assertRaisesRegex(ValueError, "argv"):
                graphql_arguments(changed, "legacy", root)
            for case in self.correction["cases"]:
                if case is nul:
                    continue
                for kind in ("reference", "legacy", "companion"):
                    self.assertEqual(case["transport"][kind], {"kind": "argv"})
                    self.assertEqual(
                        graphql_arguments(case, kind, root),
                        ["graphql", case["document"], *case["args"]],
                    )

    def test_exact_composition_preserves_stage01_and_ordinary_ownership(self):
        path = scoped.PARITY_ROOT / STAGE01_FILE
        old_bytes = path.read_bytes()
        historical = json.loads(old_bytes)
        untouched = copy.deepcopy(historical)
        promoted = promote_stage01_cases(historical["cases"], self.correction)
        self.assertEqual(historical, untouched)
        self.assertEqual(path.read_bytes(), old_bytes)
        changed = []
        for index, (old, new) in enumerate(
            zip(historical["cases"], promoted, strict=True)
        ):
            if old != new:
                changed.append(index)
                self.assertEqual(new, dict(old, expectation="oracle"))
        self.assertEqual(changed, [120])
        self.assertEqual(promoted[120]["name"], SLASH_NAME)
        self.assertEqual(len(promoted), 150)
        ownership = json.loads(scoped.OWNERSHIP_PATH.read_text())
        scoped.ownership_control_replacements(ownership)
        slash = ownership["cases"][24]
        self.assertEqual(slash["expectation"], "baseline")
        self.assertTrue(slash["document"].startswith("/"))
        # Existing frozen inventories, crash/recursion residuals and all original
        # consumers remain unchanged. This test does not execute those inputs.
        for contract, spec in scoped.CONTRACTS.items():
            scoped.scoped_control_replacements(
                json.loads(spec["path"].read_text()), contract
            )

    def test_mutations_of_identity_transport_inputs_and_envelopes_fail_closed(self):
        mutations = [
            lambda c: c.update(contract="unversioned"),
            lambda c: c.update(extra=True),
            lambda c: c.update(stage01_replacements=[]),
            lambda c: c["stage01_replacements"].append(c["stage01_replacements"][0]),
            lambda c: c["stage01_replacements"][0].update(historical_control="other"),
            lambda c: c["stage01_replacements"][0].update(document="{profile}"),
            lambda c: c["stage01_replacements"][0].update(args=["--timeout-ms", "0"]),
            lambda c: c["stage01_replacements"][0]["expected"].update(code=True),
            lambda c: c["stage01_replacements"][0]["expected"].update(stderr="changed"),
            lambda c: c["stage01_replacements"][0]["baseline"]["legacy"].update(code=0),
            lambda c: c["stage01_replacements"][0]["baseline"]["companion"].update(
                stdout={}
            ),
            lambda c: c["cases"].pop(),
            lambda c: c["cases"].reverse(),
            lambda c: c["cases"][0].update(document="{profile}"),
            lambda c: c["cases"][0].update(args=["--variables", "[]"]),
            lambda c: c["cases"][0].update(expectation="baseline"),
            lambda c: c["cases"][0]["reference"].update(code=True),
            lambda c: c["cases"][0]["reference"].update(stderr="changed"),
            lambda c: c["cases"][0]["reference"]["stdout"]["data"]["errors"][0].update(
                locations=[]
            ),
            lambda c: c["cases"][0]["baseline"]["legacy"].update(stdout={}),
            lambda c: c["cases"][0]["baseline"]["companion"].update(code=2),
            lambda c: c["cases"][3]["transport"]["reference"].update(kind="argv"),
            lambda c: c["cases"][3]["transport"]["legacy"].update(kind="argv"),
            lambda c: c["cases"][3]["transport"]["companion"].update(kind="file"),
            lambda c: c["cases"][3]["transport"]["companion"].update(
                reason="silently skipped"
            ),
            lambda c: c["cases"][3]["baseline"].update(companion={"code": 0}),
        ]
        mutations.extend(
            lambda c, key=key: c["provenance"].update({key: "changed"})
            for key in self.correction["provenance"]
        )
        for index, mutate in enumerate(mutations):
            changed = copy.deepcopy(self.correction)
            mutate(changed)
            with self.subTest(mutation=index), self.assertRaises(ValueError):
                scalar_control_replacements(changed)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / STAGE01_FILE).write_bytes(
                (scoped.PARITY_ROOT / STAGE01_FILE).read_bytes() + b"\n"
            )
            with self.assertRaises(ValueError):
                scalar_control_replacements(self.correction, root)

    def test_composition_rejects_missing_duplicate_and_changed_stage01_cases(self):
        original = json.loads((scoped.PARITY_ROOT / STAGE01_FILE).read_text())["cases"]
        for field, value in (
            ("name", "other"),
            ("document", "{profile}"),
            ("args", []),
            ("reference", {}),
            ("baseline", {}),
            ("expectation", "oracle"),
        ):
            changed = copy.deepcopy(original)
            changed[120][field] = value if field != "args" else ["--variables", "[]"]
            with self.subTest(field=field), self.assertRaises(ValueError):
                promote_stage01_cases(changed, self.correction)
        for changed in (
            original[:-1],
            original + [original[120]],
            list(reversed(original)),
        ):
            with self.assertRaises(ValueError):
                promote_stage01_cases(changed, self.correction)


def compose_runtime_contract():
    correction = json.loads(SCALAR_PATH.read_text())
    scalar_control_replacements(correction)
    from tests import test_rust_graphql_lexical_ownership as lexical

    correction["cases"] = lexical.compose_cases(correction["cases"], CONTRACT)
    composed_replacements = lexical.composition_records(CONTRACT)
    from tests import test_rust_graphql_final_composition as combined

    correction["cases"] = combined.compose_cases(correction["cases"], CONTRACT)
    composed_replacements.extend(combined.composition_records(CONTRACT))
    return correction, composed_replacements


class UnsupportedScalarPrecedenceTest(unittest.TestCase):
    def test_scalar_process_envelopes_with_explicit_transport_coverage(self):
        correction, composed_replacements = compose_runtime_contract()
        legacy = Path(os.environ["CATABOLIC_LEGACY_BINARY"])
        companion = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        for path in (legacy, companion, fixture.PYTHON):
            self.assertTrue(path.is_file(), str(path))
        from scripts.qualify_native_mutations import oracle_snapshot

        snapshot = oracle_snapshot(fixture.REFERENCE, fixture.ROOT)
        for key in ("reference_commit", "inventory_sha256", "resource_sha256"):
            self.assertEqual(
                snapshot[key], correction["provenance"]["oracle_snapshot"][key]
            )
        with tempfile.TemporaryDirectory(
            prefix="unsupported-scalar-replay-"
        ) as temporary:
            root = Path(temporary)
            env = fixture.environment(
                overrides={
                    "HOME": str(root / "home"),
                    "PYTHONDONTWRITEBYTECODE": "1",
                }
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
                timeout=90,
            )
            self.assertEqual(
                (version.returncode, version.stdout, version.stderr),
                (0, b"3.14.6\n3.2.12\n", b""),
            )
            databases = {
                kind: root / f"{kind}.db"
                for kind in ("reference", "legacy", "companion")
            }
            initialized = subprocess.run(
                oracle + ["--db", str(databases["reference"]), "init"],
                env=env,
                capture_output=True,
                timeout=90,
            )
            self.assertEqual((initialized.returncode, initialized.stderr), (0, b""))
            for kind in ("legacy", "companion"):
                shutil.copyfile(databases["reference"], databases[kind])
            before = {
                kind: hashlib.sha256(path.read_bytes()).hexdigest()
                for kind, path in databases.items()
            }
            results = []
            for case in correction["cases"]:
                actual, raw, unsupported = {}, {}, {}
                for kind, prefix, flag in (
                    ("reference", oracle, "--machine"),
                    ("legacy", [str(legacy)], "--machine"),
                    ("companion", [str(companion)], "--json"),
                ):
                    arguments = graphql_arguments(case, kind, root)
                    if arguments is None:
                        unsupported[kind] = case["transport"][kind]
                        continue
                    process = subprocess.run(
                        prefix + ["--db", str(databases[kind]), flag, *arguments],
                        env=env,
                        capture_output=True,
                        timeout=90,
                    )
                    stdout_text, stderr_text = (
                        process.stdout.decode(),
                        process.stderr.decode(),
                    )
                    try:
                        stdout = json.loads(stdout_text)
                    except json.JSONDecodeError:
                        stdout = stdout_text
                    actual[kind] = dict(
                        code=process.returncode, stdout=stdout, stderr=stderr_text
                    )
                    raw[kind] = dict(stdout=stdout_text, stderr=stderr_text)
                expected = expected_processes(case)
                match = fixture.strict(actual) == fixture.strict(expected)
                results.append(
                    dict(
                        name=case["name"],
                        expectation=case["expectation"],
                        transport=case["transport"],
                        results=actual,
                        raw_streams=raw,
                        unsupported_transport=unsupported,
                        match=match,
                    )
                )
                with self.subTest(case=case["name"]):
                    self.assertEqual(fixture.strict(actual), fixture.strict(expected))
            after = {
                kind: hashlib.sha256(path.read_bytes()).hexdigest()
                for kind, path in databases.items()
            }
            self.assertEqual(before, after)
            if report := os.environ.get(
                "CATABOLIC_UNSUPPORTED_SCALAR_PRECEDENCE_REPORT"
            ):
                Path(report).write_text(
                    json.dumps(
                        dict(
                            contract=CONTRACT,
                            composed_replacements=composed_replacements,
                            cases=results,
                            process_captures=sum(len(r["results"]) for r in results),
                            unsupported_companion_cases=1,
                            database_sha256_before=before,
                            database_sha256_after=after,
                        ),
                        indent=2,
                    )
                    + "\n"
                )
            print(
                f"{CONTRACT}: 31 controls, 92 process envelopes, one unsupported companion NUL transport"
            )


if __name__ == "__main__":
    unittest.main()
