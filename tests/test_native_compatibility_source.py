# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Source-only forwarding/preflight controls. No native build or engine execution.

A fake provider proves reexport identity and unchanged propagation only. These
checks never qualify the real facade's behavior, installed shim wheel or parity.
"""

import ast
import contextlib
import importlib
import importlib.util
import json
import os
import sys
import tomllib
import types
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
REFERENCE = Path(os.environ.get("CATABOLIC_NATIVE_COMPAT_SOURCE_ROOT", ROOT))
PROJECT = ROOT / "crates/catabolic-python/compatibility"
PACKAGE = PROJECT / "python/catabolic"
INVENTORY = json.loads((REFERENCE / "tests/parity/python-api.json").read_text())
ROWS = [r for r in INVENTORY["rows"] if r["classification"].startswith("preserve-")]


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


DRIVER = load(ROOT / "scripts/qualify_native_compatibility.py", "compatibility_gate")
SHARED = load(REFERENCE / "scripts/native_facade_qualification.py", "shared_gate")


def fake_function(row):
    namespace = {}
    exec(
        "from __future__ import annotations\n"
        f"def implementation({row['signature']['arguments']}):\n    return None\n",
        namespace,
    )
    function = namespace["implementation"]
    decorators = row["signature"].get("decorators", [])
    if "classmethod" in decorators:
        return classmethod(function)
    if "staticmethod" in decorators:
        return staticmethod(function)
    if "property" in decorators:
        return property(function)
    return function


def fake_provider():
    modules = {}
    for source in PACKAGE.rglob("*.py"):
        for node in ast.parse(source.read_text()).body:
            if not isinstance(node, ast.ImportFrom):
                continue
            module = modules.setdefault(node.module, types.ModuleType(node.module))
            for alias in node.names:
                setattr(module, alias.name, object())
    modules["catabolic_native"].__version__ = "0.2.0"
    for row in ROWS:
        module = modules[
            DRIVER.module_name(row).replace("catabolic", "catabolic_native", 1)
        ]
        suffix = (
            row["id"]
            .removeprefix("python:" + DRIVER.module_name(row))
            .removeprefix(".")
        )
        if not suffix or row["signature"]["kind"] == "module-or-value-or-reexport":
            continue
        names = suffix.split(".")
        if row["signature"]["kind"] == "class":
            setattr(module, names[0], type(names[0], (), {}))
        elif len(names) == 1:
            setattr(module, names[0], fake_function(row))
        else:
            setattr(getattr(module, names[0]), names[1], fake_function(row))
    # Native package parents exist only in memory, and no real extension loads.
    for name in list(modules):
        parts = name.split(".")
        for end in range(1, len(parts)):
            parent = ".".join(parts[:end])
            modules.setdefault(parent, types.ModuleType(parent)).__path__ = []
    for name, module in modules.items():
        if "." in name:
            parent, child = name.rsplit(".", 1)
            setattr(modules[parent], child, module)
    return modules


@contextlib.contextmanager
def isolated_modules(provider=None):
    prefixes = ("catabolic", "catabolic_native")

    def relevant(name):
        return any(name == p or name.startswith(p + ".") for p in prefixes)

    saved = {name: module for name, module in sys.modules.items() if relevant(name)}
    old_path = sys.path[:]
    try:
        for name in saved:
            del sys.modules[name]
        if provider:
            sys.modules.update(provider)
        sys.path.insert(0, str(PROJECT / "python"))
        yield
    finally:
        sys.path[:] = old_path
        for name in tuple(sys.modules):
            if relevant(name):
                del sys.modules[name]
        sys.modules.update(saved)


class ForwardingSource(unittest.TestCase):
    def test_all_preserved_rows_are_forwarded(self):
        self.assertEqual(len(ROWS), 159)
        with isolated_modules(fake_provider()):
            result = DRIVER.verify_forwarding(PROJECT / "python", ROWS)
        self.assertEqual(len(result), 159)
        self.assertEqual({r["id"] for r in result}, {r["id"] for r in ROWS})
        self.assertEqual(
            sum(r["status"] == "native_object_identity" for r in result), 156
        )
        self.assertEqual(sum(r["status"] == "module_proxy_present" for r in result), 3)

    def test_modules_are_only_docstrings_and_explicit_native_reexports(self):
        files = list(PACKAGE.rglob("*.py"))
        self.assertEqual(len(files), 35)
        for source in files:
            for node in ast.parse(source.read_text()).body:
                if isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant):
                    self.assertIsInstance(node.value.value, str)
                    continue
                self.assertIsInstance(node, ast.ImportFrom, source)
                self.assertEqual(node.level, 0)
                self.assertTrue(
                    node.module == "catabolic_native"
                    or node.module.startswith("catabolic_native.")
                )
                for alias in node.names:
                    self.assertNotEqual(alias.name, "*")
                    self.assertEqual(alias.name, alias.asname)

    def test_root_version_and_cooperating_type_identities(self):
        provider = fake_provider()
        with isolated_modules(provider):
            root = importlib.import_module("catabolic")
            self.assertIs(root.__version__, provider["catabolic_native"].__version__)
            for module, symbol in (
                ("store", "Store"),
                ("query", "CatalogQuery"),
                ("media_catalog", "MediaCatalog"),
                ("tagging", "Tagging"),
            ):
                self.assertIs(
                    getattr(importlib.import_module("catabolic." + module), symbol),
                    getattr(provider["catabolic_native." + module], symbol),
                )

    def test_console_preserves_argument_identity_and_returning_status(self):
        provider = fake_provider()
        calls = []

        def main(argv=None):
            calls.append(argv)
            return 130

        provider["catabolic_native.cli"].main = main
        argv = ["scan", "--continue-request", "fixture"]
        with isolated_modules(provider):
            proxy = importlib.import_module("catabolic.cli")
            self.assertIs(proxy.main, main)
            self.assertEqual(proxy.main(argv), 130)
            self.assertIs(calls[0], argv)

    def test_native_gate_exception_instance_is_not_wrapped(self):
        provider = fake_provider()
        refusal = RuntimeError("native production admission remains gated")

        def refuse(*args, **kwargs):
            raise refusal

        provider["catabolic_native.http.app"].create_app = refuse
        with isolated_modules(provider):
            with self.assertRaises(RuntimeError) as caught:
                importlib.import_module("catabolic.http.app").create_app("catalog.db")
            self.assertIs(caught.exception, refusal)

    def test_missing_native_provider_cannot_fall_back(self):
        with isolated_modules():
            sys.modules["catabolic_native"] = None
            with self.assertRaises(ModuleNotFoundError):
                importlib.import_module("catabolic")

    def test_candidate_metadata_does_not_replace_default_projects(self):
        candidate = tomllib.loads((PROJECT / "pyproject.toml").read_text())
        original = tomllib.loads((REFERENCE / "pyproject.toml").read_text())
        native = tomllib.loads(
            (REFERENCE / "crates/catabolic-python/pyproject.toml").read_text()
        )
        self.assertEqual(candidate["project"]["name"], "catabolic-native-compat")
        self.assertEqual(
            candidate["project"]["dependencies"], ["catabolic-native==0.2.0"]
        )
        self.assertEqual(
            candidate["project"]["scripts"], {"catabolic": "catabolic.cli:main"}
        )
        self.assertNotIn("optional-dependencies", candidate["project"])
        self.assertEqual(
            candidate["tool"]["setuptools"]["packages"]["find"],
            {
                "where": ["python"],
                "include": ["catabolic", "catabolic.*"],
                "namespaces": False,
            },
        )
        self.assertEqual(original["project"]["name"], "catabolic")
        self.assertEqual(
            original["tool"]["setuptools"]["packages"]["find"]["where"], ["src"]
        )
        self.assertEqual(
            native["project"]["scripts"], {"catabolic-native": "catabolic_native:main"}
        )

    def test_inventory_requires_exact_count_and_frozen_reference(self):
        self.assertEqual(len(DRIVER.select_rows(INVENTORY, SHARED)), 159)
        for mode in ("missing", "duplicate", "reference"):
            value = json.loads(json.dumps(INVENTORY))
            if mode == "missing":
                value["rows"].remove(
                    next(
                        r
                        for r in value["rows"]
                        if r["classification"].startswith("preserve-")
                    )
                )
            elif mode == "duplicate":
                value["rows"].append(
                    next(
                        r
                        for r in value["rows"]
                        if r["classification"].startswith("preserve-")
                    )
                )
            else:
                value["reference_commit"] = "wrong"
            with self.subTest(mode=mode), self.assertRaises(AssertionError):
                DRIVER.select_rows(value, SHARED)

    def test_same_count_inventory_contract_changes_are_refused(self):
        for mode in ("signature", "classification", "source", "replacement"):
            value = json.loads(json.dumps(INVENTORY))
            row = next(
                r
                for r in value["rows"]
                if r["classification"].startswith("preserve-")
                and r["signature"]["kind"] == "function"
            )
            if mode == "signature":
                row["signature"]["arguments"] += ", added=None"
            elif mode == "classification":
                row["classification"] = "preserve-repository-import-binding"
            elif mode == "source":
                row["source"] = "src/catabolic/moved.py"
            else:
                row["id"] = "python:catabolic.access.unapproved"
            with (
                self.subTest(mode=mode),
                self.assertRaisesRegex(RuntimeError, "inventory contract changed"),
            ):
                DRIVER.select_rows(value, SHARED)


class CleanQualificationBoundary(unittest.TestCase):
    def test_legacy_and_candidate_distributions_refuse_before_provider_loading(self):
        for name in (
            "catabolic",
            "CATABOLIC",
            "catabolic-native-compat",
            "catabolic_native.compat",
        ):
            with (
                self.subTest(name=name),
                patch.object(
                    DRIVER.importlib.metadata,
                    "distributions",
                    return_value=[types.SimpleNamespace(metadata={"Name": name})],
                ),
                patch.object(DRIVER, "load_shared") as shared,
                patch.object(DRIVER.importlib.util, "find_spec") as find,
                self.assertRaisesRegex(RuntimeError, "clean environment"),
            ):
                DRIVER.qualify(REFERENCE, Path("unused.whl"), Path("unused.json"))
            shared.assert_not_called()
            find.assert_not_called()

    def test_preloaded_namespace_refuses(self):
        with (
            isolated_modules(),
            patch.object(DRIVER.importlib.metadata, "distributions", return_value=[]),
        ):
            sys.modules["catabolic.already_loaded"] = types.ModuleType(
                "catabolic.already_loaded"
            )
            with self.assertRaisesRegex(RuntimeError, "already imported"):
                DRIVER.refuse_existing_namespace()

    def test_orphan_importable_namespace_refuses(self):
        with (
            isolated_modules(),
            patch.object(DRIVER.importlib.metadata, "distributions", return_value=[]),
            patch.object(DRIVER.importlib.util, "find_spec", return_value=object()),
            self.assertRaisesRegex(RuntimeError, "mixed installation"),
        ):
            DRIVER.refuse_existing_namespace()

    def test_native_only_environment_is_eligible_for_strict_verification(self):
        with (
            isolated_modules(),
            patch.object(
                DRIVER.importlib.metadata,
                "distributions",
                return_value=[
                    types.SimpleNamespace(metadata={"Name": "catabolic-native"})
                ],
            ),
            patch.object(DRIVER.importlib.util, "find_spec", return_value=None),
        ):
            DRIVER.refuse_existing_namespace()

    def test_strict_native_mismatch_prevents_forwarding(self):
        shared = Mock()
        shared.inventory_rows.side_effect = SHARED.inventory_rows
        shared.strict_artifact.side_effect = AssertionError(
            "release build identity mismatch"
        )
        with (
            patch.object(DRIVER, "refuse_existing_namespace"),
            patch.object(DRIVER, "load_shared", return_value=shared),
            patch.object(DRIVER, "verify_forwarding") as forwarding,
            self.assertRaisesRegex(AssertionError, "identity mismatch"),
        ):
            DRIVER.qualify(REFERENCE, Path("exact.whl"), Path("manifest.json"))
        forwarding.assert_not_called()
        shared.NativeIdentity.assert_not_called()

    def test_success_reuses_exact_strict_verifier_before_and_after(self):
        shared = Mock()
        shared.inventory_rows.side_effect = SHARED.inventory_rows
        artifacts = {
            "wheel_sha256": "exact-wheel",
            "build_manifest_sha256": "exact-manifest",
            "binaries": {"native_binary": "native", "legacy_binary": "legacy"},
        }
        shared.strict_artifact.return_value = artifacts
        events = []
        shared.strict_artifact.side_effect = lambda *args: (
            events.append("strict") or artifacts
        )
        with (
            patch.object(
                DRIVER,
                "refuse_existing_namespace",
                side_effect=lambda: events.append("clean"),
            ),
            patch.object(DRIVER, "load_shared", return_value=shared),
            patch.object(
                DRIVER,
                "verify_forwarding",
                side_effect=lambda *args: (
                    events.append("forwarding") or [{"id": "fixture"}]
                ),
            ),
        ):
            result = DRIVER.qualify(REFERENCE, Path("exact.whl"), Path("manifest.json"))
        self.assertEqual(events, ["clean", "strict", "forwarding", "strict"])
        self.assertEqual(shared.strict_artifact.call_count, 2)
        shared.strict_artifact.assert_called_with(
            Path("exact.whl"), Path("manifest.json"), REFERENCE.resolve()
        )
        shared.NativeIdentity.return_value.verify.assert_called_once_with()
        self.assertIs(result["native_artifacts"], artifacts)
        self.assertFalse(result["installed_compatibility_wheel_qualified"])
        self.assertFalse(result["behavioral_parity_qualified"])
        self.assertFalse(result["defaults_changed"])
        self.assertEqual(result["engine_operations_executed"], 0)

    def test_changed_exact_native_artifacts_refuse(self):
        shared = Mock()
        shared.inventory_rows.side_effect = SHARED.inventory_rows
        shared.strict_artifact.side_effect = [
            {"wheel_sha256": "before"},
            {"wheel_sha256": "after"},
        ]
        with (
            patch.object(DRIVER, "refuse_existing_namespace"),
            patch.object(DRIVER, "load_shared", return_value=shared),
            patch.object(DRIVER, "verify_forwarding", return_value=[]),
            self.assertRaisesRegex(RuntimeError, "inputs changed"),
        ):
            DRIVER.qualify(REFERENCE, Path("exact.whl"), Path("manifest.json"))

    def test_optimized_library_entrypoint_refuses_before_native_loading(self):
        path = ROOT / "scripts/qualify_native_compatibility.py"
        namespace = {"__name__": "optimized_gate", "__file__": str(path)}
        exec(compile(path.read_text(), str(path), "exec", optimize=1), namespace)
        with self.assertRaisesRegex(RuntimeError, "enabled Python assertions"):
            namespace["qualify"](REFERENCE, Path("unused.whl"), Path("unused.json"))

    def test_driver_has_no_install_build_or_engine_operation_calls(self):
        tree = ast.parse((ROOT / "scripts/qualify_native_compatibility.py").read_text())
        imports = {
            node.module for node in ast.walk(tree) if isinstance(node, ast.ImportFrom)
        }
        imports.update(
            alias.name
            for node in ast.walk(tree)
            if isinstance(node, ast.Import)
            for alias in node.names
        )
        self.assertFalse(
            imports & {"subprocess", "sqlite3", "catabolic", "catabolic_native"}
        )
        text = (ROOT / "scripts/qualify_native_compatibility.py").read_text()
        self.assertEqual(text.count("shared.strict_artifact("), 2)
        self.assertNotIn("verify_installed_wheel(", text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
