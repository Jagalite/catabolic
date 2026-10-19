# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Negative controls for projection receipt wiring, using disposable files only."""

import importlib.util
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "projection_qualification_gate", ROOT / "scripts/qualify_projection_run.py"
)
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)


class ProjectionQualificationControlTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="projection-gate-controls-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for name in GATE.HARNESS_FILES:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name + "\n")
        self.binary = self.root / "target/debug/native"
        self.reference = self.root / "reference/src"
        self.oracle = self.root / "venv/bin/python"
        self.oracle.parent.mkdir(parents=True)
        self.oracle.write_text("disposable interpreter identity fixture\n")
        values = dict(
            BINARY=self.binary,
            REFERENCE=self.reference,
            PYTHON=self.oracle,
            ORACLE="frozen CLI invocation fixture",
        )
        self.cases = SimpleNamespace(
            __file__=str(self.root / "tests/test_rust_legacy_projection_run.py"),
            **values,
        )
        self.helper = SimpleNamespace(
            __file__=str(self.root / "tests/test_rust_legacy_review.py"), **values
        )

    def selection(self):
        return GATE.loaded_selection(
            self.cases, self.helper, self.binary, self.reference, self.oracle, self.root
        )

    def test_helper_mutation_is_fenced(self):
        before = GATE.harness_snapshot(self.root)
        self.assertIn("tests/test_rust_legacy_review.py", before)
        GATE.check_harness(before, self.root)
        helper = self.root / "tests/test_rust_legacy_review.py"
        helper.write_text(helper.read_text() + "ORACLE = 'altered'\n")
        with self.assertRaisesRegex(SystemExit, "qualification harness drift"):
            GATE.check_harness(before, self.root)

    def test_matching_loaded_selections_are_recorded(self):
        selected = self.selection()
        self.assertEqual(selected["binary"], str(self.binary))
        self.assertEqual(selected["reference"], str(self.reference))
        self.assertEqual(selected["python"], str(self.oracle))
        self.assertEqual(len(selected["oracle_program_sha256"]), 64)

    def test_each_loaded_path_misdirection_is_refused(self):
        for module in (self.cases, self.helper):
            for key in ("BINARY", "REFERENCE", "PYTHON"):
                with self.subTest(module=module.__file__, key=key):
                    previous = getattr(module, key)
                    setattr(module, key, self.root / "wrong-selection")
                    try:
                        with self.assertRaisesRegex(
                            SystemExit, f"loaded {key} mismatch"
                        ):
                            self.selection()
                    finally:
                        setattr(module, key, previous)

    def test_interpreter_symlink_alias_does_not_hide_venv_misdirection(self):
        alias = self.root / "other-venv-python"
        alias.symlink_to(self.oracle)
        self.assertEqual(alias.resolve(), self.oracle.resolve())
        self.cases.PYTHON = alias
        with self.assertRaisesRegex(SystemExit, "loaded PYTHON mismatch"):
            self.selection()

    def test_wrong_module_origin_and_oracle_program_are_refused(self):
        for module in (self.cases, self.helper):
            previous = module.__file__
            module.__file__ = str(self.root / "wrong-module.py")
            try:
                with self.assertRaisesRegex(SystemExit, "loaded wrong module"):
                    self.selection()
            finally:
                module.__file__ = previous
        before = self.selection()
        self.cases.ORACLE = "altered invocation"
        with self.assertRaisesRegex(SystemExit, "loaded ORACLE mismatch"):
            self.selection()
        self.helper.ORACLE = self.cases.ORACLE
        self.assertNotEqual(self.selection(), before)


if __name__ == "__main__":
    unittest.main()
