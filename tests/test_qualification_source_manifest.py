# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Disposable source provenance controls; no compiler or product qualification."""

import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "qualification_source_manifest",
    Path(__file__).resolve().parents[1] / "scripts/qualification_source_manifest.py",
)
MANIFEST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MANIFEST)


class SourceManifestTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "checkout"
        self.root.mkdir()
        self.output = self.root.parent / "source.json"
        self.git("init", "-q")
        self.put("Cargo.toml", "workspace")
        self.put(".gitignore", "artifacts/\n")
        for name in (
            "scripts/build.py",
            "tests/control.py",
            ".github/workflows/native.yml",
            "requirements/runtime.lock",
            "crates/core/src/lib.rs",
            "src/package/__init__.py",
        ):
            self.put(name, name)
        self.git("add", ".")
        self.git(
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "fixture",
        )

    def git(self, *arguments):
        return subprocess.check_output(["git", "-C", str(self.root), *arguments])

    def put(self, name, value):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value)
        return path

    def test_full_sources_and_new_untracked_files_are_fenced(self):
        self.put("crates/store/resources/035_new.sql", "SELECT 1;")
        first = MANIFEST.write_new(self.root, self.output)
        self.assertEqual(len(first["files"]), 9)
        self.assertEqual(MANIFEST.verify(self.root, self.output), first)
        for name in first["files"]:
            if name == ".gitignore":
                continue
            path = self.root / name
            original = path.read_text()
            path.write_text(original + "changed")
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "differs"):
                MANIFEST.verify(self.root, self.output)
            path.write_text(original)
        self.assertEqual(MANIFEST.verify(self.root, self.output), first)

    def test_capture_is_deterministic_and_ignored_outputs_are_not_source(self):
        first = MANIFEST.capture(self.root)
        self.put("artifacts/native", "generated binary")
        self.assertEqual(first, MANIFEST.capture(self.root))
        self.git("add", "--force", "artifacts/native")
        self.assertIn("artifacts/native", MANIFEST.capture(self.root)["files"])

    def test_missing_tracked_source_is_an_error(self):
        (self.root / "scripts/build.py").unlink()
        with self.assertRaises(FileNotFoundError):
            MANIFEST.write_new(self.root, self.output)
        self.assertFalse(self.output.exists())

    def test_mode_and_added_or_renamed_files_change_snapshot(self):
        before = MANIFEST.capture(self.root)
        (self.root / "scripts/build.py").chmod(0o755)
        self.assertNotEqual(before, MANIFEST.capture(self.root))
        (self.root / "scripts/build.py").chmod(0o644)
        self.assertEqual(before, MANIFEST.capture(self.root))
        self.put("extra.py", "new")
        self.assertNotEqual(before, MANIFEST.capture(self.root))

    def test_symlink_input_is_refused_without_following(self):
        (self.root / "linked").symlink_to(self.root.parent / "absent")
        with self.assertRaisesRegex(ValueError, "regular file"):
            MANIFEST.capture(self.root)

    def test_nested_checkout_root_is_refused(self):
        with self.assertRaisesRegex(ValueError, "checkout root"):
            MANIFEST.capture(self.root / "scripts")

    def test_in_tree_output_is_refused_without_creating_it(self):
        output = self.root / "manifest.json"
        with self.assertRaisesRegex(ValueError, "outside"):
            MANIFEST.write_new(self.root, output)
        self.assertFalse(output.exists())

    def test_existing_output_is_preserved(self):
        self.output.write_text("prior receipt")
        with self.assertRaises(FileExistsError):
            MANIFEST.write_new(self.root, self.output)
        self.assertEqual(self.output.read_text(), "prior receipt")

    def test_rewritten_manifest_cannot_claim_unchanged_source(self):
        expected = MANIFEST.write_new(self.root, self.output)
        expected["files"]["scripts/build.py"]["sha256"] = "0" * 64
        self.output.write_text(json.dumps(expected))
        with self.assertRaisesRegex(ValueError, "differs"):
            MANIFEST.verify(self.root, self.output)

    def test_inventory_changes_during_capture_are_refused(self):
        names = MANIFEST.inventory(self.root)
        with patch.object(MANIFEST, "inventory", side_effect=[names, names[:-1]]):
            with self.assertRaisesRegex(ValueError, "changed during capture"):
                MANIFEST.write_new(self.root, self.output)
        self.assertFalse(self.output.exists())

    def test_file_changes_during_hash_are_refused(self):
        original = MANIFEST.hashlib.file_digest

        def mutate(stream, algorithm):
            result = original(stream, algorithm)
            with open(stream.name, "ab") as target:
                target.write(b"changed")
            return result

        with patch.object(MANIFEST.hashlib, "file_digest", side_effect=mutate):
            with self.assertRaisesRegex(ValueError, "changed during hashing"):
                MANIFEST.write_new(self.root, self.output)
        self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
