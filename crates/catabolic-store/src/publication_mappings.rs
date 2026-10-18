// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Ordinary version-two mapping admission, previews, and folder publication.
//! Existing owners evaluate the complete query and stage desired mappings.
//! Folder apply and recover publish through the shared native journal
//! executor (`public_reconcile::publish`), as frozen Python runs
//! `Reconciler.apply`/`recover`. Export and import publication stay gated:
//! they need official application execution and real-service acceptance.
use crate::{
    Error, Result, definitions,
    embedding::{Session, poll_interrupt},
    migration::rows,
};
use catabolic_core::{encode, sha256};
use serde_json::{Value, json};
use std::path::Path;

pub const PUBLICATION_GATE: &str = "native publication mapping apply/recover is blocked: filesystem publication and recovery are not yet qualified";
/// Frozen terminal event states.
const TERMINAL: [&str; 3] = ["verified", "submitted", "not_applied"];
pub const IMPORT_ACCOUNT_GATE: &str =
    "native Immich mapping preview requires the runtime account identity owner";
const MAX_PLAN: usize = 4 * 1024 * 1024;

fn fail<T>(code: &str) -> Result<T> {
    Err(Error(format!("mapping_{code}")))
}
fn text(value: &Value, maximum: usize) -> Result<&str> {
    value
        .as_str()
        .filter(|v| !v.is_empty() && v.chars().count() <= maximum && !v.chars().any(|c| c < ' '))
        .ok_or_else(|| Error("invalid_configuration".into()))
}
fn cancelled() -> Result<()> {
    if poll_interrupt() {
        return Err(Error("evaluation_cancelled".into()));
    }
    Ok(())
}

/// Preserve the frozen definition and option admission, including JSON scalar
/// types. A numeric 2.0 may dispatch here but is not a valid integer version.
pub fn validate(definition: &Value) -> Result<&Value> {
    if !definition.as_object().is_some_and(|o| {
        o.len() == 4
            && ["version", "id", "query", "destination"]
                .iter()
                .all(|k| o.contains_key(*k))
    }) || definition["version"].as_u64() != Some(2)
    {
        return fail("invalid_publication_definition");
    }
    text(&definition["id"], 255)?;
    text(&definition["query"], 255)?;
    let destination = &definition["destination"];
    if !destination.as_object().is_some_and(|o| {
        ["adapter", "operation", "catalog"]
            .iter()
            .all(|k| o.contains_key(*k))
    }) {
        return fail("invalid_publication_destination");
    }
    let adapter = text(&destination["adapter"], 255)?;
    let operation = text(&destination["operation"], 255)?;
    text(&destination["catalog"], 255)?;
    let supported = match operation {
        "folder" => crate::targets::definitions().get(adapter).is_some(),
        "export" => ["nfo", "opds", "xspf"].contains(&adapter),
        "import" => ["calibre", "calibre-web", "immich"].contains(&adapter),
        _ => false,
    };
    if !supported {
        return fail("unsupported_destination_operation");
    }
    let extras: &[&str] = match operation {
        "folder" => &["allow_empty"],
        "export" => &["path", "base_url"],
        _ => &["path"],
    };
    if destination.as_object().unwrap().keys().any(|k| {
        !["adapter", "operation", "catalog"].contains(&k.as_str()) && !extras.contains(&k.as_str())
    }) {
        return fail("unsupported_destination_options");
    }
    if operation != "folder" {
        text(&destination["path"], 4096)?;
    }
    if destination
        .get("allow_empty")
        .is_some_and(|v| !v.is_boolean())
    {
        return fail("invalid_allow_empty");
    }
    Ok(destination)
}

fn pending(session: &Session, profile: &str, scope: &Value) -> Result<Vec<Value>> {
    let mut events = Vec::new();
    for row in rows(
        &session.store()?.db,
        "SELECT id,subject FROM consumer_events WHERE profile=? AND event='mapping_publish' ORDER BY rowid",
        &[profile.to_owned().into()],
    )? {
        cancelled()?;
        let event = catabolic_core::decode_json(
            row["subject"]
                .as_str()
                .ok_or_else(|| Error("mapping history subject must be JSON text".into()))?,
        )?;
        let event = event
            .as_object()
            .ok_or_else(|| Error("mapping history subject must be a JSON object".into()))?;
        if event.get("version").and_then(Value::as_f64) != Some(2.0) {
            continue;
        }
        if event.contains_key("id") {
            return Err(Error("mapping history subject contains an id".into()));
        }
        events.push((row["id"].as_str().unwrap().to_owned(), event.clone()));
    }
    // Frozen history and scope selection finish before pending-state admission.
    // A malformed later record must not disappear behind an earlier outcome.
    let mut matching = Vec::new();
    for (id, event) in &events {
        let recorded_scope = event
            .get("scope")
            .ok_or_else(|| Error("mapping history event is missing scope".into()))?;
        if recorded_scope == scope {
            matching.push((id, event));
        }
    }
    let mut unresolved = None;
    for (id, event) in &matching {
        let state = event
            .get("state")
            .ok_or_else(|| Error("mapping history event is missing state".into()))?;
        if !["verified", "submitted", "not_applied"]
            .iter()
            .any(|terminal| state == *terminal)
            && unresolved.is_none()
        {
            unresolved = Some(id);
        }
    }
    if let Some(id) = unresolved {
        return fail(&format!("unresolved_write_recover_{id}"));
    }
    Ok(matching
        .into_iter()
        .map(|(id, event)| {
            let mut event = event.clone();
            event.insert("id".into(), json!(id));
            Value::Object(event)
        })
        .collect())
}

