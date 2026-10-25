//! Inputs-only host reservation updates on the actual admitted native owner.
use super::*;

#[derive(Debug, PartialEq, Eq)]
struct RetainedState {
    allocation: (usize, Option<usize>, u64),
    temporary: u64,
    input: (usize, usize),
    fallback: (usize, usize),
    deadline: Instant,
    custody: (Option<usize>, Option<i32>, Option<i32>),
}
fn retained_state(operation: &OfflinePreparationOperation) -> RetainedState {
    let allocation = operation.owner.allocation_for_test();
    let (_, input, capacity) = operation.owner.reservation_and_input_for_test().unwrap();
    RetainedState {
        allocation: allocation.preparation_state_for_test().unwrap(),
        temporary: allocation.temporary_bytes_for_test().unwrap(),
        input: (input, capacity),
        fallback: (
            operation.failure.0.as_ptr() as usize,
            operation.failure.0.capacity(),
        ),
        deadline: operation.deadline,
        custody: operation.owner.custody_for_test(),
    }
}
fn external(operation: &OfflinePreparationOperation) -> u64 {
    operation.owner.reservation_and_input_for_test().unwrap().0
}
fn reservation(bytes: u64) -> OfflineAllocationReservation {
    OfflineAllocationReservation::new(bytes).unwrap()
}
fn assert_no_acquisition(operation: &OfflinePreparationOperation, path: &Path) {
    assert_eq!(operation.owner.custody_for_test(), (None, None, None));
    assert_eq!(
        operation
            .owner
            .allocation_for_test()
            .preparation_state_for_test()
            .unwrap()
            .1,
        None
    );
    assert!(!sidecar(path, ".lock").exists());
    assert!(!sidecar(path, ".native-admission.lock").exists());
}
fn assert_terminal_unchanged(
    mut operation: OfflinePreparationOperation,
    before: RetainedState,
    host: u64,
    path: &Path,
) {
    assert_eq!(operation.phase(), OfflinePreparationPhase::Failed);
    assert_eq!(retained_state(&operation), before);
    assert_eq!(external(&operation), host);
    assert!(
        operation
            .refresh_external_reservation(reservation(host))
            .is_err()
    );
    assert!(operation.prepare(&AtomicBool::new(false)).is_err());
    assert_eq!(retained_state(&operation), before);
    assert_no_acquisition(&operation, path);
    let allocation = operation.owner.allocation_for_test();
    let failed = failure(operation);
    assert_eq!(retained_state(&failed.operation), before);
    assert_eq!(external(&failed.operation), host);
    assert_eq!(
        allocation.temporary_bytes_for_test().unwrap(),
        before.temporary
    );
    drop(failed);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
}

#[test]
fn borrowed_preparation_refresh_inputs_raises_host_without_acquisition_reallocation_or_polling() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("missing-parent/catalog.db");
    let mut operation = admitted(&path);
    let before = retained_state(&operation);
    assert!(before.allocation.2 > 0);
    assert_eq!(before.temporary, 32 * 1024 * 1024);
    assert_eq!(external(&operation), 0);
    for host in [1024, 2048, 2048] {
        crate::embedding::with_interrupt_check(
            || panic!("Inputs reservation refresh must not poll"),
            || operation.refresh_external_reservation(reservation(host)),
        )
        .unwrap();
        assert_eq!(operation.phase(), OfflinePreparationPhase::Inputs);
        assert_eq!(retained_state(&operation), before);
        assert_eq!(external(&operation), host);
        assert_no_acquisition(&operation, &path);
    }
    assert!(!root.path().join("missing-parent").exists());
    drop(failure(operation));
}

#[test]
fn borrowed_preparation_refresh_raised_host_survives_real_binding_and_move_only_finish() {
    let (_root, path) = source(40);
    let mut operation = admitted(&path);
    let allocation = operation.owner.allocation_for_test();
    let before = retained_state(&operation);
    operation
        .refresh_external_reservation(reservation(4096))
        .unwrap();
    assert_eq!(retained_state(&operation), before);
    operation.prepare(&AtomicBool::new(false)).unwrap();
    let after = retained_state(&operation);
    assert_eq!(after.allocation.0, before.allocation.0);
    assert_eq!(after.allocation.1, after.custody.0);
    assert!(after.allocation.2 > before.allocation.2);
    assert_eq!(after.temporary, before.temporary);
    assert_eq!(after.deadline, before.deadline);
    assert_eq!(external(&operation), 4096);
    let mut prepared = operation.finish().unwrap();
    assert_eq!(
        prepared.owner.apply_custody_for_test().0,
        after.custody.0.unwrap()
    );
    assert_eq!(prepared.identity.producer_deadline, Some(before.deadline));
    assert_eq!(
        allocation.preparation_state_for_test().unwrap(),
        after.allocation
    );
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
    // The original monotonic high-water still applies in the genuine moved owner.
    assert!(
        prepared
            .refresh_external_reservation(reservation(4095))
            .is_err()
    );
    prepared
        .refresh_external_reservation(reservation(4096))
        .unwrap();
    assert_eq!(
        allocation.preparation_state_for_test().unwrap(),
        after.allocation
    );
    assert!(crate::writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
    drop(prepared);
    released(&path);
}

#[test]
fn borrowed_preparation_refresh_lower_host_is_terminal_and_preserves_original_storage() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("missing.db");
    let mut operation = admitted(&path);
    operation
        .refresh_external_reservation(reservation(1024))
        .unwrap();
    let before = retained_state(&operation);
    let error = operation
        .refresh_external_reservation(reservation(1023))
        .unwrap_err();
    assert!(error.0.contains("cannot decrease"));
    assert_terminal_unchanged(operation, before, 1024, &path);
}

