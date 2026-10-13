#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Strict installed-artifact gate for the one-row private credential adapter."""

import argparse
import functools
import importlib.util
import inspect
import json
import os
import subprocess
import sys
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
    parser.add_argument("--scenario", choices=("reference", "native"))
    parser.add_argument("--root", type=Path)
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--reference-python", type=Path)
    parser.add_argument("--inventory", type=Path)
    parser.add_argument("--source-root", type=Path)
    parser.add_argument("--wheel", type=Path)
    parser.add_argument("--build-manifest", type=Path)
    parser.add_argument("--native-binary", type=Path)
    parser.add_argument("--legacy-binary", type=Path)
    parser.add_argument("--report", type=Path)
    return parser


if __name__ == "__main__":
    clear_stale_report(parser=argument_parser())
if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
    raise RuntimeError(
        "private secret qualification requires Python assertions; do not use -O or -OO"
    )


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CONTRACTS = {
    "python:catabolic.api_cli.save_secret": "Exclusive output, JSON transport, same-owner registration and failure effects"
}
EXPECTED_GROUPS = (
    "signature",
    "ordinary",
    "unicode_order",
    "json_types",
    "relative_path",
    "home_path",
    "existing_file",
    "missing_parent",
    "serialization_error",
    "callback_identity",
    "missing_summary",
    "nested_rollback",
    "outer_rollback",
    "registration_error",
    "registration_reuse",
    "readonly",
    "lifecycle",
)


def trace():
    from catabolic_native import api_cli

    original = api_cli.save_secret
    calls = {}

    @functools.wraps(original)
    def wrapped(*args, **kwargs):
        value = original(*args, **kwargs)
        key = "python:catabolic.api_cli.save_secret"
        calls[key] = calls.get(key, 0) + 1
        return value

    api_cli.save_secret = wrapped
    return calls


class InstalledPrivateSecret(unittest.TestCase):
    def setUp(self):
        from catabolic_native import Store, api_cli

        self.api, self.Store = api_cli, Store
        self.temp = tempfile.TemporaryDirectory(
            prefix="catabolic-fake-secret-installed-"
        )
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        Store.initialize(self.root / "fixture.db")
        self.store = Store(self.root / "fixture.db", writable=True)
        self.addCleanup(self.store.close)
        self.fake = {
            "token_id": "FAKE-ID",
            "token": "FAKE-NOT-A-CREDENTIAL",
            "expires": 0,
        }

    def test_exact_signature_bytes_mode_and_identity(self):
        self.assertEqual(
            str(inspect.signature(self.api.save_secret)), "(store, filename, result)"
        )
        output = self.root / "FAKE.json"
        result = self.api.save_secret(self.store, output, self.fake)
        self.assertEqual(
            result,
            {"token_id": "FAKE-ID", "expires": 0, "credential_file": str(output)},
        )
        self.assertEqual(output.read_bytes(), (json.dumps(self.fake) + "\n").encode())
        self.assertEqual(output.stat().st_mode & 0o777, 0o600)
        self.assertEqual(
            self.store.rows(
                "SELECT device,inode FROM api_private_files WHERE path=?",
                (str(output),),
            ),
            [{"device": output.stat().st_dev, "inode": output.stat().st_ino}],
        )
        with self.assertRaises(FileExistsError):
            self.api.save_secret(self.store, output, self.fake)

    def test_same_owner_rollback(self):
        output = self.root / "FAKE-rollback.json"
        with self.assertRaisesRegex(LookupError, "fixture rollback"):
            with self.store.transaction() as db:
                db.execute(
                    "INSERT INTO api_principals VALUES('fake-uncommitted','default',1)"
                )
                self.api.save_secret(self.store, output, self.fake)
                self.assertEqual(
                    len(self.store.rows("SELECT * FROM api_private_files")), 1
                )
                raise LookupError("fixture rollback")
        self.assertTrue(output.is_file())
        self.assertFalse(self.store.rows("SELECT * FROM api_private_files"))
        self.assertFalse(self.store.rows("SELECT * FROM api_principals"))

    def test_callback_exception_and_reuse(self):
        sentinel = KeyboardInterrupt("fake callback")

        class Broken(dict):
            def items(self):
                raise sentinel

        with self.assertRaises(KeyboardInterrupt) as caught:
            self.api.save_secret(
                self.store, self.root / "FAKE-partial.json", Broken(self.fake)
            )
        self.assertIs(caught.exception, sentinel)
        self.assertEqual((self.root / "FAKE-partial.json").read_bytes(), b"{")
        self.assertFalse(self.store.rows("SELECT * FROM api_private_files"))
        self.api.save_secret(self.store, self.root / "FAKE-reuse.json", self.fake)

    def test_lifetime_and_summary_order(self):
        from catabolic_native import CatabolicError

        with self.assertRaises(KeyError):
            self.api.save_secret(
                self.store, self.root / "FAKE-missing-summary.json", {}
            )
        self.assertEqual(len(self.store.rows("SELECT * FROM api_private_files")), 1)
        with self.store.detached():
            with self.assertRaises(CatabolicError):
                self.api.save_secret(
                    self.store, self.root / "FAKE-detached.json", self.fake
                )
        self.assertFalse((self.root / "FAKE-detached.json").exists())
        self.store.close()
        with self.assertRaises(CatabolicError):
            self.api.save_secret(self.store, self.root / "FAKE-closed.json", self.fake)
        self.assertFalse((self.root / "FAKE-closed.json").exists())


EXPECTED_TESTS = (
    "test_exact_signature_bytes_mode_and_identity",
    "test_same_owner_rollback",
    "test_callback_exception_and_reuse",
    "test_lifetime_and_summary_order",
)


