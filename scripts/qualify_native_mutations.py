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
from fnmatch import fnmatch
from pathlib import Path


def clear_stale_report(argv=None, parser=None):
    """Invalidate every canonical --report destination before fallible preflight.

    Only --report PATH and --report=PATH opt in; abbreviations are rejected by
    the full parser. Stop at -- and use that parser's option lexer to distinguish
    filename values (including version-specific negative numbers) from options.
    No argv parsing or project imports happen here. Bootstrap copies are checked
    against this implementation by the receipt-boundary tests.
    """
    if parser is None:
        parser = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
        parser.add_argument("--report")
    arguments = sys.argv[1:] if argv is None else argv
    first_error = None
    for index, argument in enumerate(arguments):
        if argument == "--":
            break
        if argument == "--report":
            value = arguments[index + 1] if index + 1 < len(arguments) else ""
            if parser._parse_optional(value) is not None:
                continue
        elif argument.startswith("--report="):
            value = argument.partition("=")[2]
        else:
            continue
        if value:
            report = Path(value)
            for suffix in (None, ".attempt.json"):
                try:
                    target = report if suffix is None else report.with_suffix(suffix)
                    target.unlink(missing_ok=True)
                except (OSError, ValueError) as error:
                    if first_error is None:
                        first_error = error
    if first_error is not None:
        raise first_error


def argument_parser():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--http-binary", type=Path)
    parser.add_argument("--legacy-binary", type=Path)
    parser.add_argument("--parser-binary", type=Path)
    parser.add_argument("--extension-binary", type=Path)
    parser.add_argument("--reference-root", type=Path)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--pattern", default="test_rust_*.py")
    parser.add_argument("--expected-count", type=int)
    parser.add_argument(
        "--defer-installed-asgi",
        action="store_true",
        help="route the exact installed ASGI prerequisite to its separate gate; never count it as passed",
    )
    return parser


REFERENCE = "453fca983222c6665a48775eef67c489c96b527e"


def fresh_import_cache():
    """Never execute an existing oracle/test .pyc with misleading source bytes."""
    cache = tempfile.TemporaryDirectory(prefix="catabolic-qualification-pycache-")
    sys.pycache_prefix = cache.name
    os.environ["PYTHONPYCACHEPREFIX"] = cache.name
    return cache


def verified_file(root, relative):
    """Require an exact relative regular file, with no linked path components."""
    relative_path = Path(relative)
    if (
        relative_path.is_absolute()
        or relative_path.as_posix() != relative
        or ".." in relative_path.parts
    ):
        raise ValueError(f"invalid provenance path: {relative}")
    path = root / relative_path
    if any(part.is_symlink() for part in (path, *path.parents)):
        raise ValueError(f"linked provenance input: {relative}")
    if not path.is_file():
        raise ValueError(f"missing provenance input: {relative}")
    return path


def oracle_snapshot(reference, root):
    """Verify frozen bytes, including an archived oracle without its own Git."""
    inventory_path = verified_file(root, "tests/parity/inventory.json")
    inventory = json.loads(inventory_path.read_text())["resource_sha256"]
    for relative, expected in inventory.items():
        path = verified_file(reference, relative)
        if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            raise ValueError(f"frozen oracle byte drift: {relative}")
    for directory in ("src/catabolic", "tests"):
        actual = set()
        for path in (reference / directory).rglob("*"):
            if path.is_symlink():
                raise ValueError("frozen oracle contains a symlink")
            if path.is_file() and "__pycache__" not in path.parts:
                actual.add(str(path.relative_to(reference)))
        if actual != {name for name in inventory if name.startswith(directory + "/")}:
            raise ValueError(f"frozen oracle {directory} has unrecorded payloads")
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


