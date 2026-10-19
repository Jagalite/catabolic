// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Isolated helper ownership only. The shell is a controllable test double;
//! actual scan effects and cross-platform/crash recovery need separate receipts.
#![cfg(unix)]

use catabolic_runtime::{helper, observation};
use catabolic_store::{Store, migration};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

fn register_helper() {
    static HELPER: OnceLock<tempfile::TempDir> = OnceLock::new();
    HELPER.get_or_init(|| {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("supervised-observation-fixture");
        fs::write(
            &path,
            "#!/bin/sh\n\
             printf '%s' \"$$\" > \"$2.started\"\n\
             printf '%s' \"$6\" > \"$2.request\"\n\
             while [ ! -f \"$2.release\" ]; do /bin/sleep 0.01; done\n\
             case \"$6\" in\n\
               *fixture_failure*) printf 'catabolic: fixture helper failure\\n' >&2; exit 7;;\n\
               *fixture_malformed*) printf 'not json';;\n\
               *) printf '{\"complete\":true,\"fixture\":\"supervised\"}';;\n\
             esac\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        helper::register(&path, None).unwrap();
        root
    });
}

fn fixture() -> (tempfile::TempDir, PathBuf, Store) {
    register_helper();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    (root, path, store)
}

#[derive(Clone, Default)]
struct Probe {
    drops: Arc<AtomicUsize>,
    pid: Arc<AtomicU32>,
    released_while_alive: Arc<AtomicBool>,
}

struct Permit(Probe);

impl Drop for Permit {
    fn drop(&mut self) {
        let pid = self.0.pid.load(Ordering::SeqCst);
        if pid != 0 && alive(pid) {
            self.0.released_while_alive.store(true, Ordering::SeqCst);
        }
        self.0.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal zero only queries existence of the fixture's child PID.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn release(path: &Path) {
    fs::write(format!("{}.release", path.display()), "release").unwrap();
}

fn assert_released(probe: &Probe) {
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert!(!probe.released_while_alive.load(Ordering::SeqCst));
}

#[test]
fn pending_waits_retain_permit_until_verified_exit_and_terminal_result_is_repeatable() {
    let (_root, path, mut store) = fixture();
    let probe = Probe::default();
    let started = observation::start_supervised(
        &mut store,
        "default",
        &json!({"_native_catalog_id":"untrusted-override"}),
        Permit(probe.clone()),
    )
    .unwrap();
    assert!(started.reconnect_error.is_none());
    let mut worker = started.worker;
    probe.pid.store(worker.pid(), Ordering::SeqCst);
    assert!(worker.requester().starts_with("native:"));
    assert_eq!(worker.request_id(&store).unwrap(), None);
    for timeout in [Duration::ZERO, Duration::from_millis(30)] {
        let now = Instant::now();
        assert_eq!(worker.wait(&mut store, timeout).unwrap(), None);
        assert!(now.elapsed() < Duration::from_secs(1));
        assert!(!worker.is_finished());
        assert_eq!(probe.drops.load(Ordering::SeqCst), 0);
        assert!(alive(worker.pid()));
    }
    release(&path);
    let result = worker.wait(&mut store, Duration::from_secs(3)).unwrap();
    assert_eq!(
        result,
        Some(json!({"complete":true,"fixture":"supervised"}))
    );
    assert!(worker.is_finished());
    assert_released(&probe);
    let envelope = catabolic_core::decode_json(
        &fs::read_to_string(format!("{}.request", path.display())).unwrap(),
    )
    .unwrap();
    assert_eq!(envelope["_native_catalog_id"], store.database_id);
    assert_eq!(envelope["isolate"], false);
    assert_eq!(worker.wait(&mut store, Duration::ZERO).unwrap(), result);
    drop(worker);
    assert_released(&probe);
}

#[test]
fn bounded_drain_after_catalog_close_keeps_pending_ownership() {
    let (_root, path, mut store) = fixture();
    let probe = Probe::default();
    let started = observation::start_supervised(
        &mut store,
        "default",
        &json!({"requester":"embedding-consumer"}),
        Permit(probe.clone()),
    )
    .unwrap();
    assert!(started.reconnect_error.is_none());
    let mut worker = started.worker;
    probe.pid.store(worker.pid(), Ordering::SeqCst);
    assert_eq!(worker.requester(), "embedding-consumer");
    drop(store);
    assert_eq!(
        worker
            .wait_without_store(Duration::from_millis(10))
            .unwrap(),
        None
    );
    assert_eq!(probe.drops.load(Ordering::SeqCst), 0);
    release(&path);
    assert!(
        worker
            .wait_without_store(Duration::from_secs(3))
            .unwrap()
            .is_some()
    );
    assert_released(&probe);
    assert!(Store::open(&path, true, false).is_ok());
}

#[test]
fn helper_error_and_invalid_result_release_only_after_process_exit() {
    for mode in ["fixture_failure", "fixture_malformed"] {
        let (_root, path, mut store) = fixture();
        let probe = Probe::default();
        let started = observation::start_supervised(
            &mut store,
            "default",
            &json!({"fixture":mode}),
            Permit(probe.clone()),
        )
        .unwrap();
        assert!(started.reconnect_error.is_none());
        let mut worker = started.worker;
        probe.pid.store(worker.pid(), Ordering::SeqCst);
        assert_eq!(worker.wait(&mut store, Duration::ZERO).unwrap(), None);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 0);
        release(&path);
        let error = worker.wait(&mut store, Duration::from_secs(3)).unwrap_err();
        if mode == "fixture_failure" {
            assert_eq!(error.0, "fixture helper failure");
        }
        assert!(worker.is_finished());
        assert_released(&probe);
        assert_eq!(
            worker.wait(&mut store, Duration::ZERO).unwrap_err().0,
            error.0
        );
    }
}

#[test]
fn drop_after_catalog_close_joins_instead_of_creating_a_reaper_thread() {
    let (_root, path, mut store) = fixture();
    let probe = Probe::default();
    let started =
        observation::start_supervised(&mut store, "default", &json!({}), Permit(probe.clone()))
            .unwrap();
    assert!(started.reconnect_error.is_none());
    let worker = started.worker;
    probe.pid.store(worker.pid(), Ordering::SeqCst);
    drop(store);
    let release_worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        release(&path);
    });
    drop(worker);
    assert_released(&probe);
    release_worker.join().unwrap();
}

