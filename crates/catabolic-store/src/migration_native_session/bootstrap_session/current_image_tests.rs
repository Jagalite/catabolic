//! Genuine retained completion, not a synthetic source admission or image grant.
use super::super::tests::committed;
use super::*;
use std::time::Duration;

fn assert_additive_retention(image: &CurrentCatalogImage) -> (u64, u64) {
    let persistent = image.owner.session.preservation.allocated_bytes().unwrap()
        + image.owner.retained_completed_bytes().unwrap();
    let copied =
        image.image.retained_allocations().unwrap() + retained_history(&image.history).unwrap();
    let measured = image.retained_allocations().unwrap();
    let context = &image
        .owner
        .session
        .store
        .offline_bootstrap
        .as_ref()
        .unwrap()
        .allocation;
    let (preservation, copy) = context.retained_components_for_test().unwrap();
    assert!(preservation >= persistent);
    assert!(copy >= copied);
    assert!(measured >= preservation + copy);
    (preservation, copy)
}

#[test]
fn current_image_preserves_full_inventory_and_discard_keeps_same_custody() {
    let (_root, session, command) = committed(Duration::from_secs(3600));
    let owner = session.into_completed_catalog(&command).unwrap();
    let before = snapshot(&owner.session.store.db).unwrap();
    let generation = owner.session.generation().unwrap();
    let expected = crate::catalog_backup::inventory::capture(
        &owner.session.store.db,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let mut image = owner.into_current_image(&AtomicBool::new(false)).unwrap();
    let external = image.external_retained_reservation().unwrap();
    image
        .refresh_external_reservation(OfflineAllocationReservation::new(external + 1024).unwrap())
        .unwrap();
    assert_eq!(
        image.external_retained_reservation().unwrap(),
        external + 1024
    );
    assert!(
        image
            .refresh_external_reservation(OfflineAllocationReservation::new(external).unwrap())
            .is_err()
    );
    assert_eq!(
        image.external_retained_reservation().unwrap(),
        external + 1024
    );
    assert_eq!(image.generation(), generation);
    assert_eq!(image.current_inventory_sha256(), expected.complete);
    assert!(image.source_effects().is_empty());
    assert_eq!(snapshot(&image.owner.session.store.db).unwrap(), before);
    let retained = assert_additive_retention(&image);
    let owner = image.discard_copy();
    owner.retained_allocations().unwrap();
    assert_eq!(
        owner
            .session
            .store
            .offline_bootstrap
            .as_ref()
            .unwrap()
            .allocation
            .retained_components_for_test()
            .unwrap(),
        retained
    );
    assert!(owner.session.store.db.is_autocommit());
    owner.validate_current().unwrap();
    assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
    // The actual retained owner remains usable; no reopen or pristine recovery.
    let mut operation = admitted_capture(owner, Instant::now() + Duration::from_secs(60));
    operation.capture(&AtomicBool::new(false)).unwrap();
    let image = operation.finish().unwrap();
    assert_eq!(image.current_inventory_sha256(), expected.complete);
    let recaptured = assert_additive_retention(&image);
    assert!(recaptured.0 >= retained.0 && recaptured.1 >= retained.1);
}

#[test]
fn current_image_precancel_retains_session_and_foreign_owner_is_refused() {
    let (_root, session, command) = committed(Duration::from_secs(3600));
    let owner = session.into_completed_catalog(&command).unwrap();
    let (_other_root, other_session, other_command) = committed(Duration::from_secs(3600));
    let other = other_session
        .into_completed_catalog(&other_command)
        .unwrap();
    let before = snapshot(&owner.session.store.db).unwrap();
    let token = CompletedCurrentImageAdmission { owner: &owner, control: None };
    assert!(token.check(&other.session.store).is_err());
    let failure = match owner.into_current_image(&AtomicBool::new(true)) {
        Err(failure) => failure,
        Ok(_) => panic!("cancelled capture unexpectedly succeeded"),
    };
    assert!(failure.error().to_string().contains("cancelled"));
    let owner = failure.into_session();
    assert!(owner.session.store.db.is_autocommit());
    assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
    let image = owner.into_current_image(&AtomicBool::new(false)).unwrap();
    assert!(image.source_effects().is_empty());
}

#[test]
fn current_image_copy_corruption_and_late_cancel_rollback_without_source_effects() {
    let (_root, session, command) = committed(Duration::from_secs(3600));
    let owner = session.into_completed_catalog(&command).unwrap();
    let before = snapshot(&owner.session.store.db).unwrap();
    for fault in [
        completed_image::CaptureFault::CorruptCopy,
        completed_image::CaptureFault::CancelAfterSnapshot,
        completed_image::CaptureFault::CancelBeforeFinalSource,
    ] {
        let cancel = AtomicBool::new(false);
        assert!(
            owner
                .capture_current_image_with_fault(&cancel, fault)
                .is_err()
        );
        assert!(owner.session.store.db.is_autocommit());
        assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
        owner.validate_current().unwrap();
    }
    // Every failed copy has dropped while the original source locks remain.
    let image = owner.into_current_image(&AtomicBool::new(false)).unwrap();
    assert_eq!(snapshot(&image.owner.session.store.db).unwrap(), before);
}

#[test]
fn current_image_actual_external_retention_refuses_before_copy_and_keeps_source() {
    let (_root, session, command) = committed(Duration::from_secs(3600));
    let mut owner = session.into_completed_catalog(&command).unwrap();
    let before = snapshot(&owner.session.store.db).unwrap();
    owner
        .refresh_external_reservation(OfflineAllocationReservation::new(200 * 1024 * 1024).unwrap())
        .unwrap();
    let failed = match owner.into_current_image(&AtomicBool::new(false)) {
        Err(failed) => failed,
        Ok(_) => panic!("simultaneous current-image retention was not bounded"),
    };
    assert!(failed.error().to_string().contains("allocation"));
    let owner = failed.into_session();
    assert!(owner.session.store.db.is_autocommit());
    assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
}

fn admitted_capture(
    mut owner: CompletedCatalogSession,
    deadline: Instant,
) -> CurrentImageCaptureOperation {
    owner.admit_current_image_operation(deadline).unwrap();
    owner.begin_current_image_capture().unwrap()
}

#[test]
fn borrowed_current_capture_retains_original_across_entry_and_copy_unwinds() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::{Arc, atomic::Ordering};
    for fault in 0..3 {
        let (_root, session, command) = committed(Duration::from_secs(3600));
        let owner = session.into_completed_catalog(&command).unwrap();
        let before = snapshot(&owner.session.store.db).unwrap();
        let mut operation = admitted_capture(owner, Instant::now() + Duration::from_secs(60));
        let raw = unsafe { operation.owner.session.store.db.handle() } as usize;
        let seen = Arc::new(AtomicBool::new(false));
        let observed = seen.clone();
        let result = catch_unwind(AssertUnwindSafe(|| {
            if fault == 0 {
                crate::embedding::with_interrupt_check(
                    move || {
                        if unsafe { rusqlite::ffi::sqlite3_get_autocommit(raw as *mut _) } == 0
                            && !observed.swap(true, Ordering::SeqCst)
                        {
                            panic!("controlled capture entry poll");
                        }
                        false
                    },
                    || operation.capture(&AtomicBool::new(false)),
                )
            } else {
                operation.capture_inner(&AtomicBool::new(false), fault)
            }
        }));
        assert!(result.is_err(), "controlled capture unwind not reached");
        if fault == 0 {
            assert!(seen.load(Ordering::SeqCst));
        }
        assert!(matches!(operation.state, CaptureState::Failed));
        assert!(operation.owner.session.store.owns_writer_lock());
        assert!(operation.owner.session.store.db.is_autocommit());
        assert_eq!(snapshot(&operation.owner.session.store.db).unwrap(), before);
        assert!(operation.capture(&AtomicBool::new(false)).is_err());
        let owner = match operation.finish() {
            Err(failure) => failure.into_session(),
            Ok(_) => panic!("unwound capture returned image"),
        };
        owner.validate_current().unwrap();
        assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
    }
}

