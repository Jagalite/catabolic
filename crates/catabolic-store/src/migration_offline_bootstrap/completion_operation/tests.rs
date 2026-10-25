//! Real closed40 migration, bootstrap receipt and retained native custody.
//! These fixtures do not register41, activate Motion or invent a source grant.
use super::*;
use crate::{
    catalog_commands::CommandOutcome,
    migration::{self, snapshot},
};
use std::{path::PathBuf, sync::Arc, time::Duration};

const HOST: &str = "borrowed-completion-fixture";

fn quiescence() -> NativeUpgradeQuiescence {
    NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}
fn committed() -> (
    tempfile::TempDir,
    PathBuf,
    OfflineBootstrap<'static>,
    CommandEnvelope,
) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize_native_at(&path, 40).unwrap();
    let owner = PreparedOfflineBootstrap::prepare(&path, quiescence())
        .unwrap()
        .apply(None)
        .unwrap();
    let payload = owner.prepare(HOST, &[]).unwrap();
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(owner.catalog_id(), HOST, "committed-bootstrap").unwrap(),
        owner.incarnation(),
        owner.generation().unwrap(),
        &payload.to_string(),
    )
    .unwrap();
    let outcome = owner
        .execute(
            &command,
            HOST,
            &[],
            ReceiptPolicy {
                retention: Duration::from_secs(3600),
                max_receipts: 100,
                max_hold_records: 100,
                max_logical_bytes: 16 * 1024 * 1024,
            },
        )
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Committed { .. }));
    (root, path, owner, command)
}
fn wrong_command(command: &CommandEnvelope) -> CommandEnvelope {
    CommandEnvelope::new(
        command.contract_version(),
        CommandKey::new(
            command.key().catalog_id(),
            HOST,
            "not-the-committed-bootstrap",
        )
        .unwrap(),
        command.admission_incarnation(),
        command.expected_generation(),
        command.canonical_payload(),
    )
    .unwrap()
}
fn assert_custody(owner: &OfflineBootstrap<'_>, path: &Path, identity: (usize, usize, usize)) {
    assert_eq!(owner.session.completion_custody_for_test(), identity);
    let store = owner.session.completion_store_for_test();
    assert!(store.owns_writer_lock());
    assert!(crate::writer_lock(path).is_err());
    assert!(crate::native_admission_exclusive(path).is_err());
}
fn failure<'plan>(operation: OfflineCompletionOperation<'plan>) -> OfflineCompletionFailure<'plan> {
    match operation.finish() {
        Err(failure) => failure,
        Ok(_) => panic!("failed completion produced a completed owner"),
    }
}

