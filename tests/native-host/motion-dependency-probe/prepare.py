#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Prepare an isolated native dependency smoke; never edit either checkout."""
from __future__ import annotations

import argparse
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import urllib.request

ARCHIVE_URL = "https://static.crates.io/crates/sqlx-sqlite/sqlx-sqlite-0.8.6.crate"
ARCHIVE_SHA256 = "c2d12fe70b2c1b4401038055f90f151b78208de1f9f89a7dbfd41587a10c3eea"
MOTION_REVISION = "1e5a24675b059337b34e98f86b225178798cc080"
CRATE_ROOT = "sqlx-sqlite-0.8.6"
OLD = '[dependencies.libsqlite3-sys]\nversion = "0.30.1"'
NEW = '[dependencies.libsqlite3-sys]\nversion = "=0.38.2"'


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def prepare(motion: Path, catabolic: Path, output: Path) -> None:
    assets = Path(__file__).resolve().parent
    motion, catabolic, output = motion.resolve(), catabolic.resolve(), output.resolve()
    if output.exists():
        raise ValueError(f"Output already exists; choose a fresh staging directory: {output}")
    for root in (motion, catabolic):
        if output == root or root in output.parents:
            raise ValueError("Output must be outside both source checkouts")
        if not (root / "Cargo.toml").is_file():
            raise ValueError(f"Missing checkout Cargo.toml: {root}")
    motion_revision = git(motion, "rev-parse", "HEAD")
    motion_status = git(motion, "status", "--porcelain")
    if motion_revision != MOTION_REVISION or motion_status:
        raise ValueError("This qualification requires the documented clean Motion revision")
    catabolic_revision = git(catabolic, "rev-parse", "HEAD")
    catabolic_status = git(catabolic, "status", "--porcelain")
    with urllib.request.urlopen(ARCHIVE_URL, timeout=60) as response:
        archive_bytes = response.read(10 * 1024 * 1024 + 1)
    if hashlib.sha256(archive_bytes).hexdigest() != ARCHIVE_SHA256:
        raise ValueError("Official archive SHA-256 mismatch; refusing to prepare")
    output.mkdir(parents=True)
    vendor = output / "vendor"
    vendor.mkdir()
    with tarfile.open(fileobj=io.BytesIO(archive_bytes)) as archive:
        for member in archive.getmembers():
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts or path.parts[0] != CRATE_ROOT:
                raise ValueError(f"Unexpected archive path: {member.name}")
            target = vendor.joinpath(*path.parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            elif member.isfile():
                target.parent.mkdir(parents=True, exist_ok=True)
                source = archive.extractfile(member)
                assert source is not None
                target.write_bytes(source.read())
            else:
                raise ValueError(f"Unexpected archive member type: {member.name}")
    manifest = vendor / CRATE_ROOT / "Cargo.toml"
    original = manifest.read_text()
    if original.count(OLD) != 1:
        raise ValueError("Expected exact dependency declaration missing")
    manifest.write_text(original.replace(OLD, NEW))
    template = (assets / "Cargo.toml.in").read_text()
    for token, path in {
        "__MOTION_PATH__": motion,
        "__CATABOLIC_RUNTIME_PATH__": catabolic / "crates/catabolic-runtime",
        "__CATABOLIC_STORE_PATH__": catabolic / "crates/catabolic-store",
    }.items():
        template = template.replace(token, json.dumps(str(path)))
    (output / "Cargo.toml").write_text(template)
    (output / "src").mkdir()
    shutil.copyfile(assets / "src/main.rs", output / "src/main.rs")
    shutil.copyfile(assets / "Cargo.lock", output / "Cargo.lock")
    (output / "preparation.json").write_text(json.dumps({
        "archive_url": ARCHIVE_URL,
        "archive_sha256": ARCHIVE_SHA256,
        "motion_commit": motion_revision,
        "motion_status": motion_status,
        "catabolic_commit": catabolic_revision,
        "catabolic_status": catabolic_status,
        "patch": {"file": "vendor/sqlx-sqlite-0.8.6/Cargo.toml", "old": OLD, "new": NEW},
        "result": "prepared_only_build_and_runtime_not_yet_verified",
    }, indent=2) + "\n")
    print(f"Prepared {output}; no build or runtime result is claimed")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--motion-root", required=True, type=Path)
    parser.add_argument("--catabolic-root", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    prepare(args.motion_root, args.catabolic_root, args.output)


if __name__ == "__main__":
    main()
