// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
use super::*;
use serde_json::json;

fn file(name: &str) -> FileSnapshot {
    FileSnapshot {
        order_key: name.into(),
        file: FileKey {
            profile: "profile".into(),
            file_id: format!("native-{name}"),
        },
        subject: "target".into(),
        source_id: "source".into(),
        relative_path: format!("{name}.mp4"),
        exact_revision: "a".repeat(64),
        binding_fingerprint: "b".repeat(64),
        ownership_fingerprint: "c".repeat(64),
    }
}
fn origin(file: &FileSnapshot) -> FileOrigin {
    FileOrigin {
        file: file.file.clone(),
        provenance: OriginProvenance::Observed {
            exact_revision: file.exact_revision.clone(),
            binding_fingerprint: file.binding_fingerprint.clone(),
            ownership_fingerprint: file.ownership_fingerprint.clone(),
        },
    }
}
fn values(title: &str) -> Contribution {
    Contribution {
        fields: [("title".into(), json!(title))].into(),
        tags: ["NFO".into()].into(),
    }
}
fn parsed(title: &str, identity: Option<&str>) -> SidecarObservation {
    SidecarObservation::Parsed {
        contribution: values(title),
        external_identity: identity.map(str::to_owned),
    }
}
fn empty_head() -> SubjectHead {
    SubjectHead {
        subject_state: SubjectState::Active { item_revision: 1 },
        schema_fingerprint: Some("d".repeat(64)),
        system_fields: BTreeSet::new(),
        source: None,
        attribution: None,
        pin: PinHead::NeverDecided,
    }
}
fn head(title: &str, identity: Option<&str>, file: Option<&FileSnapshot>) -> SubjectHead {
    SubjectHead {
        source: Some(DocumentHead {
            revision: 3,
            document: DocumentState::Present {
                contribution: values(title),
                external_identity: identity.map(str::to_owned),
            },
        }),
        attribution: Some(AttributionHead {
            revision: 8,
            document_revision: 3,
            origin: file.map(origin),
            active_identity: identity.map(str::to_owned),
        }),
        ..empty_head()
    }
}
fn fixture(
    files: Vec<FileSnapshot>,
    heads: BTreeMap<String, SubjectHead>,
    sidecars: Vec<SidecarObservation>,
) -> (RefreshSnapshot, RefreshRequest) {
    let fence = Fence {
        catalog_id: "catalog".into(),
        incarnation: "incarnation".into(),
        generation: 7,
    };
    let mut claims = BTreeMap::new();
    for (subject, head) in &heads {
        if let Some(value) = head
            .attribution
            .as_ref()
            .and_then(|a| a.active_identity.as_ref())
        {
            assert!(
                claims
                    .insert(
                        value.clone(),
                        IdentityClaim::Subject {
                            subject: subject.clone()
                        }
                    )
                    .is_none()
            );
        }
    }
    for sidecar in &sidecars {
        if let SidecarObservation::Parsed {
            external_identity: Some(value),
            ..
        } = sidecar
        {
            claims
                .entry(value.clone())
                .or_insert(IdentityClaim::Available);
        }
    }
    let request = RefreshRequest {
        contract_version: CONTRACT_VERSION,
        fence: fence.clone(),
        subject: "target".into(),
        expected_heads: heads
            .iter()
            .map(|(id, head)| (id.clone(), head.stamp()))
            .collect(),
        observations: files
            .iter()
            .cloned()
            .zip(sidecars)
            .map(|(expected_file, sidecar)| Observation {
                expected_file,
                sidecar,
            })
            .collect(),
    };
    (
        RefreshSnapshot {
            contract_version: CONTRACT_VERSION,
            fence,
            subject: "target".into(),
            files,
            subjects: heads,
            identity_claims: claims,
        },
        request,
    )
}
fn fresh(sidecar: SidecarObservation) -> (RefreshSnapshot, RefreshRequest) {
    fixture(
        vec![file("a")],
        [("target".into(), empty_head())].into(),
        vec![sidecar],
    )
}