fn stage_folder(session: &mut Session, profile: &str, definition: &Value) -> Result<Value> {
    let destination = &definition["destination"];
    let catalog = destination["catalog"].as_str().unwrap();
    let layout_name = format!(
        "mapping-{}",
        &sha256(encode(&json!([
            definition["id"],
            destination["adapter"],
            catalog
        ])))[..24]
    );
    let store = session.store()?;
    let prior = definitions::projection_get(store, profile, catalog)?;
    if !prior["layout"].is_null() && prior["layout"] != layout_name {
        return fail("folder_owned_by_another_projection");
    }
    if crate::fallback_projection::binding(store, profile, catalog)?.is_some() {
        return fail("folder_has_fallback_owner");
    }
    let mode = rows(
        &store.db,
        "SELECT mode FROM catalog_link_modes WHERE catalog=?",
        &[catalog.to_owned().into()],
    )?;
    if mode.first().is_some_and(|row| row["mode"] != "symlink") {
        return fail("folder_mapping_requires_symlink_catalog");
    }
    definitions::dispatch(
        store,
        profile,
        "layout-put",
        &json!({"name":layout_name,"value":crate::targets::definitions()[destination["adapter"].as_str().unwrap()]}),
    )?;
    definitions::dispatch(
        store,
        profile,
        "projection-put",
        &json!({"catalog":catalog,"query_id":definition["query"],"layout":layout_name}),
    )?;
    let layout = crate::public_layouts::call(
        session,
        profile,
        "run",
        &json!({"name":layout_name,"catalog":catalog,"apply":true,"allow_empty":destination.get("allow_empty").unwrap_or(&json!(false)),"limit":1000}),
    )?;
    if layout["details_truncated"] == true {
        return fail("folder_plan_exceeds_limit");
    }
    cancelled()?;
    let store = session.store()?;
    store.db.progress_handler(1000, Some(poll_interrupt))?;
    let mut reconciliation =
        crate::public_reconcile::call(store, profile, "preview", &json!({"catalog":catalog}))?;
    let actions = reconciliation["actions"]
        .as_array()
        .ok_or_else(|| Error("publication reconciliation returned no action list".into()))?;
    if actions.len() > 1000 {
        return fail("folder_plan_exceeds_limit");
    }
    let blocked = actions
        .iter()
        .any(|action| action["kind"] == "blocked_source");
    // Python's zero-denominator branch returns integer zero, which is part of
    // the canonical plan identity as well as the portable report scalar types.
    if reconciliation["removal_budget"]["owned_entries"] == 0 {
        reconciliation["removal_budget"]["percent"] = json!(0);
    }
    let safe = layout["safe"] == true && reconciliation["safe"] == true && !blocked;
    let state = folder_state(store, profile, catalog)?;
    cancelled()?;
    Ok(json!({"layout":layout,"reconciliation":reconciliation,"safe":safe,"state":state}))
}

fn folder_state(store: &crate::Store, profile: &str, catalog: &str) -> Result<Value> {
    Ok(json!({
        "projection":definitions::projection_get(store, profile, catalog)?,
        "mappings":rows(&store.db, "SELECT * FROM mappings WHERE catalog=? ORDER BY id", &[catalog.to_owned().into()])?,
        "output":crate::observation::binding(store, profile, "output", catalog)?,
    }))
}

