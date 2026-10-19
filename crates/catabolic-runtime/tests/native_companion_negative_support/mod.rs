//! Test-only deliberate protocol failures. Enrolling these control programs is
//! not production packaging verification and cannot qualify positive cleanup.
use catabolic_core::catalog_host_lease::HostAuthorization;
use catabolic_runtime::{
    helper::{ClosedObservationProtocol, TrustedClosedObservationArtifact},
    host::{CONTRACT_VERSION, ErrorCode, HostSession, ScanAdmissionContract},
};
use catabolic_store::{Store, migration, native_worker_inventory};
use rusqlite::params;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
pub struct Permit(pub Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
pub fn gate() -> HostAuthorization {
    HostAuthorization::new("negative-only-control".into(), "r1".into()).unwrap()
}
pub fn fixture() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, 37).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let metadata = source.metadata().unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store
        .transaction(|db| {
            db.execute("INSERT INTO locations VALUES('media')", [])?;
            db.execute(
                "INSERT INTO bindings VALUES('default','source','media',?,?,?)",
                params![
                    source.to_str().unwrap(),
                    metadata.dev() as i64,
                    metadata.ino() as i64
                ],
            )?;
            Ok(())
        })
        .unwrap();
    drop(store);
    (root, path)
}
pub fn control_grant(root: &Path, program: &str) -> TrustedClosedObservationArtifact {
    let path = root.join("negative-control");
    fs::write(&path, program).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    // Deliberate test-only enrollment of a failing control. This fixture never
    // emits a valid acknowledgment or qualifies a production closed artifact.
    TrustedClosedObservationArtifact::after_packaging_verification(
        &path,
        &catabolic_core::sha256(fs::read(&path).unwrap()),
        ClosedObservationProtocol::NoChildV1,
    )
    .unwrap()
}
pub fn assert_reserved_and_reopen_refused(path: &Path) {
    let store = Store::open(path, false, false).unwrap();
    let row: (String, Option<i64>, Option<String>, Option<String>) = store
        .db
        .query_row(
            "SELECT state,child_pid,cleanup_kind,owned_job_id FROM native_observation_companions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(row, ("reserved".into(), None, None, None));
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
    for table in ["observation_jobs", "observation_requests", "scans"] {
        assert_eq!(
            store
                .db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    let inventory = native_worker_inventory::inspect(&store).unwrap();
    assert!(!inventory.truncated);
    assert!(!inventory.admits_new_work());
    assert_eq!(inventory.unresolved.len(), 1);
    assert_eq!(inventory.unresolved[0].kind, "observation_companion");
    drop(store);
    let mut reopened =
        HostSession::open_authorized(path, "default", true, CONTRACT_VERSION, gate()).unwrap();
    assert_eq!(
        reopened.scan_admission_contract().unwrap(),
        ScanAdmissionContract::ExactCleanupV2
    );
    assert_eq!(
        reopened.start_scan("media", ()).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    reopened.close().unwrap();
}
