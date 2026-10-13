#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Freeze four immutable imports from the pinned oracle; no runtime Python owner.

This deliberately does not generate query behavior, schemas, layouts or HTTP
applications. Existing native SQL/schema resources remain their sole owners.
"""

import argparse
import hashlib
import importlib
import importlib.util
import json
import sys
from pathlib import Path

REFERENCE = "453fca983222c6665a48775eef67c489c96b527e"
ROOT = Path(__file__).resolve().parents[1]
BINDINGS = {
    "python:catabolic.component_sql.OCCURRENCES_SQL": "src/catabolic/component_sql.py",
    "python:catabolic.documentation.TOPICS": "src/catabolic/documentation.py",
    "python:catabolic.graphql_query.SDL": "src/catabolic/graphql_query.py",
    "python:catabolic.http.contract.VERSION": "src/catabolic/http/contract.py",
}
GENERATED = "crates/catabolic-python/src/public_import_values.rs"
CONTRACT = "tests/parity/public-import-bindings.json"


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sha(data):
    return hashlib.sha256(data).hexdigest()


def tagged(value):
    if type(value) is str:
        return {"type": "str", "value": value}
    if type(value) is tuple:
        return {"type": "tuple", "items": [tagged(item) for item in value]}
    raise TypeError(f"unexpected immutable binding type: {type(value)}")


def metadata(value):
    result = {
        "type": type(value).__name__,
        "typed_value_sha256": sha(
            json.dumps(
                tagged(value), ensure_ascii=False, separators=(",", ":")
            ).encode()
        ),
    }
    if type(value) is str:
        result.update(utf8_bytes=len(value.encode()), utf8_sha256=sha(value.encode()))
    else:
        assert len(value) == 49
        assert all(type(row) is tuple and len(row) == 4 for row in value)
        assert all(type(item) is str for row in value for item in row)
        result.update(rows=49, row_type="tuple", row_width=4, element_type="str")
    return result


def select_rows(inventory):
    assert inventory["reference_commit"] == REFERENCE
    rows = [row for row in inventory["rows"] if row["id"] in BINDINGS]
    assert len(rows) == len(BINDINGS) == 4
    assert {row["id"] for row in rows} == set(BINDINGS)
    for row in rows:
        assert row["classification"] == "preserve-repository-import-binding"
        assert row["source"] == BINDINGS[row["id"]]
        assert row["signature"]["kind"] == "module-or-value-or-reexport"
    return rows


def capture(source):
    assert sys.version_info[:3] == (3, 14, 6), "use the pinned Python 3.14.6 oracle"
    source = source.resolve()
    provenance = sibling("qualify_native_embedding")
    before = provenance.reference_provenance(source, REFERENCE)
    inventory_path = ROOT / "tests/parity/python-api.json"
    rows = select_rows(json.loads(inventory_path.read_text()))
    frozen = json.loads((ROOT / "tests/parity/inventory.json").read_text())[
        "resource_sha256"
    ]
    for relative in BINDINGS.values():
        assert before["tracked_source_sha256"][relative] == frozen[relative]
    assert not any(
        name == "catabolic" or name.startswith("catabolic.") for name in sys.modules
    )
    sys.path.insert(0, str(source / "src"))
    values = {}
    for identifier in BINDINGS:
        module, name = identifier.removeprefix("python:").rsplit(".", 1)
        values[identifier] = getattr(importlib.import_module(module), name)
    imports = provenance.reference_imports(source, before["tracked_source_sha256"])
    assert provenance.reference_provenance(source, REFERENCE) == before
    sql, topics, sdl, version = values.values()
    # Reuse the existing native owners only after proving exact bytes.
    store = ROOT / "crates/catabolic-store/resources"
    native_sql = json.loads((store / "reference.json").read_text())
    matches = [
        row
        for row in native_sql["views"]
        if row["name"] == "catalog_component_occurrences"
    ]
    assert len(matches) == 1 and matches[0]["sql"] == sql
    local_sdl = (store / "schema.graphql").read_bytes()
    from catabolic.graphql_query import LOCAL_SDL

    assert local_sdl == LOCAL_SDL.encode()
    assert local_sdl.startswith(sdl.encode()) and len(local_sdl) > len(sdl.encode())
    owners = {
        "python:catabolic.component_sql.OCCURRENCES_SQL": {
            "resource": "crates/catabolic-store/resources/reference.json",
            "selector": "views[name=catalog_component_occurrences].sql",
        },
        "python:catabolic.documentation.TOPICS": {
            "resource": GENERATED,
            "selector": "TOPICS",
        },
        "python:catabolic.graphql_query.SDL": {
            "resource": "crates/catabolic-store/resources/schema.graphql",
            "selector": f"utf8_bytes[0:{len(sdl.encode())}]",
            "resource_sha256": sha(local_sdl),
            "resource_utf8_bytes": len(local_sdl),
        },
        "python:catabolic.http.contract.VERSION": {
            "resource": GENERATED,
            "selector": "HTTP_CONTRACT_VERSION",
        },
    }
    contract = {
        "format": 1,
        "reference_commit": REFERENCE,
        "oracle_python": ".".join(map(str, sys.version_info[:3])),
        "inventory_sha256": sha(inventory_path.read_bytes()),
        "scope": "Four immutable companion-namespace values only; no Python default replacement or callable/SQL/schema behavior qualification",
        "oracle_imports": imports,
        "rows": [
            {
                "id": row["id"],
                "classification": row["classification"],
                "source": row["source"],
                "source_sha256": before["tracked_source_sha256"][row["source"]],
                "value": metadata(values[row["id"]]),
                "native_owner": owners[row["id"]],
            }
            for row in rows
        ],
    }

    # ASCII Rust literals avoid language-specific JSON escaping differences.
    def literal(text):
        return (
            '"'
            + "".join(
                {"\\": "\\\\", '"': '\\"', "\n": "\\n", "\r": "\\r", "\t": "\\t"}.get(
                    c, c if " " <= c <= "~" else f"\\u{{{ord(c):x}}}"
                )
                for c in text
            )
            + '"'
        )

    generated = (
        "// SPDX-FileCopyrightText: 2026 The Catabolic Contributors\n"
        "// SPDX-License-Identifier: MIT\n"
        "// Generated by scripts/generate_public_import_bindings.py; do not edit.\n"
        f"// Oracle: {REFERENCE}; value/type/source hashes: tests/parity/public-import-bindings.json\n"
        f"pub const GRAPHQL_PUBLIC_SDL_BYTES: usize = {len(sdl.encode())};\n"
        f"pub const HTTP_CONTRACT_VERSION: &str = {literal(version)};\n"
        "pub const TOPICS: &[(&str, &str, &str, &str)] = &[\n"
        + "".join(
            "    (\n"
            + "".join(f"        {literal(item)},\n" for item in row)
            + "    ),\n"
            for row in topics
        )
        + "];\n"
    )
    return values, contract, generated


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--snapshot", action="store_true")
    args = parser.parse_args()
    if not __debug__:
        raise RuntimeError("source qualification requires assertions")
    values, contract, generated = capture(args.source)
    outputs = {
        GENERATED: generated.encode(),
        CONTRACT: (json.dumps(contract, ensure_ascii=False, indent=2) + "\n").encode(),
    }
    for relative, expected in outputs.items():
        path = ROOT / relative
        if args.check or args.snapshot:
            assert path.read_bytes() == expected, (
                f"public import resource drift: {relative}"
            )
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(expected)
    if args.snapshot:
        print(
            json.dumps(
                {identifier: tagged(value) for identifier, value in values.items()},
                ensure_ascii=False,
            )
        )
    else:
        print(
            f"{'Verified' if args.check else 'Generated'} 4 exact immutable bindings; 49 tuple rows; 2 reused native resources"
        )


if __name__ == "__main__":
    main()
