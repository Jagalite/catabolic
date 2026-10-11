// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent raw input and session-ownership guards. These do not qualify all
//! parser rules or replace paired frozen GraphQL response tests.
use catabolic_store::{Store, embedding, graphql, migration};
use serde_json::{Value, json};

fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("independent.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    (dir, store)
}

#[test]
fn envelope_duplicate_replacement_retains_latest_nested_order() {
    let (_dir, store) = fixture();
    let input = graphql::InputVariables::from_request_json(
        r#"{"variables":{"f":{"old":0}},"other":true,"variables":{"f":{"z\u007az":1,"aaa":2,"zzz":3,"current":"bad"}}}"#,
        "variables",
    )
    .unwrap();
    let (_, result) = graphql::execute_store(
        store,
        "query($f:RenditionFilter){items(hasRendition:$f){nodes{id}}}",
        "default",
        input,
        None,
        5000,
    )
    .unwrap();
    let errors = result["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 3);
    assert!(errors[0]["message"].as_str().unwrap().contains("f.current"));
    assert!(
        errors[1]["message"]
            .as_str()
            .unwrap()
            .contains("{'zzz': 3, 'aaa': 2, 'current': 'bad'}")
    );
    assert!(
        errors[1]["message"]
            .as_str()
            .unwrap()
            .contains("Field 'zzz'")
    );
    assert!(
        errors[2]["message"]
            .as_str()
            .unwrap()
            .contains("Field 'aaa'")
    );
    assert_eq!(result["extensions"]["records"], 0);
}

#[test]
fn literal_number_sentinel_objects_never_become_id_numbers() {
    let (_dir, mut store) = fixture();
    for raw in [
        r#"{"id":{"$serde_json::private::Number":"123"}}"#,
        r#"{"id":{"$serde_json::private::Number":"123","$serde_json::private::Number":"456"}}"#,
        r#"{"id":{"$serde_json::private::Number":"1e400"}}"#,
        r#"{"id":{"$serde_json::private::Number":123}}"#,
    ] {
        let (returned, result) = graphql::execute_store(
            store,
            "query($id:ID){item(id:$id){id}}",
            "default",
            graphql::InputVariables::from_json(raw).unwrap(),
            None,
            5000,
        )
        .unwrap();
        store = returned;
        assert!(
            result["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("ID cannot represent value: {"),
            "{result}"
        );
        assert_eq!(result["extensions"]["records"], 0);
    }
}

#[test]
fn huge_id_defaults_and_fragment_literals_reach_exact_rows() {
    let (_dir, mut store) = fixture();
    let positive = format!("{}7", "9".repeat(399));
    let negative = format!("-{positive}");
    for id in [&positive, &negative] {
        store
            .db
            .execute(
                "INSERT INTO items(id,kind,metadata) VALUES(?,'movie','{}')",
                [id],
            )
            .unwrap();
    }
    for id in [&positive, &negative] {
        for document in [
            format!("query($v:ID={id}){{alias:item(id:$v){{id}}}}"),
            format!("query{{...Find}} fragment Find on Query{{alias:item(id:{id}){{id}}}}"),
            format!("query{{... on Query{{alias:item(id:{id}){{id}}}}}}"),
        ] {
            let (returned, result) =
                graphql::execute_store(store, &document, "default", json!({}), None, 5000).unwrap();
            store = returned;
            assert!(result.get("errors").is_none(), "{result}");
            assert_eq!(result["data"]["alias"]["id"], *id);
        }
    }
}

#[test]
fn early_coercion_and_operation_errors_preserve_cancelled_owner() {
    let (dir, store) = fixture();
    drop(store);
    let mut session =
        embedding::Session::open(&dir.path().join("independent.db"), true, false).unwrap();
    catabolic_store::sql::setup(&session.store().unwrap().db).unwrap();
    session.store().unwrap().db.execute_batch("PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE graphql_review_marker(v); INSERT INTO graphql_review_marker VALUES(71)").unwrap();
    for args in [
        json!({"document":"query($v:ID!){item(id:$v){id}}","variables":{"v":false}}),
        json!({"document":"query A{profile} query B{schemaVersion}"}),
        json!({"document":"query A{profile}","operation_name":"missing"}),
        json!({"document":"query($f:RenditionFilter){items(hasRendition:$f){nodes{id}}}","variables":{"f":{"zzz":1,"aaa":2}}}),
    ] {
        let result = embedding::with_interrupt_check(
            || true,
            || session.query_call_json("default", "graphql", &args.to_string()),
        )
        .unwrap();
        assert_eq!(
            result,
            json!({"data":null,"errors":[{"message":"query execution interrupted"}]})
        );
        let store = session.store().unwrap();
        assert!(!store.db.is_autocommit());
        assert_eq!(
            store
                .db
                .query_row("SELECT v FROM graphql_review_marker", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            71
        );
        assert!(
            !store
                .db
                .pragma_query_value::<bool, _>(None, "query_only", |r| r.get(0))
                .unwrap()
        );
    }
    assert_eq!(
        session
            .query_call_json("default", "graphql", r#"{"document":"{profile}"}"#)
            .unwrap()["data"]["profile"],
        "default"
    );
    session
        .store()
        .unwrap()
        .db
        .execute_batch("ROLLBACK")
        .unwrap();
}

#[test]
fn hardened_decoder_remains_authoritative_and_general_json_stays_canonical() {
    for raw in [
        "",
        " ",
        "{",
        "[",
        "{\"x\":1,}",
        "{\"x\":1e400}",
        "{\"x\":NaN}",
        r#"{"x":"\uD800"}"#,
    ] {
        assert!(graphql::InputVariables::from_json(raw).is_err(), "{raw}");
    }
    let raw = r#"{"zzz":1,"aaa":{"zzz":2,"aaa":3},"zzz":4,"$serde_json::private::Number":"123"}"#;
    let expected =
        r#"{"$serde_json::private::Number": "123", "aaa": {"aaa": 3, "zzz": 2}, "zzz": 4}"#;
    let before = catabolic_core::decode_json(raw).unwrap();
    let _ = graphql::InputVariables::from_json(raw).unwrap();
    assert_eq!(catabolic_core::encode(&before), expected);
    assert_eq!(
        catabolic_core::encode(&catabolic_core::decode_json(raw).unwrap()),
        expected
    );
    assert_eq!(
        serde_json::from_str::<Value>(r#"{"zzz":1,"aaa":2}"#)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["aaa", "zzz"]
    );
}

#[test]
fn runtime_null_cancellation_preserves_same_transaction() {
    let (dir, store) = fixture();
    drop(store);
    let mut session =
        embedding::Session::open(&dir.path().join("independent.db"), true, false).unwrap();
    catabolic_store::sql::setup(&session.store().unwrap().db).unwrap();
    session.store().unwrap().db.execute_batch("PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE runtime_marker(v); INSERT INTO runtime_marker VALUES(91)").unwrap();
    for args in [
        json!({"document":"query($n:Int){items(first:$n){nodes{id}}}","variables":{"n":null}}),
        json!({"document":"query($b:Boolean=false){profile @skip(if:$b)}","variables":{"b":null}}),
    ] {
        let result = embedding::with_interrupt_check(
            || true,
            || session.query_call_json("default", "graphql", &args.to_string()),
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
        assert!(!session.store().unwrap().db.is_autocommit());
        assert_eq!(
            session
                .store()
                .unwrap()
                .db
                .query_row("SELECT v FROM runtime_marker", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            91
        );
        assert!(
            !session
                .store()
                .unwrap()
                .db
                .pragma_query_value::<bool, _>(None, "query_only", |r| r.get(0))
                .unwrap()
        );
    }
    assert_eq!(
        session
            .query_call_json("default", "graphql", r#"{"document":"{profile}"}"#)
            .unwrap()["data"]["profile"],
        "default"
    );
    session
        .store()
        .unwrap()
        .db
        .execute_batch("ROLLBACK")
        .unwrap();
}
