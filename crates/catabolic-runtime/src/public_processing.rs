// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Public Processing orchestration over the native admission/publication owner.
//! Worker threads never own a catalog connection. Every blocking wait detaches
//! the caller's session; scoped claims are renewed and fenced before publication.
use crate::media;
use catabolic_store::{Error, Result, Store, media_mutation as storage, migration::rows};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

fn integer(args: &Value, key: &str, default: u64, min: u64, max: u64, error: &str) -> Result<u64> {
    let n = args.get(key).map_or(Some(default), Value::as_u64);
    n.filter(|n| (min..=max).contains(n))
        .ok_or_else(|| Error(error.into()))
}
fn ids(args: &Value, key: &str) -> Result<Option<Vec<String>>> {
    let Some(value) = args.get(key).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let error = || Error(format!("{key} must contain at most 1000 IDs"));
    let values = value
        .as_array()
        .filter(|a| a.len() <= 1000)
        .ok_or_else(error)?;
    values
        .iter()
        .map(|v| v.as_str().map(str::to_owned).ok_or_else(error))
        .collect::<Result<BTreeSet<_>>>()
        .map(|v| Some(v.into_iter().collect()))
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
fn object_or_empty(value: &Value) -> Result<Value> {
    if !truthy(value) {
        return Ok(json!({}));
    }
    if !value.is_object() || catabolic_core::encode(value).len() > 256 * 1024 {
        return Err(Error(
            "payload must be a JSON object of at most 256 KiB".into(),
        ));
    }
    Ok(value.clone())
}
fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error("processing cancelled".into()))
    } else {
        Ok(())
    }
}

pub fn execute(
    store: &mut Store,
    profile: &str,
    operation: &str,
    args: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    if !args.is_object() {
        return Err(Error("processing arguments must be an object".into()));
    }
    match operation {
        "enqueue" => enqueue(store, profile, args, cancel),
        "run" => run(store, profile, args, cancel),
        _ => Err(Error("unknown processing operation".into())),
    }
}

/// Select all requested file IDs before admitting any processing job. The same
/// catalog owner carries selection and enqueue, and the existing public owner
/// handles every batch. This mirrors the preserved CLI selection adapter.
pub fn enqueue_selection(
    store: Store,
    profile: &str,
    selection: &Value,
    args: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    if truthy(&args["file_ids"]) || truthy(&args["location"]) || truthy(&args["after"]) {
        return Err(Error(
            "selection cannot be combined with file IDs, location or after".into(),
        ));
    }
    if !selection.is_object() || selection.get("profile").unwrap_or(&json!("default")) != profile {
        return Err(Error(
            "selection profile must match the processing profile".into(),
        ));
    }
    cancelled(&cancel)?;
    let mut evaluation =
        catabolic_store::selection::Evaluation::new(store, profile, 60000, 100000, false)?;
    let selected = evaluation.select(selection)?;
    if selected.entity != "file_id" {
        return Err(Error("processing selection must return file_id".into()));
    }
    let mut store = evaluation.into_store();
    let mut result = json!({"queued":[],"cached":[],"errors":[],"complete":true,"next_after":null});
    let ids = selected.ids.into_iter().collect::<Vec<_>>();
    for batch in ids.chunks(1000) {
        cancelled(&cancel)?;
        let mut request =
            json!({"operation":args["operation"],"file_ids":batch,"refresh":args["refresh"]});
        request["options"] = if let Some(raw) = args["options_json"].as_str() {
            catabolic_core::decode_json(raw).map_err(|e| Error(e.to_string()))?
        } else {
            args.get("options").cloned().unwrap_or(json!({}))
        };
        let value = execute(&mut store, profile, "enqueue", &request, cancel.clone())?;
        for key in ["queued", "cached", "errors"] {
            result[key].as_array_mut().unwrap().extend(
                value[key]
                    .as_array()
                    .ok_or_else(|| Error("invalid processing batch result".into()))?
                    .iter()
                    .cloned(),
            );
        }
        result["complete"] = json!(result["complete"] == true && value["complete"] == true);
    }
    Ok(result)
}

