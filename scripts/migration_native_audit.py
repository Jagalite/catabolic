#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Audit native milestone evidence against the exact local source and artifacts."""

import hashlib
import json
from pathlib import Path


def main():
    root = Path(__file__).resolve().parents[1]
    ledger = json.loads((root / "tests/parity/native-m2-m3.json").read_text())
    frozen = json.loads((root / "tests/parity/ledger.json").read_text())
    ids = {row["id"] for row in frozen["rows"]}
    for row in ledger["rows"]:
        assert set(row["mapped_frozen_rows"]) <= ids, row["id"]
        assert row["rust_tests"] and row["scenario_ids"], row["id"]
        assert row["status"] == "qualified", row["id"]
    assert set(ledger["milestones"].values()) == {"qualified"}
    for section in ("source_hashes", "evidence_hashes", "historical_fixture_hashes"):
        assert ledger[section], section
        for relative, expected in ledger[section].items():
            path = root / relative
            assert hashlib.sha256(path.read_bytes()).hexdigest() == expected, relative
    for platform, receipt in ledger["platforms"].items():
        assert receipt["tests_passed"] and not receipt["tests_skipped"], platform
        assert {wheel["python"] for wheel in receipt["wheels"]} == {"3.11.15", "3.14.6"}
        for wheel in receipt["wheels"]:
            value = json.loads((root / wheel["receipt"]).read_text())
            assert (
                value["passed"] and value["gil_released"] and value["outside_checkout"]
            )
            assert (
                hashlib.sha256((root / wheel["artifact"]).read_bytes()).hexdigest()
                == value["wheel_sha256"]
            )
            assert (
                hashlib.sha256((root / receipt["binary"]).read_bytes()).hexdigest()
                == value["binary_sha256"]
            )
            assert (
                value["immutable_database"] and value["canonical_float_samples"] >= 3000
            )
    print("Native M1/M2/M3 source and local macOS/Linux evidence verified")


if __name__ == "__main__":
    main()