#[test]
fn borrowed_completion_moves_original_custody_and_uses_exact_admitted_command() {
    let (_root, path, owner, command) = committed();
    let identity = owner.session.completion_custody_for_test();
    let mut owner = match owner.begin_completion() {
        Err(failure) => failure.into_bootstrap(),
        Ok(_) => panic!("unadmitted completion began"),
    };
    assert_custody(&owner, &path, identity);
    owner
        .admit_completion_operation(&command, Instant::now() + Duration::from_secs(60))
        .unwrap();
    let before = snapshot(&owner.session.completion_store_for_test().db).unwrap();
    // An installed callback that would panic proves handoffs do not poll it.
    let mut operation = crate::embedding::with_interrupt_check(
        || panic!("begin must not poll"),
        || Ok(owner.begin_completion().unwrap()),
    )
    .unwrap();
    assert_custody(&operation.owner, &path, identity);
    operation.complete(&AtomicBool::new(false)).unwrap();
    assert_eq!(
        snapshot(&operation.owner.session.completion_store_for_test().db).unwrap(),
        before
    );
    let completed = crate::embedding::with_interrupt_check(
        || panic!("finish must not poll"),
        || Ok(operation.finish().unwrap()),
    )
    .unwrap();
    assert_eq!(completed.completion_custody_for_test(), identity);
    assert!(completed.retained_allocations().unwrap() > 0);
    assert!(crate::writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
    drop(completed);
    drop(crate::writer_lock(&path).unwrap());
    drop(crate::native_admission_exclusive(&path).unwrap());
    assert!(crate::Store::open(&path, true, false).is_err());
}

#[test]
fn borrowed_completion_storage_refusal_leaves_original_unmoved() {
    let (_root, path, mut owner, command) = committed();
    let identity = owner.session.completion_custody_for_test();
    let full = owner.session.fill_completion_headroom_for_test().unwrap();
    assert!(
        owner
            .admit_completion_operation(&command, Instant::now() + Duration::from_secs(60))
            .is_err()
    );
    assert!(owner.completion_admission.is_none());
    assert_custody(&owner, &path, identity);
    drop(full);
    owner
        .admit_completion_operation(&command, Instant::now() + Duration::from_secs(60))
        .unwrap();
    let failure = failure(owner.begin_completion().unwrap());
    assert_custody(&failure._owner, &path, identity);
}

#[test]
fn borrowed_completion_readmission_only_tightens_and_never_changes_command() {
    let (_root, path, mut owner, command) = committed();
    let identity = owner.session.completion_custody_for_test();
    let initial = Instant::now() + Duration::from_secs(60);
    owner.admit_completion_operation(&command, initial).unwrap();
    owner
        .admit_completion_operation(&command, initial + Duration::from_secs(60))
        .unwrap();
    assert_eq!(
        owner.completion_admission.as_ref().unwrap().deadline,
        initial
    );
    assert!(
        owner
            .admit_completion_operation(&wrong_command(&command), initial)
            .is_err()
    );
    let expired = Instant::now();
    owner.admit_completion_operation(&command, expired).unwrap();
    owner.admit_completion_operation(&command, initial).unwrap();
    assert_eq!(
        owner.completion_admission.as_ref().unwrap().deadline,
        expired
    );
    assert!(
        owner.complete(&command).is_err(),
        "legacy borrowed Complete bypassed admission"
    );
    let mut operation = owner.begin_completion().unwrap();
    assert!(operation.complete(&AtomicBool::new(false)).is_err());
    assert!(matches!(operation.state, CompletionState::Failed));
    assert!(operation.complete(&AtomicBool::new(false)).is_err());
    let failure = failure(operation);
    assert_custody(&failure._owner, &path, identity);
    assert_eq!(failure.admission.deadline, expired);
}

#[test]
fn borrowed_completion_legacy_consumers_cannot_bypass_admitted_deadline() {
    for image in [false, true] {
        let (_root, _path, mut owner, command) = committed();
        owner
            .admit_completion_operation(&command, Instant::now())
            .unwrap();
        let error = if image {
            match owner.into_completed_image(&command, &AtomicBool::new(false)) {
                Err(error) => error,
                Ok(_) => panic!("legacy image bypassed completion admission"),
            }
        } else {
            match owner.into_completed_catalog(&command) {
                Err(error) => error,
                Ok(_) => panic!("legacy conversion bypassed completion admission"),
            }
        };
        assert!(
            error
                .to_string()
                .contains("use admitted offline completion")
        );
    }
}

#[test]
fn borrowed_completion_errors_retain_opaque_terminal_custody() {
    for fault in [0, 1, 3] {
        let (_root, path, mut owner, command) = committed();
        let identity = owner.session.completion_custody_for_test();
        let before = snapshot(&owner.session.completion_store_for_test().db).unwrap();
        let admitted = if fault == 1 {
            wrong_command(&command)
        } else {
            command
        };
        owner
            .admit_completion_operation(&admitted, Instant::now() + Duration::from_secs(60))
            .unwrap();
        let mut operation = owner.begin_completion().unwrap();
        let cancelled = AtomicBool::new(fault == 0);
        assert!(
            operation
                .complete_inner(&cancelled, if fault == 3 { 3 } else { 0 })
                .is_err()
        );
        assert!(matches!(operation.state, CompletionState::Failed));
        assert!(operation.measured_facts().is_none());
        if fault == 3 {
            assert!(!operation.admission.storage.state_digest().is_empty(),
                "late cancellation must refuse already measured facts");
        }
        assert!(operation.refresh_external_reservation(OfflineAllocationReservation::default()).is_err());
        assert!(operation.complete(&AtomicBool::new(false)).is_err());
        let failure = failure(operation);
        assert_custody(&failure._owner, &path, identity);
        assert_eq!(
            snapshot(&failure._owner.session.completion_store_for_test().db).unwrap(),
            before
        );
        assert!(
            failure
                ._owner
                .session
                .completion_store_for_test()
                .db
                .is_autocommit()
        );
    }
}

#[test]
fn borrowed_completion_callback_and_phase_panics_retain_original_owner() {
    for fault in [0, 1, 2] {
        let (_root, path, mut owner, command) = committed();
        let identity = owner.session.completion_custody_for_test();
        let before = snapshot(&owner.session.completion_store_for_test().db).unwrap();
        owner
            .admit_completion_operation(&command, Instant::now() + Duration::from_secs(60))
            .unwrap();
        let mut operation = owner.begin_completion().unwrap();
        let seen = Arc::new(AtomicBool::new(false));
        let callback_seen = seen.clone();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if fault == 0 {
                crate::embedding::with_interrupt_check(
                    move || {
                        callback_seen.store(true, Ordering::SeqCst);
                        panic!("actual embedding callback completion unwind");
                    },
                    || operation.complete(&AtomicBool::new(false)),
                )
            } else {
                operation.complete_inner(&AtomicBool::new(false), fault)
            }
        }));
        assert!(unwind.is_err());
        assert!(fault != 0 || seen.load(Ordering::SeqCst));
        assert!(matches!(operation.state, CompletionState::Failed));
        assert!(operation.measured_facts().is_none());
        if fault == 2 {
            assert!(!operation.admission.storage.state_digest().is_empty(),
                "post-measure panic must not publish completed buffers");
        }
        assert!(operation.refresh_external_reservation(OfflineAllocationReservation::default()).is_err());
        assert!(operation.complete(&AtomicBool::new(false)).is_err());
        let failure = failure(operation);
        assert_custody(&failure._owner, &path, identity);
        assert_eq!(
            snapshot(&failure._owner.session.completion_store_for_test().db).unwrap(),
            before
        );
        assert!(
            failure
                ._owner
                .session
                .completion_store_for_test()
                .db
                .is_autocommit()
        );
    }
}

