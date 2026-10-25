//! Real cooperative Store/CLI admission at the retained pre-SQLite boundary.
//! These are source-only fixtures here; qualification must execute them separately.
use super::*;
use std::{fs::File, os::fd::AsRawFd, os::unix::fs::MetadataExt};

fn busy_store(path: &Path, writable: bool) {
    let error = crate::Store::open_typed(path, writable, false)
        .err()
        .expect("Store unexpectedly passed native leases");
    assert_eq!(error.code, crate::OpenErrorCode::BusyWriter);
}
fn terminal_locked(operation: &mut OfflinePreparationOperation, source: &File) {
    assert_eq!(operation.phase(), OfflinePreparationPhase::Failed);
    let custody = operation.owner.custody_for_test();
    let allocation = operation.owner.allocation_for_test();
    let counters = allocation.preparation_state_for_test().unwrap();
    let temporary = allocation.temporary_bytes_for_test().unwrap();
    let external = operation.owner.reservation_and_input_for_test().unwrap();
    let deadline = operation.deadline;
    assert!(
        operation
            .acquire_source_locks(&AtomicBool::new(false))
            .is_err()
    );
    assert!(operation.prepare(&AtomicBool::new(false)).is_err());
    assert!(
        operation
            .prepare_locked(source, &AtomicBool::new(false))
            .is_err()
    );
    assert!(
        operation
            .refresh_external_reservation(OfflineAllocationReservation::default())
            .is_err()
    );
    assert_eq!(operation.owner.custody_for_test(), custody);
    assert_eq!(allocation.preparation_state_for_test().unwrap(), counters);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), temporary);
    assert_eq!(
        operation.owner.reservation_and_input_for_test().unwrap(),
        external
    );
    assert_eq!(operation.deadline, deadline);
}

#[test]
fn split_lock_boundary_excludes_actual_store_readers_and_cli_writers_before_sqlite_open() {
    let (_root, path) = source(40);
    let source = File::open(&path).unwrap();
    let source_fd = source.as_raw_fd();
    let bytes = std::fs::read(&path).unwrap();
    let mut operation = admitted(&path);
    operation
        .refresh_external_reservation(OfflineAllocationReservation::new(4096).unwrap())
        .unwrap();
    let allocation = operation.owner.allocation_for_test();
    let initial = allocation.preparation_state_for_test().unwrap();
    let initial_input = operation.owner.reservation_and_input_for_test().unwrap();
    let deadline = operation.deadline;
    operation
        .acquire_source_locks(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(operation.phase(), OfflinePreparationPhase::SourceLocked);
    let custody = operation.owner.custody_for_test();
    assert!(custody.0.is_none() && custody.1.is_some() && custody.2.is_some());
    let locked = allocation.preparation_state_for_test().unwrap();
    assert_eq!(locked.0, initial.0);
    assert_eq!(locked.1, None);
    assert!(locked.2 >= initial.2);
    assert_eq!(
        allocation.temporary_bytes_for_test().unwrap(),
        32 * 1024 * 1024
    );
    assert_eq!(
        operation.owner.reservation_and_input_for_test().unwrap(),
        initial_input
    );
    assert_eq!(operation.deadline, deadline);
    for writable in [false, true] {
        busy_store(&path, writable);
    }
    assert!(crate::writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    // No refresh, legacy preparation, or second acquisition can reset admission.
    assert!(operation.prepare(&AtomicBool::new(false)).is_err());
    assert!(
        operation
            .acquire_source_locks(&AtomicBool::new(false))
            .is_err()
    );
    assert!(
        operation
            .refresh_external_reservation(OfflineAllocationReservation::default())
            .is_err()
    );
    assert_eq!(operation.phase(), OfflinePreparationPhase::SourceLocked);
    assert_eq!(allocation.preparation_state_for_test().unwrap(), locked);
    assert_eq!(operation.owner.custody_for_test(), custody);
    let mut readmission = admitted(&path);
    assert!(
        readmission
            .acquire_source_locks(&AtomicBool::new(false))
            .is_err()
    );
    drop(failure(readmission));
    assert!(PreparedOfflineBootstrap::prepare(&path, quiescence()).is_err());
    assert_eq!(allocation.preparation_state_for_test().unwrap(), locked);
    assert_eq!(operation.owner.custody_for_test(), custody);
    operation
        .prepare_locked(&source, &AtomicBool::new(false))
        .unwrap();
    let prepared_custody = operation.owner.custody_for_test();
    assert!(prepared_custody.0.is_some());
    assert_eq!(
        (prepared_custody.1, prepared_custody.2),
        (custody.1, custody.2)
    );
    let prepared_counters = allocation.preparation_state_for_test().unwrap();
    assert_eq!(prepared_counters.0, initial.0);
    assert_eq!(prepared_counters.1, prepared_custody.0);
    assert_eq!(prepared_counters.2, locked.2);
    assert_eq!(operation.deadline, deadline);
    assert_eq!(source.as_raw_fd(), source_fd);
    assert_eq!(
        operation.owner.reservation_and_input_for_test().unwrap(),
        initial_input
    );
    let prepared = operation.finish().unwrap();
    assert_eq!(prepared.identity.producer_deadline, Some(deadline));
    assert_eq!(prepared.identity.device, source.metadata().unwrap().dev());
    assert_eq!(prepared.identity.inode, source.metadata().unwrap().ino());
    assert_eq!(
        prepared.owner.apply_custody_for_test().0,
        prepared_custody.0.unwrap()
    );
    assert_eq!(
        allocation.preparation_state_for_test().unwrap(),
        prepared_counters
    );
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
    busy_store(&path, true);
    drop(prepared);
    // The supported Store writer can enter once, and only once, custody ends.
    drop(crate::Store::open_typed(&path, true, false).unwrap());
    released(&path);
}

#[test]
fn split_lock_busy_actual_store_is_terminal_without_lock_order_leak() {
    for writable in [false, true] {
        let (_root, path) = source(40);
        let source = File::open(&path).unwrap();
        let store = crate::Store::open_typed(&path, writable, false).unwrap();
        let mut operation = admitted(&path);
        assert!(
            operation
                .acquire_source_locks(&AtomicBool::new(false))
                .is_err()
        );
        let custody = operation.owner.custody_for_test();
        assert!(custody.0.is_none() && custody.1.is_none() && custody.2.is_some());
        terminal_locked(&mut operation, &source);
        drop(store);
        // The failed open admission descriptor never became a lease.
        drop(crate::Store::open_typed(&path, true, false).unwrap());
        held_descriptors(&operation);
        drop(failure(operation));
        released(&path);
    }
}

#[test]
fn split_lock_store_writer_first_refusal_releases_its_writer_without_deadlock() {
    let (_root, path) = source(40);
    let source = File::open(&path).unwrap();
    let mut operation = admitted(&path);
    // Native admission is held, but native writer acquisition has not started.
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            operation
                .acquire_source_locks_inner(&AtomicBool::new(false), 2)
                .unwrap();
        }))
        .is_err()
    );
    let custody = operation.owner.custody_for_test();
    assert!(custody.0.is_none() && custody.1.is_none() && custody.2.is_some());
    // Store obtains .lock first, then fails its shared lifecycle acquisition.
    // Its failed open MUST release .lock despite opposite ordering.
    busy_store(&path, true);
    drop(crate::writer_lock(&path).unwrap());
    terminal_locked(&mut operation, &source);
    assert!(crate::native_admission_exclusive(&path).is_err());
    drop(failure(operation));
    drop(crate::Store::open_typed(&path, true, false).unwrap());
    released(&path);
}

