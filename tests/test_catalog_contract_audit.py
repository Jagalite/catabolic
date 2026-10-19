# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Planning evidence never replaces old acceptance or new behavioral gates."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from scripts.catalog_contract_audit import validate

ROOT = Path(__file__).resolve().parents[1]
LEDGER = Path("tests/parity/native-catalog-contract-ledger.json")


class CatalogContractAuditTest(unittest.TestCase):
    def fixture(self, root):
        source = json.loads((ROOT / LEDGER).read_text())
        references = {source["milestone_requirements_source"]}
        for row in source["rows"]:
            references.update(row["fixtures"])
            if row["evidence_revision"]:
                references.add(row["evidence_revision"])
        for relative in references:
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture placeholder for audit unit test")
        frozen = root / source["frozen_ledger"]
        frozen.parent.mkdir(parents=True, exist_ok=True)
        frozen.write_bytes((ROOT / source["frozen_ledger"]).read_bytes())
        return source

    def check_mutation(self, change):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = self.fixture(root)
            change(source, root)
            (root / LEDGER).write_text(json.dumps(source))
            with self.assertRaises(AssertionError):
                validate(root)

    def test_current_dispositions_reference_retained_contracts(self):
        self.assertGreaterEqual(validate(ROOT), 22)

    def test_deferred_or_pending_work_cannot_close_a_gate(self):
        for change in ("status", "closure", "reference"):

            def mutate(source, root, change=change):
                if change == "status":
                    source["rows"][-1]["status"] = "qualified"
                elif change == "closure":
                    source["milestone_closures"] = ["M0"]
                else:
                    source["rows"][0]["mapped_frozen_rows"].append("invented:contract")

            with self.subTest(change=change):
                self.check_mutation(mutate)

    def test_planned_commands_cannot_become_passing_evidence(self):
        for field, value in (
            ("status", "passed"),
            ("evidence_revision", "an-unrun-receipt.json"),
            ("commands_are_qualification_results", True),
            ("passing_tests", 100),
            ("release_qualified", True),
        ):
            with self.subTest(field=field):
                self.check_mutation(
                    lambda source, root, field=field, value=value: source[
                        "evidence_slots"
                    ]["r6-native-import"].update({field: value})
                )

    def test_historical_evidence_needs_explicit_narrow_scope(self):
        def mutate(source, root):
            row = next(
                row
                for row in source["rows"]
                if row["status"] == "historical_bounded_evidence"
            )
            row["evidence_scope"] = ""

        self.check_mutation(mutate)

    def test_all_original_milestones_and_zero_waivers_remain_required(self):
        self.check_mutation(
            lambda source, root: source["required_milestones"].remove("M0")
        )
        self.check_mutation(
            lambda source, root: source["obligations_waived"].append("M11")
        )

    def test_frozen_ledger_bytes_cannot_be_rewritten(self):
        def mutate(source, root):
            path = root / source["frozen_ledger"]
            path.write_bytes(path.read_bytes() + b"\n")

        self.check_mutation(mutate)

    def test_blocked_reviews_cannot_be_cleared_or_retried(self):
        self.check_mutation(
            lambda source, root: source["blocked_independent_reviews"].clear()
        )
        self.check_mutation(
            lambda source, root: source.update(review_retry_authorized=True)
        )

    def test_default_cannot_switch_from_a_disposition_update(self):
        self.check_mutation(
            lambda source, root: source["default_transition"].update(
                python_default="retired"
            )
        )
        self.check_mutation(
            lambda source, root: source["default_transition"].update(release_ready=True)
        )

    def test_rows_require_registered_pending_evidence_slots(self):
        self.check_mutation(
            lambda source, root: source["rows"][0].update(required_evidence_slots=[])
        )
        self.check_mutation(
            lambda source, root: source["rows"][0]["required_evidence_slots"].append(
                "invented-proof"
            )
        )

    def test_import_and_companion_migration_have_separate_pending_owners(self):
        source = json.loads((ROOT / LEDGER).read_text())
        rows = {row["id"]: row for row in source["rows"]}
        self.assertIn(
            "r6-native-import", rows["native-import-v1-v2"]["required_evidence_slots"]
        )
        self.assertNotIn("intended_native_schema", rows["native-import-v1-v2"])
        self.assertEqual(
            rows["native-companion-cleanup37-v1"]["intended_native_schema"], 37
        )
        for name in ("r6-native-import", "r5-companion-cleanup37"):
            self.assertEqual(source["evidence_slots"][name]["status"], "pending")
            self.assertIsNone(source["evidence_slots"][name]["evidence_revision"])

    def test_optimized_audit_refuses_instead_of_skipping_assertions(self):
        result = subprocess.run(
            [
                sys.executable,
                "-I",
                "-B",
                "-O",
                str(ROOT / "scripts/catalog_contract_audit.py"),
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires unoptimized Python", result.stderr)


if __name__ == "__main__":
    unittest.main()
