// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Complete rule-selection planning and atomic requirement/history attachment.
//! Execution belongs to runtime and existing fenced processing owners.
use crate::{
    Error, Result, Store, definitions, estimates, item_workflow, layout,
    migration::rows,
    query::{decode, view_sql},
    selection::Evaluation,
    sql::scalar,
};
use catabolic_core::{decode_json, encode, sha256};
use rusqlite::params;
use serde_json::{Value, json};

/// Durable execution membership, owned by the Store rather than its runtime.
pub fn registered_jobs(store: &Store, rule: &str) -> Result<std::collections::BTreeSet<String>> {
    Ok(store
        .db
        .prepare("SELECT job_id FROM rule_jobs WHERE rule_id=?")?
        .query_map([rule], |row| row.get(0))?
        .collect::<std::result::Result<_, _>>()?)
}
pub fn enabled_rules(store: &Store, profile: &str) -> Result<Vec<Value>> {
    crate::migration::rows(
        &store.db,
        "SELECT id FROM processing_rules WHERE profile=? AND enabled=1 ORDER BY name,id LIMIT 1001",
        &[profile.to_owned().into()],
    )
}
use std::{collections::BTreeSet, os::fd::AsRawFd};
pub const STATES: [&str; 7] = [
    "missing",
    "stale",
    "satisfied",
    "queued",
    "running",
    "failed",
    "deferred",
];
pub fn counts(matches: &[Value]) -> Value {
    let mut v = json!({});
    for s in STATES {
        v[s] = json!(matches.iter().filter(|r| r["state"] == s).count());
    }
    v
}
pub fn integer(value: &Value) -> bool {
    value.is_number() && value.to_string().bytes().all(|c| c.is_ascii_digit())
}
pub fn compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    let a = a.to_string();
    let b = b.to_string();
    a.len().cmp(&b.len()).then_with(|| a.cmp(&b))
}
pub fn add(a: &Value, b: &Value) -> Result<Value> {
    if !integer(a) || !integer(b) {
        return Err(Error("byte budget must be a nonnegative integer".into()));
    }
    let a = a.to_string().bytes().rev().collect::<Vec<_>>();
    let b = b.to_string().bytes().rev().collect::<Vec<_>>();
    let mut out = vec![];
    let mut carry = 0;
    for i in 0..a.len().max(b.len()) {
        let n = a.get(i).copied().unwrap_or(b'0') - b'0' + b.get(i).copied().unwrap_or(b'0') - b'0'
            + carry;
        out.push(b'0' + n % 10);
        carry = n / 10;
    }
    if carry > 0 {
        out.push(b'0' + carry);
    }
    out.reverse();
    Ok(decode_json(std::str::from_utf8(&out).unwrap())?)
}
pub fn subtract(a: &Value, b: &Value) -> Result<Value> {
    if compare(a, b).is_lt() {
        return Ok(json!(0));
    }
    let a = a.to_string().bytes().rev().collect::<Vec<_>>();
    let b = b.to_string().bytes().rev().collect::<Vec<_>>();
    let mut out = vec![];
    let mut borrow = 0i16;
    for (i, digit) in a.iter().enumerate() {
        let n = i16::from(*digit) - i16::from(b.get(i).copied().unwrap_or(b'0')) - borrow;
        borrow = i16::from(n < 0);
        out.push(b'0' + (if n < 0 { n + 10 } else { n }) as u8);
    }
    while out.len() > 1 && out.last() == Some(&b'0') {
        out.pop();
    }
    out.reverse();
    Ok(decode_json(std::str::from_utf8(&out).unwrap())?)
}
pub fn total(matches: &[Value]) -> Result<Value> {
    estimates::total(&json!(
        matches
            .iter()
            .map(|r| r["estimate"].clone())
            .collect::<Vec<_>>()
    ))
}
pub fn bounded(plan: &Value, limit: usize) -> Value {
    let mut result = plan.clone();
    let matches = plan["matches"].as_array().unwrap();
    result["matches"] = json!(&matches[..matches.len().min(limit)]);
    result["details_truncated"] = json!(matches.len() > limit);
    result
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| Error(format!("missing rule {key}")))
}
fn zero_estimate(external: bool) -> Value {
    json!({"expected_bytes":0,"low_bytes":0,"high_bytes":0,"output_limit_bytes":0,"exceeds_output_limit":false,"assumptions":[if external {"No local output allocation. External storage and runtime are unknown; the processor owns destination policy."} else {"Records catalog evidence; no generated file. Catalog database growth and runtime are not estimated."}]})
}
fn empty_row(file: &str, item: &Value, estimate: Value) -> Value {
    json!({"file_id":file,"item_id":item,"job_id":null,"state":"missing","reason":null,"existing_bytes":0,"estimate":estimate})
}
fn in_scope(store: &Store, profile: &str, file: &str, scan_ids: Option<&Value>) -> Result<bool> {
    let Some(scans) = scan_ids else {
        return Ok(true);
    };
    let found = rows(
        &store.db,
        "SELECT scan_id FROM observations WHERE profile=? AND file_id=?",
        &[profile.to_owned().into(), file.to_owned().into()],
    )?;
    Ok(found
        .first()
        .is_some_and(|r| scans.as_array().is_some_and(|a| a.contains(&r["scan_id"]))))
}
fn snapshot_matches(observation: &Value, snapshot: &Value) -> bool {
    snapshot.as_object().is_some_and(|o| {
        o.iter()
            .all(|(k, v)| k == "ctime_ns" || observation[k] == *v)
    })
}
#[allow(clippy::too_many_arguments)]
fn analysis_candidate(
    store: &Store,
    profile: &str,
    rule: &Value,
    operation: &Value,
    config: &Value,
    file: &str,
    item: &Value,
    scan_ids: Option<&Value>,
) -> Result<Value> {
    if rule["required"] == true && item.is_null() {
        return Err(Error("required item work needs an item association".into()));
    }
    let mut row = empty_row(file, item, zero_estimate(false));
    let result: Result<()> = (|| {
        let mut observed = crate::media_mutation::snapshot(store, profile, file)?;
        let live = crate::source::validate(&observed)?;
        observed["ctime_ns"] = live["ctime_ns"].clone();
        let key = sha256(encode(&json!([observed, operation["preset"], config])));
        let jobs = rows(
            &store.db,
            "SELECT id,state FROM processing_jobs WHERE profile=? AND file_id=? AND operation=? AND cache_key=? ORDER BY created_at DESC,rowid DESC LIMIT 1",
            &[
                profile.to_owned().into(),
                file.to_owned().into(),
                scalar(&operation["preset"])?,
                key.into(),
            ],
        )?;
        if let Some(job) = jobs.first() {
            row["job_id"] = job["id"].clone();
            row["state"] = match job["state"].as_str() {
                Some("queued" | "running") => job["state"].clone(),
                Some("complete") => {
                    let fact = layout::fact(store, profile, file, string(operation, "preset")?)?;
                    json!(if fact["current"] == true && fact["snapshot"] == observed {
                        "satisfied"
                    } else {
                        "stale"
                    })
                }
                _ => {
                    row["reason"] =
                        json!("Prior attempt did not succeed; explicit retry is required.");
                    json!("failed")
                }
            };
        }
        if row["state"] != "satisfied" && !in_scope(store, profile, file, scan_ids)? {
            return Err(Error("input is outside this maintenance scan scope".into()));
        }
        Ok(())
    })();
    if let Err(error) = result {
        row["state"] = json!("deferred");
        row["reason"] = json!(error.0);
    }
    Ok(row)
}
fn external_candidate(
    store: &Store,
    profile: &str,
    operation: &Value,
    file: &str,
    item: &Value,
    scan_ids: Option<&Value>,
) -> Result<Value> {
    let mut row = empty_row(file, item, zero_estimate(true));
    let result: Result<()> = (|| {
        let (_, digest) = crate::processor_mutation::prepare(
            store,
            profile,
            string(operation, "preset")?,
            file,
            item.as_str().unwrap_or(""),
            string(&operation["definition"], "output_definition_id")?,
            &operation["definition"]["options"],
            Some(string(operation, "id")?),
        )?;
        let jobs = rows(
            &store.db,
            "SELECT id,state,receipt_id FROM processor_jobs WHERE profile=? AND processor=? AND digest=?",
            &[
                profile.to_owned().into(),
                scalar(&operation["preset"])?,
                digest.into(),
            ],
        )?;
        if let Some(job) = jobs.first() {
            row["job_id"] = job["id"].clone();
            row["state"] = json!(match job["state"].as_str() {
                Some("queued") => "queued",
                Some("leased" | "submitted") => "running",
                Some("complete") => "satisfied",
                _ => "failed",
            });
            if job["state"] == "complete" {
                let outputs = rows(
                    &store.db,
                    &format!(
                        "SELECT r.* FROM ({}) r JOIN receipt_outputs ro ON ro.output_id=r.id WHERE ro.receipt_id=?",
                        view_sql("catalog_renditions")
                    ),
                    &[scalar(&job["receipt_id"])?],
                )?;
                if outputs.is_empty() {
                    return Err(Error(
                        "completed processor job has no accepted outputs".into(),
                    ));
                }
                for output in outputs {
                    crate::publication::ready(store, profile, &output, &[])?;
                }
            }
        }
        if row["state"] != "satisfied" && !in_scope(store, profile, file, scan_ids)? {
            return Err(Error("input is outside this maintenance scan scope".into()));
        }
        Ok(())
    })();
    if let Err(error) = result {
        row["state"] = json!("deferred");
        row["reason"] = json!(error.0);
    }
    Ok(row)
}
#[allow(clippy::too_many_arguments)]
fn render_candidate(
    store: &Store,
    profile: &str,
    rule: &Value,
    recipe: &Value,
    binding: &Value,
    file: &str,
    item: &Value,
    input_error: &impl Fn(&Value, &Value) -> Result<Option<String>>,
) -> Result<Value> {
    let observation = crate::media_mutation::snapshot(store, profile, file)?;
    let fact = layout::fact(store, profile, file, "probe")?;
    let mut probe = if fact["current"] == true {
        fact["data"].clone()
    } else {
        Value::Null
    };
    let mut row = empty_row(
        file,
        item,
        estimates::estimate(
            &recipe["definition"],
            &observation,
            &probe,
            &rule["estimate_options"],
        )?,
    );
    let live: Result<Value> = (|| {
        for output in rows(
            &store.db,
            &format!(
                "SELECT * FROM ({}) WHERE file_id=? AND profile=?",
                view_sql("catalog_renditions")
            ),
            &[file.to_owned().into(), profile.to_owned().into()],
        )? {
            crate::publication::ready(store, profile, &output, &[])?;
        }
        crate::source::validate(&observation)
    })();
    let live = match live {
        Ok(v) => v,
        Err(error) => {
            row["state"] = json!("deferred");
            row["reason"] = json!(error.0);
            return Ok(row);
        }
    };
    if !fact.is_null() && fact["snapshot"]["ctime_ns"] != live["ctime_ns"] {
        probe = Value::Null;
        row["estimate"] = estimates::estimate(
            &recipe["definition"],
            &observation,
            &probe,
            &rule["estimate_options"],
        )?;
    }
    let jobs = rows(
        &store.db,
        "SELECT * FROM processing_jobs WHERE profile=? AND file_id=? AND operation='render' AND recipe_id=? AND json_extract(options,'$.item_id')=? AND json_extract(options,'$.destination.owner')=? ORDER BY created_at DESC,rowid DESC LIMIT 1001",
        &[
            profile.to_owned().into(),
            file.to_owned().into(),
            scalar(&rule["recipe_id"])?,
            scalar(item)?,
            scalar(&rule["location"])?,
        ],
    )?;
    if jobs.len() > 1000 {
        return Err(Error(
            "more than 1000 historical jobs for one rule input; inspect before planning".into(),
        ));
    }
    for mut job in jobs {
        decode(&mut job, &["snapshot", "options"])?;
        if !snapshot_matches(&observation, &job["snapshot"])
            || job["snapshot"]["ctime_ns"] != live["ctime_ns"]
            || job["options"]["destination"] != *binding
        {
            row["state"] = json!("stale");
            continue;
        }
        row["job_id"] = job["id"].clone();
        if matches!(job["state"].as_str(), Some("queued" | "running")) {
            row["state"] = job["state"].clone();
            return Ok(row);
        }
        if job["state"] != "complete" {
            row["state"] = json!("failed");
            row["reason"] =
                json!("Prior job failed or was cancelled; use --retry-failed explicitly.");
            return Ok(row);
        }
        if let Some(artifact) = rows(
            &store.db,
            "SELECT * FROM processing_artifacts WHERE job_id=? AND state='ready'",
            &[scalar(&job["id"])?],
        )?
        .first()
        {
            let current: Result<bool> = (|| {
                let output =
                    crate::media_mutation::snapshot(store, profile, string(artifact, "file_id")?)?;
                let publication = decode_json(
                    artifact["publication_snapshot"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("{}"),
                )?;
                let published = publication.as_object().is_some_and(|o| !o.is_empty())
                    && snapshot_matches(&output, &publication);
                let verified =
                    layout::fact(store, profile, string(artifact, "file_id")?, "verify")?;
                if !published
                    && !(verified["current"] == true
                        && verified["data"]["digest"] == artifact["sha256"])
                {
                    return Ok(false);
                }
                crate::source::validate(&output)?;
                Ok(true)
            })();
            if current.unwrap_or(false) {
                row["state"] = json!("satisfied");
                row["existing_bytes"] = artifact["size"].clone();
                row["artifact_id"] = artifact["id"].clone();
                return Ok(row);
            }
        }
        row["state"] = json!("stale");
        break;
    }
    if probe.is_null() || probe.as_object().is_some_and(|o| o.is_empty()) {
        row["state"] = json!("deferred");
        row["reason"] = json!("Current probe facts are required; enqueue probe work first.");
    } else if let Some(error) = input_error(&recipe["definition"], &probe)? {
        row["state"] = json!("deferred");
        row["reason"] = json!(error);
    }
    Ok(row)
}
/// All selection and candidate database reads share one read-only snapshot.
pub fn plan(
    evaluation: &mut Evaluation,
    id: &str,
    config: &Value,
    scan_ids: Option<&Value>,
    input_error: impl Fn(&Value, &Value) -> Result<Option<String>>,
) -> Result<Value> {
    let profile = evaluation.profile.clone();
    let rule = definitions::rule_get(evaluation.store(), &profile, id)?;
    let operation =
        crate::media_mutation::operation_get(evaluation.store(), string(&rule, "recipe_id")?)?;
    let kind = string(&operation, "operation_kind")?;
    let derived_allowed = rule["allow_derived"] == true || rule["allow_derived"] == 1;
    let mut pairs = BTreeSet::<(String, Option<String>)>::new();
    let mut excluded = 0;
    let selection;
    if kind == "analysis" {
        let selected = evaluation.select(&rule["selection"])?;
        if selected.entity == "file_id" {
            for file in selected.ids {
                if !evaluation
                    .store()
                    .db
                    .prepare("SELECT 1 FROM files WHERE id=?")?
                    .exists([&file])?
                {
                    return Err(Error("selection returned an unknown file".into()));
                }
                let items = rows(
                    &evaluation.store().db,
                    "SELECT DISTINCT item_id FROM item_files WHERE file_id=? AND active=1 ORDER BY item_id",
                    &[file.clone().into()],
                )?;
                if items.is_empty() {
                    pairs.insert((file.clone(), None));
                } else {
                    for item in items {
                        pairs.insert((file.clone(), item["item_id"].as_str().map(str::to_owned)));
                    }
                }
                if pairs.len() > 100000 {
                    return Err(Error("analysis selection exceeds 100000 inputs".into()));
                }
            }
            selection = selected.report;
        } else {
            let (associations, report) = evaluation.associations(&rule["selection"], &[])?;
            for a in associations {
                pairs.insert((
                    string(&a, "file_id")?.into(),
                    Some(string(&a, "item_id")?.into()),
                ));
            }
            selection = report;
        }
    } else {
        let mut admitted = vec![];
        if kind == "render" && derived_allowed {
            let selected = evaluation.select(&rule["selection"])?;
            let column=match selected.entity.as_str(){"association_id"=>"a.id","item_id"=>"a.item_id","file_id"=>"a.file_id",_=>return Err(Error("component selections require package resolution; they are not file associations".into()))};
            let ids = selected.ids.into_iter().collect::<Vec<_>>();
            for chunk in ids.chunks(500) {
                let mut values = chunk
                    .iter()
                    .cloned()
                    .map(Into::into)
                    .collect::<Vec<rusqlite::types::Value>>();
                values.push(profile.clone().into());
                values.push((100001 - admitted.len() as i64).into());
                admitted.extend(rows(&evaluation.store().db,&format!("SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id WHERE {column} IN ({}) AND EXISTS (SELECT 1 FROM media_outputs m WHERE m.file_id=a.file_id AND m.item_id=a.item_id AND m.profile=?) LIMIT ?",vec!["?";chunk.len()].join(",")),&values)?);
                if admitted.len() > 100000 {
                    return Err(Error(
                        "derived selection expands beyond 100000 associations".into(),
                    ));
                }
            }
        }
        let (associations, report) = evaluation.associations(&rule["selection"], &admitted)?;
        selection = report;
        let mut derived = BTreeSet::new();
        if kind == "render" {
            let ids = associations
                .iter()
                .map(|a| string(a, "file_id").map(str::to_owned))
                .collect::<Result<BTreeSet<_>>>()?
                .into_iter()
                .collect::<Vec<_>>();
            for chunk in ids.chunks(500) {
                for row in rows(
                    &evaluation.store().db,
                    &format!(
                        "SELECT f.id FROM files f WHERE f.id IN ({}) AND (EXISTS (SELECT 1 FROM media_outputs m WHERE m.file_id=f.id) OR EXISTS (SELECT 1 FROM generated_locations g WHERE g.location=f.location))",
                        vec!["?"; chunk.len()].join(",")
                    ),
                    &chunk.iter().cloned().map(Into::into).collect::<Vec<_>>(),
                )? {
                    derived.insert(string(&row, "id")?.to_owned());
                }
            }
        }
        for a in &associations {
            let file = string(a, "file_id")?;
            if (a["role"] == "primary"
                || matches!(
                    selection["entity"].as_str(),
                    Some("file_id" | "association_id")
                )
                || derived_allowed && derived.contains(file))
                && (kind != "render" || derived_allowed || !derived.contains(file))
            {
                pairs.insert((file.into(), Some(string(a, "item_id")?.into())));
            }
        }
        excluded = associations.len() - pairs.len();
        if pairs.len() > 100000 {
            return Err(Error(
                "rule expands beyond 100000 inputs; narrow the saved selection".into(),
            ));
        }
    }
    let store = evaluation.store();
    let mut binding = Value::Null;
    let mut available = Value::Null;
    if kind == "render" {
        binding =
            crate::observation::binding(store, &profile, "source", string(&rule, "location")?)?;
        let root = crate::artifact_mutation::owned_root(store, &profile, &binding)?;
        // SAFETY: zero is a valid initial representation of this C output struct.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: the descriptor is live and the initialized buffer is writable.
        if unsafe { libc::fstatvfs(root.as_raw_fd(), &mut stat) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        available =
            decode_json(&(u128::from(stat.f_bavail) * u128::from(stat.f_frsize)).to_string())?;
    }
    let measured = if kind == "render" {
        estimates::calibration(store, &profile, &operation, &rule["estimate_options"])?
    } else {
        Value::Null
    };
    let mut matches = vec![];
    for (file, item) in pairs {
        let item = json!(item);
        let mut row = match kind {
            "render" => render_candidate(
                store,
                &profile,
                &rule,
                &operation,
                &binding,
                &file,
                &item,
                &input_error,
            )?,
            "analysis" => analysis_candidate(
                store, &profile, &rule, &operation, config, &file, &item, scan_ids,
            )?,
            "external" => external_candidate(store, &profile, &operation, &file, &item, scan_ids)?,
            _ => return Err(Error("unknown operation kind".into())),
        };
        if kind == "render" {
            row["estimate"] = estimates::calibrated(&row["estimate"], &measured)?;
            if row["state"] != "satisfied" && !in_scope(store, &profile, &file, scan_ids)? {
                row["state"] = json!("deferred");
                row["reason"] = json!("Input was outside the successful maintenance scan scope.");
            }
        }
        matches.push(row);
    }
    let outstanding = matches
        .iter()
        .filter(|r| kind == "external" || r["state"] != "satisfied")
        .cloned()
        .collect::<Vec<_>>();
    let mut space = total(&outstanding)?;
    let mut existing = json!(0);
    for row in &matches {
        existing = add(&existing, &row["existing_bytes"])?;
    }
    let reserve = if kind == "render" {
        operation["definition"]["reserve_bytes"].clone()
    } else {
        json!(0)
    };
    space["already_satisfied_bytes"] = existing;
    space["available_bytes"] = available.clone();
    space["destination"] = binding["root"].clone();
    space["device"] = binding["device"].clone();
    space["reserve_bytes"] = reserve.clone();
    space["fits_planning_estimate"] = if kind == "external" {
        Value::Null
    } else {
        json!(
            available.is_null()
                || !compare(&available, &add(&space["planning_bytes"], &reserve)?).is_lt()
        )
    };
    space["note"] = if kind == "render" {
        json!(
            "Additional logical storage, retaining originals and old outputs. Unknown sizes use recipe limits for planning. Ranges are heuristics, not guaranteed bounds; no sample encode or full-file read was performed."
        )
    } else {
        zero_estimate(kind == "external")["assumptions"][0].clone()
    };
    let mut result = json!({"rule":rule,"selection":selection,"complete":true,"matched_inputs":matches.len(),"excluded_associations":excluded,"counts":counts(&matches),"space":space,"matches":matches,"calibration":measured});
    if kind == "external" {
        result["external_storage_bytes"] = Value::Null;
    }
    Ok(result)
}
pub fn record_evaluation(store: &Store, profile: &str, plan: &Value) -> Result<()> {
    let mut reasons: Vec<(Value, usize)> = vec![];
    for row in plan["matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["state"] == "deferred")
    {
        let reason = row.get("reason").cloned().unwrap_or(json!("unspecified"));
        if let Some((_, count)) = reasons.iter_mut().find(|(r, _)| *r == reason) {
            *count += 1;
        } else {
            reasons.push((reason, 1));
        }
    }
    reasons.sort_by_key(|a| std::cmp::Reverse(a.1));
    let value = json!({"evaluated_at":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros,false),"scope":"last explicit rule evaluation; full evaluated rule selection at evaluated_at, not live satisfaction","matched_inputs":plan["matched_inputs"],"counts":plan["counts"],"deferred_reasons":reasons.iter().take(100).map(|(r,n)|json!({"reason":r,"count":n})).collect::<Vec<_>>(),"deferred_reasons_truncated":reasons.len()>100});
    let key = format!(
        "statistics:{}",
        encode(&json!([profile, "rule", plan["rule"]["id"]]))
    );
    store.transaction(|db|{db.execute("INSERT INTO meta(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,encode(&value)])?;Ok(())})
}
pub fn requirements(store: &Store, profile: &str, plan: &Value) -> Result<()> {
    record_evaluation(store, profile, plan)?;
    let rule = &plan["rule"];
    if rule["required"] != true {
        return Ok(());
    }
    let evaluation = uuid::Uuid::new_v4().to_string();
    store.transaction(|db|{db.execute("INSERT INTO rule_evaluations(id,rule_id,matched_inputs,counts) VALUES (?,?,?,?)",params![evaluation,string(rule,"id")?,scalar(&plan["matched_inputs"])?,encode(&plan["counts"])])?;
        for row in plan["matches"].as_array().unwrap(){let old=rows(db,"SELECT id FROM rule_requirements WHERE rule_id=? AND file_id=? AND item_id=?",&[scalar(&rule["id"])?,scalar(&row["file_id"])?,scalar(&row["item_id"])?])?;if let Some(old)=old.first(){db.execute("UPDATE rule_requirements SET evaluation_id=? WHERE id=?",params![evaluation,scalar(&old["id"])?])?;continue;}
            let id=uuid::Uuid::new_v4().to_string();db.execute("INSERT INTO rule_requirements(id,rule_id,profile,file_id,item_id,evaluation_id) VALUES (?,?,?,?,?,?)",params![id,string(rule,"id")?,profile,string(row,"file_id")?,string(row,"item_id")?,evaluation])?;
            let (status,revision)=item_workflow::revision(store,db,string(row,"item_id")?,&Value::Null)?;
            item_workflow::append(db,profile,string(row,"item_id")?,"requirement",&json!("A current operation result is required, including work not yet queued."),&json!(format!("rule:{}",string(rule,"id")?)),&json!({"requirement_id":id,"rule_id":rule["id"],"file_id":row["file_id"]}),&status,revision)?;
        }Ok(())})
}
pub fn attach(
    store: &Store,
    profile: &str,
    rule: &Value,
    row: &Value,
    job: &str,
    external: bool,
) -> Result<()> {
    let table = if external {
        "rule_processor_jobs"
    } else {
        "rule_jobs"
    };
    let field = if external {
        "processor_job_id"
    } else {
        "job_id"
    };
    store.transaction(|db|{db.execute(&format!("INSERT INTO {table}(rule_id,job_id,file_id,item_id) VALUES (?,?,?,?) ON CONFLICT DO NOTHING"),params![string(rule,"id")?,job,string(row,"file_id")?,scalar(&row["item_id"])?])?;
        if rule["required"]==true {let old=rows(db,&format!("SELECT id,{field} FROM rule_requirements WHERE rule_id=? AND file_id=? AND item_id=?"),&[scalar(&rule["id"])?,scalar(&row["file_id"])?,scalar(&row["item_id"])?])?;if let Some(old)=old.first()&&old[field]!=job {db.execute(&format!("UPDATE rule_requirements SET {field}=? WHERE id=?"),params![job,scalar(&old["id"])?])?;let(status,revision)=item_workflow::revision(store,db,string(row,"item_id")?,&Value::Null)?;let previous=format!("previous_{field}");item_workflow::append(db,profile,string(row,"item_id")?,"requirement",&json!(if external{"Rule requirement now tracks a fenced external job."}else{"Rule requirement now tracks this job; prior attempts remain in history."}),&json!(format!("rule:{}",string(rule,"id")?)),&json!({"requirement_id":old["id"],previous:old[field],field:job}),&status,revision)?;}}
        Ok(())})
}
pub fn statistics(store: &Store, profile: &str, id: &str) -> Result<Value> {
    let rule = definitions::rule_get(store, profile, id)?;
    let operation = crate::media_mutation::operation_get(store, string(&rule, "recipe_id")?)?;
    let external = operation["operation_kind"] == "external";
    let attempts = rows(
        &store.db,
        if external {
            "SELECT a.state,count(*) AS count,sum(CASE WHEN a.finished_at IS NOT NULL THEN max(0,a.finished_at-a.started_at) END) AS lease_elapsed_seconds FROM rule_processor_jobs r JOIN processor_attempts a ON a.job_id=r.job_id WHERE r.rule_id=? GROUP BY a.state ORDER BY a.state"
        } else {
            "SELECT p.state,count(*) AS count,sum(a.size) AS output_bytes,sum(CASE WHEN p.started_at IS NOT NULL AND p.finished_at IS NOT NULL THEN max(0,(julianday(p.finished_at)-julianday(p.started_at))*86400) END) AS elapsed_seconds FROM rule_jobs r JOIN processing_attempts p ON p.job_id=r.job_id LEFT JOIN processing_artifacts a ON a.job_id=p.job_id AND a.attempt=p.attempt WHERE r.rule_id=? GROUP BY p.state ORDER BY p.state"
        },
        &[id.to_owned().into()],
    )?;
    Ok(
        json!({"rule_id":id,"calibration":if operation["operation_kind"]=="render"{estimates::calibration(store,profile,&operation,&rule["estimate_options"])?}else{Value::Null},"attempts":attempts,"note":if external{"Historical external lease attempts; lease duration is not measured processor runtime. Output storage and runtime remain unknown."}else{"Historical attempt totals, including retained old outputs. Elapsed times are recorded wall time; no savings or future ETA is inferred."}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budgets_preserve_unbounded_integer_arithmetic() {
        let enormous = decode_json("100000000000000000000000000000000000000000000000000").unwrap();
        let incremented = add(&enormous, &json!(123)).unwrap();
        assert!(compare(&incremented, &enormous).is_gt());
        assert_eq!(subtract(&incremented, &enormous).unwrap(), json!(123));
        assert_eq!(subtract(&enormous, &incremented).unwrap(), json!(0));
        assert_eq!(
            subtract(&enormous, &json!(1)).unwrap().to_string(),
            "99999999999999999999999999999999999999999999999999"
        );
        assert!(!integer(&json!(true)));
        assert!(!integer(&json!(1.5)));
        assert!(add(&json!(-1), &json!(0)).is_err());
    }
}
