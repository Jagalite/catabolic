//! Source-only fixtures for real acquisition/setup/planning and move-only custody.
#[path = "producer_exit_tests.rs"]
mod producer_exit_tests;
#[path = "refresh_tests.rs"]
mod refresh_tests;
#[path = "source_locked_refresh_tests.rs"]
mod source_locked_refresh_tests;
#[path = "split_lock_tests.rs"]
mod split_lock_tests;
use super::*;
use crate::migration;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    time::Duration,
};

fn quiescence() -> NativeUpgradeQuiescence {
    NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}
fn source(version: u32) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize_native_at(&path, version).unwrap();
    (root, path)
}
fn admitted(path: &Path) -> OfflinePreparationOperation {
    OfflinePreparationOperation::admit(
        path,
        quiescence(),
        OfflineAllocationReservation::default(),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
fn failure(operation: OfflinePreparationOperation) -> OfflinePreparationFailure {
    match operation.finish() {
        Err(failure) => failure,
        Ok(_) => panic!("failed preparation minted Prepared"),
    }
}
fn held_descriptors(operation: &OfflinePreparationOperation) {
    let (_, writer, admission) = operation.owner.custody_for_test();
    for fd in [writer, admission].into_iter().flatten() {
        // SAFETY: a read-only descriptor query; the inspected operation owns it.
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
    }
}
fn released(path: &Path) {
    drop(crate::writer_lock(path).unwrap());
    drop(crate::native_admission_exclusive(path).unwrap());
}
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

#[test]
fn borrowed_preparation_admission_has_no_native_resources_and_refuses_impossible_storage() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("not-created.db");
    let operation = admitted(&missing);
    assert_eq!(operation.owner.custody_for_test(), (None, None, None));
    assert!(!sidecar(&missing, ".lock").exists());
    assert!(!sidecar(&missing, ".native-admission.lock").exists());
    let allocation = operation.owner.allocation_for_test();
    assert_eq!(allocation.preparation_state_for_test().unwrap().1, None);
    assert_eq!(
        allocation.temporary_bytes_for_test().unwrap(),
        32 * 1024 * 1024
    );
    let failure = crate::embedding::with_interrupt_check(
        || panic!("unattempted finish must not poll"),
        || Ok(failure(operation)),
    )
    .unwrap();
    held_descriptors(&failure.operation);
    drop(failure);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
    let full = OfflineAllocationReservation::new(224 * 1024 * 1024).unwrap();
    assert!(
        OfflinePreparationOperation::admit(&missing, quiescence(), full, Instant::now()).is_err()
    );
    assert!(!sidecar(&missing, ".lock").exists());
    assert!(!sidecar(&missing, ".native-admission.lock").exists());
}

#[test]
fn borrowed_preparation_path_cohort_refuses_before_any_sidecar_or_source() {
    let path = PathBuf::from("x".repeat(8 * 1024 * 1024 + 1));
    let mut operation = admitted(&path);
    let error = operation.prepare(&AtomicBool::new(false)).unwrap_err();
    assert!(error.0.contains("scratch cohort"));
    assert_eq!(operation.owner.custody_for_test(), (None, None, None));
    assert_eq!(operation.phase(), OfflinePreparationPhase::Failed);
    drop(failure(operation));
}

#[test]
fn borrowed_preparation_missing_symlink_and_hardlink_paths_fail_before_native_acquisition() {
    let (root, path) = source(40);
    let missing = root.path().join("missing.db");
    let symlink = root.path().join("symlink.db");
    std::os::unix::fs::symlink(&path, &symlink).unwrap();
    for rejected in [&missing, &symlink] {
        let mut operation = admitted(rejected);
        assert!(operation.prepare(&AtomicBool::new(false)).is_err());
        assert_eq!(operation.owner.custody_for_test(), (None, None, None));
        drop(failure(operation));
    }
    let alias = root.path().join("alias.db");
    std::fs::hard_link(&path, &alias).unwrap();
    let mut operation = admitted(&path);
    assert!(operation.prepare(&AtomicBool::new(false)).is_err());
    assert_eq!(operation.owner.custody_for_test(), (None, None, None));
    drop(failure(operation));
}

#[test]
fn borrowed_preparation_lock_metadata_failure_retains_successfully_opened_file() {
    for suffix in [".native-admission.lock", ".lock"] {
        let (root, path) = source(40);
        let lock = sidecar(&path, suffix);
        std::fs::write(&lock, b"").unwrap();
        std::fs::hard_link(&lock, root.path().join("lock-alias")).unwrap();
        let mut operation = admitted(&path);
        assert!(operation.prepare(&AtomicBool::new(false)).is_err());
        let custody = operation.owner.custody_for_test();
        assert_eq!(custody.0, None);
        assert_eq!(custody.1.is_some(), suffix == ".lock");
        assert!(custody.2.is_some());
        let failure = failure(operation);
        assert_eq!(failure.operation.owner.custody_for_test(), custody);
        held_descriptors(&failure.operation);
        if suffix == ".lock" {
            assert!(crate::native_admission_exclusive(&path).is_err());
        }
    }
}

#[test]
fn borrowed_preparation_busy_lifecycle_and_writer_failures_retain_partial_real_owners() {
    let (_root, path) = source(40);
    let original = crate::native_admission_exclusive(&path).unwrap();
    let mut operation = admitted(&path);
    assert!(operation.prepare(&AtomicBool::new(false)).is_err());
    let (_, writer, lifecycle) = operation.owner.custody_for_test();
    assert!(writer.is_none() && lifecycle.is_some());
    let failed = failure(operation);
    held_descriptors(&failed.operation);
    drop(original);
    // The failed raw descriptor is not misrepresented as a lifecycle lease.
    drop(crate::native_admission_exclusive(&path).unwrap());
    drop(failed);

    let original = crate::writer_lock(&path).unwrap();
    let mut operation = admitted(&path);
    assert!(operation.prepare(&AtomicBool::new(false)).is_err());
    let custody = operation.owner.custody_for_test();
    assert!(custody.0.is_none() && custody.1.is_some() && custody.2.is_some());
    let failed = failure(operation);
    held_descriptors(&failed.operation);
    assert!(crate::native_admission_exclusive(&path).is_err());
    drop(original);
    drop(crate::writer_lock(&path).unwrap());
    assert!(crate::native_admission_exclusive(&path).is_err());
    drop(failed);
    released(&path);
}

#[test]
fn borrowed_preparation_corrupt_setup_retains_original_open_connection_and_leases() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("corrupt.db");
    std::fs::write(&path, vec![b'x'; 8192]).unwrap();
    let mut operation = admitted(&path);
    assert!(operation.prepare(&AtomicBool::new(false)).is_err());
    let custody = operation.owner.custody_for_test();
    assert!(custody.0.is_some() && custody.1.is_some() && custody.2.is_some());
    let allocation = operation.owner.allocation_for_test();
    assert_eq!(
        allocation.preparation_state_for_test().unwrap().1,
        custody.0
    );
    let failed = failure(operation);
    assert_eq!(failed.operation.owner.custody_for_test(), custody);
    held_descriptors(&failed.operation);
    assert!(crate::writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
    drop(failed);
    released(&path);
}

#[test]
fn borrowed_preparation_real_plan_refusal_keeps_open_transaction_without_retry() {
    let (_root, path) = source(39);
    let mut operation = admitted(&path);
    assert!(operation.prepare(&AtomicBool::new(false)).is_err());
    let custody = operation.owner.custody_for_test();
    assert!(custody.0.is_some());
    assert!(
        !operation
            .owner
            .connection_for_test()
            .unwrap()
            .is_autocommit()
    );
    assert!(operation.prepare(&AtomicBool::new(false)).is_err());
    let failed = failure(operation);
    assert_eq!(failed.operation.owner.custody_for_test(), custody);
    assert!(
        !failed
            .operation
            .owner
            .connection_for_test()
            .unwrap()
            .is_autocommit()
    );
    assert!(crate::writer_lock(&path).is_err());
    drop(failed);
    released(&path);
}

#[test]
fn borrowed_preparation_bind_budget_refusal_keeps_same_context_and_stored_connection() {
    let (_root, path) = source(40);
    let mut operation = admitted(&path);
    let allocation = operation.owner.allocation_for_test();
    let before = allocation.preparation_state_for_test().unwrap();
    assert!(
        operation
            .prepare_inner(&AtomicBool::new(false), 16)
            .is_err()
    );
    let custody = operation.owner.custody_for_test();
    assert!(custody.0.is_some());
    let after = allocation.preparation_state_for_test().unwrap();
    assert_eq!(before.0, after.0);
    assert_eq!(after.1, custody.0);
    let failed = failure(operation);
    held_descriptors(&failed.operation);
    assert!(crate::writer_lock(&path).is_err());
    drop(failed);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
    released(&path);
}

#[test]
fn borrowed_preparation_real_named_file_change_fails_with_completed_plan_retained() {
    let (_root, path) = source(40);
    let mut operation = admitted(&path);
    assert!(
        operation
            .prepare_inner(&AtomicBool::new(false), 14)
            .is_err()
    );
    let custody = operation.owner.custody_for_test();
    assert!(operation.owner.plan_for_test().is_some());
    assert!(
        operation
            .owner
            .connection_for_test()
            .unwrap()
            .is_autocommit()
    );
    let failed = failure(operation);
    assert_eq!(failed.operation.owner.custody_for_test(), custody);
    assert!(failed.operation.owner.plan_for_test().is_some());
    assert!(crate::writer_lock(&path).is_err());
    drop(failed);
    released(&path);
}

#[test]
fn borrowed_preparation_unwind_at_each_acquisition_and_planning_boundary_keeps_custody() {
    for fault in 1..=13 {
        let (_root, path) = source(40);
        let mut operation = admitted(&path);
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                operation
                    .prepare_inner(&AtomicBool::new(false), fault)
                    .unwrap();
            }))
            .is_err()
        );
        assert_eq!(operation.phase(), OfflinePreparationPhase::Failed);
        let custody = operation.owner.custody_for_test();
        assert!(custody.2.is_some());
        assert_eq!(custody.1.is_some(), fault >= 3);
        assert_eq!(custody.0.is_some(), fault >= 5);
        if fault >= 2 {
            assert!(crate::native_admission_exclusive(&path).is_err());
        }
        if fault >= 4 {
            assert!(crate::writer_lock(&path).is_err());
        }
        if fault >= 5 {
            assert_eq!(
                operation
                    .owner
                    .connection_for_test()
                    .unwrap()
                    .is_autocommit(),
                !(8..=10).contains(&fault)
            );
        }
        assert_eq!(operation.owner.plan_for_test().is_some(), fault >= 9);
        assert!(operation.prepare(&AtomicBool::new(false)).is_err());
        let failed = failure(operation);
        assert_eq!(failed.operation.owner.custody_for_test(), custody);
        held_descriptors(&failed.operation);
        drop(failed);
        released(&path);
    }
}

