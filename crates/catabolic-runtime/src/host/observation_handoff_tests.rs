//! Actual closed-helper handoff regression. No synthetic helper protocol,
//! observation row, claimed PID, or cleanup witness is introduced by these tests.
use super::*;
use crate::helper::{ClosedObservationProtocol, TrustedClosedObservationArtifact};
use catabolic_core::catalog_host_lease::LeaseError;
use std::{
    fs,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

static TESTS: Mutex<()> = Mutex::new(());
struct Permit(Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
struct Fixture {
    host: HostSession,
    gate: HostAuthorization,
    path: PathBuf,
    _root: tempfile::TempDir,
}
fn fixture() -> Fixture {
    let binary = PathBuf::from(
        std::env::var_os("CATABOLIC_HOST_TEST_HELPER")
            .expect("actual packaging-verified CATABOLIC_HOST_TEST_HELPER is required"),
    );
    let grant = TrustedClosedObservationArtifact::after_packaging_verification(
        &binary,
        &catabolic_core::sha256(fs::read(&binary).unwrap()),
        ClosedObservationProtocol::NoChildWithExactJoinV2,
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    catabolic_store::migration::initialize_native_at(&path, 40).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("one.txt"), b"actual observed bytes").unwrap();
    let gate = HostAuthorization::new("handoff-fixture".into(), "revision-1".into()).unwrap();
    let mut host =
        HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate.clone())
            .unwrap();
    host.register_source("media", &source, &[]).unwrap();
    host.register_closed_observation_helper(&grant).unwrap();
    Fixture {
        host,
        gate,
        path,
        _root: root,
    }
}
fn prepared(
    f: &mut Fixture,
    drops: Arc<AtomicUsize>,
) -> crate::observation::PreparedObservation<Box<dyn Send>> {
    let lease = f.host.acquire_authorization().unwrap();
    crate::observation::prepare_supervised(
        f.host.session.store_mut().unwrap(),
        "default",
        &json!({"source":"media","isolate":true}),
        Box::new((Permit(drops), lease)) as Box<dyn Send>,
    )
    .unwrap()
}
fn companion(f: &Fixture) -> (String, Option<i64>, Option<String>) {
    // Independent read-only inspection is evidence, never cleanup authority.
    let db =
        rusqlite::Connection::open_with_flags(&f.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    db.query_row(
        "SELECT state,child_pid,cleanup_kind FROM native_observation_companions",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}
fn finish(host: &mut HostSession, handle: &ScanHandle) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(value) = host.poll_scan(handle, Duration::from_millis(50)).unwrap() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "real helper did not reach terminal join"
        );
    }
}

#[test]
fn prepared_drop_before_and_after_detach_never_fabricates_cleanup() {
    let _serial = TESTS.lock().unwrap();
    for detach in [false, true] {
        let mut f = fixture();
        let drops = Arc::new(AtomicUsize::new(0));
        let token = prepared(&mut f, drops.clone());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        if detach {
            f.host.session.detach().unwrap();
            f.host.detached = true;
        }
        drop(token);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(companion(&f), ("reserved".into(), None, None));
        assert!(f.host.active_scan().is_none());
        if detach {
            f.host.session.reconnect().unwrap();
            f.host.detached = false;
        }
        assert!(!f.host.worker_inventory().unwrap().admits_new_work());
        assert_eq!(
            f.host.start_scan("media", ()).unwrap_err().code,
            ErrorCode::RecoveryRequired
        );
        // No test SQL removes or upgrades the retained reservation.
        f.host.close().unwrap();
    }
}

#[test]
fn prepared_spawn_failure_retains_reservation_and_closes_admission() {
    let _serial = TESTS.lock().unwrap();
    let mut f = fixture();
    let drops = Arc::new(AtomicUsize::new(0));
    let missing = f._root.path().join("does-not-exist").join("helper");
    let token = prepared(&mut f, drops.clone()).with_missing_executable_for_test(missing);
    let error = f.host.start_prepared(token).unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryRequired);
    assert_eq!(f.host.lifecycle(), Lifecycle::Draining);
    assert!(f.host.active_scan().is_none());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(companion(&f), ("reserved".into(), None, None));
    assert!(f.host.start_scan("media", ()).is_err());
    f.host.close().unwrap();
}

