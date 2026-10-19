// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen document-validation results and caller-owned transaction preservation.
use catabolic_store::{embedding, graphql, migration};
use serde_json::{Value, json};

#[test]
fn frozen_document_validation_cases_preserve_execution_preflight_and_the_owner() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-document-validation.json"
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
fn historical_overlap_controls_now_match_frozen_validation() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-document-validation.json"
    ))
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("controls.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE validation_control_marker(value); INSERT INTO validation_control_marker VALUES(53)",
    ).unwrap();
    for case in fixture["remaining_overlap_controls"].as_array().unwrap() {
        let document = case["document"].as_str().unwrap();
        let native = &case["expected"]["data"];
        assert_ne!(
            &case["native"]["stdout"]["data"], native,
            "{} must retain its historical gap",
            case["name"]
        );
        assert_eq!(
            graphql::execute(
                &path,
                document,
                "default",
                case["variables"].clone(),
                None,
                5000
            )
            .unwrap(),
            *native,
            "{}; resolved overlap execution control",
            case["name"],
        );
        for http in [false, true] {
            assert_eq!(
                graphql::validate_before_context(document, &case["variables"], 5000, http).unwrap(),
                Some(native.clone()),
                "{}; resolved overlap preflight http={http}",
                case["name"],
            );
        }
        for query_only in [0, 1] {
            owner
                .store()
                .unwrap()
                .db
                .pragma_update(None, "query_only", query_only)
                .unwrap();
            assert_eq!(
                owner
                    .query_call(
                        "default",
                        "graphql",
                        &json!({"document":document,"variables":case["variables"]})
                    )
                    .unwrap(),
                *native,
                "{}; resolved overlap persistent query_only={query_only}",
                case["name"],
            );
            assert!(owner.in_transaction().unwrap());
            let db = &owner.store().unwrap().db;
            assert_eq!(
                db.query_row("SELECT value FROM validation_control_marker", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                53,
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

#[test]
fn validation_limits_and_cancellation_preserve_the_borrowed_owner() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("bounds.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE bound_marker(value); INSERT INTO bound_marker VALUES(61)",
    ).unwrap();
    let invalid =
        "query($f:RenditionFilter={current:true,current:false}){renditions(matching:$f){nodes}}";
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-document-validation.json"
    ))
    .unwrap();
    let expected = &fixture["cases"][1]["expected"]["data"];
    let document_budget = format!("{invalid}{}", " ".repeat(131073));
    for query_only in [0, 1] {
        owner
            .store()
            .unwrap()
            .db
            .pragma_update(None, "query_only", query_only)
            .unwrap();
        for (document, variables, timeout, message) in [
            (
                document_budget.as_str(),
                json!({}),
                5000,
                "query document must be at most 131072 bytes",
            ),
            (invalid, json!([]), 5000, "variables must be a JSON object"),
            (
                invalid,
                json!({"ignored":"x".repeat(131073)}),
                5000,
                "variables exceed input budget",
            ),
            (
                invalid,
                json!({}),
                0,
                "timeout-ms must be between 1 and 60000",
            ),
            (
                invalid,
                json!({}),
                60001,
                "timeout-ms must be between 1 and 60000",
            ),
        ] {
            for http in [false, true] {
                assert_eq!(
                    graphql::validate_before_context(document, &variables, timeout, http)
                        .unwrap_err()
                        .to_string(),
                    message
                );
            }
            assert_eq!(
                owner
                    .query_call(
                        "default",
                        "graphql",
                        &json!({"document":document,"variables":variables,"timeout_ms":timeout})
                    )
                    .unwrap_err()
                    .to_string(),
                message
            );
        }
        // Document validation still precedes execution/cancellation and scope.
        assert_eq!(
            embedding::with_interrupt_check(
                || true,
                || owner.query_call("default", "graphql", &json!({"document":invalid}))
            )
            .unwrap(),
            *expected
        );
        let cancelled = embedding::with_interrupt_check(
            || true,
            || {
                owner.query_call(
                    "default",
                    "graphql",
                    &json!({"document":"{a:profile a:profile}"}),
                )
            },
        )
        .unwrap();
        assert_eq!(cancelled["data"], Value::Null);
        assert!(
            cancelled["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["message"] == "query execution interrupted"),
            "{cancelled}"
        );
        assert!(owner.in_transaction().unwrap());
        let db = &owner.store().unwrap().db;
        assert_eq!(
            db.query_row("SELECT value FROM bound_marker", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            61
        );
        assert_eq!(
            db.pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0))
                .unwrap(),
            query_only
        );
        assert_eq!(
            owner
                .query_call("default", "graphql", &json!({"document":"{profile}"}))
                .unwrap()["data"]["profile"],
            "default"
        );
    }
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
}
