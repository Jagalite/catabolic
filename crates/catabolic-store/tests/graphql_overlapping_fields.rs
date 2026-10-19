// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen document-validation results and caller-owned transaction preservation.
use catabolic_store::{embedding, graphql, migration};
use serde_json::{Value, json};

#[test]
fn frozen_overlapping_fields_cases_preserve_execution_preflight_and_the_owner() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-overlapping-fields.json"
    ))
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    for (catalog_name, catalog) in fixture["catalogs"].as_object().unwrap() {
        let path = root.path().join(format!("{catalog_name}.db"));
        migration::initialize(&path).unwrap();
        let mut owner = embedding::Session::open(&path, true, false).unwrap();
        for item_id in catalog["item_ids"].as_array().unwrap() {
            owner
                .store()
                .unwrap()
                .db
                .execute(
                    "INSERT INTO items(id,kind,metadata) VALUES(?,'movie','{}')",
                    [item_id.as_str().unwrap()],
                )
                .unwrap();
        }
        let cases: Vec<_> = fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| case["catalog"].as_str() == Some(catalog_name.as_str()))
            .collect();
        for case in &cases {
            let document = case["document"].as_str().unwrap();
            for (http, context) in [(false, "local"), (true, "http")] {
                let result =
                    graphql::validate_before_context(document, &case["variables"], 5000, http)
                        .unwrap();
                let expected = (!case["preflight"][context].is_null())
                    .then(|| case["preflight"][context].clone());
                assert_eq!(result, expected, "{}; preflight={context}", case["name"]);
            }
            assert_eq!(
                graphql::execute(
                    &path,
                    document,
                    "default",
                    case["variables"].clone(),
                    None,
                    5000,
                )
                .unwrap(),
                case["expected"]["data"],
                "{}; direct library",
                case["name"],
            );
        }
        catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
        owner.store().unwrap().db.execute_batch(
            "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE validation_marker(value); INSERT INTO validation_marker VALUES(47)",
        ).unwrap();
        // Both caller-selected query_only states survive every query, as do the
        // open transaction and connection-local TEMP marker.
        for query_only in [0, 1] {
            owner
                .store()
                .unwrap()
                .db
                .pragma_update(None, "query_only", query_only)
                .unwrap();
            for case in &cases {
                let result = owner
                    .query_call(
                        "default",
                        "graphql",
                        &json!({"document":case["document"],"variables":case["variables"]}),
                    )
                    .unwrap();
                assert_eq!(
                    result, case["expected"]["data"],
                    "{}; persistent query_only={query_only}",
                    case["name"],
                );
                assert!(owner.in_transaction().unwrap());
                let db = &owner.store().unwrap().db;
                assert_eq!(
                    db.query_row("SELECT value FROM validation_marker", [], |row| row
                        .get::<_, i64>(0))
                        .unwrap(),
                    47,
                );
                assert_eq!(
                    db.pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0))
                        .unwrap(),
                    query_only,
                );
            }
        }
        owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
    }
}

#[test]
fn overlap_validation_precedes_cancellation_without_losing_the_owner() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cancel.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch("PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE overlap_marker(value); INSERT INTO overlap_marker VALUES(83)").unwrap();
    for query_only in [0, 1] {
        owner
            .store()
            .unwrap()
            .db
            .pragma_update(None, "query_only", query_only)
            .unwrap();
        let document = "{...A ...B} fragment A on Query {same:profile} fragment B on Query {same:schemaVersion}";
        let expected = graphql::validate_before_context(document, &json!({}), 5000, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            embedding::with_interrupt_check(
                || true,
                || owner.query_call("default", "graphql", &json!({"document":document}))
            )
            .unwrap(),
            expected
        );
        let cancelled = embedding::with_interrupt_check(
            || true,
            || {
                owner.query_call(
                    "default",
                    "graphql",
                    &json!({"document":"{profile ...F} fragment F on Query {profile}"}),
                )
            },
        )
        .unwrap();
        assert!(cancelled["data"].is_null());
        assert!(
            cancelled["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["message"] == "query execution interrupted")
        );
        assert!(owner.in_transaction().unwrap());
        let db = &owner.store().unwrap().db;
        assert_eq!(
            db.query_row("SELECT value FROM overlap_marker", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            83
        );
        assert_eq!(
            db.pragma_query_value(None, "query_only", |r| r.get::<_, i64>(0))
                .unwrap(),
            query_only
        );
        assert_eq!(
            owner
                .query_call(
                    "default",
                    "graphql",
                    &json!({"document":"{profile ...F} fragment F on Query {profile}"})
                )
                .unwrap()["data"]["profile"],
            "default"
        );
    }
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
}
