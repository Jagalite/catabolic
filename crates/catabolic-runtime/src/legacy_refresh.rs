// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded compatibility delivery through the existing consumer network and
//! subprocess owners. Each request fully detaches the catalog's writer first.
use catabolic_store::{Error, Result, Store, legacy_refresh as policy};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub const WORKER_ARGUMENT: &str = "__legacy-refresh-worker";

/// Private CLI helper. Neither the helper output nor its arguments expose a
/// credential. The supervisor owns the complete DNS/TLS/body lifetime.
pub fn worker_main() {
    let delivered = (|| -> Result<bool> {
        let mut raw = Vec::new();
        std::io::stdin().take(262145).read_to_end(&mut raw)?;
        if raw.len() > 262144 {
            return Ok(false);
        }
        let target = catabolic_core::decode_json_bytes(&raw)?;
        if !target.is_object()
            || target["application"] != "jellyfin"
            || !target["endpoint"].as_str().is_some_and(|v| v.len() <= 4096)
            || !target["credential_env"].is_string()
        {
            return Ok(false);
        }
        Ok(crate::consumers::Adapter::new(&target)
            .and_then(|adapter| adapter.legacy_refresh())
            .is_ok())
    })()
    .unwrap_or(false);
    let _ = writeln!(
        std::io::stdout(),
        "{}",
        if delivered { "true" } else { "false" }
    );
}
fn deliver(executable: &Path, target: &Value, cancel: &AtomicBool) -> Result<bool> {
    let input = catabolic_core::encode(target);
    if input.len() > 262144 {
        return Ok(false);
    }
    let result = crate::process::command_output(
        &[executable.as_os_str(), OsStr::new(WORKER_ARGUMENT)],
        crate::process::CommandOptions {
            input_bytes: Some(input.as_bytes()),
            cancel: Some(cancel),
            timeout: Duration::from_secs(15),
            maximum: 4096,
            ..Default::default()
        },
    );
    if cancel.load(Ordering::Acquire)
        || result
            .as_ref()
            .is_err_and(|error| error.state == "cancelled")
    {
        return Err(Error("native legacy CLI interrupted".into()));
    }
    Ok(result.is_ok_and(|result| {
        result.returncode == 0 && result.stderr.is_empty() && result.stdout == b"true\n"
    }))
}
pub fn run(
    store: &mut Store,
    profile: &str,
    request: &Value,
    executable: &Path,
    cancel: &AtomicBool,
) -> Result<Value> {
    catabolic_store::mutation::profile(store, profile)?;
    let limit = policy::limit(request)?;
    if !store.owns_writer_lock() || !store.db.is_autocommit() {
        return Err(Error(
            "legacy refresh requires a writer outside an active transaction".into(),
        ));
    }
    let mut events = Vec::new();
    for _ in 0..limit {
        if cancel.load(Ordering::Acquire) {
            return Err(Error("native legacy CLI interrupted".into()));
        }
        let Some(row) = policy::next(store, profile, catabolic_store::notifications::now())? else {
            break;
        };
        let id = row["id"]
            .as_i64()
            .ok_or_else(|| Error("invalid refresh event id".into()))?;
        let catalog = row["catalog"]
            .as_str()
            .ok_or_else(|| Error("invalid refresh event catalog".into()))?;
        let verified = catabolic_store::filesystem_mutation::execute_mut(
            store,
            profile,
            "verify",
            &json!({"catalog":catalog}),
            &mut crate::fallback::probe,
        )?;
        if verified["healthy"] != true {
            events.push(json!({"id":id,"state":"blocked","error":"publication_unhealthy"}));
            break;
        }
        let token = uuid::Uuid::new_v4().to_string();
        if !policy::lease(
            store,
            profile,
            id,
            &token,
            catabolic_store::notifications::now(),
        )? {
            continue;
        }
        let delivered = store.detached(|| deliver(executable, &row["target"], cancel))?;
        policy::acknowledge(store, profile, id, &token, delivered)?;
        events.push(json!({"id":id,"state":if delivered {"complete"} else {"failed"},"error":if delivered {None} else {Some(policy::DELIVERY_ERROR)}}));
        if !delivered {
            break;
        }
    }
    let unresolved = policy::unresolved(store, profile)?;
    Ok(
        json!({"events":events,"unresolved":unresolved,"complete":unresolved==0,"indexing_verified":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    #[test]
    fn cancellation_reaps_the_private_helper() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("helper");
        let pid_file = root.path().join("pid");
        // Only a disposable test helper. Production always uses its own native
        // executable and never invokes a shell or puts credentials in argv.
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nsleep 60\n",
                pid_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let child = std::thread::spawn(move || deliver(&executable, &json!({}), &token));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !pid_file.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid: libc::pid_t = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
        cancel.store(true, Ordering::Release);
        assert_eq!(
            child.join().unwrap().unwrap_err().0,
            "native legacy CLI interrupted"
        );
        let mut status = 0;
        // SAFETY: pid was emitted by our direct disposable subprocess, status
        // is writable storage, and WNOHANG avoids waiting for an unrelated task.
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        assert_eq!(waited, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
