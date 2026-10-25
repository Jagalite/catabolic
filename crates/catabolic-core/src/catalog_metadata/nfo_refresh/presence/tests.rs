// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
use super::*;
use serde_json::json;

fn empty() -> PresenceSnapshot {
    PresenceSnapshot {
        contract_version: PRESENCE_CONTRACT_VERSION,
        fence: Fence {
            catalog_id: "catalog".into(),
            incarnation: "restore-1".into(),
            generation: 4,
        },
        subject: "work".into(),
        aggregate_revision: 1,
        head: SubjectHead {
            subject_state: SubjectState::Active { item_revision: 1 },
            schema_fingerprint: Some("a".repeat(64)),
            system_fields: BTreeSet::new(),
            source: None,
            attribution: None,
            pin: PinHead::NeverDecided,
        },
        presence: SourcePresence {
            revision: 0,
            incarnation: 0,
            public_revision: None,
        },
    }
}
fn values(title: &str) -> Contribution {
    Contribution {
        fields: [("title".into(), json!(title))].into(),
        tags: ["drama".into()].into(),
    }
}
fn origin(file: &str) -> FileOrigin {
    FileOrigin {
        file: FileKey {
            profile: "profile".into(),
            file_id: file.into(),
        },
        provenance: OriginProvenance::Observed {
            exact_revision: "b".repeat(64),
            binding_fingerprint: "c".repeat(64),
            ownership_fingerprint: "d".repeat(64),
        },
    }
}
fn publish(title: &str, file: &str) -> PresenceChange {
    PresenceChange::Publish {
        contribution: values(title),
        external_identity: Some("imdb:tt1".into()),
        origin: origin(file),
    }
}
fn put(revision: i64, title: &str) -> PresenceChange {
    PresenceChange::PublicPut {
        expected_public_revision: revision,
        contribution: values(title),
        external_identity: Some("imdb:tt1".into()),
    }
}
fn request(snapshot: &PresenceSnapshot, change: PresenceChange) -> PresenceRequest {
    PresenceRequest {
        contract_version: PRESENCE_CONTRACT_VERSION,
        fence: snapshot.fence.clone(),
        subject: snapshot.subject.clone(),
        expected: snapshot.stamp(),
        change,
    }
}
fn apply(snapshot: &PresenceSnapshot, change: PresenceChange) -> PresencePlan {
    plan_presence(snapshot, &request(snapshot, change)).unwrap()
}
fn present() -> PresenceSnapshot {
    apply(&empty(), publish("Title", "file-a")).final_snapshot
}
fn native_revision(snapshot: &PresenceSnapshot) -> i64 {
    snapshot.head.source.as_ref().unwrap().revision
}
fn attribution_revision(snapshot: &PresenceSnapshot) -> i64 {
    snapshot.head.attribution.as_ref().unwrap().revision
}

#[test]
fn absent_get_is_omitted_put_zero_inserts_public_one() {
    let snapshot = empty();
    assert_eq!(public_nfo_document(&snapshot).unwrap(), None);
    let plan = apply(&snapshot, put(0, "Title"));
    assert_eq!(
        plan.final_snapshot.presence,
        SourcePresence {
            revision: 1,
            incarnation: 1,
            public_revision: Some(1)
        }
    );
    assert_eq!(native_revision(&plan.final_snapshot), 1);
    assert_eq!(attribution_revision(&plan.final_snapshot), 1);
    let view = public_nfo_document(&plan.final_snapshot).unwrap().unwrap();
    assert_eq!(view.revision, 1);
    assert_eq!(view.contribution, values("Title"));
    assert!(
        plan.final_snapshot
            .head
            .attribution
            .unwrap()
            .origin
            .is_none()
    );
}

#[test]
fn unchanged_refresh_has_no_document_presence_attribution_or_aggregate_write() {
    let snapshot = present();
    let plan = apply(&snapshot, publish("Title", "file-a"));
    assert_eq!(plan.effect, PresenceEffect::Unchanged);
    assert_eq!(plan.final_snapshot, snapshot);
}

#[test]
fn changed_values_or_external_identity_advance_both_document_heads() {
    let snapshot = present();
    let changed = apply(&snapshot, publish("Changed", "file-a")).final_snapshot;
    assert_eq!(changed.presence.public_revision, Some(2));
    assert_eq!(changed.presence.incarnation, 1);
    assert_eq!(native_revision(&changed), 2);
    assert_eq!(attribution_revision(&changed), 2);
    assert_eq!(changed.aggregate_revision, snapshot.aggregate_revision + 1);
    let mut identity = publish("Changed", "file-a");
    if let PresenceChange::Publish {
        external_identity, ..
    } = &mut identity
    {
        *external_identity = None;
    }
    let changed = apply(&changed, identity).final_snapshot;
    assert_eq!(changed.presence.public_revision, Some(3));
    assert_eq!(native_revision(&changed), 3);
    assert_eq!(
        public_nfo_document(&changed)
            .unwrap()
            .unwrap()
            .external_identity,
        None
    );
}

