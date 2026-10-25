use super::*;
use crate::{
    catalog_commands::{
        self as commands, CommandOutcome, ReceiptPolicy, TrustedReceiptMaintenance,
    },
    migration,
};
use catabolic_core::catalog_command::CommandKey;
use serde_json::json;
use std::time::Duration;

fn authority() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: "nfo-proof",
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: false,
        identity_authority: None,
    }
}
fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap()
}
fn command(store: &Store, operation: &str, title: &str) -> CommandEnvelope {
    CommandEnvelope::new(1, CommandKey::new(&store.database_id,"nfo-proof",operation).unwrap(),store.catalog_incarnation.clone().unwrap(),generation(store),&json!({"operation":"item_put","item":{"id":"subject","kind":"video","metadata":{"title":title}}}).to_string()).unwrap()
}
fn fixture() -> (tempfile::TempDir, Store, CommandEnvelope) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("proof.db");
    migration::initialize_native_at(&path, 40).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    let command = command(&store, "original", "Original");
    let write = TrustedCommandAuthority {
        allow_new_commands: true,
        ..authority()
    };
    assert!(matches!(
        commands::execute(
            &store,
            &command,
            write,
            ReceiptPolicy {
                retention: Duration::ZERO,
                max_receipts: 100,
                max_logical_bytes: 4 * 1024 * 1024,
                max_hold_records: 100
            }
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    (root, store, command)
}
fn verify(store: &Store, command: &CommandEnvelope) -> Result<VerifiedCommittedCommand> {
    store.transaction_detailed(|_| {
        verify_committed_command_in(store, command, authority(), generation(store))
    })
}
#[test]
fn full_and_expired_commit_provenance_never_resurrects_replay_result() {
    let (_root, store, command) = fixture();
    let before = generation(&store);
    let full = verify(&store, &command).unwrap();
    assert!(full.matches(&command));
    assert_eq!(full.committed_generation(), before);
    assert_eq!(full.observed_generation(), before);
    assert_eq!(
        commands::compact_receipts(
            &store,
            TrustedReceiptMaintenance {
                host_namespace: "nfo-proof",
                holder_namespace: "proof-maintenance"
            },
            1
        )
        .unwrap(),
        1
    );
    let expired = verify(&store, &command).unwrap();
    assert!(expired.matches(&command));
    assert_eq!(expired.committed_generation(), full.committed_generation());
    assert!(matches!(
        commands::reconcile_receipt(&store, &command, authority()).unwrap(),
        Some(CommandOutcome::ReceiptExpired)
    ));
    assert_eq!(generation(&store), before);
    let body: Option<String> = store
        .db
        .query_row(
            "SELECT result_json FROM catalog_command_receipts",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(body, None);
}
#[test]
fn provenance_requires_exact_envelope_scope_writer_and_current_generation() {
    let (_root, store, original) = fixture();
    assert!(
        verify_committed_command_in(&store, &original, authority(), generation(&store)).is_err()
    );
    let altered = CommandEnvelope::new(1,original.key().clone(),original.admission_incarnation(),original.expected_generation(),&json!({"operation":"item_put","item":{"id":"subject","kind":"video","metadata":{"title":"Forged"}}}).to_string()).unwrap();
    assert!(verify(&store, &altered).is_err());
    let missing = command(&store, "missing", "Original");
    assert!(verify(&store, &missing).is_err());
    for denied in [
        TrustedCommandAuthority {
            host_namespace: "wrong",
            ..authority()
        },
        TrustedCommandAuthority {
            allow_new_commands: true,
            ..authority()
        },
        TrustedCommandAuthority {
            receipt_visibility: ReceiptVisibility::CommittedStatusOnly,
            ..authority()
        },
    ] {
        assert!(
            store
                .transaction_detailed::<_, Error>(|_| verify_committed_command_in(
                    &store,
                    &original,
                    denied,
                    generation(&store)
                ))
                .is_err()
        );
    }
    assert!(
        store
            .transaction_detailed::<_, Error>(|_| verify_committed_command_in(
                &store,
                &original,
                authority(),
                generation(&store) + 1
            ))
            .is_err()
    );
    assert!(verify(&store, &original).is_ok());
}
#[test]
fn historical_provenance_refuses_prior_incarnation_even_with_retained_commit() {
    let (_root, store, original) = fixture();
    let path = store.path.clone();
    drop(store);
    // Disposable restore-incarnation fixture, using the catalog's exact restore
    // permit. This does not expose a production restore operation.
    let db = crate::connect(&path, true).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) SELECT 'restore',catalog_id,incarnation FROM catalog_state; UPDATE catalog_state SET incarnation=lower(hex(randomblob(16))); DELETE FROM catalog_mutation_permits; COMMIT;").unwrap();
    drop(db);
    let store = Store::open(&path, true, false).unwrap();
    assert_ne!(
        store.catalog_incarnation.as_deref(),
        Some(original.admission_incarnation())
    );
    assert!(verify(&store, &original).is_err());
}
