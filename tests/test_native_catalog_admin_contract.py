# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Source-only delegation seam, not an installed native acceptance gate."""

import importlib.util
import inspect
import json
import sys
import types
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / "crates/catabolic-python/python/catabolic_native"


class NativeCatalogAdminContract(unittest.TestCase):
    def setUp(self):
        self.native = types.SimpleNamespace(
            native_catalog_inspect=Mock(return_value='{"owner":"native-inspection"}'),
            native_catalog_plan_upgrade=Mock(return_value='{"owner":"native-plan"}'),
        )
        package = types.ModuleType("catalog_fixture")
        package._native = self.native
        spec = importlib.util.spec_from_file_location(
            "catalog_fixture.catalog_admin", PACKAGE / "catalog_admin.py"
        )
        self.admin = importlib.util.module_from_spec(spec)
        with patch.dict(sys.modules, {"catalog_fixture": package}):
            spec.loader.exec_module(self.admin)

    def test_import_does_not_open_inspect_or_plan_a_catalog(self):
        self.native.native_catalog_inspect.assert_not_called()
        self.native.native_catalog_plan_upgrade.assert_not_called()

    def test_exact_optional_module_signatures(self):
        self.assertEqual(
            str(inspect.signature(self.admin.inspect_database)), "(path, *, full=False)"
        )
        self.assertEqual(str(inspect.signature(self.admin.plan_upgrade)), "(path)")

    def test_inspection_forwards_path_and_native_full_flag_unchanged(self):
        path = Path("relative/catalog.sqlite3")
        self.assertEqual(
            self.admin.inspect_database(path), {"owner": "native-inspection"}
        )
        self.native.native_catalog_inspect.assert_called_once_with(str(path), False)
        self.admin.inspect_database(path, full=True)
        self.native.native_catalog_inspect.assert_called_with(str(path), True)
        self.native.native_catalog_plan_upgrade.assert_not_called()

    def test_plan_preserves_native_result_without_inventing_authority(self):
        value = {"to_schema": 999, "quiescence_required": True, "input": {"id": "é"}}
        self.native.native_catalog_plan_upgrade.return_value = json.dumps(value)
        self.assertEqual(self.admin.plan_upgrade("~/operator-input"), value)
        self.native.native_catalog_plan_upgrade.assert_called_once_with(
            "~/operator-input"
        )
        self.native.native_catalog_inspect.assert_not_called()

    def test_native_errors_propagate_without_retry_or_reclassification(self):
        for name, function in (
            ("native_catalog_inspect", self.admin.inspect_database),
            ("native_catalog_plan_upgrade", self.admin.plan_upgrade),
        ):
            error = RuntimeError("native owner diagnostic")
            getattr(self.native, name).side_effect = error
            with self.subTest(name=name), self.assertRaises(RuntimeError) as caught:
                function("catalog")
            self.assertIs(caught.exception, error)
            getattr(self.native, name).assert_called_once()

    def test_missing_extension_method_has_no_python_fallback(self):
        del self.native.native_catalog_inspect
        with self.assertRaises(AttributeError):
            self.admin.inspect_database("catalog")

    def test_rust_boundary_delegates_to_read_only_native_owners(self):
        rust = (ROOT / "crates/catabolic-python/src/catalog_inspection.rs").read_text()
        self.assertIn("migration::inspect_native(Path::new(path), full)", rust)
        self.assertIn("migration::plan_native(Path::new(path))", rust)
        self.assertEqual(rust.count("py.detach("), 2)
        for forbidden in (
            "migration::initialize",
            "migration::upgrade_native",
            "Store::open",
            "rusqlite",
            "CommandEnvelope",
        ):
            self.assertNotIn(forbidden, rust)
        lib = (ROOT / "crates/catabolic-python/src/lib.rs").read_text()
        self.assertEqual(lib.count("catalog_inspection::register(m)?;"), 1)

    def test_default_scripts_and_legacy_import_surface_are_unchanged(self):
        self.assertIn(
            'catabolic = "catabolic.cli:main"', (ROOT / "pyproject.toml").read_text()
        )
        self.assertIn(
            'catabolic-native = "catabolic_native:main"',
            (ROOT / "crates/catabolic-python/pyproject.toml").read_text(),
        )
        self.assertNotIn("catalog_admin", (PACKAGE / "__init__.py").read_text())
        for name in ("upgrade_database", "initialize_database", "execute", "Store"):
            self.assertFalse(hasattr(self.admin, name))


if __name__ == "__main__":
    unittest.main()
