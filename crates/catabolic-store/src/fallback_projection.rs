// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Evaluated logical projection plans, generation admission and publication
//! completion. Filesystem intent execution remains in the journal owner.
use crate::{
    Error, Result, Store, definitions,
    fallback_resolution::{self, Context, Probe, Resolver},
    migration::rows,
    selection::Evaluation,
    sql::scalar,
};
use catabolic_core::{
    encode,
    resolution::{self, ProjectionAdmission},
    sha256,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::UnicodeNormalization;
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| Error(format!("invalid projection {key}")))
}
pub fn binding(store: &Store, profile: &str, catalog: &str) -> Result<Option<Value>> {
    Ok(rows(
        &store.db,
        "SELECT * FROM fallback_bindings WHERE profile=? AND catalog=?",
        &[profile.to_owned().into(), catalog.to_owned().into()],
    )?
    .into_iter()
    .next())
}
fn symlink(store: &Store, catalog: &str) -> Result<()> {
    let hard: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_link_modes WHERE catalog=? AND mode!='symlink')",
        [catalog],
        |r| r.get(0),
    )?;
    if hard {
        return Err(Error("fallback publication supports symlinks only".into()));
    }
    Ok(())
}
pub fn bind(
    evaluation: &mut Evaluation,
    catalog: &str,
    policy_id: &str,
    query_id: &str,
    layout: &str,
) -> Result<Value> {
    definitions::require_recovered(evaluation.store())?;
    crate::fallback_mutation::get(evaluation.store(), &evaluation.profile, policy_id)?;
    definitions::layout_get(evaluation.store(), layout)?;
    let selection =
        evaluation.select(&json!({"query_id":query_id,"profile":evaluation.profile}))?;
    if selection.entity != "item_id" || selection.report["complete"] != true {
        return Err(Error("logical fallback membership requires a complete item_id selection; exact file selections are not converted".into()));
    }
    let store = evaluation.store();
    let profile = &evaluation.profile;
    symlink(store, catalog)?;
    if binding(store,profile,catalog)?.is_none() && store.db.query_row("SELECT EXISTS(SELECT 1 FROM mappings WHERE catalog=? AND active=1) OR EXISTS(SELECT 1 FROM owned_links WHERE profile=? AND catalog=?)",params![catalog,profile,catalog],|r|r.get::<_,bool>(0))? {return Err(Error("existing exact mappings require an explicit separate logical projection; automatic adoption is forbidden".into()));}
    store.transaction(|db|{db.execute("INSERT INTO fallback_bindings(profile,catalog,policy_id,query_id,layout) VALUES (?,?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET policy_id=excluded.policy_id,query_id=excluded.query_id,layout=excluded.layout,next_attempt=0",params![profile,catalog,policy_id,query_id,layout])?;Ok(())})?;
    Ok(binding(store, profile, catalog)?.unwrap())
}
fn entries(store: &Store, profile: &str, catalog: &str) -> Result<BTreeMap<String, Value>> {
    rows(
        &store.db,
        "SELECT * FROM fallback_entries WHERE profile=? AND catalog=?",
        &[profile.to_owned().into(), catalog.to_owned().into()],
    )?
    .into_iter()
    .map(|r| Ok((text(&r, "id")?.to_owned(), r)))
    .collect()
}
pub struct PreparedProjection {
    plan: Value,
    old: BTreeMap<String, Value>,
    database: String,
    profile: String,
    seal: String,
}
impl PreparedProjection {
    pub fn present(&self) -> Value {
        present(&self.plan)
    }
    pub fn digest(&self) -> &str {
        &self.seal
    }
    pub fn is_safe(&self) -> bool {
        self.plan["safe"] == true
    }
}
fn seal(plan: &Value) -> String {
    let mut signed = plan.clone();
    for key in ["health", "plan_id"] {
        signed.as_object_mut().unwrap().remove(key);
    }
    sha256(encode(&signed))
}
pub fn present(plan: &Value) -> Value {
    let mut value = plan.clone();
    for key in ["captures", "health"] {
        value.as_object_mut().unwrap().remove(key);
    }
    value
}
pub fn plan(
    evaluation: &mut Evaluation,
    catalog: &str,
    manual_failback: bool,
    probe: &mut Probe<'_>,
) -> Result<PreparedProjection> {
    let profile = evaluation.profile.clone();
    let config = binding(evaluation.store(), &profile, catalog)?
        .ok_or_else(|| Error("projection has no fallback binding".into()))?;
    definitions::require_recovered(evaluation.store())?;
    symlink(evaluation.store(), catalog)?;
    let policy = crate::fallback_mutation::get(
        evaluation.store(),
        &profile,
        text(&config, "policy_id")?,
    )?["definition"]
        .clone();
    let selected = evaluation.select(&json!({"query_id":config["query_id"],"profile":profile}))?;
    if selected.entity != "item_id" || selected.report["complete"] != true {
        return Err(Error("incomplete_logical_membership".into()));
    }
    let maximum = policy["budgets"]["max_candidates"].as_u64().unwrap() as usize;
    if selected.ids.len() > policy["budgets"]["max_entries"].as_u64().unwrap() as usize {
        return Err(Error("logical_membership_budget".into()));
    }
    let associations = rows(
        &evaluation.store().db,
        "SELECT item_id,role,part,metadata FROM item_files WHERE active=1 AND role IN (SELECT value FROM json_each(?)) AND item_id IN (SELECT value FROM json_each(?)) ORDER BY id LIMIT ?",
        &[
            encode(&policy["slot_roles"]).into(),
            encode(&json!(selected.ids)).into(),
            ((maximum + 1) as i64).into(),
        ],
    )?;
    evaluation.account_value(&json!(associations))?;
    if associations.len() > maximum {
        return Err(Error("logical_membership_budget".into()));
    }
    let mut slots = BTreeMap::new();
    let mut covered = BTreeSet::new();
    for row in associations {
        let metadata: Value = catabolic_core::decode_json(text(&row, "metadata")?)?;
        let slot = json!([
            row["item_id"],
            row["role"],
            row["part"],
            metadata.get("variant").cloned().unwrap_or(json!(""))
        ]);
        covered.insert(text(&row, "item_id")?.to_owned());
        slots.insert(encode(&slot), slot);
    }
    for id in selected.ids.difference(&covered) {
        let slot = json!([id, "primary", null, ""]);
        slots.insert(encode(&slot), slot);
    }
    let logical = slots
        .into_values()
        .map(|s| json!({"item_id":s[0],"role":s[1],"part":s[2],"variant":s[3]}))
        .collect::<Vec<_>>();
    let old = entries(evaluation.store(), &profile, catalog)?;
    let mut result = if logical.is_empty() {
        json!({"policy_id":config["policy_id"],"epoch":fallback_resolution::epoch(evaluation.store())?,"decisions":[],"captures":{},"health":[],"tiers":[],"checks":0})
    } else {
        let primary = logical
            .iter()
            .filter(|e| e["role"] == "primary")
            .cloned()
            .collect::<Vec<_>>();
        let sidecars = logical
            .iter()
            .filter(|e| e["role"] != "primary")
            .cloned()
            .collect::<Vec<_>>();
        let previous = old
            .iter()
            .filter(|(_, v)| v["policy_id"] == config["policy_id"])
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut resolver = Resolver::new(Context::Local(evaluation), probe, None);
        let mut result = resolver.resolve(
            if primary.is_empty() {
                &logical
            } else {
                &primary
            },
            text(&config, "policy_id")?,
            catalog,
            &previous,
            manual_failback,
        )?;
        if !primary.is_empty() && !sidecars.is_empty() {
            resolver.primary_choices = result["decisions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| d["state"].as_str().unwrap().starts_with("resolved"))
                .map(|d| Ok((text(d, "item_id")?.into(), text(d, "file_id")?.into())))
                .collect::<Result<_>>()?;
            let extra = resolver.resolve(
                &sidecars,
                text(&config, "policy_id")?,
                catalog,
                &old,
                manual_failback,
            )?;
            let checks = result["checks"].as_u64().unwrap() + extra["checks"].as_u64().unwrap();
            if checks > policy["budgets"]["max_checks"].as_u64().unwrap() {
                return Err(Error("resolution_probe_budget".into()));
            }
            for key in ["decisions", "health"] {
                result[key]
                    .as_array_mut()
                    .unwrap()
                    .extend(extra[key].as_array().unwrap().iter().cloned());
            }
            result["captures"]
                .as_object_mut()
                .unwrap()
                .extend(extra["captures"].as_object().unwrap().clone());
            result["checks"] = json!(checks);
            result["sidecar_tiers"] = extra["tiers"].clone();
        }
        result
    };
    if !policy["package"].is_null() {
        crate::component_selection::projection_members(evaluation.store(), &profile, &mut result)?;
    }
    let layout =
        definitions::layout_get(evaluation.store(), text(&config, "layout")?)?["definition"]
            .clone();
    let mut destinations = BTreeMap::new();
    for captured in result["captures"].as_object().unwrap().values() {
        let mut row = captured.clone();
        row["metadata"] = json!(encode(&row["metadata"]));
        let path = crate::layout::render_association(evaluation.store(), &profile, &row, &layout)?
            .ok_or_else(|| Error("fallback logical entry has no matching layout rule".into()))?;
        destinations.insert(text(&row, "id")?.to_owned(), path);
    }
    let layout_digest = sha256(encode(&layout));
    let mut desired = Vec::new();
    let mut paths = BTreeMap::new();
    for decision in result["decisions"].as_array().unwrap() {
        let mut d = decision.clone();
        let id = text(&d, "entry_id")?;
        let previous = old.get(id).cloned().unwrap_or(json!({}));
        let chosen = &result["captures"][id];
        d["path"] = if !chosen.is_null() && text(&d, "state")?.starts_with("resolved") {
            json!(destinations[text(chosen, "id")?])
        } else {
            previous["path"].clone()
        };
        if let (Some(path), Some(before)) = (d["path"].as_str(), previous["path"].as_str())
            && fallback_resolution::decode_evidence(&previous["evidence"])?["layout_digest"]
                == layout_digest
        {
            let mut stable = std::path::PathBuf::from(before);
            stable.set_extension(std::path::Path::new(path).extension().unwrap_or_default());
            d["path"] = json!(stable.to_string_lossy());
        }
        d["evidence"]["layout_digest"] = json!(layout_digest);
        if let Some(path) = d["path"].as_str().filter(|p| !p.is_empty()) {
            let folded = crate::sql::casefold(&path.nfc().collect::<String>());
            if paths
                .insert(folded, text(&d, "entry_id")?.to_owned())
                .is_some()
            {
                return Err(Error("fallback destination collision".into()));
            }
        }
        desired.push(d);
    }
    for path in paths.keys() {
        let pieces = path.split('/').collect::<Vec<_>>();
        for i in 1..pieces.len() {
            if paths.contains_key(&pieces[..i].join("/")) {
                return Err(Error("fallback destination parent collision".into()));
            }
        }
    }
    // Python's decisions, captures and desired rows share evidence dictionaries.
    // Preserve that wire contract explicitly with Rust's owned values.
    for d in &desired {
        let id = text(d, "entry_id")?;
        if let Some(captured) = result["captures"].as_object_mut().unwrap().get_mut(id)
            && d["evidence"].get("parent_entry_id").is_none()
        {
            captured["evidence"]["layout_digest"] = json!(layout_digest);
        }
        for decision in result["decisions"].as_array_mut().unwrap().iter_mut() {
            if decision["entry_id"] == id {
                decision["evidence"]["layout_digest"] = json!(layout_digest);
            }
        }
    }
    let ids = desired
        .iter()
        .map(|d| text(d, "entry_id").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    let removed = old
        .iter()
        .filter(|(k, v)| v["active"] == 1 && !ids.contains(*k))
        .map(|(k, _)| k.clone())
        .collect::<Vec<_>>();
    let safe = desired.iter().all(|d| {
        d["state"].as_str().unwrap().starts_with("resolved")
            && d["evidence"]["component_package"]["packaging"]["ready"] != false
    });
    for (key, value) in [
        ("catalog", json!(catalog)),
        ("generation", config["generation"].clone()),
        ("desired", json!(desired)),
        ("removed", json!(removed)),
        ("safe", json!(safe)),
    ] {
        result[key] = value;
    }
    let seal = seal(&result);
    result["plan_id"] = json!(seal);
    Ok(PreparedProjection {
        plan: result,
        old,
        database: evaluation.store().database_id.clone(),
        profile,
        seal,
    })
}
pub struct AdmittedProjection {
    prepared: PreparedProjection,
    generation: i64,
}
impl AdmittedProjection {
    pub fn is_safe(&self) -> bool {
        self.prepared.is_safe()
    }
    pub fn retained(&self) -> Value {
        let mut v = self.prepared.present();
        v["applied"] = json!(false);
        v["membership_retained"] = json!(true);
        v
    }
}
fn changed(previous: &Value, decision: &Value, stored: bool) -> Result<bool> {
    for (a, b) in [
        ("file_id", "file_id"),
        ("revision", "revision"),
        ("path", "path"),
        ("state", "state"),
        ("policy_id", "policy_id"),
        ("tier", if stored { "tier" } else { "selected_tier" }),
    ] {
        if previous[a] != decision[b] {
            return Ok(true);
        }
    }
    Ok(previous["active"] != 1
        || fallback_resolution::decode_evidence(&previous["evidence"])? != decision["evidence"])
}
pub fn admit(
    store: &Store,
    profile: &str,
    prepared: PreparedProjection,
    max_removals: usize,
    max_changes: usize,
) -> Result<AdmittedProjection> {
    if store.database_id != prepared.database
        || profile != prepared.profile
        || seal(&prepared.plan) != prepared.seal
    {
        return Err(Error("stale_resolution_plan".into()));
    }
    let plan = &prepared.plan;
    let catalog = text(plan, "catalog")?;
    let generation=store.transaction(|db|{
 let config=binding(store,profile,catalog)?.ok_or_else(||Error("stale_resolution_generation".into()))?;
 let desired=plan["desired"].as_array().unwrap();let empty=json!({});
 let changes=desired.iter().filter(|d|{let previous=prepared.old.get(d["entry_id"].as_str().unwrap()).unwrap_or(&empty);["file_id","path","revision"].iter().any(|k|previous[*k]!=d[*k])}).count();
 let removals=plan["removed"].as_array().unwrap().len()+desired.iter().filter(|d|prepared.old.get(d["entry_id"].as_str().unwrap()).is_some_and(|old|old["path"]!=d["path"])).count();
 let effect=resolution::projection(ProjectionAdmission{expected_epoch:plan["epoch"].as_i64().unwrap(),current_epoch:fallback_resolution::epoch(store)?,expected_generation:plan["generation"].as_i64().unwrap(),current_generation:config["generation"].as_i64().unwrap(),safe:plan["safe"]==true,changes,removals,max_changes,max_removals}).map_err(|e|Error(e.into()))?;
 db.execute("DELETE FROM fallback_health WHERE profile=? AND catalog=?",params![profile,catalog])?;
 for h in plan["health"].as_array().unwrap(){db.execute("INSERT INTO fallback_health VALUES (?,?,?,?,?,?,?,?)",params![profile,catalog,text(h,"entry_id")?,text(h,"file_id")?,text(h,"revision")?,scalar(&h["healthy_since"])?,scalar(&h["successes"])?,scalar(&h["last_check"]) ?])?;}
 for d in desired{let previous=prepared.old.get(text(d,"entry_id")?).unwrap_or(&empty);let mut selected=if effect.retain{previous.clone()}else{let mut v=d.clone();v["tier"]=d["selected_tier"].clone();v["tier_name"]=d["selected_tier_name"].clone();v};
 selected["state"]=if effect.retain&&text(d,"state")?.starts_with("resolved"){json!("blocked")}else{d["state"].clone()};selected["evidence"]=if effect.retain{fallback_resolution::decode_evidence(&previous["evidence"])?}else{d["evidence"].clone()};selected["policy_id"]=plan["policy_id"].clone();
 db.execute("INSERT INTO fallback_entries(id,profile,catalog,item_id,role,part,variant,policy_id,file_id,revision,tier,tier_name,path,state,generation,evidence) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET policy_id=excluded.policy_id,file_id=excluded.file_id,revision=excluded.revision,tier=excluded.tier,tier_name=excluded.tier_name,path=excluded.path,state=excluded.state,generation=excluded.generation,evidence=excluded.evidence,active=1",params![text(d,"entry_id")?,profile,catalog,text(d,"item_id")?,text(d,"role")?,scalar(&d["part"])?,text(d,"variant")?,text(plan,"policy_id")?,scalar(&selected["file_id"])?,scalar(&selected["revision"])?,scalar(&selected["tier"])?,scalar(&selected["tier_name"])?,scalar(&selected["path"])?,text(&selected,"state")?,effect.generation,encode(&selected["evidence"])])?;
 if changed(previous,&selected,true)?{let mut history=d.clone();history["phase"]=json!(if effect.retain{"retained"}else{"planned"});db.execute("INSERT INTO fallback_history(profile,catalog,entry_id,generation,decision,created_at) VALUES (?,?,?,?,?,?)",params![profile,catalog,text(d,"entry_id")?,effect.generation,encode(&history),crate::notifications::now()])?;}
 }
 if !effect.retain{for id in plan["removed"].as_array().unwrap(){db.execute("INSERT INTO fallback_history(profile,catalog,entry_id,generation,decision,created_at) VALUES (?,?,?,?,?,?)",params![profile,catalog,id.as_str(),effect.generation,encode(&json!({"entry_id":id,"state":"removed","phase":"planned","policy_id":plan["policy_id"]})),crate::notifications::now()])?;db.execute("UPDATE fallback_entries SET active=0,generation=? WHERE id=?",params![effect.generation,id.as_str()])?;}}
 db.execute("UPDATE fallback_bindings SET generation=? WHERE profile=? AND catalog=?",params![effect.generation,profile,catalog])?;Ok(effect.generation)
 })?;
    if plan["safe"] != true {
        store.transaction(|db| {
            for d in plan["desired"].as_array().unwrap() {
                let state = if text(d, "state")?.starts_with("resolved") {
                    json!("blocked")
                } else {
                    d["state"].clone()
                };
                if prepared.old.get(text(d, "entry_id")?).map(|p| &p["state"]) != Some(&state) {
                    crate::notifications::emit(
                        db,
                        profile,
                        "fallback_unresolved",
                        "warning",
                        catalog,
                        &json!([d["entry_id"], generation]),
                    )?;
                }
            }
            Ok(())
        })?;
    }
    Ok(AdmittedProjection {
        prepared,
        generation,
    })
}
pub fn finish(
    store: &Store,
    profile: &str,
    admitted: &AdmittedProjection,
    publication: &Value,
) -> Result<Value> {
    let prepared = &admitted.prepared;
    let plan = &prepared.plan;
    let catalog = text(plan, "catalog")?;
    if !admitted.is_safe() {
        return Ok(admitted.retained());
    }
    if store.database_id != prepared.database
        || profile != prepared.profile
        || binding(store, profile, catalog)?.is_none_or(|b| b["generation"] != admitted.generation)
    {
        return Err(Error("stale_resolution_generation".into()));
    }
    if publication["healthy"] == true {
        store.transaction(|db|{let empty=json!({});for d in plan["desired"].as_array().unwrap(){let previous=prepared.old.get(text(d,"entry_id")?).unwrap_or(&empty);if previous["file_id"]!=d["file_id"]{crate::notifications::emit(db,profile,"fallback_selected","info",catalog,&json!([d["entry_id"],admitted.generation]))?;}
 let changed=changed(previous,d,false)?;if changed||previous["published_generation"].as_i64().unwrap_or(0)==0||crate::exports::truth(&publication["applied"]){db.execute("UPDATE fallback_entries SET published_generation=generation WHERE id=?",[text(d,"entry_id")?])?;}
 if changed{let mut history=d.clone();history["phase"]=json!("verified");db.execute("INSERT INTO fallback_history(profile,catalog,entry_id,generation,decision,created_at) VALUES (?,?,?,?,?,?)",params![profile,catalog,text(d,"entry_id")?,admitted.generation,encode(&history),crate::notifications::now()])?;}}
 Ok(())})?;
    }
    let mut result = prepared.present();
    result["applied"] = json!(true);
    result["complete"] = json!(publication["healthy"] == true);
    result["publication"] = publication.clone();
    Ok(result)
}