#[test]
fn put_identical_always_advances_but_retains_origin() {
    let snapshot = present();
    let plan = apply(&snapshot, put(1, "Title"));
    assert_eq!(plan.final_snapshot.presence.public_revision, Some(2));
    assert_eq!(native_revision(&plan.final_snapshot), 2);
    assert_eq!(
        plan.final_snapshot
            .head
            .attribution
            .as_ref()
            .unwrap()
            .origin,
        Some(origin("file-a"))
    );
    assert!(matches!(
        plan.effect,
        PresenceEffect::Changed {
            document: DocumentEffect::Replace { .. },
            presence: PresenceHeadEffect::Replace { .. },
            ..
        }
    ));
}

#[test]
fn withdrawal_hides_get_reappearance_restarts_public_only() {
    let snapshot = apply(&present(), publish("Changed", "file-a")).final_snapshot;
    let withdrawn = apply(&snapshot, PresenceChange::Withdraw).final_snapshot;
    assert_eq!(public_nfo_document(&withdrawn).unwrap(), None);
    assert_eq!(
        withdrawn.presence,
        SourcePresence {
            revision: 3,
            incarnation: 1,
            public_revision: None
        }
    );
    assert_eq!(native_revision(&withdrawn), 3);
    assert_eq!(
        withdrawn.head.source.as_ref().unwrap().document,
        DocumentState::Withdrawn
    );
    assert_eq!(
        apply(&withdrawn, PresenceChange::Withdraw).effect,
        PresenceEffect::Unchanged
    );
    let reappeared = apply(&withdrawn, publish("Changed", "file-b")).final_snapshot;
    assert_eq!(
        reappeared.presence,
        SourcePresence {
            revision: 4,
            incarnation: 2,
            public_revision: Some(1)
        }
    );
    assert_eq!(native_revision(&reappeared), 4);
    assert_eq!(attribution_revision(&reappeared), 4);
    assert_eq!(
        public_nfo_document(&reappeared).unwrap().unwrap().revision,
        1
    );
}

#[test]
fn withdrawn_put_requires_zero_not_retained_native_revision() {
    let withdrawn = apply(&present(), PresenceChange::Withdraw).final_snapshot;
    assert_eq!(
        plan_presence(&withdrawn, &request(&withdrawn, put(2, "Title"))),
        Err(PresenceError::StalePublicRevision {
            expected: 2,
            actual: 0
        })
    );
    let restored = apply(&withdrawn, put(0, "Title")).final_snapshot;
    assert_eq!(native_revision(&restored), 3);
    assert_eq!(restored.presence.public_revision, Some(1));
    assert_eq!(restored.presence.incarnation, 2);
    assert_eq!(restored.head.attribution.as_ref().unwrap().origin, None);
}

#[test]
fn captured_old_presence_cas_cannot_be_rebound_after_public_revision_reuse() {
    let first = present();
    let old_request = request(&first, put(1, "Old request"));
    let absent = apply(&first, PresenceChange::Withdraw).final_snapshot;
    let second = apply(&absent, publish("Title", "file-b")).final_snapshot;
    assert_eq!(
        first.presence.public_revision,
        second.presence.public_revision
    );
    assert_eq!(
        plan_presence(&second, &old_request),
        Err(PresenceError::StaleAggregate)
    );
    let mut partly_refenced = old_request.clone();
    partly_refenced.expected.aggregate_revision = second.aggregate_revision;
    assert_eq!(
        plan_presence(&second, &partly_refenced),
        Err(PresenceError::StaleSource)
    );
    partly_refenced.expected.head = second.head.stamp();
    assert_eq!(
        plan_presence(&second, &partly_refenced),
        Err(PresenceError::StalePresence)
    );
    // This is intentionally a DIFFERENT, newly admitted operation. Legacy wire
    // revision 1 alone cannot distinguish its client from the old incarnation.
    assert!(plan_presence(&second, &request(&second, put(1, "Newly admitted"))).is_ok());
}

