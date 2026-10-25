// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Immutable output definitions and provenance-preserving output registration.
//! Registration establishes lineage and presence; it does not claim verified conversion.
use crate::{
    Error, Result, Store, components,
    migration::rows,
    mutation,
    query::{decode, view_sql_for},
    source,
};
use catabolic_core::{encode, encode_with, sha256};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

fn valid_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return Err(Error(
            "names must be 1–64 letters, digits, dots, underscores, or hyphens".into(),
        ));
    }
    Ok(())
}
fn vocabulary<'a>(value: &'a Value, allowed: &[&str], label: &str) -> Result<&'a str> {
    let name = value.as_str().unwrap_or("");
    let custom = name.strip_prefix("custom:").is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 63
            && s.as_bytes()[0].is_ascii_lowercase()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
    });
    if !allowed.contains(&name) && !custom {
        return Err(Error(format!(
            "unknown {label}: {value}; use a built-in name or custom:lowercase_name"
        )));
    }
    Ok(name)
}
const KINDS: &[&str] = &[
    "movie",
    "series",
    "season",
    "episode",
    "video",
    "music_video",
    "artist",
    "person",
    "album",
    "recording",
    "track",
    "audiobook",
    "audiobook_chapter",
    "podcast",
    "podcast_episode",
    "book",
    "book_edition",
    "comic_series",
    "comic_volume",
    "comic_issue",
    "photo",
    "photo_album",
    "collection",
    "document",
    "other",
];
pub fn definition(value: &Value) -> Result<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| Error("invalid output definition fields".into()))?;
    if object.keys().any(|k| {
        ![
            "version",
            "mode",
            "role",
            "item_kind",
            "relationship",
            "item_metadata",
            "file_metadata",
            "purpose",
        ]
        .contains(&k.as_str())
    }) {
        return Err(Error("invalid output definition fields".into()));
    }
    let mut result = json!({"version":if object.contains_key("purpose") {2} else {1},"mode":"same_item","role":"primary","file_metadata":{}});
    for (key, value) in object {
        result[key] = value.clone();
    }
    if !matches!(result["version"].as_u64(), Some(1 | 2)) {
        return Err(Error("unsupported output definition version".into()));
    }
    if result["version"] == 2 {
        if !matches!(
            result["purpose"].as_str(),
            Some("transcode" | "remux" | "preview" | "thumbnail" | "audio" | "subtitle" | "custom")
        ) {
            return Err(Error(
                "version 2 output definitions require a valid purpose".into(),
            ));
        }
    } else if result.get("purpose").is_some() {
        return Err(Error("purpose requires output definition version 2".into()));
    }
    vocabulary(
        &result["role"],
        &[
            "primary",
            "cover",
            "subtitle",
            "transcript",
            "lyrics",
            "thumbnail",
            "extra",
            "source",
        ],
        "file role",
    )?;
    match result["mode"].as_str() {
        Some("same_item") => {
            if ["item_kind", "relationship", "item_metadata"]
                .iter()
                .any(|k| result.get(k).is_some())
            {
                return Err(Error(
                    "same_item outputs cannot replace item metadata or relationships".into(),
                ));
            }
        }
        Some("new_item") => {
            vocabulary(&result["item_kind"], KINDS, "media kind")?;
            if result.get("relationship").is_none() {
                result["relationship"] = json!("derived_from");
            }
            if !matches!(
                result["relationship"].as_str(),
                Some("derived_from" | "edition_of")
            ) {
                return Err(Error(
                    "new_item outputs require derived_from or edition_of".into(),
                ));
            }
            if result.get("item_metadata").is_none() {
                result["item_metadata"] = json!({});
            }
        }
        _ => return Err(Error("output mode must be same_item or new_item".into())),
    }
    for key in ["file_metadata", "item_metadata"] {
        if result.get(key).is_some_and(|v| !v.is_object()) {
            return Err(Error(format!("{key} must be a JSON object")));
        }
    }
    // Python's size guard uses json.dumps' default ASCII escaping, not stored encoding.
    if encode_with(&result, true, false).len() > 65536 {
        return Err(Error("output definition exceeds 64 KiB".into()));
    }
    Ok(result)
}
pub fn get_definition(store: &Store, id: &str) -> Result<Value> {
    let mut value = rows(
        &store.db,
        "SELECT * FROM output_definitions WHERE id=?",
        &[id.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown output definition; use an immutable definition ID".into()))?;
    decode(&mut value, &["definition"])?;
    Ok(value)
}
pub fn define(store: &Store, name: &str, value: &Value) -> Result<Value> {
    valid_name(name)?;
    let serialized = encode(&definition(value)?);
    let digest = sha256(&serialized);
    let id = store.transaction(|db| {
        let old: Option<String> = db
            .query_row(
                "SELECT id FROM output_definitions WHERE name=? AND digest=?",
                params![name, digest],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = old {
            return Ok(id);
        }
        let revision: i64 = db.query_row(
            "SELECT coalesce(max(revision),0)+1 FROM output_definitions WHERE name=?",
            [name],
            |r| r.get(0),
        )?;
        let id = uuid::Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO output_definitions(id,name,revision,definition,digest) VALUES (?,?,?,?,?)",
            params![id, name, revision, serialized, digest],
        )?;
        Ok(id)
    })?;
    get_definition(store, &id)
}
pub fn get(store: &Store, profile: &str, id: &str) -> Result<Value> {
    let mut value = rows(
        &store.db,
        &format!(
            "SELECT * FROM ({}) WHERE profile=? AND id=?",
            view_sql_for(store, "catalog_renditions")
        ),
        &[profile.to_owned().into(), id.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown media output".into()))?;
    decode(&mut value, &["metadata"])?;
    Ok(value)
}
pub fn validate_source_item(
    store: &Store,
    profile: &str,
    file: &str,
    item: &str,
    value: &Value,
) -> Result<()> {
    validate_source_item_mode(store, profile, file, item, value, None)
}
pub fn validate_source_item_mode(
    store: &Store,
    profile: &str,
    file: &str,
    item: &str,
    value: &Value,
    lineage: Option<&str>,
) -> Result<()> {
    if lineage
        .is_some_and(|v| !["current_source_revision", "accepted_source_revision"].contains(&v))
    {
        return Err(Error("invalid source lineage mode".into()));
    }
    if !store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM item_files WHERE file_id=? AND item_id=? AND active=1)",
        params![file, item],
        |r| r.get::<_, bool>(0),
    )? {
        let outputs = rows(
            &store.db,
            &format!(
                "SELECT * FROM ({}) WHERE profile=? AND file_id=? AND item_id=?",
                view_sql_for(store, "catalog_renditions")
            ),
            &[
                profile.to_owned().into(),
                file.to_owned().into(),
                item.to_owned().into(),
            ],
        )?;
        if outputs.is_empty() {
            return Err(Error("input must have an active association or a validated rendition for the requested item".into()));
        }
        for output in outputs {
            if let Some(mode) = lineage {
                let (checks, _) =
                    crate::publication::fallback_evidence(store, profile, &output, mode, &[])?;
                for snapshot in checks {
                    source::validate(&snapshot)?;
                }
            } else {
                crate::publication::ready(store, profile, &output, &[])?;
            }
        }
    }
    if value["mode"] == "new_item" && value["relationship"] == "edition_of" {
        let kind: String =
            store
                .db
                .query_row("SELECT kind FROM items WHERE id=?", [item], |r| r.get(0))?;
        if !matches!(
            (value["item_kind"].as_str(), kind.as_str()),
            (Some("book_edition" | "audiobook"), "book") | (Some("movie"), "movie")
        ) {
            return Err(Error(format!(
                "invalid relationship: {} edition_of {kind}",
                value["item_kind"].as_str().unwrap_or("")
            )));
        }
    }
    Ok(())
}
/// Only call inside the owning registration/artifact transaction. Files and
/// content evidence must have been validated before this provenance write.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record(
    db: &Connection,
    profile: &str,
    file: &str,
    source_file: &str,
    source_item: &str,
    definition_id: &str,
    artifact: Option<&str>,
    metadata: &Value,
) -> Result<Value> {
    if db.is_autocommit() {
        return Err(Error(
            "output record requires the publication transaction".into(),
        ));
    }
    if db.query_row("WITH RECURSIVE ancestors(id) AS (VALUES (?) UNION SELECT m.source_file_id FROM media_outputs m JOIN ancestors a ON m.file_id=a.id) SELECT EXISTS(SELECT 1 FROM ancestors WHERE id=?)",params![source_file,file],|r|r.get::<_,bool>(0))? {return Err(Error("output lineage would create a cycle".into()));}
    let serialized: String = db.query_row(
        "SELECT definition FROM output_definitions WHERE id=?",
        [definition_id],
        |r| r.get(0),
    )?;
    let value: Value = catabolic_core::decode_json(&serialized)?;
    let item = if value["mode"] == "new_item" {
        let id = uuid::Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO items(id,kind,metadata) VALUES (?,?,?)",
            params![
                id,
                value["item_kind"].as_str(),
                encode(&value["item_metadata"])
            ],
        )?;
        db.execute(
            "INSERT INTO item_relationships(id,source_id,target_id,kind) VALUES (?,?,?,?)",
            params![
                uuid::Uuid::new_v4().to_string(),
                id,
                source_item,
                value["relationship"].as_str()
            ],
        )?;
        id
    } else {
        source_item.to_owned()
    };
    let mut payload = value["file_metadata"].clone();
    for (key, value) in metadata
        .as_object()
        .ok_or_else(|| Error("output metadata must be a JSON object".into()))?
    {
        payload[key] = value.clone();
    }
    if !db.query_row(
        "SELECT EXISTS(SELECT 1 FROM item_files WHERE file_id=? AND item_id=? AND role=?)",
        params![file, item, value["role"].as_str()],
        |r| r.get::<_, bool>(0),
    )? {
        db.execute("INSERT INTO item_files(id,file_id,item_id,role,metadata,origin,active) VALUES (?,?,?,?,?,'explicit',0)",params![uuid::Uuid::new_v4().to_string(),file,item,value["role"].as_str(),encode(&payload)])?;
    }
    let id = uuid::Uuid::new_v4().to_string();
    db.execute("INSERT INTO media_outputs(id,profile,file_id,source_file_id,source_item_id,item_id,definition_id,artifact_id,origin,metadata) VALUES (?,?,?,?,?,?,?,?,?,?)",params![id,profile,file,source_file,source_item,item,definition_id,artifact,if artifact.is_some(){"generated"}else{"registered"},encode(&payload)])?;
    Ok(json!({"output_id":id,"item_id":item}))
}
pub fn register(
    store: &Store,
    profile: &str,
    file: &str,
    source_file: &str,
    item: &str,
    definition_id: &str,
    metadata: &Value,
) -> Result<Value> {
    mutation::require_recovered(&store.db)?;
    mutation::profile(store, profile)?;
    let value = get_definition(store, definition_id)?["definition"].clone();
    let mut combined = value["file_metadata"].clone();
    for (key, value) in metadata
        .as_object()
        .ok_or_else(|| Error("output metadata must be a JSON object".into()))?
    {
        combined[key] = value.clone();
    }
    let payload = definition(&json!({"file_metadata":combined}))?["file_metadata"].clone();
    validate_source_item(store, profile, source_file, item, &value)?;
    let mut old = rows(
        &store.db,
        "SELECT * FROM media_outputs WHERE profile=? AND file_id=?",
        &[profile.to_owned().into(), file.to_owned().into()],
    )?;
    if let Some(row) = old.first_mut() {
        decode(row, &["metadata"])?;
        if row["origin"] == "registered"
            && row["source_file_id"] == source_file
            && row["source_item_id"] == item
            && row["definition_id"] == definition_id
            && row["metadata"] == payload
        {
            return Ok(row.clone());
        }
        return Err(Error(
            "file already has a different output registration".into(),
        ));
    }
    if store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM processing_artifacts WHERE file_id=?)",
        [file],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(Error(
            "generated files keep their recorded processing provenance".into(),
        ));
    }
    for file in [source_file, file] {
        source::validate(&components::occurrence(&store.db, profile, file)?)?;
    }
    let result = store.transaction(|db| {
        record(
            db,
            profile,
            file,
            source_file,
            item,
            definition_id,
            None,
            metadata,
        )
    })?;
    get(store, profile, result["output_id"].as_str().unwrap())
}
