#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Strict installed-wheel selected partial legacy adapter evidence.

Disposable paired fixtures only. Negative/nonparity witnesses stay explicit;
this is never full command-leaf, M9, filesystem-safety, or release acceptance.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch


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
    for name in (
        "wheel",
        "source-root",
        "build-manifest",
        "native-binary",
        "legacy-binary",
        "inventory",
        "reference-root",
        "reference-python",
        "report",
    ):
        parser.add_argument("--" + name, type=Path, required=True)
    return parser


def report_destinations(argv, parser):
    """Find only canonical receipt destinations, even on parser/preflight failure."""
    result = []
    for index, argument in enumerate(argv):
        if argument == "--":
            break
        if argument == "--report":
            value = argv[index + 1] if index + 1 < len(argv) else ""
            if parser._parse_optional(value) is not None:
                continue
        elif argument.startswith("--report="):
            value = argument.partition("=")[2]
        else:
            continue
        if value and Path(value) not in result:
            result.append(Path(value))
    return result


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
    snapshot = sibling("qualify_native_mutations").source_snapshot(root)
    snapshot["workflow_sha256"] = digest(
        root / ".github/workflows/native-qualification.yml"
    )
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


def preflight(args):
    if not __debug__ or sys.flags.optimize or os.environ.get("PYTHONOPTIMIZE"):
        raise RuntimeError(
            "legacy adapter qualification requires Python assertions; do not use -O or -OO"
        )
    if not sys.flags.isolated:
        raise RuntimeError("installed legacy adapter qualification requires -I")
    if sys.version_info[:2] != (3, 11) and sys.version_info[:3] != (3, 14, 6):
        raise RuntimeError(
            "installed legacy adapter runtime must be Python 3.11 or 3.14.6"
        )
    root = args.source_root.resolve(strict=True)
    if root != Path(__file__).resolve().parents[1]:
        raise RuntimeError("source-root must contain this exact qualification script")
    if Path.cwd().resolve().is_relative_to(root):
        raise RuntimeError(
            "run installed legacy adapter qualification outside the checkout"
        )
    return root


def load_fixtures(root, scenarios, binary, reference, python):
    """Load only declared stdlib-only test modules; never add checkout sys.path."""
    scenarios.validate_sources(root)
    selections = {
        "CATABOLIC_LEGACY_BINARY": str(binary),
        "CATABOLIC_PYTHON_REFERENCE": str(reference),
        "CATABOLIC_PYTHON_ORACLE": str(python),
    }
    modules = {}
    with patch.dict(os.environ, selections):
        for name, relative in (
            ("tests", "tests/__init__.py"),
            ("tests.test_rust_legacy_review", "tests/test_rust_legacy_review.py"),
            *(
                (name, name.replace(".", "/") + ".py")
                for name in scenarios.LOAD_MODULES
            ),
        ):
            if name in sys.modules:
                raise RuntimeError(f"legacy fixture imported before selection: {name}")
            spec = importlib.util.spec_from_file_location(name, root / relative)
            module = importlib.util.module_from_spec(spec)
            sys.modules[name] = module
            spec.loader.exec_module(module)
            modules[name] = module
    return modules