#[test]
fn borrowed_current_capture_requires_admission_and_preserves_original_deadline() {
    let (_root, session, command) = committed(Duration::from_secs(3600));
    let owner = session.into_completed_catalog(&command).unwrap();
    let before = snapshot(&owner.session.store.db).unwrap();
    let owner = match owner.begin_current_image_capture() {
        Err(failure) => failure.into_session(),
        Ok(_) => panic!("unadmitted handoff succeeded"),
    };
    let mut owner = owner;
    let initial = Instant::now() + Duration::from_secs(60);
    owner.admit_current_image_operation(initial).unwrap();
    let retained_deadline = owner.capture_admission.as_ref().unwrap().deadline;
    owner
        .admit_current_image_operation(initial + Duration::from_secs(60))
        .unwrap();
    assert_eq!(
        owner.capture_admission.as_ref().unwrap().deadline,
        retained_deadline
    );
    let expired = Instant::now();
    owner.admit_current_image_operation(expired).unwrap();
    owner.admit_current_image_operation(initial).unwrap();
    assert_eq!(owner.capture_admission.as_ref().unwrap().deadline, expired);
    let owner = match owner.into_current_image(&AtomicBool::new(false)) {
        Err(failure) => failure.into_session(),
        Ok(_) => panic!("legacy capture bypassed inherited deadline"),
    };
    assert_eq!(owner.capture_admission.as_ref().unwrap().deadline, expired);
    assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
    let mut operation = owner.begin_current_image_capture().unwrap();
    assert!(operation.capture(&AtomicBool::new(false)).is_err());
    assert!(matches!(operation.state, CaptureState::Failed));
    assert!(operation.capture(&AtomicBool::new(false)).is_err());
    let owner = match operation.finish() {
        Err(failure) => failure.into_session(),
        Ok(_) => panic!("expired capture returned image"),
    };
    owner.validate_current().unwrap();
    assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
}


