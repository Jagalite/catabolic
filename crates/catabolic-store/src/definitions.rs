// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Native immutable query/rule definitions and catalog-scoped projection settings.
//! All mutations use the catalog writer transaction; no query text is executed here.
use crate::{
    Error, Result, Store, layout,
    migration::rows,
    query::{decode, name},
    selection,
};
use catabolic_core::{encode, sha256};
use rusqlite::{Connection, OptionalExtension, params, types::Value as SqlValue};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use uuid::Uuid;

fn fail<T>(message: &str) -> Result<T> {
    Err(Error(message.into()))
}
fn field<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .ok_or_else(|| Error(format!("{key} must be text")))
}
fn allowed(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.keys().all(|k| keys.contains(&k.as_str())))
}
fn python_truth(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
pub fn require_recovered(store: &Store) -> Result<()> {
    if store
        .db
        .prepare("SELECT id FROM journal LIMIT 1")?
        .exists([])?
    {
        return fail("recover pending operations before changing desired catalog state");
    }
    Ok(())
}
fn one(db: &Connection, sql: &str, args: &[SqlValue], missing: &str) -> Result<Value> {
    rows(db, sql, args)?
        .into_iter()
        .next()
        .ok_or_else(|| Error(missing.into()))
}
fn limit(args: &Value) -> Result<usize> {
    args.get("limit")
        .unwrap_or(&json!(100))
        .as_u64()
        .filter(|n| (1..=1000).contains(n))
        .map(|n| n as usize)
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))
}
// Stop at the byte boundary while stepping SQLite; do not materialize an
// entire 1,001-record page before applying the four-MiB response budget.
fn page_rows(db: &Connection, sql: &str, args: &[SqlValue]) -> Result<Vec<Value>> {
    let mut statement = db.prepare(sql)?;
    let names = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let mut cursor = statement.query(rusqlite::params_from_iter(args))?;
    let mut records = vec![];
    let mut used = 0;
    while let Some(row) = cursor.next()? {
        let mut value = serde_json::Map::new();
        for (index, name) in names.iter().enumerate() {
            value.insert(name.clone(), crate::migration::value(row.get_ref(index)?));
        }
        let value = Value::Object(value);
        used += encode(&value).len();
        records.push(value);
        if used > 4 * 1024 * 1024 {
            break;
        }
    }
    Ok(records)
}
fn bounded_page(
    mut values: Vec<Value>,
    limit: usize,
    key: &str,
    fields: &[&str],
    bools: &[&str],
) -> Result<Value> {
    let mut used = 0;
    let mut count = 0;
    for row in &values {
        let size = encode(row).len();
        if count == limit || used + size > 4 * 1024 * 1024 {
            if count == 0 {
                return fail("record exceeds the result budget");
            }
            break;
        }
        used += size;
        count += 1;
    }
    let more = count < values.len();
    values.truncate(count);
    let next = if more {
        values.last().unwrap()["id"].clone()
    } else {
        Value::Null
    };
    for row in &mut values {
        decode(row, fields)?;
        for key in bools {
            row[*key] = json!(python_truth(&row[*key]));
        }
    }
    Ok(json!({key: values, "next_after": next}))
}
fn params_valid(value: &Value) -> Result<()> {
    crate::sql::named_parameters(value).map(drop)
}
pub fn query_get(store: &Store, profile: &str, id: &str) -> Result<Value> {
    let mut value = one(
        &store.db,
        "SELECT * FROM saved_queries WHERE id=? AND profile=?",
        &[id.to_owned().into(), profile.to_owned().into()],
        "unknown query revision in this profile",
    )?;
    decode(&mut value, &["definition"])?;
    Ok(value)
}
pub fn query_validate(store: &Store, profile: &str, input: &Value) -> Result<Value> {
    if !allowed(
        input,
        &[
            "version",
            "mode",
            "selection",
            "combine",
            "queries",
            "entity",
            "max_ids",
            "timeout_ms",
        ],
    ) {
        return fail("invalid query definition fields");
    }
    let mut value = json!({"version": 1, "mode": "selection"});
    value
        .as_object_mut()
        .unwrap()
        .extend(input.as_object().unwrap().clone());
    if value["version"].as_i64() != Some(1)
        || !matches!(
            value["mode"].as_str(),
            Some("selection" | "rows" | "document")
        )
    {
        return fail("query requires version 1 and mode selection, rows or document");
    }
    if !value["entity"].is_null()
        && !matches!(
            value["entity"].as_str(),
            Some("item_id" | "file_id" | "association_id" | "component_id" | "occurrence_id")
        )
    {
        return fail(
            "query selection entity must be item_id, file_id, association_id, component_id or occurrence_id",
        );
    }
    if value["selection"].is_object() {
        for (key, default) in [("max_ids", 10000), ("timeout_ms", 5000)] {
            let inherited = value["selection"]
                .get(key)
                .cloned()
                .unwrap_or(json!(default));
            value
                .as_object_mut()
                .unwrap()
                .entry(key)
                .or_insert(inherited);
        }
    }
    for (key, default, max) in [("max_ids", 10000, 100000), ("timeout_ms", 5000, 60000)] {
        if value
            .get(key)
            .unwrap_or(&json!(default))
            .as_u64()
            .is_none_or(|n| !(1..=max).contains(&n))
        {
            return Err(Error(format!("invalid query {key}")));
        }
    }
    if value.get("combine").is_some() {
        if value["mode"] != "selection"
            || value.get("selection").is_some()
            || !matches!(
                value["combine"].as_str(),
                Some("union" | "intersection" | "difference")
            )
        {
            return fail("combine requires a selection union, intersection or difference");
        }
        let refs = value["queries"]
            .as_array()
            .filter(|r| (2..=16).contains(&r.len()) && r.iter().all(Value::is_string))
            .ok_or_else(|| Error("composition requires 2..16 query revision IDs".into()))?;
        for id in refs {
            if query_get(store, profile, id.as_str().unwrap())?["definition"]["mode"] != "selection"
            {
                return fail("only typed selections can be composed");
            }
        }
    } else {
        if value.get("queries").is_some() || !value["selection"].is_object() {
            return fail("query requires selection or composition");
        }
        let mut body = json!({"profile": profile});
        body.as_object_mut()
            .unwrap()
            .extend(value["selection"].as_object().unwrap().clone());
        if body["profile"] != profile || body.get("query_id").is_some() {
            return fail("query body must use its profile and inline SQL/GraphQL");
        }
        if value["mode"] == "selection" {
            // Selection admission includes the complete SQL parameter contract.
            selection::validate(&body)?;
        } else {
            let expected = if value["mode"] == "rows" {
                "sql"
            } else {
                "graphql"
            };
            if body["language"] != expected
                || !allowed(
                    &body,
                    &[
                        "language",
                        "profile",
                        "query",
                        "params",
                        "variables",
                        "timeout_ms",
                    ],
                )
            {
                return fail("rows uses SQL; document uses GraphQL");
            }
            if body["query"]
                .as_str()
                .is_none_or(|q| q.trim().is_empty() || q.len() > crate::sql::MAX_SQL_BYTES)
            {
                return fail("query text must be nonempty and bounded");
            }
            if expected == "sql" {
                params_valid(body.get("params").unwrap_or(&json!({})))?;
            } else if !body.get("variables").unwrap_or(&json!({})).is_object() {
                return fail("GraphQL variables must be an object");
            }
        }
        value["selection"] = body;
    }
    if encode(&value).len() > 1024 * 1024 {
        return fail("query definition exceeds 1 MiB");
    }
    Ok(value)
}
pub fn query_put(store: &Store, profile: &str, query_name: &str, value: &Value) -> Result<Value> {
    name(query_name)?;
    let value = query_validate(store, profile, value)?;
    let serialized = encode(&value);
    let digest = sha256(&serialized);
    let id = store.transaction(|db| {
        if let Some(id) = db.query_row("SELECT id FROM saved_queries WHERE profile=? AND name=? AND digest=?", params![profile,query_name,digest], |r| r.get::<_,String>(0)).optional()? { return Ok(id); }
        let revision: i64 = db.query_row("SELECT coalesce(max(revision),0)+1 FROM saved_queries WHERE profile=? AND name=?", params![profile,query_name], |r| r.get(0))?;
        let id = Uuid::new_v4().to_string();
        db.execute("INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES (?,?,?,?,?,?)", params![id,profile,query_name,revision,serialized,digest])?;
        let refs: BTreeSet<&str> = value["queries"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        for reference in refs { db.execute("INSERT INTO query_dependencies VALUES (?,?)", params![id,reference])?; }
        Ok(id)
    })?;
    query_get(store, profile, &id)
}
fn rule_decode(mut row: Value) -> Result<Value> {
    decode(&mut row, &["selection", "estimate_options"])?;
    for key in ["enabled", "required"] {
        row[key] = json!(python_truth(&row[key]));
    }
    Ok(row)
}
pub fn rule_get(store: &Store, profile: &str, id: &str) -> Result<Value> {
    rule_decode(one(
        &store.db,
        "SELECT * FROM processing_rules WHERE id=? AND profile=?",
        &[id.to_owned().into(), profile.to_owned().into()],
        "unknown rule revision in this profile",
    )?)
}
fn estimate_options(value: &Value) -> Result<()> {
    if !allowed(value, &["video_kbps"]) {
        return fail("estimate options accept only video_kbps");
    }
    if value
        .get("video_kbps")
        .is_some_and(|v| v.as_u64().is_none_or(|n| !(1..=1000000).contains(&n)))
    {
        return fail("estimated video_kbps must be 1..1000000");
    }
    Ok(())
}
pub fn rule_put(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    require_recovered(store)?;
    let rule_name = field(args, "name")?;
    name(rule_name)?;
    let recipe = field(args, "recipe_id")?;
    let operation = one(
        &store.db,
        "SELECT * FROM processing_recipes WHERE id=?",
        &[recipe.to_owned().into()],
        "unknown immutable operation revision",
    )?;
    if !args["selection"].is_object() {
        return fail("selection must be a SQL/GraphQL selection object");
    }
    let mut body = json!({"profile":profile});
    body.as_object_mut()
        .unwrap()
        .extend(args["selection"].as_object().unwrap().clone());
    selection::validate(&body)?;
    if let Some(id) = body.get("query_id")
        && query_get(store, profile, id.as_str().unwrap())?["definition"]["mode"] != "selection"
    {
        return fail("rule requires a complete ID selection");
    }
    if body["profile"] != profile {
        return fail("rule selection must use the rule's profile");
    }
    let location = args.get("location").unwrap_or(&Value::Null);
    if operation["operation_kind"] != "render" && !location.is_null() {
        return fail("analysis and external rules do not take a local generated destination");
    }
    if operation["operation_kind"] == "render"
        && !store
            .db
            .prepare("SELECT 1 FROM generated_locations WHERE profile=? AND location=?")?
            .exists(params![profile, crate::sql::scalar(location)?])?
    {
        return fail("rules require a bound generated destination");
    }
    let assumptions = args
        .get("estimates")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!({}));
    estimate_options(&assumptions)?;
    let derived = args
        .get("allow_derived")
        .unwrap_or(&Value::Bool(false))
        .as_bool()
        .ok_or_else(|| Error("allow_derived must be boolean".into()))?;
    let required = args.get("required").is_some_and(python_truth);
    let mut value = vec![
        json!(recipe),
        location.clone(),
        body.clone(),
        assumptions.clone(),
        json!(required),
    ];
    if derived {
        value.push(json!(true));
    }
    let digest = sha256(encode(&json!(value)));
    let id = store.transaction(|db| {
        if let Some(id) = db.query_row("SELECT id FROM processing_rules WHERE profile=? AND name=? AND digest=?",params![profile,rule_name,digest],|r|r.get::<_,String>(0)).optional()? { return Ok(id); }
        let revision: i64 = db.query_row("SELECT coalesce(max(revision),0)+1 FROM processing_rules WHERE profile=? AND name=?",params![profile,rule_name],|r|r.get(0))?;
        let id = Uuid::new_v4().to_string();
        db.execute("UPDATE processing_rules SET enabled=0 WHERE profile=? AND name=?",params![profile,rule_name])?;
        db.execute("INSERT INTO processing_rules(id,profile,name,revision,recipe_id,location,selection,estimate_options,required,digest,query_id,allow_derived) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",params![id,profile,rule_name,revision,recipe,crate::sql::scalar(location)?,encode(&body),encode(&assumptions),required,digest,body["query_id"].as_str(),derived])?;
        Ok(id)
    })?;
    rule_get(store, profile, &id)
}
pub fn layout_get(store: &Store, id: &str) -> Result<Value> {
    name(id)?;
    let row = one(
        &store.db,
        "SELECT value FROM meta WHERE key=?",
        &[format!("layout:{id}").into()],
        &format!("unknown layout: {id}"),
    )?;
    let definition: Value = catabolic_core::decode_json(row["value"].as_str().unwrap())?;
    layout::validate(&definition)?;
    Ok(json!({"name":id,"definition":definition}))
}
fn catalog(store: &Store, id: &str) -> Result<()> {
    if !store
        .db
        .prepare("SELECT 1 FROM catalogs WHERE id=?")?
        .exists([id])?
    {
        return fail("unknown catalog");
    }
    Ok(())
}
fn json_column(store: &Store, sql: &str, args: &[SqlValue]) -> Result<Value> {
    rows(&store.db, sql, args)?
        .first()
        .map(|r| catabolic_core::decode_json(r["definition"].as_str().unwrap()).map_err(Into::into))
        .unwrap_or(Ok(Value::Null))
}
pub fn projection_get(store: &Store, profile: &str, id: &str) -> Result<Value> {
    catalog(store, id)?;
    let owner = rows(
        &store.db,
        "SELECT value AS definition FROM meta WHERE key=?",
        &[format!("layout-catalog:{id}").into()],
    )?
    .first()
    .map(|r| catabolic_core::decode_json(r["definition"].as_str().unwrap()))
    .transpose()?
    .unwrap_or(Value::Null);
    let binding = rows(
        &store.db,
        "SELECT * FROM projection_bindings WHERE profile=? AND catalog=?",
        &[profile.to_owned().into(), id.to_owned().into()],
    )?;
    let legacy = binding.is_empty();
    let mut result = binding.into_iter().next().unwrap_or(
        json!({"profile":profile,"catalog":id,"query_id":null,"layout":owner["layout"]}),
    );
    result["legacy"] = json!(legacy);
    result["copy_policy"] = json_column(
        store,
        "SELECT definition FROM copy_policies WHERE catalog=?",
        &[id.to_owned().into()],
    )?;
    result["rendition_policy"] = json_column(
        store,
        "SELECT definition FROM rendition_policies WHERE profile=? AND catalog=?",
        &[profile.to_owned().into(), id.to_owned().into()],
    )?;
    result["applied_layout"] = owner["layout"].clone();
    result["refresh"] = json!(rows(
        &store.db,
        "SELECT * FROM catalog_refresh_settings WHERE profile=? AND catalog=?",
        &[profile.to_owned().into(), id.to_owned().into()]
    )?);
    Ok(result)
}
fn payload(value: &Value) -> Result<()> {
    if !value.is_object() || encode(value).len() > 256 * 1024 {
        return fail("payload must be a JSON object of at most 256 KiB");
    }
    Ok(())
}
pub fn copy_validate(store: &Store, id: &str, value: &Value) -> Result<()> {
    require_recovered(store)?;
    payload(value)?;
    if !allowed(value, &["require", "prefer", "tie_break", "available_only"]) {
        return fail("copy policy supports require, prefer, tie_break, available_only");
    }
    catalog(store, id)?;
    if !value["tie_break"].is_null() && value["tie_break"] != "file_id" {
        return fail("tie_break must be null or file_id");
    }
    if !value
        .get("available_only")
        .unwrap_or(&json!(false))
        .is_boolean()
    {
        return fail("available_only must be boolean");
    }
    let fields = [
        "size",
        "location",
        "path",
        "height",
        "width",
        "hdr",
        "duration",
        "language",
        "video_codec",
        "audio_codec",
    ];
    if !allowed(value.get("require").unwrap_or(&json!({})), &fields) {
        return fail("unsupported required field");
    }
    let prefs = value.get("prefer").cloned().unwrap_or(json!([]));
    let prefs = prefs
        .as_array()
        .filter(|a| a.len() <= 20)
        .ok_or_else(|| Error("prefer requires at most 20 rules".into()))?;
    for rule in prefs {
        if !allowed(rule, &["field", "order", "values"])
            || !fields.contains(&rule["field"].as_str().unwrap_or(""))
        {
            return fail("preference supports field, order or values");
        }
        if rule.get("order").is_some() == rule.get("values").is_some() {
            return fail("preference needs order or values");
        }
        if rule.get("order").is_some() && !matches!(rule["order"].as_str(), Some("asc" | "desc")) {
            return fail("order must be asc or desc");
        }
        if rule
            .get("values")
            .is_some_and(|v| v.as_array().is_none_or(|a| a.len() > 100))
        {
            return fail("values must be a bounded ordered list");
        }
    }
    Ok(())
}
fn publication_text(value: &Value, label: &str) -> Result<()> {
    let text = value
        .as_str()
        .filter(|t| !t.contains('\0') && !t.trim().is_empty())
        .ok_or_else(|| {
            Error(format!(
                "{label} must be nonempty text without NUL characters"
            ))
        })?;
    if text.len() > 255 {
        return Err(Error(format!("{label} exceeds 255 UTF-8 bytes")));
    }
    Ok(())
}
pub fn rendition_validate(store: &Store, profile: &str, id: &str, input: &Value) -> Result<Value> {
    require_recovered(store)?;
    catalog(store, id)?;
    if !allowed(
        input,
        &[
            "version",
            "purpose",
            "definition_id",
            "rule_id",
            "mode",
            "include_originals",
        ],
    ) {
        return fail("invalid rendition publication policy");
    }
    let mut value = json!({"version":1,"mode":"all","include_originals":false});
    value
        .as_object_mut()
        .unwrap()
        .extend(input.as_object().unwrap().clone());
    if value["version"].as_i64() != Some(1)
        || !matches!(value["mode"].as_str(), Some("all" | "preferred"))
        || !value["include_originals"].is_boolean()
    {
        return fail("invalid rendition policy version, mode or include_originals");
    }
    if !["purpose", "definition_id", "rule_id"]
        .iter()
        .any(|k| value.get(k).is_some())
    {
        return fail("choose a purpose, output definition or rule revision");
    }
    if value.get("purpose").is_some()
        && !matches!(
            value["purpose"].as_str(),
            Some("transcode" | "remux" | "preview" | "thumbnail" | "audio" | "subtitle" | "custom")
        )
    {
        return fail("unknown rendition purpose");
    }
    for key in ["definition_id", "rule_id"] {
        if value.get(key).is_some() {
            publication_text(&value[key], &format!("publication {key}"))?;
        }
    }
    if let Some(id) = value["definition_id"].as_str()
        && !store
            .db
            .prepare("SELECT 1 FROM output_definitions WHERE id=?")?
            .exists([id])?
    {
        return fail("unknown output definition");
    }
    if let Some(id) = value["rule_id"].as_str()
        && !store
            .db
            .prepare("SELECT 1 FROM processing_rules WHERE id=? AND profile=?")?
            .exists(params![id, profile])?
    {
        return fail("unknown rule revision in this profile");
    }
    Ok(value)
}
pub fn dispatch(store: &Store, profile: &str, action: &str, args: &Value) -> Result<Value> {
    match action {
        "estimate" => crate::estimates::estimate(
            &args["recipe"],
            &args["observation"],
            &args["probe"],
            &args["assumptions"],
        ),
        "estimate-total" => crate::estimates::total(&args["values"]),
        "estimate-calibration" => {
            crate::estimates::calibration(store, profile, &args["recipe"], &args["assumptions"])
        }
        "estimate-calibrated" => crate::estimates::calibrated(&args["value"], &args["report"]),
        "query-validate" => query_validate(store, profile, &args["value"]),
        "query-get" => query_get(store, profile, field(args, "id")?),
        "query-put" => query_put(store, profile, field(args, "name")?, &args["value"]),
        "query-list" => {
            let n = limit(args)?;
            bounded_page(
                page_rows(
                    &store.db,
                    "SELECT id,profile,name,revision,digest,created_at,CASE WHEN length(definition)<=8192 THEN definition END AS definition,length(definition)>8192 AS definition_omitted FROM saved_queries WHERE profile=? AND id>? ORDER BY id LIMIT ?",
                    &[
                        profile.to_owned().into(),
                        crate::sql::scalar(args.get("after").unwrap_or(&json!("")))?,
                        ((n + 1) as i64).into(),
                    ],
                )?,
                n,
                "queries",
                &["definition"],
                &[],
            )
        }
        "rule-get" => rule_get(store, profile, field(args, "id")?),
        "rule-put" => rule_put(store, profile, args),
        "rule-list" => {
            let n = limit(args)?;
            bounded_page(
                page_rows(
                    &store.db,
                    "SELECT * FROM processing_rules WHERE profile=? AND id>? AND (?=0 OR enabled=1) ORDER BY id LIMIT ?",
                    &[
                        profile.to_owned().into(),
                        args["after"].as_str().unwrap_or("").to_owned().into(),
                        i64::from(args.get("enabled").is_some_and(python_truth)).into(),
                        ((n + 1) as i64).into(),
                    ],
                )?,
                n,
                "rules",
                &["selection", "estimate_options"],
                &["enabled", "required"],
            )
        }
        "rule-enable" => {
            let id = field(args, "id")?;
            let rule = rule_get(store, profile, id)?;
            let enabled = args.get("enabled").is_some_and(python_truth);
            store.transaction(|db| {
                if enabled {
                    db.execute(
                        "UPDATE processing_rules SET enabled=0 WHERE profile=? AND name=?",
                        params![profile, rule["name"].as_str().unwrap()],
                    )?;
                }
                db.execute(
                    "UPDATE processing_rules SET enabled=? WHERE id=?",
                    params![enabled, id],
                )?;
                Ok(())
            })?;
            rule_get(store, profile, id)
        }
        "layout-get" => layout_get(store, field(args, "name")?),
        "layout-list" => {
            let mut result = vec![];
            for row in rows(
                &store.db,
                "SELECT key,value FROM meta WHERE key LIKE 'layout:%' ORDER BY key",
                &[],
            )? {
                result.push(json!({"name":&row["key"].as_str().unwrap()[7..],"definition":catabolic_core::decode_json(row["value"].as_str().unwrap())?}));
            }
            Ok(json!({"layouts":result}))
        }
        "layout-put" => {
            let id = field(args, "name")?;
            name(id)?;
            layout::validate(&args["value"])?;
            store.transaction(|db| { db.execute("INSERT INTO meta(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![format!("layout:{id}"),encode(&args["value"])])?; Ok(()) })?;
            layout_get(store, id)
        }
        "copy-get" => json_column(
            store,
            "SELECT definition FROM copy_policies WHERE catalog=?",
            &[field(args, "catalog")?.to_owned().into()],
        ),
        "copy-put" => {
            let id = field(args, "catalog")?;
            copy_validate(store, id, &args["value"])?;
            store.transaction(|db| { db.execute("INSERT INTO copy_policies VALUES (?,?) ON CONFLICT(catalog) DO UPDATE SET definition=excluded.definition",params![id,encode(&args["value"])])?; Ok(()) })?;
            Ok(json!({"catalog":id,"definition":args["value"]}))
        }
        "rendition-get" => {
            let id = field(args, "catalog")?;
            catalog(store, id)?;
            Ok(
                json!({"catalog":id,"profile":profile,"definition":json_column(store,"SELECT definition FROM rendition_policies WHERE profile=? AND catalog=?",&[profile.to_owned().into(),id.to_owned().into()])?}),
            )
        }
        "rendition-put" => {
            let id = field(args, "catalog")?;
            let value = rendition_validate(store, profile, id, &args["value"])?;
            let serialized = encode(&value);
            store.transaction(|db| { db.execute("INSERT INTO rendition_policies VALUES (?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET definition=excluded.definition,digest=excluded.digest",params![profile,id,serialized,sha256(&serialized)])?; Ok(()) })?;
            Ok(json!({"catalog":id,"profile":profile,"definition":value}))
        }
        "projection-get" => projection_get(store, profile, field(args, "catalog")?),
        "projection-put" => {
            require_recovered(store)?;
            let id = field(args, "catalog")?;
            projection_get(store, profile, id)?;
            let query = field(args, "query_id")?;
            if query_get(store, profile, query)?["definition"]["mode"] != "selection" {
                return fail("projection requires a complete ID selection");
            }
            let layout = field(args, "layout")?;
            layout_get(store, layout)?;
            // These calls remain inside the same outer transaction. Any later
            // validation failure rolls back earlier policy changes atomically.
            store.transaction(|db| {
                if let Some(copies)=args.get("copies").filter(|v|!v.is_null()) { dispatch(store,profile,"copy-put",&json!({"catalog":id,"value":copies}))?; }
                if let Some(renditions)=args.get("renditions").filter(|v|!v.is_null()) { dispatch(store,profile,"rendition-put",&json!({"catalog":id,"value":renditions}))?; }
                db.execute("INSERT INTO projection_bindings VALUES (?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET query_id=excluded.query_id,layout=excluded.layout",params![profile,id,query,layout])?;
                Ok(())
            })?;
            projection_get(store, profile, id)
        }
        _ => Err(Error(format!("unknown definitions action: {action}"))),
    }
}
