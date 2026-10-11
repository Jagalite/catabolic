// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Rule orchestration delegates every effect to its existing native owner.
use catabolic_store::{
    Error, Result, Store, definitions, media_mutation, processor_mutation, rules as storage,
    selection::Evaluation,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, atomic::AtomicBool},
};
fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .ok_or_else(|| Error(format!("{k} must be text")))
}
fn page(v: &Value, key: &str, default: usize) -> Result<usize> {
    v.get(key)
        .map(Value::as_u64)
        .unwrap_or(Some(default as u64))
        .filter(|n| (1..=1000).contains(n))
        .map(|n| n as usize)
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))
}
fn scan_ids(args: &Value) -> Result<Option<&Value>> {
    let value = &args["scan_ids"];
    if value.is_null() {
        Ok(None)
    } else if value
        .as_array()
        .is_some_and(|a| a.iter().all(|v| v.as_str().is_some()))
    {
        Ok(Some(value))
    } else {
        Err(Error("scan_ids must be an array of scan IDs".into()))
    }
}
fn plan(store: &Store, profile: &str, id: &str, scans: Option<&Value>) -> Result<Value> {
    let snapshot = Store::open(&store.path, false, false)?;
    if snapshot.database_id != store.database_id {
        return Err(Error(
            "database identity changed before rule planning".into(),
        ));
    }
    let rule = definitions::rule_get(&snapshot, profile, id)?;
    let operation = media_mutation::operation_get(&snapshot, field(&rule, "recipe_id")?)?;
    let mut config = if operation["operation_kind"] == "analysis" {
        crate::media::operation_config(
            field(&operation, "preset")?,
            &operation["definition"]["options"],
        )?
    } else {
        Value::Null
    };
    if operation["operation_kind"] == "analysis" {
        config["operation_id"] = operation["id"].clone();
    }
    let mut evaluation = Evaluation::new(snapshot, profile, 60000, 100000, false)?;
    storage::plan(
        &mut evaluation,
        id,
        &config,
        scans,
        crate::rendering::input_error,
    )
}
fn maximum(args: &Value) -> Result<Option<&Value>> {
    let v = &args["max_new_bytes"];
    if v.is_null() {
        Ok(None)
    } else if storage::integer(v) {
        Ok(Some(v))
    } else {
        Err(Error("max new bytes must be a nonnegative integer".into()))
    }
}
fn apply(store: &mut Store, profile: &str, id: &str, args: &Value) -> Result<Value> {
    let batch = page(args, "batch", 100)?;
    let limit = page(args, "limit", 100)?;
    let max = maximum(args)?;
    let retry = args["retry_failed"] == true;
    let scans = scan_ids(args)?;
    definitions::require_recovered(store)?;
    let plan = plan(store, profile, id, scans)?;
    let rule = &plan["rule"];
    let operation = media_mutation::operation_get(store, field(rule, "recipe_id")?)?;
    let external = operation["operation_kind"] == "external";
    let matches = plan["matches"].as_array().unwrap();
    let candidates = matches
        .iter()
        .filter(|r| {
            r["state"] == "missing"
                || !external && r["state"] == "stale"
                || retry && r["state"] == "failed"
        })
        .cloned()
        .collect::<Vec<_>>();
    let selected = &candidates[..candidates.len().min(batch)];
    let batch_space = if external {
        storage::total(&[])?
    } else {
        storage::total(selected)?
    };
    let mut blockers = vec![];
    if external {
        if !candidates.is_empty() && max.is_some() {
            blockers.push(json!("External storage is unknown; a local byte budget cannot authorize this external batch."));
        }
    } else {
        if !selected.is_empty()
            && !plan["space"]["available_bytes"].is_null()
            && storage::compare(
                &storage::add(
                    &batch_space["planning_bytes"],
                    &plan["space"]["reserve_bytes"],
                )?,
                &plan["space"]["available_bytes"],
            )
            .is_gt()
        {
            blockers.push(json!(
                "Selected batch exceeds available destination space using the planning estimate."
            ));
        }
        if max.is_some_and(|max| storage::compare(&batch_space["planning_bytes"], max).is_gt()) {
            blockers.push(json!(
                "Selected batch exceeds --max-new-bytes using the planning estimate."
            ));
        }
    }
    let mut result = storage::bounded(&plan, limit);
    result["batch_space"] = batch_space;
    result["queued"] = json!([]);
    result["adopted"] = json!(0);
    result["errors"] = json!([]);
    result["blockers"] = json!(blockers);
    result["remaining_to_enqueue"] = json!(candidates.len());
    result["safe"] = json!(blockers.is_empty());
    result["complete"] = json!(false);
    if !blockers.is_empty() {
        return Ok(result);
    }
    storage::requirements(store, profile, &plan)?;
    let mut adopted = 0;
    for row in matches {
        if matches!(
            row["state"].as_str(),
            Some("satisfied" | "queued" | "running")
        ) && let Some(job) = row["job_id"].as_str()
        {
            storage::attach(store, profile, rule, row, job, external)?;
            adopted += 1;
        }
    }
    result["adopted"] = json!(adopted);
    let mut queued = vec![];
    let mut errors = vec![];
    for row in selected {
        let admitted: Result<Value> = (|| {
            let operation = media_mutation::operation_get(store, field(rule, "recipe_id")?)?;
            let queued = if external {
                let job = if row["state"] == "failed" {
                    processor_mutation::retry(store, profile, field(row, "job_id")?)?
                } else {
                    processor_mutation::enqueue(
                        store,
                        profile,
                        field(&operation, "preset")?,
                        field(row, "file_id")?,
                        field(row, "item_id")?,
                        field(&operation["definition"], "output_definition_id")?,
                        &operation["definition"]["options"],
                        Some(field(&operation, "id")?),
                    )?
                };
                json!({"job_id":job["id"],"executor":"processor","state":job["state"]})
            } else if operation["operation_kind"] == "analysis" {
                if row["state"] == "failed" && !row["job_id"].is_null() {
                    media_mutation::retry(store, profile, field(row, "job_id")?)?;
                    json!({"job_id":row["job_id"]})
                } else {
                    let outcome = crate::media::enqueue(
                        store,
                        profile,
                        field(&operation, "preset")?,
                        &[field(row, "file_id")?.into()],
                        &operation["definition"]["options"],
                        row["state"] == "stale",
                        Some(field(&operation, "id")?),
                    )?;
                    if let Some(error) = outcome["errors"].as_array().and_then(|a| a.first()) {
                        return Err(Error(
                            error["error"]
                                .as_str()
                                .unwrap_or("analysis enqueue failed")
                                .into(),
                        ));
                    }
                    let job = outcome["queued"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .chain(outcome["cached"].as_array().unwrap())
                        .next()
                        .ok_or_else(|| Error("analysis enqueue produced no job".into()))?;
                    json!({"job_id":job})
                }
            } else {
                crate::artifacts::enqueue(
                    store,
                    profile,
                    field(row, "file_id")?,
                    field(rule, "recipe_id")?,
                    field(rule, "location")?,
                    field(row, "item_id")?,
                )?
            };
            if !external && row["state"] == "stale" && !queued["artifact_id"].is_null() {
                return Err(Error("Existing output checksum is intact but its recorded evidence is stale; scan the generated location and run process enqueue verify for the artifact file before reapplying this rule.".into()));
            }
            storage::attach(
                store,
                profile,
                rule,
                row,
                field(&queued, "job_id")?,
                external,
            )?;
            Ok(queued)
        })();
        match admitted {
            Ok(v) => queued.push(v),
            Err(error) => {
                let mut failure = json!({"file_id":row["file_id"],"message":error.0});
                if !external {
                    failure["item_id"] = row["item_id"].clone();
                }
                errors.push(failure);
                break;
            }
        }
    }
    let remaining = candidates.len() - queued.len();
    result["remaining_to_enqueue"] = json!(remaining);
    result["complete"] = json!(
        errors.is_empty()
            && remaining == 0
            && plan["counts"]["deferred"] == 0
            && (retry || plan["counts"]["failed"] == 0)
    );
    result["queued"] = json!(queued);
    result["errors"] = json!(errors);
    Ok(result)
}
fn run(store: &mut Store, profile: &str, id: &str, args: &Value, refresh: bool) -> Result<Value> {
    let batch = page(args, "batch", 1)?;
    let limit = page(args, "limit", 100)?;
    let scans = scan_ids(args)?;
    let before = plan(store, profile, id, scans)?;
    let operation = media_mutation::operation_get(store, field(&before["rule"], "recipe_id")?)?;
    if operation["operation_kind"] == "external" {
        return Ok(
            json!({"rule_id":id,"execution":{"completed":[],"errors":[],"complete":true,"executor":"processor tick or leased processor dispatch","pending_external":before["counts"]["queued"].as_u64().unwrap()+before["counts"]["running"].as_u64().unwrap()},"after":storage::bounded(&before,limit),"complete":before["counts"]["satisfied"]==before["matched_inputs"]}),
        );
    }
    let registered = storage::registered_jobs(store, id)?;
    let selected = before["matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            r["state"] == "queued"
                && r["job_id"]
                    .as_str()
                    .is_some_and(|id| registered.contains(id))
        })
        .take(batch)
        .filter_map(|r| r["job_id"].as_str())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let planned = before["matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            r["job_id"]
                .as_str()
                .is_some_and(|id| selected.iter().any(|s| s == id))
        })
        .cloned()
        .collect::<Vec<_>>();
    let space = storage::total(&planned)?;
    let execution = if !selected.is_empty()
        && !before["space"]["available_bytes"].is_null()
        && storage::compare(
            &storage::add(&space["planning_bytes"], &before["space"]["reserve_bytes"])?,
            &before["space"]["available_bytes"],
        )
        .is_gt()
    {
        json!({"completed":[],"errors":[{"error":"Render batch exceeds available space using the planning estimate."}],"complete":false})
    } else if operation["operation_kind"] == "analysis" {
        let mut result =
            crate::media::run(store, profile, &selected, Arc::new(AtomicBool::new(false)))?;
        let mut remaining = 0;
        for job in selected.iter().collect::<BTreeSet<_>>() {
            if media_mutation::get_job(store, profile, job)?["state"] == "queued" {
                remaining += 1;
            }
        }
        result["remaining"] = json!(remaining);
        result
    } else {
        crate::artifacts::run(
            store,
            profile,
            batch,
            Some(&selected),
            Arc::new(AtomicBool::new(false)),
        )?
    };
    let execution = if refresh {
        crate::catalog_refresh::finish(store, profile, execution)?
    } else {
        execution
    };
    let after = plan(store, profile, id, scans)?;
    storage::record_evaluation(store, profile, &after)?;
    Ok(
        json!({"rule_id":id,"complete":execution["complete"]==true&&after["counts"]["satisfied"]==after["matched_inputs"],"execution":execution,"after":storage::bounded(&after,limit)}),
    )
}
fn maintain(store: &mut Store, profile: &str, args: &Value) -> Result<Value> {
    let batch = page(args, "batch", 100)?;
    let limit = page(args, "limit", 100)?;
    maximum(args)?;
    let scans = scan_ids(args)?;
    let render_batch = args
        .get("render_batch")
        .map(Value::as_u64)
        .unwrap_or(Some(0))
        .filter(|n| *n <= 1000)
        .ok_or_else(|| Error("render batch must be between 0 and 1000".into()))?
        as usize;
    let enabled = storage::enabled_rules(store, profile)?;
    if enabled.len() > 1000 {
        return Err(Error(
            "maintenance supports at most 1000 enabled rules".into(),
        ));
    }
    let plans = enabled
        .iter()
        .map(|r| plan(store, profile, field(r, "id")?, scans))
        .collect::<Result<Vec<_>>>()?;
    let mut volumes = BTreeMap::<String, Value>::new();
    let mut order = vec![];
    let mut seen = BTreeSet::new();
    for plan in &plans {
        let space = &plan["space"];
        if space["device"].is_null() {
            continue;
        }
        let key = space["device"].to_string();
        if !volumes.contains_key(&key) {
            order.push(key.clone());
            volumes.insert(key.clone(),json!({"device":space["device"],"available_bytes":space["available_bytes"],"reserve_bytes":0,"estimates":[],"queued_estimates":[]}));
        }
        let group = volumes.get_mut(&key).unwrap();
        if storage::compare(&space["reserve_bytes"], &group["reserve_bytes"]).is_gt() {
            group["reserve_bytes"] = space["reserve_bytes"].clone();
        }
        for row in plan["matches"].as_array().unwrap() {
            let identity = catabolic_core::encode(&json!([
                row["file_id"],
                row["item_id"],
                plan["rule"]["recipe_id"],
                plan["rule"]["location"]
            ]));
            if row["state"] != "satisfied" && seen.insert(identity) {
                group["estimates"]
                    .as_array_mut()
                    .unwrap()
                    .push(row["estimate"].clone());
                if matches!(row["state"].as_str(), Some("queued" | "running")) {
                    group["queued_estimates"]
                        .as_array_mut()
                        .unwrap()
                        .push(row["estimate"].clone());
                }
            }
        }
    }
    let mut reports = vec![];
    let mut allowances = BTreeMap::new();
    for key in order {
        let group = &volumes[&key];
        let mut report = catabolic_store::estimates::total(&group["estimates"])?;
        for field in ["device", "available_bytes", "reserve_bytes"] {
            report[field] = group[field].clone();
        }
        reports.push(report);
        allowances.insert(key,storage::subtract(&group["available_bytes"],&storage::add(&group["reserve_bytes"],&catabolic_store::estimates::total(&group["queued_estimates"])?["planning_bytes"])?)?);
    }
    let (mut remaining, mut render_remaining, mut bytes_remaining) =
        (batch, render_batch, args["max_new_bytes"].clone());
    let mut healthy = true;
    let mut results = vec![];
    for original in &plans {
        let id = field(&original["rule"], "id")?;
        let key = original["space"]["device"].to_string();
        let mut result = json!({"rule_id":id,"preview":storage::bounded(original,limit)});
        if remaining > 0 {
            let mut allowance = allowances.get(&key).cloned().unwrap_or(Value::Null);
            if !bytes_remaining.is_null()
                && (allowance.is_null() || storage::compare(&bytes_remaining, &allowance).is_lt())
            {
                allowance = bytes_remaining.clone();
            }
            let mut request = args.clone();
            request["batch"] = json!(remaining);
            request["max_new_bytes"] = allowance;
            let applied = apply(store, profile, id, &request)?;
            remaining -= applied["queued"].as_array().unwrap().len();
            if applied["safe"] == true {
                if !bytes_remaining.is_null() {
                    bytes_remaining = storage::subtract(
                        &bytes_remaining,
                        &applied["batch_space"]["planning_bytes"],
                    )?;
                }
                if let Some(allowance) = allowances.get_mut(&key) {
                    *allowance =
                        storage::subtract(allowance, &applied["batch_space"]["planning_bytes"])?;
                }
            }
            if applied["safe"] != true || !applied["errors"].as_array().unwrap().is_empty() {
                healthy = false;
            }
            result["apply"] = applied;
        }
        if render_remaining > 0 && healthy {
            let mut request = args.clone();
            request["batch"] = json!(render_remaining);
            let executed = run(store, profile, id, &request, false)?;
            let execution = &executed["execution"];
            let processed = execution["processed"].as_u64().unwrap_or_else(|| {
                execution["completed"]
                    .as_array()
                    .map_or(0, |a| a.len() as u64)
                    + execution["errors"].as_array().map_or(0, |a| a.len() as u64)
            }) as usize;
            render_remaining = render_remaining.saturating_sub(processed);
            healthy = execution["complete"] == true;
            result["run"] = executed;
        }
        results.push(result);
        if !healthy {
            break;
        }
    }
    let after = enabled
        .iter()
        .map(|r| plan(store, profile, field(r, "id")?, scans))
        .collect::<Result<Vec<_>>>()?;
    let mut backlog = json!({});
    for state in storage::STATES {
        backlog[state] = json!(
            after
                .iter()
                .map(|p| p["counts"][state].as_u64().unwrap())
                .sum::<u64>()
        );
    }
    Ok(
        json!({"rules":results,"backlog":backlog,"space_by_device":reports,"space_scope":"All outstanding enabled-rule matches before this cycle, deduplicated by input/item/recipe/destination.","safe_to_continue":healthy,"complete":healthy&&after.iter().all(|p|p["counts"]["satisfied"]==p["matched_inputs"])}),
    )
}
pub fn dispatch(store: &mut Store, profile: &str, action: &str, args: &Value) -> Result<Value> {
    match action {
        "preview" => {
            let limit = page(args, "limit", 100)?;
            Ok(storage::bounded(
                &plan(store, profile, field(args, "id")?, scan_ids(args)?)?,
                limit,
            ))
        }
        "statistics" => storage::statistics(store, profile, field(args, "id")?),
        "apply" => apply(store, profile, field(args, "id")?, args),
        "run" => run(store, profile, field(args, "id")?, args, true),
        "maintain" => maintain(store, profile, args),
        _ => Err(Error("unknown rule operation".into())),
    }
}
