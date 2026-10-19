# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Source-only ownership ledger checks; no runtime qualification is implied."""

import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "legacy_cli_owner_generator", ROOT / "scripts/legacy_cli_owners.py"
)
GENERATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GENERATOR)


class NativeCliOwnerLedger(unittest.TestCase):
    def test_external_rule_rows_point_to_native_owners_and_retain_gaps(self):
        inventory = json.loads((ROOT / "tests/parity/inventory.json").read_text())
        rows = {
            row["command"]: row for row in GENERATOR.build_ledger(inventory)["rows"]
        }
        for command, owner in (
            ("operation put", "catabolic_runtime::operations::put"),
            ("rule run", "catabolic_runtime::rules::{dispatch,run_external}"),
        ):
            with self.subTest(command=command):
                self.assertEqual(rows[command]["status"], "partial")
                self.assertTrue(rows[command]["native_owner"].startswith(owner))
                self.assertEqual(rows[command]["owner_mapping"], "entrypoint")
                self.assertIn("platform", rows[command]["gap"])
                self.assertIn(
                    "RUST_MIGRATION_EXTERNAL_RULE.md", rows[command]["qualification"]
                )

    def test_fallback_worker_records_existing_tick_and_retained_gate(self):
        inventory = json.loads((ROOT / "tests/parity/inventory.json").read_text())
        ledger = GENERATOR.build_ledger(inventory)
        row = next(row for row in ledger["rows"] if row["command"] == "fallback worker")
        self.assertEqual(row["status"], "partial")
        self.assertEqual(row["owner_mapping"], "entrypoint")
        self.assertEqual(
            row["native_owner"],
            "catabolic_cli::legacy::fallback_worker::run -> "
            "catabolic_runtime::public_fallback::run -> public_fallback::tick",
        )
        self.assertIn("M6 refusals", row["gap"])
        self.assertIn(
            "selected standalone SIGTERM exits by signal after native cleanup",
            row["qualification"],
        )
        self.assertIn("due/publication refusal remains explicit", row["qualification"])
        self.assertIn("RUST_MIGRATION_LEGACY_SIGTERM.md", row["qualification"])

    def test_checked_ledger_matches_generator(self):
        path = ROOT / "tests/parity/legacy-cli-owners.json"
        before = path.read_bytes()
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(GENERATOR.main(["--check"]), 0)
        self.assertEqual(path.read_bytes(), before)

    def test_new_adapters_have_exact_owners_without_qualification(self):
        inventory = json.loads((ROOT / "tests/parity/inventory.json").read_text())
        ledger = GENERATOR.build_ledger(inventory)
        rows = {row["command"]: row for row in ledger["rows"]}
        self.assertEqual(len(rows), 242)
        self.assertTrue(ledger["python_remains_default"])
        self.assertFalse(ledger["full_surface_parity"])
        for command, owner in {
            "api worker": "catabolic_cli::legacy::api_worker::run -> catabolic_runtime::http_worker::run",
            "catalog-refresh watch": "catabolic_cli::legacy::catalog_refresh::execute -> catabolic_runtime::catalog_refresh::execute",
        }.items():
            with self.subTest(command=command):
                self.assertEqual(rows[command]["status"], "adapted_unqualified")
                self.assertEqual(rows[command]["native_owner"], owner)
                self.assertEqual(rows[command]["owner_mapping"], "entrypoint")
                self.assertNotIn("gap", rows[command])
        ordinary = {
            "principal-put",
            "grant-put",
            "token-revoke",
            "tokens",
            "principal-disable",
            "grant-disable",
            "operation-disable",
            "source-expose",
            "operation-approve",
        }
        for operation in ordinary:
            row = rows["api " + operation]
            self.assertEqual(row["status"], "adapted_unqualified")
            self.assertEqual(row["owner_mapping"], "entrypoint")
            self.assertEqual(
                row["native_owner"],
                "catabolic_cli::legacy::admin_cli::execute_detailed -> "
                "catabolic_store::admin_cli::execute_detailed(" + operation + ")",
            )
            self.assertIn("remain unqualified", row["qualification"])
        for operation, owner in {
            "mapping-events": "catabolic_cli::legacy::mapping_events::execute -> catabolic_store::mapping_history::events_with_subject",
            "mapping-recover": "catabolic_cli::legacy::projection::history -> catabolic_runtime::destination_mappings::recover",
        }.items():
            row = rows["projection " + operation]
            self.assertEqual(row["status"], "partial")
            self.assertEqual(row["owner_mapping"], "entrypoint")
            self.assertEqual(
                row["native_owner"],
                owner,
            )
            self.assertTrue(row["gap"])
        self.assertEqual(rows["api serve"]["status"], "safety_gated")
        self.assertEqual(rows["fallback execute"]["status"], "partial")
        self.assertEqual(rows["recover"]["status"], "partial")

    def test_credential_routes_are_implemented_partial_with_explicit_gaps(self):
        inventory = json.loads((ROOT / "tests/parity/inventory.json").read_text())
        ledger = GENERATOR.build_ledger(inventory)
        rows = {row["command"]: row for row in ledger["rows"]}
        dispatch = (ROOT / "crates/catabolic-cli/src/legacy/dispatch.rs").read_text()
        owner = (ROOT / "crates/catabolic-store/src/admin_cli.rs").read_text()
        for operation in ("token-create", "token-rotate"):
            with self.subTest(operation=operation):
                row = rows["api " + operation]
                self.assertEqual(row["status"], "partial")
                self.assertEqual(row["owner_mapping"], "entrypoint")
                self.assertEqual(
                    row["native_owner"],
                    "catabolic_cli::legacy::admin_cli::execute_detailed -> "
                    "catabolic_store::admin_cli::execute_detailed(" + operation + ")",
                )
                self.assertIn(operation, GENERATOR.ADAPTED["api"].split())
                self.assertIn('"' + operation + '"', dispatch)
                self.assertIn('"' + operation + '" => {', owner)
                for limitation in (
                    "named ~user",
                    "HOME-unset",
                    "exception envelopes",
                    "remain unqualified",
                ):
                    self.assertIn(limitation, row["gap"])
                self.assertIn(
                    "private_secret exclusive-file owner are implemented", row["gap"]
                )
                self.assertIn(
                    "strict installed-artifact gate passed on both pinned Pythons",
                    row["qualification"],
                )
                self.assertIn(
                    "no full-leaf behavioral parity claim", row["qualification"]
                )
                self.assertNotIn("missing_adapter", json.dumps(row))
        self.assertIn("access_admin::issue_detailed(", owner)
        self.assertIn("crate::private_secret::save(", owner)
        self.assertTrue(ledger["python_remains_default"])
        self.assertFalse(ledger["full_surface_parity"])
        self.assertEqual(len(rows), 242)

    def test_check_refuses_stale_or_missing_without_writing(self):
        inventory = (ROOT / "tests/parity/inventory.json").read_bytes()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            parity = root / "tests/parity"
            parity.mkdir(parents=True)
            (parity / "inventory.json").write_bytes(inventory)
            ledger = parity / "legacy-cli-owners.json"
            for content in (None, b'{"rows": []}\n'):
                if content is not None:
                    ledger.write_bytes(content)
                with (
                    self.subTest(content=content),
                    patch.object(GENERATOR, "ROOT", root),
                    contextlib.redirect_stderr(io.StringIO()),
                    self.assertRaises(SystemExit) as raised,
                ):
                    GENERATOR.main(["--check"])
                self.assertEqual(raised.exception.code, 1)
                if content is None:
                    self.assertFalse(ledger.exists())
                else:
                    self.assertEqual(ledger.read_bytes(), content)


if __name__ == "__main__":
    unittest.main()
