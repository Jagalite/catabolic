// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded replacement for the historical CatalogQuery scalar-sort witnesses.
use catabolic_store::{Error, ErrorClass, Store, migration, query::CatalogQuery};
use serde_json::{Value, json};

#[test]
fn versioned_sort_contract_preserves_same_owner_and_typed_unhashable_failures() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sort.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store.db.execute_batch("BEGIN; CREATE TEMP TABLE sort_owner(value); INSERT INTO sort_owner VALUES(71); PRAGMA query_only=ON").unwrap();
    let query = CatalogQuery::new(&store, "default").unwrap();
    let changes = store.db.total_changes();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/parity/catalog-query-sort-diagnostics-v3.json"
    ))
    .unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 88);
    for case in cases {
        let expected = &case["expected_companion"];
        let result = query.execute_detailed(case["operation"].as_str().unwrap(), &case["args"]);
        if expected["code"] == 0 {
            let value: Value = serde_json::from_str(expected["stdout"].as_str().unwrap()).unwrap();
            assert_eq!(result.unwrap(), value, "{}", case["name"]);
        } else {
            let error = result.unwrap_err();
            let owner: Value =
                serde_json::from_str(case["expected_native_owner"]["stdout"].as_str().unwrap())
                    .unwrap();
            assert_eq!(error.class().as_str(), owner["type"], "{}", case["name"]);
            assert_eq!(
                format!("catabolic: {error}\n"),
                expected["stderr"].as_str().unwrap(),
                "{}",
                case["name"]
            );
        }
        assert!(!store.db.is_autocommit());
        assert!(
            store
                .db
                .pragma_query_value::<bool, _>(None, "query_only", |r| r.get(0))
                .unwrap()
        );
        assert_eq!(store.db.total_changes(), changes);
        assert_eq!(
            store
                .db
                .query_row("SELECT value FROM sort_owner", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            71
        );
    }
    // Legacy callers erase the class while retaining the exact list diagnostic.
    for sort in [json!([]), json!(["id"])] {
        assert_eq!(
            query
                .execute("items", &json!({"sort":sort}))
                .unwrap_err()
                .to_string(),
            "cannot use 'list' as a dict key (unhashable type: 'list')"
        );
    }
    assert!(query.execute("items", &json!({"sort":"id"})).is_ok());
    store.db.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn scalar_sort_diagnostic_uses_owner_available_sort_order() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sort.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, false, false).unwrap();
    let query = CatalogQuery::new(&store, "default").unwrap();
    for sort in [json!(5), json!(1.5), json!(true), json!(false), Value::Null] {
        assert_eq!(
            query
                .page(
                    "items",
                    "id",
                    "items",
                    &[],
                    vec![],
                    &[("title", "id"), ("id", "id")],
                    &json!({"sort":sort}),
                    json!({}),
                )
                .unwrap_err()
                .to_string(),
            "sort must be one of: title, id"
        );
    }
}

#[test]
fn exact_diagnostic_text_cannot_spoof_typed_provenance() {
    let message = "cannot use 'dict' as a dict key (unhashable type: 'dict')";
    for error in [
        Error(message.into()),
        Error::from(catabolic_core::Error(message.into())),
        Error::from(std::io::Error::other(message)),
        Error::from(rusqlite::Error::InvalidParameterName(message.into())),
    ] {
        assert_eq!(
            catabolic_store::DetailedError::from(error).class(),
            ErrorClass::CatabolicError
        );
    }
    let constructor: fn(String) -> Error = Error;
    let error = constructor(message.into());
    assert_eq!(error.to_string(), message);
    assert_eq!(error.0, message);
    assert_eq!(format!("{error:?}"), format!("Error({message:?})"));
    assert_eq!(
        catabolic_store::DetailedError::from(error).class().as_str(),
        "CatabolicError"
    );
}

#[test]
fn list_provenance_is_local_to_detailed_page_after_limit_validation() {
    use catabolic_store::DetailedError;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sort-page.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, false, false).unwrap();
    let query = CatalogQuery::new(&store, "default").unwrap();
    let message = "cannot use 'list' as a dict key (unhashable type: 'list')";
    for sort in [
        json!([]),
        json!(["id"]),
        json!([[{"nested":[]}]]),
        json!([null, false, 5]),
    ] {
        let failure = query
            .page_detailed(
                "items",
                "id",
                "items",
                &[],
                vec![],
                &[("id", "id")],
                &json!({"sort":sort}),
                json!({}),
            )
            .unwrap_err();
        assert_eq!(failure.class(), ErrorClass::TypeError);
        assert_eq!(failure.to_string(), message);
        let Error(erased) = failure.into_error();
        assert_eq!(erased, message);
        let failure = query
            .page_detailed(
                "items",
                "id",
                "items",
                &[],
                vec![],
                &[("id", "id")],
                &json!({"sort":sort,"limit":0}),
                json!({}),
            )
            .unwrap_err();
        assert_eq!(failure.class(), ErrorClass::CatabolicError);
        assert_eq!(failure.to_string(), "limit must be between 1 and 1000");
    }
    for ordinary in [
        Error(message.into()),
        Error::from(catabolic_core::Error(message.into())),
        Error::from(std::io::Error::other(message)),
        Error::from(rusqlite::Error::InvalidParameterName(message.into())),
    ] {
        assert_eq!(
            DetailedError::from(ordinary).class(),
            ErrorClass::CatabolicError
        );
    }
    let constructor: fn(String) -> Error = Error;
    let error = constructor(message.into());
    assert_eq!(std::any::type_name::<Error>(), "catabolic_store::Error");
    assert_eq!(error.0, message);
    assert_eq!(format!("{error:?}"), format!("Error({message:?})"));
    assert_eq!(error.to_string(), message);
    let Error(destructured) = error;
    assert_eq!(destructured, message);
}
