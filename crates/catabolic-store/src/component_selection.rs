// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Revision-pinned component package capture. SQL ownership stays in Store;
//! live checks are requested through the resolver's detached, bounded adapter.
use crate::{
    Error, Result, Store, component_mutation, components,
    fallback_resolution::{Context, Resolver},
    migration::rows,
    query,
};
use catabolic_core::{component, decode_json, encode, sha256};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| Error(format!("invalid component package {key}")))
}
fn get_occurrence(
    resolver: &mut Resolver<'_, '_>,
    id: &str,
    require_current: bool,
) -> Result<Value> {
    let row = resolver
        .rows(
            &format!(
                "SELECT * FROM ({}) WHERE profile=? AND occurrence_id=?",
                query::view_sql("catalog_component_occurrences")
            ),
            &[resolver.profile().to_owned().into(), id.to_owned().into()],
        )?
        .into_iter()
        .next()
        .ok_or_else(|| Error("unknown component occurrence in this profile".into()))?;
    let scoped = resolver.require_component_context();
    let mut occurrence = component_mutation::decode_occurrence(
        resolver.store(),
        resolver.profile(),
        row,
        require_current && scoped.is_ok(),
    )?;
    if let Err(error) = scoped {
        // Frozen scoped GraphQL masks the source identity. The primary must
        // refuse stale evidence; optional requirement candidates are skipped.
        occurrence["current"] = json!(false);
        if require_current {
            return Err(error);
        }
    }
    Ok(occurrence)
}
fn component_access(resolver: &mut Resolver<'_, '_>, id: &str) -> Result<bool> {
    match &mut resolver.context {
        Context::Http(access) => Ok(access.component(id)),
        _ => Ok(true),
    }
}
fn supporting(resolver: &mut Resolver<'_, '_>, value: &Value) -> Result<Option<Vec<Value>>> {
    let mut dependencies = vec![];
    for dep in value["dependencies"]
        .as_array()
        .ok_or_else(|| Error("invalid component dependencies".into()))?
    {
        let snap = components::occurrence(
            &resolver.store().db,
            resolver.profile(),
            text(dep, "file_id")?,
        )?;
        if components::revision(&snap) != dep["revision"] || snap["status"] != "present" {
            return Ok(None);
        }
        if let Context::Http(access) = &mut resolver.context {
            crate::content_access::authorize(
                access,
                text(dep, "file_id")?,
                text(dep, "revision")?,
            )?;
        }
        let mut dep = dep.clone();
        dep["snapshot"] = snap;
        dep["for_occurrence_id"] = value["occurrence_id"].clone();
        dependencies.push(dep);
    }
    Ok(Some(dependencies))
}

