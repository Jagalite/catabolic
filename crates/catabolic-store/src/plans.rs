// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Typed plan descriptions and semantic comparison. Fallback evaluation receives
//! its caller's bounded session and detached probe owner explicitly.
use crate::{Error, Result, Store, definitions, migration::rows, selection::Evaluation};
use catabolic_core::{encode, sha256};
use serde_json::{Value, json};
use std::collections::BTreeSet;
fn fail<T>(message: &str) -> Result<T> {
    Err(Error(message.into()))
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| Error(format!("invalid plan: {key} must be a string")))
}
pub fn normalize(reference: &Value) -> Result<Value> {
    let kind = string(reference, "kind")?;
    let fields: &[&str] = match kind {
        "query" => &["kind", "query_id"],
        "fallback" => &[
            "kind",
            "membership_query_id",
            "policy_id",
            "role",
            "part",
            "variant",
        ],
        "projection" => &["kind", "catalog", "binding_digest"],
        "observation" => &["kind", "sources", "exclusions"],
        _ => return fail("invalid plan kind"),
    };
    if reference
        .as_object()
        .is_none_or(|o| o.keys().any(|k| !fields.contains(&k.as_str())))
    {
        return fail("invalid plan: extra fields are not permitted");
    }
    let mut value = reference.clone();
    match kind {
        "query" => {
            string(reference, "query_id")?;
        }
        "projection" => {
            string(reference, "catalog")?;
            string(reference, "binding_digest")?;
        }
        "fallback" => {
            string(reference, "membership_query_id")?;
            string(reference, "policy_id")?;
            for (key, default) in [
                ("role", json!("primary")),
                ("part", Value::Null),
                ("variant", json!("")),
            ] {
                if value.get(key).is_none() {
                    value[key] = default;
                }
            }
            string(&value, "role")?;
            string(&value, "variant")?;
            if !value["part"].is_null()
                && (!value["part"].is_number()
                    || value["part"].to_string().contains(['.', 'e', 'E']))
            {
                return fail("invalid plan: part must be an integer or null");
            }
        }
        "observation" => {
            if value.get("exclusions").is_none() {
                value["exclusions"] = json!([]);
            }
            for key in ["sources", "exclusions"] {
                let list = value[key]
                    .as_array()
                    .filter(|a| a.len() <= 1000 && (key != "sources" || !a.is_empty()))
                    .ok_or_else(|| Error(format!("invalid plan: {key} must be a bounded list")))?;
                let mut normalized = BTreeSet::new();
                for element in list {
                    let text = element.as_str().ok_or_else(|| {
                        Error(format!("invalid plan: {key} must contain strings"))
                    })?;
                    if key == "sources" {
                        crate::query::name(text)?;
                    } else {
                        crate::layout::relative_path(text)?;
                    }
                    normalized.insert(text.to_owned());
                }
                value[key] = json!(normalized);
            }
        }
        _ => unreachable!(),
    }
    Ok(value)
}
fn projection_binding(store: &Store, profile: &str, catalog: &str) -> Result<Value> {
    rows(
        &store.db,
        "SELECT * FROM fallback_bindings WHERE profile=? AND catalog=?",
        &[profile.to_owned().into(), catalog.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("watcher projection requires an authoritative fallback binding".into()))
}
pub fn projection_digest(store: &Store, profile: &str, catalog: &str) -> Result<String> {
    let config = projection_binding(store, profile, catalog)?;
    Ok(sha256(encode(
        &json!({"query_id":config["query_id"],"policy_id":config["policy_id"],"layout":definitions::layout_get(store,string(&config,"layout")?)?,"output":rows(&store.db,"SELECT * FROM bindings WHERE profile=? AND kind='output' AND owner=?",&[profile.to_owned().into(),catalog.to_owned().into()])?}),
    )))
}
pub fn describe(store: &Store, profile: &str, reference: &Value) -> Result<Value> {
    let plan = normalize(reference)?;
    let kind = string(&plan, "kind")?;
    if kind == "observation" {
        for source in plan["sources"].as_array().unwrap() {
            crate::observation::binding(store, profile, "source", source.as_str().unwrap())?;
        }
        return Ok(
            json!({"plan":plan,"queries":[],"policies":[],"coverage":"exact_full_sources","sources":plan["sources"],"exclusions":plan["exclusions"],"dependency_precision":"exact"}),
        );
    }
    let mut queries = BTreeSet::new();
    let mut policies = BTreeSet::new();
    let mut pending = vec![];
    if kind == "query" {
        pending.push(string(&plan, "query_id")?.to_owned());
    } else {
        let (membership, policy) = if kind == "projection" {
            let catalog = string(&plan, "catalog")?;
            if projection_digest(store, profile, catalog)? != string(&plan, "binding_digest")? {
                return fail("stale_watcher_projection_binding");
            }
            let config = projection_binding(store, profile, catalog)?;
            (
                string(&config, "query_id")?.to_owned(),
                string(&config, "policy_id")?.to_owned(),
            )
        } else {
            (
                string(&plan, "membership_query_id")?.to_owned(),
                string(&plan, "policy_id")?.to_owned(),
            )
        };
        if definitions::query_get(store, profile, &membership)?["definition"]["mode"] != "selection"
        {
            return fail("logical plan requires typed membership");
        }
        pending.push(membership);
        policies.insert(policy.clone());
        let definition =
            crate::fallback_mutation::get(store, profile, &policy)?["definition"].clone();
        for tier in definition["fallbacks"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            pending.push(string(tier, "query_id")?.to_owned());
        }
        if let Some(requirements) = definition["package"]["requirements"].as_array() {
            for requirement in requirements {
                for tier in requirement["fallbacks"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                {
                    pending.push(string(tier, "query_id")?.to_owned());
                }
            }
        }
    }
    while let Some(reference) = pending.pop() {
        if queries.contains(&reference) {
            continue;
        }
        if queries.len() >= 64 {
            return fail("plan_dependency_budget");
        }
        queries.insert(reference.clone());
        let definition = definitions::query_get(store, profile, &reference)?["definition"].clone();
        for query in definition["queries"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            pending.push(query.as_str().unwrap().to_owned());
        }
    }
    Ok(
        json!({"plan":plan,"queries":queries,"policies":policies,"coverage":"all_bound_profile_sources","dependency_precision":"conservative","catalog_facts":["all"],"periodic_reevaluation_required":true}),
    )
}
pub fn observation_requirements(store: &Store, profile: &str, reference: &Value) -> Result<Value> {
    let description = describe(store, profile, reference)?;
    if reference["kind"] == "observation" {
        return Ok(json!(description["sources"].as_array().unwrap().iter().map(|source|json!({"source":source,"kind":"full_inventory","scope":"","exclusions":description["exclusions"]})).collect::<Vec<_>>()));
    }
    Ok(json!(
        rows(
            &store.db,
            "SELECT owner FROM bindings WHERE profile=? AND kind='source' ORDER BY owner",
            &[profile.to_owned().into()]
        )?
        .iter()
        .map(|r| json!({"source":r["owner"],"kind":"full_inventory","scope":"","exclusions":[]}))
        .collect::<Vec<_>>()
    ))
}
fn component_evidence(
    evaluation: &mut Evaluation,
    entity: &str,
    ids: &BTreeSet<String>,
) -> Result<Value> {
    let records = {
        let mut statement=evaluation.store().db.prepare(&format!("SELECT * FROM ({}) WHERE profile=? AND {entity} IN (SELECT value FROM json_each(?)) ORDER BY occurrence_id LIMIT 100001",crate::query::view_sql("catalog_component_occurrences")))?;
        let names = statement
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let mut cursor = statement.query([&evaluation.profile, &encode(&json!(ids))])?;
        let mut records = vec![];
        let mut bytes = 0;
        while let Some(row) = cursor.next()? {
            let mut record = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                record.insert(name.clone(), crate::migration::value(row.get_ref(i)?));
            }
            let value = Value::Object(record);
            bytes += encode(&value).len();
            if bytes > 4 * 1024 * 1024 {
                return fail("evaluation_result_byte_budget");
            }
            records.push(value);
            if records.len() > 100000 {
                return fail("component_selection_expansion_budget");
            }
        }
        json!(records)
    };
    evaluation.account_value(&records)?;
    Ok(records)
}
/// Evaluate supported plans within the caller's snapshot and cumulative budget.
/// The native fallback owner must be connected before non-query plans can run.
pub fn evaluate(evaluation: &mut Evaluation, reference: &Value) -> Result<Value> {
    if evaluation.http {
        return fail("plan orchestration currently requires local-owner authority");
    }
    if reference["kind"] == "observation" {
        return fail("observation plans execute through the observation service");
    }
    let description = describe(evaluation.store(), &evaluation.profile, reference)?;
    let plan = normalize(reference)?;
    if plan["kind"] != "query" {
        return fail("native fallback/projection plan evaluation is not yet supported");
    }
    let query = definitions::query_get(
        evaluation.store(),
        &evaluation.profile,
        string(&plan, "query_id")?,
    )?["definition"]
        .clone();
    if query["mode"] == "selection" {
        let selected = evaluation
            .select(&json!({"query_id":plan["query_id"],"profile":evaluation.profile}))?;
        let mut value = json!({"entity":selected.entity,"ids":selected.ids});
        if ["component_id", "occurrence_id"].contains(&selected.entity.as_str()) {
            value["component_evidence"] =
                component_evidence(evaluation, &selected.entity, &selected.ids)?;
        }
        return Ok(
            json!({"kind":"selection","complete":true,"value":value,"description":description}),
        );
    }
    let value = evaluation.run(string(&plan, "query_id")?, 1000)?;
    let complete = value
        .get("complete")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| value["errors"].as_array().is_none_or(|a| a.is_empty()))
        && value["truncated"] != true;
    Ok(json!({"kind":query["mode"],"complete":complete,"value":value,"description":description}))
}

