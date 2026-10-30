// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Principal-isolated playback sessions backed by shared, revision-pinned HLS.
//! The cache is not a catalog rendition and never publishes processing outputs.
use crate::{
    Error, Result, Store, access::Access, artifact_mutation, content_access,
    media_mutation::validated_source, migration::rows, notifications::now, observation,
    playback_cache as cache,
};
use catabolic_core::{decode_json, encode, sha256};
use rusqlite::params;
use serde_json::{Value, json};
use std::{fs::File, os::unix::fs::MetadataExt};

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| Error("invalid_playback_request".into()))
}
fn decode(v: &Value, key: &str) -> Result<Value> {
    Ok(decode_json(text(v, key)?)?)
}

pub fn permitted(access: &mut Access, body: &Value) -> Result<Value> {
    if !access.processing(body)? {
        return Err(Error("not_found".into()));
    }
    let mut content = false;
    for grant in access.matching("content:read") {
        if grant["operator"] == true
            || (grant["derivatives"] == true
                && access.members(&grant)?.contains(text(body, "item_id")?))
        {
            content = true;
            break;
        }
    }
    if !content {
        return Err(Error("playback_content_not_authorized".into()));
    }
    let store = access.store();
    let operation = rows(
        &store.db,
        "SELECT * FROM api_operations WHERE id=? AND profile=? AND enabled=1",
        &[
            text(body, "operation_id")?.to_owned().into(),
            access.profile().to_owned().into(),
        ],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("operation_not_approved".into()))?;
    if !store
        .db
        .prepare("SELECT 1 FROM api_sources WHERE profile=? AND location=?")?
        .exists(params![access.profile(), text(&operation, "location")?])?
    {
        return Err(Error("source_not_exposed".into()));
    }
    if !store.db.prepare("SELECT 1 FROM item_files WHERE item_id=? AND file_id=? AND active=1 AND role IN ('primary','source')")?.exists(params![text(body,"item_id")?,text(body,"source_file_id")?])? {return Err(Error("source_not_associated".into()));}
    if content_access::revision_of(store, access.profile(), text(body, "source_file_id")?)?
        != text(body, "source_revision")?
    {
        return Err(Error("revision_mismatch".into()));
    }
    Ok(operation)
}
fn existing_result(
    access: &mut Access,
    body: &Value,
    operation: &Value,
    source: &Value,
) -> Result<Option<Value>> {
    let candidates = rows(
        &access.store().db,
        "SELECT a.id,o.file_id,o.id AS rendition_id,j.snapshot FROM processing_jobs j JOIN processing_artifacts a ON a.job_id=j.id JOIN media_outputs o ON o.artifact_id=a.id WHERE j.profile=? AND j.file_id=? AND j.recipe_id=? AND j.state='complete' AND a.state='ready' AND o.source_item_id=? AND a.location=? ORDER BY j.created_at DESC LIMIT 20",
        &[
            access.profile().to_owned().into(),
            text(body, "source_file_id")?.to_owned().into(),
            text(operation, "recipe_id")?.to_owned().into(),
            text(body, "item_id")?.to_owned().into(),
            text(operation, "location")?.to_owned().into(),
        ],
    )?;
    for row in candidates {
        let old = decode(&row, "snapshot")?;
        if [
            "location",
            "path",
            "size",
            "mtime_ns",
            "device",
            "inode",
            "root",
            "root_device",
            "root_inode",
            "source_policy",
            "volume_uuid",
            "ctime_ns",
        ]
        .iter()
        .any(|k| old[k] != source[k])
        {
            continue;
        }
        let artifact = artifact_mutation::get(access.store(), access.profile(), text(&row, "id")?)?;
        if !artifact_mutation::usable_metadata(access.store(), access.profile(), &artifact)? {
            continue;
        }
        let file = text(&row, "file_id")?;
        let revision = content_access::revision_of(access.store(), access.profile(), file)?;
        if content_access::authorize(access, file, &revision).is_err() {
            continue;
        }
        return Ok(Some(
            json!({"file_id":file,"revision":revision,"content_path":format!("/v1/files/{file}/content?revision={revision}"),"source_snapshot":source}),
        ));
    }
    Ok(None)
}

