// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Shared Application.scan orchestration. All observation effects and writer
//! release/reconnect remain owned by the native observation service.
use catabolic_store::{Error, Result};
use serde_json::{Value, json};
pub fn execute(
    store: &mut catabolic_store::Store,
    profile: &str,
    request: &Value,
) -> Result<Value> {
    execute_with(store, profile, request, |store, profile, args| {
        crate::observation::execute(store, profile, "scan", args)
    })
}

/// Use the same orchestration with an embedding's publication transport.
pub fn execute_with(
    store: &mut catabolic_store::Store,
    profile: &str,
    request: &Value,
    mut observe: impl FnMut(&mut catabolic_store::Store, &str, &Value) -> Result<Value>,
) -> Result<Value> {
    catabolic_store::mutation::profile(store, profile)?;
    let mut args = request.clone();
    if request["scopes"].as_array().is_some_and(|a| !a.is_empty())
        && request["continue_id"].is_null()
    {
        return Err(Error(
            "--scope selects outstanding work; use --continue-request".into(),
        ));
    }
    let excludes = request["exclude"].as_array().cloned().unwrap_or_default();
    let continuation = !request["continue_id"].is_null();
    let resume = !request["request_id"].is_null();
    if continuation && (resume || !request["location"].is_null() || !excludes.is_empty()) {
        return Err(Error(
            "continuation uses the original source and exclusions".into(),
        ));
    }
    if resume
        && (request["extended"] == true
            || request["budgets"]
                .as_object()
                .is_some_and(|o| !o.is_empty())
            || request["scopes"].as_array().is_some_and(|a| !a.is_empty()))
    {
        return Err(Error(
            "use --continue-request to amend execution budgets".into(),
        ));
    }
    if resume && (!request["location"].is_null() || !excludes.is_empty()) {
        return Err(Error(
            "resuming a scan uses its recorded scope (source and exclusions)".into(),
        ));
    }
    args["exclude"] = json!(excludes);
    for key in ["budgets", "scopes", "continue_id", "request_id"] {
        if args[key].is_null()
            || key == "scopes" && args[key].as_array().is_some_and(|a| a.is_empty())
        {
            args.as_object_mut().unwrap().remove(key);
        }
    }
    if resume {
        // Public admission above rejected actual scope changes. Do not turn
        // its normalized empty default into an amendment at the owner boundary.
        args.as_object_mut().unwrap().remove("exclude");
    }
    if resume || continuation {
        return observe(store, profile, &args);
    }
    let sources = if let Some(source) = request["location"].as_str() {
        vec![source.to_owned()]
    } else {
        catabolic_store::migration::rows(&store.db, "SELECT id FROM locations ORDER BY id", &[])?
            .into_iter()
            .map(|r| r["id"].as_str().unwrap().to_owned())
            .collect()
    };
    // Record the actual public request boundary. The native owner retains its
    // existing (later) admission-time minimum_start floor when reuse is off.
    args["after"] = json!(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    );
    let mut scans = vec![];
    let mut observations = vec![];
    let mut complete = true;
    let mut started = 0;
    for source in &sources {
        args["source"] = json!(source);
        args["reuse_completed"] = json!(false);
        let value = observe(store, profile, &args)?;
        complete &= value["complete"] == true;
        started += 1;
        if let Some(v) = value["scans"].as_array() {
            scans.extend(v.iter().cloned());
        }
        if let Some(v) = value["observations"].as_array() {
            observations.extend(v.iter().cloned());
        }
        if observations.last().is_some_and(|r| {
            matches!(
                r["report"]["blocker"].as_str(),
                Some("temporary_storage_low" | "temporary_storage_budget")
            )
        }) {
            break;
        }
    }
    Ok(
        json!({"complete":complete,"scans":scans,"observations":observations,"unstarted_sources":&sources[started..]}),
    )
}
