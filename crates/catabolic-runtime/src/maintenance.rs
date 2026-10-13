// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Ordinary one-cycle orchestration. Every effect stays with its native owner.
//! Output planning/publication and rule rendering remain explicitly unqualified.
use catabolic_store::{Error, Result, Store, maintenance as storage, migration::rows};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub const INTERRUPTED: &str = "native maintenance interrupted";
pub const RULE_RENDER_GATE: &str = "native maintenance rule rendering is blocked: filesystem publication and recovery are not yet qualified";

/// A text-progress callback failure keeps its original boundary category.
#[derive(Clone, Debug)]
pub struct ProgressError {
    pub message: String,
    pub kind: String,
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error(INTERRUPTED.into()))
    } else {
        Ok(())
    }
}
fn number(args: &Value, key: &str, default: u64) -> u64 {
    args.get(key).and_then(Value::as_u64).unwrap_or(default)
}
fn validate(store: &Store, args: &Value) -> Result<Vec<Value>> {
    if !store.owns_writer_lock() {
        return Err(Error("maintenance requires the catalog writer lock".into()));
    }
    for (key, low, high, default, label) in [
        ("settle", 0, 86400, 30, "settle"),
        ("batch", 1, 1000, 100, "batch"),
        ("workers", 1, 16, 2, "workers"),
        ("limit", 1, 1000, 100, "limit"),
        ("rule_batch", 1, 1000, 100, "rule batch"),
        ("render_rules", 0, 1000, 0, "render rules"),
    ] {
        if !args
            .get(key)
            .map_or(Some(default), Value::as_u64)
            .is_some_and(|n| (low..=high).contains(&n))
        {
            return Err(Error(format!("{label} must be {low}..{high}")));
        }
    }
    if args["rules"] != true
        && (number(args, "render_rules", 0) != 0 || !args["rule_max_new_bytes"].is_null())
    {
        return Err(Error(
            "render-rules and rule-max-new-bytes require --rules".into(),
        ));
    }
    if !args["rule_max_new_bytes"].is_null()
        && !catabolic_store::rules::integer(&args["rule_max_new_bytes"])
    {
        return Err(Error(
            "rule max new bytes must be a nonnegative integer".into(),
        ));
    }
    if !args["process"].is_null()
        && !["sniff", "hash", "verify", "probe", "text", "decode"]
            .contains(&args["process"].as_str().unwrap_or(""))
    {
        return Err(Error("unsupported processing operation".into()));
    }
    if args
        .get("max_removals")
        .is_some_and(|v| !catabolic_store::rules::integer(v))
    {
        return Err(Error("max removals must be a nonnegative integer".into()));
    }
    if !args["max_removal_percent"].is_null()
        && !args["max_removal_percent"]
            .as_f64()
            .is_some_and(|v| v.is_finite() && (0.0..=100.0).contains(&v))
    {
        return Err(Error(
            "max removal percent must be finite and between 0 and 100".into(),
        ));
    }
    if args["inventory_only"] == true && args["manifest"] == true {
        return Err(Error("--manifest requires output catalogs".into()));
    }
    let excludes = args["exclude"].as_array().cloned().unwrap_or_default();
    for path in &excludes {
        catabolic_store::layout::relative_path(
            path.as_str()
                .ok_or_else(|| Error("path must be text".into()))?,
        )?;
    }
    Ok(excludes)
}

fn record(
    report: &mut Value,
    name: &str,
    mut result: Value,
    limit: usize,
    progress: &mut dyn FnMut(&Value) -> Result<()>,
) -> Result<()> {
    for key in ["actions", "applied", "blockers"] {
        if let Some(entries) = result[key].as_array().cloned() {
            result[format!("{key}_count")] = json!(entries.len());
            if matches!(key, "actions" | "applied") {
                let mut kinds = serde_json::Map::new();
                for entry in &entries {
                    let kind = entry["kind"].as_str().unwrap_or("");
                    let count = kinds.get(kind).and_then(Value::as_u64).unwrap_or(0);
                    kinds.insert(kind.into(), json!(count + 1));
                }
                result[format!("{key}_by_kind")] = Value::Object(kinds);
            }
            result[format!("{key}_truncated")] = json!(entries.len() > limit);
            result[key] = json!(&entries[..entries.len().min(limit)]);
        }
    }
    let complete = result
        .get("complete")
        .or_else(|| result.get("healthy"))
        .or_else(|| result.get("safe"))
        .cloned()
        .unwrap_or(json!(true));
    result["stage"] = json!(name);
    report["stages"].as_array_mut().unwrap().push(result);
    progress(&json!({"stage":name,"complete":complete}))
}

