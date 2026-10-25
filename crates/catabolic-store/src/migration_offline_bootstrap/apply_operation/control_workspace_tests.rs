//! Fixed original backing and borrowed actual custody, never a high-H credit.
use super::*;

fn prepared_with_control() -> (tempfile::TempDir, PathBuf, PreparedOfflineBootstrap, OfflineControlWorkspaceBacking) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize_native_at(&path, 40).unwrap();
    let mut backing = OfflineControlWorkspaceBacking::new();
    let mut preparation = OfflinePreparationOperation::admit_with_control_workspace(
        &path, quiescence(), OfflineAllocationReservation::new(1024).unwrap(), &mut backing,
        Instant::now() + Duration::from_secs(60),
    ).unwrap();
    assert_eq!(backing.state_for_test(), (OfflineControlWorkspaceBacking::BYTES + 1024, false, false));
    assert!(OfflinePreparationOperation::admit_with_control_workspace(
        &path, quiescence(), OfflineAllocationReservation::default(), &mut backing,
        Instant::now() + Duration::from_secs(60),
    ).is_err());
    preparation.refresh_external_reservation(
        OfflineAllocationReservation::new(OfflineControlWorkspaceBacking::BYTES + 2048).unwrap(),
    ).unwrap();
    preparation.prepare(&AtomicBool::new(false)).unwrap();
    let owner = preparation.finish().unwrap();
    (root, path, owner, backing)
}

#[test]
fn original_control_loan_survives_refresh_and_conversion_strings_remain_charged() {
    let (_root, path, owner, mut backing) = prepared_with_control();
    let connection = owner.owner.apply_custody_for_test().0;
    let mut operation = admitted(owner);
    let full = backing.state_for_test().0;
    operation.apply_with_control_workspace(&AtomicBool::new(false), &mut backing).unwrap();
    assert_eq!(backing.state_for_test(), (full, false, false));
    assert_custody(&operation.owner, &path, connection);
    assert_eq!(version(&operation.owner), 41);
    assert!(operation.apply_with_control_workspace(&AtomicBool::new(false), &mut backing).is_err());
    let bootstrap = operation.finish().unwrap();
    let (allocation, strings) = bootstrap.session.apply_conversion_allocation_for_test();
    assert_eq!(strings, 1920);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), strings);
    assert_eq!(allocation.external_retained_bytes_for_test().unwrap(), full);
    // Backing lifetime is not a grant to subsequent Store calls: validation
    // uses its ordinary full metadata guard, without any implicit active loan.
    drop(backing);
    bootstrap.session.completion_store_for_test().validate_native_identity().unwrap();
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), strings);
    assert_eq!(allocation.external_retained_bytes_for_test().unwrap(), full);
    drop(bootstrap);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
    assert_eq!(allocation.external_retained_bytes_for_test().unwrap(), full);
    assert_released(&path);
}

#[test]
fn control_apply_rejects_unregistered_and_foreign_backings_without_mutation() {
    for foreign in [false, true] {
        let (_root, path, owner, mut original) = prepared_with_control();
        let connection = owner.owner.apply_custody_for_test().0;
        let mut operation = admitted(owner);
        let (_other_root, _other_path, _other_owner, mut bound_elsewhere) = prepared_with_control();
        let mut unbound = OfflineControlWorkspaceBacking::new();
        let wrong = if foreign { &mut bound_elsewhere } else { &mut unbound };
        assert!(operation.apply_with_control_workspace(&AtomicBool::new(false), wrong).is_err());
        assert!(matches!(operation.state, ApplyState::Failed));
        assert!(operation.apply_with_control_workspace(&AtomicBool::new(false), &mut original).is_err());
        assert_eq!(version(&operation.owner), 40);
        assert_custody(&operation.owner, &path, connection);
        assert!(!operation.owner.owner.apply_has_backup_for_test());
        let failed = failure(operation);
        assert_custody(&failed._owner, &path, connection);
    }
}