def adaptation_snapshot(root, oracle, sources):
    """Admit individually reviewed fixture changes, never as frozen oracle bytes."""
    relative = "tests/parity/native-fixture-adaptations.json"
    manifest_path = verified_file(root, relative)
    digest = hashlib.sha256(manifest_path.read_bytes()).hexdigest()
    if sources["files"].get(relative) != digest:
        raise ValueError("fixture adaptation manifest changed since source snapshot")
    manifest = json.loads(manifest_path.read_text())
    if set(manifest) != {"schema", "reference_commit", "modules"} or (
        manifest["schema"] != 1 or manifest["reference_commit"] != REFERENCE
    ):
        raise ValueError("invalid fixture adaptation manifest")
    modules = manifest["modules"]
    if not isinstance(modules, dict) or not modules:
        raise ValueError("empty or invalid fixture adaptation manifest")
    for name, entry in modules.items():
        if set(entry) != {
            "path",
            "kind",
            "frozen_sha256",
            "sha256",
            "reviewed_commit",
            "reason",
        }:
            raise ValueError(f"invalid fixture adaptation entry: {name}")
        if (
            not name.startswith("tests.")
            or name.startswith(("tests.test_rust_", "tests.test_native_"))
            or not all(part.isidentifier() for part in name.split("."))
            or entry["path"] != name.replace(".", "/") + ".py"
            or entry["kind"] not in ("fixture-adaptation", "fixture-helper")
            or not entry["reason"]
            or len(entry["reviewed_commit"]) != 40
            or any(char not in "0123456789abcdef" for char in entry["reviewed_commit"])
        ):
            raise ValueError(f"invalid fixture adaptation identity: {name}")
        baseline = oracle["resource_sha256"].get(entry["path"])
        if baseline != entry["frozen_sha256"] or (
            (baseline is None) != (entry["kind"] == "fixture-helper")
        ):
            raise ValueError(f"fixture adaptation frozen baseline differs: {name}")
        path = verified_file(root, entry["path"])
        current = hashlib.sha256(path.read_bytes()).hexdigest()
        if current != entry["sha256"] or sources["files"].get(entry["path"]) != current:
            raise ValueError(f"reviewed fixture byte drift: {name}")
        if current == baseline:
            raise ValueError(f"unchanged fixture must use frozen provenance: {name}")
    return {"path": str(manifest_path), "sha256": digest, **manifest}


def oracle_imports(reference, snapshot, root, sources, adaptations):
    imported = {}
    for name, module in sorted(sys.modules.items()):
        package = name == "catabolic" or name.startswith("catabolic.")
        fixture = name == "tests" or name.startswith("tests.")
        if not package and not fixture:
            continue
        filename = getattr(module, "__file__", None)
        if not filename:
            raise ValueError(f"import lacks a verifiable source file: {name}")
        path = Path(filename).absolute()
        module_path = name.replace(".", "/")
        candidates = {module_path + ".py", module_path + "/__init__.py"}
        if package:
            if not path.is_relative_to(reference / "src"):
                raise ValueError(f"oracle import escaped frozen source: {name}")
            origin = reference
            relative = str(path.relative_to(origin))
            if relative.removeprefix("src/") not in candidates:
                raise ValueError(f"oracle import identity differs: {name}")
            provenance = "frozen-application"
        else:
            if path.is_relative_to(root / "tests"):
                origin = root
            elif path.is_relative_to(reference / "tests"):
                origin = reference
            else:
                raise ValueError(
                    f"oracle fixture import escaped verified roots: {name}"
                )
            relative = str(path.relative_to(origin))
            if relative not in candidates:
                raise ValueError(f"fixture import identity differs: {name}")
            native = name.startswith(("tests.test_rust_", "tests.test_native_"))
            adaptation = adaptations["modules"].get(name)
            if native or adaptation:
                if origin != root:
                    raise ValueError(
                        f"local qualification fixture escaped source: {name}"
                    )
                provenance = (
                    "native-qualification-test"
                    if native
                    else "reviewed-" + adaptation["kind"]
                )
            else:
                provenance = "frozen-fixture"
        path = verified_file(origin, relative)
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if provenance.startswith("frozen-"):
            if snapshot["resource_sha256"].get(relative) != digest:
                raise ValueError(f"oracle import does not match frozen bytes: {name}")
        else:
            if sources["files"].get(relative) != digest:
                raise ValueError(
                    f"local qualification import changed since snapshot: {name}"
                )
            if adaptation and adaptation["sha256"] != digest:
                raise ValueError(f"reviewed fixture byte drift: {name}")
        imported[name] = {"path": str(path), "sha256": digest, "provenance": provenance}
    return imported


