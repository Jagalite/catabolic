// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Metadata revalidation for each logical journal boundary. The reconciler must
//! run every returned live group detached and require its epoch before I/O.
use crate::{
    Error, Result, Store, components, content_access, fallback_mutation, fallback_projection,
    fallback_resolution::{self, Context, Resolver},
    migration::rows,
    selection::{Evaluation, Selected},
};
use catabolic_core::{decode_json, encode};
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| Error(format!("invalid fallback mapping {key}")))
}
pub fn effective_mappings(store: &Store, profile: &str, catalog: &str) -> Result<Vec<Value>> {
    if fallback_projection::binding(store, profile, catalog)?.is_some() {
        rows(
            &store.db,
            "SELECT e.id,e.catalog,e.file_id,e.item_id,e.path,e.active,f.location,f.path AS source_path,o.status,o.size,o.mtime_ns,o.device,o.inode,e.revision,e.generation,e.policy_id,e.tier,e.role,e.part,e.variant FROM fallback_entries e JOIN files f ON f.id=e.file_id LEFT JOIN observations o ON o.file_id=f.id AND o.profile=e.profile WHERE e.profile=? AND e.catalog=? AND e.active=1 AND e.path IS NOT NULL ORDER BY e.path",
            &[profile.to_owned().into(), catalog.to_owned().into()],
        )
    } else {
        rows(
            &store.db,
            "SELECT m.*,f.location,f.path AS source_path,o.status,o.size,o.mtime_ns,o.device,o.inode FROM mappings m JOIN files f ON f.id=m.file_id LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? WHERE m.catalog=? AND m.active=1 ORDER BY m.path",
            &[profile.to_owned().into(), catalog.to_owned().into()],
        )
    }
}
#[derive(Default)]
pub struct Selections {
    epoch: Option<i64>,
    queries: BTreeMap<String, Selected>,
}
impl Selections {
    fn select(&mut self, store: &Store, profile: &str, epoch: i64, id: &str) -> Result<Selected> {
        if self.epoch != Some(epoch) {
            self.queries.clear();
            self.epoch = Some(epoch);
        }
        if let Some(selected) = self.queries.get(id) {
            return Ok(selected.clone());
        }
        let snapshot = Store::open(&store.path, false, false)?;
        if snapshot.database_id != store.database_id {
            return Err(Error("stale_resolution_plan".into()));
        }
        let mut evaluation = Evaluation::new(snapshot, profile, 5000, 10000, false)?;
        let selected = evaluation.select(&json!({"query_id":id,"profile":profile}))?;
        if selected.report["complete"] != true {
            return Err(Error("incomplete_publication_selection".into()));
        }
        self.queries.insert(id.into(), selected.clone());
        Ok(selected)
    }
}
pub struct Capture {
    pub snapshot: Value,
    pub checks: Value,
    pub timeout_ms: u64,
    pub epoch: i64,
}
/// Stale is a complete catalog proof that the admitted intent is obsolete.
/// Unavailable is a source verdict. Query/probe failures must remain errors and
/// must never be converted into permission to remove or cancel an intent.
pub enum Outcome {
    Ready(Capture),
    Unavailable(&'static str),
    Stale(&'static str),
}
pub fn capture(
    store: &Store,
    profile: &str,
    mapping: &Value,
    queries: &mut Selections,
) -> Result<Outcome> {
    if !store.db.is_autocommit() {
        return Err(Error(
            "fallback publication metadata requires an autocommit owner".into(),
        ));
    }
    let current_epoch = fallback_resolution::epoch(store)?;
    let current = rows(
        &store.db,
        "SELECT file_id,revision,generation,active,path FROM fallback_entries WHERE id=? AND profile=?",
        &[
            text(mapping, "id")?.to_owned().into(),
            profile.to_owned().into(),
        ],
    )?;
    if current.first().is_none_or(|r| {
        ["file_id", "revision", "generation", "active", "path"]
            .iter()
            .any(|key| r[*key] != mapping[*key])
    }) {
        return Err(Error("stale_resolution_generation".into()));
    }
    if content_access::revision_of(store, profile, text(mapping, "file_id")?)?
        != mapping["revision"]
    {
        return Ok(Outcome::Unavailable("fallback_revision_changed"));
    }
    let Some(config) = fallback_projection::binding(store, profile, text(mapping, "catalog")?)?
    else {
        return Ok(Outcome::Stale("stale_resolution_policy"));
    };
    if config["policy_id"] != mapping["policy_id"] {
        return Ok(Outcome::Stale("stale_resolution_policy"));
    }
    let policy =
        fallback_mutation::get(store, profile, text(mapping, "policy_id")?)?["definition"].clone();
    let membership = queries.select(store, profile, current_epoch, text(&config, "query_id")?)?;
    if membership.entity != "item_id" || !membership.ids.contains(text(mapping, "item_id")?) {
        return Ok(Outcome::Stale("stale_resolution_membership"));
    }
    let tier = mapping["tier"]
        .as_u64()
        .and_then(|i| {
            policy["fallbacks"]
                .as_array()
                .and_then(|v| v.get(i as usize))
        })
        .ok_or_else(|| Error("invalid fallback publication tier".into()))?;
    let selected = queries.select(store, profile, current_epoch, text(tier, "query_id")?)?;
    let accepted = if !policy["package"].is_null() {
        crate::component_selection::publication_capture(
            store,
            profile,
            mapping,
            &policy,
            &selected.entity,
            &selected.ids,
        )?
    } else {
        let key = match selected.entity.as_str() {
            "file_id" => "file_id",
            "item_id" => "item_id",
            "association_id" => "id",
            _ => return Err(Error("invalid publication selection entity".into())),
        };
        let associations = rows(
            &store.db,
            "SELECT a.*,f.location,f.path FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.item_id=? AND a.file_id=? AND a.role=? AND a.part IS ?",
            &[
                crate::sql::scalar(&mapping["item_id"])?,
                crate::sql::scalar(&mapping["file_id"])?,
                crate::sql::scalar(&mapping["role"])?,
                crate::sql::scalar(&mapping["part"])?,
            ],
        )?;
        let mut candidate = None;
        for row in associations {
            if row[key]
                .as_str()
                .is_some_and(|id| selected.ids.contains(id))
                && decode_json(text(&row, "metadata")?)?
                    .get("variant")
                    .cloned()
                    .unwrap_or(json!(""))
                    == mapping["variant"]
            {
                candidate = Some(row);
                break;
            }
        }
        let Some(candidate) = candidate else {
            return Ok(Outcome::Stale("stale_resolution_candidate"));
        };
        let mut probe = |_: &std::path::Path, _: &str, _: &str, _: &Value, _: u64| {
            Err(Error("metadata publication capture cannot probe".into()))
        };
        let mut resolver = Resolver::new(Context::Borrowed(store, profile), &mut probe, None);
        resolver.primary_choices=rows(&store.db,"SELECT item_id,file_id FROM fallback_entries WHERE profile=? AND catalog=? AND active=1 AND role='primary'",&[profile.to_owned().into(),text(mapping,"catalog")?.to_owned().into()])?.into_iter().filter(|r|r["file_id"].is_string()).map(|r|Ok((text(&r,"item_id")?.to_owned(),text(&r,"file_id")?.to_owned()))).collect::<Result<_>>()?;
        resolver.capture_with_checks(&candidate, &policy, text(mapping, "catalog")?, false)?
    };
    let Some(accepted) = accepted else {
        return Ok(Outcome::Stale("stale_resolution_evidence"));
    };
    let snapshot = components::occurrence(&store.db, profile, text(mapping, "file_id")?)?;
    let checks = accepted["checks"].clone();
    if checks
        .as_array()
        .is_none_or(|c| c.is_empty() || c.len() > 65)
        || encode(&checks).len() > 1024 * 1024
    {
        return Err(Error("probe_snapshot_limit".into()));
    }
    fallback_resolution::require_epoch(store, current_epoch)?;
    Ok(Outcome::Ready(Capture {
        snapshot,
        checks,
        timeout_ms: policy["budgets"]["probe_timeout_ms"].as_u64().unwrap(),
        epoch: current_epoch,
    }))
}
