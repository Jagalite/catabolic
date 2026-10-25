#!/usr/bin/env python3
"""Verify an official runner-built helper's bounded, stable artifact identity.

The caller must build this artifact from the current workspace's catabolic-cli
catabolic-native target. Hashing an arbitrary program does not qualify its closed
observation protocol. This preflight neither runs the artifact nor edits it.
"""
import hashlib
import json
import os
from pathlib import Path
import stat
import sys


def verify(path):
    path = Path(path).resolve(strict=True)
    before = path.stat()
    if not stat.S_ISREG(before.st_mode) or not os.access(path, os.X_OK):
        raise ValueError("native test helper must be an executable regular file")
    if before.st_size > 128 * 1024 * 1024:
        raise ValueError("native test helper exceeds the closed protocol's 128 MiB bound")
    digest = hashlib.sha256()
    count = 0
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            count += len(chunk)
            if count > 128 * 1024 * 1024:
                raise ValueError("native test helper grew beyond its byte bound")
            digest.update(chunk)
    after = path.stat()
    identity = lambda s: (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
    if identity(before) != identity(after) or count != before.st_size:
        raise ValueError("native test helper changed during verification")
    return {"path": str(path), "bytes": count, "sha256": digest.hexdigest(),
            "protocol": "NoChildWithExactJoinV2", "identity": identity(after)}


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: verify_native_test_helper.py OFFICIAL_BUILT_HELPER")
    print(json.dumps(verify(sys.argv[1]), sort_keys=True))
