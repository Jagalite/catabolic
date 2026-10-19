# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Sealed observations and explicit pending lexical expectations.

Metadata tests never import products or launch processes. Runtime consumers are
separate and require source-bound helper/environment admission by qualification.
"""

import copy
import hashlib
import json
import unittest
from pathlib import Path

PARITY_ROOT = Path(__file__).resolve().parent / "parity"
FILE = "graphql-parser-lexical-capture-v1.json"
CONTRACT = "native-graphql-parser-lexical-capture-v1"
FILE_SHA256 = "7a0c2f8c3da34fe83e15a4aa5cd087e45c4884086e629a8e6329347bc21f46f2"
SECTION_DIGESTS = {
    "cases": "cd6cc93f64e62234efbf9ae6fd5877a8794845dd1c395b4ffe3c60a262a80985",
    "cli_controls": "79b3ad1e7bed063ab844b46344cd81385f99aa44d5a6d263eff634724f230b57",
    "composition": "61a855ea346edb007da24ac00a3a59981035b3da62825c9c3c0798b55cb46b6f",
    "promotions": "408ecaa8cd6253a98f8a2e838822a15c14b52711c52229190a71b49b54ce7a9a",
    "provenance": "8dc8d713ee076962db80352e8e78c50aa57462e0d4b5e71a4f60818368c51a33",
    "unsupported": "16a7ab279f61d5255c7de5bd45b57e9e925bab8a1a8d0e8d3976311b50dd667b",
}


def digest(value):
    return hashlib.sha256(
        json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode()
    ).hexdigest()


def expand(value):
    if value["encoding"] == "literal":
        return value["text"]
    if value["encoding"] != "prefix-repeat-suffix":
        raise ValueError("Unknown input encoding")
    return value["prefix"] + value["repeat"] * value["count"] + value["suffix"]


def validate_contract(contract):
    if (
        set(contract) != {"contract", *SECTION_DIGESTS}
        or contract["contract"] != CONTRACT
    ):
        raise ValueError("Unknown captured lexical contract")
    for key, expected in SECTION_DIGESTS.items():
        if digest(contract[key]) != expected:
            raise ValueError(f"Captured lexical section drifted: {key}")
    for target in contract["promotions"]:
        case = contract["cases"][target["index"]]
        if (
            digest(case) != target["case_sha256"]
            or case["name"] != target["name"]
            or digest(case["reference"]) != digest(target["expected"])
        ):
            raise ValueError("Captured lexical promotion preimage differs")
        if digest(case["reference"]) == digest(case["baseline"]["legacy"]):
            raise ValueError("Promotion is not an observed diagnostic mismatch")


def load_contract(parity_root=PARITY_ROOT):
    raw = (parity_root / FILE).read_bytes()
    if hashlib.sha256(raw).hexdigest() != FILE_SHA256:
        raise ValueError("Captured lexical fixture bytes changed")
    contract = json.loads(raw)
    validate_contract(contract)
    return contract


def compose_cases(cases, contract=None):
    contract = load_contract() if contract is None else contract
    validate_contract(contract)
    if digest(cases) != contract["composition"]["raw_cases_sha256"]:
        raise ValueError(
            "Captured lexical case inventory is not the exact raw preimage"
        )
    result = copy.deepcopy(cases)
    for target in contract["promotions"]:
        result[target["index"]]["expectation"] = "oracle"
    if digest(result) != contract["composition"]["composed_cases_sha256"]:
        raise ValueError("Captured lexical post-composition inventory differs")
    return result


def verify_raw_receipts(contract):
    """Explicit evidence admission against the immutable capture seal."""
    validate_contract(contract)
    seal_record = contract["provenance"]["capture_seal"]
    seal_path = Path(seal_record["path"])
    if hashlib.sha256(seal_path.read_bytes()).hexdigest() != seal_record["sha256"]:
        raise ValueError("Capture seal differs")
    seal = json.loads(seal_path.read_bytes())
    capture_root = seal_path.parent
    manifest_path = Path(contract["provenance"]["transport_manifest"]["path"])
    if (
        hashlib.sha256(manifest_path.read_bytes()).hexdigest()
        != contract["provenance"]["transport_manifest"]["sha256"]
    ):
        raise ValueError("Capture transport preimage differs")
    manifest = json.loads(manifest_path.read_bytes())
    checked = set()
    for case in [*contract["cases"], *contract["cli_controls"]]:
        original = manifest["cases"][case["capture_index"]]
        document = case["document"] if "document" in case else expand(case["input"])
        if (
            original["name"] != case["name"]
            or original["args"] != case["args"]
            or hashlib.sha256(document.encode()).hexdigest()
            != original["document"]["sha256"]
        ):
            raise ValueError("Captured document/arguments/name changed")
        groups = dict(case["raw_receipts"])
        if "frozen_receipts" in case:
            groups["frozen"] = case["frozen_receipts"]
        for lane, receipts in groups.items():
            for item in receipts.values():
                path = Path(item["path"])
                relative = path.relative_to(capture_root).as_posix()
                if seal["files"][relative] != {k: item[k] for k in ("bytes", "sha256")}:
                    raise ValueError("Receipt is not an exact capture-seal preimage")
                if path not in checked:
                    if (
                        path.stat().st_size != item["bytes"]
                        or hashlib.sha256(path.read_bytes()).hexdigest()
                        != item["sha256"]
                    ):
                        raise ValueError("Raw capture receipt bytes changed")
                    checked.add(path)
            stdout = Path(receipts["stdout"]["path"]).read_bytes()
            try:
                stdout = json.loads(stdout)
            except (UnicodeDecodeError, json.JSONDecodeError):
                stdout = stdout.decode()
            observed = dict(
                code=json.loads(Path(receipts["exit.json"]["path"]).read_bytes())[
                    "returncode"
                ],
                stdout=stdout,
                stderr=Path(receipts["stderr"]["path"]).read_bytes().decode(),
            )
            expected = (
                case["frozen_parse"]
                if lane == "frozen"
                else case["observed"][lane]
                if "observed" in case
                else case["reference"]
                if lane == "reference"
                else case["baseline"][lane]
            )
            if digest(observed) != digest(expected):
                raise ValueError("Complete captured envelope differs from raw receipts")
    return len(checked)


class CapturedLexicalFixtureTest(unittest.TestCase):
    def test_exact_selection_and_only_104_pending_promotions(self):
        contract = load_contract()
        before = copy.deepcopy(contract["cases"])
        after = compose_cases(before, contract)
        self.assertEqual(len(before), 197)
        self.assertEqual(sum(c["preflight"] for c in before), 180)
        self.assertEqual(len(contract["promotions"]), 104)
        for target in contract["promotions"]:
            case = before[target["index"]]
            old, new = (
                copy.deepcopy(case["baseline"]["legacy"]),
                copy.deepcopy(case["reference"]),
            )
            self.assertEqual(
                case["baseline"]["companion"],
                dict(code=0, stdout=old["stdout"]["data"], stderr=""),
            )
            self.assertEqual(
                new["stdout"]["data"]["errors"],
                [case["frozen_parse"]["stdout"]["formatted"]],
            )
            for envelope in (old, new):
                self.assertEqual(len(envelope["stdout"]["data"]["errors"]), 1)
                diagnostic = envelope["stdout"]["data"]["errors"][0]
                diagnostic.pop("message")
                diagnostic.pop("locations")
            self.assertEqual(digest(old), digest(new))
        changes = {t["index"] for t in contract["promotions"]}
        for index, (old, new) in enumerate(zip(before, after, strict=True)):
            self.assertEqual(
                new, dict(old, expectation="oracle") if index in changes else old
            )
        self.assertEqual(contract["cases"], before)
        retained = [c for c in after if c["group"] == "unowned first failure must stop"]
        self.assertEqual(len(retained), 48)
        self.assertTrue(all(c["expectation"] == "baseline" for c in retained))
        self.assertEqual(
            sum(
                digest(c["reference"]) != digest(c["baseline"]["legacy"])
                for c in retained
            ),
            40,
        )

    def test_mutated_contracts_and_complete_envelopes_fail_closed(self):
        original = load_contract()
        mutations = [
            lambda c: c.update(contract="unknown"),
            lambda c: c["cases"].pop(),
            lambda c: c["cases"].reverse(),
            lambda c: c["cases"].append(c["cases"][0]),
            lambda c: c["promotions"].pop(),
            lambda c: c["promotions"][0].update(index=1),
            lambda c: c["cases"][0]["reference"].update(code=True),
            lambda c: c["cases"][0]["baseline"]["companion"].update(stderr="changed"),
            lambda c: c["cases"][0]["raw_receipts"]["reference"]["stdout"].update(
                sha256="0" * 64
            ),
            lambda c: c["unsupported"][0].update(expectations={}),
            lambda c: c["cli_controls"][0]["observed"]["companion"].update(code=0),
        ]
        for mutate in mutations:
            changed = copy.deepcopy(original)
            mutate(changed)
            with self.assertRaises(ValueError):
                validate_contract(changed)

    def test_composition_rejects_partial_reordered_or_prior_composition(self):
        contract = load_contract()
        original = contract["cases"]
        for changed in (
            original[:-1],
            original + [original[0]],
            list(reversed(original)),
            compose_cases(original, contract),
        ):
            with self.assertRaises(ValueError):
                compose_cases(changed, contract)
        changed = copy.deepcopy(original)
        changed[70]["document"] += " "
        with self.assertRaises(ValueError):
            compose_cases(changed, contract)

    def test_raw_receipts_bind_complete_observed_envelopes(self):
        self.assertEqual(verify_raw_receipts(load_contract()), 4974)

    def test_unsupported_and_file_reader_surfaces_stay_distinct(self):
        contract = load_contract()
        self.assertEqual(len(contract["unsupported"]), 4)
        self.assertTrue(all(c["expectations"] is None for c in contract["unsupported"]))
        files = [
            c
            for c in contract["cli_controls"]
            if c["surface"] == "CLI file-reader admission"
        ]
        self.assertEqual(len(files), 4)
        self.assertTrue(
            all(set(c["observed"]) == {"reference", "legacy"} for c in files)
        )
        self.assertTrue(all(len(expand(c["input"]).encode()) == 131073 for c in files))
        self.assertEqual(len(contract["cli_controls"]), 16)
        self.assertTrue(
            all(
                not c["args"] and len(c["document"].encode()) + 1 <= 131072
                for c in contract["cases"]
            )
        )


class CapturedLexicalRuntimeTest(unittest.TestCase):
    def test_captured_graphql_complete_process_envelopes(self):
        from tests import test_rust_graphql_scoped_parser_diagnostics as scoped

        scoped.ScopedParserDiagnosticTest.replay(
            self, CONTRACT, "CATABOLIC_LEXICAL_CAPTURE_REPORT"
        )

    def test_captured_cli_admission_complete_process_envelopes(self):
        import os
        import shutil
        import subprocess
        import tempfile

        from tests import test_rust_legacy_parser as fixture
        from tests.test_rust_graphql_source_locations import ORACLE

        contract = load_contract()
        from scripts.qualify_native_mutations import oracle_snapshot

        snapshot = oracle_snapshot(fixture.REFERENCE, fixture.ROOT)
        for key in ("reference_commit", "inventory_sha256", "resource_sha256"):
            self.assertEqual(
                snapshot[key], contract["provenance"]["oracle_snapshot"][key]
            )
        with tempfile.TemporaryDirectory(
            prefix="lexical-capture-admission-"
        ) as temporary:
            root = Path(temporary)
            env = fixture.environment(
                overrides={"HOME": str(root / "home"), "PYTHONDONTWRITEBYTECODE": "1"}
            )
            oracle = [str(fixture.PYTHON), "-c", ORACLE, str(fixture.REFERENCE / "src")]
            commands = {
                "reference": oracle,
                "legacy": [os.environ["CATABOLIC_LEGACY_BINARY"]],
                "companion": [os.environ["CATABOLIC_NATIVE_BINARY"]],
            }
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
            databases = {lane: root / (lane + ".db") for lane in commands}
            initialized = subprocess.run(
                oracle + ["--db", str(databases["reference"]), "init"],
                env=env,
                capture_output=True,
                timeout=90,
            )
            self.assertEqual((initialized.returncode, initialized.stderr), (0, b""))
            for lane in ("legacy", "companion"):
                shutil.copyfile(databases["reference"], databases[lane])

            def state():
                return {
                    str(p) + suffix: hashlib.sha256(
                        Path(str(p) + suffix).read_bytes()
                    ).hexdigest()
                    if Path(str(p) + suffix).exists()
                    else None
                    for p in databases.values()
                    for suffix in ("", "-wal", "-shm", "-journal")
                }

            before = state()
            results = []
            for case in contract["cli_controls"]:
                document = expand(case["input"])
                document_path = root / (case["name"] + ".graphql")
                document_path.write_bytes(document.encode())
                for lane, expected in case["observed"].items():
                    selection = case["lanes"][lane]
                    args = (
                        ["graphql", "--file", str(document_path), *case["args"]]
                        if selection["transport"] == "document_file"
                        else ["graphql", document, *case["args"]]
                    )
                    argv = commands[lane] + [
                        "--db",
                        str(databases[lane]),
                        "--json" if lane == "companion" else "--machine",
                        *args,
                    ]
                    self.assertTrue(
                        all(
                            len(os.fsencode(a)) + 1 <= 131072 and "\0" not in a
                            for a in argv
                        )
                    )
                    process = subprocess.run(
                        argv, env=env, capture_output=True, text=True, timeout=90
                    )
                    try:
                        stdout = json.loads(process.stdout)
                    except json.JSONDecodeError:
                        stdout = process.stdout
                    actual = dict(
                        code=process.returncode, stdout=stdout, stderr=process.stderr
                    )
                    with self.subTest(case=case["name"], lane=lane):
                        self.assertEqual(digest(actual), digest(expected))
                    results.append(dict(name=case["name"], lane=lane, result=actual))
                    self.assertEqual(state(), before)
            self.assertEqual(len(results), 44)
            if report := os.environ.get("CATABOLIC_LEXICAL_CLI_ADMISSION_REPORT"):
                Path(report).write_text(
                    json.dumps(
                        dict(
                            contract=CONTRACT,
                            cases=results,
                            database_before=before,
                            database_after=state(),
                        ),
                        indent=2,
                    )
                    + "\n"
                )
