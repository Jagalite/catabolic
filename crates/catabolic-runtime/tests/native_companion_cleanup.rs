//! Real schema37 companion tests. A store fixture or terminal PID is not exit proof.
#![cfg(unix)]
use catabolic_core::catalog_host_lease::{HostAuthorization, LeaseError};
use catabolic_runtime::{
    helper,
    host::{CONTRACT_VERSION, ErrorCode, HostSession},
    observation,
};
use catabolic_store::{
    Store, migration, native_observation_companion as companion, native_worker_inventory,
};
use rusqlite::params;
use serde_json::json;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
struct Permit(Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
fn gate() -> HostAuthorization {
    HostAuthorization::new("closed-fixture".into(), "r1".into()).unwrap()
}
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, 37).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    for n in 0..12 {
        let dir = source.join(format!("dir-{n}"));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("one.txt"), "one").unwrap();
    }
    let m = source.metadata().unwrap();
    let store = Store::open(&path, true, false).unwrap();
    // Fresh explicitly unenrolled source fixture. No existing UUID policy changes.
    store
        .transaction(|db| {
            db.execute("INSERT INTO locations VALUES('media')", [])?;
            db.execute(
                "INSERT INTO bindings VALUES('default','source','media',?,?,?)",
                params![source.to_str().unwrap(), m.dev() as i64, m.ino() as i64],
            )?;
            Ok(())
        })
        .unwrap();
    drop(store);
    (root, path)
}
fn wait<P>(worker: &mut observation::SupervisedWorker<P>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if worker
            .wait_without_store(Duration::from_millis(50))
            .unwrap()
            .is_some()
        {
            break;
        }
        assert!(Instant::now() < deadline);
    }
}
fn binary() -> PathBuf {
    PathBuf::from(
        std::env::var_os("CATABOLIC_HOST_TEST_HELPER").expect("rebuilt candidate helper required"),
    )
}
fn grant(path: &Path) -> helper::TrustedClosedObservationArtifact {
    helper::TrustedClosedObservationArtifact::after_packaging_verification(
        path,
        &catabolic_core::sha256(fs::read(path).unwrap()),
        helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
    )
    .unwrap()
}
#[test]
#[ignore = "requires rebuilt exact candidate helper in CATABOLIC_HOST_TEST_HELPER; run this test alone"]
fn native37_exact_companion_seals_and_reopened_host_admits_supported_scan() {
    let binary = binary();
    let digest = catabolic_core::sha256(fs::read(&binary).unwrap());
    let (_root, path) = fixture();
    let authorization = gate();
    let mut host = HostSession::open_authorized(
        &path,
        "default",
        true,
        CONTRACT_VERSION,
        authorization.clone(),
    )
    .unwrap();
    host.register_helper(&binary, &digest).unwrap();
    assert!(
        host.start_scan("media", ())
            .unwrap_err()
            .to_string()
            .contains("native owner failed")
    );
    // Generic registration cannot mint the closed capability even for these bytes.
    host.close().unwrap();
    let store = Store::open(&path, false, false).unwrap();
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM native_observation_companions",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(store);
    let qualified = grant(&binary);
    let mut host = HostSession::open_authorized(
        &path,
        "default",
        true,
        CONTRACT_VERSION,
        authorization.clone(),
    )
    .unwrap();
    host.register_closed_observation_helper(&qualified).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let handle = host.start_scan("media", Permit(drops.clone())).unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let revoked = authorization.begin_revocation().unwrap();
    assert_eq!(
        revoked.wait_drained(Duration::ZERO),
        Err(LeaseError::Timeout)
    );
    assert_eq!(
        host.start_scan("media", ()).unwrap_err().code,
        ErrorCode::Conflict
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(value) = host.poll_scan(&handle, Duration::from_millis(50)).unwrap() {
            assert_eq!(value["complete"], true);
            break;
        }
        assert!(Instant::now() < deadline);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    revoked.wait_drained(Duration::ZERO).unwrap();
    host.close().unwrap();
    drop(host);
    let store = Store::open(&path, false, false).unwrap();
    let record = store
        .db
        .query_row(
            "SELECT reservation_nonce FROM native_observation_companions",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap();
    let record = companion::read(&store, &record).unwrap().unwrap();
    assert_eq!(record.state, "quiescent");
    assert!(record.owned.is_some());
    assert!(companion::has_cleanup_witness(&store, &record.owned.unwrap().job_id).unwrap());
    let inventory = native_worker_inventory::inspect(&store).unwrap();
    assert_eq!(inventory.witnessed_observation, 1);
    assert!(inventory.admits_new_work());
    drop(store);
    // A new host receives a new gate only from the explicit trusted fixture owner.
    let mut host =
        HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate()).unwrap();
    host.register_closed_observation_helper(&qualified).unwrap();
    assert!(host.worker_inventory().unwrap().admits_new_work());
    let handle = host.start_scan("media", Permit(drops.clone())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if host
            .poll_scan(&handle, Duration::from_millis(50))
            .unwrap()
            .is_some()
        {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    assert!(host.worker_inventory().unwrap().admits_new_work());
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    host.close().unwrap();
}
#[test]
#[ignore = "requires rebuilt exact candidate helper in CATABOLIC_HOST_TEST_HELPER; run this test alone"]
fn native37_child_exit_without_parent_seal_and_changed_claim_remain_unresolved() {
    let binary = binary();
    helper::register_closed_observation(&grant(&binary)).unwrap();
    for changed in [false, true] {
        let (_root, path) = fixture();
        let mut store = Store::open(&path, true, false).unwrap();
        let mut worker =
            observation::start_supervised(&mut store, "default", &json!({"source":"media"}), ())
                .unwrap()
                .worker;
        drop(store);
        wait(&mut worker);
        let store = Store::open(&path, true, false).unwrap();
        let nonce = store
            .db
            .query_row(
                "SELECT reservation_nonce FROM native_observation_companions",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        let record = companion::read(&store, &nonce).unwrap().unwrap();
        assert_eq!(record.state, "completed");
        assert_eq!(record.child_pid, Some(worker.pid()));
        let job = record.owned.as_ref().unwrap().job_id.clone();
        assert!(!companion::has_cleanup_witness(&store, &job).unwrap());
        assert!(
            !native_worker_inventory::inspect(&store)
                .unwrap()
                .admits_new_work()
        );
        if changed {
            store
                .transaction(|db| {
                    db.execute(
                        "UPDATE observation_jobs SET claim_token=? WHERE id=?",
                        params![uuid::Uuid::new_v4().to_string(), job],
                    )?;
                    Ok(())
                })
                .unwrap();
            assert!(worker.record_cleanup(&store).is_err());
            assert_eq!(
                companion::read(&store, &nonce).unwrap().unwrap().state,
                "completed"
            );
        } else {
            worker.record_cleanup(&store).unwrap();
            assert!(companion::has_cleanup_witness(&store, &job).unwrap());
            worker.record_cleanup(&store).unwrap();
        }
    }
}
fn writer(path: &Path) -> Store {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Store::open(path, true, false) {
            Ok(s) => return s,
            Err(e) if e.0.contains("another Catabolic writer") && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) => panic!("{e}"),
        }
    }
}
fn await_request<P>(path: &Path, worker: &observation::SupervisedWorker<P>) -> (Store, String) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let store = writer(path);
        if let Some(id) = worker.request_id(&store).unwrap() {
            return (store, id);
        }
        drop(store);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
#[ignore = "requires rebuilt exact candidate helper in CATABOLIC_HOST_TEST_HELPER; run this test alone"]
fn native37_shared_demand_seal_does_not_discharge_the_live_owner() {
    let binary = binary();
    helper::register_closed_observation(&grant(&binary)).unwrap();
    let (root, path) = fixture();
    for n in 0..60 {
        let dir = root.path().join(format!("source/dir-{n}"));
        fs::create_dir_all(&dir).unwrap();
        if n >= 12 {
            fs::write(dir.join("one.txt"), "one").unwrap();
        }
        for k in 0..2 {
            fs::write(dir.join(format!("more-{k}.txt")), "more").unwrap();
        }
    }
    let authorization = gate();
    let first_drops = Arc::new(AtomicUsize::new(0));
    let second_drops = Arc::new(AtomicUsize::new(0));
    let mut store = writer(&path);
    let mut first = observation::start_supervised(
        &mut store,
        "default",
        &json!({"source":"media","max_age":600}),
        (
            Permit(first_drops.clone()),
            authorization.acquire(authorization.revision()).unwrap(),
        ),
    )
    .unwrap()
    .worker;
    drop(store);
    let (mut store, request_a) = await_request(&path, &first);
    let mut second = observation::start_supervised(
        &mut store,
        "default",
        &json!({"source":"media","max_age":600}),
        (
            Permit(second_drops.clone()),
            authorization.acquire(authorization.revision()).unwrap(),
        ),
    )
    .unwrap()
    .worker;
    drop(store);
    let (store, request_b) = await_request(&path, &second);
    let job: String = store
        .db
        .query_row(
            "SELECT job_id FROM observation_requests WHERE id=?",
            [&request_a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT job_id FROM observation_requests WHERE id=?",
                [&request_b],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        job
    );
    drop(store);
    wait(&mut second);
    let mut store = writer(&path);
    second.record_cleanup(&store).unwrap();
    let shared: String = store
        .db
        .query_row(
            "SELECT reservation_nonce FROM native_observation_companions WHERE requester=?",
            [second.requester()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        companion::read(&store, &shared)
            .unwrap()
            .unwrap()
            .owned
            .is_none()
    );
    assert!(!companion::has_cleanup_witness(&store, &job).unwrap());
    assert_eq!(first_drops.load(Ordering::SeqCst), 0);
    assert_eq!(second_drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT state FROM observation_jobs WHERE id=?",
                [&job],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "running"
    );
    let revoked = authorization.begin_revocation().unwrap();
    assert_eq!(
        revoked.wait_drained(Duration::ZERO),
        Err(LeaseError::Timeout)
    );
    catabolic_store::observation::execute_detached(
        &mut store,
        "default",
        "cancel",
        &json!({"request_id":request_a}),
    )
    .unwrap();
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT state FROM observation_requests WHERE id=?",
                [&request_b],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "active"
    );
    drop(store);
    wait(&mut first);
    assert_eq!(first_drops.load(Ordering::SeqCst), 1);
    revoked.wait_drained(Duration::ZERO).unwrap();
    let store = writer(&path);
    assert!(!companion::has_cleanup_witness(&store, &job).unwrap());
    first.record_cleanup(&store).unwrap();
    assert!(companion::has_cleanup_witness(&store, &job).unwrap());
    assert!(
        native_worker_inventory::inspect(&store)
            .unwrap()
            .admits_new_work()
    );
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
}

#[test]
#[ignore = "requires older exact helper in CATABOLIC_OLD_HOST_TEST_HELPER; run this test alone"]
fn native37_mispackaged_old_helper_cannot_fallback_or_seal() {
    let binary = PathBuf::from(
        std::env::var_os("CATABOLIC_OLD_HOST_TEST_HELPER").expect("older helper required"),
    );
    // Deliberately wrong packaging grant tests that an old protocol still cannot
    // fabricate a successful closed acknowledgment or cause a legacy fallback.
    helper::register_closed_observation(&grant(&binary)).unwrap();
    let (_root, path) = fixture();
    let mut store = Store::open(&path, true, false).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut worker = observation::start_supervised(
        &mut store,
        "default",
        &json!({"source":"media"}),
        Permit(drops.clone()),
    )
    .unwrap()
    .worker;
    drop(store);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match worker.wait_without_store(Duration::from_millis(50)) {
            Ok(None) => assert!(Instant::now() < deadline),
            Err(_) => break,
            Ok(Some(_)) => panic!("old helper returned closed protocol success"),
        }
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let store = Store::open(&path, true, false).unwrap();
    worker.record_cleanup(&store).unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM observation_jobs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT state FROM native_observation_companions", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "reserved"
    );
    assert!(
        !native_worker_inventory::inspect(&store)
            .unwrap()
            .admits_new_work()
    );
}
#[test]
#[ignore = "requires rebuilt exact candidate helper in CATABOLIC_HOST_TEST_HELPER; run this test alone"]
fn native37_uuid_checking_cohort_is_refused_without_reservation_or_policy_change() {
    let binary = binary();
    helper::register_closed_observation(&grant(&binary)).unwrap();
    let (_root, path) = fixture();
    let mut store = Store::open(&path, true, false).unwrap();
    store.transaction(|db|{db.execute("INSERT INTO binding_volumes(profile,kind,owner,volume_uuid) VALUES('default','source','media','requires-real-uuid-check')",[])?;Ok(())}).unwrap();
    let before = migration::snapshot(&store.db).unwrap();
    let result =
        observation::start_supervised(&mut store, "default", &json!({"source":"media"}), ());
    assert!(result.is_err());
    assert_eq!(before, migration::snapshot(&store.db).unwrap());
}

#[test]
#[ignore = "requires rebuilt exact candidate helper in CATABOLIC_HOST_TEST_HELPER; run this test alone"]
fn native37_two_authorized_hosts_join_exact_owned_scan_and_cancel_only_one_demand() {
    use catabolic_runtime::host::ScanAdmissionContract;
    let binary = binary();
    let qualified = grant(&binary);
    let (root, path) = fixture();
    for n in 0..60 {
        let dir = root.path().join(format!("source/dir-{n}"));
        fs::create_dir_all(&dir).unwrap();
        if n >= 12 {
            fs::write(dir.join("one.txt"), "one").unwrap();
        }
        for k in 0..2 {
            fs::write(dir.join(format!("more-{k}.txt")), "more").unwrap();
        }
    }
    let authorization = gate();
    let a_drops = Arc::new(AtomicUsize::new(0));
    let b_drops = Arc::new(AtomicUsize::new(0));
    let mut owner = HostSession::open_authorized(
        &path,
        "default",
        true,
        CONTRACT_VERSION,
        authorization.clone(),
    )
    .unwrap();
    owner
        .register_closed_observation_helper(&qualified)
        .unwrap();
    assert_eq!(
        owner.scan_admission_contract().unwrap(),
        ScanAdmissionContract::ExactCleanupV2
    );
    let handle_a = owner.start_scan("media", Permit(a_drops.clone())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let (job, request_a) = loop {
        let store = writer(&path);
        let row=store.db.query_row("SELECT j.id,r.id FROM observation_jobs j JOIN observation_requests r ON r.job_id=j.id WHERE j.state='running'",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).ok();
        drop(store);
        if let Some(row) = row {
            break row;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut wrong =
        HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate()).unwrap();
    wrong
        .register_closed_observation_helper(&qualified)
        .unwrap();
    assert_eq!(
        wrong
            .join_scan_from(&owner, &handle_a, ())
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    wrong.close().unwrap();
    drop(wrong);
    let mut joined = HostSession::open_authorized(
        &path,
        "default",
        true,
        CONTRACT_VERSION,
        authorization.clone(),
    )
    .unwrap();
    joined
        .register_closed_observation_helper(&qualified)
        .unwrap();
    // A fresh scan refuses existing ownership; only explicit exact-owner join may pass.
    assert_eq!(
        joined.start_scan("media", ()).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    let handle_b = joined
        .join_scan_from(&owner, &handle_a, Permit(b_drops.clone()))
        .unwrap();
    loop {
        match joined.poll_scan(&handle_b, Duration::from_millis(50)) {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(error) => {
                let _ = joined.close();
                let store = writer(&path);
                let snapshot = migration::snapshot(&store.db).unwrap();
                let kept = root.keep();
                panic!(
                    "join failed {error:?}; retained fixture {kept:?}; jobs {}; requests {}; companions {}",
                    snapshot["observation_jobs"],
                    snapshot["observation_requests"],
                    snapshot["native_observation_companions"]
                );
            }
        }
        assert!(Instant::now() < deadline);
    }
    assert_eq!(b_drops.load(Ordering::SeqCst), 1);
    assert_eq!(a_drops.load(Ordering::SeqCst), 0);
    joined.close().unwrap();
    drop(joined);
    let store = writer(&path);
    let request_b: String = store
        .db
        .query_row(
            "SELECT id FROM observation_requests WHERE job_id=? AND id!=?",
            params![job, request_a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM observation_jobs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(!companion::has_cleanup_witness(&store, &job).unwrap());
    assert_eq!(store.db.query_row("SELECT count(*) FROM native_observation_companions WHERE state='quiescent' AND owned_job_id IS NULL",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    drop(store);
    let revoked = authorization.begin_revocation().unwrap();
    assert_eq!(
        revoked.wait_drained(Duration::ZERO),
        Err(LeaseError::Timeout)
    );
    assert_eq!(
        owner.cancel_scan(&handle_a).unwrap().unwrap()["state"],
        "cancelled"
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while !owner.drain(Duration::from_millis(50)).unwrap() {
        assert_eq!(a_drops.load(Ordering::SeqCst), 0);
        assert!(Instant::now() < deadline);
    }
    assert_eq!(a_drops.load(Ordering::SeqCst), 1);
    revoked.wait_drained(Duration::ZERO).unwrap();
    let store = writer(&path);
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT state FROM observation_requests WHERE id=?",
                [request_b],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "active"
    );
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
    assert!(companion::has_cleanup_witness(&store, &job).unwrap());
    assert!(
        native_worker_inventory::inspect(&store)
            .unwrap()
            .admits_new_work()
    );
}

#[test]
#[ignore = "requires the pre-shared-command candidate artifact in CATABOLIC_PRE_SHARED_HELPER"]
fn native37_pre_shared_helper_rejects_join_command_before_catalog_open() {
    let binary = PathBuf::from(
        std::env::var_os("CATABOLIC_PRE_SHARED_HELPER")
            .expect("archived pre-shared artifact required"),
    );
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("must-not-exist");
    let output = std::process::Command::new(binary)
        .arg("--db")
        .arg(&path)
        .args([observation::SHARED_WORKER_COMMAND, "{}"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("supported commands"));
    assert!(!path.exists());
}

#[test]
#[ignore = "requires rebuilt exact candidate helper in CATABOLIC_HOST_TEST_HELPER; run this test alone"]
fn native37_publication_writer_contention_exhausts_fixed_bound_without_replacement() {
    let binary = binary();
    let qualified = grant(&binary);
    let (root, path) = fixture();
    for n in 12..60 {
        let dir = root.path().join(format!("source/dir-{n}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("one.txt"), "one").unwrap();
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host =
        HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate()).unwrap();
    host.register_closed_observation_helper(&qualified).unwrap();
    let handle = host.start_scan("media", Permit(drops.clone())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let held = loop {
        let store = writer(&path);
        let started: bool = store
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM observation_batches)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        if started {
            assert_eq!(
                store
                    .db
                    .query_row("SELECT state FROM observation_jobs", [], |r| r
                        .get::<_, String>(0))
                    .unwrap(),
                "running"
            );
            break store;
        }
        drop(store);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    // Intentional fixture contention: exceed the existing single5s publication
    // acquisition deadline; never retry an already-published result.
    std::thread::sleep(Duration::from_millis(6200));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(held);
    let deadline = Instant::now() + Duration::from_secs(20);
    let result = loop {
        if let Some(value) = host.poll_scan(&handle, Duration::from_millis(50)).unwrap() {
            break value;
        }
        assert!(Instant::now() < deadline);
    };
    assert_eq!(result["complete"], false);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    host.close().unwrap();
    let store = writer(&path);
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM observation_jobs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT state FROM observation_jobs", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "failed"
    );
    let report: String = store
        .db
        .query_row("SELECT report FROM observation_jobs", [], |r| r.get(0))
        .unwrap();
    assert!(report.contains("another Catabolic writer"));
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM native_observation_companions",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn shared_command_is_required_and_malformed_claims_fail_before_open() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("missing");
    assert!(
        observation::worker_v2(&path, "default", &json!({"_native_shared_claim":{}}))
            .unwrap_err()
            .0
            .contains("versioned command")
    );
    assert!(observation::shared_worker_v1(&path, "default", &json!({})).is_err());
    assert!(
        observation::shared_worker_v1(
            &path,
            "default",
            &json!({"_native_shared_claim":{"owned":true}})
        )
        .is_err()
    );
    assert!(!path.exists());
}