fn stable_document(document: &Value) -> Value {
    let mut document = document.clone();
    if let Some(object) = document.as_object_mut() {
        object.remove("generated_at");
    }
    document
}
fn plan_digest(plan: &Value) -> String {
    let mut plan = plan.clone();
    if plan.get("document").is_some() {
        plan["document"] = stable_document(&plan["document"]);
    }
    if plan["data"].is_object() && plan["data"].get("document").is_some() {
        plan["data"]["document"] = stable_document(&plan["data"]["document"]);
    } else if let Some(groups) = plan["data"].as_array_mut() {
        for group in groups {
            group["document"] = stable_document(&group["document"]);
        }
    }
    sha256(encode(&plan))
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value]> {
    value[key]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| Error(format!("invalid publication {key}")))
}
fn root_identity(path: &Path) -> Result<Value> {
    let file = crate::filesystem_mutation::open_root(
        path.to_str()
            .ok_or_else(|| Error("paths must be valid UTF-8".into()))?,
    )?;
    let (device, inode) = crate::filesystem_mutation::identity(&file.metadata()?);
    Ok(json!({"device":device,"inode":inode}))
}
fn group_document(document: &Value, associations: &[Value]) -> Result<Value> {
    use std::collections::BTreeSet;
    let mut subset = document.clone();
    let content = &mut subset["content"];
    let file_ids: BTreeSet<_> = associations
        .iter()
        .map(|a| a["file_id"].as_str().unwrap())
        .collect();
    let association_ids: BTreeSet<_> = associations
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    content["associations"] = json!(associations);
    content["files"]
        .as_array_mut()
        .unwrap()
        .retain(|f| file_ids.contains(f["id"].as_str().unwrap()));
    content["entries"]
        .as_array_mut()
        .unwrap()
        .retain_mut(|entry| {
            entry["association_ids"]
                .as_array_mut()
                .unwrap()
                .retain(|a| association_ids.contains(a.as_str().unwrap()));
            !entry["association_ids"].as_array().unwrap().is_empty()
        });
    let paths: BTreeSet<_> = content["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_owned())
        .collect();
    for key in ["recorded_links", "hardlinks", "retained_hardlinks"] {
        content[key]
            .as_array_mut()
            .unwrap()
            .retain(|e| paths.contains(e["path"].as_str().unwrap()));
    }
    content["taggings"].as_array_mut().unwrap().retain(|t| {
        t["subject_type"] != "file" || file_ids.contains(t["subject_id"].as_str().unwrap())
    });
    for key in content["counts"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>()
    {
        content["counts"][&key] = json!(array(content, &key)?.len());
    }
    subset["content_sha256"] = json!(sha256(encode(content)));
    crate::interchange::validate(&subset)?;
    Ok(subset)
}
fn import_signature(
    sources: &Value,
    items: &[Value],
    associations: &[Value],
    adapter: &str,
) -> String {
    let files: Vec<_> = sources
        .as_object()
        .unwrap()
        .values()
        .map(|source| {
            let mut value = serde_json::Map::new();
            for key in [
                "id", "location", "path", "root", "device", "inode", "size", "mtime_ns", "ctime_ns",
            ] {
                value.insert(key.into(), source[key].clone());
            }
            Value::Object(value)
        })
        .collect();
    let metadata: Vec<_> = items
        .iter()
        .map(|item| {
            let mut value = json!({"id":item["id"],"kind":item["kind"]});
            if adapter != "immich" {
                value["title"] = if crate::exports::truth(&item["metadata"]["title"]) {
                    item["metadata"]["title"].clone()
                } else {
                    item["id"].clone()
                };
                value["author"] = item["metadata"]["author"].clone();
            }
            value
        })
        .collect();
    let links: Vec<_> = associations.iter().map(|a| json!({"item_id":a["item_id"],"file_id":a["file_id"],"role":a["role"],"part":a["part"]})).collect();
    sha256(encode(&json!([files, metadata, links])))
}
fn submitted<'a>(past: &'a [Value], key: &str) -> Result<Option<&'a Value>> {
    let mut previous = None;
    for event in past {
        let recorded = event
            .get("key")
            .ok_or_else(|| Error("mapping history event is missing key".into()))?;
        if recorded == key && event["state"] == "submitted" {
            previous = Some(event);
        }
    }
    if previous.is_some_and(|event| event.get("signature").is_none()) {
        return Err(Error("mapping history event is missing signature".into()));
    }
    Ok(previous)
}

