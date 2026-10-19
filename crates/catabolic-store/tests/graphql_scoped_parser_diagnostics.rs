// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Separately versioned admission and ordinary diagnostics on disposable owners.
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
fn separate_diagnostic_contracts_preserve_preflight_cancellation_and_owner_state() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("scoped-diagnostics.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE scoped_diagnostic_marker(value); INSERT INTO scoped_diagnostic_marker VALUES(83)",
    ).unwrap();
    let before_scope = temp_scope(&owner.store().unwrap().db);
    let before_changes = owner.store().unwrap().db.total_changes();
    let scalar: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-unsupported-scalar-precedence-v1.json"
    ))
    .unwrap();
    assert_eq!(scalar["stage01_replacements"].as_array().unwrap().len(), 1);
    let boundary: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-boundary-ownership-v1.json"
    ))
    .unwrap();
    assert_eq!(
        boundary["contract"],
        "native-graphql-parser-boundary-ownership-v1"
    );
    let historical_ownership: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-diagnostic-ownership-v1.json"
    ))
    .unwrap();
    let replacements = boundary["ownership_replacements"].as_array().unwrap();
    assert_eq!(replacements.len(), 4);
    for (replacement, (index, name)) in replacements.iter().zip([
        (18, "unowned-noninitial-question"),
        (19, "unowned-initial-unicode"),
        (20, "unowned-empty-arguments"),
        (24, "unowned-unsupported-slash"),
    ]) {
        let original = &historical_ownership["cases"][index];
        assert_eq!(replacement["historical_pointer"], format!("/cases/{index}"));
        assert_eq!(replacement["historical_control"], name);
        assert_eq!(original["name"], name);
        assert_eq!(original["expectation"], "baseline");
        for (old, new) in [
            ("document", "document"),
            ("args", "args"),
            ("reference", "expected"),
            ("baseline", "baseline"),
        ] {
            assert_eq!(original[old], replacement[new]);
        }
        assert_ne!(original["reference"], original["baseline"]["legacy"]);
    }
    let lexical: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-lexical-ownership-v1.json"
    ))
    .unwrap();
    assert_eq!(
        lexical["contract"],
        "native-graphql-parser-lexical-ownership-v1"
    );
    let lexical_replacements = lexical["replacements"].as_array().unwrap();
    assert_eq!(lexical_replacements.len(), 13);
    assert_eq!(lexical["preserved_promotions"].as_array().unwrap().len(), 5);
    let field_tokens: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-field-value-tokens-v1.json"
    ))
    .unwrap();
    assert_eq!(
        field_tokens["contract"],
        "native-graphql-parser-field-value-tokens-v1"
    );
    let field_token_replacements = field_tokens["replacements"].as_array().unwrap();
    assert_eq!(field_token_replacements.len(), 6);
    let directive_closers: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-directive-value-closers-v1.json"
    ))
    .unwrap();
    assert_eq!(
        directive_closers["contract"],
        "native-graphql-parser-directive-value-closers-v1"
    );
    let grammar: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-grammar-provenance-v1.json"
    ))
    .unwrap();
    assert_eq!(
        grammar["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["origin"] == "historical" && r["promotion"] == true)
            .count(),
        24
    );
    let directive_closer_replacements = directive_closers["replacements"].as_array().unwrap();
    assert_eq!(directive_closer_replacements.len(), 7);

    let executable: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-executable-punctuation-v1.json"
    ))
    .unwrap();
    assert_eq!(
        executable["contract"],
        "native-graphql-parser-executable-punctuation-v1"
    );
    let executable_replacements = executable["replacements"].as_array().unwrap();
    assert_eq!(executable_replacements.len(), 3);

    for (text, contract, replacement_key, promoted_count, case_count, preflight_count) in [
        (
            include_str!("../../../tests/parity/graphql-parser-budget-precedence-v2.json"),
            "native-graphql-parser-budget-precedence-v2",
            "historical_replacements",
            6,
            150,
            148,
        ),
        (
            include_str!("../../../tests/parity/graphql-parser-diagnostic-ownership-v1.json"),
            "native-graphql-parser-diagnostic-ownership-v1",
            "historical_replacements",
            3,
            38,
            29,
        ),
        (
            include_str!("../../../tests/parity/graphql-unsupported-scalar-precedence-v1.json"),
            "native-graphql-unsupported-scalar-precedence-v1",
            "stage01_replacements",
            1,
            31,
            31,
        ),
        (
            include_str!("../../../tests/parity/graphql-parser-boundary-ownership-v1.json"),
            "native-graphql-parser-boundary-ownership-v1",
            "ownership_replacements",
            4,
            61,
            56,
        ),
        (
            include_str!("../../../tests/parity/graphql-parser-required-value-precedence-v1.json"),
            "native-graphql-parser-required-value-precedence-v1",
            "",
            0,
            50,
            50,
        ),
    ] {
        let fixture: Value = serde_json::from_str(text).unwrap();
        assert_eq!(fixture["contract"], contract);
        assert_eq!(fixture["cases"].as_array().unwrap().len(), case_count);
        let lexical_indices: &[usize] = match contract {
            "native-graphql-parser-budget-precedence-v2" => &[117, 118, 119],
            "native-graphql-parser-diagnostic-ownership-v1" => &[22, 23],
            "native-graphql-unsupported-scalar-precedence-v1" => &[22, 23, 29],
            "native-graphql-parser-boundary-ownership-v1" => &[19, 22, 27],
            "native-graphql-parser-required-value-precedence-v1" => &[40, 43],
            _ => panic!("unknown lexical composition lane"),
        };
        let lexical_rows: Vec<_> = lexical_replacements
            .iter()
            .filter(|r| r["fixture_contract"] == contract)
            .collect();
        assert_eq!(lexical_rows.len(), lexical_indices.len());
        for (replacement, index) in lexical_rows.iter().zip(lexical_indices) {
            let original = &fixture["cases"][*index];
            assert_eq!(replacement["pointer"], format!("/cases/{index}"));
            assert_eq!(replacement["name"], original["name"]);
            assert_eq!(replacement["preimage"], *original);
            assert_eq!(original["expectation"], "baseline");
            assert_ne!(original["reference"], original["baseline"]["legacy"]);
        }
        let field_token_indices: &[usize] = match contract {
            "native-graphql-parser-boundary-ownership-v1" => &[36, 37, 40, 56, 57],
            "native-graphql-parser-required-value-precedence-v1" => &[46],
            _ => &[],
        };
        let field_token_rows: Vec<_> = field_token_replacements
            .iter()
            .filter(|r| r["fixture_contract"] == contract)
            .collect();
        assert_eq!(field_token_rows.len(), field_token_indices.len());
        for (replacement, index) in field_token_rows.iter().zip(field_token_indices) {
            let original = &fixture["cases"][*index];
            assert_eq!(replacement["pointer"], format!("/cases/{index}"));
            assert_eq!(replacement["name"], original["name"]);
            assert_eq!(replacement["preimage"], *original);
            assert_eq!(original["expectation"], "baseline");
            assert_ne!(original["reference"], original["baseline"]["legacy"]);
        }
        let directive_closer_indices: &[usize] = match contract {
            "native-graphql-parser-boundary-ownership-v1" => &[12, 13, 14, 53],
            "native-graphql-parser-required-value-precedence-v1" => &[35, 36, 37],
            _ => &[],
        };
        let directive_closer_rows: Vec<_> = directive_closer_replacements
            .iter()
            .filter(|r| r["fixture_contract"] == contract)
            .collect();
        assert_eq!(directive_closer_rows.len(), directive_closer_indices.len());
        for (replacement, index) in directive_closer_rows.iter().zip(directive_closer_indices) {
            let original = &fixture["cases"][*index];
            assert_eq!(replacement["pointer"], format!("/cases/{index}"));
            assert_eq!(replacement["name"], original["name"]);
            assert_eq!(replacement["preimage"], *original);
            assert_eq!(original["expectation"], "baseline");
            assert_ne!(original["reference"], original["baseline"]["legacy"]);
        }
        let executable_indices: &[usize] = match contract {
            "native-graphql-parser-diagnostic-ownership-v1" => &[21],
            "native-graphql-unsupported-scalar-precedence-v1" => &[25],
            "native-graphql-parser-boundary-ownership-v1" => &[18],
            _ => &[],
        };
        let executable_rows: Vec<_> = executable_replacements
            .iter()
            .filter(|r| r["fixture_contract"] == contract)
            .collect();
        assert_eq!(executable_rows.len(), executable_indices.len());
        for (replacement, index) in executable_rows.iter().zip(executable_indices) {
            let original = &fixture["cases"][*index];
            assert_eq!(replacement["pointer"], format!("/cases/{index}"));
            assert_eq!(replacement["name"], original["name"]);
            assert_eq!(replacement["preimage"], *original);
            assert_eq!(original["expectation"], "baseline");
            assert_ne!(original["reference"], original["baseline"]["legacy"]);
        }
        if !replacement_key.is_empty() {
            assert_eq!(
                fixture[replacement_key].as_array().unwrap().len(),
                promoted_count
            );
        } else {
            assert_eq!(promoted_count, 0);
            assert!(fixture.get("ownership_replacements").is_none());
            assert!(fixture.get("historical_replacements").is_none());
        }
        for query_only in [0, 1] {
            owner
                .store()
                .unwrap()
                .db
                .pragma_update(None, "query_only", query_only)
                .unwrap();
            let mut checked = 0;
            let mut nul_documents = 0;
            for case in fixture["cases"].as_array().unwrap() {
                if !case["args"].as_array().unwrap().is_empty() || case["preflight"] == false {
                    continue;
                }
                let replacement = scalar["stage01_replacements"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|replacement| {
                        contract == "native-graphql-parser-budget-precedence-v2"
                            && replacement["historical_control"] == case["name"]
                    })
                    .or_else(|| {
                        replacements.iter().find(|replacement| {
                            contract == "native-graphql-parser-diagnostic-ownership-v1"
                                && replacement["historical_control"] == case["name"]
                        })
                    });
                if let Some(replacement) = replacement {
                    for (original, promoted) in [
                        ("document", "document"),
                        ("args", "args"),
                        ("reference", "expected"),
                        ("baseline", "baseline"),
                    ] {
                        assert_eq!(case[original], replacement[promoted]);
                    }
                }
                let lexical_promotion = lexical_rows.iter().any(|r| r["name"] == case["name"]);
                let field_token_promotion =
                    field_token_rows.iter().any(|r| r["name"] == case["name"]);
                let directive_closer_promotion = directive_closer_rows
                    .iter()
                    .any(|r| r["name"] == case["name"]);
                let grammar_promotion = grammar["rows"].as_array().unwrap().iter().any(|r| {
                    r["fixture_contract"] == contract
                        && r["name"] == case["name"]
                        && r["promotion"] == true
                });
                let executable_promotion =
                    executable_rows.iter().any(|r| r["name"] == case["name"]);
                let expected = if case["expectation"] == "oracle"
                    || replacement.is_some()
                    || lexical_promotion
                    || field_token_promotion
                    || directive_closer_promotion
                    || grammar_promotion
                    || executable_promotion
                {
                    &case["reference"]["stdout"]["data"]
                } else {
                    assert_eq!(case["expectation"], "baseline");
                    &case["baseline"]["legacy"]["stdout"]["data"]
                };
                // Full-process replay owns admitted neighboring documents,
                // including valid numbers rejected later by resolver policy.
                if !expected["errors"].is_array() || !expected["data"].is_null() {
                    continue;
                }
                checked += 1;
                let document = case["document"].as_str().unwrap();
                nul_documents += usize::from(document.contains('\0'));
                // NUL cannot travel through OS argv. This exercises its exact
                // captured document through direct preflight and the owner API.
                // The fixture deliberately contains no companion argv result.
                for http in [false, true] {
                    assert_eq!(
                        graphql::validate_before_context(document, &json!({}), 5000, http).unwrap(),
                        Some(expected.clone()),
                        "{contract} {} http={http}",
                        case["name"],
                    );
                }
                assert_eq!(
                    embedding::with_interrupt_check(
                        || true,
                        || owner.query_call("default", "graphql", &json!({"document": document})),
                    )
                    .unwrap(),
                    *expected,
                    "{contract} {} query_only={query_only}",
                    case["name"],
                );
                assert!(owner.in_transaction().unwrap());
                let db = &owner.store().unwrap().db;
                assert_eq!(
                    db.query_row("SELECT value FROM scoped_diagnostic_marker", [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    83
                );
                assert_eq!(
                    db.pragma_query_value(None, "query_only", |r| r.get::<_, i64>(0))
                        .unwrap(),
                    query_only
                );
                assert_eq!(db.total_changes(), before_changes);
                assert_eq!(temp_scope(db), before_scope);
            }
            assert_eq!(checked, preflight_count, "{contract} error inventory");
            assert_eq!(
                nul_documents,
                usize::from(contract == "native-graphql-unsupported-scalar-precedence-v1"),
                "{contract} direct NUL coverage",
            );
        }
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
        "default",
    );
}
