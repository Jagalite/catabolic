// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Comment accounting rejects before context/resolvers without altering owners.
use catabolic_store::{embedding, graphql, migration};
use serde_json::{Value, json};

#[test]
fn frozen_comment_rejections_preserve_owner_and_both_preflight_modes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("comment-tokens.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE comment_marker(value); INSERT INTO comment_marker VALUES(73)",
    ).unwrap();
    let before = owner.store().unwrap().db.total_changes();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-comment-token-budget-v1.json"
    ))
    .unwrap();
    let mut cases: Vec<(&str, &Value)> = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["token_count"].as_u64().unwrap() > 4000)
        .map(|case| {
            (
                case["document"].as_str().unwrap(),
                &case["reference"]["stdout"]["data"],
            )
        })
        .collect();
    let original = &fixture["historical_replacements"][0];
    cases.push((
        original["document"].as_str().unwrap(),
        &original["expected"]["stdout"]["data"],
    ));
    assert_eq!(cases.len(), 35);
    for (document, expected) in cases {
        for http in [false, true] {
            assert_eq!(
                graphql::validate_before_context(document, &json!({}), 5000, http).unwrap(),
                Some(expected.clone()),
            );
        }
        assert_eq!(
            owner
                .query_call("default", "graphql", &json!({"document":document}))
                .unwrap(),
            *expected,
        );
        assert!(owner.in_transaction().unwrap());
        let db = &owner.store().unwrap().db;
        assert_eq!(db.total_changes(), before);
        assert_eq!(
            db.query_row("SELECT value FROM comment_marker", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            73,
        );
        assert_eq!(
            db.pragma_query_value(None, "query_only", |r| r.get::<_, i64>(0))
                .unwrap(),
            0,
        );
    }
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn comment_padding_cannot_bypass_timeout_byte_or_variable_admission() {
    let comments = format!("{{profile}}{}", "\n# comment".repeat(4001));
    for http in [false, true] {
        for (document, variables, timeout, expected) in [
            (
                comments.clone(),
                json!({}),
                0,
                "timeout-ms must be between 1 and 60000",
            ),
            (
                comments.clone(),
                json!({}),
                60001,
                "timeout-ms must be between 1 and 60000",
            ),
            (
                format!("{{profile}}#{}", "雪".repeat(43688)),
                json!({}),
                5000,
                "query document must be at most 131072 bytes",
            ),
            (
                comments.clone(),
                json!([]),
                5000,
                "variables must be a JSON object",
            ),
            (
                comments.clone(),
                json!({"x":"x".repeat(131072)}),
                5000,
                "variables exceed input budget",
            ),
        ] {
            assert_eq!(
                graphql::validate_before_context(&document, &variables, timeout, http)
                    .unwrap_err()
                    .to_string(),
                expected,
            );
        }
    }
}