#[test]
fn rejection_before_spawn_releases_permit_without_starting_a_helper() {
    let (_root, path, mut store) = fixture();
    for request in [json!(null), json!({})] {
        let probe = Probe::default();
        store.db.execute_batch("BEGIN").unwrap();
        let result =
            observation::start_supervised(&mut store, "default", &request, Permit(probe.clone()));
        assert!(result.is_err());
        assert_released(&probe);
        assert!(!Path::new(&format!("{}.started", path.display())).exists());
        store.db.execute_batch("ROLLBACK").unwrap();
    }
}

#[test]
fn reconnect_failure_after_spawn_returns_live_worker_and_retained_permit() {
    let (root, _path, mut store) = fixture();
    // Fault injection in the public Store field: existing connection remains
    // valid, but its post-spawn reopen target is absent. No real catalog is moved.
    let missing = root.path().join("missing.db");
    store.path = missing.clone();
    let probe = Probe::default();
    let started =
        observation::start_supervised(&mut store, "default", &json!({}), Permit(probe.clone()))
            .unwrap();
    assert!(started.reconnect_error.is_some());
    let mut worker = started.worker;
    probe.pid.store(worker.pid(), Ordering::SeqCst);
    assert!(!store.owns_writer_lock());
    assert!(!worker.is_finished());
    assert_eq!(probe.drops.load(Ordering::SeqCst), 0);
    drop(store);
    assert_eq!(worker.wait_without_store(Duration::ZERO).unwrap(), None);
    release(&missing);
    assert!(
        worker
            .wait_without_store(Duration::from_secs(3))
            .unwrap()
            .is_some()
    );
    assert_released(&probe);
}

#[test]
fn foreign_catalog_wait_is_refused_without_releasing_permit() {
    let (_root, path, mut store) = fixture();
    let (_other_root, _other_path, mut other) = fixture();
    let probe = Probe::default();
    let started =
        observation::start_supervised(&mut store, "default", &json!({}), Permit(probe.clone()))
            .unwrap();
    assert!(started.reconnect_error.is_none());
    let mut worker = started.worker;
    probe.pid.store(worker.pid(), Ordering::SeqCst);
    assert_eq!(
        worker.wait(&mut other, Duration::ZERO).unwrap_err().0,
        "observation worker catalog identity mismatch"
    );
    assert_eq!(probe.drops.load(Ordering::SeqCst), 0);
    assert!(worker.request_id(&other).is_err());
    release(&path);
    assert!(
        worker
            .wait(&mut store, Duration::from_secs(3))
            .unwrap()
            .is_some()
    );
    assert_released(&probe);
}

#[test]
fn replaced_catalog_is_refused_by_helper_before_observation_admission() {
    let (root, path, store) = fixture();
    let expected = store.database_id.clone();
    drop(store);
    let replacement = root.path().join("replacement.db");
    migration::initialize(&replacement).unwrap();
    fs::rename(&replacement, &path).unwrap();
    let error =
        observation::worker(&path, "default", &json!({"_native_catalog_id":expected})).unwrap_err();
    assert_eq!(error.0, "observation worker catalog identity mismatch");
    let reopened = Store::open(&path, true, false).unwrap();
    assert_ne!(reopened.database_id, expected);
    for table in ["observation_requests", "observation_jobs", "scans"] {
        let count: i64 = reopened
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    drop(reopened);
    // An old caller without the private field still reaches the legacy owner.
    assert_eq!(
        observation::worker(&path, "missing", &json!({}))
            .unwrap_err()
            .0,
        "unknown profile: missing"
    );
}

#[test]
fn same_path_reopened_replacement_cannot_be_used_to_wait_or_find_a_demand() {
    let (root, path, mut store) = fixture();
    let probe = Probe::default();
    let started =
        observation::start_supervised(&mut store, "default", &json!({}), Permit(probe.clone()))
            .unwrap();
    assert!(started.reconnect_error.is_none());
    let mut worker = started.worker;
    probe.pid.store(worker.pid(), Ordering::SeqCst);
    drop(store);
    let replacement = root.path().join("replacement.db");
    migration::initialize(&replacement).unwrap();
    fs::rename(&replacement, &path).unwrap();
    let mut reopened = Store::open(&path, true, false).unwrap();
    assert_eq!(
        worker.wait(&mut reopened, Duration::ZERO).unwrap_err().0,
        "observation worker catalog identity mismatch"
    );
    assert!(worker.request_id(&reopened).is_err());
    assert_eq!(probe.drops.load(Ordering::SeqCst), 0);
    drop(reopened);
    release(&path);
    assert!(
        worker
            .wait_without_store(Duration::from_secs(3))
            .unwrap()
            .is_some()
    );
    assert_released(&probe);
}
