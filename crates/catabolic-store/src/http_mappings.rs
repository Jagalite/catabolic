// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Complete saved-query results mapped to immutable, approved HTTP requests.
use crate::{Error, Result, Store, http_operations, selection::Evaluation};
use catabolic_core::{encode, sha256};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn fail<T>(code: &str) -> Result<T> {
    Err(Error(format!("mapping_{code}")))
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| Error("invalid_configuration".into()))
}
fn text(value: &str) -> Result<()> {
    if value.is_empty() || value.chars().count() > 255 || value.chars().any(|c| c < ' ') {
        return Err(Error("invalid_configuration".into()));
    }
    Ok(())
}
pub fn expression(spec: &Value, row: &Value) -> Result<Value> {
    let Some(spec) = spec.as_object() else {
        return fail("invalid_expression");
    };
    if spec
        .keys()
        .any(|k| !["column", "constant", "lookup", "json"].contains(&k.as_str()))
    {
        return fail("invalid_expression");
    }
    if spec.contains_key("column") == spec.contains_key("constant") {
        return fail("expression_requires_column_or_constant");
    }
    let mut value = if let Some(column) = spec.get("column") {
        row.get(
            column
                .as_str()
                .ok_or_else(|| Error("mapping_missing_column".into()))?,
        )
        .ok_or_else(|| Error("mapping_missing_column".into()))?
        .clone()
    } else {
        spec["constant"].clone()
    };
    if let Some(parse) = spec.get("json") {
        if parse != true || !value.is_string() {
            return fail("invalid_json_expression");
        }
        value = catabolic_core::decode_json(value.as_str().unwrap())
            .map_err(|_| Error("mapping_invalid_json_value".into()))?;
    }
    if let Some(lookup) = spec.get("lookup") {
        value = lookup
            .as_object()
            .and_then(|m| value.as_str().and_then(|v| m.get(v)))
            .ok_or_else(|| Error("mapping_unmapped_lookup_value".into()))?
            .clone();
    }
    Ok(value)
}
fn mapped(spec: &Value, row: &Value, depth: usize) -> Result<Value> {
    if depth > 16 {
        return fail("body_depth");
    }
    if let Some(object) = spec.as_object() {
        if object.contains_key("column") || object.contains_key("constant") {
            return expression(spec, row);
        }
        return Ok(Value::Object(
            object
                .iter()
                .map(|(k, v)| Ok((k.clone(), mapped(v, row, depth + 1)?)))
                .collect::<Result<_>>()?,
        ));
    }
    if let Some(values) = spec.as_array() {
        return Ok(Value::Array(
            values
                .iter()
                .map(|v| mapped(v, row, depth + 1))
                .collect::<Result<_>>()?,
        ));
    }
    fail("body_requires_expressions")
}
fn percent(value: &str, plus: bool) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(byte as char);
        } else if plus && byte == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
