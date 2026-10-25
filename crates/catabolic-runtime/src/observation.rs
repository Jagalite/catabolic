// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Detached native observation owner; requester timeouts never cancel shared work.
use crate::helper_fence::{self, NativeFence};
use catabolic_store::{Error, Result, Store, native_observation_companion as companion};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

/// Versioned companion entrypoint. An older helper must reject this command;
/// native spawns never retry through the unfenced legacy command.
pub const NATIVE_WORKER_COMMAND: &str = "native-observation-worker-v1";
pub const CLOSED_WORKER_COMMAND: &str = "native-observation-worker-v2";
pub const SHARED_WORKER_COMMAND: &str = "native-observation-shared-worker-v1";
struct ClosedEnvelope {
    protocol_kind: String,
    protocol_version: u32,
    reservation_nonce: String,
    completion: companion::Record,
    result: Value,
    shared: Option<companion::SharedClaim>,
}
impl ClosedEnvelope {
    fn parse(value: Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| Error("closed observation acknowledgment must be an object".into()))?;
        if object.len() != 5 + usize::from(object.contains_key("shared_claim")) {
            return Err(Error(
                "closed observation acknowledgment fields mismatch".into(),
            ));
        }
        let protocol_kind = value["protocol_kind"]
            .as_str()
            .ok_or_else(|| Error("closed observation protocol kind missing".into()))?
            .to_owned();
        let protocol_version = value["protocol_version"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| Error("closed observation protocol version invalid".into()))?;
        let reservation_nonce = value["reservation_nonce"]
            .as_str()
            .ok_or_else(|| Error("closed observation nonce missing".into()))?
            .to_owned();
        let completion = serde_json::from_value(value["completion"].clone())?;
        let result = object
            .get("result")
            .ok_or_else(|| Error("closed observation result missing".into()))?
            .clone();
        let shared = object
            .get("shared_claim")
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()?;
        Ok(Self {
            protocol_kind,
            protocol_version,
            reservation_nonce,
            completion,
            result,
            shared,
        })
    }
}
// This type has no public constructor. Deserializing an acknowledgment is not
// proof: only finish() after successful wait on the retained Child creates it.
struct ExitedCompanion {
    record: companion::Record,
}

fn worker_command(native: bool) -> &'static str {
    if native {
        NATIVE_WORKER_COMMAND
    } else {
        "observation-worker"
    }
}
/// A started helper is returned even when reopening its parent catalog failed.
/// Register `worker` with the host before reporting `reconnect_error`; the live
/// helper and its admission permit must not be lost on that error path.
#[must_use]
pub struct SupervisedStart<P> {
    pub worker: SupervisedWorker<P>,
    pub reconnect_error: Option<Error>,
}

/// An isolated observation and the host's admission permit for its real lifetime.
///
/// A pending wait retains the permit. Neither cancellation of a caller's durable
/// demand nor a catalog reconnect failure releases it. Only a successful OS wait
/// proving helper exit does so, including when the helper result is an error.
/// This is an ownership adapter for the existing observation service, not a new
/// job owner: durable admission, shared demands and recovery stay in that service.
///
/// Explicit bounded waits are the supported shutdown path. Drop is a last-resort
/// blocking join; first close/release every catalog writer that the helper could
/// need. Drop never kills the helper or cancels another consumer's shared work.
/// On an OS wait failure, Drop deliberately leaks the permit rather than falsely
/// admitting replacement work. Process-crash recovery remains the host's job.
#[must_use]
pub struct SupervisedWorker<P> {
    child: Option<Child>,
    output: File,
    permit: Option<P>,
    pid: u32,
    path: PathBuf,
    database_id: String,
    native_fence: Option<NativeFence>,
    profile: String,
    source: String,
    requester: String,
    resumed_request: Option<String>,
    completed: Option<Result<Value>>,
    companion: Option<companion::Reservation>,
    shared: Option<companion::SharedClaim>,
    exited_companion: Option<ExitedCompanion>,
}

