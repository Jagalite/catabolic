// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
use super::*;
use crate::catalog_metadata::{
    ContributionKey, ContributionReplacement, FieldPolicy, FieldSchema, Normalization, SourceKind,
    ValueType,
};

fn trim_schema() -> FieldSchema {
    FieldSchema::new_v2(
        vec![(
            "title".into(),
            FieldPolicy {
                value_type: ValueType::String,
                nullable: false,
                normalization: Normalization::TrimString,
            },
        )],
        vec![],
    )
    .unwrap()
}

#[test]
fn classification_allows_normalization_before_exhausted_revision_arithmetic() {
    let schema = trim_schema();
    let file = file("a");
    let mut retained = head("Same", None, &file);
    retained.schema_fingerprint = Some(schema.fingerprint().into());
    retained.source.as_mut().unwrap().revision = i64::MAX;
    let attribution = retained.attribution.as_mut().unwrap();
    attribution.revision = i64::MAX;
    attribution.document_revision = i64::MAX;
    let (snapshot, mut request) = fixture(
        vec![file.clone()],
        [("target".into(), retained)].into(),
        vec![parsed(" Same ", None)],
    );
    let before = request.clone();
    assert_eq!(
        classify_refresh_v2(&snapshot, &request).unwrap(),
        vec![ObservationEligibilityV2 {
            file: file.file,
            disposition: EligibilityDispositionV2::Parsed,
        }]
    );
    assert_eq!(request, before);
    assert_eq!(
        plan_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::RevisionExhausted))
    );
    // The existing metadata constructor supplies normalization. Classification
    // neither duplicates it nor allocates a speculative source revision.
    let normalized = ContributionReplacement::from_json(
        &schema,
        ContributionKey::new("target", "motion.nfo", "document").unwrap(),
        SourceKind::Provider,
        i64::MAX,
        r#"{"title":" Same "}"#,
        "{}",
    )
    .unwrap();
    let SidecarObservation::Parsed { contribution, .. } = &mut request.observations[0].sidecar
    else {
        unreachable!()
    };
    contribution.fields = normalized.fields().clone();
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert!(plan.effects.is_empty());
    assert_eq!(plan.final_heads, snapshot.subjects);
    assert_eq!(plan.counts.processed, 1);
}

#[test]
fn classification_preserves_order_skips_and_pin_noop_without_typed_normalization() {
    let a = file("a");
    let c = file("c");
    let mut target = empty_head();
    target.pin = PinHead::Decision {
        revision: 9,
        value: Some("accepted".into()),
    };
    let mut prior = head("Retained", None, &c);
    prior.source.as_mut().unwrap().revision = i64::MAX;
    prior.attribution.as_mut().unwrap().document_revision = i64::MAX;
    let mut not_a_string = values("unused");
    not_a_string.fields.insert("title".into(), json!(42));
    let (mut snapshot, request) = fixture(
        vec![a.clone(), file("b"), c, file("d"), file("e")],
        [
            ("target".into(), target),
            ("prior-a".into(), head("Keep malformed", None, &a)),
            ("prior-c".into(), prior),
        ]
        .into(),
        vec![
            unusable(),
            SidecarObservation::Absent,
            SidecarObservation::Parsed {
                contribution: not_a_string,
                external_identity: None,
            },
            parsed("Admitted", Some("accepted")),
            parsed("Stale", None),
        ],
    );
    current(&mut snapshot.files[4]).exact_revision = "e".repeat(64);
    let eligibility = classify_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(
        eligibility.iter().map(|e| &e.file).collect::<Vec<_>>(),
        request
            .observations
            .iter()
            .map(|o| &o.expected_file.file)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        eligibility
            .into_iter()
            .map(|e| e.disposition)
            .collect::<Vec<_>>(),
        vec![
            EligibilityDispositionV2::Skipped {
                reason: SkipReason::Unusable {
                    detail: "malformed".into()
                }
            },
            EligibilityDispositionV2::Absent,
            EligibilityDispositionV2::TargetPinned {
                pin_revision: 9,
                value: "accepted".into()
            },
            EligibilityDispositionV2::Parsed,
            EligibilityDispositionV2::Skipped {
                reason: SkipReason::RevisionChanged
            },
        ]
    );
    // This typed value would refuse if the adapter normalized an ineligible
    // pin-mismatch row. Pure bounded input validation correctly still admits it.
    assert!(
        ContributionReplacement::from_json(
            &trim_schema(),
            ContributionKey::new("target", "motion.nfo", "document").unwrap(),
            SourceKind::Provider,
            0,
            r#"{"title":42}"#,
            "{}",
        )
        .is_err()
    );
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(plan.counts.processed, 3);
    assert_eq!(plan.counts.skipped, 2);
    assert_eq!(plan.final_heads["prior-a"], snapshot.subjects["prior-a"]);
    assert_eq!(plan.final_heads["prior-c"], snapshot.subjects["prior-c"]);
    assert!(matches!(
        &plan.observations[2].result,
        ObservationResultV2::Processed {
            disposition: ObservationDisposition::TargetPinned { .. },
            subjects,
        } if subjects.is_empty()
    ));
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(plan.effects[0].file, file("d").file);
}