#[test]
fn borrowed_preparation_cancelled_and_expired_deadlines_never_start_acquisition() {
    for cancelled in [false, true] {
        let (_root, path) = source(40);
        let mut operation = OfflinePreparationOperation::admit(
            &path,
            quiescence(),
            OfflineAllocationReservation::default(),
            if cancelled {
                Instant::now() + Duration::from_secs(60)
            } else {
                Instant::now()
            },
        )
        .unwrap();
        assert!(operation.prepare(&AtomicBool::new(cancelled)).is_err());
        assert_eq!(operation.owner.custody_for_test(), (None, None, None));
        assert!(operation.prepare(&AtomicBool::new(false)).is_err());
        drop(failure(operation));
        released(&path);
    }
}

#[test]
fn borrowed_preparation_success_moves_same_connection_context_and_durable_storage_into_store() {
    let (_root, path) = source(40);
    let mut operation = admitted(&path);
    let allocation = operation.owner.allocation_for_test();
    let initial = allocation.preparation_state_for_test().unwrap();
    let deadline = operation.deadline;
    operation.prepare(&AtomicBool::new(false)).unwrap();
    let custody = operation.owner.custody_for_test();
    let before = allocation.preparation_state_for_test().unwrap();
    assert_eq!(initial.0, before.0);
    assert_eq!(before.1, custody.0);
    assert!(before.2 > initial.2);
    let mut prepared = crate::embedding::with_interrupt_check(
        || panic!("finish must not poll"),
        || Ok(operation.finish().unwrap()),
    )
    .unwrap();
    assert_eq!(
        prepared.owner.apply_custody_for_test().0,
        custody.0.unwrap()
    );
    assert_eq!(prepared.identity.producer_deadline, Some(deadline));
    assert_eq!(allocation.preparation_state_for_test().unwrap(), before);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
    prepared
        .admit_apply_operation(None, deadline + Duration::from_secs(60))
        .unwrap();
    let mut apply = prepared.begin_apply().unwrap();
    apply.apply(&AtomicBool::new(false)).unwrap();
    let bootstrap = apply.finish().unwrap();
    assert_eq!(bootstrap.producer_deadline, Some(deadline));
    let (stored, _) = bootstrap.session.apply_conversion_allocation_for_test();
    assert_eq!(stored.preparation_state_for_test().unwrap(), before);
    assert_eq!(
        bootstrap.session.completion_custody_for_test().0,
        custody.0.unwrap()
    );
    assert!(crate::writer_lock(&path).is_err());
    drop(bootstrap);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
    released(&path);
}

