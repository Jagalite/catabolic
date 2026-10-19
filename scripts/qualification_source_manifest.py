#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Freeze or verify all Git-visible source bytes without building or changing them.

Tracked files and nonignored untracked files are covered, including scripts,
tests, workflows, lockfiles and new source inputs. Ignored build products,
external oracle/fixture trees, toolchains, Cargo home/ancestor configuration and
dependency caches require separate input/artifact fences. This detects ordinary
source drift; it is not an atomic snapshot or hostile-filesystem attestation.
"""

import argparse
import hashlib
import json
import os
import stat
import subprocess
from pathlib import Path


def git(root, *arguments):
    return subprocess.check_output(["git", "-C", str(root), *arguments])


def inventory(root):
    names = git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard")
    paths = sorted({os.fsdecode(name) for name in names.split(b"\0") if name})
    if not paths or len(paths) > 100000:
        raise ValueError("source inventory is empty or exceeds its bound")
    for name in paths:
        relative = Path(name)
        if relative.is_absolute() or ".." in relative.parts or ".git" in relative.parts:
            raise ValueError("invalid source inventory path")
    return paths


def identity(info):
    return (
        info.st_dev,
        info.st_ino,
        info.st_mode,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
    )


def capture(root):
    root = Path(root).resolve(strict=True)
    top = Path(os.fsdecode(git(root, "rev-parse", "--show-toplevel")).strip()).resolve()
    if top != root:
        raise ValueError("source root must be the Git checkout root")
    head = git(root, "rev-parse", "HEAD").decode("ascii").strip()
    names = inventory(root)
    files = {}
    total = 0
    for name in names:
        path = root / name
        # A symlinked ancestor is outside this helper's source-input contract.
        if any(parent.is_symlink() for parent in path.parents if parent != root):
            raise ValueError(f"symlinked source ancestor: {name}")
        before = path.lstat()
        if not stat.S_ISREG(before.st_mode):
            raise ValueError(f"source input is not a regular file: {name}")
        total += before.st_size
        if before.st_size > 128 * 1024 * 1024 or total > 1024 * 1024 * 1024:
            raise ValueError("source content exceeds its bound")
        with path.open("rb") as stream:
            if identity(os.fstat(stream.fileno())) != identity(before):
                raise ValueError(f"source changed before hashing: {name}")
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
            if identity(os.fstat(stream.fileno())) != identity(before):
                raise ValueError(f"source changed during hashing: {name}")
        if identity(path.lstat()) != identity(before):
            raise ValueError(f"source changed after hashing: {name}")
        files[name] = {
            "sha256": digest,
            "bytes": before.st_size,
            "mode": stat.S_IMODE(before.st_mode),
        }
    if (
        inventory(root) != names
        or git(root, "rev-parse", "HEAD").decode().strip() != head
    ):
        raise ValueError("source inventory or HEAD changed during capture")
    encoded = json.dumps(files, sort_keys=True, separators=(",", ":")).encode()
    return {
        "format": 1,
        "scope": "all Git-tracked and nonignored untracked regular source files",
        "source_root": str(root),
        "git_head": head,
        "files_sha256": hashlib.sha256(encoded).hexdigest(),
        "files": files,
    }


def write_new(root, destination):
    root = Path(root).resolve(strict=True)
    destination = Path(destination).absolute()
    if destination.resolve().is_relative_to(root):
        raise ValueError("source manifest must be outside the checkout")
    snapshot = capture(root)
    # No source writes, directory creation, replacement or stale-success reuse.
    with destination.open("x", encoding="utf-8") as stream:
        json.dump(snapshot, stream, indent=2, sort_keys=True)
        stream.write("\n")
    return snapshot


def verify(root, manifest):
    expected = json.loads(Path(manifest).read_text(encoding="utf-8"))
    actual = capture(root)
    if actual != expected:
        raise ValueError("source manifest differs from the current checkout")
    return actual


def main():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("action", choices=("capture", "verify"))
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--manifest", required=True, type=Path)
    args = parser.parse_args()
    try:
        result = (write_new if args.action == "capture" else verify)(
            args.root, args.manifest
        )
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        raise SystemExit(f"source manifest failed: {error}") from error
    print(
        json.dumps(
            {"files": len(result["files"]), "files_sha256": result["files_sha256"]}
        )
    )


if __name__ == "__main__":
    main()