def loaded_selection(modules, root, binary, reference, python):
    output = {}
    helper = modules["tests.test_rust_legacy_review"]
    for name, module in modules.items():
        path = root / (
            "tests/__init__.py" if name == "tests" else name.replace(".", "/") + ".py"
        )
        if (
            sys.modules.get(name) is not module
            or Path(module.__file__).resolve() != path
        ):
            raise RuntimeError(f"legacy fixture module identity changed: {name}")
        values = {"file": str(path), "sha256": digest(path)}
        if name != "tests":
            # These modules borrow their launch methods from an exact loaded
            # module, rather than defining independent binary/oracle globals.
            delegated = {
                "tests.test_rust_publication_bundle_preview": (
                    "mapping_preview",
                    "tests.test_rust_publication_mapping_preview",
                ),
                "tests.test_rust_legacy_mapping_history": (
                    "projection",
                    "tests.test_rust_legacy_projection",
                ),
                "tests.test_rust_legacy_mapping_event_order": (
                    "history",
                    "tests.test_rust_legacy_mapping_history",
                ),
                "tests.test_rust_legacy_external_worker_diagnostics": (
                    "fixture",
                    "tests.test_rust_legacy_external_rule",
                ),
            }.get(name)
            if delegated:
                alias, owner = delegated
                if (
                    owner not in modules
                    or getattr(module, alias, None) is not modules[owner]
                ):
                    raise RuntimeError(f"loaded legacy selection owner changed: {name}")
                values["selection_owner"] = owner
            for key, expected in (
                ("BINARY", binary),
                ("PYTHON", python),
                ("REFERENCE", reference),
            ):
                if not hasattr(module, key) and not delegated:
                    raise RuntimeError(f"loaded legacy {key} missing: {name}")
                if hasattr(module, key):
                    actual = Path(os.path.abspath(getattr(module, key)))
                    if actual != expected:
                        raise RuntimeError(f"loaded legacy {key} changed: {name}")
                    values[key] = str(actual)
            if hasattr(module, "ORACLE"):
                if module.ORACLE != helper.ORACLE:
                    raise RuntimeError(f"loaded legacy ORACLE changed: {name}")
                values["oracle_program_sha256"] = hashlib.sha256(
                    module.ORACLE.encode()
                ).hexdigest()
        output[name] = values
    return output


def required_suite(modules, scenarios):
    suite = unittest.TestSuite()
    expected = {}
    for name, methods in scenarios.REQUIRED_CASES.items():
        module_name, _, attribute = name.rpartition(".")
        case = getattr(modules[module_name], attribute)
        inherited = scenarios.INHERITED.get(name)
        base = unittest.TestCase
        if inherited:
            base_module, _, base_name = inherited.rpartition(".")
            base = getattr(modules[base_module], base_name)
        if case.__bases__ != (base,):
            raise RuntimeError(f"loaded legacy inheritance changed: {name}")
        discovered = unittest.defaultTestLoader.getTestCaseNames(case)
        complete = (
            scenarios.DECLARED_CASES.get(inherited, ()) + scenarios.DECLARED_CASES[name]
        )
        if len(methods) != len(set(methods)) or sorted(discovered) != sorted(complete):
            raise RuntimeError(f"loaded legacy method inventory changed: {name}")
        if getattr(case, "__unittest_skip__", False):
            raise RuntimeError(f"legacy fixture class is skipped: {name}")
        for method in complete:
            if getattr(getattr(case, method), "__unittest_skip__", False):
                raise RuntimeError(f"legacy fixture method is skipped: {name}.{method}")
        expected[case] = methods
        if name in scenarios.OWN_ONLY:
            selected = unittest.defaultTestLoader.loadTestsFromModule(
                modules[module_name]
            )
            identifiers = list(
                sibling("native_qualification_results").scenario_ids(selected)
            )
            if sorted(identifiers) != sorted(f"{name}.{method}" for method in methods):
                raise RuntimeError(f"loaded legacy own-method routing changed: {name}")
            suite.addTests(selected)
        else:
            suite.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(case))
    return suite, expected


def oracle_environment(environment, reference, cache):
    """Preserve frozen child argv while removing cwd and user-site import roots."""
    env = dict(environment)
    if env.get("PYTHONPATH", str(reference)) != str(reference):
        raise RuntimeError("oracle subprocess selected a different frozen source")
    env.update(
        PYTHONPATH=str(reference),
        PYTHONPYCACHEPREFIX=cache,
        PYTHONDONTWRITEBYTECODE="1",
        PYTHONSAFEPATH="1",
        PYTHONNOUSERSITE="1",
    )
    env.pop("PYTHONHOME", None)
    return env


