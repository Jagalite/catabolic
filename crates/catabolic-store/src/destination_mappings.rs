// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Ordinary v1 destination mapping decisions and durable publication records.
//! No external effects occur here. The runtime owns the complete operation.
use crate::{Error, Result, Store, consumers, selection::Evaluation};
use catabolic_core::{encode, sha256};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

// The frozen snapshot contract compares Python JSON values: booleans and
// numerically equal integers/floats compare equal, including inside DTOs.
// Do not round a large integer through f64 when comparing it with a float.
pub fn equal(left: &Value, right: &Value) -> bool {
    let numeric = |v: &Value| matches!(v, Value::Number(_) | Value::Bool(_));
    if numeric(left) && numeric(right) {
        let whole = |v: &Value| -> Option<String> {
            match v {
                Value::Bool(b) => Some(if *b { "1" } else { "0" }.into()),
                Value::Number(n) if !n.to_string().contains(['.', 'e', 'E']) => Some(n.to_string()),
                Value::Number(n) => n
                    .as_f64()
                    .filter(|f| f.is_finite() && f.fract() == 0.0)
                    .map(|f| format!("{f:.0}")),
                _ => None,
            }
        };
        if let (Some(l), Some(r)) = (whole(left), whole(right)) {
            let l = if l == "-0" { "0" } else { &l };
            let r = if r == "-0" { "0" } else { &r };
            return l == r;
        }
        return left
            .as_f64()
            .zip(right.as_f64())
            .is_some_and(|(l, r)| l == r);
    }
    match (left, right) {
        (Value::Array(l), Value::Array(r)) => {
            l.len() == r.len() && l.iter().zip(r).all(|(l, r)| equal(l, r))
        }
        (Value::Object(l), Value::Object(r)) => {
            l.len() == r.len() && l.iter().all(|(k, l)| r.get(k).is_some_and(|r| equal(l, r)))
        }
        _ => left == right,
    }
}

/// Python permits arbitrary-size nonnegative integer removal budgets. Any
/// overflow already exceeds the bounded plan, so saturating preserves comparison.
pub fn removal_budget(value: &Value) -> Result<usize> {
    let Value::Number(number) = value else {
        return fail("invalid_removal_budget");
    };
    let text = number.to_string();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return fail("invalid_removal_budget");
    }
    Ok(text.parse().unwrap_or(usize::MAX))
}

