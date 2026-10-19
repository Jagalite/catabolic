# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Disposable admission controls, separate from real native scenario evidence."""

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts import native_facade_qualification as facade
from scripts import qualify_native_mutations as qualifier
from tests import test_rust_artifact_build as artifact_support
from tests import test_rust_qualification_plumbing_review as plumbing_support


class DynamicPrerequisiteTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.wrapper = self.root / "crates/catabolic-python/python/catabolic_native"
        self.wrapper.mkdir(parents=True)
        for name in ("__init__.py", "graphql_query.py", "store.py"):
            (self.wrapper / name).write_text("# disposable wrapper\n")
        self.extension = self.root / "extension.so"
        self.extension.write_bytes(
            b"disposable extension bytes, not executable evidence"
        )
        self.sources = qualifier.source_snapshot(self.root)
        self.extension_sha256 = hashlib.sha256(self.extension.read_bytes()).hexdigest()
        self.environment = patch.dict(os.environ)
        self.environment.start()
        self.addCleanup(self.environment.stop)
        os.environ.pop("CATABOLIC_COMPLETION_EXTENSION_ROOT", None)

    def stage(self):
        with patch.object(qualifier.atexit, "register", self.addCleanup):
            return qualifier.stage_completion_extension(
                self.root, self.sources, self.extension, self.extension_sha256
            )

    def runtime_fixture(self):
        self.python = self.root / "venv/bin/python"
        self.python.parent.mkdir(parents=True)
        self.python.symlink_to(sys.executable)
        self.library = self.root / "venv/lib/python3.14/site-packages"
        self.package = self.library / "catabolic"
        self.package.mkdir(parents=True)
        self.frozen = {}
        for name in ("__init__.py", "domain.py", "http_worker.py"):
            path = self.package / name
            path.write_text("# frozen " + name + "\n")
            self.frozen["src/catabolic/" + name] = hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
        self.identity = {
            "version": [3, 14, 6],
            "executable": str(self.python),
            "prefix": str(self.root / "venv"),
            "base_prefix": "/base",
            "executable_sha256": "a" * 64,
        }
        self.probe = {
            "purelib": str(self.library),
            "modules": {
                "catabolic": str(self.package / "__init__.py"),
                "catabolic.domain": str(self.package / "domain.py"),
                "catabolic.http_worker": str(self.package / "http_worker.py"),
            },
        }
        os.environ["CATABOLIC_PYTHON_ORACLE"] = str(self.python)

    def admit_runtime(self, run=None):
        with (
            patch.object(facade, "reference_runtime", return_value=self.identity),
            patch.object(qualifier.subprocess, "run", side_effect=run) as invocation,
            patch.dict(
                sys.modules,
                {
                    "tests.test_rust_legacy_parser": types.SimpleNamespace(
                        PYTHON=self.python
                    )
                },
            ),
        ):
            if run is None:
                invocation.return_value = types.SimpleNamespace(
                    stdout=json.dumps(self.probe)
                )
            result = qualifier.worker_oracle_snapshot(
                self.python, {"resource_sha256": self.frozen}
            )
        return result

    def test_exact_sort_owner_artifact_admission(self):
        self.assertEqual(len(qualifier.SORT_OWNER_SCENARIOS), 3)
        for scenario in qualifier.SORT_OWNER_SCENARIOS:
            with self.subTest(scenario=scenario):
                with self.assertRaisesRegex(ValueError, "--extension-binary"):
                    qualifier.validate_native_scenarios([scenario], None)
                qualifier.validate_native_scenarios(
                    [scenario], None, extension_binary=self.extension
                )
        qualifier.validate_native_scenarios(
            [
                "tests.test_rust_catalog_sort_diagnostics.CatalogSortDiagnosticsTest.test_historical_fixture_and_full_baselines_remain_explicit"
            ],
            None,
        )

    def test_exact_completion_artifact_admission(self):
        for scenario, options, error in (
            (qualifier.COMPLETION_EMBEDDING, {}, "--legacy-binary"),
            (
                qualifier.COMPLETION_EMBEDDING,
                {"legacy_binary": self.extension},
                "--extension-binary",
            ),
            (
                qualifier.COMPLETION_HTTP,
                {"legacy_binary": self.extension},
                "--http-binary",
            ),
        ):
            with (
                self.subTest(scenario=scenario, options=options),
                self.assertRaisesRegex(ValueError, error),
            ):
                qualifier.validate_native_scenarios([scenario], None, **options)
        qualifier.validate_native_scenarios(
            [qualifier.COMPLETION_EMBEDDING, qualifier.COMPLETION_HTTP],
            self.extension,
            legacy_binary=self.extension,
            extension_binary=self.extension,
        )
        qualifier.validate_native_scenarios(
            ["tests.test_rust_unrelated.Probe.test_one"], None
        )

    def test_query_ownership_requires_and_stages_the_explicit_extension(self):
        for method in (
            "test_first_queries_preserve_temp_storage_and_owner",
            "test_rolled_back_first_query_setup_is_recreated",
        ):
            scenario = (
                qualifier.QUERY_OWNERSHIP_PREFIX
                + "NativeQuerySetupOwnershipTest."
                + method
            )
            with self.subTest(scenario=scenario):
                with self.assertRaisesRegex(ValueError, "--extension-binary"):
                    qualifier.validate_native_scenarios([scenario], None)
                qualifier.validate_native_scenarios(
                    [scenario], None, extension_binary=self.extension
                )
                self.assertTrue(qualifier.needs_staged_extension([scenario]))
        self.assertFalse(
            qualifier.needs_staged_extension(
                ["tests.test_rust_unrelated.Probe.test_one"]
            )
        )

    def test_worker_scope_is_exact_and_does_not_include_path_restoring_fixtures(self):
        for family in ("identify.IdentifyTest", "legacy_refresh.LegacyRefreshTest"):
            self.assertTrue(
                ("tests.test_rust_" + family + ".test_one").startswith(
                    qualifier.WORKER_PREFIXES
                )
            )
        for family in (
            "legacy_consumer_watch",
            "legacy_destination_mappings",
            "legacy_external_rule",
            "legacy_parser",
            "legacy_cli",
        ):
            self.assertFalse(
                ("tests.test_rust_" + family + ".Probe.test_one").startswith(
                    qualifier.WORKER_PREFIXES
                )
            )

    def test_stage_exact_fenced_development_sources_and_explicit_extension(self):
        staged = self.stage()
        self.assertEqual(len(staged["files"]), 4)
        self.assertEqual(
            staged["files"]["catabolic_native/_native.abi3.so"],
            hashlib.sha256(self.extension.read_bytes()).hexdigest(),
        )
        self.assertEqual(staged["source_content_sha256"], self.sources["sha256"])
        self.assertIn("not installed-wheel", staged["scope"])
        self.assertNotEqual(Path(staged["root"]), self.wrapper.parent)
        self.assertEqual(
            qualifier.package_snapshot(Path(staged["root"])), staged["files"]
        )
        self.assertFalse((self.wrapper / "_native.abi3.so").exists())

    def test_ambient_completion_root_and_missing_extension_are_refused(self):
        os.environ["CATABOLIC_COMPLETION_EXTENSION_ROOT"] = str(self.wrapper.parent)
        with self.assertRaisesRegex(ValueError, "ambient"):
            self.stage()
        os.environ.pop("CATABOLIC_COMPLETION_EXTENSION_ROOT")
        with self.assertRaisesRegex(ValueError, "explicit --extension-binary"):
            qualifier.stage_completion_extension(
                self.root, self.sources, None, self.extension_sha256
            )

    def test_source_drift_missing_inventory_and_unexpected_payload_are_refused(self):
        path = self.wrapper / "store.py"
        path.write_text("changed\n")
        with self.assertRaisesRegex(ValueError, "changed since snapshot"):
            self.stage()
        self.sources = qualifier.source_snapshot(self.root)
        del self.sources["files"][str(path.relative_to(self.root))]
        with self.assertRaisesRegex(ValueError, "missing from qualified source"):
            self.stage()
        (self.wrapper / "unreviewed.so").write_bytes(b"unexpected")
        self.sources = qualifier.source_snapshot(self.root)
        with self.assertRaisesRegex(ValueError, "unexpected payload"):
            self.stage()

    def test_extension_drift_since_artifact_snapshot_is_refused(self):
        self.extension.write_bytes(b"changed after artifact snapshot")
        with self.assertRaisesRegex(ValueError, "changed since artifact snapshot"):
            self.stage()

    def test_owned_temporary_parent_alias_resolves_without_relaxing_payload_fence(self):
        physical = self.root / "physical-temporary-parent"
        physical.mkdir()
        alias = self.root / "temporary-parent-alias"
        alias.symlink_to(physical, target_is_directory=True)
        temporary = tempfile.TemporaryDirectory(dir=alias)
        self.addCleanup(temporary.cleanup)
        with patch.object(
            qualifier.tempfile, "TemporaryDirectory", return_value=temporary
        ):
            staged = self.stage()
        root = Path(staged["root"])
        self.assertEqual(root, Path(temporary.name).resolve(strict=True))
        self.assertFalse(root.is_relative_to(alias))
        self.assertEqual(qualifier.package_snapshot(root), staged["files"])
        # Resolving the owned root must not admit linked package contents.
        payload = root / "catabolic_native/store.py"
        payload.unlink()
        payload.symlink_to(self.wrapper / "store.py")
        with self.assertRaisesRegex(ValueError, "linked"):
            qualifier.package_snapshot(root)

    def test_temporary_stage_cleanup_is_registered(self):
        callbacks = []
        with patch.object(qualifier.atexit, "register", callbacks.append):
            staged = qualifier.stage_completion_extension(
                self.root, self.sources, self.extension, self.extension_sha256
            )
        self.assertEqual(len(callbacks), 1)
        callbacks[0]()
        self.assertFalse(Path(staged["root"]).exists())

    def test_staged_source_extension_and_environment_drift_fail_before_import(self):
        staged = self.stage()
        for name in staged["files"]:
            path = Path(staged["root"]) / name
            old = path.read_bytes()
            path.write_bytes(old + b"changed")
            os.environ["CATABOLIC_COMPLETION_EXTENSION_ROOT"] = staged["root"]
            with (
                self.subTest(name=name),
                patch.object(qualifier.subprocess, "run") as run,
            ):
                with self.assertRaisesRegex(ValueError, "package changed"):
                    qualifier.completion_extension_snapshot(staged)
                run.assert_not_called()
            path.write_bytes(old)
        os.environ["CATABOLIC_COMPLETION_EXTENSION_ROOT"] = "/ambient"
        with self.assertRaisesRegex(ValueError, "environment identity changed"):
            qualifier.completion_extension_snapshot(staged)

    def test_completion_import_identity_is_checked(self):
        staged = self.stage()
        os.environ["CATABOLIC_COMPLETION_EXTENSION_ROOT"] = staged["root"]
        modules = {
            "catabolic_native": staged["root"] + "/catabolic_native/__init__.py",
            "catabolic_native._native": staged["root"]
            + "/catabolic_native/_native.abi3.so",
        }
        with patch.object(
            qualifier.subprocess,
            "run",
            return_value=types.SimpleNamespace(stdout=json.dumps(modules)),
        ):
            self.assertEqual(
                qualifier.completion_extension_snapshot(staged)["imported_modules"],
                modules,
            )
        for invalid in (
            {},
            modules | {"catabolic_native._native": "/outside/_native.abi3.so"},
        ):
            with (
                self.subTest(invalid=invalid),
                patch.object(
                    qualifier.subprocess,
                    "run",
                    return_value=types.SimpleNamespace(stdout=json.dumps(invalid)),
                ),
            ):
                with self.assertRaises(ValueError):
                    qualifier.completion_extension_snapshot(staged)

    def test_linked_package_payloads_are_refused(self):
        staged = self.stage()
        path = Path(staged["root"]) / "catabolic_native/store.py"
        path.unlink()
        path.symlink_to(self.wrapper / "store.py")
        with self.assertRaisesRegex(ValueError, "linked"):
            qualifier.package_snapshot(Path(staged["root"]))

    def test_missing_explicit_oracle_cannot_fall_back_to_ambient(self):
        os.environ["CATABOLIC_PYTHON_ORACLE"] = sys.executable
        with self.assertRaisesRegex(ValueError, "explicit --python-oracle"):
            qualifier.worker_oracle_snapshot(None, {})

    def test_clean_worker_probe_preserves_venv_path_and_frozen_identity(self):
        self.runtime_fixture()
        os.environ["PYTHONPATH"] = "/ambient/source"
        os.environ["PYTHONHOME"] = "/ambient/home"

        def run(command, **options):
            self.assertEqual(command[0], str(self.python))
            self.assertNotEqual(Path(command[0]), self.python.resolve())
            self.assertNotIn("PYTHONPATH", options["env"])
            self.assertNotIn("PYTHONHOME", options["env"])
            self.assertEqual(options["env"]["HOME"], options["cwd"])
            self.assertTrue(Path(options["cwd"]).is_dir())
            return types.SimpleNamespace(stdout=json.dumps(self.probe))

        before = self.admit_runtime(run)
        self.assertEqual(before, self.admit_runtime())
        self.assertEqual(len(before["files"]), 3)
        self.assertEqual(before["reference_commit"], qualifier.REFERENCE)
        self.assertIn("not native installed-wheel", before["scope"])

    def test_missing_or_drifted_frozen_worker_and_extra_package_files_fail(self):
        self.runtime_fixture()
        worker = self.package / "http_worker.py"
        original = worker.read_bytes()
        for payload in (None, original + b"changed"):
            if payload is None:
                worker.unlink()
            else:
                worker.write_bytes(payload)
            with (
                self.subTest(payload=payload),
                self.assertRaisesRegex(ValueError, "exact frozen inventory"),
            ):
                self.admit_runtime()
        worker.write_bytes(original)
        (self.package / "extra.py").write_text("unexpected\n")
        with self.assertRaisesRegex(ValueError, "exact frozen inventory"):
            self.admit_runtime()

    def test_worker_import_escape_wrong_runtime_and_incomplete_identity_fail(self):
        self.runtime_fixture()
        for change in (
            {"modules": {}},
            {
                "modules": self.probe["modules"]
                | {"catabolic.http_worker": "/outside/http_worker.py"}
            },
            {"purelib": "/outside/site-packages"},
        ):
            before = self.probe
            self.probe = before | change
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.admit_runtime()
            self.probe = before

    def test_worker_import_failure_is_fail_closed(self):
        self.runtime_fixture()

        def fail(*args, **kwargs):
            raise subprocess.CalledProcessError(
                1, args[0], stderr="No module named 'catabolic'"
            )

        with self.assertRaises((ValueError, subprocess.CalledProcessError)):
            self.admit_runtime(fail)

    def test_oracle_environment_or_captured_fixture_mismatch_fails(self):
        self.runtime_fixture()
        os.environ["CATABOLIC_PYTHON_ORACLE"] = "/ambient/python"
        with self.assertRaisesRegex(ValueError, "environment identity changed"):
            self.admit_runtime()
        os.environ["CATABOLIC_PYTHON_ORACLE"] = str(self.python)
        with (
            patch.object(facade, "reference_runtime", return_value=self.identity),
            patch.dict(
                sys.modules,
                {
                    "tests.test_rust_legacy_parser": types.SimpleNamespace(
                        PYTHON=Path("/ambient/python")
                    )
                },
            ),
        ):
            with self.assertRaisesRegex(ValueError, "captured a different"):
                qualifier.worker_oracle_snapshot(
                    self.python, {"resource_sha256": self.frozen}
                )