#[test]
fn classification_never_turns_missing_authority_or_stale_heads_into_eligibility() {
    for reason in [
        MissingEvidence::Authorization,
        MissingEvidence::Ownership,
        MissingEvidence::SourceCustody,
        MissingEvidence::CompleteSelection,
        MissingEvidence::CompleteLookup,
    ] {
        let (mut snapshot, request) = fixture(
            vec![file("a"), file("b")],
            [("target".into(), empty_head())].into(),
            vec![parsed("First", None), unusable()],
        );
        snapshot.files[1].current = FileRevalidation::Unproven {
            reason: reason.clone(),
        };
        let expected = RefreshV2Error::UnprovenObservation {
            file: file("b").file,
            reason,
        };
        assert_eq!(
            classify_refresh_v2(&snapshot, &request),
            Err(expected.clone())
        );
        assert_eq!(plan_refresh_v2(&snapshot, &request), Err(expected));
    }
    let (snapshot, mut request) = fresh(unusable());
    request.expected_heads.get_mut("target").unwrap().pin = PinHead::Decision {
        revision: 2,
        value: None,
    };
    assert_eq!(
        classify_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::StaleHead {
            subject: "target".into()
        }))
    );
    request.expected_heads.clear();
    assert_eq!(
        classify_refresh_v2(&snapshot, &request).unwrap_err(),
        plan_refresh_v2(&snapshot, &request).unwrap_err()
    );
}

#[test]
fn classification_keeps_skipped_document_and_whole_closure_budgets() {
    let (mut snapshot, request) = fresh(parsed("Unused", None));
    current(&mut snapshot.files[0]).exact_revision = "e".repeat(64);
    let mut fields: Vec<BTreeMap<String, Value>> = vec![
        (0..101).map(|i| (format!("field-{i}"), json!(i))).collect(),
        [("x".repeat(101), json!(1))].into(),
        [(
            "title".into(),
            json!("x".repeat(MAX_METADATA_DOCUMENT_BYTES)),
        )]
        .into(),
    ];
    let mut nested = json!(0);
    for _ in 0..MAX_METADATA_JSON_DEPTH + 1 {
        nested = json!([nested]);
    }
    fields.push([("title".into(), nested)].into());
    for fields in fields {
        let mut request = request.clone();
        request.observations[0].sidecar = SidecarObservation::Parsed {
            contribution: Contribution {
                fields,
                tags: BTreeSet::new(),
            },
            external_identity: None,
        };
        assert_eq!(
            classify_refresh_v2(&snapshot, &request).unwrap_err(),
            plan_refresh_v2(&snapshot, &request).unwrap_err()
        );
    }
    snapshot
        .subjects
        .get_mut("target")
        .unwrap()
        .system_fields
        .insert("title".into());
    assert_eq!(
        classify_refresh_v2(&snapshot, &request),
        Err(core(RefreshError::Metadata(MetadataError::ReservedField)))
    );
    for (count, sidecar, expected) in [
        (
            36,
            parsed(&"x".repeat(60 * 1024), None),
            MetadataError::SnapshotBudget,
        ),
        (400, unusable(), MetadataError::JsonNodeBudget),
    ] {
        let files = (0..count).map(|i| file(&format!("file-{i:04}"))).collect();
        let (mut snapshot, request) = fixture(
            files,
            [("target".into(), empty_head())].into(),
            vec![sidecar; count],
        );
        for file in &mut snapshot.files {
            current(file).exact_revision = "e".repeat(64);
        }
        assert_eq!(
            classify_refresh_v2(&snapshot, &request),
            Err(core(RefreshError::Metadata(expected)))
        );
    }
}

