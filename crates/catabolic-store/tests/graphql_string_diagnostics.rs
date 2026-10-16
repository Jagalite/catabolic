// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Narrow string lexical diagnostics on original source and disposable owners.
use catabolic_store::{embedding, graphql, migration};
use serde_json::{Value, json};

#[test]
fn frozen_string_errors_preserve_preflight_owner_and_local_http_state() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("strings.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE string_marker(value); INSERT INTO string_marker VALUES(71)",
    ).unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-string-diagnostics-v1.json"
    ))
    .unwrap();
    let ownership: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-diagnostic-ownership-v1.json"
    ))
    .unwrap();
    assert_eq!(
        ownership["contract"],
        "native-graphql-parser-diagnostic-ownership-v1"
    );
    assert_eq!(
        ownership["historical_replacements"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let corrections: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-budget-precedence-v1.json"
    ))
    .unwrap();
    let mut checked = 0;
    for case in fixture["cases"].as_array().unwrap() {
        if !case["args"].as_array().unwrap().is_empty() {
            continue;
        }
        let corrected = corrections["historical_replacements"]
            .as_array()
            .unwrap()
            .iter()
            .chain(ownership["historical_replacements"].as_array().unwrap())
            .find(|replacement| replacement["historical_control"] == case["name"]);
        if let Some(replacement) = corrected {
            for (original, promoted) in [
                ("document", "document"),
                ("args", "args"),
                ("reference", "expected"),
                ("baseline", "baseline"),
            ] {
                assert_eq!(case[original], replacement[promoted]);
            }
        }
        let expected = if case["expectation"] == "oracle" || corrected.is_some() {
            &case["reference"]["stdout"]["data"]
        } else {
            &case["baseline"]["legacy"]["stdout"]["data"]
        };
        // The complete-process Python replay also checks successful controls.
        if !expected["errors"].is_array() {
            continue;
        }
        checked += 1;
        let document = case["document"].as_str().unwrap();
        for http in [false, true] {
            assert_eq!(
                graphql::validate_before_context(document, &json!({}), 5000, http).unwrap(),
                Some(expected.clone()),
                "{} http={http}",
                case["name"],
            );
        }
        assert_eq!(
            owner
                .query_call("default", "graphql", &json!({"document":document}))
                .unwrap(),
            *expected,
            "{}",
            case["name"],
        );
        assert!(owner.in_transaction().unwrap());
        let db = &owner.store().unwrap().db;
        assert_eq!(
            db.query_row("SELECT value FROM string_marker", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            71
        );
        assert_eq!(
            db.pragma_query_value(None, "query_only", |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(checked, 48);
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
}
