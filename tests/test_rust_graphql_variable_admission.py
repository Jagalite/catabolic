# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Live native/oracle regression with a mandatory pinned historical baseline."""

import copy
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from scripts.qualify_native_mutations import oracle_snapshot
from tests import test_rust_legacy_parser as fixture

ORACLE = "import sys,pathlib;root=pathlib.Path(sys.argv.pop(1));sys.path.insert(0,str(root));from catabolic import cli;assert sys.version.split()[0]=='3.14.6';assert pathlib.Path(cli.__file__).resolve()==root.resolve()/'catabolic/cli.py';raise SystemExit(cli.main(sys.argv[1:]))"
BASELINE_FIXTURE = (
    fixture.ROOT / "tests/parity/graphql-variable-admission-baseline-v1.json"
)
BASELINE_FIXTURE_SHA256 = (
    "f4ff11b19959fc9682e1b4ad750cb5dbb091e979f862097fb0836c8bef175335"
)
INPUT_FIXTURE = fixture.ROOT / "tests/parity/graphql-variable-admission-v1.json"
HISTORICAL_CHECKPOINT = (
    fixture.ROOT / "tests/parity/native-graphql-variable-admission-v1-checkpoint.json"
)
BASELINE_ENV = "CATABOLIC_VARIABLE_ADMISSION_BASELINE"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def text_digest(value):
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def variants(cases):
    for case in cases:
        for mode in ([], ["--json"], ["--machine"]):
            sources = ["inline"]
            if case["name"].startswith("unused-") and mode == ["--machine"]:
                sources += ["file", "stdin"]
            for source in sources:
                yield case, mode, source


def row_key(name, mode, source):
    return name, tuple(mode), source


def validate_recording(recording, cases):
    """Check semantic provenance as well as the loader's whole-file digest pin."""
    require(recording["fixture_version"] == 1, "historical baseline version drift")
    provenance = recording["provenance"]
    require(
        provenance["input_fixture_sha256"] == digest(INPUT_FIXTURE),
        "input fixture drift",
    )
    require(
        provenance["historical_checkpoint_sha256"] == digest(HISTORICAL_CHECKPOINT),
        "historical checkpoint drift",
    )
    require(provenance["oracle_commit"] == fixture.FROZEN_COMMIT, "oracle commit drift")
    require(provenance["oracle_python"] == fixture.PINNED_PYTHON, "oracle Python drift")
    require(provenance["source_report_rows"] == 242, "historical report count drift")
    expected = list(variants(cases))
    rows = recording["rows"]
    require(len(expected) == len(rows) == 242, "historical baseline row coverage drift")
    indexed = {}
    for recorded, (case, mode, source) in zip(rows, expected, strict=True):
        key = row_key(case["name"], mode, source)
        require(
            row_key(recorded["name"], recorded["mode"], recorded["source"]) == key,
            "historical baseline row order/identity drift",
        )
        require(key not in indexed, "duplicate historical baseline row")
        require(
            recorded["expectation"] == case["expectation"],
            "historical expectation drift",
        )
        require(
            recorded["document_sha256"] == text_digest(case["document"]),
            "historical document drift",
        )
        require(
            recorded["variables_sha256"] == text_digest(case["raw"]),
            "historical variables drift",
        )
        output = recorded["baseline"]
        require(
            set(output) == {"code", "stdout", "stderr"}, "historical output shape drift"
        )
        require(type(output["code"]) is int, "historical exit code type drift")
        require(
            isinstance(output["stdout"], str) and isinstance(output["stderr"], str),
            "historical output type drift",
        )
        indexed[key] = output
    return indexed


def load_recording(path, cases):
    require(
        digest(path) == BASELINE_FIXTURE_SHA256,
        "historical baseline fixture digest mismatch",
    )
    recording = json.loads(path.read_text())
    return recording, validate_recording(recording, cases)


def select_baseline(environment):
    if BASELINE_ENV not in environment:
        return "recorded", None
    value = environment[BASELINE_ENV]
    require(value, f"{BASELINE_ENV} is set but empty")
    path = Path(value)
    require(path.is_file(), f"explicit baseline artifact is missing: {path}")
    return "live", path


def matches(expectation, oracle, native, historical, live_baseline=None):
    # A supplied baseline must execute and match every pinned historical response,
    # including rows whose native acceptance assertion otherwise uses the oracle.
    if live_baseline is not None and live_baseline != historical:
        return False
    if expectation == "equal":
        return native == oracle
    if expectation == "strict_unchanged":
        return native == historical and native["code"] == 2 and native != oracle
    if expectation == "accepted_unchanged":
        return native == historical == oracle and native["code"] == 0
    require(expectation == "unchanged", "unknown variable admission expectation")
    return native == historical


