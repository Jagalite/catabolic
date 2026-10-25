//! Real closed40 -> pending41 and native-to-Store conversion, without a reopen.
use super::*;
use crate::migration;
use std::time::Duration;

fn quiescence() -> NativeUpgradeQuiescence {
    NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}
fn prepared() -> (tempfile::TempDir, PathBuf, PreparedOfflineBootstrap) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize_native_at(&path, 40).unwrap();
    let owner = PreparedOfflineBootstrap::prepare(&path, quiescence()).unwrap();
    (root, path, owner)
}
fn admitted(mut owner: PreparedOfflineBootstrap) -> OfflineApplyOperation {
    owner.admit_apply_operation(None, Instant::now() + Duration::from_secs(60)).unwrap();
    owner.begin_apply().unwrap()
}
fn assert_custody(owner: &PreparedOfflineBootstrap, path: &Path, connection: usize) {
    assert_eq!(owner.owner.apply_custody_for_test().0, connection);
    assert!(crate::writer_lock(path).is_err());
    assert!(crate::native_admission_exclusive(path).is_err());
}
fn failure(operation: OfflineApplyOperation) -> OfflineApplyFailure {
    match operation.finish() {
        Err(failure) => failure,
        Ok(_) => panic!("failed apply minted a bootstrap owner"),
    }
}
fn version(owner: &PreparedOfflineBootstrap) -> u32 {
    owner.owner.db.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap()
}
fn assert_released(path: &Path) {
    drop(crate::writer_lock(path).unwrap());
    drop(crate::native_admission_exclusive(path).unwrap());
}

#[test]
fn borrowed_apply_success_transfers_exact_connection_and_preservation_without_polling() {
    let (_root, path, owner) = prepared();
    let connection = owner.owner.apply_custody_for_test().0;
    let plan = owner.approved_plan().clone();
    let mut owner = match owner.begin_apply() {
        Err(failure) => failure.into_prepared(),
        Ok(_) => panic!("unadmitted application began"),
    };
    assert_custody(&owner, &path, connection);
    owner.admit_apply_operation(None, Instant::now() + Duration::from_secs(60)).unwrap();
    let mut operation = crate::embedding::with_interrupt_check(
        || panic!("begin must not poll"),
        || Ok(owner.begin_apply().unwrap()),
    ).unwrap();
    operation.apply(&AtomicBool::new(false)).unwrap();
    assert_custody(&operation.owner, &path, connection);
    assert_eq!(version(&operation.owner), 41);
    assert!(operation.owner.owner.db.is_autocommit());
    assert!(operation.owner.owner.apply_has_backup_for_test());
    let (catalog, incarnation) = operation.owner.owner.apply_custody_for_test().1.unwrap();
    let bootstrap = crate::embedding::with_interrupt_check(
        || panic!("finish must not poll"),
        || Ok(operation.finish().unwrap()),
    ).unwrap();
    assert_eq!(bootstrap.session.completion_custody_for_test(), (connection, catalog, incarnation));
    let (allocation, conversion_strings) = bootstrap.session.apply_conversion_allocation_for_test();
    assert_eq!(conversion_strings, 1920);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), conversion_strings);
    assert_eq!(bootstrap.approved_plan(), &plan);
    bootstrap.session.completion_store_for_test().validate_native_identity().unwrap();
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), conversion_strings);
    assert!(bootstrap.verified_backup_path().unwrap().exists());
    assert!(crate::writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
    drop(bootstrap);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
    assert_released(&path);
    assert!(crate::Store::open(&path, true, false).is_err());
}

#[test]
fn borrowed_apply_storage_refusal_does_not_move_or_begin_original_owner() {
    let (_root, path, mut owner) = prepared();
    let connection = owner.owner.apply_custody_for_test().0;
    let full = owner.owner.fill_apply_headroom_for_test().unwrap();
    assert!(owner.admit_apply_operation(None, Instant::now() + Duration::from_secs(60)).is_err());
    assert!(owner.apply_admission.is_none());
    assert_custody(&owner, &path, connection);
    assert_eq!(version(&owner), 40);
    assert!(owner.owner.db.is_autocommit());
    drop(full);
    let failure = failure(admitted(owner));
    assert_custody(&failure._owner, &path, connection);
    drop(failure);
    assert_released(&path);
}