#[test]
fn origin_only_move_advances_attribution_aggregate_not_document_or_presence() {
    let snapshot = present();
    let plan = apply(&snapshot, publish("Title", "file-b"));
    assert_eq!(plan.final_snapshot.head.source, snapshot.head.source);
    assert_eq!(plan.final_snapshot.presence, snapshot.presence);
    assert_eq!(
        plan.final_snapshot.aggregate_revision,
        snapshot.aggregate_revision + 1
    );
    assert_eq!(attribution_revision(&plan.final_snapshot), 2);
    assert_eq!(
        plan.final_snapshot
            .head
            .attribution
            .as_ref()
            .unwrap()
            .document_revision,
        1
    );
    assert!(matches!(
        plan.effect,
        PresenceEffect::Changed {
            document: DocumentEffect::Retain,
            presence: PresenceHeadEffect::Retain,
            ..
        }
    ));
}

#[test]
fn retired_release_retains_public_native_document_and_releases_claim_only() {
    let mut snapshot = present();
    snapshot.head.subject_state = SubjectState::Retired {
        item_revision: 4,
        retirement_revision: 1,
    };
    let plan = apply(&snapshot, PresenceChange::ReleaseRetiredAttribution);
    assert_eq!(plan.final_snapshot.head.source, snapshot.head.source);
    assert_eq!(plan.final_snapshot.presence, snapshot.presence);
    let attribution = plan.final_snapshot.head.attribution.as_ref().unwrap();
    assert_eq!(attribution.origin, None);
    assert_eq!(attribution.active_identity, None);
    assert_eq!(
        public_nfo_document(&plan.final_snapshot)
            .unwrap()
            .unwrap()
            .external_identity,
        Some("imdb:tt1".into())
    );
    assert!(matches!(
        plan.effect,
        PresenceEffect::Changed {
            document: DocumentEffect::Retain,
            presence: PresenceHeadEffect::Retain,
            ..
        }
    ));
    assert_eq!(
        apply(
            &plan.final_snapshot,
            PresenceChange::ReleaseRetiredAttribution
        )
        .effect,
        PresenceEffect::Unchanged
    );
    for change in [
        publish("Title", "file-a"),
        put(1, "Title"),
        PresenceChange::Withdraw,
    ] {
        assert_eq!(
            plan_presence(&snapshot, &request(&snapshot, change)),
            Err(PresenceError::RetiredTarget)
        );
    }
}

#[test]
fn exact_independent_stamps_fence_attribution_pin_schema_item_and_restore() {
    let snapshot = present();
    let original = request(&snapshot, publish("Title", "file-a"));
    let mut changed = snapshot.clone();
    changed.head.attribution.as_mut().unwrap().revision += 1;
    assert_eq!(
        plan_presence(&changed, &original),
        Err(PresenceError::StaleAttribution)
    );
    let mut changed = snapshot.clone();
    changed.head.pin = PinHead::Decision {
        revision: 1,
        value: Some("imdb:tt1".into()),
    };
    assert_eq!(
        plan_presence(&changed, &original),
        Err(PresenceError::StaleHead)
    );
    let mut changed = snapshot.clone();
    changed.head.schema_fingerprint = Some("e".repeat(64));
    assert_eq!(
        plan_presence(&changed, &original),
        Err(PresenceError::StaleHead)
    );
    let mut changed = snapshot.clone();
    changed.head.subject_state = SubjectState::Active { item_revision: 2 };
    assert_eq!(
        plan_presence(&changed, &original),
        Err(PresenceError::StaleHead)
    );
    let mut changed = snapshot.clone();
    changed.fence.incarnation = "restored".into();
    assert_eq!(
        plan_presence(&changed, &original),
        Err(PresenceError::StaleFence)
    );
    let mut changed = snapshot.clone();
    changed.fence.generation += 1;
    assert_eq!(
        plan_presence(&changed, &original),
        Err(PresenceError::StaleFence)
    );
    let mut changed = snapshot;
    changed.subject = "other-work".into();
    assert_eq!(
        plan_presence(&changed, &original),
        Err(PresenceError::StaleSubject)
    );
}

#[test]
fn restart_retains_exact_request_recompute_is_deterministic_not_receipt_replay() {
    let snapshot = present();
    let input = request(&snapshot, publish("Changed", "file-a"));
    let snapshot_again: PresenceSnapshot =
        serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
    let input_again: PresenceRequest =
        serde_json::from_str(&serde_json::to_string(&input).unwrap()).unwrap();
    let plan = plan_presence(&snapshot, &input).unwrap();
    assert_eq!(plan_presence(&snapshot_again, &input_again).unwrap(), plan);
    assert_eq!(
        plan_presence(&plan.final_snapshot, &input_again),
        Err(PresenceError::StaleAggregate)
    );
    // An adapter must recover the original receipt, not treat this refusal as
    // permission to recapture stamps and create a second native history row.
    assert_eq!(native_revision(&plan.final_snapshot), 2);
}

