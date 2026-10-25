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
fn unusable() -> SidecarObservation {
    SidecarObservation::Unusable {
        reason: "malformed".into(),
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
fn head(title: &str, identity: Option<&str>, file: &FileSnapshot) -> SubjectHead {
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
            origin: Some(origin(file)),
            active_identity: identity.map(str::to_owned),
        }),
        ..empty_head()
    }
}
fn fixture(
    files: Vec<FileSnapshot>,
    heads: BTreeMap<String, SubjectHead>,
    sidecars: Vec<SidecarObservation>,
) -> (RefreshSnapshotV2, RefreshRequestV2) {
    assert_eq!(files.len(), sidecars.len());
    let fence = Fence {
        catalog_id: "catalog".into(),
        incarnation: "incarnation".into(),
        generation: 7,
    };
    let mut claims = BTreeMap::new();
    for (subject, head) in &heads {
        if let Some(identity) = head
            .attribution
            .as_ref()
            .and_then(|a| a.active_identity.as_ref())
        {
            assert!(
                claims
                    .insert(
                        identity.clone(),
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
            external_identity: Some(identity),
            ..
        } = sidecar
        {
            claims
                .entry(identity.clone())
                .or_insert(IdentityClaim::Available);
        }
    }
    let request = RefreshRequestV2 {
        contract_version: CONTRACT_VERSION_V2,
        fence: fence.clone(),
        subject: "target".into(),
        expected_heads: heads
            .iter()
            .map(|(name, head)| (name.clone(), head.stamp()))
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
    let snapshot = RefreshSnapshotV2 {
        contract_version: CONTRACT_VERSION_V2,
        fence,
        subject: "target".into(),
        files: files
            .into_iter()
            .map(|file| RevalidatedFile {
                current: FileRevalidation::Present {
                    current: file.clone(),
                    availability: FileAvailability::Available,
                    source_state: SourceState::Enabled,
                },
                observed_file: file,
            })
            .collect(),
        subjects: heads,
        identity_claims: claims,
    };
    (snapshot, request)
}
fn fresh(sidecar: SidecarObservation) -> (RefreshSnapshotV2, RefreshRequestV2) {
    fixture(
        vec![file("a")],
        [("target".into(), empty_head())].into(),
        vec![sidecar],
    )
}
fn current(file: &mut RevalidatedFile) -> &mut FileSnapshot {
    match &mut file.current {
        FileRevalidation::Present { current, .. } => current,
        _ => panic!("fixture requires present file"),
    }
}
fn skipped(reason: SkipReason) -> ObservationResultV2 {
    ObservationResultV2::Skipped { reason }
}
fn core(error: RefreshError) -> RefreshV2Error {
    RefreshV2Error::Core(error)
}

#[test]
fn mixed_partial_observations_preserve_skipped_origins_and_process_in_order() {
    let b = file("b");
    let c = file("c");
    let held_b = head("Keep B", Some("held-b"), &b);
    let held_c = head("Keep C", Some("held-c"), &c);
    let (mut snapshot, request) = fixture(
        vec![file("a"), b, c, file("d")],
        [
            ("target".into(), empty_head()),
            ("prior-b".into(), held_b.clone()),
            ("prior-c".into(), held_c.clone()),
        ]
        .into(),
        vec![
            parsed("First", Some("first")),
            unusable(),
            SidecarObservation::Absent,
            parsed("Last", Some("last")),
        ],
    );
    current(&mut snapshot.files[2]).exact_revision = "e".repeat(64);
    let before = snapshot.clone();
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(snapshot, before);
    assert_eq!(
        plan.counts,
        RefreshCounts {
            observed: 4,
            processed: 2,
            skipped: 2
        }
    );
    assert_eq!(plan.progress, RefreshProgress::Processed);
    assert_eq!(
        plan.observations
            .iter()
            .map(|o| &o.file)
            .collect::<Vec<_>>(),
        request
            .observations
            .iter()
            .map(|o| &o.expected_file.file)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        plan.observations[1].result,
        skipped(SkipReason::Unusable {
            detail: "malformed".into()
        })
    );
    assert_eq!(
        plan.observations[2].result,
        skipped(SkipReason::RevisionChanged)
    );
    assert_eq!(plan.final_heads["prior-b"], held_b);
    assert_eq!(plan.final_heads["prior-c"], held_c);
    assert_eq!(plan.effects.len(), 2);
    assert_eq!(plan.effects[0].file, file("a").file);
    assert_eq!(plan.effects[1].file, file("d").file);
    assert_eq!(plan.effects[1].expected.source_revision, Some(1));
    assert_eq!(
        plan.final_heads["target"].source.as_ref().unwrap(),
        &DocumentHead {
            revision: 2,
            document: DocumentState::Present {
                contribution: values("Last"),
                external_identity: Some("last".into())
            }
        }
    );
    assert_eq!(
        plan.final_heads["target"]
            .attribution
            .as_ref()
            .unwrap()
            .origin,
        Some(origin(&file("d")))
    );
}

#[test]
fn every_evidenced_stale_case_skips_without_withdrawing_even_an_absent_sidecar() {
    for cause in [
        "missing",
        "unavailable",
        "disabled",
        "subject",
        "ownership",
        "source",
        "binding",
        "path",
        "revision",
    ] {
        let a = file("a");
        let old = head("Retain", Some("identity"), &a);
        let (mut snapshot, request) = fixture(
            vec![a],
            [("target".into(), old.clone())].into(),
            vec![SidecarObservation::Absent],
        );
        let expected = match cause {
            "missing" => {
                snapshot.files[0].current = FileRevalidation::Missing {
                    lookup_digest: "e".repeat(64),
                };
                SkipReason::FileMissing
            }
            "unavailable" => {
                if let FileRevalidation::Present { availability, .. } =
                    &mut snapshot.files[0].current
                {
                    *availability = FileAvailability::Unavailable;
                }
                SkipReason::FileUnavailable
            }
            "disabled" => {
                if let FileRevalidation::Present { source_state, .. } =
                    &mut snapshot.files[0].current
                {
                    *source_state = SourceState::Disabled;
                }
                SkipReason::SourceDisabled
            }
            "subject" => {
                current(&mut snapshot.files[0]).subject = "another-work".into();
                SkipReason::OwnershipChanged
            }
            "ownership" => {
                current(&mut snapshot.files[0]).ownership_fingerprint = "e".repeat(64);
                SkipReason::OwnershipChanged
            }
            "source" => {
                current(&mut snapshot.files[0]).source_id = "another-source".into();
                SkipReason::BindingChanged
            }
            "binding" => {
                current(&mut snapshot.files[0]).binding_fingerprint = "e".repeat(64);
                SkipReason::BindingChanged
            }
            "path" => {
                current(&mut snapshot.files[0]).relative_path = "renamed.mp4".into();
                SkipReason::PathChanged
            }
            _ => {
                current(&mut snapshot.files[0]).exact_revision = "e".repeat(64);
                SkipReason::RevisionChanged
            }
        };
        let plan = plan_refresh_v2(&snapshot, &request).unwrap();
        assert_eq!(plan.observations[0].result, skipped(expected), "{cause}");
        assert_eq!(
            plan.counts,
            RefreshCounts {
                observed: 1,
                processed: 0,
                skipped: 1
            }
        );
        assert_eq!(plan.progress, RefreshProgress::NoObservationsProcessed);
        assert!(plan.effects.is_empty());
        assert_eq!(plan.final_heads["target"], old);
    }
}

#[test]
fn unusable_is_first_skip_but_never_bypasses_missing_evidence_refusal() {
    let (mut snapshot, request) = fresh(unusable());
    snapshot.files[0].current = FileRevalidation::Missing {
        lookup_digest: "e".repeat(64),
    };
    assert_eq!(
        plan_refresh_v2(&snapshot, &request).unwrap().observations[0].result,
        skipped(SkipReason::Unusable {
            detail: "malformed".into()
        })
    );
    for reason in [
        MissingEvidence::Authorization,
        MissingEvidence::Ownership,
        MissingEvidence::SourceCustody,
        MissingEvidence::CompleteSelection,
        MissingEvidence::CompleteLookup,
    ] {
        snapshot.files[0].current = FileRevalidation::Unproven {
            reason: reason.clone(),
        };
        assert_eq!(
            plan_refresh_v2(&snapshot, &request),
            Err(RefreshV2Error::UnprovenObservation {
                file: file("a").file,
                reason
            })
        );
    }
}

#[test]
fn later_unproven_row_refuses_whole_batch_instead_of_publishing_prefix() {
    let (mut snapshot, request) = fixture(
        vec![file("a"), file("b")],
        [("target".into(), empty_head())].into(),
        vec![parsed("Valid first", None), SidecarObservation::Absent],
    );
    snapshot.files[1].current = FileRevalidation::Unproven {
        reason: MissingEvidence::Ownership,
    };
    let before = snapshot.clone();
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(RefreshV2Error::UnprovenObservation {
            file: file("b").file,
            reason: MissingEvidence::Ownership
        })
    );
    assert_eq!(snapshot, before);
}

#[test]
fn manual_pin_mismatch_counts_processed_before_any_prior_withdrawal() {
    let a = file("a");
    let mut target = empty_head();
    target.pin = PinHead::Decision {
        revision: 12,
        value: Some("accepted".into()),
    };
    let prior = head("Prior", Some("incoming"), &a);
    let (snapshot, request) = fixture(
        vec![a],
        [("target".into(), target), ("prior".into(), prior)].into(),
        vec![parsed("No effect", Some("incoming"))],
    );
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(
        plan.counts,
        RefreshCounts {
            observed: 1,
            processed: 1,
            skipped: 0
        }
    );
    assert_eq!(plan.progress, RefreshProgress::Processed);
    assert_eq!(
        plan.observations[0].result,
        ObservationResultV2::Processed {
            disposition: ObservationDisposition::TargetPinned {
                pin_revision: 12,
                value: "accepted".into()
            },
            subjects: vec![],
        }
    );
    assert!(plan.effects.is_empty());
    assert_eq!(plan.final_heads, snapshot.subjects);
}

#[test]
fn all_skipped_and_empty_are_explicit_zero_not_refresh_success() {
    let (mut snapshot, request) = fixture(
        vec![file("a"), file("b")],
        [("target".into(), empty_head())].into(),
        vec![unusable(), parsed("Stale", None)],
    );
    current(&mut snapshot.files[1]).binding_fingerprint = "e".repeat(64);
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(
        plan.counts,
        RefreshCounts {
            observed: 2,
            processed: 0,
            skipped: 2
        }
    );
    assert_eq!(plan.progress, RefreshProgress::NoObservationsProcessed);
    assert!(plan.effects.is_empty());
    assert_eq!(plan.final_heads, snapshot.subjects);
    let (snapshot, request) = fixture(vec![], [("target".into(), empty_head())].into(), vec![]);
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(
        plan.counts,
        RefreshCounts {
            observed: 0,
            processed: 0,
            skipped: 0
        }
    );
    assert_eq!(plan.progress, RefreshProgress::NoObservationsProcessed);
    assert!(plan.observations.is_empty());
    assert!(plan.effects.is_empty());
}

#[test]
fn revalidated_absence_withdraws_but_respects_each_prior_pin_and_retired_document() {
    let a = file("a");
    let mut pinned_head = head("Pinned", Some("held"), &a);
    pinned_head.pin = PinHead::Decision {
        revision: 2,
        value: Some("held".into()),
    };
    let mut retired = head("Historical", Some("released"), &a);
    retired.subject_state = SubjectState::Retired {
        item_revision: 2,
        retirement_revision: 1,
    };
    let (snapshot, request) = fixture(
        vec![a],
        [
            ("target".into(), empty_head()),
            ("pinned".into(), pinned_head.clone()),
            ("retired".into(), retired.clone()),
        ]
        .into(),
        vec![SidecarObservation::Absent],
    );
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(plan.counts.processed, 1);
    assert_eq!(plan.final_heads["pinned"], pinned_head);
    assert_eq!(plan.final_heads["retired"].source, retired.source);
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(
        plan.effects[0].reason,
        EffectReason::ReleaseRetiredAttribution
    );
    assert_eq!(plan.effects[0].document, DocumentEffect::Retain);
    assert_eq!(plan.effects[0].attribution.origin, None);
    assert_eq!(plan.effects[0].attribution.active_identity, None);
}

#[test]
fn unchanged_valid_rows_count_without_revision_writes_and_collisions_keep_values() {
    let a = file("a");
    let old = head("Same", Some("id"), &a);
    let (snapshot, request) = fixture(
        vec![a],
        [("target".into(), old.clone())].into(),
        vec![parsed("Same", Some("id"))],
    );
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(plan.counts.processed, 1);
    assert!(plan.effects.is_empty());
    assert_eq!(plan.final_heads["target"], old);
    let (mut snapshot, request) = fresh(parsed("Keep evidence", Some("taken")));
    snapshot
        .identity_claims
        .insert("taken".into(), IdentityClaim::OutsideClosure);
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(
        plan.final_heads["target"].source.as_ref().unwrap().document,
        DocumentState::Present {
            contribution: values("Keep evidence"),
            external_identity: None
        }
    );
}

#[test]
fn exact_original_set_order_and_independent_head_fences_still_refuse() {
    let (snapshot, request) = fixture(
        vec![file("a"), file("b")],
        [("target".into(), empty_head())].into(),
        vec![unusable(), unusable()],
    );
    let mut bad = request.clone();
    bad.observations.pop();
    assert_eq!(
        plan_refresh_v2(&snapshot, &bad),
        Err(core(RefreshError::IncompleteObservations))
    );
    bad = request.clone();
    bad.observations.swap(0, 1);
    assert_eq!(
        plan_refresh_v2(&snapshot, &bad),
        Err(core(RefreshError::StaleFile))
    );
    bad = request.clone();
    bad.observations[1] = bad.observations[0].clone();
    assert_eq!(
        plan_refresh_v2(&snapshot, &bad),
        Err(core(RefreshError::StaleFile))
    );
    for field in ["source", "attribution", "pin", "item", "schema"] {
        let mut bad = request.clone();
        let stamp = bad.expected_heads.get_mut("target").unwrap();
        match field {
            "source" => stamp.source_revision = Some(1),
            "attribution" => stamp.attribution_revision = Some(1),
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
            plan_refresh_v2(&snapshot, &bad),
            Err(core(RefreshError::StaleHead {
                subject: "target".into()
            }))
        );
    }
    let mut bad = request.clone();
    bad.fence.generation += 1;
    assert_eq!(
        plan_refresh_v2(&snapshot, &bad),
        Err(core(RefreshError::StaleFence))
    );
}

#[test]
fn wrong_terminal_row_invalid_digest_and_incomplete_claims_are_not_stale_skips() {
    let (mut snapshot, request) = fresh(parsed("Evidence", Some("id")));
    current(&mut snapshot.files[0]).file.file_id = "different".into();
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::InvalidSnapshot))
    );
    let (mut snapshot, request) = fresh(unusable());
    current(&mut snapshot.files[0]).order_key = "different".into();
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::InvalidSnapshot))
    );
    snapshot.files[0].current = FileRevalidation::Missing {
        lookup_digest: "not-a-digest".into(),
    };
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::InvalidInput))
    );
    let (mut snapshot, request) = fresh(parsed("Stale evidence", Some("id")));
    current(&mut snapshot.files[0]).exact_revision = "e".repeat(64);
    snapshot.identity_claims.clear();
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::InvalidSnapshot))
    );
}

