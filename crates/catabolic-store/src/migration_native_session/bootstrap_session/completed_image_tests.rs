//! Genuine offline migration/bootstrap completion, not fabricated schema/grants.
use super::*;
use catabolic_core::catalog_command::CommandKey;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
const HOST: &str = "completed-image-fixture";
fn committed() -> (tempfile::TempDir, OfflineBootstrapSession, CommandEnvelope) {
    let (root, session) = super::tests::fixture();
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
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
    };
    assert!(matches!(
        session.execute(&command, HOST, &[], policy).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    (root, session, command)
}
fn inventory(session: &OfflineBootstrapSession) -> crate::catalog_backup::inventory::Audit {
    crate::catalog_backup::inventory::capture(
        &session.store.db,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
#[test]
fn genuine_complete_capture_preserves_source_and_retained_session() {
    let (_root, session, command) = committed();
    let before = inventory(&session);
    let complete = session.complete(&command).unwrap();
    let image = crate::catalog_restore_lineage::completed_image::capture(
        &session.store,
        &complete,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(session.store.db.is_autocommit());
    assert_eq!(before, inventory(&session));
    drop(image);
    assert!(session.complete(&command).is_ok());
    let owner = session
        .into_completed_image(&command, &AtomicBool::new(false))
        .unwrap();
    drop(owner);
}
#[test]
fn genuine_complete_capture_rejects_other_source_owner() {
    let (_root, session, command) = committed();
    let (_other_root, other) = super::tests::fixture();
    let complete = session.complete(&command).unwrap();
    assert!(
        crate::catalog_restore_lineage::completed_image::capture(
            &other.store,
            &complete,
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert!(other.store.db.is_autocommit());
    assert!(session.complete(&command).is_ok());
}
#[test]
fn genuine_complete_capture_cancellation_rolls_back_snapshot_without_effects() {
    let (_root, session, command) = committed();
    let before = inventory(&session);
    let complete = session.complete(&command).unwrap();
    assert!(
        crate::catalog_restore_lineage::completed_image::capture(
            &session.store,
            &complete,
            &AtomicBool::new(true)
        )
        .is_err()
    );
    let cancelled = AtomicBool::new(false);
    assert!(
        crate::catalog_restore_lineage::completed_image::capture_cancel_after_snapshot_for_test(
            &session.store,
            &complete,
            &cancelled
        )
        .is_err()
    );
    assert!(cancelled.load(Ordering::Acquire));
    assert!(session.store.db.is_autocommit());
    assert_eq!(before, inventory(&session));
    assert!(session.complete(&command).is_ok());
}
#[test]
fn incomplete_bootstrap_cannot_mint_completed_image() {
    let (_root, session) = super::tests::fixture();
    let payload = session.prepare(HOST, &[]).unwrap();
    let (catalog, incarnation) = session.identity();
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(catalog, HOST, "unexecuted").unwrap(),
        incarnation,
        session.generation().unwrap(),
        &payload.to_string(),
    )
    .unwrap();
    assert!(
        session
            .into_completed_image(&command, &AtomicBool::new(false))
            .is_err()
    );
}

fn attestation(
    image: &crate::catalog_restore_lineage::completed_image::ValidatedCompletedImage,
) -> crate::catalog_restore_lineage::completed_image::RestoreHostAttestation {
    let facts = image.description();
    // Deliberate native-owner fixture host attestations, NOT a Motion ACK or
    // independently verified paired deployment/install claim.
    crate::catalog_restore_lineage::completed_image::RestoreHostAttestation {
        pair_sha256: "a".repeat(64),
        motion_binding_sha256: "b".repeat(64),
        catalog_id: facts.catalog_id,
        incarnation: facts.incarnation,
        completed_inventory_sha256: facts.completed_inventory_sha256,
        transition_digest: facts.transition_digest,
    }
}
#[test]
fn genuine_completed_image_rotates_only_copy_and_exports_verified_bytes() {
    let (_root, session, command) = committed();
    let before = inventory(&session);
    let complete = session.complete(&command).unwrap();
    let image = crate::catalog_restore_lineage::completed_image::capture(
        &session.store,
        &complete,
        &AtomicBool::new(false),
    )
    .unwrap();
    let host = attestation(&image);
    let rotated = image
        .rotate(&session.store, &complete, &host, &AtomicBool::new(false))
        .unwrap();
    let origin: Value = serde_json::from_str(rotated.origin_json()).unwrap();
    assert_eq!(origin["from_incarnation"], complete.incarnation());
    assert_ne!(origin["to_incarnation"], complete.incarnation());
    assert_eq!(origin["pair_sha256"], host.pair_sha256);
    assert_eq!(origin["motion_binding_sha256"], host.motion_binding_sha256);
    let mut bytes = Vec::new();
    rotated
        .copy_verified_to(&mut bytes, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(bytes.len() as u64, rotated.len());
    assert_eq!(catabolic_core::sha256(&bytes), rotated.sha256());
    let mut copied = Connection::open_in_memory().unwrap();
    copied
        .deserialize_read_exact("main", std::io::Cursor::new(&bytes), bytes.len(), true)
        .unwrap();
    let actual: String = copied
        .query_row(
            "SELECT incarnation FROM catalog_state WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(origin["to_incarnation"], actual);
    assert_eq!(before, inventory(&session));
    assert!(session.complete(&command).is_ok());
    let mut refused = Vec::new();
    assert!(
        rotated
            .copy_verified_to(&mut refused, &AtomicBool::new(true))
            .is_err()
    );
    assert!(refused.is_empty());
    // The image was moved into rotate, so repeated consumption is impossible
    // through this Rust API; no Clone/Deserialize implementation exists.
}
#[test]
fn completed_image_rejects_wrong_native_attestation_without_source_effect() {
    let (_root, session, command) = committed();
    let before = inventory(&session);
    let complete = session.complete(&command).unwrap();
    let image = crate::catalog_restore_lineage::completed_image::capture(
        &session.store,
        &complete,
        &AtomicBool::new(false),
    )
    .unwrap();
    let mut host = attestation(&image);
    host.completed_inventory_sha256 = "0".repeat(64);
    assert!(
        image
            .rotate(&session.store, &complete, &host, &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(before, inventory(&session));
}
#[test]
fn completed_image_rotation_failure_returns_no_artifact_or_source_effect() {
    let (_root, session, command) = committed();
    let before = inventory(&session);
    let complete = session.complete(&command).unwrap();
    let image = crate::catalog_restore_lineage::completed_image::capture(
        &session.store,
        &complete,
        &AtomicBool::new(false),
    )
    .unwrap();
    let host = attestation(&image);
    assert!(
        image
            .fail_after_rotation_for_test(&session.store, &complete, &host, &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(before, inventory(&session));
    assert!(session.complete(&command).is_ok());
}

#[test]
fn consumed_completed_facade_rotates_and_copies_with_original_custody_retained() {
    let (_root, session, command) = committed();
    let before = inventory(&session);
    let owner = session
        .into_completed_image(&command, &AtomicBool::new(false))
        .unwrap();
    let facts = owner.description();
    let host = crate::catalog_restore_lineage::completed_image::RestoreHostAttestation {
        pair_sha256: "a".repeat(64),
        motion_binding_sha256: "b".repeat(64),
        catalog_id: facts.catalog_id,
        incarnation: facts.incarnation,
        completed_inventory_sha256: facts.completed_inventory_sha256,
        transition_digest: facts.transition_digest,
    };
    let rotated = owner.rotate(&host, &AtomicBool::new(false)).unwrap();
    assert_eq!(before, inventory(&rotated.session));
    assert!(rotated.session.complete(&command).is_ok());
    let mut bytes = Vec::new();
    rotated
        .copy_verified_to(&mut bytes, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(catabolic_core::sha256(&bytes), rotated.sha256());
    assert_eq!(bytes.len() as u64, rotated.len());
    assert_eq!(before, inventory(&rotated.session));
    struct FailingWriter;
    impl std::io::Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("fixture output failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(
        rotated
            .copy_verified_to(&mut FailingWriter, &AtomicBool::new(false))
            .is_err()
    );
    assert!(rotated.session.complete(&command).is_ok());
    assert_eq!(before, inventory(&rotated.session));
}
