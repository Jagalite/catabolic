// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent adversarial checks of the R1 production decisions.

use catabolic_core::{catalog_identity::*, component};
use serde_json::{Value, json};

fn range(start: i64, end: i64) -> IntervalUs {
    IntervalUs::new(start, end).unwrap()
}

fn version(id: &str) -> VersionContract {
    VersionContract {
        contract_version: 1,
        version_id: id.into(),
        association_item_id: format!("{id}-item"),
        edition_item_id: "edition".into(),
        timeline_id: "timeline".into(),
        state: VersionState::Ready,
        declared_coverage: Some(range(0, 20)),
        bindings: (1..=2)
            .map(|part| VersionBinding {
                binding_id: format!("{id}-binding-{part}"),
                association_id: format!("{id}-association-{part}"),
                file: ExactFileRevision {
                    file_id: format!("{id}-file-{part}"),
                    revision: format!("{id}-revision-{part}"),
                },
                part,
                source: SourceExtent::Slice {
                    interval: range(100, 110),
                },
                timeline_interval: Some(range(i64::from(part - 1) * 10, i64::from(part) * 10)),
            })
            .collect(),
    }
}

#[test]
fn normalization_matches_direct_integer_oracle_at_boundary_values() {
    // The independent oracle multiplies before division; test products fit u128.
    for start in [0_i64, 1, 9, 1_000_001, 9_007_199_254_740_991, i64::MAX - 1] {
        for numerator in [1_i64, 3, 999_983, 1_000_000] {
            for denominator in [1_i64, 3, 1_000_000, 1_000_003, i64::MAX] {
                let original = ToolSlice {
                    start_ticks: start,
                    end_ticks: start + 1,
                    time_base: ToolTimeBase {
                        numerator,
                        denominator,
                    },
                };
                let a = start as u128 * numerator as u128 * 1_000_000;
                let b = (start as u128 + 1) * numerator as u128 * 1_000_000;
                let den = denominator as u128;
                let expected_start = a.div_ceil(den);
                let expected_end = b / den;
                let actual = normalize_tool_slice(original);
                if expected_start > i64::MAX as u128 || expected_end > i64::MAX as u128 {
                    assert_eq!(
                        actual,
                        Err(IdentityError::NormalizationOverflow),
                        "{original:?}"
                    );
                } else if expected_start >= expected_end {
                    assert_eq!(
                        actual,
                        Err(IdentityError::EmptyNormalizedInterval),
                        "{original:?}"
                    );
                } else {
                    let actual = actual.unwrap();
                    assert_eq!(
                        actual.interval,
                        range(expected_start as i64, expected_end as i64)
                    );
                    assert_eq!(actual.start_rounded_up, !a.is_multiple_of(den));
                    assert_eq!(actual.end_rounded_down, !b.is_multiple_of(den));
                }
            }
        }
    }
}