#[test]
fn skipped_parsed_documents_keep_field_name_byte_and_reserved_bounds() {
    let (mut snapshot, mut request) = fresh(parsed("unused", None));
    current(&mut snapshot.files[0]).exact_revision = "e".repeat(64);
    let mut document = Contribution::default();
    document.fields = (0..99).map(|i| (format!("field-{i}"), json!(i))).collect();
    document.fields.insert("é".repeat(50), json!(0));
    request.observations[0].sidecar = SidecarObservation::Parsed {
        contribution: document.clone(),
        external_identity: None,
    };
    assert_eq!(
        plan_refresh_v2(&snapshot, &request)
            .unwrap()
            .counts
            .processed,
        0
    );
    document.fields.insert("extra".into(), json!(1));
    request.observations[0].sidecar = SidecarObservation::Parsed {
        contribution: document,
        external_identity: None,
    };
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::Metadata(MetadataError::FieldBudget)))
    );
    let mut document = values("small");
    document.fields.insert("é".repeat(51), json!(1));
    request.observations[0].sidecar = SidecarObservation::Parsed {
        contribution: document,
        external_identity: None,
    };
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::Metadata(MetadataError::InvalidField)))
    );
    request.observations[0].sidecar = parsed(&"x".repeat(MAX_METADATA_DOCUMENT_BYTES), None);
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::Metadata(MetadataError::DocumentBudget)))
    );
    request.observations[0].sidecar = parsed("Reserved", None);
    snapshot
        .subjects
        .get_mut("target")
        .unwrap()
        .system_fields
        .insert("title".into());
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::Metadata(MetadataError::ReservedField)))
    );
}