class DynamicReceiptControlTest(unittest.TestCase):
    def setUp(self):
        self.fixture = artifact_support.NativeArtifactBuildTest()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.addCleanup(patch.stopall)
        patch.dict(os.environ).start()
        os.environ.pop("CATABOLIC_COMPLETION_EXTENSION_ROOT", None)
        self.driver = artifact_support.QUALIFIER

    def receipt(self):
        return json.loads((self.fixture.root / "receipt.json").read_text())

    def test_unselected_prerequisites_are_never_staged_or_probed(self):
        with (
            patch.object(self.driver, "stage_completion_extension") as stage,
            patch.object(self.driver, "worker_oracle_snapshot") as worker,
        ):
            self.assertEqual(
                self.fixture.run_legacy_qualification_fixture(
                    set(), "tests.test_rust_probe.Probe.test_one"
                ),
                0,
            )
        stage.assert_not_called()
        worker.assert_not_called()
        self.assertEqual(self.receipt()["dynamic_prerequisites_before"], {})
        self.assertTrue(self.receipt()["dynamic_prerequisites_unchanged"])

    def test_missing_explicit_oracle_invalidates_success_before_tests(self):
        with self.assertRaises(SystemExit):
            self.fixture.run_legacy_qualification_fixture(
                {"legacy_cli"},
                "tests.test_rust_identify.IdentifyTest.test_duplicate_candidates_reuse_one_proposal_within_the_page",
            )
        receipt = self.receipt()
        self.assertEqual(receipt["qualification_status"], "preflight_failed")
        self.assertEqual(receipt["tests_run"], 0)
        self.assertFalse(receipt["passed_without_skips"])
        self.assertIn("--python-oracle", receipt["prerequisite_error"])

    def test_completion_postflight_drift_retains_outcomes_and_fails_receipt(self):
        def prepare(artifacts):
            root = self.fixture.root / "crates/catabolic-python/python/catabolic_native"
            root.mkdir(parents=True)
            for name in ("__init__.py", "store.py", "graphql_query.py"):
                (root / name).write_text("# disposable\n")

        # The legacy receipt harness replaces atexit registration. Retain this
        # real disposable stage through the TestCase's normal cleanup instead.
        stage = self.driver.stage_completion_extension

        def owned_stage(*args):
            with patch.object(self.driver.atexit, "register", self.addCleanup):
                return stage(*args)

        # Receipt plumbing control only. Actual package import and scenarios run
        # separately through the unmodified canonical entrypoint.
        with (
            patch.object(
                self.driver, "stage_completion_extension", side_effect=owned_stage
            ),
            patch.object(
                self.driver,
                "completion_extension_snapshot",
                side_effect=[{"verified": True}, ValueError("package drift")],
            ),
        ):
            code = self.fixture.run_legacy_qualification_fixture(
                {"legacy_cli", "native_extension"},
                qualifier.COMPLETION_EMBEDDING,
                prepare=prepare,
            )
        self.assertEqual(code, 1)
        receipt = self.receipt()
        self.assertEqual(receipt["tests_run"], 1)
        self.assertEqual(
            receipt["results"],
            [{"id": qualifier.COMPLETION_EMBEDDING, "outcome": "passed"}],
        )
        self.assertFalse(receipt["dynamic_prerequisites_unchanged"])
        self.assertFalse(receipt["passed_without_skips"])
        self.assertEqual(receipt["dynamic_prerequisite_error"], "package drift")