#[test]
fn complete_version_selection_matches_all_two_by_two_availability_combinations() {
    let versions = [version("first"), version("second")];
    let evidence = EquivalenceEvidence {
        basis: EquivalenceBasis::Verified,
        evidence_id: "evidence".into(),
    };
    for first_mask in 0_u8..4 {
        for second_mask in 0_u8..4 {
            let available = [first_mask, second_mask]
                .into_iter()
                .enumerate()
                .map(|(v, mask)| {
                    versions[v]
                        .bindings
                        .iter()
                        .enumerate()
                        .filter(|(b, _)| mask & (1 << b) != 0)
                        .map(|(_, binding)| binding.binding_id.clone())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let candidates = (0..2)
                .map(|i| VersionCandidate {
                    version: &versions[i],
                    equivalence: Some(&evidence),
                    available_binding_ids: &available[i],
                })
                .collect::<Vec<_>>();
            let expected = if first_mask == 3 {
                Some(0)
            } else if second_mask == 3 {
                Some(1)
            } else {
                None
            };
            assert_eq!(
                select_complete_version("edition", "timeline", range(0, 20), &candidates),
                Ok(expected)
            );
        }
    }
}

#[test]
fn ready_coverage_cannot_be_truncated_reordered_or_speed_changed() {
    let complete = version("v");
    assert_eq!(validate_version(&complete), Ok(()));
    let mut invalid = complete.clone();
    invalid.bindings.pop();
    assert_eq!(
        validate_version(&invalid),
        Err(IdentityError::IncompleteCoverage)
    );
    invalid = complete.clone();
    invalid.bindings[0].timeline_interval = Some(range(10, 20));
    invalid.bindings[1].timeline_interval = Some(range(0, 10));
    assert_eq!(
        validate_version(&invalid),
        Err(IdentityError::IncompleteCoverage)
    );
    invalid = complete.clone();
    invalid.bindings[0].timeline_interval = Some(range(0, 9));
    assert_eq!(
        validate_version(&invalid),
        Err(IdentityError::DurationMismatch)
    );
}

fn component_inputs(version: &VersionContract) -> (Value, Value) {
    let binding = &version.bindings[0];
    let video = json!({"item_id":version.association_item_id,"part":binding.part,
        "file_id":binding.file.file_id,"revision":binding.file.revision,
        "locator":{"index":0},"stream_count":1,"codec":"h264","snapshot":{"path":"video.mkv"}});
    let primary = json!({"id":binding.association_id,"item_id":version.association_item_id,
        "file_id":binding.file.file_id,"part":binding.part,"metadata":{}});
    (video, primary)
}

#[test]
fn native_slice_is_blocked_for_every_legacy_effect_mode_even_with_approved_operation() {
    let version = version("v");
    let (video, primary) = component_inputs(&version);
    let relation =
        component::compatible_binding(&version, &version.bindings[0], &video, &video, &primary)
            .unwrap();
    assert_eq!(
        relation["source_interval"],
        json!({"start_us":100,"end_us":110})
    );
    assert_eq!(relation["coordinate_offset_us"], -100);
    for publication in ["selected_only", "container", "sidecars"] {
        for supported_codecs in [json!(["h264"]), json!(["av1"])] {
            let result = component::decision(
                &json!({"publication":publication,"operation_id":"approved",
                "supported_codecs":supported_codecs}),
                &[json!({"occurrence":video,"compatibility":relation})],
                &[],
            );
            assert_eq!(result["action"], "block");
            assert_eq!(result["ready"], false);
            assert_eq!(result["reason"], "native_binding_effect_unsupported");
            assert_eq!(result["exposes_unselected_components"], false);
        }
    }
    for compatibility in [
        json!({"binding_id":"v-binding-1","requires_interval_enforcement":false,"offset_seconds":0}),
        json!({"requires_interval_enforcement":true,"offset_seconds":0}),
    ] {
        assert_eq!(
            component::decision(
                &json!({"publication":"selected_only"}),
                &[json!({"occurrence":video,"compatibility":compatibility})],
                &[]
            )["action"],
            "block"
        );
    }
}

#[test]
fn scoped_segmentation_rejects_nested_overlap_but_accepts_adjacency_in_any_order() {
    let file = ExactFileRevision {
        file_id: "file".into(),
        revision: "r1".into(),
    };
    let mut set = SegmentationSet {
        contract_version: 1,
        segmentation_set_id: "episodes".into(),
        file: file.clone(),
        segments: [("third", 20, 30), ("first", 0, 10), ("second", 10, 20)]
            .into_iter()
            .map(|(id, start, end)| ActiveSegment {
                binding_id: id.into(),
                file: file.clone(),
                source_interval: range(start, end),
            })
            .collect(),
    };
    assert_eq!(validate_segmentation_set(&set), Ok(()));
    set.segments[0].source_interval = range(11, 19);
    assert_eq!(
        validate_segmentation_set(&set),
        Err(IdentityError::OverlappingCoverage)
    );
    set.segments[0].source_interval = range(20, 30);
    set.segments[0].file.revision = "r2".into();
    assert_eq!(
        validate_segmentation_set(&set),
        Err(IdentityError::MixedFileRevision)
    );
}