def main():
    parser = argument_parser()
    clear_stale_report(parser=parser)
    qualification = sibling("native_facade_qualification")
    args = parser.parse_args()
    for name, value in vars(args).items():
        if isinstance(value, Path):
            setattr(args, name, value.absolute())
    provenance = sibling("qualify_native_embedding")
    fixture = sibling("native_private_secret_tests")
    assert tuple(fixture.GROUPS) == EXPECTED_GROUPS
    if args.scenario:
        oracle, calls = None, {}
        if args.scenario == "reference":
            assert list(sys.version_info[:3]) == qualification.REFERENCE_VERSION
            oracle = provenance.reference_provenance(
                args.reference_root, qualification.FROZEN_COMMIT
            )
            sys.path.insert(0, str(args.reference_root / "src"))
        else:
            calls = trace()
        output = fixture.scenario(args.scenario, None, args.root)
        imports = (
            provenance.reference_imports(
                args.reference_root, oracle["tracked_source_sha256"]
            )
            if oracle
            else None
        )
        print(
            json.dumps(
                dict(
                    result=output["groups"], calls=calls, oracle=oracle, imports=imports
                )
            )
        )
        return
    assert all(
        (
            args.reference_python,
            args.inventory,
            args.source_root,
            args.wheel,
            args.build_manifest,
            args.report,
        )
    )
    inventory = json.loads(args.inventory.read_text())
    rows = qualification.inventory_rows(inventory, CONTRACTS, 1)
    identity = qualification.NativeIdentity(args.source_root)

    def fence():
        value = qualification.file_fence(__file__, args.inventory, args.build_manifest)
        value["paired_fixture_sha256"] = qualification.digest(
            Path(__file__).with_name("native_private_secret_tests.py")
        )
        return value

    before = fence()
    runtime = qualification.reference_runtime(args.reference_python)
    native_runtime = dict(
        version=list(sys.version_info[:3]),
        executable=sys.executable,
        prefix=sys.prefix,
        executable_sha256=qualification.digest(sys.executable),
    )
    source = sibling("build_native_wheel").source_digest(args.source_root)
    manifest = json.loads(args.build_manifest.read_text())
    assert manifest["profile"] == "release"
    assert manifest["source_sha256"] == manifest["source_sha256_after"] == source

    def artifact():
        return qualification.strict_artifact(
            args.wheel,
            args.build_manifest,
            args.source_root,
            native_binary=args.native_binary,
            legacy_binary=args.legacy_binary,
        )

    installed = artifact()
    oracle = provenance.reference_provenance(
        args.reference_root, inventory["reference_commit"]
    )
    calls = trace()
    result = sibling("native_qualification_results").run_required_suite(
        unittest.defaultTestLoader.loadTestsFromTestCase(InstalledPrivateSecret),
        {InstalledPrivateSecret: EXPECTED_TESTS},
        4,
    )
    with tempfile.TemporaryDirectory(prefix="catabolic-fake-secret-pair-") as directory:
        outputs = {}
        for engine, interpreter in (
            ("reference", args.reference_python),
            ("native", Path(sys.executable)),
        ):
            run = subprocess.run(
                [
                    str(interpreter),
                    "-I",
                    "-B",
                    str(Path(__file__).resolve()),
                    "--scenario",
                    engine,
                    "--root",
                    str(Path(directory) / engine),
                    "--reference-root",
                    str(args.reference_root),
                ],
                cwd=directory,
                capture_output=True,
                text=True,
                timeout=120,
            )
            assert run.returncode == 0, (engine, run.stdout, run.stderr)
            outputs[engine] = json.loads(run.stdout)
        qualification.paired_groups(
            outputs["reference"]["result"],
            outputs["native"]["result"],
            EXPECTED_GROUPS,
            17,
        )
        assert outputs["reference"]["oracle"] == oracle
    coverage = []
    for row in rows:
        key = row["id"]
        assert calls.get(key, 0) > 0 and outputs["native"]["calls"].get(key, 0) > 0
        coverage.append(
            dict(
                id=key,
                installed_calls=calls[key],
                paired_calls=outputs["native"]["calls"][key],
                selected_contracts=CONTRACTS[key],
                qualification="selected-contracts-tested-with-gaps",
            )
        )
    assert sibling("build_native_wheel").source_digest(args.source_root) == source
    identity.verify()
    assert artifact() == installed
    assert fence() == before
    assert qualification.reference_runtime(args.reference_python) == runtime
    assert qualification.digest(sys.executable) == native_runtime["executable_sha256"]
    assert (
        provenance.reference_provenance(
            args.reference_root, inventory["reference_commit"]
        )
        == oracle
    )
    report = dict(
        format=1,
        scope="One-row exclusive private credential adapter",
        passed=True,
        development_only=False,
        tests=result.testsRun,
        successful_scenarios=result.successful_scenarios,
        paired_groups=17,
        paired_group_names=list(EXPECTED_GROUPS),
        coverage=coverage,
        build=manifest,
        artifact=installed,
        oracle=oracle,
        oracle_imports=outputs["reference"]["imports"],
        reference_runtime=runtime,
        native_runtime=native_runtime,
        **before,
        limitations=[
            "Native pre-effect lifetime refusal differs from frozen closed-Store file creation",
            "SQLite exception class differs; CLI ordinary path expansion and diagnostic gaps remain",
            "Selected writer checks only; no full M10, platform, failure-schedule or release-completion claim",
        ],
    )
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            dict(
                tests=result.testsRun,
                paired_groups=17,
                inventory_rows=1,
                report=str(args.report),
            )
        )
    )


if __name__ == "__main__":
    main()
