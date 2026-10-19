// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent adapter budgets. No source paths or external services are opened.
use catabolic_store::{Store, migration, public_controls, public_reads};
use rusqlite::params;
use serde_json::json;

const BUDGET: usize = 4 * 1024 * 1024;

fn fixture() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("public-budget.db");
    migration::initialize(&path).unwrap();
    (root, Store::open(&path, true, false).unwrap())
}

fn insert_http(store: &Store, id: &str, padding: usize) {
    let definition = catabolic_core::encode(&json!({
        "operation_id":"fixture", "spec_digest":"fixture", "method":"POST",
        "padding":"x".repeat(padding),
    }));
    store
        .db
        .execute(
            "INSERT INTO http_operations VALUES (?,'default',?,1,?,'digest',1,NULL,NULL)",
            params![id, id, definition],
        )
        .unwrap();
}

#[test]
fn http_record_exact_boundary_and_one_byte_over_are_read_only() {
    let (_root, store) = fixture();
    insert_http(&store, "a", 0);
    let raw = migration::rows(&store.db, "SELECT * FROM http_operations", &[]).unwrap();
    let overhead = catabolic_core::encode(&raw[0]).len();
    store.db.execute("DELETE FROM http_operations", []).unwrap();
    insert_http(&store, "a", BUDGET - overhead);
    let before = store.db.total_changes();
    let value =
        public_controls::http_operation(&store, "default", "get", &json!({"id":"a"})).unwrap();
    assert_eq!(
        value["definition"]["padding"].as_str().unwrap().len(),
        BUDGET - overhead
    );
    assert_eq!(store.db.total_changes(), before);
    store.db.execute("DELETE FROM http_operations", []).unwrap();
    insert_http(&store, "a", BUDGET - overhead + 1);
    let before = store.db.total_changes();
    assert_eq!(
        public_controls::http_operation(&store, "default", "get", &json!({"id":"a"}))
            .unwrap_err()
            .0,
        "result exceeds the result budget"
    );
    assert_eq!(store.db.total_changes(), before);
}

#[test]
fn http_list_bounds_full_definitions_and_keeps_existing_transaction() {
    let (_root, store) = fixture();
    insert_http(&store, "a", BUDGET / 2);
    insert_http(&store, "b", BUDGET / 2);
    store.db.execute_batch("CREATE TEMP TABLE identity(value); INSERT INTO identity VALUES (91); BEGIN; PRAGMA query_only=ON").unwrap();
    let before = store.db.total_changes();
    assert_eq!(
        public_controls::http_operation(&store, "default", "list", &json!({"limit":10}))
            .unwrap_err()
            .0,
        "result exceeds the result budget"
    );
    assert!(!store.db.is_autocommit());
    assert_eq!(store.db.total_changes(), before);
    assert_eq!(
        store
            .db
            .query_row("PRAGMA query_only", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT value FROM identity", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        91
    );
    store
        .db
        .execute_batch("ROLLBACK; PRAGMA query_only=OFF")
        .unwrap();
}

#[test]
fn bindings_bound_enriched_rows_and_refuse_many_rows_without_partial_success() {
    let (_root, store) = fixture();
    let base = json!({"profile":"default","kind":"source","owner":"a","root":"","device":1,"inode":1,"identity_policy":"strict"});
    let padding = BUDGET - catabolic_core::encode(&base).len();
    store
        .db
        .execute(
            "INSERT INTO bindings VALUES ('default','source','a',?,1,1)",
            ["x".repeat(padding)],
        )
        .unwrap();
    let before = store.db.total_changes();
    assert_eq!(
        public_reads::bindings(&store, "default", "source").unwrap()["bindings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.db.total_changes(), before);
    store
        .db
        .execute("UPDATE bindings SET root=root||'x'", [])
        .unwrap();
    assert_eq!(
        public_reads::bindings(&store, "default", "source")
            .unwrap_err()
            .0,
        "result exceeds the result budget"
    );
    store.db.execute("DELETE FROM bindings", []).unwrap();
    for n in 0..1050 {
        store
            .db
            .execute(
                "INSERT INTO bindings VALUES ('default','source',?,?,1,1)",
                params![n.to_string(), format!("{n}{}", "x".repeat(4000))],
            )
            .unwrap();
    }
    let before = store.db.total_changes();
    assert_eq!(
        public_reads::bindings(&store, "default", "source")
            .unwrap_err()
            .0,
        "result exceeds the result budget"
    );
    assert_eq!(store.db.total_changes(), before);
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM bindings", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1050
    );
}

#[test]
fn oversized_fact_is_rejected_before_source_collaborator_or_json_decode() {
    let (_root, store) = fixture();
    store.db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    store.db.execute("INSERT INTO file_facts(profile,file_id,operation,snapshot,job_id,data,status) VALUES ('default','missing','sniff','{}','missing-job',?,'complete')", ["x".repeat(BUDGET + 1)]).unwrap();
    let before = store.db.total_changes();
    assert_eq!(
        public_reads::processing(&store, "default", "facts", &json!({"file_id":"missing"}))
            .unwrap_err()
            .0,
        "result exceeds the result budget"
    );
    assert_eq!(store.db.total_changes(), before);
}