fn bundle_plan(
    session: &mut Session,
    profile: &str,
    definition: &Value,
    query: &Value,
    account: &mut dyn FnMut(&str) -> Result<Value>,
) -> Result<Value> {
    let dest = &definition["destination"];
    let adapter = dest["adapter"].as_str().unwrap();
    let catalog = dest["catalog"].as_str().unwrap();
    let token = session.begin()?;
    let document = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        session.with_evaluation(profile, &json!({}), 60_000, 100_000, |evaluation| {
            crate::manifest::build(
                evaluation,
                catalog,
                None,
                Some(&json!({"query_id":definition["query"],"profile":profile})),
            )
        })
    }));
    session.clear_interrupt_handler();
    let cleanup = session.finish(token, false);
    let document = match document {
        Ok(value) => {
            cleanup?;
            value?
        }
        Err(panic) => {
            let _ = cleanup;
            std::panic::resume_unwind(panic)
        }
    };
    if encode(&document).len() > MAX_PLAN {
        return fail("publication_plan_exceeds_limit");
    }
    let mut snapshots = serde_json::Map::new();
    for file in array(&document["content"], "files")? {
        cancelled()?;
        snapshots.insert(
            file["id"].as_str().unwrap().into(),
            crate::imports::source_snapshot(session.store()?, profile, file)?,
        );
    }
    let snapshots = Value::Object(snapshots);
    let mut plan = json!({"definition":definition,"database_id":session.store()?.database_id,"profile":profile,"query":query,"document":document,"sources":snapshots,"safe":true,"removals":0});
    if dest["operation"] == "export" {
        let output = std::path::absolute(dest["path"].as_str().unwrap())?;
        crate::export_bundle::destination(session.store()?, profile, "export", &output, false)?;
        let parent = root_identity(
            output
                .parent()
                .ok_or_else(|| Error("invalid output parent".into()))?,
        )?;
        let scope = json!(["export", output]);
        let past = pending(session, profile, &scope)?;
        let export = crate::exports::build(
            &document,
            adapter,
            dest.get("base_url")
                .filter(|v| adapter != "nfo" && !v.is_null())
                .map(|v| {
                    v.as_str().ok_or_else(|| {
                        Error("base URL must be an absolute HTTP(S) URL without credentials".into())
                    })
                })
                .transpose()?,
        )?;
        crate::export_bundle::preflight(session.store()?, profile, &document, &export, &output)?;
        let spec = json!({"document":document,"export":export,"sources":snapshots,"output":output,"parent":parent});
        let mut stable = spec.clone();
        stable["document"] = stable_document(&document);
        let signature = sha256(encode(&stable));
        let previous = past.iter().rev().find(|e| e["state"] == "verified");
        if let Some(event) = previous {
            if event["signature"] != signature || event["owner"] != definition["id"] {
                return fail("immutable_export_choose_new_destination");
            }
            if !crate::export_bundle::verify(event)? {
                return fail("owned_export_changed");
            }
        } else if std::fs::symlink_metadata(&output).is_ok() {
            return fail("export_destination_exists");
        }
        plan["scope"] = scope;
        plan["data"] = spec;
        plan["signature"] = json!(signature);
        plan["unchanged"] = json!(previous.is_some());
    } else {
        let prepared = crate::imports::prepare(
            session.store()?,
            profile,
            &json!({"document":document,"target":adapter,"destination":dest["path"],"limit":1000}),
        )?;
        let destination = &prepared.destination;
        let parent = if adapter == "immich" {
            Value::Null
        } else {
            root_identity(
                Path::new(destination)
                    .parent()
                    .ok_or_else(|| Error("invalid import parent".into()))?,
            )?
        };
        let mut scope = json!([
            "import",
            if adapter == "calibre-web" {
                "calibre"
            } else {
                adapter
            },
            destination
        ]);
        let destination_identity = if adapter == "immich" {
            let legacy = pending(session, profile, &scope)?;
            if legacy.iter().any(|e| e["state"] == "submitted") {
                return fail("legacy_import_account_identity_unknown");
            }
            cancelled()?;
            let identity = account(destination)?;
            let id = identity["user_id"]
                .as_str()
                .filter(|s| !s.is_empty() && s.chars().count() <= 128)
                .ok_or_else(|| Error("mapping_invalid_import_account_identity".into()))?;
            scope.as_array_mut().unwrap().push(json!(id));
            json!({"user_id":id})
        } else if std::fs::symlink_metadata(destination).is_ok() {
            root_identity(Path::new(destination))?
        } else {
            Value::Null
        };
        let past = pending(session, profile, &scope)?;
        if !parent.is_null()
            && let Some(event) = past.iter().rev().find(|e| e["state"] == "submitted")
            && event["destination_identity"] != destination_identity
        {
            return fail("import_destination_identity_changed");
        }
        let items: std::collections::BTreeMap<_, _> = array(&document["content"], "items")?
            .iter()
            .map(|i| (i["id"].as_str().unwrap(), i))
            .collect();
        let mut groups: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
        for association in array(&document["content"], "associations")? {
            let item = items[association["item_id"].as_str().unwrap()];
            let kind = item["kind"].as_str().unwrap();
            if association["role"] != "primary"
                || !(if adapter == "immich" {
                    ["photo", "video"].contains(&kind)
                } else {
                    ["book", "book_edition"].contains(&kind)
                })
            {
                return fail("unsupported_import_selection");
            }
            let key = association[if adapter == "immich" {
                "file_id"
            } else {
                "item_id"
            }]
            .as_str()
            .unwrap();
            groups
                .entry(key.into())
                .or_default()
                .push(association.clone());
        }
        let mut planned = Vec::new();
        let mut bytes = 0;
        for (key, associations) in groups {
            cancelled()?;
            let subset = group_document(&document, &associations)?;
            let file_ids: std::collections::BTreeSet<_> = associations
                .iter()
                .map(|a| a["file_id"].as_str().unwrap())
                .collect();
            let item_ids: std::collections::BTreeSet<_> = associations
                .iter()
                .map(|a| a["item_id"].as_str().unwrap())
                .collect();
            let sources = Value::Object(
                snapshots
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(k, _)| file_ids.contains(k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            );
            let selected: Vec<_> = array(&subset["content"], "items")?
                .iter()
                .filter(|i| item_ids.contains(i["id"].as_str().unwrap()))
                .cloned()
                .collect();
            let signature = import_signature(&sources, &selected, &associations, adapter);
            let previous = submitted(&past, &key)?;
            if previous.is_some_and(|e| e["signature"] != signature) {
                return fail("import_revision_changed_updates_unsupported");
            }
            let group = json!({"key":key,"document":subset,"sources":sources,"signature":signature,"unchanged":previous.is_some()});
            bytes += encode(&group).len();
            if bytes > MAX_PLAN {
                return fail("publication_plan_exceeds_limit");
            }
            planned.push(group);
        }
        plan["executable"] = json!(crate::imports::executable(prepared.executable));
        plan["destination_identity"] = destination_identity;
        plan["scope"] = scope;
        plan["destination"] = json!(destination);
        plan["parent"] = parent;
        plan["data"] = json!(planned);
        plan["actions"] = prepared.report["actions"].clone();
    }
    Ok(plan)
}

/// Lend an existing owner so an embedding can see its exact current state.
/// Staging always rolls back on successful reports, errors, cancellation, panic,
/// and plan-size refusal. Nested caller state is left to the existing Session.
pub fn preview(session: &mut Session, profile: &str, definition: &Value) -> Result<Value> {
    preview_with_account(session, profile, definition, &mut |_| {
        Err(Error(IMPORT_ACCOUNT_GATE.into()))
    })
}

pub fn preview_with_account(
    session: &mut Session,
    profile: &str,
    definition: &Value,
    account: &mut dyn FnMut(&str) -> Result<Value>,
) -> Result<Value> {
    cancelled()?;
    let destination = validate(definition)?;
    let query = definitions::query_get(
        session.store()?,
        profile,
        definition["query"].as_str().unwrap(),
    )?;
    if query["definition"]["mode"] != "selection" {
        return fail("publication_requires_selection_query");
    }
    if destination["operation"] != "folder" {
        let plan = bundle_plan(session, profile, definition, &query, account)?;
        return report(definition, plan);
    }
    let catalog = destination["catalog"].as_str().unwrap();
    definitions::require_recovered(session.store()?)?;
    let bound = crate::observation::binding(session.store()?, profile, "output", catalog)?;
    crate::observation::checked_root(session.store()?, profile, &bound)?;
    let scope = json!(["folder", catalog]);
    pending(session, profile, &scope)?;
    let database_id = session.store()?.database_id.clone();
    let token = session.begin()?;
    let staged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        stage_folder(session, profile, definition)
    }));
    // Clear the evaluator's callback before cleanup, even after cancellation.
    session.clear_interrupt_handler();
    let cleanup = session.finish(token, false);
    let staged = match staged {
        Ok(value) => {
            cleanup?;
            value?
        }
        Err(panic) => {
            let _ = cleanup;
            std::panic::resume_unwind(panic)
        }
    };
    let plan = json!({"definition":definition,"database_id":database_id,"profile":profile,"query":query,
        "scope":scope,"safe":staged["safe"],"removals":staged["reconciliation"]["removal_budget"]["removals"],"data":staged});
    report(definition, plan)
}

