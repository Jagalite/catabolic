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


def require_sqlite_exception_only(tree, filename):
    """Accept fixed exception-type references, never an engine capability."""
    expected_type, expected_count = {
        "cli.py": ("CatabolicError, OSError, sqlite3.Error, ValueError", 2),
        "scan_execution.py": ("OSError, CatabolicError, sqlite3.Error", 1),
    }[filename]
    imports = [
        node
        for node in ast.walk(tree)
        if isinstance(node, ast.Import)
        and [(alias.name, alias.asname) for alias in node.names] == [("sqlite3", None)]
    ]
    if len(imports) != 1:
        raise AssertionError("expected one unaliased sqlite3 exception import")
    # Restrict the reviewed literal facade style. This regression guard is not
    # a sandbox or a proof about arbitrary Python metaprogramming.
    dynamic_names = {
        "globals",
        "locals",
        "vars",
        "eval",
        "exec",
        "__import__",
        "__builtins__",
    }
    expected_getattr = ast.dump(ast.parse("getattr(sys, name)", mode="eval").body)
    for node in ast.walk(tree):
        if isinstance(node, ast.Name) and node.id in dynamic_names:
            raise AssertionError(
                "dynamic namespace access is outside this facade contract"
            )
        if isinstance(node, ast.alias) and (
            node.name in dynamic_names
            or node.name.split(".")[0] in {"builtins", "importlib"}
        ):
            raise AssertionError(
                "dynamic namespace import is outside this facade contract"
            )
        if isinstance(node, ast.ImportFrom) and (node.module or "").split(".")[0] in {
            "builtins",
            "importlib",
        }:
            raise AssertionError(
                "dynamic namespace import is outside this facade contract"
            )
        if isinstance(node, ast.Attribute) and node.attr in dynamic_names | {
            "modules",
            "__dict__",
            "__globals__",
        }:
            raise AssertionError(
                "dynamic namespace attribute is outside this facade contract"
            )
        if isinstance(node, ast.Name) and node.id == "getattr":
            calls = [
                call
                for call in ast.walk(tree)
                if isinstance(call, ast.Call)
                and call.func is node
                and filename == "cli.py"
                and ast.dump(call) == expected_getattr
            ]
            if len(calls) != 1:
                raise AssertionError("unsupported dynamic attribute access")
        for field, value in ast.iter_fields(node):
            if value == "sqlite3" and not (
                isinstance(node, ast.Name)
                and field == "id"
                or node is imports[0].names[0]
                and field == "name"
            ):
                raise AssertionError(
                    "sqlite3 string access or rebinding is not an exception type"
                )
    expected = (
        ast.parse(f"try: pass\nexcept ({expected_type}): pass").body[0].handlers[0].type
    )
    allowed = [
        node
        for handler in ast.walk(tree)
        if isinstance(handler, ast.ExceptHandler)
        and handler.type is not None
        and ast.dump(handler.type) == ast.dump(expected)
        for node in ast.walk(handler.type)
        if isinstance(node, ast.Name) and node.id == "sqlite3"
    ]
    uses = [
        node
        for node in ast.walk(tree)
        if isinstance(node, ast.Name) and node.id == "sqlite3"
    ]
    if len(uses) != expected_count or set(uses) != set(allowed):
        raise AssertionError(
            "sqlite3 may only identify the fixed owner exception handlers"
        )
    return imports[0]


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
                    "python:catabolic.item_workflow.ItemWorkflow",
                    "python:catabolic.curation.Curation",
                    "python:catabolic.curation.occurrence",
                )
            ):
                continue
            parts = row["id"].removeprefix("python:catabolic.").split(".")
            module, name, *method = parts
            tree = ast.parse((FACADE / f"{module}.py").read_text())
            owner = next(
                node for node in tree.body if getattr(node, "name", None) == name
            )
            if row["signature"]["kind"] != "class":
                function = (
                    next(
                        node
                        for node in owner.body
                        if isinstance(node, ast.FunctionDef) and node.name == method[0]
                    )
                    if method
                    else owner
                )
                expected = (
                    ast.parse(f"def f({row['signature']['arguments']}): pass")
                    .body[0]
                    .args
                )
                self.assertEqual(ast.dump(function.args), ast.dump(expected), row["id"])
            checked += 1
        self.assertEqual(checked, 69)

    def test_facades_do_not_import_legacy_business_logic_or_sqlite(self):
        for path in FACADE.glob("*.py"):
            tree = ast.parse(path.read_text())
            exception_import = (
                require_sqlite_exception_only(tree, path.name)
                if path.name in ("cli.py", "scan_execution.py")
                else None
            )
            for node in ast.walk(tree):
                if node is exception_import:
                    continue
                if isinstance(node, ast.Import):
                    imported = [alias.name for alias in node.names]
                elif isinstance(node, ast.ImportFrom):
                    if (
                        path.name == "catalog_refresh.py"
                        and node.level == 0
                        and node.module == "sqlite3"
                        and [(alias.name, alias.asname) for alias in node.names]
                        == [("Error", "SqliteError")]
                    ):
                        # Frozen finish catches this exception class; no SQLite
                        # module, engine, connection or cursor enters the facade.
                        continue
                    imported = [] if node.level else [node.module or ""]
                else:
                    continue
                self.assertFalse(
                    any(
                        name == "sqlite3"
                        or name.startswith("sqlite3.")
                        or name == "catabolic"
                        or name.startswith("catabolic.")
                        for name in imported
                    ),
                    str(path),
                )

    def test_sqlite_exception_imports_cannot_supply_engine_or_escape(self):
        for filename in ("cli.py", "scan_execution.py"):
            source = (FACADE / filename).read_text()
            require_sqlite_exception_only(ast.parse(source), filename)
            for mutation in (
                "sqlite3.connect('fixture.sqlite3')",
                "sqlite3.Connection('fixture.sqlite3')",
                "sqlite3.Cursor(None)",
                "getattr(sqlite3, 'connect')",
                "engine = sqlite3",
                "sqlite3 = object()",
                "sqlite3.Error('outside handler')",
                "globals()['sqlite3'].connect(':memory:')",
                "locals()['sqlite3'].connect(':memory:')",
                "vars()['sqlite3'].connect(':memory:')",
                "globals()['sqlite' + '3'].connect(':memory:')",
                "import os as sqlite3",
                "try: pass\nexcept Exception as sqlite3: pass",
                "def shadow(sqlite3): pass",
                "def sqlite3(): pass",
                "from builtins import globals as namespace; namespace()['sqlite' + '3'].connect(':memory:')",
                "import sys; sys.modules['sqlite' + '3'].connect(':memory:')",
                "import importlib; importlib.import_module('sqlite' + '3')",
                "import builtins as dynamic; dynamic.globals()['sqlite' + '3']",
                "getter = getattr",
                "getattr(sys, 'modules')",
            ):
                with (
                    self.subTest(facade=filename, mutation=mutation),
                    self.assertRaises(AssertionError),
                ):
                    require_sqlite_exception_only(
                        ast.parse(source + "\n" + mutation), filename
                    )
            for replacement in (
                "sqlite3.connect('fixture.sqlite3')",
                "getattr(sqlite3, 'Error')",
                "sqlite3.OperationalError",
            ):
                with (
                    self.subTest(facade=filename, handler=replacement),
                    self.assertRaises(AssertionError),
                ):
                    require_sqlite_exception_only(
                        ast.parse(source.replace("sqlite3.Error", replacement, 1)),
                        filename,
                    )

    def test_refresh_sqlite_exception_alias_is_handler_only(self):
        tree = ast.parse((FACADE / "catalog_refresh.py").read_text())
        aliases = [
            node
            for node in ast.walk(tree)
            if isinstance(node, ast.Name) and node.id == "SqliteError"
        ]
        handler_aliases = [
            node
            for handler in ast.walk(tree)
            if isinstance(handler, ast.ExceptHandler) and handler.type is not None
            for node in ast.walk(handler.type)
            if isinstance(node, ast.Name) and node.id == "SqliteError"
        ]
        self.assertEqual(len(aliases), 1)
        self.assertEqual(aliases, handler_aliases)
        for node in ast.walk(tree):
            if isinstance(node, ast.Name):
                self.assertNotEqual(node.id, "sqlite3")
            elif isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
                self.assertNotIn(
                    node.func.attr,
                    {
                        "connect",
                        "Connection",
                        "Cursor",
                        "cursor",
                        "execute",
                        "executemany",
                        "executescript",
                    },
                )

    def test_thin_services_never_call_path_reopening_companion(self):
        for name in (
            "store",
            "app",
            "query",
            "media_catalog",
            "tagging",
            "item_workflow",
            "curation",
        ):
            source = (FACADE / f"{name}.py").read_text()
            self.assertNotIn("execute_operation(", source)
            self.assertNotIn("catalog_query(", source)
            self.assertNotIn("tag_mutation(", source)

    def test_workflow_coverage_names_every_preserved_row_and_gap(self):
        spec = importlib.util.spec_from_file_location(
            "workflow_embedding_contracts",
            ROOT / "scripts/native_workflow_embedding_tests.py",
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        inventory = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        expected = {
            row["id"]
            for row in inventory["rows"]
            if row["id"].startswith(
                (
                    "python:catabolic.item_workflow.ItemWorkflow",
                    "python:catabolic.curation.Curation",
                    "python:catabolic.curation.occurrence",
                )
            )
        }
        self.assertEqual(len(expected), 22)
        self.assertEqual(set(module.CONTRACTS), expected)
        for contract in module.CONTRACTS.values():
            self.assertTrue(contract["tested"])
            self.assertTrue(contract["gap"])


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
