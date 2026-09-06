# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Reject edits/deletions of artifacts already present in a base Git commit."""

import argparse
import json
import subprocess
from pathlib import Path

ROOT = "src/catabolic/interchange/releases/"


def check(base):
    # Resolve to a commit before using the ref in path-bearing Git arguments.
    commit = subprocess.check_output(
        ["git", "rev-parse", "--verify", "--end-of-options", base + "^{commit}"],
        text=True,
    ).strip()
    paths = subprocess.check_output(
        ["git", "ls-tree", "-r", "--name-only", commit, "--", ROOT], text=True
    ).splitlines()
    changed = []
    for path in paths:
        previous = subprocess.check_output(["git", "show", f"{commit}:{path}"])
        current = Path(path)
        if not current.is_file():
            changed.append(path)
        elif path == ROOT + "index.json":
            old = json.loads(previous)
            new = json.loads(current.read_text())
            if any(new.get(key) != value for key, value in old.items()):
                changed.append(path)
        elif current.read_bytes() != previous:
            changed.append(path)
    if changed:
        raise SystemExit(
            "Released specification artifacts are immutable; add a new version: "
            + ", ".join(changed)
        )
    print(f"Checked {len(paths)} existing release artifacts against {commit[:12]}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base")
    check(parser.parse_args().base)