#[test]
fn borrowed_preparation_inherited_expiry_cannot_be_widened_by_fresh_apply_admission() {
    let (_root, path) = source(40);
    let mut operation = admitted(&path);
    operation.prepare(&AtomicBool::new(false)).unwrap();
    let mut prepared = operation.finish().unwrap();
    // Simulate elapsed wall time without sleeping or replacing any budget.
    prepared.identity.producer_deadline = Some(Instant::now());
    prepared
        .admit_apply_operation(None, Instant::now() + Duration::from_secs(60))
        .unwrap();
    let mut apply = prepared.begin_apply().unwrap();
    assert!(apply.apply(&AtomicBool::new(false)).is_err());
    assert!(apply.finish().is_err());
    released(&path);
}

#[test]
fn borrowed_preparation_marked_owner_cannot_use_consuming_legacy_apply() {
    let (_root, path) = source(40);
    let mut operation = admitted(&path);
    operation.prepare(&AtomicBool::new(false)).unwrap();
    let prepared = operation.finish().unwrap();
    assert!(prepared.apply(None).is_err());
    // This old plain-Result refusal deliberately still closes custody; it is
    // not falsely advertised as a retained failure API.
    released(&path);
    let db = crate::connect(&path, false).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        40
    );
}

fn marked_bootstrap() -> (tempfile::TempDir, PathBuf, OfflineBootstrap<'static>) {
    let (root, path) = source(40);
    let mut preparation = admitted(&path);
    preparation.prepare(&AtomicBool::new(false)).unwrap();
    let mut prepared = preparation.finish().unwrap();
    prepared
        .admit_apply_operation(None, Instant::now() + Duration::from_secs(120))
        .unwrap();
    let mut apply = prepared.begin_apply().unwrap();
    apply.apply(&AtomicBool::new(false)).unwrap();
    (root, path, apply.finish().unwrap())
}
fn command(bootstrap: &OfflineBootstrap<'_>) -> CommandEnvelope {
    let payload = bootstrap.prepare("preparation-fixture", &[]).unwrap();
    CommandEnvelope::new(
        1,
        CommandKey::new(bootstrap.catalog_id(), "preparation-fixture", "bootstrap").unwrap(),
        bootstrap.incarnation(),
        bootstrap.generation().unwrap(),
        &payload.to_string(),
    )
    .unwrap()
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}

