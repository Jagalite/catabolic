// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Approved retirement decisions do not change legacy Rust error identities.
use catabolic_store::{
    DetailedError, Error, Store, mapping_history, migration, query::CatalogQuery,
};
use serde_json::json;

#[test]
fn mapping_runtime_details_preserve_legacy_errors_and_cannot_be_forged_by_text() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    for (raw, native, kind, message) in [
        (
            "null",
            "mapping history subject must be a JSON object",
            "TypeError",
            "dict() argument after ** must be a mapping, not NoneType",
        ),
        (
            "123456789012345678901234567890",
            "mapping history subject must be a JSON object",
            "TypeError",
            "dict() argument after ** must be a mapping, not int",
        ),
        (
            "1.0",
            "mapping history subject must be a JSON object",
            "TypeError",
            "dict() argument after ** must be a mapping, not float",
        ),
        (
            "{\"id\":\"forged\"}",
            "mapping history subject contains an id",
            "TypeError",
            "dict() got multiple values for keyword argument 'id'",
        ),
        (
            "{}",
            "mapping history event is missing owner",
            "KeyError",
            "'owner'",
        ),
    ] {
        {
            let store = Store::open(&path, true, false).unwrap();
            store.db.execute("DELETE FROM consumer_events", []).unwrap();
            store.db.execute("INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES('event','default','mapping_publish','info',?)", [raw]).unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let error = mapping_history::events_with_subject_detailed(
            Store::open(&path, false, false).unwrap(),
            "default",
            None,
        )
        .unwrap_err();
        assert_eq!(error.mapping_runtime_diagnostic(), Some((kind, message)));
        assert_eq!(error.into_error().0, native);
        let legacy =
            mapping_history::events(Store::open(&path, false, false).unwrap(), "default", None)
                .unwrap_err();
        assert_eq!(legacy.0, native);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            DetailedError::from(Error(native.into()))
                .mapping_runtime_diagnostic()
                .is_none()
        );
    }
}

#[test]
fn ordinary_reads_reject_unknown_profiles_before_returning_populated_catalog_data() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    {
        let store = Store::open(&path, true, false).unwrap();
        catabolic_store::curation::item_put(
            &store,
            &json!({"id":"visible", "kind":"video", "metadata":{}}),
        )
        .unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    let store = Store::open(&path, false, false).unwrap();
    let known = CatalogQuery::new(&store, "default").unwrap();
    assert!(known.execute("items", &json!({})).is_ok());
    for profile in ["missing", "", "missing-雪"] {
        let error = match CatalogQuery::new(&store, profile) {
            Ok(_) => panic!("unknown profile was admitted"),
            Err(error) => error,
        };
        assert_eq!(error.0, format!("unknown profile: {profile}"));
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
