#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Check immutable schema36 inputs and reproduce only its signature/manifest.

Default is read-only verification. --output-dir must name a new directory outside
the source root. No SQL is generated or written. This is schema-only reproduction,
not independent review clearance or runtime/release qualification.
"""

import argparse
import hashlib
import importlib.util
import json
from contextlib import closing
from pathlib import Path

if __package__:
    from . import native_catalog_schema as foundation
else:
    # Direct isolated (-I) invocation does not put this script directory on sys.path.
    spec = importlib.util.spec_from_file_location(
        "native_catalog_schema", Path(__file__).with_name("native_catalog_schema.py")
    )
    foundation = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(foundation)

REPO = Path(__file__).resolve().parents[1]
RESOURCES = foundation.RESOURCE_PATH
SQL_PATH = RESOURCES / "migrations/036_native_execution_cleanup.sql"
SIGNATURE_PATH = RESOURCES / "native-schema-36.json"
PRIOR_SIGNATURE_PATH = RESOURCES / "native-schema-35.json"
MANIFEST_PATH = Path("tests/parity/native-execution-cleanup/schema-manifest.json")
# Exact schema35 prerequisites and immutable schema36 resources adopted by R5.
FIXED_INPUTS = {
    RESOURCES
    / "migrations/035_native_search.sql": "b2b68911dc9cdf60b6a318a3671996a6675779004c80102c34ba0d816a13ac56",
    PRIOR_SIGNATURE_PATH: "5b9407b72539acba0e077f9feb196cd6a6be3d73b5ce33301f7a9d06e906d555",
    SQL_PATH: "f729bf358d866cdb42daeb9cd984127b71ca420af0127e7ff8944234cf99087f",
    SIGNATURE_PATH: "5ea11ca925e2c949821a5f1d1d821f9f0a19419273ad0c3a817e53ed60f93cbe",
}


def generate(repo=REPO):
    """Return derived evidence after validating the frozen 35-to-36 boundary."""
    for path, expected in FIXED_INPUTS.items():
        if hashlib.sha256((repo / path).read_bytes()).hexdigest() != expected:
            raise ValueError(f"immutable schema input differs: {path}")
    migrations = foundation.legacy_paths(repo) + [
        repo / RESOURCES / "migrations" / name
        for name in (
            "033_native_catalog.sql",
            "034_native_curation.sql",
            "035_native_search.sql",
            "036_native_execution_cleanup.sql",
        )
    ]
    with closing(foundation.legacy_db(repo)) as db:
        db.execute("PRAGMA recursive_triggers=ON")
        for path in migrations[32:35]:
            db.executescript(path.read_text(encoding="utf-8"))
        prior = foundation.objects(db)
        if prior != json.loads((repo / PRIOR_SIGNATURE_PATH).read_text()):
            raise ValueError("schema35 prerequisite signature differs")
        old_tables = foundation.tables(db)
        generation = db.execute("SELECT generation FROM catalog_state").fetchone()
        db.executescript((repo / SQL_PATH).read_text(encoding="utf-8"))
        current = foundation.objects(db)
        by_name = {(obj[0], obj[1]): obj for obj in current}
        if any(by_name.get((obj[0], obj[1])) != obj for obj in prior):
            raise ValueError("historical DDL changed")
        if db.execute("PRAGMA foreign_key_check").fetchall():
            raise ValueError("schema36 foreign key check failed")
        if db.execute("SELECT generation FROM catalog_state").fetchone() != generation:
            raise ValueError("schema36 changed migration generation")
        if db.execute("SELECT count(*) FROM native_execution_lifetimes").fetchone() != (
            0,
        ):
            raise ValueError("schema36 backfilled execution lifetimes")
        tables = foundation.tables(db)
    signature = json.dumps(current, indent=2) + "\n"
    if signature.encode("utf-8") != (repo / SIGNATURE_PATH).read_bytes():
        raise ValueError("schema36 frozen signature differs")
    manifest = {
        "schema_version": 36,
        "prior_schema": 35,
        "first_native_schema": 33,
        "legacy_target": 32,
        "qualification_scope": "schema-only reproduction; not runtime or release qualification",
        "independent_review": "incomplete; reproduction does not close blocked review",
        "input_sha256": {
            str(path.relative_to(repo)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in [*migrations, repo / PRIOR_SIGNATURE_PATH]
        },
        "signature_sha256": hashlib.sha256(signature.encode("utf-8")).hexdigest(),
        "historical_ddl_preserved": True,
        "migration_generation_preserved": True,
        "execution_lifetimes_backfilled": False,
        "new_tables": {
            name: columns for name, columns in tables.items() if name not in old_tables
        },
        "new_objects": len(current) - len(prior),
    }
    return {
        SIGNATURE_PATH: signature,
        MANIFEST_PATH: json.dumps(manifest, indent=2) + "\n",
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="Read-only check (default)")
    mode.add_argument("--output-dir", type=Path, help="New isolated output directory")
    args = parser.parse_args(argv)
    output = args.output_dir.resolve() if args.output_dir else None
    if output is not None:
        if output.is_relative_to(REPO.resolve()) or output.exists():
            parser.error("output must be a new directory outside the source root")
    artifacts = generate()
    if output is not None:
        output.mkdir()
    for path, text in artifacts.items():
        if output is None:
            if (REPO / path).read_bytes() != text.encode("utf-8"):
                raise SystemExit(f"generated artifact differs: {path}")
        else:
            target = output / path
            target.parent.mkdir(parents=True, exist_ok=True)
            with target.open("x", encoding="utf-8") as stream:
                stream.write(text)
        print(path)


if __name__ == "__main__":
    main()