#[test]
fn borrowed_completion_retains_original_poison_without_healing() {
    let (_root, path, mut owner, command) = committed();
    let identity = owner.session.completion_custody_for_test();
    owner
        .admit_completion_operation(&command, Instant::now() + Duration::from_secs(60))
        .unwrap();
    let mut operation = owner.begin_completion().unwrap();
    let store = operation.owner.session.completion_store_for_test();
    store.db.execute_batch("BEGIN").unwrap();
    assert!(
        store
            .rollback_transaction(Some("nonexistent_completion_scope"))
            .is_err()
    );
    assert!(store.transaction_available().is_err());
    assert!(operation.complete(&AtomicBool::new(false)).is_err());
    let failure = failure(operation);
    assert_custody(&failure._owner, &path, identity);
    assert!(
        failure
            ._owner
            .session
            .completion_store_for_test()
            .transaction_available()
            .is_err()
    );
}

#[test]
fn borrowed_completion_preserves_ambient_budget_selection_without_replacement() {
    let (_root, _path, mut owner, command) = committed();
    owner
        .admit_completion_operation(&command, Instant::now() + Duration::from_secs(60))
        .unwrap();
    let mut operation = owner.begin_completion().unwrap();
    let budget = crate::catalog_search_index::default_operation_budget().unwrap();
    let _scope = budget.enter().unwrap();
    operation.complete(&AtomicBool::new(false)).unwrap();
    assert!(
        operation
            .owner
            .session
            .completion_store_for_test()
            .catalog_operation_budget()
            .unwrap()
            .same_owner(&budget)
    );
    let used = budget.usage();
    assert!(operation.complete(&AtomicBool::new(false)).is_err());
    assert_eq!(budget.usage(), used);
    assert!(operation.finish().is_ok());
}

#[test]
fn borrowed_completion_failure_keeps_original_borrowed_plan() {
    let (_root, path, owner, command) = committed();
    let plan = owner.approved_plan().clone();
    drop(owner);
    let mut recovered = OfflineBootstrap::recover(
        &path,
        quiescence(),
        &plan,
        Recovery::CompletedReceiptOnly { command: &command },
    )
    .unwrap();
    let identity = recovered.session.completion_custody_for_test();
    recovered
        .admit_completion_operation(&command, Instant::now())
        .unwrap();
    let mut operation = recovered.begin_completion().unwrap();
    assert!(operation.complete(&AtomicBool::new(false)).is_err());
    let failure = failure(operation);
    assert!(
        matches!(&failure._owner.plan, Cow::Borrowed(original) if std::ptr::eq(*original, &plan))
    );
    assert_custody(&failure._owner, &path, identity);
}