pub type Capabilities = BTreeMap<String, bool>;
pub type Ledger = BTreeMap<(String, String), Value>;
pub fn error(code: &str) -> Error {
    Error(format!("mapping_{code}"))
}
fn fail<T>(code: &str) -> Result<T> {
    Err(error(code))
}
fn object<'a>(value: &'a Value, code: &str) -> Result<&'a serde_json::Map<String, Value>> {
    value.as_object().ok_or_else(|| error(code))
}
fn keys(value: &serde_json::Map<String, Value>, expected: &[&str]) -> bool {
    value.len() == expected.len() && expected.iter().all(|key| value.contains_key(*key))
}
fn text(value: &Value, maximum: usize) -> Result<String> {
    consumers::text(value, maximum).map(str::to_owned)
}
fn checkpoint() -> Result<()> {
    if crate::embedding::poll_interrupt() {
        Err(Error("destination mapping interrupted".into()))
    } else {
        Ok(())
    }
}
/// This independent read snapshot ends before any destination request.
pub fn evaluate(
    path: &Path,
    profile: &str,
    definition: &Value,
    capabilities: &Capabilities,
    validate_value: &dyn Fn(&str, &Value) -> Result<Value>,
) -> Result<Value> {
    checkpoint()?;
    let def = object(definition, "invalid_definition")?;
    if !keys(
        def,
        &[
            "version",
            "id",
            "query",
            "destination",
            "key",
            "target",
            "fields",
        ],
    ) || definition["version"].as_i64() != Some(1)
    {
        return fail("invalid_definition");
    }
    text(&definition["id"], 255)?;
    let dest = object(&definition["destination"], "invalid_destination")?;
    if dest
        .keys()
        .any(|key| !["connection", "library", "collections"].contains(&key.as_str()))
        || !["connection", "library"]
            .iter()
            .all(|key| dest.contains_key(*key))
    {
        return fail("invalid_destination");
    }
    for key in ["connection", "library"] {
        text(&definition["destination"][key], 255)?;
    }
    let target = object(&definition["target"], "target_requires_id_and_path")?;
    if !keys(target, &["id", "path"]) {
        return fail("target_requires_id_and_path");
    }
    let fields = object(&definition["fields"], "unsupported_fields")?;
    if fields.is_empty() || fields.keys().any(|field| !capabilities.contains_key(field)) {
        return fail("unsupported_fields");
    }
    let store = Store::open(path, false, false)?;
    let database_id = store.database_id.clone();
    let mut evaluation = Evaluation::new(store, profile, 60000, 100000, false)?;
    let query = definition["query"]
        .as_str()
        .ok_or_else(|| Error("unknown query revision in this profile".into()))?;
    if evaluation.get(query)?["definition"]["mode"] != "rows" {
        return fail("requires_rows_query");
    }
    let result = evaluation.run(query, 1000)?;
    if result["complete"] != true || result["truncated"] != false {
        return fail("incomplete_query");
    }
    let columns = result["columns"]
        .as_array()
        .ok_or_else(|| error("incomplete_query"))?;
    let columns = columns
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| error("incomplete_query"))
        })
        .collect::<Result<Vec<_>>>()?;
    if columns.iter().collect::<BTreeSet<_>>().len() != columns.len() {
        return fail("duplicate_columns");
    }
    let mut grouped = BTreeMap::<String, Value>::new();
    let mut targets = BTreeMap::<String, String>::new();
    for values in result["rows"]
        .as_array()
        .ok_or_else(|| error("incomplete_query"))?
    {
        checkpoint()?;
        let values = values
            .as_array()
            .filter(|v| v.len() == columns.len())
            .ok_or_else(|| error("incomplete_query"))?;
        let row = Value::Object(
            columns
                .iter()
                .cloned()
                .zip(values.iter().cloned())
                .collect(),
        );
        let key = text(
            &crate::http_mappings::expression(&definition["key"], &row)?,
            255,
        )?;
        let identity = Value::Object(
            target
                .iter()
                .map(|(k, v)| {
                    Ok((
                        k.clone(),
                        json!(text(&crate::http_mappings::expression(v, &row)?, 4096)?),
                    ))
                })
                .collect::<Result<_>>()?,
        );
        let id = identity["id"].as_str().unwrap().to_owned();
        if targets.get(&id).is_some_and(|old| old != &key) {
            return fail("duplicate_target");
        }
        targets.insert(id, key.clone());
        let entry = grouped
            .entry(key.clone())
            .or_insert_with(|| json!({"key":key,"target":identity,"values":{}}));
        if entry["target"] != identity {
            return fail("key_has_multiple_targets");
        }
        for (field, spec) in fields {
            let value = crate::http_mappings::expression(spec, &row)?;
            if capabilities[field] {
                let values = if value.is_string() {
                    vec![value]
                } else {
                    value
                        .as_array()
                        .filter(|v| v.len() <= 100)
                        .ok_or_else(|| error("invalid_set"))?
                        .clone()
                };
                let mut set = values
                    .iter()
                    .map(|v| text(v, 255))
                    .collect::<Result<BTreeSet<_>>>()?;
                if let Some(old) = entry["values"][field].as_array() {
                    set.extend(old.iter().map(|v| v.as_str().unwrap().to_owned()));
                }
                if set.len() > 100 {
                    return fail("set_too_large");
                }
                entry["values"][field] = json!(set);
            } else {
                let value = validate_value(field, &value)?;
                if entry["values"].get(field).is_some_and(|old| old != &value) {
                    return fail("conflicting_scalar_rows");
                }
                entry["values"][field] = value;
            }
        }
    }
    if grouped.len() > 100 {
        return fail("too_many_targets");
    }
    Ok(json!({"database_id":database_id,"rows":grouped.into_values().collect::<Vec<_>>()}))
}

