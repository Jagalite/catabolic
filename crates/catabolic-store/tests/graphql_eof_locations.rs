// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen EOF errors are output-only, before context and owner changes.
use catabolic_store::{embedding, graphql, migration};
use serde_json::{Value, json};

#[test]
fn frozen_eof_locations_preserve_direct_preflight_and_persistent_owners() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-eof-locations-v1.json"
    ))
    .unwrap();
    assert_eq!(fixture["contract"], "native-graphql-eof-locations-v1");
    let historical = fixture["historical_replacements"].as_array().unwrap();
    let regressions = fixture["cases"].as_array().unwrap();
    assert_eq!((historical.len(), regressions.len()), (3, 42));
    let cases: Vec<_> = historical
        .iter()
        .map(|case| (&case["document"], &case["expected"]["stdout"]["data"]))
        .chain(
            regressions
                .iter()
                .map(|case| (&case["document"], &case["reference"]["stdout"]["data"])),
        )
        .collect();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("eof-locations.db");
    migration::initialize(&path).unwrap();
    for (document, expected) in &cases {
        let document = document.as_str().unwrap();
        for http in [false, true] {
            assert_eq!(
                graphql::validate_before_context(document, &json!({}), 5000, http).unwrap(),
                Some((*expected).clone()),
            );
        }
        assert_eq!(
            graphql::execute(&path, document, "default", json!({}), None, 5000).unwrap(),
            **expected,
        );
    }
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE eof_marker(value); INSERT INTO eof_marker VALUES(73)",
    ).unwrap();
    for query_only in [0, 1] {
        owner
            .store()
            .unwrap()
            .db
            .pragma_update(None, "query_only", query_only)
            .unwrap();
        for (document, expected) in &cases {
            assert_eq!(
                owner
                    .query_call("default", "graphql", &json!({"document":document}))
                    .unwrap(),
                **expected,
            );
            assert!(owner.in_transaction().unwrap());
            let db = &owner.store().unwrap().db;
            assert_eq!(
                db.query_row("SELECT value FROM eof_marker", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                73
            );
            assert_eq!(
                db.pragma_query_value(None, "query_only", |r| r.get::<_, i64>(0))
                    .unwrap(),
                query_only
            );
        }
    }
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
}
