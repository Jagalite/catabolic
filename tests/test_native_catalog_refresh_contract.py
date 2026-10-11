# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Source-only preserved CatalogRefresh signature/import and ownership boundary."""

import ast
import json
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "crates/catabolic-python/python/catabolic_native/catalog_refresh.py"
IDS = {
    "python:catabolic.catalog_refresh",
    "python:catabolic.catalog_refresh.enqueue",
    "python:catabolic.catalog_refresh.finish",
    "python:catabolic.catalog_refresh.CatalogRefresh",
    *{
        "python:catabolic.catalog_refresh.CatalogRefresh." + name
        for name in ("__init__", "configure", "pending", "run")
    },
}


class CatalogRefreshContract(unittest.TestCase):
    def test_exact_eight_preserved_rows_and_signatures(self):
        rows = json.loads((ROOT / "tests/parity/python-api.json").read_text())["rows"]
        rows = {row["id"]: row for row in rows if row["id"] in IDS}
        self.assertEqual(set(rows), IDS)
        self.assertEqual(len(rows), 8)
        tree = ast.parse(SOURCE.read_text())
        for node in tree.body:
            if isinstance(node, ast.FunctionDef):
                self.assertEqual(
                    ast.unparse(node.args),
                    rows["python:catabolic.catalog_refresh." + node.name]["signature"][
                        "arguments"
                    ],
                )
            elif isinstance(node, ast.ClassDef):
                for method in node.body:
                    if isinstance(method, ast.FunctionDef):
                        row = rows[
                            "python:catabolic.catalog_refresh.CatalogRefresh."
                            + method.name
                        ]
                        self.assertEqual(
                            ast.unparse(method.args), row["signature"]["arguments"]
                        )

    def test_facade_uses_persistent_owner_without_business_sql_or_bypass(self):
        source = SOURCE.read_text()
        for prohibited in (
            "execute_operation",
            "Store(",
            "disposable_fixture",
            "SELECT ",
            "INSERT ",
            "UPDATE ",
            "DELETE ",
        ):
            self.assertNotIn(prohibited, source)
        self.assertIn(
            "catalog_refresh_enqueue(profile, observation_job, db._token)", source
        )
        self.assertIn(
            "from .catalog_refresh import CatalogRefresh as CatalogRefresh",
            SOURCE.with_name("__init__.py").read_text(),
        )

    def test_qualification_rejects_optimized_python(self):
        completed = subprocess.run(
            [
                sys.executable,
                "-O",
                str(ROOT / "scripts/native_catalog_refresh_embedding_tests.py"),
                "--help",
            ],
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("requires Python assertions", completed.stderr)


if __name__ == "__main__":
    unittest.main()