/// Local-owner fallback evaluation preserves one shared selection/resolver
/// session and the previous semantic baseline for failback hysteresis.
pub fn evaluate_fallback(
    evaluation: &mut Evaluation,
    reference: &Value,
    previous: Option<&Value>,
    context: &str,
    probe: &mut crate::fallback_resolution::Probe<'_>,
) -> Result<Value> {
    if evaluation.http {
        return fail("plan orchestration currently requires local-owner authority");
    }
    let description = describe(evaluation.store(), &evaluation.profile, reference)?;
    let plan = normalize(reference)?;
    if plan["kind"] != "fallback" {
        return fail("fallback plan required");
    }
    let selected = evaluation
        .select(&json!({"query_id":plan["membership_query_id"],"profile":evaluation.profile}))?;
    if selected.entity != "item_id" {
        return fail("fallback membership requires item IDs");
    }
    if selected.report["complete"] != true {
        return fail("incomplete_evaluation_has_no_baseline");
    }
    let entries: Vec<_> = selected.ids.iter().map(|id|json!({"item_id":id,"role":plan["role"],"part":plan["part"],"variant":plan["variant"]})).collect();
    let previous = previous
        .map(|p| &p["value"])
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|d| d["policy_id"] == plan["policy_id"])
        .map(|d| {
            let mut value = d.clone();
            value["tier"] = d["selected_tier"].clone();
            value["evidence"] = json!(encode(&d["evidence"]));
            Ok((string(d, "entry_id")?.to_owned(), value))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
    let value = if entries.is_empty() {
        json!({"decisions":[],"complete":true})
    } else {
        crate::fallback_resolution::Resolver::new(
            crate::fallback_resolution::Context::Local(evaluation),
            probe,
            None,
        )
        .resolve(
            &entries,
            string(&plan, "policy_id")?,
            context,
            &previous,
            false,
        )?
    };
    let complete = value["decisions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .all(|d| d["state"] != "blocked");
    Ok(json!({"kind":"fallback","complete":complete,"value":value,"description":description}))
}
fn plan_semantic_error(error: catabolic_core::plan_semantics::Error) -> Error {
    match error {
        catabolic_core::plan_semantics::Error::InvalidKind => {
            Error("invalid plan: kind must be a string".into())
        }
        catabolic_core::plan_semantics::Error::Incomplete => {
            Error("incomplete_evaluation_has_no_baseline".into())
        }
    }
}
pub fn semantic(result: &Value) -> Result<Value> {
    catabolic_core::plan_semantics::semantic(result).map_err(plan_semantic_error)
}
pub fn compare(previous: &Value, current: &Value) -> Result<Value> {
    catabolic_core::plan_semantics::compare(previous, current).map_err(plan_semantic_error)
}
pub fn dispatch(evaluation: &mut Evaluation, action: &str, args: &Value) -> Result<Value> {
    match action {
        "normalize" => normalize(&args["reference"]),
        "describe" => describe(evaluation.store(), &evaluation.profile, &args["reference"]),
        "requirements" => {
            observation_requirements(evaluation.store(), &evaluation.profile, &args["reference"])
        }
        "projection-digest" => Ok(json!(projection_digest(
            evaluation.store(),
            &evaluation.profile,
            string(args, "catalog")?
        )?)),
        "evaluate" => evaluate(evaluation, &args["reference"]),
        "semantic" => semantic(&args["result"]),
        "compare" => compare(&args["previous"], &args["current"]),
        _ => Err(Error(format!("unknown plan action: {action}"))),
    }
}
