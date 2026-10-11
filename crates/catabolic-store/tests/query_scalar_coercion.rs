// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Coercion regressions discovered by the native inbox transport differential.
use catabolic_store::{Store, graphql, migration};
use serde_json::{Value, json};

fn fixture() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("coercion.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    // Establish connection-local views before adding the identity marker.
    catabolic_store::sql::setup(&store.db).unwrap();
    store.db.execute_batch("PRAGMA query_only=OFF; CREATE TEMP TABLE coercion_identity(value); INSERT INTO coercion_identity VALUES(19); PRAGMA query_only=ON;").unwrap();
    (root, store)
}

#[test]
fn rejects_wide_integer_before_any_resolver_and_keeps_connection() {
    let (_root, mut store) = fixture();
    for value in [
        json!(2147483648i64),
        json!(-2147483649i64),
        json!(u64::MAX),
        catabolic_core::decode_json("1237940039285380274899124224").unwrap(),
    ] {
        let (returned, report) = graphql::execute_store(
            store,
            "query($n:Int!){items(first:$n){nodes{id}}}",
            "default",
            json!({"n":value}),
            None,
            5000,
        )
        .unwrap();
        store = returned;
        assert_eq!(report["data"], Value::Null);
        assert_eq!(report["extensions"]["records"], 0);
        assert!(
            report["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("non 32-bit signed integer")
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT value FROM coercion_identity", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            19
        );
        assert!(
            store
                .db
                .pragma_query_value::<bool, _>(None, "query_only", |r| r.get(0))
                .unwrap()
        );
    }
}

#[test]
fn integral_float_int_and_id_keep_frozen_coercion() {
    let (_root, store) = fixture();
    let (store, report) = graphql::execute_store(
        store,
        "query($n:Int!){items(first:$n){nodes{id}}}",
        "default",
        json!({"n":1.0}),
        None,
        5000,
    )
    .unwrap();
    assert!(report.get("errors").is_none(), "{report}");
    let (_, report) = graphql::execute_store(
        store,
        "query($id:ID!){item(id:$id){id}}",
        "default",
        json!({"id":1.0}),
        None,
        5000,
    )
    .unwrap();
    assert!(report.get("errors").is_none(), "{report}");
}

#[test]
fn only_selected_operation_coerces_and_selection_errors_keep_precedence() {
    let (_root, mut store) = fixture();
    let document = "query A($n:Int!){items(first:$n){nodes{id}}} query B{profile}";
    for (selected, message) in [
        (Some("B"), None),
        (Some("Missing"), Some("Unknown operation")),
        (None, Some("Must provide operation name")),
    ] {
        let (returned, report) = graphql::execute_store(
            store,
            document,
            "default",
            json!({"n":2147483648i64}),
            selected,
            5000,
        )
        .unwrap();
        store = returned;
        if let Some(message) = message {
            assert!(
                report["errors"][0]["message"]
                    .as_str()
                    .unwrap()
                    .contains(message),
                "{report}"
            );
        } else {
            assert_eq!(report["data"]["profile"], "default");
            assert!(report.get("errors").is_none());
        }
    }
}

#[test]
fn composite_inputs_keep_same_store_before_resolver_and_input_order() {
    let (_root, mut store) = fixture();
    let document = "query($n:Int!,$tags:[String!],$f:RenditionFilter){items(first:$n,tags:$tags,hasRendition:$f){nodes{id}}}";
    let variables = graphql::InputVariables::from_json(
        r#"{"n":2147483648,"tags":[1,null],"f":{"zzz":1,"aaa":2,"current":"yes"}}"#,
    )
    .unwrap();
    for _ in 0..2 {
        let (returned, report) =
            graphql::execute_store(store, document, "default", variables.clone(), None, 5000)
                .unwrap();
        store = returned;
        assert_eq!(report["errors"].as_array().unwrap().len(), 6);
        assert_eq!(report["extensions"]["records"], 0);
        assert!(
            report["errors"][3]["message"]
                .as_str()
                .unwrap()
                .contains("f.current")
        );
        assert!(
            report["errors"][4]["message"]
                .as_str()
                .unwrap()
                .contains("Field 'zzz'")
        );
        assert!(
            report["errors"][5]["message"]
                .as_str()
                .unwrap()
                .contains("Field 'aaa'")
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT value FROM coercion_identity", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            19
        );
        assert!(
            store
                .db
                .pragma_query_value::<bool, _>(None, "query_only", |r| r.get(0))
                .unwrap()
        );
    }
}

#[test]
fn persistent_json_call_keeps_raw_graphql_order_and_transaction() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("session.db");
    migration::initialize(&path).unwrap();
    let mut session = catabolic_store::embedding::Session::open(&path, true, false).unwrap();
    session.store().unwrap().db.execute_batch("BEGIN; CREATE TEMP TABLE graphql_identity(value); INSERT INTO graphql_identity VALUES(31)").unwrap();
    let report = session.query_call_json("default", "graphql", r#"{"document":"query($f:RenditionFilter){items(hasRendition:$f){nodes{id}}}","variables":{"f":{"zzz":1,"aaa":2}}}"#).unwrap();
    assert!(
        report["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Field 'zzz'")
    );
    assert!(
        report["errors"][1]["message"]
            .as_str()
            .unwrap()
            .contains("Field 'aaa'")
    );
    assert!(!session.store().unwrap().db.is_autocommit());
    assert_eq!(
        session
            .store()
            .unwrap()
            .db
            .query_row("SELECT value FROM graphql_identity", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        31
    );
    session
        .store()
        .unwrap()
        .db
        .execute_batch("ROLLBACK")
        .unwrap();
}
