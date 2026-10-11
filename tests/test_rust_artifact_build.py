# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native paired-artifact source fences, independently of any compiler run."""

import hashlib
import importlib.util
import json
import os
import py_compile
import subprocess
import sys
import tempfile
import types
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "native_wheel_builder", ROOT / "scripts/build_native_wheel.py"
)
BUILDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILDER)
QUALIFIER_SPEC = importlib.util.spec_from_file_location(
    "native_mutation_qualifier", ROOT / "scripts/qualify_native_mutations.py"
)
QUALIFIER = importlib.util.module_from_spec(QUALIFIER_SPEC)
QUALIFIER_SPEC.loader.exec_module(QUALIFIER)


class NativeArtifactBuildTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml"):
            (self.root / name).write_text(name)
        source = self.root / "crates/example/src"
        source.mkdir(parents=True)
        (source / "lib.rs").write_text("pub const VALUE: u8 = 1;")

    def test_crate_workspace_resource_and_cargo_configuration_are_fenced(self):
        paths = [
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "crates/example/src/lib.rs",
            "crates/example/resources/schema.json",
            "src/catabolic/http/releases/1.7.0/openapi.json",
            ".cargo/config.toml",
            "crates/example/.cargo/config.toml",
        ]
        for name in paths:
            before = BUILDER.source_digest(self.root)
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            original = path.read_bytes() if path.exists() else None
            path.write_bytes((original or b"") + b"changed")
            with self.subTest(name=name):
                self.assertNotEqual(before, BUILDER.source_digest(self.root))
            if original is None:
                path.unlink()
            else:
                path.write_bytes(original)
            self.assertEqual(before, BUILDER.source_digest(self.root))

    def test_generated_helper_and_python_cache_do_not_self_invalidate_build(self):
        before = BUILDER.source_digest(self.root)
        for name in (
            "crates/catabolic-python/python/catabolic_native/bin/catabolic-native",
            "crates/catabolic-python/python/catabolic_native/bin/catabolic-native.tmp",
            "crates/catabolic-python/python/catabolic_native/native-helper.json",
            "crates/catabolic-python/python/catabolic_native/__pycache__/module.pyc",
        ):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("local generated artifact")
        self.assertEqual(before, BUILDER.source_digest(self.root))

    def test_optimized_qualifier_refuses_before_emitting_receipt(self):
        # Supply only a disposable import stub; optimized refusal must not
        # depend on any installed extension, wheel or successful qualification.
        (self.root / "catabolic_native.py").write_text("")
        import os

        result = subprocess.run(
            [
                sys.executable,
                "-O",
                str(ROOT / "scripts/qualify_native_wheel.py"),
                "--help",
            ],
            env=os.environ | {"PYTHONPATH": str(self.root)},
            cwd=self.root,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires Python assertions", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_mutation_qualification_fences_implementation_and_test_sources(self):
        before = QUALIFIER.source_snapshot(self.root)
        for name in (
            "crates/example/src/lib.rs",
            "src/catabolic/store.py",
            "tests/test_example.py",
            "tests/fixtures/catalog.json",
            "scripts/qualify_native_mutations.py",
            ".cargo/config.toml",
        ):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            original = path.read_bytes() if path.exists() else None
            path.write_bytes((original or b"") + b"changed")
            with self.subTest(name=name):
                after = QUALIFIER.source_snapshot(self.root)
                self.assertNotEqual(before["sha256"], after["sha256"])
                self.assertIn(name, after["files"])
            if original is None:
                path.unlink()
            else:
                path.write_bytes(original)
            self.assertEqual(before, QUALIFIER.source_snapshot(self.root))

    def test_optimized_mutation_qualification_refuses(self):
        result = subprocess.run(
            [
                sys.executable,
                "-O",
                str(ROOT / "scripts/qualify_native_mutations.py"),
                "--help",
            ],
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires unoptimized Python tests", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_http_qualification_refuses_optimized_and_fences_client_artifacts(self):
        script = ROOT / "scripts/qualify_native_http.py"
        result = subprocess.run(
            [sys.executable, "-O", str(script), "--help"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires unoptimized Python assertions", result.stderr)
        self.assertEqual(result.stdout, "")
        spec = importlib.util.spec_from_file_location("http_qualifier", script)
        qualifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(qualifier)
        binary = self.root / "native"
        binary.write_bytes(b"native fixture")
        client = self.root / "client/acceptance.js"
        client.parent.mkdir()
        client.write_text("fixture client")
        dependency = client.parent / "busy.js"
        dependency.write_text("fixture import")
        before = qualifier.artifact_hashes(binary, client)
        for path in (binary, client, dependency):
            original = path.read_bytes()
            path.write_bytes(original + b"changed")
            self.assertNotEqual(before, qualifier.artifact_hashes(binary, client))
            path.write_bytes(original)
            self.assertEqual(before, qualifier.artifact_hashes(binary, client))

    def test_mutation_qualification_refuses_imported_oracles_and_unbound_http(self):
        native = "tests.test_rust_example.NativeTest.test_case"
        oracle = "tests.test_example.ReferenceTest.test_case"
        http = "tests.test_rust_http_transport.NativeHTTPTransportTest.test_case"
        QUALIFIER.validate_native_scenarios([native], None)
        with self.assertRaisesRegex(ValueError, "non-native oracle tests leaked"):
            QUALIFIER.validate_native_scenarios([native, oracle], None)
        with self.assertRaisesRegex(ValueError, "explicit --http-binary artifact"):
            QUALIFIER.validate_native_scenarios([native, http], None)
        QUALIFIER.validate_native_scenarios([native, http], self.root / "http")

    def test_frozen_oracle_and_historical_fixture_bytes_are_verified(self):
        oracle = self.root / "oracle"
        source = oracle / "src/catabolic/app.py"
        source.parent.mkdir(parents=True)
        source.write_text("fixture frozen source")
        inventory = self.root / "tests/parity/inventory.json"
        inventory.parent.mkdir(parents=True)
        inventory.write_text(
            json.dumps(
                {
                    "resource_sha256": {
                        "src/catabolic/app.py": hashlib.sha256(
                            source.read_bytes()
                        ).hexdigest()
                    }
                }
            )
        )
        QUALIFIER.oracle_snapshot(oracle, self.root)
        source.write_text("changed source")
        with self.assertRaisesRegex(ValueError, "frozen oracle byte drift"):
            QUALIFIER.oracle_snapshot(oracle, self.root)
        source.write_text("fixture frozen source")
        (source.parent / "extra.py").write_text("unrecorded import")
        with self.assertRaisesRegex(ValueError, "unrecorded payloads"):
            QUALIFIER.oracle_snapshot(oracle, self.root)
        fixtures = (
            self.root / ".local-tests/rust-migration/m0-completion/legacy-fixtures"
        )
        entries = []
        for schema in range(1, 33):
            relative = f"schema-{schema:02d}/before.sqlite3"
            path = fixtures / relative
            path.parent.mkdir(parents=True)
            path.write_bytes(bytes([schema]))
            entries.append(
                {
                    "schema": schema,
                    "database": relative,
                    "status": "passed",
                    "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                }
            )
        (fixtures / "report.json").write_text(
            json.dumps({"reference_commit": QUALIFIER.REFERENCE, "fixtures": entries})
        )
        self.assertEqual(len(QUALIFIER.fixture_snapshot(self.root)["sha256"]), 33)
        (fixtures / "schema-01/before.sqlite3").write_bytes(b"changed fixture")
        with self.assertRaisesRegex(ValueError, "historical fixture byte drift"):
            QUALIFIER.fixture_snapshot(self.root)

    def test_fresh_oracle_cache_ignores_timestamp_valid_stale_bytecode(self):
        source = self.root / "oracle_fixture.py"
        source.write_text("VALUE = 2\n")
        original = source.stat()
        bytecode = (
            source.parent
            / "__pycache__"
            / (f"oracle_fixture.{sys.implementation.cache_tag}.pyc")
        )
        bytecode.parent.mkdir()
        py_compile.compile(str(source), cfile=str(bytecode), doraise=True)
        source.write_text("VALUE = 1\n")
        os.utime(source, ns=(original.st_atime_ns, original.st_mtime_ns))
        self.assertEqual(source.stat().st_size, original.st_size)
        environment = os.environ.copy()
        environment.pop("PYTHONPYCACHEPREFIX", None)
        environment["PYTHONPATH"] = str(self.root)
        baseline = subprocess.check_output(
            [
                sys.executable,
                "-c",
                "import oracle_fixture; print(oracle_fixture.VALUE)",
            ],
            cwd=self.root,
            env=environment,
            text=True,
        )
        self.assertEqual(baseline.strip(), "2", "fixture must execute cached bytes")
        protected = subprocess.check_output(
            [
                sys.executable,
                "-c",
                "import importlib.util; "
                f"spec=importlib.util.spec_from_file_location('qualifier',{str(ROOT / 'scripts/qualify_native_mutations.py')!r}); "
                "module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module); "
                "cache=module.fresh_import_cache(); "
                "import oracle_fixture; print(oracle_fixture.VALUE); cache.cleanup()",
            ],
            cwd=self.root,
            env=environment,
            text=True,
        )
        self.assertEqual(protected.strip(), "1")

    def test_wheel_inventory_cannot_pass_with_missing_installed_payloads(self):
        installed = self.root / "installed"
        package = installed / "catabolic_native"
        payloads = {
            "catabolic_native/__init__.py": b"fixture package",
            "catabolic_native/_native.abi3.so": b"fixture extension",
            "catabolic_native/app.py": b"fixture public facade",
            "catabolic_native/bin/catabolic-native": b"fixture helper",
            "catabolic_native/native-helper.json": b"{}",
            "catabolic_native-0.2.0.dist-info/WHEEL": b"Wheel-Version: 1.0",
            "catabolic_native-0.2.0.dist-info/METADATA": b"Name: catabolic-native",
            "catabolic_native-0.2.0.dist-info/entry_points.txt": b"fixture launcher",
        }
        for name, content in payloads.items():
            path = installed / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        stub = types.SimpleNamespace(
            __file__=str(package / "__init__.py"),
            __version__="0.2.0",
            _native=types.SimpleNamespace(__file__=str(package / "_native.abi3.so")),
        )
        spec = importlib.util.spec_from_file_location(
            "installed_wheel_verifier", ROOT / "scripts/qualify_native_wheel.py"
        )
        verifier = importlib.util.module_from_spec(spec)
        with patch.dict(sys.modules, {"catabolic_native": stub}):
            spec.loader.exec_module(verifier)
        distribution = types.SimpleNamespace(
            files=list(payloads)
            + [
                "catabolic_native-0.2.0.dist-info/RECORD",
                "../../../bin/catabolic-native",
            ],
            locate_file=lambda name: installed / name,
            version="0.2.0",
        )
        with patch.object(
            verifier.importlib.metadata, "distribution", return_value=distribution
        ):
            wheel = self.root / "fixture.whl"
            for omitted in (
                None,
                "all",
                "catabolic_native/app.py",
                "catabolic_native/bin/catabolic-native",
            ):
                with zipfile.ZipFile(wheel, "w") as archive:
                    for name, content in payloads.items():
                        if omitted != "all" and name != omitted:
                            archive.writestr(name, content)
                with self.subTest(omitted=omitted):
                    if omitted is None:
                        self.assertTrue(
                            verifier.verify_installed_wheel(wheel)[
                                "installed_payloads_match_wheel"
                            ]
                        )
                    else:
                        with self.assertRaises(AssertionError):
                            verifier.verify_installed_wheel(wheel)
            with zipfile.ZipFile(wheel, "w") as archive:
                for name, content in payloads.items():
                    archive.writestr(name, content)
            outside = self.root / "unqualified-package"
            outside.mkdir()
            (outside / "__init__.py").write_text("unqualified shadow package")
            shadow = package / "shadow"
            shadow.symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(AssertionError, "unqualified symlink"):
                verifier.verify_installed_wheel(wheel)
            shadow.unlink()
            self.assertTrue(
                verifier.verify_installed_wheel(wheel)["installed_payloads_match_wheel"]
            )

    def test_reference_loader_excludes_native_before_import(self):
        spec = importlib.util.spec_from_file_location(
            "reference_test_runner", ROOT / "scripts/run_reference_tests.py"
        )
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        (self.root / "test_reference_fixture.py").write_text(
            "import unittest\nclass Example(unittest.TestCase):\n"
            "    def test_original(self): pass\n"
        )
        (self.root / "test_rust_should_not_import.py").write_text(
            "raise RuntimeError('native prerequisites must not be imported')\n"
        )
        self.addCleanup(sys.modules.pop, "test_reference_fixture", None)
        loader = runner.ReferenceLoader()
        suite = loader.discover(str(self.root))
        self.assertEqual(
            list(runner.identifiers(suite)),
            ["test_reference_fixture.Example.test_original"],
        )
        self.assertEqual(loader.native_files, ["test_rust_should_not_import.py"])
