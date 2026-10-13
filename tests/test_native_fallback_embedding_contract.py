"""Exact selected fallback facade signatures and development evidence contracts."""

import ast
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
FACADE = ROOT / "crates/catabolic-python/python/catabolic_native"
SCRIPT = ROOT / "scripts/native_fallback_embedding_tests.py"


def gate():
    spec = importlib.util.spec_from_file_location("fallback_contracts", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FallbackFacadeContracts(unittest.TestCase):
    def test_exact_frozen_selected_rows_and_signatures(self):
        module = gate()
        inventory = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        selected = {
            "python:catabolic." + name
            for name in (
                "fallback_resolution.Resolver",
                "fallback_resolution.Resolver.__init__",
                "fallback_resolution.Resolver.resolve",
                "fallback_resolution.Resolver.health",
                "fallback_resolution.Resolver.capture",
                "fallback_projection.binding",
                "fallback_projection.FallbackProjection",
                "fallback_projection.FallbackProjection.__init__",
                "fallback_projection.FallbackProjection.bind",
                "fallback_projection.FallbackProjection.plan",
                "fallback_projection.FallbackProjection.run",
                "fallback_projection.FallbackProjection.apply_prepared",
                "fallback_projection.FallbackProjection.present",
                "fallback_worker.configure",
                "fallback_worker.tick",
            )
        }
        self.assertEqual(set(module.CONTRACTS), selected)
        rows = [row for row in inventory["rows"] if row["id"] in selected]
        self.assertEqual(len(rows), module.EXPECTED_ROWS)
        self.assertEqual(module.EXPECTED_ROWS, 15)
        self.assertEqual(len({row["id"] for row in rows}), 15)
        for row in rows:
            path = row["id"].removeprefix("python:catabolic.").split(".")
            tree = ast.parse((FACADE / (path[0] + ".py")).read_text())
            owner = next(
                node for node in tree.body if getattr(node, "name", None) == path[1]
            )
            if row["signature"]["kind"] == "class":
                self.assertIsInstance(owner, ast.ClassDef)
            else:
                function = (
                    next(
                        node
                        for node in owner.body
                        if getattr(node, "name", None) == path[2]
                    )
                    if len(path) == 3
                    else owner
                )
                expected = (
                    ast.parse("def f(" + row["signature"]["arguments"] + "): pass")
                    .body[0]
                    .args
                )
                self.assertEqual(ast.dump(function.args), ast.dump(expected), row["id"])
            self.assertTrue(row["classification"].startswith("preserve"), row["id"])
            self.assertTrue(module.CONTRACTS[row["id"]]["tested"])
            self.assertTrue(module.CONTRACTS[row["id"]]["gap"])

    def test_required_installed_cases_and_pairs_are_fixed_not_skipping(self):
        module = gate()
        self.assertEqual(len(module.EXPECTED_TESTS), 24)
        self.assertEqual(len(set(module.EXPECTED_TESTS)), 24)
        self.assertEqual(
            set(module.EXPECTED_TESTS),
            {
                name
                for name in vars(module.InstalledFallback)
                if name.startswith("test_")
            },
        )
        self.assertEqual(len(module.EXPECTED_GROUPS), 27)
        self.assertEqual(len(set(module.EXPECTED_GROUPS)), 27)
        self.assertEqual(
            module.PARTIAL_CASES, ("run_apply_refusal", "apply_prepared_refusal")
        )
        self.assertTrue(set(module.PARTIAL_CASES).isdisjoint(module.EXPECTED_GROUPS))
        source = SCRIPT.read_text()
        for unsafe in ("skipTest", "unittest.skip", "SkipTest", "expectedFailure"):
            self.assertNotIn(unsafe, source)
        self.assertIn("run_required_suite", source)
        self.assertIn("expected_count=24", source)
        self.assertIn(
            "qualification.paired_groups(reference, native, EXPECTED_GROUPS, 27)",
            source,
        )
        self.assertIn("non_parity_refusal_evidence=partial_evidence", source)
        self.assertIn("callable coverage is not exhaustive parity", source)

    def test_adapters_forward_without_python_business_engine(self):
        for name in ("fallback_resolution", "fallback_projection", "fallback_worker"):
            source = (FACADE / (name + ".py")).read_text()
            for forbidden in (
                "execute_operation",
                "sqlite3",
                "SELECT ",
                "hashlib",
                "subprocess",
                "disposable_fixture",
            ):
                self.assertNotIn(forbidden, source)
            for node in ast.walk(ast.parse(source)):
                if isinstance(node, ast.Import):
                    names = [alias.name for alias in node.names]
                elif isinstance(node, ast.ImportFrom) and not node.level:
                    names = [node.module or ""]
                else:
                    names = []
                self.assertFalse(
                    any(
                        name == "catabolic" or name.startswith("catabolic.")
                        for name in names
                    )
                )
        resolver = (FACADE / "fallback_resolution.py").read_text()
        self.assertIn("session_owner_matches=session.store is store", resolver)
        self.assertIn("session.profile == profile", resolver)
        self.assertIn("_cancelled=cancelled", resolver)
        self.assertIn("self.session = EvaluationSession(", resolver)
        self.assertIn('self.total_checks = reply["total_checks"]', resolver)
        self.assertIn("primary_choices=self.primary_choices", resolver)
        self.assertNotIn('self.primary_choices = reply["primary_choices"]', resolver)
        self.assertIn("foreign Python check callbacks", resolver)
        worker = (FACADE / "fallback_worker.py").read_text()
        self.assertIn("_native.fallback_tick", worker)

    def test_sealed_plan_and_public_publication_gates_remain(self):
        source = (FACADE / "fallback_projection.py").read_text()
        self.assertIn("plan._store is self.store", source)
        self.assertIn("plan._profile == self.profile", source)
        self.assertIn("self.store._owner.prepare_fallback(", source)
        self.assertIn("plan._lease = lease", source)
        self.assertIn("except BaseException:", source)
        self.assertIn("lease.release()", source)
        self.assertNotIn("def __del__", source)
        owner = (ROOT / "crates/catabolic-store/src/public_fallback.rs").read_text()
        self.assertIn('args["plan_owner_matches"] != true', owner)
        self.assertIn('plan.full_plan() != &args["plan"]', owner)
        self.assertIn("256", owner)
        runtime = (ROOT / "crates/catabolic-runtime/src/public_fallback.rs").read_text()
        self.assertIn("require_fallback_fixture(&json!({}))", runtime)
        self.assertIn("fallback_plans().checked(profile, args)", runtime)
        self.assertNotIn('"disposable_fixture": true', runtime)

    def test_optimization_is_refused_before_installed_import(self):
        for flag in ("-O", "-OO"):
            result = subprocess.run(
                [sys.executable, flag, str(SCRIPT)], capture_output=True, text=True
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("do not run with -O or -OO", result.stderr)

        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "report.json"
            report.write_text("prior evidence")
            attempt = report.with_suffix(".attempt.json")
            attempt.write_text("prior attempt")
            result = subprocess.run(
                [sys.executable, "-I", str(SCRIPT), "--report", str(report)],
                env={**os.environ, "PYTHONOPTIMIZE": "1"},
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("do not run with -O or -OO", result.stderr)
            self.assertFalse(report.exists())
            self.assertFalse(attempt.exists())

    def test_build_manifest_binds_current_source_wheel_extension(self):
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
        source = types.SimpleNamespace(source_digest=lambda root: "source")
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(module, "sibling", return_value=source),
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
                    path.write_text(json.dumps({**manifest, key: "stale"}))
                    with self.assertRaisesRegex(
                        AssertionError, "build identity mismatch"
                    ):
                        module.build_identity(path, artifact)

    def test_source_manifest_contains_gate_adapters_and_owners(self):
        manifest = gate().source_manifest()
        for name in (
            "scripts/native_fallback_embedding_tests.py",
            "tests/test_native_fallback_embedding_contract.py",
            "crates/catabolic-store/src/public_fallback.rs",
            "crates/catabolic-runtime/src/public_fallback.rs",
            "crates/catabolic-runtime/tests/public_fallback_facade.rs",
            "crates/catabolic-python/src/embedding.rs",
            "crates/catabolic-python/src/public_fallback.rs",
            "crates/catabolic-python/python/catabolic_native/evaluation.py",
        ):
            self.assertIn(name, manifest)
            self.assertEqual(len(manifest[name]), 64)
