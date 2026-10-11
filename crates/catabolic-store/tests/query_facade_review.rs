// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent persistent-query adapter checks using disposable catalogs only.
use catabolic_store::{
    definitions, embedding::Session, embedding::with_interrupt_check, migration,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn owner() -> (tempfile::TempDir, Session) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("query-review.db");
    migration::initialize(&path).unwrap();
    (root, Session::open(&path, true, false).unwrap())
}

fn scalar(owner: &Session, sql: &str) -> i64 {
    owner
        .store()
        .unwrap()
        .db
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

#[test]
fn lending_restores_existing_connection_flags_and_temporary_identity() {
    let (_root, mut owner) = owner();
    // Establish the production TEMP layout first; initial setup sets temp_store.
    owner
        .query_call("default", "sql", &json!({"sql":"SELECT 1"}))
        .unwrap();
    owner.store().unwrap().db.execute_batch(
        "CREATE TEMP TABLE review_identity(value); INSERT INTO review_identity VALUES (47); PRAGMA busy_timeout=2345; PRAGMA query_only=ON;"
    ).unwrap();
    for (operation, request, is_error) in [
        ("sql", json!({"sql":"SELECT 1"}), false),
        ("sql", json!({"sql":"SELECT nonexistent_column"}), true),
        ("graphql", json!({"document":"{profile}"}), false),
        (
            "graphql",
            json!({"document":"{items(first:0){nodes{id}}}"}),
            false,
        ),
    ] {
        assert_eq!(
            owner.query_call("default", operation, &request).is_err(),
            is_error
        );
        assert_eq!(scalar(&owner, "PRAGMA query_only"), 1);
        assert_eq!(scalar(&owner, "PRAGMA busy_timeout"), 2345);
        assert_eq!(scalar(&owner, "SELECT value FROM review_identity"), 47);
        assert!(owner.store().unwrap().owns_writer_lock());
    }
    owner
        .store()
        .unwrap()
        .db
        .execute_batch("PRAGMA query_only=OFF; INSERT INTO profiles VALUES ('restored')")
        .unwrap();
}

#[test]
fn sql_interrupt_preserves_outer_transaction_and_removes_progress_handler() {
    let (_root, mut owner) = owner();
    let scope = owner.begin().unwrap();
    owner
        .store()
        .unwrap()
        .db
        .execute("INSERT INTO profiles VALUES ('outer')", [])
        .unwrap();
    let polls = Arc::new(AtomicUsize::new(0));
    let captured = polls.clone();
    let result = with_interrupt_check(
        move || {
            captured.fetch_add(1, Ordering::SeqCst);
            true
        },
        || {
            owner.query_call("default", "sql", &json!({
            "sql":"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n",
            "timeout_ms":60000
        }))
        },
    );
    assert!(result.is_err());
    assert!(polls.load(Ordering::SeqCst) > 0);
    assert!(owner.in_transaction().unwrap());
    assert_eq!(
        scalar(&owner, "SELECT count(*) FROM profiles WHERE id='outer'"),
        1
    );
    assert_eq!(scalar(&owner, "PRAGMA query_only"), 0);
    assert_eq!(
        owner
            .query_call("default", "sql", &json!({"sql":"SELECT 9"}))
            .unwrap()["rows"],
        json!([[9]])
    );
    owner.finish(scope, false).unwrap();
    assert_eq!(
        scalar(&owner, "SELECT count(*) FROM profiles WHERE id='outer'"),
        0
    );
    owner
        .store()
        .unwrap()
        .db
        .execute("INSERT INTO profiles VALUES ('after')", [])
        .unwrap();
}

#[test]
fn cancellation_callback_can_query_an_independent_owner_without_losing_outer_bridge() {
    let (_outer_root, mut outer) = owner();
    let (_inner_root, mut inner) = owner();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let result = with_interrupt_check(
        move || {
            observed.fetch_add(1, Ordering::SeqCst);
            let nested = with_interrupt_check(
                || false,
                || inner.query_call("default", "sql", &json!({
                    "sql":"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000) SELECT sum(x) FROM n"
                })),
            ).unwrap();
            assert_eq!(nested["rows"], json!([[500500]]));
            true
        },
        || {
            outer.query_call("default", "sql", &json!({
            "sql":"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n"
        }))
        },
    );
    assert!(result.is_err());
    assert!(calls.load(Ordering::SeqCst) > 0);
    assert_eq!(
        outer
            .query_call("default", "sql", &json!({"sql":"SELECT 3"}))
            .unwrap()["rows"],
        json!([[3]])
    );
}

#[test]
fn resolver_interrupt_unwind_restores_owner_and_enclosing_transaction() {
    let (_root, mut owner) = owner();
    owner
        .query_call("default", "graphql", &json!({"document":"{profile}"}))
        .unwrap();
    let scope = owner.begin().unwrap();
    owner
        .store()
        .unwrap()
        .db
        .execute("INSERT INTO profiles VALUES ('outer')", [])
        .unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_interrupt_check(
            || panic!("independent resolver cancellation unwind"),
            || owner.query_call("default", "graphql", &json!({"document":"{profile}"})),
        )
    }));
    assert!(outcome.is_err());
    assert!(owner.store().unwrap().owns_writer_lock());
    assert!(owner.in_transaction().unwrap());
    assert_eq!(scalar(&owner, "PRAGMA query_only"), 0);
    assert_eq!(
        scalar(&owner, "SELECT count(*) FROM profiles WHERE id='outer'"),
        1
    );
    assert_eq!(
        owner
            .query_call("default", "graphql", &json!({"document":"{profile}"}))
            .unwrap()["data"]["profile"],
        "default"
    );
    owner.finish(scope, false).unwrap();
}

