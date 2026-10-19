// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Persistent public artifact adapter. All durable decisions remain with the
//! production artifact owner; this layer only adapts arguments and refresh.
use catabolic_store::{Error, Result, Store, artifact_mutation as storage};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .ok_or_else(|| Error(format!("{key} must be text")))
}
fn limit(args: &Value) -> Result<usize> {
    args.get("limit")
        .map_or(Some(100), Value::as_u64)
        .filter(|n| (1..=1000).contains(n))
        .map(|n| n as usize)
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))
}
pub fn execute(
    store: &mut Store,
    profile: &str,
    operation: &str,
    args: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    catabolic_store::mutation::profile(store, profile)?;
    match operation {
        "enqueue" => {
            let capabilities = match args.get("_capabilities").filter(|v| !v.is_null()) {
                Some(value) => value.clone(),
                None => store.detached(crate::rendering::capabilities)?,
            };
            if cancel.load(Ordering::Relaxed) {
                return Err(Error("artifact admission cancelled".into()));
            }
            storage::enqueue_facade(
                store,
                profile,
                text(args, "file_id")?,
                text(args, "recipe_id")?,
                text(args, "location")?,
                text(args, "item_id")?,
                &capabilities,
                args.get("_full_cache_check")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                args["_source_lineage_mode"].as_str(),
                args.get("_component_package").filter(|v| !v.is_null()),
            )
        }
        "run" => {
            let limit = limit(args)?;
            let ids = args
                .get("job_ids")
                .filter(|v| !v.is_null())
                .map(|value| {
                    value
                        .as_array()
                        .filter(|ids| ids.len() <= 1000)
                        .and_then(|ids| {
                            ids.iter()
                                .map(|id| id.as_str().map(str::to_owned))
                                .collect::<Option<Vec<_>>>()
                        })
                        .ok_or_else(|| Error("job_ids must contain at most 1000 IDs".into()))
                })
                .transpose()?;
            if !store.db.is_autocommit() {
                return Err(Error(
                    "artifact execution requires a writer outside an active transaction".into(),
                ));
            }
            let result =
                crate::artifacts::run(store, profile, limit, ids.as_deref(), cancel.clone())?;
            if !cancel.load(Ordering::Relaxed)
                && args
                    .get("_refresh")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
            {
                crate::catalog_refresh::finish(store, profile, result)
            } else {
                Ok(result)
            }
        }
        "recover" => {
            let limit = limit(args)?;
            if !store.db.is_autocommit() {
                return Err(Error(
                    "artifact recovery requires a writer outside an active transaction".into(),
                ));
            }
            let result = crate::artifacts::recover(store, profile, limit)?;
            if !cancel.load(Ordering::Relaxed) {
                crate::catalog_refresh::finish(store, profile, result)
            } else {
                Ok(result)
            }
        }
        _ => Err(Error("unknown artifact runtime operation".into())),
    }
}
pub fn call(store: &Store, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    catabolic_store::mutation::profile(store, profile)?;
    match operation {
        "bind" => storage::bind(store, profile, text(args, "location")?, text(args, "root")?),
        "recipe" => crate::artifacts::recipe(
            store,
            text(args, "recipe_name")?,
            text(args, "preset")?,
            args.get("options")
                .filter(|v| !v.is_null())
                .unwrap_or(&json!({})),
            args["output_definition_id"]
                .as_str()
                .filter(|v| !v.is_empty()),
        ),
        "default_output" => {
            let preset = text(args, "preset")?;
            catabolic_store::output_mutation::define(
                store,
                &format!("builtin-{preset}"),
                &json!({"role":crate::rendering::preset(preset)?.role}),
            )
        }
        "get_recipe" => storage::get_recipe(store, text(args, "id")?).map_err(|e| {
            if e.0 == "unknown immutable operation revision" {
                Error("unknown recipe ID; select an explicit immutable revision".into())
            } else {
                e
            }
        }),
        "get" => storage::get(store, profile, text(args, "id")?),
        "list" | "recipes" => {
            catabolic_store::public_reads::artifact(store, profile, operation, args)
        }
        _ => Err(Error("unknown artifact operation".into())),
    }
}
