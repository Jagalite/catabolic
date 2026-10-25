use super::super::super::super::tests::committed;
use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

fn fixture() -> (
    tempfile::TempDir,
    CurrentRotationPlanningOperation,
    CurrentRestoreHostAttestation,
) {
    let (root, session, command) = committed(Duration::from_secs(3600));
    let owner = session.into_completed_catalog(&command).unwrap();
    let mut image = owner.into_current_image(&AtomicBool::new(false)).unwrap();
    let host = CurrentRestoreHostAttestation {
        pair_sha256: "a".repeat(64),
        motion_binding_sha256: "b".repeat(64),
        catalog_id: image.catalog_id().into(),
        incarnation: image.incarnation().into(),
        generation: image.generation(),
        current_inventory_sha256: image.current_inventory_sha256().into(),
        transition_digest: image.transition_digest().into(),
    };
    image
        .admit_rotation_operation(Instant::now() + Duration::from_secs(60))
        .unwrap();
    let mut operation = image.begin_rotation().unwrap();
    operation.admit_planning().unwrap();
    // Once planning is admitted, the old consuming effect cannot bypass it.
    assert!(operation.rotate(&host, &AtomicBool::new(false)).is_err());
    let operation = match operation.into_planning() {
        Ok(v) => v,
        Err(_) => panic!("planning not admitted"),
    };
    (root, operation, host)
}
fn settled(mut operation: CurrentRotationPlanningOperation) -> CompletedCatalogSession {
    operation.settle().unwrap();
    match operation.finish() {
        Ok(owner) => owner,
        Err(_) => panic!("settled owner not released"),
    }
}
#[test]
fn planned_rotation_prepares_real_stable_rows_without_mutating_either_database() {
    let (_root, mut operation, host) = fixture();
    let before = snapshot(&operation.operation.owner.session.store.db).unwrap();
    let deadline = operation.operation.deadline;
    let budget = operation.admission.budget.clone();
    operation.plan(&host, &AtomicBool::new(false)).unwrap();
    let facts = operation.facts().unwrap();
    let id = facts.restore_id.to_owned();
    let to = facts.to_incarnation.to_owned();
    assert_ne!(to, host.incarnation);
    assert_eq!(facts.generation, host.generation);
    assert_eq!(facts.transition_digest, host.transition_digest);
    let origin: Value = serde_json::from_str(facts.origin_json).unwrap();
    assert_eq!(origin["restore_id"], id);
    assert_eq!(origin["from_incarnation"], host.incarnation);
    let mut moved = Box::new(operation);
    assert_eq!(moved.operation.deadline, deadline);
    assert!(moved.admission.budget.same_owner(&budget));
    assert_eq!(moved.facts().unwrap().restore_id, id);
    assert_eq!(moved.facts().unwrap().to_incarnation, to);
    assert!(moved.plan(&host, &AtomicBool::new(false)).is_err());
    let operation = *moved;
    let operation = match operation.finish() {
        Err(v) => v,
        Ok(_) => panic!("unsettled owner escaped"),
    };
    assert_eq!(
        snapshot(&operation.operation.owner.session.store.db).unwrap(),
        before
    );
    let RotationState::Ready(image) = &operation.operation.state else {
        panic!("copy missing")
    };
    let (incarnation, rows): (String, i64) = image.planning_db().query_row(
        "SELECT incarnation,(SELECT count(*) FROM catalog_restore_lineages) FROM catalog_state WHERE id=1", [],
        |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(incarnation, host.incarnation);
    assert_eq!(rows, 0);
    let owner = settled(operation);
    assert!(owner.session.store.db.is_autocommit());
    assert!(owner.session.store.owns_writer_lock());
    assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
    owner.validate_current().unwrap();
}
#[test]
fn planned_rotation_first_entry_poll_unwind_keeps_whole_operation_until_settlement() {
    let (_root, mut operation, host) = fixture();
    let raw = unsafe { operation.operation.owner.session.store.db.handle() } as usize;
    let reached = std::sync::Arc::new(AtomicBool::new(false));
    let seen = reached.clone();
    let result = catch_unwind(AssertUnwindSafe(|| {
        crate::embedding::with_interrupt_check(
            move || {
                if unsafe { rusqlite::ffi::sqlite3_get_autocommit(raw as *mut _) } == 0
                    && !seen.swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    panic!("controlled planned entry unwind");
                }
                false
            },
            || operation.plan(&host, &AtomicBool::new(false)),
        )
    }));
    assert!(result.is_err());
    assert!(reached.load(std::sync::atomic::Ordering::SeqCst));
    assert!(operation.facts().is_none());
    assert!(operation.plan(&host, &AtomicBool::new(false)).is_err());
    let operation = match operation.finish() {
        Err(v) => v,
        Ok(_) => panic!("unsettled unwind escaped"),
    };
    let owner = settled(operation);
    assert!(owner.session.store.db.is_autocommit());
    assert!(owner.session.store.owns_writer_lock());
}
#[test]
fn planned_rotation_denies_swallowed_boundary_ddl_and_write_attempts() {
    for sql in [
        "COMMIT; BEGIN IMMEDIATE",
        "SAVEPOINT hostile",
        "CREATE TEMP TABLE hostile(x)",
        "UPDATE catalog_state SET generation=generation",
        "ATTACH ':memory:' AS hostile",
    ] {
        let (_root, mut operation, host) = fixture();
        operation.plan(&host, &AtomicBool::new(false)).unwrap();
        assert!(
            operation
                .operation
                .owner
                .session
                .store
                .db
                .execute_batch(sql)
                .is_err()
        );
        assert!(
            operation
                .admission
                .source
                .check(&operation.operation.owner.session.store.db)
                .is_err()
        );
        assert!(!operation.operation.owner.session.store.db.is_autocommit());
        assert!(operation.facts().is_none());
        let RotationState::Ready(image) = &operation.operation.state else {
            panic!("missing copy")
        };
        assert!(image.planning_db().execute_batch(sql).is_err());
        assert!(operation.admission.copy.check(image.planning_db()).is_err());
        let owner = settled(operation);
        assert!(owner.session.store.db.is_autocommit());
        assert!(owner.session.store.owns_writer_lock());
    }
}
#[test]
fn planned_rotation_expiry_cancel_foreign_budget_cannot_retry_or_reset_owner() {
    for mode in 0..3 {
        let (_root, mut operation, host) = fixture();
        let deadline = operation.operation.deadline;
        let budget = operation.admission.budget.clone();
        if mode == 0 {
            operation.operation.deadline = Instant::now() - Duration::from_millis(1);
        }
        let result = if mode == 2 {
            let other = crate::catalog_search_index::default_operation_budget().unwrap();
            let _scope = other.enter().unwrap();
            operation.plan(&host, &AtomicBool::new(false))
        } else {
            operation.plan(&host, &AtomicBool::new(mode == 1))
        };
        assert!(result.is_err());
        assert!(operation.admission.budget.same_owner(&budget));
        assert!(operation.operation.deadline <= deadline);
        assert!(operation.facts().is_none());
        assert!(operation.plan(&host, &AtomicBool::new(false)).is_err());
        let owner = settled(operation);
        assert!(owner.session.store.owns_writer_lock());
    }
}

