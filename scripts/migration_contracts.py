#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Extend M0 scope from verified reference bytes; never regenerate the base freeze."""

import argparse
import hashlib
import json
from pathlib import Path

from graphql import parse, print_ast

REFERENCE = "453fca983222c6665a48775eef67c489c96b527e"
PROOFS = {
    "graphql-structure": [
        "tests.test_http_contract.ContractTest.test_graphql_artifact_and_runtime_have_identical_fields",
        "tests.test_graphql_query.GraphQLTest.test_schema_introspection_and_operation_selection",
    ],
    "graphql-execution": [
        "tests.test_graphql_query.GraphQLTest.test_nested_query_variables_fragments_aliases_and_readonly",
        "tests.test_graphql_query.GraphQLTest.test_pages_cursors_and_filters",
        "tests.test_graphql_query.GraphQLTest.test_profile_availability_and_catalog_projection",
        "tests.test_graphql_query.GraphQLTest.test_rejects_writes_invalid_fields_variables_and_cursors",
        "tests.test_graphql_query.GraphQLTest.test_query_limits",
        "tests.test_graphql_query.GraphQLTest.test_cli_file_stdin_json_errors_and_schema_without_database",
    ],
    "http-structure": [
        "tests.test_http_contract.ContractTest.test_every_operation_is_typed_unique_and_frozen",
    ],
    "interchange-structure": [
        "tests.test_interchange.InterchangeTest.test_frozen_schema_reference_and_independent_schema_validation",
        "tests.test_interchange.InterchangeTest.test_schema_drift_fails_even_if_runtime_accepts_added_fields",
    ],
    "migration-query-rebuild": [
        "tests.test_programmable_migration.ProgrammableMigrationTest.test_schema14_preserves_all_original_columns_ids_and_foreign_keys",
        "tests.test_programmable_migration.ProgrammableMigrationTest.test_failed_table_rebuild_preserves_original_database",
    ],
    "migration-components": [
        "tests.test_component_migration.MigrationTest.test_populated_schema_24_adopts_probe_and_sidecar_without_rewriting_data",
        "tests.test_component_migration.MigrationTest.test_schema_24_pending_journal_recovers_before_upgrade",
    ],
}
HOOKS = [
    {
        "id": "migration-hook:15-query-canonicalization",
        "version": 15,
        "public_inputs_outputs": "Legacy query/layout definitions and profile; deterministic catabolic_query_v1 JSON and UTF-8 SHA256 used by migration 015.",
        "mutations": "Register two connection-local deterministic UDFs during SQL execution; remove them in finally. Adopt definitions through the enclosing migration transaction without rewriting original rule/layout JSON.",
        "failure_behavior": "Malformed JSON or invalid values fail the migration; enclosing upgrade must retain the pre-upgrade database.",
        "proof": "migration-query-rebuild",
        "remaining_scenarios": [
            "Independent canonical bytes/digest golden cases for Unicode, nonfinite numbers and profile precedence",
            "UDF cleanup on SQL error",
        ],
    },
    {
        "id": "migration-hook:16-operation-rule-rebuild",
        "version": 16,
        "public_inputs_outputs": "Schema-15 recipe/rule graph; rebuilt operation/rule tables preserving IDs, original columns and relationships.",
        "mutations": "Defer foreign keys for the rebuild; allow SQLite's internal quick_check on processing_recipes through the authorizer. Verify the final FK graph before clearing deferred violation bookkeeping.",
        "failure_behavior": "A remaining foreign-key violation refuses the upgrade; failure must leave the original database intact.",
        "proof": "migration-query-rebuild",
        "remaining_scenarios": [
            "Inject a violation at the version-16 hook rather than a later migration",
            "Exercise internal quick_check authorization across qualified SQLite versions",
        ],
    },
    {
        "id": "migration-hook:25-component-backfill",
        "version": 25,
        "public_inputs_outputs": "Schema-24 probe facts and media associations; component occurrences and their evidence after schema-25 SQL.",
        "mutations": "Call components.backfill on the same connection inside the upgrade transaction, adopting legacy evidence while preserving original rows.",
        "failure_behavior": "Backfill failure propagates through the upgrade; pending legacy journals require recovery before upgrade.",
        "proof": "migration-components",
        "remaining_scenarios": [
            "Inject failure inside backfill and compare complete pre/post row digests",
            "Mixed stale, invalid and absent legacy probe evidence",
        ],
    },
]


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def verify_source(source, inventory):
    """Validate every inventoried input before importing or executing the oracle."""
    for relative, expected in inventory["resource_sha256"].items():
        path = source / relative
        if not path.is_file() or sha(path.read_bytes()) != expected:
            raise ValueError(f"reference byte drift: {relative}")


