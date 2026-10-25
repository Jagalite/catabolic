//! Public fixed-workspace calls on genuine retained closed40 -> closed41 owners.
//! Every output is observed through its borrowed result; no synthetic control
//! admission, extracted output, replacement Store, or reopened source is used.
use super::*;
use crate::migration;
use std::{
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const HOST: &str = "public-bootstrap-workspace-fixture";

fn quiescence() -> NativeUpgradeQuiescence {
    NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}
pub(super) fn migrated_with_control() -> (
    tempfile::TempDir,
    PathBuf,
    OfflineBootstrap<'static>,
    OfflineControlWorkspaceBacking,
) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize_native_at(&path, 40).unwrap();
    let mut backing = OfflineControlWorkspaceBacking::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut preparation = OfflinePreparationOperation::admit_with_control_workspace(
        &path,
        quiescence(),
        OfflineAllocationReservation::new(1024).unwrap(),
        &mut backing,
        deadline,
    ).unwrap();
    preparation.prepare(&AtomicBool::new(false)).unwrap();
    let mut prepared = preparation.finish().unwrap();
    prepared.admit_apply_operation(None, deadline).unwrap();
    let mut apply = prepared.begin_apply().unwrap();
    apply.apply_with_control_workspace(&AtomicBool::new(false), &mut backing).unwrap();
    let owner = apply.finish().unwrap();
    assert_eq!(backing.state_for_test(), (OfflineControlWorkspaceBacking::BYTES + 1024, false, false));
    (root, path, owner, backing)
}
fn command(owner: &OfflineBootstrap<'_>, generation: i64, payload: &Value) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(owner.catalog_id(), HOST, "empty-bootstrap").unwrap(),
        owner.incarnation(),
        generation,
        &payload.to_string(),
    ).unwrap()
}
fn assert_custody(owner: &OfflineBootstrap<'_>, path: &Path, identity: (usize, usize, usize)) {
    assert_eq!(owner.session.completion_custody_for_test(), identity);
    assert!(owner.session.completion_store_for_test().owns_writer_lock());
    assert!(crate::writer_lock(path).is_err());
    assert!(crate::native_admission_exclusive(path).is_err());
}
fn assert_released(path: &Path) {
    drop(crate::writer_lock(path).unwrap());
    drop(crate::native_admission_exclusive(path).unwrap());
}
fn charged_output<T, E>(
    allocation: &native_session::allocation::AllocationContext,
    call: impl FnOnce() -> std::result::Result<OfflineRetainedResult<T, E>, OfflineWorkspaceAdmissionRefused>,
) -> (OfflineRetainedResult<T, E>, u64) {
    let before = allocation.temporary_bytes_for_test().unwrap();
    let output = call().unwrap();
    let charged = allocation.temporary_bytes_for_test().unwrap().checked_sub(before).unwrap();
    // Each call independently owns its full bounded success/error cohort.
    assert!(charged >= 2 * 64 * 1024);
    (output, charged)
}

