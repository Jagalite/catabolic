# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fixed three-row source contract; installed evidence has a separate gate."""

import ast
import importlib.util
import json
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FACADE = ROOT / "crates/catabolic-python/python/catabolic_native/access.py"
SCRIPT = ROOT / "scripts/native_access_admin_tests.py"
IDS = {
    "python:catabolic.access." + name
    for name in ("principal_put", "grant_put", "issue")
}


class AccessAdminContract(unittest.TestCase):
    def test_exact_three_preserved_signatures(self):
        inventory = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        self.assertEqual(
            inventory["reference_commit"], "453fca983222c6665a48775eef67c489c96b527e"
        )
        rows = {row["id"]: row for row in inventory["rows"] if row["id"] in IDS}
        self.assertEqual(set(rows), IDS)
        nodes = {
            node.name: node
            for node in ast.parse(FACADE.read_text()).body
            if isinstance(node, ast.FunctionDef)
        }
        for identifier, row in rows.items():
            expected = ast.parse(
                "def f(" + row["signature"]["arguments"] + "): pass"
            ).body[0]
            self.assertEqual(
                ast.dump(nodes[identifier.split(".")[-1]].args), ast.dump(expected.args)
            )

    def test_thin_typed_forwarding_without_python_business_logic(self):
        text = FACADE.read_text()
        for forbidden in (
            "sqlite3",
            "SELECT ",
            "INSERT ",
            "UPDATE ",
            "hashlib",
            "Store(",
            "json.dumps",
            "save_secret",
            "open(",
        ):
            self.assertNotIn(forbidden, text)
        self.assertIn("store._owner.access_admin(operation, request)", text)
        bridge = (ROOT / "crates/catabolic-python/src/public_access.rs").read_text()
        for operation in ("principal_put", "grant_put", "issue"):
            self.assertIn("access_admin::" + operation + "_detailed(", bridge)
        for forbidden in (
            "SELECT ",
            "INSERT ",
            "UPDATE ",
            "Store::open",
            "GrantPreflight",
            "new_with_access",
        ):
            self.assertNotIn(forbidden, bridge)
        self.assertIn("access_admin::Failure::Access", bridge)
        self.assertNotIn("error_status", bridge)
        self.assertIn("clear_interrupt_handler", bridge)
        self.assertIn("is_exact_instance_of::<PyInt>()", bridge)
        self.assertIn("class AccessError(CatabolicError):", text)

    def test_fixed_scenarios_groups_rows_and_strict_gate(self):
        spec = importlib.util.spec_from_file_location("access_gate", SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertEqual(set(module.CONTRACTS), IDS)
        self.assertEqual(len(module.EXPECTED_GROUPS), 30)
        self.assertEqual(len(set(module.EXPECTED_GROUPS)), 30)
        self.assertEqual(len(module.EXPECTED_TESTS), 9)
        text = SCRIPT.read_text()
        self.assertIn("qualification.strict_artifact(", text)
        self.assertIn("run_required_suite(", text)
        self.assertIn('manifest["source_sha256_after"]', text)

    def test_ci_requires_strict_installed_administration_gate(self):
        from tests.native_ci_contract import require_installed_gate

        workflow = (ROOT / ".github/workflows/native-qualification.yml").read_text()
        require_installed_gate(
            workflow,
            "native_access_admin_tests.py",
            "native-access-admin",
            extra=(
                '--native-binary "$root/dist/native/catabolic-native"',
                '--legacy-binary "$root/dist/native/catabolic-legacy-native"',
            ),
        )

    def test_optimized_execution_refused(self):
        for flag in ("-O", "-OO"):
            result = subprocess.run(
                [sys.executable, flag, str(SCRIPT), "--help"],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("requires Python assertions", result.stderr)


if __name__ == "__main__":
    unittest.main()