fn workspace_fixture() -> (tempfile::TempDir, CompletedCatalogSession, allocation::OfflineControlWorkspaceBacking) {
    use super::super::source_workspace_tests::{fixture, source_command, authority};
    let (root, mut owner, mut backing, candidate) = fixture();
    let cancel = AtomicBool::new(false);
    let preparation = owner.prepare_source_succession_with_control_workspace(
        "default", "native-source", &candidate, &cancel, &mut backing).unwrap();
    let command = source_command(preparation.result().as_ref().unwrap());
    let output = owner.execute_source_succession_with_control_workspace(&command, authority(),
        super::super::super::tests::policy(), &cancel, &mut backing).unwrap();
    assert!(matches!(output.result(), Ok(CommandOutcome::Committed { .. })));
    drop(output);
    drop(preparation);
    (root, owner, backing)
}
fn typed_operation(owner: CompletedCatalogSession) -> CurrentImageCaptureOperation {
    let mut operation = admitted_capture(owner, Instant::now() + Duration::from_secs(60));
    operation.admit_control_workspace_result().unwrap();
    operation
}

#[test]
fn current_capture_workspace_original_owner_history_and_phase_totals_survive_handoff() {
    let (_root, owner, mut backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    let store = &operation.owner.session.store;
    let source = unsafe { store.db.handle() } as usize;
    let context = store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let before = snapshot(&store.db).unwrap();
    let identity = context.preparation_state_for_test().unwrap();
    let external = operation.owner.external_retained_reservation().unwrap();
    let token = operation.borrow_control_workspace(&mut backing).unwrap();
    let result = operation.capture_with_control_workspace(&AtomicBool::new(false), token);
    assert!(result.result().is_ok(), "{result:?}");
    assert_eq!(context.preparation_state_for_test().unwrap(), identity);
    assert_eq!(context.external_retained_bytes().unwrap(), external);
    assert_eq!(unsafe { operation.owner.session.store.db.handle() } as usize, source);
    assert_eq!(snapshot(&operation.owner.session.store.db).unwrap(), before);
    assert!(operation.owner.session.store.owns_writer_lock());
    assert!(operation.owner.session.store.db.is_autocommit());
    assert!(matches!(operation.state, CaptureState::Captured(..)));
    assert_eq!(backing.state_for_test(), (external, false, false));
    let image = operation.finish().unwrap();
    assert_eq!(image.source_effects().len(), 1);
    assert_additive_retention(&image);
    let trace = context.capture_trace_for_test().unwrap();
    for phase in trace.iter().flatten() {
        assert!(phase.total <= allocation::LIMIT);
        assert_eq!(phase.external, external);
        assert_eq!(phase.metadata, 0, "typed copier must not create a second scratch arena");
        // Qualified owned transfer has one real buffer at a time, with
        // additional actual copied SQLite. Ordinary six-image path is separate.
        assert!(phase.requested >= phase.image_bytes + phase.copy_sqlite);
        eprintln!("native_capture_phase={} K={} total={} native_temporary={} image={} copy_sqlite={} metadata={} requested={}",
            phase.name, phase.external, phase.total, phase.temporary,
            phase.image_bytes, phase.copy_sqlite, phase.metadata, phase.requested);
        eprintln!("native_capture_categories={:?}", phase.native);
    }
    for name in ["proof_return", "copier_phase", "copier_output_installed", "final_source_validated", "copier_return", "persistent_handoff"] {
        assert!(trace.iter().flatten().any(|phase| phase.name == name), "missing phase {name}");
    }
    let held = context.temporary_bytes_for_test().unwrap();
    assert!(held >= 2 * 64 * 1024, "returned result guard ended early");
    drop(image);
    drop(backing);
    assert!(result.result().is_ok());
    assert_eq!(context.temporary_bytes_for_test().unwrap(), held);
    drop(result);
    assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
}

#[test]
fn current_capture_workspace_ordinary_full_charge_and_typed_copier_remain_distinct() {
    let (_root, mut owner, mut backing) = workspace_fixture();
    let native = owner.retained_allocations().unwrap();
    let forwarded = allocation::LIMIT - native - 24 * 1024 * 1024;
    owner.refresh_external_reservation_with_control_workspace(forwarded, &mut backing).unwrap();
    assert!(owner.validate_current().is_err(), "ordinary32MiB validation must not borrow backing implicitly");
    let mut operation = typed_operation(owner);
    let token = operation.borrow_control_workspace(&mut backing).unwrap();
    let result = operation.capture_with_control_workspace(&AtomicBool::new(false), token);
    assert!(result.result().is_ok(), "owned-transfer capture must satisfy the deliberately pressured phase budget: {result:?}");
    assert_eq!(operation.owner.external_retained_reservation().unwrap(), forwarded);
    assert_eq!(backing.state_for_test(), (forwarded, false, false));
    let image = operation.finish().unwrap();
    assert_eq!(image.source_effects().len(), 1);
    assert_additive_retention(&image);
}

#[test]
fn current_capture_workspace_missing_foreign_and_abandoned_tokens_never_rebrand_or_retry() {
    let (_root, owner, mut backing) = workspace_fixture();
    let (_other_root, other, mut other_backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let held = context.temporary_bytes_for_test().unwrap();
    let mut missing = allocation::OfflineControlWorkspaceBacking::new();
    assert!(operation.borrow_control_workspace(&mut missing).is_err());
    assert!(operation.borrow_control_workspace(&mut other_backing).is_err());
    assert!(operation.workspace_result.is_some());
    assert_eq!(context.temporary_bytes_for_test().unwrap(), held);
    let token = operation.borrow_control_workspace(&mut backing).unwrap();
    drop(token);
    assert!(matches!(operation.state, CaptureState::WorkspaceClaimed));
    assert!(operation.borrow_control_workspace(&mut backing).is_err());
    assert!(operation.admit_control_workspace_result().is_err());
    assert!(operation.capture(&AtomicBool::new(false)).is_err());
    assert!(operation.owner.session.store.owns_writer_lock());
    assert!(operation.finish().is_err());
    // Foreign token consumption cannot run on another admitted owner, even
    // when both sides have genuine independently registered backings.
    let mut other = typed_operation(other);
    let own_token = other.borrow_control_workspace(&mut other_backing).unwrap();
    drop(own_token);
    assert!(matches!(other.state, CaptureState::WorkspaceClaimed));
    let (_third_root, third, mut third_backing) = workspace_fixture();
    let mut third = typed_operation(third);
    let token = third.borrow_control_workspace(&mut third_backing).unwrap();
    let foreign = other.capture_with_control_workspace(&AtomicBool::new(false), token);
    assert!(foreign.result().is_err());
    assert!(matches!(other.state, CaptureState::Failed));
    assert!(matches!(third.state, CaptureState::WorkspaceClaimed));
    assert!(other.owner.session.store.owns_writer_lock());
    assert!(third.owner.session.store.owns_writer_lock());
}

#[test]
fn current_capture_workspace_cancel_expiry_refusal_and_unwind_keep_original_custody() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    for fault in 0..4 {
        let (_root, owner, mut backing) = workspace_fixture();
        let mut operation = typed_operation(owner);
        let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
        let before = snapshot(&operation.owner.session.store.db).unwrap();
        if fault == 1 { operation.admission.deadline = Instant::now(); }
        if fault == 2 {
            let native = operation.owner.retained_allocations().unwrap();
            let forwarded = allocation::LIMIT - native - 512 * 1024;
            operation.owner.refresh_external_reservation_with_control_workspace(forwarded, &mut backing).unwrap();
        }
        {
            let token = operation.borrow_control_workspace(&mut backing).unwrap();
            if fault == 3 {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    let CurrentImageCaptureWorkspace { result, loan } = token;
                    let control = BootstrapWorkspace::from_capture_loan(&operation.owner.session.store, loan);
                    let outcome = CurrentImageCaptureOperation::capture_parts(&operation.owner,
                        &operation.admission, &mut operation.state, &AtomicBool::new(false), 2, Some(&control));
                    result.finish(outcome)
                }));
                assert!(outcome.is_err());
            } else {
                let result = operation.capture_with_control_workspace(&AtomicBool::new(fault == 0), token);
                assert!(result.result().is_err());
                assert!(context.temporary_bytes_for_test().unwrap() >= 2 * 64 * 1024);
            }
        }
        assert!(matches!(operation.state, CaptureState::Failed));
        assert!(operation.owner.session.store.owns_writer_lock());
        assert!(operation.owner.session.store.db.is_autocommit());
        assert_eq!(snapshot(&operation.owner.session.store.db).unwrap(), before);
        assert!(operation.borrow_control_workspace(&mut backing).is_err());
        assert!(operation.capture(&AtomicBool::new(false)).is_err());
        assert!(!backing.state_for_test().1 && !backing.state_for_test().2);
    }
}