fn report(definition: &Value, plan: Value) -> Result<Value> {
    if encode(&plan).len() > MAX_PLAN {
        return fail("publication_plan_exceeds_limit");
    }
    cancelled()?;
    Ok(
        json!({"plan_id":plan_digest(&plan),"mapping":definition["id"],"operation":definition["destination"]["operation"],"complete":plan["safe"],"safe":plan["safe"],"applied":false,"removals":plan["removals"],"plan":plan,"results":[]}),
    )
}

pub fn run(database: &Path, profile: &str, definition: &Value, options: &Value) -> Result<Value> {
    run_with_account(database, profile, definition, options, &mut |_| {
        Err(Error(IMPORT_ACCOUNT_GATE.into()))
    })
}

pub fn run_with_account(
    database: &Path,
    profile: &str,
    definition: &Value,
    options: &Value,
    account: &mut dyn FnMut(&str) -> Result<Value>,
) -> Result<Value> {
    let apply = options["apply"] == true;
    let expected = &options["expected_plan"];
    if apply && !truthy(expected) {
        return fail("apply_requires_expected_plan");
    }
    let budget = options
        .get("max_removals")
        .cloned()
        .unwrap_or_else(|| json!(0));
    let nonnegative_integer = budget.as_u64().is_some()
        || budget
            .as_number()
            .is_some_and(|n| n.to_string().bytes().all(|b| b.is_ascii_digit()));
    if !nonnegative_integer {
        return fail("invalid_removal_budget");
    }
    let mut session = Session::open(database, true, false)?;
    crate::query::CatalogQuery::new(session.store()?, profile)?;
    let mut result = preview_with_account(&mut session, profile, definition, account)?;
    if !apply {
        return Ok(result);
    }
    if *expected != result["plan_id"] {
        return fail("stale_plan_preview_again");
    }
    if result["safe"] != true {
        return Ok(result);
    }
    // A budget too large for u64 can never be exceeded.
    let removals = result["removals"].as_u64().unwrap_or(0);
    if budget.as_u64().is_some_and(|budget| removals > budget) {
        return fail("removal_budget_exceeded");
    }
    if definition["destination"]["operation"] != "folder" {
        // Export and import publication run official applications and stay
        // gated until real-service acceptance; nothing was effected.
        return Err(Error(PUBLICATION_GATE.into()));
    }
    apply_folder(&mut session, profile, definition, &mut result, &budget)?;
    Ok(result)
}

/// Python truthiness of a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

fn record(session: &Session, profile: &str, id: &str, payload: &Value, create: bool) -> Result<()> {
    session.store()?.transaction(|db| {
        if create {
            db.execute(
                "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,'mapping_publish','info',?)",
                rusqlite::params![id, profile, encode(payload)],
            )?;
        } else {
            let severity = if TERMINAL.iter().any(|state| payload["state"] == *state) {
                "info"
            } else {
                "warning"
            };
            db.execute(
                "UPDATE consumer_events SET subject=?,severity=? WHERE id=? AND profile=? AND event='mapping_publish'",
                rusqlite::params![encode(payload), severity, id, profile],
            )?;
        }
        Ok(())
    })
}

fn event_base(definition: &Value, plan: &Value, key: &str) -> Value {
    json!({
        "version":2,
        "owner":definition["id"],
        "key":key,
        "definition":definition,
        "scope":plan["scope"],
        "state":"started",
        "plan_id":plan_digest(plan),
        "database_id":plan["database_id"],
    })
}

