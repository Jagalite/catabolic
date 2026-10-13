// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen empty-object completion witnesses and same-owner negative controls.
use catabolic_store::{embedding, graphql, migration};
use serde_json::{Value, json};

#[test]
fn frozen_empty_completion_responses_preserve_the_persistent_owner() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("completion.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    for (name, status) in [
        ("a", "pending"),
        ("b", "deferred"),
        ("c", "complete"),
        ("雪", "in_progress"),
    ] {
        owner
            .store()
            .unwrap()
            .db
            .execute(
                "INSERT INTO items(id,kind,metadata) VALUES(?,'movie',?)",
                [
                    name,
                    &json!({"title": name, "opaque": 2_i64.pow(61) + 1}).to_string(),
                ],
            )
            .unwrap();
        owner.store().unwrap().db.execute(
            "INSERT INTO item_workflow(item_id,status,revision,updated_at,updated_by) VALUES(?,?,1,'2026-01-01 00:00:00','fixture')",
            [name, status],
        ).unwrap();
    }
    // The completion state belongs to each GraphQL execution, not the borrowed
    // connection. Run the full matrix in one transaction and retain its marker.
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE completion_marker(value); INSERT INTO completion_marker VALUES(47)",
    ).unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-empty-completion.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let result = owner
            .query_call(
                "default",
                "graphql",
                &json!({
                    "document": case["document"],
                    "variables": case["variables"],
                    "operation_name": case.get("operation_name"),
                }),
            )
            .unwrap();
        assert_eq!(result, case["expected"]["data"], "{}", case["name"]);
        assert!(owner.in_transaction().unwrap());
        let db = &owner.store().unwrap().db;
        assert_eq!(
            db.query_row("SELECT value FROM completion_marker", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            47,
        );
        assert_eq!(
            db.pragma_query_value(None, "query_only", |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn interruption_does_not_become_empty_object_success() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("interrupted.db");
    migration::initialize(&path).unwrap();
    let result = embedding::with_interrupt_check(
        || true,
        || {
            graphql::execute(
                &path,
                "{items{nodes{id @skip(if:true)}}}",
                "default",
                json!({}),
                None,
                5000,
            )
        },
    );
    assert_eq!(
        result.unwrap(),
        json!({"data": null, "errors": [{"message": "query execution interrupted"}]}),
    );
}
