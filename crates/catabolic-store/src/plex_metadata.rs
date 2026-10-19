// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Reviewed metadata values, imported/matched provenance and durable attempts.
//! No network or managed-output mutation is performed by this owner.
use crate::{Result, Store, consumers, migration::rows, plex_matching};
use catabolic_core::{decode_json, encode, sha256};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub use plex_matching::{error, field};
pub const FIELDS: &[(&str, &str)] = &[
    ("title", "title"),
    ("sort_title", "titleSort"),
    ("original_title", "originalTitle"),
    ("summary", "summary"),
    ("year", "year"),
    ("release_date", "originallyAvailableAt"),
    ("tagline", "tagline"),
    ("studio", "studio"),
    ("content_rating", "contentRating"),
    ("edition", "editionTitle"),
];
pub fn wire(field: &str) -> Result<&'static str> {
    FIELDS
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, v)| *v)
        .ok_or_else(|| error("metadata_invalid_fields"))
}
pub fn kind(kind: &str) -> Result<(&'static str, u8)> {
    match kind {
        "movie" => Ok(("movie", 1)),
        "episode" => Ok(("show", 4)),
        "track" => Ok(("artist", 10)),
        "photo" => Ok(("photo", 13)),
        _ => Err(error("metadata_item_kind_mismatch")),
    }
}
fn allowed(kind: &str, field: &str) -> bool {
    match kind {
        "movie" => wire(field).is_ok(),
        "episode" => matches!(
            field,
            "title" | "sort_title" | "summary" | "release_date" | "content_rating"
        ),
        "track" => field == "title",
        "photo" => matches!(field, "title" | "sort_title" | "summary"),
        _ => false,
    }
}
pub fn value(field: &str, raw: &Value) -> Result<Value> {
    if field == "year" {
        return raw
            .as_u64()
            .filter(|n| (1..=9999).contains(n))
            .map(|_| raw.clone())
            .ok_or_else(|| error("metadata_invalid_year"));
    }
    let text = raw
        .as_str()
        .filter(|s| {
            s.chars().count() <= 8192
                && !s
                    .chars()
                    .any(|c| c < ' ' && !matches!(c, '\n' | '\r' | '\t'))
                && (field != "title" || !s.trim().is_empty())
        })
        .ok_or_else(|| error(&format!("metadata_invalid_{field}")))?;
    if field == "release_date"
        && (text.len() != 10
            || text.starts_with("0000")
            || chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .ok()
                .is_none_or(|d| d.format("%Y-%m-%d").to_string() != text))
    {
        return Err(error("metadata_invalid_release_date"));
    }
    Ok(raw.clone())
}
#[derive(Clone)]
pub struct Options {
    pub ids: Vec<String>,
    pub matched: bool,
    pub fields: Vec<String>,
    pub clear: Vec<String>,
    pub lock: bool,
    pub unlock: bool,
    pub apply: bool,
}
pub fn options(request: &Value) -> Result<Options> {
    let empty = json!([]);
    let items = request.get("item_ids").unwrap_or(&empty);
    let matched = request.get("match_ids").is_some_and(|v| !v.is_null());
    if matched && !items.is_null() && items.as_array().is_none_or(|a| !a.is_empty()) {
        return Err(error("metadata_select_items_or_matches"));
    }
    let ids = if matched {
        &request["match_ids"]
    } else {
        items
    };
    let ids = ids
        .as_array()
        .filter(|a| (1..=100).contains(&a.len()))
        .ok_or_else(|| error("metadata_requires_1_to_100_items"))?;
    let ids = ids
        .iter()
        .map(|v| {
            consumers::text(v, 255)
                .map(str::to_owned)
                .map_err(|e| error(&e.0))
        })
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect();
    let fields = request["fields"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| error("metadata_invalid_fields"))?;
    let fields = fields
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|f| wire(f).is_ok())
                .map(str::to_owned)
                .ok_or_else(|| error("metadata_invalid_fields"))
        })
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    let boolean = |key: &str| {
        request
            .get(key)
            .map(Value::as_bool)
            .unwrap_or(Some(false))
            .ok_or_else(|| error("metadata_invalid_options"))
    };
    let (lock, unlock, apply) = (
        boolean("lock_fields")?,
        boolean("unlock_fields")?,
        boolean("apply")?,
    );
    if lock && unlock {
        return Err(error("metadata_invalid_options"));
    }
    let clear = request
        .get("clear_fields")
        .unwrap_or(&empty)
        .as_array()
        .ok_or_else(|| error("metadata_invalid_clear_fields"))?;
    // Preserve order and duplicates: the frozen review hashes the caller's clear list.
    let clear = clear
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|f| *f != "title" && fields.iter().any(|v| v == f))
                .map(str::to_owned)
                .ok_or_else(|| error("metadata_invalid_clear_fields"))
        })
        .collect::<Result<Vec<_>>>()?;
    if apply && request["expected_plan"].as_str().is_none_or(str::is_empty) {
        return Err(error("metadata_apply_requires_expected_plan"));
    }
    Ok(Options {
        ids,
        matched,
        fields,
        clear,
        lock,
        unlock,
        apply,
    })
}
fn desired(metadata: &Value, kind: &str, options: &Options) -> Result<Value> {
    if options.fields.iter().any(|f| !allowed(kind, f)) {
        return Err(error("metadata_field_unsupported_for_item_kind"));
    }
    let mut result = serde_json::Map::new();
    for f in &options.fields {
        let v = if options.clear.contains(f) {
            if f == "year" { Value::Null } else { json!("") }
        } else {
            let raw = metadata
                .get(f)
                .filter(|v| !v.is_null())
                .ok_or_else(|| error("metadata_selected_field_missing"))?;
            value(f, raw)?
        };
        result.insert(f.clone(), v);
    }
    Ok(Value::Object(result))
}
pub fn snapshot(
    store: &Store,
    profile: &str,
    connection: &Value,
    library: &Value,
    options: &Options,
) -> Result<Value> {
    if consumers::connection(store, profile, field(connection, "id")?)? != *connection {
        return Err(error("metadata_connection_changed"));
    }
    crate::definitions::require_recovered(store)?;
    let mut items = Vec::new();
    for id in &options.ids {
        let row = if options.matched {
            let (local, e) =
                plex_matching::matched_publication(store, profile, connection, library, id)?;
            let metadata = decode_json(
                local[field(&e, "metadata_source")?]["metadata"]
                    .as_str()
                    .ok_or_else(|| error("metadata_selected_field_missing"))?,
            )?;
            let desired = desired(&metadata, field(&e, "kind")?, options)?;
            json!({"item_id":local["item"]["id"],"match_id":id,"association_id":e["association_id"],"kind":e["kind"],"rating_key":e["rating_key"],"desired":desired,"evidence":[{"evidence":encode(&e)}],"publication":local})
        } else {
            let found = rows(
                &store.db,
                "SELECT * FROM items WHERE id=?",
                &[id.clone().into()],
            )?;
            let item = found
                .first()
                .filter(|_| found.len() == 1)
                .ok_or_else(|| error("metadata_unknown_item"))?;
            let item_kind = field(item, "kind")?;
            if kind(item_kind)?.0 != library["type"] {
                return Err(error("metadata_item_kind_mismatch"));
            }
            let desired = desired(
                &decode_json(item["metadata"].as_str().unwrap())?,
                item_kind,
                options,
            )?;
            let identities = rows(
                &store.db,
                "SELECT value FROM identities WHERE namespace='plex' AND item_id=? ORDER BY value",
                &[id.clone().into()],
            )?;
            let mut keys = Vec::new();
            for identity in identities {
                if let Ok(Value::Array(parts)) = decode_json(identity["value"].as_str().unwrap())
                    && parts.len() == 3
                    && parts[0] == connection["server_id"]
                    && parts[1] == library["uuid"]
                {
                    keys.push(parts[2].clone());
                }
            }
            if keys.len() != 1 || !valid_key(&keys[0]) {
                return Err(error("metadata_requires_unique_imported_plex_identity"));
            }
            let key = &keys[0];
            let proofs = rows(
                &store.db,
                "SELECT p.id,p.file_id,p.evidence FROM proposals p JOIN item_files f ON f.file_id=p.file_id AND f.item_id=? AND f.active=1 AND f.role='primary' WHERE p.profile=? AND p.source='plex-import' AND p.state='accepted' AND json_extract(p.result,'$.item_id')=? ORDER BY p.rowid DESC LIMIT 101",
                &[
                    id.clone().into(),
                    profile.to_owned().into(),
                    id.clone().into(),
                ],
            )?;
            if proofs.len() > 100 {
                return Err(error("metadata_import_evidence_limit"));
            }
            let mut evidence = Vec::new();
            for proof in proofs {
                let e = decode_json(proof["evidence"].as_str().unwrap())?;
                if e["server_id"] == connection["server_id"]
                    && e["library"]["uuid"] == library["uuid"]
                    && e["rating_key"] == *key
                {
                    if e["guid"].as_str().is_none_or(str::is_empty) {
                        return Err(error("metadata_requires_import_provenance"));
                    }
                    evidence.push(proof);
                }
            }
            if evidence.is_empty() {
                return Err(error("metadata_requires_import_provenance"));
            }
            json!({"item_id":id,"kind":item_kind,"rating_key":key,"desired":desired,"evidence":evidence})
        };
        items.push(row);
    }
    if items
        .iter()
        .map(|r| {
            r["rating_key"]
                .as_str()
                .ok_or_else(|| error("metadata_invalid_rating_key"))
        })
        .collect::<Result<BTreeSet<_>>>()?
        .len()
        != items.len()
    {
        return Err(error("metadata_duplicate_remote_item"));
    }
    Ok(json!({"database_id":store.database_id,"items":items}))
}
pub fn valid_key(key: &Value) -> bool {
    key.as_str()
        .is_some_and(|s| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
}
pub fn planned(item: &Value, remote: Value, options: &Options) -> Value {
    let mut changes = serde_json::Map::new();
    for field in &options.fields {
        let before = &remote["fields"][field];
        let after = json!({"value":item["desired"][field],"locked":!options.unlock && (options.lock || before["locked"]==true)});
        if *before != after {
            changes.insert(field.clone(), json!({"before":before,"after":after}));
        }
    }
    let mut row = json!({"item_id":item["item_id"],"rating_key":item["rating_key"],"kind":item["kind"],"remote":remote,"changes":changes});
    if item.get("match_id").is_some() {
        row["match_id"] = item["match_id"].clone();
        row["association_id"] = item["association_id"].clone();
    }
    row
}
pub fn review(
    profile: &str,
    request: &Value,
    connection: &Value,
    library: &Value,
    local: &Value,
    planned: &[Value],
    options: &Options,
) -> Result<Value> {
    let encoded = encode(&json!([
        profile,
        connection,
        library,
        local,
        options.fields,
        options.lock,
        options.unlock,
        options.clear,
        planned
    ]));
    if encoded.len() > 4 * 1024 * 1024 {
        return Err(error("metadata_plan_too_large_reduce_items"));
    }
    let digest = sha256(encoded);
    if options.apply && request["expected_plan"] != digest {
        return Err(error("metadata_plan_changed_preview_again"));
    }
    Ok(
        json!({"plan_id":digest,"applied":options.apply,"connection":request["connection"],"library_id":request["library_id"],"items":planned,"results":[],"complete":true}),
    )
}
pub fn record_attempt(
    store: &Store,
    profile: &str,
    id: &str,
    payload: &Value,
    create: bool,
) -> Result<()> {
    if !store.writable || !store.owns_writer_lock() {
        return Err(crate::Error(
            "metadata publication requires a writer owner".into(),
        ));
    }
    store.transaction(|db|{
        if create {
            db.execute("INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,'metadata_publish','info',?)",rusqlite::params![id,profile,encode(payload)])?;
        } else {
            let severity = if payload["state"]=="verified" {"info"} else {"warning"};
            if db.execute("UPDATE consumer_events SET subject=?,severity=? WHERE id=? AND profile=? AND event='metadata_publish'",rusqlite::params![encode(payload),severity,id,profile])? != 1 {
                return Err(error("metadata_attempt_changed"));
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scalar_values_and_date_boundaries_are_frozen() {
        for raw in [
            json!(true),
            json!(0),
            json!(-1),
            json!(10000),
            json!(1.0),
            Value::Null,
        ] {
            assert!(value("year", &raw).is_err());
        }
        for raw in ["", "\u{0}", "\t\n", "  "] {
            assert!(value("title", &json!(raw)).is_err());
        }
        assert_eq!(value("summary", &json!("\t\n\r")).unwrap(), json!("\t\n\r"));
        assert!(value("release_date", &json!("0000-01-01")).is_err());
        assert!(value("release_date", &json!("2025-02-29")).is_err());
        assert_eq!(
            value("release_date", &json!("2024-02-29")).unwrap(),
            json!("2024-02-29")
        );
        assert!(value("title", &json!("冬".repeat(8192))).is_ok());
        assert!(value("title", &json!("冬".repeat(8193))).is_err());
    }
    #[test]
    fn options_dedupe_selections_but_pin_clear_order() {
        let request = json!({"item_ids":["b","a","a"],"fields":["year","summary","summary"],"clear_fields":["year","summary","year"]});
        let options = options(&request).unwrap();
        assert_eq!(options.ids, ["a", "b"]);
        assert_eq!(options.fields, ["summary", "year"]);
        assert_eq!(options.clear, ["year", "summary", "year"]);
        assert!(super::options(&json!({"item_ids":vec!["a";101],"fields":["title"]})).is_err());
        assert!(
            super::options(&json!({"item_ids":["a"],"fields":["title"],"lock_fields":1})).is_err()
        );
        assert!(
            super::options(&json!({"item_ids":null,"match_ids":["a"],"fields":["title"]})).is_ok()
        );
    }
    #[test]
    fn durable_intent_requires_writer_and_updates_exact_event() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, false, false).unwrap();
        assert!(
            record_attempt(
                &store,
                "default",
                "attempt",
                &json!({"state":"started"}),
                true
            )
            .is_err()
        );
        drop(store);
        let store = Store::open(&path, true, false).unwrap();
        record_attempt(
            &store,
            "default",
            "attempt",
            &json!({"state":"started"}),
            true,
        )
        .unwrap();
        record_attempt(
            &store,
            "default",
            "attempt",
            &json!({"state":"uncertain"}),
            false,
        )
        .unwrap();
        let events = rows(
            &store.db,
            "SELECT event,severity,subject FROM consumer_events",
            &[],
        )
        .unwrap();
        assert_eq!(events,json!([{"event":"metadata_publish","severity":"warning","subject":encode(&json!({"state":"uncertain"}))}]).as_array().unwrap().clone());
        assert!(
            record_attempt(
                &store,
                "default",
                "missing",
                &json!({"state":"verified"}),
                false
            )
            .is_err()
        );
        assert_eq!(
            rows(&store.db, "SELECT id FROM consumer_events", &[])
                .unwrap()
                .len(),
            1
        );
    }
}
