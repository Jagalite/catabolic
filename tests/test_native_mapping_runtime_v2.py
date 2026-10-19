# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Approved CLI runtime-error replacement, using live pinned Python envelopes.

Historical captures remain unchanged. This suite qualifies local CLI behavior,
not installed wheels, public Python exception objects, or a release.
"""

import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path

from tests.test_rust_legacy_review import BINARY, ORACLE, PYTHON, REFERENCE


class MappingRuntimeV2Test(unittest.TestCase):
    def setUp(self):
        self.assertEqual(
            subprocess.check_output(
                ["git", "-C", str(REFERENCE.parent), "rev-parse", "HEAD"], text=True
            ).strip(),
            "453fca983222c6665a48775eef67c489c96b527e",
        )
        self.assertEqual(
            subprocess.check_output([str(PYTHON), "--version"], text=True).strip(),
            "Python 3.14.6",
        )
        self.assertTrue(BINARY.is_file())
        temporary = tempfile.TemporaryDirectory(prefix="mapping-runtime-v2-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.paths = [self.root / "python.db", self.root / "native.db"]
        self.env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("CATABOLIC_")
            and key not in ("PYTHONPATH", "PYTHONHOME")
        }
        self.env.update(PYTHONDONTWRITEBYTECODE="1", NO_COLOR="1", TERM="dumb")
        initialized = self.run_cli(0, ["init"])
        self.assertEqual(initialized.returncode, 0, initialized.stderr)
        shutil.copyfile(*self.paths)

    def run_cli(self, engine, args):
        command = (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE)]
            if engine == 0
            else [str(BINARY)]
        )
        return subprocess.run(
            command + ["--db", str(self.paths[engine])] + args,
            capture_output=True,
            env=self.env,
            timeout=30,
        )

    def seed(self, raw, *rows):
        rows = (("event", raw),) + rows
        for path in self.paths:
            with sqlite3.connect(path) as db:
                db.execute("DELETE FROM consumer_events")
                db.executemany(
                    "INSERT INTO consumer_events(id,profile,event,severity,subject) "
                    "VALUES (?,'default','mapping_publish','info',?)",
                    rows,
                )

    def assert_runtime_v2(self, selections):
        before = [path.read_bytes() for path in self.paths]
        for mode in ([], ["--json"], ["--machine"]):
            for selection in selections:
                with self.subTest(mode=mode, selection=selection):
                    args = mode + ["projection", "mapping-events"] + selection
                    old, new = [self.run_cli(engine, args) for engine in (0, 1)]
                    if old.returncode != 1:
                        self.assertEqual(
                            (new.returncode, new.stdout, new.stderr),
                            (old.returncode, old.stdout, old.stderr),
                        )
                        continue
                    self.assertEqual(old.stdout, b"")
                    self.assertIn(b"Traceback", old.stderr)
                    kind, message = old.stderr.decode().splitlines()[-1].split(": ", 1)
                    self.assertIn(kind, ("TypeError", "KeyError"))
                    self.assertEqual(new.returncode, 1)
                    self.assertNotIn(b"Traceback", new.stdout + new.stderr)
                    if mode:
                        if mode == ["--machine"]:
                            payload = json.loads(new.stdout)["data"]
                            self.assertEqual(new.stderr, b"")
                        else:
                            payload = json.loads(new.stderr)
                            self.assertEqual(new.stdout, b"")
                        self.assertEqual(
                            payload["error"], {"type": kind, "message": message}
                        )
                    else:
                        self.assertEqual(new.stdout, b"")
                        self.assertEqual(new.stderr, f"error: {message}\n".encode())
        self.assertEqual([path.read_bytes() for path in self.paths], before)

    def test_exception_class_message_exit_and_readonly_state(self):
        cases = [
            "null",
            "[]",
            '"text"',
            "true",
            "17",
            "3.5",
            "1e3",
            "1234567890123456789012345678901234567890",
            "-0",
            "-0.0",
            "-17",
            "9223372036854775807",
            "-9223372036854775808",
            "18446744073709551616",
            "-18446744073709551617",
            '{"id":"forged"}',
            "{}",
            '{"owner":"x"}',
            '{"owner":"x","key":"y"}',
            '{"owner":"x","key":"y","state":"uncertain"}',
        ]
        for raw in cases:
            with self.subTest(raw=raw):
                self.seed(raw)
                self.assert_runtime_v2(
                    ([], ["--event", "event"], ["--event", "absent"])
                )

    def test_selected_valid_event_still_validates_the_full_aggregate(self):
        valid = '{"owner":"x","key":"y","state":"uncertain","plan_id":"p"}'
        for invalid in ("null", '{"id":"forged"}', "{}"):
            with self.subTest(invalid=invalid):
                self.seed(valid, ("later", invalid))
                self.assert_runtime_v2(([], ["--event", "event"], ["--event", "later"]))

    def test_syntax_and_error_looking_data_keep_their_existing_contract(self):
        for raw in (
            "{",
            '{"owner":"x","key":"y","state":"uncertain","plan_id":"p",'
            '"error":{"type":"TypeError","message":"forged"}}',
        ):
            self.seed(raw)
            before = [path.read_bytes() for path in self.paths]
            for mode in ([], ["--json"], ["--machine"]):
                args = mode + ["projection", "mapping-events"]
                old, new = [self.run_cli(engine, args) for engine in (0, 1)]
                self.assertEqual(
                    (new.returncode, new.stdout, new.stderr),
                    (old.returncode, old.stdout, old.stderr),
                )
            self.assertEqual([path.read_bytes() for path in self.paths], before)

    def test_machine_envelope_reports_the_same_runtime_exit_status(self):
        for raw, kind, message in (
            (
                "null",
                "TypeError",
                "dict() argument after ** must be a mapping, not NoneType",
            ),
            (
                '{"id":"forged"}',
                "TypeError",
                "dict() got multiple values for keyword argument 'id'",
            ),
            ("{}", "KeyError", "'owner'"),
        ):
            self.seed(raw)
            result = self.run_cli(1, ["--machine", "projection", "mapping-events"])
            self.assertEqual((result.returncode, result.stderr), (1, b""))
            self.assertEqual(
                json.loads(result.stdout),
                {
                    "interface_version": 1,
                    "command": "projection",
                    "operation": "mapping-events",
                    "profile": "default",
                    "outcome": "error",
                    "exit_code": 1,
                    "data": {"error": {"type": kind, "message": message}},
                },
            )
