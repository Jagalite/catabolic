# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Export deterministic, database-independent public HTTP and GraphQL artifacts."""

import argparse
import hashlib
import json
import tempfile
from pathlib import Path

from graphql import build_schema, lexicographic_sort_schema, print_schema

from catabolic.graphql_query import SDL
from catabolic.http.app import create_app
from catabolic.http.contract import VERSION
from catabolic.store import Store

ROOT = Path(__file__).resolve().parents[1]
DESTINATION = ROOT / "src/catabolic/http/releases" / VERSION


def artifacts():
    with tempfile.TemporaryDirectory() as directory:
        database = Path(directory) / "catalog.sqlite3"
        Store.initialize(database)
        schema = create_app(database).openapi()
    values = {
        "openapi.json": json.dumps(schema, indent=2, sort_keys=True) + "\n",
        "schema.graphql": print_schema(lexicographic_sort_schema(build_schema(SDL)))
        + "\n",
    }
    manifest = {
        "version": VERSION,
        "artifacts": {
            name: hashlib.sha256(value.encode()).hexdigest()
            for name, value in values.items()
        },
        "operations": {
            f"{method.upper()} {path}": operation["operationId"]
            for path, methods in schema["paths"].items()
            for method, operation in methods.items()
        },
    }
    values["manifest.json"] = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    return values


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check", action="store_true", help="Fail on schema or operation-ID drift."
    )
    args = parser.parse_args()
    for name, value in artifacts().items():
        path = DESTINATION / name
        if args.check:
            if not path.exists() or path.read_text() != value:
                raise SystemExit(
                    f"HTTP contract drift: {path}; export and review a new contract revision"
                )
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(value)
    print(
        f"HTTP/GraphQL {VERSION} artifacts {'verified' if args.check else 'exported'}"
    )


if __name__ == "__main__":
    main()
