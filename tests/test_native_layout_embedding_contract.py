"""Exact selected Layouts/plans public adapter contracts; no broad safety review."""

import argparse
import ast
import contextlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/native_layout_embedding_tests.py"
FACADE = ROOT / "crates/catabolic-python/python/catabolic_native"


def gate():
    spec = importlib.util.spec_from_file_location("layout_gate_contracts", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class LayoutFacadeContracts(unittest.TestCase):
    def test_ci_requires_strict_installed_layout_gate(self):
        from tests.native_ci_contract import require_installed_gate

        workflow = (ROOT / ".github/workflows/native-qualification.yml").read_text()
        require_installed_gate(
            workflow,
            "native_layout_embedding_tests.py",
            "native-layout-embedding",
        )

    def test_exact_rows_signatures_and_fixed_scenarios(self):
        module = gate()
        rows = [
            row
            for row in json.loads((ROOT / "tests/parity/python-api.json").read_text())[
                "rows"
            ]
            if row["id"] in module.CONTRACTS
        ]
        self.assertEqual(len(rows), 9)
        self.assertEqual(len(module.EXPECTED_TESTS), 13)
        self.assertEqual(len(module.EXPECTED_GROUPS), 27)
        self.assertEqual(len(set(module.EXPECTED_TESTS)), 13)
        self.assertEqual(len(set(module.EXPECTED_GROUPS)), 27)
        for row in rows:
            names = row["id"].removeprefix("python:catabolic.").split(".")
            tree = ast.parse((FACADE / (names[0] + ".py")).read_text())
            if row["signature"]["kind"] == "module-or-value-or-reexport":
                self.assertEqual(names, ["layouts", "PRESETS"])
                continue
            owner = next(n for n in tree.body if getattr(n, "name", None) == names[1])
            if row["signature"]["kind"] != "class":
                function = (
                    next(n for n in owner.body if getattr(n, "name", None) == names[2])
                    if len(names) > 2
                    else owner
                )
                expected = (
                    ast.parse("def f(" + row["signature"]["arguments"] + "): pass")
                    .body[0]
                    .args
                )
                self.assertEqual(ast.dump(function.args), ast.dump(expected), row["id"])
            self.assertTrue(module.CONTRACTS[row["id"]]["tested"])
            self.assertTrue(module.CONTRACTS[row["id"]]["gap"])

    def test_python_adapters_forward_without_business_replica(self):
        for name in ("layouts", "plans"):
            text = (FACADE / (name + ".py")).read_text()
            for forbidden in (
                "SELECT ",
                "sqlite3",
                "hashlib",
                "execute_operation",
                "subprocess",
            ):
                self.assertNotIn(forbidden, text)
            for node in ast.walk(ast.parse(text)):
                if isinstance(node, ast.ImportFrom) and not node.level:
                    self.assertFalse((node.module or "").startswith("catabolic"))
        owner = (ROOT / "crates/catabolic-store/src/public_layouts.rs").read_text()
        self.assertIn("session.with_evaluation", owner)
        self.assertIn("layout_mutation::apply", owner)
        self.assertNotIn("Store::open", owner)
        self.assertNotIn("UPDATE mappings", owner)
        self.assertIn("progress_handler(1000, Some(poll_interrupt))", owner)
        self.assertIn("session.finish(scope, false)", owner)

    def test_presets_use_same_generated_resource(self):
        resource = json.loads(
            (ROOT / "crates/catabolic-store/resources/layouts.json").read_text()
        )
        self.assertGreater(len(resource), 3)
        self.assertIn(
            "json.loads(_native.layout_presets())", (FACADE / "layouts.py").read_text()
        )
        self.assertIn(
            'include_str!("../../catabolic-store/resources/layouts.json")',
            (ROOT / "crates/catabolic-python/src/lib.rs").read_text(),
        )
        generator = (ROOT / "scripts/migration_rust_resources.py").read_text()
        self.assertIn("from catabolic.layouts import PRESETS", generator)
        self.assertIn('generated["layouts.json"]', generator)

    def test_optimized_refusal_invalidates_old_receipts(self):
        for flag in ("-O", "-OO"):
            with self.subTest(flag=flag), tempfile.TemporaryDirectory() as directory:
                report = Path(directory) / "evidence.json"
                attempt = report.with_suffix(".attempt.json")
                report.write_text('{"passed":true}')
                attempt.write_text("{}")
                result = subprocess.run(
                    [sys.executable, flag, str(SCRIPT), "--report", str(report)],
                    capture_output=True,
                    text=True,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("do not run with -O or -OO", result.stderr)
                self.assertFalse(report.exists())
                self.assertFalse(attempt.exists())

    def test_import_does_not_remove_callers_receipts(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "caller.json"
            attempt = report.with_suffix(".attempt.json")
            report.write_text("caller report")
            attempt.write_text("caller attempt")
            with patch.object(sys, "argv", ["caller", "--report", str(report)]):
                gate()
            self.assertEqual(report.read_text(), "caller report")
            self.assertEqual(attempt.read_text(), "caller attempt")

    def test_malformed_repeated_report_options_clear_all_explicit_receipts(self):
        with tempfile.TemporaryDirectory() as directory:
            first, second = (
                Path(directory) / "first.json",
                Path(directory) / "second.json",
            )
            for path in (first, second):
                path.write_text("stale")
                path.with_suffix(".attempt.json").write_text("stale")
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--report",
                    str(first),
                    "--report=" + str(second),
                    "--report",
                ],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            for path in (first, second):
                self.assertFalse(path.exists())
                self.assertFalse(path.with_suffix(".attempt.json").exists())

    def test_build_manifest_source_and_artifact_fences(self):
        module = gate()
        artifact = {
            "wheel_sha256": "wheel",
            "package_sha256": {"_native.abi3.so": "extension"},
        }
        manifest = dict(
            profile="development",
            source_sha256="source",
            source_sha256_after="source",
            wheel_sha256="wheel",
            extension_sha256="extension",
        )
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(
                module,
                "sibling",
                return_value=types.SimpleNamespace(source_digest=lambda root: "source"),
            ),
        ):
            path = Path(directory) / "build.json"
            path.write_text(json.dumps(manifest))
            self.assertEqual(
                module.build_identity(path, artifact)["source_sha256"], "source"
            )
            with self.assertRaisesRegex(AssertionError, "requires --build-manifest"):
                module.build_identity(None, artifact)
            for key in manifest:
                with self.subTest(key=key):
                    path.write_text(json.dumps({**manifest, key: "wrong"}))
                    with self.assertRaisesRegex(
                        AssertionError, "build identity mismatch"
                    ):
                        module.build_identity(path, artifact)

    def test_gate_retains_strict_release_and_fixed_success_requirements(self):
        text = SCRIPT.read_text()
        for required in (
            "qualification.strict_artifact(",
            "qualification.inventory_rows(inventory, CONTRACTS, 9)",
            "expected_count=13",
            "EXPECTED_GROUPS, 27",
            "installed_calls.get(key, 0) > 0 and paired_calls.get(key, 0) > 0",
            "source_before == source_manifest()",
            "identity.verify()",
            "provenance.reference_imports(",
            "qualification.reference_runtime(args.reference_python)",
        ):
            self.assertIn(required, text)


class LayoutReceiptBoundaries(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.script = ROOT / "scripts/native_layout_embedding_tests.py"
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
                        self.assertIn("IsADirectoryError", run.stderr)
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

    def test_adversarial_filenames_follow_the_actual_full_parser(self):
        spec = importlib.util.spec_from_file_location(
            "layout_receipt_gate", self.script
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
            "layout_receipt_gate", self.script
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
