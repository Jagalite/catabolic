#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Run fixed pinned-oracle prerequisites against the supplied installed wheel."""

import argparse
import hashlib
import importlib
import json
import os
import sys
import unittest
from pathlib import Path

REQUIRED_CASES = {
    "tests.test_graphql_printable_generator.GraphqlPrintableGeneratorTest": (
        "test_check_rejects_missing_and_drift_without_writing",
        "test_checked_in_table_matches_pinned_interpreter",
    ),
    "tests.test_native_content_revision_contract.ContentRevisionInstalledChecks": (
        "test_frozen_paired_groups_and_persistent_ownership",
    ),
    "tests.test_native_content_revision_review.RevisionInstalledOwnerReview": (
        "test_disposable_owner_boundaries",
    ),
}
REQUIRED_IDS = tuple(
    f"{case}.{method}" for case, methods in REQUIRED_CASES.items() for method in methods
)
SOURCE_FILES = (
    "scripts/run_native_prerequisite_tests.py",
    "scripts/native_qualification_results.py",
    "scripts/native_facade_qualification.py",
    "scripts/qualify_native_wheel.py",
    "scripts/qualify_native_embedding.py",
    "scripts/native_content_revision_tests.py",
    "scripts/generate_graphql_printable.py",
    "tests/__init__.py",
    "tests/test_graphql_printable_generator.py",
    "tests/test_native_content_revision_contract.py",
    "tests/test_native_content_revision_review.py",
    "tests/test_rust_legacy_parser.py",
    "tests/parity/python-api.json",
    "crates/catabolic-store/src/graphql_printable.rs",
)


def source_manifest(root):
    return {
        name: hashlib.sha256((root / name).read_bytes()).hexdigest()
        for name in SOURCE_FILES
    }


def required_suite():
    loader = unittest.TestLoader()
    expected = {}
    for name, methods in REQUIRED_CASES.items():
        module, _, attribute = name.rpartition(".")
        case = getattr(importlib.import_module(module), attribute)
        expected[case] = methods
    suite = unittest.TestSuite(loader.loadTestsFromTestCase(case) for case in expected)
    return suite, expected


def main():
    if not __debug__:
        raise RuntimeError("native prerequisites require unoptimized Python")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wheel", required=True, type=Path)
    parser.add_argument("--reference-root", required=True, type=Path)
    parser.add_argument("--reference-python", required=True, type=Path)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    sys.path.insert(0, str(root))
    os.environ["CATABOLIC_PYTHON_ORACLE"] = str(args.reference_python.absolute())
    os.environ["CATABOLIC_PYTHON_REFERENCE"] = str(
        args.reference_root.resolve() / "src"
    )
    # Every child must use this same artifact-bound installed interpreter.
    os.environ["CATABOLIC_NATIVE_PYTHON"] = sys.executable
    report = {
        "scope": "Four required pinned-oracle/installed-native prerequisites",
        "passed": False,
        "expected_scenarios": list(REQUIRED_IDS),
        "python": sys.version,
        "native_python": sys.executable,
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)

    def save():
        args.report.write_text(json.dumps(report, indent=2) + "\n")

    save()  # A failed import or preflight cannot leave a previous success receipt.
    try:
        sources_before = source_manifest(root)
        from scripts.native_facade_qualification import FROZEN_COMMIT, reference_runtime
        from scripts.native_qualification_results import (
            run_required_suite,
            scenario_ids,
        )
        from scripts.qualify_native_embedding import reference_provenance
        from scripts.qualify_native_wheel import verify_installed_wheel

        oracle_before = reference_provenance(args.reference_root, FROZEN_COMMIT)
        runtime_before = reference_runtime(args.reference_python)
        before = verify_installed_wheel(args.wheel)
        wheel_digest = hashlib.sha256(args.wheel.read_bytes()).hexdigest()
        suite, expected = required_suite()
        if sorted(scenario_ids(suite)) != sorted(REQUIRED_IDS):
            raise RuntimeError("required native prerequisite scenario IDs differ")
        result = run_required_suite(suite, expected, expected_count=4)
        after = verify_installed_wheel(args.wheel)
        oracle_after = reference_provenance(args.reference_root, FROZEN_COMMIT)
        runtime_after = reference_runtime(args.reference_python)
        sources_after = source_manifest(root)
        if sources_before != sources_after:
            raise RuntimeError("native prerequisite source changed during execution")
        if oracle_before != oracle_after or runtime_before != runtime_after:
            raise RuntimeError("frozen oracle changed during execution")
        if (
            before != after
            or wheel_digest != hashlib.sha256(args.wheel.read_bytes()).hexdigest()
        ):
            raise RuntimeError(
                "installed prerequisite artifact changed during execution"
            )
        report.update(
            passed=True,
            tests_run=result.testsRun,
            successful_scenarios=result.successful_scenarios,
            wheel_sha256=wheel_digest,
            installed=after,
            oracle=oracle_after,
            oracle_runtime=runtime_after,
            source_sha256=sources_after,
            source_stable=True,
            provenance_scope="Local source-content and artifact identity; not a hermetic build claim",
        )
    except Exception as exc:
        report["error"] = f"{type(exc).__name__}: {exc}"
    save()
    print(json.dumps(report, indent=2))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
