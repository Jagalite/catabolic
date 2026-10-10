#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Retain synthetic historical databases and verify each reference upgrade."""

import argparse
import hashlib
import json
import sqlite3
import sys
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from scripts.migration_contracts import REFERENCE, verify_source


def generate(source, output, inventory):
    verify_source(source, inventory)
    if any(
        name == "catabolic" or name.startswith("catabolic.") for name in sys.modules
    ):
        raise ValueError("fixture generation requires a fresh reference process")
    sys.path[:0] = [str(source), str(source / "src")]
    from catabolic.migration import (
        data_snapshot,
        load_migrations,
        upgrade_database,
        validate_preservation,
    )
    from tests.test_migrations import create_legacy

    output.mkdir(parents=True, exist_ok=False)
    migrations = load_migrations()
    report = dict(
        reference_commit=REFERENCE,
        coverage="Populated v1-derived states at every schema boundary; not every historical feature combination",
        normalization="None; generated paths, filesystem identities and timestamps remain capture-specific",
        contains_user_data=False,
        fixtures=[],
    )
    for version in range(1, len(migrations) + 1):
        root = output / f"schema-{version:02d}"
        root.mkdir()
        database = create_legacy(root)
        if version > 1:
            upgrade_database(database, migrations=migrations[:version])
        with sqlite3.connect(database) as db:
            before = data_snapshot(db)
            assert db.execute("PRAGMA user_version").fetchone()[0] == version
            assert db.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
            assert db.execute("PRAGMA foreign_key_check").fetchall() == []
            retained = root / "before.sqlite3"
            with sqlite3.connect(retained) as target:
                db.backup(target)
        row = dict(
            schema=version,
            database=str(retained.relative_to(output)),
            sha256=hashlib.sha256(retained.read_bytes()).hexdigest(),
            status="running",
        )
        report["fixtures"].append(row)
        try:
            upgrade_database(database)
            with sqlite3.connect(database) as db:
                validate_preservation(db, before)
                assert db.execute("PRAGMA user_version").fetchone()[0] == len(
                    migrations
                )
                assert db.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
                assert db.execute("PRAGMA foreign_key_check").fetchall() == []
            row["status"] = "passed"
        except Exception as error:
            row.update(status="failed", error=f"{type(error).__name__}: {error}")
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(f"schema {version}: {row['status']}", flush=True)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--inventory", type=Path, default=Path("tests/parity/inventory.json")
    )
    args = parser.parse_args()
    report = generate(
        args.source.resolve(),
        args.output.resolve(),
        json.loads(args.inventory.read_text()),
    )
    return int(any(row["status"] != "passed" for row in report["fixtures"]))


if __name__ == "__main__":
    raise SystemExit(main())