fn analysis(
    store: &mut Store,
    profile: &str,
    scan: &Value,
    args: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    let mut selected = vec![];
    let (mut queued, mut cached, mut waiting, mut pending, mut failures, mut error_count) =
        (0, 0, 0, 0, 0, 0);
    let mut errors = vec![];
    let now = crate::media::now();
    let batch = number(args, "batch", 100) as usize;
    for scan in scan["scans"].as_array().unwrap() {
        if scan["complete"] != true {
            continue;
        }
        let mut after = String::new();
        loop {
            cancelled(&cancel)?;
            let (ids, unstable, next) = storage::stable_page(
                store,
                profile,
                scan,
                &after,
                now,
                number(args, "settle", 30),
            )?;
            let Some(next) = next else {
                break;
            };
            after = next;
            waiting += unstable;
            if ids.is_empty() {
                continue;
            }
            let result = crate::public_processing::execute(
                store,
                profile,
                "enqueue",
                &json!({"operation":args["process"],"file_ids":ids}),
                cancel.clone(),
            )?;
            queued += result["queued"].as_array().unwrap().len();
            cached += result["cached"].as_array().unwrap().len();
            let returned_errors = result["errors"].as_array().unwrap();
            error_count += returned_errors.len();
            errors.extend(returned_errors.iter().take(100 - errors.len()).cloned());
            let jobs = result["queued"]
                .as_array()
                .unwrap()
                .iter()
                .chain(result["cached"].as_array().unwrap())
                .cloned()
                .collect::<Vec<_>>();
            for row in rows(
                &store.db,
                "SELECT id,state FROM processing_jobs WHERE id IN (SELECT value FROM json_each(?)) ORDER BY created_at,id",
                &[catabolic_core::encode(&json!(jobs)).into()],
            )? {
                if matches!(row["state"].as_str(), Some("queued" | "running")) {
                    if selected.len() < batch {
                        selected.push(row["id"].clone());
                    } else {
                        pending += 1;
                    }
                } else if row["state"] != "complete" {
                    failures += 1;
                }
            }
        }
    }
    cancelled(&cancel)?;
    let processing = crate::public_processing::execute(
        store,
        profile,
        "run",
        &json!({"workers":number(args,"workers",2),"limit":batch,"job_ids":selected,"_refresh":false}),
        cancel,
    )?;
    Ok(
        json!({"operation":args["process"],"queued":queued,"cached":cached,"waiting_for_stability":waiting,"pending_beyond_batch":pending,"cached_unsuccessful_jobs":failures,"error_count":error_count,"errors":errors,"complete":processing["complete"]==true && failures==0 && error_count==0 && pending==0 && waiting==0,"safe_to_continue":processing["complete"]==true && failures==0 && error_count==0,"processing":processing}),
    )
}

pub fn run(
    store: &mut Store,
    profile: &str,
    args: &Value,
    cancel: Arc<AtomicBool>,
    progress: &mut dyn FnMut(&Value),
) -> Result<Value> {
    run_with_progress(store, profile, args, cancel, &mut |value| {
        progress(value);
        Ok(())
    })
}

