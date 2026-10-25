//! Real closed40 apply calls and exact phase accounting, without a new public
//! callback or a production diagnostic. Only these tests can fill headroom at
//! the post-image/pre-restoration boundary.
use super::*;
use std::cell::Cell;

#[derive(Clone, Copy, Default)]
struct Observation {
    refuse_restore: bool,
    construction: u64,
    retained: u64,
    retained_temporary: u64,
    before_restore: u64,
    restored_temporary: u64,
}
thread_local! {
    static OBSERVATION: Cell<Observation> = const { Cell::new(Observation {
        refuse_restore: false, construction: 0, retained: 0,
        retained_temporary: 0, before_restore: 0, restored_temporary: 0,
    }) };
}
fn reset(refuse_restore: bool) {
    OBSERVATION.set(Observation { refuse_restore, ..Observation::default() });
}
pub(super) fn retained_phase(context: &allocation::AllocationContext, construction: u64, retained: u64) {
    OBSERVATION.with(|slot| {
        let mut state = slot.get();
        state.construction = construction;
        state.retained = retained;
        state.retained_temporary = context.temporary_bytes_for_test().unwrap();
        slot.set(state);
    });
}
pub(super) fn before_restore(
    context: &allocation::AllocationContext,
    source: &Connection,
) -> Option<allocation::AllocationGuard> {
    let refuse = OBSERVATION.with(|slot| {
        let mut state = slot.get();
        state.before_restore = context.temporary_bytes_for_test().unwrap();
        let refuse = state.refuse_restore;
        state.refuse_restore = false;
        slot.set(state);
        refuse
    });
    if !refuse { return None; }
    let held = context.retained_native_bytes(source).unwrap()
        + context.external_retained_bytes().unwrap();
    Some(context.reserve(source, allocation::LIMIT - held).unwrap())
}
pub(super) fn restored_phase(context: &allocation::AllocationContext) {
    OBSERVATION.with(|slot| {
        let mut state = slot.get();
        state.restored_temporary = context.temporary_bytes_for_test().unwrap();
        slot.set(state);
    });
}
fn fixture(control: bool) -> (tempfile::TempDir, RetainedNativeUpgrade, Value, Option<OfflineControlWorkspaceBacking>) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    initialize_native_at(&path, 40).unwrap();
    let mut backing = control.then(OfflineControlWorkspaceBacking::new);
    let context = backing.as_mut().map(|backing| backing.admit(OfflineAllocationReservation::default()).unwrap());
    let mut owner = RetainedNativeUpgrade::acquire(&path, NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }).unwrap();
    if let Some(context) = context {
        context.bind_source(&owner.db).unwrap();
        owner.allocation = Some(context);
    }
    let (owner, plan) = owner.plan_closed41().unwrap();
    (root, owner, plan, backing)
}
fn manifest_value(owner: &RetainedNativeUpgrade) -> Value {
    let path = owner.verified_backup.as_ref().unwrap().path().parent().unwrap().join("manifest.json");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn assert_failed_custody(owner: &RetainedNativeUpgrade, context: &allocation::AllocationContext, baseline: u64) {
    assert_eq!(owner.db.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0)).unwrap(), 40);
    assert!(owner.db.is_autocommit());
    assert!(owner.verified_backup.is_some());
    assert!(owner.retained_preservation.is_some());
    assert!(writer_lock(&owner.path).is_err());
    assert!(crate::native_admission_exclusive(&owner.path).is_err());
    assert_eq!(context.temporary_bytes_for_test().unwrap(), baseline);
}
fn operation() -> (tempfile::TempDir, super::super::offline_bootstrap::OfflineApplyOperation, OfflineControlWorkspaceBacking) {
    use super::super::offline_bootstrap::OfflinePreparationOperation;
    use std::{sync::atomic::AtomicBool, time::{Duration, Instant}};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("controlled.sqlite3");
    initialize_native_at(&path, 40).unwrap();
    let mut backing = OfflineControlWorkspaceBacking::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut preparation = OfflinePreparationOperation::admit_with_control_workspace(
        &path,
        NativeUpgradeQuiescence {
            admission_stopped: true, readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        },
        OfflineAllocationReservation::default(), &mut backing, deadline,
    ).unwrap();
    preparation.prepare(&AtomicBool::new(false)).unwrap();
    let mut prepared = preparation.finish().unwrap();
    prepared.admit_apply_operation(None, deadline).unwrap();
    (root, prepared.begin_apply().unwrap(), backing)
}

