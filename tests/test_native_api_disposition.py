# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""The 1.0 API disposition: generated outputs, removed modules and names.

Pure Python. A fake native provider stands in for the extension, so these
checks only cover the explanation of deprecated names, never engine behavior.
"""

import importlib
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
DOCUMENT = "docs/PYTHON_API_1_0.md"


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


COMPAT = load(ROOT / "tests/test_native_compatibility_source.py", "compat_source")
GENERATOR = load(ROOT / "scripts/generate_api_disposition.py", "api_disposition")
DISPOSITION = json.loads(GENERATOR.DISPOSITION.read_text(encoding="utf-8"))
INVENTORY = json.loads(GENERATOR.INVENTORY.read_text(encoding="utf-8"))


class GeneratedOutputs(unittest.TestCase):
    def test_generated_files_are_current(self):
        self.assertEqual(GENERATOR.main(["--check"]), 0)

    def test_stale_outputs_are_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            stale = Path(temporary) / "PYTHON_API_1_0.md"
            stale.write_text("stale")
            with patch.object(GENERATOR, "DOCUMENT", stale):
                self.assertEqual(GENERATOR.main(["--check"]), 1)
            missing = Path(temporary) / "_api_disposition.json"
            with patch.object(GENERATOR, "DISPOSITION", missing):
                self.assertEqual(GENERATOR.main(["--check"]), 1)

    def test_counts_match_the_frozen_inventory(self):
        preserved = [
            r for r in INVENTORY["rows"] if r["classification"].startswith("preserve")
        ]
        self.assertEqual(len(preserved), 159)
        self.assertEqual(len(DISPOSITION["supported"]), 159)
        self.assertEqual(
            sum(len(names) for names in DISPOSITION["deprecated"].values()), 868
        )
        self.assertEqual(len(INVENTORY["rows"]), 159 + 868)
        self.assertEqual(DISPOSITION["inventory_sha256"], INVENTORY["inventory_sha256"])
        self.assertEqual(DISPOSITION["version"], "1.0.0")

    def test_supported_and_deprecated_names_are_disjoint(self):
        supported = {(e["module"], e["name"]) for e in DISPOSITION["supported"]}
        for module, names in DISPOSITION["deprecated"].items():
            self.assertFalse(supported & {(module, name) for name in names})

    def test_document_lists_every_deprecated_name(self):
        text = (ROOT / DOCUMENT).read_text(encoding="utf-8")
        for module, names in DISPOSITION["deprecated"].items():
            self.assertIn(f"### `{module}`", text)
            for name in names:
                self.assertIn(f"`{name}`", text)
        self.assertIn("Supported (159)", text)
        self.assertIn("Deprecated in 1.0 (868)", text)

    def test_removed_modules_are_not_shipped_or_supported(self):
        shipped = COMPAT.PACKAGE
        for module in DISPOSITION["removed_modules"]:
            relative = Path(*module.split(".")[1:])
            self.assertFalse((shipped / relative).with_suffix(".py").exists(), module)
            self.assertFalse((shipped / relative).exists(), module)
            self.assertNotIn(module, DISPOSITION["supported_modules"])


class Explanations(unittest.TestCase):
    def test_every_removed_module_raises_with_the_document(self):
        self.assertGreater(len(DISPOSITION["removed_modules"]), 100)
        with COMPAT.isolated_modules(COMPAT.fake_provider()):
            importlib.import_module("catabolic")
            for module in DISPOSITION["removed_modules"]:
                with self.subTest(module=module):
                    with self.assertRaises(ModuleNotFoundError) as caught:
                        importlib.import_module(module)
                    # A removed parent package fails first, under its own name.
                    failed = caught.exception.name
                    self.assertIn(failed, DISPOSITION["removed_modules"])
                    self.assertTrue(module == failed or module.startswith(failed + "."))
                    self.assertIn(DOCUMENT, str(caught.exception))
                    self.assertNotIn(module, sys.modules)

    def test_plain_import_statement_carries_the_message(self):
        with COMPAT.isolated_modules(COMPAT.fake_provider()):
            with self.assertRaisesRegex(ModuleNotFoundError, "PYTHON_API_1_0.md"):
                exec("import catabolic.rules")
            with self.assertRaisesRegex(ModuleNotFoundError, "PYTHON_API_1_0.md"):
                exec("import catabolic.interchange.v3")

    def test_deprecated_name_on_every_shipped_module_raises_with_the_document(self):
        checked = 0
        with COMPAT.isolated_modules(COMPAT.fake_provider()):
            importlib.import_module("catabolic")
            modules = {
                "catabolic."
                + ".".join(p.relative_to(COMPAT.PACKAGE).with_suffix("").parts)
                for p in COMPAT.PACKAGE.rglob("*.py")
                if p.name not in {"__init__.py", "_disposition.py"}
            } | {"catabolic", "catabolic.http"}
            for name in sorted(modules):
                module = importlib.import_module(name)
                bound = set(vars(module))
                for attribute in {
                    n.split(".")[0] for n in DISPOSITION["deprecated"].get(name, [])
                }:
                    if attribute in bound:
                        self.assertIn(
                            attribute, DISPOSITION["retained"].get(name, []), name
                        )
                        continue
                    with self.subTest(module=name, attribute=attribute):
                        with self.assertRaises(AttributeError) as caught:
                            getattr(module, attribute)
                        self.assertIn(DOCUMENT, str(caught.exception))
                        self.assertIn(attribute, str(caught.exception))
                        self.assertFalse(hasattr(module, attribute))
                        checked += 1
        expected = sum(
            len({n.split(".")[0] for n in DISPOSITION["deprecated"].get(name, [])})
            - len(DISPOSITION["retained"].get(name, []))
            for name in modules
        )
        self.assertEqual(checked, expected)
        self.assertGreater(checked, 50)

    def test_unlisted_names_keep_standard_errors(self):
        with COMPAT.isolated_modules(COMPAT.fake_provider()):
            store = importlib.import_module("catabolic.store")
            missing = "never_existed"
            with self.assertRaises(AttributeError) as caught:
                getattr(store, missing)
            self.assertNotIn(DOCUMENT, str(caught.exception))
            with self.assertRaises(ModuleNotFoundError) as caught:
                importlib.import_module("catabolic.never_existed")
            self.assertNotIn(DOCUMENT, str(caught.exception))
            with self.assertRaises(ModuleNotFoundError) as caught:
                importlib.import_module("catabolic.http.never_existed")
            self.assertNotIn(DOCUMENT, str(caught.exception))

    def test_supported_names_and_retained_types_are_untouched(self):
        provider = COMPAT.fake_provider()
        with COMPAT.isolated_modules(provider):
            result = COMPAT.DRIVER.verify_forwarding(
                COMPAT.PROJECT / "python", COMPAT.ROWS
            )
            self.assertEqual(len(result), 159)
            for module, names in DISPOSITION["retained"].items():
                shim = importlib.import_module(module)
                native = provider[module.replace("catabolic", "catabolic_native", 1)]
                for name in names:
                    self.assertIs(getattr(shim, name), getattr(native, name))

    def test_removed_module_finder_is_last_and_installed_once(self):
        with COMPAT.isolated_modules(COMPAT.fake_provider()):
            importlib.import_module("catabolic")
            importlib.import_module("catabolic.store")
            finders = [
                f for f in sys.meta_path if type(f).__name__ == "_RemovedModules"
            ]
            self.assertEqual(len(finders), 1)
            self.assertIs(sys.meta_path[-1], finders[0])
            self.assertIsNone(finders[0].find_spec("catabolic.never_existed"))
            self.assertIsNone(finders[0].find_spec("other.rules"))

    def test_supported_imports_never_read_the_disposition_file(self):
        with COMPAT.isolated_modules(COMPAT.fake_provider()):
            disposition = importlib.import_module("catabolic._disposition")
            with patch.object(disposition.json, "loads", side_effect=AssertionError):
                importlib.import_module("catabolic.store")
                importlib.import_module("catabolic.cli")


if __name__ == "__main__":
    unittest.main(verbosity=2)
