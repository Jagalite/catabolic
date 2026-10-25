//! Public-facade qualification against genuine on-disk closed40 migration and
//! bootstrap owners. No private provenance grants, Store escape, candidate flag
//! or synthetic receipt is used. The sole test-only operation is forwarding
//! official receipt compaction under the same retained offline owner.
use crate::{
    Store,
    catalog_commands::{CommandOutcome, ReceiptPolicy},
    migration::{
        self, NativeUpgradeQuiescence,
        offline_bootstrap::{
            HostBaselineEvidence, HostSourceEvidence, OfflineBootstrap, PreparedOfflineBootstrap,
            Recovery,
        },
    },
};
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, time::Duration};

const HOST: &str = "public-offline-bootstrap-fixture";
fn quiescence() -> NativeUpgradeQuiescence {
    // This fixture created the catalog and has started no workers/readers.
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
fn migrated() -> (tempfile::TempDir, PathBuf, Value, OfflineBootstrap<'static>) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize_native_at(&path, 40).unwrap();
    let prepared = PreparedOfflineBootstrap::prepare(&path, quiescence()).unwrap();
    // The host durably saves this exact plan in production. Here the independent
    // owned value outlives both the initial session and the recovered borrow.
    let plan = prepared.approved_plan().clone();
    let session = prepared.apply(None).unwrap();
    assert_eq!(session.approved_plan(), &plan);
    (root, path, plan, session)
}
fn command(session: &OfflineBootstrap<'_>, id: &str, payload: &Value) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(session.catalog_id(), HOST, id).unwrap(),
        session.incarnation(),
        session.generation().unwrap(),
        &payload.to_string(),
    )
    .unwrap()
}
fn unusable(command: &CommandEnvelope) -> [HostSourceEvidence<'_>; 1] {
    // Deliberately not native-created evidence: wrong first operation and an
    // invalid independent audit. Any attempted adapter conversion must refuse.
    [HostSourceEvidence {
        baseline: HostBaselineEvidence::NativeCreated {
            first: command,
            subject: "not-an-audited-source",
            complete_absence_audit_digest: "not-an-audit-digest",
        },
        later_commands: &[],
    }]
}