#[test]
fn scalar_reserved_owner_cannot_activate_new_workspace() {
    let (_root, path, mut owner) = prepared();
    owner.refresh_external_reservation(OfflineAllocationReservation::new(128 * 1024 * 1024).unwrap()).unwrap();
    let connection = owner.owner.apply_custody_for_test().0;
    let mut operation = admitted(owner);
    let mut backing = OfflineControlWorkspaceBacking::new();
    assert!(operation.apply_with_control_workspace(&AtomicBool::new(false), &mut backing).is_err());
    assert_eq!(version(&operation.owner), 40);
    assert_custody(&operation.owner, &path, connection);
}

#[test]
fn ordinary_apply_stays_available_for_host_overcapacity_fallback() {
    let (_root, _path, owner, backing) = prepared_with_control();
    let full = backing.state_for_test().0;
    let mut operation = admitted(owner);
    operation.apply(&AtomicBool::new(false)).unwrap();
    assert_eq!(backing.state_for_test(), (full, false, false));
    assert_eq!(version(&operation.owner), 41);
    let bootstrap = operation.finish().unwrap();
    assert_eq!(backing.state_for_test(), (full, false, false));
    drop(bootstrap);
}

#[test]
fn cancelled_control_apply_releases_loan_but_not_terminal_custody_or_charge() {
    let (_root, path, owner, mut backing) = prepared_with_control();
    let connection = owner.owner.apply_custody_for_test().0;
    let full = backing.state_for_test().0;
    let mut operation = admitted(owner);
    assert!(operation.apply_with_control_workspace(&AtomicBool::new(true), &mut backing).is_err());
    assert_eq!(backing.state_for_test(), (full, false, false));
    assert_eq!(version(&operation.owner), 40);
    assert_custody(&operation.owner, &path, connection);
    let failed = failure(operation);
    assert_custody(&failed._owner, &path, connection);
    assert_eq!(backing.state_for_test(), (full, false, false));
    drop(failed);
    assert_released(&path);
}

#[test]
fn control_apply_faults_release_all_nested_workspace_borrows_and_keep_custody() {
    for fault in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13] {
        let (_root, path, owner, mut backing) = prepared_with_control();
        let connection = owner.owner.apply_custody_for_test().0;
        let full = backing.state_for_test().0;
        let mut operation = admitted(owner);
        let cancel = AtomicBool::new(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            operation.apply_with_control_workspace_inner(&cancel, &mut backing, fault)
        }));
        assert!(result.is_err() || result.unwrap().is_err(), "fault {fault} unexpectedly succeeded");
        assert_eq!(backing.state_for_test(), (full, false, false), "fault {fault}");
        assert!(matches!(operation.state, ApplyState::Failed));
        assert_custody(&operation.owner, &path, connection);
        assert!(operation.apply_with_control_workspace(&AtomicBool::new(false), &mut backing).is_err());
        let failed = failure(operation);
        assert_custody(&failed._owner, &path, connection);
        drop(failed);
        assert_released(&path);
        assert_eq!(backing.state_for_test(), (full, false, false));
    }
}


#[test]
fn control_error_output_preserves_bounded_original_and_moves_prepaid_fallback_while_loaned() {
    let (_root, path, owner, mut backing) = prepared_with_control();
    let connection = owner.owner.apply_custody_for_test().0;
    let full = backing.state_for_test().0;
    let mut operation = admitted(owner);
    operation.begin_attempt().unwrap();
    let control = operation.owner.owner.control_workspace_loan(&mut backing).unwrap();
    let mut message = String::with_capacity(ERROR_OUTPUT_STORAGE);
    message.push_str("preserve original native error verbatim");
    let original_pointer = message.as_ptr();
    let original_capacity = message.capacity();
    let preserved = operation.bound_control_error_output(Error(message));
    assert_eq!(preserved.0, "preserve original native error verbatim");
    assert_eq!(preserved.0.as_ptr(), original_pointer);
    assert_eq!(preserved.0.capacity(), original_capacity);
    assert!(operation.admission.output_failure.is_some());
    drop(preserved);
    let fallback = operation.admission.output_failure.as_ref().unwrap();
    let fallback_pointer = fallback.0.as_ptr();
    let fallback_capacity = fallback.0.capacity();
    let mut oversized = String::with_capacity(ERROR_OUTPUT_STORAGE + 1024);
    oversized.push_str("short text with oversized spare capacity");
    let bounded = operation.bound_control_error_output(Error(oversized));
    assert_eq!(bounded.0, ERROR_OUTPUT_FAILED);
    assert_eq!(bounded.0.as_ptr(), fallback_pointer);
    assert_eq!(bounded.0.capacity(), fallback_capacity);
    assert!(bounded.0.capacity() <= ERROR_OUTPUT_FALLBACK_STORAGE);
    assert!(operation.admission.output_failure.is_none());
    // Explicit loan remains in scope until AFTER both output decisions.
    drop(control);
    assert_eq!(backing.state_for_test(), (full, false, false));
    assert!(matches!(operation.state, ApplyState::Failed));
    assert_custody(&operation.owner, &path, connection);
    let failed = failure(operation);
    assert_custody(&failed._owner, &path, connection);
    assert_eq!(bounded.0, ERROR_OUTPUT_FAILED);
    drop(bounded);
    drop(failed);
    assert_released(&path);
}