impl<P> SupervisedWorker<P> {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn requester(&self) -> &str {
        &self.requester
    }
    pub(crate) fn shared_claim(&self, store: &Store) -> Result<companion::SharedClaim> {
        self.check_store(store)?;
        if self.child.is_none() {
            return Err(Error(
                "shared join requires the original retained Child".into(),
            ));
        }
        let original = self
            .companion
            .as_ref()
            .ok_or_else(|| Error("shared join requires a closed protocol owner".into()))?;
        companion::capture_shared_claim(store, original, self.pid)
    }

    /// True only after this handle has observed and reaped the helper's exit.
    pub fn is_finished(&self) -> bool {
        self.child.is_none()
    }

    fn check_store(&self, store: &Store) -> Result<()> {
        if store.path != self.path || store.database_id != self.database_id {
            return Err(Error("observation worker catalog identity mismatch".into()));
        }
        if NativeFence::capture(store)? != self.native_fence {
            return Err(Error(
                "observation worker native lifetime identity mismatch".into(),
            ));
        }
        Ok(())
    }

    /// Look up the existing observation owner's durable demand, if admitted.
    pub fn request_id(&self, store: &Store) -> Result<Option<String>> {
        self.check_store(store)?;
        if let Some(id) = &self.resumed_request {
            return Ok(store
                .db
                .prepare("SELECT 1 FROM observation_requests WHERE id=? AND profile=?")?
                .exists([id, &self.profile])?
                .then(|| id.clone()));
        }
        catabolic_store::observation::request_id(
            store,
            &self.profile,
            &self.source,
            &self.requester,
        )
    }

    /// Wait with the catalog writer released so the existing owner can publish.
    /// `None` is pending and keeps this handle and its permit alive. The timeout
    /// bounds the helper wait; the existing Store reconnect can additionally wait
    /// up to five seconds for its writer lock. A reconnect error leaves this
    /// handle owned and drainable with `wait_without_store` after closing Store.
    pub fn wait(&mut self, store: &mut Store, timeout: Duration) -> Result<Option<Value>> {
        self.check_store(store)?;
        let result = if self.is_finished() {
            self.completed_result()
        } else {
            store.detached(|| self.wait_without_store(timeout))
        };
        if self.is_finished() && store.owns_writer_lock() {
            self.record_cleanup(store)?;
        }
        result
    }

