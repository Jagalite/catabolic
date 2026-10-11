#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Regenerate subtitle numeric/cue evidence from the pinned Python interpreter.

Python 3.11 (UCD14) and 3.12 (UCD15) reject later decimal blocks; the native
contract follows the frozen 3.14.6/UCD16 reference, independent of host tables.
The original processing.text_payload function is executed without dependencies.
"""

import argparse
import ast
import itertools
import json
import math
import platform
import re
import unicodedata
from pathlib import Path

# This generator runs only during qualification, never in the native runtime.
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument(
    "--reference", type=Path, required=True, help="frozen processing.py"
)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
if platform.python_version() != "3.14.6" or unicodedata.unidata_version != "16.0.0":
    raise SystemExit("frozen oracle requires CPython 3.14.6 / Unicode 16.0.0")
# Execute only the exact frozen function AST, avoiding optional dependencies.
source = args.reference.read_text()
node = next(
    n
    for n in ast.parse(source).body
    if isinstance(n, ast.FunctionDef) and n.name == "text_payload"
)
namespace = {"re": re}
exec(
    compile(ast.Module(body=[node], type_ignores=[]), str(args.reference), "exec"),
    namespace,
)
digits = [chr(cp) for cp in range(0x110000) if unicodedata.category(chr(cp)) == "Nd"]
spellings = [
    "".join(chars)
    for length in range(5)
    for chars in itertools.product("01_.e+-", repeat=length)
]
spellings += [f"{d}_0.5e+0_1" for d in digits]
spellings += [
    " ١_٢.٣e+٤ ",
    "\u001c1\u001c",
    "\u001f1\u001f",
    "\u00851\u0085",
    "\u00a01\u00a0",
    "\u20071\u202f",
    "+١_٢.٣_٤e-٥_٦",
    "in_f",
    "Na_N",
    "1__2",
    "_1",
    "1_",
    "1._0",
    "1_.0",
    ".١",
    "١.",
    "١_e١",
    "١e_١",
    "١e+_١",
    "①",
    "²",
    "𝟜",
    "-0",
    "+0",
    "-0.0",
    "-nan",
    "+NaN",
    "Infinity",
    "infinity",
    "INF",
    "0x1",
    "𝟣_٢",
    "1e999",
    "-1e999",
    "5e-324",
    "-5e-324",
    "1_2_3.4_5_6e-7_8",
    "\u22121",
    "1\x002",
    "",
]
spellings = list(dict.fromkeys(spellings))
rows = []
for text in spellings:
    try:
        value = float(text)
    except ValueError:
        row = {"text": text, "disposition": "rejected"}
    else:
        row = (
            {"text": text, "disposition": "finite", "value": value}
            if math.isfinite(value)
            else {"text": text, "disposition": "nonfinite"}
        )
    rows.append(row)
subtitles = []
for extension in (".srt", ".vtt"):
    cues = [
        "٠٠:٠٠:٠١,٥٠٠",
        "00:00:1_0.25",
        "00:00:-١_٠.٢٥",
        "00:00:+.٥e١",
        "00:00:𝟜_٢",
        "00:\u00a0 ١ \u00a0:2",
        "00:00:1__0",
        "00:00:1._0",
        "00:\u001f1:2",
    ] + [f"00:00:{d},5" for d in digits]
    text = "\n\n".join(f"{i}\n{stamp} --> 99\ncue {i}" for i, stamp in enumerate(cues))
    subtitles.append(
        {
            "extension": extension,
            "text": text,
            "expected": namespace["text_payload"](text.encode(), extension),
        }
    )
data = (
    json.dumps(
        {
            "python": platform.python_version(),
            "unicode": unicodedata.unidata_version,
            "reference_function": "frozen catabolic.processing.text_payload",
            "decimal_digits": len(digits),
            "numbers": rows,
            "subtitles": subtitles,
        },
        ensure_ascii=False,
        separators=(",", ":"),
        allow_nan=False,
    )
    + "\n"
)
path = Path(__file__).with_name("subtitle-float-oracle.json")
if args.check:
    if path.read_text() != data:
        raise SystemExit("subtitle float oracle differs from frozen Python")
else:
    path.write_text(data)
print(
    f"Verified {len(rows)} numeric spellings and {len(digits)} decimal digits against {platform.python_version()} / UCD {unicodedata.unidata_version}"
)
