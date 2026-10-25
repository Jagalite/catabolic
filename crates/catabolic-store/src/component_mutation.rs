// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Current component evidence and accepted assertions. Proposal snapshot and
//! decision ownership remains in curation; this module owns assertion invariants.
// Unregistered fixed source composite; no public transfer authority constructor.
#[allow(dead_code)]
mod source_succession;
use crate::{Error, Result, Store, components, migration::rows, query};
use catabolic_core::{decode_json, encode};
use rusqlite::params;
use serde_json::{Value, json};
pub(crate) use source_succession::{PreparedAssertionTransfer, prepare_assertion_transfer};

pub fn get(store: &Store, profile: &str, identifier: &str, require_current: bool) -> Result<Value> {
    let _source_read = crate::catalog_source_succession::current::enter_read(store)?;
    let row = rows(
        &store.db,
        &format!(
            "SELECT * FROM ({}) WHERE profile=? AND occurrence_id=?",
            query::view_sql_for(store, "catalog_component_occurrences")
        ),
        &[profile.to_owned().into(), identifier.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown component occurrence in this profile".into()))?;
    decode_occurrence(store, profile, row, require_current)
}

/// Decode the frozen row shape once, after either a direct lookup or a bounded
/// evaluation-session lookup. Currentness always includes the live catalog's
/// recorded source-policy and file revision evidence.
pub(crate) fn decode_occurrence(
    store: &Store,
    profile: &str,
    mut row: Value,
    require_current: bool,
) -> Result<Value> {
    for key in [
        "locator",
        "snapshot",
        "technical",
        "observed",
        "asserted",
        "compatibility",
        "dependencies",
        "provenance",
        "conflicts",
    ] {
        if let Some(raw) = row[key].as_str() {
            row[key] = decode_json(raw)?;
        }
    }
    row["current"] = json!(
        row["current"].as_i64().unwrap_or(0) != 0
            && components::revision(&components::occurrence(
                &store.db,
                profile,
                row["file_id"].as_str().unwrap_or("")
            )?) == row["revision"]
    );
    if require_current
        && (row["current"] != true || row["conflicts"].as_array().is_some_and(|a| !a.is_empty()))
    {
        return Err(Error("stale_or_conflicting_component_occurrence".into()));
    }
    Ok(row)
}
fn invalid(message: &str) -> Error {
    Error(format!("invalid component assertion: {message}"))
}
fn object(value: &Value, allowed: &[&str]) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| invalid("expected an object"))?;
    if obj.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid("extra fields are not permitted"));
    }
    Ok(())
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| invalid(&format!("{key} must be a string")))
}
fn optional(value: &Value, key: &str, predicate: impl Fn(&Value) -> bool) -> Result<()> {
    if let Some(v) = value.get(key)
        && !v.is_null()
        && !predicate(v)
    {
        return Err(invalid(&format!("invalid {key}")));
    }
    Ok(())
}
fn compact_nulls(value: &mut Value) {
    match value {
        Value::Object(o) => {
            o.retain(|_, v| !v.is_null());
            for v in o.values_mut() {
                compact_nulls(v);
            }
        }
        Value::Array(a) => {
            for v in a {
                compact_nulls(v);
            }
        }
        _ => {}
    }
}
/// Strict frozen Claim schema; accepted canonical values/defaults match Python.
/// Structural diagnostic wording deliberately remains native, not Pydantic text.
pub fn normalize_claim(raw: &Value) -> Result<Value> {
    object(
        raw,
        &[
            "occurrence_id",
            "component_id",
            "attributes",
            "compatibility",
            "dependencies_complete",
            "dependencies",
            "supersedes",
        ],
    )?;
    text(raw, "occurrence_id")?;
    optional(raw, "component_id", Value::is_string)?;
    optional(raw, "dependencies_complete", Value::is_boolean)?;
    let mut claim = json!({"attributes":{},"dependencies":[],"supersedes":[]});
    for (k, v) in raw.as_object().unwrap() {
        claim[k] = v.clone();
    }
    let attrs = &claim["attributes"];
    object(
        attrs,
        &[
            "language",
            "title",
            "forced",
            "default",
            "commentary",
            "hearing_impaired",
            "visual_impaired",
        ],
    )?;
    for key in ["language", "title"] {
        optional(attrs, key, Value::is_string)?;
    }
    for key in [
        "forced",
        "default",
        "commentary",
        "hearing_impaired",
        "visual_impaired",
    ] {
        optional(attrs, key, Value::is_boolean)?;
    }
    if !claim["compatibility"].is_null() {
        let c = &mut claim["compatibility"];
        object(
            c,
            &[
                "edition_id",
                "video_file_id",
                "video_revision",
                "part",
                "timeline_id",
                "coverage",
                "offset_seconds",
                "synchronization",
            ],
        )?;
        for key in [
            "edition_id",
            "video_file_id",
            "video_revision",
            "timeline_id",
            "coverage",
            "synchronization",
        ] {
            text(c, key)?;
        }
        if c.get("part").is_none() || (!c["part"].is_null() && c["part"].as_i64().is_none()) {
            return Err(invalid("part must be an integer or null"));
        }
        let length = c["timeline_id"].as_str().unwrap().chars().count();
        if !(1..=256).contains(&length)
            || c["coverage"] != "full"
            || !["declared", "verified"].contains(&c["synchronization"].as_str().unwrap())
        {
            return Err(invalid("invalid compatibility declaration"));
        }
        let offset = c["offset_seconds"]
            .as_f64()
            .filter(|n| n.is_finite())
            .ok_or_else(|| invalid("offset_seconds must be finite"))?;
        c["offset_seconds"] = json!(offset);
    }
    let deps = claim["dependencies"]
        .as_array()
        .filter(|a| a.len() <= 32)
        .ok_or_else(|| invalid("dependencies must have at most 32 entries"))?;
    for dep in deps {
        object(dep, &["file_id", "revision", "purpose"])?;
        text(dep, "file_id")?;
        text(dep, "revision")?;
        if !["font", "subtitle_index", "subtitle_data", "support"].contains(&text(dep, "purpose")?)
        {
            return Err(invalid("invalid dependency purpose"));
        }
    }
    let supersedes = claim["supersedes"]
        .as_array()
        .filter(|a| a.len() <= 32)
        .ok_or_else(|| invalid("supersedes must have at most 32 entries"))?;
    if supersedes.iter().any(|v| !v.is_string()) {
        return Err(invalid("supersedes entries must be strings"));
    }
    compact_nulls(&mut claim);
    Ok(claim)
}
pub fn validate_claim(
    store: &Store,
    profile: &str,
    file: &str,
    item: &str,
    raw: &Value,
) -> Result<(Value, Value, Vec<Value>)> {
    let claim = normalize_claim(raw)?;
    let row = get(store, profile, text(&claim, "occurrence_id")?, false)?;
    if row["file_id"] != file || row["item_id"] != item || row["current"] != true {
        return Err(Error(
            "component assertion target is stale or mismatched".into(),
        ));
    }
    validate_claim_row(store, profile, file, item, claim, row)
}

