//! Store half of the closed observation-helper lifetime protocol.
//!
//! Reservation, child entry, and successful closed execution do not prove exit.
//! Only the runtime's private retained-Child exit owner may make the final seal.
//! Low-level Store/SQL access remains privileged; none of these public methods
//! accept a PID or a claimed exit boolean as cleanup authority.
use crate::{Error, Result, Store, migration, observation};
use catabolic_core::{encode, sha256};
use rusqlite::{OptionalExtension, params, types::Value as SqlValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 37;
pub const PROTOCOL_KIND: &str = "native_observation_no_child";
pub const PROTOCOL_VERSION: u32 = 1;
pub const CLEANUP_KIND: &str = "closed_child_exit_v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    pub reservation_nonce: String,
    pub catalog_id: String,
    pub incarnation: String,
    pub protocol_kind: String,
    pub protocol_version: u32,
    pub helper_digest: String,
    pub profile: String,
    pub source: String,
    pub requester: String,
    pub args_digest: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedClaim {
    pub job_id: String,
    pub generation: i64,
    pub claim_token_digest: String,
}
/// Exact durable shared-owner identity, not retained-Child or host authority.
/// Only runtime's private join path may introduce this into helper arguments.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedClaim {
    pub reservation_nonce: String,
    pub owned: OwnedClaim,
    pub child_pid: u32,
}
fn shared_argument(args: &Value) -> Result<Option<SharedClaim>> {
    args.get("_native_shared_claim")
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|_| Error("invalid_shared_observation_claim".into()))
        })
        .transpose()
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub reservation: Reservation,
    pub state: String,
    pub child_pid: Option<u32>,
    pub request_id: Option<String>,
    pub demand_job_id: Option<String>,
    pub owned: Option<OwnedClaim>,
    pub cleanup_kind: Option<String>,
    pub sealed_at: Option<String>,
}
fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn eligible_store(store: &Store) -> Result<()> {
    if store.schema_version < SCHEMA_VERSION
        || !migration::is_supported_native_schema(store.schema_version)
        || !store.owns_writer_lock()
        || !store.db.is_autocommit()
    {
        return Err(Error("closed_observation_requires_schema37_writer".into()));
    }
    store.validate_native_identity()
}
/// Read-only cohort check. UUID validation is never disabled to obtain a witness.
pub fn preflight(
    store: &Store,
    profile: &str,
    source: &str,
    requester: &str,
    args: &Value,
) -> Result<()> {
    eligible_store(store)?;
    if profile.is_empty()
        || source.is_empty()
        || requester.is_empty()
        || !args.is_object()
        || args["requester"].as_str() != Some(requester)
        || args["source"]
            .as_str()
            .or(args["owner"].as_str())
            .unwrap_or("media")
            != source
        || [
            "continue_id",
            "continue_request",
            "request_id",
            "scopes",
            "crash_after_scope",
        ]
        .iter()
        .any(|key| args.get(*key).is_some())
    {
        return Err(Error("unsupported_closed_observation_arguments".into()));
    }
    observation::binding(store, profile, "source", source)?;
    let policy = observation::trust_policy(store, profile, source)?;
    let enrolled = store.db.prepare("SELECT 1 FROM binding_volumes WHERE profile=? AND kind='source' AND owner=? AND volume_uuid IS NOT NULL")?.exists([profile, source])?;
    if enrolled && policy["settings"]["uuid"] != "skip" {
        return Err(Error(
            "closed_observation_uuid_subprocess_unsupported".into(),
        ));
    }
    Ok(())
}
/// Reserve before launch. A stranded reservation remains unresolved; no historical
/// worker or definite-no-spawn capability is inferred by this initial protocol.
pub fn reserve(
    store: &Store,
    profile: &str,
    source: &str,
    requester: &str,
    helper_digest: &str,
    args: &Value,
) -> Result<Reservation> {
    preflight(store, profile, source, requester, args)?;
    if !valid_hex(helper_digest, 64) {
        return Err(Error("invalid_observation_helper_digest".into()));
    }
    let reservation = Reservation {
        reservation_nonce: uuid::Uuid::new_v4().simple().to_string(),
        catalog_id: store.database_id.clone(),
        incarnation: store
            .catalog_incarnation
            .clone()
            .ok_or_else(|| Error("missing_catalog_incarnation".into()))?,
        protocol_kind: PROTOCOL_KIND.into(),
        protocol_version: PROTOCOL_VERSION,
        helper_digest: helper_digest.into(),
        profile: profile.into(),
        source: source.into(),
        requester: requester.into(),
        args_digest: sha256(encode(args)),
    };
    if let Some(shared) = shared_argument(args)? {
        validate_shared_claim(store, &reservation, &shared)?;
    }
    store.transaction(|db| {
        db.execute("INSERT INTO native_observation_companions(reservation_nonce,catalog_id,incarnation,protocol_kind,protocol_version,helper_digest,profile,source,requester,args_digest) VALUES(?,?,?,?,?,?,?,?,?,?)", params![reservation.reservation_nonce,reservation.catalog_id,reservation.incarnation,reservation.protocol_kind,reservation.protocol_version,reservation.helper_digest,reservation.profile,reservation.source,reservation.requester,reservation.args_digest])?;
        Ok(())
    })?;
    Ok(reservation)
}
/// Exact durable state, not an exit proof or a capability to signal a process.
pub fn read(store: &Store, nonce: &str) -> Result<Option<Record>> {
    store.validate_native_identity()?;
    if store.schema_version < SCHEMA_VERSION {
        return Ok(None);
    }
    Ok(store.db.query_row("SELECT reservation_nonce,catalog_id,incarnation,protocol_kind,protocol_version,helper_digest,profile,source,requester,args_digest,state,child_pid,request_id,demand_job_id,owned_job_id,owned_generation,owned_claim_token_digest,cleanup_kind,sealed_at FROM native_observation_companions WHERE reservation_nonce=?", [nonce], |r| {
        let job_id: Option<String> = r.get(14)?;
        Ok(Record {
            reservation: Reservation { reservation_nonce:r.get(0)?,catalog_id:r.get(1)?,incarnation:r.get(2)?,protocol_kind:r.get(3)?,protocol_version:r.get(4)?,helper_digest:r.get(5)?,profile:r.get(6)?,source:r.get(7)?,requester:r.get(8)?,args_digest:r.get(9)? },
            state:r.get(10)?,child_pid:r.get(11)?,request_id:r.get(12)?,demand_job_id:r.get(13)?,
            owned:job_id.map(|job_id| Ok::<_,rusqlite::Error>(OwnedClaim {job_id,generation:r.get(15)?,claim_token_digest:r.get(16)?})).transpose()?,
            cleanup_kind:r.get(17)?,sealed_at:r.get(18)?,
        })
    }).optional()?)
}

