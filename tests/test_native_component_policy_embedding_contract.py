"""Exact selected Components/Policies public adapter source contracts."""

import ast
import importlib.util
import json
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
FACADE = ROOT / "crates/catabolic-python/python/catabolic_native"
SCRIPT = ROOT / "scripts/native_component_policy_embedding_tests.py"


class ComponentPolicyFacadeContracts(unittest.TestCase):
    def test_exact_frozen_rows_and_signatures(self):
        spec = importlib.util.spec_from_file_location("component_contracts", SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        rows = [
            r
            for r in json.loads((ROOT / "tests/parity/python-api.json").read_text())[
                "rows"
            ]
            if r["id"] in module.CONTRACTS
        ]
        self.assertEqual(len(rows), 10)
        self.assertEqual(len(module.EXPECTED_TESTS), 13)
        self.assertEqual(len(module.EXPECTED_GROUPS), 37)
        for row in rows:
            path = row["id"].removeprefix("python:catabolic.").split(".")
            tree = ast.parse((FACADE / (path[0] + ".py")).read_text())
            if len(path) == 1:
                self.assertEqual(
                    row["signature"]["kind"], "module-or-value-or-reexport"
                )
                continue
            owner = next(n for n in tree.body if getattr(n, "name", None) == path[1])
            if row["signature"]["kind"] != "class":
                function = (
                    next(n for n in owner.body if getattr(n, "name", None) == path[2])
                    if len(path) > 2
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

    def test_adapters_delegate_without_python_engine_or_path_reopen(self):
        for name in ("components", "fallback_policies"):
            text = (FACADE / (name + ".py")).read_text()
            self.assertNotIn("execute_operation", text)
            self.assertNotIn("hashlib", text)
            self.assertNotIn("SELECT ", text)
            self.assertNotIn("sqlite3", text)
            for node in ast.walk(ast.parse(text)):
                if isinstance(node, ast.ImportFrom) and not node.level:
                    self.assertFalse((node.module or "").startswith("catabolic"))
        self.assertIn("dict(token=db._token)", (FACADE / "components.py").read_text())
        self.assertIn(
            "session_owner_matches=session.store is store",
            (FACADE / "components.py").read_text(),
        )
        owner = (ROOT / "crates/catabolic-store/src/embedding.rs").read_text()
        self.assertIn("self.check_scope(token)?;", owner)
        self.assertIn("crate::public_components::get_evaluated", owner)

    def test_optimization_is_refused_without_importing_installed_package(self):
        for flag in ("-O", "-OO"):
            result = subprocess.run(
                [sys.executable, flag, str(SCRIPT)], capture_output=True, text=True
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("do not run with -O or -OO", result.stderr)

    def test_build_manifest_binds_source_wheel_and_extension(self):
        spec = importlib.util.spec_from_file_location(
            "component_build_identity", SCRIPT
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        artifact = {
            "wheel_sha256": "current-wheel",
            "package_sha256": {"_native.abi3.so": "current-extension"},
        }
        manifest = {
            "profile": "development",
            "source_sha256": "current-source",
            "source_sha256_after": "current-source",
            "wheel_sha256": "current-wheel",
            "extension_sha256": "current-extension",
        }
        source = types.SimpleNamespace(source_digest=lambda root: "current-source")
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(module, "sibling", return_value=source),
        ):
            path = Path(directory) / "build.json"
            path.write_text(json.dumps(manifest))
            self.assertEqual(
                module.build_identity(path, artifact)["source_sha256"], "current-source"
            )
            with self.assertRaisesRegex(AssertionError, "requires --build-manifest"):
                module.build_identity(None, artifact)
            for key in (
                "source_sha256",
                "source_sha256_after",
                "wheel_sha256",
                "extension_sha256",
            ):
                with self.subTest(mismatch=key):
                    path.write_text(json.dumps({**manifest, key: "older-build"}))
                    with self.assertRaisesRegex(
                        AssertionError, "build identity mismatch"
                    ):
                        module.build_identity(path, artifact)