fn enqueue(
    store: &mut Store,
    profile: &str,
    args: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    let limit = integer(
        args,
        "limit",
        100,
        1,
        1000,
        "limit must be between 1 and 1000",
    )? as usize;
    let location = args.get("location").filter(|v| !v.is_null());
    if let Some(location) = location {
        let location = location
            .as_str()
            .ok_or_else(|| Error("unknown source location".into()))?;
        if rows(
            &store.db,
            "SELECT id FROM locations WHERE id=?",
            &[location.to_owned().into()],
        )?
        .is_empty()
        {
            return Err(Error("unknown source location".into()));
        }
    }
    let operation = args["operation"]
        .as_str()
        .ok_or_else(|| Error("unsupported processing operation".into()))?;
    let options = object_or_empty(&args["options"])?;
    let external = matches!(operation, "probe" | "decode")
        || operation == "text" && matches!(options["backend"].as_str(), Some("pdf" | "ocr"));
    cancelled(&cancel)?;
    let config = if external {
        // This also rejects active caller transactions before launching a tool.
        store.detached(|| {
            crate::process::with_cancellation(Arc::clone(&cancel), || {
                media::operation_config(operation, &options)
            })
        })?
    } else {
        media::operation_config(operation, &options)?
    };
    cancelled(&cancel)?;
    let recipe = args
        .get("recipe_id")
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_str()
                .ok_or_else(|| Error("unknown immutable operation revision".into()))
        })
        .transpose()?;
    // Match the public validation order even when the selected file page is empty.
    if let Some(recipe) = recipe {
        let definition = storage::operation_get(store, recipe)?;
        if definition["operation_kind"] != "analysis"
            || definition["preset"] != operation
            || definition["definition"]["options"] != options
        {
            return Err(Error(
                "processing operation does not match its immutable definition".into(),
            ));
        }
    }
    let (files, next_after) = match ids(args, "file_ids")? {
        Some(files) => (files, Value::Null),
        None => {
            let after = catabolic_store::sql::scalar(
                args.get("after")
                    .filter(|v| truthy(v))
                    .unwrap_or(&json!("")),
            )?;
            let location = location.and_then(Value::as_str);
            let selected = rows(
                &store.db,
                "SELECT f.id FROM files f JOIN observations o ON o.file_id=f.id AND o.profile=? WHERE o.status='present' AND f.id>? AND (? IS NULL OR f.location=?) ORDER BY f.id LIMIT ?",
                &[
                    profile.to_owned().into(),
                    after,
                    location.map(str::to_owned).into(),
                    location.map(str::to_owned).into(),
                    ((limit + 1) as i64).into(),
                ],
            )?;
            let next = if selected.len() > limit {
                selected[limit - 1]["id"].clone()
            } else {
                Value::Null
            };
            (
                selected
                    .into_iter()
                    .take(limit)
                    .map(|r| r["id"].as_str().unwrap().to_owned())
                    .collect(),
                next,
            )
        }
    };
    if !files.is_empty() {
        if !store.writable {
            return Err(Error(
                "a read-only session cannot start a write transaction".into(),
            ));
        }
        if !store.owns_writer_lock() {
            return Err(Error(
                "catalog session unavailable after detached execution".into(),
            ));
        }
    }
    let mut result = media::enqueue_config(
        store,
        profile,
        operation,
        &files,
        &options,
        config,
        truthy(&args["refresh"]),
        recipe,
        Some(&cancel),
    )?;
    result["next_after"] = next_after;
    Ok(result)
}

struct Pending {
    claim: storage::Claim,
    started_at: String,
    physical: String,
    buckets: Vec<String>,
    thread: Option<JoinHandle<Result<WorkerOutcome>>>,
    cleanup: Option<crate::execution_cleanup::CompletedBuiltin>,
    capacity_reserved: bool,
}
impl Pending {
    fn join(&mut self) -> Result<Option<Value>> {
        let Some(thread) = self.thread.take() else {
            return Ok(None);
        };
        let WorkerOutcome { value, cleanup } = thread
            .join()
            .map_err(|_| Error("processing worker panicked".into()))??;
        self.cleanup = cleanup;
        Ok(Some(value))
    }
    fn release_capacity(&mut self, buckets: &mut BTreeMap<String, usize>) {
        if self.capacity_reserved {
            for bucket in &self.buckets {
                *buckets.get_mut(bucket).expect("owned capacity bucket") -= 1;
            }
            self.capacity_reserved = false;
        }
    }
}
struct Pool {
    cancel: Arc<AtomicBool>,
    pending: BTreeMap<String, Pending>,
}
impl Pool {
    fn stop_and_join(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        for task in self.pending.values_mut() {
            // Preserve evidence returned by the exact joined worker even if
            // cancellation discarded its channel notification or publication.
            let _ = task.join();
        }
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        if !self.pending.is_empty() {
            self.stop_and_join();
        }
    }
}
fn physical_key(job: &Value) -> String {
    let s = &job["snapshot"];
    let extension = if job["operation"] == "text" {
        let path = s["path"].as_str().unwrap_or("");
        Path::new(path)
            .extension()
            .map(|s| format!(".{}", s.to_string_lossy().to_lowercase()))
            .unwrap_or_default()
    } else {
        String::new()
    };
    catabolic_core::encode(&json!([
        job["operation"],
        job["options"],
        [
            s["device"],
            s["inode"],
            s["size"],
            s["mtime_ns"],
            s["ctime_ns"]
        ],
        extension
    ]))
}

fn renew_pending(store: &Store, pool: &Pool, last: &mut Instant) -> Result<()> {
    if last.elapsed() >= Duration::from_secs(5) {
        for task in pool.pending.values() {
            storage::heartbeat(store, &task.claim, media::now())?;
        }
        *last = Instant::now();
    }
    Ok(())
}

/// Read queued candidates in bounded pages, skipping live owners without
/// starving a later runnable job in the same source queue.
#[allow(clippy::too_many_arguments)]
fn queued_candidates(
    store: &Store,
    profile: &str,
    source: &str,
    scope: Option<&str>,
    limit: usize,
    cancel: &AtomicBool,
    pool: &Pool,
    last_heartbeat: &mut Instant,
) -> Result<Vec<String>> {
    let mut selected = Vec::new();
    let mut after_created = String::new();
    let mut after_id = String::new();
    loop {
        cancelled(cancel)?;
        renew_pending(store, pool, last_heartbeat)?;
        let scoped = if scope.is_some() {
            " AND id IN (SELECT value FROM json_each(?))"
        } else {
            ""
        };
        let mut params = vec![
            profile.to_owned().into(),
            source.to_owned().into(),
            after_created.clone().into(),
            after_created.clone().into(),
            after_id.clone().into(),
        ];
        if let Some(scope) = scope {
            params.push(scope.to_owned().into());
        }
        let page = rows(
            &store.db,
            &format!(
                "SELECT id,created_at FROM processing_jobs WHERE profile=? AND operation!='render' AND state='queued' AND location=? AND (created_at>? OR (created_at=? AND id>?)){scoped} ORDER BY created_at,id LIMIT 128"
            ),
            &params,
        )?;
        for row in &page {
            let id = row["id"].as_str().unwrap();
            if !storage::active(store, id, media::now())? {
                selected.push(id.to_owned());
                if selected.len() == limit {
                    return Ok(selected);
                }
            }
            after_created = row["created_at"].as_str().unwrap().to_owned();
            after_id = id.to_owned();
        }
        if page.len() < 128 {
            return Ok(selected);
        }
    }
}

