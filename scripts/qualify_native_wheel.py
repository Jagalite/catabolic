#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Installed native wheel smoke; execute with cwd outside the source checkout."""

import argparse
import hashlib
import importlib.metadata
import json
import math
import os
import random
import shutil
import signal
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zipfile
from pathlib import Path

import catabolic_native as native

if not __debug__:
    raise RuntimeError(
        "wheel qualification requires Python assertions; do not use -O or PYTHONOPTIMIZE"
    )


def verify_installed_wheel(wheel):
    """Bind the supplied archive to every installed payload, not just its label."""
    distribution = importlib.metadata.distribution("catabolic-native")
    root = Path(distribution.locate_file("")).resolve()
    imported = Path(native.__file__).resolve()
    extension = Path(native._native.__file__).resolve()
    generated_metadata = {"INSTALLER", "REQUESTED", "direct_url.json", "RECORD"}
    installed_payloads = set()
    for entry in distribution.files or ():
        relative = Path(entry)
        if "__pycache__" in relative.parts:
            continue
        if (
            len(relative.parts) >= 2
            and relative.parts[0].endswith(".dist-info")
            and relative.name in generated_metadata
        ):
            continue
        if relative.is_absolute() or ".." in relative.parts:
            # pip's generated launcher is tested independently, and is not
            # an archive payload. No other outside-root file is qualified.
            assert relative.name == "catabolic-native" and "bin" in relative.parts, (
                "unexpected relocated distribution payload"
            )
            continue
        installed_payloads.add(str(relative))
    assert installed_payloads, "installed distribution has no payload inventory"
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
    assert set(hashes) == installed_payloads, (
        "wheel and installed distribution payload inventories differ",
        sorted(installed_payloads - set(hashes)),
        sorted(set(hashes) - installed_payloads),
    )
    for name in ("WHEEL", "METADATA", "entry_points.txt"):
        assert sum(key.endswith(".dist-info/" + name) for key in hashes) == 1, (
            "missing or ambiguous wheel metadata",
            name,
        )
    for name in ("bin/catabolic-native", "native-helper.json"):
        assert "catabolic_native/" + name in hashes, "missing packaged native helper"
    for path in imported.parent.rglob("*"):
        assert not path.is_symlink(), (
            "installed package contains an unqualified symlink"
        )
        if path.is_file() and "__pycache__" not in path.parts:
            assert path.resolve().is_relative_to(root), "package payload escaped root"
            assert str(path.relative_to(root)) in hashes, (
                "installed package file missing from supplied wheel",
                str(path.relative_to(root)),
            )
    assert distribution.version == native.__version__, (
        "distribution/import version mismatch"
    )
    return {
        "installed_payloads_sha256": hashes,
        "imported_extension_sha256": extension_hash,
        "installed_payloads_match_wheel": True,
    }


