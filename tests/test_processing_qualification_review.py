"""Independent fail-closed checks for the processing qualification driver."""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

DRIVER = (
    Path(__file__).resolve().parents[1] / "scripts/native_processing_embedding_tests.py"
)
MESSAGE = "processing qualification requires assertions; do not run with -O or -OO"


class ProcessingQualificationOptimization(unittest.TestCase):
    def test_optimized_modes_fail_before_loading_oracle_or_writing_report(self):
        for flags, optimize in ((["-O"], None), (["-OO"], None), ([], "1"), ([], "2")):
            for scenario in (None, "native", "reference"):
                with self.subTest(flags=flags, optimize=optimize, scenario=scenario):
                    with tempfile.TemporaryDirectory() as directory:
                        report = Path(directory) / "report.json"
                        command = [
                            sys.executable,
                            *flags,
                            str(DRIVER),
                            "--report",
                            str(report),
                        ]
                        if scenario:
                            command += ["--scenario", scenario]
                        env = os.environ.copy()
                        env.pop("PYTHONOPTIMIZE", None)
                        if optimize:
                            env["PYTHONOPTIMIZE"] = optimize
                        result = subprocess.run(
                            command,
                            cwd=directory,
                            env=env,
                            capture_output=True,
                            text=True,
                            timeout=10,
                        )
                        self.assertNotEqual(result.returncode, 0)
                        self.assertEqual(result.stderr.strip(), MESSAGE)
                        self.assertEqual(result.stdout, "")
                        self.assertFalse(report.exists())

    def test_normal_help_remains_available(self):
        env = os.environ.copy()
        env.pop("PYTHONOPTIMIZE", None)
        result = subprocess.run(
            [sys.executable, str(DRIVER), "--help"],
            env=env,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--scenario", result.stdout)
