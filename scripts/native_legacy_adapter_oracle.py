#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Pinned subprocess bootstrap for existing legacy adapter fixture programs.

Runs frozen Python only. The reviewed programs and their sys.argv are preserved;
this file adds identity fences, not a replacement CLI or business implementation.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import sys
from pathlib import Path


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(allow_abbrev=False)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--cache", required=True)
    parser.add_argument("program")
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not __debug__ or sys.flags.optimize or not sys.flags.isolated:
        raise RuntimeError("legacy reference bootstrap requires -I and assertions")
    if digest(args.manifest) != args.manifest_sha256:
        raise RuntimeError("legacy reference manifest changed")
    manifest = json.loads(args.manifest.read_text())
    if manifest["oracle"]["head"] != "453fca983222c6665a48775eef67c489c96b527e":
        raise RuntimeError("legacy reference commit changed")
    runtime = manifest["reference_interpreter"]
    actual = {
        "version": list(sys.version_info[:3]),
        "executable": sys.executable,
        "prefix": sys.prefix,
        "base_prefix": sys.base_prefix,
        "executable_sha256": digest(sys.executable),
    }
    if runtime != actual or runtime["version"] != [3, 14, 6]:
        raise RuntimeError("legacy reference interpreter identity changed")
    root = Path(manifest["oracle"]["root"]).resolve()
    reference = root / "src"
    # Existing fixtures already set this for HTTP/identity worker descendants.
    # Reject a conflicting root and make the pin explicit for every seed/CLI too.
    if os.environ.get("PYTHONPATH") != str(reference):
        raise RuntimeError("legacy reference child source selection changed")
    if (
        os.environ.get("PYTHONSAFEPATH") != "1"
        or os.environ.get("PYTHONNOUSERSITE") != "1"
    ):
        raise RuntimeError("legacy reference child safe import flags changed")
    if os.environ.get("PYTHONPYCACHEPREFIX") != args.cache:
        raise RuntimeError("legacy reference child cache selection changed")
    sys.pycache_prefix = args.cache
    sys.dont_write_bytecode = True
    scripts = Path(__file__).resolve().parent

    def fence():
        if digest(args.manifest) != args.manifest_sha256:
            raise RuntimeError("legacy reference manifest changed")
        for relative, expected in manifest["oracle_scripts_sha256"].items():
            if digest(scripts / relative) != expected:
                raise RuntimeError("legacy reference bootstrap source changed")
        if actual["executable_sha256"] != digest(sys.executable):
            raise RuntimeError("legacy reference interpreter bytes changed")

    fence()
    spec = importlib.util.spec_from_file_location(
        "qualified_reference_provenance", scripts / "qualify_native_embedding.py"
    )
    provenance = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(provenance)
    sys.path.insert(0, str(reference))
    from catabolic import app, store  # noqa: F401

    tracked = manifest["oracle"]["tracked_source_sha256"]
    provenance.reference_imports(root, tracked)
    # Trusted frozen descendants keep their original argv/supervision. Safe
    # path and no-user-site remove ambient cwd/user roots before PYTHONPATH;
    # the gate independently records their real import selection before/after.
    # This is an immutable-source qualification boundary, not a Python sandbox.
    sys.argv = ["-c", *args.arguments]
    try:
        exec(compile(args.program, "<string>", "exec"), {"__name__": "__main__"})
    finally:
        provenance.reference_imports(root, tracked)
        fence()


if __name__ == "__main__":
    main()
