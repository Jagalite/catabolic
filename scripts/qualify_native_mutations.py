#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Retain unique native scenarios, exact artifact identity and explicit outcomes.

This local report is checkpoint evidence, not a milestone or release assertion.
Run only against disposable fixtures; production effect gates remain closed.
"""

import argparse
import hashlib
import json
import os
import platform
import subprocess
import sys
import time
import unittest
from pathlib import Path


class Results(unittest.TextTestResult):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.scenarios = []

    def addSuccess(self, test):
        super().addSuccess(test)
        self.scenarios.append({"id": test.id(), "outcome": "passed"})

    def addFailure(self, test, err):
        super().addFailure(test, err)
        self.scenarios.append({"id": test.id(), "outcome": "failed"})

    def addError(self, test, err):
        super().addError(test, err)
        self.scenarios.append({"id": test.id(), "outcome": "error"})

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        self.scenarios.append({"id": test.id(), "outcome": "skipped", "reason": reason})

    def addSubTest(self, test, subtest, err):
        super().addSubTest(test, subtest, err)
        if err is not None:
            self.scenarios.append({"id": subtest.id(), "outcome": "failed-subtest"})


def cases(suite):
    for value in suite:
        if isinstance(value, unittest.TestSuite):
            yield from cases(value)
        else:
            yield value.id()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--pattern", default="test_rust_*.py")
    parser.add_argument("--expected-count", type=int)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    os.chdir(root)
    binary = args.binary.resolve(strict=True)
    os.environ["CATABOLIC_NATIVE_BINARY"] = str(binary)
    sys.path.insert(0, str(root))
    suite = unittest.defaultTestLoader.discover(
        "tests", args.pattern, top_level_dir="."
    )
    identifiers = list(cases(suite))
    if not identifiers:
        parser.error(
            "no scenarios match the requested pattern; no qualification was performed"
        )
    if args.expected_count is not None and len(identifiers) != args.expected_count:
        parser.error(
            f"expected {args.expected_count} scenarios, discovered {len(identifiers)}"
        )
    if len(identifiers) != len(set(identifiers)):
        raise AssertionError(
            "duplicate discovered test IDs cannot inflate qualification"
        )
    start = time.monotonic()
    result = unittest.TextTestRunner(verbosity=2, resultclass=Results).run(suite)
    complete = (
        result.testsRun == len(identifiers)
        and {item["id"] for item in result.scenarios} == set(identifiers)
        and len(result.scenarios) == len(identifiers)
    )
    value = {
        "scope": "Opt-in native companion checkpoint; not complete milestone qualification",
        "platform": platform.platform(),
        "python": sys.version,
        "binary": str(binary),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "version": json.loads(
            subprocess.check_output([str(binary), "--version"], text=True)
        ),
        "tests_run": result.testsRun,
        "unique_discovered_scenarios": identifiers,
        "results": result.scenarios,
        "failure_details": [
            {"id": t.id(), "traceback": detail} for t, detail in result.failures
        ],
        "error_details": [
            {"id": t.id(), "traceback": detail} for t, detail in result.errors
        ],
        "skips": [{"id": t.id(), "reason": reason} for t, reason in result.skipped],
        "seconds": time.monotonic() - start,
        "outcome_coverage_complete": complete,
        "passed_without_skips": complete
        and result.wasSuccessful()
        and all(item["outcome"] == "passed" for item in result.scenarios),
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(value, indent=2) + "\n")
    return 0 if value["passed_without_skips"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
