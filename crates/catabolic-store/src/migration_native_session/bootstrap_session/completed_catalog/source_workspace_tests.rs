//! Genuine original40 registration, migration, bootstrap and completed-source
//! ownership. Fixed empty-source renewal exercises all source operation layers
//! except nonempty structural-context decoding, which has its own focused probe.
use super::*;
use super::super::tests::{HOST, command as bootstrap_command, policy};
use catabolic_core::catalog_command::{CommandKey, ReceiptVisibility};
use std::sync::atomic::Ordering;

pub(super) fn authority() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
pub(super) fn fixture() -> (tempfile::TempDir, CompletedCatalogSession, allocation::OfflineControlWorkspaceBacking, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source-workspace.db");
    let source = root.path().join("source");
    let candidate = root.path().join("candidate");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&candidate).unwrap();
    initialize_native_at(&path, 40).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    crate::catalog_import_typed::register_host_source(&store, "default", "native-source", &source, &[]).unwrap();
    drop(store);
    let mut backing = allocation::OfflineControlWorkspaceBacking::new();
    let context = backing.admit(OfflineAllocationReservation::default()).unwrap();
    let mut owner = RetainedNativeUpgrade::acquire(&path, NativeUpgradeQuiescence {
        admission_stopped: true, readers_and_workers_stopped: true, exclusive_operational_ownership: true,
    }).unwrap();
    context.bind_source(&owner.db).unwrap();
    owner.allocation = Some(context);
    let (owner, plan) = owner.plan_closed41().unwrap();
    let (owner, _) = owner.apply_closed41(&plan, None, &|_, _, _| Ok(())).unwrap();
    let session = owner.into_bootstrap().unwrap();
    let command = bootstrap_command(&session, session.prepare(HOST, &[]).unwrap());
    assert!(matches!(session.execute(&command, HOST, &[], policy()).unwrap(), CommandOutcome::Committed { .. }));
    let owner = session.into_completed_catalog(&command).unwrap();
    (root, owner, backing, candidate)
}
pub(super) fn source_command(prepared: &crate::catalog_source_succession::SourceRenewalPreparation) -> CommandEnvelope {
    CommandEnvelope::new(1,
        CommandKey::new(&prepared.catalog_id, HOST, "source-workspace-renewal").unwrap(),
        &prepared.incarnation, prepared.expected_generation,
        &serde_json::json!({"operation":"source_succession", "profile":prepared.profile,
            "source":prepared.source, "candidate_root":prepared.candidate_root,
            "content_commitment":prepared.content_commitment, "admission_digest":"a".repeat(64)}).to_string(),
    ).unwrap()
}
fn pressure(owner: &mut CompletedCatalogSession, backing: &mut allocation::OfflineControlWorkspaceBacking) -> u64 {
    let native = owner.retained_allocations().unwrap();
    let forwarded = allocation::LIMIT.checked_sub(native + 8 * 1024 * 1024).unwrap();
    assert!(OfflineAllocationReservation::new(forwarded).is_err(), "fixture must cross the ordinary constructor floor");
    owner.refresh_external_reservation_with_control_workspace(forwarded, backing).unwrap();
    assert!(owner.validate_current().is_err(), "ordinary metadata remains fully charged");
    forwarded
}

#[test]
fn source_workspace_prepare_execute_receipt_replay_and_outputs_survive_native_close() {
    let (_root, mut owner, mut backing, candidate) = fixture();
    let context = owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let forwarded = pressure(&mut owner, &mut backing);
    let cancel = AtomicBool::new(false);
    let before = context.temporary_bytes_for_test().unwrap();
    let preparation = owner.prepare_source_succession_with_control_workspace("default", "native-source", &candidate, &cancel, &mut backing).unwrap();
    let prepared = preparation.result().as_ref().unwrap();
    let command = source_command(prepared);
    let prep_pointer = prepared.content_commitment.as_ptr();
    let after_preparation = context.temporary_bytes_for_test().unwrap();
    assert!(after_preparation >= before + 2 * 64 * 1024);
    let absent = owner.source_succession_receipt_with_control_workspace(&command, authority(), &mut backing).unwrap();
    assert!(absent.result().as_ref().unwrap().is_none());
    let execution = owner.execute_source_succession_with_control_workspace(&command, authority(), policy(), &cancel, &mut backing).unwrap();
    assert!(matches!(execution.result(), Ok(CommandOutcome::Committed { .. })), "{:?}", execution);
    let receipt = owner.source_succession_receipt_with_control_workspace(&command, authority(), &mut backing).unwrap();
    assert_eq!(receipt.result().as_ref().unwrap().as_ref().unwrap().command(), &command);
    let replay = owner.execute_source_succession_with_control_workspace(&command, authority(), policy(), &cancel, &mut backing).unwrap();
    assert!(matches!(replay.result(), Ok(CommandOutcome::Replay { .. })), "{:?}", replay);
    assert_eq!(backing.state_for_test(), (forwarded, false, false));
    assert!(owner.execute_source_succession(&command, authority(), policy(), &cancel).is_err());
    assert!(owner.source_succession_receipt(&command, authority()).is_err());
    assert!(owner.prepare_source_succession("default", "native-source", &candidate, &cancel).is_err());
    let held = context.temporary_bytes_for_test().unwrap();
    assert!(held >= before + 5 * 2 * 64 * 1024);
    drop(owner);
    drop(backing);
    assert_eq!(preparation.result().as_ref().unwrap().content_commitment.as_ptr(), prep_pointer);
    assert!(matches!(execution.result(), Ok(CommandOutcome::Committed { .. })));
    assert_eq!(receipt.result().as_ref().unwrap().as_ref().unwrap().effect_description()["source_id"], "native-source");
    let mut remaining = context.temporary_bytes_for_test().unwrap();
    drop(absent);
    assert!(context.temporary_bytes_for_test().unwrap() < remaining);
    remaining = context.temporary_bytes_for_test().unwrap();
    drop(replay);
    assert!(context.temporary_bytes_for_test().unwrap() < remaining);
    drop(execution);
    drop(receipt);
    drop(preparation);
    assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
}

