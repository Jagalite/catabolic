#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Exact installed immutable import gate: four inventory rows, eight tests.

The --dev mode is helperless development evidence only. Release evidence requires
an exact installed wheel, paired CLIs, build manifest, and unchanged source fence.
"""

import argparse
import importlib
import importlib.util
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
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--wheel", type=Path)
    mode.add_argument("--dev", action="store_true")
    parser.add_argument("--build-manifest", type=Path)
    parser.add_argument(
        "--source-root", type=Path, default=Path(__file__).resolve().parents[1]
    )
    parser.add_argument("--inventory", type=Path, required=True)
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--reference-python", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    return parser


# CLI preflight precedes project imports and optimized-mode refusal.
# Library imports never inspect caller arguments or invalidate their receipts.
if __name__ == "__main__":
    clear_stale_report(parser=argument_parser())

ROOT = Path(__file__).resolve().parents[1]
EXPECTED_TESTS = (
    "test_occurrences_sql_exact_bytes",
    "test_topics_exact_recursive_type",
    "test_topics_nested_immutability",
    "test_graphql_sdl_public_exact_bytes",
    "test_http_contract_version_exact_bytes",
    "test_reexports_share_native_objects",
    "test_reload_preserves_values",
    "test_fresh_import_has_no_optional_engines_or_effects",
)


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def bindings():
    from catabolic_native.component_sql import OCCURRENCES_SQL
    from catabolic_native.documentation import TOPICS
    from catabolic_native.graphql_query import SDL
    from catabolic_native.http.contract import VERSION

    return dict(
        zip(
            sibling("generate_public_import_bindings").BINDINGS,
            (OCCURRENCES_SQL, TOPICS, SDL, VERSION),
            strict=True,
        )
    )


IMPORT_PROBE = r"""
import importlib.abc, importlib.metadata, json, os, pathlib, sqlite3, sys
assert sys.flags.isolated and sys.dont_write_bytecode
assert not any(n == 'catabolic_native' or n.startswith('catabolic_native.') for n in sys.modules)
blocked = {'catabolic', 'graphql', 'fastapi', 'pydantic', 'uvicorn'}
class RefuseOptional(importlib.abc.MetaPathFinder):
    def find_spec(self, fullname, path=None, target=None):
        if fullname.split('.')[0] in blocked:
            raise AssertionError('unexpected engine import: ' + fullname)
sys.meta_path.insert(0, RefuseOptional())
installed = {d.metadata['Name'].lower().replace('_', '-') for d in importlib.metadata.distributions()}
assert not (installed & {'catabolic', 'graphql-core', 'fastapi', 'pydantic', 'uvicorn'})
def audit(event, args):
    if event.startswith(('socket.', 'subprocess.', 'sqlite3.connect')) or event in {'os.system', 'os.fork', 'os.posix_spawn'}:
        raise AssertionError('unexpected import side effect: ' + event)
    if event == 'open':
        _, mode, flags = args
        if (mode and any(c in mode for c in 'wax+')) or flags & (os.O_WRONLY | os.O_RDWR | os.O_CREAT | os.O_TRUNC | os.O_APPEND):
            raise AssertionError('unexpected import file write')
