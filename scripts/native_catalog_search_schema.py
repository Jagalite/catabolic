#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Reproduce additive schema35 search resources; default is read-only comparison."""

import argparse
import json
from contextlib import closing
from pathlib import Path

try:
    from . import native_catalog_schema as foundation
except ImportError:
    import native_catalog_schema as foundation
REPO = Path(__file__).resolve().parents[1]
RESOURCES = foundation.RESOURCE_PATH
SQL_PATH = RESOURCES / "migrations/035_native_search.sql"
SIGNATURE_PATH = RESOURCES / "native-schema-35.json"
MANIFEST_PATH = Path("tests/parity/native-catalog-search/schema-manifest.json")
SQL = """-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors
-- SPDX-License-Identifier: MIT
-- Additive derived search schema35. Existing source rows/generation are unchanged.
-- Input revision is local to the derived index, not catalog generation.
CREATE TABLE catalog_search_state(
 id INTEGER PRIMARY KEY CHECK(id=1),
 contract_version INTEGER NOT NULL CHECK(contract_version=1),
 tokenizer TEXT NOT NULL CHECK(tokenizer='unicode61 remove_diacritics 2'),
 extractor TEXT NOT NULL CHECK(extractor='native-search-values-v1'),
 ready INTEGER NOT NULL CHECK(ready IN (0,1)),
 input_revision INTEGER NOT NULL CHECK(typeof(input_revision)='integer' AND input_revision>=0),
 indexed_revision INTEGER NOT NULL CHECK(typeof(indexed_revision)='integer' AND indexed_revision>=0 AND indexed_revision<=input_revision),
 catalog_id TEXT, incarnation TEXT, rebuilt_generation INTEGER,
 CHECK(ready=0 OR (catalog_id IS NOT NULL AND incarnation IS NOT NULL AND rebuilt_generation IS NOT NULL))
);
INSERT INTO catalog_search_state VALUES(1,1,'unicode61 remove_diacritics 2','native-search-values-v1',0,0,0,NULL,NULL,NULL);
CREATE TABLE catalog_search_values(
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 field_key TEXT NOT NULL,
 origin_key TEXT NOT NULL,
 rank_class TEXT NOT NULL CHECK(rank_class IN ('title','aliases','accepted_identities','effective_tags','description')),
 value_text TEXT NOT NULL,
 provenance_json TEXT NOT NULL CHECK(json_valid(provenance_json) AND json_type(provenance_json)='object'),
 required_subjects_json TEXT NOT NULL CHECK(json_valid(required_subjects_json) AND json_type(required_subjects_json)='array'),
 PRIMARY KEY(item_id,field_key,origin_key)
);
CREATE TABLE catalog_search_documents(
 rowid INTEGER PRIMARY KEY,
 item_id TEXT NOT NULL UNIQUE REFERENCES items(id) ON DELETE CASCADE,
 title TEXT NOT NULL, aliases TEXT NOT NULL, accepted_identities TEXT NOT NULL,
 effective_tags TEXT NOT NULL, description TEXT NOT NULL
);
CREATE VIRTUAL TABLE catalog_search_fts USING fts5(
 title, aliases, accepted_identities, effective_tags, description,
 content='catalog_search_documents',content_rowid='rowid',
 tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER catalog_search_document_insert AFTER INSERT ON catalog_search_documents BEGIN
 INSERT INTO catalog_search_fts(rowid,title,aliases,accepted_identities,effective_tags,description)
 VALUES(NEW.rowid,NEW.title,NEW.aliases,NEW.accepted_identities,NEW.effective_tags,NEW.description);
END;
CREATE TRIGGER catalog_search_document_delete AFTER DELETE ON catalog_search_documents BEGIN
 INSERT INTO catalog_search_fts(catalog_search_fts,rowid,title,aliases,accepted_identities,effective_tags,description)
 VALUES('delete',OLD.rowid,OLD.title,OLD.aliases,OLD.accepted_identities,OLD.effective_tags,OLD.description);
END;
CREATE TRIGGER catalog_search_document_update AFTER UPDATE ON catalog_search_documents BEGIN
 INSERT INTO catalog_search_fts(catalog_search_fts,rowid,title,aliases,accepted_identities,effective_tags,description)
 VALUES('delete',OLD.rowid,OLD.title,OLD.aliases,OLD.accepted_identities,OLD.effective_tags,OLD.description);
 INSERT INTO catalog_search_fts(rowid,title,aliases,accepted_identities,effective_tags,description)
 VALUES(NEW.rowid,NEW.title,NEW.aliases,NEW.accepted_identities,NEW.effective_tags,NEW.description);
END;
CREATE TABLE catalog_search_dirty(
 item_id TEXT PRIMARY KEY,
 barrier_id INTEGER NOT NULL DEFAULT 1 CHECK(barrier_id=1),
 FOREIGN KEY(barrier_id) REFERENCES catalog_commit_barrier(id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_search_dirty_limit BEFORE INSERT ON catalog_search_dirty
 WHEN NOT EXISTS(SELECT 1 FROM catalog_search_dirty WHERE item_id=NEW.item_id)
 AND (SELECT count(*) FROM catalog_search_dirty)>=100000
 BEGIN SELECT RAISE(ABORT,'catalog_search_dirty_budget'); END;
CREATE INDEX catalog_search_alias_reverse ON catalog_logical_retirements(survivor_item_id,item_id) WHERE kind='merge';
"""