class DynamicCallerControlTest(unittest.TestCase):
    def test_native_ci_installs_the_exact_noneditable_reference_before_caller(self):
        root = Path(__file__).resolve().parents[1]
        workflow = (root / ".github/workflows/native-qualification.yml").read_text()
        native, installed = workflow.split("\n  native:\n", 1)[1].split(
            "\n  installed:\n", 1
        )
        expected = "python -m pip install --no-deps --no-build-isolation .local-tests/rust-migration/reference"

        def check(text):
            lines = [
                line.strip()
                for line in text.splitlines()
                if "pip install" in line and "rust-migration/reference" in line
            ]
            self.assertEqual(lines, [expected])
            self.assertIn("ref: " + qualifier.REFERENCE, text)
            self.assertLess(
                text.index(expected), text.index("scripts/check-native-mutations.sh")
            )

        check(native)
        for replacement in (
            expected.replace(" .local-tests", " -e .local-tests"),
            "# " + expected,
            "",
        ):
            with (
                self.subTest(replacement=replacement),
                self.assertRaises(AssertionError),
            ):
                check(native.replace(expected, replacement))
        # The separate installed job remains outside this caller repair.
        self.assertIn(
            '"$oracle" -m pip install --no-deps --no-build-isolation -e .local-tests/rust-migration/reference',
            installed,
        )

    def test_supported_shell_platform_branches_pass_the_selected_oracle(self):
        # Existing stubbed shell harness verifies invocation wiring only; the
        # Darwin branch is not actual macOS/platform qualification.
        for platform, relative in (
            ("Linux", False),
            ("Darwin", False),
            ("Linux", True),
        ):
            with self.subTest(platform=platform, relative=relative):
                fixture = plumbing_support.QualificationShellReview()
                self.addCleanup(fixture.doCleanups)
                actual_run = subprocess.run

                def run(command, relative=relative, actual_run=actual_run, **options):
                    if relative:
                        checkout = Path(command[1]).parents[1]
                        selected = checkout / "relative interpreter/python"
                        selected.parent.mkdir()
                        selected.symlink_to(
                            options["env"]["CATABOLIC_QUALIFICATION_PYTHON"]
                        )
                        options["env"]["CATABOLIC_QUALIFICATION_PYTHON"] = (
                            "relative interpreter/python"
                        )
                    return actual_run(command, **options)

                with patch.object(plumbing_support.subprocess, "run", side_effect=run):
                    checkout, _, calls = fixture.run_shell(platform)
                command = next(
                    args
                    for tool, args in calls
                    if tool == "python"
                    and args[0] == "scripts/qualify_native_mutations.py"
                )
                selected = Path(command[command.index("--python-oracle") + 1])
                expected = (
                    checkout / "relative interpreter/python"
                    if relative
                    else checkout.parent / "tools/python"
                )
                self.assertEqual(selected, expected)
                self.assertTrue(selected.is_absolute())
                self.assertTrue(selected.is_symlink())
                self.assertNotEqual(selected, selected.resolve())
                self.assertIn("--defer-installed-asgi", command)


if __name__ == "__main__":
    unittest.main()
