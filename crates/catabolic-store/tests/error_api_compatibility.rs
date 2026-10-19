// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! External-crate compatibility of the historical tuple error and additive APIs.
use catabolic_store::{DetailedError, Error, ErrorClass, Result, Store, query::CatalogQuery};
use serde_json::{Value, json};

#[test]
fn ordinary_error_preserves_constructor_pattern_type_field_and_traits() {
    let constructor: fn(String) -> Error = Error;
    let mut error = constructor("original".into());
    assert_eq!(std::any::type_name::<Error>(), "catabolic_store::Error");
    assert_eq!(error.0, "original");
    error.0 = "changed".into();
    assert_eq!(error.to_string(), "changed");
    assert_eq!(format!("{error:?}"), "Error(\"changed\")");
    assert!(std::error::Error::source(&error).is_none());
    let catabolic_store::Error(message) = error;
    assert_eq!(message, "changed");
    let result: Result<u8> = Err(Error(message));
    let same: std::result::Result<u8, Error> = result;
    assert!(matches!(same, Err(Error(message)) if message == "changed"));
}

#[test]
fn existing_conversions_and_explicit_flattening_never_infer_provenance() {
    let text = "cannot use 'dict' as a dict key (unhashable type: 'dict')";
    let json_error = serde_json::from_str::<Value>("{").unwrap_err();
    let json_text = json_error.to_string();
    let errors = [
        (Error(text.into()), text.to_owned()),
        (catabolic_core::Error(text.into()).into(), text.to_owned()),
        (std::io::Error::other(text).into(), text.to_owned()),
        (
            rusqlite::Error::InvalidQuery.into(),
            rusqlite::Error::InvalidQuery.to_string(),
        ),
        (json_error.into(), json_text),
    ];
    for (error, expected) in errors {
        let detailed = DetailedError::from(error);
        assert_eq!(detailed.class(), ErrorClass::CatabolicError);
        assert!(detailed.mapping_subject_json().is_none());
        assert_eq!(detailed.error().0, expected);
        let flattened: Error = detailed.into();
        assert_eq!(flattened.to_string(), expected);
    }
    for detailed in [
        DetailedError::from(catabolic_core::Error(text.into())),
        DetailedError::from(std::io::Error::other(text)),
        DetailedError::from(rusqlite::Error::InvalidQuery),
        DetailedError::from(serde_json::from_str::<Value>("{").unwrap_err()),
    ] {
        assert_eq!(detailed.class(), ErrorClass::CatabolicError);
        assert!(detailed.mapping_subject_json().is_none());
    }
}

#[test]
fn query_literal_legacy_signatures_and_detailed_equivalence_keep_one_owner() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("compat.db");
    catabolic_store::migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store.db.execute_batch("BEGIN; CREATE TEMP TABLE compatibility_owner(value); INSERT INTO compatibility_owner VALUES(73); PRAGMA query_only=ON").unwrap();
    let query = CatalogQuery {
        store: &store,
        profile: "default",
    };
    let changes = store.db.total_changes();
    for args in [
        json!({}),
        json!({"sort":{}}),
        json!({"limit":0,"sort":{}}),
        json!({"kind":7,"sort":{}}),
    ] {
        let legacy: Result<Value> = query.execute("items", &args);
        let detailed = query.execute_detailed("items", &args);
        assert_eq!(
            legacy.map_err(|e| e.to_string()),
            detailed.map_err(|e| e.to_string())
        );
        assert!(!store.db.is_autocommit());
        assert_eq!(store.db.total_changes(), changes);
    }
    let legacy: Result<Value> = query.page(
        "items",
        "id",
        "items",
        &[],
        vec![],
        &[("id", "id")],
        &json!({}),
        json!({}),
    );
    let detailed = query.page_detailed(
        "items",
        "id",
        "items",
        &[],
        vec![],
        &[("id", "id")],
        &json!({}),
        json!({}),
    );
    assert_eq!(legacy.unwrap(), detailed.unwrap());
    let detailed = query
        .execute_detailed("items", &json!({"sort":{}}))
        .unwrap_err();
    assert_eq!(detailed.class(), ErrorClass::TypeError);
    assert!(detailed.mapping_subject_json().is_none());
    let flattened = detailed.into_error();
    assert_eq!(
        flattened.0,
        "cannot use 'dict' as a dict key (unhashable type: 'dict')"
    );
    assert_eq!(
        DetailedError::from(flattened).class(),
        ErrorClass::CatabolicError
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT value FROM compatibility_owner", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        73
    );
    store.db.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn mapping_legacy_signature_and_detailed_raw_redaction_are_additive() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("mapping.db");
    catabolic_store::migration::initialize(&path).unwrap();
    let raw = "{\"private-raw-marker\":";
    let store = Store::open(&path, true, false).unwrap();
    store.db.execute("INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES ('event','default','mapping_publish','info',?)", [raw]).unwrap();
    drop(store);
    let legacy: Result<(Value, Option<String>)> =
        catabolic_store::mapping_history::events_with_subject(
            Store::open(&path, false, false).unwrap(),
            "default",
            Some("event"),
        );
    let detailed = catabolic_store::mapping_history::events_with_subject_detailed(
        Store::open(&path, false, false).unwrap(),
        "default",
        Some("event"),
    )
    .unwrap_err();
    assert_eq!(legacy.unwrap_err().to_string(), detailed.to_string());
    assert_eq!(detailed.mapping_subject_json(), Some(raw));
    assert_eq!(detailed.class(), ErrorClass::CatabolicError);
    assert!(!detailed.to_string().contains("private-raw-marker"));
    assert!(!format!("{detailed:?}").contains("private-raw-marker"));
    let mut source = std::error::Error::source(&detailed);
    while let Some(error) = source {
        assert!(!error.to_string().contains("private-raw-marker"));
        assert!(!format!("{error:?}").contains("private-raw-marker"));
        source = error.source();
    }
    assert_eq!(detailed.into_error().0, "invalid JSON");
}
