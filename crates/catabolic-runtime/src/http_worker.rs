// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! The combined API-worker supervisor. The singleton lease outlives each short
//! catalog owner; neither sleeps nor network delivery retain a writer session.
use catabolic_store::{Error, Result, Store, api_events, playback as sessions};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

fn busy(error: &Error) -> bool {
    error.0.contains("another Catabolic writer")
        || error.0.contains("database is locked")
        || error.0.contains("database is busy")
}
fn unavailable() -> Value {
    json!({"processed":0,"complete":false,"blockers":["catalog_busy"]})
}
fn checked_store(path: &Path, profile: &str, identity: &str) -> Result<Store> {
    let store = Store::open(path, true, false)?;
    if store.database_id != identity {
        return Err(Error("catalog replaced during API worker execution".into()));
    }
    catabolic_store::query::CatalogQuery::new(&store, profile)?;
    Ok(store)
}
fn tick(
    path: &Path,
    profile: &str,
    identity: &str,
    caps: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    let mut store = checked_store(path, profile, identity)?;
    sessions::worker_heartbeat(&store, profile, std::process::id(), caps)?;
    api_events::prune(&store)?;
    if let Some(result) = crate::playback::tick_with_cancel(&mut store, profile, caps, &cancel)? {
        return Ok(result);
    }
    crate::renditions::tick_store(&mut store, profile, caps, cancel)
}
fn drain(path: &Path, profile: &str, identity: &str) -> Result<Value> {
    let result = (|| {
        let mut store = checked_store(path, profile, identity)?;
        crate::notifications::drain(&mut store, profile, 4, None)
    })();
    match result {
        Err(error) if busy(&error) => Ok(json!({"complete":false,"blockers":["catalog_busy"]})),
        other => other,
    }
}
fn joined(task: JoinHandle<Result<Value>>) -> Result<Value> {
    task.join()
        .map_err(|_| Error("API notification worker terminated unexpectedly".into()))?
}
/// Explicitly selected native worker; no Python dispatch or transport fallback.
pub fn run(
    path: &Path,
    profile: &str,
    once: bool,
    interval: Duration,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    #[cfg(not(target_os = "linux"))]
    return Err(Error(
        "native API worker parent-death supervision is unqualified on this platform".into(),
    ));
    // Frozen admission resolves the path before the singleton, but validates
    // the profile only inside a short writable owner. Contention must retain
    // its precedence over an unknown profile, without weakening reopen checks.
    let path = catabolic_store::database_path(path)?;
    let mut lockpath = path.as_os_str().to_os_string();
    lockpath.push(".api-worker");
    let _supervisor = catabolic_store::writer_lock(Path::new(&lockpath))?;
    let initial = Store::open(&path, false, false)?;
    let identity = initial.database_id.clone();
    drop(initial);
    let mut notification: Option<JoinHandle<Result<Value>>> = None;
    let result = (|| {
        let caps = crate::playback::capabilities()?;
        let mut recovered = false;
        loop {
            let work = (|| {
                if !recovered {
                    let store = checked_store(&path, profile, &identity)?;
                    crate::playback::recover(&store, profile)?;
                    recovered = true;
                }
                tick(&path, profile, &identity, &caps, cancel.clone())
            })();
            let mut result = match work {
                Err(error) if busy(&error) => unavailable(),
                other => other?,
            };
            if notification.as_ref().is_some_and(JoinHandle::is_finished) {
                result["notifications"] = joined(notification.take().unwrap())?;
            }
            if notification.is_none() {
                let (path, profile, identity) =
                    (path.clone(), profile.to_owned(), identity.clone());
                notification = Some(
                    std::thread::Builder::new()
                        .name("catabolic-notify".into())
                        .spawn(move || drain(&path, &profile, &identity))?,
                );
            }
            if once || cancel.load(Ordering::Acquire) {
                result["notifications"] = joined(notification.take().unwrap())?;
                return Ok(result);
            }
            let start = std::time::Instant::now();
            while start.elapsed() < interval && !cancel.load(Ordering::Acquire) {
                std::thread::sleep(
                    Duration::from_millis(50).min(interval.saturating_sub(start.elapsed())),
                );
            }
            if cancel.load(Ordering::Acquire) {
                result["notifications"] = joined(notification.take().unwrap())?;
                return Ok(result);
            }
        }
    })();
    let joined_result = notification.take().map(joined).transpose();
    let removed = (|| {
        let store = checked_store(&path, profile, &identity)?;
        sessions::worker_remove(&store, profile, std::process::id())
    })();
    let removed = match removed {
        Err(error) if busy(&error) => Ok(()),
        other => other,
    };
    result.and_then(|value| {
        joined_result?;
        removed?;
        Ok(value)
    })
}
/// Runtime adapter preserves the caller's writer identity and rejects readonly
/// or nested transaction calls before acquiring a supervisor lock or probing.
pub fn execute(
    store: &mut Store,
    profile: &str,
    operation: &str,
    request: &Value,
) -> Result<Value> {
    if !matches!(operation, "run" | "tick") {
        return Err(Error("unknown API worker operation".into()));
    }
    if !store.writable || !store.owns_writer_lock() {
        return Err(Error(
            "API worker requires a writable catalog session".into(),
        ));
    }
    if !store.db.is_autocommit() {
        return Err(Error(
            "API worker requires a standalone transaction boundary".into(),
        ));
    }
    let interval = request
        .get("interval")
        .map(|v| {
            v.as_f64()
                .filter(|v| v.is_finite() && *v > 0. && *v <= 86400.)
                .ok_or_else(|| Error("invalid API worker interval".into()))
        })
        .transpose()?
        .unwrap_or(2.);
    let path = store.path.clone();
    let once = operation == "tick" || request["once"] == true;
    store.detached(|| {
        run(
            &path,
            profile,
            once,
            Duration::from_secs_f64(interval),
            Arc::new(AtomicBool::new(false)),
        )
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_owner_keeps_identity_and_profile_validation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        catabolic_store::migration::initialize(&path).unwrap();
        let initial = Store::open(&path, false, false).unwrap();
        let identity = initial.database_id.clone();
        drop(initial);
        for (profile, identity, expected) in [
            (
                "missing",
                "different-database",
                "catalog replaced during API worker execution",
            ),
            ("missing", identity.as_str(), "unknown profile: missing"),
        ] {
            let Err(error) = checked_store(&path, profile, identity) else {
                panic!("invalid owner admitted");
            };
            assert_eq!(error.0, expected);
            // Every rejected short owner releases its writer lease.
            assert!(catabolic_store::writer_lock(&path).is_ok());
        }
        assert!(checked_store(&path, "default", &identity).is_ok());
    }
    #[test]
    fn readonly_and_transaction_calls_have_no_side_effects() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        catabolic_store::migration::initialize(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut read = Store::open(&path, false, false).unwrap();
        for profile in ["default", "missing"] {
            assert!(
                execute(&mut read, profile, "tick", &json!({}))
                    .unwrap_err()
                    .0
                    .contains("writable")
            );
        }
        drop(read);
        let mut write = Store::open(&path, true, false).unwrap();
        write.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        for profile in ["default", "missing"] {
            assert!(
                execute(&mut write, profile, "tick", &json!({}))
                    .unwrap_err()
                    .0
                    .contains("transaction boundary")
            );
        }
        write.db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!root.path().join("catalog.sqlite3.api-worker.lock").exists());
    }
}