#[test]
fn apply_rehearsal_uses_actual_survivors_then_restores_full_construction() {
    use std::sync::atomic::AtomicBool;
    reset(false);
    let (_root, mut operation, mut backing) = operation();
    let context = operation.apply_lifetime_owner_for_test().allocation.as_ref().unwrap().clone();
    let baseline = context.temporary_bytes_for_test().unwrap();
    let external = context.external_retained_bytes().unwrap();
    operation.apply_with_control_workspace(&AtomicBool::new(false), &mut backing).unwrap();
    let observed = OBSERVATION.get();
    assert!(observed.retained > 0 && observed.retained < observed.construction);
    assert_eq!(observed.retained_temporary, baseline + observed.retained);
    // Both image connection and its complete4×pages+SQLite guard have died.
    assert_eq!(observed.before_restore, baseline + observed.retained);
    assert_eq!(observed.restored_temporary, baseline + observed.construction);
    assert_eq!(context.external_retained_bytes().unwrap(), external);
    let receipt = manifest_value(operation.apply_lifetime_owner_for_test());
    assert_eq!(receipt["status"], "committed");
    let bootstrap = operation.finish().unwrap();
    let plan = bootstrap.approved_plan();
    assert_eq!(observed.construction, 6 * allocation::value_bytes(plan).unwrap());
    assert_eq!(receipt["plan"], *plan);
    assert_eq!(receipt["tables_before"], plan["input"]["tables"]);
    assert_eq!(context.temporary_bytes_for_test().unwrap(), 1920);
    drop(bootstrap);
    assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
}

#[test]
fn successful_image_restore_refusal_skips_only_optional_failure_manifest() {
    use std::sync::atomic::AtomicBool;
    let (_root, mut operation, mut backing) = operation();
    let context = operation.apply_lifetime_owner_for_test().allocation.as_ref().unwrap().clone();
    let baseline = context.temporary_bytes_for_test().unwrap();
    reset(true);
    let error = operation.apply_with_control_workspace(&AtomicBool::new(false), &mut backing).unwrap_err();
    assert_eq!(error.0, "offline aggregate allocation budget exhausted");
    let owner = operation.apply_lifetime_owner_for_test();
    let receipt = manifest_value(owner);
    assert_eq!(receipt["status"], "backup_verified");
    assert!(receipt.get("error").is_none());
    assert_eq!(OBSERVATION.get().restored_temporary, 0);
    assert_failed_custody(owner, &context, baseline);
    let failure = match operation.finish() {
        Err(failure) => failure,
        Ok(_) => panic!("restoration refusal minted a bootstrap owner"),
    };
    assert_eq!(context.temporary_bytes_for_test().unwrap(), baseline);
    drop(failure);
    assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
}

#[test]
fn image_error_and_restore_refusal_preserve_bounded_error_and_backup_receipt() {
    use std::sync::atomic::AtomicBool;
    for fault in [15, 17] {
        let (_root, mut operation, mut backing) = operation();
        let context = operation.apply_lifetime_owner_for_test().allocation.as_ref().unwrap().clone();
        let baseline = context.temporary_bytes_for_test().unwrap();
        reset(true);
        let error = operation.apply_lifetime_control_for_test(&AtomicBool::new(false), &mut backing, fault).unwrap_err();
        assert_eq!(error.0, if fault == 15 { "exact original image failure" } else { "offline apply error exceeded reserved output storage" });
        assert!(error.0.capacity() <= 64 * 1024);
        let owner = operation.apply_lifetime_owner_for_test();
        let receipt = manifest_value(owner);
        assert_eq!(receipt["status"], "backup_verified");
        assert!(receipt.get("error").is_none());
        assert_failed_custody(owner, &context, baseline);
    }
}

