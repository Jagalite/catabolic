// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded read adapters shared by terminal and persistent public facades.
//! Mutation, admission and current-evidence decisions stay with their owners.
use crate::{Error, Result, Store, migration, query, sql};
use rusqlite::types::{Value as SqlValue, ValueRef};
use serde_json::{Value, json};
pub(crate) const RESULT_BUDGET: usize = 4 * 1024 * 1024;

fn raw_size(row: &rusqlite::Row<'_>, columns: usize) -> rusqlite::Result<usize> {
    let mut total = 0usize;
    for index in 0..columns {
        let size = match row.get_ref(index)? {
            ValueRef::Text(bytes) => bytes.len(),
            ValueRef::Blob(bytes) => bytes.len().saturating_mul(2),
            _ => 0,
        };
        total = total.saturating_add(size);
    }
    Ok(total)
}

/// Non-paged public shapes cannot advertise a continuation. Refuse an oversized
/// result instead of silently returning a complete-looking prefix. Inspect raw
/// SQLite text/blob lengths before copying or decoding a large record.
pub(crate) fn bounded_rows(store: &Store, sql: &str, arguments: &[SqlValue]) -> Result<Vec<Value>> {
    let _source_read = crate::catalog_source_succession::current::enter_read(store)?;
    let mut statement = store.db.prepare(sql)?;
    let names = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let mut cursor = statement.query(rusqlite::params_from_iter(arguments))?;
    let mut result = vec![];
    let mut used = 0usize;
    while let Some(row) = cursor.next()? {
        if raw_size(row, names.len())? > RESULT_BUDGET - used {
            return Err(Error("result exceeds the result budget".into()));
        }
        let mut value = serde_json::Map::new();
        for (index, name) in names.iter().enumerate() {
            value.insert(name.clone(), migration::value(row.get_ref(index)?));
        }
        let value = Value::Object(value);
        charge(&mut used, &value)?;
        result.push(value);
    }
    Ok(result)
}
fn charge(used: &mut usize, value: &Value) -> Result<()> {
    *used = used.saturating_add(catabolic_core::encode(value).len());
    if *used > RESULT_BUDGET {
        return Err(Error("result exceeds the result budget".into()));
    }
    Ok(())
}
fn limit(args: &Value) -> Result<usize> {
    args.get("limit")
        .map(Value::as_u64)
        .unwrap_or(Some(100))
        .filter(|n| (1..=1000).contains(n))
        .map(|n| n as usize)
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))
}
fn arg(args: &Value, key: &str) -> Result<SqlValue> {
    sql::scalar(&args[key])
}
fn after(args: &Value) -> Result<SqlValue> {
    sql::scalar(args.get("after").unwrap_or(&json!("")))
}
fn page(
    store: &Store,
    query: &str,
    args: &[SqlValue],
    limit: usize,
    key: &str,
    cursor: &str,
    decode: &[&str],
) -> Result<Value> {
    let mut stmt = store.db.prepare(query)?;
    let names = stmt
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let mut cursor_rows = stmt.query(rusqlite::params_from_iter(args))?;
    let mut values = vec![];
    let mut used = 0usize;
    let mut more = false;
    while let Some(row) = cursor_rows.next()? {
        if values.len() == limit || raw_size(row, names.len())? > RESULT_BUDGET - used {
            if values.is_empty() {
                return Err(Error("record exceeds the result budget".into()));
            }
            more = true;
            break;
        }
        let mut object = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            object.insert(name.clone(), migration::value(row.get_ref(i)?));
        }
        let mut value = Value::Object(object);
        let size = catabolic_core::encode(&value).len();
        if used + size > RESULT_BUDGET {
            if values.is_empty() {
                return Err(Error("record exceeds the result budget".into()));
            }
            more = true;
            break;
        }
        used += size;
        query::decode(&mut value, decode)?;
        values.push(value);
    }
    let next = if more {
        values.last().unwrap()[cursor].clone()
    } else {
        Value::Null
    };
    Ok(json!({key:values,"next_after":next}))
}
pub fn artifact(store: &Store, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    crate::mutation::profile(store, profile)?;
    let n = limit(args)?;
    match operation {
        "recipes" => page(
            store,
            "SELECT * FROM processing_recipes WHERE id>? ORDER BY id LIMIT ?",
            &[after(args)?, ((n + 1) as i64).into()],
            n,
            "recipes",
            "id",
            &[
                "definition",
                "binding",
                "validation",
                "publication_snapshot",
            ],
        ),
        "list" => page(
            store,
            "SELECT a.id,a.job_id,a.attempt,a.state,a.file_id,a.item_id,a.role,a.location,a.path,a.size,a.sha256,a.error,a.created_at,j.file_id AS input_file_id,j.recipe_id FROM processing_artifacts a JOIN processing_jobs j ON j.id=a.job_id WHERE a.profile=? AND a.id>? AND (? IS NULL OR a.state=?) AND (? IS NULL OR j.file_id=?) ORDER BY a.id LIMIT ?",
            &[
                profile.to_owned().into(),
                after(args)?,
                arg(args, "state")?,
                arg(args, "state")?,
                arg(args, "file_id")?,
                arg(args, "file_id")?,
                ((n + 1) as i64).into(),
            ],
            n,
            "artifacts",
            "id",
            &[],
        ),
        _ => Err(Error("unknown artifact read operation".into())),
    }
}
pub fn output(store: &Store, profile: &str, definitions: bool, args: &Value) -> Result<Value> {
    crate::mutation::profile(store, profile)?;
    let n = limit(args)?;
    if definitions {
        page(
            store,
            "SELECT * FROM output_definitions WHERE id>? ORDER BY id LIMIT ?",
            &[after(args)?, ((n + 1) as i64).into()],
            n,
            "definitions",
            "id",
            &["definition"],
        )
    } else {
        page(
            store,
            &format!(
                "SELECT * FROM ({}) WHERE profile=? AND id>? AND (? IS NULL OR source_file_id=?) ORDER BY id LIMIT ?",
                query::view_sql_for(store, "catalog_renditions")
            ),
            &[
                profile.to_owned().into(),
                after(args)?,
                arg(args, "source_file_id")?,
                arg(args, "source_file_id")?,
                ((n + 1) as i64).into(),
            ],
            n,
            "outputs",
            "id",
            &["metadata"],
        )
    }
}
pub fn processing(store: &Store, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    crate::mutation::profile(store, profile)?;
    match operation {
        "list" => {
            let n = limit(args)?;
            page(
                store,
                "SELECT id,profile,file_id,operation,state,attempts,error,created_at,finished_at,recipe_id,snapshot,options,CASE WHEN length(result)<=8192 THEN result END AS result,coalesce(length(result)>8192,0) AS result_omitted FROM processing_jobs WHERE profile=? AND id>? AND (? IS NULL OR state=?) AND (? IS NULL OR file_id=?) ORDER BY id LIMIT ?",
                &[
                    profile.to_owned().into(),
                    after(args)?,
                    arg(args, "state")?,
                    arg(args, "state")?,
                    arg(args, "file_id")?,
                    arg(args, "file_id")?,
                    ((n + 1) as i64).into(),
                ],
                n,
                "jobs",
                "id",
                &["snapshot", "options", "result"],
            )
        }
        "attempts" => {
            let id = args["id"]
                .as_str()
                .ok_or_else(|| Error("unknown job".into()))?;
            crate::media_mutation::get_job(store, profile, id)?;
            let n = limit(args)?;
            page(
                store,
                "SELECT job_id,attempt,state,error,retryable,retry_after,created_at,started_at,finished_at,CASE WHEN length(result)<=8192 THEN result END AS result,coalesce(length(result)>8192,0) AS result_omitted FROM processing_attempts WHERE job_id=? AND attempt>? ORDER BY attempt LIMIT ?",
                &[
                    id.to_owned().into(),
                    sql::scalar(args.get("after").unwrap_or(&json!(0)))?,
                    ((n + 1) as i64).into(),
                ],
                n,
                "attempts",
                "attempt",
                &[],
            )
        }
        "facts" => {
            let file = args["file_id"]
                .as_str()
                .ok_or_else(|| Error("file_id must be text".into()))?;
            let mut facts = vec![];
            let mut used = 0usize;
            for operation in ["sniff", "hash", "verify", "probe", "text", "decode"] {
                if let Some(fact) = fact(store, profile, file, operation)? {
                    charge(&mut used, &fact)?;
                    facts.push(fact);
                }
            }
            Ok(json!({"file_id":file,"facts":facts}))
        }
        "duplicates" => {
            let n = limit(args)?;
            let mut value = page(
                store,
                "SELECT digest,algorithm,size,count(*) AS occurrences FROM content_baselines WHERE profile=? AND digest>? GROUP BY digest,algorithm,size HAVING count(*)>1 ORDER BY digest LIMIT ?",
                &[
                    profile.to_owned().into(),
                    after(args)?,
                    ((n + 1) as i64).into(),
                ],
                n,
                "groups",
                "digest",
                &[],
            )?;
            value["evidence"] =
                json!("recorded full SHA-256 baselines; verify before relying on current equality");
            Ok(value)
        }
        "search" => {
            let n = limit(args)?;
            let text = args["text"].as_str().ok_or_else(|| {
                Error("search requires 1..20 words and at most 4096 bytes".into())
            })?;
            let terms = crate::media_mutation::words(text)
                .into_iter()
                .collect::<Vec<_>>();
            if terms.is_empty() || terms.len() > 20 || text.len() > 4096 {
                return Err(Error(
                    "search requires 1..20 words and at most 4096 bytes".into(),
                ));
            }
            let clauses = if terms.len() == 1 {
                "1".into()
            } else {
                vec![
                    "EXISTS (SELECT 1 FROM text_terms t WHERE t.segment_id=s.id AND t.term=?)";
                    terms.len() - 1
                ]
                .join(" AND ")
            };
            let mut params = vec![
                terms[0].clone().into(),
                sql::scalar(args.get("after").unwrap_or(&json!(0)))?,
                profile.to_owned().into(),
            ];
            params.extend(terms[1..].iter().cloned().map(SqlValue::Text));
            params.push(((n + 1) as i64).into());
            let mut value = page(
                store,
                &format!(
                    "SELECT s.* FROM text_terms seed JOIN text_segments s ON s.id=seed.segment_id WHERE seed.term=? AND seed.segment_id>? AND s.profile=? AND {clauses} ORDER BY seed.segment_id LIMIT ?"
                ),
                &params,
                n,
                "hits",
                "id",
                &["locator"],
            )?;
            for hit in value["hits"].as_array_mut().unwrap() {
                let file = hit["file_id"]
                    .as_str()
                    .ok_or_else(|| Error("invalid text file ID".into()))?;
                hit["current"] = json!(
                    fact(store, profile, file, "text")?.is_some_and(|fact| fact["current"] == true)
                );
                hit["text"] = json!(
                    hit["text"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(2000)
                        .collect::<String>()
                );
            }
            value["match"] = json!("all normalized words");
            Ok(value)
        }
        _ => Err(Error("unknown processing read operation".into())),
    }
}
pub(crate) fn fact(
    store: &Store,
    profile: &str,
    file: &str,
    operation: &str,
) -> Result<Option<Value>> {
    let mut rows = bounded_rows(
        store,
        &crate::catalog_source_succession::current::view(
            store.schema_version,
            "SELECT * FROM file_facts WHERE profile=? AND file_id=? AND operation=?",
        ),
        &[
            profile.to_owned().into(),
            file.to_owned().into(),
            operation.to_owned().into(),
        ],
    )?;
    let Some(mut row) = rows.pop() else {
        return Ok(None);
    };
    query::decode(&mut row, &["snapshot", "data"])?;
    let now = crate::curation::occurrence(store, profile, file)?;
    row["current"] = json!(
            row["status"] == "complete"
                && row["snapshot"]
                    .as_object()
                    .ok_or_else(|| Error("invalid fact snapshot".into()))?
                    .iter()
                    .filter(|(k, _)| !["ctime_ns", "source_policy", "volume_uuid"]
                        .contains(&k.as_str()))
                    .all(|(k, v)| now[k] == *v)
        );
    Ok(Some(row))
}
pub fn bindings(store: &Store, profile: &str, kind: &str) -> Result<Value> {
    crate::mutation::profile(store, profile)?;
    let mut rows = bounded_rows(
        store,
        "SELECT * FROM bindings WHERE profile=? AND kind=? ORDER BY owner",
        &[profile.to_owned().into(), kind.to_owned().into()],
    )?;
    let mut used = 0usize;
    for row in &mut rows {
        if kind == "source" {
            let policies = bounded_rows(
                store,
                "SELECT identity_policy FROM source_identity_policies WHERE profile=? AND location=?",
                &[profile.to_owned().into(), sql::scalar(&row["owner"])?],
            )?;
            row["identity_policy"] = policies
                .first()
                .map(|p| p["identity_policy"].clone())
                .unwrap_or(json!("strict"));
        }
        if kind == "output" {
            row["link_mode"] = crate::embedding::application(
                store,
                profile,
                "link_mode",
                &json!({"catalog":row["owner"]}),
            )?;
        }
        charge(&mut used, row)?;
    }
    Ok(json!({"bindings":rows}))
}
