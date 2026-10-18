# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Record frozen scan-filter regex behavior for the native dialect fixture.

Run with the pinned oracle interpreter and the frozen reference on sys.path:

    PYTHONPATH=REFERENCE/src python scripts/generate_scan_regex_dialect.py OUT

For every pattern the frozen ``scan_policy.patterns`` admission result is
recorded, and for every admitted pattern ``compile_pattern(p).search(path)``
is recorded against every sample path. Nothing here is native behavior.
"""

import json
import sys

import regex

from catabolic import scan_policy
from catabolic.domain import CatabolicError

PATHS = [
    "Movies/Alien (1979)/Alien.mkv",
    "Movies/Alien (1979)/alien.sample.MKV",
    "Movies/aliens/ALIENS.mp4",
    "Music/Café Été/01 – Straße.flac",
    "Music/STRASSE/02 strasse.flac",
    "Music/ΑΒΓ/αβγ.flac",
    "Shows/١٢٣/Episode ٤.mkv",
    "Shows/Show 1/S01E02.mkv",
    "Shows/Show_1/s01e02.MKV",
    "Books/a-b/aa.epub",
    "Books/ab/b.epub",
    "Books/aab/x.epub",
    "Photos/😀/IMG 0001.jpg",
    "Photos/tab\there.jpg",
    "kitchen/sink.txt",
    "a",
    "",
    # Sol round 1 divergences: Turkish and long-s folding, Unicode 17 class
    # members, literal angle brackets, flag scope, backreference shapes.
    "İstanbul/ılık.mkv",
    "Idea/idea.mkv",
    "sſ/x",
    "\U00011DE0",
    "\U0001E6C0",
    "Movies/<Alien>/x.mkv",
    "aba2",
    "abb",
    "abC",
    "abc",
    "a\n",
    "aab",
    "Alien.mkv\n",
    "Straße/STRASSE/ẞ",
    "Kelvin/K/k",
    # Unicode 17 case pair (Sol round 2).
    "\uA7CF",
]

PATTERNS = [
    # Ordinary and anchors.
    r"\.mkv$",
    r"\.mkv\Z",
    r"\Aмovies",
    r"\AMovies/",
    r"^Movies/",
    r"(?i)\.MKV$",
    r"(?i:alien)",
    r"(?-i:Alien)",
    r"sample",
    r"\bAlien\b",
    r"\Bien\b",
    r"/$",
    # Classes and Unicode.
    r"\d",
    r"\d{3}",
    r"[0-9]{3}",
    r"\w+\.flac$",
    r"^\w+$",
    r"\s",
    r"\S+$",
    r"[[:alpha:]]+/",
    r"[[:digit:]]",
    r"[[:upper:]]{3}",
    r"\p{L}",
    r"\p{Lu}{3}",
    r"\p{Greek}",
    r"\p{Script=Greek}",
    r"\P{L}",
    r"\p{N}",
    r"\N{LATIN SMALL LETTER E WITH ACUTE}",
    r"\xe9",
    r"\u00e9",
    r"\U0001F600",
    r"\x41",
    r"\t",
    r"\101",
    r"\0",
    r"[\w-]+/",
    r"[]a]",
    r"[^]a]",
    r"[a-z&&[^c]]",
    r"[\d\s]",
    # Case folding.
    r"(?i)straße",
    r"(?i)STRASSE",
    r"(?i)é",
    r"(?i)αβγ",
    r"(?fi)straße",
    # Quantifiers.
    r"a{,2}b",
    r"^a{2,}b",
    r"a{2}",
    r"x{0}",
    r"a*+b",
    r"a++b",
    r"(?>a+)b",
    r"a?+b",
    r"a{1,2}?b",
    # Groups and references.
    r"(?P<n>a)(?P=n)",
    r"(?<n>a)\k<n>",
    r"(a)\1",
    r"(?:Alien|Aliens)",
    r"(?#comment)Alien",
    r"(?x) A l i e n  # verbose",
    r"(?|(a)|(b))\1",
    # Flags.
    r"(?s)Show.S01",
    r"(?m)^a$",
    r"(?a)\w+\.flac$",
    r"(?u)\w+\.flac$",
    r"(?L)\w",
    r"(?V0)a",
    r"(?V1)a",
    r"(?r)a",
    r"(?e)a",
    r"(?b)a",
    # Lookaround.
    r"(?<=/)Alien",
    r"(?<!x)Alien",
    r"Alien(?=\.mkv)",
    r"Alien(?!\.mkv)",
    r"(?<=a+)b",
    r"(?<=Movies|Music)/",
    # Regex-module extensions.
    r"\K",
    r"\X",
    r"(?(1)a|b)",
    r"(a)?(?(1)b|c)",
    r"\m",
    r"\M",
    r"\G",
    r"\L<x>",
    r"(?:a|b){e<=1}",
    r"(?p)a",
    r"(?w)\bAlien\b",
    r"\h",
    r"\R",
    r"\z",
    # Errors.
    r"(",
    r"a**",
    r"[z-a]",
    r"(?P<1>a)",
    "\\",
    r"(?<",
    r"\p{NoSuchProperty}",
    r"*a",
    r"a{2,1}",
    r"(?P=missing)",
    # Sol round 1 divergences.
    r"(*ACCEPT)abc",
    r"(*FAIL)",
    r"(?i)i",
    r"(?i)I",
    r"(?i)İ",
    r"(?i)ı",
    r"(?i)idea",
    r"(?i)[a-z]+\.mkv",
    r"(?i)[^a-z]",
    r"(?i)[h-j]",
    r"(?i)k",
    r"(?i)ß",
    r"(?i)strasse",
    r"(?i)(s)\1",
    r"\<Alien\>",
    r"\x3cAlien",
    r"(a)()()()()()()()()()()(b)\1\x32",
    r"(a)\1\x32",
    r"(a(?i)b)c",
    r"((?s).).",
    r"(?i-i)a",
    r"(?P<n>a)(?P<n>b)\1",
    r"(?<=(a)\1)b",
    r"(a\1)",
    r"(?i:alien)\.mkv",
    r"(?s:a.)",
    r"(?is)a.b",
    r"(?i)(?s)A.",
    r"(?si)x",
    r"[\d_]+",
    r"[^\W]",
    r"\W",
    r"\D",
    r"\B",
    r"\b",
    r"a{3,}",
    r"^$",
    r"\d\d",
    r"\w+/\w+",
    r"a(?=b)*",
    r"^*a",
    r"(?<!a)b",
    # Sol round 2: a Unicode 17 case pair outside Python's case mappings.
    r"(?i)\uA7CE",
    r"(?i)[^\uA7CE]",
    r"(?i)[\uA7CD-\uA7CE]",
    r"\uA7CE",
    # Sol round 3: captures inside lookahead backtrack differently.
    r"(?=(a|ab).)\1c",
    r"(?=(?P<n>a))\w",
    r"(?!(a))b",
    # Sol round 4: repeated captures and assertions inside lookbehind.
    r"^(a+)+\1$",
    r"(a)+\1",
    r"(?:(a)\1)+",
    r"(?<=(?<=a)a+)b",
    r"(?<=\ba)b",
    r"(?<=^a)b",
    # Sol round 5: composed repeat widths beyond any path.
    r"(a{100})(\1{100})(\2{100})(\3{100})(\4{100})(\5{100})(\6{100})(\7{100})(\8{100})(\9{100})",
    r"a{1000}b",
    # Sol round 6: zero-width repeats bounded by composed repeat counts.
    r"(?>){1000000000}",
    r"(?:(?:a{100}){100}){100}",
    r"(?:a{100}){100}",
    # Sol round 7: zero-width work multiplied inside consuming repeats.
    r"\A(?:(?>){100000}.)*\Z",
    r"(?:(?>){10}.)*x",
    r"(\w+/)*\w+\.mkv",
]


def main(out):
    cases = []
    for pattern in PATTERNS:
        try:
            scan_policy.patterns([pattern])
        except CatabolicError as exc:
            cases.append({"pattern": pattern, "frozen": "refused", "error": str(exc)})
            continue
        except Exception as exc:  # noqa: BLE001 - frozen admission leaks these
            cases.append(
                {"pattern": pattern, "frozen": "crashed", "error": type(exc).__name__}
            )
            continue
        compiled = scan_policy.compile_pattern(pattern)
        cases.append(
            {
                "pattern": pattern,
                "frozen": "admitted",
                "matches": [bool(compiled.search(path)) for path in PATHS],
            }
        )
    document = {
        "contract": "native-scan-regex-dialect-v1",
        "oracle": {
            "python": ".".join(map(str, sys.version_info[:3])),
            "regex": regex.__version__,
            "reference_commit": "453fca983222c6665a48775eef67c489c96b527e",
        },
        "paths": PATHS,
        "cases": cases,
    }
    with open(out, "w", encoding="utf-8") as stream:
        json.dump(document, stream, ensure_ascii=False, indent=1)
        stream.write("\n")


if __name__ == "__main__":
    main(sys.argv[1])