// Shared invariants after the public owner has obtained current query evidence,
// or the private fixed succession owner has checked its exact planned post-row.
fn validate_claim_row(
    store: &Store,
    profile: &str,
    _file: &str,
    item: &str,
    claim: Value,
    row: Value,
) -> Result<(Value, Value, Vec<Value>)> {
    let logical = claim["component_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| row["component_id"].as_str().unwrap());
    if !store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM media_components WHERE id=? AND kind=?)",
        params![logical, row["kind"].as_str()],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(Error(
            "component identity requires an existing component of the same kind".into(),
        ));
    }
    for old in claim["supersedes"].as_array().unwrap() {
        if !store.db.query_row(
            &crate::catalog_source_succession::current::view(
                store.schema_version,
                "SELECT EXISTS(SELECT 1 FROM component_assertions WHERE id=? AND occurrence_id=?)",
            ),
            params![old.as_str(), row["occurrence_id"].as_str()],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error(
                "superseded assertion must belong to this occurrence".into(),
            ));
        }
    }
    let mut checks = vec![];
    if let Some(c) = claim.get("compatibility") {
        if c["part"] != row["part"] {
            return Err(Error(
                "component compatibility edition or part mismatch".into(),
            ));
        }
        let primary = rows(
            &store.db,
            "SELECT metadata FROM item_files WHERE item_id=? AND file_id=? AND role='primary' AND active=1 AND part IS ?",
            &[
                item.to_owned().into(),
                text(c, "video_file_id")?.to_owned().into(),
                c["part"].as_i64().into(),
            ],
        )?;
        if primary.is_empty() {
            return Err(Error(
                "compatibility requires a primary of this exact item and part".into(),
            ));
        }
        for p in primary {
            let metadata = decode_json(p["metadata"].as_str().unwrap())?;
            let edition = metadata
                .get("edition_id")
                .cloned()
                .unwrap_or_else(|| json!(item));
            if edition != c["edition_id"] {
                return Err(Error("component compatibility edition mismatch".into()));
            }
        }
        checks.push((text(c, "video_file_id")?, text(c, "video_revision")?));
    }
    for dep in claim["dependencies"].as_array().unwrap() {
        checks.push((text(dep, "file_id")?, text(dep, "revision")?));
    }
    let mut pinned = vec![];
    for (identifier, rev) in checks {
        let snap = components::occurrence(&store.db, profile, identifier)?;
        if snap["status"] != "present" || components::revision(&snap) != rev {
            return Err(Error(
                "stale component compatibility or dependency revision".into(),
            ));
        }
        pinned.push(snap);
    }
    Ok((claim, row, pinned))
}
pub fn accept_claim(store: &Store, profile: &str, proposal: &Value) -> Result<Value> {
    store.transaction(|db|{
        let raw=&proposal["payload"];let (claim,row,_)=validate_claim(store,profile,text(proposal,"file_id")?,text(raw,"item_id")?,&raw["component"])?;
        for old in claim["supersedes"].as_array().unwrap(){db.execute("UPDATE component_assertions SET active=0 WHERE id=?",[old.as_str().unwrap()])?;}
        let logical=claim["component_id"].as_str().filter(|s|!s.is_empty()).unwrap_or_else(||row["component_id"].as_str().unwrap());
        db.execute("INSERT INTO component_assertions(id,occurrence_id,component_id,payload) VALUES (?,?,?,?)",params![text(proposal,"id")?,row["occurrence_id"].as_str(),logical,encode(&claim)])?;
        Ok(json!({"component_id":logical,"occurrence_id":row["occurrence_id"]}))
    })
}

