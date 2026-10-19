# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Installed-ASGI gate controls only; no route, auth or content replicas."""

import argparse
import importlib.util
import io
import json
import sys
import tempfile
import types
import unittest
import zipfile
from contextlib import ExitStack, redirect_stderr
from pathlib import Path
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]


def load(name):
    spec = importlib.util.spec_from_file_location(
        name, ROOT / "scripts" / (name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


GATE = load("native_asgi_installed_tests")
DEPS = load("native_asgi_dependencies")
SCENARIOS = load("native_asgi_scenarios")
RESULTS = load("native_qualification_results")


class InstalledGateControls(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_exact_declared_lane_counts(self):
        self.assertEqual(len(SCENARIOS.REQUIRED_IDS), 21)
        self.assertEqual(len(SCENARIOS.SMOKE_IDS), 6)
        self.assertEqual(len(set(SCENARIOS.REQUIRED_IDS)), 21)
        self.assertTrue(set(SCENARIOS.SMOKE_IDS) < set(SCENARIOS.REQUIRED_IDS))
        SCENARIOS.validate_sources(ROOT)

    def test_lane_refuses_nonisolated_wrong_runtime_and_optional_packages(self):
        with patch.object(GATE.sys, "flags", types.SimpleNamespace(isolated=0)):
            with self.assertRaisesRegex(RuntimeError, "requires -I"):
                GATE.lane_preflight("full", self.root)
        with patch.object(GATE.sys, "flags", types.SimpleNamespace(isolated=1)):
            with patch.object(GATE.sys, "version_info", (3, 11, 15)):
                with self.assertRaisesRegex(RuntimeError, "3.14.6"):
                    GATE.lane_preflight("full", self.root)
                with patch.object(
                    GATE.importlib.util, "find_spec", return_value=object()
                ):
                    with self.assertRaisesRegex(RuntimeError, "optional dependency"):
                        GATE.lane_preflight("minimal", None)
                with self.assertRaisesRegex(RuntimeError, "must not install"):
                    GATE.lane_preflight("minimal", self.root)
            with patch.object(GATE.sys, "version_info", (3, 14, 6)):
                with self.assertRaisesRegex(RuntimeError, "exact dependency wheels"):
                    GATE.lane_preflight("full", None)
                with self.assertRaisesRegex(RuntimeError, "3.11"):
                    GATE.lane_preflight("minimal", None)

    def harness(
        self,
        *,
        artifact_error=None,
        artifact_drift=False,
        source_drift=False,
        reference_drift=False,
        runtime_drift=False,
        dependency_drift=False,
        skip=False,
        duplicate=False,
        missing=False,
        full=False,
    ):
        """Exercise actual gate sequencing; synthetic cases test the gate only."""
        selected = (
            SCENARIOS.REQUIRED_CASES
            if full
            else {
                "tests.test_rust_asgi_smoke.InstalledASGISmoke": SCENARIOS.REQUIRED_CASES[
                    "tests.test_rust_asgi_smoke.InstalledASGISmoke"
                ]
            }
        )
        suite = unittest.TestSuite()
        expected = {}
        for class_name, names in selected.items():
            methods = {name: (lambda self: None) for name in names}
            if skip:
                methods[names[0]] = unittest.skip("negative control")(lambda self: None)
            module_name, _, attribute = class_name.rpartition(".")
            case = type(attribute, (unittest.TestCase,), methods)
            case.__module__ = module_name
            expected[case] = names
            suite.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(case))
        if duplicate:
            suite.addTest(case(names[0]))
        if missing:
            suite = unittest.TestSuite(list(suite)[:-1])
        oracle = {"tracked_source_sha256": {}}
        artifact = {"installed_wheel": {"installed_payloads_sha256": {}}}
        strict = Mock(
            side_effect=([artifact, {}] if artifact_drift else None),
            return_value=artifact,
        )
        if artifact_error:
            strict.side_effect = RuntimeError(artifact_error)
        identity = types.SimpleNamespace(
            package=self.root / "catabolic_native/__init__.py", verify=Mock()
        )
        qualification = types.SimpleNamespace(
            FROZEN_COMMIT="frozen",
            NativeIdentity=Mock(return_value=identity),
            strict_artifact=strict,
            reference_runtime=Mock(
                side_effect=[{}, {"changed": True}] if runtime_drift else None,
                return_value={},
            ),
        )
        provenance = types.SimpleNamespace(
            reference_provenance=Mock(
                side_effect=[oracle, {}] if reference_drift else None,
                return_value=oracle,
            )
        )
        deps = types.SimpleNamespace(
            dependency_snapshot=Mock(
                side_effect=[{}, {"changed": True}] if dependency_drift else None,
                return_value={},
            )
        )
        modules = dict(
            native_facade_qualification=qualification,
            qualify_native_embedding=provenance,
            native_asgi_dependencies=deps,
            native_asgi_scenarios=SCENARIOS,
            native_qualification_results=RESULTS,
        )
        args = argparse.Namespace(
            source_root=ROOT,
            lane="full" if full else "minimal",
            dependency_wheelhouse=self.root if full else None,
            reference_root=self.root,
            reference_python=Path(sys.executable),
            wheel=self.root / "wheel",
            build_manifest=self.root / "build",
            native_binary=self.root / "native",
            legacy_binary=self.root / "legacy",
            report=self.root / "report.json",
        )
        report = {"passed": False}
        import_guard = Mock(
            return_value=types.SimpleNamespace(snapshot=Mock(return_value={}))
        )
        with ExitStack() as stack:
            stack.enter_context(
                patch.object(GATE, "sibling", side_effect=modules.__getitem__)
            )
            stack.enter_context(patch.object(GATE, "lane_preflight"))
            stack.enter_context(patch.object(GATE.Path, "cwd", return_value=self.root))
            stack.enter_context(
                patch.object(
                    GATE,
                    "source_fence",
                    side_effect=[{"files": {}}, {"changed": True}]
                    if source_drift
                    else None,
                    return_value={"files": {}},
                )
            )
            stack.enter_context(patch.object(GATE, "runtime_identity", return_value={}))
            stack.enter_context(patch.object(GATE, "ImportedBytes", import_guard))
            stack.enter_context(
                patch.object(GATE, "required_suite", return_value=(suite, expected))
            )
            stack.enter_context(redirect_stderr(io.StringIO()))
            # Keep interpreter state untouched by this control harness.
            stack.enter_context(patch.object(GATE.sys, "path", GATE.sys.path.copy()))
            stack.enter_context(patch.dict(GATE.os.environ, GATE.os.environ.copy()))
            stack.enter_context(patch.object(GATE.sys, "pycache_prefix", None))
            stack.enter_context(patch.object(GATE.sys, "dont_write_bytecode", False))
            GATE.run_gate(args, report)
        self.assertEqual(import_guard.call_args.args[4:], (ROOT, {}))
        return report, strict

    def test_gate_uses_release_artifacts_twice_and_exact_six_successes(self):
        report, strict = self.harness()
        self.assertTrue(report["passed"])
        self.assertEqual(report["tests_run"], 6)
        self.assertEqual(set(report["successful_scenarios"]), set(SCENARIOS.SMOKE_IDS))
        self.assertEqual(strict.call_count, 2)
        self.assertEqual(strict.call_args_list[0], strict.call_args_list[1])

    def test_full_gate_requires_twenty_one_successes_and_dependency_fences(self):
        report, strict = self.harness(full=True)
        self.assertEqual(report["tests_run"], 21)
        self.assertEqual(
            set(report["successful_scenarios"]), set(SCENARIOS.REQUIRED_IDS)
        )
        self.assertEqual(strict.call_count, 2)
        with self.assertRaisesRegex(
            RuntimeError, "dependency wheel or installed bytes changed"
        ):
            self.harness(full=True, dependency_drift=True)

    def test_gate_refuses_helperless_debug_artifact_and_changed_fences(self):
        for options, message in (
            ({"artifact_error": "missing packaged native helper"}, "missing packaged"),
            ({"artifact_drift": True}, "artifact changed"),
            ({"source_drift": True}, "source changed"),
            ({"reference_drift": True}, "reference changed"),
            ({"runtime_drift": True}, "interpreter changed"),
        ):
            with (
                self.subTest(options=options),
                self.assertRaisesRegex(RuntimeError, message),
            ):
                self.harness(**options)

    def test_gate_does_not_count_skip_duplicate_or_missing_scenario(self):
        for option in ("skip", "duplicate", "missing"):
            with self.subTest(option=option), self.assertRaises(RuntimeError):
                self.harness(**{option: True})

    def test_main_retains_attempt_but_no_success_after_failure(self):
        args = [
            "--lane",
            "minimal",
            "--wheel",
            "unused",
            "--source-root",
            str(ROOT),
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
            "--report",
            str(self.root / "report.json"),
        ]
        with (
            patch.object(sys, "argv", ["gate", *args]),
            patch.object(
                GATE, "run_gate", side_effect=RuntimeError("negative control")
            ),
            patch("sys.stdout", new_callable=io.StringIO),
        ):
            self.assertEqual(GATE.main(), 1)
        self.assertFalse((self.root / "report.json").exists())
        attempt = json.loads((self.root / "report.attempt.json").read_text())
        self.assertFalse(attempt["passed"])
        self.assertIn("negative control", attempt["error"])

    def test_real_import_guard_accepts_fenced_main_and_refuses_foreign_source(self):
        path = ROOT / "scripts/native_asgi_installed_tests.py"
        main = types.ModuleType("__main__")
        main.__file__ = str(path)
        identity = types.SimpleNamespace(
            package=self.root / "catabolic_native/__init__.py", payloads={}
        )
        guard = GATE.ImportedBytes(
            identity,
            {"tracked_source_sha256": {}},
            {},
            self.root,
            ROOT,
            {"scripts/native_asgi_installed_tests.py": GATE.digest(path)},
        )
        with patch.dict(sys.modules, {"__main__": main}, clear=True):
            self.assertIn("__main__", guard.snapshot())
            foreign = types.ModuleType("foreign_fixture")
            foreign.__file__ = str(self.root / "foreign.py")
            Path(foreign.__file__).write_text("VALUE = 1")
            sys.modules[foreign.__name__] = foreign
            with self.assertRaisesRegex(RuntimeError, "unqualified import"):
                guard.snapshot()

    def test_imported_bytes_reject_source_drift_and_identity_replacement(self):
        root = self.root
        path = root / "catabolic_native/fixture.py"
        path.parent.mkdir()
        path.write_text("x = 1\n")
        identity = types.SimpleNamespace(
            package=path.parent / "__init__.py",
            payloads={"catabolic_native/fixture.py": GATE.digest(path)},
        )
        module = types.ModuleType("catabolic_native.fixture")
        module.__file__ = str(path)
        with patch.dict(sys.modules, {"catabolic_native.fixture": module}):
            imports = GATE.ImportedBytes(
                identity, {"tracked_source_sha256": {}}, {}, root
            )
            # Test only our package rather than ambient third-party test tooling.
            with patch.dict(
                sys.modules, {"catabolic_native.fixture": module}, clear=True
            ):
                imports.snapshot()
                path.write_text("x = 2\n")
                with self.assertRaisesRegex(RuntimeError, "bytes changed"):
                    imports.snapshot()
                path.write_text("x = 1\n")
                replacement = types.ModuleType(module.__name__)
                replacement.__file__ = str(path)
                sys.modules[module.__name__] = replacement
                with self.assertRaisesRegex(RuntimeError, "identity changed"):
                    imports.snapshot()


class DependencyWheelControls(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.installed = self.root / "installed"
        self.installed.mkdir()
        self.wheel = self.root / "sample-1.0-py3-none-any.whl"
        self.payloads = {
            "sample/__init__.py": b"VALUE = 1\n",
            "sample-1.0.dist-info/METADATA": b"Name: sample\nVersion: 1.0\n",
            "sample-1.0.dist-info/WHEEL": b"Wheel-Version: 1.0\nTag: py3-none-any\n",
        }
        self.write_wheel()
        for name, payload in self.payloads.items():
            path = self.installed / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(payload)
        self.distribution = types.SimpleNamespace(
            version="1.0",
            files=list(self.payloads) + ["sample-1.0.dist-info/RECORD"],
            locate_file=lambda name: self.installed / name,
        )

    def write_wheel(self):
        with zipfile.ZipFile(self.wheel, "w") as archive:
            for name, payload in self.payloads.items():
                archive.writestr(name, payload)
            archive.writestr("sample-1.0.dist-info/RECORD", "")

    def verify(self):
        with patch.object(
            DEPS.importlib.metadata, "distribution", return_value=self.distribution
        ):
            return DEPS.verify_dependency_wheel(self.wheel, "sample", "1.0")

    def test_exact_archive_payloads_and_metadata(self):
        report = self.verify()
        self.assertEqual(report["wheel_sha256"], GATE.digest(self.wheel))
        self.assertEqual(set(report["payloads_sha256"]), set(self.payloads))
        self.assertEqual(
            DEPS.wheel_metadata(self.wheel), ("sample", "1.0", self.wheel.name)
        )

    def test_changed_missing_extra_linked_installed_payloads_refused(self):
        path = self.installed / "sample/__init__.py"
        path.write_text("changed")
        with self.assertRaisesRegex(ValueError, "bytes differ"):
            self.verify()
        path.unlink()
        with self.assertRaisesRegex(ValueError, "bytes differ"):
            self.verify()
        path.write_bytes(self.payloads["sample/__init__.py"])
        extra = path.parent / "unrecorded.py"
        extra.write_text("x = 1")
        with self.assertRaisesRegex(ValueError, "unrecorded"):
            self.verify()
        extra.unlink()
        path.unlink()
        path.symlink_to(self.wheel)
        with self.assertRaisesRegex(ValueError, "linked"):
            self.verify()

    def test_unknown_missing_duplicate_and_unlocked_wheels_refused(self):
        locked = {"sample": {"version": "1.0", "sha256": [DEPS.digest(self.wheel)]}}
        with (
            patch.object(DEPS, "locked_dependencies", return_value=locked),
            patch.object(
                DEPS.importlib.metadata, "distribution", return_value=self.distribution
            ),
        ):
            DEPS.dependency_snapshot(ROOT, self.root)
            extra = self.root / "duplicate.whl"
            extra.write_bytes(self.wheel.read_bytes())
            with self.assertRaisesRegex(ValueError, "duplicate"):
                DEPS.dependency_snapshot(ROOT, self.root)
            extra.unlink()
            self.wheel.unlink()
            with self.assertRaisesRegex(ValueError, "missing"):
                DEPS.dependency_snapshot(ROOT, self.root)
            self.write_wheel()
            self.payloads["sample/__init__.py"] = b"changed"
            self.write_wheel()
            with self.assertRaisesRegex(ValueError, "unlocked"):
                DEPS.dependency_snapshot(ROOT, self.root)

    def test_export_pip_http_body_and_wheel_cache_without_network(self):
        locked = {"sample": {"version": "1.0", "sha256": [DEPS.digest(self.wheel)]}}
        cache = self.root / "cache"
        cache.mkdir()
        (cache / "opaque.body").write_bytes(self.wheel.read_bytes())
        (cache / "metadata").write_text("not a wheel")
        with (
            patch.object(DEPS, "locked_dependencies", return_value=locked),
            patch.object(
                DEPS.importlib.metadata, "distribution", return_value=self.distribution
            ),
        ):
            output = DEPS.export_cached(ROOT, cache, self.root / "export")
            self.assertEqual(set(output), {"sample"})
            self.assertEqual(
                (self.root / "export" / self.wheel.name).read_bytes(),
                self.wheel.read_bytes(),
            )
            with self.assertRaisesRegex(ValueError, "must be empty"):
                DEPS.export_cached(ROOT, cache, self.root / "export")
            (cache / "opaque.body").unlink()
            with self.assertRaisesRegex(ValueError, "lacks exact"):
                DEPS.export_cached(ROOT, cache, self.root / "missing")

    def test_locked_distribution_set_is_exact(self):
        locked = DEPS.locked_dependencies(ROOT)
        self.assertEqual(len(locked), 18)
        self.assertEqual(locked["httpx"]["version"], "0.28.1")
        self.assertEqual(len(locked["httpx"]["sha256"]), 1)


if __name__ == "__main__":
    unittest.main()
