// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent read-adapter checks. All data belongs to disposable catalogs.
use catabolic_store::{
    Store, embedding::with_interrupt_check, graphql, migration, sql, work_inbox,
};
use serde_json::{Value, json};

fn fixture() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("inbox-review.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store
        .db
        .execute_batch(
            "INSERT INTO profiles VALUES ('other');
         INSERT INTO items(id,kind,metadata) VALUES ('a','movie','{}');
         INSERT INTO item_workflow(item_id,status,revision,updated_at,updated_by)
         VALUES('a','pending',1,'2026-01-01 00:00:00','fixture');",
        )
        .unwrap();
    sql::setup(&store.db).unwrap();
    store
        .db
        .execute_batch(
        "PRAGMA query_only=OFF; CREATE TEMP TABLE inbox_identity(value); INSERT INTO inbox_identity VALUES (79);
         PRAGMA busy_timeout=1234; BEGIN IMMEDIATE;
         INSERT INTO profiles VALUES ('outer'); PRAGMA query_only=ON;",
        )
        .unwrap();
    (root, store)
}

fn same_owner(store: &Store) {
    assert!(store.owns_writer_lock());
    assert!(!store.db.is_autocommit());
    for (query, expected) in [
        ("PRAGMA query_only", 1),
        ("PRAGMA busy_timeout", 1234),
        ("SELECT value FROM inbox_identity", 79),
        ("SELECT count(*) FROM profiles WHERE id='outer'", 1),
    ] {
        assert_eq!(
            store
                .db
                .query_row(query, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            expected
        );
    }
}

#[test]
fn inbox_success_returns_original_store_and_transaction() {
    let (_root, mut store) = fixture();
    for (operation, request) in [
        ("list", json!({"limit":1})),
        ("summary", json!({})),
        ("show", json!({"work_key":"item:61"})),
    ] {
        let (returned, result) =
            work_inbox::execute(store, "default", operation, &request).unwrap();
        store = returned;
        assert_eq!(result["evaluation_complete"], true);
        same_owner(&store);
    }
    store
        .db
        .execute_batch("PRAGMA query_only=OFF; ROLLBACK")
        .unwrap();
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM profiles WHERE id='outer'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn scalar_rejections_leave_original_store_state_untouched() {
    let (_root, mut store) = fixture();
    let document =
        "query A($n:Int!,$id:ID!){items(first:$n){nodes{id}} item(id:$id){id}} query B{profile}";
    for selected in [Some("A"), Some("Missing"), None] {
        let (returned, result) = graphql::execute_store(
            store,
            document,
            "default",
            json!({"n":2147483648_i64}),
            selected,
            5000,
        )
        .unwrap();
        store = returned;
        assert_eq!(result["data"], Value::Null);
        assert_eq!(result["extensions"]["records"], 0);
        if selected == Some("A") {
            assert_eq!(result["errors"].as_array().unwrap().len(), 2);
        }
        same_owner(&store);
    }
}

#[test]
fn all_early_query_branches_poll_cancellation_without_losing_store() {
    let (_root, mut store) = fixture();
    let document = "query A($n:Int!){items(first:$n){nodes{id}}} query B{profile}";
    for selected in [Some("A"), Some("Missing"), None] {
        let (returned, result) = with_interrupt_check(
            || true,
            || {
                graphql::execute_store(
                    store,
                    document,
                    "default",
                    json!({"n":2147483648_i64}),
                    selected,
                    5000,
                )
            },
        )
        .unwrap();
        store = returned;
        assert_eq!(
            result,
            json!({"data":null,"errors":[{"message":"query execution interrupted"}]})
        );
        same_owner(&store);
    }
}

#[test]
fn evidence_refresh_detects_changed_owner_revision() {
    let (_root, store) = fixture();
    let (store, first) =
        work_inbox::execute(store, "default", "show", &json!({"work_key":"item:61"})).unwrap();
    let expected = first["evidence_token"].as_str().unwrap();
    assert_eq!(work_inbox::evidence_token(&first["entry"]), expected);
    let (store, _) = work_inbox::execute(
        store,
        "default",
        "show",
        &json!({"work_key":"item:61","expected_evidence":expected}),
    )
    .unwrap();
    store.db.execute_batch("PRAGMA query_only=OFF; UPDATE item_workflow SET revision=revision+1 WHERE item_id='a'; PRAGMA query_only=ON").unwrap();
    let error = work_inbox::execute(
        store,
        "default",
        "show",
        &json!({"work_key":"item:61","expected_evidence":expected}),
    )
    .err()
    .unwrap();
    assert_eq!(
        error.0,
        "inbox evidence changed; reload the owner record and replan"
    );
}

#[test]
fn inbox_cancellation_is_reported_without_a_success_report() {
    let (_root, store) = fixture();
    let result = with_interrupt_check(
        || true,
        || work_inbox::execute(store, "default", "list", &json!({})),
    );
    assert!(result.is_err());
    assert!(
        result
            .err()
            .unwrap()
            .0
            .contains("inbox evaluation incomplete")
    );
}

#[test]
fn retained_read_preserves_owner_and_profile_boundary() {
    let (_root, store) = fixture();
    store.db.execute_batch(
        "PRAGMA query_only=OFF;
         INSERT INTO retained_hardlinks VALUES('first','default','global','recorded/first','original/first','{}','2026-01-01');
         INSERT INTO retained_hardlinks VALUES('second','default','global','recorded/second','original/second','{}','2026-01-01');
         INSERT INTO retained_hardlinks VALUES('private','other','global','recorded/private','original/private','{}','2026-01-01');
         PRAGMA query_only=ON;",
    ).unwrap();
    let first = work_inbox::retained(&store, "default", "global", &json!({"limit":1})).unwrap();
    assert_eq!(first["retained"].as_array().unwrap().len(), 1);
    assert_eq!(first["next_after"], "first");
    let second = work_inbox::retained(
        &store,
        "default",
        "global",
        &json!({"limit":1,"after":first["next_after"]}),
    )
    .unwrap();
    assert_eq!(second["retained"][0]["id"], "second");
    assert_eq!(second["next_after"], Value::Null);
    same_owner(&store);
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM retained_hardlinks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
}
