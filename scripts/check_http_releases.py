# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Preserve published artifacts and operation IDs within each major API version."""

import json
import subprocess
import sys
from pathlib import Path


def check(base):
    prefix = "src/catabolic/http/releases/"
    paths = subprocess.check_output(
        ["git", "ls-tree", "-r", "--name-only", base, "--", prefix], text=True
    ).splitlines()
    manifests = [
        json.loads(path.read_text()) for path in Path(prefix).glob("*/manifest.json")
    ]
    for path in paths:
        before = subprocess.check_output(["git", "show", f"{base}:{path}"])
        current = Path(path)
        if not current.is_file() or current.read_bytes() != before:
            raise SystemExit(
                f"Published HTTP artifact changed: {path}; add a new contract version"
            )
        if path.endswith("/manifest.json"):
            old = json.loads(before)
            for new in manifests:
                if new["version"].split(".")[0] != old["version"].split(".")[0]:
                    continue
                if tuple(map(int, new["version"].split("."))) < tuple(
                    map(int, old["version"].split("."))
                ):
                    continue
                for route, identifier in old["operations"].items():
                    if new["operations"].get(route) != identifier:
                        raise SystemExit(
                            f"Operation removed or renamed within API major version: {route} ({identifier})"
                        )
    print(
        f"Preserved {len(paths)} published HTTP contract artifacts and their operation IDs"
    )


if __name__ == "__main__":
    check(sys.argv[1])
