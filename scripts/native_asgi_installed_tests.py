#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Strict installed disposable-ASGI fixture gate, never production HTTP acceptance.

Full: exactly 15 adapter and 6 smoke scenarios on Python 3.14.6, with pinned
frozen Python source and exact dependency wheels. Minimal: the same 6 smoke
scenarios on Python 3.11 without optional frameworks. No debug/development mode.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import sys
import sysconfig
import tempfile
import unittest
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
    parser.add_argument("--lane", choices=("full", "minimal"), required=True)
    parser.add_argument("--wheel", type=Path, required=True)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--build-manifest", type=Path, required=True)
    parser.add_argument("--native-binary", type=Path, required=True)
    parser.add_argument("--legacy-binary", type=Path, required=True)
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--reference-python", type=Path, required=True)
    parser.add_argument("--dependency-wheelhouse", type=Path)
    parser.add_argument("--report", type=Path, required=True)
    return parser


if __name__ == "__main__":
    clear_stale_report(parser=argument_parser())

if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
    raise RuntimeError(
        "ASGI qualification requires Python assertions; do not use -O or -OO"
    )


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def source_fence(root):
    # Includes all crate, Python facade and fixture sources; the remaining
    # routing/lock inputs are explicit rather than hidden in a build receipt.
    snapshot = sibling("qualify_native_mutations").source_snapshot(root)
    snapshot["routing_sha256"] = {
        name: digest(root / name)
        for name in (
            ".github/workflows/native-qualification.yml",
            "requirements/runtime.lock",
            "requirements/http-test.lock",
        )
    }
    return snapshot


def runtime_identity():
    return {
        "version": list(sys.version_info[:3]),
        "executable": str(Path(sys.executable).absolute()),
        "executable_sha256": digest(sys.executable),
        "prefix": sys.prefix,
        "base_prefix": sys.base_prefix,
        "isolated": sys.flags.isolated,
        "optimize": sys.flags.optimize,
    }


def lane_preflight(lane, wheelhouse):
    if not sys.flags.isolated:
        raise RuntimeError("installed ASGI qualification requires -I")
    if lane == "full":
        if list(sys.version_info[:3]) != [3, 14, 6]:
            raise RuntimeError("full paired ASGI fixtures require Python 3.14.6")
        if wheelhouse is None:
            raise RuntimeError(
                "full paired ASGI fixtures require exact dependency wheels"
            )
    elif lane == "minimal":
        if sys.version_info[:2] != (3, 11):
            raise RuntimeError("minimal ASGI fixtures require Python 3.11")
        if wheelhouse is not None:
            raise RuntimeError(
                "minimal ASGI lane must not install optional dependencies"
            )
        for name in (
            "httpx",
            "httpcore",
            "fastapi",
            "starlette",
            "anyio",
            "pydantic",
            "graphql",
            "regex",
            "uvicorn",
        ):
            if importlib.util.find_spec(name) is not None:
                raise RuntimeError(
                    f"minimal ASGI lane contains optional dependency: {name}"
                )
    else:
        raise RuntimeError("unknown ASGI qualification lane")


