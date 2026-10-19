// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Scoped parser-budget admission on original source and disposable owners.
use catabolic_store::{embedding, graphql, migration};
use serde_json::{Value, json};

#[test]
fn frozen_precedence_envelopes_preserve_owner_scope_and_cancellation_order() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("precedence.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE precedence_marker(value); INSERT INTO precedence_marker VALUES(79)",
    ).unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-budget-precedence-v1.json"
    ))
    .unwrap();
    let corrections: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-budget-precedence-v2.json"
    ))
    .unwrap();
    assert_eq!(
        corrections["contract"],
        "native-graphql-parser-budget-precedence-v2"
    );
    assert_eq!(
        corrections["historical_replacements"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    let temp_scope = |db: &rusqlite::Connection| {
        db.prepare("SELECT type,name,sql FROM sqlite_temp_master ORDER BY type,name")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    };
    let before_scope = temp_scope(&owner.store().unwrap().db);
    let before_changes = owner.store().unwrap().db.total_changes();
    for query_only in [0, 1] {
        owner
            .store()
            .unwrap()
            .db
            .pragma_update(None, "query_only", query_only)
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
                    case["name"]
                );
            }
            assert_eq!(
                embedding::with_interrupt_check(
                    || true,
                    || owner.query_call("default", "graphql", &json!({"document":document}))
                )
                .unwrap(),
                *expected,
                "{}",
                case["name"]
            );
            assert!(owner.in_transaction().unwrap());
            let db = &owner.store().unwrap().db;
            assert_eq!(
                db.query_row("SELECT value FROM precedence_marker", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                79
            );
            assert_eq!(
                db.pragma_query_value(None, "query_only", |r| r.get::<_, i64>(0))
                    .unwrap(),
                query_only
            );
            assert_eq!(db.total_changes(), before_changes);
            assert_eq!(temp_scope(db), before_scope);
        }
        assert_eq!(checked, 64);
    }
    owner
        .store()
        .unwrap()
        .db
        .pragma_update(None, "query_only", 0)
        .unwrap();
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        owner
            .query_call("default", "graphql", &json!({"document":"{profile}"}))
            .unwrap()["data"]["profile"],
        "default"
    );
}

#[test]
fn byte_variable_timeout_admission_still_precedes_scoped_syntax_errors() {
    for prefix in [
        "?",
        "{items(first:)}",
        "{items()}",
        "{items(first 1)}",
        "{items(first:01)}",
        "!",
        "😀",
        r#"{items(search:"\q")}"#,
        "{items(first:1e+)}",
        "{items(first:1x)}",
        "'",
        r#"{items(search:"\uD800")}"#,
    ] {
        let over = format!("{prefix}{}", " profile".repeat(4001));
        let oversize = format!("{prefix}{}", " ".repeat(131073));
        for (document, variables, timeout, expected) in [
            (
                over.as_str(),
                json!({}),
                0,
                "timeout-ms must be between 1 and 60000",
            ),
            (
                over.as_str(),
                json!({}),
                60001,
                "timeout-ms must be between 1 and 60000",
            ),
            (
                oversize.as_str(),
                json!({}),
                5000,
                "query document must be at most 131072 bytes",
            ),
            (
                over.as_str(),
                json!([]),
                5000,
                "variables must be a JSON object",
            ),
            (
                over.as_str(),
                json!({"x":"x".repeat(131072)}),
                5000,
                "variables exceed input budget",
            ),
        ] {
            for http in [false, true] {
                assert_eq!(
                    graphql::validate_before_context(document, &variables, timeout, http)
                        .unwrap_err()
                        .to_string(),
                    expected
                );
            }
        }
    }
}