#[test]
fn first_publication_records_exact_origin_and_claim_without_pin_effects() {
    let (snapshot, request) = fresh(parsed("New", Some("imdb:tt1")));
    let before = snapshot.clone();
    let plan = plan_refresh(&snapshot, &request).unwrap();
    assert_eq!(snapshot, before);
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(plan.effects[0].expected.source_revision, None);
    assert!(matches!(
        &plan.effects[0].document,
        DocumentEffect::Replace {
            head: DocumentHead { revision: 1, .. }
        }
    ));
    let head = &plan.final_heads["target"];
    assert_eq!(head.pin, PinHead::NeverDecided);
    assert_eq!(
        head.attribution.as_ref().unwrap().origin,
        Some(origin(&file("a")))
    );
    assert_eq!(
        head.attribution
            .as_ref()
            .unwrap()
            .active_identity
            .as_deref(),
        Some("imdb:tt1")
    );
    assert_eq!(
        serde_json::from_value::<RefreshPlan>(serde_json::to_value(&plan).unwrap()).unwrap(),
        plan
    );
}

#[test]
fn identical_document_is_noop_but_origin_only_move_has_separate_revision() {
    let a = file("a");
    let old = head("Same", Some("id"), Some(&a));
    let (snapshot, request) = fixture(
        vec![a.clone()],
        [("target".into(), old.clone())].into(),
        vec![parsed("Same", Some("id"))],
    );
    let plan = plan_refresh(&snapshot, &request).unwrap();
    assert!(plan.effects.is_empty());
    assert_eq!(plan.final_heads["target"], old);
    let b = file("b");
    let (snapshot, request) = fixture(
        vec![b.clone()],
        [("target".into(), old.clone())].into(),
        vec![parsed("Same", Some("id"))],
    );
    let plan = plan_refresh(&snapshot, &request).unwrap();
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(plan.effects[0].document, DocumentEffect::Retain);
    assert_eq!(plan.final_heads["target"].source, old.source);
    assert_eq!(plan.effects[0].attribution.revision, 9);
    assert_eq!(plan.effects[0].attribution.origin, Some(origin(&b)));
}

#[test]
fn target_pin_rejection_precedes_every_prior_owner_withdrawal() {
    let a = file("a");
    let mut target = head("Pinned", Some("accepted"), None);
    target.pin = PinHead::Decision {
        revision: 12,
        value: Some("accepted".into()),
    };
    let prior = head("Prior", Some("incoming"), Some(&a));
    let (snapshot, request) = fixture(
        vec![a],
        [("target".into(), target), ("prior".into(), prior)].into(),
        vec![parsed("Rejected", Some("incoming"))],
    );
    let plan = plan_refresh(&snapshot, &request).unwrap();
    assert!(plan.effects.is_empty());
    assert_eq!(plan.final_heads, snapshot.subjects);
    assert_eq!(
        plan.observations[0].disposition,
        ObservationDisposition::TargetPinned {
            pin_revision: 12,
            value: "accepted".into()
        }
    );
}

#[test]
fn allowed_target_pin_keeps_decision_and_can_refresh_values() {
    let mut target = head("Before", Some("id"), None);
    target.pin = PinHead::Decision {
        revision: 9,
        value: Some("id".into()),
    };
    let pin = target.pin.clone();
    let (snapshot, request) = fixture(
        vec![file("a")],
        [("target".into(), target)].into(),
        vec![parsed("After", Some("id"))],
    );
    let plan = plan_refresh(&snapshot, &request).unwrap();
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(plan.final_heads["target"].pin, pin);
    assert_eq!(
        plan.final_heads["target"].source.as_ref().unwrap().revision,
        4
    );
}