struct CurrentClaim {
    profile: String,
    source: String,
    pid: u32,
    owned: OwnedClaim,
}
type ClaimRow = (String, String, SqlValue, SqlValue, SqlValue);
fn current_claim(store: &Store, job: &str) -> Result<Option<CurrentClaim>> {
    let row: Option<ClaimRow> = store.db.query_row("SELECT profile,source,generation,worker_pid,claim_token FROM observation_jobs WHERE id=?", [job], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((
        profile,
        source,
        SqlValue::Integer(generation),
        SqlValue::Integer(pid),
        SqlValue::Text(token),
    )) = row
    else {
        return Ok(None);
    };
    // A malformed legacy claim must never acquire a witness by hashing arbitrary
    // labels. The observation owner generates canonical non-nil UUID claims.
    if generation <= 0
        || !(1..=i64::from(i32::MAX)).contains(&pid)
        || uuid::Uuid::parse_str(&token)
            .ok()
            .is_none_or(|id| id.is_nil() || id.to_string() != token)
    {
        return Ok(None);
    }
    Ok(Some(CurrentClaim {
        profile,
        source,
        pid: pid as u32,
        owned: OwnedClaim {
            job_id: job.into(),
            generation,
            claim_token_digest: sha256(token),
        },
    }))
}
/// Called only with the exact private Admitted object before traversal.
pub(crate) fn capture_claim(
    store: &Store,
    job: &str,
    request: &str,
    token: &str,
) -> Result<OwnedClaim> {
    let current = current_claim(store, job)?
        .ok_or_else(|| Error("invalid_closed_observation_claim".into()))?;
    if current.pid != std::process::id() || current.owned.claim_token_digest != sha256(token)
        || !store.db.prepare("SELECT 1 FROM observation_requests WHERE id=? AND job_id=? AND profile=? AND source=?")?.exists([request,job,&current.profile,&current.source])?
    { return Err(Error("stale_closed_observation_claim".into())); }
    Ok(current.owned)
}
/// Revalidate completion bindings inside the runtime seal transaction. This is
/// necessary but never sufficient: runtime must also own the actual Child proof.
pub fn completion_matches(store: &Store, record: &Record) -> Result<bool> {
    store.validate_native_identity()?;
    let r = &record.reservation;
    if r.catalog_id != store.database_id
        || Some(&r.incarnation) != store.catalog_incarnation.as_ref()
        || r.protocol_kind != PROTOCOL_KIND
        || r.protocol_version != PROTOCOL_VERSION
        || !matches!(record.state.as_str(), "completed" | "quiescent")
    {
        return Ok(false);
    }
    let (Some(pid), Some(request), Some(demand)) = (
        record.child_pid,
        record.request_id.as_deref(),
        record.demand_job_id.as_deref(),
    ) else {
        return Ok(false);
    };
    if !store.db.prepare("SELECT 1 FROM observation_requests r JOIN observation_jobs j ON j.id=r.job_id WHERE r.id=? AND r.job_id=? AND r.profile=? AND r.source=? AND r.requester=? AND j.profile=r.profile AND j.source=r.source")?.exists([request,demand,&r.profile,&r.source,&r.requester])? { return Ok(false); }
    if let Some(owned) = &record.owned {
        let Some(current) = current_claim(store, &owned.job_id)? else {
            return Ok(false);
        };
        if owned.job_id != demand
            || current.owned != *owned
            || current.pid != pid
            || current.profile != r.profile
            || current.source != r.source
        {
            return Ok(false);
        }
    }
    Ok(true)
}
/// Read-only capture for a runtime that separately retains the actual Child.
/// Supplied PID/JSON cannot grant host authority or mint a cleanup witness.
pub fn capture_shared_claim(
    store: &Store,
    original: &Reservation,
    child_pid: u32,
) -> Result<SharedClaim> {
    let record = read(store, &original.reservation_nonce)?
        .ok_or_else(|| Error("unknown_shared_observation_owner".into()))?;
    if record.reservation != *original {
        return Err(Error("shared_observation_owner_mismatch".into()));
    }
    if record.state == "reserved" {
        return Err(Error("shared_observation_owner_not_admitted".into()));
    }
    if record.child_pid != Some(child_pid) {
        return Err(Error("shared_observation_owner_mismatch".into()));
    }
    let job: Option<String> = store
        .db
        .query_row(
            "SELECT job_id FROM observation_requests WHERE profile=? AND source=? AND requester=?",
            params![original.profile, original.source, original.requester],
            |r| r.get(0),
        )
        .optional()?;
    let job = job.ok_or_else(|| {
        Error(if record.state == "started" {
            "shared_observation_owner_not_admitted".into()
        } else {
            "shared_observation_demand_changed".into()
        })
    })?;
    let current = current_claim(store, &job)?
        .ok_or_else(|| Error("invalid_shared_observation_claim".into()))?;
    let shared = SharedClaim {
        reservation_nonce: original.reservation_nonce.clone(),
        owned: current.owned,
        child_pid,
    };
    validate_shared_claim(store, original, &shared)?;
    Ok(shared)
}
/// Read-only validation of exact current durable identity. This never grants
/// runtime admission authority and never treats a PID as a retained Child.
pub fn validate_shared_claim(
    store: &Store,
    context: &Reservation,
    shared: &SharedClaim,
) -> Result<()> {
    shared_demand(store, context, shared).map(|_| ())
}
/// Original request identity and exact guarantee bytes, checked in the attach
/// transaction. Joining does not create a fresh execution/freshness guarantee.
pub(crate) fn shared_demand(
    store: &Store,
    context: &Reservation,
    shared: &SharedClaim,
) -> Result<(String, String)> {
    store.validate_native_identity()?;
    if store.schema_version < SCHEMA_VERSION
        || context.catalog_id != store.database_id
        || Some(&context.incarnation) != store.catalog_incarnation.as_ref()
        || context.protocol_kind != PROTOCOL_KIND
        || context.protocol_version != PROTOCOL_VERSION
        || !valid_hex(&shared.reservation_nonce, 32)
        || !valid_hex(&shared.owned.claim_token_digest, 64)
    {
        return Err(Error("shared_observation_lifetime_mismatch".into()));
    }
    let original = read(store, &shared.reservation_nonce)?
        .ok_or_else(|| Error("unknown_shared_observation_owner".into()))?;
    let origin = &original.reservation;
    if origin.catalog_id != context.catalog_id
        || origin.incarnation != context.incarnation
        || origin.protocol_kind != context.protocol_kind
        || origin.protocol_version != context.protocol_version
        || origin.helper_digest != context.helper_digest
        || origin.profile != context.profile
        || origin.source != context.source
        || original.child_pid != Some(shared.child_pid)
        || !matches!(
            original.state.as_str(),
            "started" | "completed" | "quiescent"
        )
    {
        return Err(Error("shared_observation_owner_mismatch".into()));
    }
    let current = current_claim(store, &shared.owned.job_id)?
        .ok_or_else(|| Error("invalid_shared_observation_claim".into()))?;
    if current.owned != shared.owned
        || current.pid != shared.child_pid
        || current.profile != context.profile
        || current.source != context.source
    {
        return Err(Error("shared_observation_claim_changed".into()));
    }
    let demand: Option<(String,String)> = store.db.query_row(
        "SELECT r.id,r.guarantees FROM observation_requests r JOIN observation_jobs j ON j.id=r.job_id WHERE r.profile=? AND r.source=? AND r.requester=? AND r.job_id=? AND j.state IN('running','complete')",
        params![origin.profile,origin.source,origin.requester,shared.owned.job_id],
        |r| Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    let demand = demand.ok_or_else(|| Error("shared_observation_demand_changed".into()))?;
    if original.state != "started"
        && (original.request_id.as_ref() != Some(&demand.0)
            || original.demand_job_id.as_ref() != Some(&shared.owned.job_id)
            || original.owned.as_ref() != Some(&shared.owned))
    {
        return Err(Error("shared_observation_completion_mismatch".into()));
    }
    Ok(demand)
}
/// Versioned child entrypoint. Executes only the built-in no-callback traversal,
/// with source subprocess creation forbidden for the complete call chain.
/// A successful return records completed, never quiescent.
pub fn execute_closed(
    store: &mut Store,
    reservation: &Reservation,
    helper_digest: &str,
    args: &Value,
) -> Result<Value> {
    preflight(
        store,
        &reservation.profile,
        &reservation.source,
        &reservation.requester,
        args,
    )?;
    let row = read(store, &reservation.reservation_nonce)?
        .ok_or_else(|| Error("unknown_observation_companion_reservation".into()))?;
    if row.reservation != *reservation
        || row.state != "reserved"
        || reservation.catalog_id != store.database_id
        || Some(&reservation.incarnation) != store.catalog_incarnation.as_ref()
        || reservation.helper_digest != helper_digest
        || !valid_hex(helper_digest, 64)
        || reservation.protocol_kind != PROTOCOL_KIND
        || reservation.protocol_version != PROTOCOL_VERSION
        || reservation.args_digest != sha256(encode(args))
    {
        return Err(Error("observation_companion_reservation_mismatch".into()));
    }
    let shared = shared_argument(args)?;
    if let Some(shared) = &shared {
        validate_shared_claim(store, reservation, shared)?;
    }
    store.transaction(|db| {
        if db.execute("UPDATE native_observation_companions SET state='started',child_pid=? WHERE reservation_nonce=? AND state='reserved'",params![std::process::id(),reservation.reservation_nonce])? != 1 { return Err(Error("stale_observation_companion_reservation".into())); }
        Ok(())
    })?;
    let (result, owned) = crate::source::without_children(|| {
        if let Some(shared) = &shared {
            observation::attach_closed_shared(store, reservation, shared)
                .map(|result| (result, None))
        } else {
            observation::execute_closed_detached(store, &reservation.profile, args)
        }
    })?;
    eligible_store(store)?;
    let (request, demand): (String, String) = store.db.query_row(
        "SELECT id,job_id FROM observation_requests WHERE profile=? AND source=? AND requester=?",
        params![
            reservation.profile,
            reservation.source,
            reservation.requester
        ],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let completed = Record {
        reservation: reservation.clone(),
        state: "completed".into(),
        child_pid: Some(std::process::id()),
        request_id: Some(request),
        demand_job_id: Some(demand),
        owned,
        cleanup_kind: None,
        sealed_at: None,
    };
    store.transaction(|db| {
        if !completion_matches(store,&completed)? { return Err(Error("stale_observation_companion_completion".into())); }
        if db.execute("UPDATE native_observation_companions SET state='completed',request_id=?,demand_job_id=?,owned_job_id=?,owned_generation=?,owned_claim_token_digest=? WHERE reservation_nonce=? AND state='started' AND child_pid=?",params![completed.request_id,completed.demand_job_id,completed.owned.as_ref().map(|c| &c.job_id),completed.owned.as_ref().map(|c| c.generation),completed.owned.as_ref().map(|c| &c.claim_token_digest),reservation.reservation_nonce,std::process::id()])? != 1 { return Err(Error("stale_observation_companion_completion".into())); }
        Ok(())
    })?;
    Ok(result)
}
/// Positive witness for the exact current observation claim only. Shared demand
/// completion, terminal state, lease expiry and PID liveness never qualify.
pub fn has_cleanup_witness(store: &Store, job: &str) -> Result<bool> {
    store.validate_native_identity()?;
    if store.schema_version < SCHEMA_VERSION {
        return Ok(false);
    }
    let Some(current) = current_claim(store, job)? else {
        return Ok(false);
    };
    let nonces = store.db.prepare("SELECT reservation_nonce FROM native_observation_companions WHERE catalog_id=? AND incarnation=? AND owned_job_id=? AND owned_generation=? AND owned_claim_token_digest=? AND child_pid=? AND profile=? AND source=? AND state='quiescent' AND protocol_kind=? AND protocol_version=? AND cleanup_kind=? AND sealed_at IS NOT NULL LIMIT 2")?.query_map(params![store.database_id,store.catalog_incarnation,job,current.owned.generation,current.owned.claim_token_digest,current.pid,current.profile,current.source,PROTOCOL_KIND,PROTOCOL_VERSION,CLEANUP_KIND], |r| r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
    for nonce in nonces {
        if let Some(record) = read(store, &nonce)?
            && completion_matches(store, &record)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}