#[test]
fn planned_rotation_synthetic_cleanup_boundaries_retain_whole_failure_owner() {
    // Synthetic fixed boundary failures, NOT simulated SQLite engine failures.
    for fault in [1, 2] {
        let (_root, mut operation, host) = fixture();
        operation.plan(&host, &AtomicBool::new(false)).unwrap();
        assert!(operation.settle_inner(fault).is_err());
        assert!(operation.facts().is_none());
        assert!(operation.plan(&host, &AtomicBool::new(false)).is_err());
        let mut operation = match operation.finish() {
            Err(owner) => owner,
            Ok(_) => panic!("cleanup failure lost owner"),
        };
        assert!(operation.operation.owner.session.store.owns_writer_lock());
        assert!(operation.admission.source.installed());
        if fault == 1 {
            let owner = settled(operation);
            assert!(owner.session.store.db.is_autocommit());
        } else {
            assert!(
                operation
                    .operation
                    .owner
                    .session
                    .store
                    .transaction_poisoned
                    .get()
            );
            assert!(operation.settle().is_err());
            assert!(operation.finish().is_err());
        }
    }
}

#[test]
fn planned_rotation_post_construction_unwind_retains_plan_and_original_custody() {
    let (_root, mut operation, host) = fixture();
    let result = catch_unwind(AssertUnwindSafe(|| {
        operation.plan_inner(&host, &AtomicBool::new(false), 1)
    }));
    assert!(result.is_err());
    assert!(operation.plan.is_some());
    assert!(operation.facts().is_none());
    assert!(operation.plan(&host, &AtomicBool::new(false)).is_err());
    let operation = match operation.finish() {
        Err(owner) => owner,
        Ok(_) => panic!("unsettled plan escaped"),
    };
    assert!(operation.operation.owner.session.store.owns_writer_lock());
    let owner = settled(operation);
    assert!(owner.session.store.db.is_autocommit());
}

#[test]
fn planned_rotation_final_poll_swallowed_parent_failure_refuses_without_losing_owner() {
    let (_root, mut operation, host) = fixture();
    let phase = std::sync::Arc::new(AtomicBool::new(false));
    operation.final_poll = Some(phase.clone());
    let parent = operation.admission.budget.clone();
    let reached = std::sync::Arc::new(AtomicBool::new(false));
    let seen = reached.clone();
    let result = crate::embedding::with_interrupt_check(
        move || {
            if phase.load(std::sync::atomic::Ordering::Acquire)
                && !seen.swap(true, std::sync::atomic::Ordering::AcqRel)
            {
                assert!(
                    parent
                        .charge(BudgetCharge {
                            work: usize::MAX,
                            ..Default::default()
                        })
                        .is_err()
                );
                assert!(parent.failure().is_some());
            }
            false
        },
        || operation.plan(&host, &AtomicBool::new(false)),
    );
    assert!(reached.load(std::sync::atomic::Ordering::Acquire));
    assert!(result.is_err());
    assert!(operation.facts().is_none());
    assert!(operation.admission.budget.failure().is_some());
    let operation = match operation.finish() {
        Err(owner) => owner,
        Ok(_) => panic!("failed final poll escaped"),
    };
    assert!(!operation.operation.owner.session.store.db.is_autocommit());
    let owner = settled(operation);
    assert!(owner.session.store.db.is_autocommit());
    assert!(owner.session.store.owns_writer_lock());
}
