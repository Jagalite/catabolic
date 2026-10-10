#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Initialize or validate the review ledger against a frozen reference inventory."""

import argparse
import copy
import hashlib
import json
from pathlib import Path

OWNERS = {
    "A": "Storage and compatibility",
    "B": "Domain, queries and definitions",
    "C": "Filesystem and observation",
    "D": "Media and workflows",
    "E": "Integrations and transport",
    "F": "Verification and release",
}
CLI_OWNERS = {
    "A": "init db profile location catalog remount maintenance",
    "B": "proposal expected inbox rule target export manifest graphql layout query item tag association relationship mapping program operation projection",
    "C": "scan files sync verify recover",
    "D": "processor catalog-refresh artifact rendition watch sidecar copies process content refresh identify fallback component watcher supervise",
    "E": "api consumer notify",
    "F": "<root> status docs spec",
}


def surfaces(inventory, contracts=None):
    result = {}
    roots = {
        root: owner for owner, names in CLI_OWNERS.items() for root in names.split()
    }
    for kind in ("cli", "http", "migrations"):
        for entry in inventory[kind]:
            identifier = entry["id"]
            if identifier in result:
                raise ValueError(f"duplicate inventory ID: {identifier}")
            if kind == "cli":
                root = identifier.removeprefix("cli:").split()[0]
                if root not in roots:
                    raise ValueError(f"unassigned CLI group: {root}")
                owner = roots[root]
            else:
                owner = "E" if kind == "http" else "A"
            result[identifier] = owner
    for row in (contracts or {}).get("rows", []):
        if row["id"] in result:
            raise ValueError(f"duplicate inventory ID: {row['id']}")
        result[row["id"]] = row["owner"]
    return result


def initialize(inventory, inventory_hash, reference):
    return dict(
        format_version=1,
        reference_commit=reference,
        inventory_sha256=inventory_hash,
        milestone="M0",
        milestone_status="in-progress",
        owners=OWNERS,
        scope_note="CLI parser nodes, frozen HTTP operations and SQL migrations only. Python surface, GraphQL fields, adapters, resource schemas and Python migration hooks still need operation-level review.",
        rows=[
            dict(
                id=identifier,
                owner=owner,
                status="inventoried",
                contract_reference=f"inventory.json#{identifier}",
                public_inputs_outputs="see frozen contract_reference; semantic review pending",
                mutations=None,
                failure_behavior=None,
                reference_tests=[],
                rust_tests=[],
                differential_fixtures=[],
                proof_plan=None,
            )
            for identifier, owner in surfaces(inventory).items()
        ],
    )


def extend(ledger, contracts, contracts_hash):
    """Add newly inventoried surfaces without resetting any existing review."""
    if "contracts_sha256" in ledger:
        raise ValueError(
            "ledger already has supplemental contracts; review updates explicitly"
        )
    if (
        contracts["inventory_sha256"] != ledger["inventory_sha256"]
        or contracts["reference_commit"] != ledger["reference_commit"]
    ):
        raise ValueError("contracts reference a different oracle")
    result = copy.deepcopy(ledger)
    existing = {row["id"] for row in result["rows"]}
    for row in contracts["rows"]:
        if row["id"] in existing:
            raise ValueError(f"duplicate supplemental ID: {row['id']}")
        existing.add(row["id"])
        proof = row["proof_group"]
        command = (
            [
                "python",
                "scripts/migration_proofs.py",
                "--source",
                "REFERENCE_SOURCE",
                "--output",
                "NEW_EVIDENCE_ROOT",
                "--proof",
                proof,
            ]
            if proof
            else [
                "python",
                "scripts/migration_contracts.py",
                "--source",
                "REFERENCE_SOURCE",
                "--check",
            ]
        )
        result["rows"].append(
            dict(
                id=row["id"],
                owner=row["owner"],
                status=row["status"],
                contract_reference=f"contracts.json#{row['id']}",
                public_inputs_outputs=row.get(
                    "public_inputs_outputs",
                    "see supplemental contract; behavioral review pending",
                ),
                mutations=row.get("mutations"),
                failure_behavior=row.get("failure_behavior"),
                reference_tests=row["reference_tests"],
                rust_tests=[],
                differential_fixtures=[],
                proof_plan=dict(
                    command=command,
                    scope=row["proof_scope"],
                    remaining_scenarios=row.get(
                        "remaining_scenarios", ["Operation-specific behavioral mapping"]
                    ),
                ),
            )
        )
    result["contracts_sha256"] = contracts_hash
    result["scope_note"] = (
        "Includes CLI, HTTP, SQL migrations, GraphQL types/fields, HTTP component schemas, released artifacts and reviewed Python migration hooks. Adapter enumeration, Python API classification and behavioral proof coverage remain incomplete."
    )
    return result