#[test]
fn direct_partial_plan_is_allowed_but_metadata_job_refuses_before_effects() {
    let (snapshot, request) = fixture(
        vec![file("a"), file("b")],
        [("target".into(), empty_head())].into(),
        vec![parsed("New", None), unusable()],
    );
    let before = snapshot.clone();
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert!(!plan.effects.is_empty());
    assert_eq!(
        require_refresh_completion(RefreshCompletionMode::DirectRefresh, &plan.counts),
        Ok(RefreshProgress::Processed)
    );
    assert_eq!(
        require_refresh_completion(RefreshCompletionMode::MetadataJob, &plan.counts),
        Err(RefreshCompletionError::IncompleteJob {
            counts: plan.counts.clone()
        })
    );
    assert_eq!(snapshot, before);
}

#[test]
fn empty_and_all_skipped_are_explicit_zero_for_direct_and_refused_for_jobs() {
    for sidecars in [vec![], vec![unusable()]] {
        let files = sidecars
            .iter()
            .enumerate()
            .map(|(i, _)| file(&i.to_string()))
            .collect();
        let (snapshot, request) =
            fixture(files, [("target".into(), empty_head())].into(), sidecars);
        let plan = plan_refresh_v2(&snapshot, &request).unwrap();
        assert!(plan.effects.is_empty());
        assert_eq!(
            require_refresh_completion(RefreshCompletionMode::DirectRefresh, &plan.counts),
            Ok(RefreshProgress::NoObservationsProcessed)
        );
        assert_eq!(
            require_refresh_completion(RefreshCompletionMode::MetadataJob, &plan.counts),
            Err(RefreshCompletionError::IncompleteJob {
                counts: plan.counts.clone()
            })
        );
        assert_eq!(
            classify_refresh_v2(&snapshot, &request).unwrap().len(),
            plan.counts.observed
        );
    }
}

#[test]
fn job_completion_counts_pin_noop_and_unchanged_publication_as_processed() {
    let a = file("a");
    let mut target = head("Same", Some("accepted"), &a);
    target.pin = PinHead::Decision {
        revision: 8,
        value: Some("accepted".into()),
    };
    let (snapshot, request) = fixture(
        vec![a, file("b")],
        [("target".into(), target)].into(),
        vec![parsed("Same", Some("accepted")), parsed("Ignored", None)],
    );
    let plan = plan_refresh_v2(&snapshot, &request).unwrap();
    assert_eq!(
        plan.counts,
        RefreshCounts {
            observed: 2,
            processed: 2,
            skipped: 0
        }
    );
    assert!(plan.effects.is_empty());
    assert_eq!(
        require_refresh_completion(RefreshCompletionMode::MetadataJob, &plan.counts),
        Ok(RefreshProgress::Processed)
    );
}

#[test]
fn completion_rejects_mismatched_and_overflowing_counts_in_both_modes() {
    for mode in [
        RefreshCompletionMode::DirectRefresh,
        RefreshCompletionMode::MetadataJob,
    ] {
        for counts in [
            RefreshCounts {
                observed: 2,
                processed: 1,
                skipped: 0,
            },
            RefreshCounts {
                observed: 0,
                processed: 1,
                skipped: 0,
            },
            RefreshCounts {
                observed: 0,
                processed: usize::MAX,
                skipped: 1,
            },
            RefreshCounts {
                observed: usize::MAX,
                processed: usize::MAX,
                skipped: 1,
            },
        ] {
            assert_eq!(
                require_refresh_completion(mode, &counts),
                Err(RefreshCompletionError::InvalidCounts)
            );
        }
    }
}