def npm_bin_link_snapshot(root, relative):
    """Bind only canonical npm aliases declared by the client dependency lockfile."""
    directory = "tests/clients/node_modules/.bin"
    link = root / relative
    if link.parent != root / directory or not link.is_symlink():
        raise ValueError(f"unrecognized dependency link: {relative}")
    if any(parent.is_symlink() for parent in link.parents):
        raise ValueError(f"linked dependency alias parent: {relative}")
    target = os.readlink(link)
    parts = Path(target).parts
    if (
        not parts
        or parts[0] != ".."
        or Path(target).as_posix() != target
        or any(part in (".", "..") for part in parts[1:])
    ):
        raise ValueError(f"noncanonical dependency link: {relative}")
    package_parts = 2 if len(parts) > 1 and parts[1].startswith("@") else 1
    if len(parts) <= package_parts + 1:
        raise ValueError(f"dependency link lacks an in-package file: {relative}")
    package = "/".join(parts[1 : package_parts + 1])
    lock_relative = "tests/clients/package-lock.json"
    lock_path = verified_file(root, lock_relative)
    lock_bytes = lock_path.read_bytes()
    lock = json.loads(lock_bytes)
    if lock.get("lockfileVersion") != 3 or not isinstance(lock.get("packages"), dict):
        raise ValueError("unsupported client dependency lockfile")
    package_key = "node_modules/" + package
    entry = lock["packages"].get(package_key, {})
    bins = entry.get("bin", {})
    declared = bins.get(link.name) if isinstance(bins, dict) else None
    if (
        not isinstance(declared, str)
        or not declared
        or Path(declared).is_absolute()
        or Path(declared).as_posix() != declared
        or any(part in (".", "..") for part in Path(declared).parts)
        or target != "../" + package + "/" + declared
        or not isinstance(entry.get("version"), str)
        or not entry["version"]
    ):
        raise ValueError(f"dependency link is not its locked bin alias: {relative}")
    resolved_relative = "tests/clients/" + package_key + "/" + declared
    resolved = verified_file(root, resolved_relative)
    return {
        "target": target,
        "resolved_path": resolved_relative,
        "target_sha256": hashlib.sha256(resolved.read_bytes()).hexdigest(),
        "lockfile": lock_relative,
        "lockfile_sha256": hashlib.sha256(lock_bytes).hexdigest(),
        "package": package_key,
        "package_version": entry["version"],
    }


