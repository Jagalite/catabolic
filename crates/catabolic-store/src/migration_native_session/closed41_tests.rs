use super::*;
fn quiescence() -> NativeUpgradeQuiescence {
    NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}
fn seed(path: &Path) {
    use crate::catalog_commands::{self, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority};
    use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
    initialize_native_at(path, 40).unwrap();
    let store = crate::Store::open(path, true, false).unwrap();
    let generation: i64 = store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let payload = json!({"operation":"item_put","item":{"id":"preserved-work","kind":"video","metadata":{"title":"Original40"}}});
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(
            &store.database_id,
            "offline-transition-tests",
            "source-item",
        )
        .unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        generation,
        &payload.to_string(),
    )
    .unwrap();
    let outcome = catalog_commands::execute(
        &store,
        &command,
        TrustedCommandAuthority {
            host_namespace: "offline-transition-tests",
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: None,
        },
        ReceiptPolicy {
            retention: std::time::Duration::from_secs(3600),
            max_receipts: 100,
            max_hold_records: 100,
            max_logical_bytes: 16 * 1024 * 1024,
        },
    )
    .unwrap();
    assert!(
        matches!(outcome, CommandOutcome::Committed { .. }),
        "{outcome:?}"
    );
}
#[test]
fn actual_closed40_to_pending41_preserves_rows_receipts_backup_and_retained_locks() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    seed(&path);
    let session = RetainedNativeUpgrade::acquire(&path, quiescence()).unwrap();
    let (session, plan) = session.plan_closed41().unwrap();
    let before = plan["input"]["tables"].clone();
    let (session, result) = session
        .apply_closed41(&plan, None, &|_, _, _| Ok(()))
        .unwrap();
    assert_eq!(result["bootstrap_phase"], "pending");
    assert!(session.db.is_autocommit());
    preservation(&session.db, &before).unwrap();
    closed41::Closed41Target.validate(&session.db).unwrap();
    let transition = transition::read_in(&session.db).unwrap().unwrap();
    assert_eq!(result["transition_digest"], transition.digest().unwrap());
    session
        .verified_backup
        .as_ref()
        .unwrap()
        .revalidate()
        .unwrap();
    let (session, pending) = session.pending_inventory().unwrap();
    assert!(
        pending
            .as_object()
            .unwrap()
            .values()
            .all(|v| v.as_i64() == Some(0))
    );
    assert!(writer_lock(&path).is_err());
    assert!(crate::native_admission_exclusive(&path).is_err());
    assert_eq!(
        session
            .db
            .query_row("SELECT count(*) FROM catalog_nfo_bootstrap", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(session);
    assert!(
        crate::Store::open(&path, true, false).is_err(),
        "pending41 is not normal admission"
    );
    let owner = writer_lock(&path).unwrap();
    drop(owner);
}
#[test]
fn closed41_input_plan_change_and_precommit_failure_preserve_actual40() {
    for changed_plan in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        seed(&path);
        let session = RetainedNativeUpgrade::acquire(&path, quiescence()).unwrap();
        let (session, mut plan) = session.plan_closed41().unwrap();
        let before = plan["input"]["tables"].clone();
        if changed_plan {
            plan["migration_resources"][40]["checksum"] = json!("0".repeat(64));
        }
        let result = session.apply_closed41(&plan, None, &|stage, _, _| {
            if stage == "upgrade:before_commit" {
                Err(Error("deliberate final refusal".into()))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        let store = crate::Store::open(&path, true, false).unwrap();
        assert_eq!(store.schema_version, 40);
        preservation(&store.db, &before).unwrap();
        assert!(
            !store
                .db
                .prepare("SELECT 1 FROM sqlite_schema WHERE name='catalog_native_transition_40_41'")
                .unwrap()
                .exists([])
                .unwrap()
        );
    }
}
#[test]
fn postcommit_unwind_keeps_real_pending_transition_but_never_normal_admission() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    seed(&path);
    let session = RetainedNativeUpgrade::acquire(&path, quiescence()).unwrap();
    let (session, plan) = session.plan_closed41().unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = session.apply_closed41(&plan, None, &|stage, _, _| {
            if stage == "upgrade:after_commit" {
                panic!("postcommit interruption");
            }
            Ok(())
        });
    }));
    assert!(outcome.is_err());
    assert!(crate::Store::open(&path, true, false).is_err());
    let owner = RetainedNativeUpgrade::acquire(&path, quiescence()).unwrap();
    assert_eq!(
        owner
            .db
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        41
    );
    assert_eq!(
        transition::read_in(&owner.db)
            .unwrap()
            .unwrap()
            .approved_plan_sha256,
        plan["plan_fingerprint"].as_str().unwrap()
    );
    // Historical inspection is not a replay/complete grant. Recovery composition
    // must independently validate pending41 before executing bootstrap.
    assert!(
        owner.pending_inventory().is_err(),
        "reopen cannot invent live preservation proof"
    );
}

#[test]
fn live_pending_inventory_refuses_changed_preserved_source() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    seed(&path);
    let session = RetainedNativeUpgrade::acquire(&path, quiescence()).unwrap();
    let (session, plan) = session.plan_closed41().unwrap();
    let (session, _) = session
        .apply_closed41(&plan, None, &|_, _, _| Ok(()))
        .unwrap();
    session
        .db
        .execute(
            "UPDATE catalog_state SET generation=generation+1 WHERE id=1",
            [],
        )
        .unwrap();
    assert!(session.pending_inventory().is_err());
    assert!(writer_lock(&path).is_ok(), "failure consumes custody");
    assert!(crate::Store::open(&path, true, false).is_err());
}

#[test]
fn unwitnessed_and_truncated_worker_history_never_reaches_pending41() {
    for count in [1_i64, 257] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        seed(&path);
        let store = crate::Store::open(&path, true, false).unwrap();
        store.transaction(|db| {
            for id in 0..count {
                db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,completed_at,lease_until) VALUES(?,'default','media','unwitnessed-fixture','[]',0,?,'failed',1,2,0)", rusqlite::params![format!("unwitnessed-{id}"), id + 1])?;
            }
            Ok(())
        }).unwrap();
        // Distinct real source generations exercise bounded history, not a
        // duplicate-job fixture rejected by the schema before inspection.
        let generations: (i64, i64, i64) = store.db.query_row(
            "SELECT count(DISTINCT generation),min(generation),max(generation) FROM observation_jobs WHERE compatibility='unwitnessed-fixture'",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(generations, (count, 1, count));
        let inventory = crate::native_worker_inventory::inspect(&store).unwrap();
        assert!(!inventory.admits_new_work());
        assert_eq!(inventory.truncated, count > 256);
        drop(store);
        let result = (|| -> Result<()> {
            let owner = RetainedNativeUpgrade::acquire(&path, quiescence())?;
            let (owner, plan) = owner.plan_closed41()?;
            let _ = owner.apply_closed41(&plan, None, &|_, _, _| Ok(()))?;
            Ok(())
        })();
        assert!(result.is_err());
        let reopened = crate::Store::open(&path, true, false).unwrap();
        assert_eq!(reopened.schema_version, 40);
        let retained: i64 = reopened
            .db
            .query_row(
                "SELECT count(*) FROM observation_jobs WHERE compatibility='unwitnessed-fixture'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(retained, count);
    }
}