def build(source, inventory, inventory_hash):
    verify_source(source, inventory)
    rows = []

    def add(identifier, owner, path, contract, proof=None, **review):
        rows.append(
            dict(
                id=identifier,
                owner=owner,
                source=path,
                source_sha256=inventory["resource_sha256"][path],
                contract=contract,
                status="inventoried",
                proof_group=proof,
                reference_tests=PROOFS.get(proof, []),
                proof_scope="structural reference check; field behavior still needs scenario mapping",
                **review,
            )
        )

    latest = f"src/catabolic/http/releases/{inventory['http_version']}"
    graphql = f"{latest}/schema.graphql"
    for definition in parse((source / graphql).read_text()).definitions:
        name = definition.name.value
        add(
            f"graphql:type:{name}",
            "B",
            graphql,
            print_ast(definition),
            "graphql-structure",
        )
        for field in getattr(definition, "fields", ()) or ():
            add(
                f"graphql:field:{name}.{field.name.value}",
                "B",
                graphql,
                print_ast(field),
                "graphql-structure",
            )
    openapi_path = f"{latest}/openapi.json"
    openapi = json.loads((source / openapi_path).read_text())
    for name, schema in sorted(openapi["components"]["schemas"].items()):
        add(f"http-schema:{name}", "E", openapi_path, schema, "http-structure")
    for path in sorted(inventory["resource_sha256"]):
        is_http = path.startswith("src/catabolic/http/releases/") and path.endswith(
            (".json", ".graphql")
        )
        is_interchange = path.startswith(
            "src/catabolic/interchange/releases/"
        ) and path.endswith((".json", ".md"))
        if is_http or is_interchange:
            add(
                f"artifact:{path}",
                "F",
                path,
                {"sha256": inventory["resource_sha256"][path]},
                "interchange-structure" if path.endswith(".schema.json") else None,
            )
            rows[-1]["proof_scope"] = (
                "Exact released bytes checked by verify_source; runtime interpretation is a separate obligation."
            )
    for hook in HOOKS:
        path = "src/catabolic/migration.py"
        add(
            hook["id"],
            "A",
            path,
            {
                "version": hook["version"],
                "entry_point": "catabolic.migration.execute_migration",
            },
            hook["proof"],
            public_inputs_outputs=hook["public_inputs_outputs"],
            mutations=hook["mutations"],
            failure_behavior=hook["failure_behavior"],
            remaining_scenarios=hook["remaining_scenarios"],
        )
        rows[-1]["proof_scope"] = (
            "Existing positive/rollback scenarios; listed hook-specific fault and canonicalization scenarios remain open."
        )
    known_tests = set(inventory["test_methods"])
    for proof, tests in PROOFS.items():
        if not tests or not set(tests) <= known_tests:
            raise ValueError(f"unknown reference test in {proof}")
    return dict(
        format_version=1,
        reference_commit=REFERENCE,
        inventory_sha256=inventory_hash,
        proofs=PROOFS,
        rows=sorted(rows, key=lambda row: row["id"]),
        limitations=[
            "Structural checks are not complete behavioral proof.",
            "Adapter enumeration and Python API classification remain pending.",
            "Migration hooks were manually reviewed against the verified source bytes; no general AST hook detector is claimed.",
        ],
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument(
        "--inventory", type=Path, default=Path("tests/parity/inventory.json")
    )
    parser.add_argument(
        "--output", type=Path, default=Path("tests/parity/contracts.json")
    )
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    raw = args.inventory.read_bytes()
    result = build(args.source.resolve(), json.loads(raw), sha(raw))
    content = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.check:
        if args.output.read_text() != content:
            raise SystemExit("contract inventory drift")
    else:
        with args.output.open("x") as handle:
            handle.write(content)
    print(f"{len(result['rows'])} supplemental contract surfaces verified")


if __name__ == "__main__":
    main()