pub fn ownership(history: &[Value], scope: &Value) -> Result<Ledger> {
    let mut ledger = Ledger::new();
    for event in history {
        checkpoint()?;
        let event_scope = event
            .get("scope")
            .ok_or_else(|| Error("mapping history event is missing scope".into()))?;
        if event_scope != scope {
            continue;
        }
        if !matches!(event["state"].as_str(), Some("verified" | "not_applied")) {
            return fail(&format!(
                "unresolved_write_recover_{}",
                event["id"].as_str().unwrap_or("")
            ));
        }
        if event["state"] == "verified" {
            for (field, value) in object(&event["owned"], "invalid_ownership_event")? {
                let mut owned = object(value, "invalid_ownership_event")?.clone();
                owned.insert("owner".into(), event["owner"].clone());
                owned.insert("key".into(), event["key"].clone());
                owned.insert("identity".into(), event["before"]["identity"].clone());
                ledger.insert(
                    (text(&event["target"]["id"], 4096)?, field.clone()),
                    Value::Object(owned),
                );
            }
        }
    }
    Ok(ledger)
}
fn string_set(value: &Value) -> Result<BTreeSet<String>> {
    value
        .as_array()
        .ok_or_else(|| error("invalid_ownership_event"))?
        .iter()
        .map(|v| text(v, 255))
        .collect()
}
/// The production owned-merge/scalar conflict reducer is also exercised directly.
pub fn planned_row(
    definition: &Value,
    item: &Value,
    before: Value,
    ledger: &Ledger,
    capabilities: &Capabilities,
) -> Result<(Value, usize)> {
    let mut after = serde_json::Map::new();
    let mut owned = serde_json::Map::new();
    let mut removals = 0;
    for (field, wanted) in object(&item["values"], "invalid_row")? {
        let current = &before["values"][field];
        let previous = ledger.get(&(text(&item["target"]["id"], 4096)?, field.clone()));
        if previous.is_some_and(|p| p["owner"] != definition["id"] || p["key"] != item["key"]) {
            return fail("field_owned_by_other_mapping");
        }
        if previous.is_some_and(|p| !equal(&p["identity"], &before["identity"])) {
            return fail("owned_target_identity_changed");
        }
        let (last, contribution) = if capabilities[field] {
            let old = previous
                .map(|p| string_set(&p["contribution"]))
                .transpose()?
                .unwrap_or_default();
            let current = string_set(current)?;
            if !old.is_subset(&current) {
                return fail("owned_values_changed_remotely");
            }
            let manual = current.difference(&old).cloned().collect::<BTreeSet<_>>();
            let wanted = string_set(wanted)?;
            let last = manual.union(&wanted).cloned().collect::<BTreeSet<_>>();
            let contribution = wanted.difference(&manual).cloned().collect::<BTreeSet<_>>();
            removals += current.difference(&last).count();
            (json!(last), json!(contribution))
        } else {
            if previous.is_some_and(|p| !equal(current, &p["last"])) {
                return fail("owned_value_changed_remotely");
            }
            (wanted.clone(), wanted.clone())
        };
        after.insert(field.clone(), last.clone());
        owned.insert(
            field.clone(),
            json!({"last":last,"contribution":contribution}),
        );
    }
    let mut row = object(item, "invalid_row")?.clone();
    row.insert("before".into(), before);
    row.insert("after".into(), Value::Object(after));
    row.insert("owned".into(), Value::Object(owned));
    Ok((Value::Object(row), removals))
}