pub fn request_for(operation: &Value, definition: &Value, row: &Value) -> Result<Value> {
    let mut parameters = BTreeMap::new();
    if let Some(groups) = definition["parameters"].as_object() {
        for (location, values) in groups {
            for (name, spec) in values
                .as_object()
                .ok_or_else(|| Error("mapping_invalid_parameters".into()))?
            {
                parameters.insert(format!("{location}:{name}"), expression(spec, row)?);
            }
        }
    }
    let approved = operation["parameters"]
        .as_object()
        .ok_or_else(|| Error("mapping_parameter_mismatch".into()))?;
    if parameters.keys().any(|k| !approved.contains_key(k))
        || approved
            .iter()
            .any(|(k, v)| http_operations::truthy(&v["required"]) && !parameters.contains_key(k))
    {
        return fail("parameter_mismatch");
    }
    let mut path = required(operation, "path")?.to_owned();
    let mut query = Vec::new();
    for (key, value) in parameters {
        if !http_operations::validator(&approved[&key]["schema"])?.is_valid(&value) {
            return fail("parameter_schema_mismatch");
        }
        let value = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| encode(&value));
        if let Some(name) = key.strip_prefix("path:") {
            if ["", ".", ".."].contains(&value.as_str())
                || value.contains(['/', '\\', '%'])
                || value.chars().any(|c| c < ' ' || c == '\u{7f}')
            {
                return fail("invalid_path_value");
            }
            path = path.replace(&format!("{{{name}}}"), &percent(&value, false));
        } else {
            query.push((key[6..].to_owned(), value));
        }
    }
    let mut url = format!("{}{path}", required(operation, "base_url")?);
    query.sort();
    if !query.is_empty() {
        url.push('?');
        url.push_str(
            &query
                .iter()
                .map(|(k, v)| format!("{}={}", percent(k, true), percent(v, true)))
                .collect::<Vec<_>>()
                .join("&"),
        );
    }
    if url.chars().count() > 4096 {
        return fail("url_too_large");
    }
    let body = definition
        .get("body")
        .map(|spec| mapped(spec, row, 0))
        .transpose()?
        .unwrap_or(Value::Null);
    if encode(&body).len() > 65536 {
        return fail("body_too_large");
    }
    if definition.get("body").is_some() {
        if operation["body_schema"].is_null()
            || !http_operations::validator(&operation["body_schema"])?.is_valid(&body)
        {
            return fail("body_schema_mismatch");
        }
    } else if http_operations::truthy(&operation["body_required"]) {
        return fail("body_required");
    }
    Ok(
        json!({"url":url,"method":operation["method"],"body":body,"has_body":definition.get("body").is_some()}),
    )
}
fn complete_rows(value: &Value, query: &Value, definition: &Value) -> Result<Vec<Value>> {
    if value
        .get("errors")
        .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
        || value["complete"] == false
        || value["truncated"] == true
    {
        return fail("incomplete_query");
    }
    if query["mode"] == "rows" {
        if value["complete"] != true || value["truncated"] != false {
            return fail("incomplete_query");
        }
        let columns = value["columns"]
            .as_array()
            .ok_or_else(|| Error("mapping_incomplete_query".into()))?;
        let names = columns
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| Error("mapping_incomplete_query".into()))
            })
            .collect::<Result<Vec<_>>>()?;
        if names.iter().collect::<BTreeSet<_>>().len() != names.len() {
            return fail("duplicate_columns");
        }
        return value["rows"]
            .as_array()
            .ok_or_else(|| Error("mapping_incomplete_query".into()))?
            .iter()
            .map(|row| {
                let row = row
                    .as_array()
                    .filter(|r| r.len() == names.len())
                    .ok_or_else(|| Error("mapping_incomplete_query".into()))?;
                Ok(Value::Object(
                    names
                        .iter()
                        .zip(row)
                        .map(|(k, v)| ((*k).into(), v.clone()))
                        .collect(),
                ))
            })
            .collect();
    }
    let mut node = &value["data"];
    for field in definition["rows_path"]
        .as_array()
        .ok_or_else(|| Error("mapping_invalid_connection_path".into()))?
    {
        node = node
            .get(
                field
                    .as_str()
                    .ok_or_else(|| Error("mapping_invalid_connection_path".into()))?,
            )
            .ok_or_else(|| Error("mapping_invalid_connection_path".into()))?;
    }
    if !node["nodes"].is_array() || node["pageInfo"]["hasNextPage"] != false {
        return fail("incomplete_connection");
    }
    fn check(value: &Value) -> Result<()> {
        if let Some(object) = value.as_object() {
            if object.contains_key("nodes") && value["pageInfo"]["hasNextPage"] != false {
                return fail("incomplete_nested_connection");
            }
            for value in object.values() {
                check(value)?;
            }
        } else if let Some(values) = value.as_array() {
            for value in values {
                check(value)?;
            }
        }
        Ok(())
    }
    check(node)?;
    let rows = node["nodes"].as_array().unwrap();
    if rows.iter().any(|r| !r.is_object()) {
        return fail("invalid_document_rows");
    }
    Ok(rows.clone())
}
pub fn prepare(evaluation: &mut Evaluation, definition: &Value) -> Result<Value> {
    let Some(fields) = definition.as_object() else {
        return fail("invalid_http_definition");
    };
    if definition["version"].as_i64() != Some(3)
        || fields.keys().any(|k| {
            ![
                "version",
                "id",
                "query",
                "operation",
                "key",
                "parameters",
                "body",
                "rows_path",
            ]
            .contains(&k.as_str())
        })
        || !["version", "id", "query", "operation", "key"]
            .iter()
            .all(|k| fields.contains_key(*k))
    {
        return fail("invalid_http_definition");
    }
    for key in ["id", "query", "operation"] {
        text(required(definition, key)?)?;
    }
    let profile = evaluation.profile.clone();
    let operation = http_operations::get(
        evaluation.store(),
        &profile,
        required(definition, "operation")?,
    )?;
    if operation["enabled"] != true {
        return fail("operation_disabled");
    }
    let query = evaluation.get(required(definition, "query")?)?["definition"].clone();
    if query["mode"] != "rows" && query["mode"] != "document" {
        return fail("http_requires_rows_or_document");
    }
    if query["mode"] == "document"
        && !definition["rows_path"].as_array().is_some_and(|a| {
            (1..=16).contains(&a.len())
                && a.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
        })
    {
        return fail("document_requires_connection_path");
    }
    if query["mode"] == "rows" && definition.get("rows_path").is_some_and(|v| !v.is_null()) {
        return fail("rows_path_requires_document");
    }
    if let Some(parameters) = definition.get("parameters")
        && !parameters.as_object().is_some_and(|o| {
            o.iter()
                .all(|(k, v)| ["path", "query"].contains(&k.as_str()) && v.is_object())
        })
    {
        return fail("invalid_parameters");
    }
    let old = evaluation
        .store()
        .db
        .query_row(
            "SELECT digest FROM http_mapping_definitions WHERE profile=? AND id=?",
            params![profile, required(definition, "id")?],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    if old.is_some_and(|digest| digest != sha256(encode(definition))) {
        return fail("definition_changed_use_new_mapping_id");
    }
    let evaluated = evaluation.run(required(definition, "query")?, 1000)?;
    let rows = complete_rows(&evaluated, &query, definition)?;
    if rows.len() > 1000 {
        return fail("too_many_rows");
    }
    let mut grouped = BTreeMap::new();
    for row in rows {
        let key = expression(&definition["key"], &row)?;
        let key = key
            .as_str()
            .ok_or_else(|| Error("invalid_configuration".into()))?;
        text(key)?;
        let request = request_for(&operation["definition"], definition, &row)?;
        if grouped.get(key).is_some_and(|old| old != &request) {
            return fail("conflicting_key_rows");
        }
        grouped.insert(key.to_owned(), request);
    }
    if grouped.len() > 100 {
        return fail("too_many_targets");
    }
    let mut planned = Vec::new();
    for (key, request) in grouped {
        let digest = sha256(encode(&json!([operation["digest"], request])));
        let previous=evaluation.store().db.query_row("SELECT key,request_digest,event_id FROM http_mapping_ledger WHERE profile=? AND mapping_id=? AND key=?",params![profile,required(definition,"id")?,key],|r|Ok(json!({"key":r.get::<_,String>(0)?,"request_digest":r.get::<_,String>(1)?,"event_id":r.get::<_,String>(2)?}))).optional()?.unwrap_or(Value::Null);
        let changed = previous.is_null() || previous["request_digest"] != digest;
        planned.push(json!({"key":key,"request":request,"request_digest":digest,"previous":previous,"changed":changed}));
    }
    let mut identity = json!({"database_id":evaluation.store().database_id,"profile":profile,"definition":definition,"operation_digest":operation["digest"],"rows":planned});
    if encode(&identity).len() > 4 * 1024 * 1024 {
        return fail("plan_too_large");
    }
    let plan_id = sha256(encode(&identity));
    let out = identity.as_object_mut().unwrap();
    out.insert("plan_id".into(), json!(plan_id));
    out.insert("mapping".into(), definition["id"].clone());
    out.insert("complete".into(), json!(true));
    out.insert("applied".into(), json!(false));
    out.insert("queued".into(), json!(0));
    out.insert("delivery_acknowledged".into(), json!(false));
    Ok(identity)
}
pub fn enqueue(
    store: &Store,
    profile: &str,
    plan: &Value,
    expected: &str,
    max_changes: usize,
) -> Result<Value> {
    if plan["plan_id"] != expected {
        return fail("stale_plan_preview_again");
    }
    let changed = plan["rows"]
        .as_array()
        .ok_or_else(|| Error("mapping_invalid_plan".into()))?
        .iter()
        .filter(|r| r["changed"] == true)
        .collect::<Vec<_>>();
    if changed.len() > max_changes {
        return fail("change_budget_exceeded");
    }
    let definition = &plan["definition"];
    store.transaction(|db|{
        db.execute("INSERT INTO http_mapping_definitions VALUES (?,?,?,?) ON CONFLICT DO NOTHING",params![profile,required(definition,"id")?,encode(definition),sha256(encode(definition))])?;
        for row in &changed{
            if !row["previous"].is_null(){let previous=required(&row["previous"],"event_id")?;
                if db.query_row("SELECT EXISTS(SELECT 1 FROM notification_deliveries WHERE event_id=? AND state='leased' AND lease_until>?)",params![previous,crate::notifications::now()],|r|r.get::<_,bool>(0))?{return fail("delivery_in_progress");}
                db.execute("UPDATE notification_deliveries SET state='cancelled',lease_token=NULL,lease_until=NULL WHERE event_id=? AND state!='complete'",[previous])?;
            }
            let id=format!("mapping:{}",uuid::Uuid::new_v4());
            db.execute("INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,'mapping_http','info',?)",params![id,profile,required(definition,"id")?])?;
            db.execute("INSERT INTO http_mapping_deliveries(event_id,operation_id,mapping_id,key,plan_id,request) VALUES (?,?,?,?,?,?)",params![id,required(definition,"operation")?,required(definition,"id")?,required(row,"key")?,required(plan,"plan_id")?,encode(&row["request"])])?;
            db.execute("INSERT INTO notification_deliveries(event_id,profile,destination_id) VALUES (?,?,?)",params![id,profile,format!("operation:{}",required(definition,"operation")?)])?;
            db.execute("INSERT INTO http_mapping_ledger VALUES (?,?,?,?,?) ON CONFLICT(profile,mapping_id,key) DO UPDATE SET request_digest=excluded.request_digest,event_id=excluded.event_id",params![profile,required(definition,"id")?,required(row,"key")?,required(row,"request_digest")?,id])?;
        }Ok(())
    })?;
    let mut result = plan.clone();
    result["applied"] = json!(true);
    result["queued"] = json!(changed.len());
    Ok(result)
}
pub fn execute(store: Store, profile: &str, operation: &str, request: &Value) -> Result<Value> {
    if operation != "run" {
        return Err(Error("unknown_http_mapping_operation".into()));
    }
    let apply = request["apply"] == true;
    let expected = request["expected_plan"].as_str();
    if apply && expected.is_none() {
        return fail("apply_requires_expected_plan");
    }
    let mut evaluation = Evaluation::new(store, profile, 60000, 100000, false)?;
    let plan = prepare(&mut evaluation, &request["definition"])?;
    if apply {
        enqueue(
            evaluation.store(),
            profile,
            &plan,
            expected.unwrap(),
            request
                .get("max_changes")
                .map(|v| {
                    v.as_u64()
                        .ok_or_else(|| Error("invalid_configuration".into()))
                })
                .transpose()?
                .unwrap_or(100) as usize,
        )
    } else {
        Ok(plan)
    }
}