#[test]
fn split_lock_busy_writer_retains_exact_admission_until_explicit_custody_drop() {
    let (_root, path) = source(40);
    let source = File::open(&path).unwrap();
    let competitor = crate::writer_lock(&path).unwrap();
    let mut operation = admitted(&path);
    assert!(
        operation
            .acquire_source_locks(&AtomicBool::new(false))
            .is_err()
    );
    let custody = operation.owner.custody_for_test();
    assert!(custody.0.is_none() && custody.1.is_some() && custody.2.is_some());
    terminal_locked(&mut operation, &source);
    drop(competitor);
    drop(crate::writer_lock(&path).unwrap());
    busy_store(&path, true);
    assert!(crate::native_admission_exclusive(&path).is_err());
    drop(failure(operation));
    released(&path);
}

#[test]
fn split_lock_foreign_file_with_identical_bytes_cannot_bind_or_open_sqlite() {
    let (root, path) = source(40);
    let foreign_path = root.path().join("identical-but-not-held-source.db");
    std::fs::copy(&path, &foreign_path).unwrap();
    let foreign = File::open(&foreign_path).unwrap();
    let actual = File::open(&path).unwrap();
    assert_ne!(
        foreign.metadata().unwrap().ino(),
        actual.metadata().unwrap().ino()
    );
    let mut operation = admitted(&path);
    operation
        .acquire_source_locks(&AtomicBool::new(false))
        .unwrap();
    let custody = operation.owner.custody_for_test();
    let allocation = operation.owner.allocation_for_test();
    let counters = allocation.preparation_state_for_test().unwrap();
    assert!(
        operation
            .prepare_locked(&foreign, &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(operation.owner.custody_for_test(), custody);
    assert_eq!(allocation.preparation_state_for_test().unwrap(), counters);
    terminal_locked(&mut operation, &actual);
    busy_store(&path, true);
    drop(failure(operation));
    released(&path);
}

#[test]
fn split_lock_missing_or_replaced_name_refuses_original_file_before_sqlite() {
    for replacement in [false, true] {
        let (_root, path) = source(40);
        let actual = File::open(&path).unwrap();
        let mut operation = admitted(&path);
        operation
            .acquire_source_locks(&AtomicBool::new(false))
            .unwrap();
        let custody = operation.owner.custody_for_test();
        let moved = path.with_extension("original");
        // Controlled failure injection, not a promise of hostile namespace safety.
        std::fs::rename(&path, &moved).unwrap();
        if replacement {
            std::fs::copy(&moved, &path).unwrap();
        }
        assert!(
            operation
                .prepare_locked(&actual, &AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(operation.owner.custody_for_test(), custody);
        terminal_locked(&mut operation, &actual);
        assert!(crate::writer_lock(&path).is_err());
        assert!(crate::native_admission_exclusive(&path).is_err());
        drop(failure(operation));
        released(&path);
    }
}

#[test]
fn split_lock_acquisition_unwinds_keep_real_partial_owners_and_terminal_state() {
    for fault in [1, 2, 3, 4, 19] {
        let (_root, path) = source(40);
        let source = File::open(&path).unwrap();
        let mut operation = admitted(&path);
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                operation
                    .acquire_source_locks_inner(&AtomicBool::new(false), fault)
                    .unwrap();
            }))
            .is_err()
        );
        let custody = operation.owner.custody_for_test();
        assert!(custody.0.is_none() && custody.2.is_some());
        assert_eq!(custody.1.is_some(), fault >= 3);
        if fault >= 2 {
            assert!(crate::native_admission_exclusive(&path).is_err());
        }
        if fault >= 4 {
            assert!(crate::writer_lock(&path).is_err());
        }
        terminal_locked(&mut operation, &source);
        let failed = failure(operation);
        assert_eq!(failed.operation.owner.custody_for_test(), custody);
        held_descriptors(&failed.operation);
        drop(failed);
        released(&path);
    }
}

