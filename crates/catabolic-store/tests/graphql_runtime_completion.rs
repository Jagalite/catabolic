// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Native grouped execution, frozen response contracts and borrowed-owner guards.
use catabolic_store::{Store, embedding, graphql, migration};
use serde_json::{Value, json};

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("runtime.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    for (name, status) in [
        ("a", "pending"),
        ("b", "deferred"),
        ("c", "complete"),
        ("雪", "in_progress"),
    ] {
        store
            .db
            .execute(
                "INSERT INTO items(id,kind,metadata) VALUES(?,'movie',?)",
                [
                    name,
                    &json!({"title":name,"opaque":2_i64.pow(61)+1}).to_string(),
                ],
            )
            .unwrap();
        store.db.execute("INSERT INTO item_workflow(item_id,status,revision,updated_at,updated_by) VALUES(?,?,1,'2026-01-01 00:00:00','fixture')",[name,status]).unwrap();
    }
    (root, path)
}
fn cases() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-runtime-completion.json"
    ))
    .unwrap()
}
fn same_owner(owner: &embedding::Session) {
    assert!(owner.in_transaction().unwrap());
    let store = owner.store().unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT value FROM runtime_marker", [], |r| r
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
#[test]
fn frozen_grouped_completion_preserves_direct_and_persistent_owners() {
    let (_root, path) = fixture();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch("PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE runtime_marker(value); INSERT INTO runtime_marker VALUES(71)").unwrap();
    let mut failures = vec![];
    for case in cases()["cases"].as_array().unwrap() {
        let document = case["document"].as_str().unwrap();
        for http in [false, true] {
            assert_eq!(
                graphql::validate_before_context(document, &case["variables"], 5000, http).unwrap(),
                None,
                "{}; http={http}",
                case["name"]
            );
        }
        let direct = graphql::execute(
            &path,
            document,
            "default",
            case["variables"].clone(),
            None,
            5000,
        )
        .unwrap();
        let persistent = owner
            .query_call(
                "default",
                "graphql",
                &json!({"document":document,"variables":case["variables"]}),
            )
            .unwrap();
        for (surface, result) in [("direct", direct), ("persistent", persistent)] {
            if result != case["expected"]["data"] {
                failures.push(format!(
                    "{} / {surface}: expected {}, got {result}",
                    case["name"], case["expected"]["data"]
                ));
            }
        }
        same_owner(&owner);
    }
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
#[test]
fn incompatible_groups_stay_rejected_before_resolver_effects() {
    let (_root, path) = fixture();
    for case in cases()["validation_controls"].as_array().unwrap() {
        let result = graphql::execute(
            &path,
            case["document"].as_str().unwrap(),
            "default",
            json!({}),
            None,
            5000,
        )
        .unwrap();
        assert_eq!(result, case["expected"]["data"], "{}", case["name"]);
        assert!(result["data"].is_null(), "{}: {result}", case["name"]);
        assert!(
            result["errors"]
                .as_array()
                .is_some_and(|errors| !errors.is_empty()),
            "{}: {result}",
            case["name"]
        );
        assert!(
            result.get("extensions").is_none(),
            "{} resolved before rejection: {result}",
            case["name"]
        );
    }
}
#[test]
fn grouping_preserves_record_field_and_output_budgets() {
    let (_root, path) = fixture();
    let store = Store::open(&path, true, false).unwrap();
    store.db.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000) INSERT INTO items(id,kind,metadata) SELECT printf('many-%04d',i),'movie','{}' FROM n;").unwrap();
    drop(store);
    let repeat = "items(first:1000){nodes{id @skip(if:true)}} ".repeat(11);
    let merged = graphql::execute(
        &path,
        &format!("{{{repeat}}}"),
        "default",
        json!({}),
        None,
        5000,
    )
    .unwrap();
    assert!(merged.get("errors").is_none(), "{merged}");
    assert_eq!(merged["extensions"]["records"], 1000);
    for (document, message) in [
        (
            format!(
                "{{{}}}",
                (0..11)
                    .map(|n| format!("p{n}:items(first:1000){{nodes{{id @skip(if:true)}}}} "))
                    .collect::<String>()
            ),
            "query record budget exceeded",
        ),
        (
            format!(
                "{{items(first:1000){{nodes{{{}}}}}}}",
                (0..20).map(|n| format!("f{n}:id ")).collect::<String>()
            ),
            "query field budget exceeded",
        ),
    ] {
        let result = graphql::execute(&path, &document, "default", json!({}), None, 5000).unwrap();
        assert_eq!(
            result,
            json!({"data":null,"errors":[{"message":message,"extensions":{"code":"LIMIT_EXCEEDED"}}]})
        );
    }
    let store = Store::open(&path, true, false).unwrap();
    store
        .db
        .execute(
            "UPDATE items SET metadata=? WHERE id='a'",
            [json!({"title":"x".repeat(4*1024*1024+100)}).to_string()],
        )
        .unwrap();
    drop(store);
    let result = graphql::execute(
        &path,
        "{same:item(id:\"a\"){title} same:item(id:\"a\"){title}}",
        "default",
        json!({}),
        None,
        5000,
    )
    .unwrap();
    assert_eq!(
        result,
        json!({"data":null,"errors":[{"message":"query output budget exceeded","extensions":{"code":"LIMIT_EXCEEDED"}}]})
    );
}
#[test]
fn cancellation_with_merged_and_introspection_inputs_keeps_owner_reusable() {
    let (_root, path) = fixture();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch("PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE runtime_marker(value); INSERT INTO runtime_marker VALUES(71)").unwrap();
    for case in cases()["cases"].as_array().unwrap() {
        let result = embedding::with_interrupt_check(
            || true,
            || {
                owner.query_call(
                    "default",
                    "graphql",
                    &json!({"document":case["document"],"variables":case["variables"]}),
                )
            },
        )
        .unwrap();
        assert_eq!(
            result,
            json!({"data":null,"errors":[{"message":"query execution interrupted"}]}),
            "{}",
            case["name"]
        );
        same_owner(&owner);
    }
    let result = owner
        .query_call(
            "default",
            "graphql",
            &json!({"document":"{profile __type(name:\"Query\"){name}}"}),
        )
        .unwrap();
    assert!(result.get("errors").is_none(), "{result}");
    same_owner(&owner);
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn shared_query_sort_diagnostics_match_frozen_historical_witnesses() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sort.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    let query = catabolic_store::query::CatalogQuery::new(&store, "default").unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-runtime-sort-controls.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let result = query.execute_detailed("items", &case["args"]);
        if let Some(data) = case["expected"].get("data") {
            assert_eq!(result.unwrap(), *data);
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                error.to_string(),
                case["expected"]["error"],
                "{}",
                case["args"]
            );
            assert_eq!(
                error.class().as_str(),
                case["expected"]["type"],
                "{}",
                case["args"]
            );
        }
    }
}