class ImportedBytes:
    """Pin loaded package objects and compare all imported files to declared bytes."""

    def __init__(
        self,
        native,
        oracle,
        dependencies,
        reference_root,
        source_root=None,
        source_files=None,
    ):
        self.roots = {
            "catabolic_native": (Path(native.package).parent.parent, native.payloads),
            "catabolic": (reference_root.resolve(), oracle["tracked_source_sha256"]),
        }
        self.dependencies = dependencies
        self.source_root = source_root
        self.source_files = source_files or {}
        self.stdlib = {
            Path(sysconfig.get_path(name)).resolve()
            for name in ("stdlib", "platstdlib")
        }
        self.setup = {}
        setup = sys.modules.get("_distutils_hack")
        if setup is not None and not dependencies:
            path = Path(setup.__file__).resolve()
            if (
                not path.is_relative_to(Path(sys.prefix).resolve())
                or path.parent.name != "_distutils_hack"
            ):
                raise RuntimeError("unqualified interpreter setup hook")
            self.setup["_distutils_hack"] = (str(path), digest(path))
        self.identities = {}

    def snapshot(self):
        output = {}
        for name, module in sorted(sys.modules.items()):
            top = name.partition(".")[0]
            filename = getattr(module, "__file__", None)
            if not filename:
                continue
            path = Path(filename).resolve()
            expected = None
            if top in self.roots:
                root, hashes = self.roots[top]
                if not path.is_relative_to(root):
                    raise RuntimeError(f"import escaped verified package root: {name}")
                expected = hashes.get(str(path.relative_to(root)))
                if expected is None:
                    raise RuntimeError(f"unrecorded imported package bytes: {name}")
            else:
                for dependency in self.dependencies.values():
                    root = Path(dependency["root"])
                    if path.is_relative_to(root):
                        expected = dependency["payloads_sha256"].get(
                            str(path.relative_to(root))
                        )
                        if expected is not None:
                            break
                if expected is None and name in self.setup:
                    setup_path, expected = self.setup[name]
                    if str(path) != setup_path:
                        raise RuntimeError("interpreter setup hook path changed")
                if (
                    expected is None
                    and self.source_root is not None
                    and path.is_relative_to(self.source_root)
                ):
                    expected = self.source_files.get(
                        str(path.relative_to(self.source_root))
                    )
                    if expected is None:
                        raise RuntimeError(
                            f"unrecorded qualification source import: {name}"
                        )
                if expected is None:
                    third_party = any(
                        part in {"site-packages", "dist-packages"}
                        for part in path.parts
                    )
                    if not third_party and any(
                        path.is_relative_to(root) for root in self.stdlib
                    ):
                        continue
                    raise RuntimeError(f"unqualified import: {name}: {path}")
            if digest(path) != expected:
                raise RuntimeError(f"imported bytes changed: {name}")
            previous = self.identities.setdefault(name, (module, str(path)))
            if previous[0] is not module or previous[1] != str(path):
                raise RuntimeError(f"imported module identity changed: {name}")
            output[name] = {"path": str(path), "sha256": expected}
        if any(
            sys.modules.get(name) is not identity[0]
            for name, identity in self.identities.items()
        ):
            raise RuntimeError("a verified imported module was removed or replaced")
        return output


def required_suite(root, lane, scenarios):
    scenarios.validate_sources(root)
    selected = (
        scenarios.REQUIRED_CASES
        if lane == "full"
        else {
            name: methods
            for name, methods in scenarios.REQUIRED_CASES.items()
            if name == "tests.test_rust_asgi_smoke.InstalledASGISmoke"
        }
    )
    suite = unittest.TestSuite()
    expected = {}
    for class_name, methods in selected.items():
        module_name, _, attribute = class_name.rpartition(".")
        # Load only declared fixture files, with stable IDs, rather than adding
        # the checkout to sys.path or importing arbitrary source packages.
        spec = importlib.util.spec_from_file_location(
            module_name, root / (module_name.replace(".", "/") + ".py")
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        case = getattr(module, attribute)
        expected[case] = methods
        suite.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(case))
    return suite, expected