class GraphQLVariableAdmissionTest(unittest.TestCase):
    def test_version_one_error_only_admission(self):
        legacy = Path(os.environ["CATABOLIC_LEGACY_BINARY"])
        baseline_mode, baseline = select_baseline(os.environ)
        specification = json.loads(INPUT_FIXTURE.read_text())
        self.assertEqual(specification["fixture_version"], 1)
        cases = specification["cases"]
        self.assertEqual(len(cases), 76)
        self.assertEqual(len({case["name"] for case in cases}), len(cases))
        recording, historical = load_recording(BASELINE_FIXTURE, cases)
        old = (
            fixture.ROOT / "tests/parity/native-graphql-variable-errors-checkpoint.json"
        )
        paths = [
            legacy,
            INPUT_FIXTURE,
            BASELINE_FIXTURE,
            HISTORICAL_CHECKPOINT,
            old,
            Path(__file__).resolve(),
        ]
        if baseline is not None:
            paths.append(baseline)
        before = {str(path): digest(path) for path in paths}
        oracle_before = oracle_snapshot(fixture.REFERENCE, fixture.ROOT)
        self.assertEqual(oracle_before["reference_commit"], fixture.FROZEN_COMMIT)
        self.assertEqual(
            oracle_before["inventory_sha256"],
            recording["provenance"]["oracle_inventory_sha256"],
        )
        rows = []
        runs = {"oracle": 0, "native": 0, "baseline": 0}
        with tempfile.TemporaryDirectory(
            prefix="graphql-variable-admission-"
        ) as scratch:
            root = Path(scratch)
            env = fixture.environment(overrides={"HOME": str(root / "home")})
            oracle = [str(fixture.PYTHON), "-c", ORACLE, str(fixture.REFERENCE / "src")]
            commands = {"oracle": oracle, "native": [str(legacy)]}
            if baseline is not None:
                commands["baseline"] = [str(baseline)]
            databases = {name: root / f"{name}.db" for name in commands}
            initialized = subprocess.run(
                oracle + ["--db", str(databases["oracle"]), "init"],
                capture_output=True,
                env=env,
                timeout=90,
            )
            self.assertEqual(initialized.returncode, 0, initialized.stderr)
            for name, path in databases.items():
                if name != "oracle":
                    shutil.copyfile(databases["oracle"], path)
            database_before = {name: digest(path) for name, path in databases.items()}
            query = root / "query.graphql"
            for case, mode, source in variants(cases):
                # The fixed fixture currently uses {profile}; do not silently
                # substitute it if a future version changes the document.
                query.write_text(case["document"])
                tail = (
                    [case["document"]]
                    if source == "inline"
                    else ["--file", str(query) if source == "file" else "-"]
                )
                tail += ["--variables", case["raw"]]
                outputs = {}
                for name, command in commands.items():
                    result = subprocess.run(
                        command
                        + ["--db", str(databases[name])]
                        + mode
                        + ["graphql"]
                        + tail,
                        input=case["document"].encode() if source == "stdin" else b"",
                        capture_output=True,
                        env=env,
                        timeout=90,
                    )
                    runs[name] += 1
                    outputs[name] = {
                        "code": result.returncode,
                        "stdout": result.stdout.decode(),
                        "stderr": result.stderr.decode(),
                    }
                recorded = historical[row_key(case["name"], mode, source)]
                live = outputs.get("baseline")
                rows.append(
                    {
                        "name": case["name"],
                        "mode": mode,
                        "source": source,
                        "expectation": case["expectation"],
                        "oracle": outputs["oracle"],
                        "native": outputs["native"],
                        "historical_baseline": recorded,
                        "live_baseline": live,
                        "passed": matches(
                            case["expectation"],
                            outputs["oracle"],
                            outputs["native"],
                            recorded,
                            live,
                        ),
                    }
                )
            self.assertEqual(
                {name: digest(path) for name, path in databases.items()},
                database_before,
            )
        self.assertEqual({str(path): digest(Path(path)) for path in before}, before)
        self.assertEqual(
            oracle_snapshot(fixture.REFERENCE, fixture.ROOT), oracle_before
        )
        self.assertEqual(
            runs,
            {
                "oracle": 242,
                "native": 242,
                "baseline": 242 if baseline is not None else 0,
            },
        )
        failures = [row for row in rows if not row["passed"]]
        if report := os.environ.get("CATABOLIC_GRAPHQL_VARIABLE_ADMISSION_REPORT"):
            Path(report).write_text(
                json.dumps(
                    {
                        "report_version": 2,
                        "fixture_version": 1,
                        "baseline_mode": baseline_mode,
                        "matrix_process_runs": runs,
                        "identities": before,
                        "historical_provenance": recording["provenance"],
                        "oracle_identity": oracle_before,
                        "rows": rows,
                    },
                    ensure_ascii=False,
                    indent=2,
                )
                + "\n"
            )
        print(
            f"GraphQL variable admission v1: {len(rows)} comparisons, {len(failures)} failures; baseline={baseline_mode}; matrix process runs={runs}"
        )
        self.assertFalse(failures, json.dumps(failures, ensure_ascii=False, indent=2))