#[test]
fn current_capture_workspace_proof_and_copier_return_guards_survive_cancel_and_unwind() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    for fault in 3..=6 {
        let (_root, owner, mut backing) = workspace_fixture();
        let mut operation = typed_operation(owner);
        let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
        let outcome = {
            let token = operation.borrow_control_workspace(&mut backing).unwrap();
            catch_unwind(AssertUnwindSafe(|| {
                let CurrentImageCaptureWorkspace { result, loan } = token;
                let control = BootstrapWorkspace::from_capture_loan(&operation.owner.session.store, loan);
                let outcome = CurrentImageCaptureOperation::capture_parts(&operation.owner,
                    &operation.admission, &mut operation.state, &AtomicBool::new(false), fault, Some(&control));
                result.finish(outcome)
            }))
        };
        if fault == 4 || fault == 6 { assert!(outcome.is_err()); }
        else { assert!(outcome.as_ref().unwrap().result().is_err()); }
        assert!(operation.capture_attempted());
        assert!(operation.owner.session.store.owns_writer_lock());
        assert!(operation.owner.session.store.db.is_autocommit());
        assert!(!backing.state_for_test().1 && !backing.state_for_test().2);
        drop(outcome);
        // Only original operation slots survive; no proof, inventory or image
        // guard leaks after the actual payloads were unwound/destroyed.
        let held = context.temporary_bytes_for_test().unwrap();
        assert!(held < 64 * 1024);
        assert!(operation.capture(&AtomicBool::new(false)).is_err());
        drop(operation);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
    }
}

