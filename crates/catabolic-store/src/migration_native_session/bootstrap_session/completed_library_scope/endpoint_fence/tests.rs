use super::super::super::tests::committed;
use super::*;
use std::time::Duration;
fn owner() -> (tempfile::TempDir, CompletedCatalogSession) {
    let (root, session, command) = committed(Duration::from_secs(3600));
    (root, session.into_completed_catalog(&command).unwrap())
}
#[test]
fn genuine_endpoint_fence_denies_main_temp_schema_and_transaction_changes() {
    let (_root, owner) = owner();
    let store = &owner.session.store;
    store
        .db
        .execute_batch(
            "CREATE TEMP TABLE endpoint_guard_temp(v); INSERT INTO endpoint_guard_temp VALUES(1)",
        )
        .unwrap();
    for sql in [
        "UPDATE main.meta SET value=value WHERE key='database_id'",
        "UPDATE main.catalog_source_scope_heads SET revision=revision WHERE 0",
        "UPDATE temp.endpoint_guard_temp SET v=2",
        "CREATE TABLE endpoint_guard_added(v)",
        "CREATE TEMP VIEW endpoint_guard_view AS SELECT 1",
        "PRAGMA main.schema_version=7",
        "PRAGMA temp.schema_version=7",
        "PRAGMA writable_schema=ON",
        "COMMIT",
        "ROLLBACK",
        "SAVEPOINT bad",
        "ATTACH ':memory:' AS bad",
        "DETACH temp",
    ] {
        let result: Result<()> = store.transaction_detailed(|_| {
            let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
            let fence = EndpointReadFence::new(&owner, &mut budget)?;
            assert!(
                store.db.execute_batch(sql).is_err(),
                "unexpectedly admitted {sql}"
            );
            assert!(fence.check(&owner, &mut budget).is_err());
            drop(fence);
            Err(Error("expected endpoint attempt refusal".into()))
        });
        assert_eq!(
            result.unwrap_err().to_string(),
            "expected endpoint attempt refusal"
        );
        assert!(store.db.is_autocommit());
        owner.validate_current().unwrap();
    }
    store
        .transaction_detailed::<_, Error>(|_| {
            // Hook replacement must expire a statement prepared before the fence.
            let mut prepared = store
                .db
                .prepare("UPDATE main.meta SET value=value WHERE key='database_id'")?;
            let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
            let fence = EndpointReadFence::new(&owner, &mut budget)?;
            assert!(prepared.execute([]).is_err());
            assert!(fence.check(&owner, &mut budget).is_err());
            drop(prepared);
            drop(fence);
            Ok(())
        })
        .unwrap();
}
#[test]
fn genuine_endpoint_fence_poison_survives_swallowed_embedding_reentry() {
    let (_root, owner) = owner();
    let store = &owner.session.store;
    let armed = Arc::new(AtomicBool::new(false));
    let ran = Arc::new(AtomicBool::new(false));
    let result_code = Arc::new(std::sync::atomic::AtomicI32::new(0));
    let (a, r, c) = (armed.clone(), ran.clone(), result_code.clone());
    let raw = handle(store);
    let result: Result<()> = crate::embedding::with_interrupt_check(
        move || {
            if a.swap(false, Ordering::AcqRel) {
                // Controlled test-only reentry into the actual retained connection.
                // The Connection outlives this lexical embedding callback; polling
                // is deliberately triggered outside a running SQLite statement.
                let code = unsafe {
                    rusqlite::ffi::sqlite3_exec(
                        raw as *mut _,
                        c"UPDATE main.catalog_source_scope_heads SET revision=revision WHERE 0"
                            .as_ptr(),
                        None,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
                c.store(code, Ordering::Release);
                r.store(true, Ordering::Release);
            }
            false // Host deliberately swallows the denied SQLite operation.
        },
        || {
            store.transaction_detailed::<_, Error>(|_| {
                let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
                let fence = EndpointReadFence::new(&owner, &mut budget)?;
                armed.store(true, Ordering::Release);
                assert!(fence.check(&owner, &mut budget).is_err());
                assert!(ran.load(Ordering::Acquire));
                assert_eq!(
                    result_code.load(Ordering::Acquire),
                    rusqlite::ffi::SQLITE_AUTH
                );
                drop(fence);
                Err(Error("expected swallowed reentry refusal".into()))
            })
        },
    );
    assert_eq!(
        result.unwrap_err().to_string(),
        "expected swallowed reentry refusal"
    );
    assert!(store.db.is_autocommit());
    owner.validate_current().unwrap();
}
#[test]
fn genuine_endpoint_fence_binds_owner_budget_and_restores_after_unwind() {
    let (_root, owner) = owner();
    let (_other_root, other) = self::owner();
    let store = &owner.session.store;
    store
        .transaction_detailed::<_, Error>(|_| {
            let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
            let fence = EndpointReadFence::new(&owner, &mut budget)?;
            assert!(fence.check(&other, &mut budget).is_err());
            let mut foreign =
                ProofBudget::new(crate::catalog_search_index::default_operation_budget()?);
            assert!(fence.check(&owner, &mut foreign).is_err());
            fence.finish(&owner, &mut budget)
        })
        .unwrap();
    let result: Result<()> = store.transaction_detailed(|_| {
        let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
        let fence = EndpointReadFence::new(&owner, &mut budget)?;
        // Generic NFO local-limit refusal deliberately does not poison its
        // inherited operation. Exercise the actual sticky parent here; the
        // production endpoint runner propagates every local error with `?`.
        let parent = store.catalog_operation_budget()?;
        assert!(
            parent
                .charge(BudgetCharge {
                    work: usize::MAX,
                    ..Default::default()
                })
                .is_err()
        );
        assert!(parent.failure().is_some());
        assert!(fence.finish(&owner, &mut budget).is_err());
        Err(Error("expected sticky budget refusal".into()))
    });
    assert_eq!(
        result.unwrap_err().to_string(),
        "expected sticky budget refusal"
    );
    assert!(store.db.is_autocommit());
    let entered = std::cell::Cell::new(false);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<()> = store.transaction_detailed(|_| {
                let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
                let _fence = EndpointReadFence::new(&owner, &mut budget)?;
                entered.set(true);
                panic!("controlled endpoint fence unwind");
            });
        }))
        .is_err()
    );
    assert!(entered.get());
    assert!(store.db.is_autocommit());
    owner.validate_current().unwrap();
    store
        .transaction_detailed::<_, Error>(|_| {
            let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
            EndpointReadFence::new(&owner, &mut budget)?.finish(&owner, &mut budget)
        })
        .unwrap();
}

