// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Immutable fallback policy revisions and dependencies. Execution and publication
//! are deliberately separate; persisting a policy never chooses or exposes media.
use crate::{Error, Result, Store, definitions, migration::rows, mutation};
use catabolic_core::{encode, fallback, sha256};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
pub fn get(store: &Store, profile: &str, id: &str) -> Result<Value> {
    let mut row = rows(
        &store.db,
        "SELECT * FROM fallback_policies WHERE profile=? AND id=?",
        &[profile.to_owned().into(), id.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown fallback policy revision in this profile".into()))?;
    row["definition"] = catabolic_core::decode_json(row["definition"].as_str().unwrap())?;
    Ok(row)
}
pub fn put(store: &Store, profile: &str, name: &str, input: &Value) -> Result<Value> {
    mutation::profile(store, profile)?;
    if name.is_empty()
        || name.len() > 64
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return Err(Error(
            "names must be 1–64 letters, digits, dots, underscores, or hyphens".into(),
        ));
    }
    let value = fallback::definition(input)?;
    let dependencies = fallback::query_dependencies(&value);
    for id in &dependencies {
        if definitions::query_get(store, profile, id)?["definition"]["mode"] != "selection" {
            return Err(Error(
                "fallback tiers require complete typed selections".into(),
            ));
        }
    }
    if let Some(operation) = value["package"]["operation_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        && !store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM processing_recipes WHERE id=?)",
            [operation],
            |r| r.get::<_, bool>(0),
        )?
    {
        return Err(Error("unknown approved packaging operation".into()));
    }
    let serialized = encode(&value);
    let digest = sha256(&serialized);
    let id = store.transaction(|db| {
        if let Some(id) = db
            .query_row(
                "SELECT id FROM fallback_policies WHERE profile=? AND name=? AND digest=?",
                params![profile, name, digest],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        let revision: i64 = db.query_row(
            "SELECT coalesce(max(revision),0)+1 FROM fallback_policies WHERE profile=? AND name=?",
            params![profile, name],
            |r| r.get(0),
        )?;
        let id = uuid::Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO fallback_policies VALUES (?,?,?,?,?,?)",
            params![id, profile, name, revision, serialized, digest],
        )?;
        for query in &dependencies {
            db.execute(
                "INSERT INTO fallback_dependencies VALUES (?,?)",
                params![id, query],
            )?;
        }
        Ok(id)
    })?;
    get(store, profile, &id)
}
pub fn listing(store: &Store, profile: &str, limit: usize, after: &str) -> Result<Value> {
    if !(1..=1000).contains(&limit) {
        return Err(Error("invalid policy page".into()));
    }
    let ids = rows(
        &store.db,
        "SELECT id FROM fallback_policies WHERE profile=? AND id>? ORDER BY id LIMIT ?",
        &[
            profile.to_owned().into(),
            after.to_owned().into(),
            (limit as i64 + 1).into(),
        ],
    )?;
    let policies = ids
        .iter()
        .take(limit)
        .map(|row| get(store, profile, row["id"].as_str().unwrap()))
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"policies":policies,"has_more":ids.len()>limit}))
}
pub fn execute(store: &Store, profile: &str, action: &str, args: &Value) -> Result<Value> {
    match action {
        "put" => put(
            store,
            profile,
            args["name"]
                .as_str()
                .ok_or_else(|| Error("missing policy name".into()))?,
            args.get("definition").unwrap_or(&Value::Null),
        ),
        "get" => get(
            store,
            profile,
            args["id"]
                .as_str()
                .ok_or_else(|| Error("missing policy id".into()))?,
        ),
        "list" => listing(
            store,
            profile,
            args.get("limit")
                .map_or(Some(100), Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| Error("invalid policy page".into()))?,
            args["after"].as_str().unwrap_or(""),
        ),
        _ => Err(Error(format!(
            "unknown native fallback policy action: {action}"
        ))),
    }
}
