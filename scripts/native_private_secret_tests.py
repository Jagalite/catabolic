#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Narrow development comparison for save_secret, using only fake credentials.

This extracts the supplied wheel into a disposable directory, never installs
packages, and is not an installed-wheel/release qualification gate.
"""

import argparse
import hashlib
import importlib
import importlib.util
import inspect
import json
import os
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

FROZEN = "453fca983222c6665a48775eef67c489c96b527e"
GROUPS = (
    "signature",
    "ordinary",
    "unicode_order",
    "json_types",
    "relative_path",
    "home_path",
    "existing_file",
    "missing_parent",
    "serialization_error",
    "callback_identity",
    "missing_summary",
    "nested_rollback",
    "outer_rollback",
    "registration_error",
    "registration_reuse",
    "readonly",
    "lifecycle",
)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def scenario(engine, package, root):
    if package is not None:
        sys.path.insert(0, str(package))
    namespace = "catabolic" if engine == "reference" else "catabolic_native"
    module = importlib.import_module(namespace + ".api_cli")
    Store = importlib.import_module(namespace + ".store").Store
    if package is not None:
        assert Path(module.__file__).resolve() == package / namespace / "api_cli.py"
    assert (
        "catabolic_native" if engine == "reference" else "catabolic"
    ) not in sys.modules
    if engine == "reference":
        assert sys.version_info[:3] == (3, 14, 6)
    root.mkdir()
    os.chdir(root)
    os.environ["HOME"] = str(root)
    Store.initialize(root / "fixture.db")
    store = Store(root / "fixture.db", writable=True)
    callbacks = []
    store.after_close["fixture"] = lambda: callbacks.append("closed")
    save = module.save_secret
    fake = {"token_id": "FAKE-ID", "token": "FAKE-NOT-A-CREDENTIAL", "expires": 0}
    groups = {"signature": str(inspect.signature(save))}
    classes = []

    def normalize(value):
        return json.loads(
            json.dumps(value, ensure_ascii=True).replace(str(root), "<ROOT>")
        )

    def private_rows():
        values = []
        for row in store.rows("SELECT * FROM api_private_files ORDER BY path"):
            stat = Path(row["path"]).stat()
            values.append(
                [row["path"], row["device"] == stat.st_dev, row["inode"] == stat.st_ino]
            )
        return normalize(values)

    def attempt(filename, value=fake, expected=None):
        path = Path(filename).expanduser().absolute()
        try:
            result = {"value": save(store, filename, value)}
        except BaseException as error:
            classes.append(type(error).__name__)
            # SQLite exception class is a documented shared-native-owner gap.
            kind = type(error).__name__
            if kind == "IntegrityError":
                kind = "CatabolicError"
            result = {"error": [kind, str(error)]}
            if expected is not None:
                assert error is expected
                result["original_exception"] = True
        if path.is_file():
            result["bytes"] = path.read_bytes().hex()
            result["mode"] = path.stat().st_mode & 0o777
        else:
            result["bytes"] = None
        result["rows"] = private_rows()
        result["epoch"] = store.rows(
            "SELECT generation FROM fallback_epoch WHERE id=1"
        )[0]["generation"]
        assert callbacks == []
        assert store.rows("SELECT 1 AS value") == [{"value": 1}]
        return normalize(result)

    groups["ordinary"] = attempt(root / "ordinary.json")
    ordered = {
        "z": "é😀\n",
        "token_id": "FAKE-UNICODE",
        "token": "FAKE-ONLY",
        "expires": 1.25,
        "a": "last",
    }
    groups["unicode_order"] = attempt(root / "unicode.json", ordered)
    groups["json_types"] = attempt(
        root / "types.json",
        {
            "token_id": "FAKE-TYPES",
            "expires": 0,
            "token": "FAKE-ONLY",
            "values": (True, None, 10**80, float("nan"), float("inf"), -float("inf")),
            7: {False: "fake-bool-key"},
        },
    )
    groups["relative_path"] = attempt("./relative.json")
    groups["home_path"] = attempt("~/home.json")
    groups["existing_file"] = attempt(root / "ordinary.json")
    groups["missing_parent"] = attempt(root / "absent" / "new.json")
    groups["serialization_error"] = attempt(
        root / "partial.json", {"token_id": "FAKE", "bad": object()}
    )

    sentinel = KeyboardInterrupt("fake callback exception")

    class Broken(dict):
        def items(self):
            raise sentinel

    groups["callback_identity"] = attempt(
        root / "callback.json", Broken(fake), sentinel
    )
    groups["missing_summary"] = attempt(
        root / "missing-summary.json", {"token": "FAKE-ONLY"}
    )

    class Rollback(Exception):
        pass

    with store.transaction() as outer:
        outer.execute("INSERT INTO api_principals VALUES ('fake-outer','default',1)")
        try:
            with store.transaction():
                saved = attempt(root / "nested.json")
                raise Rollback
        except Rollback:
            pass
        groups["nested_rollback"] = {
            "saved": saved,
            "rows_after": private_rows(),
            "outer_visible": store.rows("SELECT id FROM api_principals ORDER BY id"),
        }
    try:
        with store.transaction() as db:
            db.execute("INSERT INTO api_principals VALUES ('fake-inner','default',1)")
            saved = attempt(root / "outer.json")
            raise Rollback
    except Rollback:
        pass
    groups["outer_rollback"] = {
        "saved": saved,
        "rows_after": private_rows(),
        "principals": store.rows("SELECT id FROM api_principals ORDER BY id"),
    }
    store.db.execute(
        "CREATE TEMP TRIGGER fail_private BEFORE INSERT ON api_private_files BEGIN SELECT RAISE(ABORT,'invalid_grant'); END"
    )
    groups["registration_error"] = attempt(root / "registration.json")
    store.db.execute("DROP TRIGGER fail_private")
    groups["registration_reuse"] = attempt(root / "reuse.json")
    store.close()
    assert callbacks == ["closed"]
    store = Store(root / "fixture.db")
    callbacks.clear()
    groups["readonly"] = attempt(root / "readonly.json")
    store.close()
    # Native lifecycle admission intentionally refuses before creating output;
    # the frozen writer creates its file before touching the closed Store.
    try:
        save(store, root / "closed.json", fake)
    except BaseException as error:
        lifecycle = {
            "error": type(error).__name__,
            "file_exists": (root / "closed.json").exists(),
        }
    else:
        raise AssertionError("closed Store accepted registration")
    if engine == "native":
        assert lifecycle == {"error": "CatabolicError", "file_exists": False}
    else:
        assert lifecycle == {"error": "AttributeError", "file_exists": True}
    groups["lifecycle"] = "explicit native pre-effect lifecycle refusal"
    assert tuple(groups) == GROUPS
    return {
        "groups": groups,
        "lifecycle_difference": lifecycle,
        "exception_classes": classes,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("--scenario", choices=("reference", "native"))
    parser.add_argument("--package", type=Path)
    parser.add_argument("--root", type=Path)
    parser.add_argument("--wheel", type=Path)
    parser.add_argument("--reference-root", type=Path)
    parser.add_argument("--reference-python", type=Path)
    parser.add_argument("--candidate-python", type=Path, action="append")
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    assert __debug__ and not os.environ.get("PYTHONOPTIMIZE")
    if args.scenario:
        print(
            json.dumps(
                scenario(args.scenario, args.package.resolve(), args.root.resolve())
            )
        )
        return
    if args.report:
        args.report.unlink(missing_ok=True)
    assert (
        args.wheel
        and args.reference_root
        and args.reference_python
        and args.candidate_python
    )
    helper_path = Path(__file__).with_name("qualify_native_embedding.py")
    spec = importlib.util.spec_from_file_location(
        "private_secret_provenance", helper_path
    )
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    before = helper.reference_provenance(args.reference_root, FROZEN)
    wheel_hash = digest(args.wheel)
    script_hash = digest(Path(__file__))
    results = []
    with tempfile.TemporaryDirectory(prefix="catabolic-fake-secret-") as temporary:
        root = Path(temporary)
        package = root / "wheel"
        with zipfile.ZipFile(args.wheel) as wheel:
            assert all(
                not Path(name).is_absolute() and ".." not in Path(name).parts
                for name in wheel.namelist()
            )
            wheel.extractall(package)
        payload = {
            str(path.relative_to(package)): digest(path)
            for path in package.rglob("*")
            if path.is_file()
        }

        def run(python, engine, source, name):
            process = subprocess.run(
                [
                    str(python),
                    "-I",
                    "-B",
                    str(Path(__file__).resolve()),
                    "--scenario",
                    engine,
                    "--package",
                    str(source),
                    "--root",
                    str(root / name),
                ],
                text=True,
                capture_output=True,
                timeout=120,
            )
            assert process.returncode == 0, process.stderr
            return json.loads(process.stdout)

        reference = run(
            args.reference_python, "reference", args.reference_root / "src", "reference"
        )
        for index, python in enumerate(args.candidate_python):
            native = run(python, "native", package, f"native-{index}")
            for name in GROUPS:
                assert reference["groups"][name] == native["groups"][name], (
                    name,
                    reference["groups"][name],
                    native["groups"][name],
                )
            version = subprocess.check_output(
                [str(python), "-I", "-c", "import sys;print(sys.version.split()[0])"],
                text=True,
            ).strip()
            results.append(
                {
                    "python": version,
                    "paired_groups": len(GROUPS),
                    "lifecycle_difference": native["lifecycle_difference"],
                    "exception_classes": native["exception_classes"],
                }
            )
        assert payload == {
            str(path.relative_to(package)): digest(path)
            for path in package.rglob("*")
            if path.is_file()
        }
    assert before == helper.reference_provenance(args.reference_root, FROZEN)
    assert wheel_hash == digest(args.wheel) and script_hash == digest(Path(__file__))
    report = {
        "mode": "development extracted-wheel; not installed/release qualification",
        "reference_commit": FROZEN,
        "wheel_sha256": wheel_hash,
        "script_sha256": script_hash,
        "payload_sha256": payload,
        "results": results,
    }
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"mode": report["mode"], "results": results}))


if __name__ == "__main__":
    main()