#[test]
fn fixed_endpoint_observes_first_post_begin_poll_and_swallowed_reentry() {
    let (_root, owner) = owner();
    let store = &owner.session.store;
    for sql in [
        "COMMIT",
        "ROLLBACK",
        "SAVEPOINT stolen",
        "UPDATE main.catalog_source_scope_heads SET revision=revision WHERE 0",
    ] {
        let attempted = Arc::new(AtomicBool::new(false));
        let code = Arc::new(std::sync::atomic::AtomicI32::new(-1));
        let flag = attempted.clone();
        let observed = code.clone();
        let pointer = unsafe { store.db.handle() as usize };
        let sql = std::ffi::CString::new(sql).unwrap();
        let result = crate::embedding::with_interrupt_check(
            Box::new(move || {
                let db = pointer as *mut rusqlite::ffi::sqlite3;
                // Before BEGIN the callback is harmless. The first poll after
                // actual acquisition attempts a boundary/write and swallows it.
                if unsafe { rusqlite::ffi::sqlite3_get_autocommit(db) } == 0
                    && !flag.swap(true, Ordering::AcqRel)
                {
                    let result = unsafe {
                        rusqlite::ffi::sqlite3_exec(
                            db,
                            sql.as_ptr(),
                            None,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        )
                    };
                    observed.store(result, Ordering::Release);
                }
                false
            }),
            || {
                run(
                    &owner,
                    EndpointOperation::Probe {
                        allocation_failure: false,
                        panic: false,
                    },
                )
            },
        );
        assert!(result.is_err());
        assert!(attempted.load(Ordering::Acquire));
        assert_eq!(code.load(Ordering::Acquire), rusqlite::ffi::SQLITE_AUTH);
        assert!(store.db.is_autocommit());
        owner.validate_current().unwrap();
    }
    assert!(matches!(
        run(
            &owner,
            EndpointOperation::Probe {
                allocation_failure: false,
                panic: false
            }
        )
        .unwrap(),
        EndpointOutput::Revalidated
    ));
}
#[test]
fn fixed_endpoint_allocation_failure_and_unwind_keep_cleanup_observed() {
    let (_root, owner) = owner();
    let store = &owner.session.store;
    let error = run(
        &owner,
        EndpointOperation::Probe {
            allocation_failure: true,
            panic: false,
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("offline aggregate allocation budget exhausted")
    );
    assert!(store.db.is_autocommit());
    owner.validate_current().unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = run(
                &owner,
                EndpointOperation::Probe {
                    allocation_failure: false,
                    panic: true,
                },
            );
        }))
        .is_err()
    );
    assert!(store.db.is_autocommit());
    owner.validate_current().unwrap();
    run(
        &owner,
        EndpointOperation::Probe {
            allocation_failure: false,
            panic: false,
        },
    )
    .unwrap();
}
#[test]
fn fixed_endpoint_never_adopts_outer_write_or_revives_denied_owner() {
    let (_root, owner) = owner();
    let store = &owner.session.store;
    store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(
        run(
            &owner,
            EndpointOperation::Probe {
                allocation_failure: false,
                panic: false
            }
        )
        .is_err()
    );
    assert!(!store.db.is_autocommit());
    store.db.execute_batch("ROLLBACK").unwrap();
    store
        .db
        .authorizer(Some(|_: rusqlite::hooks::AuthContext<'_>| {
            rusqlite::hooks::Authorization::Deny
        }))
        .unwrap();
    assert!(
        run(
            &owner,
            EndpointOperation::Probe {
                allocation_failure: false,
                panic: false
            }
        )
        .is_err()
    );
    assert!(store.db.query_row("SELECT 1", [], |_| Ok(())).is_err());
}