#[test]
fn borrowed_preparation_refresh_over_aggregate_counts_existing_slots_and_scratch() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("missing.db");
    let mut operation = admitted(&path);
    let before = retained_state(&operation);
    // Exact remaining space under the EXISTING 256 MiB aggregate, not a new
    // allowance. Both actual admitted preparation storage and scratch survive.
    let remaining = 256 * 1024 * 1024 - before.allocation.2 - before.temporary;
    operation
        .refresh_external_reservation(reservation(remaining))
        .unwrap();
    let allocation = operation.owner.allocation_for_test();
    allocation.check(None, 0).unwrap();
    assert!(allocation.check(None, 1).is_err());
    let error = operation
        .refresh_external_reservation(reservation(remaining + 1))
        .unwrap_err();
    assert!(error.0.contains("aggregate allocation budget exhausted"));
    assert_terminal_unchanged(operation, before, remaining, &path);
}

#[test]
fn borrowed_preparation_refresh_expired_deadline_is_terminal_before_any_acquisition() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("missing.db");
    let mut operation =
        OfflinePreparationOperation::admit(&path, quiescence(), reservation(1024), Instant::now())
            .unwrap();
    let before = retained_state(&operation);
    let error = operation
        .refresh_external_reservation(reservation(2048))
        .unwrap_err();
    assert!(error.0.contains("deadline exhausted"));
    assert_terminal_unchanged(operation, before, 1024, &path);
}

#[test]
fn borrowed_preparation_refresh_non_inputs_keeps_actual_connection_leases_and_plan_or_transaction()
{
    for version in [39, 40] {
        let (_root, path) = source(version);
        let mut operation = admitted(&path);
        operation
            .refresh_external_reservation(reservation(1024))
            .unwrap();
        let prepared = operation.prepare(&AtomicBool::new(false));
        assert_eq!(prepared.is_ok(), version == 40);
        let phase = operation.phase();
        let before = retained_state(&operation);
        assert!(
            before.custody.0.is_some() && before.custody.1.is_some() && before.custody.2.is_some()
        );
        let plan = operation.owner.plan_for_test().cloned();
        let autocommit = operation
            .owner
            .connection_for_test()
            .unwrap()
            .is_autocommit();
        assert_eq!(autocommit, version == 40);
        let error = operation
            .refresh_external_reservation(reservation(2048))
            .unwrap_err();
        assert!(error.0.contains("requires Inputs"));
        assert_eq!(operation.phase(), phase);
        assert_eq!(retained_state(&operation), before);
        assert_eq!(external(&operation), 1024);
        assert_eq!(operation.owner.plan_for_test(), plan.as_ref());
        assert_eq!(
            operation
                .owner
                .connection_for_test()
                .unwrap()
                .is_autocommit(),
            autocommit
        );
        held_descriptors(&operation);
        assert!(crate::writer_lock(&path).is_err());
        assert!(crate::native_admission_exclusive(&path).is_err());
        if version == 40 {
            let prepared = operation.finish().unwrap();
            assert_eq!(
                prepared.owner.apply_custody_for_test().0,
                before.custody.0.unwrap()
            );
            assert!(crate::writer_lock(&path).is_err());
            assert!(crate::native_admission_exclusive(&path).is_err());
            drop(prepared);
        } else {
            let failed = failure(operation);
            assert_eq!(retained_state(&failed.operation), before);
            assert!(
                !failed
                    .operation
                    .owner
                    .connection_for_test()
                    .unwrap()
                    .is_autocommit()
            );
            assert!(crate::writer_lock(&path).is_err());
            assert!(crate::native_admission_exclusive(&path).is_err());
            drop(failed);
        }
        released(&path);
    }
}
