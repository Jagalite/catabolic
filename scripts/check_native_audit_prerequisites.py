#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Read-only early diagnostics, never a substitute for native qualification."""

import argparse
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def historical_python(root, executable):
    fixture = json.loads(
        (root / "tests/parity/mapping-subject-json-diagnostics-v1.json").read_bytes()
    )
    expected = fixture["oracle"]["python_sha256"]
    if not isinstance(expected, str) or len(expected) != 64:
        raise ValueError("invalid historical Python digest in retained fixture")
    actual = hashlib.sha256(executable.read_bytes()).hexdigest()
    if actual != expected:
        raise ValueError(
            f"historical Python executable mismatch: expected {expected}, got {actual}"
        )
    return {"path": str(executable), "sha256": actual}


def original_captures(root):
    # Reuse the mandatory test's immutable contract and complete raw-envelope
    # verifier rather than inventing a weaker existence-only admission rule.
    path = root / "tests/test_rust_graphql_lexical_capture.py"
    spec = importlib.util.spec_from_file_location("native_audit_capture_contract", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    count = module.verify_raw_receipts(module.load_contract())
    if count != 4974:
        raise ValueError(f"expected 4974 original raw receipts, verified {count}")
    return {"verified_receipts": count}


def linux_volume_uuid(temp_root):
    if sys.platform != "linux":
        raise ValueError("this volume diagnostic only supports Linux")
    # Hold the actual directory while resolving its path, like source.rs. This
    # does not reserve its identity for future tests or make a catalog writable.
    fd = os.open(temp_root, os.O_RDONLY | os.O_DIRECTORY)
    try:
        actual_root = os.readlink(f"/proc/self/fd/{fd}")
        result = subprocess.run(
            ["findmnt", "--json", "--target", actual_root, "--output", "UUID"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=5,
            check=True,
        )
    finally:
        os.close(fd)
    if len(result.stdout) > 65536:
        raise ValueError("findmnt output exceeds the production identity bound")
    value = json.loads(result.stdout)
    if not isinstance(value, dict):
        raise ValueError("findmnt must report a JSON object")
    rows = value.get("filesystems")
    if not isinstance(rows, list) or len(rows) != 1:
        raise ValueError("findmnt must report exactly one filesystem")
    uuid = rows[0].get("uuid") if isinstance(rows[0], dict) else None
    if not isinstance(uuid, str) or not uuid:
        raise ValueError("test temporary filesystem has no durable volume UUID")
    return {"temp_root": actual_root, "volume_uuid": uuid.lower()}


def inspect(root, executable, temp_root):
    checks = []
    for name, action, required_action in (
        (
            "restore_volume_identity",
            lambda: linux_volume_uuid(temp_root),
            "Provide a writable UUID-bearing test filesystem and set TMPDIR to it.",
        ),
        (
            "original_capture_receipts",
            lambda: original_captures(root),
            "Restore the original hash-bound capture archive at its recorded paths.",
        ),
        (
            "historical_python_identity",
            lambda: historical_python(root, executable),
            "Provide the retained fixture's exact Python executable; do not rebaseline its hash.",
        ),
    ):
        try:
            checks.append({"name": name, "status": "available", "details": action()})
        except (
            OSError,
            ValueError,
            KeyError,
            TypeError,
            subprocess.SubprocessError,
        ) as error:
            checks.append(
                {
                    "name": name,
                    "status": "blocked",
                    "reason": str(error),
                    "required_action": required_action,
                }
            )
    return {
        "format_version": 1,
        "qualification_status": "not_run",
        "known_prerequisites_available": all(
            check["status"] == "available" for check in checks
        ),
        "checks": checks,
        "limits": [
            "This checks known late blockers, not every build/runtime prerequisite.",
            "Volume identity covers the selected native Rust test root; Python fixture temporary roots may differ.",
            "No Rust models, production adapter tests or aggregate qualification ran.",
            "Reference traceback paths, SQLite capabilities, installed packages and toolchains still need their existing gates.",
            "Available identities can change; mandatory qualification checks remain authoritative.",
        ],
    }


def native_temp_root():
    # Linux std::env::temp_dir(), used by the mandatory Rust restore fixtures,
    # honors TMPDIR without Python's TEMP/TMP or writable-directory fallback.
    return Path(os.environ.get("TMPDIR", "/tmp"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--python-oracle",
        type=Path,
        default=Path(
            os.environ.get(
                "CATABOLIC_PYTHON_ORACLE",
                os.environ.get("CATABOLIC_QUALIFICATION_PYTHON", ".venv/bin/python"),
            )
        ),
    )
    parser.add_argument("--temp-root", type=Path, default=native_temp_root())
    args = parser.parse_args()
    executable = args.python_oracle
    if not executable.is_absolute():
        executable = ROOT / executable
    report = inspect(ROOT, executable, args.temp_root)
    print(json.dumps(report, indent=2))
    return 0 if report["known_prerequisites_available"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