def run_gate(args, report):
    root = args.source_root.resolve(strict=True)
    if root != Path(__file__).resolve().parents[1]:
        raise RuntimeError("source-root must contain this exact qualification script")
    if Path.cwd().resolve().is_relative_to(root):
        raise RuntimeError("run installed ASGI qualification outside the checkout")
    lane_preflight(args.lane, args.dependency_wheelhouse)
    # A fresh cache prefix prevents stale .pyc files from overriding declared
    # source bytes, including installed native wrappers and the frozen oracle.
    with tempfile.TemporaryDirectory(prefix="catabolic-asgi-pycache-") as cache:
        sys.pycache_prefix = cache
        sys.dont_write_bytecode = True
        qualification = sibling("native_facade_qualification")
        provenance = sibling("qualify_native_embedding")
        dependencies = sibling("native_asgi_dependencies")
        scenarios = sibling("native_asgi_scenarios")
        results = sibling("native_qualification_results")
        before = source_fence(root)
        runtime = runtime_identity()
        reference_runtime = qualification.reference_runtime(args.reference_python)
        oracle = provenance.reference_provenance(
            args.reference_root, qualification.FROZEN_COMMIT
        )
        identity = qualification.NativeIdentity(root)
        artifact = qualification.strict_artifact(
            args.wheel,
            args.build_manifest,
            root,
            native_binary=args.native_binary,
            legacy_binary=args.legacy_binary,
        )
        identity.payloads = artifact["installed_wheel"]["installed_payloads_sha256"]
        dependency_before = (
            dependencies.dependency_snapshot(root, args.dependency_wheelhouse)
            if args.lane == "full"
            else {}
        )
        imports = ImportedBytes(
            identity,
            oracle,
            dependency_before,
            args.reference_root,
            root,
            before["files"],
        )
        imports_before = imports.snapshot()
        scenarios.validate_sources(root)
        report["expected_scenarios"] = list(
            scenarios.REQUIRED_IDS if args.lane == "full" else scenarios.SMOKE_IDS
        )
        report.update(
            source_before=before,
            artifact_before=artifact,
            oracle=oracle,
            interpreter=runtime,
            reference_interpreter=reference_runtime,
            dependencies_before=dependency_before,
            imported_before=imports_before,
        )
        args.report.with_suffix(".attempt.json").write_text(
            json.dumps(report, indent=2) + "\n"
        )
        # Explicit, provenance-checked frozen source is the only added import root.
        if args.lane == "full":
            sys.path.insert(0, str(args.reference_root.resolve() / "src"))
            os.environ["CATABOLIC_PYTHON_REFERENCE"] = str(
                args.reference_root.resolve() / "src"
            )
        suite, expected = required_suite(root, args.lane, scenarios)
        identifiers = list(results.scenario_ids(suite))
        required = (
            scenarios.REQUIRED_IDS if args.lane == "full" else scenarios.SMOKE_IDS
        )
        count = 21 if args.lane == "full" else 6
        if (
            len(required) != count
            or len(set(required)) != count
            or set(identifiers) != set(required)
        ):
            raise RuntimeError("ASGI mandatory scenario IDs differ")
        report["expected_scenarios"] = list(required)
        imports.snapshot()
        result = results.run_required_suite(suite, expected, count)
        identity.verify()
        artifact_after = qualification.strict_artifact(
            args.wheel,
            args.build_manifest,
            root,
            native_binary=args.native_binary,
            legacy_binary=args.legacy_binary,
        )
        dependencies_after = (
            dependencies.dependency_snapshot(root, args.dependency_wheelhouse)
            if args.lane == "full"
            else {}
        )
        imports_after = imports.snapshot()
        source_after = source_fence(root)
        if before != source_after:
            raise RuntimeError("ASGI qualification source changed during execution")
        if artifact != artifact_after:
            raise RuntimeError("ASGI installed artifact changed during execution")
        if dependency_before != dependencies_after:
            raise RuntimeError(
                "ASGI dependency wheel or installed bytes changed during execution"
            )
        if (
            runtime != runtime_identity()
            or reference_runtime
            != qualification.reference_runtime(args.reference_python)
        ):
            raise RuntimeError("ASGI interpreter changed during execution")
        if oracle != provenance.reference_provenance(
            args.reference_root, qualification.FROZEN_COMMIT
        ):
            raise RuntimeError("ASGI frozen reference changed during execution")
        report.update(
            passed=True,
            tests_run=result.testsRun,
            successful_scenarios=result.successful_scenarios,
            imported_after=imports_after,
            source_stable=True,
            artifact_stable=True,
            dependencies_stable=True,
            reference_stable=True,
        )


def main():
    clear_stale_report(parser=argument_parser())
    args = argument_parser().parse_args()
    args.report.parent.mkdir(parents=True, exist_ok=True)
    report = {
        "format": 1,
        "passed": False,
        "development_only": False,
        "lane": args.lane,
        "scope": "Disposable fixture ASGI transport only; production create_app and route-owner acceptance remain open",
        "expected_count": 21 if args.lane == "full" else 6,
        "paired_python311_qualified": False,
        "provenance_scope": "Local source-content and artifact identity, not a hermetic build claim",
    }
    attempt = args.report.with_suffix(".attempt.json")
    attempt.write_text(json.dumps(report, indent=2) + "\n")
    try:
        run_gate(args, report)
    except Exception as error:
        report.update(passed=False, error=f"{type(error).__name__}: {error}")
        attempt.write_text(json.dumps(report, indent=2) + "\n")
        print(
            json.dumps(
                {"passed": False, "error": report["error"], "attempt": str(attempt)}
            )
        )
        return 1
    attempt.write_text(json.dumps(report, indent=2) + "\n")
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            {
                "passed": True,
                "tests_run": report["tests_run"],
                "report": str(args.report),
            }
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
