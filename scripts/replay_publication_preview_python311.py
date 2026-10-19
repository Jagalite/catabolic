#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Replay the exact qualified preview artifact against frozen Python 3.11.15.

No build, dependency install, publication, official import CLI, or real service.
A passing 3.14.6 build receipt is required and every relevant byte is re-fenced.
"""

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


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build-receipt", required=True, type=Path)
    parser.add_argument("--python", required=True, type=Path)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    args.report.unlink(missing_ok=True)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    if not __debug__:
        raise SystemExit("optimized Python cannot replay qualification assertions")
    if subprocess.check_output(
        ["git", "status", "--porcelain", "--untracked-files=normal"],
        cwd=ROOT,
        text=True,
    ):
        raise SystemExit("commit the exact source and harness before replay")
    commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
    ).strip()
    receipt_bytes = args.build_receipt.read_bytes()
    receipt = json.loads(receipt_bytes)
    qualification = module(
        "publication_qualification", "scripts/qualify_publication_mapping_preview.py"
    )
    builder = module("publication_builder", "scripts/build_native_wheel.py")
    oracle = args.python.absolute()
    version = subprocess.check_output(
        [str(oracle), "-I", "-c", "import platform;print(platform.python_version())"],
        text=True,
    ).strip()
    if version != "3.11.15":
        raise SystemExit("requires the full frozen Python 3.11.15 oracle")
    if (
        receipt.get("qualification") != "local-v2-read-only-publication-previews"
        or receipt.get("reference_python_version") != "3.14.6"
        or receipt.get("reference") != qualification.PIN
        or receipt.get("cases_passed")
        != len(qualification.EXPECTED) + len(qualification.BUNDLE_EXPECTED)
        or receipt.get("skipped") != 0
        or receipt.get("expected_failures") != 0
    ):
        raise SystemExit("requires the exact passing Python 3.14.6 build receipt")
    binary = Path(receipt["binary"]).resolve()
    reference = Path(
        receipt["loaded_selection"]["test_rust_legacy_review.py"]["REFERENCE"]
    ).resolve()
    harness = {name: sha(ROOT / name) for name in qualification.FILES}
    if harness != receipt["harness_sha256"]:
        raise SystemExit("replay harness differs from the qualified build")
    harness["scripts/replay_publication_preview_python311.py"] = sha(Path(__file__))
    source = builder.source_digest(ROOT)
    if source != receipt["source_sha256"]:
        raise SystemExit("replay source differs from the qualified artifact")

    def fence():
        if (
            subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
            ).strip()
            != commit
        ):
            raise SystemExit("replay source commit drift")
        if (
            args.build_receipt.read_bytes() != receipt_bytes
            or builder.source_digest(ROOT) != source
            or sha(binary) != receipt["binary_sha256"]
            or {name: sha(ROOT / name) for name in harness} != harness
        ):
            raise SystemExit("build receipt, source, harness or binary drift")
        if (
            subprocess.check_output(
                ["git", "-C", str(reference), "rev-parse", "HEAD"], text=True
            ).strip()
            != qualification.PIN
        ):
            raise SystemExit("wrong frozen reference")
        subprocess.run(
            [
                "git",
                "-C",
                str(reference),
                "diff",
                "--exit-code",
                qualification.PIN,
                "--",
            ],
            check=True,
            capture_output=True,
        )

    fence()
    os.environ.update(
        CATABOLIC_PYTHON_ORACLE=str(oracle),
        CATABOLIC_PYTHON_REFERENCE=str(reference),
        CATABOLIC_LEGACY_BINARY=str(binary),
    )
    sys.path.insert(0, str(ROOT))
    from tests import test_rust_legacy_review as helper
    from tests import test_rust_publication_bundle_preview as bundle
    from tests import test_rust_publication_mapping_preview as folder

    def selections():
        selected = {}
        for current, filename in [
            (helper, "test_rust_legacy_review.py"),
            (folder, "test_rust_publication_mapping_preview.py"),
        ]:
            if (
                Path(current.__file__).resolve()
                != (ROOT / "tests" / filename).resolve()
            ):
                raise SystemExit("wrong loaded replay module")
            selected[filename] = {}
            for key, expected in [
                ("BINARY", binary),
                ("REFERENCE", reference),
                ("PYTHON", oracle),
            ]:
                value = Path(os.path.abspath(getattr(current, key)))
                if value != expected:
                    raise SystemExit(f"wrong loaded replay {key}")
                selected[filename][key] = str(value)
        if (
            Path(bundle.__file__).resolve()
            != (ROOT / "tests/test_rust_publication_bundle_preview.py").resolve()
            or bundle.REFERENCE != reference
            or bundle.PublicationBundlePreviewTest.cli
            is not folder.PublicationMappingPreviewTest.cli
            or bundle.PublicationBundlePreviewTest.python
            is not folder.PublicationMappingPreviewTest.python
            or folder.ORACLE != helper.ORACLE
        ):
            raise SystemExit("bundle subprocess selection drift")
        selected["oracle_program_sha256"] = hashlib.sha256(
            helper.ORACLE.encode()
        ).hexdigest()
        return selected

    selected = selections()
    for case, expected in [
        (folder.PublicationMappingPreviewTest, qualification.EXPECTED),
        (bundle.PublicationBundlePreviewTest, qualification.BUNDLE_EXPECTED),
    ]:
        if set(unittest.defaultTestLoader.getTestCaseNames(case)) != expected:
            raise SystemExit("replay case set changed")
    result = unittest.TextTestRunner(verbosity=2).run(
        unittest.TestSuite(
            [
                unittest.defaultTestLoader.loadTestsFromTestCase(
                    folder.PublicationMappingPreviewTest
                ),
                unittest.defaultTestLoader.loadTestsFromTestCase(
                    bundle.PublicationBundlePreviewTest
                ),
            ]
        )
    )
    if (
        not result.wasSuccessful()
        or result.skipped
        or result.expectedFailures
        or result.testsRun
        != len(qualification.EXPECTED) + len(qualification.BUNDLE_EXPECTED)
    ):
        raise SystemExit("Python 3.11 publication preview replay failed")
    fence()
    if selections() != selected:
        raise SystemExit("loaded replay subprocess selection drift")
    args.report.write_text(
        json.dumps(
            {
                "qualification": "local-v2-read-only-preview-python311-replay",
                "source_commit": commit,
                "source_sha256": source,
                "binary": str(binary),
                "binary_sha256": receipt["binary_sha256"],
                "build_receipt": str(args.build_receipt.absolute()),
                "build_receipt_sha256": hashlib.sha256(receipt_bytes).hexdigest(),
                "build_source_commit": receipt["source_commit"],
                "reference": qualification.PIN,
                "reference_python": str(oracle),
                "reference_python_version": version,
                "harness_sha256": harness,
                "loaded_selection": selected,
                "folder_tests": sorted(qualification.EXPECTED),
                "bundle_tests": sorted(qualification.BUNDLE_EXPECTED),
                "cases_passed": result.testsRun,
                "skipped": 0,
                "expected_failures": 0,
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
