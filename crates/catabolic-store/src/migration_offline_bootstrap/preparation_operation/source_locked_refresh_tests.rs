use super::*;
use std::fs::File;

fn reservation(bytes: u64) -> OfflineAllocationReservation {
    OfflineAllocationReservation::new(bytes).unwrap()
}
fn state(
    operation: &OfflinePreparationOperation,
) -> (
    (usize, Option<usize>, u64),
    u64,
    (Option<usize>, Option<i32>, Option<i32>),
    Instant,
    usize,
    usize,
) {
    let allocation = operation.owner.allocation_for_test();
    (
        allocation.preparation_state_for_test().unwrap(),
        allocation.temporary_bytes_for_test().unwrap(),
        operation.owner.custody_for_test(),
        operation.deadline,
        operation.failure.0.as_ptr() as usize,
        operation.failure.0.capacity(),
    )
}
fn external(operation: &OfflinePreparationOperation) -> u64 {
    operation.owner.reservation_and_input_for_test().unwrap().0
}
fn assert_terminal(operation: &mut OfflinePreparationOperation, source: &File) {
    assert_eq!(operation.phase(), OfflinePreparationPhase::Failed);
    let before = state(operation);
    let host = external(operation);
    assert!(
        operation
            .refresh_source_locked_reservation(reservation(host))
            .is_err()
    );
    assert!(
        operation
            .refresh_external_reservation(reservation(host))
            .is_err()
    );
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
    assert_eq!(state(operation), before);
    assert_eq!(external(operation), host);
}

#[test]
fn source_locked_refresh_keeps_context_leases_deadline_and_counters_through_finish() {
    let (_root, path) = source(40);
    let file = File::open(&path).unwrap();
    let mut operation = admitted(&path);
    operation
        .acquire_source_locks(&AtomicBool::new(false))
        .unwrap();
    let before = state(&operation);
    for host in [1024, 2048, 2048] {
        crate::embedding::with_interrupt_check(
            || panic!("locked refresh must not poll"),
            || operation.refresh_source_locked_reservation(reservation(host)),
        )
        .unwrap();
        assert_eq!(state(&operation), before);
        assert_eq!(external(&operation), host);
        assert_eq!(operation.phase(), OfflinePreparationPhase::SourceLocked);
        assert!(crate::writer_lock(&path).is_err());
        assert!(crate::native_admission_exclusive(&path).is_err());
    }
    operation
        .prepare_locked(&file, &AtomicBool::new(false))
        .unwrap();
    let allocation = operation.owner.allocation_for_test();
    assert_eq!(
        allocation.preparation_state_for_test().unwrap().0,
        before.0.0
    );
    assert_eq!(external(&operation), 2048);
    let prepared = operation.finish().unwrap();
    assert_eq!(prepared.identity.producer_deadline, Some(before.3));
    assert_eq!(allocation.external_retained_bytes_for_test().unwrap(), 2048);
    drop(prepared);
    released(&path);
}

#[test]
fn source_locked_refresh_decrease_and_aggregate_overflow_are_terminal_with_custody() {
    for over_limit in [false, true] {
        let (_root, path) = source(40);
        let file = File::open(&path).unwrap();
        let mut operation = admitted(&path);
        operation
            .acquire_source_locks(&AtomicBool::new(false))
            .unwrap();
        let before = state(&operation);
        let host = if over_limit {
            256 * 1024 * 1024 - before.0.2 - before.1
        } else {
            1024
        };
        operation
            .refresh_source_locked_reservation(reservation(host))
            .unwrap();
        let error = operation
            .refresh_source_locked_reservation(reservation(if over_limit {
                host + 1
            } else {
                host - 1
            }))
            .unwrap_err();
        assert!(error.0.contains(if over_limit {
            "aggregate allocation budget exhausted"
        } else {
            "cannot decrease"
        }));
        assert_eq!(state(&operation), before);
        assert_eq!(external(&operation), host);
        assert_terminal(&mut operation, &file);
        assert!(crate::writer_lock(&path).is_err());
        assert!(crate::native_admission_exclusive(&path).is_err());
        drop(failure(operation));
        released(&path);
    }
}

#[test]
fn source_locked_refresh_real_expiry_preserves_same_owner_and_never_revives() {
    let (_root, path) = source(40);
    let file = File::open(&path).unwrap();
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut operation =
        OfflinePreparationOperation::admit(&path, quiescence(), reservation(1024), deadline)
            .unwrap();
    operation
        .acquire_source_locks(&AtomicBool::new(false))
        .unwrap();
    let before = state(&operation);
    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    assert!(Instant::now() >= deadline);
    assert!(
        operation
            .refresh_source_locked_reservation(reservation(2048))
            .unwrap_err()
            .0
            .contains("deadline exhausted")
    );
    assert_eq!(state(&operation), before);
    assert_eq!(external(&operation), 1024);
    assert_terminal(&mut operation, &file);
    assert!(crate::writer_lock(&path).is_err());
    drop(failure(operation));
    released(&path);
}

#[test]
fn source_locked_refresh_rejects_other_phases_and_constructor_overflow_without_mutation() {
    let (_root, path) = source(40);
    let file = File::open(&path).unwrap();
    let mut operation = admitted(&path);
    let before = state(&operation);
    assert!(
        operation
            .refresh_source_locked_reservation(reservation(1024))
            .is_err()
    );
    assert_eq!(operation.phase(), OfflinePreparationPhase::Inputs);
    assert_eq!(state(&operation), before);
    operation
        .acquire_source_locks(&AtomicBool::new(false))
        .unwrap();
    let locked = state(&operation);
    // Overflow is refused by the existing scalar constructor before invocation;
    // this is not an attempted operation and must not be described as terminal.
    assert!(OfflineAllocationReservation::new(u64::MAX).is_err());
    assert_eq!(state(&operation), locked);
    operation
        .prepare_locked(&file, &AtomicBool::new(false))
        .unwrap();
    let prepared = state(&operation);
    assert!(
        operation
            .refresh_source_locked_reservation(reservation(2048))
            .is_err()
    );
    assert_eq!(operation.phase(), OfflinePreparationPhase::Prepared);
    assert_eq!(state(&operation), prepared);
    drop(operation.finish().unwrap());
    released(&path);
}
