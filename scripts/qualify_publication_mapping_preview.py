#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Source/artifact-bound v2 read-only preview evidence, never release proof."""

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
FILES = (
    "scripts/qualify_publication_mapping_preview.py",
    "scripts/replay_publication_preview_python311.py",
    "scripts/build_native_wheel.py",
    "tests/__init__.py",
    "tests/test_rust_publication_mapping_preview.py",
    "tests/test_rust_publication_bundle_preview.py",
    "tests/test_rust_legacy_review.py",
    "tests/test_rust_legacy_mapping_history.py",
    "tests/test_rust_legacy_projection.py",
)
EXPECTED = {
    "test_populated_complete_plan_and_repeat_rollback",
    "test_complete_file_association_and_paginated_graphql_selections",
    "test_all_seventeen_presets_use_complete_selection",
    "test_definition_and_option_scalar_admission",
    "test_unknown_query_catalog_and_foreign_projection_errors",
    "test_query_revision_and_definition_options_pin_plan",
    "test_empty_query_guard_and_allow_empty_plans_rollback",
    "test_pending_history_order_and_other_profile_isolation",
    "test_nonfolder_preview_admission_and_apply_recover_no_effects",
}

BUNDLE_EXPECTED = {
    "test_nfo_ignores_unused_base_url_but_definition_stays_in_identity",
    "test_apply_and_recovery_still_refuse_before_bundle_effects",
    "test_calibre_aliases_preserve_groups_and_destination_identity",
    "test_document_limit_and_source_revision_dedup_refusal",
    "test_export_existing_foreign_destination_and_preflight_collision",
    "test_export_verified_bundle_history_and_changed_artifact",
    "test_immich_cancellation_stops_read_only_helper",
    "test_immich_identity_scope_dedup_and_account_change",
    "test_immich_invalid_identity_status_and_redirect_errors",
    "test_immich_legacy_history_refuses_before_identity_request",
    "test_import_destination_identity_and_untransmitted_metadata",
    "test_import_multiformat_groups_and_duplicate_format_refusal",
    "test_import_submitted_history_dedup_and_revision_guard",
    "test_pending_history_and_empty_selection",
    "test_selection_and_import_unsupported_association",
    "test_three_export_formats_preserve_complete_plan_and_repeat",
}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    args.report.unlink(missing_ok=True)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    if not __debug__:
        raise SystemExit("optimized Python cannot qualify assertions")
    spec = importlib.util.spec_from_file_location(
        "publication_builder", ROOT / "scripts/build_native_wheel.py"
    )
    builder = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(builder)
    source_commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
    ).strip()
    if subprocess.check_output(
        ["git", "status", "--porcelain", "--untracked-files=normal"],
        cwd=ROOT,
        text=True,
    ):
        raise SystemExit("commit the exact source and harness before qualification")
    source = builder.source_digest(ROOT)
    harness = {name: sha(ROOT / name) for name in FILES}
    reference = Path(os.environ["CATABOLIC_PYTHON_REFERENCE"]).resolve()
    oracle = Path(os.environ["CATABOLIC_PYTHON_ORACLE"]).absolute()
    version = subprocess.check_output(
        [str(oracle), "-I", "-c", "import platform;print(platform.python_version())"],
        text=True,
    ).strip()
    if version != "3.14.6":
        raise SystemExit("requires frozen Python 3.14.6")

    def fence():
        if (
            subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
            ).strip()
            != source_commit
        ):
            raise SystemExit("source commit drift")
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
        if source != builder.source_digest(ROOT) or harness != {
            name: sha(ROOT / name) for name in FILES
        }:
            raise SystemExit("source or harness drift")

    fence()
    env = dict(
        os.environ,
        CARGO_TARGET_DIR=str(ROOT / "target"),
        CARGO_NET_OFFLINE="true",
        CARGO_BUILD_JOBS="1",
        CARGO_INCREMENTAL="0",
        CARGO_PROFILE_DEV_DEBUG="0",
        CARGO_PROFILE_TEST_DEBUG="0",
    )
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
            "publication_mappings::tests",
        ],
        [
            "cargo",
            "test",
            "--locked",
            "--offline",
            "-p",
            "catabolic-store",
            "--lib",
            "mapping_history::tests",
        ],
    ]
    logs = []
    for index, command in enumerate(commands):
        path = args.report.parent / f"qualification-{index}.log"
        with path.open("w") as stream:
            subprocess.run(
                command,
                cwd=ROOT,
                env=env,
                stdout=stream,
                stderr=subprocess.STDOUT,
                check=True,
            )
        logs.append({"path": str(path.absolute()), "sha256": sha(path)})
    if "9 passed; 0 failed" not in Path(logs[1]["path"]).read_text():
        raise SystemExit("expected nine adapter Rust tests")
    if "7 passed; 0 failed" not in Path(logs[2]["path"]).read_text():
        raise SystemExit("expected seven mapping-history regression tests")
    binary = ROOT / "target/debug/catabolic-legacy-native"
    artifact = sha(binary)
    os.environ["CATABOLIC_LEGACY_BINARY"] = str(binary)
    regression_command = [
        sys.executable,
        "-m",
        "unittest",
        "tests.test_rust_legacy_projection.NativeLegacyProjectionTest.test_unimplemented_owners_stay_closed",
        "tests.test_rust_legacy_mapping_history.NativeLegacyMappingHistoryTest.test_publication_version_two_without_identity_refuses_explicitly",
        "-v",
    ]
    regression_log = args.report.parent / "legacy-gate-regressions.log"
    with regression_log.open("w") as stream:
        subprocess.run(
            regression_command,
            cwd=ROOT,
            env=dict(os.environ, PYTHONPATH=str(ROOT)),
            stdout=stream,
            stderr=subprocess.STDOUT,
            check=True,
        )
    if (
        "Ran 2 tests" not in regression_log.read_text()
        or "skipped" in regression_log.read_text()
    ):
        raise SystemExit("expected two legacy gate regressions without skips")
    logs.append({"path": str(regression_log.absolute()), "sha256": sha(regression_log)})
    sys.path.insert(0, str(ROOT))
    from tests import test_rust_legacy_review as helper
    from tests import test_rust_publication_bundle_preview as bundles
    from tests import test_rust_publication_mapping_preview as cases

    def selections():
        actual = {}
        for module, filename in (
            (cases, "test_rust_publication_mapping_preview.py"),
            (helper, "test_rust_legacy_review.py"),
        ):
            if Path(module.__file__).resolve() != (ROOT / "tests" / filename).resolve():
                raise SystemExit("wrong loaded test module")
            actual[filename] = {}
            for key, expected in (
                ("BINARY", binary.resolve()),
                ("REFERENCE", reference),
                ("PYTHON", oracle),
            ):
                value = Path(os.path.abspath(getattr(module, key)))
                if value != expected:
                    raise SystemExit(f"wrong loaded {key}")
                actual[filename][key] = str(value)
        if cases.ORACLE != helper.ORACLE:
            raise SystemExit("oracle program drift")
        actual["oracle_program_sha256"] = hashlib.sha256(
            cases.ORACLE.encode()
        ).hexdigest()
        return actual

    if (
        Path(bundles.__file__).resolve()
        != (ROOT / "tests/test_rust_publication_bundle_preview.py").resolve()
    ):
        raise SystemExit("wrong loaded bundle module")
    if (
        set(
            unittest.defaultTestLoader.getTestCaseNames(
                bundles.PublicationBundlePreviewTest
            )
        )
        != BUNDLE_EXPECTED
    ):
        raise SystemExit("bundle case set changed")
    loaded = selections()
    if (
        set(
            unittest.defaultTestLoader.getTestCaseNames(
                cases.PublicationMappingPreviewTest
            )
        )
        != EXPECTED
    ):
        raise SystemExit("case set changed")
    result = unittest.TextTestRunner(verbosity=2).run(
        unittest.TestSuite(
            [
                unittest.defaultTestLoader.loadTestsFromTestCase(
                    cases.PublicationMappingPreviewTest
                ),
                unittest.defaultTestLoader.loadTestsFromTestCase(
                    bundles.PublicationBundlePreviewTest
                ),
            ]
        )
    )
    if (
        not result.wasSuccessful()
        or result.skipped
        or result.expectedFailures
        or result.testsRun != len(EXPECTED) + len(BUNDLE_EXPECTED)
    ):
        raise SystemExit("publication preview qualification failed")
    fence()
    if sha(binary) != artifact or selections() != loaded:
        raise SystemExit("artifact or loaded subprocess selections drift")
    args.report.write_text(
        json.dumps(
            {
                "qualification": "local-v2-read-only-publication-previews",
                "source_commit": source_commit,
                "source_sha256": source,
                "binary": str(binary),
                "binary_sha256": artifact,
                "harness_sha256": harness,
                "loaded_selection": loaded,
                "reference": PIN,
                "reference_python": str(oracle),
                "reference_python_version": version,
                "rust_commands": commands,
                "logs": logs,
                "rust_adapter_tests_passed": 9,
                "rust_history_regressions_passed": 7,
                "legacy_gate_regression_command": regression_command,
                "legacy_gate_regressions_passed": 2,
                "folder_tests": sorted(EXPECTED),
                "bundle_tests": sorted(BUNDLE_EXPECTED),
                "cases_passed": result.testsRun,
                "skipped": 0,
                "expected_failures": 0,
                "refusal_only": [
                    "v2 apply",
                    "v2 recovery",
                ],
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
