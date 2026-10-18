# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native aggregate fixture admission and truthful preflight receipt boundaries."""

import ast
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/qualify_native_mutations.py"
SPEC = importlib.util.spec_from_file_location("fixture_provenance_qualifier", SCRIPT)
QUALIFIER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(QUALIFIER)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class NativeFixtureProvenanceTest(unittest.TestCase):
    def test_publication_preview_discovery_has_no_imported_testcase_duplicates(self):
        suite = unittest.TestSuite(
            unittest.defaultTestLoader.loadTestsFromName(name)
            for name in (
                "tests.test_rust_publication_mapping_preview",
                "tests.test_rust_publication_bundle_preview",
            )
        )
        identifiers = list(QUALIFIER.cases(suite))
        self.assertEqual(len(identifiers), 25)
        self.assertEqual(len(identifiers), len(set(identifiers)))
        self.assertEqual(
            sum(".PublicationMappingPreviewTest." in name for name in identifiers),
            9,
        )
        self.assertEqual(
            sum(".PublicationBundlePreviewTest." in name for name in identifiers),
            16,
        )

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root = self.base / "current"
        self.reference = self.base / "reference"
        self.frozen = {}
        self.modules = {}
        for relative in (
            "src/catabolic/__init__.py",
            "src/catabolic/app.py",
            "tests/__init__.py",
            "tests/test_unchanged.py",
            "tests/test_adapted.py",
            "tests/test_lazy.py",
        ):
            path = self.write(self.reference, relative, b"# frozen\n")
            self.frozen[relative] = digest(path)
            if relative.startswith("tests/"):
                self.write(self.root, relative, path.read_bytes())
        self.write(
            self.root,
            "tests/parity/inventory.json",
            json.dumps({"resource_sha256": self.frozen}).encode(),
        )
        entries = {}
        for name in ("test_adapted", "test_lazy", "helper"):
            relative = f"tests/{name}.py"
            path = self.write(self.root, relative, b"# reviewed local bytes\n")
            entries[f"tests.{name}"] = {
                "path": relative,
                "kind": "fixture-helper" if name == "helper" else "fixture-adaptation",
                "frozen_sha256": self.frozen.get(relative),
                "sha256": digest(path),
                "reviewed_commit": "a" * 40,
                "reason": "reviewed fixture-only scheduling correction",
            }
        self.manifest = {
            "schema": 1,
            "reference_commit": QUALIFIER.REFERENCE,
            "modules": entries,
        }
        self.save_manifest()
        self.write(self.root, "tests/test_rust_local.py", b"# native qualification\n")
        self.oracle = QUALIFIER.oracle_snapshot(self.reference, self.root)
        self.sources = QUALIFIER.source_snapshot(self.root)
        self.adaptations = QUALIFIER.adaptation_snapshot(
            self.root, self.oracle, self.sources
        )

    def write(self, root, relative, data):
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def save_manifest(self):
        return self.write(
            self.root,
            "tests/parity/native-fixture-adaptations.json",
            json.dumps(self.manifest).encode(),
        )

    def admit(self, name, path):
        self.modules[name] = types.SimpleNamespace(__file__=str(path))

    def imports(self):
        with patch.object(QUALIFIER.sys, "modules", self.modules):
            return QUALIFIER.oracle_imports(
                self.reference, self.oracle, self.root, self.sources, self.adaptations
            )

    def test_exact_frozen_and_individually_reviewed_provenance_are_distinct(self):
        self.admit("catabolic.app", self.reference / "src/catabolic/app.py")
        self.admit("tests.test_unchanged", self.root / "tests/test_unchanged.py")
        self.admit("tests.test_adapted", self.root / "tests/test_adapted.py")
        self.admit("tests.helper", self.root / "tests/helper.py")
        self.admit("tests.test_rust_local", self.root / "tests/test_rust_local.py")
        before = self.imports()
        self.assertEqual(
            {name: value["provenance"] for name, value in before.items()},
            {
                "catabolic.app": "frozen-application",
                "tests.test_unchanged": "frozen-fixture",
                "tests.test_adapted": "reviewed-fixture-adaptation",
                "tests.helper": "reviewed-fixture-helper",
                "tests.test_rust_local": "native-qualification-test",
            },
        )
        self.admit("tests.test_lazy", self.root / "tests/test_lazy.py")
        after = self.imports()
        self.assertTrue(all(after[name] == value for name, value in before.items()))
        self.assertEqual(
            after["tests.test_lazy"]["provenance"], "reviewed-fixture-adaptation"
        )
        self.assertEqual(self.sources, QUALIFIER.source_snapshot(self.root))
        self.assertEqual(
            self.oracle, QUALIFIER.oracle_snapshot(self.reference, self.root)
        )

    def test_checked_in_manifest_has_exact_reviewed_timing_set_and_baselines(self):
        manifest = json.loads(
            (ROOT / "tests/parity/native-fixture-adaptations.json").read_text()
        )
        reviewed = {
            "tests.probe_timing": "340cd70e76ba0e4b4ffbe7b697a9ce4520cd708c",
            "tests.test_component_packages": "340cd70e76ba0e4b4ffbe7b697a9ce4520cd708c",
            "tests.test_component_lifecycle": "340cd70e76ba0e4b4ffbe7b697a9ce4520cd708c",
            "tests.test_fallback_identity": "9924d23e40bffe657932ee344b3302687bc0027f",
            "tests.scan_report_support": "2784bd4601d6243899c313ff5f58243ccce0ba04",
        }
        self.assertEqual(set(manifest["modules"]), set(reviewed))
        frozen = json.loads((ROOT / "tests/parity/inventory.json").read_text())[
            "resource_sha256"
        ]
        for name, entry in manifest["modules"].items():
            self.assertEqual(entry["reviewed_commit"], reviewed[name])
            self.assertEqual(entry["frozen_sha256"], frozen.get(entry["path"]))
            self.assertEqual(entry["sha256"], digest(ROOT / entry["path"]))

    def test_unknown_fixture_and_native_prefix_escape_cannot_bypass_admission(self):
        for name, path in (
            (
                "tests.unknown",
                self.write(self.root, "tests/unknown.py", b"# unknown\n"),
            ),
            (
                "tests.test_unchanged",
                self.write(self.base, "outside/tests/test_unchanged.py", b"# frozen\n"),
            ),
            (
                "tests.test_rust_local",
                self.write(
                    self.base,
                    "outside/tests/test_rust_local.py",
                    b"# native qualification\n",
                ),
            ),
            (
                "tests.helper",
                self.write(
                    self.reference, "tests/helper.py", b"# reviewed local bytes\n"
                ),
            ),
        ):
            with self.subTest(name=name, path=path):
                self.modules = {}
                self.admit(name, path)
                with self.assertRaises(ValueError):
                    self.imports()

    def test_module_identity_and_missing_source_fail_closed(self):
        self.admit("tests.test_unchanged", self.root / "tests/test_adapted.py")
        with self.assertRaisesRegex(ValueError, "identity differs"):
            self.imports()
        self.modules = {"tests.helper": types.SimpleNamespace()}
        with self.assertRaisesRegex(ValueError, "verifiable source file"):
            self.imports()
        path = self.write(self.root, "src/catabolic/app.py", b"# frozen\n")
        self.modules = {}
        self.admit("catabolic.app", path)
        with self.assertRaisesRegex(ValueError, "escaped frozen source"):
            self.imports()

    def test_adapted_helper_and_lazy_bytes_are_hash_bound_before_and_after(self):
        for name, entry in self.manifest["modules"].items():
            path = self.root / entry["path"]
            original = path.read_bytes()
            try:
                path.write_bytes(original + b"# drift\n")
                with self.subTest(name=name):
                    with self.assertRaisesRegex(
                        ValueError, "reviewed fixture byte drift"
                    ):
                        QUALIFIER.adaptation_snapshot(
                            self.root, self.oracle, QUALIFIER.source_snapshot(self.root)
                        )
                    self.modules = {}
                    self.admit(name, path)
                    with self.assertRaisesRegex(ValueError, "changed since snapshot"):
                        self.imports()
                    self.assertNotEqual(
                        self.sources, QUALIFIER.source_snapshot(self.root)
                    )
            finally:
                path.write_bytes(original)
        self.assertEqual(self.sources, QUALIFIER.source_snapshot(self.root))

    def test_manifest_drift_or_forged_frozen_baseline_is_rejected(self):
        self.manifest["modules"]["tests.test_adapted"]["frozen_sha256"] = "0" * 64
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, "manifest changed"):
            QUALIFIER.adaptation_snapshot(self.root, self.oracle, self.sources)
        with self.assertRaisesRegex(ValueError, "frozen baseline differs"):
            QUALIFIER.adaptation_snapshot(
                self.root, self.oracle, QUALIFIER.source_snapshot(self.root)
            )

    def test_unchanged_borrowed_fixture_drift_is_never_admitted_as_local(self):
        path = self.root / "tests/test_unchanged.py"
        path.write_bytes(b"# changed\n")
        self.admit("tests.test_unchanged", path)
        with self.assertRaisesRegex(ValueError, "does not match frozen bytes"):
            self.imports()

    def test_frozen_application_and_original_adapted_fixture_remain_exact(self):
        for relative in ("src/catabolic/app.py", "tests/test_adapted.py"):
            path = self.reference / relative
            original = path.read_bytes()
            try:
                path.write_bytes(original + b"# drift\n")
                with (
                    self.subTest(relative=relative),
                    self.assertRaisesRegex(ValueError, "frozen oracle byte drift"),
                ):
                    QUALIFIER.oracle_snapshot(self.reference, self.root)
            finally:
                path.write_bytes(original)

    def test_frozen_package_and_fixture_unrecorded_payloads_are_rejected(self):
        for relative in ("src/catabolic/unrecorded.py", "tests/unrecorded.py"):
            path = self.write(self.reference, relative, b"# unrecorded\n")
            try:
                with (
                    self.subTest(relative=relative),
                    self.assertRaisesRegex(ValueError, "unrecorded payloads"),
                ):
                    QUALIFIER.oracle_snapshot(self.reference, self.root)
            finally:
                path.unlink()

    def test_equal_byte_symlink_files_and_directories_are_rejected(self):
        linked_parent = self.base / "linked-parent"
        linked_parent.symlink_to(self.base, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "linked provenance input"):
            QUALIFIER.verified_file(linked_parent / "current", "tests/helper.py")
        path = self.root / "tests/helper.py"
        outside = self.write(self.base, "outside/helper.py", path.read_bytes())
        path.unlink()
        path.symlink_to(outside)
        self.admit("tests.helper", path)
        with self.assertRaisesRegex(ValueError, "linked provenance input"):
            self.imports()
        with self.assertRaisesRegex(ValueError, "contains a symlink"):
            QUALIFIER.source_snapshot(self.root)
        path.unlink()
        path.write_bytes(outside.read_bytes())
        tests = self.root / "tests"
        tests.rename(self.root / "real-tests")
        tests.symlink_to(self.root / "real-tests", target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "linked provenance input"):
            self.imports()
        with self.assertRaisesRegex(ValueError, "contains a symlink"):
            QUALIFIER.source_snapshot(self.root)
        reference_tests = self.reference / "tests"
        reference_tests.rename(self.reference / "real-tests")
        reference_tests.symlink_to(
            self.reference / "real-tests", target_is_directory=True
        )
        with self.assertRaisesRegex(ValueError, "linked provenance input"):
            QUALIFIER.oracle_snapshot(self.reference, self.root)

    def test_new_native_payload_after_snapshot_cannot_be_imported(self):
        path = self.write(self.root, "tests/test_rust_new.py", b"# new native test\n")
        self.admit("tests.test_rust_new", path)
        with self.assertRaisesRegex(ValueError, "changed since snapshot"):
            self.imports()


class NativeDependencyLinkProvenanceTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.clients = self.root / "tests/clients"
        self.modules = self.clients / "node_modules"
        self.bin = self.modules / ".bin"
        self.bin.mkdir(parents=True)
        self.lock_path = self.clients / "package-lock.json"
        self.lock = {
            "lockfileVersion": 3,
            "packages": {
                "node_modules/example": {
                    "version": "1.2.3",
                    "bin": {"example": "bin/run.js"},
                },
                "node_modules/@scope/tool": {
                    "version": "4.5.6",
                    "bin": {"tool": "cli.js"},
                },
            },
        }
        self.save_lock()
        for package, filename, alias in (
            ("example", "bin/run.js", "example"),
            ("@scope/tool", "cli.js", "tool"),
        ):
            path = self.modules / package / filename
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"// equal locked target bytes\n")
            (self.bin / alias).symlink_to("../" + package + "/" + filename)

    def save_lock(self):
        self.lock_path.write_text(json.dumps(self.lock))

    def replace_link(self, target, alias="example"):
        path = self.bin / alias
        path.unlink(missing_ok=True)
        path.symlink_to(target)
        return path

    def test_legal_locked_aliases_preserve_all_file_bytes_and_add_exact_metadata(self):
        before = QUALIFIER.source_snapshot(self.root)
        historical = {
            str(path.relative_to(self.root)): digest(path)
            for path in (self.root / "tests").rglob("*")
            if path.is_file()
        }
        self.assertEqual(before["schema"], 2)
        self.assertEqual(before["files"], historical)
        self.assertEqual(
            before["files_sha256"],
            hashlib.sha256(
                json.dumps(historical, sort_keys=True, separators=(",", ":")).encode()
            ).hexdigest(),
        )
        alias = before["dependency_links"]["tests/clients/node_modules/.bin/tool"]
        self.assertEqual(
            alias,
            {
                "target": "../@scope/tool/cli.js",
                "resolved_path": "tests/clients/node_modules/@scope/tool/cli.js",
                "target_sha256": digest(self.modules / "@scope/tool/cli.js"),
                "lockfile": "tests/clients/package-lock.json",
                "lockfile_sha256": digest(self.lock_path),
                "package": "node_modules/@scope/tool",
                "package_version": "4.5.6",
            },
        )
        self.assertEqual(before, QUALIFIER.source_snapshot(self.root))

    def test_unknown_absolute_escaping_and_noncanonical_aliases_are_rejected(self):
        for target in (
            str(self.modules / "example/bin/run.js"),
            "../../example/bin/run.js",
            "../example/../example/bin/run.js",
            "../example//bin/run.js",
            "../example/bin/./run.js",
            "../@scope/tool/cli.js",
            "../missing/bin/run.js",
        ):
            with self.subTest(target=target):
                self.replace_link(target)
                with self.assertRaises(ValueError):
                    QUALIFIER.source_snapshot(self.root)
        self.replace_link("../example/bin/run.js")
        (self.bin / "undeclared").symlink_to("../example/bin/run.js")
        with self.assertRaisesRegex(ValueError, "locked bin alias"):
            QUALIFIER.source_snapshot(self.root)

    def test_target_bytes_and_lockfile_bytes_change_both_required_fences(self):
        before = QUALIFIER.source_snapshot(self.root)
        target = self.modules / "example/bin/run.js"
        original = target.read_bytes()
        target.write_bytes(original + b"// changed\n")
        after = QUALIFIER.source_snapshot(self.root)
        self.assertNotEqual(before["sha256"], after["sha256"])
        self.assertNotEqual(before["files_sha256"], after["files_sha256"])
        self.assertNotEqual(before["dependency_links"], after["dependency_links"])
        target.write_bytes(original)
        self.assertEqual(before, QUALIFIER.source_snapshot(self.root))
        self.lock_path.write_text(self.lock_path.read_text() + "\n")
        after = QUALIFIER.source_snapshot(self.root)
        self.assertNotEqual(before["sha256"], after["sha256"])
        self.assertNotEqual(before["dependency_links"], after["dependency_links"])

    def test_equal_byte_locked_retargeting_changes_link_identity_digest(self):
        original = self.modules / "example/bin/run.js"
        alternate = self.modules / "replacement/bin/run.js"
        alternate.parent.mkdir(parents=True)
        alternate.write_bytes(original.read_bytes())
        self.lock["packages"]["node_modules/replacement"] = {
            "version": "7.8.9",
            "bin": {"example": "bin/run.js"},
        }
        self.save_lock()
        before = QUALIFIER.source_snapshot(self.root)
        self.replace_link("../replacement/bin/run.js")
        after = QUALIFIER.source_snapshot(self.root)
        name = "tests/clients/node_modules/.bin/example"
        self.assertEqual(before["files"], after["files"])
        self.assertEqual(before["files_sha256"], after["files_sha256"])
        self.assertEqual(
            before["dependency_links"][name]["target_sha256"],
            after["dependency_links"][name]["target_sha256"],
        )
        self.assertNotEqual(
            before["dependency_links"][name]["target"],
            after["dependency_links"][name]["target"],
        )
        self.assertNotEqual(before["sha256"], after["sha256"])
        self.lock["packages"]["node_modules/replacement"]["bin"]["example"] = (
            "bin/other.js"
        )
        self.save_lock()
        with self.assertRaisesRegex(ValueError, "locked bin alias"):
            QUALIFIER.source_snapshot(self.root)

    def test_missing_directory_and_chained_targets_are_rejected(self):
        target = self.modules / "example/bin/run.js"
        original = target.read_bytes()
        for kind in ("missing", "directory", "symlink"):
            with self.subTest(kind=kind):
                target.unlink()
                if kind == "directory":
                    target.mkdir()
                elif kind == "symlink":
                    target.symlink_to(self.modules / "@scope/tool/cli.js")
                with self.assertRaises(ValueError):
                    QUALIFIER.source_snapshot(self.root)
                if target.is_symlink():
                    target.unlink()
                elif target.is_dir():
                    target.rmdir()
                target.write_bytes(original)

    def test_linked_package_alias_directory_and_lockfile_are_rejected(self):
        for path in (self.modules / "example", self.bin, self.lock_path):
            original = path.with_name(path.name + ".actual")
            with self.subTest(path=path):
                path.rename(original)
                path.symlink_to(original, target_is_directory=original.is_dir())
                with self.assertRaises(ValueError):
                    QUALIFIER.source_snapshot(self.root)
                path.unlink()
                original.rename(path)
        parent = self.root / "alias-parent"
        parent.symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "linked dependency alias parent"):
            QUALIFIER.npm_bin_link_snapshot(
                parent, "tests/clients/node_modules/.bin/example"
            )

    def test_other_source_and_dependency_links_remain_forbidden(self):
        target = self.modules / "example/bin/run.js"
        for relative in (
            "tests/test_rust_link.py",
            "tests/clients/node_modules/example/extra.js",
            "scripts/source.py",
        ):
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.symlink_to(target)
            with (
                self.subTest(relative=relative),
                self.assertRaisesRegex(ValueError, "source contains a symlink"),
            ):
                QUALIFIER.source_snapshot(self.root)
            path.unlink()

    def test_unrecorded_regular_dependency_payload_is_never_omitted(self):
        before = QUALIFIER.source_snapshot(self.root)
        extra = self.modules / "example/unrecorded.js"
        extra.write_bytes(b"// new payload\n")
        after = QUALIFIER.source_snapshot(self.root)
        self.assertIn(str(extra.relative_to(self.root)), after["files"])
        self.assertNotEqual(before["files_sha256"], after["files_sha256"])
        self.assertNotEqual(before["sha256"], after["sha256"])


class NativeMutationReceiptTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.reports = [self.root / name for name in ("one.json", "two.out")]
        self.unrequested = self.root / "unrequested.json"
        self.environment = os.environ.copy()
        self.environment.pop("PYTHONOPTIMIZE", None)

    def seed(self):
        for path in [*self.reports, self.unrequested]:
            path.write_text("old success")
            path.with_suffix(".attempt.json").write_text("old attempt")

    def command(self, arguments, *, flags=(), environment=None):
        return subprocess.run(
            [sys.executable, "-I", "-B", *flags, str(SCRIPT), *arguments],
            env=self.environment if environment is None else environment,
            cwd=self.root,
            text=True,
            capture_output=True,
            timeout=10,
        )

    def explicit(self):
        return ["--report", str(self.reports[0]), "--report=" + str(self.reports[1])]

    def assert_removed(self, removed):
        for report in [*self.reports, self.unrequested]:
            for path in (report, report.with_suffix(".attempt.json")):
                if report in removed:
                    self.assertFalse(path.exists(), path)
                else:
                    self.assertEqual(
                        path.read_text(),
                        "old success" if path == report else "old attempt",
                    )

    def test_cleanup_is_the_canonical_shared_scanner(self):
        def function(path):
            return ast.dump(
                next(
                    n
                    for n in ast.parse(path.read_text()).body
                    if isinstance(n, ast.FunctionDef) and n.name == "clear_stale_report"
                )
            )

        self.assertEqual(
            function(SCRIPT), function(ROOT / "scripts/native_facade_qualification.py")
        )

    def test_malformed_and_optimized_preflight_clear_every_explicit_receipt(self):
        for flags in ((), ("-O",), ("-OO",)):
            for trailing in (["--report"], ["--unknown"], ["--report", "--binary"]):
                with self.subTest(flags=flags, trailing=trailing):
                    self.seed()
                    run = self.command([*self.explicit(), *trailing], flags=flags)
                    self.assertNotEqual(run.returncode, 0)
                    self.assert_removed(self.reports)
        for optimize in ("0", "1", "2"):
            self.seed()
            run = self.command(
                self.explicit(),
                environment=self.environment | {"PYTHONOPTIMIZE": optimize},
            )
            self.assertNotEqual(run.returncode, 0)
            self.assertIn("unoptimized", run.stderr)
            self.assert_removed(self.reports)

    def test_abbreviated_and_post_separator_paths_never_opt_in(self):
        for args in (
            ["--rep", str(self.reports[0])],
            ["--rep=" + str(self.reports[0])],
            ["--", *self.explicit()],
        ):
            with self.subTest(args=args):
                self.seed()
                run = self.command(["--binary", "absent", *args])
                self.assertNotEqual(run.returncode, 0)
                self.assert_removed([])

    def test_cleanup_finishes_other_destinations_on_directory_failure(self):
        for suffix in (None, ".attempt.json"):
            self.seed()
            blocked = (
                self.reports[0]
                if suffix is None
                else self.reports[0].with_suffix(suffix)
            )
            blocked.unlink()
            blocked.mkdir()
            (blocked / "keep").write_text("not a receipt")
            try:
                run = self.command([*self.explicit(), "--report"])
                self.assertNotEqual(run.returncode, 0)
                # unlink(2) on a directory is EISDIR on Linux but EPERM on macOS.
                self.assertIn(
                    "PermissionError"
                    if sys.platform == "darwin"
                    else "IsADirectoryError",
                    run.stderr,
                )
                self.assertEqual((blocked / "keep").read_text(), "not a receipt")
                for report in self.reports:
                    for target in (report, report.with_suffix(".attempt.json")):
                        if target != blocked:
                            self.assertFalse(target.exists(), target)
            finally:
                (blocked / "keep").unlink()
                blocked.rmdir()

    def test_library_import_does_not_clean_callers_receipts(self):
        self.seed()
        with patch.object(sys, "argv", ["caller", *self.explicit()]):
            module = importlib.util.module_from_spec(SPEC)
            SPEC.loader.exec_module(module)
        self.assert_removed([])

    def test_failed_reference_preflight_records_zero_scenarios_only(self):
        for deferred in (False, True):
            with self.subTest(deferred=deferred):
                self.seed()
                run = self.command(
                    [
                        "--binary",
                        "absent",
                        "--reference-root",
                        "absent-reference",
                        *(["--defer-installed-asgi"] if deferred else []),
                        *self.explicit(),
                    ]
                )
                self.assertNotEqual(run.returncode, 0)
                self.assertFalse(self.reports[0].exists())
                receipt = json.loads(self.reports[1].read_text())
                self.assertEqual(
                    receipt,
                    {
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
                        if deferred
                        else [],
                    },
                )
                self.assertFalse(self.reports[1].with_suffix(".attempt.json").exists())
