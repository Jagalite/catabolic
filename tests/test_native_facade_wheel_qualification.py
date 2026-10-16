# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Synthetic public-facade gate/paired-build regressions; no compiler or services.

The fake suites test gate accounting and provenance, never product behavior.
Actual 13/37/10 and 16/43/8 acceptance remains a separate installed-wheel run.
"""

import contextlib
import copy
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


def load(path):
    spec = importlib.util.spec_from_file_location(path.stem, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def digest(value):
    return hashlib.sha256(value).hexdigest()


class FacadeDriverChecks:
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "checkout"
        self.scripts = self.source / "scripts"
        self.scripts.mkdir(parents=True)
        for name in (
            self.filename,
            "native_facade_qualification.py",
            "native_qualification_results.py",
            "qualify_native_wheel.py",
            "qualify_native_embedding.py",
            "build_native_wheel.py",
        ):
            shutil.copyfile(SCRIPTS / name, self.scripts / name)
        self.driver = load(self.scripts / self.filename)
        self.shared = load(self.scripts / "native_facade_qualification.py")
        self.builder = load(self.scripts / "build_native_wheel.py")
        if self.filename.startswith("native_component"):
            original = load(SCRIPTS / self.filename)
            for name in original.source_manifest():
                if name.startswith("scripts/"):
                    continue
                path = self.source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"disposable source content")
        source = self.source / "crates/fixture/src/lib.rs"
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_text("// disposable source")
        self.changed_source = source
        self.installed = self.root / "site-packages"
        self.artifacts = self.root / "artifacts"
        self.artifacts.mkdir()
        self.wheel = self.artifacts / "fixture.whl"
        self.manifest_path = self.artifacts / "build-manifest.json"
        self.native_path = self.artifacts / "catabolic-native"
        self.legacy_path = self.artifacts / "catabolic-legacy-native"
        self.native_path.write_bytes(b"disposable native CLI")
        self.legacy_path.write_bytes(b"disposable legacy CLI")
        self.native_path.chmod(0o755)
        self.legacy_path.chmod(0o755)
        self.provenance = {
            "scope": "local test",
            "rustc": "rustc fixture",
            "cargo": "cargo fixture",
            "maturin": "maturin 1.13.1",
            "python": "Python 3.14.6",
            "python_executable": "/fixture/python",
            "build_environment_sha256": "a" * 64,
        }
        self.metadata = {
            "format": 1,
            "version": "0.2.0",
            "path": "bin/catabolic-native",
            "sha256": digest(self.native_path.read_bytes()),
            "source_sha256": self.builder.source_digest(self.source),
            "build": self.provenance,
        }
        self.legacy_metadata = self.metadata | {
            "path": "bin/catabolic-legacy-native",
            "sha256": digest(self.legacy_path.read_bytes()),
        }
        self.payloads = {
            "catabolic_native/__init__.py": b"# disposable package\n",
            "catabolic_native/_native.so": b"disposable extension",
            "catabolic_native/components.py": b"# disposable adapter\n",
            "catabolic_native/bin/catabolic-native": self.native_path.read_bytes(),
            "catabolic_native/native-helper.json": json.dumps(self.metadata).encode(),
            "catabolic_native/bin/catabolic-legacy-native": self.legacy_path.read_bytes(),
            "catabolic_native/legacy-helper.json": json.dumps(
                self.legacy_metadata
            ).encode(),
            "catabolic_native-0.2.0.dist-info/WHEEL": b"Wheel-Version: 1.0\n",
            "catabolic_native-0.2.0.dist-info/METADATA": b"Version: 0.2.0\n",
            "catabolic_native-0.2.0.dist-info/entry_points.txt": b"[console_scripts]\ncatabolic-native = catabolic_native:main\n",
            "catabolic_native-0.2.0.dist-info/RECORD": b"fixture record\n",
        }
        for name, data in self.payloads.items():
            path = self.installed / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        self.helper_path = self.installed / "catabolic_native/bin/catabolic-native"
        self.legacy_helper_path = (
            self.installed / "catabolic_native/bin/catabolic-legacy-native"
        )
        self.helper_path.chmod(0o755)
        self.legacy_helper_path.chmod(0o755)
        with zipfile.ZipFile(self.wheel, "w") as archive:
            for name, data in self.payloads.items():
                archive.writestr(name, data)
        self.manifest = {
            "format": 1,
            "profile": "release",
            "source_sha256": self.metadata["source_sha256"],
            "source_sha256_after": self.metadata["source_sha256"],
            "wheel": self.wheel.name,
            "wheel_sha256": digest(self.wheel.read_bytes()),
            "extension_sha256": digest(self.payloads["catabolic_native/_native.so"]),
            "native_binary": "catabolic-native",
            "native_binary_sha256": self.metadata["sha256"],
            "legacy_binary": "catabolic-legacy-native",
            "legacy_binary_sha256": digest(self.legacy_path.read_bytes()),
            "build_script_sha256": digest(
                (self.scripts / "build_native_wheel.py").read_bytes()
            ),
            "build": self.provenance,
            "publication": "local-only",
        }
        self.write_manifest()
        self.native = types.ModuleType("catabolic_native")
        self.native.__file__ = str(self.installed / "catabolic_native/__init__.py")
        self.native.__version__ = "0.2.0"
        self.native._native = types.SimpleNamespace(
            __file__=str(self.installed / "catabolic_native/_native.so")
        )
        self.native.native_binary = lambda: self.helper_path
        self.cli = types.ModuleType("catabolic_native.cli")
        self.cli.legacy_binary = lambda: self.legacy_helper_path
        self.native.Store = types.SimpleNamespace(
            initialize=lambda path: Path(path).write_bytes(b"fixture database")
        )
        self.distribution = types.SimpleNamespace(
            version="0.2.0",
            files=[Path(name) for name in self.payloads],
            locate_file=lambda name: self.installed / name,
        )
        self.inventory = self.root / "inventory.json"
        inventory = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        self.inventory.write_text(json.dumps(inventory))
        self.rows = [
            row for row in inventory["rows"] if row["id"] in self.driver.CONTRACTS
        ]
        self.calls = {
            row["id"] + (".__init__" if row["signature"]["kind"] == "class" else ""): 1
            for row in self.rows
        }
        self.oracle = {
            "head": self.shared.FROZEN_COMMIT,
            "root": str(self.root / "oracle"),
            "tracked_source_sha256": {"fixture": "bytes"},
        }
        self.runtime = {"version": [3, 14, 6], "executable": "/fixture/python"}
        self.report = self.root / "report.json"
        self.report.write_text('{"passed": true, "stale": true}')
        self.case = getattr(self.driver, self.classname)
        self.arguments = [
            "--wheel",
            str(self.wheel),
            "--source-root",
            str(self.source),
            "--build-manifest",
            str(self.manifest_path),
            "--inventory",
            str(self.inventory),
            "--reference-root",
            str(self.root / "oracle"),
            "--reference-python",
            sys.executable,
            "--report",
            str(self.report),
        ]
        if self.filename.startswith("native_catalog"):
            self.arguments += [
                "--native-binary",
                str(self.native_path),
                "--legacy-binary",
                str(self.legacy_path),
            ]

    def write_manifest(self):
        self.manifest_path.write_text(json.dumps(self.manifest))

    def run_driver(
        self, *, action=None, behavior=None, layout=None, groups=None, arguments=None
    ):
        original_sibling = self.driver.sibling
        provenance = types.SimpleNamespace(
            reference_provenance=lambda *_: copy.deepcopy(self.oracle)
        )

        def sibling(name):
            if name == "native_facade_qualification":
                return self.shared
            if name == "qualify_native_embedding":
                return provenance
            return original_sibling(name)

        def paired(command, **kwargs):
            engine = command[command.index("--scenario") + 1]
            values = dict.fromkeys(self.driver.EXPECTED_GROUPS, "fixture value")
            if groups:
                groups(values, engine)
            return subprocess.CompletedProcess(
                command,
                0,
                json.dumps(
                    {
                        "result": values,
                        "oracle": copy.deepcopy(self.oracle),
                        "calls": self.calls,
                        "imports": {"catabolic.app": "fixture"},
                        "diagnostics": {},
                    }
                ),
                "",
            )

        first_name = self.driver.EXPECTED_TESTS[0]

        def first(case):
            if action:
                action()
            if behavior == "skip":
                case.skipTest("synthetic skip")
            elif behavior == "failure":
                case.fail("synthetic failure")
            elif behavior == "error":
                raise ValueError("synthetic error")

        original_loader = unittest.defaultTestLoader.loadTestsFromTestCase

        def discover(cls):
            suite = original_loader(cls)
            if cls is self.case and layout:
                cases = list(suite)
                if layout == "empty":
                    cases = []
                elif layout == "missing":
                    cases = cases[:-1]
                elif layout == "duplicate":
                    cases[-1] = cases[0]
                return unittest.TestSuite(cases)
            return suite

        with contextlib.ExitStack() as stack:
            stack.enter_context(
                patch.dict(
                    sys.modules,
                    {"catabolic_native": self.native, "catabolic_native.cli": self.cli},
                )
            )
            stack.enter_context(
                patch.object(
                    sys,
                    "argv",
                    [
                        "driver",
                        *(arguments if arguments is not None else self.arguments),
                    ],
                )
            )
            stack.enter_context(
                patch.object(self.driver, "sibling", side_effect=sibling)
            )
            stack.enter_context(
                patch.object(
                    self.shared, "reference_runtime", return_value=self.runtime
                )
            )
            self.strict = stack.enter_context(
                patch.object(
                    self.shared, "strict_artifact", wraps=self.shared.strict_artifact
                )
            )
            stack.enter_context(
                patch("importlib.metadata.distribution", return_value=self.distribution)
            )
            stack.enter_context(
                patch.object(self.driver, "trace", return_value=self.calls)
            )
            if hasattr(self.driver, "installed_signatures"):
                stack.enter_context(
                    patch.object(
                        self.driver,
                        "installed_signatures",
                        return_value={"fixture": "signature"},
                    )
                )
            stack.enter_context(
                patch.object(self.driver.subprocess, "run", side_effect=paired)
            )
            stack.enter_context(
                patch.object(
                    unittest.defaultTestLoader,
                    "loadTestsFromTestCase",
                    side_effect=discover,
                )
            )
            cases = [self.case]
            if hasattr(self.driver, "CatalogRefreshCLI"):
                cases.append(self.driver.CatalogRefreshCLI)
            for case in cases:
                stack.enter_context(patch.object(case, "setUp", lambda _: None))
                for name in unittest.defaultTestLoader.getTestCaseNames(case):
                    stack.enter_context(
                        patch.object(
                            case,
                            name,
                            first
                            if case is self.case and name == first_name
                            else lambda _: None,
                        )
                    )
            if behavior == "expected_failure":
                stack.enter_context(
                    patch.object(
                        self.case,
                        first_name,
                        unittest.expectedFailure(lambda case: case.fail("fixture")),
                    )
                )
            if behavior == "unexpected_success":
                stack.enter_context(
                    patch.object(
                        self.case, first_name, unittest.expectedFailure(lambda _: None)
                    )
                )
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            self.driver.main()

    def test_explicit_helperless_development_mode_stays_development_only(self):
        for name in (
            "catabolic_native/bin/catabolic-native",
            "catabolic_native/native-helper.json",
            "catabolic_native/bin/catabolic-legacy-native",
            "catabolic_native/legacy-helper.json",
        ):
            del self.payloads[name]
            (self.installed / name).unlink()
            self.distribution.files.remove(Path(name))
        with zipfile.ZipFile(self.wheel, "w") as archive:
            for name, data in self.payloads.items():
                archive.writestr(name, data)
        self.manifest["profile"] = "development"
        self.manifest["wheel_sha256"] = digest(self.wheel.read_bytes())
        self.write_manifest()
        arguments = self.arguments.copy()
        if self.filename.startswith("native_component"):
            arguments[arguments.index("--wheel")] = "--dev-wheel"
        else:
            arguments.append("--dev")
        self.run_driver(arguments=arguments)
        receipt = json.loads(self.report.read_text())
        self.assertTrue(receipt["development_only"])
        self.assertEqual(len(receipt["successful_scenarios"]), self.test_count)
        self.strict.assert_not_called()
        with self.assertRaises(AssertionError):
            self.run_driver()
        self.assertFalse(self.report.exists())

    def test_normal_shared_helper_import_failure_removes_prior_success(self):
        (self.scripts / "native_facade_qualification.py").write_text(
            'raise RuntimeError("fixture import failure")\n'
        )
        run = subprocess.run(
            [sys.executable, "-I", str(self.scripts / self.filename), *self.arguments],
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("fixture import failure", run.stderr)
        self.assertFalse(self.report.exists())

    def test_helper_executable_mode_and_extra_installed_payload_refuse(self):
        self.helper_path.chmod(0o644)
        with self.assertRaisesRegex(AssertionError, "not executable"):
            self.run_driver()
        self.assertFalse(self.report.exists())
        self.helper_path.chmod(0o755)
        (self.installed / "catabolic_native/extra.py").write_text(
            "# unexpected installed payload"
        )
        with self.assertRaisesRegex(AssertionError, "missing from supplied wheel"):
            self.run_driver()
        self.assertFalse(self.report.exists())

    def test_fixed_inventory_and_exact_success_receipt(self):
        self.assertEqual(len(self.driver.EXPECTED_GROUPS), self.group_count)
        self.assertEqual(
            set(unittest.defaultTestLoader.getTestCaseNames(self.case)),
            set(self.driver.EXPECTED_TESTS),
        )
        if hasattr(self.driver, "CLI_TESTS"):
            self.assertEqual(len(self.driver.CLI_TESTS), 3)
            self.assertEqual(
                set(
                    unittest.defaultTestLoader.getTestCaseNames(
                        self.driver.CatalogRefreshCLI
                    )
                ),
                set(self.driver.CLI_TESTS),
            )
        self.run_driver()
        receipt = json.loads(self.report.read_text())
        self.assertTrue(receipt["passed"])
        self.assertFalse(receipt["development_only"])
        self.assertEqual(len(receipt["successful_scenarios"]), self.test_count)
        self.assertEqual(len(set(receipt["successful_scenarios"])), self.test_count)
        self.assertEqual(receipt["paired_groups"], self.group_count)
        self.assertEqual(len(receipt["coverage"]), self.row_count)
        self.assertEqual(self.strict.call_count, 2)
        self.assertNotIn("stale", receipt)

    def test_whole_installed_payload_metadata_and_binaries_are_fenced(self):
        targets = [
            self.installed / name
            for name in self.payloads
            if not name.endswith("/RECORD")
        ]
        targets += [
            self.wheel,
            self.native_path,
            self.legacy_path,
            self.manifest_path,
            self.inventory,
            self.changed_source,
            self.scripts / self.filename,
            self.scripts / "native_facade_qualification.py",
            self.scripts / "qualify_native_wheel.py",
        ]
        for path in targets:
            with self.subTest(path=path):
                before = path.read_bytes()
                try:
                    with self.assertRaises(
                        (
                            AssertionError,
                            ValueError,
                            SyntaxError,
                            KeyError,
                            zipfile.BadZipFile,
                        )
                    ):
                        self.run_driver(
                            action=lambda path=path, before=before: path.write_bytes(
                                before + b"\n# changed"
                            )
                        )
                    self.assertFalse(self.report.exists())
                finally:
                    path.write_bytes(before)

    def test_preflight_rejects_unrelated_legacy_cli_and_helperless_strict_wheel(self):
        before = self.legacy_path.read_bytes()
        self.legacy_path.write_bytes(b"an arbitrary old executable")
        with self.assertRaises(AssertionError):
            self.run_driver()
        self.assertFalse(self.report.exists())
        self.legacy_path.write_bytes(before)
        self.distribution.files.remove(Path("catabolic_native/bin/catabolic-native"))
        with self.assertRaises(AssertionError):
            self.run_driver()
        self.assertFalse(self.report.exists())

    def replace_payloads(self, payloads):
        for name in self.payloads:
            path = self.installed / name
            if name in payloads:
                path.write_bytes(payloads[name])
            else:
                path.unlink(missing_ok=True)
        self.distribution.files = [Path(name) for name in payloads]
        with zipfile.ZipFile(self.wheel, "w") as archive:
            for name, data in payloads.items():
                archive.writestr(name, data)
        self.manifest["wheel_sha256"] = digest(self.wheel.read_bytes())
        self.write_manifest()

    def test_release_wheel_requires_both_packaged_helpers_and_manifests(self):
        for name in (
            "catabolic_native/bin/catabolic-native",
            "catabolic_native/native-helper.json",
            "catabolic_native/bin/catabolic-legacy-native",
            "catabolic_native/legacy-helper.json",
        ):
            with self.subTest(missing=name):
                payloads = self.payloads.copy()
                del payloads[name]
                self.replace_payloads(payloads)
                with self.assertRaisesRegex(
                    AssertionError, "missing packaged native helper"
                ):
                    self.run_driver()
                self.assertFalse(self.report.exists())
                self.replace_payloads(self.payloads)
                self.helper_path.chmod(0o755)
                self.legacy_helper_path.chmod(0o755)

    def test_packaged_legacy_bytes_and_provenance_cannot_diverge_from_sidecar(self):
        name = "catabolic_native/legacy-helper.json"
        cases = [
            (
                "binary",
                "catabolic_native/bin/catabolic-legacy-native",
                b"unrelated executable",
            )
        ]
        for key in ("format", "version", "path", "sha256", "source_sha256", "build"):
            metadata = self.legacy_metadata | {key: "wrong"}
            cases.append((key, name, json.dumps(metadata).encode()))
        for label, name, value in cases:
            with self.subTest(field=label):
                self.replace_payloads(self.payloads | {name: value})
                with self.assertRaisesRegex(AssertionError, "release build"):
                    self.run_driver()
                self.assertFalse(self.report.exists())
        self.replace_payloads(self.payloads)
        self.legacy_helper_path.chmod(0o644)
        with self.assertRaisesRegex(AssertionError, "not executable"):
            self.run_driver()
        self.assertFalse(self.report.exists())

    def test_legacy_accessor_must_return_the_packaged_helper(self):
        with patch.object(self.cli, "legacy_binary", return_value=self.legacy_path):
            with self.assertRaisesRegex(AssertionError, "not the packaged helper"):
                self.run_driver()
        self.assertFalse(self.report.exists())

    def test_release_verifier_refuses_packaged_python_caches(self):
        for name in (
            "catabolic_native/__pycache__/access.cpython-314.pyc",
            "catabolic_native/__pycache__/unrelated.txt",
            "catabolic_native/subpackage/__pycache__/",
            "catabolic_native/legacy.pyc",
            "catabolic_native/legacy.pyo",
        ):
            with self.subTest(name=name):
                with zipfile.ZipFile(self.wheel, "w") as archive:
                    for member, payload in self.payloads.items():
                        archive.writestr(member, payload)
                    archive.writestr(name, b"unqualified cache")
                self.manifest["wheel_sha256"] = digest(self.wheel.read_bytes())
                self.write_manifest()
                with self.assertRaisesRegex(
                    AssertionError, "must not package Python bytecode or cache files"
                ):
                    self.run_driver()
                self.assertFalse(self.report.exists())

    def test_release_manifest_identity_fields_are_required(self):
        for key in (
            "format",
            "profile",
            "source_sha256",
            "source_sha256_after",
            "wheel_sha256",
            "extension_sha256",
            "native_binary_sha256",
            "legacy_binary_sha256",
            "build_script_sha256",
        ):
            original = self.manifest[key]
            with self.subTest(key=key):
                self.manifest[key] = "wrong"
                self.write_manifest()
                with self.assertRaises(AssertionError):
                    self.run_driver()
                self.assertFalse(self.report.exists())
                self.manifest[key] = original
                self.write_manifest()

    def test_skipped_failed_and_incomplete_cases_never_write_success(self):
        for behavior in (
            "skip",
            "failure",
            "error",
            "expected_failure",
            "unexpected_success",
        ):
            with self.subTest(behavior=behavior), self.assertRaises(RuntimeError):
                self.run_driver(behavior=behavior)
            self.assertFalse(self.report.exists())
        for layout in ("empty", "missing", "duplicate"):
            with self.subTest(layout=layout), self.assertRaises(RuntimeError):
                self.run_driver(layout=layout)
            self.assertFalse(self.report.exists())

    def test_equal_but_missing_extra_or_wrong_paired_groups_refuse(self):
        for change in (
            lambda values: values.pop(next(iter(values))),
            lambda values: values.update(extra="fixture"),
        ):
            with self.assertRaisesRegex(AssertionError, "fixed paired groups"):
                self.run_driver(
                    groups=lambda values, engine, change=change: change(values)
                )
            self.assertFalse(self.report.exists())
        with self.assertRaises(AssertionError):
            self.run_driver(
                groups=lambda values, engine: values.update(
                    {next(iter(values)): engine}
                )
            )
        self.assertFalse(self.report.exists())

    def test_inventory_wrong_commit_duplicate_or_missing_selected_rows_refuse(self):
        before = self.inventory.read_bytes()
        for mutation in ("commit", "duplicate", "missing"):
            data = json.loads(before)
            if mutation == "commit":
                data["reference_commit"] = "0" * 40
            elif mutation == "duplicate":
                data["rows"].append(self.rows[0])
            else:
                data["rows"] = [
                    row for row in data["rows"] if row["id"] != self.rows[0]["id"]
                ]
            self.inventory.write_text(json.dumps(data))
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                self.run_driver()
            self.assertFalse(self.report.exists())
        self.inventory.write_bytes(before)

    def test_import_identity_substitution_refuses(self):
        extension = self.native._native
        with self.assertRaisesRegex(AssertionError, "extension identity"):
            self.run_driver(
                action=lambda: setattr(
                    self.native,
                    "_native",
                    types.SimpleNamespace(__file__=extension.__file__),
                )
            )
        self.assertFalse(self.report.exists())
        self.native._native = extension

    def test_malformed_arguments_clear_old_success_before_import(self):
        for arguments in (
            [],
            ["--unknown"],
            ["--wheel", str(self.wheel), "--dev-wheel", str(self.wheel)],
        ):
            self.report.write_text('{"passed": true, "stale": true}')
            with self.assertRaises((SystemExit, AssertionError)):
                self.run_driver(arguments=[*arguments, "--report", str(self.report)])
            self.assertFalse(self.report.exists())
            self.strict.assert_not_called()

    def test_all_optimized_entry_modes_clear_receipts_before_import(self):
        for flags, optimize in (
            (["-O"], None),
            (["-OO"], None),
            (["-I", "-O"], None),
            (["-I"], "1"),
            (["-I"], "0"),
        ):
            env = dict(os.environ)
            env.pop("PYTHONOPTIMIZE", None)
            if optimize is not None:
                env["PYTHONOPTIMIZE"] = optimize
            self.report.write_text("stale success")
            attempt = self.report.with_suffix(".attempt.json")
            attempt.write_text("stale attempt")
            run = subprocess.run(
                [
                    sys.executable,
                    *flags,
                    str(self.scripts / self.filename),
                    *self.arguments,
                ],
                env=env,
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertNotEqual(run.returncode, 0)
            self.assertIn("requires", run.stderr)
            self.assertIn("assertions", run.stderr)
            self.assertEqual(run.stdout, "")
            self.assertFalse(self.report.exists())
            self.assertFalse(attempt.exists())


class ComponentPolicyWheelChecks(FacadeDriverChecks, unittest.TestCase):
    def test_source_manifest_accepts_equivalent_checkout_alias(self):
        alias = self.root / "checkout-alias"
        alias.symlink_to(self.source.resolve(), target_is_directory=True)
        aliased = load(alias / "scripts" / self.filename)
        self.assertEqual(aliased.source_manifest(), self.driver.source_manifest())

    filename = "native_component_policy_embedding_tests.py"
    classname = "InstalledComponentPolicies"
    test_count, group_count, row_count = 13, 37, 10


class CatalogRefreshWheelChecks(FacadeDriverChecks, unittest.TestCase):
    filename = "native_catalog_refresh_embedding_tests.py"
    classname = "InstalledCatalogRefresh"
    test_count, group_count, row_count = 16, 43, 8


class ProvenanceHelpers(unittest.TestCase):
    def test_pinned_runtime_and_venv_executable_are_preserved(self):
        shared = load(SCRIPTS / "native_facade_qualification.py")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "python"
            path.symlink_to(sys.executable)
            identity = {
                "version": [3, 14, 6],
                "executable": str(path),
                "prefix": directory,
                "base_prefix": "/base",
            }
            with patch.object(
                shared.subprocess,
                "run",
                return_value=subprocess.CompletedProcess(
                    [], 0, json.dumps(identity), ""
                ),
            ) as run:
                self.assertEqual(
                    shared.reference_runtime(path)["executable"], str(path)
                )
                self.assertEqual(run.call_args.args[0][0], str(path))
                for bad in (
                    {**identity, "version": [3, 14, 5]},
                    {**identity, "executable": sys.executable},
                ):
                    run.return_value.stdout = json.dumps(bad)
                    with self.assertRaises(AssertionError):
                        shared.reference_runtime(path)

    def test_catalog_cli_uses_isolated_frozen_bootstrap_and_bound_legacy_binary(self):
        driver = load(SCRIPTS / "native_catalog_refresh_embedding_tests.py")
        options = types.SimpleNamespace(
            reference_python=Path("/venv/bin/python"),
            reference_root=Path("/oracle"),
            oracle_manifest=Path("/attempt.json"),
            oracle_manifest_sha256="a" * 64,
            legacy_binary=Path("/artifacts/catabolic-legacy-native"),
        )
        case = driver.CatalogRefreshCLI(driver.CLI_TESTS[0])
        case.paths = {
            engine: Path("/tmp") / (engine + ".db")
            for engine in ("reference", "native")
        }
        with patch.object(driver, "OPTIONS", options):
            reference = case.command("reference", ["pending"])
            self.assertEqual(reference[:3], ["/venv/bin/python", "-I", "-B"])
            self.assertIn("--oracle-sha256", reference)
            self.assertEqual(
                case.command("native", ["pending"])[0], str(options.legacy_binary)
            )


class PairedBuilderChecks(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.script = self.root / "scripts/build_native_wheel.py"
        self.script.parent.mkdir()
        shutil.copyfile(SCRIPTS / self.script.name, self.script)
        self.builder = load(self.script)
        (self.root / "Cargo.toml").write_text(
            '[workspace.package]\nversion = "0.2.0"\n'
        )
        self.source = self.root / "crates/fixture/src/lib.rs"
        self.source.parent.mkdir(parents=True)
        self.source.write_text("// fixture source\n")
        self.package = self.root / "crates/catabolic-python/python/catabolic_native"
        self.package.mkdir(parents=True)
        self.native = self.root / "native"
        self.native.write_bytes(b"fixture native release binary")
        self.legacy = self.root / "legacy"
        self.legacy.write_bytes(b"fixture legacy release binary")
        self.out = self.root / "out"
        self.out.mkdir()
        self.manifest = self.out / "build-manifest.json"
        self.manifest.write_text('{"passed": true, "stale": true}')

    def run_builder(
        self,
        *,
        mutate=None,
        missing=None,
        maturin="maturin 1.13.1",
        wheel_missing=None,
        wheel_change=None,
        extra_payload=None,
        entry_points="[console_scripts]\ncatabolic-native = catabolic_native:main\n",
        duplicate_entry_points=False,
    ):
        self.commands = []

        def check_output(command, **kwargs):
            if "maturin" in command:
                return maturin
            if command[0] in ("rustc", "cargo"):
                return command[0] + " fixture version"
            return "Python 3.14.6"

        def run(command, **kwargs):
            self.commands.append(command)
            if command[0] == "cargo":
                artifacts = [
                    {
                        "reason": "compiler-artifact",
                        "target": {"name": name},
                        "executable": str(path),
                    }
                    for name, path in (
                        ("catabolic-native", self.native),
                        ("catabolic-legacy-native", self.legacy),
                    )
                    if name != missing
                ]
                return subprocess.CompletedProcess(
                    command, 0, "\n".join(map(json.dumps, artifacts))
                )
            wheel = Path(command[command.index("--out") + 1]) / "fixture.whl"
            with zipfile.ZipFile(wheel, "w") as archive:
                for name in (
                    "bin/catabolic-native",
                    "native-helper.json",
                    "bin/catabolic-legacy-native",
                    "legacy-helper.json",
                ):
                    if name == wheel_missing:
                        continue
                    path = self.package / name
                    if wheel_change and name == wheel_change[0]:
                        if wheel_change[1] == "mode":
                            path.chmod(0o644)
                        else:
                            path.write_bytes(wheel_change[1])
                    archive.write(path, "catabolic_native/" + name)
                archive.writestr(
                    "catabolic_native/_native.abi3.so", b"fixture native extension"
                )
                if entry_points is not None:
                    archive.writestr(
                        "catabolic_native-0.2.0.dist-info/entry_points.txt",
                        entry_points,
                    )
                    if duplicate_entry_points:
                        archive.writestr(
                            "catabolic_native-0.2.0.dist-info/entry_points.txt",
                            entry_points,
                        )
                if extra_payload:
                    archive.writestr(extra_payload, b"disposable payload")
            if mutate:
                mutate()
            return subprocess.CompletedProcess(command, 0)

        captured = io.StringIO()
        with (
            patch.object(
                sys,
                "argv",
                ["build", "--python", sys.executable, "--out", str(self.out)],
            ),
            patch.object(
                self.builder.subprocess, "check_output", side_effect=check_output
            ),
            patch.object(self.builder.subprocess, "run", side_effect=run),
            contextlib.redirect_stdout(captured),
        ):
            self.builder.main()
        return json.loads(captured.getvalue())

    def test_both_cli_artifacts_and_extension_share_release_sidecar(self):
        result = self.run_builder()
        manifest = json.loads(self.manifest.read_text())
        self.assertEqual(manifest["profile"], "release")
        self.assertEqual(manifest["source_sha256"], manifest["source_sha256_after"])
        self.assertEqual(
            manifest["source_sha256"], self.builder.source_digest(self.root)
        )
        for name in ("catabolic-native", "catabolic-legacy-native"):
            self.assertIn(name, self.commands[0])
            self.assertTrue(os.access(self.out / name, os.X_OK))
        for field in (
            "wheel",
            "wheel_sha256",
            "native_helper_sha256",
            "native_helper_bytes",
            "legacy_helper_sha256",
            "legacy_helper_bytes",
            "publication",
        ):
            self.assertIn(field, result)
        self.assertEqual(
            manifest["native_binary_sha256"], result["native_helper_sha256"]
        )
        self.assertEqual(
            manifest["legacy_binary_sha256"],
            digest((self.out / "catabolic-legacy-native").read_bytes()),
        )
        self.assertEqual(
            manifest["build_script_sha256"], digest(self.script.read_bytes())
        )
        with zipfile.ZipFile(self.out / manifest["wheel"]) as archive:
            self.assertEqual(
                manifest["extension_sha256"],
                digest(archive.read("catabolic_native/_native.abi3.so")),
            )
            for helper, filename, field in (
                ("catabolic-native", "native-helper.json", "native_binary_sha256"),
                (
                    "catabolic-legacy-native",
                    "legacy-helper.json",
                    "legacy_binary_sha256",
                ),
            ):
                self.assertEqual(
                    digest(archive.read("catabolic_native/bin/" + helper)),
                    manifest[field],
                )
                self.assertTrue(
                    archive.getinfo("catabolic_native/bin/" + helper).external_attr
                    >> 16
                    & 0o111
                )
                self.assertEqual(
                    json.loads(archive.read("catabolic_native/" + filename)),
                    {
                        "format": 1,
                        "version": "0.2.0",
                        "path": "bin/" + helper,
                        "sha256": manifest[field],
                        "source_sha256": manifest["source_sha256"],
                        "build": manifest["build"],
                    },
                )
        self.assertEqual(manifest["publication"], "local-only")

    def test_missing_cli_or_wrong_toolchain_cannot_retain_success_sidecar(self):
        for name in ("catabolic-native", "catabolic-legacy-native"):
            self.manifest.write_text('{"passed": true}')
            with self.subTest(missing=name), self.assertRaises(SystemExit):
                self.run_builder(missing=name)
            self.assertFalse(self.manifest.exists())
        self.manifest.write_text('{"passed": true}')
        with self.assertRaises(SystemExit):
            self.run_builder(maturin="maturin 0.0.0")
        self.assertFalse(self.manifest.exists())

    def test_missing_packaged_helper_or_manifest_cannot_emit_success_sidecar(self):
        for name in (
            "bin/catabolic-native",
            "native-helper.json",
            "bin/catabolic-legacy-native",
            "legacy-helper.json",
        ):
            self.manifest.write_text('{"passed": true}')
            with (
                self.subTest(missing=name),
                self.assertRaisesRegex(SystemExit, "exactly one"),
            ):
                self.run_builder(wheel_missing=name)
            self.assertFalse(self.manifest.exists())
            self.assertEqual(list(self.out.glob("*.whl")), [])

    def test_changed_packaged_bytes_modes_and_metadata_cannot_emit_success_sidecar(
        self,
    ):
        for name, value, error in (
            ("bin/catabolic-native", b"changed", "differs from the Cargo artifact"),
            (
                "bin/catabolic-legacy-native",
                b"changed",
                "differs from the Cargo artifact",
            ),
            ("bin/catabolic-native", "mode", "executable permissions"),
            ("bin/catabolic-legacy-native", "mode", "executable permissions"),
            ("native-helper.json", b"{}", "manifest differs"),
            ("legacy-helper.json", b"{}", "manifest differs"),
        ):
            self.manifest.write_text('{"passed": true}')
            with (
                self.subTest(payload=name, value=value),
                self.assertRaisesRegex(SystemExit, error),
            ):
                self.run_builder(wheel_change=(name, value))
            self.assertFalse(self.manifest.exists())
            self.assertEqual(list(self.out.glob("*.whl")), [])

    def test_python_cache_payload_cannot_emit_success_sidecar(self):
        for name in (
            "catabolic_native/__pycache__/access.cpython-314.pyc",
            "catabolic_native/__pycache__/unrelated.txt",
            "catabolic_native/subpackage/__pycache__/",
            "catabolic_native/legacy.pyc",
            "catabolic_native/legacy.pyo",
        ):
            with self.subTest(name=name):
                self.manifest.write_text('{"passed": true}')
                with self.assertRaisesRegex(SystemExit, "Python bytecode or cache"):
                    self.run_builder(extra_payload=name)
                self.assertFalse(self.manifest.exists())
                self.assertEqual(list(self.out.glob("*.whl")), [])

    def test_similar_noncache_data_is_not_misclassified(self):
        result = self.run_builder(extra_payload="catabolic_native/data.pyc.txt")
        self.assertEqual(result["publication"], "local-only")
        self.assertTrue(self.manifest.is_file())

    def test_launcher_requires_exact_console_script_name_and_target(self):
        for metadata in (
            "[console_scripts]\nother = catabolic_native:main\n",
            "[gui_scripts]\ncatabolic-native = catabolic_native:main\n",
            "[console_scripts]\n# catabolic-native = catabolic_native:main\n",
            "[console_scripts]\ncatabolic-native = catabolic_native:main_other\n",
            "[console_scripts]\ncatabolic-native = other:main\n"
            "other = catabolic_native:main\n",
            "[console_scripts]\nCatabolic-Native = catabolic_native:main\n",
            "[DEFAULT]\ncatabolic-native = catabolic_native:main\n[console_scripts]\n",
            "[console_scripts]\ncatabolic-native = catabolic_native:main [extra]\n",
        ):
            with self.subTest(metadata=metadata):
                self.manifest.write_text('{"passed": true}')
                with self.assertRaisesRegex(SystemExit, "launcher"):
                    self.run_builder(entry_points=metadata)
                self.assertFalse(self.manifest.exists())
                self.assertEqual(list(self.out.glob("*.whl")), [])

    def test_missing_or_duplicate_launcher_metadata_cannot_emit_success_sidecar(self):
        import warnings

        for options in (
            {"entry_points": None},
            {"duplicate_entry_points": True},
        ):
            with self.subTest(options=options), warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                self.manifest.write_text('{"passed": true}')
                with self.assertRaisesRegex(SystemExit, "launcher"):
                    self.run_builder(**options)
                self.assertFalse(self.manifest.exists())
                self.assertEqual(list(self.out.glob("*.whl")), [])

    def test_malformed_or_ambiguous_launcher_metadata_cannot_emit_success_sidecar(self):
        for metadata in (
            b"[console_scripts]\ncatabolic-native = catabolic_native:main\n\xff",
            "[console_scripts]\ncatabolic-native = catabolic_native:main\n"
            "catabolic-native = catabolic_native:main\n",
            "[console_scripts]\ncatabolic-native = catabolic_native:main\n"
            "[console_scripts]\ncatabolic-native = catabolic_native:main\n",
            "catabolic-native = catabolic_native:main\n",
        ):
            with self.subTest(metadata=metadata):
                self.manifest.write_text('{"passed": true}')
                with self.assertRaisesRegex(SystemExit, "launcher"):
                    self.run_builder(entry_points=metadata)
                self.assertFalse(self.manifest.exists())
                self.assertEqual(list(self.out.glob("*.whl")), [])

    def test_exact_launcher_accepts_standard_spacing_and_unrelated_entries(self):
        result = self.run_builder(
            entry_points="# native companion\n[console_scripts]\n"
            "catabolic-native  =  catabolic_native:main\n"
            "other = other:main\n"
        )
        self.assertEqual(result["publication"], "local-only")
        self.assertTrue(self.manifest.is_file())

    def test_source_builder_or_cli_changes_during_build_refuse(self):
        for path in (self.source, self.script, self.native, self.legacy):
            before = path.read_bytes()
            self.manifest.write_text('{"passed": true}')
            try:
                with self.subTest(path=path), self.assertRaises(SystemExit):
                    self.run_builder(
                        mutate=lambda path=path: path.write_bytes(
                            b"changed during build"
                        )
                    )
                self.assertFalse(self.manifest.exists())
            finally:
                path.write_bytes(before)


if __name__ == "__main__":
    unittest.main()
