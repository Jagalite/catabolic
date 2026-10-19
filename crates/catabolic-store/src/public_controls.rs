// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Local operator controls shared by preserved terminal and future facades.
//! Publication admission remains in the existing native effect owners.
use crate::{
    Error, Result, Store, fallback_projection, http_operations, mutation,
    public_reads::bounded_rows, query,
};
use rusqlite::params;
use serde_json::{Value, json};

pub fn fallback(store: &Store, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    mutation::profile(store, profile)?;
    let catalog = args["catalog"]
        .as_str()
        .ok_or_else(|| Error("catalog must be text".into()))?;
    if operation == "history" {
        let mut values = bounded_rows(
            store,
            "SELECT * FROM fallback_history WHERE profile=? AND catalog=? ORDER BY id DESC LIMIT 100",
            &[profile.to_owned().into(), catalog.to_owned().into()],
        )?;
        for row in &mut values {
            query::decode(row, &["decision"])?;
        }
        return Ok(json!({"history":values}));
    }
    if !matches!(operation, "enable" | "disable") {
        return Err(Error("unknown fallback control".into()));
    }
    if fallback_projection::binding(store, profile, catalog)?.is_none() {
        return Err(Error("bind an explicit fallback projection first".into()));
    }
    let bounded = |key: &str, default: u64, minimum: u64, maximum: u64| {
        args.get(key)
            .map_or(Some(default), Value::as_u64)
            .filter(|n| (minimum..=maximum).contains(n))
            .ok_or_else(|| Error("invalid fallback maintenance limits".into()))
    };
    let interval = bounded("interval", 60, 1, 86400)? as i64;
    let changes = bounded("max_changes", 100, 1, 10000)? as i64;
    let removals = bounded("max_removals", 0, 0, 10000)? as i64;
    let enabled = operation == "enable";
    store.transaction(|db| {
        db.execute("UPDATE fallback_bindings SET enabled=?,interval_seconds=?,max_changes=?,max_removals=?,next_attempt=0 WHERE profile=? AND catalog=?",params![enabled,interval,changes,removals,profile,catalog])?;
        db.execute("INSERT INTO catalog_refresh_settings VALUES (?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET enabled=excluded.enabled,max_removals=excluded.max_removals",params![profile,catalog,enabled,removals])?;
        Ok(())
    })?;
    fallback_projection::binding(store, profile, catalog)?
        .ok_or_else(|| Error("fallback binding disappeared".into()))
}
pub fn http_operation(
    store: &Store,
    profile: &str,
    operation: &str,
    args: &Value,
) -> Result<Value> {
    if operation == "get" {
        let id = args["id"]
            .as_str()
            .ok_or_else(|| Error("invalid_configuration".into()))?;
        let mut value = bounded_rows(
            store,
            "SELECT * FROM http_operations WHERE profile=? AND id=?",
            &[profile.to_owned().into(), id.to_owned().into()],
        )?
        .into_iter()
        .next()
        .ok_or_else(|| Error("not_found".into()))?;
        query::decode(&mut value, &["definition"])?;
        return Ok(value);
    }
    if operation != "list" {
        return Err(Error("unknown local HTTP operation read".into()));
    }
    let n = args
        .get("limit")
        .map_or(Some(100), Value::as_u64)
        .filter(|n| (1..=1000).contains(n))
        .ok_or_else(|| Error("invalid_page".into()))? as usize;
    let after = args.get("after").and_then(Value::as_str).unwrap_or("");
    // Bound the backing definitions too; bounding IDs alone still allowed an
    // oversized document to be materialized by the later per-ID owner read.
    let mut rows = bounded_rows(
        store,
        "SELECT * FROM http_operations WHERE profile=? AND id>? ORDER BY id LIMIT ?",
        &[
            profile.to_owned().into(),
            after.to_owned().into(),
            ((n + 1) as i64).into(),
        ],
    )?;
    let next = if rows.len() > n {
        rows[n - 1]["id"].clone()
    } else {
        Value::Null
    };
    rows.truncate(n);
    let values = rows
        .iter_mut()
        .map(|row| {
            query::decode(row, &["definition"])?;
            row["enabled"] = json!(row["enabled"].as_i64().unwrap_or(0) != 0);
            Ok(http_operations::present(row))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"operations":values,"next_cursor":next}))
}

/// Bounded existing-association copy selection; the production ranker owns all
/// policy, fact-currentness, tie-break and sidecar attachment decisions.
pub fn copies_plan(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    mutation::profile(store, profile)?;
    let n = args
        .get("limit")
        .map_or(Some(10000), Value::as_u64)
        .filter(|n| (1..=100000).contains(n))
        .ok_or_else(|| Error("limit must be 1..100000".into()))?;
    let catalog = args["catalog"].as_str().unwrap_or("global");
    let rows = bounded_rows(
        store,
        "SELECT a.*,f.location,f.path FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.active=1 ORDER BY a.id LIMIT ?",
        &[((n + 1) as i64).into()],
    )?;
    if rows.len() > n as usize {
        return Err(Error(
            "copy selection exceeds requested limit; narrow the saved layout selection".into(),
        ));
    }
    let (selected, mut report) = crate::layout::copy_filter(store, profile, catalog, &rows)?;
    report["catalog"] = json!(catalog);
    report["association_ids"] = json!(selected.iter().map(|r| r["id"].clone()).collect::<Vec<_>>());
    Ok(report)
}
/// Record a per-catalog rendition choice through the caller-owned transaction.
pub fn rendition_decide(
    store: &Store,
    profile: &str,
    args: &Value,
    excluded: bool,
) -> Result<Value> {
    mutation::profile(store, profile)?;
    let catalog = args["catalog"]
        .as_str()
        .ok_or_else(|| Error("catalog must be text".into()))?;
    crate::definitions::dispatch(store, profile, "rendition-get", args)?;
    let reason = crate::item_workflow::text(&args["reason"], "decision reason", 4096, false)?;
    let id = args["id"]
        .as_str()
        .ok_or_else(|| Error("unknown rendition in this profile".into()))?;
    if !store
        .db
        .prepare("SELECT 1 FROM media_outputs WHERE id=? AND profile=?")?
        .exists(params![id, profile])?
    {
        return Err(Error("unknown rendition in this profile".into()));
    }
    store.transaction(|db| {
        db.execute("INSERT INTO rendition_decisions VALUES (?,?,?,?,?) ON CONFLICT(profile,catalog,output_id) DO UPDATE SET excluded=excluded.excluded,reason=excluded.reason",params![profile,catalog,id,excluded,reason])?;
        Ok(())
    })?;
    Ok(json!({"catalog":catalog,"output_id":id,"excluded":excluded,"reason":reason}))
}
