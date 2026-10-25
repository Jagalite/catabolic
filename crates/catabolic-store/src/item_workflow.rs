// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Recorded-evidence entry completion and append-only optimistic worklogs.
use crate::{
    Error, Result, Store, definitions,
    migration::rows,
    query::{decode, view_sql_for},
    sql::scalar,
};
use catabolic_core::encode;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

fn fail<T>(message: &str) -> Result<T> {
    Err(Error(message.into()))
}
pub(crate) fn text<'a>(
    value: &'a Value,
    label: &str,
    maximum: usize,
    empty: bool,
) -> Result<&'a str> {
    let value = value
        .as_str()
        .filter(|v| {
            !v.contains('\0')
                && (empty
                    || !v
                        .trim_matches(|c: char| {
                            c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
                        })
                        .is_empty())
        })
        .ok_or_else(|| {
            Error(format!(
                "{label} must be {}text without NUL characters",
                if empty { "" } else { "nonempty " }
            ))
        })?;
    if value.len() > maximum {
        return Err(Error(format!("{label} exceeds {maximum} UTF-8 bytes")));
    }
    Ok(value)
}
fn item(store: &Store, id: &str) -> Result<()> {
    if !store
        .db
        .prepare("SELECT 1 FROM items WHERE id=?")?
        .exists([id])?
    {
        return fail("unknown item");
    }
    Ok(())
}
pub fn get(store: &Store, profile: &str, id: &str) -> Result<Value> {
    item(store, id)?;
    rows(
        &store.db,
        &format!(
            "SELECT * FROM ({}) WHERE item_id=? AND profile=?",
            view_sql_for(store, "catalog_item_workflow")
        ),
        &[id.to_owned().into(), profile.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown profile".into()))
}
pub(crate) fn revision(
    store: &Store,
    db: &Connection,
    id: &str,
    expected: &Value,
) -> Result<(String, i64)> {
    item(store, id)?;
    let (status, revision) = db
        .query_row(
            "SELECT status,revision FROM item_workflow WHERE item_id=?",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?
        .unwrap_or(("pending".into(), 0));
    if !expected.is_null() {
        if !expected.is_number() || !expected.to_string().bytes().all(|c| c.is_ascii_digit()) {
            return Err(Error(
                "expected revision must be a nonnegative integer".into(),
            ));
        }
        if expected != &json!(revision) {
            return Err(Error(format!(
                "entry changed: expected revision {expected}, current revision {revision}; read the worklog before retrying"
            )));
        }
    }
    Ok((status, revision))
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn append(
    db: &Connection,
    profile: &str,
    id: &str,
    kind: &str,
    body: &Value,
    actor: &Value,
    data: &Value,
    status: &str,
    revision: i64,
) -> Result<Value> {
    let body = text(body, "worklog text", 65536, true)?;
    let actor = text(actor, "actor", 255, false)?;
    let revision = revision
        .checked_add(1)
        .ok_or_else(|| Error("workflow revision overflow".into()))?;
    db.execute("INSERT INTO item_workflow(item_id,status,revision,updated_by) VALUES (?,?,?,?) ON CONFLICT(item_id) DO UPDATE SET status=excluded.status,revision=excluded.revision,updated_by=excluded.updated_by,updated_at=CURRENT_TIMESTAMP",params![id,status,revision,actor])?;
    db.execute("INSERT INTO item_worklog(item_id,profile,revision,kind,body,actor,data) VALUES (?,?,?,?,?,?,?)",params![id,profile,revision,kind,body,actor,encode(data)])?;
    Ok(json!({"event_id":db.last_insert_rowid(),"revision":revision}))
}
fn page(store: &Store, profile: &str, action: &str, args: &Value, id: &str) -> Result<Value> {
    item(store, id)?;
    let limit = args
        .get("limit")
        .unwrap_or(&json!(100))
        .as_u64()
        .filter(|n| (1..=1000).contains(n))
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))?;
    let (sql, parameters, key, cursor) = if action == "checks" {
        let after = args.get("after").cloned().unwrap_or(json!(""));
        (
            format!(
                "SELECT *,coalesce(satisfied,0) AS passed FROM ({}) WHERE item_id=? AND profile=? AND check_id>? ORDER BY check_id LIMIT ?",
                view_sql_for(store, "catalog_workflow_checks")
            ),
            vec![
                id.to_owned().into(),
                profile.to_owned().into(),
                scalar(&after)?,
                ((limit + 1) as i64).into(),
            ],
            "checks",
            "check_id",
        )
    } else {
        let after = args
            .get("after")
            .unwrap_or(&json!(0))
            .as_u64()
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| Error("after must be a nonnegative event ID".into()))?;
        (
            "SELECT * FROM item_worklog WHERE item_id=? AND id>? ORDER BY id LIMIT ?".into(),
            vec![
                id.to_owned().into(),
                (after as i64).into(),
                ((limit + 1) as i64).into(),
            ],
            "entries",
            "id",
        )
    };
    let mut statement = store.db.prepare(&sql)?;
    let names = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let mut cursor_rows = statement.query(rusqlite::params_from_iter(parameters))?;
    let mut values = vec![];
    let mut size = 0;
    let mut more = false;
    while let Some(row) = cursor_rows.next()? {
        let mut value = serde_json::Map::new();
        for (i, key) in names.iter().enumerate() {
            value.insert(key.clone(), crate::migration::value(row.get_ref(i)?));
        }
        let value = Value::Object(value);
        let bytes = encode(&value).len();
        if values.len() == limit as usize || size + bytes > 4 * 1024 * 1024 {
            if values.is_empty() {
                return fail("record exceeds the result budget");
            }
            more = true;
            break;
        }
        size += bytes;
        values.push(value);
    }
    let next = if more {
        values.last().unwrap()[cursor].clone()
    } else {
        Value::Null
    };
    if action == "log" {
        for value in &mut values {
            decode(value, &["data"])?;
        }
    }
    let mut result = json!({key:values,"next_after":next});
    if action == "checks" {
        result["evidence"] = json!("recorded state; no live filesystem or media verification");
    }
    Ok(result)
}
pub fn dispatch(store: &Store, profile: &str, action: &str, args: &Value) -> Result<Value> {
    let actor = args.get("actor").cloned().unwrap_or(json!("user"));
    let note = args.get("note").cloned().unwrap_or(json!(""));
    let expected = &args["expected_revision"];
    let id = args["item_id"].as_str().unwrap_or("");
    match action {
        "get" => get(store, profile, id),
        "checks" | "log" => page(store, profile, action, args, id),
        "note" => {
            let kind = match args.get("kind") {
                None => "note",
                Some(v) => v.as_str().unwrap_or(""),
            };
            if !["note", "decision", "progress"].contains(&kind) {
                return fail("note kind must be note, decision, or progress");
            }
            text(&args["body"], "worklog text", 65536, false)?;
            let mut result = store.transaction(|db| {
                let (status, revision) = revision(store, db, id, expected)?;
                append(
                    db,
                    profile,
                    id,
                    kind,
                    &args["body"],
                    &actor,
                    &json!({}),
                    &status,
                    revision,
                )
            })?;
            result["workflow"] = get(store, profile, id)?;
            Ok(result)
        }
        "status" => {
            let status = args["status"].as_str().unwrap_or("");
            if !["pending", "in_progress", "complete", "deferred", "ignored"].contains(&status) {
                return fail(
                    "invalid entry status; needs_attention is computed from completion checks",
                );
            }
            if status == "complete" {
                definitions::require_recovered(store)?;
            }
            let mut result = store.transaction(|db| {
                let (before,revision)=revision(store,db,id,expected)?;
                if status == "complete" {
                    let blockers=rows(db,&format!("SELECT check_id,label,state FROM ({}) WHERE item_id=? AND profile=? AND coalesce(satisfied,0)=0 ORDER BY check_id LIMIT 11",view_sql_for(store, "catalog_workflow_checks")),&[id.to_owned().into(),profile.to_owned().into()])?;
                    if !blockers.is_empty() { return Err(Error(format!("cannot mark entry complete: {}{}",blockers.iter().take(10).map(|r|format!("{} ({}; missing or stale evidence)",r["label"].as_str().unwrap_or(""),r["state"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("; "),if blockers.len()>10 {"; more blockers exist"} else {""}))); }
                }
                append(db,profile,id,"status",&note,&actor,&json!({"before":before,"after":status}),status,revision)
            })?;
            result["workflow"] = get(store, profile, id)?;
            Ok(result)
        }
        "require" => {
            let kind = args["kind"].as_str().unwrap_or("");
            if !["review", "file", "job", "artifact", "proposal"].contains(&kind) {
                return fail("unknown requirement kind");
            }
            let label = text(&args["label"], "requirement label", 255, false)?;
            let target = &args["target"];
            if (kind == "review") != target.is_null() {
                return fail(
                    "review requirements have no target; other requirements need a target ID",
                );
            }
            let mut result=store.transaction(|db| {
                let (status,revision)=revision(store,db,id,expected)?;
                if db.query_row("SELECT count(*) FROM item_requirements WHERE item_id=?",[id],|r|r.get::<_,i64>(0))? >=1000 { return fail("entry exceeds 1000 requirements"); }
                let field=if kind != "review" {
                    let (table,field)=match kind { "file"=>("files","file_id"),"job"=>("processing_jobs","job_id"),"artifact"=>("processing_artifacts","artifact_id"),_=>("proposals","proposal_id") };
                    let row=rows(db,&format!("SELECT * FROM {table} WHERE id=?"),&[scalar(target)?])?.into_iter().next().filter(|r|kind=="file" || r["profile"]==profile).ok_or_else(|| Error("unknown requirement target in this profile".into()))?;
                    let file=if kind=="file" { &row["id"] } else { &row["file_id"] };
                    let belongs=if kind=="artifact" { row["item_id"]==id } else { db.prepare("SELECT 1 FROM item_files WHERE item_id=? AND file_id=?")?.exists(params![id,scalar(file)?])? };
                    if !belongs { return fail("requirement target does not belong to this entry"); } Some(field)
                } else { None };
                let requirement=uuid::Uuid::new_v4().to_string();
                if let Some(field)=field { db.execute(&format!("INSERT INTO item_requirements(id,item_id,profile,kind,label,state,{field}) VALUES (?,?,?,?,?,'pending',?)"),params![requirement,id,profile,kind,label,scalar(target)?])?; }
                else { db.execute("INSERT INTO item_requirements(id,item_id,profile,kind,label,state) VALUES (?,?,?,?,?,'pending')",params![requirement,id,profile,kind,label])?; }
                let mut result=append(db,profile,id,"requirement",&note,&actor,&json!({"action":"add","id":requirement,"kind":kind,"label":label,"target":target}),&status,revision)?;
                result["requirement_id"]=json!(requirement); Ok(result)
            })?;
            result["workflow"] = get(store, profile, id)?;
            Ok(result)
        }
        "resolve" => {
            let state = args["state"].as_str().unwrap_or("");
            if !["pending", "complete", "waived"].contains(&state) {
                return fail("requirement state must be pending, complete, or waived");
            }
            text(&note, "resolution note", 65536, false)?;
            let requirement = args["requirement_id"].as_str().unwrap_or("");
            let (mut result,item)=store.transaction(|db| {
                let found=rows(db,"SELECT * FROM item_requirements WHERE id=?",&[requirement.to_owned().into()])?.into_iter().next();
                let (row,table)=if let Some(row)=found { (row,"item_requirements") } else { (rows(db,"SELECT *,'rule' AS kind FROM rule_requirements WHERE id=?",&[requirement.to_owned().into()])?.into_iter().next().ok_or_else(|| Error("unknown requirement".into()))?,"rule_requirements") };
                if row["kind"]!="review" && state=="complete" { return fail("file/job/artifact/proposal requirements are evaluated from evidence; they cannot be manually completed"); }
                let item=row["item_id"].as_str().unwrap(); let (status,revision)=revision(store,db,item,expected)?;
                db.execute(&format!("UPDATE {table} SET state=? WHERE id=?"),params![state,requirement])?;
                let result=append(db,profile,item,"requirement",&note,&actor,&json!({"action":"resolve","id":requirement,"before":row["state"],"after":state}),&status,revision)?;
                Ok((result,item.to_owned()))
            })?;
            result["workflow"] = get(store, profile, &item)?;
            Ok(result)
        }
        _ => fail("unknown item workflow operation"),
    }
}
