# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Regression gates for oracle provenance and incomplete migration evidence."""

import argparse
import copy
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from scripts.migration_baseline import REFERENCE, UNIT_RUNNER, run_check
from scripts.migration_inventory import cli_inventory
from scripts.migration_scope import initialize, validate

ROOT = Path(__file__).resolve().parents[1]


class MigrationInventoryTest(unittest.TestCase):
    def test_parser_retains_aliases_parent_flags_and_required_exclusions(self):
        parser = argparse.ArgumentParser()
        parser.add_argument("--profile", default="default")
        child = parser.add_subparsers(dest="command", required=True).add_parser(
            "inspect", aliases=["show"]
        )
        group = child.add_mutually_exclusive_group(required=True)
        group.add_argument("--all", action="store_true")
        group.add_argument("--id", type=int)
        rows = cli_inventory(parser)
        self.assertEqual(
            [row["id"] for row in rows], ["cli:<root>", "cli:inspect", "cli:show"]
        )
        self.assertEqual(rows[0]["arguments"][-1]["choices"], ["inspect", "show"])
        self.assertEqual(rows[0]["arguments"][1]["default"], "default")
        self.assertEqual(
            rows[1]["mutually_exclusive_groups"],
            [{"required": True, "members": ["all", "id"]}],
        )
        self.assertEqual(rows[1]["arguments"][-1]["type"], "builtins.int")

    def test_only_home_prefix_in_default_paths_is_symbolic(self):
        parser = argparse.ArgumentParser()
        parser.add_argument(
            "--credentials", default="/example/user/.config/credentials"
        )
        parser.add_argument("--other", default="/example/user-other/file")
        parser.add_argument("--literal", default="literal /example/user remains")
        actions = cli_inventory(parser, user_home="/example/user")[0]["arguments"]
        self.assertEqual(actions[1]["default"], "${HOME}/.config/credentials")
        self.assertEqual(actions[1]["default_encoding"], "home-relative-path")
        self.assertEqual(actions[2]["default"], "/example/user-other/file")
        self.assertEqual(actions[3]["default"], "literal /example/user remains")

    def test_frozen_inventory_matches_review_ledger_and_plan_pin(self):
        raw = (ROOT / "tests/parity/inventory.json").read_bytes()
        inventory = json.loads(raw)
        ledger = json.loads((ROOT / "tests/parity/ledger.json").read_text())
        contracts_raw = (ROOT / "tests/parity/contracts.json").read_bytes()
        validate(
            ledger,
            inventory,
            hashlib.sha256(raw).hexdigest(),
            json.loads(contracts_raw),
            hashlib.sha256(contracts_raw).hexdigest(),
        )
        self.assertEqual(ledger["reference_commit"], REFERENCE)
        reference = json.loads((ROOT / "tests/parity/reference.json").read_text())
        self.assertEqual(reference["reference_commit"], REFERENCE)
        self.assertEqual(reference["inventory_sha256"], hashlib.sha256(raw).hexdigest())
        for name in ("fixtures.json", "platforms.json", "known-defects.json"):
            document = json.loads((ROOT / "tests/parity" / name).read_text())
            self.assertEqual(document["reference_commit"], REFERENCE)
        self.assertIn(REFERENCE, (ROOT / "docs/RUST_MIGRATION_PLAN.md").read_text())
        self.assertEqual(inventory["schema_version"], 32)
        self.assertEqual(inventory["http_version"], "1.7.0")
        self.assertEqual(len(inventory["migrations"]), 32)
        self.assertIn("tests/test_migrations.py", inventory["resource_sha256"])

    def test_inventory_refuses_preimported_candidate(self):
        proc = subprocess.run(
            [
                sys.executable,
                "-c",
                "import sys, types; from pathlib import Path; "
                "from scripts.migration_inventory import inventory; "
                "sys.modules['catabolic'] = types.ModuleType('catabolic'); "
                "inventory(Path('.'))",
            ],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("requires a fresh process", proc.stderr)


class MigrationScopeTest(unittest.TestCase):
    def setUp(self):
        self.inventory = {"cli": [{"id": "cli:scan"}], "http": [], "migrations": []}
        self.ledger = initialize(self.inventory, "hash", REFERENCE)

    def test_missing_duplicate_or_extra_surface_is_rejected(self):
        for rows in (
            [],
            self.ledger["rows"] * 2,
            self.ledger["rows"] + [dict(self.ledger["rows"][0], id="cli:files")],
        ):
            with self.subTest(rows=rows):
                ledger = dict(self.ledger, rows=rows)
                with self.assertRaises(ValueError):
                    validate(ledger, self.inventory, "hash")

    def test_inventory_change_requires_explicit_review(self):
        with self.assertRaisesRegex(ValueError, "different inventory"):
            validate(self.ledger, self.inventory, "changed")

    def test_qualification_requires_behavior_and_executable_evidence(self):
        ledger = copy.deepcopy(self.ledger)
        ledger["rows"][0]["status"] = "qualified"
        with self.assertRaisesRegex(ValueError, "lacks mutations"):
            validate(ledger, self.inventory, "hash")
        ledger["rows"][0].update(
            mutations="read-only",
            failure_behavior="refuse",
            reference_tests=["tests.reference"],
            proof_plan="compare",
        )
        with self.assertRaisesRegex(ValueError, "Rust/differential"):
            validate(ledger, self.inventory, "hash")

    def test_enumeration_cannot_claim_m0_complete(self):
        self.ledger["milestone_status"] = "complete"
        with self.assertRaisesRegex(ValueError, "separate full acceptance"):
            validate(self.ledger, self.inventory, "hash")


class BaselineRunnerTest(unittest.TestCase):
    def test_failure_and_timeout_are_preserved_with_logs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            failed = run_check(
                "failed",
                [sys.executable, "-c", "print('evidence'); raise SystemExit(7)"],
                root,
                root,
                os.environ.copy(),
                5,
            )
            self.assertEqual((failed["status"], failed["exit_code"]), ("failed", 7))
            self.assertIn("evidence", (root / failed["log"]).read_text())
            limited = run_check(
                "limited",
                [sys.executable, "-c", "import time; time.sleep(60)"],
                root,
                root,
                os.environ.copy(),
                0.1,
            )
            self.assertEqual(limited["status"], "budget-limited")
            self.assertIsNone(limited["exit_code"])

    def test_successful_unittest_with_skips_is_not_an_unqualified_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tests").mkdir()
            (root / "tests/test_example.py").write_text(
                "import unittest\nclass Example(unittest.TestCase):\n"
                "    def test_pass(self): pass\n"
                "    @unittest.skip('requires unavailable platform')\n"
                "    def test_skip(self): pass\n"
            )
            result = run_check(
                "unit",
                [sys.executable, "-c", UNIT_RUNNER, str(root / "unit-result.json")],
                root,
                root,
                os.environ.copy(),
                5,
            )
            self.assertEqual(result["status"], "passed-with-skips")
            self.assertEqual(result["test_result"]["tests_run"], 2)
            self.assertTrue(result["test_result"]["complete"])
            self.assertEqual(
                result["test_result"]["skipped"][0]["reason"],
                "requires unavailable platform",
            )

    def test_zero_exit_without_final_unit_receipt_is_incomplete(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = run_check(
                "unit",
                [sys.executable, "-c", "import os; os._exit(0)"],
                root,
                root,
                os.environ.copy(),
                5,
            )
            self.assertEqual(result["exit_code"], 0)
            self.assertEqual(result["status"], "incomplete")

    def test_existing_capture_cannot_be_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "keep"
            marker.write_text("previous evidence")
            proc = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "scripts/migration_baseline.py"),
                    "--output",
                    directory,
                ],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(proc.returncode, 0)
            self.assertEqual(marker.read_text(), "previous evidence")
            self.assertEqual(list(Path(directory).iterdir()), [marker])

    def test_crashed_suite_retains_partial_results_without_claiming_completion(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tests").mkdir()
            (root / "tests/test_example.py").write_text(
                "import os, unittest\nclass Example(unittest.TestCase):\n"
                "    def test_a_pass(self): pass\n"
                "    def test_b_crash(self): os._exit(2)\n"
            )
            result = run_check(
                "unit",
                [sys.executable, "-c", UNIT_RUNNER, str(root / "unit-result.json")],
                root,
                root,
                os.environ.copy(),
                5,
            )
            self.assertEqual(result["status"], "failed")
            self.assertEqual(result["exit_code"], 2)
            self.assertEqual(result["test_result"]["tests_run"], 1)
            self.assertFalse(result["test_result"]["complete"])


if __name__ == "__main__":
    unittest.main()