/// Frozen folder apply: restage and record the event in one transaction, then
/// publish through the shared executor and verify. Any failure after the
/// event is recorded leaves it `uncertain` for recovery.
fn apply_folder(
    session: &mut Session,
    profile: &str,
    definition: &Value,
    result: &mut Value,
    budget: &Value,
) -> Result<()> {
    let catalog = definition["destination"]["catalog"]
        .as_str()
        .unwrap()
        .to_owned();
    let identifier = uuid::Uuid::new_v4().to_string();
    let plan = result["plan"].clone();
    let token = session.begin()?;
    let staged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        stage_folder(session, profile, definition)
    }));
    session.clear_interrupt_handler();
    let staged = match staged {
        Ok(Ok(staged)) => staged,
        Ok(Err(error)) => {
            let _ = session.finish(token, false);
            return Err(error);
        }
        Err(panic) => {
            let _ = session.finish(token, false);
            std::panic::resume_unwind(panic)
        }
    };
    let mut event = event_base(definition, &plan, &catalog);
    let written = if staged != plan["data"] {
        fail("folder_changed_preview_again")
    } else {
        event["data"] = staged;
        record(session, profile, &identifier, &event, true)
    };
    if let Err(error) = written {
        let _ = session.finish(token, false);
        return Err(error);
    }
    session.finish(token, true)?;
    // Frozen's `try` covers publication, verification and the verified
    // record; an ordinary failure anywhere there records `uncertain`.
    let published = (|| -> Result<(Value, Value)> {
        let store = session.store()?;
        let execution = crate::public_reconcile::publish(
            store,
            profile,
            "apply",
            &json!({"catalog":catalog,"max_removals":budget,"notify_consumers":true}),
            &|_| Ok(()),
        )?;
        let verified =
            crate::public_reconcile::call(store, profile, "verify", &json!({"catalog":catalog}))?;
        if execution["healthy"] != true || verified["healthy"] != true {
            return fail("folder_verification_failed");
        }
        let mut done = event.clone();
        done["state"] = json!("verified");
        record(session, profile, &identifier, &done, false)?;
        Ok((execution, verified))
    })();
    match published {
        Ok((execution, verified)) => {
            event["state"] = json!("verified");
            result["applied"] = json!(true);
            result["execution"] = execution;
            result["verification"] = verified;
        }
        // An interrupt is not an ordinary failure: like a frozen
        // KeyboardInterrupt it propagates and leaves the event `started`.
        Err(error) if error.0 == crate::filesystem_mutation::PUBLICATION_INTERRUPTED => {
            return Err(error);
        }
        Err(_) => {
            event["state"] = json!("uncertain");
            record(session, profile, &identifier, &event, false)?;
            result["complete"] = json!(false);
            result["event_id"] = json!(identifier);
            result["state"] = json!("uncertain");
            result["safe_to_retry"] = json!(false);
        }
    }
    result["results"]
        .as_array_mut()
        .unwrap()
        .push(json!({"event_id":identifier,"state":event["state"]}));
    Ok(())
}

