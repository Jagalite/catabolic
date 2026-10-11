#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Retain unique native scenarios, exact artifact identity and explicit outcomes.

This local report is checkpoint evidence, not a milestone or release assertion.
Run only against disposable fixtures; production effect gates remain closed.
"""

import argparse
import atexit
import hashlib
import json
import os
import platform
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

REFERENCE = "453fca983222c6665a48775eef67c489c96b527e"


def fresh_import_cache():
    """Never execute an existing oracle/test .pyc with misleading source bytes."""
    cache = tempfile.TemporaryDirectory(prefix="catabolic-qualification-pycache-")
    sys.pycache_prefix = cache.name
    os.environ["PYTHONPYCACHEPREFIX"] = cache.name
    return cache


def oracle_snapshot(reference, root):
    """Verify frozen bytes, including an archived oracle without its own Git."""
    inventory_path = root / "tests/parity/inventory.json"
    inventory = json.loads(inventory_path.read_text())["resource_sha256"]
    for relative, expected in inventory.items():
        path = reference / relative
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"missing or linked frozen oracle input: {relative}")
        if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            raise ValueError(f"frozen oracle byte drift: {relative}")
    actual = set()
    for path in (reference / "src/catabolic").rglob("*"):
        if path.is_symlink():
            raise ValueError("frozen oracle contains a symlink")
        if path.is_file() and "__pycache__" not in path.parts:
            actual.add(str(path.relative_to(reference)))
    if actual != {name for name in inventory if name.startswith("src/catabolic/")}:
        raise ValueError("frozen oracle package has unrecorded payloads")
    return {
        "root": str(reference),
        "reference_commit": REFERENCE,
        "verification": "exact frozen inventory bytes; not enclosing Git inference",
        "inventory_sha256": hashlib.sha256(inventory_path.read_bytes()).hexdigest(),
        "resource_sha256": inventory,
    }


def fixture_snapshot(root):
    directory = root / ".local-tests/rust-migration/m0-completion/legacy-fixtures"
    manifest_path = directory / "report.json"
    manifest = json.loads(manifest_path.read_text())
    if manifest["reference_commit"] != REFERENCE:
        raise ValueError("historical fixture reference differs")
    fixtures = manifest["fixtures"]
    if len(fixtures) != 32 or {item["schema"] for item in fixtures} != set(
        range(1, 33)
    ):
        raise ValueError("historical fixture inventory must contain all 32 schemas")
    hashes = {"report.json": hashlib.sha256(manifest_path.read_bytes()).hexdigest()}
    for item in fixtures:
        relative = f"schema-{item['schema']:02d}/before.sqlite3"
        if item["database"] != relative or item["status"] != "passed":
            raise ValueError("invalid historical fixture manifest entry")
        path = directory / relative
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"missing or linked historical fixture: {relative}")
        hashes[relative] = hashlib.sha256(path.read_bytes()).hexdigest()
        if hashes[relative] != item["sha256"]:
            raise ValueError(f"historical fixture byte drift: {relative}")
    return {"root": str(directory), "sha256": hashes}


def oracle_imports(reference, snapshot, root):
    imported = {}
    for name, module in sorted(sys.modules.items()):
        package = name == "catabolic" or name.startswith("catabolic.")
        fixture = name == "tests" or name.startswith("tests.")
        if not package and not fixture:
            continue
        if name.startswith(("tests.test_rust_", "tests.test_native_")):
            continue
        path = Path(module.__file__).resolve()
        if package:
            if not path.is_relative_to(reference / "src"):
                raise ValueError(f"oracle import escaped frozen source: {name}")
            relative = str(path.relative_to(reference))
        elif path.is_relative_to(root):
            relative = str(path.relative_to(root))
        elif path.is_relative_to(reference):
            relative = str(path.relative_to(reference))
        else:
            raise ValueError(f"oracle fixture import escaped verified roots: {name}")
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if snapshot["resource_sha256"].get(relative) != digest:
            raise ValueError(f"oracle import does not match frozen bytes: {name}")
        imported[name] = {"path": str(path), "sha256": digest}
    return imported


def source_snapshot(root):
    """Identify the local implementation, oracle fixtures and executed tests.

    This is a content fence, not a hermetic-build or platform qualification.
    Generated helper payloads are separately identified by their artifact hash.
    """
    generated = {
        "crates/catabolic-python/python/catabolic_native/bin/catabolic-native",
        "crates/catabolic-python/python/catabolic_native/bin/catabolic-native.tmp",
        "crates/catabolic-python/python/catabolic_native/native-helper.json",
    }
    paths = [
        root / name
        for name in (
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "pyproject.toml",
        )
    ]
    for name in (".cargo", "crates", "src", "scripts", "tests"):
        paths.extend((root / name).rglob("*"))
    files = {
        str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(set(paths))
        if path.is_file()
        and "__pycache__" not in path.parts
        and "target" not in path.parts
        and str(path.relative_to(root)) not in generated
    }
    return {
        "sha256": hashlib.sha256(
            json.dumps(files, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest(),
        "files": files,
    }


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


def validate_native_scenarios(
    identifiers,
    http_binary,
    legacy_binary=None,
    parser_binary=None,
    extension_binary=None,
):
    foreign = [name for name in identifiers if not name.startswith("tests.test_rust_")]
    if foreign:
        raise ValueError(
            f"non-native oracle tests leaked into qualification: {foreign}"
        )
    if not http_binary and any(
        name.startswith("tests.test_rust_http_")
        and not name.startswith("tests.test_rust_http_retry_fixture.")
        for name in identifiers
    ):
        raise ValueError("HTTP scenarios require an explicit --http-binary artifact")
    if not parser_binary and any(
        name.startswith("tests.test_rust_legacy_parser.") for name in identifiers
    ):
        raise ValueError(
            "parser scenarios require an explicit --parser-binary artifact"
        )
    if not legacy_binary and any(
        (
            name.startswith("tests.test_rust_legacy_")
            and not name.startswith("tests.test_rust_legacy_parser.")
        )
        or name.startswith("tests.test_rust_native_events")
        for name in identifiers
    ):
        raise ValueError(
            "legacy CLI scenarios require an explicit --legacy-binary artifact"
        )

    if not extension_binary and any(
        name.startswith("tests.test_rust_plex_login.")
        and name.endswith(".test_publication_interrupt_cleanup_cli_and_python")
        for name in identifiers
    ):
        raise ValueError(
            "PyO3 interruption scenarios require an explicit --extension-binary artifact"
        )


def extra_artifact_hashes(artifacts):
    return {
        name: {
            "path": str(path),
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        }
        for name, path in artifacts.items()
    }


def main():
    if not __debug__:
        raise RuntimeError("native qualification requires unoptimized Python tests")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--http-binary", type=Path)
    parser.add_argument("--legacy-binary", type=Path)
    parser.add_argument("--parser-binary", type=Path)
    parser.add_argument("--extension-binary", type=Path)
    parser.add_argument("--reference-root", type=Path)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--pattern", default="test_rust_*.py")
    parser.add_argument("--expected-count", type=int)
    args = parser.parse_args()
    cache = fresh_import_cache()
    atexit.register(cache.cleanup)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(
        json.dumps({"qualification_status": "started", "passed_without_skips": False})
        + "\n"
    )
    root = Path(__file__).resolve().parents[1]
    os.chdir(root)
    reference = (
        args.reference_root or root / ".local-tests/rust-migration/reference"
    ).resolve(strict=True)
    oracle_before = oracle_snapshot(reference, root)
    fixtures_before = fixture_snapshot(root)
    os.environ["CATABOLIC_PYTHON_REFERENCE"] = str(reference / "src")
    os.environ["PYTHONPATH"] = str(reference / "src") + os.pathsep + str(root)
    binary = args.binary.resolve(strict=True)
    binary_before = hashlib.sha256(binary.read_bytes()).hexdigest()
    http_binary = args.http_binary.resolve(strict=True) if args.http_binary else None
    http_before = (
        hashlib.sha256(http_binary.read_bytes()).hexdigest() if http_binary else None
    )
    extra_artifacts = {}
    for name, option, environment in (
        ("legacy_cli", args.legacy_binary, "CATABOLIC_LEGACY_BINARY"),
        ("legacy_parser", args.parser_binary, "CATABOLIC_NATIVE_PARSER_BINARY"),
        ("native_extension", args.extension_binary, "CATABOLIC_NATIVE_EXTENSION"),
    ):
        os.environ.pop(environment, None)
        if option is not None:
            extra_artifacts[name] = option.resolve(strict=True)
            os.environ[environment] = str(extra_artifacts[name])
    extra_before = extra_artifact_hashes(extra_artifacts)
    sources_before = source_snapshot(root)
    os.environ["CATABOLIC_NATIVE_BINARY"] = str(binary)
    if http_binary:
        os.environ["CATABOLIC_HTTP_BINARY"] = str(http_binary)
    sys.path.insert(0, str(root))
    sys.path.insert(0, str(reference / "src"))
    suite = unittest.defaultTestLoader.discover(
        "tests", args.pattern, top_level_dir="."
    )
    identifiers = list(cases(suite))
    if not identifiers:
        parser.error(
            "no scenarios match the requested pattern; no qualification was performed"
        )
    try:
        validate_native_scenarios(
            identifiers,
            http_binary,
            extra_artifacts.get("legacy_cli"),
            extra_artifacts.get("legacy_parser"),
            extra_artifacts.get("native_extension"),
        )
    except ValueError as error:
        parser.error(str(error))
    if args.expected_count is not None and len(identifiers) != args.expected_count:
        parser.error(
            f"expected {args.expected_count} scenarios, discovered {len(identifiers)}"
        )
    if len(identifiers) != len(set(identifiers)):
        raise AssertionError(
            "duplicate discovered test IDs cannot inflate qualification"
        )
    start = time.monotonic()
    imports_before = oracle_imports(reference, oracle_before, root)
    result = unittest.TextTestRunner(verbosity=2, resultclass=Results).run(suite)
    oracle_after = oracle_snapshot(reference, root)
    fixtures_after = fixture_snapshot(root)
    imports_after = oracle_imports(reference, oracle_after, root)
    sources_after = source_snapshot(root)
    binary_after = hashlib.sha256(binary.read_bytes()).hexdigest()
    http_after = (
        hashlib.sha256(http_binary.read_bytes()).hexdigest() if http_binary else None
    )
    extra_after = extra_artifact_hashes(extra_artifacts)
    source_unchanged = (
        sources_before == sources_after
        and oracle_before == oracle_after
        and fixtures_before == fixtures_after
    )
    artifact_unchanged = (
        binary_before == binary_after
        and http_before == http_after
        and extra_before == extra_after
    )
    complete = (
        result.testsRun == len(identifiers)
        and {item["id"] for item in result.scenarios} == set(identifiers)
        and len(result.scenarios) == len(identifiers)
    )
    value = {
        "qualification_status": "completed",
        "scope": "Opt-in native companion checkpoint; not complete milestone qualification",
        "platform": platform.platform(),
        "python": sys.version,
        "binary": str(binary),
        "binary_sha256": binary_before,
        "binary_sha256_after": binary_after,
        "http_binary": str(http_binary) if http_binary else None,
        "http_binary_sha256": http_before,
        "http_binary_sha256_after": http_after,
        "extra_artifacts_before": extra_before,
        "extra_artifacts_after": extra_after,
        "artifact_unchanged_during_qualification": artifact_unchanged,
        "source_content_before": sources_before,
        "source_content_sha256_after": sources_after["sha256"],
        "source_content_unchanged_during_qualification": source_unchanged,
        "frozen_oracle": oracle_before,
        "historical_fixtures": fixtures_before,
        "oracle_imports_before": imports_before,
        "oracle_imports_after": imports_after,
        "oracle_and_fixtures_unchanged": oracle_before == oracle_after
        and fixtures_before == fixtures_after,
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
        and source_unchanged
        and artifact_unchanged
        and result.wasSuccessful()
        and all(item["outcome"] == "passed" for item in result.scenarios),
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(value, indent=2) + "\n")
    return 0 if value["passed_without_skips"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
