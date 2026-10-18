# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fail if a shipped crate's normal feature graph enables catabolic-store failpoints.

catabolic-store's ``failpoints`` feature adds test-only observation hooks to
native restore, native backup and journal conversion. Test builds enable it
through catabolic-cli's dev-dependency, so the workspace test build has it on.
The shipped binaries and the wheel must not. This inspects the normal and
build-dependency feature graph offline with ``cargo tree``, which resolves
metadata without compiling anything, so it does not depend on how a test build
unifies features.
"""

import subprocess
import sys

SHIPPED = ("catabolic-cli", "catabolic-python", "catabolic-dist")


def store_features(package):
    output = subprocess.run(
        [
            "cargo",
            "tree",
            "--offline",
            "--locked",
            "-e",
            "normal,build",
            "-p",
            package,
            "-f",
            "{p} {f}",
        ],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    features = set()
    found = False
    for line in output.splitlines():
        if "catabolic-store v" not in line:
            continue
        found = True
        # "{p} {f}" ends with a comma-separated feature list; "(*)" marks a repeat.
        tail = line.split(")", 1)[-1].replace("(*)", "").strip()
        features.update(feature for feature in tail.split(",") if feature)
    return found, features


def main():
    failures = []
    for package in SHIPPED:
        found, features = store_features(package)
        state = ",".join(sorted(features)) or "(none)"
        print(
            f"{package}: catabolic-store {'present' if found else 'absent'}; features {state}"
        )
        if "failpoints" in features:
            failures.append(package)
    if failures:
        print(
            "failpoints enabled in the normal feature graph of: " + ", ".join(failures)
        )
        return 1
    print("ok: no shipped crate enables catabolic-store/failpoints")
    return 0


if __name__ == "__main__":
    sys.exit(main())