#[test]
fn actual_control_apply_oversized_error_moves_preexisting_fallback_and_keeps_terminal_custody() {
    let (_root, path, owner, mut backing) = prepared_with_control();
    let connection = owner.owner.apply_custody_for_test().0;
    let full = backing.state_for_test().0;
    let mut operation = admitted(owner);
    let fallback_pointer = operation.admission.output_failure.as_ref().unwrap().0.as_ptr();
    let error = operation.apply_with_control_workspace_inner(&AtomicBool::new(false), &mut backing, 14).unwrap_err();
    assert_eq!(error.0, ERROR_OUTPUT_FAILED);
    assert_eq!(error.0.as_ptr(), fallback_pointer);
    assert!(error.0.capacity() <= ERROR_OUTPUT_FALLBACK_STORAGE);
    assert!(operation.admission.output_failure.is_none());
    assert_eq!(backing.state_for_test(), (full, false, false));
    assert!(matches!(operation.state, ApplyState::Failed));
    assert_eq!(version(&operation.owner), 40);
    assert_custody(&operation.owner, &path, connection);
    let repeated = operation.apply_with_control_workspace(&AtomicBool::new(false), &mut backing).unwrap_err();
    assert_eq!(repeated.0, "offline apply operation already attempted");
    assert!(repeated.0.capacity() <= ERROR_OUTPUT_STORAGE);
    drop(repeated);
    let failed = failure(operation);
    assert_custody(&failed._owner, &path, connection);
    assert_eq!(error.0, ERROR_OUTPUT_FAILED);
    drop(error);
    drop(failed);
    assert_released(&path);
}

#[test]
fn ordinary_apply_preserves_original_error_even_when_control_output_cohort_is_exceeded() {
    let (_root, path, owner, backing) = prepared_with_control();
    let connection = owner.owner.apply_custody_for_test().0;
    let mut operation = admitted(owner);
    let error = operation.apply_inner(&AtomicBool::new(false), 14).unwrap_err();
    assert_eq!(error.0, "controlled oversized native apply error");
    assert!(error.0.capacity() > ERROR_OUTPUT_STORAGE);
    assert!(operation.admission.output_failure.is_some());
    assert_eq!(backing.state_for_test().1, false);
    assert_custody(&operation.owner, &path, connection);
}

#[test]
fn actual_bounded_control_error_keeps_original_message_and_preallocated_fallback() {
    let (_root, path, owner, mut backing) = prepared_with_control();
    let connection = owner.owner.apply_custody_for_test().0;
    let mut operation = admitted(owner);
    let fallback_pointer = operation.admission.output_failure.as_ref().unwrap().0.as_ptr();
    let error = operation.apply_with_control_workspace_inner(&AtomicBool::new(false), &mut backing, 4).unwrap_err();
    assert_eq!(error.0, "controlled native precommit failure");
    assert!(error.0.capacity() <= ERROR_OUTPUT_STORAGE);
    assert_eq!(operation.admission.output_failure.as_ref().unwrap().0.as_ptr(), fallback_pointer);
    assert!(matches!(operation.state, ApplyState::Failed));
    assert_custody(&operation.owner, &path, connection);
    assert_eq!(backing.state_for_test().1, false);
}
