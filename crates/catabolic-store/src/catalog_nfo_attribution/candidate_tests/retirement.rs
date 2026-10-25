//! Actual logical-owner composition on the unregistered candidate schema.
use super::*;
use crate::catalog_logical_edits::{
    self as logical, LogicalChange, LogicalEditAuthority, LogicalEditRequest, ReviewedLogicalEdit,
};

fn ready() -> Fixture {
    let f = Fixture::imported(true);
    committed(
        pin(
            &f.store,
            "retirement-manual-pin",
            SUBJECT,
            0,
            PinDecision::Set {
                value: "manual:retained".into(),
            },
        )
        .unwrap(),
    );
    let mut local = put_request(&f.store, 0, json!({}), &[]);
    local.namespace = "motion.local".into();
    local.excluded_tags = vec!["independently-hidden".into()];
    replace_40(
        &f.store,
        "retirement-local-suppression",
        &local,
        SourceKind::Local,
    );
    let f = f.install();
    f.bootstrap();
    f
}
fn reviewed(f: &Fixture, id: &str) -> (LogicalEditAuthority, ReviewedLogicalEdit, CommandEnvelope) {
    reviewed_endpoints(f, id, SUBJECT, OTHER)
}
fn reviewed_endpoints(
    f: &Fixture,
    id: &str,
    source: &str,
    survivor: &str,
) -> (LogicalEditAuthority, ReviewedLogicalEdit, CommandEnvelope) {
    let grant = LogicalEditAuthority {
        host_namespace: HOST.into(),
        item_ids: [SUBJECT.into(), OTHER.into()].into(),
        new_item_ids: BTreeSet::new(),
    };
    let preview = logical::preview_logical_edit(
        &f.store,
        LogicalEditRequest {
            contract_version: 1,
            change: LogicalChange::Merge {
                sources: vec![source.into()],
                survivor: survivor.into(),
            },
            new_items: vec![],
            association_clones: vec![],
            contexts: vec![],
        },
        &grant,
        None,
    )
    .unwrap();
    let command = envelope(
        &f.store,
        id,
        json!({"operation":"logical_edit","reviewed":preview.reviewed}),
    );
    (grant, preview.reviewed, command)
}
fn retained(store: &Store) -> Vec<Value> {
    [
        "SELECT * FROM catalog_metadata_sources ORDER BY item_id,namespace,record_id",
        "SELECT * FROM catalog_metadata_history ORDER BY item_id,namespace,record_id,revision",
        "SELECT * FROM catalog_identity_pins ORDER BY namespace,item_id",
        "SELECT * FROM catalog_item_tag_suppressions ORDER BY item_id,tag_id",
        "SELECT * FROM catalog_tag_suppression_revisions ORDER BY subject_type,subject_id,tag_id,revision",
    ].into_iter().map(|sql|json!(migration::rows(&store.db,sql,&[]).unwrap())).collect()
}

#[test]
fn actual_logical_retirement_releases_automatic_claim_only_and_replays_after_compaction() {
    let f = ready();
    let old = f.state();
    let active = old
        .head()
        .attribution
        .as_ref()
        .unwrap()
        .active_identity
        .clone()
        .unwrap();
    assert!(old.head().attribution.as_ref().unwrap().origin.is_some());
    assert!(old.origin_physical().is_some());
    assert!(
        pin(
            &f.store,
            "before-retirement-collision",
            OTHER,
            0,
            PinDecision::Set {
                value: active.clone()
            }
        )
        .is_err()
    );
    let retained_before = retained(&f.store);
    let old_history = migration::rows(
        &f.store.db,
        "SELECT * FROM catalog_nfo_state_revisions WHERE item_id=? ORDER BY revision",
        &[SUBJECT.to_owned().into()],
    )
    .unwrap();
    let (grant, reviewed, command) = reviewed(&f, "retire-attributed-owner");
    let mut retention = policy();
    retention.retention = Duration::ZERO;
    let result = committed(
        commands::execute_candidate_for_test(
            &f.store,
            &command,
            authority(true),
            retention,
            |store, outer, time| {
                logical::apply_logical_edit(store, &reviewed, outer, &grant, None, time)
                    .map_err(|e| e.error)
            },
        )
        .unwrap(),
    );
    let next = f.state();
    assert!(matches!(
        next.head().subject_state,
        SubjectState::Retired { .. }
    ));
    assert_eq!(next.head().source, old.head().source);
    assert_eq!(next.presence().presence, old.presence().presence);
    let attribution = next.head().attribution.as_ref().unwrap();
    assert_eq!(
        attribution.revision,
        old.head().attribution.as_ref().unwrap().revision + 1
    );
    assert!(attribution.active_identity.is_none() && attribution.origin.is_none());
    assert!(next.origin_physical().is_none() && next.origin_proof_digest().is_none());
    assert_eq!(retained(&f.store), retained_before);
    assert_eq!(
        migration::rows(
            &f.store.db,
            "SELECT * FROM catalog_nfo_state_revisions WHERE item_id=? AND revision=1",
            &[SUBJECT.to_owned().into()]
        )
        .unwrap(),
        old_history
    );
    for table in [
        "catalog_nfo_identity_claims",
        "catalog_nfo_file_attributions",
    ] {
        assert!(
            migration::rows(
                &f.store.db,
                &format!("SELECT * FROM {table} WHERE item_id=?"),
                &[SUBJECT.to_owned().into()]
            )
            .unwrap()
            .is_empty()
        );
    }
    let snapshot = migration::snapshot(&f.store.db).unwrap();
    assert!(
        matches!(commands::execute_candidate_for_test(&f.store,&command,authority(false),policy(),|_,_,_|panic!("receipt replay cannot reprepare retirement")).unwrap(),CommandOutcome::Replay{result:got,..} if got==result)
    );
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), snapshot);
    let f = f.reopen();
    assert!(
        f.state()
            .head()
            .attribution
            .as_ref()
            .unwrap()
            .active_identity
            .is_none()
    );
    committed(
        pin(
            &f.store,
            "after-retirement-reuse",
            OTHER,
            0,
            PinDecision::Set { value: active },
        )
        .unwrap(),
    );
    commands::compact_candidate_receipts_for_test(
        &f.store,
        commands::TrustedReceiptMaintenance {
            host_namespace: HOST,
            holder_namespace: "retirement-expiry",
        },
        100,
    )
    .unwrap();
    assert!(matches!(
        commands::execute_candidate_for_test(
            &f.store,
            &command,
            authority(false),
            policy(),
            |_, _, _| panic!("expired receipt cannot reexecute retirement")
        )
        .unwrap(),
        CommandOutcome::ReceiptExpired
    ));
    assert_eq!(f.state().head().source, old.head().source);
}

