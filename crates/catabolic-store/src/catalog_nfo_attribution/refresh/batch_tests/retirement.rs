//! Real withdrawal followed by logical retirement must retain public absence
//! and native source history without manufacturing another attribution revision.
use super::*;

#[test]
fn actual_withdrawn_source_retirement_is_attribution_noop_and_remains_readable() {
    let fixture = ready();
    let publish = request(
        &fixture,
        Workflow::DirectRefresh,
        [
            parsed("before withdrawal", Some("retirement-withdrawn")),
            unusable(),
        ],
    );
    execute(&fixture, "retirement-initial-publication", &publish);
    let absent = request(
        &fixture,
        Workflow::DirectRefresh,
        [SidecarObservation::Absent, SidecarObservation::Absent],
    );
    execute(&fixture, "retirement-real-withdrawal", &absent);
    let before = fixture
        .store
        .transaction_detailed(|_| {
            state::read_in(
                &fixture.store,
                physical::WORK,
                physical::generation(&fixture.store),
                7,
            )
        })
        .unwrap();
    assert!(matches!(
        before.head().source.as_ref().unwrap().document,
        DocumentState::Withdrawn
    ));
    assert!(before.presence().presence.public_revision.is_none());
    let attribution = before.head().attribution.as_ref().unwrap();
    assert!(attribution.origin.is_none() && attribution.active_identity.is_none());
    let history = crate::migration::rows(
        &fixture.store.db,
        "SELECT * FROM catalog_nfo_state_revisions ORDER BY item_id,revision",
        &[],
    )
    .unwrap();
    let metadata = crate::migration::rows(
        &fixture.store.db,
        "SELECT * FROM catalog_metadata_history ORDER BY item_id,namespace,record_id,revision",
        &[],
    )
    .unwrap();
    let fixture = fixture.retire();
    let after = fixture
        .store
        .transaction_detailed(|_| {
            state::read_in(
                &fixture.store,
                physical::WORK,
                physical::generation(&fixture.store),
                7,
            )
        })
        .unwrap();
    assert!(matches!(
        after.head().subject_state,
        SubjectState::Retired { .. }
    ));
    assert_eq!(after.head().source, before.head().source);
    assert_eq!(after.head().attribution, before.head().attribution);
    assert_eq!(after.presence().presence, before.presence().presence);
    assert_eq!(
        crate::migration::rows(
            &fixture.store.db,
            "SELECT * FROM catalog_nfo_state_revisions ORDER BY item_id,revision",
            &[]
        )
        .unwrap(),
        history
    );
    assert_eq!(
        crate::migration::rows(
            &fixture.store.db,
            "SELECT * FROM catalog_metadata_history ORDER BY item_id,namespace,record_id,revision",
            &[]
        )
        .unwrap(),
        metadata
    );
    let fixture = fixture.reopen_candidate();
    fixture
        .store
        .transaction_detailed(|_| {
            state::read_in(
                &fixture.store,
                physical::WORK,
                physical::generation(&fixture.store),
                7,
            )
        })
        .unwrap();
}