#[test]
fn imported_public_revision_is_independent_of_native_baseline_revision() {
    let mut snapshot = present();
    snapshot.presence.public_revision = Some(17);
    assert_eq!(
        public_nfo_document(&snapshot).unwrap().unwrap().revision,
        17
    );
    let next = apply(&snapshot, put(17, "Changed")).final_snapshot;
    assert_eq!(next.presence.public_revision, Some(18));
    assert_eq!(native_revision(&next), 2);
}

#[test]
fn empty_present_document_is_not_absent() {
    let change = PresenceChange::PublicPut {
        expected_public_revision: 0,
        contribution: Contribution::default(),
        external_identity: None,
    };
    let snapshot = apply(&empty(), change).final_snapshot;
    let public = public_nfo_document(&snapshot).unwrap().unwrap();
    assert_eq!(public.revision, 1);
    assert_eq!(public.contribution, Contribution::default());
    assert_eq!(public.external_identity, None);
}

#[test]
fn exhausted_heads_refuse_atomically_while_identical_refresh_can_still_noop() {
    for counter in 0..6 {
        let mut snapshot = present();
        match counter {
            0 => snapshot.aggregate_revision = i64::MAX,
            1 => {
                snapshot.head.source.as_mut().unwrap().revision = i64::MAX;
                snapshot
                    .head
                    .attribution
                    .as_mut()
                    .unwrap()
                    .document_revision = i64::MAX;
            }
            2 => snapshot.head.attribution.as_mut().unwrap().revision = i64::MAX,
            3 => snapshot.presence.revision = i64::MAX,
            4 => snapshot.presence.public_revision = Some(i64::MAX),
            5 => {
                snapshot.presence.revision = i64::MAX;
                snapshot.presence.incarnation = i64::MAX;
            }
            _ => unreachable!(),
        }
        let before = snapshot.clone();
        assert_eq!(
            plan_presence(&snapshot, &request(&snapshot, publish("Changed", "file-a"))),
            Err(PresenceError::RevisionExhausted)
        );
        assert_eq!(snapshot, before);
        assert_eq!(
            apply(&snapshot, publish("Title", "file-a")).effect,
            PresenceEffect::Unchanged
        );
    }
}

#[test]
fn malformed_persisted_presence_and_history_mapping_refuse() {
    for case in 0..9 {
        let mut snapshot = present();
        match case {
            0 => snapshot.presence.public_revision = None,
            1 => snapshot.presence.public_revision = Some(0),
            2 => snapshot.presence.incarnation = 0,
            3 => snapshot.presence.revision = 0,
            4 => snapshot.presence.incarnation = 2,
            5 => {
                snapshot
                    .head
                    .attribution
                    .as_mut()
                    .unwrap()
                    .document_revision = 2
            }
            6 => snapshot.head.attribution = None,
            7 => snapshot.head.source.as_mut().unwrap().revision = 0,
            8 => snapshot.head.attribution.as_mut().unwrap().active_identity = None,
            _ => unreachable!(),
        }
        assert_eq!(
            public_nfo_document(&snapshot),
            Err(PresenceError::InvalidSnapshot)
        );
    }
    let mut empty = empty();
    empty.presence.public_revision = Some(1);
    assert_eq!(
        public_nfo_document(&empty),
        Err(PresenceError::InvalidSnapshot)
    );
}

#[test]
fn contribution_field_name_document_node_and_depth_bounds_are_not_bypassed() {
    let snapshot = empty();
    let valid = Contribution {
        fields: (0..100)
            .map(|i| (format!("{i:03}{}", "x".repeat(97)), json!(i)))
            .collect(),
        tags: BTreeSet::new(),
    };
    let change = |value| PresenceChange::PublicPut {
        expected_public_revision: 0,
        contribution: value,
        external_identity: None,
    };
    assert!(plan_presence(&snapshot, &request(&snapshot, change(valid.clone()))).is_ok());
    let mut too_many = valid.clone();
    too_many.fields.insert("extra".into(), json!(1));
    assert!(plan_presence(&snapshot, &request(&snapshot, change(too_many))).is_err());
    let mut long_name = Contribution::default();
    long_name.fields.insert("x".repeat(101), json!(1));
    assert!(plan_presence(&snapshot, &request(&snapshot, change(long_name))).is_err());
    let mut huge = Contribution::default();
    huge.fields.insert(
        "extension".into(),
        json!("x".repeat(MAX_METADATA_DOCUMENT_BYTES)),
    );
    assert!(plan_presence(&snapshot, &request(&snapshot, change(huge))).is_err());
    let mut nodes = Contribution::default();
    nodes
        .fields
        .insert("extension".into(), json!(vec![0; MAX_METADATA_JSON_NODES]));
    assert!(plan_presence(&snapshot, &request(&snapshot, change(nodes))).is_err());
    let mut deep = json!(0);
    for _ in 0..MAX_METADATA_JSON_DEPTH {
        deep = json!([deep]);
    }
    let mut document = Contribution::default();
    document.fields.insert("extension".into(), deep);
    assert!(plan_presence(&snapshot, &request(&snapshot, change(document))).is_err());
}

