// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded token-budget diagnostics retain original input and owner safeguards.
use catabolic_store::{embedding, graphql, migration};
use serde_json::{Value, json};

#[test]
fn three_budget_locations_preserve_owner_and_local_http_preflight() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("token-locations.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE token_marker(value); INSERT INTO token_marker VALUES(71)",
    ).unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-source-locations.json"
    ))
    .unwrap();
    let mut count = 0;
    for case in fixture["unchanged_parser_and_budget_controls"]
        .as_array()
        .unwrap()
    {
        if !case["name"].as_str().unwrap().contains("token-budget") {
            continue;
        }
        count += 1;
        let document = case["document"].as_str().unwrap();
        let expected = &case["reference"]["stdout"]["data"];
        for http in [false, true] {
            assert_eq!(
                graphql::validate_before_context(document, &json!({}), 5000, http).unwrap(),
                Some(expected.clone())
            );
        }
        let result = owner
            .query_call("default", "graphql", &json!({"document":document}))
            .unwrap();
        assert_eq!(&result, expected);
        assert!(owner.in_transaction().unwrap());
        let db = &owner.store().unwrap().db;
        assert_eq!(
            db.query_row("SELECT value FROM token_marker", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            71
        );
        assert_eq!(
            db.pragma_query_value(None, "query_only", |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(count, 3);
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn input_budget_timeout_variables_and_earlier_error_precedence_stay_bounded() {
    let over = "profile ".repeat(4001);
    for http in [false, true] {
        for (document, variables, timeout, expected) in [
            (
                over.clone(),
                json!({}),
                0,
                "timeout-ms must be between 1 and 60000",
            ),
            (
                "profile ".repeat(16385),
                json!({}),
                5000,
                "query document must be at most 131072 bytes",
            ),
            (
                over.clone(),
                json!([]),
                5000,
                "variables must be a JSON object",
            ),
            (
                over.clone(),
                json!({"x":"x".repeat(131072)}),
                5000,
                "variables exceed input budget",
            ),
        ] {
            assert_eq!(
                graphql::validate_before_context(&document, &variables, timeout, http)
                    .unwrap_err()
                    .to_string(),
                expected
            );
        }
        let earlier = "{items(first:){nodes{id}}}";
        let under = graphql::validate_before_context(earlier, &json!({}), 5000, http)
            .unwrap()
            .unwrap();
        assert_eq!(
            under["errors"][0]["message"],
            "Syntax Error: Unexpected ')'."
        );
        for prefix in [earlier, "?"] {
            let result = graphql::validate_before_context(
                &format!("{prefix} {over}"),
                &json!({}),
                5000,
                http,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                result["errors"][0]["message"],
                if prefix == earlier {
                    "Syntax Error: Unexpected ')'."
                } else {
                    "Syntax Error: Unexpected character: '?'."
                }
            );
        }
    }
}