    /// Bounded drain when the parent catalog is closed or already detached.
    /// The caller must not retain a writer lease needed by this helper. A timeout
    /// never transfers ownership to an untracked thread and never releases P.
    pub fn wait_without_store(&mut self, timeout: Duration) -> Result<Option<Value>> {
        if self.is_finished() {
            return self.completed_result();
        }
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.as_mut().unwrap().try_wait()? {
                self.finish(status);
                return self.completed_result();
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(None);
            }
            std::thread::sleep(remaining.min(Duration::from_millis(20)));
        }
    }

    fn completed_result(&self) -> Result<Option<Value>> {
        match self.completed.as_ref().unwrap() {
            Ok(value) => Ok(Some(value.clone())),
            Err(error) => Err(Error(error.0.clone())),
        }
    }

    fn finish(&mut self, status: ExitStatus) {
        self.child.take();
        self.permit.take();
        self.completed = Some((|| {
            self.output.seek(SeekFrom::Start(0))?;
            let mut data = String::new();
            (&mut self.output)
                .take(8 * 1024 * 1024 + 1)
                .read_to_string(&mut data)?;
            if data.len() > 8 * 1024 * 1024 {
                return Err(Error("observation helper output exceeded limit".into()));
            }
            if !status.success() {
                let message = data
                    .trim()
                    .strip_prefix("catabolic: ")
                    .unwrap_or(data.trim());
                return Err(Error(if message.is_empty() {
                    format!("observation helper exited with {status}")
                } else {
                    message.to_owned()
                }));
            }
            let value = catabolic_core::decode_json(&data)?;
            if let Some(reservation) = &self.companion {
                let envelope = ClosedEnvelope::parse(value)?;
                if envelope.protocol_kind != companion::PROTOCOL_KIND
                    || envelope.protocol_version != companion::PROTOCOL_VERSION
                    || envelope.reservation_nonce != reservation.reservation_nonce
                    || envelope.completion.reservation != *reservation
                    || envelope.completion.child_pid != Some(self.pid)
                    || envelope.completion.state != "completed"
                    || envelope.completion.cleanup_kind.is_some()
                    || envelope.completion.sealed_at.is_some()
                    || envelope.shared != self.shared
                    || self.shared.as_ref().is_some_and(|shared| {
                        envelope.completion.owned.is_some()
                            || envelope.completion.demand_job_id.as_deref()
                                != Some(shared.owned.job_id.as_str())
                    })
                {
                    return Err(Error("closed observation acknowledgment mismatch".into()));
                }
                self.exited_companion = Some(ExitedCompanion {
                    record: envelope.completion,
                });
                Ok(envelope.result)
            } else {
                Ok(value)
            }
        })());
    }
    /// Record cleanup after reconnecting the original catalog. This consumes only
    /// the private proof created by this owner's exact Child wait, never caller
    /// JSON/PID or terminal rows. Reconnect and seal errors retain the proof.
    pub fn record_cleanup(&mut self, store: &Store) -> Result<()> {
        self.check_store(store)?;
        let Some(proof) = self.exited_companion.as_ref() else {
            return Ok(());
        };
        if !store.owns_writer_lock() || !store.db.is_autocommit() {
            return Err(Error(
                "closed observation seal requires current writer".into(),
            ));
        }
        let record = &proof.record;
        let r = &record.reservation;
        store.transaction(|db| {
            if companion::read(store,&r.reservation_nonce)?.as_ref() != Some(record) || !companion::completion_matches(store,record)? {
                return Err(Error("stale closed observation completion".into()));
            }
            let changed=db.execute("UPDATE native_observation_companions SET state='quiescent',cleanup_kind='closed_child_exit_v1',sealed_at=CURRENT_TIMESTAMP WHERE reservation_nonce=? AND catalog_id=? AND incarnation=? AND protocol_kind='native_observation_no_child' AND protocol_version=1 AND helper_digest=? AND profile=? AND source=? AND requester=? AND args_digest=? AND state='completed' AND child_pid=? AND request_id=? AND demand_job_id=? AND owned_job_id IS ? AND owned_generation IS ? AND owned_claim_token_digest IS ? AND cleanup_kind IS NULL AND sealed_at IS NULL",params![r.reservation_nonce,r.catalog_id,r.incarnation,r.helper_digest,r.profile,r.source,r.requester,r.args_digest,self.pid,record.request_id,record.demand_job_id,record.owned.as_ref().map(|c| &c.job_id),record.owned.as_ref().map(|c| c.generation),record.owned.as_ref().map(|c| &c.claim_token_digest)])?;
            if changed != 1 { return Err(Error("closed observation seal lost ownership".into())); }
            Ok(())
        })?;
        self.exited_companion.take();
        Ok(())
    }
}

impl<P> Drop for SupervisedWorker<P> {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            loop {
                match child.wait() {
                    Ok(_) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        // Exit is unverified: releasing a resource permit would
                        // allow the caller to over-admit still-running work.
                        if let Some(permit) = self.permit.take() {
                            std::mem::forget(permit);
                        }
                        return;
                    }
                }
            }
        }
        self.permit.take();
    }
}

/// Start the existing isolated scan owner under an embedding-owned permit.
///
/// Unlike the legacy `execute` timeout path, this never creates an unowned
/// reaper. The host must retain the returned worker on both successful reconnect
/// and `reconnect_error`, and stop new admission while closing/draining workers.
/// Store reconnects before returning: a host returning pending must then detach
/// or close its catalog session so that the worker can continue between calls.
/// Reconnect only briefly for demand lookup/cancellation, then release it again.
pub fn start_supervised<P>(
    store: &mut Store,
    profile: &str,
    request: &Value,
    permit: P,
) -> Result<SupervisedStart<P>> {
    start_supervised_inner(store, profile, request, permit, None)
}
/// Prepare under the current writer, without creating a child. The host must
/// detach its session before consuming this private, nonserializable token.
pub(crate) fn prepare_supervised<P>(
    store: &mut Store,
    profile: &str,
    request: &Value,
    permit: P,
) -> Result<PreparedObservation<P>> {
    prepare_supervised_inner(store, profile, request, permit, None)
}
/// Borrow the actual original supervisor while preparing the exact shared claim.
pub(crate) fn prepare_shared_supervised<P, Q>(
    store: &mut Store,
    profile: &str,
    owner: &SupervisedWorker<Q>,
    permit: P,
) -> Result<PreparedObservation<P>> {
    if profile != owner.profile {
        return Err(Error("shared join profile mismatch".into()));
    }
    let shared = owner.shared_claim(store)?;
    prepare_supervised_inner(
        store,
        profile,
        &json!({"source":owner.source,"isolate":true}),
        permit,
        Some(shared),
    )
}

