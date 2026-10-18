# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native paired-artifact source fences, independently of any compiler run."""

import hashlib
import importlib.util
import io
import json
import os
import py_compile
import subprocess
import sys
import tempfile
import types
import unittest
import zipfile
from contextlib import ExitStack, redirect_stderr
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "native_wheel_builder", ROOT / "scripts/build_native_wheel.py"
)
BUILDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILDER)
QUALIFIER_SPEC = importlib.util.spec_from_file_location(
    "native_mutation_qualifier", ROOT / "scripts/qualify_native_mutations.py"
)
QUALIFIER = importlib.util.module_from_spec(QUALIFIER_SPEC)
QUALIFIER_SPEC.loader.exec_module(QUALIFIER)


class NativeArtifactBuildTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml"):
            (self.root / name).write_text(name)
        source = self.root / "crates/example/src"
        source.mkdir(parents=True)
        (source / "lib.rs").write_text("pub const VALUE: u8 = 1;")

    def test_crate_workspace_resource_and_cargo_configuration_are_fenced(self):
        paths = [
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "crates/example/src/lib.rs",
            "crates/example/resources/schema.json",
            "crates/catabolic-http/resources/releases/1.7.0/openapi.json",
            ".cargo/config.toml",
            "crates/example/.cargo/config.toml",
        ]
        for name in paths:
            before = BUILDER.source_digest(self.root)
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            original = path.read_bytes() if path.exists() else None
            path.write_bytes((original or b"") + b"changed")
            with self.subTest(name=name):
                self.assertNotEqual(before, BUILDER.source_digest(self.root))
            if original is None:
                path.unlink()
            else:
                path.write_bytes(original)
            self.assertEqual(before, BUILDER.source_digest(self.root))

    def test_generated_helper_and_python_cache_do_not_self_invalidate_build(self):
        before = BUILDER.source_digest(self.root)
        qualification_before = QUALIFIER.source_snapshot(self.root)
        for name in (
            "crates/catabolic-python/python/catabolic_native/bin/catabolic-native",
            "crates/catabolic-python/python/catabolic_native/bin/catabolic-native.tmp",
            "crates/catabolic-python/python/catabolic_native/native-helper.json",
            "crates/catabolic-python/python/catabolic_native/bin/catabolic-legacy-native",
            "crates/catabolic-python/python/catabolic_native/bin/catabolic-legacy-native.tmp",
            "crates/catabolic-python/python/catabolic_native/legacy-helper.json",
            "crates/catabolic-python/python/catabolic_native/__pycache__/module.pyc",
        ):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("local generated artifact")
        self.assertEqual(before, BUILDER.source_digest(self.root))
        self.assertEqual(qualification_before, QUALIFIER.source_snapshot(self.root))

    def test_optimized_qualifier_refuses_before_emitting_receipt(self):
        # Supply only a disposable import stub; optimized refusal must not
        # depend on any installed extension, wheel or successful qualification.
        (self.root / "catabolic_native.py").write_text("")
        import os

        result = subprocess.run(
            [
                sys.executable,
                "-O",
                str(ROOT / "scripts/qualify_native_wheel.py"),
                "--help",
            ],
            env=os.environ | {"PYTHONPATH": str(self.root)},
            cwd=self.root,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires Python assertions", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_query_qualification_refuses_optimized_gate_and_scenarios(self):
        for optimize in ("-O", "-OO"):
            for arguments in (["--help"], ["--scenario", "reference"]):
                with self.subTest(optimize=optimize, arguments=arguments):
                    run = subprocess.run(
                        [
                            sys.executable,
                            optimize,
                            str(ROOT / "scripts/native_query_embedding_tests.py"),
                            *arguments,
                        ],
                        capture_output=True,
                        text=True,
                        timeout=10,
                    )
                    self.assertNotEqual(run.returncode, 0)
                    self.assertIn("query qualification requires assertions", run.stderr)
                    self.assertEqual(run.stdout, "")

    def test_pyo3_interruption_requires_and_fences_explicit_extension(self):
        scenario = "tests.test_rust_plex_login.NativePlexLoginTest.test_publication_interrupt_cleanup_cli_and_python"
        with self.assertRaisesRegex(ValueError, "explicit --extension-binary"):
            QUALIFIER.validate_native_scenarios([scenario], None)
        QUALIFIER.validate_native_scenarios(
            [scenario], None, extension_binary=self.root / "extension"
        )
        with self.assertRaises(SystemExit):
            self.run_legacy_qualification_fixture(set(), scenario)

        def discover(artifacts):
            self.assertEqual(
                os.environ["CATABOLIC_NATIVE_EXTENSION"],
                str(artifacts["native_extension"]),
            )

        self.assertEqual(
            self.run_legacy_qualification_fixture(
                {"native_extension"}, scenario, discover=discover
            ),
            0,
        )
        receipt = json.loads((self.root / "receipt.json").read_text())
        self.assertTrue(receipt["passed_without_skips"])
        self.assertIn("native_extension", receipt["extra_artifacts_before"])

        def change(artifacts):
            artifacts["native_extension"].write_bytes(b"changed extension")

        self.assertEqual(
            self.run_legacy_qualification_fixture(
                {"native_extension"}, scenario, action=change
            ),
            1,
        )
        receipt = json.loads((self.root / "receipt.json").read_text())
        self.assertFalse(receipt["passed_without_skips"])
        self.assertNotEqual(
            receipt["extra_artifacts_before"], receipt["extra_artifacts_after"]
        )

    def test_mutation_qualification_fences_implementation_and_test_sources(self):
        before = QUALIFIER.source_snapshot(self.root)
        for name in (
            "crates/example/src/lib.rs",
            "crates/catabolic-cli/src/legacy_main.rs",
            "crates/catabolic-cli/examples/legacy_parse.rs",
            "crates/catabolic-cli/resources/legacy-cli.json",
            "src/catabolic/store.py",
            "tests/test_example.py",
            "tests/fixtures/catalog.json",
            "scripts/qualify_native_mutations.py",
            ".cargo/config.toml",
        ):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            original = path.read_bytes() if path.exists() else None
            path.write_bytes((original or b"") + b"changed")
            with self.subTest(name=name):
                after = QUALIFIER.source_snapshot(self.root)
                self.assertNotEqual(before["sha256"], after["sha256"])
                self.assertIn(name, after["files"])
            if original is None:
                path.unlink()
            else:
                path.write_bytes(original)
            self.assertEqual(before, QUALIFIER.source_snapshot(self.root))

    def test_optimized_mutation_qualification_refuses(self):
        result = subprocess.run(
            [
                sys.executable,
                "-O",
                str(ROOT / "scripts/qualify_native_mutations.py"),
                "--help",
            ],
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires unoptimized Python tests", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_http_qualification_refuses_optimized_and_fences_client_artifacts(self):
        script = ROOT / "scripts/qualify_native_http.py"
        result = subprocess.run(
            [sys.executable, "-O", str(script), "--help"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires unoptimized Python assertions", result.stderr)
        self.assertEqual(result.stdout, "")
        spec = importlib.util.spec_from_file_location("http_qualifier", script)
        qualifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(qualifier)
        binary = self.root / "native"
        binary.write_bytes(b"native fixture")
        client = self.root / "client/acceptance.js"
        client.parent.mkdir()
        client.write_text("fixture client")
        dependency = client.parent / "busy.js"
        dependency.write_text("fixture import")
        before = qualifier.artifact_hashes(binary, client)
        for path in (binary, client, dependency):
            original = path.read_bytes()
            path.write_bytes(original + b"changed")
            self.assertNotEqual(before, qualifier.artifact_hashes(binary, client))
            path.write_bytes(original)
            self.assertEqual(before, qualifier.artifact_hashes(binary, client))

    def test_mutation_qualification_refuses_imported_oracles_and_unbound_http(self):
        native = "tests.test_rust_example.NativeTest.test_case"
        oracle = "tests.test_example.ReferenceTest.test_case"
        http = "tests.test_rust_http_transport.NativeHTTPTransportTest.test_case"
        QUALIFIER.validate_native_scenarios([native], None)
        with self.assertRaisesRegex(ValueError, "non-native oracle tests leaked"):
            QUALIFIER.validate_native_scenarios([native, oracle], None)
        with self.assertRaisesRegex(ValueError, "explicit --http-binary artifact"):
            QUALIFIER.validate_native_scenarios([native, http], None)
        QUALIFIER.validate_native_scenarios([native, http], self.root / "http")

    def test_legacy_cli_and_parser_require_distinct_fenced_artifacts(self):
        parser_case = "tests.test_rust_legacy_parser.NativeParserTest.test_case"
        cli_case = "tests.test_rust_legacy_cli.NativeCLITest.test_case"
        event_case = "tests.test_rust_native_events.NativeEventSupervisorTest.test_case"
        with self.assertRaisesRegex(ValueError, "explicit --legacy-binary"):
            QUALIFIER.validate_native_scenarios([event_case], None)
        QUALIFIER.validate_native_scenarios([event_case], None, self.root / "legacy")
        with self.assertRaisesRegex(ValueError, "explicit --parser-binary"):
            QUALIFIER.validate_native_scenarios([parser_case], None)
        with self.assertRaisesRegex(ValueError, "explicit --legacy-binary"):
            QUALIFIER.validate_native_scenarios(
                [parser_case, cli_case], None, parser_binary=self.root / "parser"
            )
        artifacts = {
            "legacy_cli": self.root / "legacy",
            "legacy_parser": self.root / "parser",
        }
        for name, path in artifacts.items():
            path.write_text(name)
        QUALIFIER.validate_native_scenarios(
            [parser_case, cli_case],
            None,
            artifacts["legacy_cli"],
            artifacts["legacy_parser"],
        )
        before = QUALIFIER.extra_artifact_hashes(artifacts)
        for path in artifacts.values():
            content = path.read_bytes()
            path.write_bytes(content + b"changed")
            self.assertNotEqual(before, QUALIFIER.extra_artifact_hashes(artifacts))
            path.write_bytes(content)
            self.assertEqual(before, QUALIFIER.extra_artifact_hashes(artifacts))
        artifacts["legacy_parser"].unlink()
        with self.assertRaises(FileNotFoundError):
            QUALIFIER.extra_artifact_hashes(artifacts)

    def run_legacy_qualification_fixture(
        self,
        options,
        scenario,
        *,
        discover=None,
        action=None,
        prepare=None,
        prerequisites=None,
        import_snapshots=None,
    ):
        """Exercise receipt/binding control flow without real builds or oracles."""
        artifacts = {
            name: self.root / name
            for name in ("native", "legacy_cli", "legacy_parser", "native_extension")
        }
        for name, path in artifacts.items():
            path.write_text(name)
        report = self.root / "receipt.json"
        report.write_text(json.dumps({"passed_without_skips": True}))
        reference = self.root / "reference"
        reference.mkdir(exist_ok=True)
        if prepare:
            prepare(artifacts)
        argv = [
            "qualify_native_mutations.py",
            "--binary",
            str(artifacts["native"]),
            "--reference-root",
            str(reference),
            "--report",
            str(report),
            "--pattern",
            "test_rust_fixture.py",
        ]
        if "defer_installed_asgi" in options:
            argv.append("--defer-installed-asgi")
        for option, name in (
            ("--legacy-binary", "legacy_cli"),
            ("--parser-binary", "legacy_parser"),
            ("--extension-binary", "native_extension"),
        ):
            if name in options:
                argv.extend([option, str(artifacts[name])])

        class FixtureCase(unittest.TestCase):
            def id(self):
                return scenario

            def runTest(self):
                if action:
                    action(artifacts)

        def load(*args, **kwargs):
            if discover:
                discover(artifacts)
            return unittest.TestSuite([FixtureCase()])

        with ExitStack() as stack:
            stack.enter_context(
                patch.object(
                    QUALIFIER,
                    "__file__",
                    str(self.root / "scripts/qualify_native_mutations.py"),
                )
            )
            stack.enter_context(patch.object(sys, "argv", argv))
            stack.enter_context(patch.object(sys, "path", sys.path.copy()))
            stack.enter_context(
                patch.dict(
                    os.environ,
                    {
                        "CATABOLIC_LEGACY_BINARY": "/ambient/unqualified-legacy",
                        "CATABOLIC_NATIVE_PARSER_BINARY": "/ambient/unqualified-parser",
                    },
                )
            )
            stack.enter_context(patch.object(QUALIFIER.os, "chdir"))
            stack.enter_context(
                patch.object(
                    QUALIFIER,
                    "fresh_import_cache",
                    return_value=types.SimpleNamespace(cleanup=lambda: None),
                )
            )
            stack.enter_context(patch.object(QUALIFIER.atexit, "register"))
            for name in (
                "oracle_snapshot",
                "fixture_snapshot",
                "adaptation_snapshot",
            ):
                stack.enter_context(patch.object(QUALIFIER, name, return_value={}))
            stack.enter_context(
                patch.object(
                    QUALIFIER,
                    "oracle_imports",
                    return_value={},
                    side_effect=import_snapshots,
                )
            )
            stack.enter_context(
                patch.object(unittest.defaultTestLoader, "discover", side_effect=load)
            )
            if prerequisites is not None:
                stack.enter_context(
                    patch.object(
                        QUALIFIER,
                        "discover_native_suite",
                        side_effect=lambda *args: (load(), prerequisites),
                    )
                )
            stack.enter_context(
                patch.object(QUALIFIER.subprocess, "check_output", return_value="{}")
            )
            stack.enter_context(redirect_stderr(io.StringIO()))
            return QUALIFIER.main()

    def test_deferred_asgi_receipt_cannot_claim_installed_prerequisite_success(self):
        from scripts.native_asgi_scenarios import (
            REQUIRED_IDS,
            installed_prerequisite,
            validate_sources,
        )

        prerequisite = installed_prerequisite(validate_sources(ROOT))
        selected = "tests.test_rust_probe.NativeProbe.test_selected"

        def inspect_running_marker(_artifacts):
            marker = json.loads((self.root / "receipt.json").read_text())
            self.assertEqual(marker["phase"], "running")
            self.assertEqual(marker["tests_run"], 0)
            self.assertFalse(marker["passed_without_skips"])
            self.assertFalse(marker["selected_aggregate_passed_without_skips"])
            self.assertEqual(marker["installed_prerequisites"], [prerequisite])

        self.assertEqual(
            self.run_legacy_qualification_fixture(
                {"defer_installed_asgi"},
                selected,
                prerequisites=[prerequisite],
                action=inspect_running_marker,
            ),
            0,
        )
        receipt = json.loads((self.root / "receipt.json").read_text())
        self.assertEqual(
            receipt["qualification_status"],
            "selected_aggregate_passed_installed_prerequisites_pending",
        )
        self.assertTrue(receipt["selected_aggregate_passed_without_skips"])
        self.assertFalse(receipt["passed_without_skips"])
        self.assertEqual(receipt["installed_prerequisites"], [prerequisite])
        self.assertEqual(receipt["tests_run"], 1)
        self.assertEqual(receipt["unique_discovered_scenarios"], [selected])
        self.assertEqual(receipt["results"], [{"id": selected, "outcome": "passed"}])
        self.assertEqual(receipt["skips"], [])
        self.assertTrue(
            set(REQUIRED_IDS).isdisjoint(receipt["unique_discovered_scenarios"])
        )

        def fail(_artifacts):
            raise AssertionError("selected aggregate failure")

        self.assertEqual(
            self.run_legacy_qualification_fixture(
                {"defer_installed_asgi"},
                selected,
                prerequisites=[prerequisite],
                action=fail,
            ),
            1,
        )
        receipt = json.loads((self.root / "receipt.json").read_text())
        self.assertFalse(receipt["selected_aggregate_passed_without_skips"])
        self.assertFalse(receipt["passed_without_skips"])
        self.assertEqual(receipt["installed_prerequisites"][0]["status"], "pending")

    def test_deferred_asgi_discovery_failure_invalidates_previous_receipt(self):
        with patch.object(
            QUALIFIER,
            "discover_native_suite",
            side_effect=ValueError("installed ASGI scenario inventory drift"),
        ):
            with self.assertRaises(SystemExit) as error:
                self.run_legacy_qualification_fixture(
                    {"defer_installed_asgi"},
                    "tests.test_rust_probe.NativeProbe.test_selected",
                )
        self.assertEqual(error.exception.code, 2)
        receipt = json.loads((self.root / "receipt.json").read_text())
        self.assertEqual(receipt["qualification_status"], "started")
        self.assertFalse(receipt["selected_aggregate_passed_without_skips"])
        self.assertFalse(receipt["passed_without_skips"])
        self.assertEqual(receipt["installed_prerequisites"][0]["status"], "pending")
        self.assertFalse(receipt["installed_prerequisites"][0]["manifest_verified"])

    def test_mutation_during_scenarios_invalidates_source_stability_receipt(self):
        def change(_artifacts):
            path = self.root / "tests/new_fixture.py"
            path.parent.mkdir(parents=True)
            path.write_text("# added during qualification\n")

        self.assertEqual(
            self.run_legacy_qualification_fixture(
                set(), "tests.test_rust_fixture.Case.test_case", action=change
            ),
            1,
        )
        receipt = json.loads((self.root / "receipt.json").read_text())
        self.assertEqual(receipt["tests_run"], 1)
        self.assertFalse(receipt["source_content_unchanged_during_qualification"])
        self.assertFalse(receipt["passed_without_skips"])

    def test_discovery_failure_cannot_claim_scenarios_ran(self):
        def refuse(_artifacts):
            raise ValueError("fixture discovery refused")

        with self.assertRaises(SystemExit) as error:
            self.run_legacy_qualification_fixture(
                set(), "tests.test_rust_fixture.Case.test_case", discover=refuse
            )
        self.assertEqual(error.exception.code, 2)
        receipt = json.loads((self.root / "receipt.json").read_text())
        self.assertEqual(receipt["phase"], "preflight")
        self.assertEqual(receipt["tests_run"], 0)
        self.assertFalse(receipt["passed_without_skips"])

    def test_import_preflight_failure_records_zero_executed_scenarios(self):
        executed = []
        with self.assertRaisesRegex(ValueError, "fixture import refused"):
            self.run_legacy_qualification_fixture(
                set(),
                "tests.test_rust_fixture.Case.test_case",
                action=lambda _artifacts: executed.append(True),
                import_snapshots=[ValueError("fixture import refused")],
            )
        self.assertEqual(executed, [])
        receipt = json.loads((self.root / "receipt.json").read_text())
        self.assertEqual(receipt["phase"], "preflight")
        self.assertEqual(receipt["tests_run"], 0)
        self.assertFalse(receipt["passed_without_skips"])

    def test_import_disappearance_and_equal_byte_rebinding_invalidate_receipt(self):
        before = {
            "tests.fixture": {
                "path": "/current/tests/fixture.py",
                "sha256": "a" * 64,
                "provenance": "frozen-fixture",
            }
        }
        rebound = {
            "tests.fixture": before["tests.fixture"]
            | {"path": "/reference/tests/fixture.py"}
        }
        for after in ({}, rebound):
            with self.subTest(after=after):
                self.assertEqual(
                    self.run_legacy_qualification_fixture(
                        set(),
                        "tests.test_rust_fixture.Case.test_case",
                        import_snapshots=[before, after],
                    ),
                    1,
                )
                receipt = json.loads((self.root / "receipt.json").read_text())
                self.assertEqual(receipt["tests_run"], 1)
                self.assertFalse(
                    receipt["source_content_unchanged_during_qualification"]
                )
                self.assertFalse(receipt["passed_without_skips"])

    def test_dependency_link_target_and_lock_drift_invalidate_completed_receipt(self):
        modules = self.root / "tests/clients/node_modules"
        alias = modules / ".bin/tool"
        lock = self.root / "tests/clients/package-lock.json"

        def prepare(_artifacts):
            (modules / ".bin").mkdir(parents=True, exist_ok=True)
            packages = {}
            for name in ("original", "alternate"):
                path = modules / name / "bin.js"
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"// identical fixture payload\n")
                packages["node_modules/" + name] = {
                    "version": "1.0.0",
                    "bin": {"tool": "bin.js"},
                }
            lock.write_text(json.dumps({"lockfileVersion": 3, "packages": packages}))
            alias.unlink(missing_ok=True)
            alias.symlink_to("../original/bin.js")

        for kind in ("link", "target", "lock"):

            def change(_artifacts, kind=kind):
                if kind == "link":
                    alias.unlink()
                    alias.symlink_to("../alternate/bin.js")
                elif kind == "target":
                    (modules / "original/bin.js").write_bytes(b"// changed payload\n")
                else:
                    lock.write_text(lock.read_text() + "\n")

            with self.subTest(kind=kind):
                self.assertEqual(
                    self.run_legacy_qualification_fixture(
                        set(),
                        "tests.test_rust_fixture.Case.test_case",
                        prepare=prepare,
                        action=change,
                    ),
                    1,
                )
                receipt = json.loads((self.root / "receipt.json").read_text())
                self.assertEqual(receipt["tests_run"], 1)
                self.assertFalse(
                    receipt["source_content_unchanged_during_qualification"]
                )
                self.assertFalse(receipt["passed_without_skips"])

    def test_legacy_artifacts_are_bound_before_discovery_and_recorded(self):
        def discover(artifacts):
            for name, environment in (
                ("legacy_cli", "CATABOLIC_LEGACY_BINARY"),
                ("legacy_parser", "CATABOLIC_NATIVE_PARSER_BINARY"),
            ):
                self.assertEqual(os.environ[environment], str(artifacts[name]))

        self.assertEqual(
            self.run_legacy_qualification_fixture(
                {"legacy_cli", "legacy_parser"},
                "tests.test_rust_legacy_parser.Fixture.test_case",
                discover=discover,
            ),
            0,
        )
        receipt = json.loads((self.root / "receipt.json").read_text())
        self.assertTrue(receipt["passed_without_skips"])
        self.assertEqual(
            receipt["extra_artifacts_before"],
            {
                name: {
                    "path": str(self.root / name),
                    "sha256": hashlib.sha256(name.encode()).hexdigest(),
                }
                for name in ("legacy_cli", "legacy_parser")
            },
        )
        self.assertEqual(
            receipt["extra_artifacts_before"], receipt["extra_artifacts_after"]
        )

    def test_ambient_legacy_artifacts_do_not_satisfy_explicit_requirements(self):
        def discover(artifacts):
            self.assertNotIn("CATABOLIC_LEGACY_BINARY", os.environ)
            self.assertNotIn("CATABOLIC_NATIVE_PARSER_BINARY", os.environ)

        for module in (
            "legacy_cli",
            "legacy_parser",
            "native_events",
            "native_events_independent",
        ):
            with self.subTest(module=module), self.assertRaises(SystemExit) as error:
                self.run_legacy_qualification_fixture(
                    set(),
                    f"tests.test_rust_{module}.Fixture.test_case",
                    discover=discover,
                )
            self.assertEqual(error.exception.code, 2)
            receipt = json.loads((self.root / "receipt.json").read_text())
            self.assertEqual(receipt["qualification_status"], "started")
            self.assertFalse(receipt["passed_without_skips"])

    def test_changed_legacy_artifacts_invalidate_successful_scenario_receipt(self):
        for name in ("legacy_cli", "legacy_parser"):
            with self.subTest(name=name):
                self.assertEqual(
                    self.run_legacy_qualification_fixture(
                        {"legacy_cli", "legacy_parser"},
                        "tests.test_rust_legacy_parser.Fixture.test_case",
                        action=lambda artifacts, name=name: artifacts[name].write_text(
                            "changed runtime artifact"
                        ),
                    ),
                    1,
                )
                receipt = json.loads((self.root / "receipt.json").read_text())
                self.assertEqual(receipt["qualification_status"], "completed")
                self.assertTrue(receipt["outcome_coverage_complete"])
                self.assertEqual(receipt["results"][0]["outcome"], "passed")
                self.assertFalse(receipt["artifact_unchanged_during_qualification"])
                self.assertFalse(receipt["passed_without_skips"])

    def test_missing_legacy_artifacts_cannot_leave_old_passing_receipt(self):
        for name in ("legacy_cli", "legacy_parser"):
            for stage in ("prepare", "action"):
                with self.subTest(name=name, stage=stage):
                    with self.assertRaises(FileNotFoundError):
                        self.run_legacy_qualification_fixture(
                            {"legacy_cli", "legacy_parser"},
                            "tests.test_rust_legacy_parser.Fixture.test_case",
                            **{
                                stage: lambda artifacts, name=name: artifacts[
                                    name
                                ].unlink()
                            },
                        )
                    receipt = json.loads((self.root / "receipt.json").read_text())
                    self.assertEqual(receipt["qualification_status"], "started")
                    self.assertFalse(receipt["passed_without_skips"])

    def test_frozen_oracle_and_historical_fixture_bytes_are_verified(self):
        oracle = self.root / "oracle"
        source = oracle / "src/catabolic/app.py"
        source.parent.mkdir(parents=True)
        source.write_text("fixture frozen source")
        inventory = self.root / "tests/parity/inventory.json"
        inventory.parent.mkdir(parents=True)
        inventory.write_text(
            json.dumps(
                {
                    "resource_sha256": {
                        "src/catabolic/app.py": hashlib.sha256(
                            source.read_bytes()
                        ).hexdigest()
                    }
                }
            )
        )
        QUALIFIER.oracle_snapshot(oracle, self.root)
        source.write_text("changed source")
        with self.assertRaisesRegex(ValueError, "frozen oracle byte drift"):
            QUALIFIER.oracle_snapshot(oracle, self.root)
        source.write_text("fixture frozen source")
        (source.parent / "extra.py").write_text("unrecorded import")
        with self.assertRaisesRegex(ValueError, "unrecorded payloads"):
            QUALIFIER.oracle_snapshot(oracle, self.root)
        fixtures = (
            self.root / ".local-tests/rust-migration/m0-completion/legacy-fixtures"
        )
        entries = []
        for schema in range(1, 33):
            relative = f"schema-{schema:02d}/before.sqlite3"
            path = fixtures / relative
            path.parent.mkdir(parents=True)
            path.write_bytes(bytes([schema]))
            entries.append(
                {
                    "schema": schema,
                    "database": relative,
                    "status": "passed",
                    "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                }
            )
        (fixtures / "report.json").write_text(
            json.dumps({"reference_commit": QUALIFIER.REFERENCE, "fixtures": entries})
        )
        self.assertEqual(len(QUALIFIER.fixture_snapshot(self.root)["sha256"]), 33)
        (fixtures / "schema-01/before.sqlite3").write_bytes(b"changed fixture")
        with self.assertRaisesRegex(ValueError, "historical fixture byte drift"):
            QUALIFIER.fixture_snapshot(self.root)

    def test_fresh_oracle_cache_ignores_timestamp_valid_stale_bytecode(self):
        source = self.root / "oracle_fixture.py"
        source.write_text("VALUE = 2\n")
        original = source.stat()
        bytecode = (
            source.parent
            / "__pycache__"
            / (f"oracle_fixture.{sys.implementation.cache_tag}.pyc")
        )
        bytecode.parent.mkdir()
        py_compile.compile(str(source), cfile=str(bytecode), doraise=True)
        source.write_text("VALUE = 1\n")
        os.utime(source, ns=(original.st_atime_ns, original.st_mtime_ns))
        self.assertEqual(source.stat().st_size, original.st_size)
        environment = os.environ.copy()
        environment.pop("PYTHONPYCACHEPREFIX", None)
        environment["PYTHONPATH"] = str(self.root)
        baseline = subprocess.check_output(
            [
                sys.executable,
                "-c",
                "import oracle_fixture; print(oracle_fixture.VALUE)",
            ],
            cwd=self.root,
            env=environment,
            text=True,
        )
        self.assertEqual(baseline.strip(), "2", "fixture must execute cached bytes")
        protected = subprocess.check_output(
            [
                sys.executable,
                "-c",
                "import importlib.util; "
                f"spec=importlib.util.spec_from_file_location('qualifier',{str(ROOT / 'scripts/qualify_native_mutations.py')!r}); "
                "module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module); "
                "cache=module.fresh_import_cache(); "
                "import oracle_fixture; print(oracle_fixture.VALUE); cache.cleanup()",
            ],
            cwd=self.root,
            env=environment,
            text=True,
        )
        self.assertEqual(protected.strip(), "1")

    def test_wheel_inventory_cannot_pass_with_missing_installed_payloads(self):
        installed = self.root / "installed"
        package = installed / "catabolic_native"
        payloads = {
            "catabolic_native/__init__.py": b"fixture package",
            "catabolic_native/_native.abi3.so": b"fixture extension",
            "catabolic_native/app.py": b"fixture public facade",
            "catabolic_native/bin/catabolic-native": b"fixture helper",
            "catabolic_native/native-helper.json": b"{}",
            "catabolic_native/bin/catabolic-legacy-native": b"fixture legacy helper",
            "catabolic_native/legacy-helper.json": b"{}",
            "catabolic/__init__.py": b"fixture compatibility package",
            "catabolic/cli.py": b"fixture compatibility console",
            "catabolic/_api_disposition.json": b"{}",
            "catabolic-1.0.0.dist-info/WHEEL": b"Wheel-Version: 1.0",
            "catabolic-1.0.0.dist-info/METADATA": b"Name: catabolic",
            "catabolic-1.0.0.dist-info/entry_points.txt": b"fixture launcher",
        }
        for name, content in payloads.items():
            path = installed / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        stub = types.SimpleNamespace(
            __file__=str(package / "__init__.py"),
            __version__="1.0.0",
            _native=types.SimpleNamespace(__file__=str(package / "_native.abi3.so")),
        )
        spec = importlib.util.spec_from_file_location(
            "installed_wheel_verifier", ROOT / "scripts/qualify_native_wheel.py"
        )
        verifier = importlib.util.module_from_spec(spec)
        with patch.dict(sys.modules, {"catabolic_native": stub}):
            spec.loader.exec_module(verifier)
        distribution = types.SimpleNamespace(
            files=list(payloads)
            + [
                "catabolic-1.0.0.dist-info/RECORD",
                "../../../bin/catabolic-native",
                "../../../bin/catabolic",
            ],
            locate_file=lambda name: installed / name,
            version="1.0.0",
            metadata={"Name": "catabolic"},
            entry_points=[
                types.SimpleNamespace(group="console_scripts", name=name, value=value)
                for name, value in (
                    ("catabolic", "catabolic.cli:main"),
                    ("catabolic-native", "catabolic_native:main"),
                )
            ],
        )
        modules = {
            key: value
            for key, value in sys.modules.items()
            if key != "catabolic" and not key.startswith("catabolic.")
        }
        with (
            patch.object(
                verifier.importlib.metadata, "distribution", return_value=distribution
            ),
            patch.object(sys, "path", [str(installed), *sys.path]),
            patch.dict(sys.modules, modules, clear=True),
        ):
            wheel = self.root / "fixture.whl"
            for omitted in (
                None,
                "all",
                "catabolic_native/app.py",
                "catabolic_native/bin/catabolic-native",
                "catabolic_native/native-helper.json",
                "catabolic_native/bin/catabolic-legacy-native",
                "catabolic_native/legacy-helper.json",
                "catabolic/cli.py",
                "catabolic/_api_disposition.json",
            ):
                with zipfile.ZipFile(wheel, "w") as archive:
                    for name, content in payloads.items():
                        if omitted != "all" and name != omitted:
                            archive.writestr(name, content)
                with self.subTest(omitted=omitted):
                    if omitted is None:
                        self.assertTrue(
                            verifier.verify_installed_wheel(wheel)[
                                "installed_payloads_match_wheel"
                            ]
                        )
                    else:
                        with self.assertRaises(AssertionError):
                            verifier.verify_installed_wheel(wheel)
            with zipfile.ZipFile(wheel, "w") as archive:
                for name, content in payloads.items():
                    archive.writestr(name, content)
            outside = self.root / "unqualified-package"
            outside.mkdir()
            (outside / "__init__.py").write_text("unqualified shadow package")
            shadow = package / "shadow"
            shadow.symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(AssertionError, "unqualified symlink"):
                verifier.verify_installed_wheel(wheel)
            shadow.unlink()
            self.assertTrue(
                verifier.verify_installed_wheel(wheel)["installed_payloads_match_wheel"]
            )

    def test_reference_loader_excludes_native_before_import(self):
        spec = importlib.util.spec_from_file_location(
            "scripts.reference_test_runner", ROOT / "scripts/run_reference_tests.py"
        )
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        (self.root / "test_reference_fixture.py").write_text(
            "import unittest\nclass Example(unittest.TestCase):\n"
            "    def test_original(self): pass\n"
        )
        (self.root / "test_rust_should_not_import.py").write_text(
            "raise RuntimeError('native prerequisites must not be imported')\n"
        )
        self.addCleanup(sys.modules.pop, "test_reference_fixture", None)
        loader = runner.ReferenceLoader()
        suite = loader.discover(str(self.root))
        self.assertEqual(
            list(runner.identifiers(suite)),
            ["test_reference_fixture.Example.test_original"],
        )
        self.assertEqual(loader.native_files, ["test_rust_should_not_import.py"])