/// Frozen `publication_mappings.recover` for a version-two event. Folder
/// events recover through the shared executor; export and import recovery
/// stay gated with their publication.
pub fn recover(database: &Path, profile: &str, identifier: &str, request: &Value) -> Result<Value> {
    let apply = request["apply"] == true;
    let resolution = &request["resolution"];
    let session = Session::open(database, true, false)?;
    let mut found = Vec::new();
    for row in rows(
        &session.store()?.db,
        "SELECT id,subject FROM consumer_events WHERE profile=? AND event='mapping_publish' ORDER BY rowid",
        &[profile.to_owned().into()],
    )? {
        let subject = catabolic_core::decode_json(
            row["subject"]
                .as_str()
                .ok_or_else(|| Error("mapping history subject must be JSON text".into()))?,
        )?;
        if subject["version"].as_f64() == Some(2.0) && row["id"] == identifier {
            let mut event = subject;
            event["id"] = row["id"].clone();
            found.push(event);
        }
    }
    if found.len() != 1 {
        return fail("unknown_event");
    }
    let mut event = found.pop().unwrap();
    let plan_id = sha256(encode(&event));
    if event["database_id"] != json!(session.store()?.database_id) {
        return fail("event_database_changed");
    }
    if TERMINAL.iter().any(|state| event["state"] == *state) {
        return Ok(json!({"event_id":identifier,"state":event["state"],"complete":true}));
    }
    let operation = event["definition"]["destination"]["operation"].clone();
    if operation == "import" {
        // Import recovery probes the destination identity first; it stays
        // gated with import publication.
        return Err(Error(PUBLICATION_GATE.into()));
    }
    if !resolution.is_null() {
        return fail("unsupported_recovery_resolution");
    }
    if operation != "folder" {
        return Err(Error(PUBLICATION_GATE.into()));
    }
    let catalog = event["definition"]["destination"]["catalog"]
        .as_str()
        .ok_or_else(|| Error("mapping event catalog must be text".into()))?
        .to_owned();
    let store = session.store()?;
    if folder_state(store, profile, &catalog)? != event["data"]["state"] {
        return fail("folder_configuration_changed");
    }
    let target = json!({"catalog":catalog});
    if apply {
        crate::public_reconcile::publish(
            store,
            profile,
            "recover",
            &json!({"catalog":catalog,"cancel_unapplied":false}),
            &|_| Ok(()),
        )?;
        let remaining = crate::public_reconcile::call(store, profile, "preview", &target)?;
        let authorized = event["data"]["reconciliation"]["actions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let unauthorized = remaining["actions"].as_array().is_none_or(|actions| {
            actions
                .iter()
                .any(|action| action["kind"] != "unchanged" && !authorized.contains(action))
        });
        if remaining["safe"] != true || unauthorized {
            return fail("folder_recovery_requires_new_plan");
        }
        crate::public_reconcile::publish(
            store,
            profile,
            "apply",
            &json!({"catalog":catalog,"max_removals":event["data"]["reconciliation"]["removal_budget"]["removals"],"notify_consumers":true}),
            &|_| Ok(()),
        )?;
    }
    let state =
        if crate::public_reconcile::call(store, profile, "verify", &target)?["healthy"] == true {
            "verified"
        } else {
            "uncertain"
        };
    let data = event["data"].clone();
    if apply {
        event.as_object_mut().unwrap().remove("id");
        event["state"] = json!(state);
        record(&session, profile, identifier, &event, false)?;
    }
    Ok(json!({
        "event_id":identifier,
        "state":state,
        "complete":TERMINAL.contains(&state),
        "plan_id":plan_id,
        "recorded":apply,
        "requires_destination_inspection":false,
        "destination_identity":event.get("destination_identity").cloned().unwrap_or(Value::Null),
        "data":data,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn fixture(count: usize) -> (tempfile::TempDir, Session, Value) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let output = root.path().join("output");
        std::fs::create_dir(&output).unwrap();
        let metadata = output.metadata().unwrap();
        let session = Session::open(&path, true, false).unwrap();
        session.store().unwrap().transaction(|db| {
            db.execute("INSERT INTO bindings(profile,kind,owner,root,device,inode) VALUES ('default','output','global',?,?,?)", rusqlite::params![output.to_str(),metadata.dev() as i64,metadata.ino() as i64])?;
            db.execute("INSERT INTO locations(id) VALUES ('offline')", [])?;
            for n in 0..count {
                db.execute("INSERT INTO files(id,location,path) VALUES (?,'offline',?)", [format!("f{n}"), format!("book{n}.epub")])?;
                db.execute("INSERT INTO items(id,kind,metadata) VALUES (?,'book',?)", [format!("i{n}"), encode(&json!({"title":format!("Book {n}"),"author":"Author"}))])?;
                db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES (?,?,?,'primary','explicit')", [format!("a{n}"),format!("f{n}"),format!("i{n}")])?;
            }
            Ok(())
        }).unwrap();
        let query = definitions::query_put(session.store().unwrap(), "default", "selected", &json!({"mode":"selection","selection":{"language":"sql","query":"SELECT item_id FROM catalog_items"}})).unwrap();
        let definition = json!({"version":2,"id":"books","query":query["id"],"destination":{"adapter":"komga","operation":"folder","catalog":"global","allow_empty":true}});
        (root, session, definition)
    }

    fn state(session: &Session) -> Value {
        let store = session.store().unwrap();
        let mut result = serde_json::Map::new();
        for row in rows(&store.db,"SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",&[]).unwrap() {
            let name = row["name"].as_str().unwrap();
            result.insert(name.into(), json!(rows(&store.db,&format!("SELECT * FROM \"{name}\""),&[]).unwrap()));
        }
        Value::Object(result)
    }

    #[test]
    fn success_and_nested_caller_state_are_rollback_only() {
        let (root, mut session, definition) = fixture(0);
        let outer = session.begin().unwrap();
        session
            .store()
            .unwrap()
            .db
            .execute(
                "INSERT INTO meta(key,value) VALUES ('caller','uncommitted')",
                [],
            )
            .unwrap();
        let before = state(&session);
        let first = preview(&mut session, "default", &definition).unwrap();
        assert_eq!(first["safe"], true);
        assert_eq!(first["plan"]["data"]["layout"]["applied"], true);
        assert_eq!(state(&session), before);
        assert_eq!(
            preview(&mut session, "default", &definition).unwrap(),
            first
        );
        assert_eq!(state(&session), before);
        assert!(!session.store().unwrap().db.is_autocommit());
        session.finish(outer, false).unwrap();
        assert_eq!(
            std::fs::read_dir(root.path().join("output"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn full_selection_detail_limit_refuses_after_staging_and_rolls_back() {
        let (_root, mut session, definition) = fixture(1001);
        let before = state(&session);
        assert_eq!(
            preview(&mut session, "default", &definition).unwrap_err().0,
            "mapping_folder_plan_exceeds_limit"
        );
        assert_eq!(state(&session), before);
        assert!(session.store().unwrap().db.is_autocommit());
    }

    #[test]
    fn new_adapter_staging_error_rolls_back_and_releases_scope() {
        let (_root, mut session, definition) = fixture(1);
        session.store().unwrap().db.execute_batch("CREATE TRIGGER reject_staging BEFORE INSERT ON mappings BEGIN SELECT RAISE(ABORT,'adapter staging refusal'); END").unwrap();
        let before = state(&session);
        let error = preview(&mut session, "default", &definition).unwrap_err().0;
        assert!(error.contains("adapter staging refusal"), "{error}");
        assert_eq!(state(&session), before);
        assert!(session.store().unwrap().db.is_autocommit());
        session
            .store()
            .unwrap()
            .db
            .execute_batch("DROP TRIGGER reject_staging")
            .unwrap();
    }

    #[test]
    fn malformed_v2_history_refuses_before_staging() {
        let (_root, mut session, definition) = fixture(0);
        session.store().unwrap().db.execute("INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES ('prior','default','mapping_publish','info',?)", [encode(&json!({"version":2,"scope":["folder","global"],"state":"uncertain"}))]).unwrap();
        for (subject, expected) in [
            (json!([]), "mapping history subject must be a JSON object"),
            (
                json!({"version":2,"id":"other","scope":["folder","global"],"state":"verified"}),
                "mapping history subject contains an id",
            ),
            (
                json!({"version":2,"state":"verified"}),
                "mapping history event is missing scope",
            ),
            (
                json!({"version":2,"scope":["folder","global"]}),
                "mapping history event is missing state",
            ),
        ] {
            session.store().unwrap().db.execute("INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES ('malformed','default','mapping_publish','info',?)", [encode(&subject)]).unwrap();
            let before = state(&session);
            assert_eq!(
                preview(&mut session, "default", &definition).unwrap_err().0,
                expected
            );
            assert_eq!(state(&session), before);
            session
                .store()
                .unwrap()
                .db
                .execute("DELETE FROM consumer_events WHERE id='malformed'", [])
                .unwrap();
        }
    }

    #[test]
    fn import_history_never_skips_a_later_malformed_record() {
        let valid = json!({"key":"book","state":"submitted","signature":"signed"});
        for malformed in [json!({"state":"not_applied"}), json!({"state":"submitted"})] {
            assert_eq!(
                submitted(&[valid.clone(), malformed], "book")
                    .unwrap_err()
                    .0,
                "mapping history event is missing key"
            );
        }
        assert_eq!(
            submitted(
                &[valid.clone(), json!({"key":"book","state":"submitted"})],
                "book"
            )
            .unwrap_err()
            .0,
            "mapping history event is missing signature"
        );
        assert_eq!(
            submitted(std::slice::from_ref(&valid), "book").unwrap(),
            Some(&valid)
        );
    }

    #[test]
    fn bundle_nested_snapshot_and_account_failure_leave_caller_state() {
        let (root, mut session, mut definition) = fixture(0);
        definition["destination"] = json!({"adapter":"nfo","operation":"export","catalog":"global","path":root.path().join("bundle")});
        let outer = session.begin().unwrap();
        session
            .store()
            .unwrap()
            .db
            .execute(
                "INSERT INTO meta(key,value) VALUES ('caller','uncommitted')",
                [],
            )
            .unwrap();
        let before = state(&session);
        let first = preview(&mut session, "default", &definition).unwrap();
        assert_eq!(
            first["plan_id"],
            preview(&mut session, "default", &definition).unwrap()["plan_id"]
        );
        assert_eq!(state(&session), before);
        assert!(!root.path().join("bundle").exists());
        definition["destination"] = json!({"adapter":"immich","operation":"import","catalog":"global","path":"http://127.0.0.1:1"});
        let error = preview_with_account(&mut session, "default", &definition, &mut |_| {
            Err(Error("toy account refusal".into()))
        })
        .unwrap_err();
        assert_eq!(error.0, "toy account refusal");
        assert_eq!(state(&session), before);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = preview_with_account(&mut session, "default", &definition, &mut |_| {
                panic!("toy callback unwind")
            });
        }));
        assert!(panic.is_err());
        assert_eq!(state(&session), before);
        assert!(!session.store().unwrap().db.is_autocommit());
        session.finish(outer, false).unwrap();
    }

    #[test]
    fn bundle_document_limit_refuses_before_source_or_identity_access() {
        let (root, mut session, mut definition) = fixture(1);
        session.store().unwrap().db.execute("INSERT INTO mappings(id,catalog,file_id,item_id,path) VALUES ('m','global','f0','i0','book.epub')", []).unwrap();
        session
            .store()
            .unwrap()
            .db
            .execute(
                "UPDATE items SET metadata=?",
                [encode(&json!({"title":"x".repeat(MAX_PLAN)}))],
            )
            .unwrap();
        definition["destination"] = json!({"adapter":"immich","operation":"import","catalog":"global","path":"http://127.0.0.1:1"});
        let before = state(&session);
        let error = preview_with_account(&mut session, "default", &definition, &mut |_| {
            panic!("identity must not be read")
        })
        .unwrap_err();
        assert_eq!(error.0, "mapping_publication_plan_exceeds_limit");
        assert_eq!(state(&session), before);
        assert!(session.store().unwrap().db.is_autocommit());
        assert_eq!(
            std::fs::read_dir(root.path().join("output"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn bundle_cancellation_keeps_rollback_and_effect_gate() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let (_root, mut session, mut definition) = fixture(0);
        definition["destination"] = json!({"adapter":"immich","operation":"import","catalog":"global","path":"http://127.0.0.1:1"});
        let before = state(&session);
        let flag = Arc::new(AtomicBool::new(false));
        let check = flag.clone();
        let error = crate::embedding::with_interrupt_check(
            move || check.load(Ordering::Acquire),
            || {
                preview_with_account(&mut session, "default", &definition, &mut |_| {
                    flag.store(true, Ordering::Release);
                    Ok(json!({"user_id":"toy-account"}))
                })
            },
        )
        .unwrap_err();
        assert_eq!(error.0, "evaluation_cancelled");
        assert_eq!(state(&session), before);
        assert!(session.store().unwrap().db.is_autocommit());
    }

    #[test]
    fn admission_scalar_types_and_explicit_effect_gates() {
        let root = tempfile::tempdir().unwrap();
        let absent = root.path().join("absent.db");
        // Frozen order: an apply without a truthy expected plan is refused
        // before the removal budget or the database is examined.
        for expected in [Value::Null, json!(""), json!(0), json!(false)] {
            assert_eq!(
                run(
                    &absent,
                    "default",
                    &Value::Null,
                    &json!({"apply":true,"expected_plan":expected,"max_removals":-1})
                )
                .unwrap_err()
                .0,
                "mapping_apply_requires_expected_plan"
            );
        }
        // A private option cannot open anything; an absent catalog refuses.
        assert!(
            run(
                &absent,
                "default",
                &Value::Null,
                &json!({"apply":true,"disposable_fixture":true,"expected_plan":"anything","max_removals":100000})
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        for value in [json!(true), json!(0.0), json!(-1), Value::Null] {
            assert_eq!(
                run(
                    &absent,
                    "default",
                    &Value::Null,
                    &json!({"max_removals":value})
                )
                .unwrap_err()
                .0,
                "mapping_invalid_removal_budget"
            );
        }
        let mut definition = json!({"version":2,"id":"mapping","query":"query","destination":{"adapter":"komga","operation":"folder","catalog":"global"}});
        assert!(validate(&definition).is_ok());
        definition["version"] = json!(2.0);
        assert_eq!(
            validate(&definition).unwrap_err().0,
            "mapping_invalid_publication_definition"
        );
    }
}
