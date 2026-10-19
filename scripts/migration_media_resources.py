#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Regenerate the Unicode-16 alphanumeric ranges used by native text indexing.

Use Python with Unicode 16 tables, or explicitly install unicodedata2==16.0.0 in
an isolated development environment. Runtime Catabolic never invokes Python.
The version matches the existing frozen casefold and category resources.
"""

import argparse
import json
import unicodedata
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    tables = unicodedata
    if tables.unidata_version != "16.0.0":
        import unicodedata2 as tables
    if tables.unidata_version != "16.0.0":
        raise SystemExit("Unicode 16.0.0 tables are required")
    ranges = []
    for number in range(0x110000):
        if tables.category(chr(number))[0] in ("L", "N"):
            if ranges and ranges[-1][1] + 1 == number:
                ranges[-1][1] = number
            else:
                ranges.append([number, number])
    data = (json.dumps(ranges, separators=(",", ":")) + "\n").encode()
    path = (
        Path(__file__).resolve().parents[1]
        / "crates/catabolic-store/resources/text-word-ranges.json"
    )
    if args.check:
        if path.read_bytes() != data:
            raise SystemExit("native text word ranges differ from Unicode 16.0.0")
    else:
        path.write_bytes(data)
    print(f"Unicode 16.0.0 text word ranges: {len(ranges)} verified")


if __name__ == "__main__":
    main()
