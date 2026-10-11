"""The separate 24-row persistent Artifacts/Processing facade source gate."""

import ast
import importlib.util
import json
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FACADE = ROOT / "crates/catabolic-python/python/catabolic_native"


class ProcessingFacadeContracts(unittest.TestCase):
    def test_exact_frozen_signatures_and_coverage(self):
        spec = importlib.util.spec_from_file_location(
            "processing_contracts",
            ROOT / "scripts/native_processing_embedding_tests.py",
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        inventory = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        rows = [row for row in inventory["rows"] if row["id"] in module.CONTRACTS]
        self.assertEqual(len(rows), 24)
        for row in rows:
            mod, name, *method = row["id"].removeprefix("python:catabolic.").split(".")
            tree = ast.parse((FACADE / (mod + ".py")).read_text())
            owner = next(
                node for node in tree.body if getattr(node, "name", None) == name
            )
            if row["signature"]["kind"] != "class":
                function = (
                    next(
                        node
                        for node in owner.body
                        if getattr(node, "name", None) == method[0]
                    )
                    if method
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

    def test_persistent_routes_do_not_use_companion_or_python_engine(self):
        for name in ("artifacts", "processing"):
            text = (FACADE / (name + ".py")).read_text()
            self.assertNotIn("execute_operation(", text)
            self.assertNotIn("_native.execute_", text)
            for node in ast.walk(ast.parse(text)):
                if isinstance(node, ast.Import):
                    names = [entry.name for entry in node.names]
                elif isinstance(node, ast.ImportFrom):
                    names = [] if node.level else [node.module or ""]
                else:
                    continue
                self.assertFalse(
                    any(
                        name == "sqlite3"
                        or name == "catabolic"
                        or name.startswith("catabolic.")
                        for name in names
                    )
                )