#[test]
fn borrowed_completion_measured_facts_match_real_complete_without_rescan() {
    let (_root, path, mut owner, command) = committed();
    let identity = owner.session.completion_custody_for_test();
    // This ordinary legacy fixture has not admitted a retained operation yet.
    // It is a legitimate independent Complete oracle, not a marked bypass.
    let (catalog, incarnation, inventory, transition, state, generation, count) = {
        let complete = owner.complete(&command).unwrap();
        assert_eq!(complete.command_key(), command.key());
        assert_eq!(complete.payload_digest(), command.digest().as_str());
        (
            complete.catalog_id().to_owned(),
            complete.incarnation().to_owned(),
            complete.inventory_digest().to_owned(),
            complete.transition_digest().to_owned(),
            complete.state_digest().to_owned(),
            complete.committed_generation(),
            complete.source_count(),
        )
    };
    owner
        .admit_completion_operation(&command, Instant::now() + Duration::from_secs(60))
        .unwrap();
    assert!(owner.complete(&command).is_err());
    let mut operation = owner.begin_completion().unwrap();
    assert!(operation.measured_facts().is_none());
    operation.complete(&AtomicBool::new(false)).unwrap();
    let budget = operation
        .owner
        .session
        .completion_store_for_test()
        .catalog_operation_budget()
        .unwrap();
    let used = budget.usage();
    let queries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = queries.clone();
    operation
        .owner
        .session
        .completion_store_for_test()
        .db
        .authorizer(Some(move |_: rusqlite::hooks::AuthContext<'_>| {
            observed.fetch_add(1, Ordering::SeqCst);
            rusqlite::hooks::Authorization::Deny
        }))
        .unwrap();
    crate::embedding::with_interrupt_check(
        || panic!("measured facts must not poll"),
        || {
            let facts = operation
                .measured_facts()
                .expect("successful Complete facts");
            assert_eq!(facts.command(), &command);
            assert_eq!(facts.command_key(), command.key());
            assert_eq!(facts.catalog_id(), catalog);
            assert_eq!(facts.incarnation(), incarnation);
            assert_eq!(facts.payload_digest(), command.digest().as_str());
            assert_eq!(facts.inventory_digest(), inventory);
            assert_eq!(facts.transition_digest(), transition);
            assert_eq!(facts.state_digest(), state);
            assert_eq!(facts.committed_generation(), generation);
            assert_eq!(facts.source_count(), count);
            assert!(std::ptr::eq(
                facts.command(),
                operation.admission.storage.command()
            ));
            assert_eq!(
                facts.state_digest().as_ptr(),
                operation.admission.storage.state_digest().as_ptr()
            );
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(queries.load(Ordering::SeqCst), 0);
    assert_eq!(budget.usage(), used);
    operation
        .owner
        .session
        .completion_store_for_test()
        .db
        .authorizer(Some(|_: rusqlite::hooks::AuthContext<'_>| {
            rusqlite::hooks::Authorization::Allow
        }))
        .unwrap();
    assert_custody(&operation.owner, &path, identity);
    let completed = crate::embedding::with_interrupt_check(
        || panic!("measured finish must not poll"),
        || Ok(operation.finish().unwrap()),
    )
    .unwrap();
    assert_eq!(completed.completion_custody_for_test(), identity);
    assert!(crate::writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
}

#[test]
fn borrowed_completion_expiry_after_measurement_keeps_facts_unavailable() {
    let (_root, path, mut owner, command) = committed();
    let identity = owner.session.completion_custody_for_test();
    let deadline = Instant::now() + Duration::from_secs(1);
    owner
        .admit_completion_operation(&command, deadline)
        .unwrap();
    let mut operation = owner.begin_completion().unwrap();
    let seen = Arc::new(AtomicBool::new(false));
    let observed = seen.clone();
    let result = crate::embedding::with_interrupt_check(
        move || {
            if !observed.swap(true, Ordering::SeqCst) {
                // Expire the actual original host deadline during Complete's
                // existing callback, without changing any clock or budget.
                std::thread::sleep(
                    deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(1),
                );
            }
            false
        },
        || operation.complete(&AtomicBool::new(false)),
    );
    assert!(seen.load(Ordering::SeqCst));
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("deadline exhausted")
    );
    assert_eq!(operation.admission.deadline, deadline);
    assert!(!operation.admission.storage.state_digest().is_empty());
    assert!(matches!(operation.state, CompletionState::Failed));
    assert!(operation.measured_facts().is_none());
    assert!(operation.complete(&AtomicBool::new(false)).is_err());
    let failure = failure(operation);
    assert_custody(&failure._owner, &path, identity);
}