pub fn capture(
    resolver: &mut Resolver<'_, '_>,
    mut primary: Value,
    policy: &Value,
    check_live: bool,
) -> Result<Option<Value>> {
    resolver.admit_binding_read(text(&primary, "id")?)?;
    let config = &policy["package"];
    let mut videos = resolver.rows(
        &format!(
            "SELECT occurrence_id FROM ({}) WHERE profile=? AND association_id=? AND kind='video' AND current=1 ORDER BY occurrence_id LIMIT 257",
            query::view_sql("catalog_component_occurrences")
        ),
        &[
            resolver.profile().to_owned().into(),
            text(&primary, "id")?.to_owned().into(),
        ],
    )?;
    if primary["video_occurrence_id"]
        .as_str()
        .is_some_and(|v| !v.is_empty())
    {
        videos.retain(|r| r["occurrence_id"] == primary["video_occurrence_id"]);
    }
    if videos.is_empty() {
        return Ok(None);
    }
    if videos.len() != 1 {
        return Err(Error("ambiguous_video_component".into()));
    }
    let video = get_occurrence(resolver, text(&videos[0], "occurrence_id")?, true)?;
    if !component_access(resolver, text(&video, "occurrence_id")?)? {
        return Ok(None);
    }
    let mut selected = vec![
        json!({"requirement":"video","occurrence":video,"compatibility":component::compatible(&video,&video,&primary)}),
    ];
    let pinned = primary
        .as_object_mut()
        .ok_or_else(|| Error("invalid component primary".into()))?
        .remove("component_choices");
    let mut total = 0usize;
    let mut query_reports = vec![];
    let maximum = policy["budgets"]["max_candidates"]
        .as_u64()
        .ok_or_else(|| Error("invalid component candidate budget".into()))?
        as usize;
    let max_checks = policy["budgets"]["max_checks"]
        .as_u64()
        .ok_or_else(|| Error("invalid component check budget".into()))?
        as usize;
    for requirement in config["requirements"]
        .as_array()
        .ok_or_else(|| Error("invalid package requirements".into()))?
    {
        let mut chosen = None;
        for tier in requirement["fallbacks"]
            .as_array()
            .ok_or_else(|| Error("invalid component fallbacks".into()))?
        {
            let selection = resolver.query(text(tier, "query_id")?)?;
            if selection.report["complete"] != true
                || !matches!(selection.entity.as_str(), "component_id" | "occurrence_id")
            {
                return Err(Error(
                    "component_requirement_needs_complete_component_selection".into(),
                ));
            }
            let candidates = resolver.rows(
                &format!(
                    "SELECT occurrence_id FROM ({}) WHERE profile=? AND item_id=? AND {} IN (SELECT value FROM json_each(?)) AND current=1 ORDER BY occurrence_id LIMIT ?",
                    query::view_sql("catalog_component_occurrences"),
                    selection.entity
                ),
                &[
                    resolver.profile().to_owned().into(),
                    text(&primary, "item_id")?.to_owned().into(),
                    encode(&json!(selection.ids)).into(),
                    ((maximum + 1) as i64).into(),
                ],
            )?;
            total += candidates.len();
            if total > maximum {
                return Err(Error("component_candidate_budget".into()));
            }
            let mut usable = vec![];
            for row in candidates {
                if !component_access(resolver, text(&row, "occurrence_id")?)? {
                    continue;
                }
                let value = get_occurrence(resolver, text(&row, "occurrence_id")?, false)?;
                if value["current"] != true
                    || value["conflicts"].as_array().is_some_and(|a| !a.is_empty())
                {
                    continue;
                }
                if let Some(relation) = component::compatible(&value, &video, &primary) {
                    usable.push(json!({"requirement":requirement["name"],"occurrence":value,"compatibility":relation}));
                }
            }
            query_reports.push(json!({"requirement":requirement["name"],"query_id":tier["query_id"],"eligible":usable.len()}));
            let identities = usable
                .iter()
                .map(|v| encode(&v["occurrence"]["component_id"]))
                .collect::<BTreeSet<_>>();
            if identities.len() > 1 {
                return Err(Error(format!(
                    "ambiguous_component_requirement:{}",
                    text(requirement, "name")?
                )));
            }
            for candidate in usable {
                let value = &candidate["occurrence"];
                if let Some(pinned) = pinned.as_ref().filter(|v| !v.is_null())
                    && pinned[text(requirement, "name")?] != value["occurrence_id"]
                {
                    continue;
                }
                let Some(dependencies) = supporting(resolver, value)? else {
                    continue;
                };
                let mut snapshots = vec![value["snapshot"].clone()];
                snapshots.extend(dependencies.iter().map(|d| d["snapshot"].clone()));
                if snapshots.len() > max_checks {
                    return Err(Error("component_dependency_budget".into()));
                }
                if check_live
                    && !resolver.check(&json!(snapshots), text(&value["snapshot"], "location")?)?
                {
                    continue;
                }
                chosen = Some(candidate);
                break;
            }
            if chosen.is_some() {
                break;
            }
        }
        if let Some(chosen) = chosen {
            selected.push(chosen);
        } else if requirement["required"] == true {
            return Ok(None);
        }
    }
    let mut checks = primary["checks"]
        .as_array()
        .ok_or_else(|| Error("invalid primary checks".into()))?
        .clone();
    let mut dependencies = vec![];
    for entry in &selected {
        let value = &entry["occurrence"];
        checks.push(value["snapshot"].clone());
        let Some(supporting) = supporting(resolver, value)? else {
            return Ok(None);
        };
        checks.extend(supporting.iter().map(|d| d["snapshot"].clone()));
        dependencies.extend(supporting);
    }
    let mut seen = BTreeSet::new();
    checks.retain(|c| seen.insert(encode(c)));
    if checks.len() > max_checks {
        return Err(Error("component_dependency_budget".into()));
    }
    let packaging = component::decision(config, &selected, &dependencies);
    let mut package = json!({"version":1,"video":{"occurrence_id":video["occurrence_id"],"file_id":video["file_id"],"revision":video["revision"]},"components":selected,"dependencies":dependencies,"packaging":packaging});
    package["signature"] = json!(sha256(encode(&package)));
    package["queries"] = json!(query_reports);
    resolver.account_value(&package)?;
    if packaging["action"] == "block" {
        return Err(Error(text(&packaging, "reason")?.to_owned()));
    }
    primary["checks"] = json!(checks);
    primary["evidence"]["component_package"] = package.clone();
    if packaging["ready"] != true {
        return resolved_artifact(resolver, primary, package).map(Some);
    }
    Ok(Some(primary))
}

