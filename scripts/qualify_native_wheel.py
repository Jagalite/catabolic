#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Installed native wheel smoke; execute with cwd outside the source checkout."""

import argparse
import hashlib
import importlib.metadata
import json
import math
import random
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zipfile
from pathlib import Path

import catabolic_native as native


def verify_installed_wheel(wheel):
    """Bind the supplied archive to every installed payload, not just its label."""
    distribution = importlib.metadata.distribution("catabolic-native")
    root = Path(distribution.locate_file("")).resolve()
    imported = Path(native.__file__).resolve()
    extension = Path(native._native.__file__).resolve()
    hashes = {}
    with zipfile.ZipFile(wheel) as archive:
        members = archive.infolist()
        assert members and len(members) < 10000, "invalid wheel member count"
        assert sum(member.file_size for member in members) <= 256 * 1024 * 1024, (
            "wheel exceeds qualification payload budget"
        )
        assert len({member.filename for member in members}) == len(members), (
            "duplicate wheel members"
        )
        for member in members:
            if member.is_dir():
                continue
            relative = Path(member.filename)
            assert not relative.is_absolute() and ".." not in relative.parts, (
                "invalid wheel path"
            )
            assert not any(part.endswith(".data") for part in relative.parts), (
                "wheel relocation scheme requires explicit qualification"
            )
            if member.filename.endswith(".dist-info/RECORD"):
                continue  # pip rewrites RECORD to include its installation metadata.
            target = Path(distribution.locate_file(member.filename)).resolve()
            assert target.is_relative_to(root), (
                "installed payload escaped distribution root"
            )
            expected = hashlib.sha256(archive.read(member)).hexdigest()
            assert (
                target.is_file()
                and hashlib.sha256(target.read_bytes()).hexdigest() == expected
            ), f"installed payload differs from supplied wheel: {member.filename}"
            hashes[member.filename] = expected
    assert hashes.get(str(imported.relative_to(root))), (
        "imported package missing from supplied wheel"
    )
    extension_hash = hashes.get(str(extension.relative_to(root)))
    assert extension_hash, "imported native extension missing from supplied wheel"
    assert any(name.endswith(".dist-info/WHEEL") for name in hashes), (
        "missing wheel metadata"
    )
    assert distribution.version == native.__version__, (
        "distribution/import version mismatch"
    )
    return {
        "installed_payloads_sha256": hashes,
        "imported_extension_sha256": extension_hash,
        "installed_payloads_match_wheel": True,
    }


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
    installed = verify_installed_wheel(args.wheel)
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
        for document in (
            "query($v:String){renditions(matching:{purpose:$v}){nodes}}",
            "query($v:Boolean){items(hasRendition:{current:$v}){nodes{id}}}",
        ):
            assert "errors" not in native.execute_graphql(path, document)
        document = (
            "query A($v:Int){items(year:$v){nodes{id}}} "
            "query B($v:ID!){item(id:$v){id}}"
        )
        for operation in ("A", "B"):
            for _ in range(20):
                assert "errors" not in native.execute_graphql(
                    path, document, variables={"v": 2020}, operation_name=operation
                )
        ticks = []
        stop = threading.Event()

        def tick():
            while not stop.wait(0.001):
                ticks.append(time.monotonic())

        thread = threading.Thread(target=tick)
        thread.start()
        start = time.monotonic()
        try:
            native.execute_sql(
                path,
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n) SELECT sum(x) FROM n",
                timeout_ms=100,
            )
            raise AssertionError("deadline did not stop recursive query")
        except RuntimeError as error:
            assert "timed out" in str(error)
        finally:
            end = time.monotonic()
            stop.set()
            thread.join()
        assert any(start + 0.02 < tick < end - 0.02 for tick in ticks), (
            "native SQL held Python's GIL"
        )
        assert path.read_bytes() == before
        tag = native.execute_operation(path, "tag", "put", name="installed:Café")
        repeated = json.loads(
            subprocess.check_output(
                [
                    str(args.binary),
                    "--db",
                    str(path),
                    "operation",
                    "tag",
                    "put",
                    "--options",
                    json.dumps({"name": "installed:Café"}),
                ],
                text=True,
            )
        )
        assert repeated == tag and tag["name"] == "installed:café"
        query = native.execute_operation(
            path,
            "definitions",
            "query-put",
            name="installed",
            value={
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items",
                },
            },
        )
        assert query["revision"] == 1
        assert (
            native.execute_operation(path, "definitions", "query-get", id=query["id"])
            == query
        )
        assert native.run_query(path, query["id"])["ids"] == []
        after = path.read_bytes()
        try:
            native.execute_operation(path, "missing-owner", "put")
            raise AssertionError("unsupported operation did not fail")
        except RuntimeError as error:
            assert "unknown native operation surface" in str(error)
        assert path.read_bytes() == after
        for version in (1, 2, 3):
            assert native.interchange("schema", version=version)["$schema"]
    randomizer = random.Random(453)
    values = [0.0, -0.0, 1.0, 1e-5, 1e-4, 1e16, 5e-324, 1.7976931348623157e308]
    values += [
        struct.unpack("d", randomizer.getrandbits(64).to_bytes(8, "little"))[0]
        for _ in range(3000)
    ]
    values = [v for v in values if math.isfinite(v)]
    for value in values + [
        {"z": "Café😀", "a": [True, None, 2**80]},
        {"$serde_json::private::Number": "1e400"},
    ]:
        assert native.encode(value) == json.dumps(
            value, sort_keys=True, ensure_ascii=False, allow_nan=False
        ), value
    receipt = {
        "python": sys.version,
        "installed_module": native.__file__,
        "outside_checkout": True,
        "wheel": str(args.wheel),
        "wheel_sha256": hashlib.sha256(args.wheel.read_bytes()).hexdigest(),
        **installed,
        "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
        "schema": 32,
        "canonical_float_samples": len(values),
        "same_use_case": "read-only SQL profile discovery",
        "immutable_database": True,
        "gil_released": True,
        "native_mutation_and_definition_roundtrip": True,
        "unsupported_owner_no_fallback": True,
        "interchange_versions": [1, 2, 3],
        "passed": True,
    }
    args.receipt.write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
