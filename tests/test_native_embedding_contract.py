# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Source-level facade inventory checks; installed proof is a separate required lane."""

import ast
import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FACADE = ROOT / "crates/catabolic-python/python/catabolic_native"


class NativeEmbeddingInventory(unittest.TestCase):
    def test_preserved_rows_have_matching_signatures(self):
        inventory = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        checked = 0
        for row in inventory["rows"]:
            if row["classification"] != "preserve-embedding-facade" and not row[
                "id"
            ].startswith(
                (
                    "python:catabolic.query.CatalogQuery",
                    "python:catabolic.media_catalog.MediaCatalog",
                    "python:catabolic.tagging.Tagging",
                )
            ):
                continue
            module, cls, *method = (
                row["id"].removeprefix("python:catabolic.").split(".")
            )
            tree = ast.parse((FACADE / f"{module}.py").read_text())
            owner = next(
                node
                for node in tree.body
                if isinstance(node, ast.ClassDef) and node.name == cls
            )
            if method:
                function = next(
                    node
                    for node in owner.body
                    if isinstance(node, ast.FunctionDef) and node.name == method[0]
                )
                expected = (
                    ast.parse(f"def f({row['signature']['arguments']}): pass")
                    .body[0]
                    .args
                )
                self.assertEqual(ast.dump(function.args), ast.dump(expected), row["id"])
            checked += 1
        self.assertEqual(checked, 47)

    def test_facades_do_not_import_legacy_business_logic_or_sqlite(self):
        for path in FACADE.glob("*.py"):
            for node in ast.walk(ast.parse(path.read_text())):
                if isinstance(node, ast.Import):
                    imported = [alias.name for alias in node.names]
                elif isinstance(node, ast.ImportFrom):
                    imported = [] if node.level else [node.module or ""]
                else:
                    continue
                self.assertFalse(
                    any(
                        name == "sqlite3"
                        or name == "catabolic"
                        or name.startswith("catabolic.")
                        for name in imported
                    ),
                    str(path),
                )

    def test_thin_services_never_call_path_reopening_companion(self):
        for name in ("store", "app", "query", "media_catalog", "tagging"):
            source = (FACADE / f"{name}.py").read_text()
            self.assertNotIn("execute_operation(", source)
            self.assertNotIn("catalog_query(", source)
            self.assertNotIn("tag_mutation(", source)


class NativeEmbeddingProvenance(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location(
            "embedding_qualification_test", ROOT / "scripts/qualify_native_embedding.py"
        )
        self.qualifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.qualifier)
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        package = self.root / "src/catabolic"
        package.mkdir(parents=True)
        (package / "app.py").write_text("# immutable app fixture\n")
        (package / "store.py").write_text("# immutable store fixture\n")
        self.git("init", "-q")
        self.git("add", "src")
        self.git(
            "-c",
            "user.name=Embedding Test",
            "-c",
            "user.email=embedding@example.invalid",
            "commit",
            "-qm",
            "frozen fixture",
        )
        self.head = self.git("rev-parse", "HEAD").decode().strip()

    def git(self, *args):
        return subprocess.check_output(
            ["git", "-C", str(self.root), *args], stderr=subprocess.STDOUT
        )

    def test_clean_own_git_root_and_committed_bytes_qualify(self):
        result = self.qualifier.reference_provenance(self.root, self.head)
        self.assertEqual(result["head"], self.head)
        self.assertEqual(len(result["tracked_source_sha256"]), 2)

    def test_containing_repository_and_wrong_head_are_rejected(self):
        nested = self.root / "archived"
        nested.mkdir()
        with self.assertRaisesRegex(AssertionError, "own Git root"):
            self.qualifier.reference_provenance(nested, self.head)
        with self.assertRaisesRegex(AssertionError, "differs"):
            self.qualifier.reference_provenance(self.root, "0" * 40)

    def test_assume_unchanged_source_modification_is_rejected(self):
        self.git("update-index", "--assume-unchanged", "src/catabolic/app.py")
        (self.root / "src/catabolic/app.py").write_text("# changed behind Git status\n")
        with self.assertRaisesRegex(AssertionError, "source bytes changed"):
            self.qualifier.reference_provenance(self.root, self.head)

    def test_untracked_package_code_is_rejected(self):
        (self.root / "src/catabolic/injected.py").write_text(
            "# untracked injection fixture\n"
        )
        with self.assertRaisesRegex(AssertionError, "untracked/ignored"):
            self.qualifier.reference_provenance(self.root, self.head)


if __name__ == "__main__":
    unittest.main()
