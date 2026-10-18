// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! One native owner for v1 preview, reviewed metadata writes and readback recovery.
//! Every remote call runs outside all Store lifetimes/SQLite transactions. The
//! interoperable metadata lock alone spans publication; no unknown effect is replayed.
mod adapters;
use adapters::MappingAdapter;
use catabolic_core::encode;
use catabolic_store::{
    Error, Result, Store, consumers, destination_mappings as policy, mapping_history,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub const INTERRUPTED: &str = "destination mapping interrupted";
fn checkpoint(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error(INTERRUPTED.into()))
    } else {
        Ok(())
    }
}
fn remote<T>(cancel: &AtomicBool, action: impl FnOnce() -> Result<T>) -> Result<T> {
    checkpoint(cancel)?;
    let result = action();
    checkpoint(cancel)?;
    result
}
fn serialization_path(database: &Path) -> Result<PathBuf> {
    let path = database.canonicalize().or_else(|_| {
        let absolute = std::path::absolute(database)?;
        Ok::<_, std::io::Error>(
            absolute
                .parent()
                .unwrap()
                .canonicalize()?
                .join(absolute.file_name().unwrap()),
        )
    })?;
    let mut path = path.into_os_string();
    path.push(".plex-metadata");
    Ok(path.into())
}
fn connection(database: &Path, profile: &str, identifier: &Value) -> Result<Value> {
    let store = Store::open(database, false, false)?;
    consumers::connection(&store, profile, identifier.as_str().unwrap_or(""))
}
fn history(database: &Path, profile: &str) -> Result<(String, Vec<Value>)> {
    let store = Store::open(database, false, false)?;
    Ok((
        store.database_id.clone(),
        mapping_history::history(&store, profile)?,
    ))
}
fn prepare(
    database: &Path,
    profile: &str,
    definition: &Value,
    cancel: &AtomicBool,
) -> Result<(Value, MappingAdapter)> {
    if !definition.is_object() || !definition["destination"].is_object() {
        return Err(policy::error("invalid_definition"));
    }
    let c = connection(database, profile, &definition["destination"]["connection"])?;
    // Frozen preparation validates the remote before constructing collection scope.
    let base = MappingAdapter::new(&c, &Value::Null)?;
    remote(cancel, || base.validate_remote(None))?;
    let adapter = MappingAdapter::new(&c, &definition["destination"]["collections"])?;
    let local = policy::evaluate(
        database,
        profile,
        definition,
        &adapter.capabilities(),
        &|field, value| adapter.validate_value(field, value),
    )?;
    let libraries = remote(cancel, || adapter.libraries())?;
    let libraries = libraries
        .into_iter()
        .filter(|r| r["id"] == definition["destination"]["library"])
        .collect::<Vec<_>>();
    if libraries.len() != 1 {
        return Err(policy::error("unknown_library"));
    }
    let library = &libraries[0];
    let scope = json!([c["application"], c["server_id"], library["uuid"]]);
    let (_, events) = history(database, profile)?;
    let ledger = policy::ownership(&events, &scope)?;
    let mut rows = Vec::new();
    let mut removals = 0;
    let mut bytes = 0;
    for item in local["rows"].as_array().unwrap() {
        let before = remote(cancel, || {
            adapter.read(library, &item["target"], &item["values"], cancel)
        })?;
        let (row, count) =
            policy::planned_row(definition, item, before, &ledger, &adapter.capabilities())?;
        adapter.validate(library, &row)?;
        bytes += encode(&row).len();
        if bytes > 4 * 1024 * 1024 {
            return Err(policy::error("plan_too_large"));
        }
        removals += count;
        rows.push(row);
    }
    Ok((
        policy::plan(definition, &local, &c, library, rows, removals, &ledger)?,
        adapter,
    ))
}
fn code(error: &Error) -> &str {
    error
        .0
        .strip_prefix("consumer ")
        .and_then(|e| e.split(';').next())
        .unwrap_or(&error.0)
}
/// The public owner takes only a database path, never a foreign/live connection.
pub fn run(database: &Path, profile: &str, request: &Value, cancel: &AtomicBool) -> Result<Value> {
    let apply = request["apply"] == true;
    let _serialization = if apply {
        Some(
            catabolic_store::writer_lock(&serialization_path(database)?)
                .map_err(|_| Error("metadata_writer_busy_preview_again".into()))?,
        )
    } else {
        None
    };
    let maximum = request
        .get("max_removals")
        .map(policy::removal_budget)
        .transpose()?
        .unwrap_or(0);
    let expected = request["expected_plan"].as_str();
    if apply && expected.is_none_or(str::is_empty) {
        return Err(policy::error("apply_requires_expected_plan"));
    }
    checkpoint(cancel)?;
    let definition = &request["definition"];
    let (plan, adapter) = prepare(database, profile, definition, cancel)?;
    let mut result = policy::review(&plan, apply, expected, maximum)?;
    if !apply {
        return Ok(result);
    }
    let database_id = plan["local"]["database_id"].as_str().unwrap();
    for row in plan["rows"].as_array().unwrap() {
        checkpoint(cancel)?;
        if policy::evaluate(
            database,
            profile,
            definition,
            &adapter.capabilities(),
            &|field, value| adapter.validate_value(field, value),
        )? != plan["local"]
        {
            return Err(policy::error("query_changed_preview_again"));
        }
        if connection(database, profile, &definition["destination"]["connection"])?
            != plan["connection"]
        {
            return Err(policy::error("connection_changed"));
        }
        remote(cancel, || adapter.validate_remote(Some(&plan["library"])))?;
        if !policy::equal(
            &remote(cancel, || {
                adapter.read(&plan["library"], &row["target"], &row["values"], cancel)
            })?,
            &row["before"],
        ) {
            return Err(policy::error("remote_changed_preview_again"));
        }
        checkpoint(cancel)?;
        let id = uuid::Uuid::new_v4().to_string();
        let mut payload = policy::event(&plan, row, &result["plan_id"]);
        policy::save_event(database, profile, database_id, &id, &payload, true)?;
        let outcome = (|| {
            if !policy::equal(&row["before"]["values"], &row["after"]) {
                remote(cancel, || adapter.write(&plan["library"], row, cancel))?;
            }
            remote(cancel, || adapter.validate_remote(Some(&plan["library"])))?;
            let observed = remote(cancel, || {
                adapter.read(&plan["library"], &row["target"], &row["values"], cancel)
            })?;
            if !adapter.matches(&observed, &row["before"], &row["after"]) {
                return Err(policy::error("verification_failed"));
            }
            checkpoint(cancel)?;
            payload["state"] = json!("verified");
            policy::save_event(database, profile, database_id, &id, &payload, false)?;
            Ok(())
        })();
        match outcome {
            Ok(()) => result["results"]
                .as_array_mut()
                .unwrap()
                .push(json!({"key":row["key"],"event_id":id,"state":"verified"})),
            Err(error) => {
                // KeyboardInterrupt is not an Exception in the frozen owner.
                // Retain started for readback recovery; never claim an outcome.
                if error.0 == INTERRUPTED || code(&error) == "interrupted" {
                    return Err(error);
                }
                payload["state"] = json!("uncertain");
                policy::save_event(database, profile, database_id, &id, &payload, false)?;
                result["event_id"] = json!(id);
                result["state"] = json!("uncertain");
                result["complete"] = json!(false);
                result["safe_to_retry"] = json!(false);
                let code = code(&error);
                result["error"] = json!(
                    if code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                        code
                    } else {
                        "mapping_write_or_verification_failed"
                    }
                );
                return Ok(result);
            }
        }
    }
    result["applied"] = json!(true);
    Ok(result)
}