#[cfg(test)]
type TestProcessor = dyn Fn(&Value, &AtomicBool) -> Value + Send + Sync;
enum Processor {
    Native,
    #[cfg(test)]
    CancelBuiltin,
    #[cfg(test)]
    PanicBuiltin,
    #[cfg(test)]
    SpawnFailure,
    #[cfg(test)]
    Injected(Arc<TestProcessor>),
}
impl Processor {
    fn process(&self, job: &Value, cancel: &AtomicBool) -> Value {
        match self {
            Self::Native => media::process(job, cancel),
            #[cfg(test)]
            Self::CancelBuiltin | Self::PanicBuiltin | Self::SpawnFailure => {
                media::process(job, cancel)
            }
            #[cfg(test)]
            Self::Injected(processor) => processor(job, cancel),
        }
    }
    fn cleanup(
        &self,
        store: &Store,
        claim: &storage::Claim,
        job: &Value,
    ) -> Result<Option<crate::execution_cleanup::OwnedBuiltin>> {
        match self {
            Self::Native => crate::execution_cleanup::OwnedBuiltin::capture(store, claim, job),
            #[cfg(test)]
            Self::CancelBuiltin | Self::PanicBuiltin | Self::SpawnFailure => {
                crate::execution_cleanup::OwnedBuiltin::capture(store, claim, job)
            }
            #[cfg(test)]
            Self::Injected(_) => Ok(None),
        }
    }
}
struct WorkerOutcome {
    value: Value,
    cleanup: Option<crate::execution_cleanup::CompletedBuiltin>,
}
fn run(store: &mut Store, profile: &str, args: &Value, cancel: Arc<AtomicBool>) -> Result<Value> {
    run_owned_processor(store, profile, args, cancel, Processor::Native)
}