def trigger(table, event, selects, when="", timing="AFTER", suffix=""):
    global SQL
    name = f"catalog_search_input_{table}_{event.lower()}{suffix}"
    SQL += (
        f"CREATE TRIGGER {name} {timing} {event} ON {table}"
        + (f" WHEN {when}" if when else "")
        + " BEGIN\n"
    )
    for select in selects:
        expression = select[7:].split(" FROM ", 1)[0].split(" WHERE ", 1)[0]
        selected = select.replace(
            "SELECT " + expression, "SELECT " + expression + " AS item_id", 1
        )
        SQL += (
            " INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM ("
            + selected
            + ") AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);\n"
        )

    SQL += " UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);\n"
    SQL += "END;\n"


# Conservative old/new subject capture also covers source changes made outside
# the typed dispatcher, while the shared finalizer owns derived serialization.
for table, key in [
    ("items", "id"),
    ("catalog_metadata_subjects", "item_id"),
    ("catalog_metadata_projection", "item_id"),
    ("catalog_metadata_sources", "item_id"),
    ("catalog_identity_pins", "item_id"),
    ("item_tags", "item_id"),
    ("catalog_item_tag_suppressions", "item_id"),
]:
    for event, aliases in [
        ("INSERT", ["NEW"]),
        ("UPDATE", ["OLD", "NEW"]),
        ("DELETE", ["OLD"]),
    ]:
        when = ""
        if table == "items" and event == "UPDATE":
            when = "NEW.id IS NOT OLD.id OR NEW.metadata IS NOT OLD.metadata"
        trigger(table, event, [f"SELECT {alias}.{key}" for alias in aliases], when)
trigger(
    "catalog_tag_suppression_revisions",
    "INSERT",
    ["SELECT NEW.subject_id WHERE NEW.subject_type='item'"],
    "NEW.subject_type='item'",
)
for table, key in [("tag_names", "tag_id"), ("tags", "id")]:
    for event, aliases in [
        ("INSERT", ["NEW"]),
        ("UPDATE", ["OLD", "NEW"]),
        ("DELETE", ["OLD"]),
    ]:
        # BEFORE captures memberships before alias/tag rows move or disappear.
        trigger(
            table,
            event,
            [
                f"SELECT item_id FROM item_tags WHERE tag_id={alias}.{key}"
                for alias in aliases
            ],
            timing="BEFORE",
        )
for event, aliases in [("INSERT", ["NEW"]), ("UPDATE", ["OLD", "NEW"])]:
    trigger(
        "catalog_tag_redirects",
        event,
        [
            f"SELECT item_id FROM item_tags WHERE tag_id IN ({alias}.source_tag_id,{alias}.target_tag_id)"
            for alias in aliases
        ],
    )
# All-live conservative closure captures alias ancestors and later split paths.
trigger(
    "catalog_logical_retirements",
    "INSERT",
    [
        "SELECT id FROM catalog_live_items",
        "SELECT item_id FROM catalog_search_documents",
        "SELECT NEW.item_id",
        "SELECT NEW.survivor_item_id WHERE NEW.survivor_item_id IS NOT NULL",
    ],
)
trigger(
    "catalog_version_retirements",
    "INSERT",
    ["SELECT association_item_id FROM catalog_versions WHERE id=NEW.version_id"],
)
for event, aliases in [
    ("INSERT", ["NEW"]),
    ("UPDATE", ["OLD", "NEW"]),
    ("DELETE", ["OLD"]),
]:
    trigger(
        "catalog_versions",
        event,
        [f"SELECT {alias}.association_item_id" for alias in aliases],
    )


def generate(repo=REPO):
    with closing(foundation.legacy_db(repo)) as db:
        db.execute("PRAGMA recursive_triggers=ON")
        for name in ["033_native_catalog.sql", "034_native_curation.sql"]:
            db.executescript((repo / RESOURCES / "migrations" / name).read_text())
        prior = foundation.objects(db)
        old_tables = foundation.tables(db)
        db.executescript(SQL)
        current = foundation.objects(db)
        by_name = {(obj[0], obj[1]): obj for obj in current}
        assert all(by_name[obj[0], obj[1]] == obj for obj in prior), (
            "historical DDL changed"
        )
        assert not db.execute("PRAGMA foreign_key_check").fetchall()
        assert db.execute(
            "SELECT ready,input_revision,indexed_revision FROM catalog_search_state"
        ).fetchone() == (0, 0, 0)
        tables = foundation.tables(db)
        manifest = {
            "schema_version": 35,
            "first_native_schema": 33,
            "legacy_target": 32,
            "independent_review": "incomplete; generation is not review clearance",
            "new_tables": {
                name: cols for name, cols in tables.items() if name not in old_tables
            },
            "new_objects": len(current) - len(prior),
        }
    return {
        SQL_PATH: SQL,
        SIGNATURE_PATH: json.dumps(current, indent=2) + "\n",
        MANIFEST_PATH: json.dumps(manifest, indent=2) + "\n",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path)
    args = parser.parse_args()
    for path, text in generate().items():
        if args.output_dir:
            target = args.output_dir / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text)
        elif (REPO / path).read_text() != text:
            raise SystemExit(f"generated artifact differs: {path}")
        print(path)


if __name__ == "__main__":
    main()