/// Fallible progress transport; ordinary callers retain the original run API.
pub fn run_with_progress(
    store: &mut Store,
    profile: &str,
    args: &Value,
    cancel: Arc<AtomicBool>,
    progress: &mut dyn FnMut(&Value) -> std::result::Result<(), ProgressError>,
) -> Result<Value> {
    let excludes = validate(store, args)?;
    let limit = number(args, "limit", 100) as usize;
    let selected = if args["all_catalogs"] == true {
        Value::Null
    } else {
        args.get("catalog").cloned().unwrap_or(json!("global"))
    };
    let catalogs = if args["inventory_only"] == true {
        json!([])
    } else {
        catabolic_store::public_reconcile::call(
            store,
            profile,
            "catalogs",
            &json!({"catalog":selected}),
        )?
    };
    if !rows(&store.db,"SELECT catalog,watcher FROM watcher_owners WHERE profile=? AND catalog IN (SELECT value FROM json_each(?))",&[profile.to_owned().into(),catabolic_core::encode(&catalogs).into()])?.is_empty() { return Err(Error("watcher_owned_projection: use watcher run or maintenance --inventory-only".into())); }
    let mut report = json!({"report_version":1,"database":store.path.to_string_lossy(),"profile":profile,"catalogs":catalogs,"complete":false,"inventory_updated":false,"stages":[],"errors":[]});
    let before: i64 = store
        .db
        .query_row("SELECT count(*) FROM files", [], |r| r.get(0))?;
    let mut stage = "preflight";
    let mut blocked = false;
    let mut progress_error = None;
    let mut forward = |value: &Value| match progress(value) {
        Ok(()) => Ok(()),
        Err(error) => {
            let message = error.message.clone();
            progress_error = Some(error);
            Err(Error(message))
        }
    };
    let outcome: Result<()> = (|| {
        cancelled(&cancel)?;
        catabolic_store::mutation::require_recovered(&store.db)?;
        if !rows(&store.db,"SELECT 1 FROM processing_artifacts WHERE profile=? AND state IN ('planned','writing','validating','publishing') LIMIT 1",&[profile.to_owned().into()])?.is_empty() { return Err(Error("pending artifact publication requires artifact recover".into())); }
        let sources = rows(&store.db, "SELECT id FROM locations ORDER BY id", &[])?
            .into_iter()
            .map(|v| v["id"].clone())
            .collect::<Vec<_>>();
        if sources.is_empty() {
            return Err(Error("no source locations configured".into()));
        }
        if args["inventory_only"] != true {
            let bindings = sources
                .iter()
                .map(|id| json!(["source", id]))
                .chain(
                    catalogs
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|id| json!(["output", id])),
                )
                .collect::<Vec<_>>();
            let recovery = catabolic_store::remount::recover_device_numbers(
                store,
                profile,
                &json!({"required_bindings":bindings}),
            )?;
            if !recovery.is_null() {
                record(&mut report, "remount", recovery, limit, &mut forward)?;
            }
        }
        for (kind, owners) in [
            ("source", &sources),
            ("output", catalogs.as_array().unwrap()),
        ] {
            for owner in owners {
                cancelled(&cancel)?;
                storage::check_root(store, profile, kind, owner.as_str().unwrap())?;
            }
        }
        record(
            &mut report,
            stage,
            json!({"complete":true,"sources":sources}),
            limit,
            &mut forward,
        )?;
        cancelled(&cancel)?;
        stage = "scan";
        let scan = crate::public_scan::execute(store, profile, &json!({"exclude":excludes}))?;
        report["inventory_updated"] = scan["complete"].clone();
        record(&mut report, stage, scan.clone(), limit, &mut forward)?;
        cancelled(&cancel)?;
        if scan["complete"] != true {
            blocked = true;
            return Err(Error(
                "incomplete scan; processing and output changes skipped".into(),
            ));
        }
        let mut complete = true;
        if !args["process"].is_null() {
            stage = "processing";
            let analysis = analysis(store, profile, &scan, args, cancel.clone())?;
            complete = analysis["complete"] == true;
            let safe = analysis["safe_to_continue"] == true;
            record(&mut report, stage, analysis, limit, &mut forward)?;
            cancelled(&cancel)?;
            if !safe {
                blocked = true;
                return Err(Error("analysis failed; output changes skipped".into()));
            }
        }
        if args["rules"] == true {
            stage = "rules";
            // No request or fixture flag can authorize publication through this
            // ordinary command while the production M6 boundary is closed.
            if number(args, "render_rules", 0) > 0 {
                return Err(Error(RULE_RENDER_GATE.into()));
            }
            cancelled(&cancel)?;
            let ids = scan["scans"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["scan_id"].clone())
                .collect::<Vec<_>>();
            let rules = crate::rules::dispatch(
                store,
                profile,
                "maintain",
                &json!({"batch":number(args,"rule_batch",100),"render_batch":0,"max_new_bytes":args["rule_max_new_bytes"],"limit":number(args,"limit",100),"scan_ids":ids}),
            )?;
            complete &= rules["complete"] == true;
            let safe = rules["safe_to_continue"] == true;
            record(&mut report, stage, rules, limit, &mut forward)?;
            cancelled(&cancel)?;
            if !safe {
                blocked = true;
                return Err(Error(
                    "processing rules have blockers or failures; output synchronization skipped"
                        .into(),
                ));
            }
        }
        if !catalogs.as_array().unwrap().is_empty() {
            stage = "planning";
            // Refuse before staging desired mappings, so an incomplete native
            // cycle cannot commit a layout it is not qualified to synchronize.
            // Call the public gate, never the private fixture executor.
            let gate = store.transaction(|_| {
                catabolic_store::public_reconcile::call(
                    store,
                    profile,
                    "apply",
                    &json!({"catalog":selected}),
                )
            });
            record(
                &mut report,
                stage,
                json!({"complete":false,"layouts":[],"mappings_committed":false}),
                limit,
                &mut forward,
            )?;
            gate?;
            return Err(Error(
                "native maintenance output synchronization remains unqualified".into(),
            ));
        }
        report["complete"] = json!(complete);
        Ok(())
    })();
    if cancel.load(Ordering::Acquire) {
        return Err(Error(INTERRUPTED.into()));
    }
    if let Err(error) = outcome {
        report["errors"].as_array_mut().unwrap().push(json!({"stage":stage,"message":error.0,"type":progress_error.as_ref().map(|error| error.kind.as_str()).unwrap_or(if blocked {"_Blocked"} else {"CatabolicError"})}));
    }
    report["stopped_at"] = if report["errors"].as_array().unwrap().is_empty() {
        Value::Null
    } else {
        json!(stage)
    };
    report["new_files"] = json!(
        store
            .db
            .query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))?
            - before
    );
    report["summary"] = storage::statistics(store, profile, &catalogs)?;
    if let Some(rules) = report["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["stage"] == "rules")
        .cloned()
    {
        report["summary"]["rules"] =
            json!({"backlog":rules["backlog"],"space_by_device":rules["space_by_device"]});
    }
    let mut warnings = vec![];
    for entry in report["stages"].as_array().unwrap() {
        for warning in entry["warnings"].as_array().into_iter().flatten() {
            if !warnings.contains(warning) {
                warnings.push(warning.clone());
            }
        }
    }
    report["warnings"] = json!(warnings);
    report["summary"]["outputs"] = json!({"checked":false,"healthy":null,"catalogs":[]});
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn record_bounds_details_without_changing_totals_or_progress() {
        let mut report = json!({"stages":[]});
        let result = json!({"complete":false,"actions":[{"kind":"create"},{"kind":"create"},{"kind":"remove"}],"applied":[],"blockers":[{"reason":"one"},{"reason":"two"}]});
        let mut progress = vec![];
        record(&mut report, "remount", result.clone(), 1, &mut |v| {
            progress.push(v.clone());
            Ok(())
        })
        .unwrap();
        let stage = &report["stages"][0];
        assert_eq!(stage["actions_count"], 3);
        assert_eq!(stage["actions_by_kind"], json!({"create":2,"remove":1}));
        assert_eq!(stage["actions_truncated"], true);
        assert_eq!(stage["actions"], json!([{"kind":"create"}]));
        assert_eq!(stage["blockers_count"], 2);
        assert_eq!(stage["blockers_truncated"], true);
        assert_eq!(stage["applied_count"], 0);
        assert_eq!(stage["applied_truncated"], false);
        assert_eq!(
            progress,
            json!([{"stage":"remount","complete":false}])
                .as_array()
                .unwrap()
                .clone()
        );
        assert_eq!(result["actions"].as_array().unwrap().len(), 3);
    }
}