#[test]
fn current_capture_workspace_direct_drop_destroys_real_image_before_slots_and_source() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    struct ImageCloseProbe {
        context: allocation::AllocationContext,
        path: PathBuf,
        expected_slots: u64,
        observed: Arc<AtomicUsize>,
    }
    impl Drop for ImageCloseProbe {
        fn drop(&mut self) {
            // SQLite drops the actual installed function closure when this
            // copied connection closes. Observe rather than panic over FFI.
            let slots = self.context.temporary_bytes_for_test()
                .is_ok_and(|bytes| bytes >= self.expected_slots && bytes != 0);
            let source = crate::writer_lock(&self.path).is_err();
            self.observed.store(1 | (usize::from(slots) << 1) | (usize::from(source) << 2), Ordering::Release);
        }
    }
    let (_root, owner, mut backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let path = operation.owner.session.store.path.clone();
    let token = operation.borrow_control_workspace(&mut backing).unwrap();
    let result = operation.capture_with_control_workspace(&AtomicBool::new(false), token);
    assert!(result.result().is_ok(), "{result:?}");
    drop(result);
    let expected_slots = context.temporary_bytes_for_test().unwrap();
    assert!(expected_slots != 0);
    assert!(operation.capture(&AtomicBool::new(false)).is_err());
    let CaptureState::Captured(image, _, _) = &operation.state else { panic!("completed image was discarded by re-entry refusal") };
    let observed = Arc::new(AtomicUsize::new(0));
    let probe = ImageCloseProbe { context: context.clone(), path: path.clone(), expected_slots, observed: observed.clone() };
    image.planning_db().create_scalar_function("current_capture_drop_probe", 0,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8, move |_| {
            let _retained_until_real_copy_close = &probe;
            Ok(0i64)
        }).unwrap();
    drop(operation);
    assert_eq!(observed.load(Ordering::Acquire), 7, "image must close while original slots and source lock are live");
    assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
    assert!(crate::writer_lock(&path).is_ok());
}


