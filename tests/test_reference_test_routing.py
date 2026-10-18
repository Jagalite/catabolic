# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Reference membership and required, non-skippable installed prerequisite routing."""

import contextlib
import io
import json
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts import run_native_prerequisite_tests as native
from scripts.run_reference_tests import (
    NATIVE_ORACLE_FILES,
    ReferenceLoader,
    identifiers,
)

ROOT = Path(__file__).resolve().parents[1]


class ReferenceRoutingTest(unittest.TestCase):
    def test_only_the_fixed_four_prerequisites_leave_reference_discovery(self):
        class PreviousLoader(ReferenceLoader):
            loadTestsFromTestCase = unittest.TestLoader.loadTestsFromTestCase

        before = PreviousLoader()
        after = ReferenceLoader()
        args = {"start_dir": str(ROOT / "tests"), "top_level_dir": str(ROOT)}
        old_ids = set(identifiers(before.discover(**args)))
        new_ids = list(identifiers(after.discover(**args)))
        self.assertEqual(
            before.errors, [], "prerequisite comparison requires complete discovery"
        )
        self.assertEqual(
            after.errors, [], "reference discovery contains import failures"
        )
        self.assertEqual(old_ids - set(new_ids), set(native.REQUIRED_IDS))
        self.assertEqual(set(new_ids) - old_ids, set())
        self.assertEqual(len(new_ids), len(set(new_ids)))
        self.assertEqual(
            set(after.native_prerequisite_scenarios), set(native.REQUIRED_IDS)
        )
        self.assertEqual(before.native_files, after.native_files)

    def test_every_frozen_application_test_module_remains_in_reference_lane(self):
        inventory = json.loads((ROOT / "tests/parity/inventory.json").read_text())
        originals = [
            name
            for name in inventory["resource_sha256"]
            if name.startswith("tests/test_") and name.endswith(".py")
        ]
        self.assertTrue(originals)
        deferred_modules = {name.rsplit(".", 1)[0] for name in native.REQUIRED_CASES}
        loader = ReferenceLoader()
        for name in originals:
            with self.subTest(module=name):
                self.assertTrue(
                    loader._match_path(Path(name).name, str(ROOT / name), "test*.py")
                )
                self.assertNotIn(name[:-3].replace("/", "."), deferred_modules)

    def test_new_native_oracle_modules_move_exactly_seventeen_scenarios(self):
        class PreviousLoader(ReferenceLoader):
            def _match_path(self, path, full_path, pattern):
                if Path(path).name.startswith("test_rust_"):
                    self.native_files.append(path)
                    return False
                return unittest.TestLoader._match_path(self, path, full_path, pattern)

        before = PreviousLoader()
        after = ReferenceLoader()
        args = {"start_dir": str(ROOT / "tests"), "top_level_dir": str(ROOT)}
        old_ids = set(identifiers(before.discover(**args)))
        new_ids = set(identifiers(after.discover(**args)))
        self.assertEqual(before.errors + after.errors, [])
        moved = old_ids - new_ids
        self.assertEqual(len(moved), 17)
        self.assertEqual(new_ids - old_ids, set())
        self.assertEqual(
            {test.split(".")[1] + ".py" for test in moved}, set(NATIVE_ORACLE_FILES)
        )
        gate = (ROOT / "scripts/check-native-mutations.sh").read_text()
        for filename in NATIVE_ORACLE_FILES:
            self.assertIn("tests." + filename[:-3], gate)

    def test_native_prerequisite_discovery_is_exact_and_nonempty(self):
        suite, _ = native.required_suite()
        self.assertEqual(sorted(identifiers(suite)), sorted(native.REQUIRED_IDS))
        self.assertEqual(len(native.REQUIRED_IDS), 4)

    def test_inventory_drift_is_not_silently_deferred(self):
        suite, expected = native.required_suite()
        case = next(iter(expected))
        with patch.object(
            case, "test_unlisted_fixture", lambda self: None, create=True
        ):
            with self.assertRaisesRegex(RuntimeError, "inventory drift"):
                ReferenceLoader().loadTestsFromTestCase(case)

    def test_source_manifest_detects_driver_or_test_drift(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in native.SOURCE_FILES:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"disposable source fixture")
            before = native.source_manifest(root)
            name = "tests/test_native_content_revision_contract.py"
            (root / name).write_bytes(b"changed source fixture")
            after = native.source_manifest(root)
            self.assertEqual(
                [key for key in before if before[key] != after[key]], [name]
            )

    def test_required_runner_refuses_incomplete_results_and_stale_receipts(self):
        for behavior in (
            "pass",
            "skip",
            "failure",
            "error",
            "expected_failure",
            "unexpected_success",
            "missing",
            "empty",
            "foreign",
            "duplicate",
            "unavailable",
            "artifact_changed",
            "wrong_oracle",
            "oracle_changed",
            "wrong_runtime",
            "source_changed",
        ):
            with (
                self.subTest(behavior=behavior),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                wheel = root / "fixture.whl"
                wheel.write_bytes(b"disposable archive")
                report = root / "report.json"
                report.write_text('{"passed": true, "stale": true}')

                def first(case, behavior=behavior):
                    if behavior == "skip":
                        case.skipTest("fixture skip")
                    elif behavior == "failure":
                        case.fail("fixture failure")
                    elif behavior == "error":
                        raise ValueError("fixture error")

                if behavior == "expected_failure":
                    first = unittest.expectedFailure(lambda case: case.fail("fixture"))
                elif behavior == "unexpected_success":
                    first = unittest.expectedFailure(lambda case: None)
                case = type(
                    "Fixture",
                    (unittest.TestCase,),
                    {
                        "test_one": first,
                        "test_two": lambda self: None,
                        "test_three": lambda self: None,
                        "test_four": lambda self: None,
                    },
                )
                methods = ("test_one", "test_two", "test_three", "test_four")
                cases = [case(name) for name in methods]
                if behavior == "missing":
                    cases.pop()
                elif behavior == "empty":
                    cases.clear()
                elif behavior == "duplicate":
                    cases[-1] = case(methods[0])
                verification = types.ModuleType("scripts.qualify_native_wheel")
                verification.verify_installed_wheel = lambda path: {
                    "fixture": "unchanged"
                }
                if behavior == "artifact_changed":
                    values = iter([{"fixture": "before"}, {"fixture": "after"}])
                    verification.verify_installed_wheel = lambda path, values=values: (
                        next(values)
                    )
                args = [
                    "runner",
                    "--wheel",
                    str(wheel),
                    "--reference-root",
                    str(root),
                    "--reference-python",
                    sys.executable,
                    "--report",
                    str(report),
                ]
                with contextlib.ExitStack() as stack:
                    stack.enter_context(patch.object(sys, "argv", args))
                    stack.enter_context(patch.object(sys, "path", sys.path.copy()))
                    if behavior != "foreign":
                        stack.enter_context(
                            patch.object(
                                native,
                                "REQUIRED_IDS",
                                tuple(case(name).id() for name in methods),
                            )
                        )
                    stack.enter_context(patch.dict("os.environ"))
                    stack.enter_context(
                        patch.dict(
                            sys.modules, {"scripts.qualify_native_wheel": verification}
                        )
                    )
                    provenance = stack.enter_context(
                        patch(
                            "scripts.qualify_native_embedding.reference_provenance",
                            return_value={"head": "fixture frozen commit"},
                        )
                    )
                    runtime = stack.enter_context(
                        patch(
                            "scripts.native_facade_qualification.reference_runtime",
                            return_value={"version": [3, 14, 6]},
                        )
                    )
                    sources = stack.enter_context(
                        patch.object(
                            native, "source_manifest", return_value={"fixture": "same"}
                        )
                    )
                    if behavior == "wrong_oracle":
                        provenance.side_effect = AssertionError("wrong frozen oracle")
                    elif behavior == "oracle_changed":
                        provenance.side_effect = [{"head": "before"}, {"head": "after"}]
                    elif behavior == "wrong_runtime":
                        runtime.side_effect = AssertionError("wrong oracle runtime")
                    elif behavior == "source_changed":
                        sources.side_effect = [
                            {"fixture": "before"},
                            {"fixture": "after"},
                        ]
                    stack.enter_context(
                        patch.object(
                            native,
                            "required_suite",
                            side_effect=FileNotFoundError("missing prerequisite")
                            if behavior == "unavailable"
                            else None,
                            return_value=(unittest.TestSuite(cases), {case: methods}),
                        )
                    )
                    stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                    stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
                    status = native.main()
                saved = json.loads(report.read_text())
                self.assertEqual(status, 0 if behavior == "pass" else 1)
                self.assertEqual(saved["passed"], behavior == "pass")
                self.assertNotIn("stale", saved)
                if behavior == "pass":
                    self.assertEqual(saved["tests_run"], 4)
                    self.assertEqual(len(saved["successful_scenarios"]), 4)