def source_snapshot(root):
    """Identify the local implementation, oracle fixtures and executed tests.

    This is a content fence, not a hermetic-build or platform qualification.
    Generated helper payloads are separately identified by their artifact hash.
    """
    generated = {
        "crates/catabolic-python/python/catabolic_native/bin/catabolic-native",
        "crates/catabolic-python/python/catabolic_native/bin/catabolic-native.tmp",
        "crates/catabolic-python/python/catabolic_native/native-helper.json",
        "crates/catabolic-python/python/catabolic_native/bin/catabolic-legacy-native",
        "crates/catabolic-python/python/catabolic_native/bin/catabolic-legacy-native.tmp",
        "crates/catabolic-python/python/catabolic_native/legacy-helper.json",
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
        paths.append(root / name)
        paths.extend((root / name).rglob("*"))
    dependency_links = {}
    for path in paths:
        if path.is_symlink():
            relative = str(path.relative_to(root))
            if path.parent != root / "tests/clients/node_modules/.bin":
                raise ValueError(f"qualified source contains a symlink: {relative}")
            dependency_links[relative] = npm_bin_link_snapshot(root, relative)
    files = {
        str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(set(paths))
        if path.is_file()
        and "__pycache__" not in path.parts
        and "target" not in path.parts
        and str(path.relative_to(root)) not in generated
    }
    # Keep every historical file/target-byte entry. Schema 2 additionally binds
    # npm alias identity; dependency trees are never silently omitted.
    payload = {"schema": 2, "files": files, "dependency_links": dependency_links}
    return {
        **payload,
        "sha256": hashlib.sha256(
            json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest(),
        "files_sha256": hashlib.sha256(
            json.dumps(files, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest(),
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


def discover_native_suite(root, pattern, defer_installed_asgi=False):
    if not defer_installed_asgi:
        from scripts.native_asgi_scenarios import REQUIRED_FILES, validate_sources

        expected_installed = any(
            fnmatch(Path(name).name, pattern) for name in REQUIRED_FILES
        )
        matching_installed = [
            path
            for path in (root / "tests").rglob("test_rust_asgi*.py")
            if fnmatch(path.name, pattern)
        ]
        if expected_installed or matching_installed:
            validate_sources(root)
            raise ValueError(
                "installed ASGI scenarios require the strict scripts/native_asgi_installed_tests.py "
                "gate or explicit --defer-installed-asgi; aggregate discovery cannot import them"
            )
        return (
            unittest.defaultTestLoader.discover("tests", pattern, top_level_dir="."),
            [],
        )
    from scripts.native_asgi_scenarios import InstalledASGIDeferredLoader

    loader = InstalledASGIDeferredLoader(root)
    suite = loader.discover("tests", pattern, top_level_dir=".")
    prerequisite = loader.verify_routing(list(cases(suite)))
    return suite, [prerequisite]


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
    clear_stale_report(parser=argument_parser())
    if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
        raise RuntimeError("native qualification requires unoptimized Python tests")
    parser = argument_parser()
    args = parser.parse_args()
    cache = fresh_import_cache()
    atexit.register(cache.cleanup)
    args.report = args.report.absolute()
    args.report.parent.mkdir(parents=True, exist_ok=True)
    initial_report = {
        "qualification_status": "started",
        "phase": "preflight",
        "tests_run": 0,
        "passed_without_skips": False,
        "selected_aggregate_passed_without_skips": False,
        "installed_prerequisites": [
            {
                "name": "installed_asgi",
                "mandatory": True,
                "status": "pending",
                "manifest_verified": False,
            }
        ]
        if args.defer_installed_asgi
        else [],
    }
    args.report.write_text(json.dumps(initial_report) + "\n")
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
    adaptations_before = adaptation_snapshot(root, oracle_before, sources_before)
    os.environ["CATABOLIC_NATIVE_BINARY"] = str(binary)
    if http_binary:
        os.environ["CATABOLIC_HTTP_BINARY"] = str(http_binary)
    sys.path.insert(0, str(root))
    sys.path.insert(0, str(reference / "src"))
    try:
        suite, installed_prerequisites = discover_native_suite(
            root, args.pattern, args.defer_installed_asgi
        )
    except ValueError as error:
        parser.error(str(error))
    initial_report["installed_prerequisites"] = installed_prerequisites
    args.report.write_text(json.dumps(initial_report, indent=2) + "\n")
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
    imports_before = oracle_imports(
        reference, oracle_before, root, sources_before, adaptations_before
    )
    initial_report["phase"] = "running"
    args.report.write_text(json.dumps(initial_report, indent=2) + "\n")
    result = unittest.TextTestRunner(verbosity=2, resultclass=Results).run(suite)
    oracle_after = oracle_snapshot(reference, root)
    fixtures_after = fixture_snapshot(root)
    sources_after = source_snapshot(root)
    adaptations_after = adaptation_snapshot(root, oracle_after, sources_after)
    imports_after = oracle_imports(
        reference, oracle_after, root, sources_before, adaptations_before
    )
    binary_after = hashlib.sha256(binary.read_bytes()).hexdigest()
    http_after = (
        hashlib.sha256(http_binary.read_bytes()).hexdigest() if http_binary else None
    )
    extra_after = extra_artifact_hashes(extra_artifacts)
    source_unchanged = (
        sources_before == sources_after
        and oracle_before == oracle_after
        and fixtures_before == fixtures_after
        and adaptations_before == adaptations_after
        and all(
            imports_after.get(name) == value for name, value in imports_before.items()
        )
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
    selected_passed = (
        complete
        and source_unchanged
        and artifact_unchanged
        and result.wasSuccessful()
        and all(item["outcome"] == "passed" for item in result.scenarios)
    )
    value = {
        "qualification_status": (
            "selected_aggregate_passed_installed_prerequisites_pending"
            if selected_passed
            else "selected_aggregate_failed_installed_prerequisites_pending"
        )
        if installed_prerequisites
        else "completed",
        "scope": (
            "Selected native aggregate only; mandatory installed ASGI prerequisites remain pending; "
            "not complete milestone qualification"
            if installed_prerequisites
            else "Opt-in native companion checkpoint; not complete milestone qualification"
        ),
        "installed_prerequisites": installed_prerequisites,
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
        "import_provenance_before": imports_before,
        "import_provenance_after": imports_after,
        "reviewed_fixture_adaptations": adaptations_before,
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
        "selected_aggregate_passed_without_skips": selected_passed,
        "passed_without_skips": selected_passed and not installed_prerequisites,
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(value, indent=2) + "\n")
    return 0 if selected_passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