#[test]
fn all_skipped_closure_still_charges_total_bytes_and_nodes_without_truncation() {
    let files: Vec<_> = (0..36).map(|i| file(&format!("file-{i:04}"))).collect();
    let sidecars = (0..files.len())
        .map(|_| parsed(&"x".repeat(60 * 1024), None))
        .collect();
    let (mut snapshot, request) =
        fixture(files, [("target".into(), empty_head())].into(), sidecars);
    for file in &mut snapshot.files {
        current(file).exact_revision = "e".repeat(64);
    }
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::Metadata(MetadataError::SnapshotBudget)))
    );
    let files: Vec<_> = (0..400).map(|i| file(&format!("file-{i:04}"))).collect();
    let sidecars = (0..files.len()).map(|_| unusable()).collect();
    let (snapshot, request) = fixture(files, [("target".into(), empty_head())].into(), sidecars);
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::Metadata(MetadataError::JsonNodeBudget)))
    );
}

#[test]
fn v2_skip_work_uses_same_exact_work_limit() {
    let (snapshot, _) = fresh(unusable());
    let common = RefreshSnapshot {
        contract_version: CONTRACT_VERSION,
        fence: snapshot.fence,
        subject: snapshot.subject,
        files: vec![file("a")],
        subjects: snapshot.subjects,
        identity_claims: snapshot.identity_claims,
    };
    let mut planner = Planner::new(&common);
    planner.work = MAX_METADATA_JSON_NODES - 1;
    planner.step().unwrap();
    assert_eq!(planner.step(), Err(RefreshError::WorkBudget));
}

