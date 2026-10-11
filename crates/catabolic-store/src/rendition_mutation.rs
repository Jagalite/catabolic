// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Principal-owned demand on shared immutable render jobs. Schema triggers own
//! job-to-demand fanout and release reservations; caller cancellation never owns
//! the job, including work requested by CLI users and durable processing rules.
use crate::{
    Error, Result, Store, access::Access, artifact_mutation, content_access, filesystem_mutation,
    media_mutation, observation, playback_cache, webhooks,
};
use catabolic_core::{
    encode,
    rendition::{self, Capacity, DemandPhase, Effect, Input},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::os::unix::fs::MetadataExt;

#[derive(Clone, Copy, Debug)]
pub struct RequestLimits {
    pub max_requests: u64,
    pub max_global: u64,
}
impl Default for RequestLimits {
    fn default() -> Self {
        Self {
            max_requests: 10,
            max_global: 100,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageMode {
    CurrentSourceRevision,
    AcceptedSourceRevision,
}
impl LineageMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CurrentSourceRevision => "current_source_revision",
            Self::AcceptedSourceRevision => "accepted_source_revision",
        }
    }
}
fn decide(input: Input) -> Result<Effect> {
    rendition::reduce(&input).map_err(|e| Error(e.code().into()))
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| Error("invalid_rendition_request".into()))
}
fn normalize(body: &Value, key: &str) -> Result<Value> {
    let mut body = body
        .as_object()
        .cloned()
        .ok_or_else(|| Error("invalid_rendition_request".into()))?;
    if body.get("callback_url").is_some_and(Value::is_null) {
        body.remove("callback_url");
    }
    if !(1..=128).contains(&key.chars().count())
        || body.keys().any(|k| {
            ![
                "item_id",
                "source_file_id",
                "source_revision",
                "operation_id",
                "callback_url",
            ]
            .contains(&k.as_str())
        })
        || [
            "item_id",
            "source_file_id",
            "source_revision",
            "operation_id",
        ]
        .iter()
        .any(|k| {
            !body
                .get(*k)
                .and_then(Value::as_str)
                .is_some_and(|s| (1..=256).contains(&s.chars().count()))
        })
        || body.get("callback_url").is_some_and(|v| !v.is_string())
    {
        return Err(Error("invalid_rendition_request".into()));
    }
    Ok(Value::Object(body))
}
fn operation(store: &Store, profile: &str, id: &str) -> Result<Value> {
    store
        .db
        .query_row(
            "SELECT recipe_id,location FROM api_operations WHERE id=? AND profile=? AND enabled=1",
            params![id, profile],
            |r| Ok(json!({"recipe_id":r.get::<_,String>(0)?,"location":r.get::<_,String>(1)?})),
        )
        .optional()?
        .ok_or_else(|| Error("operation_not_approved".into()))
}
fn source(store: &Store, profile: &str, body: &Value, lineage: bool) -> Result<()> {
    if !lineage && !store.db.query_row("SELECT EXISTS(SELECT 1 FROM item_files WHERE item_id=? AND file_id=? AND active=1 AND role IN ('primary','source'))",params![text(body,"item_id")?,text(body,"source_file_id")?],|r|r.get::<_,bool>(0))? {return Err(Error("source_not_associated".into()));}
    if content_access::revision_of(store, profile, text(body, "source_file_id")?)?
        != text(body, "source_revision")?
    {
        return Err(Error("revision_mismatch".into()));
    }
    crate::source::validate(&content_access::snapshot(
        store,
        profile,
        text(body, "source_file_id")?,
    )?)?;
    Ok(())
}
struct Budget {
    device: u64,
    available: u128,
    output: u64,
    floor: u64,
}
fn budget(store: &Store, profile: &str, operation: &Value) -> Result<Budget> {
    let recipe =
        artifact_mutation::get_recipe(store, text(operation, "recipe_id")?)?["definition"].clone();
    let binding = observation::binding(store, profile, "source", text(operation, "location")?)?;
    let root = filesystem_mutation::open_root(text(&binding, "root")?)?;
    let metadata = root.metadata()?;
    if binding["device"].as_u64() != Some(metadata.dev())
        || binding["inode"].as_u64() != Some(metadata.ino())
    {
        return Err(Error("source root identity changed".into()));
    }
    Ok(Budget {
        device: metadata.dev(),
        available: playback_cache::available_bytes(&root)?,
        output: recipe["max_output_bytes"]
            .as_u64()
            .ok_or_else(|| Error("invalid render byte budget".into()))?,
        floor: recipe["reserve_bytes"]
            .as_u64()
            .ok_or_else(|| Error("invalid render reserve".into()))?,
    })
}
fn capacity(
    access: &Access,
    budget: &Budget,
    job: Option<&str>,
    exclude: Option<&str>,
    limits: RequestLimits,
) -> Result<Capacity> {
    let (global,principal):(i64,i64)=access.store().db.query_row("SELECT count(*),coalesce(sum(r.principal=?),0) FROM api_requests r JOIN processing_jobs j ON j.id=r.job_id WHERE j.state IN ('queued','running') AND (? IS NULL OR r.id!=?)",params![access.principal(),exclude,exclude],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let reserved = playback_cache::reserved_bytes(access.store(), budget.device)?;
    Ok(Capacity {
        principal_pending: u64::try_from(principal)
            .map_err(|_| Error("invalid pending count".into()))?,
        global_pending: u64::try_from(global).map_err(|_| Error("invalid pending count".into()))?,
        principal_limit: limits.max_requests,
        global_limit: limits.max_global,
        available: budget.available,
        reserved: u128::try_from(reserved)
            .map_err(|_| Error("invalid storage reservation".into()))?,
        output_budget: budget.output,
        reserve_floor: budget.floor,
        job_reserved: job
            .map(|id| {
                access.store().db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM api_reservations WHERE job_id=?)",
                    [id],
                    |r| r.get::<_, bool>(0),
                )
            })
            .transpose()?
            .unwrap_or(false),
    })
}
/// Worker capabilities are durable and profile-bound. Ingress must not discover
/// external tools while holding its request-owned catalog writer.
pub fn capabilities(access: &Access) -> Result<Value> {
    let raw = access
        .store()
        .db
        .query_row(
            "SELECT capabilities FROM api_worker_status WHERE profile=?",
            [access.profile()],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| Error("worker_unavailable".into()))?;
    Ok(catabolic_core::decode_json(&raw)?)
}
#[allow(clippy::too_many_arguments)]
pub fn admit(
    access: &mut Access,
    body: &Value,
    key: &str,
    capabilities: &Value,
    limits: RequestLimits,
    lineage: Option<LineageMode>,
) -> Result<Value> {
    access.require("processing:request")?;
    let body = normalize(body, key)?;
    if !access.processing(&body)? {
        return Err(Error("forbidden".into()));
    }
    if let Some(url) = body["callback_url"].as_str() {
        access.allow_callback(url)?;
    }
    source(access.store(), access.profile(), &body, lineage.is_some())?;
    let approved = operation(
        access.store(),
        access.profile(),
        text(&body, "operation_id")?,
    )?;
    let old = access
        .store()
        .db
        .query_row(
            "SELECT id,body FROM api_requests WHERE principal=? AND idempotency_key=?",
            params![access.principal(), key],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((id, stored)) = old {
        decide(Input::Admit {
            existing_body_matches: Some(stored == encode(&body)),
            job_phase: String::new(),
            capacity: Capacity::default(),
        })?;
        return status(access, &id);
    }
    let budget = budget(access.store(), access.profile(), &approved)?;
    let id = uuid::Uuid::new_v4().to_string();
    access.transaction(|access|{
        // Count before creating even a provisional job. A cancelled demand
        // still consumes capacity while its physical job remains pending.
        decide(Input::Admit{existing_body_matches:None,job_phase:"complete".into(),capacity:capacity(access,&budget,None,None,limits)?})?;
        let queued=artifact_mutation::enqueue_request(access.store(),access.profile(),text(&body,"source_file_id")?,text(&approved,"recipe_id")?,text(&approved,"location")?,text(&body,"item_id")?,capabilities,lineage.map(LineageMode::as_str))?;
        let job=text(&queued,"job_id")?;
        let phase=access.store().db.query_row("SELECT state FROM processing_jobs WHERE id=?",[job],|r|r.get::<_,String>(0))?;
        let Effect::Attach{demand,reserve}=decide(Input::Admit{existing_body_matches:None,job_phase:phase,capacity:capacity(access,&budget,Some(job),None,limits)?})? else{return Err(Error("invalid demand admission effect".into()));};
        if reserve {access.store().db.execute("INSERT INTO api_reservations VALUES (?,?,?)",params![job,budget.device as i64,budget.output as i64])?;}
        access.store().db.execute("INSERT INTO api_requests(id,profile,principal,token_id,idempotency_key,body,job_id,state,reserved_bytes,reservation_device) VALUES (?,?,?,?,?,?,?,?,?,?)",params![id,access.profile(),access.principal(),access.token_id(),key,encode(&body),job,demand.as_str(),if demand==DemandPhase::Ready{0}else{budget.output as i64},budget.device as i64])?;
        if let Some(url)=body["callback_url"].as_str(){webhooks::register(access,url,&json!([]),Some(&id))?;}
        Ok(())
    })?;
    status(access, &id)
}
pub fn status(access: &mut Access, id: &str) -> Result<Value> {
    let row = access
        .store()
        .db
        .query_row(
            "SELECT body,state,job_id FROM api_requests WHERE id=? AND principal=? AND profile=?",
            params![id, access.principal(), access.profile()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error("not_found".into()))?;
    let body = catabolic_core::decode_json(&row.0)?;
    if !access.processing(&body)? {
        return Err(Error("not_found".into()));
    }
    let original = DemandPhase::parse(&row.1);
    let current = original == DemandPhase::Cancelled
        || content_access::revision_of(
            access.store(),
            access.profile(),
            text(&body, "source_file_id")?,
        )? == text(&body, "source_revision")?;
    let output = if current && original == DemandPhase::Ready {
        access.store().db.query_row("SELECT a.file_id,o.id FROM processing_artifacts a JOIN media_outputs o ON o.artifact_id=a.id WHERE a.job_id=? AND a.state='ready'",[&row.2],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?
    } else {
        None
    };
    let Effect::Status(phase) = decide(Input::Status {
        demand: original,
        source_current: current,
        output_present: output.is_some(),
    })?
    else {
        return Err(Error("invalid demand status effect".into()));
    };
    let state = if phase == original {
        row.1.as_str()
    } else {
        phase.as_str()
    };
    let mut result = Value::Null;
    if phase == DemandPhase::Ready
        && let Some((file, output)) = output
    {
        let revision = content_access::revision_of(access.store(), access.profile(), &file)?;
        if access.file(&file, "content:read", Some(&revision))? {
            result = json!({"rendition_id":output,"file_id":file,"revision":revision,"content_path":format!("/v1/files/{file}/content?revision={revision}")});
        }
    }
    let blockers = if phase == DemandPhase::Ready && result.is_null() {
        json!(["result_not_authorized"])
    } else if original == DemandPhase::Blocked {
        json!(["authorization_or_operation_unavailable"])
    } else {
        json!([])
    };
    Ok(
        json!({"request_id":id,"state":state,"status_url":format!("/v1/rendition-requests/{id}"),"result":result,"progress":null,"blockers":blockers}),
    )
}
pub fn cancel(access: &mut Access, id: &str) -> Result<Value> {
    status(access, id)?;
    let Effect::CancelDemand = decide(Input::Cancel)? else {
        return Err(Error("invalid demand cancellation effect".into()));
    };
    access.transaction(|a| {
        a.store()
            .db
            .execute("UPDATE api_requests SET state='cancelled' WHERE id=?", [id])?;
        Ok(())
    })?;
    status(access, id)
}
pub fn retry(access: &mut Access, id: &str) -> Result<Value> {
    let report = status(access, id)?;
    let demand = DemandPhase::parse(text(&report, "state")?);
    if !matches!(
        demand,
        DemandPhase::Failed | DemandPhase::Cancelled | DemandPhase::Blocked
    ) {
        return Err(Error("request_not_retryable".into()));
    }
    let (raw, job): (String, String) = access.store().db.query_row(
        "SELECT body,job_id FROM api_requests WHERE id=?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let body = catabolic_core::decode_json(&raw)?;
    if let Some(url) = body["callback_url"].as_str() {
        access.allow_callback(url)?;
    }
    let saved = media_mutation::get_job(access.store(), access.profile(), &job)?;
    // Frozen retry keeps the concrete job and source. It never re-runs policy
    // selection, recipe admission or artifact cache matching.
    if content_access::revision_of(
        access.store(),
        access.profile(),
        text(&body, "source_file_id")?,
    )? != text(&body, "source_revision")?
    {
        return Err(Error("revision_mismatch".into()));
    }
    source(
        access.store(),
        access.profile(),
        &body,
        saved["options"].get("source_lineage_mode").is_some(),
    )?;
    let approved = operation(
        access.store(),
        access.profile(),
        text(&body, "operation_id")?,
    )?;
    let budget = budget(access.store(), access.profile(), &approved)?;
    access.transaction(|access|{
        let Effect::Retry{demand,requeue_job,reserve}=decide(Input::Retry{demand,job_phase:text(&saved,"state")?.into(),capacity:capacity(access,&budget,Some(&job),Some(id),RequestLimits::default())?})? else{return Err(Error("invalid demand retry effect".into()));};
        if reserve {access.store().db.execute("INSERT INTO api_reservations VALUES (?,?,?)",params![job,budget.device as i64,budget.output as i64])?;}
        if requeue_job {media_mutation::retry(access.store(),access.profile(),&job)?;}
        access.store().db.execute("UPDATE api_requests SET state=?,reserved_bytes=?,reservation_device=?,token_id=? WHERE id=?",params![demand.as_str(),if demand==DemandPhase::Ready{0}else{budget.output as i64},budget.device as i64,access.token_id(),id])?;
        access.store().db.execute("UPDATE api_webhooks SET token_id=? WHERE request_id=? AND principal=?",params![access.token_id(),id,access.principal()])?;
        Ok(())
    })?;
    status(access, id)
}
/// Local/wheel adapter; HTTP calls typed owners directly.
pub fn execute(access: &mut Access, operation: &str, request: &Value) -> Result<Value> {
    match operation {
        "admit" => {
            let capabilities = capabilities(access)?;
            admit(
                access,
                &request["body"],
                text(request, "idempotency_key")?,
                &capabilities,
                RequestLimits::default(),
                None,
            )
        }
        "status" => status(access, text(request, "id")?),
        "cancel" => cancel(access, text(request, "id")?),
        "retry" => retry(access, text(request, "id")?),
        _ => Err(Error("unknown rendition request operation".into())),
    }
}

/// The durable worker owner returns only a physical job selection. The runtime
/// owns recovery/render effects, after this short authorization boundary ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerSelection {
    Idle,
    Selected(String),
}
pub fn select_work(store: &Store, profile: &str, capabilities: &Value) -> Result<WorkerSelection> {
    crate::mutation::profile(store, profile)?;
    store.transaction(|db|{db.execute("INSERT INTO api_worker_status VALUES (?,?,?,?) ON CONFLICT(profile) DO UPDATE SET worker_pid=excluded.worker_pid,capabilities=excluded.capabilities,updated_at=excluded.updated_at",params![profile,std::process::id(),encode(capabilities),crate::notifications::now()])?;Ok(())})?;
    let rows = crate::migration::rows(
        &store.db,
        "SELECT r.*,j.state AS job_state FROM api_requests r JOIN processing_jobs j ON j.id=r.job_id WHERE r.profile=? AND r.state IN ('queued','running','validating','blocked','cancelled') AND j.state IN ('queued','running') ORDER BY r.created_at,r.id LIMIT 100",
        &[profile.to_owned().into()],
    )?;
    for row in rows {
        let token = text(&row, "token_id")?;
        // A read-only authorization snapshot may coexist with the sole writer;
        // it ends before any recovery or process execution releases that writer.
        let auth_store = Store::open(&store.path, false, false)?;
        if auth_store.database_id != store.database_id {
            return Err(Error("catalog replaced during worker admission".into()));
        }
        let authorization = Access::from_stored_token(auth_store, token)
            .and_then(|mut access| status(&mut access, text(&row, "id")?));
        let body = catabolic_core::decode_json(text(&row, "body")?)?;
        let enabled: bool = if authorization.is_ok() {
            store.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM api_operations WHERE id=? AND enabled=1)",
                [body["operation_id"].as_str()],
                |r| r.get(0),
            )?
        } else {
            false
        };
        if let Err(error) = &authorization
            && ![
                "unauthorized",
                "forbidden",
                "not_found",
                "operation_disabled",
            ]
            .contains(&error.0.as_str())
        {
            return Err(Error(error.0.clone()));
        }
        let effect = decide(Input::Worker {
            demand: DemandPhase::parse(text(&row, "state")?),
            job_phase: text(&row, "job_state")?.into(),
            authorized: authorization.is_ok(),
            operation_enabled: enabled,
            source_current: authorization.as_ref().is_ok_and(|r| r["state"] != "stale"),
        })?;
        let job = text(&row, "job_id")?;
        match effect {
            Effect::BlockDemand => {
                store.transaction(|db| {
                    db.execute(
                        "UPDATE api_requests SET state='blocked' WHERE id=? AND state!='cancelled'",
                        [row["id"].as_str()],
                    )?;
                    Ok(())
                })?;
            }
            Effect::ChangeQueuedJob => {
                store.transaction(|db| {
                    db.execute(
                        "UPDATE processing_jobs SET state='changed' WHERE id=? AND state='queued'",
                        [job],
                    )?;
                    Ok(())
                })?;
            }
            Effect::SelectJob => {
                let claims =
                    crate::migration::rows(&store.db, "SELECT job_id FROM execution_claims", &[])?;
                for claim in claims {
                    if media_mutation::active(
                        store,
                        text(&claim, "job_id")?,
                        crate::notifications::now(),
                    )? {
                        return Ok(WorkerSelection::Idle);
                    }
                }
                return Ok(WorkerSelection::Selected(job.into()));
            }
            Effect::Ignore => {}
            _ => return Err(Error("invalid demand worker effect".into())),
        }
    }
    Ok(WorkerSelection::Idle)
}
/// Called only after durable artifact recovery has fenced/reconciled old work.
pub fn resume_work(store: &Store, id: &str) -> Result<()> {
    store.transaction(|db| {
        db.execute(
            "UPDATE processing_jobs SET state='queued' WHERE id=? AND state='running'",
            [id],
        )?;
        Ok(())
    })
}

/// A logical idempotency key is permanently bound to its first concrete source.
/// Retries call the ordinary request owner and never re-run policy resolution.
pub fn logical_admit(
    access: &mut Access,
    body: &Value,
    key: &str,
    capabilities: &Value,
    probe: &mut crate::fallback_resolution::Probe<'_>,
) -> Result<Value> {
    let object = body
        .as_object()
        .ok_or_else(|| Error("invalid_rendition_request".into()))?;
    if object.keys().any(|k| {
        ![
            "item_id",
            "fallback_policy_id",
            "operation_id",
            "role",
            "part",
            "variant",
        ]
        .contains(&k.as_str())
    }) {
        return Err(Error("invalid_rendition_request".into()));
    }
    let mut normalized = json!({"role":"primary","part":null,"variant":""});
    for (key, value) in object {
        normalized[key] = value.clone();
    }
    let item = text(&normalized, "item_id")?.to_owned();
    let policy_id = text(&normalized, "fallback_policy_id")?.to_owned();
    let operation_id = text(&normalized, "operation_id")?.to_owned();
    let policy =
        crate::fallback_resolution::require_policy(access, &policy_id, "processing:request")?;
    if !policy["definition"]["package"].is_null() {
        return Err(Error(
            "component_packaging_requires_explicit_local_admission".into(),
        ));
    }
    let old=access.store().db.query_row("SELECT body,request_id FROM api_logical_demands WHERE principal=? AND idempotency_key=?",params![access.principal(),key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?;
    if let Some((old, id)) = old {
        if old != encode(&normalized) {
            return Err(Error("idempotency_conflict".into()));
        }
        return status(access, &id);
    }
    if capabilities.is_null() {
        return Err(Error("worker_unavailable".into()));
    }
    let decision = crate::fallback_resolution::resolve_access(
        access,
        &item,
        &normalized,
        Some(&operation_id),
        probe,
    )?;
    if decision["file_id"].is_null() {
        return Err(Error("no_eligible_processing_source".into()));
    }
    let concrete = json!({"item_id":item,"source_file_id":decision["file_id"],"source_revision":decision["revision"],"operation_id":operation_id});
    let lineage = if policy["definition"]["lineage_requirement"] == "accepted_source_revision" {
        LineageMode::AcceptedSourceRevision
    } else {
        LineageMode::CurrentSourceRevision
    };
    access.transaction(|access| {
        let result = admit(
            access,
            &concrete,
            key,
            capabilities,
            RequestLimits::default(),
            Some(lineage),
        )?;
        access.store().db.execute(
            "INSERT INTO api_logical_demands VALUES (?,?,?,?)",
            params![
                access.principal(),
                key,
                encode(&normalized),
                result["request_id"].as_str()
            ],
        )?;
        Ok(result)
    })
}
