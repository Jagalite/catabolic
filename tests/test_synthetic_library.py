import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from tests.synthetic_benchmark import run_fixture
from tests.synthetic_library import SyntheticLibrary


class SyntheticLibraryTest(unittest.TestCase):
    def test_generated_library_handles_full_lifecycle(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "fake-library"
            report = run_fixture(root, 40)
            self.assertTrue(report["passed"])
            self.assertEqual(report["final_healthy_links"], 40)
            self.assertEqual(report["inventory_files"], 44)
            self.assertEqual(len(report["checks"]), 10)
            self.assertTrue(all(report["checks"].values()))
            self.assertEqual(json.loads((root / "report.json").read_text()), report)

    def test_generator_refuses_existing_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sentinel = root / "keep.txt"
            sentinel.write_text("existing data")
            with self.assertRaises(FileExistsError):
                SyntheticLibrary.create(root, 10)
            self.assertEqual(list(root.iterdir()), [sentinel])
            self.assertEqual(sentinel.read_text(), "existing data")

    def test_crash_worker_refuses_unmarked_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            child = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "tests.synthetic_benchmark",
                    "--crash-worker",
                    str(root / "catalog.sqlite3"),
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertNotEqual(child.returncode, 0)
            self.assertNotEqual(child.returncode, 86)
            self.assertEqual(list(root.iterdir()), [])