#[test]
fn prior_owners_are_independently_pinned_and_collision_retains_values() {
    let a = file("a");
    let mut held = head("Held", Some("incoming"), Some(&a));
    held.pin = PinHead::Decision {
        revision: 2,
        value: Some("incoming".into()),
    };
    let free = head("Free", Some("released"), Some(&a));
    let target = head("Old target", Some("old"), None);
    let (snapshot, request) = fixture(
        vec![a],
        [
            ("target".into(), target),
            ("held".into(), held.clone()),
            ("free".into(), free),
        ]
        .into(),
        vec![parsed("New evidence", Some("incoming"))],
    );
    let plan = plan_refresh(&snapshot, &request).unwrap();
    assert_eq!(plan.final_heads["held"], held);
    assert_eq!(
        plan.final_heads["free"].source.as_ref().unwrap().document,
        DocumentState::Withdrawn
    );
    assert_eq!(
        plan.final_heads["target"].source.as_ref().unwrap().document,
        DocumentState::Present {
            contribution: values("New evidence"),
            external_identity: None
        }
    );
    assert!(
        plan.final_heads["target"]
            .attribution
            .as_ref()
            .unwrap()
            .origin
            .is_some()
    );
    assert_eq!(
        plan.observations[0].disposition,
        ObservationDisposition::Parsed {
            identity: IdentityDecision::Collision {
                value: "incoming".into()
            }
        }
    );
    assert!(plan.observations[0].subjects.iter().any(|o| matches!(o, SubjectOutcome::Pinned { subject, pin_revision: 2, .. } if subject == "held")));
}

#[test]
fn split_release_happens_before_claim_and_retired_metadata_is_never_written() {
    for retired in [false, true] {
        let a = file("a");
        let mut prior = head("Historical", Some("same"), Some(&a));
        if retired {
            prior.subject_state = SubjectState::Retired {
                item_revision: 7,
                retirement_revision: 2,
            };
        }
        let original = prior.source.clone();
        let (snapshot, request) = fixture(
            vec![a],
            [("target".into(), empty_head()), ("prior".into(), prior)].into(),
            vec![parsed("Current", Some("same"))],
        );
        let plan = plan_refresh(&snapshot, &request).unwrap();
        assert_eq!(plan.effects.len(), 2);
        assert_eq!(plan.effects[0].subject, "prior");
        assert_eq!(plan.effects[1].subject, "target");
        assert_eq!(
            plan.final_heads["target"]
                .attribution
                .as_ref()
                .unwrap()
                .active_identity
                .as_deref(),
            Some("same")
        );
        assert!(
            plan.final_heads["prior"]
                .attribution
                .as_ref()
                .unwrap()
                .origin
                .is_none()
        );
        assert!(
            plan.final_heads["prior"]
                .attribution
                .as_ref()
                .unwrap()
                .active_identity
                .is_none()
        );
        if retired {
            assert_eq!(
                plan.effects[0].reason,
                EffectReason::ReleaseRetiredAttribution
            );
            assert_eq!(plan.effects[0].document, DocumentEffect::Retain);
            assert_eq!(plan.final_heads["prior"].source, original);
        } else {
            assert!(matches!(
                plan.effects[0].document,
                DocumentEffect::Replace { .. }
            ));
        }
    }
}

#[test]
fn absence_only_withdraws_matching_file_and_retains_revision_tombstone() {
    let a = file("a");
    let b = file("b");
    let old = head("Keep", Some("id"), Some(&a));
    let (snapshot, request) = fixture(
        vec![b],
        [("target".into(), old.clone())].into(),
        vec![SidecarObservation::Absent],
    );
    assert!(
        plan_refresh(&snapshot, &request)
            .unwrap()
            .effects
            .is_empty()
    );
    let (snapshot, request) = fixture(
        vec![a],
        [("target".into(), old)].into(),
        vec![SidecarObservation::Absent],
    );
    let plan = plan_refresh(&snapshot, &request).unwrap();
    let target = &plan.final_heads["target"];
    assert_eq!(target.source.as_ref().unwrap().revision, 4);
    assert_eq!(
        target.source.as_ref().unwrap().document,
        DocumentState::Withdrawn
    );
    assert_eq!(target.attribution.as_ref().unwrap().revision, 9);
    assert!(target.attribution.as_ref().unwrap().origin.is_none());
}