// The standalone API deliberately retains its documented reconnect contract.
// HostSession uses prepare -> session detach -> spawn instead, so it never
// competes with its own child during parent catalog validation.
fn start_supervised_inner<P>(
    store: &mut Store,
    profile: &str,
    request: &Value,
    permit: P,
    shared: Option<companion::SharedClaim>,
) -> Result<SupervisedStart<P>> {
    let prepared = prepare_supervised_inner(store, profile, request, permit, shared)?;
    let mut worker = None;
    let reconnect = store.detached(|| {
        worker = Some(prepared.spawn()?);
        Ok(())
    });
    match worker {
        Some(worker) => Ok(SupervisedStart {
            worker,
            reconnect_error: reconnect.err(),
        }),
        None => Err(reconnect
            .err()
            .unwrap_or_else(|| Error("observation helper did not start".into()))),
    }
}

#[must_use]
pub(crate) struct PreparedObservation<P> {
    binary: PathBuf,
    command: &'static str,
    encoded_request: String,
    output: File,
    stdout: File,
    reader: File,
    permit: P,
    path: PathBuf,
    database_id: String,
    native_fence: Option<NativeFence>,
    profile: String,
    source: String,
    requester: String,
    resumed_request: Option<String>,
    companion: Option<companion::Reservation>,
    shared: Option<companion::SharedClaim>,
}
impl<P> PreparedObservation<P> {
    #[cfg(test)]
    pub(crate) fn with_missing_executable_for_test(mut self, missing: PathBuf) -> Self {
        assert!(!missing.exists(), "fault path must be absent");
        self.binary = missing;
        self
    }
    // No fallible work follows successful spawn before the Child is owned by
    // SupervisedWorker. Dropping an unconsumed token proves no child was spawned,
    // but never fabricates a durable companion cleanup witness.
    pub(crate) fn spawn(self) -> Result<SupervisedWorker<P>> {
        let child = catabolic_core::spawn_guard::spawn(
            Command::new(self.binary)
                .arg("--db")
                .arg(&self.path)
                .args([
                    "--profile",
                    &self.profile,
                    self.command,
                    &self.encoded_request,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::from(self.stdout))
                .stderr(Stdio::from(self.output)),
        )?;
        let pid = child.id();
        Ok(SupervisedWorker {
            child: Some(child),
            output: self.reader,
            permit: Some(self.permit),
            pid,
            path: self.path,
            database_id: self.database_id,
            native_fence: self.native_fence,
            profile: self.profile,
            source: self.source,
            requester: self.requester,
            resumed_request: self.resumed_request,
            completed: None,
            companion: self.companion,
            shared: self.shared,
            exited_companion: None,
        })
    }
}
fn prepare_supervised_inner<P>(
    store: &mut Store,
    profile: &str,
    request: &Value,
    permit: P,
    shared: Option<companion::SharedClaim>,
) -> Result<PreparedObservation<P>> {
    if !request.is_object() {
        return Err(Error("observation request must be an object".into()));
    }
    if !store.writable || !store.owns_writer_lock() {
        return Err(Error("native scan requires an available writer".into()));
    }
    let closed = store.schema_version >= companion::SCHEMA_VERSION;
    let (binary, closed_digest) = if closed {
        let (path, digest) = if shared.is_some() {
            crate::helper::closed_shared_observation()?
        } else {
            crate::helper::closed_observation()?
        };
        (path, Some(digest))
    } else {
        (crate::helper::executable()?, None)
    };
    let mut child_request = request.clone();
    child_request
        .as_object_mut()
        .unwrap()
        .remove("_native_companion");
    child_request
        .as_object_mut()
        .unwrap()
        .remove("_native_shared_claim");
    if let Some(shared) = &shared {
        child_request["_native_shared_claim"] = serde_json::to_value(shared)?;
    }
    child_request["isolate"] = json!(false);
    // This is a private helper envelope field, always set by the trusted parent
    // rather than accepted from caller input. Path equality alone cannot fence a
    // catalog replaced between spawn and the helper's first open.
    child_request["_native_catalog_id"] = json!(store.database_id);
    let native_fence = helper_fence::write(store, &mut child_request)?;
    let command = if shared.is_some() {
        SHARED_WORKER_COMMAND
    } else if closed {
        CLOSED_WORKER_COMMAND
    } else {
        worker_command(native_fence.is_some())
    };
    if !child_request["requester"].is_string() {
        child_request["requester"] = json!(format!("native:{}", uuid::Uuid::new_v4()));
    }
    let requester = child_request["requester"].as_str().unwrap().to_owned();
    let resumed_request = request["request_id"].as_str().map(str::to_owned);
    let source = if let Some(id) = request
        .get("request_id")
        .or(request.get("continue_id"))
        .or(request.get("continue_request"))
        .and_then(Value::as_str)
    {
        store.db.query_row(
            "SELECT source FROM observation_requests WHERE id=? AND profile=?",
            [id, profile],
            |row| row.get::<_, String>(0),
        )?
    } else {
        request["source"]
            .as_str()
            .or(request["owner"].as_str())
            .unwrap_or("media")
            .to_owned()
    };
    // Allocate all fallible output descriptors before durable reservation.
    let output = tempfile::tempfile()?;
    let reader = output.try_clone()?;
    let stdout = output.try_clone()?;
    let companion = if let Some(digest) = closed_digest {
        let reservation =
            companion::reserve(store, profile, &source, &requester, &digest, &child_request)?;
        child_request["_native_companion"] = serde_json::to_value(&reservation)?;
        Some(reservation)
    } else {
        None
    };
    Ok(PreparedObservation {
        binary,
        command,
        encoded_request: catabolic_core::encode(&child_request),
        output,
        stdout,
        reader,
        permit,
        path: store.path.clone(),
        database_id: store.database_id.clone(),
        native_fence,
        profile: profile.to_owned(),
        source,
        requester,
        resumed_request,
        companion,
        shared,
    })
}

/// Closed schema37 entrypoint: only the built-in store operation is reachable.
/// It cannot accept a publisher callback or turn ordinary completion into exit.
pub fn worker_v2(path: &Path, profile: &str, request: &Value) -> Result<Value> {
    if request.get("_native_shared_claim").is_some() {
        return Err(Error(
            "shared observation requires its versioned command".into(),
        ));
    }
    closed_worker(path, profile, request, None)
}
/// Distinct pre-effect command fence: older closed helpers must reject this
/// before entering ordinary scan admission. No fallback is attempted.
pub fn shared_worker_v1(path: &Path, profile: &str, request: &Value) -> Result<Value> {
    let shared = serde_json::from_value(
        request
            .get("_native_shared_claim")
            .ok_or_else(|| Error("shared observation claim missing".into()))?
            .clone(),
    )?;
    closed_worker(path, profile, request, Some(shared))
}
fn closed_worker(
    path: &Path,
    profile: &str,
    request: &Value,
    shared: Option<companion::SharedClaim>,
) -> Result<Value> {
    helper_fence::require(request)?;
    let mut args = request.clone();
    let encoded = args
        .as_object_mut()
        .and_then(|a| a.remove("_native_companion"))
        .ok_or_else(|| Error("closed observation reservation missing".into()))?;
    let reservation: companion::Reservation = serde_json::from_value(encoded)?;
    if reservation.profile != profile {
        return Err(Error("closed observation profile mismatch".into()));
    }
    let digest = crate::helper::current_digest()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Store::open(path, true, false) {
            Ok(mut store) => {
                helper_fence::check(&store, request)?;
                let result = companion::execute_closed(&mut store, &reservation, &digest, &args)?;
                let completion = companion::read(&store, &reservation.reservation_nonce)?
                    .ok_or_else(|| Error("closed observation completion missing".into()))?;
                let mut envelope = json!({"protocol_kind":companion::PROTOCOL_KIND,"protocol_version":companion::PROTOCOL_VERSION,"reservation_nonce":reservation.reservation_nonce,"completion":completion,"result":result});
                if let Some(shared) = shared {
                    envelope["shared_claim"] = serde_json::to_value(shared)?;
                }
                return Ok(envelope);
            }
            Err(error)
                if error.0.contains("another Catabolic writer") && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(error) => return Err(error),
        }
    }
}