pub fn admit(
    access: &mut Access,
    body: &Value,
    key: &str,
    ttl: u64,
    tools: &Value,
) -> Result<Value> {
    access.require("processing:request")?;
    if key.is_empty() || key.chars().count() > 128 {
        return Err(Error("invalid_idempotency_key".into()));
    }
    if !(60..=21600).contains(&ttl)
        || !body.as_object().is_some_and(|b| {
            b.len() == 4
                && [
                    "item_id",
                    "source_file_id",
                    "source_revision",
                    "operation_id",
                ]
                .iter()
                .all(|k| {
                    b.get(*k)
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty() && s.chars().count() <= 256)
                })
        })
    {
        return Err(Error("invalid_playback_request".into()));
    }
    let operation = permitted(access, body)?;
    let mut stored_body = body.clone();
    stored_body["ttl"] = json!(ttl);
    let stored_body = encode(&stored_body);
    let previous = rows(
        &access.store().db,
        "SELECT * FROM api_playback_sessions WHERE principal=? AND idempotency_key=?",
        &[access.principal().to_owned().into(), key.to_owned().into()],
    )?;
    if let Some(row) = previous.first() {
        if row["body"].as_str() != Some(&stored_body) {
            return Err(Error("idempotency_conflict".into()));
        }
        let report = status(access, text(row, "id")?, tools)?;
        access.store().transaction(|db| {
            db.execute(
                "UPDATE api_playback_sessions SET token_id=? WHERE id=?",
                params![access.token_id(), text(row, "id")?],
            )?;
            Ok(())
        })?;
        return Ok(report);
    }
    let pending = rows(
        &access.store().db,
        "SELECT principal,count(*) AS count FROM api_playback_sessions WHERE cancelled=0 AND expires>? GROUP BY principal",
        &[now().into()],
    )?;
    let total: i64 = pending
        .iter()
        .map(|v| v["count"].as_i64().unwrap_or(0))
        .sum();
    let principal_total = pending
        .iter()
        .filter(|v| v["principal"] == access.principal())
        .map(|v| v["count"].as_i64().unwrap_or(0))
        .sum::<i64>();
    if !catabolic_core::playback_admission::session_allowed(total, principal_total) {
        return Err(Error("playback_session_limit".into()));
    }
    let recipe=artifact_mutation::get_recipe(access.store(),text(&operation,"recipe_id")?)?["definition"].clone();
    if !catabolic_core::playback_admission::preset_supported(
        recipe["preset"].as_str().unwrap_or(""),
    ) {
        return Err(Error("playback_profile_unsupported".into()));
    }
    let mut source = content_access::snapshot(
        access.store(),
        access.profile(),
        text(body, "source_file_id")?,
    )?;
    let ctime = validated_source(&source, |fd| {
        let st = fd.metadata()?;
        Ok(i128::from(st.ctime()) * 1_000_000_000 + i128::from(st.ctime_nsec()))
    })?;
    source["ctime_ns"] = json!(ctime);
    let result = existing_result(access, body, &operation, &source)?;
    let identifier = uuid::Uuid::new_v4().to_string();
    // Access owns the normal catalog writer lock. This transaction contains both
    // the reservation/job and the caller session, including rollback on failure.
    access.store().transaction(|db| {
        let store=access.store(); let instant=now(); let mut job_id=None;
        if result.is_none() {
            let binding=observation::binding(store,access.profile(),"source",text(&operation,"location")?)?;
            let key=sha256(encode(&json!([body["item_id"],source,recipe,tools,binding,"hls-v1"])).as_bytes());
            let mut jobs=rows(db,"SELECT * FROM api_playback_jobs WHERE profile=? AND cache_key=? AND state IN ('queued','running','ready')",&[access.profile().to_owned().into(),key.clone().into()])?;
            if jobs.first().is_some_and(|j|j["state"]=="ready") {
                let checked=(|| {let directory=cache::job_directory(store,&jobs[0])?;let p=cache::playlist(&directory)?;if p.segments.is_empty() || !p.raw.as_deref().unwrap_or("").contains("#EXT-X-ENDLIST") {return Err(Error("invalid_playback_cache".into()));} for segment in &p.segments {cache::open_segment(&directory,segment,&p)?;} Ok(())})();
                if checked.is_err() {db.execute("UPDATE api_playback_jobs SET state='stale',reserved_bytes=0,updated_at=? WHERE id=?",params![instant,text(&jobs[0],"id")?])?;jobs.clear();}
            }
            if jobs.first().is_none_or(|j|j["state"]!="ready") {
                let workers=rows(db,"SELECT * FROM api_worker_status WHERE profile=?",&[access.profile().to_owned().into()])?;
                let worker=workers.first().ok_or_else(||Error("worker_unavailable".into()))?;
                let pid=worker["worker_pid"].as_i64().and_then(|p|i32::try_from(p).ok()).filter(|p|*p>0).ok_or_else(||Error("worker_unavailable".into()))?;
                // SAFETY: signal zero only checks liveness of the registered PID.
                if instant-worker["updated_at"].as_f64().unwrap_or(0.)>30. || unsafe {libc::kill(pid,0)}!=0 {return Err(Error("worker_unavailable".into()));}
                let caps=decode(worker,"capabilities")?;
                if caps["playback_hls"]!=true || caps["tools"]!=*tools {return Err(Error("playback_worker_unavailable".into()));}
            }
            if let Some(job)=jobs.first() {job_id=Some(text(job,"id")?.to_owned());}
            else {
                let root=cache::destination_root(store,access.profile(),&binding)?;
                let device=root.metadata()?.dev();
                let max=recipe["max_output_bytes"].as_u64().ok_or_else(||Error("invalid_playback_request".into()))?;
                let reserve=recipe["reserve_bytes"].as_u64().ok_or_else(||Error("invalid_playback_request".into()))?;
                let reserved=cache::reserved_bytes(store,device)?;
                if reserved<0 || !catabolic_core::playback_admission::reservation_available(reserved,cache::available_bytes(&root)?,max,reserve) {return Err(Error("storage_reservation_unavailable".into()));}
                let id=uuid::Uuid::new_v4().to_string();
                db.execute("INSERT INTO api_playback_jobs(id,profile,cache_key,body,snapshot,recipe,tools,destination,reserved_bytes,reservation_device,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",params![id,access.profile(),key,encode(body),encode(&source),encode(&recipe),encode(tools),encode(&binding),i64::try_from(max).map_err(|_|Error("invalid_playback_request".into()))?,i64::try_from(device).map_err(|_|Error("invalid_playback_request".into()))?,instant,instant])?;
                job_id=Some(id);
            }
        }
        db.execute("INSERT INTO api_playback_sessions(id,profile,principal,token_id,idempotency_key,body,job_id,result,created_at,expires) VALUES (?,?,?,?,?,?,?,?,?,?)",params![identifier,access.profile(),access.principal(),access.token_id(),key,stored_body,job_id,result.as_ref().map(encode),instant,instant+ttl as f64])?;
        Ok(())
    })?;
    status(access, &identifier, tools)
}
fn session_row(access: &mut Access, identifier: &str) -> Result<(Value, Value)> {
    let row = rows(
        &access.store().db,
        "SELECT * FROM api_playback_sessions WHERE id=? AND profile=? AND principal=?",
        &[
            identifier.to_owned().into(),
            access.profile().to_owned().into(),
            access.principal().to_owned().into(),
        ],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("not_found".into()))?;
    let mut body = decode(&row, "body")?;
    body.as_object_mut()
        .ok_or_else(|| Error("invalid_playback_request".into()))?
        .remove("ttl");
    permitted(access, &body)?;
    validated_source(
        &content_access::snapshot(
            access.store(),
            access.profile(),
            text(&body, "source_file_id")?,
        )?,
        |_| Ok(()),
    )?;
    if row["expires"].as_f64().unwrap_or(0.) <= now() {
        return Err(Error("playback_session_expired".into()));
    }
    Ok((row, body))
}
pub fn job(store: &Store, identifier: &str) -> Result<Value> {
    rows(
        &store.db,
        "SELECT * FROM api_playback_jobs WHERE id=?",
        &[identifier.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("not_found".into()))
}
fn live_job(access: &Access, row: &Value, tools: &Value) -> Result<Value> {
    if row["cancelled"].as_i64().unwrap_or(0) != 0 {
        return Err(Error("playback_session_cancelled".into()));
    }
    let job = job(
        access.store(),
        row["job_id"]
            .as_str()
            .ok_or_else(|| Error("not_found".into()))?,
    )?;
    if !["running", "ready"].contains(&job["state"].as_str().unwrap_or("")) {
        return Err(Error(
            if job["state"] == "queued" {
                "playback_not_ready"
            } else {
                "playback_failed"
            }
            .into(),
        ));
    }
    validated_source(&decode(&job, "snapshot")?, |_| Ok(()))?;
    if decode(&job, "tools")? != *tools {
        return Err(Error("playback_tools_changed".into()));
    }
    Ok(job)
}
pub fn status(access: &mut Access, identifier: &str, tools: &Value) -> Result<Value> {
    let mut current = access.current()?;
    let (row, _) = session_row(&mut current, identifier)?;
    let (mut state, mut count, mut seconds, mut blockers) = ("ready".to_owned(), 0, 0.0, vec![]);
    let (mut content, mut playlist) = (None, None);
    if row["cancelled"].as_i64().unwrap_or(0) != 0 {
        state = "cancelled".into();
    } else if row["result"].is_string() {
        let result = decode(&row, "result")?;
        validated_source(&result["source_snapshot"], |_| Ok(()))?;
        content_access::authorize(
            &mut current,
            text(&result, "file_id")?,
            text(&result, "revision")?,
        )?;
        validated_source(
            &content_access::snapshot(
                current.store(),
                current.profile(),
                text(&result, "file_id")?,
            )?,
            |_| Ok(()),
        )?;
        content = Some(text(&result, "content_path")?.to_owned());
    } else {
        let job = job(current.store(), text(&row, "job_id")?)?;
        state = text(&job, "state")?.to_owned();
        if ["running", "ready"].contains(&state.as_str()) {
            live_job(&current, &row, tools)?;
            let directory = cache::job_directory(current.store(), &job)?;
            let p = cache::playlist(&directory)?;
            if state == "ready"
                && (p.segments.is_empty()
                    || !p.raw.as_deref().unwrap_or("").contains("#EXT-X-ENDLIST"))
            {
                return Err(Error("invalid_playback_cache".into()));
            }
            count = p.segments.len();
            seconds = p.seconds;
            if count > 0 {
                playlist = Some(format!("/v1/playback-sessions/{identifier}/index.m3u8"));
                if state == "running" {
                    state = "playing".into();
                }
            }
        }
        if let Some(error) = job["error"].as_str() {
            blockers.push(error.to_owned());
        }
    }
    Ok(
        json!({"session_id":identifier,"state":state,"status_url":format!("/v1/playback-sessions/{identifier}"),"playlist_path":playlist,"content_path":content,"segment_count":count,"available_seconds":seconds,"expires":row["expires"],"blockers":blockers}),
    )
}
pub fn cancel(access: &mut Access, identifier: &str, tools: &Value) -> Result<Value> {
    session_row(&mut access.current()?, identifier)?;
    access.store().transaction(|db| {
        db.execute(
            "UPDATE api_playback_sessions SET cancelled=1 WHERE id=? AND principal=? AND profile=?",
            params![identifier, access.principal(), access.profile()],
        )?;
        Ok(())
    })?;
    status(access, identifier, tools)
}
/// Ownership of the validated descriptor never escapes this authorization scope.
/// Transports can stream a whole closed segment without resolving a pathname.
pub enum Media<'a> {
    Playlist(Vec<u8>),
    Segment(&'a File),
}
pub fn with_media<T>(
    access: &mut Access,
    identifier: &str,
    segment: Option<&str>,
    tools: &Value,
    deliver: impl FnOnce(Media<'_>) -> Result<T>,
) -> Result<T> {
    let mut current = access.current()?;
    let (row, _) = session_row(&mut current, identifier)?;
    let job = live_job(&current, &row, tools)?;
    let directory = cache::job_directory(current.store(), &job)?;
    let playlist = cache::playlist(&directory)?;
    if playlist.segments.is_empty() {
        return Err(Error("playback_not_ready".into()));
    }
    if let Some(segment) = segment {
        let file = cache::open_segment(&directory, segment, &playlist)?;
        deliver(Media::Segment(&file))
    } else {
        let lines = playlist
            .raw
            .unwrap_or_default()
            .lines()
            .map(|line| {
                if cache::segment(line) {
                    format!("segments/{line}/content")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>();
        deliver(Media::Playlist(
            format!("{}\n", lines.join("\n")).into_bytes(),
        ))
    }
}
/// Each poll authenticates every stored token against a freshly opened catalog.
/// A cached Access/grant evaluation is never reused across a worker wait.
pub fn interested(store: &Store, job: &Value) -> Result<u32> {
    let sessions = rows(
        &store.db,
        "SELECT * FROM api_playback_sessions WHERE job_id=? AND cancelled=0 AND expires>?",
        &[text(job, "id")?.to_owned().into(), now().into()],
    )?;
    let pinned_body = decode(job, "body")?;
    let pinned_recipe = decode(job, "recipe")?;
    let pinned_destination = decode(job, "destination")?;
    let pinned_source = decode(job, "snapshot")?;
    if !pinned_source.is_object() {
        return Err(Error("invalid_playback_request".into()));
    }
    let mut count = 0;
    for session in sessions {
        let fresh = Store::open(&store.path, false, false)?;
        if fresh.database_id != store.database_id {
            return Err(Error("playback_catalog_changed".into()));
        }
        let Ok(mut access) = Access::from_stored_token(fresh, text(&session, "token_id")?) else {
            continue;
        };
        if access.profile() != text(job, "profile")?
            || access.principal() != text(&session, "principal")?
        {
            continue;
        }
        // The cache key intentionally omits the operation ID, so two callers
        // may share an encode under independently granted operation aliases.
        // Each token must authorize its own request, never the first viewer's.
        let authorized: Result<bool> = (|| {
            let mut body = decode(&session, "body")?;
            body.as_object_mut()
                .ok_or_else(|| Error("invalid_playback_request".into()))?
                .remove("ttl");
            if ["item_id", "source_file_id", "source_revision"]
                .iter()
                .any(|key| body[key] != pinned_body[key])
            {
                return Ok(false);
            }
            let operation = permitted(&mut access, &body)?;
            // A reassigned operation cannot keep an old encode authorized.
            // Recipe content and the full destination binding, rather than
            // alias names alone, identify the originally admitted work.
            if artifact_mutation::get_recipe(access.store(), text(&operation, "recipe_id")?)?["definition"]
                != pinned_recipe
                || operation["location"] != pinned_destination["owner"]
                || observation::binding(
                    access.store(),
                    access.profile(),
                    "source",
                    text(&operation, "location")?,
                )? != pinned_destination
                || !same_source(
                    &content_access::snapshot(
                        access.store(),
                        access.profile(),
                        text(&body, "source_file_id")?,
                    )?,
                    &pinned_source,
                )
            {
                return Ok(false);
            }
            Ok(true)
        })();
        if matches!(authorized, Ok(true)) {
            count += 1;
        }
    }
    Ok(count)
}

/// Admission pins the content snapshot with the live ctime it verified. From
/// schema 39 the current snapshot carries the ctime a native scan bound to the
/// observation, which must equal the pinned one; a snapshot without a bound
/// ctime (schema 32, or before the first native scan) is compared without it.
fn same_source(current: &Value, pinned: &Value) -> bool {
    if current.get("ctime_ns").is_some() {
        return current == pinned;
    }
    let mut pinned = pinned.clone();
    if let Some(object) = pinned.as_object_mut() {
        object.remove("ctime_ns");
    }
    *current == pinned
}
/// Fenced persistence for decisions from the shared production reducer.
pub fn apply_decision(
    store: &Store,
    job: &Value,
    decision: &catabolic_core::playback::Decision,
) -> Result<()> {
    use catabolic_core::playback::State;
    let state = match decision.state {
        State::Queued => "queued",
        State::Running => "running",
        State::Ready => "ready",
        State::Failed => "failed",
        State::Cancelled => "cancelled",
        State::Stale => "stale",
    };
    store.transaction(|db| {let changed=db.execute("UPDATE api_playback_jobs SET state=?,error=?,reserved_bytes=CASE WHEN ? THEN 0 ELSE reserved_bytes END,updated_at=? WHERE id=? AND state=?",params![state,decision.error,decision.release_reservation,now(),text(job,"id")?,text(job,"state")?])?;if changed!=1{return Err(Error("playback_claim_lost".into()));}Ok(())})
}
pub fn start_job(store: &Store, id: &str, device: u64, inode: u64) -> Result<()> {
    let device = i64::try_from(device).map_err(|_| Error("invalid_playback_cache".into()))?;
    let inode = i64::try_from(inode).map_err(|_| Error("invalid_playback_cache".into()))?;
    store.transaction(|db|{let changed=db.execute("UPDATE api_playback_jobs SET state='running',directory_device=?,directory_inode=?,updated_at=? WHERE id=? AND state='queued'",params![device,inode,now(),id])?;if changed!=1{return Err(Error("playback_claim_lost".into()));}Ok(())})
}
pub fn progress(store: &Store, job: &Value, count: usize, seconds: f64, pid: u32) -> Result<()> {
    store.transaction(|db|{db.execute("UPDATE api_playback_jobs SET segment_count=?,available_seconds=?,updated_at=? WHERE id=? AND state='running'",params![i64::try_from(count).map_err(|_|Error("invalid_playback_cache".into()))?,seconds,now(),text(job,"id")?])?;db.execute("UPDATE api_worker_status SET updated_at=? WHERE profile=? AND worker_pid=?",params![now(),text(job,"profile")?,pid])?;Ok(())})
}
pub fn complete_job(
    store: &Store,
    job: &Value,
    decision: &catabolic_core::playback::Decision,
    count: usize,
    seconds: f64,
) -> Result<()> {
    store.transaction(|db|{apply_decision(store,job,decision)?;if decision.state==catabolic_core::playback::State::Ready {db.execute("UPDATE api_playback_jobs SET segment_count=?,available_seconds=? WHERE id=? AND state='ready'",params![i64::try_from(count).map_err(|_|Error("invalid_playback_cache".into()))?,seconds,text(job,"id")?])?;}Ok(())})
}
pub fn fail_job(store: &Store, id: &str, code: &str) -> Result<()> {
    let state = if code == "playback_cancelled" {
        "cancelled"
    } else {
        "failed"
    };
    store.transaction(|db|{db.execute("UPDATE api_playback_jobs SET state=?,error=?,reserved_bytes=0,updated_at=? WHERE id=? AND state IN ('queued','running')",params![state,code,now(),id])?;Ok(())})
}
pub fn running_jobs(store: &Store, profile: &str) -> Result<Vec<Value>> {
    rows(
        &store.db,
        "SELECT * FROM api_playback_jobs WHERE profile=? AND state='running'",
        &[profile.to_owned().into()],
    )
}
pub fn next_job(store: &Store, profile: &str) -> Result<Option<Value>> {
    Ok(rows(&store.db,"SELECT * FROM api_playback_jobs WHERE profile=? AND state='queued' ORDER BY created_at,id LIMIT 1",&[profile.to_owned().into()])?.into_iter().next())
}
pub fn worker_heartbeat(store: &Store, profile: &str, pid: u32, caps: &Value) -> Result<()> {
    store.transaction(|db|{db.execute("INSERT INTO api_worker_status VALUES (?,?,?,?) ON CONFLICT(profile) DO UPDATE SET worker_pid=excluded.worker_pid,capabilities=excluded.capabilities,updated_at=excluded.updated_at",params![profile,pid,encode(caps),now()])?;Ok(())})
}
pub fn worker_remove(store: &Store, profile: &str, pid: u32) -> Result<()> {
    store.transaction(|db| {
        db.execute(
            "DELETE FROM api_worker_status WHERE profile=? AND worker_pid=?",
            params![profile, pid],
        )?;
        Ok(())
    })
}

/// Closed segment capability for transport: no SQLite owner survives between
/// chunks. Current session/token/grants and the named segment are checked anew.
pub struct SegmentTransfer {
    file: File,
    stat: std::fs::Metadata,
    path: std::path::PathBuf,
    database_id: String,
    token: String,
    principal: String,
    profile: String,
    session: String,
    segment: String,
    tools: Value,
}
impl SegmentTransfer {
    pub fn length(&self) -> u64 {
        self.stat.len()
    }
    pub fn read_chunk(&self, offset: u64, length: usize) -> Result<Vec<u8>> {
        let store = Store::open(&self.path, false, false)?;
        if store.database_id != self.database_id {
            return Err(Error("unauthorized".into()));
        }
        let mut access = Access::from_stored_token(store, &self.token)?;
        if access.principal() != self.principal || access.profile() != self.profile {
            return Err(Error("unauthorized".into()));
        }
        with_media(
            &mut access,
            &self.session,
            Some(&self.segment),
            &self.tools,
            |media| match media {
                Media::Segment(file) => {
                    let stat = file.metadata()?;
                    if stat.dev() != self.stat.dev() || stat.ino() != self.stat.ino() {
                        return Err(Error("playback_cache_changed".into()));
                    }
                    Ok(())
                }
                _ => Err(Error("invalid_playback_cache".into())),
            },
        )?;
        drop(access);
        content_access::read_chunk(&self.file, &self.stat, offset, length)
    }
}
pub fn open_transfer(
    mut access: Access,
    session: &str,
    segment: &str,
    tools: &Value,
) -> Result<SegmentTransfer> {
    let file = with_media(
        &mut access,
        session,
        Some(segment),
        tools,
        |media| match media {
            Media::Segment(file) => Ok(file.try_clone()?),
            _ => Err(Error("invalid_playback_cache".into())),
        },
    )?;
    let stat = file.metadata()?;
    Ok(SegmentTransfer {
        file,
        stat,
        path: access.store().path.clone(),
        database_id: access.store().database_id.clone(),
        token: access.token_id().into(),
        principal: access.principal().into(),
        profile: access.profile().into(),
        session: session.into(),
        segment: segment.into(),
        tools: tools.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use catabolic_core::playback::{Event, Evidence, State, decide};

    #[test]
    fn persisted_lifecycle_matches_independent_safety_expectations() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("playback.sqlite3");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        for viewers in 0..=2 {
            for mask in 0..16 {
                for completed in [false, true] {
                    let id = uuid::Uuid::new_v4().to_string();
                    store.transaction(|db| {
                        db.execute("INSERT INTO api_playback_jobs(id,profile,cache_key,body,snapshot,recipe,tools,destination,state,reserved_bytes,reservation_device,created_at,updated_at) VALUES (?,'default',?,'{}','{}','{}','{}','{}','running',4096,1,0,0)",params![id,id])?;
                        Ok(())
                    }).unwrap();
                    let before = job(&store, &id).unwrap();
                    let decision = decide(
                        State::Running,
                        Evidence {
                            authorized_viewers: viewers,
                            source_current: mask & 1 != 0,
                            tools_current: mask & 2 != 0,
                            destination_current: mask & 4 != 0,
                            within_budget: mask & 8 != 0,
                        },
                        if completed {
                            Event::Complete { valid_output: true }
                        } else {
                            Event::Poll
                        },
                    );
                    apply_decision(&store, &before, &decision).unwrap();
                    let after = job(&store, &id).unwrap();
                    let permitted = viewers > 0 && mask == 15;
                    // These expectations are independent of the reducer's
                    // returned flags and exercise the actual schema32 adapter.
                    if permitted && !completed {
                        assert_eq!(after["state"], "running");
                        assert_eq!(after["reserved_bytes"], 4096);
                    } else {
                        assert_eq!(after["reserved_bytes"], 0);
                        if permitted {
                            assert_eq!(after["state"], "ready");
                        } else {
                            assert!(
                                ["failed", "cancelled"].contains(&after["state"].as_str().unwrap())
                            );
                        }
                        // The previous snapshot cannot complete a terminal job.
                        assert_eq!(
                            apply_decision(&store, &before, &decision).unwrap_err().0,
                            "playback_claim_lost"
                        );
                    }
                }
            }
        }
    }
}
