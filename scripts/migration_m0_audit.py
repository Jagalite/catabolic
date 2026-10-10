#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Audit M0 planning coverage and report explicit evidence blockers."""

import argparse
import json
import sys
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from scripts.migration_contracts import REFERENCE, sha
from scripts.migration_scope import validate


def scale_status(runner_status, report):
    """The reference scale CLI returns zero even when a worker times out."""
    if runner_status != "passed":
        return runner_status
    rows = report.get("results", [])
    if any(row["status"] == "timeout" for row in rows):
        return "budget-limited"
    if any(row["status"] not in ("ok", "injected_crash", "guarded") for row in rows):
        return "failed"
    database = {
        "seed",
        "indexed_association",
        "metadata_sql",
        "title_search",
        "graphql_nested",
        "graphql_filtered",
        "layout_select_one",
        "layout_full",
        "manifest",
        "manual_mapping",
        "saved_gap_selection",
    }
    physical = {
        "seed",
        "sync",
        "rescan",
        "repeat_sync",
        "change_one_percent",
        "crash",
        "recover",
    }
    expected = {
        (False, count, action)
        for count in report.get("sizes", [])
        for action in database
    }
    expected |= {
        (True, count, action)
        for count in report.get("filesystem_sizes", [])
        for action in physical
    }
    observed = {(row["physical"], row["count"], row["action"]) for row in rows}
    if not expected or not expected.issubset(observed):
        return "incomplete"
    return "guarded" if any(row["status"] == "guarded" for row in rows) else "passed"


def audit(root):
    def read(name):
        return json.loads((root / name).read_text())

    inventory = read("inventory.json")
    contracts = read("contracts.json")
    ledger = read("ledger.json")
    validate(
        ledger,
        inventory,
        sha((root / "inventory.json").read_bytes()),
        contracts,
        sha((root / "contracts.json").read_bytes()),
    )
    issues = []
    for row in ledger["rows"]:
        if not all(
            row.get(key)
            for key in (
                "mutations",
                "failure_behavior",
                "proof_plan",
                "reference_tests",
            )
        ):
            issues.append(f"Missing planning contract: {row['id']}")
    extras = {}
    for name in ("integrations.json", "dispatch-variants.json", "python-api.json"):
        document = read(name)
        if document["reference_commit"] != REFERENCE:
            raise ValueError(f"Different reference: {name}")
        ids = [row["id"] for row in document["rows"]]
        if len(ids) != len(set(ids)):
            raise ValueError(f"Duplicate surface: {name}")
        for row in document["rows"]:
            if not row.get("owner") or not row.get("proof_plan"):
                issues.append(f"Missing owner/proof: {row['id']}")
        extras[name] = len(ids)
    ci = read("ci-evidence.json")
    if ci["headSha"] != REFERENCE:
        raise ValueError("CI evidence belongs to another commit")
    fixtures = read("legacy-fixture-evidence.json")
    if fixtures["reference_commit"] != REFERENCE:
        raise ValueError("Fixture evidence belongs to another commit")
    if {r["schema"] for r in fixtures["fixtures"] if r["status"] == "passed"} != set(
        range(1, 33)
    ):
        issues.append("Missing successful historical schema-boundary capture")
    gates = read("m0-gates.json")
    if gates["reference_commit"] != REFERENCE:
        raise ValueError("Gate register belongs to another commit")
    required = {
        "reference",
        "ledger",
        "python-platforms",
        "fixtures",
        "ci-acceptance",
        "storage",
        "benchmarks",
        "performance-metrics",
        "known-defects",
    }
    ids = [gate["id"] for gate in gates["gates"]]
    if len(ids) != len(set(ids)) or set(ids) != required:
        raise ValueError("Missing, duplicate or unexpected M0 gates")
    for gate in gates["gates"]:
        if gate["status"] in ("captured", "specified") and not gate.get("evidence"):
            issues.append(f"Missing evidence reference: {gate['id']}")
        if gate["status"] not in ("captured", "specified"):
            issues.append(f"{gate['id']}: {gate['status']} ({gate['reason']})")
    return dict(
        reference_commit=REFERENCE,
        milestone="M0",
        complete=not issues,
        main_surfaces=len(ledger["rows"]),
        supplemental_inventories=extras,
        blockers=issues,
        policy="Captured baseline failures remain failures; a complete freeze is not a passing release or Rust parity claim.",
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path("tests/parity"))
    args = parser.parse_args()
    result = audit(args.root)
    print(json.dumps(result, indent=2))
    return 0 if result["complete"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