#[test]
fn seven_public_control_outputs_survive_actual_source_close_and_retire_independently() {
    let (_root, path, mut owner, mut backing) = migrated_with_control();
    let identity = owner.session.completion_custody_for_test();
    let (allocation, strings) = owner.session.apply_conversion_allocation_for_test();
    assert_eq!(strings, 1920);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), strings);
    let full = backing.state_for_test().0;

    let (first_transition, first_charge) = charged_output(&allocation, || owner.transition_with_control_workspace(&mut backing));
    let (generation, generation_charge) = charged_output(&allocation, || owner.generation_with_control_workspace(&mut backing));
    let (preparation, preparation_charge) = charged_output(&allocation, || owner.prepare_empty_with_control_workspace(HOST, &mut backing));
    let command = command(&owner, *generation.result().as_ref().unwrap(), preparation.result().as_ref().unwrap());
    let (reconciliation, reconciliation_charge) = charged_output(&allocation, || owner.reconcile_with_control_workspace(&command, HOST, &mut backing));
    assert!(reconciliation.result().as_ref().unwrap().is_none());
    let (execution, execution_charge) = charged_output(&allocation, || owner.execute_empty_with_control_workspace(&command, HOST, policy(), &mut backing));
    assert!(matches!(execution.result(), Ok(CommandOutcome::Committed { .. })));
    let (second_transition, second_charge) = charged_output(&allocation, || owner.transition_with_control_workspace(&mut backing));
    assert_eq!(first_transition.result().as_ref().unwrap().digest(), second_transition.result().as_ref().unwrap().digest());
    assert_eq!(first_transition.result().as_ref().unwrap().catalog_id(), owner.catalog_id());
    assert_eq!(first_transition.result().as_ref().unwrap().incarnation(), owner.incarnation());
    assert_eq!(first_transition.result().as_ref().unwrap().approved_plan_sha256().len(), 64);
    assert!(first_transition.result().as_ref().unwrap().source_generation() <= *generation.result().as_ref().unwrap());
    assert_custody(&owner, &path, identity);
    assert_eq!(backing.state_for_test(), (full, false, false));

    owner.admit_completion_operation(&command, Instant::now() + Duration::from_secs(60)).unwrap();
    let mut completion = owner.begin_completion().unwrap();
    let (complete, complete_charge) = charged_output(&allocation, || completion.complete_with_control_workspace(&AtomicBool::new(false), &mut backing));
    assert!(complete.result().is_ok());
    let facts = completion.measured_facts().unwrap();
    assert_eq!(facts.source_count(), 0);
    assert_eq!(facts.command_key(), command.key());
    assert_eq!(facts.transition_digest(), first_transition.result().as_ref().unwrap().digest());
    let completed = completion.finish().unwrap();
    assert_eq!(completed.completion_custody_for_test(), identity);
    assert!(crate::writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
    assert_eq!(backing.state_for_test(), (full, false, false));

    // Close the actual source and backing before ANY of the seven wrappers.
    // The surviving context is accounting only, not native custody or a grant.
    drop(completed);
    drop(backing);
    assert_released(&path);
    let mut remaining = first_charge + generation_charge + preparation_charge
        + reconciliation_charge + execution_charge + second_charge + complete_charge;
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), remaining);
    assert_eq!(allocation.external_retained_bytes_for_test().unwrap(), full);
    assert!(first_transition.result().is_ok());
    assert!(generation.result().is_ok());
    assert!(preparation.result().is_ok());
    assert!(reconciliation.result().as_ref().unwrap().is_none());
    assert!(matches!(execution.result(), Ok(CommandOutcome::Committed { .. })));
    assert!(second_transition.result().is_ok());
    assert!(complete.result().is_ok());

    drop(preparation);
    remaining -= preparation_charge;
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), remaining);
    drop(second_transition);
    remaining -= second_charge;
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), remaining);
    drop(complete);
    remaining -= complete_charge;
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), remaining);
    drop(first_transition);
    remaining -= first_charge;
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), remaining);
    drop(execution);
    remaining -= execution_charge;
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), remaining);
    drop(generation);
    remaining -= generation_charge;
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), remaining);
    drop(reconciliation);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
    assert_eq!(allocation.external_retained_bytes_for_test().unwrap(), full);
}

#[test]
fn repeated_public_preparations_accumulate_while_prior_outputs_remain_alive() {
    let (_root, path, owner, mut backing) = migrated_with_control();
    let (allocation, strings) = owner.session.apply_conversion_allocation_for_test();
    let full = backing.state_for_test().0;
    let (first, first_charge) = charged_output(&allocation, || owner.prepare_empty_with_control_workspace(HOST, &mut backing));
    let (second, second_charge) = charged_output(&allocation, || owner.prepare_empty_with_control_workspace(HOST, &mut backing));
    let (third, third_charge) = charged_output(&allocation, || owner.prepare_empty_with_control_workspace(HOST, &mut backing));
    assert_eq!(first.result().as_ref().unwrap(), second.result().as_ref().unwrap());
    assert_eq!(first.result().as_ref().unwrap(), third.result().as_ref().unwrap());
    assert_eq!(first_charge, second_charge);
    assert_eq!(second_charge, third_charge);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), strings + first_charge + second_charge + third_charge);
    assert_eq!(backing.state_for_test(), (full, false, false));
    drop(owner);
    assert_released(&path);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), first_charge + second_charge + third_charge);
    drop(second);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), first_charge + third_charge);
    assert!(first.result().is_ok());
    assert!(third.result().is_ok());
    drop(first);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), third_charge);
    drop(third);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
    assert_eq!(backing.state_for_test(), (full, false, false));
}