#[test]
fn ordered_observations_use_evolving_heads_and_a_different_absent_copy_cannot_erase() {
    let (snapshot, request) = fixture(
        vec![file("a"), file("b"), file("c")],
        [("target".into(), empty_head())].into(),
        vec![
            parsed("First", Some("first")),
            parsed("Last", Some("last")),
            SidecarObservation::Absent,
        ],
    );
    let plan = plan_refresh(&snapshot, &request).unwrap();
    assert_eq!(plan.effects.len(), 2);
    assert_eq!(plan.effects[1].expected.source_revision, Some(1));
    assert_eq!(plan.effects[1].expected.attribution_revision, Some(1));
    assert_eq!(
        plan.final_heads["target"].source.as_ref().unwrap().revision,
        2
    );
    assert_eq!(
        plan.final_heads["target"].source.as_ref().unwrap().document,
        DocumentState::Present {
            contribution: values("Last"),
            external_identity: Some("last".into())
        }
    );
}

#[test]
fn external_collision_is_opaque_and_explicit_pin_removal_is_not_an_active_pin() {
    let mut target = empty_head();
    target.pin = PinHead::Decision {
        revision: 3,
        value: None,
    };
    let (mut snapshot, request) = fixture(
        vec![file("a")],
        [("target".into(), target)].into(),
        vec![parsed("Evidence", Some("taken"))],
    );
    snapshot
        .identity_claims
        .insert("taken".into(), IdentityClaim::OutsideClosure);
    let plan = plan_refresh(&snapshot, &request).unwrap();
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(
        plan.final_heads["target"].pin,
        PinHead::Decision {
            revision: 3,
            value: None
        }
    );
    assert_eq!(
        plan.final_heads["target"].source.as_ref().unwrap().document,
        DocumentState::Present {
            contribution: values("Evidence"),
            external_identity: None
        }
    );
}

#[test]
fn empty_duplicate_reordered_missing_and_unusable_inputs_fail_without_mutation() {
    let (snapshot, request) = fixture(
        vec![file("a"), file("b")],
        [("target".into(), empty_head())].into(),
        vec![parsed("First", None), parsed("Second", None)],
    );
    let before = snapshot.clone();
    let mut bad = request.clone();
    bad.observations.clear();
    assert_eq!(
        plan_refresh(&snapshot, &bad),
        Err(RefreshError::EmptyObservations)
    );
    bad = request.clone();
    bad.observations.pop();
    assert_eq!(
        plan_refresh(&snapshot, &bad),
        Err(RefreshError::IncompleteObservations)
    );
    bad = request.clone();
    bad.observations.swap(0, 1);
    assert_eq!(plan_refresh(&snapshot, &bad), Err(RefreshError::StaleFile));
    bad = request.clone();
    bad.observations[1] = bad.observations[0].clone();
    assert_eq!(plan_refresh(&snapshot, &bad), Err(RefreshError::StaleFile));
    bad = request.clone();
    bad.observations[1].sidecar = SidecarObservation::Unusable {
        reason: "malformed".into(),
    };
    assert_eq!(
        plan_refresh(&snapshot, &bad),
        Err(RefreshError::UnusableObservation)
    );
    assert_eq!(snapshot, before);
}

#[test]
fn every_independent_head_and_file_fence_is_compared_exactly() {
    let (snapshot, request) = fresh(parsed("New", None));
    let mut bad = request.clone();
    bad.fence.generation += 1;
    assert_eq!(plan_refresh(&snapshot, &bad), Err(RefreshError::StaleFence));
    bad = request.clone();
    bad.observations[0].expected_file.binding_fingerprint = "e".repeat(64);
    assert_eq!(plan_refresh(&snapshot, &bad), Err(RefreshError::StaleFile));
    for field in ["source", "origin", "pin", "item", "schema"] {
        let mut bad = request.clone();
        let stamp = bad.expected_heads.get_mut("target").unwrap();
        match field {
            "source" => stamp.source_revision = Some(1),
            "origin" => stamp.attribution_revision = Some(1),
            "pin" => {
                stamp.pin = PinHead::Decision {
                    revision: 1,
                    value: None,
                }
            }
            "item" => stamp.subject_state = SubjectState::Active { item_revision: 2 },
            _ => stamp.schema_fingerprint = Some("e".repeat(64)),
        }
        assert_eq!(
            plan_refresh(&snapshot, &bad),
            Err(RefreshError::StaleHead {
                subject: "target".into()
            })
        );
    }
}

