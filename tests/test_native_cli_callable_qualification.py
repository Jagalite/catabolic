# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fail-closed callable gate/receipt controls; no development qualification."""

import ast
import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
GATE = SCRIPTS / "native_cli_callable_tests.py"


def load(path=GATE):
    spec = importlib.util.spec_from_file_location(path.stem, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CallableQualification(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.report = self.root / "result.json"
        self.attempt = self.report.with_suffix(".attempt.json")
        self.environment = dict(os.environ)
        self.environment.pop("PYTHONOPTIMIZE", None)

    def seed(self):
        self.report.write_text("old passed report")
        self.attempt.write_text("old attempt")

    def run_gate(self, *arguments, flags=()):
        return subprocess.run(
            [sys.executable, "-I", "-B", *flags, str(GATE), *arguments],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            text=True,
            timeout=20,
        )

    def assert_cleared(self):
        self.assertFalse(self.report.exists())
        self.assertFalse(self.attempt.exists())

    def test_bootstrap_exactly_matches_shared_scanner(self):
        def scanner(path):
            return ast.dump(
                next(
                    node
                    for node in ast.parse(path.read_text()).body
                    if isinstance(node, ast.FunctionDef)
                    and node.name == "clear_stale_report"
                )
            )

        self.assertEqual(
            scanner(GATE), scanner(SCRIPTS / "native_facade_qualification.py")
        )

    def test_every_explicit_report_is_invalidated_before_invalid_arguments(self):
        other = self.root / "other.json"
        for arguments in (
            ("--dev",),
            ("--legacy-binary", "fake"),
            ("--native-binary", "fake"),
            ("--report",),
            ("--unknown",),
        ):
            self.seed()
            other.write_text("old passed report")
            other.with_suffix(".attempt.json").write_text("old attempt")
            result = self.run_gate(
                "--report", str(self.report), "--report=" + str(other), *arguments
            )
            self.assertNotEqual(result.returncode, 0)
            self.assert_cleared()
            self.assertFalse(other.exists())
            self.assertFalse(other.with_suffix(".attempt.json").exists())

    def test_optimized_mode_invalidates_before_refusal(self):
        for flag in ("-O", "-OO"):
            self.seed()
            result = self.run_gate("--report=" + str(self.report), flags=(flag,))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("requires Python assertions", result.stderr)
            self.assert_cleared()

    def test_abbreviations_and_delimiter_do_not_authorize_invalidation(self):
        for arguments in (
            ("--rep", str(self.report)),
            ("--", "--report", str(self.report)),
        ):
            self.seed()
            result = self.run_gate(*arguments)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(self.report.read_text(), "old passed report")
            self.assertEqual(self.attempt.read_text(), "old attempt")

    def test_import_never_consumes_callers_report_arguments(self):
        self.seed()
        with patch.object(sys, "argv", [str(GATE), "--report", str(self.report)]):
            gate = load()
        self.assertTrue(self.report.exists())
        self.assertTrue(self.attempt.exists())
        self.assertEqual(gate.REQUIRED_COUNT, 15)

    def test_fixed_required_cases_are_distinct_and_no_skips(self):
        gate = load()
        names = unittest.defaultTestLoader.getTestCaseNames(gate.CallableCases)
        self.assertEqual(set(names), set(gate.EXPECTED_TESTS))
        self.assertEqual(len(names), gate.REQUIRED_COUNT)
        self.assertEqual(len(gate.EXPECTED_TESTS), len(set(gate.EXPECTED_TESTS)))
        self.assertEqual(
            set(gate.EXPECTED_GROUPS), {name.removeprefix("test_") for name in names}
        )
        for name in names:
            self.assertFalse(
                getattr(getattr(gate.CallableCases, name), "__unittest_skip__", False)
            )

    def test_missing_artifacts_cannot_leave_success_receipt(self):
        self.seed()
        arguments = []
        for name in (
            "reference-root",
            "reference-python",
            "inventory",
            "source-root",
            "wheel",
            "build-manifest",
        ):
            arguments += ["--" + name, str(self.root / "missing")]
        result = self.run_gate(*arguments, "--report", str(self.report))
        self.assertNotEqual(result.returncode, 0)
        self.assert_cleared()

    def test_order_normalizer_preserves_cross_stream_transitions(self):
        gate = load()
        events = [
            ["stderr", "write", "progress"],
            ["stderr", "write", "\n"],
            ["stderr", "flush"],
            ["stdout", "write", "done"],
            ["stderr", "write", "later"],
        ]
        self.assertEqual(
            gate.ordered_segments(events),
            [["stderr", "progress\n"], ["stdout", "done"], ["stderr", "later"]],
        )


if __name__ == "__main__":
    unittest.main()