#[test]
fn actual_post_retirement_publication_error_cannot_commit_and_exact_retry_succeeds() {
    let f = ready();
    let before = migration::snapshot(&f.store.db).unwrap();
    let (grant, reviewed, command) = reviewed(&f, "retirement-swallowed-error");
    let failed=f.store.transaction_detailed::<(),Error>(|db| {
        let mut probe=|store:&Store|->Result<()> {
            let heads:(bool,bool,i64)=store.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?1),NOT EXISTS(SELECT 1 FROM catalog_nfo_identity_claims WHERE item_id=?1),(SELECT current_revision FROM catalog_nfo_states WHERE item_id=?1)",[SUBJECT],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            assert_eq!(heads,(true,true,2));
            Err(Error("terminal retirement publication failure".into()))
        };
        let error=logical::apply_logical_edit_with_probe(&f.store,&reviewed,&command,&grant,None,1,&mut probe).unwrap_err();
        assert!(error.to_string().contains("terminal retirement publication failure"));
        assert!(db.execute_batch("COMMIT").is_err(),"caught terminal error cannot clear logical barrier");
        Err(Error("expected rollback".into()))
    });
    assert!(failed.is_err());
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    committed(
        commands::execute_candidate_for_test(
            &f.store,
            &command,
            authority(true),
            policy(),
            |store, outer, time| {
                logical::apply_logical_edit(store, &reviewed, outer, &grant, None, time)
                    .map_err(|e| e.error)
            },
        )
        .unwrap(),
    );
    assert!(
        f.state()
            .head()
            .attribution
            .as_ref()
            .unwrap()
            .active_identity
            .is_none()
    );
}

#[test]
fn unbootstrapped_candidate_retirement_refuses_before_effects() {
    let f = Fixture::created().install();
    let (grant, reviewed, command) = reviewed(&f, "retirement-before-bootstrap");
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(
        commands::execute_candidate_for_test(
            &f.store,
            &command,
            authority(true),
            policy(),
            |store, outer, time| logical::apply_logical_edit(
                store, &reviewed, outer, &grant, None, time
            )
            .map_err(|e| e.error)
        )
        .is_err()
    );
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
}

#[test]
fn retirement_of_empty_or_unattributed_endpoint_does_not_invent_state_revision() {
    for empty in [true, false] {
        let f = Fixture::ready();
        let subject = if empty { OTHER } else { SUBJECT };
        let survivor = if empty { SUBJECT } else { OTHER };
        let old = f
            .store
            .transaction_detailed(|_| state::read_in(&f.store, subject, generation(&f.store), 0))
            .unwrap();
        if empty {
            assert!(old.head().source.is_none());
        } else {
            assert!(old.head().source.is_some());
            let attribution = old.head().attribution.as_ref().unwrap();
            assert!(attribution.active_identity.is_none() && attribution.origin.is_none());
        }
        let before = migration::rows(
            &f.store.db,
            "SELECT * FROM catalog_nfo_state_revisions ORDER BY item_id,revision",
            &[],
        )
        .unwrap();
        let (grant, reviewed, command) =
            reviewed_endpoints(&f, "retirement-no-attribution", subject, survivor);
        committed(
            commands::execute_candidate_for_test(
                &f.store,
                &command,
                authority(true),
                policy(),
                |store, outer, time| {
                    logical::apply_logical_edit(store, &reviewed, outer, &grant, None, time)
                        .map_err(|e| e.error)
                },
            )
            .unwrap(),
        );
        assert_eq!(
            migration::rows(
                &f.store.db,
                "SELECT * FROM catalog_nfo_state_revisions ORDER BY item_id,revision",
                &[]
            )
            .unwrap(),
            before
        );
        let next = f
            .store
            .transaction_detailed(|_| state::read_in(&f.store, subject, generation(&f.store), 0))
            .unwrap();
        assert!(matches!(
            next.head().subject_state,
            SubjectState::Retired { .. }
        ));
        assert_eq!(next.head().source, old.head().source);
        assert_eq!(next.head().attribution, old.head().attribution);
        assert_eq!(next.presence().presence, old.presence().presence);
    }
}
