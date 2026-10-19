#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Validate redesign traceability without converting plans into qualification."""

import hashlib
import json
from pathlib import Path

if not __debug__:
    raise RuntimeError("catalog contract audit requires unoptimized Python")


def validate(root: Path) -> int:
    ledger = json.loads(
        (root / "tests/parity/native-catalog-contract-ledger.json").read_text()
    )
    frozen = json.loads((root / ledger["frozen_ledger"]).read_text())
    assert ledger["format_version"] == 2
    assert (
        hashlib.sha256((root / ledger["frozen_ledger"]).read_bytes()).hexdigest()
        == ledger["frozen_ledger_sha256"]
    ), "frozen compatibility ledger changed"
    assert ledger["required_milestones"] == [f"M{number}" for number in range(12)]
    assert (root / ledger["milestone_requirements_source"]).is_file()
    assert ledger["obligations_waived"] == []
    assert ledger["blocked_independent_reviews"] == [
        "schema",
        "broad_definition",
        "filesystem",
    ]
    assert ledger["review_retry_authorized"] is False
    assert ledger["default_transition"] == {
        "python_default": "preserved",
        "native_surface": "opt_in",
        "release_ready": False,
    }
    slots = ledger["evidence_slots"]
    assert slots, "required proof slots are missing"
    for name, slot in slots.items():
        # These slots are plans. Actual bounded results belong in separate immutable
        # receipts, never a changed status or a test count attached to a command.
        assert slot["status"] == "pending", name
        assert slot["evidence_revision"] is None, name
        assert slot["commands_are_qualification_results"] is False, name
        assert (
            slot["scope"] and slot["required_artifacts"] and slot["required_scenarios"]
        ), name
        assert isinstance(slot["planned_commands"], list), name
        assert all(
            isinstance(command, str) and command for command in slot["planned_commands"]
        ), name
        assert (
            not {"passed", "passing_tests", "qualified", "release_qualified"}
            & slot.keys()
        ), name
    frozen_ids = {row["id"] for row in frozen["rows"]}
    seen = set()
    for row in ledger["rows"]:
        assert row["id"] not in seen, f"duplicate contract: {row['id']}"
        seen.add(row["id"])
        assert row["disposition"] in {"preserve", "adapt", "versioned_change", "defer"}
        assert row["replacement_contract"] and row["migration_rule"], row["id"]
        assert row["required_evidence_slots"], row["id"]
        assert set(row["required_evidence_slots"]) <= slots.keys(), row["id"]
        assert set(row["mapped_frozen_rows"]) <= frozen_ids, row["id"]
        assert row["status"] in {
            "pending",
            "historical_bounded_evidence",
            "deferred",
        }, row["id"]
        if row["disposition"] == "defer":
            assert row["status"] == "deferred", row["id"]
        if row["status"] == "historical_bounded_evidence":
            assert row["evidence_revision"], row["id"]
            assert row["evidence_scope"], row["id"]
        for path in row["fixtures"]:
            assert (root / path).is_file(), path
        if row["evidence_revision"]:
            assert (root / row["evidence_revision"]).is_file(), row["id"]
    # Gate closure needs a separate reviewed evidence schema, never this planning ledger.
    assert ledger["milestone_closures"] == []
    assert (
        ledger["sequencing_amendment"]["external_motion_integration"]
        == "deferred_by_user"
    )
    assert ledger["sequencing_amendment"]["motion_changes_or_tests_authorized"] is False
    return len(seen)


if __name__ == "__main__":
    count = validate(Path(__file__).resolve().parents[1])
    print(
        f"{count} catalog compatibility dispositions valid; no milestone gates closed"
    )