def qualify_import_interrupt(path, source, temporary):
    """Exercise installed PyO3 cancellation after a disposable tool launch."""
    file_id = native.execute_sql(path, "SELECT id FROM files")["rows"][0][0]
    native.execute_operation(
        path,
        "curation",
        "item-put",
        id="installed-book",
        kind="book_edition",
        identities={},
        metadata={"title": "Installed fixture"},
    )
    native.execute_operation(
        path,
        "curation",
        "associate",
        file_id=file_id,
        item_id="installed-book",
        role="primary",
    )
    # A fixture-only desired mapping. No output is created or reconciled.
    with sqlite3.connect(path) as db:
        db.execute(
            "INSERT INTO mappings VALUES (?, 'global', ?, ?, ?, 1)",
            ("installed-import", file_id, "installed-book", "Books/installed.epub"),
        )
    document = native.build_manifest(path)
    request = temporary / "import-request.json"
    request.write_text(json.dumps(document))
    tools = temporary / "tools"
    tools.mkdir()
    staging = temporary / "import-staging"
    staging.mkdir()
    marker = temporary / "tool-started.json"
    executable = tools / "calibredb"
    executable.write_text(
        f"#!{sys.executable}\n"
        "import json,os,pathlib,sys,time\n"
        f"pathlib.Path({str(marker)!r}).write_text(json.dumps({{'pid':os.getpid(),'copy':sys.argv[-1]}}))\n"
        "time.sleep(60)\n"
    )
    executable.chmod(0o755)
    environment = os.environ | {
        "PATH": str(tools) + os.pathsep + os.environ.get("PATH", ""),
        "TMPDIR": str(staging),
    }
    environment.pop("CATABOLIC_NATIVE_BINARY", None)
    before = (source.stat().st_ino, hashlib.sha256(source.read_bytes()).hexdigest())
    child = subprocess.Popen(
        [
            sys.executable,
            "-c",
            "import catabolic_native as n,json,sys; "
            "print(json.dumps(n.execute_operation(sys.argv[1],'import','catalog',"
            "document=json.load(open(sys.argv[2])),target='calibre',"
            "destination=sys.argv[3],apply=True,timeout=120)))",
            str(path),
            str(request),
            str(temporary / "import-library"),
        ],
        cwd=temporary,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 20
        while not marker.exists() and time.monotonic() < deadline:
            assert child.poll() is None, child.communicate()
            time.sleep(0.01)
        assert marker.exists(), "installed importer did not launch fixture tool"
        # The long external call must not retain the catalog's writer lease.
        native.execute_operation(path, "tag", "put", name="installed:during-import")
        child.send_signal(signal.SIGINT)
        output, error = child.communicate(timeout=15)
        assert child.returncode == 0, error
        report = json.loads(output)
        assert report["interrupted"] is True, report
        assert report["outcome"] == "external_outcome_unknown", report
        assert report["applied"] is None and not report["safe_to_retry"], report
        assert not report["complete"] and report["completed"] == [], report
        assert not list(staging.iterdir()), "interrupted staging was retained"
        launched = json.loads(marker.read_text())
        assert "catabolic-import-" in launched["copy"]
        assert not Path(launched["copy"]).exists()
        try:
            os.kill(launched["pid"], 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError("interrupted external tool was not reaped")
        assert before == (
            source.stat().st_ino,
            hashlib.sha256(source.read_bytes()).hexdigest(),
        ), "import changed source bytes or identity"
    finally:
        if child.poll() is None:
            child.kill()
        child.communicate(timeout=15)


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
    optimized = subprocess.run(
        [sys.executable, "-O", str(Path(__file__).resolve()), "--help"],
        capture_output=True,
        text=True,
    )
    if (
        optimized.returncode == 0
        or "requires Python assertions" not in optimized.stderr
    ):
        raise RuntimeError("optimized qualification did not refuse")
    helper = native.native_binary()
    helper_metadata = json.loads(
        (helper.parent.parent / "native-helper.json").read_text()
    )
    assert helper.is_file() and os.access(helper, os.X_OK)
    assert hashlib.sha256(helper.read_bytes()).hexdigest() == helper_metadata["sha256"]
    assert (
        hashlib.sha256(args.binary.read_bytes()).hexdigest()
        == helper_metadata["sha256"]
    ), "supplied native CLI differs from the helper packaged with this extension"
    assert helper_metadata["version"] == native.__version__
    launcher = Path(sys.executable).parent / "catabolic-native"
    assert launcher.is_file(), "installed native launcher missing"
    launched = json.loads(subprocess.check_output([launcher, "--version"], text=True))
    assert launched["version"] == native.__version__
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
        # This actually launches the Rust observation worker from an embedded
        # Python host. Neither ambient PATH nor an explicit fixture override
        # supplies the helper; the exact helper packaged beside _native does.
        source = Path(temporary) / "source"
        source.mkdir()
        (source / "installed.epub").write_text("packaged native worker evidence")
        native.execute_operation(
            path, "observe", "bind", source="media", root=str(source)
        )
        override = os.environ.pop("CATABOLIC_NATIVE_BINARY", None)
        try:
            observation = native.execute_operation(
                path, "observe", "scan", source="media", isolate=True, wait_seconds=30
            )
        finally:
            if override is not None:
                os.environ["CATABOLIC_NATIVE_BINARY"] = override
        assert observation["complete"], observation
        assert observation["scans"][0]["observed"] == 1, observation
        assert (
            source / "installed.epub"
        ).read_text() == "packaged native worker evidence"
        qualify_import_interrupt(path, source / "installed.epub", Path(temporary))
    with tempfile.TemporaryDirectory(prefix="catabolic-helper-check-") as temporary:
        copied = Path(temporary) / "catabolic_native"
        shutil.copytree(Path(native.__file__).parent, copied)
        binary = copied / "bin/catabolic-native"
        environment = os.environ | {"PYTHONPATH": temporary}
        environment.pop("CATABOLIC_NATIVE_BINARY", None)
        saved = binary.read_bytes()
        for state in ("missing", "changed", "not-executable"):
            if state == "missing":
                binary.unlink()
            elif state == "changed":
                binary.write_bytes(saved + b"changed")
            else:
                binary.write_bytes(saved)
                binary.chmod(0o644)
            check = subprocess.run(
                [sys.executable, "-c", "import catabolic_native"],
                cwd=temporary,
                env=environment,
                capture_output=True,
                text=True,
            )
            assert check.returncode != 0 and "native helper" in check.stderr, (
                state,
                check.stderr,
            )
            binary.write_bytes(saved)
            binary.chmod(0o755)
        check = subprocess.run(
            [
                sys.executable,
                "-c",
                (
                    "import catabolic_native as n; p=n.native_binary(); "
                    "p.write_bytes(p.read_bytes()+b'changed'); n.native_binary()"
                ),
            ],
            cwd=temporary,
            env=environment,
            capture_output=True,
            text=True,
        )
        assert check.returncode != 0 and "identity changed" in check.stderr, (
            check.stderr
        )
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
        "packaged_native_helper_sha256": helper_metadata["sha256"],
        "packaged_native_cli_launcher": True,
        "installed_pyo3_isolated_observation": True,
        "installed_pyo3_import_interrupt_and_writer_progress": True,
        "missing_changed_nonexecutable_helper_refused": True,
        "passed": True,
    }
    args.receipt.write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
