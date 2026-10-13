# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Strict XML qualification wiring with disposable package and wheel payloads."""

import contextlib
import hashlib
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
import zipfile
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "scripts"


class XmlDriverChecks:
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.scripts = self.root / "scripts"
        self.scripts.mkdir()
        for name in (
            self.filename,
            "native_plex_metadata_embedding_tests.py",
            "native_qualification_results.py",
            "qualify_native_wheel.py",
        ):
            shutil.copyfile(SCRIPTS / name, self.scripts / name)
        spec = importlib.util.spec_from_file_location(
            "xml_driver_under_review", self.scripts / self.filename
        )
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)
        self.case = getattr(self.module, self.classname)
        self.installed = self.root / "site-packages"
        self.payloads = {
            "catabolic_native/__init__.py": b"# disposable package\n",
            "catabolic_native/_native.so": b"disposable extension",
            "catabolic_native/bin/catabolic-native": b"disposable paired helper",
            "catabolic_native/native-helper.json": b'{"disposable": true}',
            "catabolic_native/bin/catabolic-legacy-native": b"disposable legacy helper",
            "catabolic_native/legacy-helper.json": b'{"disposable": true}',
            "catabolic_native-0.2.0.dist-info/WHEEL": b"Wheel-Version: 1.0\n",
            "catabolic_native-0.2.0.dist-info/METADATA": b"Version: 0.2.0\n",
            "catabolic_native-0.2.0.dist-info/entry_points.txt": b"[console_scripts]\n",
            "catabolic_native-0.2.0.dist-info/RECORD": b"fixture record\n",
        }
        self.restore_payloads()
        self.extension = self.installed / "catabolic_native/_native.so"
        self.digest = hashlib.sha256(self.extension.read_bytes()).hexdigest()
        self.native = types.ModuleType("catabolic_native")
        self.native.__file__ = str(self.installed / "catabolic_native/__init__.py")
        self.native.__version__ = "0.2.0"
        self.native._native = types.SimpleNamespace(__file__=str(self.extension))
        self.distribution = types.SimpleNamespace(
            version="0.2.0",
            files=[Path(name) for name in self.payloads],
            locate_file=lambda name: self.installed / name,
        )
        self.wheel = self.root / "fixture.whl"
        self.write_wheel()
        self.report = self.root / "report.json"
        self.report.write_text('{"passed": true, "stale": true}')

    def restore_payloads(self):
        for name, content in self.payloads.items():
            path = self.installed / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)

    def write_wheel(self, payloads=None):
        with zipfile.ZipFile(self.wheel, "w") as archive:
            for name, content in (payloads or self.payloads).items():
                archive.writestr(name, content)

    def run_driver(self, arguments=None, *, action=None, layout=None, report=True):
        if arguments is None:
            arguments = ["--wheel", str(self.wheel)]
        argv = ["driver", *arguments]
        if report:
            argv += ["--report", str(self.report)]

        def first(case):
            if action:
                action(case)

        def suite(tests):
            cases = list(tests)
            if layout == "zero":
                cases = []
            elif layout == "missing":
                cases = cases[:1]
            elif layout == "duplicate":
                cases = [cases[0], cases[0]]
            return unittest.TestSuite(cases)

        with contextlib.ExitStack() as stack:
            stack.enter_context(
                patch.dict(sys.modules, {"catabolic_native": self.native})
            )
            stack.enter_context(patch.object(sys, "argv", argv))
            self.distribution_lookup = stack.enter_context(
                patch.object(
                    self.module.importlib.metadata,
                    "distribution",
                    return_value=self.distribution,
                )
            )
            self.wheel_verifier = stack.enter_context(
                patch.object(
                    self.module.fixture,
                    "wheel_verifier",
                    wraps=self.module.fixture.wheel_verifier,
                )
            )
            stack.enter_context(patch.object(self.case, "setUp", lambda case: None))
            stack.enter_context(
                patch.object(self.case, self.module.EXPECTED_TESTS[0], first)
            )
            stack.enter_context(
                patch.object(
                    self.case, self.module.EXPECTED_TESTS[1], lambda case: None
                )
            )
            if layout:
                stack.enter_context(
                    patch.object(
                        self.module, "unittest", types.SimpleNamespace(TestSuite=suite)
                    )
                )
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            try:
                self.module.main()
            except SystemExit as error:
                return error.code
        self.fail("driver did not return an exit status")

    def test_fixed_inventory_has_the_exact_two_declared_cases(self):
        self.assertEqual(self.module.EXPECTED_TESTS, self.expected)
        self.assertEqual(
            {name for name in self.case.__dict__ if name.startswith("test_")},
            set(self.expected),
        )

    def test_required_exclusive_mode_and_development_hash_refuse_stale_receipts(self):
        for args in (
            [],
            ["--dev"],
            ["--expected-extension-sha256", self.digest],
            ["--wheel", str(self.wheel), "--dev"],
            ["--wheel", str(self.wheel), "--expected-extension-sha256", self.digest],
            ["--wheel", str(self.wheel), "--unknown-argument"],
        ):
            with self.subTest(args=args):
                self.report.write_text('{"passed": true}')
                self.assertNotEqual(self.run_driver(args), 0)
                self.assertFalse(self.report.exists())
                self.distribution_lookup.assert_not_called()
                self.wheel_verifier.assert_not_called()

    def test_strict_wheel_checks_every_payload_twice_and_records_exact_successes(self):
        self.assertEqual(self.run_driver(), 0)
        self.assertEqual(self.wheel_verifier.call_count, 2)
        receipt = json.loads(self.report.read_text())
        self.assertFalse(receipt["development_only"])
        self.assertTrue(receipt["passed"])
        self.assertNotIn("stale", receipt)
        self.assertEqual(receipt["tests"], 2)
        self.assertEqual(
            set(receipt["successful_scenarios"]),
            {self.case(name).id() for name in self.expected},
        )
        self.assertEqual(receipt["extension_sha256"], self.digest)
        self.assertEqual(
            receipt["wheel_sha256"], hashlib.sha256(self.wheel.read_bytes()).hexdigest()
        )
        self.assertEqual(
            receipt["helper_sha256"],
            hashlib.sha256(
                self.payloads["catabolic_native/bin/catabolic-native"]
            ).hexdigest(),
        )
        self.assertEqual(
            receipt["installed_wheel_verification"]["installed_payloads_sha256"],
            {
                name: hashlib.sha256(content).hexdigest()
                for name, content in self.payloads.items()
                if not name.endswith("/RECORD")
            },
        )
        for field, filename in (
            ("script_sha256", self.filename),
            ("fixture_sha256", "native_plex_metadata_embedding_tests.py"),
            ("result_verifier_sha256", "native_qualification_results.py"),
            ("wheel_verifier_sha256", "qualify_native_wheel.py"),
        ):
            self.assertEqual(
                receipt[field],
                hashlib.sha256((self.scripts / filename).read_bytes()).hexdigest(),
            )

    def test_report_is_optional(self):
        self.report.unlink()
        self.assertEqual(self.run_driver(report=False), 0)
        self.assertFalse(self.report.exists())

    def test_explicit_development_mode_preserves_hash_invocation(self):
        self.assertEqual(
            self.run_driver(["--dev", "--expected-extension-sha256", self.digest]), 0
        )
        self.wheel_verifier.assert_not_called()
        receipt = json.loads(self.report.read_text())
        self.assertTrue(receipt["development_only"])
        for field in ("wheel_sha256", "installed_wheel_verification", "helper_sha256"):
            self.assertIsNone(receipt[field])

    def test_wrong_development_hash_refuses_stale_receipt(self):
        with self.assertRaises(AssertionError):
            self.run_driver(["--dev", "--expected-extension-sha256", "0" * 64])
        self.assertFalse(self.report.exists())

    def test_zero_missing_and_duplicate_cases_refuse_stale_receipts(self):
        for layout in ("zero", "missing", "duplicate"):
            with self.subTest(layout=layout), self.assertRaises(RuntimeError):
                self.report.write_text('{"passed": true}')
                self.run_driver(layout=layout)
            self.assertFalse(self.report.exists())

    def test_deleting_a_declared_case_does_not_shrink_the_expected_inventory(self):
        name = self.expected[0]
        original = getattr(self.case, name)
        # Remove the method after driver setup so its ordinary stubs cannot hide deletion.
        real_verifier = self.module.fixture.result_verifier

        def delete_then_verify():
            delattr(self.case, name)
            return real_verifier()

        try:
            with (
                patch.object(
                    self.module.fixture, "result_verifier", delete_then_verify
                ),
                self.assertRaises(ValueError),
            ):
                self.run_driver()
        finally:
            setattr(self.case, name, original)
        self.assertEqual(self.module.EXPECTED_TESTS, self.expected)
        self.assertFalse(self.report.exists())

    def test_unexpected_new_case_cannot_be_silently_omitted(self):
        with (
            patch.object(
                self.case, "test_unexpected_case", lambda case: None, create=True
            ),
            self.assertRaises(RuntimeError),
        ):
            self.run_driver()
        self.assertFalse(self.report.exists())

    def test_skipped_failed_and_errored_cases_refuse_stale_receipts(self):
        def error(case):
            raise RuntimeError("synthetic fixture error")

        for action in (
            lambda case: case.skipTest("synthetic skip"),
            lambda case: case.fail("synthetic failure"),
            error,
        ):
            with self.subTest(action=action), self.assertRaises(RuntimeError):
                self.report.write_text('{"passed": true}')
                self.run_driver(action=action)
            self.assertFalse(self.report.exists())

    def test_changed_or_deleted_installed_payloads_refuse_before_and_after(self):
        for name in self.payloads:
            if name.endswith("/RECORD"):
                continue
            path = self.installed / name
            for stage in ("before", "after"):
                for change in (
                    lambda path=path: path.unlink(),
                    lambda path=path: path.write_bytes(b"changed"),
                ):
                    self.restore_payloads()
                    self.report.write_text('{"passed": true}')
                    if stage == "before":
                        change()
                    with (
                        self.subTest(name=name, stage=stage, change=change),
                        self.assertRaises((AssertionError, FileNotFoundError)),
                    ):
                        self.run_driver(
                            action=(lambda case, change=change: change())
                            if stage == "after"
                            else None
                        )
                    self.assertFalse(self.report.exists())

    def test_changed_or_deleted_wheel_refuses_before_and_after(self):
        for stage in ("before", "after"):
            for change in (
                lambda: self.wheel.unlink(),
                lambda: self.wheel.write_bytes(b"changed"),
            ):
                self.write_wheel()
                self.report.write_text('{"passed": true}')
                if stage == "before":
                    change()
                with (
                    self.subTest(stage=stage, change=change),
                    self.assertRaises(
                        (AssertionError, FileNotFoundError, zipfile.BadZipFile)
                    ),
                ):
                    self.run_driver(
                        action=(lambda case, change=change: change())
                        if stage == "after"
                        else None
                    )
                self.assertFalse(self.report.exists())

    def test_even_unverified_record_bytes_cannot_change_the_supplied_archive(self):
        payloads = self.payloads | {
            "catabolic_native-0.2.0.dist-info/RECORD": b"rewritten record"
        }
        with self.assertRaisesRegex(AssertionError, "supplied wheel changed"):
            self.run_driver(action=lambda case: self.write_wheel(payloads))
        self.assertFalse(self.report.exists())

    def test_extension_identity_cannot_be_swapped_for_identical_bytes(self):
        with self.assertRaisesRegex(AssertionError, "extension identity changed"):
            self.run_driver(
                action=lambda case: setattr(
                    self.native,
                    "_native",
                    types.SimpleNamespace(__file__=str(self.extension)),
                )
            )
        self.assertFalse(self.report.exists())

    def test_extension_path_cannot_be_swapped_for_identical_bytes(self):
        other = self.root / "identical-extension.so"
        other.write_bytes(self.extension.read_bytes())
        with self.assertRaisesRegex(AssertionError, "extension path changed"):
            self.run_driver(
                action=lambda case: setattr(self.native._native, "__file__", str(other))
            )
        self.assertFalse(self.report.exists())

    def test_replacing_the_imported_package_identity_cannot_pass(self):
        replacement = types.ModuleType("catabolic_native")
        with self.assertRaisesRegex(AssertionError, "package identity changed"):
            self.run_driver(
                action=lambda case: sys.modules.__setitem__(
                    "catabolic_native", replacement
                )
            )
        self.assertFalse(self.report.exists())

    def test_changed_or_deleted_qualification_sources_refuse_stale_receipts(self):
        for filename in (
            self.filename,
            "native_plex_metadata_embedding_tests.py",
            "native_qualification_results.py",
            "qualify_native_wheel.py",
        ):
            path = self.scripts / filename
            original = path.read_bytes()
            for delete in (False, True):
                self.report.write_text('{"passed": true}')
                try:
                    with (
                        self.subTest(filename=filename, delete=delete),
                        self.assertRaises((AssertionError, FileNotFoundError)),
                    ):
                        self.run_driver(
                            action=lambda case, path=path, original=original, delete=delete: (
                                path.unlink()
                                if delete
                                else path.write_bytes(original + b"\n# changed\n")
                            )
                        )
                    self.assertFalse(self.report.exists())
                finally:
                    path.write_bytes(original)

    def test_cli_fixture_import_failure_removes_stale_receipt(self):
        fixture = self.scripts / "native_plex_metadata_embedding_tests.py"
        env = os.environ.copy()
        env.pop("PYTHONOPTIMIZE", None)
        for content, error in (
            (None, "FileNotFoundError"),
            ('raise RuntimeError("broken disposable fixture")\n', "RuntimeError"),
            ("def broken(\n", "SyntaxError"),
        ):
            if content is None:
                fixture.unlink(missing_ok=True)
            else:
                fixture.write_text(content)
            for arguments in (
                ["--wheel", str(self.wheel)],
                ["--dev", "--expected-extension-sha256", self.digest],
            ):
                with self.subTest(error=error, arguments=arguments):
                    self.report.write_text('{"passed": true, "stale": true}')
                    run = subprocess.run(
                        [
                            sys.executable,
                            "-I",
                            str(self.scripts / self.filename),
                            *arguments,
                            "--report",
                            str(self.report),
                        ],
                        env=env,
                        cwd=self.root,
                        capture_output=True,
                        text=True,
                        timeout=10,
                    )
                    self.assertNotEqual(run.returncode, 0)
                    self.assertEqual(run.stdout, "")
                    self.assertIn(error, run.stderr)
                    self.assertFalse(self.report.exists())

    def test_library_import_does_not_parse_arguments_or_clear_report(self):
        before = self.report.read_bytes()
        env = os.environ.copy()
        env.pop("PYTHONOPTIMIZE", None)
        run = subprocess.run(
            [
                sys.executable,
                "-I",
                "-c",
                "import importlib.util, sys; "
                "path, report = sys.argv[1:]; "
                "sys.argv = ['caller', '--report', report, '--report']; "
                "spec = importlib.util.spec_from_file_location('xml_library', path); "
                "module = importlib.util.module_from_spec(spec); "
                "spec.loader.exec_module(module); print('imported')",
                str(self.scripts / self.filename),
                str(self.report),
            ],
            env=env,
            cwd=self.root,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertEqual(run.stdout, "imported\n")
        self.assertEqual(self.report.read_bytes(), before)

    def test_optimized_failure_stops_shell_before_a_downstream_gate(self):
        marker = self.root / "downstream-success"
        env = os.environ.copy()
        env.pop("PYTHONOPTIMIZE", None)
        run = subprocess.run(
            [
                "bash",
                "-e",
                "-c",
                '"$1" -I -O "$2" --wheel "$3" --report "$4"; printf success > "$5"',
                "xml-qualification-shell",
                sys.executable,
                str(self.scripts / self.filename),
                str(self.wheel),
                str(self.report),
                str(marker),
            ],
            env=env,
            cwd=self.root,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(run.returncode, 0)
        self.assertEqual(run.stdout, "")
        self.assertIn("qualification requires assertions", run.stderr)
        self.assertFalse(marker.exists())
        self.assertFalse(self.report.exists())

    def test_all_optimized_entry_modes_clear_requested_receipts_before_fixture_import(
        self,
    ):
        (self.scripts / "native_plex_metadata_embedding_tests.py").write_text(
            'raise RuntimeError("fixture must not be imported")\n'
        )
        modes = (
            (["-O"], None),
            (["-OO"], None),
            ([], "1"),
            ([], "2"),
            (["-I", "-O"], None),
            (["-I", "-OO"], None),
            (["-I"], "1"),
            (["-I"], "2"),
            (["-I"], "0"),
        )
        for flags, optimize in modes:
            for args in (
                ["--help"],
                ["--wheel", str(self.wheel), "--report", str(self.report)],
                [
                    "--dev",
                    "--expected-extension-sha256",
                    self.digest,
                    "--report",
                    str(self.report),
                ],
            ):
                with self.subTest(flags=flags, optimize=optimize, args=args):
                    env = os.environ.copy()
                    env.pop("PYTHONOPTIMIZE", None)
                    if optimize is not None:
                        env["PYTHONOPTIMIZE"] = optimize
                    self.report.write_text("stale success")
                    attempt = self.report.with_suffix(".attempt.json")
                    attempt.write_text("stale attempt")
                    run = subprocess.run(
                        [
                            sys.executable,
                            "-B",
                            *flags,
                            str(self.scripts / self.filename),
                            *args,
                        ],
                        env=env,
                        cwd=self.root,
                        capture_output=True,
                        text=True,
                        timeout=10,
                    )
                    self.assertNotEqual(run.returncode, 0)
                    self.assertEqual(run.stdout, "")
                    self.assertIn("qualification requires assertions", run.stderr)
                    self.assertNotIn("fixture must not be imported", run.stderr)
                    if "--report" in args:
                        self.assertFalse(self.report.exists())
                        self.assertFalse(attempt.exists())
                    else:
                        self.assertEqual(self.report.read_text(), "stale success")
                        self.assertEqual(attempt.read_text(), "stale attempt")


class XmlEmbeddingQualificationReview(XmlDriverChecks, unittest.TestCase):
    filename = "native_plex_xml_embedding_tests.py"
    classname = "InstalledXml"
    expected = (
        "test_installed_utf16_discovery_import_metadata_and_readback",
        "test_installed_conflicting_declaration_is_recoverable_then_legacy_readback",
    )


class XmlIndependentQualificationReview(XmlDriverChecks, unittest.TestCase):
    filename = "native_plex_xml_review_tests.py"
    classname = "InstalledXmlReview"
    expected = (
        "test_installed_reserved_pi_then_namespace_reset_recovery",
        "test_installed_literal_reserved_target_and_stylesheet_controls",
    )


class XmlQualificationWorkflowReview(unittest.TestCase):
    def test_installed_python_pin_drift_is_refused(self):
        workflow = (ROOT / ".github/workflows/native-qualification.yml").read_text()
        exact = "python: ['3.11', '3.14.6']"
        self.assertEqual(workflow.count(exact), 1)
        for replacement in (
            "python: ['3.11', '3.14']",
            "python: ['3.11', '3.14.5']",
            "python: ['3.14.6']",
            "python: ['3.11']",
        ):
            with self.subTest(replacement=replacement):
                mutated = workflow.replace(exact, replacement)
                with patch.object(Path, "read_text", return_value=mutated):
                    with self.assertRaises(AssertionError):
                        self.test_both_installed_drivers_use_exact_wheel_and_upload_their_receipts()

    def test_both_installed_drivers_use_exact_wheel_and_upload_their_receipts(self):
        workflow = (ROOT / ".github/workflows/native-qualification.yml").read_text()
        installed = workflow.split("  installed:\n", 1)[1]
        self.assertIn("python: ['3.11', '3.14.6']", installed)
        self.assertIn('cd "$RUNNER_TEMP"', installed)
        for script, receipt in (
            ("native_plex_xml_embedding_tests.py", "native-plex-xml.json"),
            ("native_plex_xml_review_tests.py", "native-plex-xml-review.json"),
        ):
            command = (
                f'"$RUNNER_TEMP/native-installed/bin/python" -I "$root/scripts/{script}" '
                f'--wheel "$wheel" --report "$RUNNER_TEMP/{receipt}"'
            )
            self.assertEqual(installed.count(command), 1)
            self.assertIn("${{ runner.temp }}/" + receipt, installed)
        self.assertIn(
            'native_plex_metadata_review_tests.py" --wheel "$wheel"', installed
        )
        self.assertIn(
            'qualify_native_wheel.py" --binary "$root/dist/native/catabolic-native" --wheel "$wheel" --receipt "$RUNNER_TEMP/native-wheel-after.json"',
            installed,
        )


if __name__ == "__main__":
    unittest.main()
