# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""One preserved adapter row; source checks are not artifact qualification."""

import ast
import importlib.util
import json
import unittest
from pathlib import Path
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
FACADE = ROOT / "crates/catabolic-python/python/catabolic_native/api_cli.py"


class PrivateSecretContract(unittest.TestCase):
    def test_preserved_signature_and_thin_forwarding(self):
        inventory = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        row = next(
            row
            for row in inventory["rows"]
            if row["id"] == "python:catabolic.api_cli.save_secret"
        )
        expected = ast.parse("def f(" + row["signature"]["arguments"] + "): pass").body[
            0
        ]
        tree = ast.parse(FACADE.read_text())
        function = next(node for node in tree.body if isinstance(node, ast.FunctionDef))
        self.assertEqual(ast.dump(function.args), ast.dump(expected.args))
        self.assertEqual(len(function.body), 1)
        spec = importlib.util.spec_from_file_location("private_secret_facade", FACADE)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        filename, result, expected_result = object(), object(), object()
        seen = []

        def native(*args):
            seen.append(args)
            return expected_result

        store = SimpleNamespace(_owner=SimpleNamespace(save_secret=native))
        self.assertIs(module.save_secret(store, filename, result), expected_result)
        self.assertEqual(seen, [(filename, result)])
        sentinel = KeyboardInterrupt("fake owner exception")

        def failure(*args):
            raise sentinel

        store._owner.save_secret = failure
        with self.assertRaises(KeyboardInterrupt) as captured:
            module.save_secret(store, filename, result)
        self.assertIs(captured.exception, sentinel)

    def test_native_effects_are_exclusive_and_same_owner(self):
        owner = (ROOT / "crates/catabolic-store/src/private_secret.rs").read_text()
        self.assert_owner_source_contract(owner)
        bridge = (ROOT / "crates/catabolic-python/src/public_secret.rs").read_text()
        self.assertIn("private_secret::save(store, path", bridge)
        self.assertIn('getattr("_default_encoder")', bridge)
        for forbidden in ("SELECT ", "INSERT ", "Store::open", "OpenOptions"):
            self.assertNotIn(forbidden, bridge)

    def assert_owner_source_contract(self, owner):
        # Clippy requires the test module after production items. Its fixture
        # Store::open calls are not part of the production save implementation.
        self.assertEqual(owner.count("\n#[cfg(test)]\nmod tests {"), 1)
        production, test_module = owner[owner.index("/// `path`") :].split(
            "\n#[cfg(test)]\nmod tests {", 1
        )
        # This is a deliberately literal, rustfmt-shaped source contract, not
        # a Rust parser. Refuse unsupported layout rather than drop later items.
        lines = test_module.splitlines()
        self.assertEqual(
            [line for line in lines if line and not line[0].isspace()], ["}"]
        )
        self.assertEqual(lines[-1], "}")
        for required in (
            "create_new(true)",
            "mode(0o600)",
            "libc::O_NOFOLLOW",
            "stream.flush()",
            "sync_all()",
            "path.metadata()",
            ".transaction(|db|",
            "INSERT OR REPLACE INTO api_private_files",
        ):
            self.assertIn(required, production)
        for forbidden in (
            "Store::open",
            "rename(",
            "remove_file(",
            "set_permissions",
            "plex",
        ):
            self.assertNotIn(forbidden, production)

    def test_source_contract_distinguishes_test_and_production_reopening(self):
        owner = (ROOT / "crates/catabolic-store/src/private_secret.rs").read_text()
        self.assert_owner_source_contract(
            owner.replace("mod tests {", "mod tests {\n    // Store::open fixture")
        )
        changed = owner.replace(
            "    let file =", "    Store::open(path);\n    let file =", 1
        )
        self.assertNotEqual(changed, owner)
        with self.assertRaises(AssertionError):
            self.assert_owner_source_contract(changed)
        for trailing in (
            "fn reopen(path: &str) { let _ = Store::open(path); }\n",
            "    fn reopen(path: &str) { let _ = Store::open(path); }\n",
        ):
            with self.subTest(trailing=trailing), self.assertRaises(AssertionError):
                self.assert_owner_source_contract(owner + trailing)

    def test_fixed_development_groups_and_explicit_artifact_scope(self):
        script = ROOT / "scripts/native_private_secret_tests.py"
        spec = importlib.util.spec_from_file_location(
            "private_secret_development", script
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertEqual(len(module.GROUPS), 17)
        self.assertEqual(len(set(module.GROUPS)), 17)
        self.assertIn("not installed/release qualification", script.read_text())
        self.assertIn('"-I"', script.read_text())
        self.assertIn("reference_provenance", script.read_text())

    def test_strict_gate_declares_exact_rows_cases_and_final_fences(self):
        script = ROOT / "scripts/native_private_secret_gate.py"
        spec = importlib.util.spec_from_file_location("private_secret_strict", script)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertEqual(
            set(module.CONTRACTS), {"python:catabolic.api_cli.save_secret"}
        )
        self.assertEqual(len(module.EXPECTED_TESTS), 4)
        self.assertEqual(len(set(module.EXPECTED_TESTS)), 4)
        self.assertEqual(len(module.EXPECTED_GROUPS), 17)
        text = script.read_text()
        for required in (
            "qualification.strict_artifact(",
            "run_required_suite(",
            "identity.verify()",
            "assert artifact() == installed",
            "assert fence() == before",
            'manifest["source_sha256_after"]',
            "reference_imports",
            "reference_provenance",
        ):
            self.assertIn(required, text)
        from tests.native_ci_contract import require_installed_gate

        require_installed_gate(
            (ROOT / ".github/workflows/native-qualification.yml").read_text(),
            "native_private_secret_gate.py",
            "native-private-secret",
            extra=(
                '--native-binary "$root/dist/native/catabolic-native"',
                '--legacy-binary "$root/dist/native/catabolic-legacy-native"',
            ),
        )
