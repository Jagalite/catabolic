"""Exact ordinary Watchers/source_events signatures and evidence boundaries."""

import ast
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
FACADE = ROOT / "crates/catabolic-python/python/catabolic_native"
SCRIPT = ROOT / "scripts/native_watchers_embedding_tests.py"


def gate():
    spec = importlib.util.spec_from_file_location("watchers_contracts", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class WatchersFacadeContracts(unittest.TestCase):
    def test_exact_frozen_selected_rows_and_signatures(self):
        module = gate()
        inventory = json.loads((ROOT / "tests/parity/python-api.json").read_text())
        selected = {
            "python:catabolic.watchers.Watchers" + suffix
            for suffix in (
                "",
                ".__init__",
                ".get",
                ".list",
                ".put",
                ".admit",
                ".enable",
                ".require_run",
                ".history",
                ".run",
            )
        } | {"python:catabolic.source_events.invalidate"}
        self.assertEqual(set(module.CONTRACTS), selected)
        self.assertEqual(module.EXPECTED_ROWS, 11)
        rows = [row for row in inventory["rows"] if row["id"] in selected]
        self.assertEqual(len(rows), 11)
        self.assertEqual(len({row["id"] for row in rows}), 11)
        for row in rows:
            path = row["id"].removeprefix("python:catabolic.").split(".")
            tree = ast.parse((FACADE / (path[0] + ".py")).read_text())
            owner = next(
                node for node in tree.body if getattr(node, "name", None) == path[1]
            )
            if row["signature"]["kind"] == "class":
                self.assertIsInstance(owner, ast.ClassDef)
            else:
                function = (
                    next(
                        node
                        for node in owner.body
                        if getattr(node, "name", None) == path[2]
                    )
                    if len(path) == 3
                    else owner
                )
                expected = (
                    ast.parse("def f(" + row["signature"]["arguments"] + "): pass")
                    .body[0]
                    .args
                )
                self.assertEqual(ast.dump(function.args), ast.dump(expected), row["id"])
            self.assertTrue(row["classification"].startswith("preserve"), row["id"])
            self.assertTrue(module.CONTRACTS[row["id"]]["tested"])
            self.assertTrue(module.CONTRACTS[row["id"]]["gap"])

    def test_fixed_complete_installed_cases_and_pairs_without_skips(self):
        module = gate()
        self.assertEqual(len(module.EXPECTED_TESTS), 18)
        self.assertEqual(len(set(module.EXPECTED_TESTS)), 18)
        self.assertEqual(
            set(module.EXPECTED_TESTS),
            {
                name
                for name in vars(module.InstalledWatchers)
                if name.startswith("test_")
            },
        )
        self.assertEqual(len(module.EXPECTED_GROUPS), 32)
        self.assertEqual(len(set(module.EXPECTED_GROUPS)), 32)
        self.assertEqual(
            module.PARTIAL_CASES,
            (
                "observation_run_refusal",
                "fallback_run_refusal",
                "processing_run_refusal",
                "http_run_refusal",
                "projection_run_refusal",
            ),
        )
        self.assertTrue(set(module.PARTIAL_CASES).isdisjoint(module.EXPECTED_GROUPS))
        source = SCRIPT.read_text()
        for unsafe in ("skipTest", "unittest.skip", "SkipTest", "expectedFailure"):
            self.assertNotIn(unsafe, source)
        self.assertIn("expected_count=18", source)
        self.assertIn(
            "qualification.paired_groups(reference, native, EXPECTED_GROUPS, 32)",
            source,
        )
        self.assertIn("non_parity_refusal_evidence=partial_evidence", source)
        self.assertIn("callable coverage is not exhaustive parity", source)

    def test_installed_ci_requires_strict_gate_and_receipts(self):
        from tests.native_ci_contract import require_installed_gate

        workflow = (ROOT / ".github/workflows/native-qualification.yml").read_text()
        require_installed_gate(
            workflow,
            "native_watchers_embedding_tests.py",
            "native-watchers-embedding",
        )

    def test_adapters_forward_to_persistent_native_owner(self):
        for name in ("watchers", "source_events"):
            source = (FACADE / (name + ".py")).read_text()
            for forbidden in (
                "execute_operation",
                "sqlite3",
                "SELECT ",
                "hashlib",
                "subprocess",
                "disposable_fixture",
            ):
                self.assertNotIn(forbidden, source)
            for node in ast.walk(ast.parse(source)):
                if isinstance(node, ast.Import):
                    imports = [alias.name for alias in node.names]
                elif isinstance(node, ast.ImportFrom) and not node.level:
                    imports = [node.module or ""]
                else:
                    imports = []
                self.assertFalse(
                    any(
                        name == "catabolic" or name.startswith("catabolic.")
                        for name in imports
                    )
                )
        watchers = (FACADE / "watchers.py").read_text()
        self.assertIn("app, app.store, app.profile", watchers)
        self.assertIn("self.store._call(", watchers)
        events = (FACADE / "source_events.py").read_text()
        self.assertIn("app.store._call(", events)
        self.assertIn('result["reason"] = reason', events)

    def test_strict_pre_and_post_artifact_oracle_and_source_fences(self):
        source = SCRIPT.read_text()
        for required in (
            "qualification.NativeIdentity(args.source_root)",
            "qualification.strict_artifact(",
            "qualification.reference_runtime(args.reference_python)",
            "qualification.inventory_rows(inventory, CONTRACTS, EXPECTED_ROWS)",
            "assert list(sys.version_info[:3]) == qualification.REFERENCE_VERSION",
            "assert args.reference_commit == qualification.FROZEN_COMMIT",
            "provenance.reference_imports(",
            "installed_signatures(rows)",
            "identity.verify()",
            "source_before == source_manifest()",
            "fence == qualification.file_fence(",
            "runtime == qualification.reference_runtime(args.reference_python)",
            'assert outputs["reference"]["oracle"] == oracle',
        ):
            self.assertIn(required, source)
        self.assertGreaterEqual(source.count("qualification.strict_artifact("), 2)
        self.assertGreaterEqual(source.count("installed_signatures(rows)"), 3)
        self.assertIn('"-I",', source)
        self.assertIn('"-B",', source)
        self.assertIn(
            "installed_calls.get(key, 0) > 0 and paired_calls.get(key, 0) > 0", source
        )
        self.assertIn("development_only=bool(args.dev_wheel)", source)

    def test_optimization_refused_before_import_and_stale_report_cleared(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "report.json"
            attempt = report.with_suffix(".attempt.json")
            for flag in ("-O", "-OO"):
                with self.subTest(flag=flag):
                    report.write_text("stale success")
                    attempt.write_text("stale attempt")
                    result = subprocess.run(
                        [sys.executable, flag, str(SCRIPT), "--report", str(report)],
                        capture_output=True,
                        text=True,
                    )
                    self.assertEqual(result.returncode, 1)
                    self.assertIn("do not run with -O or -OO", result.stderr)
                    self.assertFalse(report.exists())
                    self.assertFalse(attempt.exists())
            report.write_text("stale success")
            attempt.write_text("stale attempt")
            result = subprocess.run(
                [sys.executable, "-I", str(SCRIPT), "--report", str(report)],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(report.exists())
            self.assertFalse(attempt.exists())
            report.write_text("stale success")
            attempt.write_text("stale attempt")
            result = subprocess.run(
                [sys.executable, "-I", str(SCRIPT), "--report", str(report)],
                env={**os.environ, "PYTHONOPTIMIZE": "1"},
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("do not run with -O or -OO", result.stderr)
            self.assertFalse(report.exists())
            self.assertFalse(attempt.exists())

    def test_development_manifest_still_binds_source_wheel_and_extension(self):
        module = gate()
        artifact = {
            "wheel_sha256": "wheel",
            "package_sha256": {"_native.abi3.so": "extension"},
        }
        manifest = dict(
            profile="development",
            source_sha256="source",
            source_sha256_after="source",
            wheel_sha256="wheel",
            extension_sha256="extension",
        )
        helper = types.SimpleNamespace(source_digest=lambda root: "source")
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(module, "sibling", return_value=helper),
        ):
            path = Path(directory) / "build.json"
            path.write_text(json.dumps(manifest))
            self.assertEqual(
                module.build_identity(path, artifact)["source_sha256"], "source"
            )
            with self.assertRaisesRegex(AssertionError, "requires --build-manifest"):
                module.build_identity(None, artifact)
            for key in manifest:
                with self.subTest(key=key):
                    path.write_text(json.dumps({**manifest, key: "stale"}))
                    with self.assertRaisesRegex(
                        AssertionError, "build identity mismatch"
                    ):
                        module.build_identity(path, artifact)

    def test_source_manifest_contains_gate_adapters_and_owners(self):
        manifest = gate().source_manifest()
        for name in (
            "scripts/native_watchers_embedding_tests.py",
            "tests/test_native_watchers_contract.py",
            "scripts/native_facade_qualification.py",
            "scripts/native_qualification_results.py",
            "crates/catabolic-store/src/watcher.rs",
            "crates/catabolic-store/src/observation.rs",
            "crates/catabolic-store/src/public_watchers.rs",
            "crates/catabolic-store/src/lib.rs",
            "crates/catabolic-runtime/src/watchers.rs",
            "crates/catabolic-runtime/tests/public_watchers_facade.rs",
            "crates/catabolic-runtime/src/public_watchers.rs",
            "crates/catabolic-python/src/public_watchers.rs",
            "crates/catabolic-python/src/embedding.rs",
            "crates/catabolic-python/python/catabolic_native/store.py",
            "crates/catabolic-python/python/catabolic_native/watchers.py",
            "crates/catabolic-python/python/catabolic_native/source_events.py",
        ):
            self.assertIn(name, manifest)
            self.assertEqual(len(manifest[name]), 64)

    def test_normalization_validates_identity_clock_and_pid(self):
        import time

        module = gate()
        normalize = module.Normalize(time.time())
        first = "8b359953-ac7d-4a0e-a001-c8d849fdfb48"
        second = "bb68d6df-8fd1-45a9-bd13-7a096c3b753d"
        result = normalize(
            dict(
                definition_id=first,
                result=json.dumps(dict(definition_id=first, run_id=second)),
            )
        )
        self.assertEqual(result["definition_id"], result["result"]["definition_id"])
        self.assertNotEqual(result["definition_id"], result["result"]["run_id"])
        self.assertEqual(
            normalize(dict(completed_at=time.time(), worker_pid=os.getpid())),
            dict(completed_at="<clock>", worker_pid="<pid>"),
        )
        with self.assertRaises(AssertionError):
            normalize(dict(completed_at=1))
        with self.assertRaises(AssertionError):
            normalize(dict(worker_pid=-1))

    def test_unsupported_run_witnesses_do_not_invoke_oracle_effects(self):
        module = gate()
        calls = []
        store = types.SimpleNamespace(
            db=types.SimpleNamespace(execute=lambda *args: calls.append(args))
        )
        watchers = types.SimpleNamespace(
            put=lambda *args: {
                "definition": module.definition(),
                "definition_id": "fixture",
            }
        )
        a = types.SimpleNamespace(encode=json.dumps)
        evidence = module.refusal_evidence(a, store, watchers, "reference")
        self.assertEqual(len(calls), 5)
        self.assertEqual(set(evidence), set(module.PARTIAL_CASES))
        self.assertTrue(
            all(
                "oracle_not_executed" in value and value["persisted_fixture_only"]
                for value in evidence.values()
            )
        )
