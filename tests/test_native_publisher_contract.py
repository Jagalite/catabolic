# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Source-only shape and transport contracts for the public Publisher adapter."""

import ast
import importlib.util
import json
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "crates/catabolic-python/python/catabolic_native/scan_execution.py"
PREFIX = "python:catabolic.scan_execution.Publisher"
METHODS = ("__init__", "initial_scopes", "directory", "checkpoint", "stop", "flush")


class PublisherContract(unittest.TestCase):
    def test_exact_seven_frozen_rows_and_signatures(self):
        inventory = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        identifiers = {PREFIX, *(PREFIX + "." + name for name in METHODS)}
        rows = {row["id"]: row for row in inventory["rows"] if row["id"] in identifiers}
        self.assertEqual(len(rows), 7)
        cls = next(
            node
            for node in ast.parse(SOURCE.read_text()).body
            if isinstance(node, ast.ClassDef) and node.name == "Publisher"
        )
        methods = {
            node.name: node for node in cls.body if isinstance(node, ast.FunctionDef)
        }
        for name in METHODS:
            self.assertEqual(
                ast.unparse(methods[name].args),
                rows[PREFIX + "." + name]["signature"]["arguments"],
            )
        self.assertEqual(
            [ast.unparse(item) for item in methods["initial_scopes"].decorator_list],
            ["property"],
        )

    def test_transport_only_and_production_scan_wiring(self):
        source = SOURCE.read_text()
        for text in (
            "SELECT ",
            "UPDATE ",
            "INSERT ",
            "DELETE ",
            "Store(",
            "subprocess",
            "os.stat",
            "root_handle",
        ):
            self.assertNotIn(text, source)
        app = SOURCE.with_name("app.py").read_text()
        self.assertIn("self.store._owner.scan_with_publisher", app)
        store = (ROOT / "crates/catabolic-store/src/observation.rs").read_text()
        self.assertIn("struct Publication", store)
        self.assertIn("publisher: Publisher", store)
        self.assertIn("scan.publisher.clone()", store)
        self.assertIn("self.publisher.flush(store)?", store)
        self.assertIn("self.publisher.entry(entry)?", store)
        self.assertIn("LIMIT 256", store)
        bridge = (ROOT / "crates/catabolic-python/src/public_publisher.rs").read_text()
        self.assertNotIn("unsafe", bridge)
        self.assertIn("failure.value(py).is_instance", bridge)

    def test_strict_gate_has_fixed_contract_counts_and_ci(self):
        from tests.native_ci_contract import require_installed_gate

        script = ROOT / "scripts/native_publisher_embedding_tests.py"
        spec = importlib.util.spec_from_file_location("publisher_gate", script)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertEqual(len(module.CONTRACTS), 7)
        self.assertEqual(len(module.EXPECTED_GROUPS), 24)
        self.assertEqual(len(module.EXPECTED_TESTS), 11)
        self.assertNotIn("--dev", script.read_text())
        require_installed_gate(
            (ROOT / ".github/workflows/native-qualification.yml").read_text(),
            script.name,
            "native-publisher-embedding",
        )