/// Revalidate all admitted evidence. Currentness alone is insufficient: accepted
/// language/compatibility/dependency assertions are part of the immutable plan.
pub fn validate_package(
    store: &Store,
    profile: &str,
    package: &Value,
    recipe: Option<&Value>,
    live: bool,
) -> Result<()> {
    let entries = package["components"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 17)
        .ok_or_else(|| Error("invalid component package".into()))?;
    if entries.iter().any(|entry| {
        catabolic_core::component::requires_native_binding_effect(&entry["compatibility"])
    }) {
        return Err(Error("native_binding_effect_unsupported".into()));
    }
    for entry in entries {
        let old = &entry["occurrence"];
        // A package captured before an explicit native claim has no native
        // marker. Recheck current typed ownership before permitting any effect.
        crate::catalog_identity_mutation::legacy_association_admission(
            store,
            profile,
            old["association_id"].as_str().unwrap_or(""),
        )?;
        let row = get(
            store,
            profile,
            old["occurrence_id"].as_str().unwrap_or(""),
            true,
        )?;
        if encode(&row) != encode(old) {
            return Err(Error(
                "component evidence changed since packaging admission".into(),
            ));
        }
    }
    let dependencies = package["dependencies"]
        .as_array()
        .filter(|a| a.len() <= 544)
        .ok_or_else(|| Error("invalid component dependencies".into()))?;
    for dep in dependencies {
        let snap = components::occurrence(&store.db, profile, text(dep, "file_id")?)?;
        if components::revision(&snap) != dep["revision"] {
            return Err(Error(
                "component dependency changed since packaging admission".into(),
            ));
        }
    }
    if live {
        for snapshot in entries
            .iter()
            .map(|e| &e["occurrence"]["snapshot"])
            .chain(dependencies.iter().map(|d| &d["snapshot"]))
        {
            crate::source::validate(snapshot)?;
        }
    }
    let action = package["packaging"]["action"].as_str().unwrap_or("");
    if !["extract", "convert", "mux"].contains(&action) {
        return Err(Error("package does not require processing".into()));
    }
    if action != "mux" && (entries.len() != 1 || !dependencies.is_empty()) {
        return Err(Error(
            "multi-component conversion is not supported by this operation".into(),
        ));
    }
    if let Some(recipe) = recipe {
        if package["packaging"]["operation_id"] != recipe["id"] {
            return Err(Error(
                "package requires its explicitly approved operation".into(),
            ));
        }
        let preset = recipe["preset"].as_str().unwrap_or("");
        if action == "mux" {
            if preset != "component-mux" {
                return Err(Error(
                    "multi-input packaging requires a component-mux operation".into(),
                ));
            }
            return Ok(());
        }
        if ![
            "subtitle-srt",
            "audio-flac",
            "audio-aac",
            "audio-opus",
            "audio-normalize",
            "remux-mkv",
        ]
        .contains(&preset)
        {
            return Err(Error(
                "multi-component conversion is not supported by this operation".into(),
            ));
        }
        let kind = if preset == "subtitle-srt" {
            Some("subtitle")
        } else if preset.starts_with("audio-") {
            Some("audio")
        } else {
            None
        };
        if kind.is_some_and(|kind| entries[0]["occurrence"]["kind"] != kind) {
            return Err(Error(
                "approved extraction operation has a different component kind".into(),
            ));
        }
    }
    Ok(())
}
pub fn extraction_plan(
    store: &Store,
    profile: &str,
    identifier: &str,
    operation: &str,
) -> Result<Value> {
    let row = get(store, profile, identifier, true)?;
    if row["technically_verified"].as_i64().unwrap_or(0) == 0
        || row["dependencies"].as_array().is_none_or(|a| !a.is_empty())
    {
        return Err(Error(
            "extraction requires verified technical evidence and no unsupported dependencies"
                .into(),
        ));
    }
    let mut package = json!({"version":1,"video":{"file_id":row["file_id"],"revision":row["revision"],"occurrence_id":identifier},"components":[{"requirement":"component","occurrence":row,"compatibility":{"offset_seconds":0.0}}],"dependencies":[],"packaging":{"action":"extract","ready":false,"operation_id":operation,"exposes_unselected_components":false}});
    package["signature"] = json!(catabolic_core::sha256(encode(&package)));
    let recipe = crate::artifact_mutation::get_recipe(store, operation)?;
    let row = &package["components"][0]["occurrence"];
    let index = &row["locator"]["index"];
    if recipe["preset"] == "remux-mkv" {
        if recipe["definition"]["stream_indices"] != json!([index]) {
            return Err(Error(
                "approved remux operation must select exactly this component".into(),
            ));
        }
    } else {
        let siblings = rows(
            &store.db,
            &format!(
                "SELECT locator FROM ({}) WHERE profile=? AND file_id=? AND association_id=? AND revision=? AND kind=? AND current=1",
                query::view_sql_for(store, "catalog_component_occurrences")
            ),
            &[
                profile.to_owned().into(),
                text(row, "file_id")?.to_owned().into(),
                text(row, "association_id")?.to_owned().into(),
                text(row, "revision")?.to_owned().into(),
                text(row, "kind")?.to_owned().into(),
            ],
        )?;
        let mut indexes = vec![];
        for sibling in siblings {
            let locator = decode_json(sibling["locator"].as_str().unwrap())?;
            indexes.push(
                locator["index"]
                    .as_i64()
                    .ok_or_else(|| Error("extraction needs an exact stream locator".into()))?,
            );
        }
        indexes.sort();
        let ordinal = recipe["definition"]["stream"].as_u64().unwrap_or(0) as usize;
        if indexes.get(ordinal).copied() != index.as_i64() {
            return Err(Error(
                "approved extraction operation selects a different stream".into(),
            ));
        }
    }
    validate_package(store, profile, &package, Some(&recipe), false)?;
    Ok(json!({"plan_id":package["signature"],"package":package}))
}
pub(crate) fn record_lineage(
    db: &rusqlite::Connection,
    artifact: &Value,
    validation: &Value,
    package: &Value,
) -> Result<()> {
    let entries = package["components"]
        .as_array()
        .ok_or_else(|| Error("invalid component package".into()))?;
    let rows = validation["components"]
        .as_array()
        .filter(|a| a.len() == entries.len())
        .ok_or_else(|| Error("component output lineage verification is incomplete".into()))?;
    if validation["component_signature"] != package["signature"] {
        return Err(Error(
            "component output signature does not match admission".into(),
        ));
    }
    for (ordinal, (row, entry)) in rows.iter().zip(entries).enumerate() {
        if row["occurrence_id"] != entry["occurrence"]["occurrence_id"]
            || row["output_stream_index"].as_i64().is_none_or(|i| i < 0)
            || !["extracted_copy", "extracted_conversion", "mux_copy"]
                .contains(&row["relationship"].as_str().unwrap_or(""))
        {
            return Err(Error("invalid component output lineage".into()));
        }
        db.execute(
            "INSERT OR IGNORE INTO component_output_lineage VALUES (?,?,?,?,?,?)",
            params![
                artifact["id"].as_str(),
                ordinal as i64,
                row["occurrence_id"].as_str(),
                row["output_stream_index"].as_i64(),
                row["relationship"].as_str(),
                entry["occurrence"]["component_id"].as_str()
            ],
        )?;
    }
    Ok(())
}
