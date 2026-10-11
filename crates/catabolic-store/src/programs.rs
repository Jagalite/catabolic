// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Bounded configuration bundles. Export never executes queries or media work.
use crate::{
    Error, Result, Store, definitions, media_mutation,
    migration::rows,
    output_mutation,
    query::{decode, name},
};
use catabolic_core::encode;
use serde_json::{Value, json};
use std::collections::BTreeSet;
const KINDS: &[&str] = &[
    "queries",
    "outputs",
    "operations",
    "rules",
    "layouts",
    "projections",
];
fn fail<T>(message: &str) -> Result<T> {
    Err(Error(message.into()))
}
fn kinds(version: u64) -> Vec<&'static str> {
    let mut kinds = KINDS.to_vec();
    if version >= 2 {
        kinds.push("fallback_policies");
    }
    if version == 3 {
        kinds.push("watchers");
    }
    kinds
}
fn exact(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.len() == keys.len() && o.keys().all(|k| keys.contains(&k.as_str())))
}
pub fn validate(bundle: &Value) -> Result<()> {
    let version = bundle["version"].as_u64().unwrap_or(0);
    let kinds = kinds(version);
    let mut fields = vec!["format", "version"];
    fields.extend(&kinds);
    if !exact(bundle, &fields)
        || bundle["format"] != "catabolic.program"
        || !(1..=3).contains(&version)
    {
        return fail("expected a version 1, 2 or 3 catabolic.program bundle");
    }
    if encode(bundle).len() > 4 * 1024 * 1024 {
        return fail("program bundle exceeds 4 MiB");
    }
    let mut count = 0;
    for kind in kinds {
        let records = bundle[kind]
            .as_object()
            .ok_or_else(|| Error("program sections must be objects".into()))?;
        count += records.len();
        let fields = match kind {
            "rules" => &[
                "name",
                "operation_id",
                "location",
                "selection",
                "estimates",
                "required",
                "allow_derived",
            ][..],
            "projections" => &["catalog", "query_id", "layout", "copies", "renditions"][..],
            _ => &["name", "definition"][..],
        };
        for (key, value) in records {
            if key.is_empty() || key.chars().count() > 255 || !exact(value, fields) {
                return Err(Error(format!("invalid program {kind} record")));
            }
            if kind != "projections" {
                name(
                    value["name"]
                        .as_str()
                        .ok_or_else(|| Error("program names must be strings".into()))?,
                )?;
            }
            if value.get("definition").is_some_and(|v| !v.is_object()) {
                return fail("program definition must be an object");
            }
            if kind == "rules"
                && (!value["required"].is_boolean()
                    || !value["allow_derived"].is_boolean()
                    || !value["selection"].is_object()
                    || !value["estimates"].is_object())
            {
                return fail("invalid program rule options");
            }
        }
    }
    if !(1..=256).contains(&count) {
        return fail("program requires 1..256 definitions");
    }
    Ok(())
}
struct Export<'a> {
    store: &'a Store,
    profile: &'a str,
    records: Value,
    visiting: BTreeSet<(String, String)>,
}
impl Export<'_> {
    fn add(&mut self, kind: &str, id: &str) -> Result<()> {
        if self.records[kind].get(id).is_some() {
            return Ok(());
        }
        let key = (kind.to_owned(), id.to_owned());
        if self.visiting.contains(&key) || self.visiting.len() >= 32 {
            return fail("program dependency cycle or depth exceeds 32");
        }
        let count: usize = self
            .records
            .as_object()
            .unwrap()
            .values()
            .map(|v| v.as_object().unwrap().len())
            .sum();
        if count + self.visiting.len() >= 256 {
            return fail("program exceeds 256 definitions");
        }
        self.visiting.insert(key.clone());
        let record = match kind {
            "queries" => {
                let row = definitions::query_get(self.store, self.profile, id)?;
                let mut value = row["definition"].clone();
                for reference in value["queries"].as_array().into_iter().flatten() {
                    self.add("queries", reference.as_str().unwrap())?;
                }
                if let Some(selection) = value.get_mut("selection").and_then(Value::as_object_mut) {
                    selection.remove("profile");
                }
                json!({"name":row["name"],"definition":value})
            }
            "outputs" => {
                let row = output_mutation::get_definition(self.store, id)?;
                json!({"name":row["name"],"definition":row["definition"]})
            }
            "operations" => {
                let row = media_mutation::operation_get(self.store, id)?;
                let mut value = row["definition"].clone();
                if row["operation_kind"] == "render" {
                    let mut options = value.clone();
                    for key in ["version", "preset", "output_definition_id"] {
                        options.as_object_mut().unwrap().remove(key);
                    }
                    value = json!({"kind":"render","operation":row["preset"],"output_definition_id":row["output_definition_id"],"options":options});
                    if row["output_definition_id"].is_null() {
                        value
                            .as_object_mut()
                            .unwrap()
                            .remove("output_definition_id");
                    }
                }
                if let Some(id) = value.get("output_definition_id") {
                    self.add(
                        "outputs",
                        id.as_str()
                            .ok_or_else(|| Error("unknown output definition".into()))?,
                    )?;
                }
                json!({"name":row["name"],"definition":value})
            }
            "rules" => {
                let row = definitions::rule_get(self.store, self.profile, id)?;
                self.add("operations", row["recipe_id"].as_str().unwrap())?;
                let mut selection = row["selection"].clone();
                selection.as_object_mut().unwrap().remove("profile");
                if let Some(id) = selection["query_id"].as_str() {
                    self.add("queries", id)?;
                }
                json!({"name":row["name"],"operation_id":row["recipe_id"],"location":row["location"],"selection":selection,"estimates":row["estimate_options"],"required":row["required"],"allow_derived":row["allow_derived"].as_i64()!=Some(0)})
            }
            "layouts" => {
                let row = definitions::layout_get(self.store, id)?;
                let mut value = row["definition"].clone();
                if let Some(selection) = value.get_mut("selection").and_then(Value::as_object_mut) {
                    selection.remove("profile");
                    if let Some(id) = selection.get("query_id").and_then(Value::as_str) {
                        self.add("queries", id)?;
                    }
                }
                json!({"name":row["name"],"definition":value})
            }
            "fallback_policies" | "watchers" => {
                let (sql, missing) = if kind == "watchers" {
                    (
                        "SELECT w.*,d.definition,d.revision,d.digest FROM watchers w JOIN watcher_definitions d ON d.id=w.definition_id WHERE w.profile=? AND w.name=?",
                        "unknown watcher",
                    )
                } else {
                    (
                        "SELECT * FROM fallback_policies WHERE profile=? AND id=?",
                        "unknown fallback policy revision in this profile",
                    )
                };
                let mut row = rows(
                    &self.store.db,
                    sql,
                    &[self.profile.to_owned().into(), id.to_owned().into()],
                )?
                .into_iter()
                .next()
                .ok_or_else(|| Error(missing.into()))?;
                decode(&mut row, &["definition"])?;
                let value = &row["definition"];
                if kind == "fallback_policies" {
                    for tier in value["fallbacks"].as_array().into_iter().flatten() {
                        self.add("queries", tier["query_id"].as_str().unwrap())?;
                    }
                } else {
                    if let Some(operation) =
                        value["operation_id"].as_str().filter(|s| !s.is_empty())
                    {
                        self.add("operations", operation)?;
                    }
                    let plan = &value["plan"];
                    if plan["kind"] == "query" {
                        self.add("queries", plan["query_id"].as_str().unwrap())?;
                    } else if plan["kind"] == "fallback" {
                        self.add("queries", plan["membership_query_id"].as_str().unwrap())?;
                        self.add("fallback_policies", plan["policy_id"].as_str().unwrap())?;
                    }
                }
                json!({"name":row["name"],"definition":value})
            }
            "projections" => {
                let row = definitions::projection_get(self.store, self.profile, id)?;
                if row["legacy"] == true {
                    return fail("export requires an explicit projection binding");
                }
                self.add("queries", row["query_id"].as_str().unwrap())?;
                self.add("layouts", row["layout"].as_str().unwrap())?;
                let policy = &row["rendition_policy"];
                if !policy.is_null() {
                    for (field, target) in [("definition_id", "outputs"), ("rule_id", "rules")] {
                        if let Some(id) = policy[field].as_str() {
                            self.add(target, id)?;
                        }
                    }
                }
                json!({"catalog":id,"query_id":row["query_id"],"layout":row["layout"],"copies":row["copy_policy"],"renditions":policy})
            }
            _ => return Err(Error(format!("unknown program section: {kind}"))),
        };
        self.records[kind][id] = record;
        self.visiting.remove(&key);
        Ok(())
    }
}
pub fn export(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    let version = if args["watchers"].as_array().is_some_and(|a| !a.is_empty()) {
        3
    } else if args["fallback_policies"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
    {
        2
    } else {
        1
    };
    let mut records = json!({});
    for kind in kinds(version) {
        records[kind] = json!({});
    }
    let mut state = Export {
        store,
        profile,
        records,
        visiting: BTreeSet::new(),
    };
    for kind in [
        "queries",
        "rules",
        "projections",
        "fallback_policies",
        "watchers",
    ] {
        for id in args[kind].as_array().into_iter().flatten() {
            state.add(
                kind,
                id.as_str()
                    .ok_or_else(|| Error("program roots must be revision names or IDs".into()))?,
            )?;
        }
    }
    let mut bundle = state.records;
    bundle["format"] = json!("catabolic.program");
    bundle["version"] = json!(version);
    validate(&bundle)?;
    Ok(bundle)
}
pub fn dispatch(store: &Store, profile: &str, action: &str, args: &Value) -> Result<Value> {
    match action {
        "export" => export(store, profile, args),
        "validate" => {
            validate(&args["value"])?;
            Ok(json!({"valid":true}))
        }
        _ => Err(Error(format!("unknown programs action: {action}"))),
    }
}

struct Import<'a, F> {
    store: &'a Store,
    profile: &'a str,
    bundle: &'a Value,
    bindings: &'a Value,
    prefix: &'a str,
    references: Value,
    visiting: BTreeSet<(String, String)>,
    catalogs: BTreeSet<String>,
    put_operation: &'a F,
}
impl<F: Fn(&str, &Value) -> Result<Value>> Import<'_, F> {
    fn bound(&self, kind: &str, id: &Value) -> Result<String> {
        let local = id.as_str().and_then(|id| self.bindings[kind][id].as_str());
        local.map(str::to_owned).ok_or_else(|| {
            let shown = match id {
                Value::String(id) => format!("'{id}'"),
                Value::Null => "None".into(),
                Value::Bool(true) => "True".into(),
                Value::Bool(false) => "False".into(),
                v => encode(v),
            };
            Error(format!("explicit {kind} binding required for {shown}"))
        })
    }
    fn selection(&mut self, input: &Value) -> Result<Value> {
        let mut value = input.clone();
        value["profile"] = json!(self.profile);
        if value.get("query_id").is_some() {
            let id = value["query_id"].clone();
            value["query_id"] = json!(self.put("queries", &id)?);
        }
        Ok(value)
    }
    fn put(&mut self, kind: &str, reference: &Value) -> Result<String> {
        let id = reference
            .as_str()
            .filter(|id| self.bundle[kind].get(id).is_some())
            .ok_or_else(|| Error(format!("missing program {kind} reference")))?;
        if let Some(local) = self.references[kind][id].as_str() {
            return Ok(local.into());
        }
        let key = (kind.to_owned(), id.to_owned());
        if self.visiting.contains(&key) || self.visiting.len() >= 32 {
            return fail("program dependency cycle or depth exceeds 32");
        }
        self.visiting.insert(key.clone());
        let row = self.bundle[kind][id].clone();
        let local_name = row["name"].as_str().map(|n| format!("{}.{n}", self.prefix));
        let local = match kind {
            "fallback_policies" => {
                let mut value = row["definition"].clone();
                let tiers = value["fallbacks"]
                    .as_array_mut()
                    .ok_or_else(|| Error("fallback tiers must be an array".into()))?;
                for tier in tiers {
                    tier["query_id"] = json!(self.put("queries", &tier["query_id"])?);
                }
                crate::fallback_mutation::put(
                    self.store,
                    self.profile,
                    local_name.as_deref().unwrap(),
                    &value,
                )?["id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            }
            "watchers" => {
                let mut value = row["definition"].clone();
                if value["operation_id"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                {
                    value["operation_id"] = json!(self.put("operations", &value["operation_id"])?);
                }
                if value["destination"].as_str().is_some_and(|s| !s.is_empty()) {
                    value["destination"] = json!(self.bound("locations", &value["destination"])?);
                }
                let plan = &mut value["plan"];
                match plan["kind"].as_str() {
                    Some("query") => {
                        plan["query_id"] = json!(self.put("queries", &plan["query_id"])?)
                    }
                    Some("fallback") => {
                        plan["membership_query_id"] =
                            json!(self.put("queries", &plan["membership_query_id"])?);
                        plan["policy_id"] =
                            json!(self.put("fallback_policies", &plan["policy_id"])?);
                    }
                    Some("projection") => {
                        let catalog = self.bound("catalogs", &plan["catalog"])?;
                        let config = rows(
                            &self.store.db,
                            "SELECT * FROM fallback_bindings WHERE profile=? AND catalog=?",
                            &[self.profile.to_owned().into(), catalog.clone().into()],
                        )?
                        .into_iter()
                        .next()
                        .ok_or_else(|| {
                            Error(
                                "watcher projection requires an authoritative fallback binding"
                                    .into(),
                            )
                        })?;
                        let digest = catabolic_core::sha256(encode(
                            &json!({"query_id":config["query_id"],"policy_id":config["policy_id"],"layout":definitions::layout_get(self.store,config["layout"].as_str().unwrap())?,"output":rows(&self.store.db,"SELECT * FROM bindings WHERE profile=? AND kind='output' AND owner=?",&[self.profile.to_owned().into(),catalog.clone().into()])?}),
                        ));
                        plan["catalog"] = json!(catalog);
                        plan["binding_digest"] = json!(digest);
                    }
                    _ => (),
                }
                crate::watcher::execute(
                    self.store,
                    self.profile,
                    "put",
                    &json!({"name":local_name.as_deref().unwrap(),"definition":value}),
                )?["definition_id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            }
            "queries" => {
                let mut value = row["definition"].clone();
                if let Some(refs) = value.get("queries") {
                    let refs = refs
                        .as_array()
                        .ok_or_else(|| Error("query references must be an array".into()))?;
                    let mut new = vec![];
                    for id in refs {
                        new.push(json!(self.put("queries", id)?));
                    }
                    value["queries"] = json!(new);
                }
                if value.get("selection").is_some() {
                    if !value["selection"].is_object() {
                        return fail("query selection must be an object");
                    }
                    value["selection"] = self.selection(&value["selection"])?;
                }
                definitions::query_put(
                    self.store,
                    self.profile,
                    local_name.as_deref().unwrap(),
                    &value,
                )?["id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            }
            "outputs" => output_mutation::define(
                self.store,
                local_name.as_deref().unwrap(),
                &row["definition"],
            )?["id"]
                .as_str()
                .unwrap()
                .to_owned(),
            "operations" => {
                let mut value = row["definition"].clone();
                if value.get("output_definition_id").is_some() {
                    value["output_definition_id"] =
                        json!(self.put("outputs", &value["output_definition_id"])?);
                }
                if value["kind"] == "external" {
                    value["operation"] = json!(self.bound("processors", &value["operation"])?);
                }
                (self.put_operation)(local_name.as_deref().unwrap(), &value)?["id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            }
            "rules" => {
                let local_name = local_name.as_deref().unwrap();
                let existing = rows(
                    &self.store.db,
                    "SELECT id FROM processing_rules WHERE profile=? AND name=?",
                    &[self.profile.to_owned().into(), local_name.to_owned().into()],
                )?
                .iter()
                .map(|r| r["id"].as_str().unwrap().to_owned())
                .collect::<BTreeSet<_>>();
                let operation = self.put("operations", &row["operation_id"])?;
                let location = if row["location"].is_null() {
                    Value::Null
                } else {
                    json!(self.bound("locations", &row["location"])?)
                };
                let selection = self.selection(&row["selection"])?;
                let result = definitions::rule_put(
                    self.store,
                    self.profile,
                    &json!({"name":local_name,"recipe_id":operation,"location":location,"selection":selection,"estimates":row["estimates"],"required":row["required"],"allow_derived":row["allow_derived"]}),
                )?;
                let local = result["id"].as_str().unwrap().to_owned();
                if !existing.is_empty() && !existing.contains(&local) {
                    return fail("import would replace an existing rule; choose another prefix");
                }
                if !existing.contains(&local) {
                    definitions::dispatch(
                        self.store,
                        self.profile,
                        "rule-enable",
                        &json!({"id":local,"enabled":false}),
                    )?;
                }
                local
            }
            "layouts" => {
                let local_name = local_name.as_deref().unwrap();
                let mut value = row["definition"].clone();
                if value.get("selection").is_some() {
                    if !value["selection"].is_object() {
                        return fail("layout selection must be an object");
                    }
                    value["selection"] = self.selection(&value["selection"])?;
                }
                let old = rows(
                    &self.store.db,
                    "SELECT value FROM meta WHERE key=?",
                    &[format!("layout:{local_name}").into()],
                )?;
                if let Some(old) = old.first()
                    && catabolic_core::decode_json(old["value"].as_str().unwrap())? != value
                {
                    return fail("import would replace an existing layout; choose another prefix");
                }
                definitions::dispatch(
                    self.store,
                    self.profile,
                    "layout-put",
                    &json!({"name":local_name,"value":value}),
                )?;
                local_name.into()
            }
            "projections" => {
                let local = self.bound("catalogs", &row["catalog"])?;
                if !self.catalogs.insert(local.clone()) {
                    return fail("multiple projections cannot share an import destination");
                }
                let query = self.put("queries", &row["query_id"])?;
                let layout = self.put("layouts", &row["layout"])?;
                let mut policy = row["renditions"].clone();
                if !policy.is_null() {
                    if !policy.is_object() {
                        return fail("rendition policy must be an object");
                    }
                    for (field, target) in [("definition_id", "outputs"), ("rule_id", "rules")] {
                        if policy.get(field).is_some() {
                            policy[field] = json!(self.put(target, &policy[field])?);
                        }
                    }
                }
                let before = definitions::projection_get(self.store, self.profile, &local)?;
                let expected = json!({"query_id":query,"layout":layout,"copy_policy":row["copies"],"rendition_policy":policy});
                if before["legacy"] == false
                    && expected
                        .as_object()
                        .unwrap()
                        .iter()
                        .any(|(k, v)| before[k] != *v)
                {
                    return fail("import would replace an existing projection");
                }
                if before["legacy"] == true
                    && (!before["applied_layout"].is_null()
                        || !before["copy_policy"].is_null()
                        || !before["rendition_policy"].is_null()
                        || before["refresh"].as_array().is_some_and(|a| !a.is_empty()))
                {
                    return fail("import requires an unconfigured destination catalog");
                }
                definitions::dispatch(
                    self.store,
                    self.profile,
                    "projection-put",
                    &json!({"catalog":local,"query_id":query,"layout":layout,"copies":row["copies"],"renditions":policy}),
                )?;
                local
            }
            _ => {
                return Err(Error(format!(
                    "native program import does not yet support {kind}"
                )));
            }
        };
        self.references[kind][id] = json!(local);
        self.visiting.remove(&key);
        Ok(local)
    }
}
/// Native import delegates operation construction to its native runtime owner.
/// No raw recipe rows are inserted and no Python fallback is permitted.
pub fn import<F: Fn(&str, &Value) -> Result<Value>>(
    store: &Store,
    profile: &str,
    args: &Value,
    put_operation: F,
) -> Result<Value> {
    let bundle = &args["value"];
    validate(bundle)?;
    definitions::require_recovered(store)?;
    if !store.writable || !store.db.is_autocommit() {
        return fail("program import requires the writer lock outside a transaction");
    }
    let prefix =
        args.get("prefix")
            .and_then(Value::as_str)
            .unwrap_or(if args.get("prefix").is_some() {
                ""
            } else {
                "imported"
            });
    if args.get("prefix").is_some_and(|v| !v.is_string()) {
        return fail("import prefix must be a name");
    }
    name(prefix)?;
    let bindings = args
        .get("bindings")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!({}));
    if bindings.as_object().is_none_or(|o| {
        o.keys()
            .any(|k| !["catalogs", "locations", "processors"].contains(&k.as_str()))
    }) {
        return fail("bindings supports catalogs, locations and processors");
    }
    for mapping in bindings.as_object().unwrap().values() {
        let mapping = mapping
            .as_object()
            .ok_or_else(|| Error("binding maps must be objects".into()))?;
        for (key, value) in mapping {
            let value = value
                .as_str()
                .ok_or_else(|| Error("binding names must be strings".into()))?;
            name(key)?;
            name(value)?;
        }
    }
    let version = bundle["version"].as_u64().unwrap();
    let mut references = json!({});
    for kind in kinds(version) {
        references[kind] = json!({});
    }
    let mut state = Import {
        store,
        profile,
        bundle,
        bindings: &bindings,
        prefix,
        references,
        visiting: BTreeSet::new(),
        catalogs: BTreeSet::new(),
        put_operation: &put_operation,
    };
    let dry_run = args["dry_run"] == true;
    let result = store.transaction(|_| {
        for kind in kinds(version) {
            for id in bundle[kind].as_object().unwrap().keys() {
                state.put(kind, &json!(id))?;
            }
        }
        if dry_run {
            return fail("native_program_preview_rollback");
        }
        Ok(())
    });
    match result {
        Ok(()) => (),
        Err(e) if dry_run && e.0 == "native_program_preview_rollback" => (),
        Err(e) => return Err(e),
    }
    Ok(
        json!({"format":"catabolic.program","version":version,"complete":true,"applied":!dry_run,"dry_run":dry_run,"references":state.references,"preview_ids_ephemeral":dry_run}),
    )
}