#[test]
fn split_lock_preopen_and_continuation_unwinds_keep_same_leases_context_and_deadline() {
    for fault in [20, 5, 6, 7, 8, 9, 10, 11, 12, 13] {
        let (_root, path) = source(40);
        let source = File::open(&path).unwrap();
        let mut operation = admitted(&path);
        operation
            .acquire_source_locks(&AtomicBool::new(false))
            .unwrap();
        let custody = operation.owner.custody_for_test();
        let allocation = operation.owner.allocation_for_test();
        let context = allocation.preparation_state_for_test().unwrap().0;
        let deadline = operation.deadline;
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                operation
                    .prepare_locked_inner(&source, &AtomicBool::new(false), fault)
                    .unwrap();
            }))
            .is_err()
        );
        let failed_custody = operation.owner.custody_for_test();
        assert_eq!((failed_custody.1, failed_custody.2), (custody.1, custody.2));
        assert_eq!(failed_custody.0.is_none(), fault == 20);
        assert_eq!(allocation.preparation_state_for_test().unwrap().0, context);
        assert_eq!(operation.deadline, deadline);
        terminal_locked(&mut operation, &source);
        assert!(crate::writer_lock(&path).is_err());
        assert!(crate::native_admission_exclusive(&path).is_err());
        let failed = failure(operation);
        assert_eq!(failed.operation.owner.custody_for_test(), failed_custody);
        held_descriptors(&failed.operation);
        drop(failed);
        released(&path);
    }
}

#[test]
fn split_lock_expiry_or_cancellation_between_phases_never_reopens_or_resets_deadline() {
    for cancelled in [false, true] {
        let (_root, path) = source(40);
        let source = File::open(&path).unwrap();
        let deadline = Instant::now() + Duration::from_millis(250);
        let mut operation = OfflinePreparationOperation::admit(
            &path,
            quiescence(),
            OfflineAllocationReservation::default(),
            deadline,
        )
        .unwrap();
        operation
            .acquire_source_locks(&AtomicBool::new(false))
            .unwrap();
        let custody = operation.owner.custody_for_test();
        let allocation = operation.owner.allocation_for_test();
        let counters = allocation.preparation_state_for_test().unwrap();
        if !cancelled {
            // Let the actual original deadline expire; do not overwrite it.
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            assert!(Instant::now() >= deadline);
        }
        assert_eq!(operation.deadline, deadline);
        assert!(
            operation
                .prepare_locked(&source, &AtomicBool::new(cancelled))
                .is_err()
        );
        assert_eq!(operation.owner.custody_for_test(), custody);
        assert_eq!(allocation.preparation_state_for_test().unwrap(), counters);
        assert_eq!(operation.deadline, deadline);
        terminal_locked(&mut operation, &source);
        assert!(crate::writer_lock(&path).is_err());
        drop(failure(operation));
        released(&path);
    }
}

#[test]
fn split_lock_continuation_requires_locks_and_locked_finish_never_mints_prepared() {
    let (_root, path) = source(40);
    let source = File::open(&path).unwrap();
    let mut inputs = admitted(&path);
    assert!(
        inputs
            .prepare_locked(&source, &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(inputs.phase(), OfflinePreparationPhase::Inputs);
    assert_eq!(inputs.owner.custody_for_test(), (None, None, None));
    inputs
        .acquire_source_locks(&AtomicBool::new(false))
        .unwrap();
    let custody = inputs.owner.custody_for_test();
    let failed = failure(inputs);
    assert_eq!(failed.operation.phase(), OfflinePreparationPhase::Failed);
    assert_eq!(failed.operation.owner.custody_for_test(), custody);
    assert!(crate::writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
    drop(failed);
    released(&path);
}
