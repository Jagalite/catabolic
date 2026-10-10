#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Run mapped reference proof groups against an existing verified oracle export."""

import argparse
import hashlib
import json
import os
import sys
from pathlib import Path

# Support both direct script execution and imports from source tests.
if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from scripts.migration_baseline import UNIT_RUNNER, run_check, write_json
from scripts.migration_contracts import PROOFS, REFERENCE, build, sha


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--inventory", type=Path, default=Path("tests/parity/inventory.json")
    )
    parser.add_argument(
        "--contracts", type=Path, default=Path("tests/parity/contracts.json")
    )
    parser.add_argument("--proof", choices=sorted(PROOFS), action="append")
    parser.add_argument("--timeout", type=float, default=300)
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("timeout must be positive")
    source = args.source.resolve()
    raw = args.inventory.read_bytes()
    contracts = json.loads(args.contracts.read_bytes())
    if build(source, json.loads(raw), sha(raw)) != contracts:
        raise SystemExit("supplemental contract drift; no reference tests executed")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = {k: v for k, v in os.environ.items() if not k.startswith("CATABOLIC_")}
    env["PYTHONPATH"] = str(source / "src")
    env["PYTHONNOUSERSITE"] = "1"
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    report = dict(
        reference_commit=REFERENCE,
        inventory_sha256=sha(raw),
        contracts_sha256=sha(args.contracts.read_bytes()),
        source=str(source),
        python=sys.version,
        tool_sha256={
            str(path): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in (
                Path(__file__),
                Path(__file__).with_name("migration_baseline.py"),
                Path(__file__).with_name("migration_contracts.py"),
            )
        },
        scope="Mapped Python reference checks only; not Rust parity or M0 completion",
        checks=[],
    )
    write_json(output / "report.json", report)
    for proof in dict.fromkeys(args.proof or PROOFS):
        directory = output / proof
        directory.mkdir()
        command = [
            sys.executable,
            "-c",
            UNIT_RUNNER,
            str(directory / "unit-result.json"),
            *PROOFS[proof],
        ]
        result = run_check("unit", command, source, directory, env, args.timeout)
        result["proof"] = proof
        result["reference_tests"] = PROOFS[proof]
        result["log"] = f"{proof}/{result['log']}"
        report["checks"].append(result)
        write_json(output / "report.json", report)
        print(f"{proof}: {result['status']}", flush=True)
    return int(any(row["status"] != "passed" for row in report["checks"]))


if __name__ == "__main__":
    raise SystemExit(main())
