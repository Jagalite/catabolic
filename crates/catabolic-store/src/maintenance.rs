// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Maintenance metadata and aggregates. Scanning and root admission remain
//! owned by observation; no filesystem publication is implemented here.
use crate::{Error, Result, Store, migration::rows};
use catabolic_core::{encode, sha256};
use rusqlite::params;
use serde_json::{Value, json};

pub const STATUSES: [&str; 6] = [
    "pending",
    "in_progress",
    "complete",
    "deferred",
    "ignored",
    "needs_attention",
];

pub fn check_root(store: &Store, profile: &str, kind: &str, owner: &str) -> Result<()> {
    let binding = crate::observation::binding(store, profile, kind, owner)?;
    crate::observation::checked_root(store, profile, &binding)?;
    Ok(())
}

/// A single page of the frozen stable-revision protocol. The caller streams
/// pages, retaining only its bounded processing selection, never all files.
pub fn stable_page(
    store: &Store,
    profile: &str,
    scan: &Value,
    after: &str,
    now: f64,
    settle: u64,
) -> Result<(Vec<String>, usize, Option<String>)> {
    stable_page_with_limit(store, profile, scan, after, now, settle, 1000)
}

/// The polling watch contract uses its processing batch size for each stable
/// revision transaction too. Maintenance retains its existing 1,000-row page.
pub fn stable_page_with_limit(
    store: &Store,
    profile: &str,
    scan: &Value,
    after: &str,
    now: f64,
    settle: u64,
    limit: usize,
) -> Result<(Vec<String>, usize, Option<String>)> {
    if !(1..=1000).contains(&limit) {
        return Err(Error("batch must be 1..1000".into()));
    }
    let page = rows(
        &store.db,
        "SELECT o.file_id,o.size,o.mtime_ns,o.device,o.inode,o.status,b.root,b.device AS root_device,b.inode AS root_inode FROM observations o JOIN files f ON f.id=o.file_id JOIN bindings b ON b.profile=o.profile AND b.kind='source' AND b.owner=f.location WHERE o.profile=? AND f.location=? AND o.scan_id=? AND o.file_id>? ORDER BY o.file_id LIMIT ?",
        &[
            profile.to_owned().into(),
            scan["location"].as_str().unwrap_or("").to_owned().into(),
            scan["scan_id"].as_str().unwrap_or("").to_owned().into(),
            after.to_owned().into(),
            (limit as i64).into(),
        ],
    )?;
    if page.is_empty() {
        return Ok((vec![], 0, None));
    }
    store.transaction(|db| {
        for row in &page {
            db.execute("INSERT INTO watch_revisions VALUES (?,?,?,?) ON CONFLICT(profile,file_id) DO UPDATE SET revision=excluded.revision,stable_since=CASE WHEN watch_revisions.revision=excluded.revision THEN watch_revisions.stable_since ELSE excluded.stable_since END", params![profile,row["file_id"].as_str(),sha256(encode(row)),now])?;
        }
        Ok(())
    })?;
    let mut ids = vec![];
    let mut waiting = 0;
    for row in &page {
        if row["status"] != "present" {
            continue;
        }
        let id = row["file_id"].as_str().unwrap();
        let since: f64 = store.db.query_row(
            "SELECT stable_since FROM watch_revisions WHERE profile=? AND file_id=?",
            [profile, id],
            |r| r.get(0),
        )?;
        if since <= now - settle as f64 {
            ids.push(id.to_owned());
        } else {
            waiting += 1;
        }
    }
    Ok((
        ids,
        waiting,
        page.last()
            .and_then(|v| v["file_id"].as_str())
            .map(str::to_owned),
    ))
}

pub fn statistics(store: &Store, profile: &str, catalogs: &Value) -> Result<Value> {
    let count = |sql: &str, args: &[&str]| -> Result<i64> {
        Ok(store
            .db
            .query_row(sql, rusqlite::params_from_iter(args), |r| r.get(0))?)
    };
    let mut statuses = json!({});
    for key in STATUSES {
        statuses[key] = json!(0);
    }
    for row in rows(
        &store.db,
        &format!(
            "SELECT status,count(*) AS n FROM ({}) WHERE profile=? GROUP BY status",
            crate::query::view_sql_for(store, "catalog_item_workflow")
        ),
        &[profile.to_owned().into()],
    )? {
        statuses[row["status"].as_str().unwrap()] = row["n"].clone();
    }
    let mut availability = json!({"present":0,"missing":0,"unknown":0});
    for row in rows(
        &store.db,
        "SELECT coalesce(o.status,'unknown') AS status,count(*) AS n FROM files f LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? GROUP BY 1",
        &[profile.to_owned().into()],
    )? {
        availability[row["status"].as_str().unwrap()] = row["n"].clone();
    }
    let uncataloged = "NOT EXISTS (SELECT 1 FROM item_files a WHERE a.file_id=f.id AND a.active=1)";
    let mut unmapped = json!({});
    for catalog in catalogs.as_array().unwrap() {
        let name = catalog.as_str().unwrap();
        unmapped[name] = json!(count(
            "SELECT count(*) FROM files f JOIN observations o ON o.file_id=f.id WHERE o.profile=? AND o.status='present' AND NOT EXISTS (SELECT 1 FROM mappings m WHERE m.file_id=f.id AND m.catalog=? AND m.active=1)",
            &[profile, name]
        )?);
    }
    let mut grouped = json!({});
    for (key, table) in [("jobs", "processing_jobs"), ("proposals", "proposals")] {
        grouped[key] = json!({});
        for row in rows(
            &store.db,
            &format!("SELECT state,count(*) AS n FROM {table} WHERE profile=? GROUP BY state"),
            &[profile.to_owned().into()],
        )? {
            grouped[key][row["state"].as_str().unwrap()] = row["n"].clone();
        }
    }
    Ok(json!({
        "scope":{"items":"whole database; readiness in selected profile","files":"whole database; availability in selected profile","jobs_and_proposals":"selected profile","output_catalogs":catalogs},
        "items":{"total":statuses.as_object().unwrap().values().map(|v|v.as_i64().unwrap()).sum::<i64>(),"incomplete":(["pending","in_progress","deferred","needs_attention"].iter().map(|k|statuses[*k].as_i64().unwrap()).sum::<i64>()),"by_status":statuses},
        "files":{"total":availability.as_object().unwrap().values().map(|v|v.as_i64().unwrap()).sum::<i64>(),"by_availability":availability,"uncataloged":count(&format!("SELECT count(*) FROM files f WHERE {uncataloged}"), &[])?,"uncataloged_present":count(&format!("SELECT count(*) FROM files f JOIN observations o ON o.file_id=f.id WHERE o.profile=? AND o.status='present' AND {uncataloged}"), &[profile])?,"unmapped_present_by_catalog":unmapped},
        "jobs":grouped["jobs"],"proposals":grouped["proposals"],
        "inspect":["files --unidentified","item list --curation-status pending","item list --curation-status in_progress","item list --curation-status deferred","item list --curation-status needs_attention","process list --state failed","proposal list --state pending"]
    }))
}