#[test]
fn valid_v2_effects_equal_v1_and_v1_strict_version_empty_unusable_semantics_remain() {
    let (snapshot, request) = fresh(parsed("New", Some("id")));
    let mut v1_snapshot = RefreshSnapshot {
        contract_version: CONTRACT_VERSION,
        fence: snapshot.fence.clone(),
        subject: snapshot.subject.clone(),
        files: snapshot
            .files
            .iter()
            .map(|f| f.observed_file.clone())
            .collect(),
        subjects: snapshot.subjects.clone(),
        identity_claims: snapshot.identity_claims.clone(),
    };
    let mut v1_request = RefreshRequest {
        contract_version: CONTRACT_VERSION,
        fence: request.fence.clone(),
        subject: request.subject.clone(),
        expected_heads: request.expected_heads.clone(),
        observations: request.observations.clone(),
    };
    let v1 = plan_refresh(&v1_snapshot, &v1_request).unwrap();
    let v2 = plan_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(v1.effects, v2.effects);
    assert_eq!(v1.final_heads, v2.final_heads);
    assert_eq!(
        v2.observations[0].result,
        ObservationResultV2::Processed {
            disposition: v1.observations[0].disposition.clone(),
            subjects: v1.observations[0].subjects.clone()
        }
    );
    v1_request.observations[0].sidecar = unusable();
    assert_eq!(
        plan_refresh(&v1_snapshot, &v1_request),
        Err(RefreshError::UnusableObservation)
    );
    v1_request.observations.clear();
    v1_snapshot.files.clear();
    assert_eq!(
        plan_refresh(&v1_snapshot, &v1_request),
        Err(RefreshError::EmptyObservations)
    );
    v1_snapshot.contract_version = CONTRACT_VERSION_V2;
    assert_eq!(
        plan_refresh(&v1_snapshot, &v1_request),
        Err(RefreshError::UnsupportedContract)
    );
    let mut wrong = request;
    wrong.contract_version = CONTRACT_VERSION;
    assert_eq!(
        plan_refresh_v2(&snapshot, &wrong),
        Err(core(RefreshError::UnsupportedContract))
    );
}