#[test]
fn collection_limits_and_mid_completion_cancellation_keep_owner() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (_root, path) = fixture();
    let deep = format!(
        "{{__type(name:\"Query\"){{{}name{}}}}}",
        "ofType{".repeat(21),
        "}".repeat(21)
    );
    assert_eq!(
        graphql::validate_before_context(&deep, &json!({}), 5000, false)
            .unwrap()
            .unwrap()["errors"][0]["message"],
        "query depth exceeds 20"
    );
    let expanded = format!(
        "{{{}}} fragment F on Query{{profile}}",
        "...F ".repeat(1100)
    );
    assert_eq!(
        graphql::validate_before_context(&expanded, &json!({}), 5000, false)
            .unwrap()
            .unwrap()["errors"][0]["message"],
        "query exceeds 2000 expanded selections"
    );
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch("PRAGMA query_only=OFF; BEGIN; CREATE TEMP TABLE runtime_marker(value); INSERT INTO runtime_marker VALUES(71)").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let callback = calls.clone();
    let result = embedding::with_interrupt_check(move||callback.fetch_add(1,Ordering::SeqCst)>=30,||owner.query_call("default","graphql",&json!({"document":"{same:items{nodes{id title workflow{status revision}}} same:items{nodes{kind}} __schema{types{name kind}}}"}))).unwrap();
    assert!(calls.load(Ordering::SeqCst) > 30);
    assert!(result.get("errors").is_some(), "{result}");
    same_owner(&owner);
    let result = owner
        .query_call(
            "default",
            "graphql",
            &json!({"document":"{item(id:\"a\"){id} __schema{queryType{name}}}"}),
        )
        .unwrap();
    assert!(result.get("errors").is_none(), "{result}");
    same_owner(&owner);
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn arbitrary_recorded_scalar_outputs_match_retained_reference() {
    let (_root, path) = fixture();
    let cases: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/graphql-runtime-scalar-remaining.json"
    ))
    .unwrap();
    for case in cases["cases"].as_array().unwrap() {
        let store = Store::open(&path, true, false).unwrap();
        store
            .db
            .execute("DELETE FROM item_relationships", [])
            .unwrap();
        store.db.execute("INSERT INTO item_relationships(id,source_id,target_id,kind,position) VALUES('r','a','b','related',?)",[case["position_value"].as_str().unwrap()]).unwrap();
        let (_, result) = graphql::execute_store(
            store,
            case["document"].as_str().unwrap(),
            "default",
            json!({}),
            None,
            5000,
        )
        .unwrap();
        assert_eq!(result, case["reference"]["stdout"]["data"]);
        assert_ne!(case["native"], case["reference"]);
        assert_ne!(case["baseline"], case["reference"]);
    }
}

