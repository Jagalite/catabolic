// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Durable principal-bound replay. Request state remains the authority.
use crate::{Error, Result, Store, access::Access, migration::rows, notifications::now};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
pub fn prune(store: &Store) -> Result<()> {
    store.transaction(|db|{for(table,column,deadline)in[("api_events","created_at",now()-86400.),("api_snapshots","expires",now()),("api_tickets","expires",now())]{db.execute(&format!("DELETE FROM {table} WHERE id IN (SELECT id FROM {table} WHERE {column}<? LIMIT 1000)"),[deadline])?;}Ok(())})
}
fn offset(access: &Access, cursor: Option<&str>) -> Result<i64> {
    access.require("events:read")?;
    let mut offset = 0i64;
    if let Some(cursor) = cursor.filter(|v| !v.is_empty()) {
        let saved:Option<String>=access.store().db.query_row("SELECT ids FROM api_snapshots WHERE id=? AND principal=? AND scope='events' AND expires>?",params![cursor,access.principal(),now()],|r|r.get(0)).optional()?;
        offset = saved
            .ok_or_else(|| Error("resync_required".into()))?
            .parse()
            .map_err(|_| Error("resync_required".into()))?;
    }
    let oldest: Option<i64> =
        access
            .store()
            .db
            .query_row("SELECT min(id) FROM api_events", [], |r| r.get(0))?;
    let sequence: Option<i64> = access
        .store()
        .db
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='api_events'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if cursor.is_some_and(|v| !v.is_empty())
        && offset < oldest.map(|v| v - 1).unwrap_or(sequence.unwrap_or(0))
    {
        return Err(Error("resync_required".into()));
    }
    Ok(offset)
}
/// Ingress validation before streaming response headers without allocating an
/// unused replay snapshot. Every emitted page is computed again after capacity.
pub fn validate_cursor(access: &Access, cursor: Option<&str>) -> Result<()> {
    offset(access, cursor).map(|_| ())
}
pub fn page(access: &mut Access, cursor: Option<&str>) -> Result<Value> {
    let mut offset = offset(access, cursor)?;
    let events = rows(
        &access.store().db,
        "SELECT * FROM api_events WHERE id>? ORDER BY id LIMIT 100",
        &[offset.into()],
    )?;
    let mut visible = Vec::new();
    for event in events {
        offset = event["id"]
            .as_i64()
            .ok_or_else(|| Error("catalog_error".into()))?;
        let requests = rows(
            &access.store().db,
            &format!(
                "SELECT id FROM api_requests WHERE principal=? AND {}=?",
                if event["resource_type"] == "job" {
                    "job_id"
                } else {
                    "id"
                }
            ),
            &[
                access.principal().to_owned().into(),
                event["resource_id"]
                    .as_str()
                    .ok_or_else(|| Error("catalog_error".into()))?
                    .to_owned()
                    .into(),
            ],
        )?;
        for request in requests {
            let id = request["id"].as_str().unwrap();
            match crate::rendition_mutation::status(access, id) {
                Ok(current) => visible.push(
                    json!({"type":"request.changed","request_id":id,"state":current["state"]}),
                ),
                Err(error)
                    if [
                        "not_found",
                        "forbidden",
                        "unauthorized",
                        "revision_mismatch",
                        "operation_not_approved",
                    ]
                    .contains(&error.0.as_str()) => {}
                Err(error) => return Err(error),
            }
        }
    }
    let mut random = [0u8; 24];
    getrandom::fill(&mut random).map_err(|e| Error(e.to_string()))?;
    let next = URL_SAFE_NO_PAD.encode(random);
    access.store().transaction(|db| {
        db.execute(
            "INSERT INTO api_snapshots VALUES (?,?,?,?,?)",
            params![
                next,
                access.principal(),
                "events",
                offset.to_string(),
                now() + 3600.
            ],
        )?;
        Ok(())
    })?;
    Ok(json!({"events":visible,"cursor":next,"page_complete":true}))
}
