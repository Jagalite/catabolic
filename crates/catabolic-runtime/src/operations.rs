// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Public immutable operation admission. Existing owners retain configuration
//! validation, recipe persistence, and all subsequent execution effects.
use catabolic_store::{Error, Result, Store, media_mutation};
use serde_json::{Value, json};

pub fn put(store: &Store, profile: &str, name: &str, value: &Value) -> Result<Value> {
    catabolic_store::mutation::require_recovered(&store.db)?;
    catabolic_store::query::name(name)?;
    if !value.as_object().is_some_and(|object| {
        object.keys().all(|key| {
            [
                "version",
                "kind",
                "operation",
                "options",
                "output_definition_id",
            ]
            .contains(&key.as_str())
        })
    }) || value.get("version").is_some_and(|v| v.as_u64() != Some(1))
    {
        return Err(Error("invalid version 1 operation definition".into()));
    }
    let options = value.get("options").cloned().unwrap_or(json!({}));
    let operation = value["operation"]
        .as_str()
        .filter(|_| options.is_object())
        .ok_or_else(|| Error("operation must be a name and options must be an object".into()))?;
    match value["kind"].as_str() {
        Some("render") => crate::artifacts::recipe(
            store,
            name,
            operation,
            &options,
            value["output_definition_id"].as_str(),
        ),
        Some("analysis") if value.get("output_definition_id").is_none() => {
            crate::media::operation_config(operation, &options)?;
            media_mutation::operation_put(
                store,
                name,
                &json!({
                    "version":1,"kind":"analysis","operation":operation,"options":options,
                }),
            )
        }
        Some("external") => {
            catabolic_store::processor_mutation::get(store, profile, operation)?;
            // Missing/null/non-text IDs all fail the immutable-definition lookup.
            let output = value["output_definition_id"].as_str().unwrap_or("");
            catabolic_store::output_mutation::get_definition(store, output)?;
            if catabolic_core::encode(&options).len() > 65536 {
                return Err(Error(
                    "external configuration must be an object bounded to 64 KiB".into(),
                ));
            }
            media_mutation::operation_put(
                store,
                name,
                &json!({
                    "version":1,"kind":"external","operation":operation,"options":options,
                    "output_definition_id":output,
                }),
            )
        }
        _ => Err(Error(
            "analysis operations record facts; render operations produce files".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Store, Value) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.db");
        catabolic_store::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        catabolic_store::processor_mutation::put(
            &store,
            "default",
            "fixture",
            &json!({"endpoint":"http://127.0.0.1:9"}),
        )
        .unwrap();
        let output = catabolic_store::output_mutation::define(
            &store,
            "fixture",
            &json!({"purpose":"custom"}),
        )
        .unwrap();
        (
            dir,
            store,
            json!({"kind":"external","operation":"fixture","output_definition_id":output["id"]}),
        )
    }
    #[test]
    fn external_metadata_reuses_digest_and_increments_immutable_revision() {
        let (_dir, store, mut input) = setup();
        let first = put(&store, "default", "fixture", &input).unwrap();
        input["version"] = json!(1);
        input["options"] = json!({});
        assert_eq!(first, put(&store, "default", "fixture", &input).unwrap());
        input["options"] = json!({"unicode":"é", "float":1.0,"integer":1});
        let second = put(&store, "default", "fixture", &input).unwrap();
        assert_eq!(second["revision"], 2);
        assert_ne!(second["id"], first["id"]);
        assert_eq!(second["definition"], input);
        assert_eq!(second["operation_kind"], "external");
        assert_eq!(
            second["output_definition_id"],
            input["output_definition_id"]
        );
        assert_eq!(
            first,
            media_mutation::operation_get(&store, first["id"].as_str().unwrap()).unwrap()
        );
        for table in [
            "processor_jobs",
            "external_receipts",
            "media_outputs",
            "journal",
        ] {
            assert_eq!(
                store
                    .db
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
    #[test]
    fn api_definition_options_are_not_interpreted_as_publication_reports() {
        let (_dir, store, mut input) = setup();
        let path = store.path.clone();
        input["options"] = json!({"consumers":{
            "scope":"profile","processed":0,"unresolved_bindings":0,"state":"idle",
        }});
        drop(store);
        let result = crate::api::execute(
            &path,
            "default",
            "media",
            "operation-put",
            &json!({"name":"fixture","definition":input}),
        )
        .unwrap();
        assert_eq!(result["definition"]["options"], input["options"]);
        let store = Store::open(&path, false, false).unwrap();
        assert_eq!(
            result,
            media_mutation::operation_get(&store, result["id"].as_str().unwrap()).unwrap()
        );
    }
    #[test]
    fn bounded_options_and_admission_errors_do_not_create_recipes() {
        let (_dir, store, mut input) = setup();
        for (key, value, expected) in [
            (
                "version",
                json!(true),
                "invalid version 1 operation definition",
            ),
            (
                "version",
                json!(1.0),
                "invalid version 1 operation definition",
            ),
            (
                "operation",
                json!("missing"),
                "unknown processor in this profile",
            ),
            (
                "options",
                json!([]),
                "operation must be a name and options must be an object",
            ),
            (
                "options",
                json!({"x":"a".repeat(65528)}),
                "external configuration must be an object bounded to 64 KiB",
            ),
        ] {
            let mut rejected = input.clone();
            rejected[key] = value;
            assert_eq!(
                put(&store, "default", "fixture", &rejected).unwrap_err().0,
                expected
            );
            assert_eq!(
                store
                    .db
                    .query_row("SELECT count(*) FROM processing_recipes", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        input["options"] = json!({"x":"a".repeat(65527)});
        assert_eq!(catabolic_core::encode(&input["options"]).len(), 65536);
        assert!(put(&store, "default", "fixture", &input).is_ok());
    }
}
