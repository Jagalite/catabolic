#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Reproduce schema 33 SQL, normalized signature, and schema-only evidence.

Derived from the authorized R2 schema-preparation artifacts recorded in
``tests/parity/native-catalog-schema/source-provenance.json``. Generation never
modifies frozen schema 1-32 inputs. The default operation is a read-only check;
``--output-dir`` writes only the four generated artifacts under another root.

This is schema-only regression tooling, not Rust-owner/release qualification or
completion of the blocked independent schema review. No external dependencies.
"""

import argparse
import hashlib
import json
import re
import sqlite3
import uuid
from contextlib import ExitStack, closing
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
RESOURCE_PATH = Path("crates/catabolic-store/resources")
BASE_PATH = RESOURCE_PATH / "native-catalog/033_native_catalog.base.sql"
SQL_PATH = RESOURCE_PATH / "migrations/033_native_catalog.sql"
SIGNATURE_PATH = RESOURCE_PATH / "native-schema-33.json"
EVIDENCE_PATH = Path("tests/parity/native-catalog-schema")
INVENTORY_PATH = EVIDENCE_PATH / "generation-revision-inventory.json"
MANIFEST_PATH = EVIDENCE_PATH / "preparation-manifest.json"
LEGACY_SCHEMA = 32


def legacy_paths(repo=REPO):
    """Select exactly migrations 001-032, even when 033 or later exists."""
    paths = []
    for path in sorted((repo / RESOURCE_PATH / "migrations").glob("*.sql")):
        match = re.fullmatch("(\\d{3})_.+\\.sql", path.name)
        if match and 1 <= int(match[1]) <= LEGACY_SCHEMA:
            paths.append(path)
    versions = [int(path.name[:3]) for path in paths]
    if versions != list(range(1, LEGACY_SCHEMA + 1)):
        raise ValueError(
            "expected exactly one frozen migration for each version 001-032"
        )
    return paths


def legacy_db(repo=REPO):
    """Return a caller-owned fixture; close it here only if setup fails."""
    db = sqlite3.connect(":memory:", isolation_level=None)
    try:
        db.create_function(
            "catabolic_catalog_id_v1",
            1,
            lambda s: str(uuid.UUID(s)),
            deterministic=True,
        )
        db.create_function("catabolic_query_v1", 2, lambda s, p: s)
        db.create_function(
            "catabolic_sha256_v1", 1, lambda s: hashlib.sha256(s.encode()).hexdigest()
        )
        for path in legacy_paths(repo):
            db.executescript(path.read_text(encoding="utf-8"))
        db.execute(
            "INSERT INTO meta VALUES ('database_id','00000000-0000-0000-0000-000000000001')"
        )
        db.execute("PRAGMA foreign_keys=ON")
    except BaseException:
        db.close()
        raise
    return db


def objects(db):
    return [
        [t, n, " ".join(s.split())]
        for t, n, s in db.execute(
            "SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY type,name"
        )
    ]


def tables(db):
    return {
        name: [
            {"name": c[1], "type": c[2], "pk": c[5]}
            for c in db.execute('PRAGMA table_info("' + name + '")')
        ]
        for (name,) in db.execute(
            "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        )
    }


def script(db, sql):
    # executescript commits enclosing transactions; use the SQLite parser instead.
    statement = ""
    for line in sql.splitlines(True):
        statement += line
        if sqlite3.complete_statement(statement):
            db.execute(statement)
            statement = ""
    assert not statement.strip(), statement


def ident(x):
    return '"' + x.replace('"', '""') + '"'


def changed(cols):
    return " OR ".join(
        (f"NEW.{ident(c['name'])} IS NOT OLD.{ident(c['name'])}" for c in cols)
    )


def pk(cols, alias):
    cols = sorted((c for c in cols if c["pk"]), key=lambda c: c["pk"])
    assert cols
    return (
        "json_array("
        + ",".join(((alias + "." if alias else "") + ident(c["name"]) for c in cols))
        + ")"
    )


def generate(repo=REPO):
    """Return deterministic artifact content without writing any files."""
    with ExitStack() as connections:
        return _generate(repo, connections)


def _generate(repo, connections):
    base = (repo / BASE_PATH).read_text(encoding="utf-8")
    db = connections.enter_context(closing(legacy_db(repo)))
    oldobjects = objects(db)
    oldtables = tables(db)
    script(db, base)
    newtables = tables(db)
    parts = [
        base,
        "\n-- Generated immutable history and transaction-owner protections.\n",
    ]

    def add(s):
        parts.append(s + "\n")

    def trigger(name, timing, event, table, when, body):
        add(
            f"CREATE TRIGGER {name} {timing} {event} ON {table}"
            + (f" WHEN {when}" if when else "")
            + f" BEGIN {body} END;"
        )

    def fail(code):
        return "SELECT RAISE(ABORT,'" + code + "');"

    def permitted(kind, scope):
        return f"EXISTS (SELECT 1 FROM catalog_mutation_permits p JOIN catalog_state state ON state.id=1 WHERE p.scope_kind='{kind}' AND p.scope_id={scope} AND p.incarnation=state.incarnation)"

    # REPLACE can skip DELETE triggers when recursive_triggers is disabled.
    # Guard immutable unique keys before INSERT as well as UPDATE/DELETE.
    immutable = [
        "catalog_legacy_associations",
        "catalog_legacy_item_metadata",
        "catalog_identity_evidence",
        "catalog_exact_file_revisions",
        "catalog_timeline_revisions",
        "catalog_segmentation_revisions",
        "catalog_version_revisions",
        "catalog_bindings",
        "catalog_binding_revisions",
        "catalog_version_bindings",
        "catalog_segmentation_sets",
        "catalog_segment_members",
    ]
    for t in immutable:
        for event in ("UPDATE", "DELETE"):
            trigger(
                t + "_no_" + event.lower(),
                "BEFORE",
                event,
                t,
                None,
                fail("catalog_history_immutable"),
            )
    for t in immutable + [
        "catalog_command_receipts",
        "catalog_receipt_holds",
        "catalog_timelines",
        "catalog_versions",
        "catalog_segmentation_contexts",
    ]:
        conditions = []
        for index in db.execute('PRAGMA index_list("' + t + '")'):
            if not index[2]:
                continue
            columns = [
                c[2] for c in db.execute('PRAGMA index_info("' + index[1] + '")')
            ]
            equal = " AND ".join(
                (f"existing.{ident(c)} IS NEW.{ident(c)}" for c in columns)
            )
            nonnull = " AND ".join((f"NEW.{ident(c)} IS NOT NULL" for c in columns))
            conditions.append(
                "("
                + nonnull
                + " AND EXISTS (SELECT 1 FROM "
                + ident(t)
                + " existing WHERE "
                + equal
                + "))"
            )
        trigger(
            t + "_no_replace",
            "BEFORE",
            "INSERT",
            t,
            " OR ".join(conditions),
            fail("catalog_immutable_key_reuse"),
        )
    heads = [
        ("catalog_timelines", "timeline"),
        ("catalog_versions", "version"),
        ("catalog_segmentation_contexts", "context"),
    ]
    for t, kind in heads:
        fixed = [c for c in newtables[t] if c["name"] != "current_revision"]
        trigger(
            t + "_initial",
            "BEFORE",
            "INSERT",
            t,
            "NEW.current_revision != 1 OR NOT " + permitted(kind, "NEW.id"),
            fail("catalog_typed_owner_required"),
        )
        trigger(
            t + "_advance",
            "BEFORE",
            "UPDATE",
            t,
            changed(fixed)
            + " OR NEW.current_revision != OLD.current_revision+1 OR NOT "
            + permitted(kind, "OLD.id"),
            fail("catalog_revision_or_owner_conflict"),
        )
        trigger(
            t + "_no_delete",
            "BEFORE",
            "DELETE",
            t,
            None,
            fail("catalog_identity_history_retained"),
        )
    histories = [
        ("catalog_timeline_revisions", "timeline_id", "timeline"),
        ("catalog_version_revisions", "version_id", "version"),
        ("catalog_segmentation_revisions", "context_id", "context"),
        ("catalog_binding_revisions", "binding_id", "binding"),
    ]
    for t, owner, kind in histories:
        scope = "NEW." + owner
        if kind == "binding":
            kind = "version"
            scope = "(SELECT version_id FROM catalog_bindings WHERE id=NEW.binding_id)"
        condition = (
            f"NEW.revision != COALESCE((SELECT MAX(revision) FROM {t} WHERE {owner}=NEW.{owner}),0)+1 OR NOT "
            + permitted(kind, scope)
        )
        trigger(
            t + "_append",
            "BEFORE",
            "INSERT",
            t,
            condition,
            fail("catalog_revision_or_owner_conflict"),
        )
        trigger(
            t + "_uncommitted",
            "BEFORE",
            "INSERT",
            t,
            "EXISTS (SELECT 1 FROM catalog_command_receipts r WHERE r.catalog_id=NEW.command_catalog_id AND r.host_namespace=NEW.command_host_namespace AND r.operation_id=NEW.command_operation_id)",
            fail("catalog_committed_history_sealed"),
        )
    for t, parent, key, revision in [
        (
            "catalog_version_bindings",
            "catalog_version_revisions",
            "version_id",
            "version_revision",
        ),
        (
            "catalog_segment_members",
            "catalog_segmentation_revisions",
            "context_id",
            "context_revision",
        ),
    ]:
        trigger(
            t + "_uncommitted",
            "BEFORE",
            "INSERT",
            t,
            f"EXISTS (SELECT 1 FROM {parent} history JOIN catalog_command_receipts r ON r.catalog_id=history.command_catalog_id AND r.host_namespace=history.command_host_namespace AND r.operation_id=history.command_operation_id WHERE history.{key}=NEW.{key} AND history.revision=NEW.{revision})",
            fail("catalog_committed_history_sealed"),
        )
    trigger(
        "catalog_binding_claim_consistent",
        "BEFORE",
        "INSERT",
        "catalog_bindings",
        "NOT "
        + permitted("version", "NEW.version_id")
        + " OR NOT EXISTS (SELECT 1 FROM catalog_versions v JOIN item_files a ON a.item_id=v.association_item_id WHERE v.id=NEW.version_id AND a.id=NEW.association_id AND a.role='primary' AND a.active=1)",
        fail("catalog_binding_owner_mismatch"),
    )
    trigger(
        "catalog_binding_revision_association",
        "BEFORE",
        "INSERT",
        "catalog_binding_revisions",
        "NOT EXISTS (SELECT 1 FROM catalog_bindings b JOIN catalog_versions v ON v.id=b.version_id JOIN item_files a ON a.id=b.association_id WHERE b.id=NEW.binding_id AND a.item_id=v.association_item_id AND a.file_id=NEW.file_id AND a.part=NEW.part AND a.role='primary' AND a.active=1)",
        fail("catalog_binding_association_mismatch"),
    )
    trigger(
        "catalog_version_distinct_owner",
        "BEFORE",
        "INSERT",
        "catalog_version_revisions",
        "EXISTS (SELECT 1 FROM catalog_versions v JOIN catalog_timelines t ON t.id=NEW.timeline_id WHERE v.id=NEW.version_id AND v.association_item_id=t.edition_item_id)",
        fail("catalog_identity_invalid_version_owner"),
    )
    for event, aliases in [
        ("INSERT", ["NEW"]),
        ("UPDATE", ["OLD", "NEW"]),
        ("DELETE", ["OLD"]),
    ]:
        checks = []
        for a in aliases:
            checks.append(
                f"EXISTS (SELECT 1 FROM catalog_versions v WHERE (v.association_item_id={a}.item_id OR EXISTS (SELECT 1 FROM catalog_bindings b WHERE b.version_id=v.id AND b.association_id={a}.id)) AND NOT "
                + permitted("version", "v.id")
                + ")"
            )
        trigger(
            "catalog_item_files_owner_" + event.lower(),
            "BEFORE",
            event,
            "item_files",
            " OR ".join(checks),
            fail("catalog_typed_owner_required"),
        )
    for event, aliases in [
        ("INSERT", ["NEW"]),
        ("UPDATE", ["OLD", "NEW"]),
        ("DELETE", ["OLD"]),
    ]:
        condition = " OR ".join(
            (
                f"EXISTS (SELECT 1 FROM catalog_versions v WHERE v.association_item_id={a}.id AND NOT "
                + permitted("version", "v.id")
                + ")"
                for a in aliases
            )
        )
        trigger(
            "catalog_version_item_owner_" + event.lower(),
            "BEFORE",
            event,
            "items",
            condition,
            fail("catalog_typed_owner_required"),
        )
    trigger(
        "catalog_entity_revisions_no_delete",
        "BEFORE",
        "DELETE",
        "catalog_entity_revisions",
        None,
        fail("catalog_entity_revision_history_retained"),
    )
    trigger(
        "catalog_entity_revisions_advance",
        "BEFORE",
        "UPDATE",
        "catalog_entity_revisions",
        "OLD.revision=9223372036854775807 OR NEW.entity_kind IS NOT OLD.entity_kind OR NEW.entity_key IS NOT OLD.entity_key OR NEW.revision != OLD.revision+1",
        fail("catalog_entity_revision_conflict"),
    )
    # Generation is an opaque invalidation token, not a transaction counter.
    # Exclude only derived/internal sentinels and receipt retention bookkeeping.
    excluded = {
        "schema_migrations": "Explicit lifecycle metadata; no ordinary mutation or cursor semantics",
        "fallback_epoch": "Derived legacy epoch; native source-table triggers cover it without recursive duplication",
        "fallback_probe_slots": "Internal ephemeral probe semaphore, no logical catalog state",
        "remount_guard": "Internal ephemeral repair guard, no logical catalog state",
        "catalog_state": "Generation/incarnation owner; recursion forbidden",
        "catalog_commit_barrier": "Permanently empty transaction commit barrier",
        "catalog_mutation_permits": "Ephemeral typed-owner permits",
        "catalog_entity_revisions": "Derived per-entity counters; recursion forbidden",
        "catalog_command_receipts": "Immutable command result/tombstone bookkeeping; committed_generation must stay final",
        "catalog_receipt_holds": "Retention bookkeeping; does not affect catalog snapshots",
    }
    inventory = []
    # Backfill counters before installing triggers, retaining migration generation 0.
    tracked = {t: cols for t, cols in newtables.items() if t not in excluded}
    for t, cols in tracked.items():
        add(
            f"INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) SELECT '{t}',{pk(cols, '')},1,0 FROM {ident(t)};"
        )
    add(
        "-- Source-row counters keep deleted-key tombstones, preventing delete/reinsert ABA."
    )
    for t, cols in newtables.items():
        legacy = t in oldtables
        oldtriggers = [
            x[0]
            for x in db.execute(
                "SELECT name FROM sqlite_schema WHERE type='trigger' AND tbl_name=? AND sql LIKE '%UPDATE fallback_epoch%'",
                [t],
            )
        ]
        owners = []
        if legacy:
            rx = re.compile(
                '(?i)\\b(?:INSERT\\s+(?:OR\\s+\\w+\\s+)?INTO|UPDATE|DELETE\\s+FROM)\\s+[\\"`]?'
                + re.escape(t)
                + "\\b"
            )
            for path in sorted((repo / "crates").glob("*/src/**/*.rs")):
                if rx.search(path.read_text()):
                    owners.append(str(path.relative_to(repo)))
        inventory.append(
            {
                "table": t,
                "legacy": legacy,
                "generation": "excluded"
                if t in excluded
                else "insert/delete/changed-update",
                "entity_revision": "excluded"
                if t in excluded
                else "retained-key counter and deletion tombstone",
                "excluded_reason": excluded.get(t),
                "legacy_fallback_triggers": oldtriggers,
                "mutation_owner_files": owners,
                "primary_key": [
                    c["name"] for c in sorted(cols, key=lambda c: c["pk"]) if c["pk"]
                ],
            }
        )
        if t in excluded:
            continue
        for event in ["INSERT", "UPDATE", "DELETE"]:
            a = "OLD" if event == "DELETE" else "NEW"
            key = pk(cols, a)
            when = changed(cols) if event == "UPDATE" else None
            body = "SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END; "
            if event == "UPDATE":
                body += f"INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) SELECT '{t}',{pk(cols, 'OLD')},1,1 WHERE {pk(cols, 'NEW')} IS NOT {pk(cols, 'OLD')} ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=1; "
            body += f"INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('{t}',{key},1,{int(event == 'DELETE')}) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted; "
            body += "UPDATE catalog_state SET generation=generation+1 WHERE id=1;"
            trigger(
                "catalog_change_" + t + "_" + event.lower(),
                "AFTER",
                event,
                t,
                when,
                body,
            )
    sql = "\n".join(parts)
    connections.close()
    db = connections.enter_context(closing(legacy_db(repo)))
    db.execute("BEGIN IMMEDIATE")
    script(db, sql)
    db.execute("PRAGMA recursive_triggers=ON")
    db.execute("PRAGMA read_uncommitted=OFF")
    db.execute("COMMIT")
    assert db.execute("PRAGMA foreign_keys").fetchone() == (1,)
    assert db.execute("PRAGMA recursive_triggers").fetchone() == (1,)
    assert db.execute("PRAGMA read_uncommitted").fetchone() == (0,)
    assert not list(db.execute("PRAGMA foreign_key_check"))
    nowobjects = objects(db)
    byname = {(x[0], x[1]): x for x in nowobjects}
    assert all((byname[x[0], x[1]] == x for x in oldobjects)), "historical DDL changed"
    assert db.execute("SELECT generation FROM catalog_state WHERE id=1").fetchone() == (
        0,
    )
    connections.close()
    manifest = {
        "format_version": 1,
        "status": "schema-only-regression-independent-review-incomplete",
        "qualification_scope": "Schema-only; not Rust-owner or release qualification",
        "independent_review_evidence": str(
            Path("tests/parity/native-catalog-schema") / "schema-review-blocked.json"
        ),
        "migration": 33,
        "migration_filename": str(SQL_PATH),
        "migration_sha256": hashlib.sha256(sql.encode()).hexdigest(),
        "base_filename": str(BASE_PATH),
        "base_sha256": hashlib.sha256((repo / BASE_PATH).read_bytes()).hexdigest(),
        "legacy_schema": LEGACY_SCHEMA,
        "legacy_tables": len(oldtables),
        "native_tables_added": len(newtables) - len(oldtables),
        "source_table_generation_coverage": len(tracked),
        "generation_exclusions": excluded,
        "signature_filename": str(SIGNATURE_PATH),
        "new_ddl_objects": len(nowobjects) - len(oldobjects),
        "legacy_ddl_preserved": True,
        "empty_upgrade_generation": 0,
        "legacy_resource_sha256": {
            str(p.relative_to(repo)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in legacy_paths(repo)
        },
    }
    return {
        SQL_PATH: sql,
        SIGNATURE_PATH: json.dumps(nowobjects, indent=2, ensure_ascii=False) + "\n",
        INVENTORY_PATH: json.dumps(inventory, indent=2) + "\n",
        MANIFEST_PATH: json.dumps(manifest, indent=2) + "\n",
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--check",
        action="store_true",
        help="Compare with checked-in artifacts (default); write nothing",
    )
    mode.add_argument(
        "--output-dir",
        type=Path,
        help="Write regenerated artifacts using repository-relative paths below DIR",
    )
    args = parser.parse_args(argv)
    artifacts = generate()
    if args.output_dir is not None:
        for relative, content in artifacts.items():
            destination = args.output_dir / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(content.encode("utf-8"))
        print(f"Wrote {len(artifacts)} schema-only artifacts under {args.output_dir}")
        return 0
    mismatches = [
        str(relative)
        for relative, content in artifacts.items()
        if not (REPO / relative).is_file()
        or (REPO / relative).read_bytes() != content.encode("utf-8")
    ]
    if mismatches:
        parser.exit(
            1, "Native schema artifacts differ: " + ", ".join(mismatches) + "\n"
        )
    print(
        f"{len(artifacts)} native schema artifacts match; schema-only, not Rust-owner/release qualification"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
