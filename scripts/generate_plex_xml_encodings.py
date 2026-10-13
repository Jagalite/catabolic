#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Pin the frozen CPython/Expat XML single-byte fallback, without a runtime codec.

Run with the frozen reference's Python 3.14.6 and src first on PYTHONPATH. The
output records static unknown-encoding maps, not Python's general-purpose codec
semantics: Expat asks Python to decode bytes(range(256)) once, then maps each
wire byte independently. Thus utf8 aliases, HZ, and escape codecs are deliberately
not interpreted as their ordinary multibyte/stateful Python codecs.
"""

import argparse
import codecs
import encodings
import encodings.aliases
import hashlib
import json
import pkgutil
import platform
import re
import subprocess
import sys
import warnings
from pathlib import Path
from xml.parsers import expat

from catabolic.consumer_adapters import ConsumerError, Plex

REVISION = "453fca983222c6665a48775eef67c489c96b527e"
ROOT = Path(__file__).resolve().parents[1]


def normalize(name):
    return re.sub("[-_]+", "_", name.lower()).strip("_")


def parse(raw):
    adapter = Plex({"endpoint": "http://127.0.0.1:9"})
    adapter.call = lambda *_args, **_kwargs: raw
    return adapter.xml("/")


def wire_cases():
    result = []

    def record(label, raw):
        entry = {"label": label, "raw_hex": raw.hex()}
        try:
            entry["value"] = parse(raw).get("a", "")
        except (ConsumerError, LookupError, UnicodeError, ValueError, TypeError) as exc:
            entry["error"] = type(exc).__name__
        result.append(entry)

    for wire in (
        "utf-8",
        "utf-8-sig",
        "utf-16",
        "utf-16-le",
        "utf-16-be",
        "utf-32",
        "utf-32-le",
        "utf-32-be",
    ):
        for declared in (
            None,
            "UTF-8",
            "utf8",
            "UTF-16",
            "UTF-16LE",
            "UTF-16BE",
            "UTF-32",
            "ISO-8859-1",
            "unknown-fixture",
        ):
            prefix = f'<?xml version="1.0" encoding="{declared}"?>' if declared else ""
            record(
                f"{wire}: {declared}", (prefix + '<x a="Café 漢字😀"/>').encode(wire)
            )
        for separator in (" ", "\t", "\r", "\n", "\r\n"):
            record(
                f"{wire}: declaration whitespace {separator!r}",
                (f'<?xml{separator}version="1.0"?><x a="unicode 😀"/>').encode(wire),
            )
            record(
                f"{wire}: leading whitespace {separator!r}",
                (separator + '<x a="unicode 😀"/>').encode(wire),
            )
    for wire, bom in (
        ("utf-8", b"\xef\xbb\xbf"),
        ("utf-16-le", b"\xff\xfe"),
        ("utf-16-be", b"\xfe\xff"),
    ):
        record(f"{wire}: double BOM", bom + bom + '<x a="value"/>'.encode(wire))
        record(f"{wire}: empty BOM", bom)
        raw = bom + '<x a="unicode 😀"/>'.encode(wire)
        for trim in (1, 2, 3):
            record(f"{wire}: truncated {trim}", raw[:-trim])
    for wire, declared, sample in (
        ("cp1252", "windows--1252", "Café€"),
        ("latin1", "ISO-8859-1", "Café\x80"),
        ("cp1251", "WINDOWS_1251", "Привет"),
        ("mac_roman", "mac-roman", "Café"),
        ("shift_jis", "SHIFT_JIS", "漢字"),
        ("gb18030", "GB18030", "漢字"),
    ):
        raw = (f'<?xml version="1.0" encoding="{declared}"?><x a="{sample}"/>').encode(
            wire
        )
        record(f"{wire}: declared sample", raw)
        record(f"{wire}: UTF8 BOM plus declared sample", b"\xef\xbb\xbf" + raw)
    for raw in (
        b"\xff",
        b"\xef\xbb",
        b"\xff\xfe<\x00x",
        b"\xff\xfe<\x00x\x00>\x00\x00\xd8<\x00/\x00x\x00>\x00",
        b"<x a='\xc0\xaf'/>",
        b"<x a='\xed\xa0\x80'/>",
    ):
        record("malformed " + raw.hex(), raw)
    return result


def generate():
    if sys.version_info[:3] != (3, 14, 6) or expat.EXPAT_VERSION != "expat_2.8.2":
        raise RuntimeError("Use the pinned Python 3.14.6 / Expat 2.8.2 oracle")
    if platform.system() != "Linux":
        raise RuntimeError("Use the pinned Linux standard-library codec registry")
    import catabolic.consumer_adapters as owner

    source = Path(owner.__file__).resolve()
    # The caller must supply the immutable frozen checkout; report its exact
    # source hash rather than silently substituting this working tree.
    if source.is_relative_to((ROOT / "src").resolve()):
        raise RuntimeError("Put the frozen reference src first on PYTHONPATH")
    reference = source.parents[2]
    git_root = Path(
        subprocess.check_output(
            ["git", "-C", str(reference), "rev-parse", "--show-toplevel"], text=True
        ).strip()
    ).resolve()
    if git_root != reference:
        raise RuntimeError("Frozen reference must have its own Git root")
    revision = subprocess.check_output(
        ["git", "-C", str(reference), "rev-parse", "HEAD"], text=True
    ).strip()
    if revision != REVISION:
        raise RuntimeError("Wrong frozen reference revision: " + revision)
    frozen_source = subprocess.check_output(
        [
            "git",
            "-C",
            str(reference),
            "show",
            f"{REVISION}:src/catabolic/consumer_adapters.py",
        ]
    )
    if frozen_source != source.read_bytes():
        raise RuntimeError("Frozen consumer adapter source has changed")
    names = (
        set(encodings.aliases.aliases)
        | set(encodings.aliases.aliases.values())
        | {m.name for m in pkgutil.iter_modules(encodings.__path__)}
    )
    names |= {codecs.lookup(n).name for n in list(names) if lookup(n)}
    tables, aliases, cases, outcomes, rejected = [], {}, {}, [], {}
    for name in sorted(names):
        if not re.fullmatch("[A-Za-z][A-Za-z0-9._-]*", name):
            continue
        prefix = f'<?xml version="1.0" encoding="{name}"?><x a="'.encode()
        try:
            mapping = bytes(range(256)).decode(name, "replace")
            if len(mapping) != 256:
                raise ValueError("not a 256-character fallback table")
            parse(prefix + b'"/>')
        except (ConsumerError, LookupError, UnicodeError, ValueError, TypeError) as exc:
            rejected[name] = type(exc).__name__
            continue
        table = [ord(c) if c != "\ufffd" else 0xFFFFFFFF for c in mapping]
        if table not in tables:
            tables.append(table)
        normalized = normalize(name)
        index = tables.index(table)
        if normalized in aliases and aliases[normalized] != index:
            raise AssertionError("inconsistent normalized alias: " + name)
        aliases[normalized] = index
        # Independent expected parser outcomes for every individual wire byte.
        expected = []
        for byte in range(256):
            try:
                expected.append(parse(prefix + bytes([byte]) + b'"/>').get("a"))
            except ConsumerError:
                expected.append(None)
        if expected not in outcomes:
            outcomes.append(expected)
        cases[name] = outcomes.index(expected)
    lines = [
        "// SPDX-FileCopyrightText: 2026 The Catabolic Contributors",
        "// SPDX-License-Identifier: MIT",
        "// Generated by scripts/generate_plex_xml_encodings.py; do not edit.",
        f"// Frozen reference {REVISION}; CPython {platform.python_version()}; {expat.EXPAT_VERSION}.",
        "// Each table is Expat's static unknown-encoding map, not a general codec.",
        "#[rustfmt::skip]",
        "pub(super) const ALIASES: &[(&str, usize)] = &[",
    ]
    lines.extend(f'    ("{name}", {index}),' for name, index in sorted(aliases.items()))
    lines.extend(
        [
            "];",
            "#[rustfmt::skip]",
            f"pub(super) static TABLES: [[u32; 256]; {len(tables)}] = [",
        ]
    )
    for table in tables:
        lines.append("    [")
        for offset in range(0, 256, 16):
            lines.append(
                "        "
                + ", ".join(f"0x{n:x}" for n in table[offset : offset + 16])
                + ","
            )
        lines.append("    ],")
    lines.append("];")
    resource = {
        "reference": REVISION,
        "python": platform.python_version(),
        "expat": expat.EXPAT_VERSION,
        "consumer_adapter_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
        "scope": "Default Linux CPython standard-library codec registry; no custom codec registrations",
        "tables": len(tables),
        "normalized_aliases": len(aliases),
        "cases": cases,
        "outcomes": outcomes,
        "rejected_declarations": rejected,
        "wire_cases": wire_cases(),
    }
    return "\n".join(lines) + "\n", json.dumps(
        resource, ensure_ascii=True, sort_keys=True, indent=2
    ) + "\n"


def lookup(name):
    try:
        return codecs.lookup(name)
    except LookupError:
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", DeprecationWarning)
        rust, oracle = generate()
    for path, content in [
        (ROOT / "crates/catabolic-runtime/src/consumers/xml_encodings.rs", rust),
        (ROOT / "tests/fixtures/plex_xml_encodings.json", oracle),
    ]:
        if args.check:
            if path.read_text() != content:
                raise RuntimeError("Generated resource differs: " + str(path))
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
    print(
        "Frozen XML encoding resources " + ("verified" if args.check else "generated")
    )


if __name__ == "__main__":
    main()