#[test]
fn image_failure_with_restored_construction_keeps_existing_failure_manifest() {
    use std::sync::atomic::AtomicBool;
    let (_root, mut operation, mut backing) = operation();
    let context = operation.apply_lifetime_owner_for_test().allocation.as_ref().unwrap().clone();
    let baseline = context.temporary_bytes_for_test().unwrap();
    reset(false);
    let error = operation.apply_lifetime_control_for_test(&AtomicBool::new(false), &mut backing, 15).unwrap_err();
    assert_eq!(error.0, "exact original image failure");
    let owner = operation.apply_lifetime_owner_for_test();
    let receipt = manifest_value(owner);
    assert_eq!(receipt["status"], "failed");
    assert_eq!(receipt["error"], "exact original image failure");
    assert!(OBSERVATION.get().restored_temporary > 0);
    assert_failed_custody(owner, &context, baseline);
}

#[test]
fn reduced_copy_phase_unwind_releases_images_but_retains_original_custody() {
    use std::sync::atomic::AtomicBool;
    let (_root, mut operation, mut backing) = operation();
    let context = operation.apply_lifetime_owner_for_test().allocation.as_ref().unwrap().clone();
    let baseline = context.temporary_bytes_for_test().unwrap();
    reset(false);
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = operation.apply_lifetime_control_for_test(&AtomicBool::new(false), &mut backing, 16);
    }));
    assert!(caught.is_err());
    assert!(OBSERVATION.get().retained > 0);
    assert_eq!(OBSERVATION.get().before_restore, 0);
    let owner = operation.apply_lifetime_owner_for_test();
    assert_eq!(manifest_value(owner)["status"], "backup_verified");
    assert_failed_custody(owner, &context, baseline);
}

#[test]
fn direct_control_loan_and_ordinary_calls_keep_full_copy_charge_and_error() {
    for control in [false, true] {
        let (_root, mut owner, plan, mut backing) = fixture(control);
        let context = owner.allocation.as_ref().unwrap().clone();
        let full = 6 * allocation::value_bytes(&plan).unwrap();
        let metadata = if control { 0 } else { allocation::MIGRATION_SCRATCH };
        let pointer = Cell::new(0usize);
        reset(false);
        let checkpoint = |stage: &str, _: &Connection, _: u32| {
            if stage == "upgrade:after_backup" {
                assert_eq!(context.temporary_bytes_for_test().unwrap(), metadata + full);
            }
            if stage == "rehearsal:before_commit" {
                assert_eq!(OBSERVATION.get().retained, 0);
                assert!(context.temporary_bytes_for_test().unwrap() > metadata + full);
                let mut text = String::with_capacity(100_000);
                text.push_str("unchanged direct error limitation");
                pointer.set(text.as_ptr() as usize);
                return Err(Error(text));
            }
            Ok(())
        };
        let error = if let Some(backing) = backing.as_mut() {
            let mut control = owner.control_workspace_loan(backing).unwrap();
            owner.apply_closed41_with_control_workspace(&plan, None, &checkpoint, Some(&mut control)).unwrap_err()
        } else {
            owner.apply_closed41_borrowed(&plan, None, &checkpoint).unwrap_err()
        };
        assert_eq!(error.0.as_ptr() as usize, pointer.get());
        assert!(error.0.capacity() >= 100_000);
        assert_eq!(OBSERVATION.get().retained, 0);
        assert_eq!(manifest_value(&owner)["status"], "failed");
        assert_failed_custody(&owner, &context, 0);
    }
}

#[test]
fn admitted_output_witness_requires_its_actual_native_context_and_source() {
    let (_root_a, operation_a, _backing_a) = operation();
    let (_root_b, operation_b, _backing_b) = operation();
    operation_a.apply_lifetime_require_output_owner_for_test(operation_a.apply_lifetime_owner_for_test()).unwrap();
    assert!(operation_a.apply_lifetime_require_output_owner_for_test(operation_b.apply_lifetime_owner_for_test()).is_err());
    assert!(operation_b.apply_lifetime_require_output_owner_for_test(operation_a.apply_lifetime_owner_for_test()).is_err());
}
