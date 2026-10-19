#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Source-bound local projection orchestration receipt; no release qualification."""

import argparse
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PIN = "453fca983222c6665a48775eef67c489c96b527e"
HARNESS_FILES = (
    "scripts/qualify_projection_run.py",
    "scripts/build_native_wheel.py",
    "tests/__init__.py",
    "tests/test_rust_legacy_projection_run.py",
    "tests/test_rust_legacy_review.py",
    "tests/test_projection_run_qualification.py",
)
EXPECTED_CONTROLS = {
    "test_helper_mutation_is_fenced",
    "test_matching_loaded_selections_are_recorded",
    "test_each_loaded_path_misdirection_is_refused",
    "test_interpreter_symlink_alias_does_not_hide_venv_misdirection",
    "test_wrong_module_origin_and_oracle_program_are_refused",
}
EXPECTED = {
    "test_empty_selection_guard_and_allowed_disable_both_rollback",
    "test_preview_full_and_truncated_reports_rollback_all_effects",
    "test_safe_empty_preview_and_repeat_have_no_metadata_or_output_effects",
    "test_admission_order_errors_and_post_staging_budget_rollback",
    "test_catalog_writer_contention_and_release_exact_channels",
    "test_execute_early_gate_is_explicit_nonparity_and_no_effects",
    "test_fallback_refuses_before_invalid_layout_or_output_planning",
    "test_interrupt_after_writer_admission_nonparity_and_rollback",
    "test_unsafe_layout_short_circuits_reconciliation_and_rolls_back",
}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def harness_snapshot(root=ROOT):
    return {path: sha(root / path) for path in HARNESS_FILES}


def check_harness(expected, root=ROOT):
    if harness_snapshot(root) != expected:
        raise SystemExit("qualification harness drift")


