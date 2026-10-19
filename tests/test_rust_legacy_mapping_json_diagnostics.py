# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Stored mapping subject syntax diagnostics on disposable catalogs only.

Syntax envelopes retain the preserved CLI boundary. Runtime v2 explicitly
replaces unhandled TypeError/KeyError tracebacks with the approved envelope;
public Python exception attributes remain outside this suite.
"""

import base64
import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from tests.test_rust_legacy_review import BINARY, ORACLE, PYTHON, REFERENCE

CAPTURES = []
STATE_PROOFS = []
MODES = ([], ["--json"], ["--machine"])
VALID = '{"owner":"fixture","key":"key","state":"uncertain","plan_id":"plan"}'
MALFORMED = (
    "",
    " \t\r\n",
    "{",
    "[",
    "{unquoted:1}",
    '{"x" 1}',
    '{"x":}',
    '{"x":1 "y":2}',
    '{"x":1,}',
    "[1, ]",
    "[true false]",
    '{"x":undefined}',
    '{"x":tru}',
    '{"x":1e+}',
    '{"x":01}',
    '{"x":1.}',
    '{"x":"unterminated}',
    '"unterminated',
    '{"x":"\\q"}',
    '{"x":"\\u12"}',
    '{"x":"line\nbreak"}',
    "{} trailing",
    "\ufeff{}",
    '{"é😀":1,\n "x": [true false]}',
    '{"雪":"\\q"}',
    '{"雪":"\\u12"}',
    '{"nested":[{"a":1,}]}',
    '{"error":{"type":"CatabolicError","message":"invalid JSON"},',
)


class NativeLegacyMappingJsonDiagnosticsTest(unittest.TestCase):
    def setUp(self):
        fixture = json.loads(
            (
                Path(__file__).parent
                / "parity/mapping-subject-json-diagnostics-v1.json"
            ).read_text()
        )
        self.assertEqual(fixture["format_version"], 1)
        self.assertEqual(len(fixture["cases"]), len(MALFORMED) * 3 * 4)
        self.assertEqual(
            hashlib.sha256(PYTHON.read_bytes()).hexdigest(),
            fixture["oracle"]["python_sha256"],
        )
        self.assertEqual(
            hashlib.sha256((REFERENCE / "catabolic/cli.py").read_bytes()).hexdigest(),
            fixture["oracle"]["cli_sha256"],
        )
        self.expected = {
            (case["subject"], tuple(case["args"])): case["expected"]
            for case in fixture["cases"]
        }
        for path in (PYTHON, REFERENCE / "catabolic/cli.py", BINARY):
            self.assertTrue(path.is_file(), str(path))
        temporary = tempfile.TemporaryDirectory(prefix="mapping-json-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.db = {True: self.root / "python.db", False: self.root / "native.db"}
        self.env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("CATABOLIC_")
            and key not in ("PYTHONPATH", "PYTHONHOME")
        }
        self.env.update(
            HOME=str(self.root / "home"),
            COLUMNS="80",
            TERM="dumb",
            NO_COLOR="1",
            PYTHONUTF8="1",
            PYTHONHASHSEED="0",
            PYTHONDONTWRITEBYTECODE="1",
        )
        result = self.run_cli(True, ["init"])
        self.assertEqual(result.returncode, 0, result.stderr)
        shutil.copyfile(self.db[True], self.db[False])

    def run_cli(self, oracle, args):
        command = (
            ([str(PYTHON), "-c", ORACLE, str(REFERENCE)] if oracle else [str(BINARY)])
            + ["--db", str(self.db[oracle])]
            + args
        )
        result = subprocess.run(command, env=self.env, capture_output=True, timeout=30)
        CAPTURES.append(
            {
                "test": self.id(),
                "engine": "frozen-python" if oracle else "native-cli",
                "argv": command,
                "returncode": result.returncode,
                "stdout_base64": base64.b64encode(result.stdout).decode(),
                "stderr_base64": base64.b64encode(result.stderr).decode(),
                "stdout_sha256": hashlib.sha256(result.stdout).hexdigest(),
                "stderr_sha256": hashlib.sha256(result.stderr).hexdigest(),
                "stdout_utf8": result.stdout.decode("utf-8", "replace"),
                "stderr_utf8": result.stderr.decode("utf-8", "replace"),
            }
        )
        return result

    def seed(self, raw):
        self.raw = raw
        for path in self.db.values():
            with closing(sqlite3.connect(path)) as database:
                database.execute("DELETE FROM consumer_events")
                for identifier, subject in (("first", VALID), ("later", raw)):
                    database.execute(
                        "INSERT INTO consumer_events(id,profile,event,severity,subject) "
                        "VALUES (?,'default','mapping_publish','info',?)",
                        (identifier, subject),
                    )
                # A malformed unrelated event is outside aggregate admission.
                database.execute(
                    "INSERT INTO consumer_events(id,profile,event,severity,subject) "
                    "VALUES ('ignored','default','other','info','{')"
                )
                database.commit()

    def snapshot(self):
        snapshots = {}
        for flag, path in self.db.items():
            with closing(sqlite3.connect(path)) as database:
                logical = list(database.iterdump())
            snapshots[flag] = {
                "logical": logical,
                "bytes": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
        snapshots["directory"] = sorted(path.name for path in self.root.iterdir())
        return snapshots

    def assert_unchanged(self, before):
        after = self.snapshot()
        self.assertEqual(after, before)
        STATE_PROOFS.append(
            {
                "test": self.id(),
                "before": {str(key): value for key, value in before.items()},
                "after": {str(key): value for key, value in after.items()},
                "equal": True,
            }
        )

    def exact(self, args, code=0):
        old, new = [self.run_cli(flag, args) for flag in (True, False)]
        self.assertEqual(old.returncode, code, (old.stdout, old.stderr))
        if expected := self.expected.get((self.raw, tuple(args))):
            self.assertEqual(
                (old.returncode, old.stdout, old.stderr),
                (
                    expected["code"],
                    expected["stdout"].encode(),
                    expected["stderr"].encode(),
                ),
            )
        self.assertEqual(
            (new.returncode, new.stdout, new.stderr),
            (old.returncode, old.stdout, old.stderr),
        )
        return old, new

    def test_malformed_subject_syntax_exact_full_process_envelopes(self):
        for raw in MALFORMED:
            with self.subTest(subject=raw):
                self.seed(raw)
                before = self.snapshot()
                for mode in MODES:
                    for selection in (
                        [],
                        ["--event", "later"],
                        ["--event", "first"],
                        ["--event", "absent"],
                    ):
                        self.exact(
                            mode + ["projection", "mapping-events"] + selection, 2
                        )
                self.assert_unchanged(before)

    def test_valid_error_looking_values_and_duplicate_keys_are_ordinary_data(self):
        for raw in (
            VALID,
            '{"owner":"fixture","key":"key","state":"uncertain","plan_id":"plan",'
            '"error":{"type":"JSONDecodeError","message":"invalid JSON","raw":"{"},'
            '"nested":{"id":"forged"}}',
            '{"owner":"discarded","key":"key","state":"uncertain","plan_id":"plan",'
            '"\\u006fwner":"kept","z":{"b":1,"a":2}}',
        ):
            with self.subTest(subject=raw):
                self.seed(raw)
                before = self.snapshot()
                for mode in MODES:
                    for selection in ([], ["--event", "later"]):
                        self.exact(mode + ["projection", "mapping-events"] + selection)
                self.assert_unchanged(before)

    def test_decoder_refusals_are_not_reclassified_as_syntax(self):
        for raw, message in (
            ('{"x":NaN}', "nonfinite JSON number is not permitted: NaN"),
            ('{"x":Infinity}', "nonfinite JSON number is not permitted: Infinity"),
            ('{"x":-Infinity}', "nonfinite JSON number is not permitted: -Infinity"),
            ('{"x":"\\ud800"}', "invalid JSON"),
            ('{"x":' + "[" * 514 + "0" + "]" * 514 + "}", "invalid JSON"),
        ):
            with self.subTest(subject=raw):
                self.seed(raw)
                before = self.snapshot()
                for selection in ([], ["--event", "first"]):
                    result = self.run_cli(
                        False, ["--machine", "projection", "mapping-events"] + selection
                    )
                    self.assertEqual((result.returncode, result.stderr), (2, b""))
                    self.assertEqual(
                        json.loads(result.stdout)["data"]["error"],
                        {"type": "CatabolicError", "message": message},
                    )
                self.assert_unchanged(before)

    def test_runtime_v2_preserves_exception_and_exit_without_traceback(self):
        for raw in ("null", "[]", '"text"', "true", "17", '{"id":"forged"}', "{}"):
            with self.subTest(subject=raw):
                self.seed(raw)
                before = self.snapshot()
                for mode in MODES:
                    for selection in ([], ["--event", "later"]):
                        args = mode + ["projection", "mapping-events"] + selection
                        if raw == "{}" and selection:
                            self.exact(args)
                            continue
                        old, new = [self.run_cli(flag, args) for flag in (True, False)]
                        self.assertEqual((old.returncode, old.stdout), (1, b""))
                        self.assertIn(b"Traceback (most recent call last)", old.stderr)
                        self.assertEqual(new.returncode, 1)
                        kind, message = (
                            old.stderr.decode().splitlines()[-1].split(": ", 1)
                        )
                        self.assertNotIn(b"Traceback", new.stdout + new.stderr)
                        if mode:
                            if mode == ["--machine"]:
                                payload = json.loads(new.stdout)["data"]
                                self.assertEqual(new.stderr, b"")
                            else:
                                payload = json.loads(new.stderr)
                                self.assertEqual(new.stdout, b"")
                            self.assertEqual(
                                payload["error"],
                                {"type": kind, "message": message},
                            )
                        else:
                            self.assertEqual(new.stdout, b"")
                            self.assertEqual(new.stderr, f"error: {message}\n".encode())
                self.assert_unchanged(before)


def tearDownModule():
    if destination := os.environ.get("CATABOLIC_MAPPING_JSON_REPORT"):
        Path(destination).write_text(
            json.dumps(
                {
                    "format_version": 1,
                    "processes": CAPTURES,
                    "state_proofs": STATE_PROOFS,
                },
                indent=2,
            )
            + "\n"
        )
