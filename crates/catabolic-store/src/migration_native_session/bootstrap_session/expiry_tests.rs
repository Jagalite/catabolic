//! Genuine receipt compaction, never synthetic expiry SQL or caller time.
use super::*;
use catabolic_core::catalog_command::CommandKey;
use std::time::Duration;
const HOST: &str = "offline-expiry-fixture";
#[test]
fn actual_bootstrap_expiry_preserves_complete_without_backup_or_evidence() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("expiry.db");
    initialize_native_at(&path, 40).unwrap();
    let owner = RetainedNativeUpgrade::acquire(
        &path,
        NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        },
    )
    .unwrap();
    let (owner, plan) = owner.plan_closed41().unwrap();
    let (owner, _) = owner
        .apply_closed41(&plan, None, &|_, _, _| Ok(()))
        .unwrap();
    let session = owner.into_bootstrap().unwrap();
    let payload = session.prepare(HOST, &[]).unwrap();
    let (catalog, incarnation) = session.identity();
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(catalog, HOST, "bootstrap").unwrap(),
        incarnation,
        session.generation().unwrap(),
        &payload.to_string(),
    )
    .unwrap();
    let policy = ReceiptPolicy {
        retention: Duration::ZERO,
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
    };
    assert!(matches!(
        session.execute(&command, HOST, &[], policy).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let before = session.complete(&command).unwrap();
    let state_digest = before.state_digest().to_owned();
    let inventory_digest = before.inventory_digest().to_owned();
    let generation = before.committed_generation();
    drop(before);
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
    // Inspect the actual owner-produced tombstone and preserved envelope facts.
    let tombstone: (String, Option<String>, i64, i64, i64, String, String, i64) = session.store.db.query_row(
        "SELECT result_state,result_json,expired_at_us,retain_until_us,committed_generation,payload_digest,admission_incarnation,expected_generation FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?",
        rusqlite::params![command.key().catalog_id(), HOST, command.key().operation_id()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).unwrap();
    assert_eq!(tombstone.0, "expired");
    assert!(tombstone.1.is_none());
    assert!(tombstone.2 >= tombstone.3);
    assert_eq!(tombstone.4, generation);
    assert_eq!(tombstone.5, command.digest().as_str());
    assert_eq!(tombstone.6, command.admission_incarnation());
    assert_eq!(tombstone.7, command.expected_generation());
    let backup_path = session.backup().unwrap().path_for_test().to_owned();
    fs::rename(&backup_path, backup_path.with_extension("unavailable")).unwrap();
    assert!(session.prepare(HOST, &[]).is_err());
    assert!(matches!(
        session.reconcile(&command, HOST).unwrap(),
        Some(CommandOutcome::ReceiptExpired)
    ));
    assert!(matches!(
        session.execute(&command, HOST, &[], policy).unwrap(),
        CommandOutcome::ReceiptExpired
    ));
    let after = session.complete(&command).unwrap();
    assert_eq!(after.state_digest(), state_digest);
    assert_eq!(after.inventory_digest(), inventory_digest);
    assert_eq!(after.committed_generation(), generation);
    assert_eq!(after.source_count(), 0);
    assert!(
        catalog_commands::compact_receipts(
            &session.store,
            catalog_commands::TrustedReceiptMaintenance {
                host_namespace: HOST,
                holder_namespace: "ordinary-denied"
            },
            1
        )
        .is_err()
    );
}
