//! Native bounded drain and actual shared-demand ownership, on disposable catalogs.
#![cfg(unix)]
use catabolic_runtime::{
    helper,
    host::{CONTRACT_VERSION, ErrorCode, HostSession, Lifecycle},
    observation,
};
use catabolic_store::{Store, migration};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone, Default)]
struct Probe {
    drops: Arc<AtomicUsize>,
    pid: Arc<AtomicU32>,
    premature: Arc<AtomicBool>,
}
struct Permit(Probe);
fn alive(pid: u32) -> bool {
    // SAFETY: signal zero observes only the fixture-owned child, without signaling it.
    pid != 0 && unsafe { libc::kill(pid as i32, 0) == 0 }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if alive(self.0.pid.load(Ordering::SeqCst)) {
            self.0.premature.store(true, Ordering::SeqCst);
        }
        self.0.drops.fetch_add(1, Ordering::SeqCst);
    }
}
fn writer(path: &Path) -> Store {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Store::open(path, true, false) {
            Ok(store) => return store,
            Err(error)
                if error.0.contains("another Catabolic writer") && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(error) => panic!("writer: {error}"),
        }
    }
}
fn state(store: &Store, table: &str, id: &str) -> String {
    store
        .db
        .query_row(
            &format!("SELECT state FROM {table} WHERE id=?"),
            [id],
            |r| r.get(0),
        )
        .unwrap()
}
fn demand(store: &Store, worker: &observation::SupervisedWorker<Permit>) -> Option<String> {
    worker.request_id(store).unwrap()
}
fn await_demand(path: &Path, worker: &observation::SupervisedWorker<Permit>) -> Store {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let store = writer(path);
        if demand(&store, worker).is_some() {
            return store;
        }
        drop(store);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn native35_and36_bounded_host_drain_retains_the_exact_child_permit() {
    let tools = tempfile::tempdir().unwrap();
    let binary = tools.path().join("bounded-child");
    fs::write(&binary,"#!/bin/sh\nprintf '%s' \"$$\" > \"$2.started\"\nwhile [ ! -f \"$2.release\" ]; do /bin/sleep 0.01; done\nprintf '{\"complete\":true}'\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    for schema in [35, 36] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog");
        migration::initialize_native_at(&path, schema).unwrap();
        let mut host = HostSession::open_authorized(
            &path,
            "default",
            true,
            CONTRACT_VERSION,
            catabolic_core::catalog_host_lease::HostAuthorization::new(
                "host-fixture".into(),
                "revision-1".into(),
            )
            .unwrap(),
        )
        .unwrap();
        host.register_helper(&binary, &catabolic_core::sha256(fs::read(&binary).unwrap()))
            .unwrap();
        let gate = host.authorization().unwrap();
        let authority_lease = gate.acquire(gate.revision()).unwrap();
        assert!(authority_lease.belongs_to(&host.authorization().unwrap()));
        let equal_labels = catabolic_core::catalog_host_lease::HostAuthorization::new(
            gate.namespace().into(),
            gate.revision().into(),
        )
        .unwrap();
        assert!(!authority_lease.belongs_to(&equal_labels));
        drop(authority_lease);
        let probe = Probe::default();
        let scan = host.start_scan("media", Permit(probe.clone())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            if let Ok(s) = fs::read_to_string(format!("{}.started", path.display()))
                && let Ok(pid) = s.parse()
            {
                break pid;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        };
        probe.pid.store(pid, Ordering::SeqCst);
        let revoked = gate.begin_revocation().unwrap();
        assert_eq!(
            revoked.wait_drained(Duration::ZERO),
            Err(catabolic_core::catalog_host_lease::LeaseError::Timeout)
        );
        assert_eq!(
            host.put_items(
                &[json!({"id":"denied","kind":"movie","metadata":{}})],
                &Default::default()
            )
            .unwrap_err()
            .code,
            ErrorCode::Conflict
        );
        assert_eq!(
            host.start_scan("media", ()).unwrap_err().code,
            ErrorCode::Conflict
        );
        for wait in [Duration::ZERO, Duration::from_millis(20)] {
            let started = Instant::now();
            assert!(!host.drain(wait).unwrap());
            assert!(started.elapsed() < Duration::from_secs(1));
            assert_eq!(host.lifecycle(), Lifecycle::Draining);
            assert_eq!(probe.drops.load(Ordering::SeqCst), 0);
            assert!(alive(pid));
            assert_eq!(host.close().unwrap_err().code, ErrorCode::Busy);
        }
        assert_eq!(host.active_scan(), Some(scan));
        fs::write(format!("{}.release", path.display()), "release").unwrap();
        assert!(host.drain(Duration::from_secs(3)).unwrap());
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        assert!(!probe.premature.load(Ordering::SeqCst));
        assert_eq!(host.lifecycle(), Lifecycle::Closed);
        revoked.wait_drained(Duration::ZERO).unwrap();
        assert!(
            HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate.clone())
                .is_err()
        );
        host.close().unwrap();
        drop(host);
        let mut reopened = HostSession::open_authorized(
            &path,
            "default",
            true,
            CONTRACT_VERSION,
            catabolic_core::catalog_host_lease::HostAuthorization::new(
                "host-fixture".into(),
                "revision-1".into(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(reopened.identity().unwrap()["schema_version"], schema);
        reopened.close().unwrap();
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
}
#[test]
#[ignore = "requires rebuilt exact helper in CATABOLIC_HOST_TEST_HELPER; run separately from controlled helper test"]
fn native35_and36_two_real_consumers_preserve_the_shared_scan_after_one_cancel() {
    let binary = PathBuf::from(
        std::env::var_os("CATABOLIC_HOST_TEST_HELPER").expect("exact helper required"),
    );
    helper::register(
        &binary,
        Some(&catabolic_core::sha256(fs::read(&binary).unwrap())),
    )
    .unwrap();
    for schema in [35, 36] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog");
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        for n in 0..180 {
            let directory = source.join(format!("group-{}", n / 3));
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join(format!("file-{n}.txt")), "shared source\n").unwrap();
        }
        migration::initialize_native_at(&path, schema).unwrap();
        let mut store = writer(&path);
        catabolic_store::observation::execute_detached(
            &mut store,
            "default",
            "bind",
            &json!({"source":"media","root":source}),
        )
        .unwrap();
        let a = Probe::default();
        let mut first = observation::start_supervised(
            &mut store,
            "default",
            &json!({"source":"media","requester":"consumer-a","max_age":600}),
            Permit(a.clone()),
        )
        .unwrap()
        .worker;
        a.pid.store(first.pid(), Ordering::SeqCst);
        drop(store);
        let mut store = await_demand(&path, &first);
        let b = Probe::default();
        let mut second = observation::start_supervised(
            &mut store,
            "default",
            &json!({"source":"media","requester":"consumer-b","max_age":600}),
            Permit(b.clone()),
        )
        .unwrap()
        .worker;
        b.pid.store(second.pid(), Ordering::SeqCst);
        drop(store);
        let mut store = await_demand(&path, &second);
        let request_a = demand(&store, &first).unwrap();
        let request_b = demand(&store, &second).unwrap();
        let job = |request: &str| {
            store
                .db
                .query_row(
                    "SELECT job_id FROM observation_requests WHERE id=?",
                    [request],
                    |r| r.get::<_, String>(0),
                )
                .unwrap()
        };
        let job_a = job(&request_a);
        assert_eq!(
            job_a,
            job(&request_b),
            "consumers must share the actual admitted job"
        );
        assert_eq!(
            state(&store, "observation_jobs", &job_a),
            "running",
            "cancellation must precede execution completion"
        );
        catabolic_store::observation::execute_detached(
            &mut store,
            "default",
            "cancel",
            &json!({"request_id":request_a}),
        )
        .unwrap();
        assert_eq!(state(&store, "observation_requests", &request_b), "active");
        assert_eq!(a.drops.load(Ordering::SeqCst), 0);
        assert!(alive(first.pid()));
        drop(store);
        let deadline = Instant::now() + Duration::from_secs(90);
        while first
            .wait_without_store(Duration::from_millis(50))
            .unwrap()
            .is_none()
        {
            assert!(Instant::now() < deadline);
        }
        while second
            .wait_without_store(Duration::from_millis(50))
            .unwrap()
            .is_none()
        {
            assert!(Instant::now() < deadline);
        }
        assert_eq!(a.drops.load(Ordering::SeqCst), 1);
        assert_eq!(b.drops.load(Ordering::SeqCst), 1);
        assert!(!a.premature.load(Ordering::SeqCst));
        assert!(!b.premature.load(Ordering::SeqCst));
        let store = writer(&path);
        assert_eq!(state(&store, "observation_jobs", &job_a), "complete");
        assert_eq!(
            store
                .db
                .query_row(
                    "SELECT count(*) FROM observations WHERE status='present'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            180
        );
        assert_eq!(
            state(&store, "observation_requests", &request_a),
            "cancelled"
        );
        assert_eq!(state(&store, "observation_requests", &request_b), "active");
    }
}