#[test]
fn opaque_json_numbers_remain_exact_and_bounded_on_the_same_owner() {
    let (_root, path) = fixture();
    let cases: Value = catabolic_core::decode_json(include_str!(
        "../../../tests/parity/graphql-runtime-json-completion.json"
    ))
    .unwrap();
    let mut owner = embedding::Session::open(&path, true, false).unwrap();
    catabolic_store::sql::setup(&owner.store().unwrap().db).unwrap();
    owner.store().unwrap().db.execute_batch("PRAGMA query_only=OFF; CREATE TEMP TABLE runtime_marker(value); INSERT INTO runtime_marker VALUES(71)").unwrap();
    for case in cases["cases"].as_array().unwrap() {
        owner
            .store()
            .unwrap()
            .db
            .execute(
                "UPDATE items SET metadata=? WHERE id='a'",
                [case["metadata_json"].as_str().unwrap()],
            )
            .unwrap();
        owner.store().unwrap().db.execute_batch("BEGIN").unwrap();
        let document = case["document"].as_str().unwrap();
        let direct = graphql::execute(&path, document, "default", json!({}), None, 5000).unwrap();
        let args = json!({"document":document});
        let persistent = owner.query_call("default", "graphql", &args).unwrap();
        assert_eq!(
            direct, case["expected"]["stdout"]["data"],
            "{}",
            case["name"]
        );
        assert_eq!(persistent, direct, "{}", case["name"]);
        same_owner(&owner);
        let cancelled = embedding::with_interrupt_check(
            || true,
            || owner.query_call("default", "graphql", &args),
        )
        .unwrap();
        assert!(cancelled.get("errors").is_some(), "{cancelled}");
        same_owner(&owner);
        assert_eq!(
            owner.query_call("default", "graphql", &args).unwrap(),
            direct
        );
        same_owner(&owner);
        owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
    }
    // The JSON scalar remains opaque to completion, but never escapes the
    // existing resolver's byte budget or its borrowed-connection restoration.
    let number: Value = serde_json::from_str(&"9".repeat(1000)).unwrap();
    let large = json!({"wide":vec![number;9000]});
    owner
        .store()
        .unwrap()
        .db
        .execute(
            "UPDATE items SET metadata=? WHERE id='a'",
            [large.to_string()],
        )
        .unwrap();
    owner.store().unwrap().db.execute_batch("BEGIN").unwrap();
    let result = owner
        .query_call(
            "default",
            "graphql",
            &json!({"document":"{item(id:\"a\"){metadata}}"}),
        )
        .unwrap();
    assert_eq!(
        result,
        json!({"data":null,"errors":[{"message":"query output budget exceeded","extensions":{"code":"LIMIT_EXCEEDED"}}]})
    );
    same_owner(&owner);
    assert_eq!(
        owner
            .query_call("default", "graphql", &json!({"document":"{profile}"}))
            .unwrap()["data"]["profile"],
        "default"
    );
    same_owner(&owner);
    owner.store().unwrap().db.execute_batch("ROLLBACK").unwrap();
}