def reference_child_identity(python, reference, cache, oracle, runtime):
    """Observe the actual non-isolated child interpreter's frozen import selection.

    This probes the same explicit environment as the frozen -m worker without
    invoking its request handler or changing its argv/process supervision. The
    source is a trusted immutable qualification input, not an arbitrary sandbox.
    """
    program = """
import hashlib,json,pathlib,sys
from catabolic import app,store,http_worker
files = {name:{'path':str(pathlib.Path(module.__file__).resolve()),
              'sha256':hashlib.sha256(pathlib.Path(module.__file__).read_bytes()).hexdigest()}
         for name,module in sys.modules.items()
         if name=='catabolic' or name.startswith('catabolic.')}
print(json.dumps({'version':list(sys.version_info[:3]),'executable':sys.executable,
    'prefix':sys.prefix,'base_prefix':sys.base_prefix,
    'executable_sha256':hashlib.sha256(pathlib.Path(sys.executable).read_bytes()).hexdigest(),
    'flags':{'isolated':sys.flags.isolated,'safe_path':sys.flags.safe_path,
             'no_user_site':sys.flags.no_user_site,'optimize':sys.flags.optimize,
             'dont_write_bytecode':sys.dont_write_bytecode},'imports':files}))
"""
    # PYTHONPATH from the qualification parent's shell is not a fixture choice.
    # Select the frozen root explicitly before applying the shared child fence.
    environment = dict(os.environ, PYTHONPATH=str(reference))
    completed = subprocess.run(
        [str(python), "-c", program],
        env=oracle_environment(environment, reference, cache),
        capture_output=True,
        text=True,
        check=True,
        timeout=30,
    )
    identity = json.loads(completed.stdout)
    if {key: identity.get(key) for key in runtime} != runtime:
        raise RuntimeError("frozen child interpreter identity changed")
    if identity["flags"] != {
        "isolated": 0,
        "safe_path": True,
        "no_user_site": 1,
        "optimize": 0,
        "dont_write_bytecode": True,
    }:
        raise RuntimeError("frozen child safe import flags changed")
    root = Path(oracle["root"])
    for name, data in identity["imports"].items():
        path = Path(data["path"])
        if not path.is_relative_to(root) or data["sha256"] != oracle[
            "tracked_source_sha256"
        ].get(str(path.relative_to(root))):
            raise RuntimeError(f"frozen child imported unqualified source: {name}")
    if not {
        "catabolic",
        "catabolic.app",
        "catabolic.store",
        "catabolic.http_worker",
    } <= set(identity["imports"]):
        raise RuntimeError("frozen child did not import its required owners")
    return identity


class BoundSubprocesses:
    """Fence every fixture launch, including locally imported subprocess calls."""

    def __init__(self, binary, python, reference, manifest, cache, verify_native):
        self.binary = binary
        self.python = python
        self.reference = reference
        self.manifest = manifest
        self.manifest_sha256 = digest(manifest)
        self.cache = cache
        self.verify_native = verify_native
        self.original = subprocess.Popen
        self.current = None
        self.launches = []

    def __call__(self, arguments, *positional, **kwargs):
        if self.current is None:
            raise RuntimeError("subprocess launched outside a declared legacy scenario")
        if positional or kwargs.get("shell") or kwargs.get("executable") is not None:
            raise RuntimeError("legacy fixture launch overrides are forbidden")
        if not isinstance(arguments, (list, tuple)) or not arguments:
            raise RuntimeError("legacy fixture launch requires an exact argv")
        command = [os.fspath(value) for value in arguments]
        env = dict(os.environ if kwargs.get("env") is None else kwargs["env"])
        if command[0] == str(self.binary):
            self.verify_native()
            engine = "installed_legacy"
            # The native process must never discover Python business code.
            for name in (
                "PYTHONPATH",
                "PYTHONHOME",
                "CATABOLIC_PYTHON_REFERENCE",
                "CATABOLIC_PYTHON_ORACLE",
            ):
                env.pop(name, None)
        elif (
            command[0] == str(self.python) and len(command) >= 3 and command[1] == "-c"
        ):
            engine = "frozen_python"
            env = oracle_environment(env, self.reference, self.cache)
            command = [
                str(self.python),
                "-I",
                "-B",
                str(Path(__file__).with_name("native_legacy_adapter_oracle.py")),
                "--manifest",
                str(self.manifest),
                "--manifest-sha256",
                self.manifest_sha256,
                "--cache",
                self.cache,
                command[2],
                *command[3:],
            ]
        else:
            raise RuntimeError(
                "legacy fixture launch is neither packaged helper nor pinned Python oracle"
            )
        if digest(self.manifest) != self.manifest_sha256:
            raise RuntimeError("legacy oracle manifest changed before launch")
        kwargs["env"] = env
        self.launches.append({"scenario": self.current, "engine": engine})
        return self.original(command, **kwargs)