def loaded_selection(cases, helper, binary, reference, oracle, root=ROOT):
    """Fence the subprocess selections actually consumed by the loaded suite.

    Interpreter path equality deliberately preserves the venv path: different
    venv executables can resolve to the same underlying interpreter binary.
    """
    expected = {
        "BINARY": binary.resolve(),
        "REFERENCE": reference.resolve(),
        "PYTHON": Path(os.path.abspath(oracle)),
    }
    for module, filename in (
        (cases, "test_rust_legacy_projection_run.py"),
        (helper, "test_rust_legacy_review.py"),
    ):
        if Path(module.__file__).resolve() != (root / "tests" / filename).resolve():
            raise SystemExit(f"qualification loaded wrong module: {filename}")
        for key, path in expected.items():
            actual = Path(os.path.abspath(getattr(module, key)))
            if actual != path:
                raise SystemExit(f"qualification loaded {key} mismatch: {filename}")
    if not isinstance(cases.ORACLE, str) or cases.ORACLE != helper.ORACLE:
        raise SystemExit("qualification loaded ORACLE mismatch")
    return {
        **{key.lower(): str(path) for key, path in expected.items()},
        "test_module": str(Path(cases.__file__).resolve()),
        "helper_module": str(Path(helper.__file__).resolve()),
        "oracle_program_sha256": hashlib.sha256(cases.ORACLE.encode()).hexdigest(),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    args.report.unlink(missing_ok=True)
    if not __debug__:
        raise SystemExit("optimized Python cannot qualify assertions")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    spec = importlib.util.spec_from_file_location(
        "build_native_wheel", ROOT / "scripts/build_native_wheel.py"
    )
    builder = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(builder)
    source = builder.source_digest(ROOT)
    hashes = harness_snapshot()
    reference = Path(os.environ["CATABOLIC_PYTHON_REFERENCE"]).resolve()
    oracle = Path(os.environ["CATABOLIC_PYTHON_ORACLE"]).absolute()
    version = subprocess.check_output(
        [str(oracle), "-I", "-c", "import platform;print(platform.python_version())"],
        text=True,
    ).strip()
    if version != "3.14.6":
        raise SystemExit("requires frozen Python 3.14.6")

    def reference_fence():
        if (
            subprocess.check_output(
                ["git", "-C", str(reference), "rev-parse", "HEAD"], text=True
            ).strip()
            != PIN
        ):
            raise SystemExit("wrong frozen reference")
        subprocess.run(
            ["git", "-C", str(reference), "diff", "--exit-code", PIN, "--"],
            check=True,
            capture_output=True,
        )

    reference_fence()
    env = dict(
        os.environ,
        CARGO_TARGET_DIR=str(ROOT / "target"),
        CARGO_BUILD_JOBS="2",
        CARGO_INCREMENTAL="0",
        CARGO_PROFILE_DEV_DEBUG="0",
        CARGO_PROFILE_TEST_DEBUG="0",
    )
    binary = ROOT / "target/debug/catabolic-legacy-native"
    commands = [
        [
            "cargo",
            "build",
            "--locked",
            "--offline",
            "-p",
            "catabolic-cli",
            "--bin",
            "catabolic-legacy-native",
        ],
        [
            "cargo",
            "test",
            "--locked",
            "--offline",
            "-p",
            "catabolic-store",
            "--lib",
            "projection_run::tests",
        ],
        [
            "cargo",
            "test",
            "--locked",
            "--offline",
            "-p",
            "catabolic-runtime",
            "--lib",
            "projection::tests",
        ],
    ]
    for index, command in enumerate(commands):
        with (args.report.parent / f"qualification-{index}.log").open("w") as stream:
            subprocess.run(
                command,
                cwd=ROOT,
                env=env,
                stdout=stream,
                stderr=subprocess.STDOUT,
                check=True,
            )
    artifact = sha(binary)
    os.environ["CATABOLIC_LEGACY_BINARY"] = str(binary)
    sys.path.insert(0, str(ROOT))
    from tests import test_rust_legacy_projection_run as cases
    from tests import test_rust_legacy_review as helper
    from tests.test_projection_run_qualification import (
        ProjectionQualificationControlTest,
    )

    check_harness(hashes)
    selections = loaded_selection(cases, helper, binary, reference, oracle)
    control_names = set(
        unittest.defaultTestLoader.getTestCaseNames(ProjectionQualificationControlTest)
    )
    if control_names != EXPECTED_CONTROLS:
        raise SystemExit("projection qualification control set changed")
    controls = unittest.TextTestRunner(verbosity=2).run(
        unittest.defaultTestLoader.loadTestsFromTestCase(
            ProjectionQualificationControlTest
        )
    )
    if (
        not controls.wasSuccessful()
        or controls.skipped
        or controls.expectedFailures
        or controls.testsRun != len(EXPECTED_CONTROLS)
    ):
        raise SystemExit("projection qualification controls failed")
    NativeLegacyProjectionRunTest = cases.NativeLegacyProjectionRunTest

    names = set(
        unittest.defaultTestLoader.getTestCaseNames(NativeLegacyProjectionRunTest)
    )
    if names != EXPECTED:
        raise SystemExit("projection qualification case set changed")
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(
        NativeLegacyProjectionRunTest
    )
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if (
        not result.wasSuccessful()
        or result.skipped
        or result.expectedFailures
        or result.testsRun != len(EXPECTED)
    ):
        raise SystemExit("projection orchestration qualification failed")
    reference_fence()
    if builder.source_digest(ROOT) != source or sha(binary) != artifact:
        raise SystemExit("source or binary drift")
    check_harness(hashes)
    if loaded_selection(cases, helper, binary, reference, oracle) != selections:
        raise SystemExit("qualification loaded subprocess selections drift")
    args.report.write_text(
        json.dumps(
            {
                "qualification": "local-partial-projection-orchestration-only",
                "reference": PIN,
                "reference_python": str(oracle),
                "reference_python_version": version,
                "source_sha256": source,
                "binary": str(binary),
                "binary_sha256": artifact,
                "harness_sha256": hashes,
                "loaded_selection": selections,
                "qualification_controls": sorted(EXPECTED_CONTROLS),
                "qualification_controls_passed": controls.testsRun,
                "tests": sorted(EXPECTED),
                "cases_passed": result.testsRun,
                "signal_outcomes": NativeLegacyProjectionRunTest.signal_outcomes,
                "skipped": 0,
                "expected_failures": 0,
                "rust_commands": commands,
                "refusal_only": ["execute", "fallback preview"],
                "publication_qualified": False,
                "python_default_unchanged": True,
            },
            indent=2,
        )
        + "\n"
    )
    print(args.report)


if __name__ == "__main__":
    main()