#[test]
fn query_list_binds_scalar_cursor_without_truthiness_conversion() {
    let (_root, owner) = owner();
    for id in ["0-first", "2-second", "a-third"] {
        owner.store().unwrap().db.execute(
            "INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES (?,'default',?,1,'{}',?)",
            [id, id, id],
        ).unwrap();
    }
    let ids = |cursor: Value| {
        definitions::dispatch(
            owner.store().unwrap(),
            "default",
            "query-list",
            &json!({"after":cursor}),
        )
        .unwrap()["queries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert!(ids(Value::Null).is_empty());
    for cursor in [json!(false), json!(0), json!("")] {
        assert_eq!(ids(cursor), ["0-first", "2-second", "a-third"]);
    }
    // TEXT affinity makes numeric 0.0 the text '0.0', after '0-first'.
    for cursor in [json!(0.0), json!(true), json!(1), json!(1.5), json!("1")] {
        assert_eq!(ids(cursor), ["2-second", "a-third"]);
    }
    assert_eq!(ids(json!(9)), ["a-third"]);
}

#[test]
fn reused_session_charges_distinct_queries_but_not_cached_selections() {
    let (_root, mut owner) = owner();
    let mut ids = vec![];
    for name in ["a", "b"] {
        let saved = definitions::dispatch(owner.store().unwrap(), "default", "query-put", &json!({
            "name":name,
            "value":{"selection":{"language":"sql","query":format!("SELECT '{name}' AS item_id")}}
        })).unwrap();
        ids.push(saved["id"].clone());
    }
    let session = owner
        .query_call(
            "default",
            "evaluation-create",
            &json!({"max_ids":1,"max_bytes":4194304}),
        )
        .unwrap()["token"]
        .clone();
    let first = owner
        .query_call("default", "select", &json!({"id":ids[0],"session":session}))
        .unwrap();
    assert_eq!(first[1], json!(["a"]));
    assert_eq!(
        owner
            .query_call("default", "select", &json!({"id":ids[0],"session":session}))
            .unwrap(),
        first
    );
    assert_eq!(
        owner
            .query_call("default", "select", &json!({"id":ids[1],"session":session}))
            .unwrap_err()
            .to_string(),
        "complete query selection exceeds its ID budget"
    );
    assert!(owner.store().unwrap().owns_writer_lock());
    owner
        .query_call("default", "evaluation-drop", &json!({"session":session}))
        .unwrap();
    assert_eq!(
        owner
            .query_call("default", "evaluation-check", &json!({"session":session}))
            .unwrap_err()
            .to_string(),
        "evaluation_session_identity_mismatch"
    );
}
