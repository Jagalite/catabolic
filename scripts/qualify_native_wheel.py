#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Installed native wheel smoke; execute with cwd outside the source checkout."""

import argparse
import hashlib
import json
import math
import random
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

import catabolic_native as native


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--wheel", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    assert not Path.cwd().is_relative_to(root), "run outside the checkout"
    assert not Path(native.__file__).is_relative_to(root / "crates"), (
        "installed import required"
    )
    with tempfile.TemporaryDirectory(prefix="catabolic-installed-") as temporary:
        path = Path(temporary) / "catalog.sqlite3"
        native.initialize_database(path)
        before = path.read_bytes()
        expected = native.execute_sql(path, "SELECT id FROM profiles ORDER BY id")
        assert expected["rows"] == [["default"]]
        worker = subprocess.Popen(
            [str(args.binary), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            worker.stdin.write(
                json.dumps(
                    ["--db", str(path), "query", "SELECT id FROM profiles ORDER BY id"]
                )
                + "\n"
            )
            worker.stdin.flush()
            reply = json.loads(worker.stdout.readline())
            assert reply["status"] == 0, reply
            assert reply["result"] == expected, (reply, expected)
        finally:
            worker.stdin.close()
            worker.wait(timeout=10)
            worker.stdout.close()
            worker.stderr.close()
        assert native.inspect_database(path, full=True)["schema"] == 32
        assert (
            native.select_ids(
                path, {"language": "sql", "query": "SELECT id AS item_id FROM items"}
            )["ids"]
            == []
        )
        assert native.execute_graphql(path, "{ profile schemaVersion }")["data"] == {
            "profile": "default",
            "schemaVersion": 32,
        }
        assert path.read_bytes() == before
    randomizer = random.Random(453)
    values = [0.0, -0.0, 1.0, 1e-5, 1e-4, 1e16, 5e-324, 1.7976931348623157e308]
    values += [
        struct.unpack("d", randomizer.getrandbits(64).to_bytes(8, "little"))[0]
        for _ in range(3000)
    ]
    values = [v for v in values if math.isfinite(v)]
    for value in values + [{"z": "Café😀", "a": [True, None, 2**63 - 1]}]:
        assert native.encode(value) == json.dumps(
            value, sort_keys=True, ensure_ascii=False, allow_nan=False
        ), value
    receipt = {
        "python": sys.version,
        "installed_module": native.__file__,
        "outside_checkout": True,
        "wheel": str(args.wheel),
        "wheel_sha256": hashlib.sha256(args.wheel.read_bytes()).hexdigest(),
        "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
        "schema": 32,
        "canonical_float_samples": len(values),
        "same_use_case": "read-only SQL profile discovery",
        "immutable_database": True,
        "passed": True,
    }
    args.receipt.write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