#[test]
fn owned_current_capture_transfer_faults_retain_original_and_real_failed_close() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    for fault in 7..=13 {
        let (_root, owner, mut backing) = workspace_fixture();
        let mut operation = typed_operation(owner);
        let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
        let path = operation.owner.session.store.path.clone();
        let before = std::fs::read(&path).unwrap();
        let result = {
            let token = operation.borrow_control_workspace(&mut backing).unwrap();
            catch_unwind(AssertUnwindSafe(|| {
                let CurrentImageCaptureWorkspace { result, loan } = token;
                let control = BootstrapWorkspace::from_capture_loan(&operation.owner.session.store, loan);
                let value = CurrentImageCaptureOperation::capture_parts(&operation.owner,
                    &operation.admission, &mut operation.state, &AtomicBool::new(false), fault, Some(&control));
                result.finish(value)
            }))
        };
        if [8, 10, 12].contains(&fault) { assert!(result.is_err()); }
        else { assert!(result.as_ref().unwrap().result().is_err()); }
        if fault == 13 {
            match result.as_ref().unwrap().result() {
                Err(error) => assert_eq!(error.to_string(), "not authorized"),
                Ok(_) => panic!("denied deserialize returned an image"),
            }
        }
        assert!(operation.capture_attempted());
        assert!(operation.owner.session.store.owns_writer_lock());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!backing.state_for_test().1 && !backing.state_for_test().2);
        assert!(operation.capture(&AtomicBool::new(false)).is_err());
        if fault == 11 {
            assert!(operation.owner.failed_capture.borrow().as_ref().unwrap().retained_busy_copy_for_test());
            assert!(operation.owner.validate_current().is_err());
            let owner = match operation.finish() {
                Err(failure) => failure.into_session(),
                Ok(_) => panic!("failed producer close returned an image"),
            };
            assert!(owner.failed_capture.borrow().as_ref().unwrap().retained_busy_copy_for_test());
            assert!(owner.validate_current().is_err());
            assert!(context.temporary_bytes_for_test().unwrap() > 1024 * 1024);
            drop(owner);
        } else {
            assert!(operation.owner.failed_capture.borrow().is_none());
            assert!(operation.owner.session.store.db.is_autocommit());
            drop(operation);
        }
        drop(result);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
        assert!(crate::writer_lock(&path).is_ok());
    }
}

#[test]
fn owned_current_capture_committed_guard_cannot_reopen_peak_or_replay() {
    let (_root, owner, _backing) = workspace_fixture();
    let store = &owner.session.store;
    let context = store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let mut phase = owner.session.image_allocation().unwrap();
    phase.admit(4096).unwrap();
    let mut image = phase.into_owned();
    let mut history = context.reserve(&store.db, 1024).unwrap();
    // A construction guard cannot publish persistent output. Sealing is a
    // one-shot exact-payload transition, not a new reusable growth allowance.
    assert!(image.commit_current_capture(&store.db, &mut history, 5120).is_err());
    image.seal_current_capture(&store.db, 4096).unwrap();
    assert!(image.admit(&store.db, 1).is_err());
    assert!(image.seal_current_capture(&store.db, 4096).is_err());
    image.commit_current_capture(&store.db, &mut history, 5120).unwrap();
    let before = context.retained_components_for_test().unwrap();
    assert!(image.admit(&store.db, 1).is_err());
    assert!(image.admit(&store.db, 8192).is_err());
    assert!(image.commit_current_capture(&store.db, &mut history, 8192).is_err());
    assert_eq!(context.retained_components_for_test().unwrap(), before);
}