#[test]
fn source_workspace_foreign_missing_backing_growth_refusal_cancel_and_unwind_do_not_waive_checks() {
    let (_root, mut owner, mut backing, candidate) = fixture();
    let (_other_root, other, mut other_backing, _) = fixture();
    let before = owner.external_retained_reservation().unwrap();
    let mut absent = allocation::OfflineControlWorkspaceBacking::new();
    assert!(owner.refresh_external_reservation_with_control_workspace(before + 1, &mut absent).is_err());
    assert!(owner.refresh_external_reservation_with_control_workspace(before + 1, &mut other_backing).is_err());
    assert_eq!(owner.external_retained_reservation().unwrap(), before);
    let forwarded = pressure(&mut owner, &mut backing);
    owner.refresh_external_reservation_with_control_workspace(forwarded + 1, &mut backing).unwrap();
    assert!(owner.refresh_external_reservation_with_control_workspace(forwarded, &mut backing).is_err());
    assert!(owner.refresh_external_reservation_with_control_workspace(allocation::LIMIT, &mut backing).is_err());
    let cancel = AtomicBool::new(true);
    let result = owner.prepare_source_succession_with_control_workspace("default", "native-source", &candidate, &cancel, &mut backing).unwrap();
    assert!(result.result().is_err());
    assert_eq!(backing.state_for_test(), (forwarded + 1, false, false));
    assert!(owner.session.store.db.is_autocommit());
    cancel.store(false, Ordering::Release);
    let context = owner.session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
    let held = context.temporary_bytes_for_test().unwrap();
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let control = owner.session.control_workspace(&mut backing).unwrap();
        assert!(control.validate(&other.session.store).is_err());
        owner.validate_current_with_control_workspace(Some(&control)).unwrap();
        panic!("controlled completed-source workspace unwind");
    }));
    assert!(unwind.is_err());
    assert_eq!(backing.state_for_test(), (forwarded + 1, false, false));
    assert_eq!(context.temporary_bytes_for_test().unwrap(), held);
    let again = owner.prepare_source_succession_with_control_workspace("default", "native-source", &candidate, &cancel, &mut backing).unwrap();
    assert!(again.result().is_ok());
    drop(owner);
    drop(backing);
    assert!(result.result().is_err());
    assert!(again.result().is_ok());
    drop(result);
    drop(again);
    assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
}

#[test]
fn source_workspace_nested_transaction_and_structural_entry_keep_explicit_loan_without_overlap() {
    let (_root, mut owner, mut backing, _) = fixture();
    let forwarded = pressure(&mut owner, &mut backing);
    let control = owner.session.control_workspace(&mut backing).unwrap();
    let store = &owner.session.store;
    store.transaction_detailed_with_control_workspace::<_, Error>(|_| {
        for _ in 0..2 {
            owner.validate_current_with_control_workspace(Some(&control))?;
            store.transaction_available_with_control_workspace(Some(&control))?;
        }
        // The ordinary loader must fail at its full metadata reservation. The
        // explicit loader instead reaches the unchanged missing-version proof.
        // This probes its entry without inventing a typed context via raw SQL.
        let mut budget = crate::catalog_nfo_attribution::ProofBudget::source_succession(store.catalog_operation_budget()?);
        assert!(crate::catalog_identity_mutation::load_structural_context_in(store, "default", "missing-version", &mut budget).is_err());
        let missing = match crate::catalog_identity_mutation::load_structural_context_in_with_control_workspace(
            store, "default", "missing-version", &mut budget, Some(&control),
        ) {
            Ok(_) => panic!("a control loan must not invent a missing structural context"),
            Err(error) => error,
        };
        assert!(!missing.0.contains("allocation budget"), "{}", missing.0);
        owner.validate_current_with_control_workspace(Some(&control))?;
        Ok(())
    }, Some(&control)).unwrap();
    drop(control);
    assert!(store.db.is_autocommit());
    assert_eq!(backing.state_for_test(), (forwarded, false, false));
    assert!(store.transaction_available().is_err());
}
