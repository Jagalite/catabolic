# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fail-closed ASGI aggregate routing without importing an installed extension."""

import hashlib
import io
import sys
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path
from unittest.mock import patch

from scripts import native_asgi_scenarios as manifest
from scripts import qualify_native_mutations as qualifier

ROOT = Path(__file__).resolve().parents[1]


class InstalledASGIDiscoveryTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / "tests").mkdir()
        (self.root / "tests/__init__.py").write_text("")
        for relative in manifest.REQUIRED_FILES:
            # This code must never execute in aggregate discovery. Keep the
            # actual declarations so static discovery uses the real inventory.
            (self.root / relative).write_text(
                "import installed_asgi_module_must_not_be_imported\n"
                + (ROOT / relative).read_text()
            )
        (self.root / "tests/test_rust_probe.py").write_text(
            "import unittest\n"
            "class NativeProbe(unittest.TestCase):\n"
            "    def test_selected(self): pass\n"
        )

    @contextmanager
    def clean_discovery(self):
        with patch.dict(sys.modules), patch.object(sys, "path", sys.path.copy()):
            for name in list(sys.modules):
                if name == "tests" or name.startswith("tests."):
                    del sys.modules[name]
            yield

    def discover(self, pattern="test_rust_*.py"):
        loader = manifest.InstalledASGIDeferredLoader(self.root)
        suite = loader.discover(
            str(self.root / "tests"), pattern, top_level_dir=str(self.root)
        )
        identifiers = list(qualifier.cases(suite))
        prerequisite = loader.verify_routing(identifiers)
        return loader, suite, prerequisite

    def test_real_inventory_is_exact_without_importing_either_module(self):
        before = set(sys.modules)
        hashes = manifest.validate_sources(ROOT)
        self.assertEqual(len(manifest.REQUIRED_IDS), 21)
        self.assertEqual(len(manifest.SMOKE_IDS), 6)
        self.assertEqual(len(set(manifest.REQUIRED_IDS)), 21)
        self.assertEqual(
            hashes,
            {
                name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
                for name in manifest.REQUIRED_FILES
            },
        )
        self.assertEqual(set(sys.modules), before)

    def test_exact_preimport_deferral_records_pending_mandatory_lanes(self):
        with self.clean_discovery():
            loader, suite, prerequisite = self.discover()
            self.assertEqual(
                list(qualifier.cases(suite)),
                ["tests.test_rust_probe.NativeProbe.test_selected"],
            )
            result = unittest.TextTestRunner(stream=io.StringIO()).run(suite)
            self.assertTrue(result.wasSuccessful())
            self.assertEqual(result.testsRun, 1)
            self.assertEqual(result.skipped, [])
            self.assertEqual(
                sorted(loader.deferred_files), sorted(manifest.REQUIRED_FILES)
            )
            self.assertTrue(prerequisite["mandatory"])
            self.assertTrue(prerequisite["manifest_verified"])
            self.assertEqual(prerequisite["status"], "pending")
            self.assertEqual(
                prerequisite["required_scenarios"], list(manifest.REQUIRED_IDS)
            )
            self.assertEqual(
                prerequisite["required_lanes"],
                [
                    {
                        "python": "3.14.6",
                        "gate_arguments": ["--lane", "full"],
                        "scenario_ids": list(manifest.REQUIRED_IDS),
                    },
                    {
                        "python": "3.11",
                        "gate_arguments": ["--lane", "minimal"],
                        "scenario_ids": list(manifest.SMOKE_IDS),
                    },
                ],
            )
            for name in manifest.REQUIRED_CASES:
                self.assertNotIn(name.rsplit(".", 1)[0], sys.modules)

    def test_default_discovery_is_unchanged_without_opt_in(self):
        expected = unittest.TestSuite()
        with patch.object(
            unittest.defaultTestLoader, "discover", return_value=expected
        ) as discover:
            suite, prerequisites = qualifier.discover_native_suite(
                self.root, "test_rust_probe.py"
            )
        self.assertIs(suite, expected)
        self.assertEqual(prerequisites, [])
        discover.assert_called_once_with(
            "tests", "test_rust_probe.py", top_level_dir="."
        )

    def test_default_discovery_refuses_installed_prerequisites_before_poison_import(
        self,
    ):
        with (
            self.clean_discovery(),
            patch.object(unittest.defaultTestLoader, "discover") as discover,
        ):
            with self.assertRaisesRegex(
                ValueError, "strict .* gate or explicit --defer-installed-asgi"
            ):
                qualifier.discover_native_suite(self.root, "test_rust_*.py")
            discover.assert_not_called()
            self.assertNotIn("installed_asgi_module_must_not_be_imported", sys.modules)
            self.assertNotIn("tests.test_rust_probe", sys.modules)
            for name in manifest.REQUIRED_CASES:
                self.assertNotIn(name.rsplit(".", 1)[0], sys.modules)

    def test_broad_default_refuses_both_missing_or_renamed_mandatory_files(self):
        for name in manifest.REQUIRED_FILES:
            (self.root / name).rename(self.root / (name + ".removed"))
        for pattern in (
            "test_rust_*.py",
            "test_rust_asgi_adapter.py",
            "test_rust_asgi_smoke.py",
        ):
            with (
                self.subTest(pattern=pattern),
                patch.object(unittest.defaultTestLoader, "discover") as discover,
            ):
                with self.assertRaisesRegex(ValueError, "file inventory drift"):
                    qualifier.discover_native_suite(self.root, pattern)
                discover.assert_not_called()

    def test_missing_renamed_and_additional_files_fail_before_any_import(self):
        path = self.root / manifest.REQUIRED_FILES[0]
        original = path.read_bytes()
        for replacement in (
            None,
            "test_rust_asgi_renamed.py",
            "test_rust_asgi_extra.py",
        ):
            with self.subTest(replacement=replacement), self.clean_discovery():
                extra = None
                if replacement != "test_rust_asgi_extra.py":
                    path.unlink()
                if replacement:
                    extra = path.with_name(replacement)
                    extra.write_bytes(original)
                try:
                    with self.assertRaisesRegex(ValueError, "file inventory drift"):
                        self.discover()
                    self.assertNotIn("tests.test_rust_probe", sys.modules)
                finally:
                    path.write_bytes(original)
                    if extra:
                        extra.unlink()

    def test_nested_duplicate_and_symlinked_files_are_not_silently_deferred(self):
        nested = self.root / "tests/nested"
        nested.mkdir()
        extra = nested / Path(manifest.REQUIRED_FILES[0]).name
        extra.write_text("raise RuntimeError('must not import')\n")
        with self.assertRaisesRegex(ValueError, "file inventory drift"):
            manifest.validate_sources(self.root)
        extra.unlink()
        path = self.root / manifest.REQUIRED_FILES[0]
        path.unlink()
        path.symlink_to(ROOT / manifest.REQUIRED_FILES[0])
        with self.assertRaisesRegex(ValueError, "linked installed ASGI source"):
            manifest.validate_sources(self.root)

    def test_missing_added_renamed_and_duplicate_method_ids_fail_closed(self):
        path = self.root / manifest.REQUIRED_FILES[1]
        source = path.read_text()
        method = manifest.REQUIRED_CASES[
            "tests.test_rust_asgi_smoke.InstalledASGISmoke"
        ][0]
        changes = {
            "missing": source.replace(f"def {method}(", "def no_longer_a_test("),
            "renamed": source.replace(f"def {method}(", "def test_renamed("),
            "added": source.replace(
                "    def setUp(self):",
                "    async def test_unrecorded(self): pass\n\n    def setUp(self):",
            ),
            "duplicate": source.replace(
                "    def setUp(self):",
                f"    async def {method}(self): pass\n\n    def setUp(self):",
            ),
        }
        for name, content in changes.items():
            with self.subTest(name=name):
                path.write_text(content)
                with self.assertRaisesRegex(ValueError, "scenario inventory drift"):
                    manifest.validate_sources(self.root)
        path.write_text(source)
        self.assertEqual(len(manifest.validate_sources(self.root)), 2)

    def test_changed_class_hook_decorator_and_extra_scenario_routing_fail_closed(self):
        path = self.root / manifest.REQUIRED_FILES[1]
        source = path.read_text()
        changes = {
            "duplicate class": source
            + "\nclass InstalledASGISmoke(unittest.IsolatedAsyncioTestCase): pass\n",
            "inherited class": source + "\nclass Extra(InstalledASGISmoke): pass\n",
            "unrecorded class": source
            + "\nclass Extra(unittest.TestCase):\n    def test_other(self): pass\n",
            "class base": source.replace(
                "unittest.IsolatedAsyncioTestCase", "unittest.TestCase"
            ),
            "load hook": source
            + "\ndef load_tests(loader, tests, pattern): return tests\n",
            "skip decorator": source.replace(
                "    async def test_real_http_application_without_framework",
                "    @unittest.skip('not allowed')\n    async def test_real_http_application_without_framework",
            ),
            "extra function": source + "\ndef test_unrecorded(): pass\n",
            "module alias": source + "\nExtra = InstalledASGISmoke\n",
            "class alias": source.replace(
                "    def setUp(self):",
                "    test_added = test_real_http_application_without_framework\n    def setUp(self):",
            ),
            "runtime assignment": source
            + "\nInstalledASGISmoke.test_added = lambda self: None\n",
            "runtime setattr": source
            + "\nsetattr(InstalledASGISmoke, 'test_added', lambda self: None)\n",
            "hook alias": source
            + "\nload_tests = lambda loader, tests, pattern: tests\n",
        }
        for name, content in changes.items():
            with self.subTest(name=name):
                path.write_text(content)
                with self.assertRaises(ValueError):
                    manifest.validate_sources(self.root)
        path.write_text(source)

    def test_manifest_duplicate_and_incomplete_discovery_routing_fail_closed(self):
        with patch.object(
            manifest, "REQUIRED_IDS", manifest.REQUIRED_IDS + manifest.REQUIRED_IDS[:1]
        ):
            with self.assertRaisesRegex(
                ValueError, "duplicate installed ASGI manifest"
            ):
                manifest.validate_sources(self.root)
        with self.clean_discovery():
            loader, _, _ = self.discover()
            original = loader.deferred_files.copy()
            for routed in (original[:-1], original + original[:1]):
                loader.deferred_files = routed
                with self.assertRaisesRegex(
                    ValueError, "missing or duplicate.*routing"
                ):
                    loader.verify_routing([])
            loader.deferred_files = original
            for identifier in (
                manifest.REQUIRED_IDS[0],
                "tests.test_rust_asgi_other.Extra.test_other",
            ):
                with self.assertRaisesRegex(
                    ValueError, "leaked into selected aggregate"
                ):
                    loader.verify_routing([identifier])

    def test_narrow_pattern_cannot_hide_the_mandatory_prerequisite(self):
        with self.clean_discovery():
            with self.assertRaisesRegex(ValueError, "pattern does not include all"):
                self.discover("test_rust_probe.py")

    def test_imported_prerequisite_and_source_drift_fail_closed(self):
        name = next(iter(manifest.REQUIRED_CASES)).rsplit(".", 1)[0]
        with patch.dict(sys.modules, {name: object()}):
            with self.assertRaisesRegex(ValueError, "imported before deferral"):
                manifest.InstalledASGIDeferredLoader(self.root)
        with self.clean_discovery():
            loader, _, _ = self.discover()
            with patch.dict(sys.modules, {name: object()}):
                with self.assertRaisesRegex(ValueError, "imported during aggregate"):
                    loader.verify_routing([])
            path = self.root / manifest.REQUIRED_FILES[0]
            path.write_text(path.read_text() + "\n# changed bytes\n")
            with self.assertRaisesRegex(ValueError, "source changed"):
                loader.verify_routing([])