#[test]
fn borrowed_preparation_expired_marked_bootstrap_gates_prepare_reconcile_execute_and_completion() {
    let (_root, path, mut bootstrap) = marked_bootstrap();
    let command = command(&bootstrap);
    let before = migration::snapshot(&bootstrap.session.completion_store_for_test().db).unwrap();
    let expired = Instant::now();
    bootstrap.producer_deadline = Some(expired);
    assert!(bootstrap.prepare("preparation-fixture", &[]).is_err());
    assert!(
        bootstrap
            .reconcile(&command, "preparation-fixture")
            .is_err()
    );
    assert!(
        bootstrap
            .execute(&command, "preparation-fixture", &[], policy())
            .is_err()
    );
    assert!(bootstrap.complete(&command).is_err());
    assert_eq!(
        migration::snapshot(&bootstrap.session.completion_store_for_test().db).unwrap(),
        before
    );
    bootstrap
        .admit_completion_operation(&command, Instant::now() + Duration::from_secs(60))
        .unwrap();
    assert_eq!(
        bootstrap.completion_admitted_deadline_for_test(),
        Some(expired)
    );
    let mut completion = bootstrap.begin_completion().unwrap();
    assert!(completion.complete(&AtomicBool::new(false)).is_err());
    let failed = match completion.finish() {
        Err(failed) => failed,
        Ok(_) => panic!("expired inherited deadline produced Complete"),
    };
    assert!(crate::writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
    drop(failed);
    released(&path);
}

#[test]
fn borrowed_preparation_completion_inherits_minimum_deadline_then_mints_fresh_completed_owner() {
    let (_root, path, mut bootstrap) = marked_bootstrap();
    let inherited = bootstrap.producer_deadline.unwrap();
    let command = command(&bootstrap);
    assert!(matches!(
        bootstrap
            .execute(&command, "preparation-fixture", &[], policy())
            .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    assert!(bootstrap.complete(&command).is_err()); // No legacy completion bypass.
    bootstrap
        .admit_completion_operation(&command, inherited + Duration::from_secs(60))
        .unwrap();
    assert_eq!(
        bootstrap.completion_admitted_deadline_for_test(),
        Some(inherited)
    );
    let tighter = inherited - Duration::from_secs(1);
    bootstrap
        .admit_completion_operation(&command, tighter)
        .unwrap();
    bootstrap
        .admit_completion_operation(&command, inherited + Duration::from_secs(60))
        .unwrap();
    assert_eq!(
        bootstrap.completion_admitted_deadline_for_test(),
        Some(tighter)
    );
    let connection = bootstrap.session.completion_custody_for_test();
    let mut completion = bootstrap.begin_completion().unwrap();
    completion.complete(&AtomicBool::new(false)).unwrap();
    let completed = crate::embedding::with_interrupt_check(
        || panic!("completion finish must not poll"),
        || Ok(completion.finish().unwrap()),
    )
    .unwrap();
    assert_eq!(completed.completion_custody_for_test(), connection);
    // CompletedCatalogSession has no inherited producer-deadline field. Later
    // operations select their own existing budgets, not an eternal old deadline.
    assert!(completed.retained_allocations().unwrap() > 0);
    drop(completed);
    released(&path);
}

#[test]
fn borrowed_preparation_marked_bootstrap_rejects_both_consuming_legacy_completions() {
    for image in [false, true] {
        let (_root, path, bootstrap) = marked_bootstrap();
        let command = command(&bootstrap);
        if image {
            assert!(
                bootstrap
                    .into_completed_image(&command, &AtomicBool::new(false))
                    .is_err()
            );
        } else {
            assert!(bootstrap.into_completed_catalog(&command).is_err());
        }
        // Legacy plain-Result refusal closes its consumed owner; no success or
        // retained-failure claim is made for these old APIs.
        released(&path);
    }
}

#[test]
fn borrowed_preparation_unmarked_legacy_constructors_keep_existing_behavior() {
    let (_root, path) = source(40);
    let prepared = PreparedOfflineBootstrap::prepare(&path, quiescence()).unwrap();
    assert!(prepared.identity.producer_deadline.is_none());
    let bootstrap = prepared.apply(None).unwrap();
    assert!(bootstrap.producer_deadline.is_none());
    let command = command(&bootstrap);
    assert!(matches!(
        bootstrap
            .execute(&command, "preparation-fixture", &[], policy())
            .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    assert!(
        bootstrap
            .reconcile(&command, "preparation-fixture")
            .unwrap()
            .is_some()
    );
    drop(bootstrap.complete(&command).unwrap());
    let completed = bootstrap.into_completed_catalog(&command).unwrap();
    drop(completed);
    released(&path);
}

#[test]
fn borrowed_preparation_real_connection_open_failure_retains_prior_lifecycle_and_writer() {
    let (_root, path) = source(40);
    let mut operation = admitted(&path);
    assert!(
        operation
            .prepare_inner(&AtomicBool::new(false), 17)
            .is_err()
    );
    let custody = operation.owner.custody_for_test();
    assert!(custody.0.is_none() && custody.1.is_some() && custody.2.is_some());
    let failed = failure(operation);
    held_descriptors(&failed.operation);
    assert!(crate::writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
    drop(failed);
    released(&path);
}

#[test]
fn borrowed_preparation_real_lifecycle_path_mismatch_retains_original_locked_descriptor() {
    use std::os::fd::AsRawFd;
    let (_root, path) = source(40);
    let mut operation = admitted(&path);
    assert!(
        operation
            .prepare_inner(&AtomicBool::new(false), 18)
            .is_err()
    );
    let custody = operation.owner.custody_for_test();
    assert!(custody.0.is_none() && custody.1.is_none() && custody.2.is_some());
    let original = std::fs::File::open(
        sidecar(&path, ".native-admission.lock").with_extension("retained-original"),
    )
    .unwrap();
    // SAFETY: this test file owns a live descriptor. Its failed acquisition
    // demonstrates the operation still holds the original unlinked-path lease.
    assert_ne!(
        unsafe { libc::flock(original.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let failed = failure(operation);
    held_descriptors(&failed.operation);
    drop(failed);
    // SAFETY: same live test descriptor, now after deliberate custody drop.
    assert_eq!(
        unsafe { libc::flock(original.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
}
