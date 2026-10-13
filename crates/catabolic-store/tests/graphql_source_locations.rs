// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Exact frozen static diagnostic locations; preserve the persistent owner.
use catabolic_store::{embedding, graphql, migration};
use serde_json::{Value, json};

#[test]
fn frozen_source_locations_preserve_complete_errors_and_the_owner() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("locations.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE location_marker(value); INSERT INTO location_marker VALUES(47)",
    ).unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-source-locations.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        for http in [false, true] {
            let validation = graphql::validate_before_context(
                case["document"].as_str().unwrap(),
                &case["variables"],
                5000,
                http,
            )
            .unwrap();
            // Valid literals such as first:0 can still fail at the resolver
            // boundary, with execution extensions. Preflight must allow them.
            let response = &case["expected"]["data"];
            let expected = response
                .get("extensions")
                .is_none()
                .then(|| response.clone());
            assert_eq!(validation, expected, "{}; http={http}", case["name"]);
        }
        let result = owner
            .query_call(
                "default",
                "graphql",
                &json!({"document":case["document"],"variables":case["variables"]}),
            )
            .unwrap();
        assert_eq!(result, case["expected"]["data"], "{}", case["name"]);
        assert!(owner.in_transaction().unwrap());
        let db = &owner.store().unwrap().db;
        assert_eq!(
            db.query_row("SELECT value FROM location_marker", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            47,
        );
        assert_eq!(
            db.pragma_query_value(None, "query_only", |r| r.get::<_, i64>(0))
                .unwrap(),
            0,
        );
    }
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
}
