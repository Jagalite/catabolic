#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Capture immutable Rust resources from the verified, frozen Python reference."""

import argparse
import json
import sys
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from scripts.migration_contracts import REFERENCE, sha, verify_source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    source = args.source.resolve()
    inventory = json.loads((root / "tests/parity/inventory.json").read_text())
    verify_source(source, inventory)
    sys.path[:0] = [str(source / "src")]
    from catabolic.graphql_query import LOCAL_SDL
    from catabolic.media import describe_types
    from catabolic.migration import _expected_schema, load_migrations
    from catabolic.sql_query import FUNCTIONS, VIEWS

    resources = root / "crates/catabolic-store/resources"
    migrations = load_migrations()
    generated = {}
    for migration in migrations:
        generated[f"migrations/{migration.filename}"] = migration.sql.encode()
    generated["reference.json"] = (
        json.dumps(
            {
                "reference_commit": REFERENCE,
                "migrations": [step.description() for step in migrations],
                "schemas": {
                    str(version): _expected_schema(migrations, version)
                    for version in range(1, 33)
                },
                "views": [
                    {"name": name, "description": description, "sql": sql}
                    for name, (description, sql) in VIEWS.items()
                ],
                "functions": sorted(FUNCTIONS),
                "casefold": {
                    chr(code): chr(code).casefold()
                    for code in range(0x110000)
                    if chr(code).casefold() != chr(code)
                },
                "media_types": describe_types(),
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n"
    ).encode()
    generated["schema.graphql"] = LOCAL_SDL.encode()
    generated["hashes.json"] = (
        json.dumps({name: sha(data) for name, data in generated.items()}, indent=2)
        + "\n"
    ).encode()
    for name, data in generated.items():
        path = resources / name
        if args.check:
            if not path.is_file() or path.read_bytes() != data:
                raise ValueError(f"frozen Rust resource drift: {name}")
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
    print(f"{'Verified' if args.check else 'Captured'} {len(generated)} resources")


if __name__ == "__main__":
    main()
