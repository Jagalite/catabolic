# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen public shape and non-publication boundaries for Reconciler."""

import argparse
import ast
import contextlib
import errno
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "crates/catabolic-python/python/catabolic_native/reconcile.py"
PREFIX = "python:catabolic.reconcile.Reconciler"
METHODS = ("__init__", "owner", "catalogs", "preview", "verify", "apply", "recover")
IDS = {PREFIX, *(PREFIX + "." + method for method in METHODS)}


class ReconcilerContract(unittest.TestCase):
    def test_ci_requires_strict_installed_reconciler_gate(self):
        from tests.native_ci_contract import require_installed_gate

        workflow = (ROOT / ".github/workflows/native-qualification.yml").read_text()
        require_installed_gate(
            workflow,
            "native_reconciler_embedding_tests.py",
            "native-reconciler-embedding",
        )

    def test_exact_eight_preserved_rows_and_signatures(self):
        inventory = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        rows = [row for row in inventory["rows"] if row["id"] in IDS]
        self.assertEqual(len(rows), 8)
        self.assertEqual({row["id"] for row in rows}, IDS)
        rows = {row["id"]: row for row in rows}
        cls = next(
            node
            for node in ast.parse(SOURCE.read_text()).body
            if isinstance(node, ast.ClassDef)
        )
        methods = [node for node in cls.body if isinstance(node, ast.FunctionDef)]
        self.assertEqual({node.name for node in methods}, set(METHODS))
        for method in methods:
            self.assertEqual(
                ast.unparse(method.args),
                rows[PREFIX + "." + method.name]["signature"]["arguments"],
            )

    def test_persistent_adapter_no_python_engine_or_public_bypass(self):
        source = SOURCE.read_text()
        for forbidden in (
            "execute_operation",
            "Store(",
            "disposable_fixture",
            "sqlite3",
            "SELECT ",
            "INSERT ",
            "UPDATE ",
            "DELETE ",
            "subprocess",
        ):
            self.assertNotIn(forbidden, source)
        self.assertIn(
            "from .reconcile import Reconciler as Reconciler",
            SOURCE.with_name("__init__.py").read_text(),
        )
        owner = (ROOT / "crates/catabolic-store/src/public_reconcile.rs").read_text()
        self.assertIn('matches!(operation, "apply" | "recover")', owner)
        self.assertIn("return Err(Error(PUBLICATION_GATE.into()))", owner)
        self.assertIn(
            "filesystem_mutation::execute(store, profile, operation, &request)", owner
        )
        self.assertNotIn("execute_mut", owner)

    def test_apply_and_recover_use_the_shared_observed_executor(self):
        # Track C3: publication goes through one native entry point with the
        # frozen after_filesystem callback, never through the read dispatcher.
        source = SOURCE.read_text()
        self.assertIn("reconcile_publish(", source)
        self.assertEqual(source.count('_publish(self, "apply"'), 1)
        self.assertEqual(source.count('_publish(self, "recover"'), 1)
        owner = (ROOT / "crates/catabolic-store/src/public_reconcile.rs").read_text()
        self.assertIn("filesystem_mutation::execute_observed(", owner)
        publish = owner[owner.index("pub fn publish(") : owner.index("pub fn call(")]
        for private in ("disposable_fixture", "failpoint", "crash"):
            self.assertNotIn(f'"{private}"', publish)

    def test_qualification_rejects_optimized_python(self):
        result = subprocess.run(
            [
                sys.executable,
                "-O",
                str(ROOT / "scripts/native_reconciler_embedding_tests.py"),
                "--help",
            ],
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires Python assertions", result.stderr)


class ReconcilerReceiptBoundaries(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.script = ROOT / "scripts/native_reconciler_embedding_tests.py"
        self.reports = [self.root / name for name in ("first.json", "second.out")]
        self.unrequested = self.root / "unrequested.json"
        self.arguments = [
            "--report",
            str(self.reports[0]),
            "--report=" + str(self.reports[1]),
        ]
        self.env = dict(os.environ)
        self.env.pop("PYTHONOPTIMIZE", None)

    def seed(self):
        for report in (*self.reports, self.unrequested):
            report.write_text("old success")
            report.with_suffix(".attempt.json").write_text("old attempt")

    def assert_receipts(self, removed):
        for report in (*self.reports, self.unrequested):
            for path in (report, report.with_suffix(".attempt.json")):
                if report in removed:
                    self.assertFalse(path.exists(), path)
                else:
                    self.assertEqual(
                        path.read_text(),
                        "old success" if path == report else "old attempt",
                    )

    def run_gate(self, arguments, *, flags=(), script=None, optimize=None):
        env = self.env | ({"PYTHONOPTIMIZE": optimize} if optimize is not None else {})
        return subprocess.run(
            [
                sys.executable,
                "-I",
                "-B",
                *flags,
                str(script or self.script),
                *arguments,
            ],
            cwd=self.root,
            env=env,
            text=True,
            capture_output=True,
            timeout=10,
        )

    def test_malformed_or_optimized_attempt_clears_every_explicit_destination(self):
        for trailing in (
            [],
            ["--report"],
            ["--rep"],
            ["--unknown"],
            ["--report", "--wheel"],
        ):
            for flags, optimize in (
                ((), None),
                (("-O",), None),
                (("-OO",), None),
                ((), "0"),
                ((), "1"),
            ):
                with self.subTest(trailing=trailing, flags=flags, optimize=optimize):
                    self.seed()
                    run = self.run_gate(
                        [*self.arguments, *trailing], flags=flags, optimize=optimize
                    )
                    self.assertNotEqual(run.returncode, 0, run.stdout + run.stderr)
                    self.assert_receipts(self.reports)

    def test_failed_shared_helper_import_still_invalidates_receipts(self):
        script = self.root / self.script.name
        shutil.copyfile(self.script, script)
        helper = self.root / "native_facade_qualification.py"
        for content, diagnostic in (
            (None, "FileNotFoundError"),
            ("def broken(\n", "SyntaxError"),
            (
                'raise RuntimeError("broken receipt fixture")\n',
                "broken receipt fixture",
            ),
        ):
            with self.subTest(diagnostic=diagnostic):
                helper.unlink(missing_ok=True)
                if content is not None:
                    helper.write_text(content)
                self.seed()
                run = self.run_gate(self.arguments, script=script)
                self.assertNotEqual(run.returncode, 0)
                self.assertIn(diagnostic, run.stderr)
                self.assert_receipts(self.reports)

    def test_cleanup_errors_preserve_directories_and_clear_later_receipts(self):
        for suffix in (None, ".attempt.json"):
            for flags in ((), ("-O",)):
                with self.subTest(suffix=suffix, flags=flags):
                    self.seed()
                    blocked = (
                        self.reports[0]
                        if suffix is None
                        else self.reports[0].with_suffix(suffix)
                    )
                    blocked.unlink()
                    blocked.mkdir()
                    marker = blocked / "must-remain"
                    marker.write_text("not a receipt")
                    try:
                        run = self.run_gate([*self.arguments, "--report"], flags=flags)
                        self.assertNotEqual(run.returncode, 0)
                        # Directory unlink uses EISDIR on Linux and EPERM on macOS.
                        self.assertRegex(
                            run.stderr,
                            rf"(?m)^(IsADirectoryError: \[Errno {errno.EISDIR}\]|"
                            rf"PermissionError: \[Errno {errno.EPERM}\])",
                        )
                        self.assertIn(str(blocked), run.stderr)
                        self.assertEqual(marker.read_text(), "not a receipt")
                        for report in self.reports:
                            for path in (report, report.with_suffix(".attempt.json")):
                                if path != blocked:
                                    self.assertFalse(path.exists(), path)
                        self.assertEqual(self.unrequested.read_text(), "old success")
                    finally:
                        marker.unlink()
                        blocked.rmdir()

    def test_first_cleanup_error_is_preserved_after_all_later_targets(self):
        spec = importlib.util.spec_from_file_location("receipt_error_gate", self.script)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        unlink = Path.unlink
        for numbers in ((errno.EISDIR, errno.EPERM), (errno.EPERM, errno.EISDIR)):
            with self.subTest(first_errno=numbers[0]):
                self.seed()
                errors = {
                    report: OSError(number, os.strerror(number), str(report))
                    for report, number in zip(self.reports, numbers, strict=True)
                }

                def fail_blocked_paths(path, *args, errors=errors, **kwargs):
                    if path in errors:
                        raise errors[path]
                    return unlink(path, *args, **kwargs)

                last = self.root / "last.json"
                last.write_text("old success")
                last.with_suffix(".attempt.json").write_text("old attempt")
                with (
                    patch.object(Path, "unlink", fail_blocked_paths),
                    self.assertRaises(OSError) as caught,
                ):
                    module.clear_stale_report(
                        [*self.arguments, "--report", str(last)],
                        parser=module.argument_parser(),
                    )
                self.assertIs(caught.exception, errors[self.reports[0]])
                for report in self.reports:
                    self.assertEqual(report.read_text(), "old success")
                    self.assertFalse(report.with_suffix(".attempt.json").exists())
                self.assertFalse(last.exists())
                self.assertFalse(last.with_suffix(".attempt.json").exists())
                self.assertEqual(self.unrequested.read_text(), "old success")
                self.assertEqual(
                    self.unrequested.with_suffix(".attempt.json").read_text(),
                    "old attempt",
                )

    def test_adversarial_filenames_follow_the_actual_full_parser(self):
        spec = importlib.util.spec_from_file_location(
            "reconciler_receipt_gate", self.script
        )
        gate = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(gate)
        parser = gate.argument_parser()
        for name in (
            "-1\n",
            "-1suffix",
            "-.5\n",
            "-file name",
            "--unknown=x y",
            "--wheel=x y",
            "-h foo",
        ):
            with self.subTest(name=name):
                arguments = ["--report", name]
                try:
                    with contextlib.redirect_stderr(io.StringIO()):
                        parsed = parser.parse_args(arguments)
                except SystemExit:
                    accepted = False
                else:
                    self.assertEqual(parsed.report, Path(name))
                    accepted = True
                self.seed()
                report = self.root / name
                attempt = report.with_suffix(".attempt.json")
                report.write_text("special success")
                attempt.write_text("special attempt")
                run = self.run_gate([*arguments, *self.arguments, "--report"])
                self.assertNotEqual(run.returncode, 0)
                self.assertEqual(report.exists(), not accepted)
                self.assertEqual(attempt.exists(), not accepted)
                self.assert_receipts(self.reports)
                report.unlink(missing_ok=True)
                attempt.unlink(missing_ok=True)

    def test_library_import_is_effect_free_and_valid_canonical_options_parse(self):
        self.seed()
        spec = importlib.util.spec_from_file_location(
            "reconciler_receipt_gate", self.script
        )
        gate = importlib.util.module_from_spec(spec)
        with patch.object(sys, "argv", ["caller", *self.arguments, "--report"]):
            spec.loader.exec_module(gate)
        self.assert_receipts([])
        original = argparse.ArgumentParser.parse_args

        class Parsed(Exception):
            pass

        def parsed(parser, *args, **kwargs):
            result = original(parser, *args, **kwargs)
            self.assertEqual(result.report, self.reports[-1])
            raise Parsed

        with (
            patch.object(sys, "argv", ["gate", *self.arguments]),
            patch.object(argparse.ArgumentParser, "parse_args", parsed),
            self.assertRaises(Parsed),
        ):
            gate.main()
        self.assert_receipts(self.reports)
        self.seed()
        with (
            patch.object(
                sys,
                "argv",
                [
                    "gate",
                    "--report",
                    str(self.reports[1]),
                    "--rep",
                    str(self.reports[0]),
                ],
            ),
            contextlib.redirect_stderr(io.StringIO()) as stderr,
            self.assertRaises(SystemExit) as caught,
        ):
            gate.main()
        self.assertEqual(caught.exception.code, 2)
        self.assertIn("unrecognized arguments", stderr.getvalue())
        self.assert_receipts([self.reports[1]])


if __name__ == "__main__":
    unittest.main()
