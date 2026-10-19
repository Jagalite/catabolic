# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent deterministic qualification fences; no native/product execution."""

import ast
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from tests import test_rust_artifact_build as artifact_support

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "scripts"


def load(name):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


RESULTS = load("native_qualification_results")
METADATA = load("native_plex_metadata_review_tests")


class QualificationResultReview(unittest.TestCase):
    def run_fixture(self, behavior="pass", layout="normal"):
        class Fixture(unittest.TestCase):
            def test_one(self):
                if behavior == "skip":
                    self.skipTest("synthetic skip")
                if behavior == "failure":
                    self.fail("synthetic failure")
                if behavior == "error":
                    raise ValueError("synthetic error")
                if behavior == "subtest":
                    with self.subTest(case="synthetic"):
                        self.fail("synthetic subtest failure")

            def test_two(self):
                pass

            def test_foreign(self):
                pass

        if behavior == "expected_failure":
            Fixture.test_one = unittest.expectedFailure(
                lambda self: self.fail("synthetic expected failure")
            )
        if behavior == "unexpected_success":
            Fixture.test_one = unittest.expectedFailure(lambda self: None)
        if behavior == "class_skip":
            Fixture = unittest.skip("synthetic class skip")(Fixture)
        names = {
            "normal": ["test_one", "test_two"],
            "empty": [],
            "missing": ["test_one"],
            "duplicate": ["test_one", "test_one"],
            "foreign": ["test_one", "test_foreign"],
        }[layout]
        suite = unittest.TestSuite(Fixture(name) for name in names)
        with contextlib.redirect_stderr(io.StringIO()):
            return RESULTS.run_required_suite(
                suite, {Fixture: ("test_one", "test_two")}, expected_count=2
            )

    def test_exact_successful_distinct_cases_pass(self):
        result = self.run_fixture()
        self.assertEqual(result.testsRun, 2)
        self.assertEqual(len(set(result.successful_scenarios)), 2)

    def test_skips_failures_errors_and_expected_failures_cannot_pass(self):
        for behavior in (
            "skip",
            "failure",
            "error",
            "subtest",
            "expected_failure",
            "unexpected_success",
            "class_skip",
        ):
            with self.subTest(behavior=behavior), self.assertRaises(RuntimeError):
                self.run_fixture(behavior)

    def test_empty_missing_duplicate_and_foreign_cases_cannot_pass(self):
        for layout in ("empty", "missing", "duplicate", "foreign"):
            with self.subTest(layout=layout), self.assertRaises(RuntimeError):
                self.run_fixture(layout=layout)

    def test_swallowed_suite_execution_cannot_pass(self):
        class Fixture(unittest.TestCase):
            def test_one(self):
                pass

        class Swallowed(unittest.TestSuite):
            def run(self, result, debug=False):
                return result

        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(RuntimeError):
            RESULTS.run_required_suite(
                Swallowed([Fixture("test_one")]),
                {Fixture: ("test_one",)},
                expected_count=1,
            )

    def test_all_installed_gate_inventories_are_explicit_and_exact(self):
        for filename, classname, count in (
            ("native_query_embedding_tests", "InstalledQueries", 10),
            ("native_processing_embedding_tests", "InstalledProcessing", 10),
            (
                "native_plex_import_embedding_tests",
                "InstalledPlexImportCancellation",
                2,
            ),
            (
                "native_plex_matching_embedding_tests",
                "InstalledPlexMatchingCancellation",
                2,
            ),
            ("native_plex_metadata_embedding_tests", "InstalledPlexMetadata", 4),
        ):
            with self.subTest(filename=filename):
                module = load(filename)
                cls = getattr(module, classname)
                self.assertEqual(len(module.EXPECTED_TESTS), count)
                self.assertEqual(
                    set(unittest.defaultTestLoader.getTestCaseNames(cls)),
                    set(module.EXPECTED_TESTS),
                )
                self.assertIn(
                    "native_qualification_results",
                    (SCRIPTS / (filename + ".py")).read_text(),
                )
        baseline = load("qualify_native_embedding")
        self.assertEqual(sum(map(len, baseline.EXPECTED_TESTS.values())), 19)
        for filename, classname in (
            ("qualify_native_embedding.py", "InstalledFacade"),
            ("native_embedding_lifecycle_tests.py", "ReviewLifecycle"),
            ("native_workflow_embedding_tests.py", "WorkflowLifecycle"),
        ):
            tree = ast.parse((SCRIPTS / filename).read_text())
            cls = next(
                node
                for node in tree.body
                if isinstance(node, ast.ClassDef) and node.name == classname
            )
            self.assertEqual(
                {
                    node.name
                    for node in cls.body
                    if isinstance(node, ast.FunctionDef)
                    and node.name.startswith("test_")
                },
                set(baseline.EXPECTED_TESTS[classname]),
            )