#[test]
fn borrowed_apply_readmission_binds_backup_path_and_only_tightens_deadline() {
    let (_root, path, mut owner) = prepared();
    let connection = owner.owner.apply_custody_for_test().0;
    let deadline = Instant::now() + Duration::from_secs(60);
    owner.admit_apply_operation(None, deadline).unwrap();
    owner.admit_apply_operation(None, deadline + Duration::from_secs(60)).unwrap();
    assert_eq!(owner.apply_admission.as_ref().unwrap().deadline, deadline);
    assert!(owner.admit_apply_operation(Some(path.parent().unwrap()), deadline).is_err());
    let expired = Instant::now();
    owner.admit_apply_operation(None, expired).unwrap();
    owner.admit_apply_operation(None, deadline).unwrap();
    assert_eq!(owner.apply_admission.as_ref().unwrap().deadline, expired);
    let mut operation = owner.begin_apply().unwrap();
    assert!(operation.apply(&AtomicBool::new(false)).is_err());
    assert!(matches!(operation.state, ApplyState::Failed));
    assert!(operation.apply(&AtomicBool::new(false)).is_err());
    let failure = failure(operation);
    assert_custody(&failure._owner, &path, connection);
    assert_eq!(version(&failure._owner), 40);
    assert!(!failure._owner.owner.apply_has_backup_for_test());
    drop(failure);
    assert_released(&path);
}

#[test]
fn borrowed_apply_cancelled_and_unattempted_handoffs_retain_custody() {
    for cancelled in [false, true] {
        let (_root, path, owner) = prepared();
        let connection = owner.owner.apply_custody_for_test().0;
        let mut operation = admitted(owner);
        if cancelled {
            assert!(operation.apply(&AtomicBool::new(true)).is_err());
        }
        let failure = crate::embedding::with_interrupt_check(
            || panic!("failure handoff must not poll"),
            || Ok(failure(operation)),
        ).unwrap();
        assert_custody(&failure._owner, &path, connection);
        assert_eq!(version(&failure._owner), 40);
        drop(failure);
        assert_released(&path);
    }
}

#[test]
fn borrowed_apply_real_plan_failure_does_not_release_original_owner() {
    let (_root, path, mut owner) = prepared();
    let connection = owner.owner.apply_custody_for_test().0;
    owner.plan["plan_fingerprint"] = serde_json::json!("different");
    let mut operation = admitted(owner);
    assert!(operation.apply(&AtomicBool::new(false)).is_err());
    assert!(operation.apply(&AtomicBool::new(false)).is_err());
    let failure = failure(operation);
    assert_custody(&failure._owner, &path, connection);
    assert_eq!(version(&failure._owner), 40);
    drop(failure);
    assert_released(&path);
}

#[test]
fn borrowed_apply_unwinds_keep_real_transaction_backup_and_preservation() {
    // Pre-producer, source precommit, source durable postcommit, pending inventory
    // in-transaction, and borrowed conversion: no panic can consume old custody.
    for fault in [1, 2, 3, 6, 8, 11] {
        let (_root, path, owner) = prepared();
        let connection = owner.owner.apply_custody_for_test().0;
        let mut operation = admitted(owner);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            operation.apply_inner(&AtomicBool::new(false), fault)
        }));
        assert!(result.is_err(), "fault {fault} did not unwind");
        assert!(matches!(operation.state, ApplyState::Failed));
        assert!(operation.apply(&AtomicBool::new(false)).is_err());
        assert_custody(&operation.owner, &path, connection);
        if fault != 1 {
            assert_eq!(operation.owner.owner.apply_has_backup_for_test(), fault != 11);
            assert!(operation.owner.owner.apply_custody_for_test().1.is_some());
        }
        if [2, 6].contains(&fault) {
            assert!(!operation.owner.owner.db.is_autocommit(), "unsettled transaction was erased");
        }
        if [3, 6, 8].contains(&fault) {
            assert_eq!(version(&operation.owner), 41, "durable schema was disguised as old owner");
        }
        let identity = operation.owner.owner.apply_custody_for_test();
        let failure = failure(operation);
        assert_eq!(failure._owner.owner.apply_custody_for_test(), identity);
        assert_custody(&failure._owner, &path, connection);
        drop(failure);
        assert_released(&path);
        if fault == 2 {
            let db = crate::connect(&path, false).unwrap();
            assert_eq!(db.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0)).unwrap(), 40);
        }
    }
}

