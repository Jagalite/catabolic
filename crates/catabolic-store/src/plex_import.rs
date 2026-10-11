// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded Plex intake decisions. HTTP belongs to the runtime; this owner pins
//! the complete review and delegates durable per-file decisions to curation.
use crate::{Error, Result, Store, consumers, curation, migration::rows};
use catabolic_core::{decode_json, encode, sha256};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

pub fn error(code: &str) -> Error {
    consumers::Failure::new(code).error()
}
fn field<'a>(value: &'a Value, key: &str, maximum: usize) -> Result<&'a str> {
    consumers::text(&value[key], maximum).map_err(|e| error(&e.0))
}
fn path(value: &Value, relative: bool) -> Result<String> {
    let value = value
        .as_str()
        .ok_or_else(|| error("invalid_configuration"))?;
    consumers::path(value, relative).map_err(|e| error(&e.0))
}
fn payload(value: &Value) -> Result<()> {
    if !value.is_object() || encode(value).len() > 256 * 1024 {
        return Err(Error(
            "payload must be a JSON object of at most 256 KiB".into(),
        ));
    }
    Ok(())
}
/// Normalize path mappings before any network I/O.
pub fn mappings(value: &Value) -> Result<Vec<Value>> {
    let values = value
        .as_array()
        .filter(|v| (1..=100).contains(&v.len()))
        .ok_or_else(|| error("import_mapping_requires_1_to_100_entries"))?;
    let mut result: Vec<Value> = Vec::new();
    for entry in values {
        if entry.as_object().is_none_or(|o| {
            o.keys()
                .any(|k| !["remote_root", "location", "subtree"].contains(&k.as_str()))
        }) {
            return Err(error("invalid_import_mapping"));
        }
        let remote = path(&entry["remote_root"], false)?;
        let row = json!({"remote_root":remote,"location":field(entry,"location",255)?,"subtree":path(entry.get("subtree").unwrap_or(&json!("")),true)?});
        if result.iter().any(|old| {
            consumers::within(&remote, old["remote_root"].as_str().unwrap())
                || consumers::within(old["remote_root"].as_str().unwrap(), &remote)
        }) {
            return Err(error("overlapping_import_mappings"));
        }
        result.push(row);
    }
    result.sort_by(|a, b| a["remote_root"].as_str().cmp(&b["remote_root"].as_str()));
    Ok(result)
}
/// Frozen bool/int distinction and the public bounded page contract.
pub fn page_bounds(request: &Value) -> Result<(u64, usize)> {
    let offset = request.get("offset").unwrap_or(&Value::Null);
    let limit = request.get("limit").unwrap_or(&Value::Null);
    let offset = if request.get("offset").is_none() {
        Some(0)
    } else {
        offset.as_u64()
    };
    let limit = if request.get("limit").is_none() {
        Some(100)
    } else {
        limit.as_u64()
    };
    match (offset, limit) {
        (Some(offset @ 0..=2147483647), Some(limit @ 1..=100)) => Ok((offset, limit as usize)),
        _ => Err(error("invalid_import_page")),
    }
}
pub fn candidate(
    store: &Store,
    profile: &str,
    record: &Value,
    part: &Value,
    identity: &str,
    mappings: &[Value],
) -> Result<Value> {
    let remote = field(part, "path", 4096)?;
    let matched: Vec<_> = mappings
        .iter()
        .filter(|m| consumers::within(remote, m["remote_root"].as_str().unwrap()))
        .collect();
    if matched.len() != 1 {
        return Err(error("unmapped_remote_path"));
    }
    let mapping = matched[0];
    let relative = remote
        .strip_prefix(mapping["remote_root"].as_str().unwrap())
        .unwrap()
        .trim_start_matches('/');
    let subtree = mapping["subtree"].as_str().unwrap();
    // PurePosixPath of an empty relative join is '.', which the frozen path
    // validator refuses. A nonempty subtree joined to '.' remains the subtree.
    let local = match (subtree.is_empty(), relative.is_empty()) {
        (true, true) => ".".to_owned(),
        (true, false) => relative.to_owned(),
        (false, true) => subtree.to_owned(),
        (false, false) => format!("{subtree}/{relative}"),
    };
    let local = consumers::path(&local, true).map_err(|e| error(&e.0))?;
    let files = rows(
        &store.db,
        "SELECT id FROM files WHERE location=? AND path=?",
        &[
            mapping["location"].as_str().unwrap().to_owned().into(),
            local.clone().into(),
        ],
    )?;
    if files.len() != 1 {
        return Err(error("file_not_in_inventory"));
    }
    let file = files[0]["id"].as_str().unwrap();
    let item_matches = rows(
        &store.db,
        "SELECT item_id FROM identities WHERE namespace='plex' AND value=?",
        &[identity.to_owned().into()],
    )?;
    let item = item_matches
        .first()
        .and_then(|r| r["item_id"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("catabolic:plex:{identity}").as_bytes(),
            )
            .to_string()
        });
    let value = json!({"item":{"id":item,"kind":record["kind"],"identities":{"plex":identity},"metadata":record["metadata"]},"role":"primary","part":part["part"]});
    payload(&value)?;
    let snapshot = curation::proposal_snapshot(store, profile, file, &value)?;
    payload(&snapshot)?;
    if snapshot["file"]["status"] != "present" {
        return Err(error("source_not_present"));
    }
    crate::source::validate(&snapshot["file"])?;
    let active: Vec<_> = snapshot["associations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["active"] == 1 || a["active"] == true)
        .collect();
    if active
        .iter()
        .any(|a| a["item_id"] != item || a["role"] != "primary" || a["part"] != part["part"])
    {
        return Err(error("existing_association_conflict"));
    }
    if !part["part"].is_null() && !rows(&store.db,"SELECT id FROM item_files WHERE item_id=? AND role='primary' AND part=? AND active=1 AND file_id!=?",&[item.clone().into(),part["part"].as_i64().unwrap().into(),file.to_owned().into()])?.is_empty() {
        return Err(error("existing_part_conflict"));
    }
    let previous = rows(
        &store.db,
        "SELECT evidence FROM proposals WHERE source='plex-import' AND state='accepted' AND json_extract(result,'$.item_id')=? ORDER BY rowid DESC LIMIT 1",
        &[item.clone().into()],
    )?;
    if let Some(previous) = previous.first() {
        let previous = decode_json(previous["evidence"].as_str().unwrap())?;
        if previous["guid"].as_str().is_some_and(|s| !s.is_empty())
            && previous["guid"] != record["guid"]
        {
            return Err(error("plex_item_guid_changed"));
        }
    }
    let existing = snapshot["item"].as_array().unwrap().first();
    if existing.is_some_and(|e| e["kind"] != record["kind"]) {
        return Err(error("existing_item_kind_conflict"));
    }
    let metadata = existing
        .map(|e| decode_json(e["metadata"].as_str().unwrap()))
        .transpose()?
        .unwrap_or(json!({}));
    let proposed = record["metadata"].as_object().unwrap();
    let preserved: Vec<_> = proposed
        .iter()
        .filter(|(k, v)| metadata.get(*k).is_some_and(|old| old != *v))
        .map(|(k, _)| k.clone())
        .collect();
    let unchanged = !item_matches.is_empty()
        && !active.is_empty()
        && proposed.keys().all(|k| metadata.get(k).is_some());
    Ok(
        json!({"file_id":file,"location":mapping["location"],"path":local,"payload":value,"snapshot":snapshot,"action":if unchanged{"unchanged"}else{"import"},"preserved_fields":preserved}),
    )
}

