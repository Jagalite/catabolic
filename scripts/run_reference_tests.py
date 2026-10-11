#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Run the Python application's tests separately from opt-in native scenarios.

Native scenarios are required by the native qualification job; excluding them
here does not qualify, skip-as-pass, or remove any original Python scenario.
"""

import argparse
import json
import os
import sys
import unittest
from pathlib import Path


class ReferenceLoader(unittest.TestLoader):
    def __init__(self):
        super().__init__()
        self.native_files = []

    def _match_path(self, path, full_path, pattern):
        # This discovery hook is exercised on the supported Python 3.11/3.14
        # lanes. Exclude before import: native-only dependencies are optional.
        if Path(path).name.startswith("test_rust_"):
            self.native_files.append(path)
            return False
        return super()._match_path(path, full_path, pattern)


def identifiers(suite):
    for test in suite:
        if isinstance(test, unittest.TestSuite):
            yield from identifiers(test)
        else:
            yield test.id()


def main():
    if not __debug__:
        raise RuntimeError("reference qualification requires unoptimized Python")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--list", action="store_true", help="discovery only; no pass claim"
    )
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    os.chdir(root)
    sys.path.insert(0, str(root))
    loader = ReferenceLoader()
    suite = loader.discover("tests", top_level_dir=".")
    cases = list(identifiers(suite))
    if not cases or len(cases) != len(set(cases)):
        raise RuntimeError("empty or duplicate Python reference test discovery")
    report = {
        "scope": "Python application tests; native scenarios run in their separate job",
        "python": sys.version,
        "scenarios": cases,
        "native_files_deferred_to_native_job": sorted(loader.native_files),
        "discovery_only": args.list,
    }
    if args.list:
        print(json.dumps(report, indent=2))
        return 0
    result = unittest.TextTestRunner(verbosity=1).run(suite)
    report.update(
        tests_run=result.testsRun,
        passed=result.wasSuccessful() and result.testsRun == len(cases),
        failures=[
            {"id": test.id(), "traceback": detail} for test, detail in result.failures
        ],
        errors=[
            {"id": test.id(), "traceback": detail} for test, detail in result.errors
        ],
        skips=[{"id": test.id(), "reason": reason} for test, reason in result.skipped],
    )
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
