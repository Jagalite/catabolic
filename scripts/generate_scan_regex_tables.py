# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Record frozen regex VERSION0 class membership and case-insensitive matching.

Run with the pinned oracle interpreter (the `regex` version frozen Python
uses):

    python -I scripts/generate_scan_regex_tables.py OUT

For every code point, ``\\w``, ``\\d`` and ``\\s`` membership is recorded as
ranges. For every code point ``c`` whose case-insensitive literal
``(?i)c`` matches anything besides ``c`` itself, the exact set it matches is
recorded. The native translator expands classes and case-insensitive
matching from these tables instead of using its regex engine's own Unicode
data, so the two engines agree even where their Unicode versions or folding
rules differ (for example Turkish dotted and dotless i).
"""

import json
import sys
from collections import defaultdict

import regex

MAX = 0x110000


def valid(cp):
    return not 0xD800 <= cp <= 0xDFFF


def ranges(predicate):
    out = []
    start = None
    for cp in range(MAX + 1):
        inside = cp < MAX and valid(cp) and predicate(chr(cp))
        if inside and start is None:
            start = cp
        elif not inside and start is not None:
            out.append([start, cp - 1])
            start = None
    return out


def classes():
    tables = {}
    for name, pattern in (("word", r"\w"), ("digit", r"\d"), ("space", r"\s")):
        compiled = regex.compile(pattern, regex.V0)
        tables[name] = ranges(lambda c, p=compiled: p.fullmatch(c) is not None)
    return tables


def related(c):
    """Single code points case-related to c by any Unicode case mapping."""
    out = {c}
    for mapped in (c.lower(), c.upper(), c.casefold(), c.title()):
        out.add(mapped[0])
        if len(mapped) == 1:
            out.add(mapped)
    return out


def folds():
    # Union every code point with its case relations, then test each pair in
    # a component with the frozen engine; the engine decides, not Python.
    parent = list(range(MAX))

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    for cp in range(MAX):
        if not valid(cp):
            continue
        for other in related(chr(cp)):
            a, b = find(cp), find(ord(other))
            if a != b:
                parent[a] = b
    components = defaultdict(list)
    for cp in range(MAX):
        if valid(cp):
            components[find(cp)].append(cp)
    table = {}
    for members in components.values():
        if len(members) < 2:
            continue
        for cp in members:
            pattern = regex.compile("(?i)" + regex.escape(chr(cp)), regex.V0)
            matched = [x for x in members if pattern.fullmatch(chr(x))]
            if matched != [cp]:
                table[str(cp)] = matched
    return table


def main():
    out = sys.argv[1]
    document = {
        "contract": "native-scan-regex-tables-v1",
        "oracle": {
            "python": sys.version.split()[0],
            "regex": regex.__version__,
        },
        **classes(),
        "fold": folds(),
    }
    with open(out, "w", encoding="utf-8") as handle:
        json.dump(document, handle, separators=(",", ":"), sort_keys=True)
        handle.write("\n")


if __name__ == "__main__":
    main()
