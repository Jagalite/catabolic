// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Isolated native source probes. An unreaped helper retains its durable slot,
//! even after timeout; source reads never run in the caller's SQLite session.
use catabolic_store::{
    Error, Result, Store,
    fallback_resolution::{self, ProbeSlot},
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
struct Pending {
    child: Child,
    path: PathBuf,
    database_id: String,
    slot: ProbeSlot,
}
static PENDING: OnceLock<Mutex<Vec<Pending>>> = OnceLock::new();
fn pending() -> &'static Mutex<Vec<Pending>> {
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}
fn reap_pending() -> Result<()> {
    let mut pending = pending()
        .lock()
        .map_err(|_| Error("fallback probe supervisor unavailable".into()))?;
    let mut index = 0;
    while index < pending.len() {
        if pending[index].child.try_wait()?.is_some() {
            let old = pending.remove(index);
            fallback_resolution::release_probe(&old.path, &old.database_id, &old.slot)?;
        } else {
            index += 1;
        }
    }
    Ok(())
}
fn nonblocking(file: &impl AsRawFd) -> Result<()> {
    // SAFETY: fcntl only changes flags on the live owned pipe descriptor.
    unsafe {
        let flags = libc::fcntl(file.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}
fn retire(mut child: Child, path: &Path, database_id: &str, slot: ProbeSlot) -> Result<()> {
    // The child has not been reaped, reserving the process-group identity.
    // SAFETY: process_group(0) assigned this child a private group before exec.
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    child.stdin.take();
    child.stdout.take();
    child.stderr.take();
    let deadline = Instant::now() + Duration::from_millis(50);
    loop {
        if child.try_wait()?.is_some() {
            return fallback_resolution::release_probe(path, database_id, &slot);
        }
        if Instant::now() >= deadline {
            pending()
                .lock()
                .map_err(|_| Error("fallback probe supervisor unavailable".into()))?
                .push(Pending {
                    child,
                    path: path.into(),
                    database_id: database_id.into(),
                    slot,
                });
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
pub const NATIVE_PROBE_COMMAND: &str = "native-fallback-probe-v1";
pub fn probe_check_v1(request: &Value) -> Result<Value> {
    crate::helper_fence::require(request)?;
    probe_check(request)
}
pub fn probe_check(request: &Value) -> Result<Value> {
    request["snapshots"]
        .as_array()
        .filter(|v| (1..=65).contains(&v.len()))
        .ok_or_else(|| Error("probe_snapshot_limit".into()))?;
    let path = request["database"]
        .as_str()
        .ok_or_else(|| Error("invalid fallback probe request".into()))?;
    let store = Store::open(Path::new(path), false, false)?;
    crate::helper_fence::check(&store, request)?;
    fallback_resolution::probe_check_with_store(&store, request)
}
pub fn probe(
    path: &Path,
    profile: &str,
    database_id: &str,
    snapshots: &Value,
    timeout_ms: u64,
) -> Result<Value> {
    if !(10..=10000).contains(&timeout_ms)
        || snapshots
            .as_array()
            .is_none_or(|v| !(1..=65).contains(&v.len()))
    {
        return Err(Error("invalid fallback probe bounds".into()));
    }
    let mut request =
        json!({"database":path,"database_id":database_id,"profile":profile,"snapshots":snapshots});
    let store = Store::open(path, false, false)?;
    if store.database_id != database_id {
        return Err(Error("catalog replaced during fallback probe".into()));
    }
    let native = crate::helper_fence::write(&store, &mut request)?.is_some();
    drop(store);
    let input = catabolic_core::encode(&request).into_bytes();
    if input.len() > 1024 * 1024 {
        return Err(Error("fallback probe input exceeds 1 MiB".into()));
    }
    let executable = crate::helper::executable()?;
    reap_pending()?;
    let Some(slot) = fallback_resolution::reserve_probe(path, database_id)? else {
        return Ok(json!({"usable":false,"reason":"probe_capacity"}));
    };
    let mut command = Command::new(executable);
    command
        .args([
            if native {
                NATIVE_PROBE_COMMAND
            } else {
                "fallback-probe"
            },
            "check",
        ])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    #[cfg(target_os = "linux")]
    {
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: this callback performs only async-signal-safe syscalls between
        // fork and exec; all Rust allocation/serialization happened above.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(127);
                }
                Ok(())
            });
        }
    }
    let mut child = match catabolic_core::spawn_guard::spawn(&mut command) {
        Ok(child) => child,
        Err(_) => {
            fallback_resolution::release_probe(path, database_id, &slot)?;
            return Ok(json!({"usable":false,"reason":"probe_failed"}));
        }
    };
    // No source payload is delivered until the child's PID is durable. EOF on
    // parent death during this handoff makes the helper exit without a source.
    if let Err(error) = fallback_resolution::attach_probe(path, database_id, &slot, child.id()) {
        retire(child, path, database_id, slot)?;
        return Err(error);
    }
    let result = (|| -> Result<(Value, bool)> {
        let mut stdin = child.stdin.take();
        let mut stdout = child.stdout.take();
        if let Some(pipe) = &stdin {
            nonblocking(pipe)?;
        }
        if let Some(pipe) = &stdout {
            nonblocking(pipe)?;
        }
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut written = 0;
        let mut output = Vec::new();
        let mut status = None;
        loop {
            if let Some(pipe) = stdin.as_mut() {
                match pipe.write(&input[written..]) {
                    Ok(0) => return Err(Error("probe_failed".into())),
                    Ok(count) => written += count,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => return Err(Error("probe_failed".into())),
                }
                if written == input.len() {
                    stdin = None;
                }
            }
            if let Some(pipe) = stdout.as_mut() {
                let mut buffer = [0u8; 4096];
                loop {
                    match pipe.read(&mut buffer) {
                        Ok(0) => {
                            stdout = None;
                            break;
                        }
                        Ok(count) => {
                            output.extend_from_slice(&buffer[..count]);
                            if output.len() > 65536 {
                                return Err(Error("probe_failed".into()));
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(_) => return Err(Error("probe_failed".into())),
                    }
                }
            }
            if status.is_none() {
                status = child.try_wait()?;
            }
            if let Some(status) = status
                && stdout.is_none()
            {
                if !status.success() {
                    return Ok((json!({"usable":false,"reason":"probe_failed"}), true));
                }
                let parsed = catabolic_core::decode_json_bytes(&output)
                    .map_err(|_| Error("probe_failed".into()))?;
                if !parsed["usable"].is_boolean()
                    || (!parsed["reason"].is_null() && !parsed["reason"].is_string())
                {
                    return Err(Error("probe_failed".into()));
                }
                return Ok((parsed, true));
            }
            if Instant::now() >= deadline {
                return Ok((
                    json!({"usable":false,"reason":"probe_timeout"}),
                    status.is_some(),
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    match result {
        Ok((value, true)) => {
            fallback_resolution::release_probe(path, database_id, &slot)?;
            Ok(value)
        }
        Ok((value, false)) => {
            retire(child, path, database_id, slot)?;
            Ok(value)
        }
        Err(_) => {
            // A parse/error can occur after try_wait already reaped the child;
            // never signal a reusable PGID in that case.
            if child.try_wait()?.is_some() {
                fallback_resolution::release_probe(path, database_id, &slot)?;
            } else {
                retire(child, path, database_id, slot)?;
            }
            Ok(json!({"usable":false,"reason":"probe_failed"}))
        }
    }
}

pub fn resolve(store: catabolic_store::Store, profile: &str, request: &Value) -> Result<Value> {
    let policy_id = request["policy_id"]
        .as_str()
        .ok_or_else(|| Error("policy_id must be text".into()))?;
    let policy =
        catabolic_store::fallback_mutation::get(&store, profile, policy_id)?["definition"].clone();
    let mut evaluation = catabolic_store::selection::Evaluation::new(
        store,
        profile,
        policy["budgets"]["query_timeout_ms"].as_u64().unwrap(),
        policy["budgets"]["max_candidates"].as_u64().unwrap() as usize,
        false,
    )?;
    let entries = request["entries"]
        .as_array()
        .ok_or_else(|| Error("entries must be an array".into()))?;
    let mut probe = probe;
    let mut resolver = fallback_resolution::Resolver::new(
        fallback_resolution::Context::Local(&mut evaluation),
        &mut probe,
        None,
    );
    resolver.resolve(
        entries,
        policy_id,
        request["catalog"].as_str().unwrap_or(""),
        &std::collections::BTreeMap::new(),
        request["manual_failback"] == true,
    )
}

/// Projection planning owns one Evaluation across all detached probes, while
/// the publication guard spans admission and existing journal execution.
pub fn projection(
    store: catabolic_store::Store,
    profile: &str,
    action: &str,
    request: &Value,
) -> Result<Value> {
    projection_owned(store, profile, action, request, true)
}
fn projection_owned(
    store: catabolic_store::Store,
    profile: &str,
    action: &str,
    request: &Value,
    drain_consumers: bool,
) -> Result<Value> {
    use catabolic_store::{
        fallback_projection as projection, filesystem_mutation, selection::Evaluation,
    };
    if action == "run" && request["apply"] == true {
        // Refuse unqualified publication before planning claims probe capacity
        // or launches a helper. The later admission check remains defensive.
        require_projection_publication(request)?;
    }
    let catalog = request["catalog"].as_str().unwrap_or("global");
    let guard = filesystem_mutation::publication_lock(&store)?;
    let policy_id = if action == "bind" {
        request["policy_id"]
            .as_str()
            .ok_or_else(|| Error("policy_id must be text".into()))?
            .to_owned()
    } else {
        projection::binding(&store, profile, catalog)?
            .and_then(|b| b["policy_id"].as_str().map(str::to_owned))
            .ok_or_else(|| Error("projection has no fallback binding".into()))?
    };
    let policy = catabolic_store::fallback_mutation::get(&store, profile, &policy_id)?;
    let mut evaluation = Evaluation::new(
        store,
        profile,
        policy["definition"]["budgets"]["query_timeout_ms"]
            .as_u64()
            .unwrap(),
        policy["definition"]["budgets"]["max_candidates"]
            .as_u64()
            .unwrap() as usize,
        false,
    )?;
    if action == "bind" {
        return projection::bind(
            &mut evaluation,
            catalog,
            &policy_id,
            request["query_id"]
                .as_str()
                .ok_or_else(|| Error("query_id must be text".into()))?,
            request["layout"]
                .as_str()
                .ok_or_else(|| Error("layout must be text".into()))?,
        );
    }
    if !matches!(action, "run" | "plan") {
        return Err(Error("unknown fallback projection action".into()));
    }
    let mut probe = probe;
    let prepared = projection::plan(
        &mut evaluation,
        catalog,
        request["manual_failback"] == true,
        &mut probe,
    )?;
    if request["apply"] != true || action == "plan" {
        return Ok(prepared.present());
    }
    let mut result = apply_prepared(evaluation.store_mut(), profile, prepared, request, &guard)?;
    drop(guard);
    if drain_consumers {
        let mut store = evaluation.into_store();
        crate::consumers::after_publication(&mut store, profile, &mut result)?;
    }
    Ok(result)
}

/// Park a producer's Store while the projection owns its bounded evaluation.
/// The two writable owners never coexist, including during helper waits.
pub fn projection_store(
    store: &mut catabolic_store::Store,
    profile: &str,
    action: &str,
    request: &Value,
) -> Result<Value> {
    if !store.owns_writer_lock() || !store.db.is_autocommit() {
        return Err(Error(
            "fallback projection requires a writer outside an active transaction".into(),
        ));
    }
    catabolic_store::mutation::profile(store, profile)?;
    let path = store.path.clone();
    let database = store.database_id.clone();
    store.detached(|| {
        let writer = catabolic_store::Store::open(&path, true, false)?;
        if writer.database_id != database {
            return Err(Error("catalog replaced during fallback projection".into()));
        }
        projection_owned(writer, profile, action, request, false)
    })
}

fn require_projection_publication(request: &Value) -> Result<()> {
    if cfg!(debug_assertions) && request["disposable_fixture"] == true {
        return Ok(());
    }
    Err(Error("native fallback publication is restricted to disposable debug fixtures pending filesystem qualification".into()))
}

/// Consume precisely the plan whose baseline a watcher has persisted. No
/// selection or live planning is repeated between observation and admission.
pub fn apply_prepared(
    store: &mut catabolic_store::Store,
    profile: &str,
    prepared: catabolic_store::fallback_projection::PreparedProjection,
    request: &Value,
    guard: &catabolic_store::filesystem_mutation::PublicationGuard,
) -> Result<Value> {
    use catabolic_store::{fallback_projection as projection, filesystem_mutation};
    let catalog = prepared.present()["catalog"]
        .as_str()
        .ok_or_else(|| Error("invalid projection catalog".into()))?
        .to_owned();
    if request["automatic"] != true && request["expected_plan"].as_str() != Some(prepared.digest())
    {
        return Err(Error("stale_or_missing_resolution_plan".into()));
    }
    let maximum = |key: &str, default: u64| -> Result<usize> {
        let count = request
            .get(key)
            .map(Value::as_u64)
            .unwrap_or(Some(default))
            .ok_or_else(|| Error(format!("{key} must be nonnegative")))?;
        usize::try_from(count).map_err(|_| Error(format!("{key} exceeds bounds")))
    };
    let removals = maximum("max_removals", 0)?;
    let changes = maximum("max_changes", 100)?;
    require_projection_publication(request)?;
    let admitted = projection::admit(store, profile, prepared, removals, changes)?;
    if !admitted.is_safe() {
        return Ok(admitted.retained());
    }
    let mut probe = probe;
    let mut options = json!({"catalog":catalog,"max_removals":removals,"max_changes":changes,"notify_consumers":request.get("notify_consumers").cloned().unwrap_or(json!(true)),"disposable_fixture":true});
    // Forward failpoint fields whenever present: the store refuses them in
    // builds without the failpoints feature instead of silently ignoring them.
    for key in ["failpoint", "crash"] {
        if let Some(value) = request.get(key) {
            options[key] = value.clone();
        }
    }
    let publication = filesystem_mutation::execute_mut_locked(
        store, profile, "apply", &options, &mut probe, guard,
    )?;
    projection::finish(store, profile, &admitted, &publication)
}

pub fn with_projection_plan<T>(
    store: &mut catabolic_store::Store,
    profile: &str,
    catalog: &str,
    manual_failback: bool,
    operation: impl FnOnce(
        &mut catabolic_store::Store,
        catabolic_store::fallback_projection::PreparedProjection,
        &catabolic_store::filesystem_mutation::PublicationGuard,
    ) -> Result<T>,
) -> Result<T> {
    if !store.owns_writer_lock() || !store.db.is_autocommit() {
        return Err(Error(
            "fallback projection requires a writer outside an active transaction".into(),
        ));
    }
    catabolic_store::mutation::profile(store, profile)?;
    let path = store.path.clone();
    let database = store.database_id.clone();
    store.detached(|| {
        let writer = catabolic_store::Store::open(&path, true, false)?;
        if writer.database_id != database {
            return Err(Error("catalog replaced during fallback projection".into()));
        }
        let guard = catabolic_store::filesystem_mutation::publication_lock(&writer)?;
        let config = catabolic_store::fallback_projection::binding(&writer, profile, catalog)?
            .ok_or_else(|| Error("projection has no fallback binding".into()))?;
        let policy = catabolic_store::fallback_mutation::get(
            &writer,
            profile,
            config["policy_id"].as_str().unwrap(),
        )?["definition"]
            .clone();
        let mut evaluation = catabolic_store::selection::Evaluation::new(
            writer,
            profile,
            policy["budgets"]["query_timeout_ms"].as_u64().unwrap(),
            policy["budgets"]["max_candidates"].as_u64().unwrap() as usize,
            false,
        )?;
        let mut probe = probe;
        let prepared = catabolic_store::fallback_projection::plan(
            &mut evaluation,
            catalog,
            manual_failback,
            &mut probe,
        )?;
        let mut writer = evaluation.into_store();
        operation(&mut writer, prepared, &guard)
    })
}