#[test]
fn public_offline_facade_commits_empty_bootstrap_and_replays_before_host_conversion() {
    let (_root, path, _plan, session) = migrated();
    let transition = session.transition().unwrap();
    assert_eq!(transition.catalog_id(), session.catalog_id());
    assert_eq!(transition.incarnation(), session.incarnation());
    assert_eq!(
        transition.migrated_generation(),
        session.generation().unwrap()
    );
    assert!(transition.source_generation() <= transition.migrated_generation());
    for digest in [
        transition.digest(),
        transition.approved_plan_sha256(),
        transition.source_full_ledger_sha256(),
        transition.source_tables_sha256(),
        transition.verified_backup_image_sha256(),
        transition.migration41_sql_sha256(),
        transition.closed41_reference_sha256(),
    ] {
        assert_eq!(digest.len(), 64);
    }
    let transition_digest = transition.digest().to_owned();
    drop(transition);
    let payload = session.prepare(HOST, &[]).unwrap();
    let command = command(&session, "bootstrap", &payload);
    let unusable = unusable(&command);
    assert!(session.prepare(HOST, &unusable).is_err());
    assert!(session.reconcile(&command, HOST).unwrap().is_none());
    assert!(session.complete(&command).is_err());
    assert!(matches!(
        session.execute(&command, HOST, &[], policy()).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let complete = session.complete(&command).unwrap();
    assert_eq!(complete.catalog_id(), session.catalog_id());
    assert_eq!(complete.incarnation(), session.incarnation());
    assert_eq!(complete.transition_digest(), transition_digest);
    assert_eq!(complete.command_key(), command.key());
    assert_eq!(complete.payload_digest(), command.digest().as_str());
    assert_eq!(
        complete.inventory_digest(),
        payload["inventory_digest"].as_str().unwrap()
    );
    assert_eq!(complete.source_count(), 0);
    assert_eq!(
        complete.committed_generation(),
        session.generation().unwrap()
    );
    assert_eq!(
        complete.committed_generation(),
        command.expected_generation() + 1
    );
    let state_digest = complete.state_digest().to_owned();
    let generation = complete.committed_generation();
    drop(complete);

    // Lost host ACK: neither missing backup nor unusable archive evidence may
    // prevent observing the existing exact receipt or mint a second effect.
    let backup = session.verified_backup_path().unwrap().to_owned();
    fs::rename(&backup, backup.with_extension("unavailable")).unwrap();
    assert!(session.prepare(HOST, &[]).is_err());
    assert!(matches!(
        session.reconcile(&command, HOST).unwrap(),
        Some(CommandOutcome::Replay { .. })
    ));
    assert!(matches!(
        session
            .execute(&command, HOST, &unusable, policy())
            .unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(session.generation().unwrap(), generation);
    assert_eq!(
        session.complete(&command).unwrap().state_digest(),
        state_digest
    );
    drop(session);
    assert!(
        Store::open(&path, true, false).is_err(),
        "offline completion is not ordinary41 registration"
    );
}

#[test]
fn public_offline_facade_recovers_pending_then_completed_with_exact_saved_plan_and_key() {
    let (_root, path, plan, session) = migrated();
    let backup = session.verified_backup_path().unwrap().to_owned();
    let payload = session.prepare(HOST, &[]).unwrap();
    let first = command(&session, "bootstrap", &payload);
    drop(session);
    let mut wrong_plan = plan.clone();
    wrong_plan["input"]["catalog_state"][0]["generation"] = json!(999);
    assert!(
        OfflineBootstrap::recover(
            &path,
            quiescence(),
            &wrong_plan,
            Recovery::Pending {
                backup_path: &backup
            }
        )
        .is_err()
    );
    assert!(
        OfflineBootstrap::recover(
            &path,
            quiescence(),
            &plan,
            Recovery::Pending {
                backup_path: &backup.with_extension("missing")
            }
        )
        .is_err()
    );
    assert!(
        OfflineBootstrap::recover(
            &path,
            quiescence(),
            &plan,
            Recovery::CompletedReceiptOnly { command: &first }
        )
        .is_err(),
        "pending state is not receipt-only completion"
    );
    let session = OfflineBootstrap::recover(
        &path,
        quiescence(),
        &plan,
        Recovery::Pending {
            backup_path: &backup,
        },
    )
    .unwrap();
    assert!(
        std::ptr::eq(session.approved_plan(), &plan),
        "recovery borrows the exact retained plan"
    );
    assert!(session.reconcile(&first, HOST).unwrap().is_none());
    assert!(matches!(
        session.execute(&first, HOST, &[], policy()).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let state_digest = session.complete(&first).unwrap().state_digest().to_owned();
    // Change only the key, retaining the exact original payload and fence.
    let wrong_key = CommandEnvelope::new(
        first.contract_version(),
        CommandKey::new(first.key().catalog_id(), HOST, "wrong-operation-key").unwrap(),
        first.admission_incarnation(),
        first.expected_generation(),
        first.canonical_payload(),
    )
    .unwrap();
    let generation = session.generation().unwrap();
    drop(session);
    fs::rename(&backup, backup.with_extension("unavailable")).unwrap();
    assert!(
        OfflineBootstrap::recover(
            &path,
            quiescence(),
            &plan,
            Recovery::CompletedReceiptOnly {
                command: &wrong_key
            }
        )
        .is_err()
    );
    let session = OfflineBootstrap::recover(
        &path,
        quiescence(),
        &plan,
        Recovery::CompletedReceiptOnly { command: &first },
    )
    .unwrap();
    assert!(session.prepare(HOST, &[]).is_err());
    assert!(matches!(
        session.reconcile(&first, HOST).unwrap(),
        Some(CommandOutcome::Replay { .. })
    ));
    assert!(matches!(
        session
            .execute(&first, HOST, &unusable(&first), policy())
            .unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert!(session.reconcile(&wrong_key, HOST).is_err());
    assert!(session.execute(&wrong_key, HOST, &[], policy()).is_err());
    assert!(session.complete(&wrong_key).is_err());
    assert_eq!(session.generation().unwrap(), generation);
    assert_eq!(
        session.complete(&first).unwrap().state_digest(),
        state_digest
    );
}

#[test]
fn public_offline_facade_expired_receipt_recovery_never_reconstructs_unusable_evidence() {
    let (_root, path, plan, session) = migrated();
    let payload = session.prepare(HOST, &[]).unwrap();
    let first = command(&session, "bootstrap", &payload);
    let policy = ReceiptPolicy {
        retention: Duration::ZERO,
        ..policy()
    };
    assert!(matches!(
        session.execute(&first, HOST, &[], policy).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let complete = session.complete(&first).unwrap();
    let state_digest = complete.state_digest().to_owned();
    let inventory_digest = complete.inventory_digest().to_owned();
    let generation = complete.committed_generation();
    drop(complete);
    // Exact official due/hold-aware compactor, not synthetic receipt mutation.
    assert_eq!(session.compact_receipt_for_test(&first, HOST).unwrap(), 1);
    let backup = session.verified_backup_path().unwrap().to_owned();
    fs::rename(&backup, backup.with_extension("unavailable")).unwrap();
    assert!(matches!(
        session.reconcile(&first, HOST).unwrap(),
        Some(CommandOutcome::ReceiptExpired)
    ));
    assert!(matches!(
        session
            .execute(&first, HOST, &unusable(&first), policy)
            .unwrap(),
        CommandOutcome::ReceiptExpired
    ));
    assert_eq!(
        session.complete(&first).unwrap().state_digest(),
        state_digest
    );
    drop(session);
    let session = OfflineBootstrap::recover(
        &path,
        quiescence(),
        &plan,
        Recovery::CompletedReceiptOnly { command: &first },
    )
    .unwrap();
    assert!(session.prepare(HOST, &[]).is_err());
    assert!(matches!(
        session
            .execute(&first, HOST, &unusable(&first), policy)
            .unwrap(),
        CommandOutcome::ReceiptExpired
    ));
    let complete = session.complete(&first).unwrap();
    assert_eq!(complete.source_count(), 0);
    assert_eq!(complete.inventory_digest(), inventory_digest);
    assert_eq!(complete.state_digest(), state_digest);
    assert_eq!(complete.committed_generation(), generation);
    assert_eq!(session.generation().unwrap(), generation);
}