def paired_result_class(results, launcher, holder):
    class PairedResult(results.QualificationResult):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            holder.append(self)

        def startTest(self, test):
            super().startTest(test)
            launcher.current = test.id()
            self.start_launch = len(launcher.launches)

        def addSuccess(self, test):
            engines = {row["engine"] for row in launcher.launches[self.start_launch :]}
            if engines != {"installed_legacy", "frozen_python"}:
                try:
                    raise AssertionError(
                        "each selected legacy scenario must exercise both exact engines"
                    )
                except AssertionError:
                    self.addFailure(test, sys.exc_info())
            else:
                super().addSuccess(test)

        def stopTest(self, test):
            launcher.current = None
            super().stopTest(test)

    return PairedResult


def run_gate(args, report):
    root = preflight(args)
    with tempfile.TemporaryDirectory(prefix="catabolic-legacy-installed-") as scratch:
        cache = str(Path(scratch) / "pycache")
        sys.pycache_prefix = cache
        sys.dont_write_bytecode = True
        qualification = sibling("native_facade_qualification")
        provenance = sibling("qualify_native_embedding")
        scenarios = sibling("native_legacy_adapter_scenarios")
        results = sibling("native_qualification_results")
        before = source_fence(root)
        runtime = runtime_identity()
        reference_runtime = qualification.reference_runtime(args.reference_python)
        oracle = provenance.reference_provenance(
            args.reference_root, qualification.FROZEN_COMMIT
        )
        inventory = json.loads(args.inventory.read_text())
        qualification.inventory_rows(
            inventory,
            {"python:catabolic.cli.main": "selected partial legacy adapters"},
            1,
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
        from catabolic_native.cli import legacy_binary

        binary = Path(legacy_binary()).resolve()
        python = Path(args.reference_python).absolute()
        reference = args.reference_root.resolve() / "src"
        child_identity = reference_child_identity(
            python, reference, cache, oracle, reference_runtime
        )
        manifest = Path(scratch) / "oracle.json"
        oracle_manifest = {
            "oracle": oracle,
            "reference_interpreter": reference_runtime,
            "oracle_scripts_sha256": {
                name: digest(root / "scripts" / name)
                for name in (
                    "native_legacy_adapter_oracle.py",
                    "qualify_native_embedding.py",
                )
            },
        }
        manifest.write_text(json.dumps(oracle_manifest))
        modules = load_fixtures(root, scenarios, binary, reference, python)
        loaded = loaded_selection(modules, root, binary, reference, python)
        suite, expected = required_suite(modules, scenarios)
        imports = sibling("native_asgi_installed_tests").ImportedBytes(
            identity, oracle, {}, args.reference_root, root, before["files"]
        )

        def imported():
            if any(
                name == "catabolic" or name.startswith("catabolic.")
                for name in sys.modules
            ):
                raise RuntimeError(
                    "installed fixture process imported Python business logic"
                )
            return imports.snapshot()

        imports_before = imported()
        report.update(
            source_before=before,
            artifact_before=artifact,
            oracle=oracle,
            interpreter=runtime,
            reference_interpreter=reference_runtime,
            reference_child_identity=child_identity,
            loaded_selection=loaded,
            imported_before=imports_before,
            inventory_sha256=digest(args.inventory),
            expected_scenarios=list(scenarios.REQUIRED_IDS),
            expected_count=scenarios.REQUIRED_COUNT,
            explicit_nonparity_witnesses=list(scenarios.NONPARITY_WITNESSES),
            native_only_witnesses=list(scenarios.NATIVE_ONLY_WITNESSES),
        )
        args.report.with_suffix(".attempt.json").write_text(
            json.dumps(report, indent=2) + "\n"
        )

        def verify_native():
            identity.verify()
            if (
                Path(legacy_binary()).resolve() != binary
                or digest(binary) != artifact["build"]["legacy_binary_sha256"]
            ):
                raise RuntimeError(
                    "installed legacy helper identity changed before launch"
                )
            if loaded_selection(modules, root, binary, reference, python) != loaded:
                raise RuntimeError("loaded legacy subprocess selection changed")

        launcher = BoundSubprocesses(
            binary, python, reference, manifest, cache, verify_native
        )
        holder = []
        try:
            with (
                patch.object(subprocess, "Popen", launcher),
                patch.object(
                    results,
                    "QualificationResult",
                    paired_result_class(results, launcher, holder),
                ),
            ):
                result = results.run_required_suite(
                    suite, expected, scenarios.REQUIRED_COUNT
                )
        finally:
            report["launches"] = launcher.launches
            if holder:
                result = holder[0]
                report.update(
                    tests_run=result.testsRun,
                    successful_scenarios=result.successful_scenarios,
                    failures=len(result.failures),
                    errors=len(result.errors),
                    skips=len(result.skipped),
                    expected_failures=len(result.expectedFailures),
                    unexpected_successes=len(result.unexpectedSuccesses),
                )
            # Failures retain postflight identity evidence as well as attempts.
            identity.verify()
            artifact_after = qualification.strict_artifact(
                args.wheel,
                args.build_manifest,
                root,
                native_binary=args.native_binary,
                legacy_binary=args.legacy_binary,
            )
            report["artifact_stable"] = artifact_after == artifact
            report["source_stable"] = source_fence(root) == before
            report["reference_stable"] = (
                provenance.reference_provenance(
                    args.reference_root, qualification.FROZEN_COMMIT
                )
                == oracle
            )
            report["interpreter_stable"] = (
                runtime_identity() == runtime
                and qualification.reference_runtime(args.reference_python)
                == reference_runtime
            )
            report["reference_child_stable"] = (
                reference_child_identity(
                    python, reference, cache, oracle, reference_runtime
                )
                == child_identity
            )
            report["loaded_selection_stable"] = (
                loaded_selection(modules, root, binary, reference, python) == loaded
            )
            report["inventory_stable"] = (
                digest(args.inventory) == report["inventory_sha256"]
            )
            report["imported_after"] = imported()
            if not all(
                report[key]
                for key in (
                    "artifact_stable",
                    "source_stable",
                    "reference_stable",
                    "interpreter_stable",
                    "loaded_selection_stable",
                    "reference_child_stable",
                    "inventory_stable",
                )
            ):
                raise RuntimeError("legacy installed qualification provenance changed")
        report["passed"] = True


def main(argv=None):
    arguments = sys.argv[1:] if argv is None else argv
    parser = argument_parser()
    destinations = report_destinations(arguments, parser)
    report = {
        "format": 1,
        "passed": False,
        "development_only": False,
        "qualification": "selected-partial-legacy-adapter-evidence",
        "scope": "Disposable destination, publication-preview, mapping-history/order and external-rule/diagnostic fixtures; setup launches do not establish paired branch coverage, and explicit native-only and nonparity witnesses are not full-leaf, M9, filesystem-safety or release parity",
        "full_leaf_parity": False,
        "m9_complete": False,
        "release_qualified": False,
        "provenance_scope": "Local source-content and exact installed artifact identity, not a hermetic build claim",
    }
    started = time.monotonic()

    def save_attempts():
        report["elapsed_seconds"] = time.monotonic() - started
        for path in destinations:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.with_suffix(".attempt.json").write_text(
                json.dumps(report, indent=2) + "\n"
            )

    try:
        clear_stale_report(arguments, parser)
        save_attempts()
        # Refuse optimization before parsing can return even a misleading help success.
        if not __debug__ or sys.flags.optimize or os.environ.get("PYTHONOPTIMIZE"):
            raise RuntimeError(
                "legacy adapter qualification requires Python assertions; do not use -O or -OO"
            )
        args = parser.parse_args(arguments)
        run_gate(args, report)
    except BaseException as error:
        report.update(passed=False, error=f"{type(error).__name__}: {error}")
        save_attempts()
        print(json.dumps({"passed": False, "error": report["error"]}))
        return 1
    save_attempts()
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
