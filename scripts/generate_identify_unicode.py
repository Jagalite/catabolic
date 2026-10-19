#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Freeze Python 3.14.6 Unicode word/decimal semantics for TMDB ranking."""

import json
import sys
import unicodedata
from pathlib import Path

assert sys.version.split()[0] == "3.14.6"
assert unicodedata.unidata_version == "16.0.0"


def ranges(predicate):
    result = []
    for point in range(0x110000):
        if predicate(chr(point)):
            if result and result[-1][1] == point - 1:
                result[-1][1] = point
            else:
                result.append([point, point])
    return result


value = {
    "unicode_version": unicodedata.unidata_version,
    "word": ranges(lambda c: c.isalnum() or c == "_"),
    "decimal": ranges(lambda c: unicodedata.category(c) == "Nd"),
}
path = (
    Path(__file__).resolve().parents[1]
    / "crates/catabolic-runtime/resources/identify-unicode.json"
)
path.write_text(json.dumps(value, separators=(",", ":")) + "\n")