#[test]
fn unregistered_and_foreign_backings_refuse_every_public_call_without_lending_or_consuming_custody() {
    for foreign in [false, true] {
        let (_root, path, mut owner, mut original) = migrated_with_control();
        let (_other_root, other_path, other_owner, mut other_backing) = migrated_with_control();
        let identity = owner.session.completion_custody_for_test();
        let other_identity = other_owner.session.completion_custody_for_test();
        let (allocation, strings) = owner.session.apply_conversion_allocation_for_test();
        let full = original.state_for_test().0;
        let other_full = other_backing.state_for_test().0;
        let payload = owner.prepare(HOST, &[]).unwrap();
        let command = command(&owner, owner.generation().unwrap(), &payload);
        let mut unregistered = OfflineControlWorkspaceBacking::new();
        {
            let wrong = if foreign { &mut other_backing } else { &mut unregistered };
            assert!(owner.transition_with_control_workspace(wrong).is_err());
            assert!(owner.generation_with_control_workspace(wrong).is_err());
            assert!(owner.prepare_empty_with_control_workspace(HOST, wrong).is_err());
            assert!(owner.reconcile_with_control_workspace(&command, HOST, wrong).is_err());
            assert!(owner.execute_empty_with_control_workspace(&command, HOST, policy(), wrong).is_err());
        }
        assert_eq!(allocation.temporary_bytes_for_test().unwrap(), strings);
        assert_eq!(original.state_for_test(), (full, false, false));
        assert_eq!(other_backing.state_for_test(), (other_full, false, false));
        assert_custody(&owner, &path, identity);
        assert_custody(&other_owner, &other_path, other_identity);
        assert!(owner.reconcile(&command, HOST).unwrap().is_none());
        assert!(matches!(owner.execute(&command, HOST, &[], policy()).unwrap(), CommandOutcome::Committed { .. }));
        owner.admit_completion_operation(&command, Instant::now() + Duration::from_secs(60)).unwrap();
        let held = allocation.temporary_bytes_for_test().unwrap();
        let mut completion = owner.begin_completion().unwrap();
        {
            let wrong = if foreign { &mut other_backing } else { &mut unregistered };
            assert!(completion.complete_with_control_workspace(&AtomicBool::new(false), wrong).is_err());
        }
        assert!(completion.measured_facts().is_none());
        assert_eq!(allocation.temporary_bytes_for_test().unwrap(), held);
        assert_eq!(original.state_for_test(), (full, false, false));
        assert_eq!(other_backing.state_for_test(), (other_full, false, false));
        assert!(crate::writer_lock(&path).is_err());
        assert!(crate::native_admission_exclusive(&path).is_err());
        // Completion claims its one attempt before workspace admission. Even
        // supplying the original backing afterward cannot revive that attempt.
        assert!(completion.complete_with_control_workspace(&AtomicBool::new(false), &mut original).is_err());
        assert!(completion.complete(&AtomicBool::new(false)).is_err());
        assert!(completion.measured_facts().is_none());
        let failed = match completion.finish() {
            Err(failed) => failed,
            Ok(_) => panic!("refused completion published a completed owner"),
        };
        assert!(crate::writer_lock(&path).is_err());
        assert!(crate::native_admission_exclusive(&path).is_err());
        assert_eq!(allocation.temporary_bytes_for_test().unwrap(), held);
        drop(failed);
        assert_released(&path);
        assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
    }
}

