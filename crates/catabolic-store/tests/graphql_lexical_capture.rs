// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Captured syntax controls only: no CLI-file or unsupported-argv substitution.
use catabolic_store::{embedding, graphql, migration};
use serde_json::{Value, json};

fn temp_scope(db: &rusqlite::Connection) -> Vec<(String, String, Option<String>)> {
    db.prepare("SELECT type,name,sql FROM sqlite_temp_master ORDER BY type,name")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn captured_180_syntax_controls_preserve_local_http_and_borrowed_owner_state() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-lexical-capture-v1.json"
    ))
    .unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 197);
    assert_eq!(fixture["promotions"].as_array().unwrap().len(), 104);
    let grammar: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-grammar-provenance-v1.json"
    ))
    .unwrap();
    assert_eq!(
        grammar["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["origin"] == "lexical-capture" && r["promotion"] == true)
            .count(),
        40
    );
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("lexical-capture.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE lexical_capture_marker(value); INSERT INTO lexical_capture_marker VALUES(105)"
    ).unwrap();
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
        for (index, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
            if case["preflight"] != true {
                continue;
            }
            checked += 1;
            assert!(case["args"].as_array().unwrap().is_empty());
            assert_eq!(case["frozen_parse"]["stdout"]["accepted"], false);
            let promoted = fixture["promotions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["index"].as_u64().unwrap() as usize == index);
            let grammar_promoted = grammar["rows"].as_array().unwrap().iter().any(|r| {
                r["fixture_contract"] == fixture["contract"]
                    && r["index"].as_u64().unwrap() as usize == index
                    && r["promotion"] == true
            });
            let expected = if promoted || grammar_promoted {
                &case["reference"]["stdout"]["data"]
            } else {
                &case["baseline"]["legacy"]["stdout"]["data"]
            };
            assert!(expected["errors"].is_array());
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
                "{} query_only={query_only}",
                case["name"]
            );
            assert!(owner.in_transaction().unwrap());
            let db = &owner.store().unwrap().db;
            assert_eq!(
                db.query_row("SELECT value FROM lexical_capture_marker", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                105
            );
            assert_eq!(
                db.pragma_query_value(None, "query_only", |r| r.get::<_, i64>(0))
                    .unwrap(),
                query_only
            );
            assert_eq!(db.total_changes(), before_changes);
            assert_eq!(temp_scope(db), before_scope);
        }
        assert_eq!(checked, 180);
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
