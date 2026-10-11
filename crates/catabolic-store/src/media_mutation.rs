// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Durable media-analysis ownership. Admission, claims, attempts, facts and
//! component indexing share the catalog transaction and Python schema bytes.
use crate::{Error, Result, Store, components, migration::rows};
use catabolic_core::{
    encode,
    execution::{self, Effect, Input, Lease, Phase, Proof, Rejection, State},
    sha256,
};
use rusqlite::{Connection, OptionalExtension, params, types::Value as SqlValue};
use serde_json::{Value, json};
use std::fs::File;

pub const LEASE_SECONDS: f64 = 60.0;

#[derive(Clone, Debug)]
pub struct Claim {
    pub job_id: String,
    pub generation: i64,
    pub token: String,
    pub worker_pid: u32,
    pub attempt: i64,
    pub lease_until: f64,
    pub snapshot: String,
    pub options: String,
}
fn claim_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Claim> {
    Ok(Claim {
        job_id: row.get(0)?,
        generation: row.get(1)?,
        token: row.get(2)?,
        worker_pid: row.get(3)?,
        attempt: row.get(4)?,
        lease_until: row.get(5)?,
        snapshot: row.get(6)?,
        options: row.get(7)?,
    })
}
pub fn claim_live(claim: &Claim, now: f64) -> bool {
    if !now.is_finite()
        || claim.lease_until <= now
        || claim.worker_pid == 0
        || claim.worker_pid > i32::MAX as u32
    {
        return false;
    }
    // SAFETY: signal zero only checks process existence and does not signal it.
    unsafe {
        libc::kill(claim.worker_pid as i32, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}
fn validate_now(now: f64) -> Result<()> {
    if !now.is_finite() || now < 0.0 {
        return Err(Error("invalid execution time".into()));
    }
    Ok(())
}
fn proof(claim: &Claim) -> Proof {
    Proof {
        generation: claim.generation,
        attempt: claim.attempt,
        token_digest: sha256(&claim.token),
        snapshot_digest: sha256(&claim.snapshot),
        options_digest: sha256(&claim.options),
    }
}
fn observation(db: &Connection, id: &str, now: f64) -> Result<State> {
    let (phase, attempts, snapshot, options): (String, i64, String, String) = db.query_row(
        "SELECT state,attempts,snapshot,options FROM processing_jobs WHERE id=?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let claim = db.query_row("SELECT job_id,generation,token,worker_pid,attempt,lease_until,snapshot,options FROM execution_claims WHERE job_id=?", [id], claim_row).optional()?;
    Ok(State {
        phase: Phase::parse(&phase).ok_or_else(|| Error("invalid processing state".into()))?,
        attempts,
        snapshot_digest: sha256(snapshot),
        options_digest: sha256(options),
        claim: claim.map(|c| Lease {
            proof: proof(&c),
            until: c.lease_until,
            worker_alive: claim_live(&c, now),
        }),
    })
}
fn rejected(error: Rejection) -> Error {
    Error(
        match error {
            Rejection::InvalidTime => "invalid execution time",
            Rejection::LiveOwner => "job is already owned by a live worker",
            Rejection::NotRunnable => "only queued or abandoned jobs can be started",
            Rejection::StaleClaim => "execution claim expired, cancelled or replaced",
            Rejection::PublicationPending => {
                "recover pending artifacts before retrying or cancelling this job"
            }
            Rejection::NotRetryable => {
                "retry requires a failed, timed-out or cancelled job; changed files need a new job"
            }
            Rejection::InvalidOutcome => "invalid processing result state",
            Rejection::Overflow => "execution generation or attempt overflow",
        }
        .into(),
    )
}
fn decide(db: &Connection, id: &str, now: f64, input: Input) -> Result<Effect> {
    execution::reduce(&observation(db, id, now)?, &input).map_err(rejected)
}
fn fence_db(db: &Connection, claim: &Claim, now: f64) -> Result<()> {
    match decide(
        db,
        &claim.job_id,
        now,
        Input::Fence {
            now,
            proof: proof(claim),
        },
    )? {
        Effect::Fenced => Ok(()),
        _ => Err(Error("invalid execution fence effect".into())),
    }
}
/// Called only inside the transaction that also installs the job/attempt state.
/// The reducer owns generation/attempt allocation, including recovery semantics.
pub(crate) fn acquire(
    db: &Connection,
    id: &str,
    now: f64,
    recover_attempt: Option<i64>,
) -> Result<Claim> {
    let input = match recover_attempt {
        Some(attempt) => Input::Recover {
            now,
            duration: LEASE_SECONDS,
            attempt,
        },
        None => Input::Start {
            now,
            duration: LEASE_SECONDS,
        },
    };
    let Effect::Acquire {
        generation,
        attempt,
        until,
    } = decide(db, id, now, input)?
    else {
        return Err(Error("invalid execution acquire effect".into()));
    };
    let (snapshot, options): (String, String) = db.query_row(
        "SELECT snapshot,options FROM processing_jobs WHERE id=?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let claim = Claim {
        job_id: id.into(),
        generation,
        attempt,
        lease_until: until,
        token: format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ),
        worker_pid: std::process::id(),
        snapshot,
        options,
    };
    db.execute("INSERT OR REPLACE INTO execution_claims(job_id,generation,token,worker_pid,attempt,lease_until,snapshot,options) VALUES (?,?,?,?,?,?,?,?)", params![id,claim.generation,claim.token,claim.worker_pid,claim.attempt,claim.lease_until,claim.snapshot,claim.options])?;
    Ok(claim)
}

pub fn snapshot(store: &Store, profile: &str, file: &str) -> Result<Value> {
    components::occurrence(&store.db, profile, file)
}
pub fn validated_source<T>(
    snapshot: &Value,
    operation: impl FnOnce(&mut File) -> Result<T>,
) -> Result<T> {
    crate::source::with_validated_source(snapshot, operation)
}

pub fn enqueue(
    store: &Store,
    profile: &str,
    file: &str,
    operation: &str,
    config: &Value,
    refresh: bool,
    recipe: Option<&str>,
) -> Result<Value> {
    if !["sniff", "hash", "verify", "probe", "decode", "text"].contains(&operation) {
        return Err(Error("native analysis operation is not implemented".into()));
    }
    let mut observed = snapshot(store, profile, file)?;
    let revision = crate::source::validate(&observed)?;
    observed["ctime_ns"] = revision["ctime_ns"].clone();
    let key = sha256(encode(&json!([observed, operation, config])));
    store.transaction(|db| {
        if let Some(recipe) = recipe {
            let found: Option<(String,String,String)> = db.query_row("SELECT operation_kind,preset,definition FROM processing_recipes WHERE id=?", [recipe], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let (kind,preset,definition) = found.ok_or_else(|| Error("unknown immutable operation revision".into()))?;
            let definition: Value = catabolic_core::decode_json(&definition)?;
            let mut expected = json!({"timeout":if ["hash","verify","decode"].contains(&operation) {120} else {20},"max_bytes":2*1024*1024,"analysis_bytes":1024*1024,"analysis_us":1_000_000});
            if operation == "probe" { expected["required_streams"] = json!([]); }
            if operation == "text" {
                match definition["options"]["backend"].as_str() {
                    Some("pdf") => { expected["max_pages"] = json!(100); }
                    Some("ocr") => { expected["language"] = json!("eng"); }
                    _ => {}
                }
            }
            for (key,value) in definition["options"].as_object().ok_or_else(|| Error("invalid immutable operation definition".into()))? { expected[key] = value.clone(); }
            if let Some(required) = expected.get_mut("required_streams").and_then(Value::as_array_mut) { required.sort_by_key(|v| v.as_str().unwrap_or("").to_owned()); required.dedup(); }
            let mut actual = config.clone(); if let Some(object) = actual.as_object_mut() { object.remove("extractor"); object.remove("operation_id"); }
            if kind != "analysis" || preset != operation || config["operation_id"] != recipe || expected != actual { return Err(Error("processing operation does not match its immutable definition".into())); }
        }
        let old: Option<(String,String,bool)> = db.query_row("SELECT j.id,j.state,EXISTS(SELECT 1 FROM file_facts f WHERE f.profile=j.profile AND f.file_id=j.file_id AND f.operation=j.operation AND f.job_id=j.id AND f.status='complete') FROM processing_jobs j WHERE j.profile=? AND j.file_id=? AND j.operation=? AND j.cache_key=? AND j.state!='cancelled' ORDER BY j.created_at DESC,j.id DESC LIMIT 1", params![profile,file,operation,key], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((id,state,current)) = old && (state != "complete" || current) && (!refresh || ["queued","running"].contains(&state.as_str())) && (operation != "verify" || recipe.is_some()) { return Ok(json!({"id":id,"cached":true})); }
        if operation == "verify" && !db.query_row("SELECT EXISTS(SELECT 1 FROM content_baselines WHERE profile=? AND file_id=?)", params![profile,file], |r| r.get::<_,bool>(0))? { return Err(Error("no checksum baseline; run hash first".into())); }
        let id = uuid::Uuid::new_v4().to_string();
        db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,recipe_id) VALUES (?,?,?,?,?,?,?,?,?)", params![id,profile,file,operation,observed["location"].as_str(),encode(&observed),encode(config),key,recipe])?;
        Ok(json!({"id":id,"cached":false}))
    })
}
pub fn get_job(store: &Store, profile: &str, id: &str) -> Result<Value> {
    let mut result = rows(
        &store.db,
        "SELECT * FROM processing_jobs WHERE profile=? AND id=?",
        &[SqlValue::Text(profile.into()), SqlValue::Text(id.into())],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown processing job".into()))?;
    for key in ["snapshot", "options", "result"] {
        if let Some(value) = result[key].as_str() {
            result[key] = catabolic_core::decode_json(value)?;
        }
    }
    Ok(result)
}
pub fn start(store: &Store, profile: &str, id: &str, now: f64) -> Result<Claim> {
    validate_now(now)?;
    store.transaction(|db| {
        let operation: String = db
            .query_row(
                "SELECT operation FROM processing_jobs WHERE id=? AND profile=?",
                params![id, profile],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| Error("unknown processing job".into()))?;
        if !["sniff", "hash", "verify", "probe", "decode", "text"].contains(&operation.as_str()) {
            return Err(Error(
                "only queued or abandoned native analysis jobs can be started".into(),
            ));
        }
        let claim = acquire(db, id, now, None)?;
        db.execute(
            "UPDATE processing_jobs SET state='running',attempts=? WHERE id=?",
            params![claim.attempt, id],
        )?;
        Ok(claim)
    })
}
pub fn fence(store: &Store, claim: &Claim, now: f64) -> Result<()> {
    fence_db(&store.db, claim, now)
}
pub fn heartbeat(store: &Store, claim: &Claim, now: f64) -> Result<()> {
    store.transaction(|db| {
        let Effect::Renew { until } = decide(
            db,
            &claim.job_id,
            now,
            Input::Renew {
                now,
                duration: LEASE_SECONDS,
                proof: proof(claim),
            },
        )?
        else {
            return Err(Error("invalid execution renewal effect".into()));
        };
        db.execute(
            "UPDATE execution_claims SET lease_until=? WHERE job_id=? AND token=?",
            params![until, claim.job_id, claim.token],
        )?;
        Ok(())
    })
}
pub fn release(store: &Store, claim: &Claim) -> Result<()> {
    store.transaction(|db| {
        db.execute(
            "UPDATE execution_claims SET lease_until=0 WHERE job_id=? AND token=?",
            params![claim.job_id, claim.token],
        )?;
        Ok(())
    })
}
pub fn cancel(store: &Store, profile: &str, id: &str) -> Result<Value> {
    store.transaction(|db| {
        let publication_intent = db.query_row("SELECT EXISTS(SELECT 1 FROM processing_artifacts WHERE job_id=? AND state='publishing')",[id],|r| r.get::<_,bool>(0))?;
        if publication_intent { return Err(Error("recover publication intent before cancelling this job".into())); }
        if !db.query_row("SELECT EXISTS(SELECT 1 FROM processing_jobs WHERE id=? AND profile=?)",params![id,profile],|r|r.get::<_,bool>(0))? { return Ok(json!({"cancelled":false,"id":id})); }
        let Effect::Cancel { changed } = decide(db,id,0.0,Input::Cancel { publication_intent })? else { return Err(Error("invalid cancellation effect".into())); };
        if changed { db.execute("UPDATE processing_jobs SET state='cancelled',finished_at=CURRENT_TIMESTAMP WHERE id=? AND profile=?",params![id,profile])?; }
        Ok(json!({"cancelled": changed,"id": id}))
    })
}
pub fn retry(store: &Store, profile: &str, id: &str) -> Result<Value> {
    store.transaction(|db| {
        let pending_artifact = db.query_row("SELECT EXISTS(SELECT 1 FROM processing_artifacts WHERE job_id=? AND profile=? AND state IN ('planned','writing','validating','publishing'))",params![id,profile],|r| r.get::<_,bool>(0))?;
        if !db.query_row("SELECT EXISTS(SELECT 1 FROM processing_jobs WHERE id=? AND profile=?)",params![id,profile],|r|r.get::<_,bool>(0))? { return Err(rejected(Rejection::NotRetryable)); }
        let Effect::Requeue = decide(db,id,0.0,Input::Retry { pending_artifact })? else { return Err(Error("invalid retry effect".into())); };
        db.execute("UPDATE processing_jobs SET state='queued',error=NULL,finished_at=NULL WHERE id=? AND profile=?",params![id,profile])?;
        Ok(json!({"queued":id}))
    })
}
fn enqueue_refresh(db: &Connection, profile: &str) -> Result<()> {
    crate::catalog_refresh::enqueue(db, profile, None)
}
/// A late, cancelled or replaced worker cannot write facts, retry intents, or
/// components. The fence is rechecked inside the same transaction as publication.
pub fn complete(
    store: &Store,
    profile: &str,
    claim: &Claim,
    result: &Value,
    now: f64,
    retry_delay: u32,
    started_at: Option<&str>,
) -> Result<String> {
    validate_now(now)?;
    if !(1..=3600).contains(&retry_delay) {
        return Err(Error("retry delay must be 1..3600 seconds".into()));
    }
    let job = get_job(store, profile, &claim.job_id)?;
    let mut state = result["state"]
        .as_str()
        .ok_or_else(|| Error("missing processing result state".into()))?
        .to_owned();
    if ![
        "complete",
        "partial",
        "unsupported",
        "timeout",
        "changed",
        "failed",
        "mismatch",
        "cancelled",
    ]
    .contains(&state.as_str())
    {
        return Err(Error("invalid processing result state".into()));
    }
    let mut data = result["data"].clone();
    if !data.is_object() {
        return Err(Error("processing data must be an object".into()));
    }
    let mut error = result["error"].as_str().map(str::to_owned);
    let invalid = data["media_health"]["status"] == "invalid";
    if state == "complete" || invalid {
        let current = snapshot(store, profile, job["file_id"].as_str().unwrap())?;
        if job["snapshot"]
            .as_object()
            .unwrap()
            .iter()
            .any(|(k, v)| k != "ctime_ns" && current.get(k) != Some(v))
        {
            state = "changed".into();
            error = Some("inventory changed before publishing".into());
        } else if let Err(e) = crate::source::validate(&job["snapshot"]) {
            state = "changed".into();
            error = Some(e.0);
        }
    }
    store.transaction(|db| {
        let outcome = Phase::parse(&state).ok_or_else(|| Error("invalid processing result state".into()))?;
        let Effect::Finish { outcome } = decide(db,&claim.job_id,now,Input::Finish { now, proof: proof(claim), outcome })? else { return Err(Error("invalid completion effect".into())); };
        state = outcome.as_str().into();
        let file = job["file_id"].as_str().unwrap();
        let operation = job["operation"].as_str().unwrap();
        if state != "changed" && invalid { db.execute("UPDATE file_facts SET status='invalidated' WHERE profile=? AND file_id=?",params![profile,file])?; enqueue_refresh(db,profile)?; }
        if state == "complete" && ["hash","verify"].contains(&operation) {
            let digest = data["digest"].as_str().filter(|s| s.len()==64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())).ok_or_else(|| Error("invalid SHA-256 processing result".into()))?;
            let size = data["size"].as_i64().filter(|n| *n>=0).ok_or_else(|| Error("invalid checksum size".into()))?;
            let baseline: Option<String> = db.query_row("SELECT digest FROM content_baselines WHERE profile=? AND file_id=? AND algorithm='sha256'",params![profile,file],|r|r.get(0)).optional()?;
            match baseline { Some(old) if old != digest => { state="mismatch".into(); error=Some("checksum differs from preserved baseline".into()); data["expected_digest"]=json!(old); db.execute("UPDATE file_facts SET status='invalidated' WHERE profile=? AND file_id=?",params![profile,file])?; }, None => { db.execute("INSERT INTO content_baselines VALUES (?,?,?,?,?,?)",params![profile,file,"sha256",digest,size,claim.job_id])?; }, _ => {} }
        }
        if state == "complete" {
            db.execute("INSERT INTO file_facts VALUES (?,?,?,?,?,?,?) ON CONFLICT(profile,file_id,operation) DO UPDATE SET job_id=excluded.job_id,snapshot=excluded.snapshot,data=excluded.data,status=excluded.status", params![profile,file,operation,claim.job_id,encode(&job["snapshot"]),encode(&data),state])?;
            if operation == "text" {
                let segments = data["segments"].as_array().filter(|a|a.len()<=10000).ok_or_else(||Error("invalid text segments".into()))?;
                db.execute("DELETE FROM text_segments WHERE profile=? AND file_id=?",params![profile,file])?;
                for (ordinal,segment) in segments.iter().enumerate() {
                    let text = segment["text"].as_str().ok_or_else(||Error("invalid text segment".into()))?;
                    if !segment["locator"].is_object() {return Err(Error("invalid text locator".into()));}
                    db.execute("INSERT INTO text_segments(profile,file_id,job_id,ordinal,locator,text) VALUES (?,?,?,?,?,?)",params![profile,file,claim.job_id,ordinal as i64,encode(&segment["locator"]),text])?;
                    let id = db.last_insert_rowid();
                    for word in words(text) { db.execute("INSERT INTO text_terms VALUES (?,?)",params![word,id])?; }
                }
            }
            if ["probe","verify","hash"].contains(&operation) && db.query_row("SELECT EXISTS(SELECT 1 FROM media_outputs WHERE profile=? AND file_id=?)",params![profile,file],|r|r.get::<_,bool>(0))? { enqueue_refresh(db,profile)?; }
        }
        db.execute("UPDATE processing_jobs SET state=?,result=?,error=?,finished_at=CURRENT_TIMESTAMP WHERE id=?",params![state,encode(&data),error,claim.job_id])?;
        if state == "complete" && operation == "probe" { components::index_file(db,profile,file,false)?; }
        let retryable = ["failed","timeout"].contains(&state.as_str()) && result["retryable"] == true;
        let retry_after = now + f64::from(retry_delay.saturating_mul(1 << (claim.attempt-1).clamp(0,12)).min(3600));
        db.execute("DELETE FROM processing_retry_queue WHERE job_id=?",[&claim.job_id])?;
        if retryable && claim.attempt <= 10 { db.execute("INSERT INTO processing_retry_queue VALUES (?,?,?,?)",params![claim.job_id,profile,claim.attempt,retry_after])?; }
        db.execute("INSERT INTO processing_attempts(job_id,attempt,state,error,retryable,retry_after,result,started_at,finished_at) VALUES (?,?,?,?,?,?,?,?,CURRENT_TIMESTAMP)",params![claim.job_id,claim.attempt,state,error,retryable,retry_after,encode(&data),started_at])?;
        db.execute("UPDATE execution_claims SET lease_until=0 WHERE job_id=? AND token=?",params![claim.job_id,claim.token])?;
        Ok(state)
    })
}

pub fn operation_get(store: &Store, id: &str) -> Result<Value> {
    let mut value = rows(
        &store.db,
        "SELECT * FROM processing_recipes WHERE id=?",
        &[SqlValue::Text(id.into())],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown immutable operation revision".into()))?;
    value["definition"] = catabolic_core::decode_json(value["definition"].as_str().unwrap())?;
    Ok(value)
}
pub fn operation_put(store: &Store, name: &str, definition: &Value) -> Result<Value> {
    if name.is_empty()
        || name.len() > 64
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return Err(Error("invalid name".into()));
    }
    let serialized = encode(definition);
    let digest = sha256(&serialized);
    let id=store.transaction(|db| {
        let old: Option<String>=db.query_row("SELECT id FROM processing_recipes WHERE name=? AND digest=?",params![name,digest],|r|r.get(0)).optional()?;
        if let Some(id)=old {return Ok(id);}
        let id=uuid::Uuid::new_v4().to_string();
        let revision:i64=db.query_row("SELECT coalesce(max(revision),0)+1 FROM processing_recipes WHERE name=?",[name],|r|r.get(0))?;
        db.execute("INSERT INTO processing_recipes(id,name,revision,preset,definition,digest,operation_kind,output_definition_id) VALUES (?,?,?,?,?,?,?,NULL)",params![id,name,revision,definition["operation"].as_str(),serialized,digest,"analysis"])?;
        Ok(id)
    })?;
    operation_get(store, &id)
}

pub(crate) fn words(text: &str) -> std::collections::BTreeSet<String> {
    static RANGES: std::sync::OnceLock<Vec<(u32, u32)>> = std::sync::OnceLock::new();
    let ranges = RANGES.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/text-word-ranges.json"))
            .expect("frozen Unicode word ranges")
    });
    let folded = crate::sql::casefold(text);
    folded
        .split(|c: char| {
            let n = c as u32;
            let i = ranges.partition_point(|(_, end)| *end < n);
            !ranges
                .get(i)
                .is_some_and(|(start, end)| *start <= n && n <= *end)
        })
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn active(store: &Store, id: &str, now: f64) -> Result<bool> {
    let claim=store.db.query_row("SELECT job_id,generation,token,worker_pid,attempt,lease_until,snapshot,options FROM execution_claims WHERE job_id=?",[id],claim_row).optional()?;
    Ok(claim.as_ref().is_some_and(|claim| claim_live(claim, now)))
}

/// Claim only retries due at invocation start and normalize abandoned analysis
/// attempts. Scope is exact, including an explicitly empty ID list. Every state
/// change is interpreted from the shared lifecycle reducer in this transaction.
pub fn prepare_run(
    store: &Store,
    profile: &str,
    job_ids: Option<&[String]>,
    now: f64,
    retry_transient: u32,
    limit: usize,
) -> Result<usize> {
    validate_now(now)?;
    let scope = if job_ids.is_some() {
        " AND j.id IN (SELECT value FROM json_each(?))"
    } else {
        ""
    };
    store.transaction(|db| {
        let mut args = vec![SqlValue::Real(now), profile.to_owned().into(), i64::from(retry_transient).into()];
        if let Some(ids) = job_ids { args.push(catabolic_core::encode(&json!(ids)).into()); }
        args.push((limit.min(1000) as i64).into());
        let due = rows(db, &format!("SELECT j.id FROM processing_retry_queue a JOIN processing_jobs j ON j.id=a.job_id WHERE a.retry_after<=? AND a.profile=? AND j.operation!='render' AND j.attempts=a.attempt AND j.attempts<=? AND j.state IN ('failed','timeout'){scope} ORDER BY a.retry_after,a.job_id LIMIT ?"), &args)?;
        for row in &due {
            let id = row["id"].as_str().unwrap();
            let pending_artifact = db.query_row("SELECT EXISTS(SELECT 1 FROM processing_artifacts WHERE job_id=? AND state IN ('planned','writing','validating','publishing'))", [id], |r| r.get::<_,bool>(0))?;
            let Effect::Requeue = decide(db, id, now, Input::Retry { pending_artifact })? else {
                return Err(Error("invalid retry effect".into()));
            };
            db.execute("DELETE FROM processing_retry_queue WHERE job_id=?", [id])?;
            db.execute("UPDATE processing_jobs SET state='queued',finished_at=NULL WHERE id=?", [id])?;
        }
        let mut after = String::new();
        loop {
            let mut args = vec![profile.to_owned().into(), after.clone().into()];
            if let Some(ids) = job_ids { args.push(catabolic_core::encode(&json!(ids)).into()); }
            let abandoned = rows(db, &format!("SELECT j.id FROM processing_jobs j WHERE j.profile=? AND j.id>? AND j.state='running' AND j.operation!='render'{scope} ORDER BY j.id LIMIT 1000"), &args)?;
            for row in &abandoned {
                let id = row["id"].as_str().unwrap();
                let state = observation(db, id, now)?;
                match execution::reduce(&state, &Input::Reclaim { now }) {
                    Ok(Effect::Requeue) => {
                        db.execute("UPDATE execution_claims SET lease_until=0 WHERE job_id=?", [id])?;
                        db.execute("UPDATE processing_jobs SET state='queued' WHERE id=?", [id])?;
                    }
                    Err(Rejection::LiveOwner) => {}
                    Err(error) => return Err(rejected(error)),
                    _ => return Err(Error("invalid reclaim effect".into())),
                }
                after = id.to_owned();
            }
            if abandoned.len() < 1000 { break; }
        }
        Ok(due.len())
    })
}
