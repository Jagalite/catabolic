# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fixed inventory/source and paired pure content-revision facade checks."""

import ast
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from tests.test_rust_legacy_parser import PYTHON, REFERENCE, environment

ROOT = Path(__file__).resolve().parents[1]
FACADE = ROOT / "crates/catabolic-python/python/catabolic_native"
SCRIPT = ROOT / "scripts/native_content_revision_tests.py"
NATIVE_PYTHON = Path(os.environ.get("CATABOLIC_NATIVE_PYTHON", sys.executable))
CONTRACTS = (
    "python:catabolic.access.AccessError",
    "python:catabolic.access.AccessError.__init__",
    "python:catabolic.content_access.revision_of",
)


class ContentRevisionSourceContracts(unittest.TestCase):
    def test_fixed_frozen_inventory_and_signatures(self):
        spec = importlib.util.spec_from_file_location("revision_contracts", SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertEqual(module.CONTRACTS, CONTRACTS)
        self.assertEqual(len(module.EXPECTED_GROUPS), 12)
        inventory = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        self.assertEqual(
            inventory["reference_commit"], "453fca983222c6665a48775eef67c489c96b527e"
        )
        rows = [r for r in inventory["rows"] if r["id"] in CONTRACTS]
        self.assertEqual(len(rows), 3)
        for row in rows:
            path = row["id"].removeprefix("python:catabolic.").split(".")
            tree = ast.parse((FACADE / (path[0] + ".py")).read_text())
            current = tree
            for name in path[1:]:
                current = next(
                    n for n in current.body if getattr(n, "name", None) == name
                )
            if row["signature"]["kind"] == "class":
                self.assertEqual(
                    [ast.unparse(n) for n in current.bases], ["CatabolicError"]
                )
            else:
                expected = ast.parse(
                    "def f(" + row["signature"]["arguments"] + "): pass"
                ).body[0]
                self.assertEqual(ast.dump(current.args), ast.dump(expected.args))

    def test_no_python_revision_engine_or_path_reopen(self):
        text = (FACADE / "content_access.py").read_text()
        for forbidden in (
            "hashlib",
            "SELECT ",
            "sqlite3",
            "open(",
            "execute_operation",
        ):
            self.assertNotIn(forbidden, text)
        self.assertIn("return store._call(", text)
        embedding = (ROOT / "crates/catabolic-python/src/embedding.rs").read_text()
        self.assertIn('"content_access" if operation == "revision_of"', embedding)
        self.assertIn("catabolic_store::content_access::revision_of(", embedding)

    def test_optimization_refused(self):
        for flag in ("-O", "-OO"):
            result = subprocess.run(
                [sys.executable, flag, str(SCRIPT)], capture_output=True, text=True
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("do not run with -O or -OO", result.stderr)


class ContentRevisionInstalledChecks(unittest.TestCase):
    def test_frozen_paired_groups_and_persistent_ownership(self):
        outputs = {}
        with tempfile.TemporaryDirectory() as directory:
            env = environment(
                overrides={"HOME": directory, "PYTHONDONTWRITEBYTECODE": "1"}
            )
            for engine, python in (("reference", PYTHON), ("native", NATIVE_PYTHON)):
                command = [str(python), str(SCRIPT), "--engine", engine]
                if engine == "reference":
                    command += ["--reference-src", str(REFERENCE / "src")]
                run = subprocess.run(
                    command,
                    cwd=directory,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=90,
                )
                self.assertEqual(run.returncode, 0, (engine, run.stderr))
                outputs[engine] = json.loads(run.stdout)
        reference = outputs["reference"]["groups"]
        native = outputs["native"]["groups"]
        self.assertEqual(len(reference), 12)
        self.assertEqual(reference, native)
        self.assertEqual(
            outputs["native"]["native_checks"],
            [
                "same-owner-uncommitted-row-and-rollback",
                "detached-refusal-and-reconnect",
                "ordinary-interrupt-rollback-and-reuse",
                "closed-before-lookup-precedence",
                "caller-pinned-readonly-snapshot",
            ],
        )


if __name__ == "__main__":
    unittest.main()