pub fn plan(
    definition: &Value,
    local: &Value,
    connection: &Value,
    library: &Value,
    rows: Vec<Value>,
    removals: usize,
    ledger: &Ledger,
) -> Result<Value> {
    let scope = json!([
        connection["application"],
        connection["server_id"],
        library["uuid"]
    ]);
    let ownership = ledger
        .iter()
        .map(|((id, field), v)| json!([[id, field], v]))
        .collect::<Vec<_>>();
    let plan = json!({"definition":definition,"local":local,"connection":connection,"library":library,"scope":scope,"rows":rows,"removals":removals,"ownership":ownership});
    if encode(&plan).len() > 4 * 1024 * 1024 {
        return fail("plan_too_large");
    }
    Ok(plan)
}
pub fn review(plan: &Value, apply: bool, expected: Option<&str>, maximum: usize) -> Result<Value> {
    let id = sha256(encode(plan));
    if apply && expected != Some(&id) {
        return fail("stale_plan_preview_again");
    }
    if apply && plan["removals"].as_u64().unwrap_or(0) > maximum as u64 {
        return fail("removal_budget_exceeded");
    }
    let rows = plan["rows"].as_array().unwrap().iter().map(|r|json!({"key":r["key"],"target":r["target"],"before":r["before"],"after":r["after"]})).collect::<Vec<_>>();
    Ok(
        json!({"plan_id":id,"mapping":plan["definition"]["id"],"removals":plan["removals"],"rows":rows,"applied":false,"complete":true,"results":[]}),
    )
}
pub fn event(plan: &Value, row: &Value, plan_id: &Value) -> Value {
    let definition = &plan["definition"];
    json!({"owner":definition["id"],"query_id":definition["query"],"definition_digest":sha256(encode(definition)),"key":row["key"],"scope":plan["scope"],"collections":definition["destination"]["collections"],"connection":plan["connection"],"library":plan["library"],"target":row["target"],"before":row["before"],"after":row["after"],"owned":row["owned"],"state":"started","plan_id":plan_id})
}
/// Each durable write owns its Store/transaction and ends before remote work.
pub fn save_event(
    path: &Path,
    profile: &str,
    database_id: &str,
    id: &str,
    payload: &Value,
    create: bool,
) -> Result<()> {
    let store = Store::open(path, true, false)?;
    if store.database_id != database_id {
        return Err(Error("catalog replaced during destination mapping".into()));
    }
    store.transaction(|db| {
        if create {
            db.execute("INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,'mapping_publish','info',?)",rusqlite::params![id,profile,encode(payload)])?;
        } else {
            let severity = if matches!(payload["state"].as_str(),Some("verified"|"submitted"|"not_applied")){"info"}else{"warning"};
            if db.execute("UPDATE consumer_events SET subject=?,severity=? WHERE id=? AND profile=? AND event='mapping_publish'",rusqlite::params![encode(payload),severity,id,profile])? != 1 {
                return Err(error("attempt_changed"));
            }
        }
        Ok(())
    })
}
/// Recovery observes only; this reducer never proposes a remote write or undo.
pub fn recovered(observed: &Value, before: &Value, matches: bool) -> &'static str {
    if matches {
        "verified"
    } else if equal(observed, before) {
        "not_applied"
    } else {
        "uncertain"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capabilities() -> Capabilities {
        [("genres".into(), true), ("title".into(), false)].into()
    }
    #[test]
    fn owned_merge_preserves_manual_values_and_counts_only_owned_removals() {
        let definition = json!({"id":"owner"});
        let item = json!({"key":"one","target":{"id":"1","path":"/fixture"},"values":{"genres":["Wanted","Manual"]}});
        let before = json!({"identity":{"guid":"g"},"values":{"genres":["Manual","Old"]},"locks":{"genres":false}});
        let ledger=[(("1".into(),"genres".into()),json!({"owner":"owner","key":"one","identity":{"guid":"g"},"last":["Manual","Old"],"contribution":["Old"]}))].into();
        let (row, removals) =
            planned_row(&definition, &item, before, &ledger, &capabilities()).unwrap();
        assert_eq!(removals, 1);
        assert_eq!(row["after"]["genres"], json!(["Manual", "Wanted"]));
        assert_eq!(row["owned"]["genres"]["contribution"], json!(["Wanted"]));
    }
    #[test]
    fn scalar_owner_identity_and_external_change_are_fenced() {
        let definition = json!({"id":"owner"});
        let item =
            json!({"key":"one","target":{"id":"1","path":"/fixture"},"values":{"title":"Wanted"}});
        let before = json!({"identity":{"guid":"g"},"values":{"title":"Old"}});
        let owned = json!({"owner":"owner","key":"one","identity":{"guid":"g"},"last":"Old","contribution":"Old"});
        for (field, value, code) in [
            ("owner", json!("other"), "field_owned_by_other_mapping"),
            ("key", json!("other"), "field_owned_by_other_mapping"),
            (
                "identity",
                json!({"guid":"new"}),
                "owned_target_identity_changed",
            ),
            ("last", json!("Manual"), "owned_value_changed_remotely"),
        ] {
            let mut changed = owned.clone();
            changed[field] = value;
            let ledger = [(("1".into(), "title".into()), changed)].into();
            assert_eq!(
                planned_row(&definition, &item, before.clone(), &ledger, &capabilities())
                    .unwrap_err()
                    .0,
                format!("mapping_{code}")
            );
        }
    }
    #[test]
    fn recovery_and_ledger_are_observation_only() {
        let before = json!({"values":{"title":"old"}});
        assert_eq!(recovered(&before, &before, false), "not_applied");
        assert_eq!(
            recovered(&json!({"values":{"title":"new"}}), &before, true),
            "verified"
        );
        assert_eq!(
            recovered(&json!({"values":{"title":"manual"}}), &before, false),
            "uncertain"
        );
        let scope = json!(["plex", "server", "library"]);
        assert_eq!(
            ownership(
                &[json!({"scope":scope,"id":"event","state":"started"})],
                &scope
            )
            .unwrap_err()
            .0,
            "mapping_unresolved_write_recover_event"
        );
        assert!(
            ownership(
                &[json!({"scope":scope,"id":"event","state":"not_applied"})],
                &scope
            )
            .unwrap()
            .is_empty()
        );
    }
    #[test]
    fn event_persistence_is_atomic_and_database_identity_bound() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, false, false).unwrap();
        let id = store.database_id.clone();
        drop(store);
        let payload = json!({"state":"started"});
        assert!(save_event(&path, "default", "wrong", "event", &payload, true).is_err());
        save_event(&path, "default", &id, "event", &payload, true).unwrap();
        assert!(save_event(&path, "default", &id, "event", &payload, true).is_err());
        save_event(
            &path,
            "default",
            &id,
            "event",
            &json!({"state":"uncertain"}),
            false,
        )
        .unwrap();
        let store = Store::open(&path, false, false).unwrap();
        let (subject, severity): (String, String) = store
            .db
            .query_row(
                "SELECT subject,severity FROM consumer_events WHERE id='event'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            catabolic_core::decode_json(&subject).unwrap(),
            json!({"state":"uncertain"})
        );
        assert_eq!(severity, "warning");
        drop(store);
        assert_eq!(
            save_event(&path, "default", &id, "missing", &payload, false)
                .unwrap_err()
                .0,
            "mapping_attempt_changed"
        );
    }
    #[test]
    fn mixed_scope_less_history_does_not_enable_a_publication() {
        assert_eq!(
            ownership(
                &[json!({"version":3,"id":"http","state":"complete"})],
                &json!(["plex", "server", "library"])
            )
            .unwrap_err()
            .0,
            "mapping history event is missing scope"
        );
    }
    #[test]
    fn removal_budgets_preserve_arbitrary_python_integer_magnitudes() {
        assert_eq!(removal_budget(&json!(0)).unwrap(), 0);
        assert_eq!(
            removal_budget(
                &catabolic_core::decode_json("10000000000000000000000000000000000000000").unwrap()
            )
            .unwrap(),
            usize::MAX
        );
        for value in [json!(-1), json!(true), json!(1.0), Value::Null, json!("1")] {
            assert_eq!(
                removal_budget(&value).unwrap_err().0,
                "mapping_invalid_removal_budget"
            );
        }
    }
}
