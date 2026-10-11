# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Pinned printable-table generation and non-mutating drift checks."""

import subprocess
import tempfile
import unittest
from pathlib import Path

from tests import test_rust_legacy_parser as reference


class GraphqlPrintableGeneratorTest(unittest.TestCase):
    def run_generator(self, *args):
        return subprocess.run(
            [
                str(reference.PYTHON),
                str(reference.ROOT / "scripts/generate_graphql_printable.py"),
                *map(str, args),
            ],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_checked_in_table_matches_pinned_interpreter(self):
        result = self.run_generator("--check")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_check_rejects_missing_and_drift_without_writing(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "printable.rs"
            self.assertNotEqual(
                self.run_generator("--check", "--output", path).returncode, 0
            )
            self.assertFalse(path.exists())
            self.assertEqual(self.run_generator("--output", path).returncode, 0)
            self.assertEqual(
                self.run_generator("--check", "--output", path).returncode, 0
            )
            path.write_bytes(path.read_bytes() + b"// deliberate test drift\n")
            before = path.read_bytes()
            self.assertNotEqual(
                self.run_generator("--check", "--output", path).returncode, 0
            )
            self.assertEqual(path.read_bytes(), before)