def validate(ledger, inventory, inventory_hash, contracts=None, contracts_hash=None):
    if ledger["inventory_sha256"] != inventory_hash:
        raise ValueError("ledger references different inventory bytes")
    if ledger.get("contracts_sha256") != contracts_hash:
        raise ValueError("missing or different supplemental contracts")
    if contracts is not None and (
        contracts["inventory_sha256"] != inventory_hash
        or contracts["reference_commit"] != ledger["reference_commit"]
    ):
        raise ValueError("supplemental contracts reference a different oracle")
    expected = surfaces(inventory, contracts)
    rows = ledger["rows"]
    ids = [row["id"] for row in rows]
    if len(ids) != len(set(ids)):
        raise ValueError("duplicate ledger IDs")
    if set(ids) != set(expected):
        raise ValueError(
            f"ledger coverage drift: missing={sorted(set(expected) - set(ids))}; extra={sorted(set(ids) - set(expected))}"
        )
    for row in rows:
        if row["owner"] not in OWNERS:
            raise ValueError(f"unassigned owner: {row['id']}")
        if row["status"] not in {
            "inventoried",
            "specified",
            "implemented",
            "qualified",
            "blocked",
        }:
            raise ValueError(f"invalid status: {row['id']}")
        for field in (
            "contract_reference",
            "public_inputs_outputs",
            "mutations",
            "failure_behavior",
            "reference_tests",
            "rust_tests",
            "differential_fixtures",
            "proof_plan",
        ):
            if field not in row:
                raise ValueError(f"missing {field}: {row['id']}")
        if row["status"] in {"specified", "implemented", "qualified"}:
            for field in (
                "mutations",
                "failure_behavior",
                "reference_tests",
                "proof_plan",
            ):
                if not row[field]:
                    raise ValueError(f"{row['id']} lacks {field}")
        if row["status"] == "qualified":
            if not row["rust_tests"] or not row["differential_fixtures"]:
                raise ValueError(f"{row['id']} lacks Rust/differential evidence")
    # This tool validates enumeration, not the entire M0 acceptance gate.
    if ledger["milestone_status"] != "in-progress":
        raise ValueError("M0 completion requires a separate full acceptance review")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inventory", type=Path, required=True)
    parser.add_argument("--ledger", type=Path, required=True)
    parser.add_argument("--initialize", action="store_true")
    parser.add_argument("--contracts", type=Path)
    parser.add_argument(
        "--reference", default="453fca983222c6665a48775eef67c489c96b527e"
    )
    args = parser.parse_args()
    raw = args.inventory.read_bytes()
    inventory = json.loads(raw)
    inventory_hash = hashlib.sha256(raw).hexdigest()
    contracts = None
    contracts_hash = None
    if args.contracts:
        contract_bytes = args.contracts.read_bytes()
        contracts = json.loads(contract_bytes)
        contracts_hash = hashlib.sha256(contract_bytes).hexdigest()
    if args.initialize:
        ledger = initialize(inventory, inventory_hash, args.reference)
        if contracts is not None:
            ledger = extend(ledger, contracts, contracts_hash)
        validate(ledger, inventory, inventory_hash, contracts, contracts_hash)
        with args.ledger.open("x") as handle:
            handle.write(json.dumps(ledger, indent=2, sort_keys=True) + "\n")
    else:
        ledger = json.loads(args.ledger.read_text())
        validate(ledger, inventory, inventory_hash, contracts, contracts_hash)
    print(f"{len(ledger['rows'])} enumerated surfaces; M0 remains in progress")


if __name__ == "__main__":
    main()