/// `None` hands v2 back to its unavailable owner. HTTP-v3 remains read-only.
/// Ordinary recovery never performs, retries, or undoes a destination mutation.
pub fn recover(
    database: &Path,
    profile: &str,
    request: &Value,
    cancel: &AtomicBool,
) -> Result<Option<Value>> {
    let apply = request["apply"] == true;
    let _serialization = if apply {
        Some(
            catabolic_store::writer_lock(&serialization_path(database)?)
                .map_err(|_| Error("metadata_writer_busy_preview_again".into()))?,
        )
    } else {
        None
    };
    checkpoint(cancel)?;
    let id = request["event_id"].as_str().unwrap_or("");
    let (database_id, events) = history(database, profile)?;
    let matches = events
        .into_iter()
        .filter(|e| e["id"] == id)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(policy::error("unknown_event"));
    }
    let event = &matches[0];
    if event["version"].as_f64() == Some(3.0) {
        if apply {
            return Err(policy::error("http_recovery_uses_notification_queue"));
        }
        return Ok(Some(
            json!({"event_id":id,"state":event["state"],"complete":event["state"]=="complete","delivery_acknowledged":event["state"]=="complete"}),
        ));
    }
    if event["version"].as_f64() == Some(2.0) {
        return catabolic_store::publication_mappings::recover(database, profile, id, request)
            .map(Some);
    }
    if !request["resolution"].is_null() || !request["expected_plan"].is_null() {
        return Err(policy::error("unsupported_recovery_options"));
    }
    if matches!(event["state"].as_str(), Some("verified" | "not_applied")) {
        return Ok(Some(
            json!({"event_id":id,"state":event["state"],"complete":true}),
        ));
    }
    let c = connection(database, profile, &event["connection"]["id"])?;
    if c != event["connection"] {
        return Err(policy::error("connection_changed"));
    }
    let base = MappingAdapter::new(&c, &Value::Null)?;
    remote(cancel, || base.validate_remote(Some(&event["library"])))?;
    let adapter = MappingAdapter::new(&c, &event["collections"])?;
    let observed = remote(cancel, || {
        adapter.read(&event["library"], &event["target"], &event["after"], cancel)
    })?;
    let state = policy::recovered(
        &observed,
        &event["before"],
        adapter.matches(&observed, &event["before"], &event["after"]),
    );
    if apply {
        checkpoint(cancel)?;
        let mut payload = event.clone();
        payload.as_object_mut().unwrap().remove("id");
        payload["state"] = json!(state);
        policy::save_event(database, profile, &database_id, id, &payload, false)?;
    }
    Ok(Some(
        json!({"event_id":id,"state":state,"recorded":apply,"complete":state!="uncertain","safe_to_retry":state=="not_applied"}),
    ))
}
