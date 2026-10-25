//! Actual on-disk migration, owner drop and exclusive recovery. No fabricated
//! transition, receipt, bootstrap marker or candidate admission flag.
use super::*;
use catabolic_core::catalog_command::CommandKey;
use std::time::Duration;
const HOST: &str = "offline-recovery-fixture";
fn acquire(path: &Path) -> RetainedNativeUpgrade {
    RetainedNativeUpgrade::acquire(
        path,
        NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        },
    )
    .unwrap()
}
fn migrated() -> (
    tempfile::TempDir,
    PathBuf,
    Value,
    PathBuf,
    OfflineBootstrapSession,
) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("recovery.db");
    initialize_native_at(&path, 40).unwrap();
    let (owner, plan) = acquire(&path).plan_closed41().unwrap();
    let (owner, _) = owner
        .apply_closed41(&plan, None, &|_, _, _| Ok(()))
        .unwrap();
    let session = owner.into_bootstrap().unwrap();
    let backup = session.backup().unwrap().path_for_test().to_owned();
    (root, path, plan, backup, session)
}
fn command(session: &OfflineBootstrapSession, operation: &str) -> CommandEnvelope {
    let payload = session.prepare(HOST, &[]).unwrap();
    CommandEnvelope::new(
        1,
        CommandKey::new(session.identity().0, HOST, operation).unwrap(),
        session.identity().1,
        session.generation().unwrap(),
        &payload.to_string(),
    )
    .unwrap()
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}
fn assert_recovered_connection_settings(session: &OfflineBootstrapSession) {
    for (name, expected) in [
        ("foreign_keys", 1_i64),
        ("recursive_triggers", 1),
        ("read_uncommitted", 0),
    ] {
        let actual: i64 = session
            .store
            .db
            .pragma_query_value(None, name, |row| row.get(0))
            .unwrap();
        assert_eq!(actual, expected, "private recovery must configure {name}");
    }
}
#[test]
fn actual_pending_restart_requires_exact_plan_and_original_backup_then_completes() {
    let (_root, path, plan, backup, session) = migrated();
    drop(session);
    let mut wrong = plan.clone();
    wrong["input"]["catalog_state"][0]["generation"] = json!(999);
    assert!(
        acquire(&path)
            .recover_bootstrap(
                &wrong,
                BootstrapRecovery::Pending {
                    backup_path: &backup
                }
            )
            .is_err()
    );
    assert!(
        acquire(&path)
            .recover_bootstrap(
                &plan,
                BootstrapRecovery::Pending {
                    backup_path: &backup.with_extension("absent")
                }
            )
            .is_err()
    );
    let session = acquire(&path)
        .recover_bootstrap(
            &plan,
            BootstrapRecovery::Pending {
                backup_path: &backup,
            },
        )
        .unwrap();
    assert_recovered_connection_settings(&session);
    let command = command(&session, "bootstrap");
    assert!(matches!(
        session.execute(&command, HOST, &[], policy()).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    assert_eq!(session.complete(&command).unwrap().source_count(), 0);
    drop(session);
    assert!(Store::open(&path, true, false).is_err());
}
#[test]
fn actual_completed_restart_replays_without_backup_and_refuses_new_commands() {
    let (_root, path, plan, backup, session) = migrated();
    let command = command(&session, "bootstrap");
    let other = CommandEnvelope::new(
        1,
        CommandKey::new(session.identity().0, HOST, "other").unwrap(),
        session.identity().1,
        session.generation().unwrap(),
        &command.payload().to_string(),
    )
    .unwrap();
    session.execute(&command, HOST, &[], policy()).unwrap();
    let digest = session
        .complete(&command)
        .unwrap()
        .state_digest()
        .to_owned();
    drop(session);
    fs::rename(&backup, backup.with_extension("unavailable")).unwrap();
    assert!(
        acquire(&path)
            .recover_bootstrap(
                &plan,
                BootstrapRecovery::CompletedReceiptOnly { command: &other }
            )
            .is_err()
    );
    let session = acquire(&path)
        .recover_bootstrap(
            &plan,
            BootstrapRecovery::CompletedReceiptOnly { command: &command },
        )
        .unwrap();
    assert_recovered_connection_settings(&session);
    assert!(session.prepare(HOST, &[]).is_err());
    assert!(session.reconcile(&other, HOST).is_err());
    assert!(session.execute(&other, HOST, &[], policy()).is_err());
    assert!(matches!(
        session.reconcile(&command, HOST).unwrap(),
        Some(CommandOutcome::Replay { .. })
    ));
    assert!(matches!(
        session.execute(&command, HOST, &[], policy()).unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(session.complete(&command).unwrap().state_digest(), digest);
    let original_preservation = session.preservation.allocated_bytes().unwrap();
    let command_bytes = completed_catalog::retained_command_bytes(&command).unwrap();
    let allocation = session
        .store
        .offline_bootstrap
        .as_ref()
        .unwrap()
        .allocation
        .clone();
    let held_before = allocation.retained_components_for_test().unwrap().0;
    assert!(held_before >= original_preservation + command_bytes);
    let owner = session.into_completed_catalog(&command).unwrap();
    owner.retained_allocations().unwrap();
    let held = allocation.retained_components_for_test().unwrap().0;
    assert!(held >= original_preservation + 2 * command_bytes);
    drop(owner);
    assert!(Store::open(&path, true, false).is_err());
}
#[test]
fn completed_recovery_refuses_unrelated_old_table_mutation_and_releases_custody() {
    let (_root, path, plan, _backup, session) = migrated();
    let command = command(&session, "bootstrap");
    session.execute(&command, HOST, &[], policy()).unwrap();
    session.complete(&command).unwrap();
    // Supported diagnostic connection, not a forged NFO/transition capability.
    session
        .store
        .db
        .execute(
            "INSERT INTO meta(key,value) VALUES('offline-recovery-negative','changed')",
            [],
        )
        .unwrap();
    drop(session);
    assert!(
        acquire(&path)
            .recover_bootstrap(
                &plan,
                BootstrapRecovery::CompletedReceiptOnly { command: &command }
            )
            .is_err()
    );
    // Failure consumes the owner and releases actual lifecycle/writer custody.
    drop(acquire(&path));
}
#[test]
fn actual_expired_completed_receipt_reopens_without_reconstructing_effects() {
    let (_root, path, plan, backup, session) = migrated();
    let command = command(&session, "bootstrap");
    session
        .execute(
            &command,
            HOST,
            &[],
            ReceiptPolicy {
                retention: Duration::ZERO,
                ..policy()
            },
        )
        .unwrap();
    let execution = OfflineBootstrapExecution {
        session: &session,
        command: &command,
        host: HOST,
        evidence: None,
        control: None,
    };
    assert_eq!(
        catalog_commands::compact_offline_bootstrap_receipt_for_test(
            &session.store,
            &command,
            &execution
        )
        .unwrap(),
        1
    );
    let digest = session
        .complete(&command)
        .unwrap()
        .state_digest()
        .to_owned();
    drop(session);
    fs::rename(&backup, backup.with_extension("unavailable")).unwrap();
    let session = acquire(&path)
        .recover_bootstrap(
            &plan,
            BootstrapRecovery::CompletedReceiptOnly { command: &command },
        )
        .unwrap();
    assert!(matches!(
        session.reconcile(&command, HOST).unwrap(),
        Some(CommandOutcome::ReceiptExpired)
    ));
    assert!(matches!(
        session.execute(&command, HOST, &[], policy()).unwrap(),
        CommandOutcome::ReceiptExpired
    ));
    assert_eq!(session.complete(&command).unwrap().state_digest(), digest);
}