class GraphQLVariableAdmissionFixtureTest(unittest.TestCase):
    def setUp(self):
        self.cases = json.loads(INPUT_FIXTURE.read_text())["cases"]
        self.recording = json.loads(BASELINE_FIXTURE.read_text())

    def test_pinned_recording_has_every_input_variant(self):
        recording, indexed = load_recording(BASELINE_FIXTURE, self.cases)
        self.assertEqual(len(indexed), 242)
        self.assertEqual(
            recording["provenance"]["baseline_artifact"]["sha256"],
            "f994075a4ff5f71f102bf28d216c8c99406d71f296a2db262a2cdea805fad6f1",
        )

    def test_missing_or_modified_recording_cannot_fall_back(self):
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / "baseline.json"
            with self.assertRaises(FileNotFoundError):
                load_recording(path, self.cases)
            path.write_bytes(BASELINE_FIXTURE.read_bytes() + b" ")
            with self.assertRaisesRegex(AssertionError, "digest mismatch"):
                load_recording(path, self.cases)

    def test_recorded_mode_requires_unset_baseline_and_explicit_mode_requires_file(
        self,
    ):
        self.assertEqual(select_baseline({}), ("recorded", None))
        with self.assertRaisesRegex(AssertionError, "empty"):
            select_baseline({BASELINE_ENV: ""})
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / "baseline"
            with self.assertRaisesRegex(AssertionError, "missing"):
                select_baseline({BASELINE_ENV: str(path)})
            path.write_bytes(b"artifact identity is fenced by the live runner")
            self.assertEqual(select_baseline({BASELINE_ENV: str(path)}), ("live", path))

    def test_provenance_and_input_drift_are_rejected(self):
        for field in (
            "input_fixture_sha256",
            "historical_checkpoint_sha256",
            "oracle_commit",
            "oracle_python",
            "source_report_rows",
        ):
            with self.subTest(field=field):
                changed = copy.deepcopy(self.recording)
                changed["provenance"][field] = "drift"
                with self.assertRaises(AssertionError):
                    validate_recording(changed, self.cases)
        for field in ("document_sha256", "variables_sha256", "expectation"):
            with self.subTest(field=field):
                changed = copy.deepcopy(self.recording)
                changed["rows"][0][field] = "drift"
                with self.assertRaises(AssertionError):
                    validate_recording(changed, self.cases)

    def test_missing_duplicate_and_reordered_rows_are_rejected(self):
        for mutation in ("missing", "duplicate", "reordered"):
            with self.subTest(mutation=mutation):
                changed = copy.deepcopy(self.recording)
                if mutation == "missing":
                    changed["rows"].pop()
                elif mutation == "duplicate":
                    changed["rows"][1] = changed["rows"][0]
                else:
                    changed["rows"][0], changed["rows"][1] = (
                        changed["rows"][1],
                        changed["rows"][0],
                    )
                with self.assertRaises(AssertionError):
                    validate_recording(changed, self.cases)

    def test_assertion_modes_and_live_baseline_drift_remain_strict(self):
        ok = {"code": 0, "stdout": "success\n", "stderr": ""}
        strict = {"code": 2, "stdout": "", "stderr": "strict refusal\n"}
        alternate = {"code": 2, "stdout": "", "stderr": "other refusal\n"}
        for expectation, oracle, native, recorded in [
            ("equal", ok, ok, strict),
            ("strict_unchanged", ok, strict, strict),
            ("accepted_unchanged", ok, ok, ok),
            ("unchanged", alternate, strict, strict),
        ]:
            with self.subTest(expectation=expectation):
                self.assertTrue(matches(expectation, oracle, native, recorded))
                self.assertTrue(
                    matches(expectation, oracle, native, recorded, recorded)
                )
                self.assertFalse(
                    matches(expectation, oracle, native, recorded, alternate)
                )
                self.assertFalse(matches(expectation, oracle, alternate, recorded))
        self.assertFalse(matches("strict_unchanged", strict, strict, strict))
        self.assertFalse(matches("accepted_unchanged", strict, strict, strict))


if __name__ == "__main__":
    unittest.main()
