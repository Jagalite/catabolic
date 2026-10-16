// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen raw-recorded Int output controls; no public mutation-input claim.
use catabolic_store::{Store, embedding, graphql, migration};
use serde_json::{Value, json};

#[test]
fn recorded_int_output_matches_frozen_envelopes_and_preserves_borrowed_owner() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("recorded-int.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    for (name, status) in [
        ("a", "pending"),
        ("b", "deferred"),
        ("c", "complete"),
        ("雪", "in_progress"),
    ] {
        store
            .db
            .execute(
                "INSERT INTO items(id,kind,metadata) VALUES(?,'movie',?)",
                [
                    name,
                    &json!({"title":name,"opaque":2_i64.pow(61)+1}).to_string(),
                ],
            )
            .unwrap();
        store.db.execute("INSERT INTO item_workflow(item_id,status,revision,updated_at,updated_by) VALUES(?,?,1,'2026-01-01 00:00:00','fixture')",[name,status]).unwrap();
    }
    drop(store);
    let cases: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-recorded-int-v1.json"
    ))
    .unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch("PRAGMA query_only=OFF; CREATE TEMP TABLE scalar_marker(value); INSERT INTO scalar_marker VALUES(71)").unwrap();
    for case in cases["cases"].as_array().unwrap() {
        let raw = case["position_value"].as_str().unwrap();
        owner
            .store()
            .unwrap()
            .db
            .execute("DELETE FROM item_relationships", [])
            .unwrap();
        owner.store().unwrap().db.execute("INSERT INTO item_relationships(id,source_id,target_id,kind,position) VALUES('r','a','b','related',?)", [raw]).unwrap();
        owner.store().unwrap().db.execute_batch("BEGIN").unwrap();
        let document = case["document"].as_str().unwrap();
        let direct = graphql::execute(&path, document, "default", json!({}), None, 5000).unwrap();
        let persistent = owner
            .query_call("default", "graphql", &json!({"document":document}))
            .unwrap();
        for (surface, actual) in [("direct", direct), ("persistent", persistent)] {
            assert_eq!(
                actual, case["reference"]["stdout"]["data"],
                "{} / {surface}",
                case["name"]
            );
        }
        assert!(owner.in_transaction().unwrap());
        let store = owner.store().unwrap();
        assert!(
            !store
                .db
                .pragma_query_value::<bool, _>(None, "query_only", |row| row.get(0))
                .unwrap()
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT value FROM scalar_marker", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            71
        );
        let stored: (String, String) = store
            .db
            .query_row(
                "SELECT typeof(position), position FROM item_relationships",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, ("text".into(), raw.into()));
        store.db.execute_batch("ROLLBACK").unwrap();
    }
}