#[test]
fn v2_wire_roundtrips_but_serialized_grants_and_unrecognized_proofs_are_rejected() {
    let (snapshot, request) = fresh(unusable());
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(
        serde_json::from_value::<RefreshSnapshotV2>(serde_json::to_value(&snapshot).unwrap())
            .unwrap(),
        snapshot
    );
    assert_eq!(
        serde_json::from_value::<RefreshRequestV2>(serde_json::to_value(&request).unwrap())
            .unwrap(),
        request
    );
    assert_eq!(
        serde_json::from_value::<RefreshPlanV2>(serde_json::to_value(&plan).unwrap()).unwrap(),
        plan
    );
    let mut bad = serde_json::to_value(&snapshot).unwrap();
    bad["authorization_grant"] = json!(true);
    assert!(serde_json::from_value::<RefreshSnapshotV2>(bad).is_err());
    let mut bad = serde_json::to_value(&snapshot).unwrap();
    bad["files"][0]["current"] = json!({"state":"missing"});
    assert!(serde_json::from_value::<RefreshSnapshotV2>(bad).is_err());
}

#[test]
fn skipped_prior_claim_still_blocks_later_identity_but_not_later_values() {
    let a = file("a");
    let prior = head("Retained", Some("shared-id"), &a);
    let (snapshot, request) = fixture(
        vec![a, file("b")],
        [
            ("target".into(), empty_head()),
            ("prior".into(), prior.clone()),
        ]
        .into(),
        vec![unusable(), parsed("Later values", Some("shared-id"))],
    );
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(
        plan.counts,
        RefreshCounts {
            observed: 2,
            processed: 1,
            skipped: 1
        }
    );
    assert_eq!(plan.final_heads["prior"], prior);
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(plan.effects[0].subject, "target");
    assert!(matches!(
        &plan.observations[1].result,
        ObservationResultV2::Processed {
            disposition: ObservationDisposition::Parsed { identity: IdentityDecision::Collision { value } }, ..
        } if value == "shared-id"
    ));
    assert_eq!(
        plan.final_heads["target"].source.as_ref().unwrap().document,
        DocumentState::Present {
            contribution: values("Later values"),
            external_identity: None
        }
    );
}

#[test]
fn oversized_nested_skipped_values_refuse_before_snapshot_serialization() {
    let (mut snapshot, mut request) = fresh(parsed("unused", None));
    snapshot.files[0].current = FileRevalidation::Missing {
        lookup_digest: "e".repeat(64),
    };
    let mut nested = Value::Null;
    for _ in 0..=MAX_METADATA_JSON_DEPTH {
        nested = Value::Array(vec![nested]);
    }
    request.observations[0].sidecar = SidecarObservation::Parsed {
        contribution: Contribution {
            fields: [("nested".into(), nested)].into(),
            tags: BTreeSet::new(),
        },
        external_identity: None,
    };
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::Metadata(MetadataError::JsonDepthBudget)))
    );
    request.observations[0].sidecar = SidecarObservation::Parsed {
        contribution: Contribution {
            fields: [(
                "nodes".into(),
                Value::Array(vec![Value::Null; MAX_METADATA_JSON_NODES]),
            )]
            .into(),
            tags: BTreeSet::new(),
        },
        external_identity: None,
    };
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::Metadata(MetadataError::JsonNodeBudget)))
    );
}

mod eligibility;
