// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen parser witnesses, complete preflight responses, and owner preservation.
use catabolic_store::{embedding, graphql, migration};
use serde_json::{Value, json};

#[test]
fn frozen_parser_cases_preserve_execution_preflight_and_the_owner() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-cases.json"
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
            "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE parser_marker(value); INSERT INTO parser_marker VALUES(47)",
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
                    db.query_row("SELECT value FROM parser_marker", [], |row| row
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
fn malformed_parser_controls_remain_explicit_non_parity_evidence() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-parser-cases.json"
    ))
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("controls.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE parser_control_marker(value); INSERT INTO parser_control_marker VALUES(53)",
    ).unwrap();
    for case in fixture["remaining_parser_controls"].as_array().unwrap() {
        let document = case["document"].as_str().unwrap();
        let native = &case["native"]["stdout"]["data"];
        assert_ne!(
            native, &case["expected"]["data"],
            "{} is not a remaining gap",
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
            "{}; malformed execution control",
            case["name"],
        );
        for http in [false, true] {
            assert_eq!(
                graphql::validate_before_context(document, &case["variables"], 5000, http).unwrap(),
                Some(native.clone()),
                "{}; malformed preflight http={http}",
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
                "{}; malformed persistent query_only={query_only}",
                case["name"],
            );
            assert!(owner.in_transaction().unwrap());
            let db = &owner.store().unwrap().db;
            assert_eq!(
                db.query_row("SELECT value FROM parser_control_marker", [], |row| row
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
fn transformed_queries_keep_original_budgets_and_cancelled_owner() {
    let too_large = format!(r#"{{item(id:"{}"){{id}}}}"#, "\\uD83D\\uDE00".repeat(11000));
    assert!(
        graphql::validate_before_context(&too_large, &json!({}), 5000, false)
            .unwrap_err()
            .to_string()
            .contains("query document must be at most 131072 bytes")
    );
    let tokens = format!(
        r#"{{item(id:"\uD83D\uDE00"){{id}} {}}}"#,
        "profile ".repeat(4000)
    );
    assert!(
        graphql::validate_before_context(&tokens, &json!({}), 5000, false)
            .unwrap()
            .unwrap()["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Document contains more than 4000 tokens")
    );
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cancel.db");
    migration::initialize(&path).unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch(
        "PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE cancel_marker(value); INSERT INTO cancel_marker VALUES(53)",
    ).unwrap();
    for args in [
        json!({"document":r#"{item(id:"\uD83D\uDE00"){id}}"#}),
        json!({"document":"query($tags:[ String ! ] !){items(tags:$tags){nodes{id}}}","variables":{"tags":[]}}),
        json!({"document":"query($tags:[ String ! ] ! = []){items(tags:$tags){nodes{id}}}"}),
    ] {
        let result = embedding::with_interrupt_check(
            || true,
            || owner.query_call("default", "graphql", &args),
        )
        .unwrap();
        assert!(
            result["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|error| error["message"] == "query execution interrupted"),
            "{result}"
        );
        assert!(owner.in_transaction().unwrap());
        assert_eq!(
            owner
                .store()
                .unwrap()
                .db
                .query_row("SELECT value FROM cancel_marker", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            53
        );
        assert!(
            !owner
                .store()
                .unwrap()
                .db
                .pragma_query_value::<bool, _>(None, "query_only", |row| row.get(0))
                .unwrap()
        );
    }
    // Static preflight errors retain precedence even on an already-cancelled
    // owner. The grammar adapter must not turn these into execution errors.
    for args in [
        json!({"document":"query($n:Int=1 @skip(if:true)){items(first:$n){nodes{id}}}"}),
        json!({"document":"query($n:Int=$bad){items(first:$n){nodes{id}}}"}),
    ] {
        let expected = owner.query_call("default", "graphql", &args).unwrap();
        let observed = embedding::with_interrupt_check(
            || true,
            || owner.query_call("default", "graphql", &args),
        )
        .unwrap();
        assert_eq!(observed, expected);
        assert!(owner.in_transaction().unwrap());
        assert_eq!(
            owner
                .store()
                .unwrap()
                .db
                .query_row("SELECT value FROM cancel_marker", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            53
        );
        assert!(
            !owner
                .store()
                .unwrap()
                .db
                .pragma_query_value::<bool, _>(None, "query_only", |row| row.get(0))
                .unwrap()
        );
    }
    assert_eq!(
        owner
            .query_call("default", "graphql", &json!({"document":"{profile}"}))
            .unwrap()["data"]["profile"],
        "default"
    );
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn selection_contract_admits_wrapped_types_and_surrogate_pairs() {
    let document = r#"query($after:String,$tags:[ String ! ] !){items(after:$after,tags:$tags,search:"\uD83D\uDE00"){nodes{id}pageInfo{hasNextPage endCursor}}}"#;
    assert_eq!(
        catabolic_store::selection::graphql_contract(document).unwrap(),
        ("item_id".to_owned(), "items".to_owned()),
    );
}

#[test]
fn variable_boundary_adapter_preserves_original_preflight_limits() {
    let deep = format!(
        "query($n:Int=1 @skip(if:true)){{{}profile{}}}",
        "item{".repeat(21),
        "}".repeat(21)
    );
    assert_eq!(
        graphql::validate_before_context(&deep, &json!({}), 5000, false)
            .unwrap()
            .unwrap()["errors"][0]["message"],
        "query depth exceeds 20"
    );
    let too_many = format!(
        "query($n:Int=1 @skip(if:true)){{{}}}",
        "profile ".repeat(4000)
    );
    assert!(
        graphql::validate_before_context(&too_many, &json!({}), 5000, false)
            .unwrap()
            .unwrap()["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Document contains more than 4000 tokens")
    );
    let too_large = format!(
        "query($n:Int=1 @skip(if:true)){{profile}} #{}",
        "x".repeat(131072)
    );
    assert!(
        graphql::validate_before_context(&too_large, &json!({}), 5000, false)
            .unwrap_err()
            .to_string()
            .contains("query document must be at most 131072 bytes")
    );
    assert!(
        graphql::validate_before_context(
            "query($n:Int=1 @skip(if:true)){profile}",
            &json!({}),
            0,
            false
        )
        .unwrap_err()
        .to_string()
        .contains("timeout-ms must be between 1 and 60000")
    );
}