sys.addaudithook(audit)
from catabolic_native.component_sql import OCCURRENCES_SQL
from catabolic_native.documentation import TOPICS
from catabolic_native.graphql_query import SDL
from catabolic_native.http.contract import VERSION
assert type(OCCURRENCES_SQL) is type(SDL) is type(VERSION) is str
assert type(TOPICS) is tuple and len(TOPICS) == 49
assert not any(n.split('.')[0] in blocked for n in sys.modules)
assert not list(pathlib.Path.cwd().iterdir())
print(json.dumps({'imports': sorted(n for n in sys.modules if n.startswith('catabolic_native')), 'optional_engines_absent': True, 'import_writes_network_database_and_subprocess_calls': 0}))
"""


class InstalledPublicImports(unittest.TestCase):
    values = None
    expected = None
    generator = None
    import_probe = None

    def check(self, name):
        identifier = "python:catabolic." + name
        value = self.values[identifier]
        self.assertEqual(self.generator.tagged(value), self.expected[identifier])
        return value

    def test_occurrences_sql_exact_bytes(self):
        self.assertIs(type(self.check("component_sql.OCCURRENCES_SQL")), str)

    def test_topics_exact_recursive_type(self):
        topics = self.check("documentation.TOPICS")
        self.assertIs(type(topics), tuple)
        self.assertEqual(len(topics), 49)
        self.assertTrue(all(type(row) is tuple and len(row) == 4 for row in topics))
        self.assertTrue(all(type(value) is str for row in topics for value in row))

    def test_topics_nested_immutability(self):
        topics = self.check("documentation.TOPICS")
        with self.assertRaises(TypeError):
            topics[0] = ()
        with self.assertRaises(TypeError):
            topics[0][0] = "changed"
        self.check("documentation.TOPICS")

    def test_graphql_sdl_public_exact_bytes(self):
        self.assertIs(type(self.check("graphql_query.SDL")), str)

    def test_http_contract_version_exact_bytes(self):
        self.assertIs(type(self.check("http.contract.VERSION")), str)

    def test_reexports_share_native_objects(self):
        from catabolic_native import _native

        for name, value in zip(
            ("OCCURRENCES_SQL", "TOPICS", "SDL", "HTTP_CONTRACT_VERSION"),
            self.values.values(),
            strict=True,
        ):
            self.assertIs(value, getattr(_native, name))

    def test_reload_preserves_values(self):
        for identifier, expected in self.values.items():
            module, name = identifier.replace(
                "python:catabolic.", "catabolic_native."
            ).rsplit(".", 1)
            self.assertIs(
                getattr(importlib.reload(importlib.import_module(module)), name),
                expected,
            )
        self.assertEqual(bindings(), self.values)

    def test_fresh_import_has_no_optional_engines_or_effects(self):
        with tempfile.TemporaryDirectory() as directory:
            environment = {
                key: value
                for key, value in os.environ.items()
                if not key.startswith(("CATABOLIC_", "PYTHON"))
            }
            environment["HOME"] = directory
            run = subprocess.run(
                [sys.executable, "-I", "-B", "-c", IMPORT_PROBE],
                cwd=directory,
                env=environment,
                text=True,
                capture_output=True,
                timeout=30,
            )
            self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
            self.assertEqual(list(Path(directory).iterdir()), [])
            type(self).import_probe = json.loads(run.stdout)


def main():
    parser = argument_parser()
    clear_stale_report(parser=parser)
    shared = sibling("native_facade_qualification")
    args = parser.parse_args()
    if not __debug__ or not sys.flags.isolated or os.environ.get("PYTHONOPTIMIZE"):
        raise RuntimeError(
            "public import qualification requires assertions and isolated Python -I"
        )
    assert args.source_root.resolve() == ROOT
    generator = sibling("generate_public_import_bindings")
    rows = generator.select_rows(json.loads(args.inventory.read_text()))
    assert shared.digest(args.inventory) == shared.digest(
        ROOT / "tests/parity/python-api.json"
    )
    identity = shared.NativeIdentity(ROOT)
    assert not any(
        name == "catabolic" or name.startswith("catabolic.") for name in sys.modules
    )
    for entry in sys.path:
        assert not Path(entry).resolve().is_relative_to(ROOT)
        assert not Path(entry).resolve().is_relative_to(args.reference_root.resolve())
    artifact = None
    if args.wheel:
        assert args.build_manifest, (
            "release evidence requires the paired build manifest"
        )
        artifact = shared.strict_artifact(args.wheel, args.build_manifest, ROOT)
    else:
        assert args.build_manifest is None, (
            "development evidence cannot claim a release manifest"
        )
        package = identity.package.parent
        assert not (package / "native-helper.json").exists()
        assert not (package / "bin/catabolic-native").exists()
    source_fence = shared.sibling("build_native_wheel").source_digest(ROOT)
    files = [
        Path(__file__),
        Path(__file__).with_name("generate_public_import_bindings.py"),
        Path(__file__).with_name("native_facade_qualification.py"),
        Path(__file__).with_name("native_qualification_results.py"),
        Path(__file__).with_name("qualify_native_wheel.py"),
        Path(__file__).with_name("qualify_native_embedding.py"),
        ROOT / generator.CONTRACT,
        args.inventory,
    ]
    file_fence = {str(path.relative_to(ROOT)): shared.digest(path) for path in files}
    oracle_runtime = shared.reference_runtime(args.reference_python)
    # The oracle runs in its own interpreter and never shares the native process.
    run = subprocess.run(
        [
            str(args.reference_python.absolute()),
            "-I",
            "-B",
            str(Path(__file__).with_name("generate_public_import_bindings.py")),
            "--source",
            str(args.reference_root.resolve()),
            "--snapshot",
        ],
        cwd=Path.cwd(),
        text=True,
        capture_output=True,
        timeout=120,
    )
    assert run.returncode == 0, run.stdout + run.stderr
    expected = json.loads(run.stdout)
    values = bindings()
    actual = {
        identifier: generator.tagged(value) for identifier, value in values.items()
    }
    shared.paired_groups(expected, actual, tuple(generator.BINDINGS), expected_count=4)
    InstalledPublicImports.values = values
    InstalledPublicImports.expected = expected
    InstalledPublicImports.generator = generator
    result = sibling("native_qualification_results").run_required_suite(
        unittest.defaultTestLoader.loadTestsFromTestCase(InstalledPublicImports),
        {InstalledPublicImports: EXPECTED_TESTS},
        expected_count=8,
    )
    identity.verify()
    assert source_fence == shared.sibling("build_native_wheel").source_digest(ROOT)
    assert file_fence == {
        str(path.relative_to(ROOT)): shared.digest(path) for path in files
    }
    if args.wheel:
        assert artifact == shared.strict_artifact(args.wheel, args.build_manifest, ROOT)
    report = {
        "format": 1,
        "scope": "Four immutable companion-namespace imports only; no Python default replacement or callable/schema/SQL semantic claim",
        "development_only": args.dev,
        "release_artifact": artifact,
        "python": sys.version,
        "executable": sys.executable,
        "source_sha256": source_fence,
        "files_sha256": file_fence,
        "oracle_runtime": oracle_runtime,
        "oracle_contract": json.loads((ROOT / generator.CONTRACT).read_text()),
        "acceptance_tests": result.testsRun,
        "successful_scenarios": result.successful_scenarios,
        "paired_groups": 4,
        "inventory_rows": 4,
        "passed_without_skips": True,
        "import_probe": InstalledPublicImports.import_probe,
        "installed_extension_sha256": shared.digest(identity.path),
        "coverage": [
            {
                "id": row["id"],
                "classification": row["classification"],
                "qualification": "exact-immutable-value-and-type-tested",
                "value": generator.metadata(values[row["id"]]),
            }
            for row in rows
        ],
    }
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            {
                "tests": 8,
                "paired_groups": 4,
                "inventory_rows": 4,
                "development_only": args.dev,
                "report": str(args.report),
            }
        )
    )


if __name__ == "__main__":
    main()