#[test]
fn missing_identity_absence_proof_extra_scope_and_retired_target_are_rejected() {
    let (mut snapshot, request) = fresh(parsed("New", Some("id")));
    snapshot.identity_claims.clear();
    assert_eq!(
        plan_refresh(&snapshot, &request),
        Err(RefreshError::InvalidSnapshot)
    );
    let (mut snapshot, mut request) = fresh(parsed("New", None));
    snapshot.subjects.insert("unrelated".into(), empty_head());
    request
        .expected_heads
        .insert("unrelated".into(), empty_head().stamp());
    assert_eq!(
        plan_refresh(&snapshot, &request),
        Err(RefreshError::InvalidSnapshot)
    );
    let (mut snapshot, mut request) = fresh(parsed("New", None));
    snapshot.subjects.get_mut("target").unwrap().subject_state = SubjectState::Retired {
        item_revision: 2,
        retirement_revision: 1,
    };
    request
        .expected_heads
        .insert("target".into(), snapshot.subjects["target"].stamp());
    assert_eq!(
        plan_refresh(&snapshot, &request),
        Err(RefreshError::RetiredTarget)
    );
}

#[test]
fn imported_provenance_is_preserved_until_an_actual_origin_change() {
    let a = file("a");
    let mut target = head("Old", None, Some(&a));
    target
        .attribution
        .as_mut()
        .unwrap()
        .origin
        .as_mut()
        .unwrap()
        .provenance = OriginProvenance::Imported {
        import_id: "import".into(),
        source_occurrence_id: "motion-file".into(),
        mapping_digest: "e".repeat(64),
    };
    let original = target.clone();
    let (snapshot, request) = fixture(
        vec![file("b")],
        [("target".into(), target)].into(),
        vec![SidecarObservation::Absent],
    );
    let plan = plan_refresh(&snapshot, &request).unwrap();
    assert_eq!(plan.final_heads["target"], original);
}

#[test]
fn explicit_field_name_document_tag_and_reserved_bounds_are_not_relaxed() {
    let mut document = Contribution::default();
    document.fields = (0..100).map(|n| (format!("field_{n}"), json!(n))).collect();
    document.fields.remove("field_0");
    document.fields.insert("é".repeat(50), json!(0));
    contribution(&document, &BTreeSet::new()).unwrap();
    document.fields.insert("extra".into(), json!(1));
    assert_eq!(
        contribution(&document, &BTreeSet::new()),
        Err(RefreshError::Metadata(MetadataError::FieldBudget))
    );
    document.fields.remove("extra");
    document.fields.remove(&"é".repeat(50));
    document.fields.insert("é".repeat(51), json!(0));
    assert_eq!(
        contribution(&document, &BTreeSet::new()),
        Err(RefreshError::Metadata(MetadataError::InvalidField))
    );
    let mut document = values("x");
    assert_eq!(
        contribution(&document, &["title".into()].into()),
        Err(RefreshError::Metadata(MetadataError::ReservedField))
    );
    document.tags = (0..101).map(|i| format!("tag-{i}")).collect();
    assert_eq!(
        contribution(&document, &BTreeSet::new()),
        Err(RefreshError::InvalidInput)
    );
    document.tags.clear();
    let overhead = serde_json::to_string(&json!({"title":""})).unwrap().len();
    document.fields.insert(
        "title".into(),
        json!("x".repeat(MAX_METADATA_DOCUMENT_BYTES - overhead)),
    );
    contribution(&document, &BTreeSet::new()).unwrap();
    document.fields.insert(
        "title".into(),
        json!("x".repeat(MAX_METADATA_DOCUMENT_BYTES - overhead + 1)),
    );
    assert_eq!(
        contribution(&document, &BTreeSet::new()),
        Err(RefreshError::Metadata(MetadataError::DocumentBudget))
    );
}

