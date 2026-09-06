# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Publish root guides as package data; --check verifies without writing."""

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from catabolic.documentation import TOPICS  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    destination = ROOT / "src/catabolic/guides"
    changed = []
    for _, filename, _, _ in TOPICS:
        source = (ROOT / filename).read_bytes()
        target = destination / filename
        if not target.exists() or target.read_bytes() != source:
            changed.append(filename)
            if not args.check:
                destination.mkdir(parents=True, exist_ok=True)
                target.write_bytes(source)
    if changed:
        print(
            ("Stale bundled guides: " if args.check else "Updated bundled guides: ")
            + ", ".join(changed)
        )
        if args.check:
            print("Run python scripts/sync_docs.py after editing root guides.")
    else:
        print("Bundled guides match their sources.")
    return 1 if args.check and changed else 0


if __name__ == "__main__":
    raise SystemExit(main())