/// This route requires a native v1 fence even before opening the catalog.
pub fn worker_v1(path: &Path, profile: &str, request: &Value) -> Result<Value> {
    helper_fence::require(request)?;
    worker(path, profile, request)
}

pub fn worker(path: &Path, profile: &str, request: &Value) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Store::open(path, true, false) {
            Ok(mut store) => {
                helper_fence::check(&store, request)?;
                if store.schema_version >= companion::SCHEMA_VERSION {
                    return Err(Error(
                        "schema37 requires the closed observation worker protocol".into(),
                    ));
                }
                if let Some(expected) = request.get("_native_catalog_id")
                    && expected.as_str() != Some(store.database_id.as_str())
                {
                    return Err(Error("observation worker catalog identity mismatch".into()));
                }
                return catabolic_store::observation::execute_detached(
                    &mut store, profile, "scan", request,
                );
            }
            Err(error)
                if error.0.contains("another Catabolic writer") && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(error) => return Err(error),
        }
    }
}
pub fn execute(store: &mut Store, profile: &str, action: &str, request: &Value) -> Result<Value> {
    if action != "scan" || request["isolate"] != true {
        return catabolic_store::observation::execute_detached(store, profile, action, request);
    }
    if store.schema_version >= companion::SCHEMA_VERSION {
        return Err(Error(
            "schema37 isolated scans require a retained supervised owner".into(),
        ));
    }
    let binary = crate::helper::executable()?;

    let timeout = request
        .get("wait_seconds")
        .map(|v| {
            v.as_f64()
                .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 30.0)
                .ok_or_else(|| Error("invalid observation wait timeout".into()))
        })
        .transpose()?
        .unwrap_or(30.0);
    let mut child_request = request.clone();
    child_request["isolate"] = json!(false);
    let command = worker_command(helper_fence::write(store, &mut child_request)?.is_some());
    if !child_request["requester"].is_string() {
        child_request["requester"] = json!(format!("native:{}", uuid::Uuid::new_v4()));
    }
    let path = store.path.clone();
    let output = tempfile::tempfile()?;
    let mut reader = output.try_clone()?;
    let (mut child, status) = store.detached(|| {
        let mut child = catabolic_core::spawn_guard::spawn(
            Command::new(binary)
                .arg("--db")
                .arg(&path)
                .args([
                    "--profile",
                    profile,
                    command,
                    &catabolic_core::encode(&child_request),
                ])
                .stdout(Stdio::from(output.try_clone()?))
                .stderr(Stdio::from(output)),
        )?;
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok((child, Some(status)));
            }
            if started.elapsed() >= Duration::from_secs_f64(timeout) {
                return Ok((child, None));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    })?;
    if let Some(status) = status {
        reader.seek(SeekFrom::Start(0))?;
        let mut data = String::new();
        (&mut reader)
            .take(8 * 1024 * 1024)
            .read_to_string(&mut data)?;
        if !status.success() {
            return Err(Error(
                data.trim()
                    .strip_prefix("catabolic: ")
                    .unwrap_or(data.trim())
                    .to_string(),
            ));
        }
        return catabolic_core::decode_json(&data).map_err(Into::into);
    }
    // Reap opportunistically while this host remains alive; dropping a waiter
    // does not signal the independent worker, whose PID and guard are durable.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let existing = catabolic_store::observation::request_id(
        store,
        profile,
        child_request["source"].as_str().unwrap_or("media"),
        child_request["requester"].as_str().unwrap(),
    )?;
    let Some(existing) = existing else {
        return Err(Error("observation_pending".into()));
    };
    catabolic_store::observation::execute_detached(
        store,
        profile,
        "scan",
        &json!({"request_id":existing}),
    )
}