#[test]
fn borrowed_completion_refresh_preserves_both_phases_context_and_budget() {
    let (_root, path, mut owner, command) = committed();
    let identity = owner.session.completion_custody_for_test();
    owner
        .refresh_external_reservation(OfflineAllocationReservation::new(1024).unwrap())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    owner
        .admit_completion_operation(&command, deadline)
        .unwrap();
    let mut operation = owner.begin_completion().unwrap();
    for (measured, bytes) in [(false, 2048), (true, 4096)] {
        if measured {
            operation.complete(&AtomicBool::new(false)).unwrap();
        }
        let budget = operation
            .owner
            .session
            .completion_store_for_test()
            .catalog_operation_budget()
            .unwrap();
        let used = budget.usage();
        let queries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = queries.clone();
        operation
            .owner
            .session
            .completion_store_for_test()
            .db
            .authorizer(Some(move |_: rusqlite::hooks::AuthContext<'_>| {
                observed.fetch_add(1, Ordering::SeqCst);
                rusqlite::hooks::Authorization::Deny
            }))
            .unwrap();
        crate::embedding::with_interrupt_check(
            || panic!("completion reservation refresh must not poll"),
            || {
                operation
                    .refresh_external_reservation(OfflineAllocationReservation::new(bytes).unwrap())
            },
        )
        .unwrap();
        assert_eq!(queries.load(Ordering::SeqCst), 0);
        assert_eq!(operation.admission.deadline, deadline);
        assert_eq!(operation.measured_facts().is_some(), measured);
        assert!(
            operation
                .owner
                .session
                .completion_store_for_test()
                .catalog_operation_budget()
                .unwrap()
                .same_owner(&budget)
        );
        assert_eq!(budget.usage(), used);
        operation
            .owner
            .session
            .completion_store_for_test()
            .db
            .authorizer(Some(|_: rusqlite::hooks::AuthContext<'_>| {
                rusqlite::hooks::Authorization::Allow
            }))
            .unwrap();
        assert_custody(&operation.owner, &path, identity);
    }
    let completed = operation.finish().unwrap();
    assert_eq!(completed.completion_custody_for_test(), identity);
    assert_eq!(completed.external_retained_reservation().unwrap(), 4096);
}

#[test]
fn borrowed_completion_refresh_refusal_or_expiry_is_terminal_and_retained() {
    for fault in 0..5 {
        let (_root, path, mut owner, command) = committed();
        let identity = owner.session.completion_custody_for_test();
        owner
            .refresh_external_reservation(OfflineAllocationReservation::new(1024).unwrap())
            .unwrap();
        let deadline = if fault == 0 {
            Instant::now()
        } else if fault == 4 {
            Instant::now() + Duration::from_secs(10)
        } else {
            Instant::now() + Duration::from_secs(60)
        };
        owner
            .admit_completion_operation(&command, deadline)
            .unwrap();
        let mut operation = owner.begin_completion().unwrap();
        if fault >= 2 {
            operation.complete(&AtomicBool::new(false)).unwrap();
            assert!(operation.measured_facts().is_some());
        }
        if fault == 4 {
            // A completed measurement is not renewed ACK authority. Expire
            // its actual inherited deadline, not a replacement test budget.
            std::thread::sleep(
                deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(1),
            );
        }
        let full = (fault == 3).then(|| {
            operation
                .owner
                .session
                .fill_completion_headroom_for_test()
                .unwrap()
        });
        let bytes = match fault {
            0 | 4 => 1024,
            3 => 1025,
            _ => 0,
        };
        let budget = operation
            .owner
            .session
            .completion_store_for_test()
            .catalog_operation_budget()
            .unwrap();
        let used = budget.usage();
        assert!(
            operation
                .refresh_external_reservation(OfflineAllocationReservation::new(bytes).unwrap())
                .is_err()
        );
        assert!(
            operation
                .owner
                .session
                .completion_store_for_test()
                .catalog_operation_budget()
                .unwrap()
                .same_owner(&budget)
        );
        assert_eq!(budget.usage(), used);
        drop(full);
        assert_eq!(operation.admission.deadline, deadline);
        assert!(matches!(operation.state, CompletionState::Failed));
        assert!(operation.measured_facts().is_none());
        // Relieving real allocation pressure or increasing a reservation cannot
        // revive an already refused attempt or publish its buffered facts.
        assert!(
            operation
                .refresh_external_reservation(OfflineAllocationReservation::new(2048).unwrap())
                .is_err()
        );
        assert!(operation.complete(&AtomicBool::new(false)).is_err());
        let failure = failure(operation);
        assert_custody(&failure._owner, &path, identity);
    }
}