#[cfg(test)]
fn run_with_processor<F>(
    store: &mut Store,
    profile: &str,
    args: &Value,
    cancel: Arc<AtomicBool>,
    processor: F,
) -> Result<Value>
where
    F: Fn(&Value, &AtomicBool) -> Value + Send + Sync + 'static,
{
    run_owned_processor(
        store,
        profile,
        args,
        cancel,
        Processor::Injected(Arc::new(processor)),
    )
}
fn run_owned_processor(
    store: &mut Store,
    profile: &str,
    args: &Value,
    cancel: Arc<AtomicBool>,
    processor: Processor,
) -> Result<Value> {
    let processor = Arc::new(processor);
    let job_ids = ids(args, "job_ids")?;
    let retries = integer(
        args,
        "retry_transient",
        0,
        0,
        10,
        "retry transient must be 0..10 additional attempts",
    )? as u32;
    let retry_delay = integer(
        args,
        "retry_delay",
        30,
        1,
        3600,
        "retry delay must be 1..3600 seconds",
    )? as u32;
    let worker_error = "workers must be 1..16; per_device must be 1..workers";
    let workers = integer(args, "workers", 2, 1, 16, worker_error)? as usize;
    let per_device = integer(args, "per_device", 1, 1, workers as u64, worker_error)? as usize;
    let limit = integer(
        args,
        "limit",
        100,
        1,
        1_000_000,
        "run limit must be 1..1000000",
    )? as usize;
    let groups = object_or_empty(&args["storage_groups"])?;
    if groups.as_object().unwrap().values().any(|v| {
        !v.as_str()
            .is_some_and(|s| !s.is_empty() && s.chars().count() <= 128)
    }) {
        return Err(Error(
            "storage groups must map source names to nonempty group names".into(),
        ));
    }
    if !store.owns_writer_lock() || !store.db.is_autocommit() {
        return Err(Error(
            "processing run requires a writer outside an active transaction".into(),
        ));
    }
    cancelled(&cancel)?;
    let retried = storage::prepare_run(
        store,
        profile,
        job_ids.as_deref(),
        media::now(),
        retries,
        limit,
    )?;
    let bindings = rows(
        &store.db,
        "SELECT owner,device FROM bindings WHERE profile=? AND kind='source' ORDER BY owner LIMIT 10001",
        &[profile.to_owned().into()],
    )?;
    if bindings.len() > 10000 {
        return Err(Error(
            "runner supports at most 10000 source bindings".into(),
        ));
    }
    let mut queues = bindings
        .into_iter()
        .map(|b| {
            let source = b["owner"].as_str().unwrap().to_owned();
            let mut buckets = vec![format!("device:{}", b["device"])];
            if let Some(group) = groups[&source].as_str() {
                buckets.push(format!("group:{group}"));
            }
            (source, buckets)
        })
        .collect::<VecDeque<_>>();
    let scope = if job_ids.is_some() {
        " AND id IN (SELECT value FROM json_each(?))"
    } else {
        ""
    };
    let scope_json = job_ids.as_ref().map(|v| catabolic_core::encode(&json!(v)));
    let (send, receive) = mpsc::channel::<String>();
    let mut pool = Pool {
        cancel: Arc::clone(&cancel),
        pending: BTreeMap::new(),
    };
    let mut buckets = BTreeMap::<String, usize>::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut cache = VecDeque::<(String, Value)>::new();
    let mut submitted = 0;
    let mut reused = 0;
    let mut last_heartbeat = Instant::now();
    let result = (|| -> Result<Value> {
        while !pool.pending.is_empty() || submitted < limit {
            cancelled(&cancel)?;
            let mut available = workers - pool.pending.len();
            if available > 0 && submitted < limit {
                for (source, device) in &queues {
                    let slots = device
                        .iter()
                        .map(|b| per_device.saturating_sub(*buckets.get(b).unwrap_or(&0)))
                        .min()
                        .unwrap_or(0);
                    if slots == 0 {
                        continue;
                    }
                    let selected = queued_candidates(
                        store,
                        profile,
                        source,
                        scope_json.as_deref(),
                        available.min(slots).min(limit - submitted),
                        &cancel,
                        &pool,
                        &mut last_heartbeat,
                    )?;
                    for id in selected {
                        cancelled(&cancel)?;
                        let job = storage::get_job(store, profile, &id)?;
                        let started_at = catabolic_store::mutation::timestamp()?;
                        let physical = physical_key(&job);
                        let claim = storage::start(store, profile, &id, media::now())?;
                        let cleanup = processor.cleanup(store, &claim, &job)?;
                        let cached = cache
                            .iter()
                            .find(|(key, _)| key == &physical)
                            .map(|(_, v)| v.clone());
                        if cached.is_some() {
                            reused += 1;
                        }
                        let sender = send.clone();
                        let worker_cancel = Arc::clone(&cancel);
                        let worker_id = id.clone();
                        let processor = Arc::clone(&processor);
                        // Record claim first so thread-spawn failure releases it too.
                        pool.pending.insert(
                            id.clone(),
                            Pending {
                                claim,
                                started_at,
                                physical,
                                buckets: device.clone(),
                                thread: None,
                                cleanup: None,
                                capacity_reserved: false,
                            },
                        );
                        // Builder::spawn consumes its closure on failure. Keep
                        // the unstarted plan in an exclusive slot so that only
                        // a definite no-thread result can recover its ownership.
                        let cleanup = Arc::new(std::sync::Mutex::new(cleanup));
                        let worker_cleanup = Arc::clone(&cleanup);
                        #[cfg(test)]
                        let spawn_failure = matches!(processor.as_ref(), Processor::SpawnFailure);
                        let work = move || {
                            let cleanup = worker_cleanup.lock().expect("owned cleanup plan").take();
                            let outcome =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    crate::process::with_cancellation(
                                        Arc::clone(&worker_cancel),
                                        || {
                                            if let Some(owned) = cleanup {
                                                #[cfg(test)]
                                                match processor.as_ref() {
                                                    Processor::CancelBuiltin => {
                                                        worker_cancel.store(true, Ordering::Relaxed)
                                                    }
                                                    Processor::PanicBuiltin => panic!(
                                                        "injected builtin panic before completion"
                                                    ),
                                                    _ => {}
                                                }
                                                let (value, completed) =
                                                    owned.execute(&job, &worker_cancel, cached);
                                                WorkerOutcome {
                                                    value,
                                                    cleanup: Some(completed),
                                                }
                                            } else {
                                                WorkerOutcome {
                                                    value: cached.unwrap_or_else(|| {
                                                        processor.process(&job, &worker_cancel)
                                                    }),
                                                    cleanup: None,
                                                }
                                            }
                                        },
                                    )
                                }))
                                .map_err(|_| Error("processing worker panicked".into()));
                            // The JoinHandle owns the outcome and cleanup
                            // proof. The channel is only a wake-up signal.
                            let _ = sender.send(worker_id);
                            outcome
                        };
                        let builder =
                            std::thread::Builder::new().name("catabolic-processing".into());
                        #[cfg(test)]
                        let spawned = if spawn_failure {
                            drop(work);
                            Err(std::io::Error::other("injected worker spawn failure"))
                        } else {
                            builder.spawn(work)
                        };
                        #[cfg(not(test))]
                        let spawned = builder.spawn(work);
                        let thread = match spawned {
                            Ok(thread) => thread,
                            Err(error) => {
                                if let Ok(mut plan) = cleanup.lock() {
                                    pool.pending.get_mut(&id).unwrap().cleanup = plan.take().map(
                                        crate::execution_cleanup::OwnedBuiltin::without_worker,
                                    );
                                }
                                return Err(error.into());
                            }
                        };
                        pool.pending.get_mut(&id).unwrap().thread = Some(thread);
                        for bucket in device {
                            *buckets.entry(bucket.clone()).or_default() += 1;
                        }
                        pool.pending.get_mut(&id).unwrap().capacity_reserved = true;
                        submitted += 1;
                        available -= 1;
                        if available == 0 || submitted == limit {
                            break;
                        }
                    }
                    if available == 0 || submitted == limit {
                        break;
                    }
                }
            }
            queues.rotate_left(usize::from(!queues.is_empty()));
            if pool.pending.is_empty() {
                break;
            }
            // No connection or process-wide writer lock is retained while waiting.
            let completed =
                store.detached(|| match receive.recv_timeout(Duration::from_millis(100)) {
                    Ok(v) => Ok(Some(v)),
                    Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        Err(Error("processing worker disconnected".into()))
                    }
                })?;
            cancelled(&cancel)?;
            renew_pending(store, &pool, &mut last_heartbeat)?;
            if let Some(id) = completed {
                let task = pool
                    .pending
                    .get_mut(&id)
                    .ok_or_else(|| Error("unknown processing worker".into()))?;
                let result = task
                    .join()?
                    .ok_or_else(|| Error("processing worker already joined".into()))?;
                let state = storage::complete(
                    store,
                    profile,
                    &task.claim,
                    &result,
                    media::now(),
                    retry_delay,
                    Some(&task.started_at),
                )?;
                if state == "complete" && catabolic_core::encode(&result).len() <= 65536 {
                    cache.retain(|(key, _)| key != &task.physical);
                    cache.push_back((task.physical.clone(), result));
                    if cache.len() > 64 {
                        cache.pop_front();
                    }
                }
                task.release_capacity(&mut buckets);
                *counts.entry(state).or_default() += 1;
                let mut task = pool.pending.remove(&id).unwrap();
                if let Some(cleanup) = task.cleanup.take() {
                    cleanup.release_capacity().seal(store)?;
                }
            }
        }
        let mut params = vec![profile.to_owned().into()];
        if let Some(scope) = &scope_json {
            params.push(scope.clone().into());
        }
        let remaining = rows(&store.db, &format!("SELECT count(*) AS count FROM processing_jobs WHERE profile=? AND state='queued'{scope}"), &params)?[0]["count"].clone();
        Ok(
            json!({"processed":counts.values().sum::<usize>(),"retried":retried,"reused_physical_results":reused,"counts":counts,"complete":counts.keys().all(|k| k == "complete"),"remaining":remaining}),
        )
    })();
    if result.is_err() {
        // Cancellation, detached reopen failures and stale publication fences all
        // stop and reap every worker before this invocation returns.
        cancel.store(true, Ordering::Relaxed);
        let _ = store.detached(|| {
            pool.stop_and_join();
            Ok(())
        });
        pool.stop_and_join();
        // Only joined builtin workers carry a proof. Drop each Pending and
        // return its exact buckets before sealing; lost joins and external
        // operations remain unproven regardless of their database state.
        for (_, mut task) in std::mem::take(&mut pool.pending) {
            task.release_capacity(&mut buckets);
            if store.owns_writer_lock() {
                if let Some(cleanup) = task.cleanup.take() {
                    // A failed seal stays fail-closed in the durable admission
                    // guard. Preserve the original operation/cancellation error.
                    let _ = cleanup.release_capacity().seal(store);
                } else {
                    let _ = storage::release(store, &task.claim);
                }
            }
        }
    }
    let report = result?;
    if args.get("_refresh").is_none_or(truthy) {
        crate::catalog_refresh::finish(store, profile, report)
    } else {
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use catabolic_store::migration;
    use std::{
        fs,
        os::unix::fs::MetadataExt,
        sync::{Mutex, atomic::AtomicUsize},
    };

    fn detached_writer(path: &Path) -> Store {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match Store::open(path, true, false) {
                Ok(store) => return store,
                Err(error)
                    if error.0.contains("another Catabolic writer")
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("detached writer: {error}"),
            }
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        root: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            Self::with_schema(32)
        }
        fn with_schema(schema: u32) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("catalog.db");
            let root = dir.path().join("source");
            fs::create_dir(&root).unwrap();
            if schema == 32 {
                migration::initialize(&path).unwrap();
            } else {
                migration::initialize_native_at(&path, schema).unwrap();
            }
            let store = Store::open(&path, true, false).unwrap();
            let r = root.metadata().unwrap();
            store.transaction(|db| {
                db.execute("INSERT INTO locations VALUES ('source')", [])?;
                db.execute("INSERT INTO bindings VALUES ('default','source','source',?,?,?)", rusqlite::params![root.to_string_lossy(),r.dev() as i64,r.ino() as i64])?;
                db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','source',1,1,'[]')", [])?;
                Ok(())
            }).unwrap();
            Self {
                _dir: dir,
                store,
                root,
            }
        }
        fn file(&self, id: &str, hardlink: Option<&str>) {
            let path = self.root.join(format!("{id}.txt"));
            if let Some(other) = hardlink {
                fs::hard_link(self.root.join(format!("{other}.txt")), &path).unwrap();
            } else {
                fs::write(&path, format!("text for {id}\n")).unwrap();
            }
            let s = path.metadata().unwrap();
            self.store
                .transaction(|db| {
                    db.execute(
                        "INSERT INTO files VALUES (?,'source',?)",
                        rusqlite::params![id, format!("{id}.txt")],
                    )?;
                    db.execute(
                        "INSERT INTO observations VALUES ('default',?,?,?,?,?,'present','scan')",
                        rusqlite::params![
                            id,
                            s.size() as i64,
                            s.mtime() * 1_000_000_000 + s.mtime_nsec(),
                            s.dev() as i64,
                            s.ino() as i64
                        ],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        fn call(&mut self, action: &str, args: Value) -> Value {
            execute(
                &mut self.store,
                "default",
                action,
                &args,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap()
        }
        fn enqueue(&mut self) -> Vec<String> {
            self.call("enqueue", json!({"operation":"hash"}))["queued"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().into())
                .collect()
        }
    }

    #[test]
    fn native_cancel_drain_preserves_exact_joined_builtin_cleanup() {
        let mut f = Fixture::with_schema(36);
        f.file("a", None);
        let jobs = f.enqueue();
        let error = run_owned_processor(
            &mut f.store,
            "default",
            &json!({"_refresh":false,"workers":1,"per_device":1}),
            Arc::new(AtomicBool::new(false)),
            Processor::CancelBuiltin,
        )
        .unwrap_err();
        assert!(error.0.contains("processing cancelled"));
        assert_eq!(f.store.db.query_row("SELECT count(*) FROM native_execution_lifetimes WHERE state='quiescent' AND capacity_released=1", [], |r|r.get::<_,i64>(0)).unwrap(), 1);
        assert_eq!(
            f.store
                .db
                .query_row(
                    "SELECT lease_until FROM execution_claims WHERE job_id=?",
                    [&jobs[0]],
                    |r| r.get::<_, f64>(0)
                )
                .unwrap(),
            0.0
        );
        // The abandoned running row can be recovered only because its exact
        // owned worker joined and sealed, even though this host PID is live.
        let result = f.call("run", json!({"_refresh":false,"workers":1,"per_device":1}));
        assert_eq!(result["processed"], 1);
        assert_eq!(
            f.store
                .db
                .query_row(
                    "SELECT generation FROM execution_claims WHERE job_id=?",
                    [&jobs[0]],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
    }

    #[test]
    fn native_spawn_failure_seals_only_the_exact_never_started_builtin() {
        let mut f = Fixture::with_schema(36);
        f.file("a", None);
        let jobs = f.enqueue();
        assert!(
            run_owned_processor(
                &mut f.store,
                "default",
                &json!({"_refresh":false}),
                Arc::new(AtomicBool::new(false)),
                Processor::SpawnFailure
            )
            .unwrap_err()
            .0
            .contains("spawn failure")
        );
        assert_eq!(f.store.db.query_row("SELECT count(*) FROM native_execution_lifetimes WHERE state='quiescent' AND capacity_released=1", [], |r|r.get::<_,i64>(0)).unwrap(), 1);
        let result = f.call("run", json!({"_refresh":false}));
        assert_eq!(result["processed"], 1);
        assert_eq!(
            f.store
                .db
                .query_row(
                    "SELECT generation FROM execution_claims WHERE job_id=?",
                    [&jobs[0]],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
    }

    #[test]
    fn native_panic_drain_never_mints_cleanup_from_a_joined_thread_alone() {
        let mut f = Fixture::with_schema(36);
        f.file("a", None);
        f.enqueue();
        assert!(
            run_owned_processor(
                &mut f.store,
                "default",
                &json!({"_refresh":false}),
                Arc::new(AtomicBool::new(false)),
                Processor::PanicBuiltin
            )
            .unwrap_err()
            .0
            .contains("panicked")
        );
        assert_eq!(
            f.store
                .db
                .query_row(
                    "SELECT count(*) FROM native_execution_lifetimes WHERE state='quiescent'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            storage::prepare_run_admitted(&f.store, "default", None, media::now(), 0, 1)
                .unwrap_err()
                .code,
            storage::AdmissionErrorCode::BusyWorker
        );
    }

    #[test]
    fn enqueue_pages_locations_recipes_and_hardlinks() {
        let mut f = Fixture::new();
        f.file("a", None);
        f.file("b", Some("a"));
        f.file("c", None);
        let page = f.call(
            "enqueue",
            json!({"operation":"hash","location":"source","limit":1}),
        );
        assert_eq!(page["queued"].as_array().unwrap().len(), 1);
        assert_eq!(page["next_after"], "a");
        let page = f.call("enqueue", json!({"operation":"hash","after":"a","limit":2}));
        assert_eq!(page["queued"].as_array().unwrap().len(), 2);
        assert_eq!(page["next_after"], Value::Null);
        let report = f.call("run", json!({"_refresh":false}));
        assert_eq!(
            report,
            json!({"processed":3,"retried":0,"reused_physical_results":1,"counts":{"complete":3},"complete":true,"remaining":0})
        );
        let recipe = media::put_operation(
            &f.store,
            "checksum",
            &json!({"kind":"analysis","operation":"hash","options":{"timeout":7}}),
        )
        .unwrap();
        let jobs = f.call("enqueue",json!({"operation":"hash","file_ids":["a","a"],"options":{"timeout":7},"recipe_id":recipe["id"]}));
        assert_eq!(jobs["queued"].as_array().unwrap().len(), 1);
        let error = execute(
            &mut f.store,
            "default",
            "enqueue",
            &json!({"operation":"hash","file_ids":[],"recipe_id":recipe["id"]}),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap_err();
        assert!(error.0.contains("immutable definition"));
    }

    #[test]
    fn scope_due_retries_and_abandoned_claims_are_exact() {
        let mut f = Fixture::new();
        f.file("a", None);
        f.file("b", None);
        let jobs = f.enqueue();
        for id in &jobs {
            let claim = storage::start(&f.store, "default", id, media::now()).unwrap();
            storage::complete(
                &f.store,
                "default",
                &claim,
                &json!({"state":"timeout","data":{},"error":"fixture","retryable":true}),
                media::now(),
                1,
                None,
            )
            .unwrap();
        }
        f.store
            .db
            .execute("UPDATE processing_retry_queue SET retry_after=0", [])
            .unwrap();
        let empty = f.call(
            "run",
            json!({"job_ids":[],"retry_transient":10,"_refresh":false}),
        );
        assert_eq!(empty["processed"], 0);
        assert_eq!(empty["retried"], 0);
        let no_retry = f.call("run", json!({"job_ids":[jobs[0]],"_refresh":false}));
        assert_eq!(no_retry["processed"], 0);
        let retried = f.call(
            "run",
            json!({"job_ids":[jobs[0],jobs[0]],"retry_transient":1,"_refresh":false}),
        );
        assert_eq!(retried["processed"], 1);
        assert_eq!(retried["retried"], 1);
        assert_eq!(
            storage::get_job(&f.store, "default", &jobs[0]).unwrap()["attempts"],
            2
        );
        assert_eq!(
            storage::get_job(&f.store, "default", &jobs[1]).unwrap()["state"],
            "timeout"
        );
        storage::retry(&f.store, "default", &jobs[1]).unwrap();
        let claim = storage::start(&f.store, "default", &jobs[1], media::now()).unwrap();
        let live = f.call("run", json!({"job_ids":[jobs[1]],"_refresh":false}));
        assert_eq!(live["processed"], 0);
        storage::release(&f.store, &claim).unwrap();
        let recovered = f.call("run", json!({"job_ids":[jobs[1]],"_refresh":false}));
        assert_eq!(recovered["processed"], 1);
        assert_eq!(
            storage::get_job(&f.store, "default", &jobs[1]).unwrap()["attempts"],
            3
        );
    }

    #[test]
    fn limits_transactions_and_remaining_preserve_contract() {
        let mut f = Fixture::new();
        f.file("a", None);
        f.file("b", None);
        f.enqueue();
        for args in [
            json!({"workers":true}),
            json!({"per_device":3}),
            json!({"limit":0}),
            json!({"retry_transient":11}),
            json!({"retry_delay":0}),
            json!({"storage_groups":{"source":""}}),
            json!({"job_ids":[1]}),
        ] {
            assert!(
                execute(
                    &mut f.store,
                    "default",
                    "run",
                    &args,
                    Arc::new(AtomicBool::new(false))
                )
                .is_err(),
                "{args}"
            );
        }
        f.store.db.execute_batch("BEGIN").unwrap();
        assert!(
            execute(
                &mut f.store,
                "default",
                "run",
                &json!({}),
                Arc::new(AtomicBool::new(false))
            )
            .unwrap_err()
            .0
            .contains("outside an active transaction")
        );
        assert!(
            execute(
                &mut f.store,
                "default",
                "enqueue",
                &json!({"operation":"probe"}),
                Arc::new(AtomicBool::new(false))
            )
            .unwrap_err()
            .0
            .contains("cannot detach an active transaction")
        );
        f.store.db.execute_batch("ROLLBACK").unwrap();
        let report = f.call("run", json!({"limit":1,"_refresh":false}));
        assert_eq!(report["processed"], 1);
        assert_eq!(report["remaining"], 1);
        assert_eq!(f.call("run", json!({"_refresh":false}))["remaining"], 0);
        f.store = Store::open(&f.store.path, false, false).unwrap();
        assert!(
            execute(
                &mut f.store,
                "default",
                "enqueue",
                &json!({"operation":"hash","file_ids":["a"],"refresh":true}),
                Arc::new(AtomicBool::new(false))
            )
            .unwrap_err()
            .0
            .contains("read-only")
        );
        assert_eq!(
            f.call("enqueue", json!({"operation":"hash","file_ids":[]}))["queued"],
            json!([])
        );
    }

    #[test]
    fn bounded_workers_detach_writer_and_reap_on_cancellation() {
        for per_device in [1, 2] {
            let mut f = Fixture::new();
            for id in ["a", "b", "c"] {
                f.file(id, None);
            }
            f.enqueue();
            let active = Arc::new(AtomicUsize::new(0));
            let peak = Arc::new(AtomicUsize::new(0));
            let seen_writer = Arc::new(AtomicBool::new(false));
            let path = f.store.path.clone();
            let a = Arc::clone(&active);
            let p = Arc::clone(&peak);
            let w = Arc::clone(&seen_writer);
            let report = run_with_processor(
                &mut f.store,
                "default",
                &json!({"workers":2,"per_device":per_device,"_refresh":false}),
                Arc::new(AtomicBool::new(false)),
                move |job, cancel| {
                    let n = a.fetch_add(1, Ordering::SeqCst) + 1;
                    p.fetch_max(n, Ordering::SeqCst);
                    crate::process::command_output(
                        &["/bin/sleep", "0.15"],
                        crate::process::CommandOptions {
                            cancel: Some(cancel),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    {
                        let _writer = detached_writer(&path);
                        w.store(true, Ordering::SeqCst);
                    }
                    let result = media::process(job, cancel);
                    a.fetch_sub(1, Ordering::SeqCst);
                    result
                },
            )
            .unwrap();
            assert_eq!(report["processed"], 3);
            assert_eq!(peak.load(Ordering::SeqCst), per_device);
            assert!(seen_writer.load(Ordering::SeqCst));
        }
        let mut f = Fixture::new();
        f.file("a", None);
        f.file("b", None);
        f.enqueue();
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel);
        let pids = Arc::new(Mutex::new(Vec::<u32>::new()));
        let worker_pids = Arc::clone(&pids);
        let started = Instant::now();
        let signal = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            trigger.store(true, Ordering::Relaxed);
        });
        let result = run_with_processor(
            &mut f.store,
            "default",
            &json!({"workers":2,"per_device":2,"_refresh":false}),
            cancel,
            move |_job, cancel| {
                // Actual bounded local tools exercise process cancellation and reaping.
                let result = crate::process::command_output(
                    &["/bin/sh", "-c", "echo $$; exec sleep 20"],
                    crate::process::CommandOptions {
                        cancel: Some(cancel),
                        ..Default::default()
                    },
                );
                if let Err(error) = result
                    && let Ok(pid) = String::from_utf8_lossy(&error.stdout).trim().parse::<u32>()
                {
                    worker_pids.lock().unwrap().push(pid);
                }
                json!({"state":"cancelled","data":{},"error":"processing cancelled"})
            },
        );
        signal.join().unwrap();
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
        let pids = pids.lock().unwrap();
        assert_eq!(pids.len(), 2);
        for pid in pids.iter() {
            // SAFETY: signal zero probes existence only, without sending a signal.
            assert_eq!(unsafe { libc::kill(*pid as i32, 0) }, -1);
        }
        assert_eq!(
            f.store
                .db
                .query_row(
                    "SELECT count(*) FROM execution_claims WHERE lease_until>0",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn storage_group_throttles_distinct_device_queues() {
        let mut f = Fixture::new();
        f.file("a", None);
        f.file("b", None);
        f.enqueue();
        // Synthetic independent devices isolate the group scheduler decision.
        // The test publishes only unsupported outcomes, never unvalidated facts.
        f.store.transaction(|db| {
            db.execute("INSERT INTO locations VALUES ('other')",[])?;
            db.execute("INSERT INTO bindings SELECT profile,kind,'other',root||'/other',device+1,inode FROM bindings WHERE owner='source'",[])?;
            db.execute("UPDATE processing_jobs SET location='other' WHERE file_id='b'",[])?;
            Ok(())
        }).unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let a = Arc::clone(&active);
        let p = Arc::clone(&peak);
        let report=run_with_processor(&mut f.store,"default",&json!({"workers":2,"per_device":1,"storage_groups":{"source":"disk-array","other":"disk-array"},"_refresh":false}),Arc::new(AtomicBool::new(false)),move |_,cancel| {
            let n=a.fetch_add(1,Ordering::SeqCst)+1; p.fetch_max(n,Ordering::SeqCst);
            crate::process::command_output(&["/bin/sleep","0.15"],crate::process::CommandOptions {cancel:Some(cancel),..Default::default()}).unwrap();
            a.fetch_sub(1,Ordering::SeqCst);
            json!({"state":"unsupported","data":{},"error":"scheduler fixture"})
        }).unwrap();
        assert_eq!(report["processed"], 2);
        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn slow_work_renews_claims_and_stale_publication_stops_all_workers() {
        let mut f = Fixture::new();
        f.file("a", None);
        let job = f.enqueue().remove(0);
        let path = f.store.path.clone();
        let id = job.clone();
        let observed = Arc::new(AtomicBool::new(false));
        let renewed = Arc::clone(&observed);
        let initial = media::now();
        let report = run_with_processor(
            &mut f.store,
            "default",
            &json!({"_refresh":false}),
            Arc::new(AtomicBool::new(false)),
            move |job, cancel| {
                crate::process::command_output(
                    &["/bin/sleep", "5.5"],
                    crate::process::CommandOptions {
                        cancel: Some(cancel),
                        ..Default::default()
                    },
                )
                .unwrap();
                let reader = Store::open(&path, false, false).unwrap();
                let until: f64 = reader
                    .db
                    .query_row(
                        "SELECT lease_until FROM execution_claims WHERE job_id=?",
                        [&id],
                        |r| r.get(0),
                    )
                    .unwrap();
                renewed.store(until > initial + 64.0, Ordering::Relaxed);
                media::process(job, cancel)
            },
        )
        .unwrap();
        assert_eq!(report["processed"], 1);
        assert!(observed.load(Ordering::Relaxed));
        f.call("enqueue", json!({"operation":"hash","refresh":true}));
        let path = f.store.path.clone();
        let stale = run_with_processor(
            &mut f.store,
            "default",
            &json!({"_refresh":false}),
            Arc::new(AtomicBool::new(false)),
            move |job, cancel| {
                crate::process::command_output(
                    &["/bin/sleep", "0.15"],
                    crate::process::CommandOptions {
                        cancel: Some(cancel),
                        ..Default::default()
                    },
                )
                .unwrap();
                let writer = detached_writer(&path);
                storage::cancel(&writer, "default", job["id"].as_str().unwrap()).unwrap();
                media::process(job, cancel)
            },
        );
        assert!(
            stale
                .unwrap_err()
                .0
                .contains("expired, cancelled or replaced")
        );
        assert_eq!(
            f.store
                .db
                .query_row("SELECT count(*) FROM processing_attempts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