#[test]
fn exact_closure_byte_node_and_work_boundaries_fail_closed() {
    let mut budget = SizeBudget::default();
    budget
        .charge(&Value::Array(vec![
            Value::Null;
            MAX_METADATA_JSON_NODES - 1
        ]))
        .unwrap();
    assert_eq!(budget.nodes, MAX_METADATA_JSON_NODES);
    assert_eq!(
        budget.charge(&Value::Null),
        Err(RefreshError::Metadata(MetadataError::JsonNodeBudget))
    );
    let mut budget = SizeBudget::default();
    budget
        .charge(&"x".repeat(MAX_METADATA_SNAPSHOT_BYTES - 2))
        .unwrap();
    assert_eq!(budget.bytes, MAX_METADATA_SNAPSHOT_BYTES);
    assert_eq!(
        budget.charge(&Value::Null),
        Err(RefreshError::Metadata(MetadataError::SnapshotBudget))
    );
    let mut planner = Planner {
        heads: BTreeMap::new(),
        claims: BTreeMap::new(),
        origins: BTreeMap::new(),
        effects: vec![],
        output: SizeBudget::default(),
        work: 0,
    };
    for _ in 0..MAX_METADATA_JSON_NODES {
        planner.step().unwrap();
    }
    assert_eq!(planner.step(), Err(RefreshError::WorkBudget));
}

#[test]
fn aggregate_node_budget_applies_across_individually_valid_documents() {
    let mut document = Contribution::default();
    document.fields.insert("items".into(), json!(vec![0; 4100]));
    contribution(&document, &BTreeSet::new()).unwrap();
    let (snapshot, request) = fixture(
        vec![file("a"), file("b")],
        [("target".into(), empty_head())].into(),
        vec![
            SidecarObservation::Parsed {
                contribution: document.clone(),
                external_identity: None,
            },
            SidecarObservation::Parsed {
                contribution: document,
                external_identity: None,
            },
        ],
    );
    assert_eq!(
        plan_refresh(&snapshot, &request),
        Err(RefreshError::Metadata(MetadataError::JsonNodeBudget))
    );
}

#[test]
fn source_or_attribution_revision_exhaustion_returns_no_partial_plan() {
    for source in [true, false] {
        let mut target = head("Old", None, Some(&file("a")));
        if source {
            target.source.as_mut().unwrap().revision = i64::MAX;
            target.attribution.as_mut().unwrap().document_revision = i64::MAX;
        } else {
            target.attribution.as_mut().unwrap().revision = i64::MAX;
        }
        let (snapshot, request) = fixture(
            vec![file("a")],
            [("target".into(), target)].into(),
            vec![parsed("New", None)],
        );
        let before = snapshot.clone();
        assert_eq!(
            plan_refresh(&snapshot, &request),
            Err(RefreshError::RevisionExhausted)
        );
        assert_eq!(snapshot, before);
    }
}

#[test]
fn late_failure_discards_provisional_prior_owner_withdrawal() {
    let a = file("a");
    let mut target = head("Old target", None, None);
    target.source.as_mut().unwrap().revision = i64::MAX;
    target.attribution.as_mut().unwrap().document_revision = i64::MAX;
    let prior = head("Prior", Some("incoming"), Some(&a));
    let (snapshot, request) = fixture(
        vec![a],
        [("target".into(), target), ("prior".into(), prior)].into(),
        vec![parsed("New", Some("incoming"))],
    );
    let before = snapshot.clone();
    assert_eq!(
        plan_refresh(&snapshot, &request),
        Err(RefreshError::RevisionExhausted)
    );
    assert_eq!(snapshot, before);
}
