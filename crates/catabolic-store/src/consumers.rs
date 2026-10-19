// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Consumer connection/binding ownership and fenced refresh delivery. This
//! module performs no network I/O and never reads a credential value.
use crate::{
    Error, Result, Store, fallback_resolution::Probe, migration::rows, notifications::now,
};
use catabolic_core::{decode_json, encode, sha256};
use rusqlite::params;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const LEASE_SECONDS: f64 = 180.0;
pub const MAX_ATTEMPTS: i64 = 5;

#[derive(Clone)]
pub struct Failure {
    pub code: String,
    pub retry_after: f64,
}
impl Failure {
    pub fn new(code: &str) -> Self {
        Self {
            code: code.into(),
            retry_after: 0.0,
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(
            self.code.as_str(),
            "temporarily_failed"
                | "rate_limited"
                | "unavailable"
                | "busy"
                | "publication_unhealthy"
                | "publication_changed"
        )
    }
    pub fn report(&self) -> Value {
        json!({"code":self.code,"safe_to_retry":self.retryable(),"message":format!("consumer {}; {}",self.code,if self.retryable(){"retry delivery"}else{"inspect and repair configuration"})})
    }
    pub fn error(&self) -> Error {
        Error(
            self.report()["message"]
                .as_str()
                .unwrap_or("consumer failed")
                .into(),
        )
    }
}
fn error(code: &str) -> Error {
    Error(code.into())
}
pub fn text(value: &Value, maximum: usize) -> Result<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.chars().count() <= maximum && !s.chars().any(|c| c < ' '))
        .ok_or_else(|| error("invalid_configuration"))
}
pub fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    text(&value[key], usize::MAX)
}
fn number(value: &Value, key: &str) -> Result<i64> {
    value[key]
        .as_i64()
        .ok_or_else(|| error("invalid_configuration"))
}
pub fn limit(request: &Value, default: usize, maximum: usize) -> Result<usize> {
    let count = request
        .get("limit")
        .map(|v| {
            v.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| error("invalid_configuration"))
        })
        .transpose()?
        .unwrap_or(default);
    if !(1..=maximum).contains(&count) {
        return Err(error("invalid_configuration"));
    }
    Ok(count)
}
pub fn path(value: &str, relative: bool) -> Result<String> {
    if relative && value.is_empty() {
        return Ok(String::new());
    }
    text(&json!(value), 4096)?;
    if value.contains('\\')
        || value.starts_with("//")
        || (value != "/"
            && value
                .trim_matches('/')
                .split('/')
                .any(|p| matches!(p, "" | "." | "..")))
        || value.starts_with('/') == relative
    {
        return Err(error("invalid_path"));
    }
    Ok(if value == "/" {
        value.into()
    } else {
        value.trim_end_matches('/').into()
    })
}
pub fn within(child: &str, parent: &str) -> bool {
    if parent.is_empty() {
        return !child.starts_with('/');
    }
    if parent == "/" {
        return child.starts_with('/');
    }
    let parent = parent.trim_end_matches('/');
    child == parent
        || child
            .strip_prefix(parent)
            .is_some_and(|s| s.starts_with('/'))
}
pub fn endpoint(value: &str) -> Result<String> {
    crate::notifications::callback_origin(value).map_err(|_| error("invalid_configuration"))?;
    if value
        .split_once('?')
        .is_some_and(|(_, q)| !q.split('#').next().unwrap_or("").is_empty())
        || value.split_once('#').is_some_and(|(_, f)| !f.is_empty())
    {
        return Err(error("invalid_configuration"));
    }
    Ok(value.trim_end_matches('/').into())
}
pub fn connection_spec(profile: &str, request: &Value) -> Result<Value> {
    let id = text(&request["id"], 255)?;
    let application = string(request, "application")?;
    if !matches!(application, "plex" | "jellyfin") {
        return Err(error("unsupported"));
    }
    let address = endpoint(string(request, "endpoint")?)?;
    let credential = string(request, "credential_env")?;
    if let Some(file) = credential.strip_prefix("file:") {
        if application != "plex" {
            return Err(error("plex_credential_requires_plex_connection"));
        }
        path(file, false)?;
    } else {
        crate::notifications::credential_reference(credential)?;
    }
    Ok(
        json!({"id":id,"profile":profile,"application":application,"endpoint":address,"credential_env":credential}),
    )
}
pub fn connection(store: &Store, profile: &str, id: &str) -> Result<Value> {
    rows(
        &store.db,
        "SELECT * FROM consumer_connections WHERE profile=? AND id=?",
        &[profile.to_owned().into(), id.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| error("unknown_connection"))
}
pub fn binding(store: &Store, profile: &str, id: &str) -> Result<Value> {
    rows(
        &store.db,
        "SELECT * FROM consumer_bindings WHERE profile=? AND id=?",
        &[profile.to_owned().into(), id.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| error("unknown_binding"))
}
pub fn put_connection(
    store: &Store,
    profile: &str,
    request: &Value,
    spec: &Value,
    identity: &Value,
) -> Result<Value> {
    let mut value = spec.clone();
    value["server_id"] = json!(string(identity, "server_id")?);
    value["evidence"] = json!(encode(identity));
    if request["apply"] == true {
        store.transaction(|db|{
            if let Some(old)=rows(db,"SELECT * FROM consumer_connections WHERE profile=? AND id=?",&[profile.to_owned().into(),string(&value,"id")?.to_owned().into()])?.first() {
                if ["application","endpoint","server_id"].iter().any(|k|old[k]!=value[k]) { return Err(error("connection_identity_changed_use_new_connection")); }
                if old["credential_env"]!=value["credential_env"] && request["repair"]!=true { return Err(error("explicit_repair_required")); }
            }
            db.execute("INSERT INTO consumer_connections(profile,id,application,endpoint,credential_env,server_id,evidence) VALUES (?,?,?,?,?,?,?) ON CONFLICT(profile,id) DO UPDATE SET credential_env=excluded.credential_env,evidence=excluded.evidence,revision=consumer_connections.revision+CASE WHEN consumer_connections.credential_env!=excluded.credential_env THEN 1 ELSE 0 END",params![profile,string(&value,"id")?,string(&value,"application")?,string(&value,"endpoint")?,string(&value,"credential_env")?,string(&value,"server_id")?,string(&value,"evidence")?])?; Ok(())
        })?;
    }
    value["evidence"] = identity.clone();
    Ok(json!({"connection":value,"applied":request["apply"]==true,"complete":true}))
}
pub fn validate_binding_request(request: &Value) -> Result<()> {
    text(&request["id"], 255)?;
    if request.get("subtree").is_some_and(|v| !v.is_string()) {
        return Err(error("invalid_configuration"));
    }
    path(
        request.get("subtree").and_then(Value::as_str).unwrap_or(""),
        true,
    )?;
    path(string(request, "remote_root")?, false)?;
    let debounce = request
        .get("debounce")
        .map(|v| v.as_i64().ok_or_else(|| error("invalid_configuration")))
        .transpose()?
        .unwrap_or(0);
    if !(0..=3600).contains(&debounce) {
        return Err(error("invalid_configuration"));
    }
    for k in ["connection", "catalog", "library_id", "type"] {
        string(request, k)?;
    }
    Ok(())
}
fn library_identity(library: &Value) -> Value {
    json!({"id":library["id"],"uuid":library["uuid"],"type":library["type"],"roots":library["roots"],"created_at":library["created_at"]})
}
pub fn same_library(left: &Value, right: &Value) -> bool {
    library_identity(left) == library_identity(right)
}
pub fn bind(
    store: &mut Store,
    profile: &str,
    request: &Value,
    observed_connection: &Value,
    libraries: &[Value],
    probe: &mut Probe<'_>,
) -> Result<Value> {
    validate_binding_request(request)?;
    let id = string(request, "id")?;
    let connection_id = string(request, "connection")?;
    let catalog = string(request, "catalog")?;
    let subtree = path(
        request.get("subtree").and_then(Value::as_str).unwrap_or(""),
        true,
    )?;
    let remote = path(string(request, "remote_root")?, false)?;
    let found: Vec<_> = libraries
        .iter()
        .filter(|r| r["id"] == request["library_id"])
        .collect();
    if found.len() != 1 || found[0]["type"] != request["type"] {
        return Err(error("library_mismatch"));
    }
    let library = found[0];
    if library["roots"]
        .as_array()
        .ok_or_else(|| error("invalid_response"))?
        .iter()
        .filter(|r| r.as_str().is_some_and(|p| within(&remote, p)))
        .count()
        != 1
    {
        return Err(error("library_root_mismatch"));
    }
    if connection(store, profile, connection_id)?["revision"] != observed_connection["revision"] {
        return Err(error("connection_changed"));
    }
    let local = crate::observation::binding(store, profile, "output", catalog)?;
    let group = sha256(encode(&json!([
        profile,
        observed_connection["application"],
        observed_connection["server_id"],
        library["id"],
        library["uuid"]
    ])));
    let old = rows(
        &store.db,
        "SELECT * FROM consumer_bindings WHERE profile=? AND id=?",
        &[profile.to_owned().into(), id.to_owned().into()],
    )?
    .into_iter()
    .next();
    let mut value = json!({"profile":profile,"id":id,"connection_id":connection_id,"catalog":catalog,"subtree":subtree,"remote_root":remote,"local_binding":encode(&local),"library":encode(library),"group_id":group,"origin":"adopted","automatic":i64::from(request["automatic"]==true),"debounce":request.get("debounce").cloned().unwrap_or(json!(0))});
    if let Some(origin) = request.get("origin") {
        if !matches!(
            origin.as_str(),
            Some("adopted" | "created" | "created_reconciled")
        ) {
            return Err(error("invalid_configuration"));
        }
        value["origin"] = origin.clone();
    }
    let changed = if let Some(old) = &old {
        let old_library = decode_json(string(old, "library")?)?;
        [
            "connection_id",
            "catalog",
            "subtree",
            "remote_root",
            "local_binding",
            "group_id",
        ]
        .iter()
        .any(|k| old[k] != value[k])
            || ["uuid", "roots", "type", "created_at"]
                .iter()
                .any(|k| old_library[k] != library[k])
    } else {
        false
    };
    if let Some(old) = &old
        && old["group_id"] == group
    {
        value["origin"] = old["origin"].clone();
    }
    if changed && request["rebind"] != true {
        return Err(error("explicit_rebind_required"));
    }
    for other in rows(
        &store.db,
        "SELECT remote_root FROM consumer_bindings WHERE profile=? AND group_id=? AND id!=? AND enabled=1",
        &[
            profile.to_owned().into(),
            group.clone().into(),
            id.to_owned().into(),
        ],
    )? {
        let other = string(&other, "remote_root")?;
        if within(&remote, other) || within(other, &remote) {
            return Err(error("overlapping_binding_roots"));
        }
    }
    if request["apply"] == true {
        store.transaction(|db|{
            db.execute("INSERT INTO consumer_deliveries(profile,id) VALUES (?,?) ON CONFLICT DO NOTHING",params![profile,group])?;
            let replace=old.as_ref().is_none_or(|old|changed||old["enabled"]!=1||old["automatic"]!=value["automatic"]||old["debounce"]!=value["debounce"]);
            if replace {
                db.execute("INSERT INTO consumer_bindings(profile,id,connection_id,catalog,subtree,remote_root,local_binding,library,group_id,origin,automatic,debounce) VALUES (?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(profile,id) DO UPDATE SET connection_id=excluded.connection_id,catalog=excluded.catalog,subtree=excluded.subtree,remote_root=excluded.remote_root,local_binding=excluded.local_binding,library=excluded.library,group_id=excluded.group_id,origin=excluded.origin,automatic=excluded.automatic,debounce=excluded.debounce,enabled=1,revision=consumer_bindings.revision+1,error=NULL,indexing=NULL",params![profile,id,connection_id,catalog,subtree,remote,encode(&local),encode(library),group,string(&value,"origin")?,number(&value,"automatic")?,number(&value,"debounce")?])?;
                if changed { db.execute("UPDATE consumer_bindings SET generation=0,verified=0,acknowledged=0 WHERE profile=? AND id=?",params![profile,id])?; }
            }
            if request["initial_scan"]==true&&(old.is_none()||changed) { db.execute("UPDATE consumer_bindings SET generation=generation+1 WHERE profile=? AND id=?",params![profile,id])?; }
            Ok(())
        })?;
        if request["initial_scan"] == true {
            let saved = binding(store, profile, id)?;
            let snapshot = binding_snapshot(store, profile, std::slice::from_ref(&saved))?;
            let outcome = crate::filesystem_mutation::published_mut(
                store, profile, catalog, &mut value, false, probe,
            );
            require_bindings(store, profile, &snapshot)?;
            outcome?;
        }
    }
    value["library"] = library.clone();
    value["local_binding"] = local;
    Ok(
        json!({"binding":value,"applied":request["apply"]==true,"path_evidence":"declared mapping and server-reported root; remote symlink target readability unverified","complete":true}),
    )
}
fn group_bindings(store: &Store, profile: &str, group: &str) -> Result<Vec<Value>> {
    rows(
        &store.db,
        "SELECT * FROM consumer_bindings WHERE profile=? AND group_id=? AND enabled=1 ORDER BY id",
        &[profile.to_owned().into(), group.to_owned().into()],
    )
}
// Only publication verification bookkeeping may change while a caller marks a
// healthy generation verified. Revisions, generations, due times and connection
// identities must remain exactly those admitted before detached probe work.
fn binding_snapshot(store: &Store, profile: &str, bindings: &[Value]) -> Result<Vec<Value>> {
    bindings
        .iter()
        .map(|row| {
            let mut value = row.clone();
            for key in ["verified", "verified_at", "error", "indexing"] {
                value
                    .as_object_mut()
                    .ok_or_else(|| error("invalid_configuration"))?
                    .remove(key);
            }
            value["connection_revision"] =
                connection(store, profile, string(row, "connection_id")?)?["revision"].clone();
            Ok(value)
        })
        .collect()
}
fn require_bindings(store: &Store, profile: &str, expected: &[Value]) -> Result<()> {
    let current = expected
        .iter()
        .map(|row| binding(store, profile, string(row, "id")?))
        .collect::<Result<Vec<_>>>()?;
    if binding_snapshot(store, profile, &current)? != expected {
        return Err(error("binding_changed"));
    }
    for row in &current {
        let local = crate::observation::binding(store, profile, "output", string(row, "catalog")?)
            .map_err(|_| error("publication_unhealthy"))?;
        if encode(&local) != string(row, "local_binding")? {
            return Err(error("output_binding_changed"));
        }
        crate::observation::checked_root(store, profile, &local)
            .map_err(|_| error("publication_unhealthy"))?;
    }
    Ok(())
}
fn unchanged_group(
    store: &Store,
    profile: &str,
    expected: &Value,
    bindings: &[Value],
) -> Result<bool> {
    let current = rows(
        &store.db,
        "SELECT * FROM consumer_deliveries WHERE profile=? AND id=?",
        &[
            profile.to_owned().into(),
            string(expected, "id")?.to_owned().into(),
        ],
    )?;
    if current.first() != Some(expected) {
        return Ok(false);
    }
    let current = group_bindings(store, profile, string(expected, "id")?)?;
    Ok(binding_snapshot(store, profile, &current)? == bindings)
}
pub fn local_valid(
    store: &mut Store,
    profile: &str,
    bindings: &[Value],
    probe: &mut Probe<'_>,
) -> Result<BTreeMap<String, Value>> {
    let snapshot = binding_snapshot(store, profile, bindings)?;
    require_bindings(store, profile, &snapshot)?;
    let catalogs = bindings
        .iter()
        .map(|row| string(row, "catalog").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    // The epoch covers the complete multi-catalog proof, not just the most
    // recent probe. A later catalog's probe may invalidate an earlier one.
    let epoch = crate::fallback_resolution::epoch(store)?;
    let mut validations = BTreeMap::new();
    for catalog in catalogs {
        let report = crate::filesystem_mutation::verify_mut(store, profile, &catalog, probe);
        require_bindings(store, profile, &snapshot)?;
        crate::fallback_resolution::require_epoch(store, epoch)
            .map_err(|_| error("publication_changed"))?;
        let report = report.map_err(|_| error("publication_unhealthy"))?;
        if report["healthy"] != true {
            return Err(error("publication_unhealthy"));
        }
        validations.insert(catalog, report);
    }
    Ok(validations)
}
/// Immutable claim data contains reference names only, never credential values.
#[derive(Clone)]
pub struct Claim {
    pub group_id: String,
    pub token: String,
    pub attempt_id: i64,
    pub snapshot: Vec<Value>,
    pub connection: Value,
    pub library: Value,
}
pub fn claim(
    store: &mut Store,
    profile: &str,
    automatic: bool,
    probe: &mut Probe<'_>,
) -> Result<Option<Claim>> {
    let at = now();
    let groups = rows(
        &store.db,
        "SELECT d.* FROM consumer_deliveries d WHERE d.profile=? AND d.state IN ('pending','accepted','retry','leased','busy') AND d.due_at<=? AND (d.lease_until IS NULL OR d.lease_until<=?) AND EXISTS (SELECT 1 FROM consumer_bindings b WHERE b.profile=d.profile AND b.group_id=d.id AND b.enabled=1 AND b.generation>b.acknowledged) ORDER BY d.due_at,d.id LIMIT 100",
        &[profile.to_owned().into(), at.into(), at.into()],
    )?;
    for group in groups {
        let id = string(&group, "id")?;
        let bindings = group_bindings(store, profile, id)?;
        let pending: Vec<_> = bindings
            .iter()
            .filter(|r| r["generation"].as_i64() > r["acknowledged"].as_i64())
            .collect();
        if pending.is_empty()
            || (automatic && !pending.iter().any(|r| r["automatic"] == 1))
            || bindings
                .iter()
                .any(|r| r["due_at"].as_f64().unwrap_or(f64::INFINITY) > at)
        {
            continue;
        }
        let captured = binding_snapshot(store, profile, &bindings)?;
        let validate = (|| {
            let evidence: Vec<_> = bindings
                .iter()
                .map(|r| {
                    decode_json(string(r, "library")?)
                        .map(|v| library_identity(&v))
                        .map_err(Error::from)
                })
                .collect::<Result<_>>()?;
            if evidence.iter().any(|v| Some(v) != evidence.first()) {
                return Err(error("library_binding_evidence_mismatch"));
            }
            local_valid(store, profile, &bindings, probe)
        })();
        // A competing worker may have claimed, delivered or reconfigured the
        // group while a probe held no Store. Do not touch its lease or error.
        if !unchanged_group(store, profile, &group, &captured)? {
            continue;
        }
        let validations = match validate {
            Ok(v) => v,
            Err(e) => {
                let failure = Failure::new(match e.0.as_str() {
                    "library_binding_evidence_mismatch" => "library_binding_evidence_mismatch",
                    "output_binding_changed" => "output_binding_changed",
                    "publication_changed" => "publication_changed",
                    _ => "publication_unhealthy",
                });
                store.transaction(|db|{db.execute("UPDATE consumer_deliveries SET state=CASE WHEN ? THEN 'repair' ELSE state END,error=?,due_at=? WHERE profile=? AND id=?",params![!failure.retryable(),failure.code,now()+30.0,profile,id])?;Ok(())})?;
                continue;
            }
        };
        if number(&group, "attempts")? >= MAX_ATTEMPTS {
            store.transaction(|db|{db.execute("UPDATE consumer_deliveries SET state='exhausted',lease_token=NULL,lease_until=NULL WHERE profile=? AND id=?",params![profile,id])?;Ok(())})?;
            continue;
        }
        let mut unchanged = true;
        for catalog in validations.keys() {
            let result = crate::filesystem_mutation::published_mut(
                store,
                profile,
                catalog,
                &mut json!({}),
                false,
                probe,
            );
            if !unchanged_group(store, profile, &group, &captured)? {
                unchanged = false;
                break;
            }
            result?;
        }
        if !unchanged {
            continue;
        }
        let mut snapshot = Vec::new();
        for b in &bindings {
            snapshot.push(json!({"id":b["id"],"revision":b["revision"],"generation":b["generation"],"publication_validation":validations[string(b,"catalog")?],"connection_revision":connection(store,profile,string(b,"connection_id")?)?["revision"]}));
        }
        let token = uuid::Uuid::new_v4().to_string();
        let attempt_id=store.transaction(|db|{
            if !unchanged_group(store, profile, &group, &captured)? {
                return Ok(None);
            }
            let at = now();
            let acquired = db.execute("UPDATE consumer_deliveries SET state='leased',lease_token=?,lease_until=?,attempts=attempts+1 WHERE profile=? AND id=? AND state IN ('pending','accepted','retry','leased','busy') AND due_at<=? AND (lease_until IS NULL OR lease_until<=?)",params![token,at+LEASE_SECONDS,profile,id,at,at])?;
            if acquired != 1 { return Ok(None); }
            db.execute("UPDATE consumer_attempts SET state='lease_expired',finished_at=CURRENT_TIMESTAMP WHERE profile=? AND group_id=? AND state='leased'",params![profile,id])?;
            db.execute("INSERT INTO consumer_attempts(profile,group_id,token,snapshot,state) VALUES (?,?,?,?,'leased')",params![profile,id,token,encode(&json!(snapshot))])?;Ok(Some(db.last_insert_rowid()))
        })?;
        let Some(attempt_id) = attempt_id else {
            continue;
        };
        return Ok(Some(Claim {
            group_id: id.to_owned(),
            token,
            attempt_id,
            snapshot,
            connection: connection(store, profile, string(&bindings[0], "connection_id")?)?,
            library: decode_json(string(&bindings[0], "library")?)?,
        }));
    }
    Ok(None)
}
pub fn fence(
    store: &Store,
    profile: &str,
    claimed: &Claim,
    before_send: bool,
) -> Result<(Value, Vec<Value>)> {
    let group=rows(&store.db,"SELECT * FROM consumer_deliveries WHERE profile=? AND id=? AND lease_token=? AND lease_until>? AND state='leased'",&[profile.to_owned().into(),claimed.group_id.clone().into(),claimed.token.clone().into(),now().into()])?.into_iter().next().ok_or_else(||error("stale_claim"))?;
    let current = group_bindings(store, profile, &claimed.group_id)?;
    if current.len() != claimed.snapshot.len() || current.is_empty() {
        return Err(error("binding_changed"));
    }
    for (old, row) in claimed.snapshot.iter().zip(&current) {
        if row["id"] != old["id"]
            || row["revision"] != old["revision"]
            || connection(store, profile, string(row, "connection_id")?)?["revision"]
                != old["connection_revision"]
        {
            return Err(error("binding_changed"));
        }
        if before_send && row["generation"] != old["generation"] {
            return Err(error("publication_changed"));
        }
    }
    if connection(store, profile, string(&current[0], "connection_id")?)?["revision"]
        != claimed.connection["revision"]
    {
        return Err(error("binding_changed"));
    }
    Ok((group, current))
}
pub fn before_send(
    store: &mut Store,
    profile: &str,
    claim: &Claim,
    probe: &mut Probe<'_>,
) -> Result<()> {
    let (_, current) = fence(store, profile, claim, true)?;
    let result = local_valid(store, profile, &current, probe);
    // Fence even failed verification: expiry/rebinding during detached work
    // must not be reported or acknowledged as this worker's current attempt.
    fence(store, profile, claim, true)?;
    result?;
    Ok(())
}
pub fn acknowledge(
    store: &Store,
    profile: &str,
    claimed: &Claim,
    failure: Option<&Failure>,
) -> Result<Value> {
    let group = match fence(store, profile, claimed, false) {
        Ok((group, _)) => group,
        Err(_) => {
            store.transaction(|db|{
            db.execute("UPDATE consumer_attempts SET state='stale_acknowledgement',finished_at=CURRENT_TIMESTAMP WHERE id=? AND token=?",params![claimed.attempt_id,claimed.token])?;
            db.execute("UPDATE consumer_deliveries SET state='pending',lease_token=NULL,lease_until=NULL WHERE profile=? AND id=? AND lease_token=?",params![profile,claimed.group_id,claimed.token])?;Ok(())
        })?;
            return Ok(json!({"state":"stale_acknowledgement","complete":false}));
        }
    };
    let attempts = number(&group, "attempts")?;
    let state = if let Some(e) = failure {
        if e.code == "publication_changed" {
            "pending"
        } else if e.code == "busy" {
            "busy"
        } else if e.retryable() && attempts < MAX_ATTEMPTS {
            "retry"
        } else if e.retryable() {
            "exhausted"
        } else {
            "repair"
        }
    } else {
        "accepted"
    };
    let due = if let Some(e) = failure {
        now()
            + if e.code == "busy" {
                30.0
            } else if e.code == "publication_changed" {
                1.0
            } else {
                e.retry_after
                    .max((5.0 * 2f64.powi(attempts.clamp(0, 10) as i32)).min(3600.0))
            }
    } else {
        0.0
    };
    store.transaction(|db|{
        if failure.is_none() {
            for row in &claimed.snapshot { db.execute("UPDATE consumer_bindings SET acknowledged=max(acknowledged,?),error=NULL WHERE profile=? AND id=? AND revision=?",params![number(row,"generation")?,profile,string(row,"id")?,number(row,"revision")?])?; }
            crate::notifications::emit(db,profile,"scan_requested","info",&claimed.group_id,&json!(format!("scan:{}",claimed.token)))?;
        }else if matches!(state,"repair"|"exhausted") {
            crate::notifications::emit(db,profile,if state=="repair"{"consumer_needs_repair"}else{"scan_failed"},"error",&claimed.group_id,&json!(format!("{state}:{}",claimed.token)))?;
        }
        if failure.is_some_and(|e|matches!(e.code.as_str(),"busy"|"publication_changed")) {db.execute("UPDATE consumer_deliveries SET attempts=max(0,attempts-1) WHERE profile=? AND id=?",params![profile,claimed.group_id])?;}
        db.execute("UPDATE consumer_deliveries SET state=?,due_at=?,lease_token=NULL,lease_until=NULL,error=?,accepted_at=CASE WHEN ?='accepted' THEN CURRENT_TIMESTAMP ELSE accepted_at END,attempts=CASE WHEN ?='accepted' THEN 0 ELSE attempts END WHERE profile=? AND id=?",params![state,due,failure.map(|e|e.code.as_str()),state,state,profile,claimed.group_id])?;
        db.execute("UPDATE consumer_attempts SET state=?,error=?,finished_at=CURRENT_TIMESTAMP WHERE id=?",params![state,failure.map(|e|e.code.as_str()),claimed.attempt_id])?;Ok(())
    })?;
    Ok(
        json!({"state":state,"complete":failure.is_none(),"error":failure.map(Failure::report),"scan_request_accepted":failure.is_none(),"indexing_verified":false}),
    )
}
pub fn status(store: &Store, profile: &str, limit: usize) -> Result<Value> {
    if !(1..=1000).contains(&limit) {
        return Err(error("invalid_configuration"));
    }
    let mut bindings = rows(
        &store.db,
        "SELECT b.*,d.state AS delivery_state,d.attempts,d.error AS delivery_error,d.lease_until,d.accepted_at FROM consumer_bindings b JOIN consumer_deliveries d ON d.profile=b.profile AND d.id=b.group_id WHERE b.profile=? ORDER BY b.id LIMIT ?",
        &[profile.to_owned().into(), ((limit + 1) as i64).into()],
    )?;
    let truncated = bindings.len() > limit;
    bindings.truncate(limit);
    for row in &mut bindings {
        for key in ["library", "local_binding", "indexing"] {
            if let Some(s) = row[key].as_str() {
                row[key] = decode_json(s)?;
            }
        }
        row["state"] = json!(if row["enabled"] != 1 {
            "disabled"
        } else if number(row, "generation")? > number(row, "verified")? {
            "unverified"
        } else if number(row, "generation")? > number(row, "acknowledged")? {
            string(row, "delivery_state")?
        } else if number(row, "acknowledged")? > 0 {
            "accepted"
        } else {
            "idle"
        });
    }
    let unresolved:i64=store.db.query_row("SELECT count(*) FROM consumer_bindings WHERE profile=? AND enabled=1 AND generation>acknowledged",[profile],|r|r.get(0))?;
    Ok(
        json!({"bindings":bindings,"truncated":truncated,"unresolved_bindings":unresolved,"complete":unresolved==0}),
    )
}
pub fn execute(store: &Store, profile: &str, operation: &str, request: &Value) -> Result<Value> {
    if matches!(operation, "bindings" | "status") {
        return status(store, profile, limit(request, 100, 1000)?);
    }
    let table = match operation {
        "connections" => Some("consumer_connections"),
        "attempts" => Some("consumer_attempts"),
        "events" => Some("consumer_events"),
        "creations" => Some("consumer_creations"),
        "publications" => Some("publication_generations"),
        _ => None,
    };
    if let Some(table) = table {
        let limit = limit(request, 100, 1000)?;
        let mut items = rows(
            &store.db,
            &format!("SELECT * FROM {table} WHERE profile=? ORDER BY rowid DESC LIMIT ?"),
            &[profile.to_owned().into(), ((limit + 1) as i64).into()],
        )?;
        let truncated = items.len() > limit;
        items.truncate(limit);
        return Ok(json!({operation:items,"truncated":truncated,"listing_complete":!truncated}));
    }
    if !matches!(operation, "enable" | "disable" | "retry") {
        return Err(error("unknown consumer operation"));
    }
    let id = string(request, "id")?;
    let binding = binding(store, profile, id)?;
    if (operation == "enable" && binding["enabled"] == 1)
        || (operation == "disable" && binding["enabled"] == 0)
    {
        return Ok(
            json!({"id":id,"operation":operation,"complete":true,"remote_library_deleted":false}),
        );
    }
    store.transaction(|db|{
        if operation=="retry" && db.prepare("SELECT 1 FROM consumer_deliveries WHERE profile=? AND id=? AND state='leased' AND lease_until>?")?.exists(params![profile,string(&binding,"group_id")?,now()])? {
            return Err(error("consumer delivery has an active lease; wait for completion or expiry before retrying"));
        }
        if matches!(operation,"enable"|"disable") {db.execute("UPDATE consumer_bindings SET generation=generation+CASE WHEN enabled=0 THEN 1 ELSE 0 END,enabled=?,revision=revision+1 WHERE profile=? AND id=?",params![operation=="enable",profile,id])?;}
        if operation=="retry" {db.execute("UPDATE consumer_bindings SET due_at=0 WHERE profile=? AND group_id=?",params![profile,string(&binding,"group_id")?])?;}
        db.execute("UPDATE consumer_deliveries SET state='pending',attempts=0,due_at=0,error=NULL,lease_token=NULL,lease_until=NULL WHERE profile=? AND id=?",params![profile,string(&binding,"group_id")?])?;
        db.execute("UPDATE consumer_deliveries SET state='cancelled' WHERE profile=? AND id=? AND NOT EXISTS (SELECT 1 FROM consumer_bindings b WHERE b.profile=consumer_deliveries.profile AND b.group_id=consumer_deliveries.id AND b.enabled=1)",params![profile,string(&binding,"group_id")?])?;Ok(())
    })?;
    Ok(json!({"id":id,"operation":operation,"complete":true,"remote_library_deleted":false}))
}

pub struct IndexingClaim {
    pub binding: Value,
    pub connection: Value,
    pub expected: Vec<String>,
}
pub fn indexing_claim(
    store: &mut Store,
    profile: &str,
    id: &str,
    limit: usize,
    probe: &mut Probe<'_>,
) -> Result<IndexingClaim> {
    if !(1..=1000).contains(&limit) {
        return Err(error("invalid_configuration"));
    }
    let binding = binding(store, profile, id)?;
    if binding["enabled"] != 1 {
        return Err(error("binding_disabled"));
    }
    local_valid(store, profile, std::slice::from_ref(&binding), probe)?;
    let connection = connection(store, profile, string(&binding, "connection_id")?)?;
    let subtree = binding["subtree"]
        .as_str()
        .ok_or_else(|| error("invalid_configuration"))?;
    // Effective fallback membership replaces exact mappings. Keep the limit
    // in SQL so indexing never materializes an unbounded catalog in memory.
    let catalog = string(&binding, "catalog")?;
    let logical = crate::fallback_projection::binding(store, profile, catalog)?.is_some();
    let source = if logical {
        "fallback_entries m JOIN files f ON f.id=m.file_id"
    } else {
        "mappings m"
    };
    let scope = if logical { "m.profile=? AND " } else { "" };
    let mut parameters = Vec::<rusqlite::types::Value>::new();
    if logical {
        parameters.push(profile.to_owned().into());
    }
    parameters.extend([
        catalog.to_owned().into(),
        subtree.to_owned().into(),
        subtree.to_owned().into(),
        subtree.to_owned().into(),
        subtree.to_owned().into(),
        ((limit + 1) as i64).into(),
    ]);
    let paths = rows(
        &store.db,
        &format!(
            "SELECT m.path AS path FROM {source} WHERE {scope}m.catalog=? AND m.active=1 AND m.path IS NOT NULL AND (?='' OR m.path=? OR substr(m.path,1,length(?)+1)=?||'/') ORDER BY m.path LIMIT ?"
        ),
        &parameters,
    )?;
    if paths.len() > limit {
        return Err(error("response_limit"));
    }
    let mut expected = Vec::new();
    for row in paths {
        let relative = path(string(&row, "path")?, true)?;
        if !within(&relative, subtree) {
            return Err(error("outside_binding_scope"));
        }
        let suffix = relative
            .strip_prefix(subtree)
            .ok_or_else(|| error("outside_binding_scope"))?
            .trim_start_matches('/');
        let remote = path(string(&binding, "remote_root")?, false)?;
        let root = remote.trim_end_matches('/');
        let translated = if suffix.is_empty() {
            remote
        } else {
            format!("{root}/{suffix}")
        };
        // Frozen translation validates each input at 4096 characters, not the
        // composed output at that same bound. The sum is still bounded by
        // 8193 characters, and the checked subtree contributes no traversal.
        expected.push(translated);
    }
    Ok(IndexingClaim {
        binding,
        connection,
        expected,
    })
}
pub fn indexing_observed(
    store: &Store,
    profile: &str,
    claim: &IndexingClaim,
    report: &Value,
) -> Result<Value> {
    let id = string(&claim.binding, "id")?;
    store.transaction(|db| {
        let current = binding(store, profile, id)?;
        if current["revision"] != claim.binding["revision"]
            || current["generation"] != claim.binding["generation"]
            || connection(store, profile, string(&current, "connection_id")?)?["revision"]
                != claim.connection["revision"]
        {
            return Err(error("binding_changed"));
        }
        let mut stored = report.clone();
        stored["generation_observed"] = claim.binding["generation"].clone();
        stored["observed_at"] = json!(now());
        db.execute(
            "UPDATE consumer_bindings SET indexing=? WHERE profile=? AND id=?",
            params![encode(&stored), profile, id],
        )?;
        Ok(())
    })?;
    Ok(report.clone())
}

pub fn creation_spec(value: &Value) -> Result<Value> {
    let fields = ["name", "type", "root", "scanner", "agent", "language"];
    let object = value
        .as_object()
        .ok_or_else(|| error("invalid_creation_spec"))?;
    if object.len() != fields.len() || fields.iter().any(|k| !object.contains_key(*k)) {
        return Err(error("invalid_creation_spec"));
    }
    for v in object.values() {
        text(v, 4096)?;
    }
    let mut value = value.clone();
    value["root"] = json!(path(string(&value, "root")?, false)?);
    Ok(value)
}
pub fn creation_intent(store: &Store, profile: &str, id: &str) -> Result<Option<Value>> {
    Ok(rows(
        &store.db,
        "SELECT * FROM consumer_creations WHERE profile=? AND id=?",
        &[profile.to_owned().into(), id.to_owned().into()],
    )?
    .into_iter()
    .next())
}
/// An uncertain intent is committed before one remote POST is allowed. Existing
/// intents can only reconcile server evidence; they never authorize a replay.
pub fn creation_admit(
    store: &Store,
    profile: &str,
    request: &Value,
    observed_connection: &Value,
    libraries: &[Value],
    choices: Option<&Value>,
) -> Result<(Value, bool)> {
    let id = text(&request["id"], 255)?;
    let connection_id = string(request, "connection")?;
    let spec = creation_spec(&request["spec"])?;
    if connection(store, profile, connection_id)?["revision"] != observed_connection["revision"] {
        return Err(error("connection_changed"));
    }
    if let Some(intent) = creation_intent(store, profile, id)? {
        if intent["spec"] != encode(&spec)
            || intent["connection_id"] != connection_id
            || intent["connection_revision"] != observed_connection["revision"]
        {
            return Err(error("creation_intent_mismatch"));
        }
        if intent["state"] == "complete" {
            let library = decode_json(string(&intent, "library")?)?;
            let matches: Vec<_> = libraries
                .iter()
                .filter(|r| r["id"] == library["id"])
                .collect();
            if matches.len() != 1 || !same_library(matches[0], &library) {
                return Err(error("library_identity_changed"));
            }
            return Ok((
                json!({"library":library,"origin":"created","reused":true,"complete":true}),
                false,
            ));
        }
        let before = decode_json(string(&intent, "before_ids")?)?;
        let before = before
            .as_array()
            .ok_or_else(|| error("invalid_configuration"))?;
        let candidates: Vec<_> = libraries
            .iter()
            .filter(|r| {
                !before.contains(&r["id"])
                    && r["name"] == spec["name"]
                    && r["type"] == spec["type"]
                    && r["roots"] == json!([spec["root"]])
                    && r["scanner"] == spec["scanner"]
                    && r["agent"] == spec["agent"]
            })
            .collect();
        if candidates.len() != 1 {
            return Ok((
                json!({"complete":false,"state":"uncertain","error":Failure::new("creation_uncertain").report(),"candidates":candidates.len(),"retry_post":false}),
                false,
            ));
        }
        if request["apply"] != true && request["reconcile"] != true {
            return Ok((
                json!({"complete":true,"applied":false,"would_reconcile":candidates[0]}),
                false,
            ));
        }
        store.transaction(|db|{db.execute("UPDATE consumer_creations SET state='complete',library=?,error=NULL WHERE profile=? AND id=?",params![encode(candidates[0]),profile,id])?;Ok(())})?;
        return Ok((
            json!({"library":candidates[0],"origin":"created_reconciled","complete":true}),
            false,
        ));
    }
    let choices = choices.ok_or_else(|| error("connection_changed"))?;
    if !choices["scanners"]
        .as_array()
        .is_some_and(|v| v.contains(&spec["scanner"]))
        || !choices["agents"]
            .as_array()
            .is_some_and(|v| v.contains(&spec["agent"]))
    {
        return Err(error("unsupported_creation_settings"));
    }
    if libraries.iter().any(|r| {
        r["name"] == spec["name"]
            && r["roots"]
                .as_array()
                .is_some_and(|v| v.contains(&spec["root"]))
    }) {
        return Err(error("existing_library_requires_explicit_binding"));
    }
    if request["apply"] != true {
        return Ok((
            json!({"spec":spec,"connection_id":connection_id,"applied":false,"complete":true}),
            false,
        ));
    }
    store.transaction(|db|{
        if db.prepare("SELECT 1 FROM consumer_creations WHERE profile=? AND connection_id=? AND spec=?")?.exists(params![profile,connection_id,encode(&spec)])? {return Err(error("creation_intent_already_exists"));}
        db.execute("INSERT INTO consumer_creations(profile,id,connection_id,connection_revision,spec,before_ids,state) VALUES (?,?,?,?,?,?,'uncertain')",params![profile,id,connection_id,number(observed_connection,"revision")?,encode(&spec),encode(&json!(libraries.iter().map(|r|r["id"].clone()).collect::<Vec<_>>()))])?;Ok(())
    })?;
    Ok((
        json!({"state":"uncertain","complete":false,"retry_post":false}),
        true,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn path_scope_and_configuration_are_not_string_prefixes() {
        assert!(within("Movies/One.mkv", "Movies"));
        assert!(!within("Movies2/One.mkv", "Movies"));
        assert!(within("Movies/One.mkv", ""));
        assert!(!within("/Movies/One.mkv", ""));
        assert!(within("/Movies/One.mkv", "/"));
        assert!(!within("Movies/One.mkv", "/"));
        for value in ["../escape", "/absolute", "a//b", "a/../b", "a\\b"] {
            assert!(path(value, true).is_err());
        }
        assert_eq!(path("Movies/", true).unwrap(), "Movies");
        assert_eq!(path("/", false).unwrap(), "/");
        for value in [
            "http://user@example.test/",
            "http://example.test/?secret=one",
            "http://example.test/#fragment",
            "file:///tmp/path",
        ] {
            assert!(endpoint(value).is_err());
        }
    }
    #[test]
    fn failure_codes_expose_no_raw_diagnostics() {
        let failure = Failure::new("unauthorized");
        assert!(!failure.retryable());
        assert_eq!(
            failure.report(),
            json!({"code":"unauthorized","safe_to_retry":false,"message":"consumer unauthorized; inspect and repair configuration"})
        );
        for code in [
            "temporarily_failed",
            "rate_limited",
            "unavailable",
            "busy",
            "publication_unhealthy",
            "publication_changed",
        ] {
            assert!(Failure::new(code).retryable());
        }
        assert!(!Failure::new("invalid_response").retryable());
    }
}