/// The remote evidence captured together while detached. Keeping these values
/// together prevents callers from accidentally digesting a different page.
pub struct RemotePage<'a> {
    pub connection: &'a Value,
    pub library: &'a Value,
    pub mappings: &'a [Value],
    pub data: &'a Value,
}

/// A complete source/connection/remote/local review. No encompassing mutation
/// transaction is used: each put and accepted decision is durably resumable.
pub fn run(
    store: &Store,
    profile: &str,
    request: &Value,
    remote: RemotePage<'_>,
    checkpoint: &mut dyn FnMut() -> Result<()>,
) -> Result<Value> {
    let RemotePage {
        connection,
        library,
        mappings,
        data,
    } = remote;
    let (offset, limit) = page_bounds(request)?;
    let apply = request["apply"] == true;
    if consumers::connection(store, profile, field(request, "connection", 255)?)? != *connection {
        return Err(error("import_connection_changed"));
    }
    crate::definitions::require_recovered(store)?;
    for mapping in mappings {
        crate::observation::binding(
            store,
            profile,
            "source",
            mapping["location"].as_str().unwrap(),
        )?;
    }
    let mut candidates = Vec::new();
    let mut deferred = Vec::new();
    for record in data["records"].as_array().unwrap() {
        checkpoint()?;
        if record["reported_type"] != record["kind"]
            || record["metadata"]["title"]
                .as_str()
                .is_none_or(str::is_empty)
            || record["parts"].as_array().unwrap().is_empty()
        {
            deferred.push(json!({"rating_key":record["rating_key"],"reason":"unsupported_or_incomplete_item"}));
            continue;
        }
        let identity = encode(&json!([
            connection["server_id"],
            library["uuid"],
            record["rating_key"]
        ]));
        for part in record["parts"].as_array().unwrap() {
            checkpoint()?;
            match candidate(store,profile,record,part,&identity,mappings) {
                Ok(mut row) => {
                    row["evidence"] = json!({"server_id":connection["server_id"],"library":library,"rating_key":record["rating_key"],"remote_path":part["path"],"guids":record["guids"],"guid":record["guid"]});
                    candidates.push(row);
                }
                Err(e) => deferred.push(json!({"rating_key":record["rating_key"],"remote_path":part["path"],"reason":e.0})),
            }
        }
    }
    let mut counts = BTreeMap::new();
    let mut slots = BTreeMap::new();
    for row in &candidates {
        *counts
            .entry(row["file_id"].as_str().unwrap().to_owned())
            .or_insert(0) += 1;
        *slots
            .entry(encode(&json!([
                row["payload"]["item"]["id"],
                row["payload"]["part"]
            ])))
            .or_insert(0) += 1;
    }
    candidates.retain(|row| {
        let slot = encode(&json!([
            row["payload"]["item"]["id"],
            row["payload"]["part"]
        ]));
        if counts[row["file_id"].as_str().unwrap()] > 1
            || (!row["payload"]["part"].is_null() && slots[&slot] > 1)
        {
            deferred
                .push(json!({"file_id":row["file_id"],"reason":"duplicate_remote_file_or_part"}));
            false
        } else {
            true
        }
    });
    if encode(&json!([data, candidates, deferred])).len() > 4 * 1024 * 1024 {
        return Err(error("import_plan_too_large_reduce_limit"));
    }
    let digest = sha256(encode(&json!([
        store.database_id,
        profile,
        connection,
        library,
        mappings,
        offset,
        limit,
        data,
        candidates,
        deferred
    ])));
    if apply && request["expected_plan"] != digest {
        return Err(error("import_plan_changed_preview_again"));
    }
    let mut imported = Vec::new();
    let mut errors = Vec::new();
    if apply {
        for row in &candidates {
            checkpoint()?;
            if row["action"] == "unchanged" {
                continue;
            }
            let proposal = match curation::proposal_put(
                store,
                profile,
                &json!({"file_id":row["file_id"],"payload":row["payload"],"source":"plex-import","evidence":row["evidence"]}),
            ) {
                Ok(proposal) => proposal,
                Err(e) => {
                    errors.push(json!({"file_id":row["file_id"],"reason":e.0}));
                    break;
                }
            };
            // Interruptions propagate rather than becoming per-file errors, as
            // Python KeyboardInterrupt does; a pending put can be reused.
            checkpoint()?;
            match curation::proposal_decide(
                store,
                profile,
                &json!({"id":proposal["id"],"accept":true,"actor":"plex-import"}),
            ) {
                Ok(result) => {
                    let mut result = result["result"].clone();
                    result["proposal_id"] = proposal["id"].clone();
                    imported.push(result);
                }
                Err(e) => {
                    errors.push(json!({"file_id":row["file_id"],"reason":e.0}));
                    break;
                }
            }
            checkpoint()?;
        }
    }
    for row in &mut candidates {
        row.as_object_mut().unwrap().remove("snapshot");
    }
    Ok(
        json!({"plan_id":digest,"applied":apply,"imported":imported,"candidates":candidates,"deferred":deferred,"errors":errors,"offset":offset,"next_offset":data["next_offset"],"total":data["total"],"page_complete":deferred.is_empty() && errors.is_empty(),"complete":data["next_offset"].is_null() && deferred.is_empty() && errors.is_empty()}),
    )
}
