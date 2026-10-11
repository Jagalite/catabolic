// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Native media identification, directed relationships and explicit expected sets.
//! Proposal/component assertions remain separate from these atomic primitives.
use crate::{
    Error, Result, Store, components,
    definitions::require_recovered,
    migration::{reference, rows},
    query::{decode, name},
    sql::scalar,
};
use catabolic_core::{decode_json, encode};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use uuid::Uuid;

fn fail<T>(message: &str) -> Result<T> {
    Err(Error(message.into()))
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| Error(format!("{key} must be text")))
}
fn get(value: &Value, key: &str, default: Value) -> Value {
    value.get(key).cloned().unwrap_or(default)
}
fn truth(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
fn allowed(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.keys().all(|k| keys.contains(&k.as_str())))
}
fn bounded_text<'a>(value: &'a Value, label: &str, max: usize) -> Result<&'a str> {
    value
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= max)
        .ok_or_else(|| {
            Error(format!(
                "{label} must be nonempty text of at most {max} bytes"
            ))
        })
}
fn payload(value: &Value) -> Result<()> {
    if !value.is_object() || encode(value).len() > 256 * 1024 {
        return fail("payload must be a JSON object of at most 256 KiB");
    }
    Ok(())
}
fn ordinal(value: &Value, label: &str) -> Result<()> {
    if !value.is_null()
        && value
            .as_i64()
            .is_none_or(|n| !(1..=2147483647).contains(&n))
    {
        return Err(Error(format!(
            "{label} must be an integer between 1 and 2147483647"
        )));
    }
    Ok(())
}
fn vocabulary(value: &str, label: &str) -> Result<()> {
    let custom = value.strip_prefix("custom:").is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 63
            && s.bytes().enumerate().all(|(i, c)| {
                c.is_ascii_lowercase() || i > 0 && (c.is_ascii_digit() || b"_.-".contains(&c))
            })
    });
    let valid = match label {
        "file role" => [
            "primary",
            "cover",
            "subtitle",
            "transcript",
            "lyrics",
            "thumbnail",
            "extra",
            "source",
        ]
        .contains(&value),
        "relationship kind" => [
            "part_of",
            "edition_of",
            "recording_of",
            "created_by",
            "performed_by",
            "narrated_by",
            "derived_from",
        ]
        .contains(&value),
        _ => reference()["media_types"]["kinds"].get(value).is_some(),
    };
    if !valid && !custom {
        return Err(Error(format!(
            "unknown {label}: {value}; use a built-in name or custom:lowercase_name"
        )));
    }
    Ok(())
}
fn identifier(store: &Store, value: &Value) -> Result<String> {
    let ns = Uuid::parse_str(&store.database_id).map_err(|e| Error(e.to_string()))?;
    Ok(Uuid::new_v5(&ns, encode(value).as_bytes()).to_string())
}
fn record(db: &Connection, table: &str, id: &str) -> Result<Value> {
    rows(
        db,
        &format!("SELECT * FROM {table} WHERE id=?"),
        &[id.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error(format!("unknown {table} ID: {id}")))
}
pub fn item_put(store: &Store, args: &Value) -> Result<Value> {
    let kind = text(args, "kind")?;
    vocabulary(kind, "media kind")?;
    let metadata = &args["metadata"];
    if !metadata.is_object() {
        return fail("item metadata must be a JSON object");
    }
    let identities = get(args, "identities", json!({}));
    let identities = identities
        .as_object()
        .ok_or_else(|| Error("identities must be an object".into()))?;
    let supplied = args["id"].as_str();
    if identities.is_empty() && supplied.is_none() {
        return fail("supply an authoritative identity or an explicit item ID");
    }
    for (namespace, value) in identities {
        name(namespace)?;
        if value.as_str().is_none_or(|s| s.trim().is_empty()) {
            return fail("identity values must be nonempty strings");
        }
    }
    let id=store.transaction(|db|{
        let mut matches=BTreeSet::new();
        for (namespace,value) in identities{for row in rows(db,"SELECT item_id FROM identities WHERE namespace=? AND value=?",&[namespace.clone().into(),value.as_str().unwrap().to_owned().into()])?{matches.insert(row["item_id"].as_str().unwrap().to_owned());}}
        if matches.len()>1||supplied.is_some_and(|id|!id.is_empty()&&!matches.is_empty()&&!matches.contains(id)){return fail("supplied identities refer to different media items");}
        let actual=supplied.filter(|s|!s.is_empty()).map(str::to_owned).or_else(||matches.into_iter().next()).unwrap_or_else(||Uuid::new_v4().to_string());
        let existing=rows(db,"SELECT kind FROM items WHERE id=?",&[actual.clone().into()])?;
        if existing.first().is_some_and(|r|r["kind"]!=kind){return fail("an existing item's kind cannot change");}
        db.execute("INSERT INTO items VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET metadata=excluded.metadata",params![actual,kind,encode(metadata)])?;
        for (namespace,value) in identities{db.execute("INSERT INTO identities VALUES (?,?,?) ON CONFLICT(namespace,value) DO NOTHING",params![namespace,value.as_str().unwrap(),actual])?;}
        Ok(actual)
    })?;
    Ok(
        json!({"id":id,"kind":kind,"metadata":metadata,"identities":rows(&store.db,"SELECT namespace,value FROM identities WHERE item_id=? ORDER BY namespace,value",&[id.clone().into()])?}),
    )
}
pub fn associate_in_transaction(
    store: &Store,
    profile: &str,
    db: &Connection,
    args: &Value,
    origin: &str,
) -> Result<String> {
    let file = text(args, "file_id")?;
    let item = text(args, "item_id")?;
    let role_value = get(args, "role", json!("primary"));
    let role = role_value.as_str().ok_or_else(|| {
        let shown = match &role_value {
            Value::Null => "None".into(),
            Value::Bool(true) => "True".into(),
            Value::Bool(false) => "False".into(),
            v => encode(v),
        };
        Error(format!(
            "unknown file role: {shown}; use a built-in name or custom:lowercase_name"
        ))
    })?;
    vocabulary(role, "file role")?;
    let mut part = args["part"].clone();
    ordinal(&part, "part")?;
    let clear = truth(&args["clear_part"]);
    if clear && !part.is_null() {
        return fail("part and clear-part cannot be combined");
    }
    let metadata = &args["metadata"];
    if !metadata.is_null() && !metadata.is_object() {
        return fail("association metadata must be a JSON object");
    }
    for (table, id) in [("files", file), ("items", item)] {
        record(db, table, id)?;
    }
    let existing = rows(
        db,
        "SELECT * FROM item_files WHERE file_id=? AND item_id=? AND role=?",
        &[
            file.to_owned().into(),
            item.to_owned().into(),
            role.to_owned().into(),
        ],
    )?;
    let existing = existing.first();
    if part.is_null()
        && !clear
        && let Some(row) = existing
    {
        part = row["part"].clone();
    }
    let id = existing
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .unwrap_or(identifier(
            store,
            &json!(["association", file, item, role]),
        )?);
    if !part.is_null()&&db.prepare("SELECT 1 FROM item_files WHERE item_id=? AND role=? AND part=? AND active=1 AND id!=?")?.exists(params![item,role,scalar(&part)?,id])?{return fail("that item/role already has a file at this part number");}
    let metadata = if !metadata.is_null() {
        encode(metadata)
    } else {
        existing
            .map(|r| r["metadata"].as_str().unwrap().to_owned())
            .unwrap_or("{}".into())
    };
    db.execute("INSERT INTO item_files(id,file_id,item_id,role,part,metadata,origin,active) VALUES (?,?,?,?,?,?,?,1) ON CONFLICT(id) DO UPDATE SET part=excluded.part,metadata=excluded.metadata,origin=excluded.origin,active=1",params![id,file,item,role,scalar(&part)?,metadata,origin])?;
    components::index_file(db, profile, file, false)?;
    Ok(id)
}
fn relationship(kind: &str, source: &str, target: &str, position: &Value) -> Result<()> {
    vocabulary(kind, "relationship kind")?;
    ordinal(position, "position")?;
    if !position.is_null() && kind != "part_of" {
        return fail("only part_of relationships have an ordered position");
    }
    if kind.starts_with("custom:") || kind == "derived_from" {
        return Ok(());
    }
    let valid = match kind {
        "part_of" => {
            target == "collection"
                || reference()["media_types"]["part_of_parents"][source]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == target))
        }
        "edition_of" => [
            ("book_edition", "book"),
            ("audiobook", "book"),
            ("movie", "movie"),
        ]
        .contains(&(source, target)),
        "recording_of" => source == "track" && target == "recording",
        _ => {
            ["person", "artist"].contains(&target)
                && (kind != "narrated_by" || ["audiobook", "audiobook_chapter"].contains(&source))
        }
    };
    if !valid {
        return Err(Error(format!(
            "invalid relationship: {source} {kind} {target}"
        )));
    }
    Ok(())
}
pub fn relate(store: &Store, args: &Value) -> Result<Value> {
    require_recovered(store)?;
    let source = text(args, "source")?;
    let target = text(args, "target")?;
    let kind = text(args, "kind")?;
    if source == target {
        return fail("an item cannot relate to itself");
    }
    let metadata = &args["metadata"];
    if !metadata.is_null() && !metadata.is_object() {
        return fail("relationship metadata must be a JSON object");
    }
    let id = identifier(store, &json!(["relationship", source, target, kind]))?;
    let mut position = args["position"].clone();
    let clear = truth(&args["clear_position"]);
    if clear && !position.is_null() {
        return fail("position and clear-position cannot be combined");
    }
    store.transaction(|db|{
        let existing=rows(db,"SELECT metadata,position FROM item_relationships WHERE id=?",&[id.clone().into()])?;
        if position.is_null()&&!clear&& let Some(row)=existing.first(){position=row["position"].clone();}
        let mut kinds=vec![];
        for item in [source,target]{kinds.push(rows(db,"SELECT kind FROM items WHERE id=?",&[item.to_owned().into()])?.first().ok_or_else(||Error(format!("unknown item: {item}")))?["kind"].as_str().unwrap().to_owned());}
        relationship(kind,&kinds[0],&kinds[1],&position)?;
        if ["part_of","edition_of","derived_from","recording_of"].contains(&kind)&&db.prepare("WITH RECURSIVE ancestors(id) AS (VALUES (?) UNION SELECT r.target_id FROM item_relationships r JOIN ancestors a ON r.source_id=a.id WHERE r.active=1 AND r.kind IN ('part_of','edition_of','derived_from','recording_of')) SELECT 1 FROM ancestors WHERE id=?")?.exists([target,source])?{return fail("relationship would create a structural cycle");}
        if !position.is_null()&&db.prepare("SELECT 1 FROM item_relationships WHERE target_id=? AND kind=? AND position=? AND active=1 AND id!=?")?.exists(params![target,kind,scalar(&position)?,id])?{return fail("that parent already has a child at this position");}
        let payload=if !metadata.is_null(){encode(metadata)}else{existing.first().map(|r|r["metadata"].as_str().unwrap().to_owned()).unwrap_or("{}".into())};
        db.execute("INSERT INTO item_relationships(id,source_id,target_id,kind,position,metadata,active) VALUES (?,?,?,?,?,?,1) ON CONFLICT(id) DO UPDATE SET position=excluded.position,metadata=excluded.metadata,active=1",params![id,source,target,kind,scalar(&position)?,payload])?;
        Ok(())
    })?;
    let mut value = record(&store.db, "item_relationships", &id)?;
    decode(&mut value, &["metadata"])?;
    Ok(value)
}
pub fn expected_put(store: &Store, collection: &str, definition: &Value) -> Result<Value> {
    payload(definition)?;
    if !allowed(
        definition,
        &["source", "edition", "ordering", "complete", "members"],
    ) {
        return fail("expected set supports source, edition, ordering, complete, members");
    }
    if !store
        .db
        .prepare("SELECT id FROM items WHERE id=?")?
        .exists([collection])?
    {
        return fail("unknown collection");
    }
    for field in ["source", "edition", "ordering"] {
        bounded_text(&definition[field], field, 4096)?;
    }
    if !definition["complete"].is_boolean() {
        return fail("expected set needs explicit complete true or false");
    }
    let members = definition["members"]
        .as_array()
        .filter(|v| v.len() <= 10000)
        .ok_or_else(|| Error("members must be a list of at most 10000 entries".into()))?;
    let mut keys = BTreeSet::new();
    for member in members {
        if !allowed(member, &["key", "title", "item_id", "identity"]) {
            return fail("member supports key, title, item_id or identity");
        }
        let key = bounded_text(&member["key"], "member key", 4096)?;
        if !keys.insert(key) {
            return fail("duplicate member key");
        }
        if member.get("item_id").is_some() == member.get("identity").is_some() {
            return fail("each member requires item_id or identity");
        }
        if member.get("item_id").is_some() {
            bounded_text(&member["item_id"], "member item id", 256)?;
        }
        if member.get("title").is_some() {
            bounded_text(&member["title"], "member title", 4096)?;
        }
        if member
            .get("identity")
            .is_some_and(|v| v.as_object().is_none_or(|o| o.len() != 1))
        {
            return fail("member identity requires one namespace/value pair");
        }
    }
    for member in members {
        if let Some(identity) = member["identity"].as_object() {
            for (namespace, value) in identity {
                bounded_text(&json!(namespace), "member identity namespace", 128)?;
                bounded_text(value, "member identity value", 4096)?;
            }
        }
    }
    let id = identifier(store, &json!([collection, definition]))?;
    store.transaction(|db|{db.execute("INSERT INTO expected_sets(id,collection_id,source,edition,ordering,complete,members) VALUES (?,?,?,?,?,?,?) ON CONFLICT(id) DO NOTHING",params![id,collection,definition["source"].as_str().unwrap(),definition["edition"].as_str().unwrap(),definition["ordering"].as_str().unwrap(),definition["complete"].as_bool().unwrap(),encode(&definition["members"])])?;Ok(())})?;
    let mut result = definition.clone();
    result["id"] = json!(id);
    result["collection_id"] = json!(collection);
    Ok(result)
}
pub fn completeness(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    let limit = get(args, "limit", json!(100))
        .as_u64()
        .filter(|n| (1..=1000).contains(n))
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))? as usize;
    let after = get(args, "after", json!(0))
        .as_u64()
        .ok_or_else(|| Error("after must be a nonnegative member offset".into()))?
        as usize;
    let id = text(args, "id")?;
    let mut value = rows(
        &store.db,
        "SELECT * FROM expected_sets WHERE id=?",
        &[id.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown expected set; supply an explicit snapshot id".into()))?;
    let members = decode_json(
        value
            .as_object_mut()
            .unwrap()
            .remove("members")
            .unwrap()
            .as_str()
            .unwrap(),
    )?;
    let members = members.as_array().unwrap();
    let mut result = vec![];
    for member in members.iter().skip(after).take(limit) {
        let ids = if let Some(item) = member["item_id"].as_str() {
            vec![item.to_owned()]
        } else {
            let mut ids = vec![];
            for (ns, val) in member["identity"].as_object().unwrap() {
                ids.extend(
                    rows(
                        &store.db,
                        "SELECT item_id FROM identities WHERE namespace=? AND value=?",
                        &[ns.clone().into(), val.as_str().unwrap().to_owned().into()],
                    )?
                    .iter()
                    .map(|r| r["item_id"].as_str().unwrap().to_owned()),
                );
            }
            ids
        };
        let mut states = vec![];
        for item in &ids {
            states.extend(rows(&store.db,"SELECT coalesce(o.status,'unknown') AS status FROM item_files a LEFT JOIN observations o ON o.file_id=a.file_id AND o.profile=? WHERE a.item_id=? AND a.active=1 AND a.role='primary'",&[profile.to_owned().into(),item.clone().into()])?.iter().map(|r|r["status"].as_str().unwrap().to_owned()));
        }
        let mut exists = false;
        for id in &ids {
            exists |= store
                .db
                .prepare("SELECT id FROM items WHERE id=?")?
                .exists([id])?;
        }
        let status = if states.iter().any(|s| s == "present") {
            "present"
        } else if !states.is_empty() && states.iter().all(|s| s == "missing") {
            "unavailable"
        } else if !states.is_empty() {
            "unknown"
        } else if !ids.is_empty() && exists {
            "unidentified"
        } else {
            "missing"
        };
        let mut row = member.clone();
        row["status"] = json!(status);
        result.push(row);
    }
    Ok(
        json!({"expected_set":value,"members":result,"expectations_complete":truth(&value["complete"]),"next_after":if after.saturating_add(limit)<members.len(){Some(after+limit)}else{None},"availability":"recorded observations; not a live source check"}),
    )
}

fn item_target(store: &Store, payload_value: &Value) -> Result<Option<String>> {
    let item = &payload_value["item"];
    if !item.is_object() {
        return fail("identify payload requires an item object");
    }
    if !allowed(item, &["id", "kind", "identities", "metadata"]) {
        return fail("item supports id, kind, identities, metadata");
    }
    let kind = bounded_text(&item["kind"], "media kind", 128)?;
    vocabulary(kind, "media kind")?;
    let identities = get(item, "identities", json!({}));
    let identities = identities
        .as_object()
        .ok_or_else(|| Error("identities must be an object".into()))?;
    payload(&get(item, "metadata", json!({})))?;
    let mut matches = BTreeSet::new();
    for (namespace, value) in identities {
        bounded_text(&json!(namespace), "identity namespace", 128)?;
        let value = bounded_text(value, "identity value", 4096)?;
        for row in rows(
            &store.db,
            "SELECT item_id FROM identities WHERE namespace=? AND value=?",
            &[namespace.clone().into(), value.to_owned().into()],
        )? {
            matches.insert(row["item_id"].as_str().unwrap().to_owned());
        }
    }
    let supplied = item["id"].as_str();
    if matches.len() > 1
        || truth(&item["id"])
            && !matches.is_empty()
            && supplied.is_none_or(|id| !matches.contains(id))
    {
        return fail("supplied identities refer to different media items");
    }
    if !item["id"].is_null() {
        bounded_text(&item["id"], "item id", 256)?;
    }
    if identities.is_empty() && !truth(&item["id"]) {
        return fail("supply an identity or explicit item id");
    }
    Ok(supplied
        .map(str::to_owned)
        .or_else(|| matches.into_iter().next()))
}
pub fn proposal_snapshot(
    store: &Store,
    profile: &str,
    file: &str,
    payload_value: &Value,
) -> Result<Value> {
    let target = if payload_value.get("item").is_some() {
        item_target(store, payload_value)?
    } else {
        payload_value["item_id"].as_str().map(str::to_owned)
    };
    let total: i64 = store.db.query_row(
        "SELECT coalesce(sum(length(metadata)),0) FROM item_files WHERE file_id=?",
        [file],
        |r| r.get(0),
    )?;
    if total > 256 * 1024 {
        return fail("association metadata exceeds the proposal snapshot budget");
    }
    let associations = rows(
        &store.db,
        "SELECT * FROM item_files WHERE file_id=? ORDER BY id LIMIT 1001",
        &[file.to_owned().into()],
    )?;
    if associations.len() > 1000 {
        return fail("proposal association snapshot exceeds 1000 records");
    }
    let component = if let Some(raw) = payload_value.get("component") {
        let (_, row, pins) = crate::component_mutation::validate_claim(
            store,
            profile,
            file,
            target.as_deref().unwrap_or(""),
            raw,
        )?;
        Some(json!({"occurrence":row,"pins":pins}))
    } else {
        None
    };
    let arg = target
        .map(rusqlite::types::Value::Text)
        .unwrap_or(rusqlite::types::Value::Null);
    let mut result = json!({"file":components::occurrence(&store.db,profile,file)?,"associations":associations,"item":rows(&store.db,"SELECT * FROM items WHERE id=?",std::slice::from_ref(&arg))?,"identities":rows(&store.db,"SELECT * FROM identities WHERE item_id=? ORDER BY namespace,value",&[arg])?});
    if let Some(component) = component {
        result["component"] = component;
    }
    Ok(result)
}
pub fn proposal_get(store: &Store, profile: &str, id: &str) -> Result<Value> {
    let mut value = rows(
        &store.db,
        "SELECT * FROM proposals WHERE id=? AND profile=?",
        &[id.to_owned().into(), profile.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown proposal in this profile".into()))?;
    decode(&mut value, &["payload", "evidence", "snapshot", "result"])?;
    Ok(value)
}
pub fn proposal_put(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    require_recovered(store)?;
    let value = &args["payload"];
    payload(value)?;
    if !allowed(
        value,
        &[
            "item",
            "item_id",
            "role",
            "part",
            "metadata",
            "overwrite_fields",
            "component",
        ],
    ) {
        return fail("proposal supports item or item_id, role, part, metadata, overwrite_fields");
    }
    if value.get("item").is_some() == value.get("item_id").is_some() {
        return fail("supply exactly one item or item_id");
    }
    if value.get("component").is_some() && value.as_object().unwrap().len() != 2 {
        return fail("component assertions require item_id and cannot mutate associations");
    }
    let role = get(value, "role", json!("primary"));
    vocabulary(role.as_str().unwrap_or("None"), "file role")?;
    if value.get("metadata").is_some() {
        payload(&value["metadata"])?;
    }
    let fields = get(value, "overwrite_fields", json!([]));
    if fields
        .as_array()
        .is_none_or(|a| a.len() > 100 || a.iter().any(|v| !v.is_string()))
    {
        return fail("overwrite_fields must be a list of field names");
    }
    let source = get(args, "source", json!("manual"));
    let source = bounded_text(&source, "source", 4096)?;
    let evidence = args
        .get("evidence")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!({}));
    payload(&evidence)?;
    let file = text(args, "file_id")?;
    let snapshot = proposal_snapshot(store, profile, file, value)?;
    payload(&snapshot)?;
    if value.get("item_id").is_some() && snapshot["item"].as_array().unwrap().is_empty() {
        return fail("unknown target item");
    }
    let id = identifier(
        store,
        &json!([profile, file, value, source, evidence, snapshot]),
    )?;
    store.transaction(|db|{db.execute("INSERT INTO proposals(id,profile,file_id,kind,payload,evidence,source,snapshot) VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(id) DO NOTHING",params![id,profile,file,if value.get("item").is_some(){"identify"}else{"associate"},encode(value),encode(&evidence),source,encode(&snapshot)])?;Ok(())})?;
    proposal_get(store, profile, &id)
}
pub fn proposal_decide(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    require_recovered(store)?;
    let actor = bounded_text(&args["actor"], "decision actor", 4096)?;
    let id = text(args, "id")?;
    let proposal = proposal_get(store, profile, id)?;
    let accept = truth(&args["accept"]);
    let desired = if accept { "accepted" } else { "rejected" };
    if proposal["state"] == desired {
        return Ok(proposal);
    }
    if proposal["state"] != "pending" {
        return fail("proposal already decided; submit a new proposal for a changed decision");
    }
    let value = &proposal["payload"];
    let file = proposal["file_id"].as_str().unwrap();
    let before = proposal_snapshot(store, profile, file, value)?;
    if accept && before != proposal["snapshot"] {
        return fail("proposal is stale; inventory or curation changed; submit a new proposal");
    }
    if accept {
        crate::source::with_validated_source(&before["file"], |_| Ok(()))?;
    }
    store.transaction(|db|{
        let mut result=json!({});
        if accept{
            let item=if value.get("item").is_some(){
                let item=&value["item"];
                let id=item_target(store,value)?.unwrap_or_else(||Uuid::new_v4().to_string());
                let mut metadata=before["item"].as_array().unwrap().first().map(|r|decode_json(r["metadata"].as_str().unwrap())).transpose()?.unwrap_or(json!({}));
                let proposed=get(item,"metadata",json!({}));let mut preserved=vec![];
                for (key,v) in proposed.as_object().unwrap(){
                    if metadata.get(key).is_none()||value["overwrite_fields"].as_array().is_some_and(|a|a.iter().any(|field|field==key)) {metadata[key]=v.clone();}
                    else if metadata[key]!=*v{preserved.push(key.clone());}
                }
                item_put(store,&json!({"kind":item["kind"],"identities":get(item,"identities",json!({})),"metadata":metadata,"id":id}))?;
                result["preserved_fields"]=json!(preserved);id
            }else{value["item_id"].as_str().unwrap().to_owned()};
            if value.get("component").is_some() {
                let accepted = crate::component_mutation::accept_claim(store,profile,&proposal)?;
                result.as_object_mut().unwrap().extend(accepted.as_object().unwrap().clone());
            } else {
                let association=associate_in_transaction(store,profile,db,&json!({"file_id":file,"item_id":item,"role":get(value,"role",json!("primary")),"part":value["part"],"metadata":value["metadata"]}),"explicit")?;
                result["item_id"]=json!(item);result["association_id"]=json!(association);
            }
        }
        db.execute("UPDATE proposals SET state=?,result=? WHERE id=?",params![desired,encode(&result),id])?;
        db.execute("INSERT INTO decision_events(proposal_id,action,actor,before_value,after_value) VALUES (?,?,?,?,?)",params![id,desired,actor,encode(&before),encode(&proposal_snapshot(store,profile,file,value)?)])?;
        Ok(())
    })?;
    proposal_get(store, profile, id)
}
pub fn accept_existing_association_evidence(
    store: &Store,
    profile: &str,
    args: &Value,
) -> Result<Value> {
    require_recovered(store)?;
    let id = text(args, "id")?;
    let proposal = proposal_get(store, profile, id)?;
    let value = &proposal["payload"];
    if value.as_object().is_none_or(|o| {
        o.len() != 3
            || !["item_id", "role", "part"]
                .iter()
                .all(|k| o.contains_key(*k))
    }) {
        return fail("existing-association evidence cannot modify metadata");
    }
    if proposal["state"] == "accepted" {
        return Ok(proposal);
    }
    if proposal["state"] != "pending" {
        return fail("proposal already decided");
    }
    let before = proposal_snapshot(store, profile, proposal["file_id"].as_str().unwrap(), value)?;
    if before != proposal["snapshot"] {
        return fail("proposal is stale");
    }
    let matches = before["associations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| {
            truth(&a["active"])
                && a["item_id"] == value["item_id"]
                && a["role"] == value["role"]
                && a["part"] == value["part"]
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return fail("evidence requires exactly one existing association");
    }
    crate::source::with_validated_source(&before["file"], |_| Ok(()))?;
    let actor = bounded_text(&args["actor"], "decision actor", 4096)?;
    let result = json!({"item_id":value["item_id"],"association_id":matches[0]["id"]});
    store.transaction(|db| {
        db.execute("UPDATE proposals SET state='accepted',result=? WHERE id=?",params![encode(&result),id])?;
        db.execute("INSERT INTO decision_events(proposal_id,action,actor,before_value,after_value) VALUES (?,?,?,?,?)",params![id,"accepted",actor,encode(&before),encode(&before)])?;
        Ok(())
    })?;
    proposal_get(store, profile, id)
}
fn proposal_page(store: &Store, profile: &str, args: &Value, history: bool) -> Result<Value> {
    let limit = get(args, "limit", json!(100))
        .as_u64()
        .filter(|n| (1..=1000).contains(n))
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))? as usize;
    if history {
        let after = get(args, "after", json!(0));
        let mut values = rows(
            &store.db,
            "SELECT e.* FROM decision_events e JOIN proposals p ON p.id=e.proposal_id WHERE p.profile=? AND e.id>? ORDER BY e.id LIMIT ?",
            &[
                profile.to_owned().into(),
                scalar(&after)?,
                ((limit + 1) as i64).into(),
            ],
        )?;
        let more = values.len() > limit;
        values.truncate(limit);
        let next = if more {
            values.last().unwrap()["id"].clone()
        } else {
            Value::Null
        };
        for row in &mut values {
            decode(row, &["before_value", "after_value"])?;
        }
        Ok(json!({"events":values,"next_after":next}))
    } else {
        let state = &args["state"];
        if !state.is_null() && !matches!(state.as_str(), Some("pending" | "accepted" | "rejected"))
        {
            return fail("invalid proposal state");
        }
        let mut values = rows(
            &store.db,
            "SELECT id,profile,file_id,kind,state,source,created_at FROM proposals WHERE profile=? AND id>? AND (? IS NULL OR state=?) ORDER BY id LIMIT ?",
            &[
                profile.to_owned().into(),
                get(args, "after", json!(""))
                    .as_str()
                    .unwrap_or("")
                    .to_owned()
                    .into(),
                scalar(state)?,
                scalar(state)?,
                ((limit + 1) as i64).into(),
            ],
        )?;
        let mut used = 0;
        let mut count = 0;
        for value in &values {
            let n = encode(value).len();
            if count == limit || used + n > 4 * 1024 * 1024 {
                if count == 0 {
                    return fail("record exceeds the result budget");
                }
                break;
            }
            used += n;
            count += 1;
        }
        let more = count < values.len();
        values.truncate(count);
        let next = if more {
            values.last().unwrap()["id"].clone()
        } else {
            Value::Null
        };
        Ok(json!({"proposals":values,"next_after":next}))
    }
}
/// Recorded-only sidecar discovery. Candidates remain proposals even when a
/// single primary matches; discovery never silently identifies a file.
pub fn sidecars(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    let limit = get(args, "limit", json!(100))
        .as_u64()
        .filter(|n| (1..=1000).contains(n))
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))? as usize;
    let location = &args["location"];
    let mut files = rows(
        &store.db,
        "SELECT f.* FROM files f JOIN observations o ON o.file_id=f.id AND o.profile=? WHERE o.status='present' AND f.id>? AND (? IS NULL OR f.location=?) ORDER BY f.id LIMIT ?",
        &[
            profile.to_owned().into(),
            get(args, "after", json!(""))
                .as_str()
                .unwrap_or("")
                .to_owned()
                .into(),
            scalar(location)?,
            scalar(location)?,
            ((limit + 1) as i64).into(),
        ],
    )?;
    let more = files.len() > limit;
    files.truncate(limit);
    let mut candidates = vec![];
    for row in &files {
        let path = std::path::Path::new(row["path"].as_str().unwrap());
        let suffix = path
            .extension()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_lowercase();
        let role = match suffix.as_str() {
            "srt" | "vtt" | "ass" | "ssa" => "subtitle",
            "jpg" | "jpeg" | "png" | "webp" => "cover",
            _ => continue,
        };
        let parent = path.parent().and_then(|p| p.to_str()).unwrap_or("");
        let pattern = if parent.is_empty() {
            String::new()
        } else {
            format!("{parent}/")
        };
        let primary = rows(
            &store.db,
            "SELECT DISTINCT a.item_id,f.id,f.path FROM item_files a JOIN files f ON f.id=a.file_id JOIN observations o ON o.file_id=f.id AND o.profile=? WHERE f.location=? AND a.active=1 AND a.role='primary' AND o.status='present' AND f.path>=? AND f.path<? AND instr(substr(f.path,length(?)+1),'/')=0 AND f.id!=? LIMIT 1001",
            &[
                profile.to_owned().into(),
                row["location"].as_str().unwrap().to_owned().into(),
                pattern.clone().into(),
                format!("{pattern}\u{10ffff}").into(),
                pattern.into(),
                row["id"].as_str().unwrap().to_owned().into(),
            ],
        )?;
        if primary.len() > 1000 {
            return fail(
                "sidecar candidate scope exceeds 1000 primary files; use a smaller source scope",
            );
        }
        let original_stem = path.file_stem().and_then(|p| p.to_str()).unwrap_or("");
        let stem = crate::sql::casefold(original_stem);
        let matches = primary
            .iter()
            .filter(|other| {
                let other_path = std::path::Path::new(other["path"].as_str().unwrap());
                if other_path.parent() != path.parent() {
                    return false;
                }
                let base = crate::sql::casefold(
                    other_path
                        .file_stem()
                        .and_then(|p| p.to_str())
                        .unwrap_or(""),
                );
                stem == base
                    || stem.starts_with(&(base + "."))
                    || role == "cover" && ["cover", "folder", "poster"].contains(&stem.as_str())
            })
            .collect::<Vec<_>>();
        let ids = matches
            .iter()
            .map(|m| m["item_id"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        let tags = original_stem
            .split('.')
            .skip(1)
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        for item in &ids {
            let mut metadata = json!({"forced":tags.iter().any(|t|t=="forced"),"sdh":tags.iter().any(|t|["sdh","hi","cc"].contains(&t.as_str()))});
            for other in &matches {
                if other["item_id"] != *item {
                    continue;
                }
                let other_stem = std::path::Path::new(other["path"].as_str().unwrap())
                    .file_stem()
                    .and_then(|p| p.to_str())
                    .unwrap_or("");
                let tail = original_stem
                    .chars()
                    .skip(other_stem.chars().count())
                    .collect::<String>();
                let languages = tail
                    .trim_matches('.')
                    .split('.')
                    .filter(|t| {
                        t.chars().all(char::is_alphabetic)
                            && (2..=3).contains(&t.chars().count())
                            && !(["sdh", "hi", "cc"].contains(&t.to_lowercase().as_str()))
                    })
                    .map(str::to_lowercase)
                    .collect::<Vec<_>>();
                if languages.len() == 1 {
                    metadata["language"] = json!(languages[0]);
                }
            }
            metadata["primary_file_ids"] = json!(
                matches
                    .iter()
                    .filter(|m| m["item_id"] == *item)
                    .map(|m| m["id"].as_str().unwrap())
                    .collect::<BTreeSet<_>>()
            );
            let payload = json!({"item_id":item,"role":role,"metadata":metadata});
            let evidence = json!({"method":"same-directory-stem-v1","path":row["path"],"ambiguous":ids.len()>1,"candidate_items":ids});
            let mut value = json!({"file_id":row["id"],"payload":payload,"evidence":evidence});
            if truth(&args["apply"]) {
                value["proposal"]=proposal_put(store,profile,&json!({"file_id":row["id"],"payload":payload,"source":"sidecar-discovery","evidence":evidence}))?["id"].clone();
            }
            candidates.push(value);
        }
    }
    Ok(
        json!({"candidates":candidates,"complete":true,"next_after":if more{files.last().unwrap()["id"].clone()}else{Value::Null}}),
    )
}
pub fn dispatch(store: &Store, profile: &str, action: &str, args: &Value) -> Result<Value> {
    match action {
        "proposal-put" => proposal_put(store, profile, args),
        "proposal-get" => proposal_get(store, profile, text(args, "id")?),
        "proposal-decide" => proposal_decide(store, profile, args),
        "proposal-accept-existing" => accept_existing_association_evidence(store, profile, args),
        "proposal-list" => proposal_page(store, profile, args, false),
        "proposal-history" => proposal_page(store, profile, args, true),
        "sidecars" => sidecars(store, profile, args),
        "item-put" => item_put(store, args),
        "associate" => {
            require_recovered(store)?;
            let id = store
                .transaction(|db| associate_in_transaction(store, profile, db, args, "explicit"))?;
            let mut value = record(&store.db, "item_files", &id)?;
            decode(&mut value, &["metadata"])?;
            Ok(value)
        }
        "association-disable" => {
            require_recovered(store)?;
            let id = text(args, "id")?;
            store.transaction(|db|{
                let row=rows(db,"SELECT * FROM item_files WHERE id=?",&[id.to_owned().into()])?.into_iter().next().ok_or_else(||Error(format!("unknown association: {id}")))?;
                let file=row["file_id"].as_str().unwrap();let item=row["item_id"].as_str().unwrap();
                if truth(&row["active"])&&db.prepare("SELECT 1 FROM mappings WHERE file_id=? AND item_id=? AND active=1")?.exists([file,item])?&&!db.prepare("SELECT 1 FROM item_files WHERE file_id=? AND item_id=? AND active=1 AND id!=?")?.exists([file,item,id])?{return fail("disable active catalog mappings before removing their last identification");}
                db.execute("UPDATE item_files SET active=0 WHERE id=?",[id])?;Ok(())
            })?;
            Ok(json!({"id":id,"active":false}))
        }
        "relate" => relate(store, args),
        "relationship-disable" => {
            require_recovered(store)?;
            let id = text(args, "id")?;
            store.transaction(|db| {
                if !db
                    .prepare("SELECT 1 FROM item_relationships WHERE id=?")?
                    .exists([id])?
                {
                    return Err(Error(format!("unknown relationship: {id}")));
                }
                db.execute("UPDATE item_relationships SET active=0 WHERE id=?", [id])?;
                Ok(())
            })?;
            Ok(json!({"id":id,"active":false}))
        }
        "expected-put" => expected_put(store, text(args, "collection")?, &args["value"]),
        "completeness" => completeness(store, profile, args),
        _ => Err(Error(format!("unknown curation action: {action}"))),
    }
}
