//! Completion and exact admission tests through the real offline owner. These
//! selector probes do not claim source/root/descriptor authority; the source
//! succession fixtures separately exercise the actual physical mutation.
use super::super::tests::{HOST, command, fixture, policy};
use super::*;
use catabolic_core::catalog_command::CommandKey;
use std::time::Duration;

pub(super) fn committed(
    retention: Duration,
) -> (tempfile::TempDir, OfflineBootstrapSession, CommandEnvelope) {
    let (root, session) = fixture();
    let first = command(&session, session.prepare(HOST, &[]).unwrap());
    let mut receipts = policy();
    receipts.retention = retention;
    assert!(matches!(
        session.execute(&first, HOST, &[], receipts).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    (root, session, first)
}

fn envelope(
    original: &CommandEnvelope,
    catalog: &str,
    host: &str,
    operation: &str,
    incarnation: &str,
    generation: i64,
    payload: &Value,
) -> CommandEnvelope {
    CommandEnvelope::new(
        original.contract_version(),
        CommandKey::new(catalog, host, operation).unwrap(),
        incarnation,
        generation,
        &payload.to_string(),
    )
    .unwrap()
}

#[test]
fn completed_catalog_requires_real_bootstrap_and_exact_committed_envelope() {
    let (_root, session) = fixture();
    let pending = command(&session, session.prepare(HOST, &[]).unwrap());
    assert!(session.reconcile(&pending, HOST).unwrap().is_none());
    assert!(session.into_completed_catalog(&pending).is_err());

    let (_root, session, first) = committed(Duration::from_secs(3600));
    let wrong = envelope(
        &first,
        first.key().catalog_id(),
        HOST,
        "not-the-committed-bootstrap",
        first.admission_incarnation(),
        first.expected_generation(),
        first.payload(),
    );
    assert!(session.complete(&first).is_ok());
    assert!(session.reconcile(&wrong, HOST).unwrap().is_none());
    assert!(session.into_completed_catalog(&wrong).is_err());
}

#[test]
fn completed_source_admission_binds_owner_and_every_envelope_fact() {
    let (_root, session, first) = committed(Duration::from_secs(3600));
    let mut owner = session.into_completed_catalog(&first).unwrap();
    let native = owner.retained_allocations().unwrap();
    assert!(native >= retained_command_bytes(&first).unwrap());
    let external = owner.external_retained_reservation().unwrap();
    owner
        .refresh_external_reservation(
            OfflineAllocationReservation::new(external.checked_add(1024).unwrap()).unwrap(),
        )
        .unwrap();
    assert_eq!(
        owner.external_retained_reservation().unwrap(),
        external + 1024
    );
    assert!(owner.retained_allocations().unwrap() > 0);
    assert!(
        owner
            .refresh_external_reservation(OfflineAllocationReservation::new(external).unwrap())
            .is_err()
    );
    assert_eq!(
        owner.external_retained_reservation().unwrap(),
        external + 1024
    );
    let (_other_root, other_session, other_first) = committed(Duration::from_secs(3600));
    let other = other_session.into_completed_catalog(&other_first).unwrap();
    // This is an exact admission probe only. A bootstrap envelope is never
    // accepted as a source operation by the separately closed source decoder.
    let admission = CompletedSourceAdmission {
        owner: &owner,
        command: &first,
        control: None,
    };
    let before = snapshot(&owner.session.store.db).unwrap();
    let other_before = snapshot(&other.session.store.db).unwrap();
    admission
        .check(&owner.session.store, &first.clone())
        .unwrap();
    assert!(admission.check(&other.session.store, &first).is_err());
    let wrong = [
        envelope(
            &first,
            "another-catalog",
            HOST,
            first.key().operation_id(),
            first.admission_incarnation(),
            first.expected_generation(),
            first.payload(),
        ),
        envelope(
            &first,
            first.key().catalog_id(),
            "another-host",
            first.key().operation_id(),
            first.admission_incarnation(),
            first.expected_generation(),
            first.payload(),
        ),
        envelope(
            &first,
            first.key().catalog_id(),
            HOST,
            "another-operation",
            first.admission_incarnation(),
            first.expected_generation(),
            first.payload(),
        ),
        envelope(
            &first,
            first.key().catalog_id(),
            HOST,
            first.key().operation_id(),
            "another-incarnation",
            first.expected_generation(),
            first.payload(),
        ),
        envelope(
            &first,
            first.key().catalog_id(),
            HOST,
            first.key().operation_id(),
            first.admission_incarnation(),
            first.expected_generation() + 1,
            first.payload(),
        ),
        envelope(
            &first,
            first.key().catalog_id(),
            HOST,
            first.key().operation_id(),
            first.admission_incarnation(),
            first.expected_generation(),
            &json!({"operation":"not-bootstrap"}),
        ),
    ];
    for different in &wrong {
        assert!(admission.check(&owner.session.store, different).is_err());
    }
    // Exact matching bytes cannot make a command from a different catalog or
    // incarnation current. Bind those bad envelopes to their own admission to
    // exercise the identity check, not just the envelope equality check.
    for different in [&wrong[0], &wrong[3]] {
        let foreign = CompletedSourceAdmission {
            owner: &owner,
            command: different,
            control: None,
        };
        assert!(foreign.check(&owner.session.store, different).is_err());
    }
    owner
        .session
        .store
        .transaction_detailed::<_, Error>(|_| {
            admission.check(&owner.session.store, &first)?;
            assert!(admission.check(&other.session.store, &first).is_err());
            for different in &wrong {
                assert!(admission.check(&owner.session.store, different).is_err());
            }
            Ok(())
        })
        .unwrap();
    assert!(owner.session.store.db.is_autocommit());
    assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
    assert_eq!(snapshot(&other.session.store.db).unwrap(), other_before);
}

#[test]
fn completed_preparation_admission_binds_exact_selectors_without_inventing_source_proof() {
    let (root, session, first) = committed(Duration::from_secs(3600));
    let mut owner = session.into_completed_catalog(&first).unwrap();
    let (_other_root, other_session, other_first) = committed(Duration::from_secs(3600));
    let other = other_session.into_completed_catalog(&other_first).unwrap();
    let candidate = root.path().join("candidate");
    let other_candidate = root.path().join("other-candidate");
    let before = snapshot(&owner.session.store.db).unwrap();
    let other_before = snapshot(&other.session.store.db).unwrap();
    {
        let admission = CompletedSourcePreparationAdmission {
            owner: &owner,
            profile: "fixture-profile",
            source: "fixture-source",
            candidate: &candidate,
            control: None,
        };
        admission
            .check(
                &owner.session.store,
                "fixture-profile",
                "fixture-source",
                &candidate,
            )
            .unwrap();
        owner
            .session
            .store
            .transaction_detailed::<_, Error>(|_| {
                admission.check(
                    &owner.session.store,
                    "fixture-profile",
                    "fixture-source",
                    &candidate,
                )?;
                assert!(
                    admission
                        .check(
                            &other.session.store,
                            "fixture-profile",
                            "fixture-source",
                            &candidate
                        )
                        .is_err()
                );
                assert!(
                    admission
                        .check(
                            &owner.session.store,
                            "other-profile",
                            "fixture-source",
                            &candidate
                        )
                        .is_err()
                );
                assert!(
                    admission
                        .check(
                            &owner.session.store,
                            "fixture-profile",
                            "other-source",
                            &candidate
                        )
                        .is_err()
                );
                assert!(
                    admission
                        .check(
                            &owner.session.store,
                            "fixture-profile",
                            "fixture-source",
                            &other_candidate
                        )
                        .is_err()
                );
                Ok(())
            })
            .unwrap();
    }
    // A valid completion and exact selector admission alone do not create a
    // source registration, root or complete physical context in an empty DB.
    assert!(
        owner
            .prepare_source_succession(
                "fixture-profile",
                "fixture-source",
                &candidate,
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert!(owner.session.store.db.is_autocommit());
    assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
    assert_eq!(snapshot(&other.session.store.db).unwrap(), other_before);
}

#[test]
fn completed_catalog_accepts_officially_expired_bootstrap_receipt_inside_writer() {
    let (_root, session, first) = committed(Duration::ZERO);
    let complete = session.complete(&first).unwrap();
    let inventory_digest = complete.inventory_digest().to_owned();
    let source_count = complete.source_count();
    let committed_generation = complete.committed_generation();
    let transition_digest = complete.transition_digest().to_owned();
    drop(complete);
    assert_eq!(session.compact_receipt_for_test(&first, HOST).unwrap(), 1);
    assert!(matches!(
        session.reconcile(&first, HOST).unwrap(),
        Some(CommandOutcome::ReceiptExpired)
    ));
    let owner = session.into_completed_catalog(&first).unwrap();
    assert_eq!(owner.inventory_digest, inventory_digest);
    assert_eq!(owner.source_count, source_count);
    assert_eq!(owner.committed_generation, committed_generation);
    assert_eq!(owner.transition_digest, transition_digest);
    let before = snapshot(&owner.session.store.db).unwrap();
    owner
        .session
        .store
        .transaction_detailed::<_, Error>(|_| {
            // This invokes historical receipt verification under the real retained
            // writer and shared operation budget, not the top-level fast path.
            owner.validate_current()?;
            let historical =
                crate::catalog_nfo_attribution::verify_historical_bootstrap_receipt_in(
                    &owner.session.store,
                    &first,
                )?;
            assert_eq!(historical.inventory_digest, inventory_digest);
            assert_eq!(historical.source_count, source_count);
            assert_eq!(historical.committed_generation, committed_generation);
            Ok(())
        })
        .unwrap();
    assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
    assert_eq!(owner.session.generation().unwrap(), committed_generation);
}