#[test]
fn whole_input_node_budget_includes_retained_and_incoming_documents() {
    let mut snapshot = present();
    if let DocumentState::Present { contribution, .. } =
        &mut snapshot.head.source.as_mut().unwrap().document
    {
        contribution.fields = [("extension".into(), json!(vec![0; 4200]))].into();
    }
    assert!(public_nfo_document(&snapshot).is_ok());
    let mut incoming = values("Changed");
    incoming.fields = [("extension".into(), json!(vec![1; 4200]))].into();
    let input = request(
        &snapshot,
        PresenceChange::PublicPut {
            expected_public_revision: 1,
            contribution: incoming,
            external_identity: None,
        },
    );
    assert!(matches!(
        plan_presence(&snapshot, &input),
        Err(PresenceError::Bounds(RefreshError::Metadata(
            MetadataError::JsonNodeBudget
        )))
    ));
}

#[test]
fn reserved_fields_bad_origin_contract_and_public_identity_bounds_refuse() {
    let mut snapshot = empty();
    snapshot.head.system_fields.insert("title".into());
    assert!(plan_presence(&snapshot, &request(&snapshot, put(0, "Title"))).is_err());
    let snapshot = empty();
    let mut change = publish("Title", "file-a");
    if let PresenceChange::Publish { origin, .. } = &mut change {
        origin.provenance = OriginProvenance::Imported {
            import_id: "import".into(),
            source_occurrence_id: "occurrence".into(),
            mapping_digest: "not-a-digest".into(),
        };
    }
    assert!(plan_presence(&snapshot, &request(&snapshot, change)).is_err());
    let mut input = request(&snapshot, put(0, "Title"));
    input.contract_version = 2;
    assert_eq!(
        plan_presence(&snapshot, &input),
        Err(PresenceError::UnsupportedContract)
    );
    let input = request(
        &snapshot,
        PresenceChange::PublicPut {
            expected_public_revision: 0,
            contribution: values("Title"),
            external_identity: Some("x".repeat(257)),
        },
    );
    assert_eq!(
        plan_presence(&snapshot, &input),
        Err(PresenceError::InvalidInput)
    );
    assert_eq!(
        plan_presence(&snapshot, &request(&snapshot, put(-1, "Title"))),
        Err(PresenceError::InvalidInput)
    );
}

#[test]
fn source_history_never_restarts_across_repeated_public_incarnations() {
    let mut snapshot = empty();
    let mut previous_source_revision = 0;
    for incarnation in 1..=12 {
        snapshot = apply(&snapshot, publish("Title", "file-a")).final_snapshot;
        assert_eq!(snapshot.presence.incarnation, incarnation);
        assert_eq!(snapshot.presence.public_revision, Some(1));
        assert_eq!(native_revision(&snapshot), previous_source_revision + 1);
        previous_source_revision = native_revision(&snapshot);
        let moved = apply(&snapshot, publish("Title", "file-b")).final_snapshot;
        assert_eq!(native_revision(&moved), previous_source_revision);
        snapshot = apply(&moved, PresenceChange::Withdraw).final_snapshot;
        assert_eq!(native_revision(&snapshot), previous_source_revision + 1);
        assert_eq!(snapshot.presence.public_revision, None);
        previous_source_revision = native_revision(&snapshot);
    }
    assert_eq!(previous_source_revision, 24);
    assert_eq!(snapshot.presence.revision, 24);
    assert_eq!(attribution_revision(&snapshot), 36);
}

#[test]
fn public_put_external_identity_is_opaque_and_never_normalized() {
    let change = PresenceChange::PublicPut {
        expected_public_revision: 0,
        contribution: values("Title"),
        external_identity: Some(" identity with spaces \n".into()),
    };
    let snapshot = apply(&empty(), change).final_snapshot;
    assert_eq!(
        public_nfo_document(&snapshot)
            .unwrap()
            .unwrap()
            .external_identity,
        Some(" identity with spaces \n".into())
    );
}