/// Reuse only a ready artifact bearing this exact captured component signature.
fn resolved_artifact(
    resolver: &mut Resolver<'_, '_>,
    mut primary: Value,
    mut package: Value,
) -> Result<Value> {
    let artifacts = rows(
        &resolver.store().db,
        "SELECT a.id FROM processing_artifacts a JOIN processing_jobs j ON j.id=a.job_id WHERE a.profile=? AND a.state='ready' AND json_extract(j.options,'$.component_package.signature')=? ORDER BY a.id LIMIT 2",
        &[
            resolver.profile().to_owned().into(),
            text(&package, "signature")?.to_owned().into(),
        ],
    )?;
    let Some(row) = artifacts.first() else {
        return Ok(primary);
    };
    let artifact =
        crate::artifact_mutation::get(resolver.store(), resolver.profile(), text(row, "id")?)?;
    if !crate::artifact_mutation::usable_metadata(resolver.store(), resolver.profile(), &artifact)?
    {
        return Ok(primary);
    }
    let file = rows(
        &resolver.store().db,
        "SELECT * FROM files WHERE id=?",
        &[text(&artifact, "file_id")?.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("missing component artifact file".into()))?;
    let rev = crate::content_access::revision_of(
        resolver.store(),
        resolver.profile(),
        text(&file, "id")?,
    )?;
    if let Context::Http(access) = &mut resolver.context {
        crate::content_access::authorize(access, text(&file, "id")?, &rev)?;
    }
    package["packaging"]["ready"] = json!(true);
    package["packaging"]["artifact_id"] = artifact["id"].clone();
    package["packaging"]["file_id"] = file["id"].clone();
    package["packaging"]["revision"] = json!(rev);
    primary["file_id"] = file["id"].clone();
    primary["path"] = file["path"].clone();
    primary["location"] = file["location"].clone();
    primary["revision"] = json!(rev);
    primary["rendition_id"] = rows(
        &resolver.store().db,
        "SELECT id FROM media_outputs WHERE artifact_id=?",
        &[text(&artifact, "id")?.to_owned().into()],
    )?
    .first()
    .ok_or_else(|| Error("missing component artifact lineage".into()))?["id"]
        .clone();
    primary["checks"]
        .as_array_mut()
        .unwrap()
        .push(components::occurrence(
            &resolver.store().db,
            resolver.profile(),
            text(&file, "id")?,
        )?);
    primary["evidence"]["component_package"] = package;
    Ok(primary)
}

/// Expand ready sidecars into the existing single publication plan and journal.
pub fn projection_members(store: &Store, profile: &str, result: &mut Value) -> Result<()> {
    let decisions = result["decisions"]
        .as_array()
        .ok_or_else(|| Error("invalid component decisions".into()))?
        .clone();
    let mut additional = vec![];
    for decision in decisions {
        let package = &decision["evidence"]["component_package"];
        if package.is_null()
            || package["packaging"]["ready"] != true
            || package["packaging"]["action"] != "publish_sidecars"
        {
            continue;
        }
        for entry in package["components"]
            .as_array()
            .ok_or_else(|| Error("invalid component package".into()))?
            .iter()
            .skip(1)
        {
            let value = &entry["occurrence"];
            if value["file_id"] == package["video"]["file_id"] {
                continue;
            }
            let association = rows(
                &store.db,
                "SELECT * FROM item_files WHERE id=?",
                &[text(value, "association_id")?.to_owned().into()],
            )?
            .into_iter()
            .next()
            .ok_or_else(|| Error("missing component association".into()))?;
            let mut metadata = decode_json(text(&association, "metadata")?)?;
            for key in ["language", "forced"] {
                if !value[key].is_null() {
                    metadata[key] = value[key].clone();
                }
            }
            let mut assets = vec![("component".to_owned(), text(value, "file_id")?.to_owned())];
            for dep in package["dependencies"]
                .as_array()
                .ok_or_else(|| Error("invalid component dependencies".into()))?
            {
                if dep["for_occurrence_id"] == value["occurrence_id"] {
                    assets.push((
                        text(dep, "purpose")?.to_owned(),
                        text(dep, "file_id")?.to_owned(),
                    ));
                }
            }
            for (purpose, file_id) in assets {
                let file = rows(
                    &store.db,
                    "SELECT * FROM files WHERE id=?",
                    &[file_id.clone().into()],
                )?
                .into_iter()
                .next()
                .ok_or_else(|| Error("missing component asset file".into()))?;
                let namespace = uuid::Uuid::parse_str(text(&decision, "entry_id")?)
                    .map_err(|e| Error(e.to_string()))?;
                let key = uuid::Uuid::new_v5(
                    &namespace,
                    encode(&json!([entry["requirement"], purpose])).as_bytes(),
                )
                .to_string();
                if result["captures"].get(&key).is_some() {
                    return Err(Error("ambiguous_component_supporting_asset".into()));
                }
                let revision = crate::content_access::revision_of(store, profile, &file_id)?;
                let mut capture = association.clone();
                for (field, value) in [
                    ("id", json!(key)),
                    ("file_id", json!(file_id)),
                    ("path", file["path"].clone()),
                    ("location", file["location"].clone()),
                    ("metadata", metadata.clone()),
                    ("revision", json!(revision)),
                    (
                        "checks",
                        json!([components::occurrence(&store.db, profile, &file_id)?]),
                    ),
                    ("evidence", json!({})),
                    ("rendition_id", Value::Null),
                ] {
                    capture[field] = value;
                }
                let mut child = decision.clone();
                for (field, value) in [
                    ("entry_id", json!(key)),
                    ("role", association["role"].clone()),
                    ("file_id", json!(file_id)),
                    ("revision", json!(revision)),
                    ("rendition_id", Value::Null),
                    (
                        "content_path",
                        json!(format!("/v1/files/{file_id}/content?revision={revision}")),
                    ),
                    (
                        "evidence",
                        json!({"component_package_signature":package["signature"],"occurrence_id":value["occurrence_id"],"parent_entry_id":decision["entry_id"]}),
                    ),
                ] {
                    child[field] = value;
                }
                result["captures"][&key] = capture;
                additional.push(child);
            }
        }
    }
    result["decisions"]
        .as_array_mut()
        .unwrap()
        .extend(additional);
    Ok(())
}

/// Reconstruct and compare the originally admitted package using the same saved
/// query tiers. This is metadata-only; the journal performs the final live group
/// check before output I/O.
pub fn publication_capture(
    store: &Store,
    profile: &str,
    mapping: &Value,
    policy: &Value,
    entity: &str,
    ids: &BTreeSet<String>,
) -> Result<Option<Value>> {
    let Some(mapping) = rows(
        &store.db,
        "SELECT * FROM fallback_entries WHERE id=? AND profile=?",
        &[
            text(mapping, "id")?.to_owned().into(),
            profile.to_owned().into(),
        ],
    )?
    .into_iter()
    .next() else {
        return Ok(None);
    };
    let evidence = decode_json(text(&mapping, "evidence")?)?;
    let parent = if let Some(id) = evidence["parent_entry_id"].as_str() {
        let Some(parent) = rows(
            &store.db,
            "SELECT * FROM fallback_entries WHERE profile=? AND catalog=? AND id=? AND active=1",
            &[
                profile.to_owned().into(),
                text(&mapping, "catalog")?.to_owned().into(),
                id.to_owned().into(),
            ],
        )?
        .into_iter()
        .next() else {
            return Ok(None);
        };
        parent
    } else {
        mapping.clone()
    };
    let parent_evidence = decode_json(text(&parent, "evidence")?)?;
    let package = &parent_evidence["component_package"];
    if package.is_null() || package["packaging"]["ready"] != true {
        return Ok(None);
    }
    let video = component_mutation::get(
        store,
        profile,
        text(&package["video"], "occurrence_id")?,
        true,
    )?;
    let eligible = json!({"file_id":video["file_id"],"item_id":video["item_id"],"association_id":video["association_id"],"component_id":video["component_id"],"occurrence_id":video["occurrence_id"]});
    if eligible[entity].as_str().is_none_or(|id| !ids.contains(id)) {
        return Ok(None);
    }
    let Some(mut primary)=rows(&store.db,"SELECT a.*,f.location,f.path FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.id=?",&[text(&video,"association_id")?.to_owned().into()])?.into_iter().next() else{return Ok(None);};
    primary["video_occurrence_id"] = video["occurrence_id"].clone();
    primary["component_choices"] = json!({});
    for entry in package["components"]
        .as_array()
        .ok_or_else(|| Error("invalid component package".into()))?
    {
        primary["component_choices"][text(entry, "requirement")?] =
            entry["occurrence"]["occurrence_id"].clone();
    }
    let mut probe = |_: &std::path::Path, _: &str, _: &str, _: &Value, _: u64| {
        Err(Error(
            "publication capture cannot perform live probes".into(),
        ))
    };
    if !store.db.is_autocommit() {
        return Err(Error(
            "metadata fallback query requires an autocommit owner".into(),
        ));
    }
    let snapshot = Store::open(&store.path, false, false)?;
    if snapshot.database_id != store.database_id {
        return Err(Error("stale_resolution_plan".into()));
    }
    let mut evaluation = crate::selection::Evaluation::new(
        snapshot,
        profile,
        policy["budgets"]["query_timeout_ms"]
            .as_u64()
            .unwrap_or(5000),
        policy["budgets"]["max_candidates"]
            .as_u64()
            .unwrap_or(10000) as usize,
        false,
    )?;
    let mut resolver = Resolver::new(Context::Local(&mut evaluation), &mut probe, None);
    let Some(accepted) =
        resolver.capture_with_checks(&primary, policy, text(&mapping, "catalog")?, false)?
    else {
        return Ok(None);
    };
    let fresh = &accepted["evidence"]["component_package"];
    if fresh["signature"] != package["signature"]
        || fresh["packaging"] != package["packaging"]
        || accepted["file_id"] != parent["file_id"]
    {
        return Ok(None);
    }
    if !evidence["parent_entry_id"].is_null() {
        let mut result = json!({"decisions":[{"entry_id":parent["id"],"evidence":{"component_package":fresh}}],"captures":{}});
        projection_members(store, profile, &mut result)?;
        let child = &result["captures"][text(&mapping, "id")?];
        if child.is_null()
            || child["file_id"] != mapping["file_id"]
            || child["revision"] != mapping["revision"]
        {
            return Ok(None);
        }
    }
    Ok(Some(accepted))
}