#[test]
fn actual_helper_progresses_during_delayed_parent_and_retains_lease_until_join() {
    let _serial = TESTS.lock().unwrap();
    let mut f = fixture();
    let drops = Arc::new(AtomicUsize::new(0));
    let handle = f.host.start_scan("media", Permit(drops.clone())).unwrap();
    assert!(f.host.detached);
    assert!(
        f.host.session.store().is_err(),
        "parent must not retain/reconnect a writer after spawn"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    // Deliberately longer than the child's existing five-second writer admission
    // bound. No parent poll/reconnect runs during this continuation delay.
    std::thread::sleep(Duration::from_millis(5_200));
    assert!(f.host.detached);
    assert!(f.host.session.store().is_err());
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let state = companion(&f);
        if state.0 == "completed" {
            assert!(state.1.is_some());
            assert!(state.2.is_none(), "child completion alone is not cleanup");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "child made no progress while parent detached: {state:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Even an exited-but-unjoined child retains the exact resource/gate lease.
    let revoke = f.gate.begin_revocation().unwrap();
    assert_eq!(
        revoke.wait_drained(Duration::ZERO),
        Err(LeaseError::Timeout)
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    // Cancellation uses the retained owner, not a new grant from the revoked
    // gate. Completion may win; this test never labels that race cancelled.
    let _ = f.host.cancel_scan(&handle).unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    finish(&mut f.host, &handle);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    revoke.wait_drained(Duration::ZERO).unwrap();
    let state = companion(&f);
    assert_eq!(state.0, "quiescent");
    assert_eq!(state.2.as_deref(), Some("closed_child_exit_v1"));
    assert!(!f.host.detached);
    assert!(f.host.start_scan("media", ()).is_err());
    f.host.close().unwrap();
}

#[test]
fn invalid_prepare_and_revoked_admission_do_not_create_a_companion() {
    let _serial = TESTS.lock().unwrap();
    let mut f = fixture();
    let drops = Arc::new(AtomicUsize::new(0));
    let bad = crate::observation::prepare_supervised(
        f.host.session.store_mut().unwrap(),
        "default",
        &json!([]),
        Permit(drops.clone()),
    );
    assert!(bad.is_err());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let count = || {
        let db = rusqlite::Connection::open_with_flags(
            &f.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row(
            "SELECT count(*) FROM native_observation_companions",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    };
    assert_eq!(count(), 0);
    let revoke = f.gate.begin_revocation().unwrap();
    revoke.wait_drained(Duration::ZERO).unwrap();
    assert_eq!(
        f.host.start_scan("media", ()).unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(count(), 0);
    f.host.close().unwrap();
}

#[test]
fn actual_joined_observation_copy_retains_exact_historical_closure() {
    let _serial = TESTS.lock().unwrap();
    let mut f = fixture();
    let drops = Arc::new(AtomicUsize::new(0));
    let handle = f.host.start_scan("media", Permit(drops.clone())).unwrap();
    let _completed = finish(&mut f.host, &handle);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let before = companion(&f);
    assert_eq!(before.0, "quiescent");
    assert!(before.2.is_some());
    let output = f._root.path().join("private-export");
    fs::create_dir(&output).unwrap();
    let cancelled = AtomicBool::new(false);
    let mut export = f
        .host
        .export_snapshot(f.host.catalog_generation().unwrap(), &output, &cancelled)
        .unwrap();
    let mut copied = Vec::new();
    export.copy_verified_to(&mut copied, &cancelled).unwrap();
    let inspected = catabolic_store::catalog_backup::image_read::inspect(
        copied.as_slice(),
        export.bytes(),
        0,
        &cancelled,
    )
    .unwrap();
    assert_eq!(inspected.sha256(), export.sha256());
    assert_eq!(inspected.worker_history().unwrap().owner_count(), 1);
    assert_eq!(
        inspected.worker_history().unwrap().origin_incarnation(),
        inspected.incarnation()
    );
    assert_eq!(companion(&f), before, "inspection must not mint cleanup");
    assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
    drop(export);
    f.host.close().unwrap();
}