#[test]
fn current_image_workspace_refresh_retains_exact_image_history_and_original_context() {
    let (_root, owner, mut backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    let context = operation.owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let identity = context.preparation_state_for_test().unwrap();
    let source = unsafe { operation.owner.session.store.db.handle() } as usize;
    let deadline = operation.deadline();
    let token = operation.borrow_control_workspace(&mut backing).unwrap();
    let result = operation.capture_with_control_workspace(&AtomicBool::new(false), token);
    assert!(result.result().is_ok(), "{result:?}");
    assert_eq!(operation.deadline(), deadline);
    let mut image = operation.finish().unwrap();
    let original = snapshot(&image.owner.session.store.db).unwrap();
    let image_bytes = image.image.retained_allocations().unwrap();
    let history_bytes = retained_history(&image.history).unwrap();
    let native = image.retained_allocations().unwrap();
    let forwarded = image.external_retained_reservation().unwrap() + 1;
    assert!(native + forwarded < allocation::LIMIT);
    // Constructor floor is independent of this real fixture's measured image
    // footprint. High-K whole-service fit is tested by the genuine service,
    // not presumed from an arbitrary8MiB remaining native margin here.
    assert!(OfflineAllocationReservation::new(allocation::LIMIT - 32 * 1024 * 1024 + 1).is_err());
    image.refresh_external_reservation_with_control_workspace(forwarded, &mut backing).unwrap();
    assert_eq!(image.external_retained_reservation().unwrap(), forwarded);
    assert_eq!(context.preparation_state_for_test().unwrap(), identity);
    assert_eq!(unsafe { image.owner.session.store.db.handle() } as usize, source);
    assert_eq!(snapshot(&image.owner.session.store.db).unwrap(), original);
    assert_eq!(image.image.retained_allocations().unwrap(), image_bytes);
    assert_eq!(retained_history(&image.history).unwrap(), history_bytes);
    assert_eq!(image.source_effects().len(), 1);
    assert_additive_retention(&image);
    let temporary = context.temporary_bytes_for_test().unwrap();
    assert_eq!(backing.state_for_test(), (forwarded, false, false));
    assert!(image.refresh_external_reservation_with_control_workspace(forwarded - 1, &mut backing).is_err());
    assert!(image.refresh_external_reservation_with_control_workspace(allocation::LIMIT, &mut backing).is_err());
    assert_eq!(image.external_retained_reservation().unwrap(), forwarded);
    assert_eq!(context.temporary_bytes_for_test().unwrap(), temporary);
    assert!(image.owner.session.store.owns_writer_lock());
    assert!(result.result().is_ok());
}

#[test]
fn current_image_workspace_refresh_missing_foreign_and_dropped_backing_refuse_without_loss() {
    let (_root, owner, mut backing) = workspace_fixture();
    let (_foreign_root, foreign, mut foreign_backing) = workspace_fixture();
    let mut operation = typed_operation(owner);
    let token = operation.borrow_control_workspace(&mut backing).unwrap();
    let result = operation.capture_with_control_workspace(&AtomicBool::new(false), token);
    assert!(result.result().is_ok(), "{result:?}");
    let mut image = operation.finish().unwrap();
    let before = image.external_retained_reservation().unwrap();
    let original = snapshot(&image.owner.session.store.db).unwrap();
    let mut missing = allocation::OfflineControlWorkspaceBacking::new();
    assert!(image.refresh_external_reservation_with_control_workspace(before + 1, &mut missing).is_err());
    assert!(image.refresh_external_reservation_with_control_workspace(before + 1, &mut foreign_backing).is_err());
    drop(backing);
    let mut replacement = allocation::OfflineControlWorkspaceBacking::new();
    assert!(image.refresh_external_reservation_with_control_workspace(before + 1, &mut replacement).is_err());
    assert_eq!(image.external_retained_reservation().unwrap(), before);
    assert_eq!(snapshot(&image.owner.session.store.db).unwrap(), original);
    assert_eq!(image.source_effects().len(), 1);
    assert_additive_retention(&image);
    assert!(image.owner.session.store.owns_writer_lock());
    assert!(foreign.session.store.owns_writer_lock());
    assert!(result.result().is_ok());
}

#[path = "current_image_cache_tests.rs"]
mod cache_policy_tests;

#[path = "current_image_verifier_cache_tests.rs"]
mod verifier_cache_tests;