#[test]
fn borrowed_apply_errors_and_postcommit_cancellation_never_publish_grants() {
    // Native precommit error, durable postcommit cancellation, pending inventory
    // failure with BEGIN still active, actual shared validation refusal, and
    // cancellation after conversion verification, and dedicated identity-string
    // reservation refusal before construction retain the exact original.
    for fault in [4, 5, 7, 9, 10, 12, 13] {
        let (_root, path, owner) = prepared();
        let connection = owner.owner.apply_custody_for_test().0;
        let mut operation = admitted(owner);
        assert!(operation.apply_inner(&AtomicBool::new(false), fault).is_err());
        assert!(matches!(operation.state, ApplyState::Failed));
        assert!(operation.apply(&AtomicBool::new(false)).is_err());
        assert_custody(&operation.owner, &path, connection);
        assert!(operation.owner.owner.apply_has_backup_for_test());
        assert!(operation.owner.owner.apply_custody_for_test().1.is_some());
        assert_eq!(version(&operation.owner), if [4, 12].contains(&fault) { 40 } else { 41 });
        if fault == 7 {
            assert!(!operation.owner.owner.db.is_autocommit());
        }
        let identity = operation.owner.owner.apply_custody_for_test();
        let failure = failure(operation);
        assert_eq!(failure._owner.owner.apply_custody_for_test(), identity);
        assert_custody(&failure._owner, &path, connection);
        drop(failure);
        assert_released(&path);
    }
}

#[test]
fn borrowed_apply_legacy_consuming_api_cannot_bypass_admitted_operation() {
    let (_root, path, mut owner) = prepared();
    owner.admit_apply_operation(None, Instant::now()).unwrap();
    assert!(owner.apply(None).is_err());
    // Legacy Result remains explicitly close-on-error, not retained failure.
    assert_released(&path);
    let db = crate::connect(&path, false).unwrap();
    assert_eq!(db.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0)).unwrap(), 40);
}

#[test]
fn borrowed_apply_real_backup_refusal_retains_captured_original_preservation() {
    let (root, path, mut owner) = prepared();
    let connection = owner.owner.apply_custody_for_test().0;
    let not_a_directory = root.path().join("not-a-backup-directory");
    std::fs::write(&not_a_directory, b"occupied").unwrap();
    owner.admit_apply_operation(
        Some(&not_a_directory), Instant::now() + Duration::from_secs(60),
    ).unwrap();
    let mut operation = owner.begin_apply().unwrap();
    assert!(operation.apply(&AtomicBool::new(false)).is_err());
    assert!(operation.owner.owner.apply_custody_for_test().1.is_some());
    assert!(!operation.owner.owner.apply_has_backup_for_test());
    let identity = operation.owner.owner.apply_custody_for_test();
    let failure = failure(operation);
    assert_eq!(failure._owner.owner.apply_custody_for_test(), identity);
    assert_custody(&failure._owner, &path, connection);
    assert_eq!(version(&failure._owner), 40);
    drop(failure);
    assert_released(&path);
}

#[path = "control_workspace_tests.rs"]
mod control_workspace_tests;