class MetadataReviewDriverReview(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        package = self.root / "site-packages/catabolic_native"
        package.mkdir(parents=True)
        self.extension = package / "_native.so"
        self.extension.write_bytes(b"disposable extension")
        self.digest = hashlib.sha256(self.extension.read_bytes()).hexdigest()
        self.native = types.ModuleType("catabolic_native")
        self.native.__file__ = str(package / "__init__.py")
        self.native._native = types.SimpleNamespace(__file__=str(self.extension))
        self.wheel = self.root / "fixture.whl"
        self.wheel.write_bytes(b"disposable wheel")
        self.report = self.root / "report.json"
        self.report.write_text(json.dumps({"passed": True, "stale": True}))

    def run_driver(self, arguments, action=None, verifier=None):
        def first(case):
            if action:
                action(case)

        methods = METADATA.EXPECTED_TESTS
        with contextlib.ExitStack() as stack:
            stack.enter_context(
                patch.dict(sys.modules, {"catabolic_native": self.native})
            )
            stack.enter_context(
                patch.object(
                    sys, "argv", ["driver", "--report", str(self.report), *arguments]
                )
            )
            stack.enter_context(
                patch.object(
                    METADATA.InstalledMetadataReview, "setUp", lambda case: None
                )
            )
            stack.enter_context(
                patch.object(METADATA.InstalledMetadataReview, methods[0], first)
            )
            stack.enter_context(
                patch.object(
                    METADATA.InstalledMetadataReview, methods[1], lambda case: None
                )
            )
            verify = stack.enter_context(
                patch.object(
                    METADATA.fixture,
                    "wheel_verifier",
                    return_value=verifier or (lambda path: {"verified": True}),
                )
            )
            stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            try:
                METADATA.main()
            except SystemExit as error:
                return error.code, verify.call_count
        self.fail("driver did not return an exit status")

    def test_mode_is_explicit_and_development_hash_is_required(self):
        for args in (
            [],
            ["--dev"],
            ["--expected-extension-sha256", self.digest],
            ["--wheel", str(self.wheel), "--dev"],
            ["--wheel", str(self.wheel), "--expected-extension-sha256", self.digest],
        ):
            with self.subTest(args=args):
                code, _ = self.run_driver(args)
                self.assertNotEqual(code, 0)

    def test_strict_wheel_has_two_exact_successes_and_checks_before_after(self):
        code, checks = self.run_driver(["--wheel", str(self.wheel)])
        self.assertEqual(code, 0)
        self.assertEqual(checks, 2)
        report = json.loads(self.report.read_text())
        self.assertFalse(report["development_only"])
        self.assertTrue(report["passed"])
        self.assertEqual(report["tests"], 2)
        self.assertEqual(len(set(report["successful_scenarios"])), 2)
        self.assertEqual(
            report["result_verifier_sha256"],
            hashlib.sha256(
                (SCRIPTS / "native_qualification_results.py").read_bytes()
            ).hexdigest(),
        )

    def test_explicit_development_mode_requires_matching_hash(self):
        args = ["--dev", "--expected-extension-sha256", self.digest]
        code, checks = self.run_driver(args)
        self.assertEqual((code, checks), (0, 0))
        self.assertTrue(json.loads(self.report.read_text())["development_only"])
        with self.assertRaises(AssertionError):
            self.run_driver(["--dev", "--expected-extension-sha256", "0" * 64])
        self.assertFalse(self.report.exists())

    def test_skipped_review_cannot_leave_passing_receipt(self):
        with self.assertRaises(RuntimeError):
            self.run_driver(
                ["--wheel", str(self.wheel)],
                action=lambda case: case.skipTest("synthetic skip"),
            )
        self.assertFalse(self.report.exists())

    def test_missing_and_changed_extension_cannot_leave_passing_receipt(self):
        for change in (
            lambda: self.extension.unlink(),
            lambda: self.extension.write_bytes(b"changed"),
        ):
            self.extension.write_bytes(b"disposable extension")
            self.report.write_text('{"passed":true}')
            with (
                self.subTest(change=change),
                self.assertRaises((AssertionError, FileNotFoundError)),
            ):
                self.run_driver(
                    ["--wheel", str(self.wheel)],
                    action=lambda case, change=change: change(),
                )
            self.assertFalse(self.report.exists())

    def test_failed_wheel_verification_cannot_leave_passing_receipt(self):
        def refuse(path):
            raise AssertionError("synthetic missing or swapped wheel")

        with self.assertRaises(AssertionError):
            self.run_driver(["--wheel", str(self.wheel)], verifier=refuse)
        self.assertFalse(self.report.exists())


class MutationBindingReview(unittest.TestCase):
    def setUp(self):
        self.support = artifact_support.NativeArtifactBuildTest()
        self.support.setUp()
        self.addCleanup(self.support.doCleanups)

    def test_extension_missing_before_or_after_clears_old_receipt(self):
        for stage in ("prepare", "action"):
            with self.subTest(stage=stage), self.assertRaises(FileNotFoundError):
                self.support.run_legacy_qualification_fixture(
                    {"native_extension"},
                    "tests.test_rust_plex_login.Fixture.test_publication_interrupt_cleanup_cli_and_python",
                    **{stage: lambda artifacts: artifacts["native_extension"].unlink()},
                )
            report = json.loads((self.support.root / "receipt.json").read_text())
            self.assertFalse(report["passed_without_skips"])

    def test_skip_and_generated_source_drift_cannot_pass(self):
        def skip(artifacts):
            raise unittest.SkipTest("synthetic skip")

        def drift(artifacts):
            path = self.support.root / "crates/catabolic-cli/resources/legacy-cli.json"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("changed generated parser")

        for action in (skip, drift):
            with self.subTest(action=action):
                status = self.support.run_legacy_qualification_fixture(
                    {"legacy_cli"},
                    "tests.test_rust_native_events_independent.Fixture.test_case",
                    action=action,
                )
                self.assertEqual(status, 1)
                self.assertFalse(
                    json.loads((self.support.root / "receipt.json").read_text())[
                        "passed_without_skips"
                    ]
                )

    def test_ambient_extension_is_removed_before_discovery(self):
        def discover(artifacts):
            self.assertNotIn("CATABOLIC_NATIVE_EXTENSION", os.environ)

        with (
            patch.dict(os.environ, {"CATABOLIC_NATIVE_EXTENSION": "/ambient/stale.so"}),
            self.assertRaises(SystemExit),
        ):
            self.support.run_legacy_qualification_fixture(
                set(),
                "tests.test_rust_plex_login.Fixture.test_publication_interrupt_cleanup_cli_and_python",
                discover=discover,
            )


class QueryOptimizationReview(unittest.TestCase):
    def test_both_optimized_flags_and_environment_refuse_all_entrypoints(self):
        for flags, value in ((["-O"], None), (["-OO"], None), ([], "1"), ([], "2")):
            for scenario in (None, "native", "reference"):
                with self.subTest(flags=flags, value=value, scenario=scenario):
                    arguments = [
                        sys.executable,
                        *flags,
                        str(SCRIPTS / "native_query_embedding_tests.py"),
                    ]
                    arguments += ["--scenario", scenario] if scenario else ["--help"]
                    env = os.environ.copy()
                    env.pop("PYTHONOPTIMIZE", None)
                    if value:
                        env["PYTHONOPTIMIZE"] = value
                    result = subprocess.run(
                        arguments, capture_output=True, text=True, env=env, timeout=10
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
                    self.assertIn(
                        "query qualification requires assertions", result.stderr
                    )


class InstalledGateDriverReview(unittest.TestCase):
    def stub_methods(self, stack, case, methods, skip=False):
        stack.enter_context(patch.object(case, "setUp", lambda self: None))
        for index, name in enumerate(methods):

            def action(self, index=index):
                if skip and index == 0:
                    self.skipTest("synthetic installed-gate skip")

            stack.enter_context(patch.object(case, name, action))

    def test_query_and_processing_driver_skip_stops_before_native_or_parity_work(self):
        for filename, classname in (
            ("native_query_embedding_tests", "InstalledQueries"),
            ("native_processing_embedding_tests", "InstalledProcessing"),
        ):
            with (
                self.subTest(filename=filename),
                tempfile.TemporaryDirectory() as directory,
            ):
                module = load(filename)
                root = Path(directory)
                report = root / "report.json"
                report.write_text('{"passed":true}')
                inventory = root / "inventory.json"
                inventory.write_text(
                    json.dumps(
                        {
                            "reference_commit": "fixture",
                            "rows": [{"id": key} for key in module.CONTRACTS],
                        }
                    )
                )
                provenance = types.SimpleNamespace(
                    reference_provenance=lambda *args: {}
                )
                with contextlib.ExitStack() as stack:
                    stack.enter_context(
                        patch.object(
                            sys,
                            "argv",
                            [
                                "driver",
                                "--dev",
                                "--inventory",
                                str(inventory),
                                "--reference-root",
                                str(root),
                                "--reference-python",
                                sys.executable,
                                "--report",
                                str(report),
                            ],
                        )
                    )
                    stack.enter_context(
                        patch.object(
                            module,
                            "sibling",
                            side_effect=lambda name, provenance=provenance: (
                                RESULTS
                                if name == "native_qualification_results"
                                else provenance
                            ),
                        )
                    )
                    stack.enter_context(patch.object(module, "trace", return_value={}))
                    self.stub_methods(
                        stack,
                        getattr(module, classname),
                        module.EXPECTED_TESTS,
                        skip=True,
                    )
                    stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
                    with self.assertRaises(RuntimeError):
                        module.main()
                self.assertFalse(report.exists())

    def test_baseline_and_plex_driver_skips_cannot_leave_receipts(self):
        for filename, classname in (
            ("qualify_native_embedding", "InstalledFacade"),
            ("native_plex_import_embedding_tests", "InstalledPlexImportCancellation"),
            (
                "native_plex_matching_embedding_tests",
                "InstalledPlexMatchingCancellation",
            ),
            ("native_plex_metadata_embedding_tests", "InstalledPlexMetadata"),
        ):
            with (
                self.subTest(filename=filename),
                tempfile.TemporaryDirectory() as directory,
            ):
                module = load(filename)
                root = Path(directory)
                report = root / "report.json"
                report.write_text('{"passed":true}')
                native = types.ModuleType("catabolic_native")
                native.__file__ = str(
                    root / "site-packages/catabolic_native/__init__.py"
                )
                with contextlib.ExitStack() as stack:
                    stack.enter_context(
                        patch.dict(sys.modules, {"catabolic_native": native})
                    )
                    stack.enter_context(
                        patch.object(sys, "argv", ["driver", "--report", str(report)])
                    )
                    stack.enter_context(
                        patch.object(
                            module.importlib.metadata,
                            "distribution",
                            return_value=types.SimpleNamespace(
                                locate_file=lambda path, root=root: root
                            ),
                        )
                    )
                    if filename == "qualify_native_embedding":
                        modules = {"native_qualification_results": RESULTS}
                        for sibling, clsname in (
                            ("native_embedding_lifecycle_tests", "ReviewLifecycle"),
                            ("native_workflow_embedding_tests", "WorkflowLifecycle"),
                        ):
                            case = type(
                                clsname,
                                (unittest.TestCase,),
                                {
                                    name: lambda self: None
                                    for name in module.EXPECTED_TESTS[clsname]
                                },
                            )
                            modules[sibling] = types.SimpleNamespace(**{clsname: case})
                        stack.enter_context(
                            patch.object(
                                module,
                                "sibling_module",
                                side_effect=modules.__getitem__,
                            )
                        )
                        stack.enter_context(
                            patch.object(module, "trace_native_facade", return_value={})
                        )
                        methods = module.EXPECTED_TESTS[classname]
                    else:
                        methods = module.EXPECTED_TESTS
                    self.stub_methods(
                        stack, getattr(module, classname), methods, skip=True
                    )
                    stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
                    with self.assertRaises(RuntimeError):
                        module.main()
                self.assertFalse(report.exists())


class QualificationShellReview(unittest.TestCase):
    def run_shell(self, platform, failure=None, script_failure=None):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        checkout = root / "checkout"
        (checkout / "scripts").mkdir(parents=True)
        script = checkout / "scripts/check-native-mutations.sh"
        script.write_bytes((SCRIPTS / script.name).read_bytes())
        reference = root / "external frozen reference"
        (reference / "src/catabolic").mkdir(parents=True)
        fixture = (
            checkout
            / ".local-tests/rust-migration/m0-completion/legacy-fixtures/schema-32/before.sqlite3"
        )
        fixture.parent.mkdir(parents=True)
        fixture.touch()
        target = root / "custom cargo target"
        for filename in (
            "debug/catabolic-native",
            "debug/catabolic-http-experimental",
            "debug/catabolic-legacy-native",
            "debug/examples/legacy_parse",
            "debug/catabolic-verification",
            "release/catabolic-native",
            "debug/lib_native.so",
            "debug/lib_native.dylib",
        ):
            path = target / filename
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                "#!/bin/sh\nexit 0\n" if "lib_native" not in filename else filename
            )
            path.chmod(0o700)
        tools = root / "tools"
        tools.mkdir()
        stub = tools / "stub"
        stub.write_text(
            f"#!{sys.executable}\n"
            "import json,os,subprocess,sys\nfrom pathlib import Path\n"
            "tool=Path(sys.argv[0]).name;args=sys.argv[1:]\n"
            "with open(os.environ['STUB_LOG'],'a') as f:f.write(json.dumps([tool,args])+ '\\n')\n"
            "if tool=='python' and args and args[0]=='scripts/generate_plex_xml_encodings.py' and os.environ.get('PYTHONPATH')!=os.environ['CATABOLIC_PYTHON_REFERENCE']:\n"
            " print('XML generator did not receive frozen imports',file=sys.stderr);raise SystemExit(1)\n"
            "if tool=='python' and args and args[0]==os.environ.get('STUB_SCRIPT_FAILURE'):\n"
            " print('generated resource drift',file=sys.stderr);raise SystemExit(1)\n"
            "if tool=='cargo' and args[0]=='metadata':print(json.dumps({'target_directory':os.environ['CARGO_TARGET_DIR']}))\n"
            "elif tool=='cargo' and '--ignored' in args:\n"
            " name=args[args.index('--')-1];fail=os.environ.get('STUB_FAILURE','')\n"
            " if '--list' in args:\n"
            "  lines=[] if fail=='missing' else [name+': test'] * (2 if fail=='duplicate' else 1)\n"
            "  print('\\n'.join(lines))\n"
            " else:print('test result: ok. '+('0' if fail=='zero' else '1')+' passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s')\n"
            "elif tool=='uname':print(os.environ['STUB_PLATFORM'])\n"
            "elif tool=='python' and args and args[0]=='-c':raise SystemExit(subprocess.call([sys.executable,*args]))\n"
        )
        stub.chmod(0o700)
        for name in (
            "cargo",
            "python",
            "uname",
            "ffmpeg",
            "ffprobe",
            "pdftotext",
            "tesseract",
        ):
            (tools / name).symlink_to(stub)
        log = root / "calls.jsonl"
        env = os.environ | {
            "PATH": str(tools) + os.pathsep + os.environ["PATH"],
            "CARGO_TARGET_DIR": str(target),
            "CATABOLIC_QUALIFICATION_PYTHON": str(tools / "python"),
            "CATABOLIC_PYTHON_REFERENCE": str(reference / "src"),
            "STUB_LOG": str(log),
            "STUB_PLATFORM": platform,
            "STUB_FAILURE": failure or "",
            "STUB_SCRIPT_FAILURE": script_failure or "",
        }
        run = subprocess.run(
            ["sh", str(script)],
            env=env,
            cwd=root,
            capture_output=True,
            text=True,
            timeout=10,
        )
        if script_failure:
            self.assertNotEqual(run.returncode, 0, run.stdout + run.stderr)
            self.assertIn("generated resource drift", run.stderr)
        elif failure:
            self.assertNotEqual(run.returncode, 0, run.stdout + run.stderr)
            self.assertIn("required ignored fixture", run.stderr)
        else:
            self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        return (
            checkout,
            reference,
            [json.loads(line) for line in log.read_text().splitlines()],
        )

    def test_linux_uses_custom_target_explicit_artifacts_oracle_and_budget_case(self):
        checkout, reference, calls = self.run_shell("Linux")
        self.assertEqual(
            calls[:2],
            [
                ["python", ["scripts/generate_graphql_printable.py", "--check"]],
                ["python", ["scripts/generate_plex_xml_encodings.py", "--check"]],
            ],
        )
        qualifier = next(
            args
            for tool, args in calls
            if tool == "python" and args[0] == "scripts/qualify_native_mutations.py"
        )
        self.assertEqual(
            qualifier[qualifier.index("--reference-root") + 1], str(reference)
        )
        for flag, filename in (
            ("--binary", "qualification-native"),
            ("--http-binary", "qualification-http"),
            ("--legacy-binary", "qualification-legacy"),
            ("--parser-binary", "qualification-parser"),
            ("--extension-binary", "qualification-extension.so"),
        ):
            path = Path(qualifier[qualifier.index(flag) + 1])
            self.assertEqual(path.name, filename)
            self.assertTrue(path.is_file())
        self.assertEqual(
            (
                checkout
                / ".local-tests/rust-migration/m4-plus/artifacts/qualification-extension.so"
            ).read_text(),
            "debug/lib_native.so",
        )
        budget = [
            args
            for tool, args in calls
            if tool == "cargo"
            and "entry_budget_exhaustion_cannot_restart_for_later_sources" in args
        ]
        self.assertEqual(len(budget), 2)
        self.assertEqual(budget[0][-3:], ["--ignored", "--exact", "--list"])
        self.assertEqual(budget[1][-2:], ["--ignored", "--exact"])
        exact_executions = [
            args
            for tool, args in calls
            if tool == "cargo" and "--ignored" in args and "--list" not in args
        ]
        expected = [
            [
                "test",
                "--locked",
                "-p",
                "catabolic-runtime",
                "--test",
                "source_events_independent",
                "entry_budget_exhaustion_cannot_restart_for_later_sources",
                "--",
                "--ignored",
                "--exact",
            ],
            *[
                [
                    "test",
                    "--locked",
                    "-p",
                    package,
                    "--lib",
                    scenario,
                    "--",
                    "--ignored",
                    "--exact",
                ]
                for package, scenario in (
                    (
                        "catabolic-runtime",
                        "document::tests::real_pdf_text_and_page_overflow",
                    ),
                    ("catabolic-runtime", "document::tests::real_single_image_ocr"),
                    (
                        "catabolic-store",
                        "artifact_mutation::tests::python_native_publication_intents_recover_in_both_directions",
                    ),
                    (
                        "catabolic-store",
                        "access_admin::tests::frozen_access_owner_differential",
                    ),
                    (
                        "catabolic-store",
                        "access_admin::tests::inherited_scalar_cache_confusion_remains_fail_closed_native",
                    ),
                )
            ],
        ]
        # Exact identities and multiplicity: adding or deleting any required
        # fixture must not be hidden by a stale or permissive count assertion.
        self.assertEqual(exact_executions, expected)
        listed = [
            args
            for tool, args in calls
            if tool == "cargo" and "--ignored" in args and "--list" in args
        ]
        self.assertEqual(listed, [args + ["--list"] for args in expected])

    def test_generated_resource_drift_stops_before_cargo_or_artifacts(self):
        for generator in ("graphql_printable", "plex_xml_encodings"):
            with self.subTest(generator=generator):
                checkout, _, calls = self.run_shell(
                    "Linux", script_failure=f"scripts/generate_{generator}.py"
                )
                self.assertFalse(any(tool == "cargo" for tool, _ in calls))
                self.assertFalse(
                    (checkout / ".local-tests/rust-migration/m4-plus").exists()
                )

    def test_missing_duplicate_and_zero_execution_cargo_fixtures_stop_gate(self):
        for failure in ("missing", "duplicate", "zero"):
            with self.subTest(failure=failure):
                _, _, calls = self.run_shell("Linux", failure=failure)
                self.assertFalse(
                    any(
                        tool == "python"
                        and args[0] == "scripts/qualify_native_mutations.py"
                        for tool, args in calls
                    )
                )

    def test_darwin_artifact_name_branch_only_is_wired_without_platform_claim(self):
        checkout, _, calls = self.run_shell("Darwin")
        self.assertEqual(
            (
                checkout
                / ".local-tests/rust-migration/m4-plus/artifacts/qualification-extension.so"
            ).read_text(),
            "debug/lib_native.dylib",
        )
        self.assertFalse(
            any(
                "entry_budget_exhaustion_cannot_restart_for_later_sources" in args
                for tool, args in calls
            )
        )