#[test]
fn output_admission_refusal_keeps_actual_custody_and_terminalizes_completion() {
    let (_root, path, mut owner, mut backing) = migrated_with_control();
    let identity = owner.session.completion_custody_for_test();
    let (allocation, strings) = owner.session.apply_conversion_allocation_for_test();
    let full = backing.state_for_test().0;
    let payload = owner.prepare(HOST, &[]).unwrap();
    let command = command(&owner, owner.generation().unwrap(), &payload);
    let full_headroom = owner.session.fill_completion_headroom_for_test().unwrap();
    let filled = allocation.temporary_bytes_for_test().unwrap();
    assert!(owner.transition_with_control_workspace(&mut backing).is_err());
    assert!(owner.generation_with_control_workspace(&mut backing).is_err());
    assert!(owner.prepare_empty_with_control_workspace(HOST, &mut backing).is_err());
    assert!(owner.reconcile_with_control_workspace(&command, HOST, &mut backing).is_err());
    assert!(owner.execute_empty_with_control_workspace(&command, HOST, policy(), &mut backing).is_err());
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), filled);
    assert_eq!(backing.state_for_test(), (full, false, false));
    assert_custody(&owner, &path, identity);
    drop(full_headroom);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), strings);
    assert!(owner.reconcile(&command, HOST).unwrap().is_none());
    assert!(matches!(owner.execute(&command, HOST, &[], policy()).unwrap(), CommandOutcome::Committed { .. }));
    owner.admit_completion_operation(&command, Instant::now() + Duration::from_secs(60)).unwrap();
    let full_headroom = owner.session.fill_completion_headroom_for_test().unwrap();
    let filled = allocation.temporary_bytes_for_test().unwrap();
    let mut completion = owner.begin_completion().unwrap();
    assert!(completion.complete_with_control_workspace(&AtomicBool::new(false), &mut backing).is_err());
    assert!(completion.measured_facts().is_none());
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), filled);
    assert_eq!(backing.state_for_test(), (full, false, false));
    assert!(crate::writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
    drop(full_headroom);
    assert!(completion.complete_with_control_workspace(&AtomicBool::new(false), &mut backing).is_err());
    assert!(completion.complete(&AtomicBool::new(false)).is_err());
    assert!(completion.measured_facts().is_none());
    let failed = match completion.finish() {
        Err(failed) => failed,
        Ok(_) => panic!("output-admission refusal published a completed owner"),
    };
    assert!(crate::writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
    assert_eq!(backing.state_for_test(), (full, false, false));
    assert!(allocation.temporary_bytes_for_test().unwrap() > strings);
    drop(failed);
    assert_released(&path);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
}

#[test]
fn ordinary_validation_keeps_full_scratch_and_scalar_refresh_cannot_register_backing() {
    let (_root, path, mut owner, mut backing) = migrated_with_control();
    let identity = owner.session.completion_custody_for_test();
    let (allocation, strings) = owner.session.apply_conversion_allocation_for_test();
    // The largest legal scalar reservation leaves less than one independent
    // 32MiB metadata phase because real native retention is already nonzero.
    // It never creates a control grant; only the original backing can lend.
    let full = 224 * 1024 * 1024;
    owner.refresh_external_reservation(OfflineAllocationReservation::new(full).unwrap()).unwrap();
    assert!(owner.generation().is_err());
    assert!(owner.transition().is_err());
    assert!(owner.prepare(HOST, &[]).is_err());
    assert!(owner.session.completion_store_for_test().validate_native_identity().is_err());
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), strings);
    assert_eq!(backing.state_for_test(), (full, false, false));
    let output = owner.generation_with_control_workspace(&mut backing).unwrap();
    assert!(output.result().is_ok());
    assert_eq!(backing.state_for_test(), (full, false, false));
    assert_custody(&owner, &path, identity);
    drop(output);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), strings);

    let scalar_root = tempfile::tempdir().unwrap();
    let scalar_path = scalar_root.path().join("catalog.db");
    migration::initialize_native_at(&scalar_path, 40).unwrap();
    let mut scalar = PreparedOfflineBootstrap::prepare(&scalar_path, quiescence()).unwrap().apply(None).unwrap();
    scalar.refresh_external_reservation(OfflineAllocationReservation::new(OfflineControlWorkspaceBacking::BYTES).unwrap()).unwrap();
    let scalar_identity = scalar.session.completion_custody_for_test();
    let mut new_backing = OfflineControlWorkspaceBacking::new();
    assert!(scalar.generation_with_control_workspace(&mut new_backing).is_err());
    assert!(scalar.generation_with_control_workspace(&mut backing).is_err());
    assert_eq!(backing.state_for_test(), (full, false, false));
    assert_custody(&scalar, &scalar_path, scalar_identity);
    assert!(scalar.generation().is_ok());
    drop(scalar);
    assert_released(&scalar_path);
    drop(owner);
    assert_released(&path);
    assert_eq!(allocation.temporary_bytes_for_test().unwrap(), 0);
}
