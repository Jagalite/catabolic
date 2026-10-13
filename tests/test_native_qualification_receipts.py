# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Receipt/argv boundaries only: no builds, native imports, or product scenarios.

Canonical --report PATH and --report=PATH are the only opt-in forms. Every
explicit destination is invalidated before fallible CLI preflight, including
superseded occurrences. Abbreviations are rejected, and words after -- are not
options. Bootstrap copies intentionally avoid imports of fallible project code.
"""

import argparse
import ast
import contextlib
import importlib.util
import io
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
SHARED = "native_facade_qualification.py"
DRIVERS = (
    "native_asgi_installed_tests.py",
    "native_access_admin_tests.py",
    "native_private_secret_gate.py",
    "native_watchers_embedding_tests.py",
    "native_publisher_embedding_tests.py",
    "native_fallback_embedding_tests.py",
    "native_component_policy_embedding_tests.py",
    "native_catalog_refresh_embedding_tests.py",
    "native_public_import_tests.py",
    "native_plex_xml_embedding_tests.py",
    "native_plex_xml_review_tests.py",
)


def load(path):
    spec = importlib.util.spec_from_file_location(path.stem, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def parser_arguments(filename):
    if "asgi_installed" in filename:
        return [
            "--lane",
            "minimal",
            "--wheel",
            "unused",
            "--source-root",
            "unused",
            "--build-manifest",
            "unused",
            "--native-binary",
            "unused",
            "--legacy-binary",
            "unused",
            "--reference-root",
            "unused",
            "--reference-python",
            sys.executable,
        ]
    if "public_import" in filename:
        return [
            "--dev",
            "--inventory",
            "unused",
            "--reference-root",
            "unused",
            "--reference-python",
            sys.executable,
        ]
    if "plex_xml" in filename:
        return ["--dev", "--expected-extension-sha256", "unused"]
    if (
        "catalog_refresh" in filename
        or "access_admin" in filename
        or "private_secret" in filename
    ):
        return ["--reference-root", "unused", "--wheel", "unused"]
    return ["--wheel", "unused"]


class ReceiptBoundaries(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.reports = [self.root / name for name in ("first.json", "second.out")]
        self.unrequested = self.root / "unrequested.json"
        self.env = dict(os.environ)
        self.env.pop("PYTHONOPTIMIZE", None)

    def seed(self):
        for path in (*self.reports, self.unrequested):
            path.write_text("old success")
            path.with_suffix(".attempt.json").write_text("old attempt")

    def assert_receipts(self, *, removed):
        for report in (*self.reports, self.unrequested):
            for path in (report, report.with_suffix(".attempt.json")):
                if report in removed:
                    self.assertFalse(path.exists(), path)
                else:
                    self.assertEqual(
                        path.read_text(),
                        "old attempt" if path != report else "old success",
                    )

    def explicit_reports(self):
        return ["--report", str(self.reports[0]), "--report=" + str(self.reports[1])]

    def run_cli(self, filename, arguments, *, flags=(), env=None, scripts=SCRIPTS):
        return subprocess.run(
            [sys.executable, "-I", "-B", *flags, str(scripts / filename), *arguments],
            cwd=self.root,
            env=self.env if env is None else env,
            text=True,
            capture_output=True,
            timeout=10,
        )

    def test_bootstrap_copies_match_shared_function_without_project_dependencies(self):
        def scanner(filename):
            tree = ast.parse((SCRIPTS / filename).read_text())
            function = next(
                n
                for n in tree.body
                if isinstance(n, ast.FunctionDef) and n.name == "clear_stale_report"
            )
            return ast.dump(function)

        expected = scanner(SHARED)
        for filename in DRIVERS:
            with self.subTest(filename=filename):
                self.assertEqual(scanner(filename), expected)

    def test_scanner_handles_all_explicit_paths_despite_malformed_arguments(self):
        clear = load(SCRIPTS / SHARED).clear_stale_report
        for trailing in (
            ["--report"],
            ["--rep"],
            ["--unknown"],
            ["--report", "--wheel"],
            ["--report="],
            ["--wheel", "unused", "--dev-wheel", "unused"],
        ):
            with self.subTest(trailing=trailing):
                self.seed()
                clear([*self.explicit_reports(), *trailing])
                self.assert_receipts(removed=self.reports)

    def test_scanner_does_not_infer_unrequested_paths_or_consume_option_tokens(self):
        clear = load(SCRIPTS / SHARED).clear_stale_report
        for arguments in (
            [],
            ["--rep", str(self.reports[0])],
            ["--rep=" + str(self.reports[0])],
            ["--reporter", str(self.reports[0])],
            ["--wheel", str(self.reports[0])],
            ["--report", "--unknown", str(self.reports[0])],
            ["--", *self.explicit_reports()],
            ["--report", "--", *self.explicit_reports()],
        ):
            with self.subTest(arguments=arguments):
                self.seed()
                clear(arguments)
                self.assert_receipts(removed=[])
        # Option names can themselves be files, but are never implicit values.
        for name in ("--wheel", "--rep", "--report", "--unknown"):
            path = self.root / name
            path.write_text("keep")
        with contextlib.chdir(self.root):
            clear(["--report", "--wheel", "--report", "--rep", "--report", "--unknown"])
        for name in ("--wheel", "--rep", "--report", "--unknown"):
            self.assertEqual((self.root / name).read_text(), "keep")

    def test_dash_and_negative_values_match_argparse_report_values(self):
        clear = load(SCRIPTS / SHARED).clear_stale_report
        parser = argparse.ArgumentParser(allow_abbrev=False)
        parser.add_argument("--report", type=Path)
        for name, option in (
            ("-", "--report"),
            ("-1", "--report"),
            ("-.5", "--report"),
            ("-1.5", "--report"),
            ("--literal", "--report="),
        ):
            with self.subTest(name=name), contextlib.chdir(self.root):
                arguments = [option + name] if option.endswith("=") else [option, name]
                self.assertEqual(parser.parse_args(arguments).report, Path(name))
                path = Path(name)
                path.write_text("old success")
                path.with_suffix(".attempt.json").write_text("old attempt")
                clear(arguments)
                self.assertFalse(path.exists())
                self.assertFalse(path.with_suffix(".attempt.json").exists())

    def test_every_cli_clears_repeated_reports_before_malformed_trailing_options(self):
        for filename in DRIVERS:
            for trailing in (
                ["--report"],
                ["--rep"],
                ["--unknown"],
                ["--report", "--wheel"],
            ):
                for flags in ((), ("-O",), ("-OO",)):
                    with self.subTest(
                        filename=filename, trailing=trailing, flags=flags
                    ):
                        self.seed()
                        run = self.run_cli(
                            filename, [*self.explicit_reports(), *trailing], flags=flags
                        )
                        self.assertNotEqual(run.returncode, 0, run.stdout + run.stderr)
                        self.assert_receipts(removed=self.reports)

    def test_missing_required_and_mutually_exclusive_modes_clear_reports(self):
        for filename in DRIVERS:
            for arguments in (
                [],
                ["--wheel", "unused", "--dev-wheel", "unused", "--dev"],
            ):
                with self.subTest(filename=filename, arguments=arguments):
                    self.seed()
                    run = self.run_cli(filename, [*arguments, *self.explicit_reports()])
                    self.assertNotEqual(run.returncode, 0, run.stdout + run.stderr)
                    self.assert_receipts(removed=self.reports)

    def test_environment_optimization_clears_only_explicit_reports(self):
        for filename in DRIVERS:
            for optimize in ("0", "1", "2"):
                with self.subTest(filename=filename, optimize=optimize):
                    self.seed()
                    run = self.run_cli(
                        filename,
                        [*parser_arguments(filename), *self.explicit_reports()],
                        env=self.env | {"PYTHONOPTIMIZE": optimize},
                    )
                    self.assertNotEqual(run.returncode, 0, run.stdout + run.stderr)
                    self.assertIn("assertions", run.stderr)
                    self.assert_receipts(removed=self.reports)

    def test_cli_project_import_failure_cannot_retain_receipts(self):
        scripts = self.root / "scripts"
        scripts.mkdir()
        for filename in DRIVERS:
            shutil.copyfile(SCRIPTS / filename, scripts / filename)
            dependency = scripts / (
                "native_plex_metadata_embedding_tests.py"
                if "plex_xml" in filename
                else SHARED
            )
            for content, diagnostic in (
                (None, "FileNotFoundError"),
                ("def broken(\n", "SyntaxError"),
                (
                    'raise RuntimeError("broken receipt fixture")\n',
                    "broken receipt fixture",
                ),
            ):
                with self.subTest(filename=filename, diagnostic=diagnostic):
                    dependency.unlink(missing_ok=True)
                    if content is not None:
                        dependency.write_text(content)
                    self.seed()
                    run = self.run_cli(
                        filename,
                        [*self.explicit_reports(), "--report"],
                        scripts=scripts,
                    )
                    self.assertNotEqual(run.returncode, 0)
                    if "asgi_installed" in filename:
                        # This driver delays project imports until after the full
                        # parser. Malformed argv must fail even with broken helpers.
                        self.assertIn("expected one argument", run.stderr)
                    else:
                        self.assertIn(diagnostic, run.stderr)
                    self.assert_receipts(removed=self.reports)

    def test_library_import_does_not_inspect_arguments_or_touch_receipts(self):
        for filename in (SHARED, *DRIVERS):
            with self.subTest(filename=filename):
                self.seed()
                with patch.object(
                    sys, "argv", ["caller", *self.explicit_reports(), "--report"]
                ):
                    load(SCRIPTS / filename)
                self.assert_receipts(removed=[])

    def test_valid_canonical_options_reach_full_parser_and_keep_last_report(self):
        class Parsed(Exception):
            pass

        parse = argparse.ArgumentParser.parse_args

        def parsed(parser, *args, **kwargs):
            result = parse(parser, *args, **kwargs)
            self.assertEqual(result.report, self.reports[-1])
            raise Parsed

        for filename in DRIVERS:
            with self.subTest(filename=filename):
                module = load(SCRIPTS / filename)
                self.seed()
                with (
                    patch.object(
                        sys,
                        "argv",
                        ["gate", *parser_arguments(filename), *self.explicit_reports()],
                    ),
                    patch.object(argparse.ArgumentParser, "parse_args", parsed),
                    self.assertRaises(Parsed),
                ):
                    module.main()
                self.assert_receipts(removed=self.reports)

    def test_every_cli_finishes_cleanup_after_report_or_sidecar_directory_errors(self):
        for filename in DRIVERS:
            for suffix in (None, ".attempt.json"):
                for flags in ((), ("-O",)):
                    with self.subTest(filename=filename, suffix=suffix, flags=flags):
                        self.seed()
                        report = self.reports[0]
                        blocked = (
                            report if suffix is None else report.with_suffix(suffix)
                        )
                        blocked.unlink()
                        blocked.mkdir()
                        marker = blocked / "must-remain"
                        marker.write_text("not a receipt")
                        try:
                            run = self.run_cli(
                                filename,
                                [*self.explicit_reports(), "--report"],
                                flags=flags,
                            )
                            self.assertNotEqual(run.returncode, 0)
                            self.assertIn("IsADirectoryError", run.stderr)
                            self.assertIn(str(blocked), run.stderr)
                            self.assertEqual(marker.read_text(), "not a receipt")
                            for requested in self.reports:
                                for path in (
                                    requested,
                                    requested.with_suffix(".attempt.json"),
                                ):
                                    if path != blocked:
                                        self.assertFalse(path.exists(), path)
                            self.assertEqual(
                                self.unrequested.read_text(), "old success"
                            )
                            self.assertEqual(
                                self.unrequested.with_suffix(
                                    ".attempt.json"
                                ).read_text(),
                                "old attempt",
                            )
                        finally:
                            marker.unlink()
                            blocked.rmdir()

    def test_first_cleanup_error_is_raised_only_after_every_later_target(self):
        clear = load(SCRIPTS / SHARED).clear_stale_report
        self.seed()
        for report in self.reports:
            report.unlink()
            report.mkdir()
        last = self.root / "last.json"
        last.write_text("old success")
        last.with_suffix(".attempt.json").write_text("old attempt")
        with self.assertRaises(IsADirectoryError) as caught:
            clear([*self.explicit_reports(), "--report", str(last), "--report"])
        self.assertEqual(caught.exception.filename, str(self.reports[0]))
        self.assertFalse(last.exists())
        self.assertFalse(last.with_suffix(".attempt.json").exists())
        for report in self.reports:
            self.assertTrue(report.is_dir())
            self.assertFalse(report.with_suffix(".attempt.json").exists())
        self.assertEqual(self.unrequested.read_text(), "old success")

    def test_filename_lexing_matches_each_full_parser_on_both_python_versions(self):
        candidates = (
            "-1\n",
            "-1.5\n",
            "-.5\n",
            "-1suffix",
            "-0x10",
            "-file name",
            "--unknown=x y",
            "--wheel=x y",
            "-h foo",
            "--rep",
            "-not-an-option",
        )
        for filename in DRIVERS:
            module = load(SCRIPTS / filename)
            parser = module.argument_parser()
            for name in candidates:
                with self.subTest(filename=filename, name=name):
                    arguments = [*parser_arguments(filename), "--report", name]
                    try:
                        with contextlib.redirect_stderr(io.StringIO()):
                            parsed = parser.parse_args(arguments)
                    except SystemExit:
                        accepted = False
                    else:
                        self.assertEqual(parsed.report, Path(name))
                        accepted = True
                    self.seed()
                    special = self.root / name
                    special.write_text("special success")
                    sidecar = special.with_suffix(".attempt.json")
                    sidecar.write_text("special attempt")
                    run = self.run_cli(
                        filename, [*arguments, *self.explicit_reports(), "--report"]
                    )
                    self.assertNotEqual(run.returncode, 0)
                    self.assertEqual(special.exists(), not accepted)
                    self.assertEqual(sidecar.exists(), not accepted)
                    self.assert_receipts(removed=self.reports)
                    special.unlink(missing_ok=True)
                    sidecar.unlink(missing_ok=True)

    def test_full_parsers_reject_abbreviations_without_touching_their_paths(self):
        for filename in DRIVERS:
            module = load(SCRIPTS / filename)
            for option in ("--rep", "--rep=" + str(self.reports[0])):
                with self.subTest(filename=filename, option=option):
                    self.seed()
                    abbreviated = (
                        [option, str(self.reports[0])]
                        if option == "--rep"
                        else [option]
                    )
                    with (
                        patch.object(
                            sys,
                            "argv",
                            [
                                "gate",
                                *parser_arguments(filename),
                                "--report",
                                str(self.reports[1]),
                                *abbreviated,
                            ],
                        ),
                        contextlib.redirect_stderr(io.StringIO()) as stderr,
                        self.assertRaises(SystemExit) as caught,
                    ):
                        module.main()
                    self.assertEqual(caught.exception.code, 2)
                    self.assertIn("unrecognized arguments", stderr.getvalue())
                    self.assert_receipts(removed=[self.reports[1]])


if __name__ == "__main__":
    unittest.main()
