// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Version 1 logical identity and exact-file binding decisions. No I/O.
//!
//! The store owns IDs, foreign keys, history and the atomic write. It must pass
//! the complete active binding set, check association ownership and compare a
//! version's declared coverage with its timeline record in that transaction.
//! These decisions do not replace component compatibility or fallback owners.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const IDENTITY_CONTRACT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityError {
    UnsupportedContract,
    InvalidIdentifier,
    InvalidInterval,
    InvalidTimeBase,
    InvalidToolInterval,
    NormalizationOverflow,
    EmptyNormalizedInterval,
    DurationMismatch,
    InvalidPart,
    DuplicatePart,
    NonconsecutiveParts,
    RepeatedFile,
    DuplicateBinding,
    DuplicateAssociation,
    MissingPlacement,
    UnknownDurationPlacement,
    UnmeasuredCoverage,
    MissingCoverage,
    IncompleteCoverage,
    OverlappingCoverage,
    MixedFileRevision,
    MissingEquivalentTimeline,
    ProvisionalTimelineCollision,
    MissingEquivalenceEvidence,
    VersionNotReady,
    VersionScopeMismatch,
    DuplicateVersion,
    DuplicateVersionOwner,
    InvalidVersionOwner,
    InvalidAvailability,
}

impl IdentityError {
    /// Stable machine code; the display text is not a separate error contract.
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedContract => "catalog_identity_unsupported_contract",
            Self::InvalidIdentifier => "catalog_identity_invalid_identifier",
            Self::InvalidInterval => "catalog_identity_invalid_interval",
            Self::InvalidTimeBase => "catalog_identity_invalid_time_base",
            Self::InvalidToolInterval => "catalog_identity_invalid_tool_interval",
            Self::NormalizationOverflow => "catalog_identity_normalization_overflow",
            Self::EmptyNormalizedInterval => "catalog_identity_empty_normalized_interval",
            Self::DurationMismatch => "catalog_identity_duration_mismatch",
            Self::InvalidPart => "catalog_identity_invalid_part",
            Self::DuplicatePart => "catalog_identity_duplicate_part",
            Self::NonconsecutiveParts => "catalog_identity_nonconsecutive_parts",
            Self::RepeatedFile => "catalog_identity_repeated_file",
            Self::DuplicateBinding => "catalog_identity_duplicate_binding",
            Self::DuplicateAssociation => "catalog_identity_duplicate_association",
            Self::MissingPlacement => "catalog_identity_missing_placement",
            Self::UnknownDurationPlacement => "catalog_identity_unknown_duration_placement",
            Self::UnmeasuredCoverage => "catalog_identity_unmeasured_coverage",
            Self::MissingCoverage => "catalog_identity_missing_coverage",
            Self::IncompleteCoverage => "catalog_identity_incomplete_coverage",
            Self::OverlappingCoverage => "catalog_identity_overlapping_coverage",
            Self::MixedFileRevision => "catalog_identity_mixed_file_revision",
            Self::MissingEquivalentTimeline => "catalog_identity_missing_equivalent_timeline",
            Self::ProvisionalTimelineCollision => "catalog_identity_provisional_timeline_collision",
            Self::MissingEquivalenceEvidence => "catalog_identity_missing_equivalence_evidence",
            Self::VersionNotReady => "catalog_identity_version_not_ready",
            Self::VersionScopeMismatch => "catalog_identity_version_scope_mismatch",
            Self::DuplicateVersion => "catalog_identity_duplicate_version",
            Self::DuplicateVersionOwner => "catalog_identity_duplicate_version_owner",
            Self::InvalidVersionOwner => "catalog_identity_invalid_version_owner",
            Self::InvalidAvailability => "catalog_identity_invalid_availability",
        }
    }
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for IdentityError {}

pub type Result<T> = std::result::Result<T, IdentityError>;

/// Checked, nonempty half-open microsecond interval. Private fields and checked
/// deserialization prevent invalid intervals entering another production owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "IntervalFields")]
pub struct IntervalUs {
    start_us: i64,
    end_us: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntervalFields {
    start_us: i64,
    end_us: i64,
}

impl TryFrom<IntervalFields> for IntervalUs {
    type Error = IdentityError;

    fn try_from(value: IntervalFields) -> Result<Self> {
        Self::new(value.start_us, value.end_us)
    }
}

impl IntervalUs {
    pub fn new(start_us: i64, end_us: i64) -> Result<Self> {
        if start_us < 0 || start_us >= end_us {
            return Err(IdentityError::InvalidInterval);
        }
        Ok(Self { start_us, end_us })
    }

    pub const fn start_us(self) -> i64 {
        self.start_us
    }

    pub const fn end_us(self) -> i64 {
        self.end_us
    }

    pub const fn duration_us(self) -> i64 {
        // Construction guarantees 0 <= start < end <= i64::MAX.
        self.end_us - self.start_us
    }

    pub const fn overlaps(self, other: Self) -> bool {
        self.start_us < other.end_us && other.start_us < self.end_us
    }

    pub const fn contains(self, other: Self) -> bool {
        self.start_us <= other.start_us && other.end_us <= self.end_us
    }

    /// Translate a source interval without introducing a speed transform.
    pub fn place_at(self, start_us: i64) -> Result<Self> {
        let end_us = start_us
            .checked_add(self.duration_us())
            .ok_or(IdentityError::NormalizationOverflow)?;
        Self::new(start_us, end_us)
    }
}

/// Seconds per tool tick, retained exactly as reported (not as a float).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolTimeBase {
    pub numerator: i64,
    pub denominator: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSlice {
    pub start_ticks: i64,
    pub end_ticks: i64,
    pub time_base: ToolTimeBase,
}

/// Persist this alongside a tool-derived slice. It reports both original bounds
/// and rounding; neither rounded durations nor this report prove equivalence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct NormalizedToolSlice {
    pub contract_version: u32,
    pub original: ToolSlice,
    pub interval: IntervalUs,
    pub start_rounded_up: bool,
    pub end_rounded_down: bool,
}

/// Normalize inward: ceil(start), floor(end). No floating-point conversion,
/// clamping, inferred frame transform or silent empty slice is allowed.
pub fn normalize_tool_slice(slice: ToolSlice) -> Result<NormalizedToolSlice> {
    if slice.time_base.numerator <= 0 || slice.time_base.denominator <= 0 {
        return Err(IdentityError::InvalidTimeBase);
    }
    if slice.start_ticks < 0 || slice.start_ticks >= slice.end_ticks {
        return Err(IdentityError::InvalidToolInterval);
    }
    let (start_us, start_fraction) = tool_endpoint_us(slice.start_ticks, slice.time_base, true)?;
    let (end_us, end_fraction) = tool_endpoint_us(slice.end_ticks, slice.time_base, false)?;
    if start_us >= end_us {
        return Err(IdentityError::EmptyNormalizedInterval);
    }
    Ok(NormalizedToolSlice {
        contract_version: IDENTITY_CONTRACT_VERSION,
        original: slice,
        interval: IntervalUs::new(start_us, end_us)?,
        start_rounded_up: start_fraction,
        end_rounded_down: end_fraction,
    })
}

fn tool_endpoint_us(ticks: i64, base: ToolTimeBase, ceil: bool) -> Result<(i64, bool)> {
    let overflow = IdentityError::NormalizationOverflow;
    let numerator = i128::from(ticks)
        .checked_mul(i128::from(base.numerator))
        .ok_or(overflow)?;
    let denominator = i128::from(base.denominator);
    // Divide before scaling: even i64::MAX * i64::MAX / i64::MAX is
    // representable in i128, whereas scaling the product first may overflow.
    let seconds = numerator / denominator;
    let scaled_remainder = (numerator % denominator)
        .checked_mul(1_000_000)
        .ok_or(overflow)?;
    let fractional = scaled_remainder % denominator != 0;
    let micros = seconds
        .checked_mul(1_000_000)
        .and_then(|v| v.checked_add(scaled_remainder / denominator))
        .and_then(|v| v.checked_add(i128::from(ceil && fractional)))
        .ok_or(overflow)?;
    Ok((i64::try_from(micros).map_err(|_| overflow)?, fractional))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactFileRevision {
    pub file_id: String,
    pub revision: String,
}

impl ExactFileRevision {
    pub fn validate(&self) -> Result<()> {
        identifier(&self.file_id)?;
        identifier(&self.revision)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceExtent {
    WholeFile { duration_us: Option<i64> },
    Slice { interval: IntervalUs },
}

impl SourceExtent {
    /// Unknown whole-file duration deliberately produces no claimed interval.
    pub fn interval(self) -> Result<Option<IntervalUs>> {
        match self {
            Self::WholeFile { duration_us: None } => Ok(None),
            Self::WholeFile {
                duration_us: Some(duration_us),
            } => IntervalUs::new(0, duration_us).map(Some),
            Self::Slice { interval } => Ok(Some(interval)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionBinding {
    pub binding_id: String,
    pub association_id: String,
    pub file: ExactFileRevision,
    pub part: u32,
    pub source: SourceExtent,
    pub timeline_interval: Option<IntervalUs>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionState {
    Draft,
    Ready,
}

/// Each contract is one complete snapshot of one version's primary bindings.
/// Timeline ownership is structural: all bindings belong to this one timeline
/// and association item. Alternative versions must be validated separately.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionContract {
    pub contract_version: u32,
    pub version_id: String,
    pub association_item_id: String,
    pub edition_item_id: String,
    pub timeline_id: String,
    pub state: VersionState,
    pub declared_coverage: Option<IntervalUs>,
    pub bindings: Vec<VersionBinding>,
}

/// Validate a version before its owner writes any bindings. Drafts may be
/// incomplete; no state may repeat a file, overlap placements, change speed or
/// claim a placement for an unmeasured whole file. A ready version covers its
/// declared interval exactly, in consecutive primary-part order.
pub fn validate_version(version: &VersionContract) -> Result<()> {
    contract(version.contract_version)?;
    identifier(&version.version_id)?;
    identifier(&version.association_item_id)?;
    identifier(&version.edition_item_id)?;
    identifier(&version.timeline_id)?;
    if version.association_item_id == version.edition_item_id {
        return Err(IdentityError::InvalidVersionOwner);
    }
    let mut binding_ids = BTreeSet::new();
    let mut association_ids = BTreeSet::new();
    let mut file_ids = BTreeSet::new();
    let mut parts = BTreeSet::new();
    let mut placements = Vec::new();
    let mut unmeasured = false;
    let mut unplaced = false;
    for binding in &version.bindings {
        identifier(&binding.binding_id)?;
        identifier(&binding.association_id)?;
        binding.file.validate()?;
        if !binding_ids.insert(&binding.binding_id) {
            return Err(IdentityError::DuplicateBinding);
        }
        if !association_ids.insert(&binding.association_id) {
            return Err(IdentityError::DuplicateAssociation);
        }
        // Different revisions do not permit a repeated use of this file.
        if !file_ids.insert(&binding.file.file_id) {
            return Err(IdentityError::RepeatedFile);
        }
        if binding.part == 0 {
            return Err(IdentityError::InvalidPart);
        }
        if !parts.insert(binding.part) {
            return Err(IdentityError::DuplicatePart);
        }
        let source = binding.source.interval()?;
        match (source, binding.timeline_interval) {
            (None, Some(_)) => return Err(IdentityError::UnknownDurationPlacement),
            (None, None) => unmeasured = true,
            (Some(_), None) => {
                if matches!(binding.source, SourceExtent::Slice { .. }) {
                    return Err(IdentityError::MissingPlacement);
                }
                unplaced = true;
            }
            (Some(source), Some(placement)) => {
                validate_placement(source, placement)?;
                if version
                    .declared_coverage
                    .is_some_and(|coverage| !coverage.contains(placement))
                {
                    return Err(IdentityError::IncompleteCoverage);
                }
                placements.push((binding.part, placement));
            }
        }
    }
    let mut ranges = placements
        .iter()
        .map(|(_, range)| *range)
        .collect::<Vec<_>>();
    reject_overlaps(&mut ranges)?;
    if unmeasured && (version.declared_coverage.is_some() || version.state == VersionState::Ready) {
        return Err(IdentityError::UnmeasuredCoverage);
    }
    if version.state == VersionState::Draft {
        return Ok(());
    }
    if unplaced {
        return Err(IdentityError::MissingPlacement);
    }
    let coverage = version
        .declared_coverage
        .ok_or(IdentityError::MissingCoverage)?;
    if parts.is_empty()
        || parts
            .iter()
            .enumerate()
            .any(|(i, part)| u32::try_from(i).ok().and_then(|n| n.checked_add(1)) != Some(*part))
    {
        return Err(IdentityError::NonconsecutiveParts);
    }
    placements.sort_unstable_by_key(|(part, _)| *part);
    let mut next_start = coverage.start_us();
    for (_, placement) in placements {
        if placement.start_us() != next_start {
            return Err(IdentityError::IncompleteCoverage);
        }
        next_start = placement.end_us();
    }
    if next_start != coverage.end_us() {
        return Err(IdentityError::IncompleteCoverage);
    }
    Ok(())
}

pub fn validate_placement(source: IntervalUs, timeline: IntervalUs) -> Result<()> {
    if source.duration_us() != timeline.duration_us() {
        return Err(IdentityError::DurationMismatch);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveSegment {
    pub binding_id: String,
    pub file: ExactFileRevision,
    pub source_interval: IntervalUs,
}

/// A complete active segmentation set, not a global prohibition on file reuse.
/// The store supplies all active members, including unchanged existing members,
/// under the same transaction that admits the proposed segment bindings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentationSet {
    pub contract_version: u32,
    pub segmentation_set_id: String,
    pub file: ExactFileRevision,
    pub segments: Vec<ActiveSegment>,
}

pub fn validate_segmentation_set(set: &SegmentationSet) -> Result<()> {
    contract(set.contract_version)?;
    identifier(&set.segmentation_set_id)?;
    set.file.validate()?;
    let mut binding_ids = BTreeSet::new();
    let mut ranges = Vec::with_capacity(set.segments.len());
    for segment in &set.segments {
        identifier(&segment.binding_id)?;
        segment.file.validate()?;
        if !binding_ids.insert(&segment.binding_id) {
            return Err(IdentityError::DuplicateBinding);
        }
        if segment.file != set.file {
            return Err(IdentityError::MixedFileRevision);
        }
        ranges.push(segment.source_interval);
    }
    reject_overlaps(&mut ranges)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EquivalenceBasis {
    Declared,
    Verified,
}

/// A reference to recorded evidence, not inferred equivalence. The persistence
/// owner verifies and retains the evidence record and its provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EquivalenceEvidence {
    pub basis: EquivalenceBasis,
    pub evidence_id: String,
}

/// The existing resolution owner reports availability by exact binding ID.
/// Candidates are in the caller's preference order; this decision never merges
/// availability across them or replaces that owner's probing/ranking policy.
#[derive(Clone, Copy, Debug)]
pub struct VersionCandidate<'a> {
    pub version: &'a VersionContract,
    pub equivalence: Option<&'a EquivalenceEvidence>,
    pub available_binding_ids: &'a [String],
}

/// Select one whole ready representation in an explicitly equivalent timeline.
/// Every candidate must have recorded declared/verified evidence, the same
/// edition, timeline and complete coverage. Provisional identities cannot be
/// treated as equivalent alternatives. Direct reads of a provisional version
/// remain possible via `select_explicit_version`, without asserting equivalence.
///
/// Returns the selected candidate index, or None when no entire candidate is
/// available. Admission checks every candidate before returning a selection.
pub fn select_complete_version(
    edition_item_id: &str,
    timeline_id: &str,
    coverage: IntervalUs,
    candidates: &[VersionCandidate<'_>],
) -> Result<Option<usize>> {
    identifier(edition_item_id)?;
    identifier(timeline_id)?;
    let mut version_ids = BTreeSet::new();
    let mut owners = BTreeSet::new();
    let mut all_binding_ids = BTreeSet::new();
    let mut all_association_ids = BTreeSet::new();
    let mut selected = None;
    for (index, candidate) in candidates.iter().enumerate() {
        let version = candidate.version;
        validate_version(version)?;
        if version.state != VersionState::Ready {
            return Err(IdentityError::VersionNotReady);
        }
        if version.edition_item_id != edition_item_id
            || version.timeline_id != timeline_id
            || version.declared_coverage != Some(coverage)
        {
            return Err(IdentityError::VersionScopeMismatch);
        }
        if !version_ids.insert(&version.version_id) {
            return Err(IdentityError::DuplicateVersion);
        }
        if !owners.insert(&version.association_item_id) {
            return Err(IdentityError::DuplicateVersionOwner);
        }
        let evidence = candidate
            .equivalence
            .ok_or(IdentityError::MissingEquivalenceEvidence)?;
        identifier(&evidence.evidence_id)?;
        for binding in &version.bindings {
            if !all_binding_ids.insert(&binding.binding_id) {
                return Err(IdentityError::DuplicateBinding);
            }
            if !all_association_ids.insert(&binding.association_id) {
                return Err(IdentityError::DuplicateAssociation);
            }
        }
        let available = complete_availability(version, candidate.available_binding_ids)?;
        if selected.is_none() && available {
            selected = Some(index);
        }
    }
    Ok(selected)
}

/// Select an explicitly named version on its own timeline, including a ready
/// provisional version. This does not assert equivalence to any other version
/// and requires no invented self-equivalence evidence. False means at least one
/// exact binding is unavailable; no partial version is selected.
pub fn select_explicit_version(
    requested_version_id: &str,
    version: &VersionContract,
    available_binding_ids: &[String],
) -> Result<bool> {
    identifier(requested_version_id)?;
    validate_version(version)?;
    if version.version_id != requested_version_id {
        return Err(IdentityError::VersionScopeMismatch);
    }
    if version.state != VersionState::Ready {
        return Err(IdentityError::VersionNotReady);
    }
    complete_availability(version, available_binding_ids)
}

fn complete_availability(
    version: &VersionContract,
    available_binding_ids: &[String],
) -> Result<bool> {
    let binding_ids = version
        .bindings
        .iter()
        .map(|binding| &binding.binding_id)
        .collect::<BTreeSet<_>>();
    let available = available_binding_ids.iter().collect::<BTreeSet<_>>();
    if available.len() != available_binding_ids.len() || !available.is_subset(&binding_ids) {
        return Err(IdentityError::InvalidAvailability);
    }
    Ok(available == binding_ids)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineAdmissionRequest {
    pub contract_version: u32,
    pub candidate_timeline_id: Option<String>,
    pub provisional_timeline_id: String,
    pub evidence: Option<EquivalenceEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimelineAdmission {
    Provisional {
        timeline_id: String,
    },
    Equivalent {
        timeline_id: String,
        evidence: EquivalenceEvidence,
    },
}

/// Unknown equivalence creates a separate provisional timeline. This API has
/// no title, path, digest, preference or duration-equality matching shortcut.
pub fn admit_timeline(request: &TimelineAdmissionRequest) -> Result<TimelineAdmission> {
    contract(request.contract_version)?;
    identifier(&request.provisional_timeline_id)?;
    if let Some(candidate) = &request.candidate_timeline_id {
        identifier(candidate)?;
    }
    if let Some(evidence) = &request.evidence {
        identifier(&evidence.evidence_id)?;
        let timeline_id = request
            .candidate_timeline_id
            .clone()
            .ok_or(IdentityError::MissingEquivalentTimeline)?;
        return Ok(TimelineAdmission::Equivalent {
            timeline_id,
            evidence: evidence.clone(),
        });
    }
    if request.candidate_timeline_id.as_ref() == Some(&request.provisional_timeline_id) {
        return Err(IdentityError::ProvisionalTimelineCollision);
    }
    Ok(TimelineAdmission::Provisional {
        timeline_id: request.provisional_timeline_id.clone(),
    })
}

fn identifier(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(IdentityError::InvalidIdentifier);
    }
    Ok(())
}

fn contract(version: u32) -> Result<()> {
    if version != IDENTITY_CONTRACT_VERSION {
        return Err(IdentityError::UnsupportedContract);
    }
    Ok(())
}

fn reject_overlaps(ranges: &mut [IntervalUs]) -> Result<()> {
    ranges.sort_unstable_by_key(|range| (range.start_us(), range.end_us()));
    if ranges.windows(2).any(|pair| pair[0].overlaps(pair[1])) {
        return Err(IdentityError::OverlappingCoverage);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interval(start: i64, end: i64) -> IntervalUs {
        IntervalUs::new(start, end).unwrap()
    }

    fn binding(part: u32, start: i64, end: i64) -> VersionBinding {
        VersionBinding {
            binding_id: format!("binding-{part}"),
            association_id: format!("association-{part}"),
            file: ExactFileRevision {
                file_id: format!("file-{part}"),
                revision: format!("revision-{part}"),
            },
            part,
            source: SourceExtent::WholeFile {
                duration_us: Some(end - start),
            },
            timeline_interval: Some(interval(start, end)),
        }
    }

    fn version() -> VersionContract {
        VersionContract {
            contract_version: IDENTITY_CONTRACT_VERSION,
            version_id: "version-1".into(),
            association_item_id: "version-item-1".into(),
            edition_item_id: "edition-1".into(),
            timeline_id: "timeline-1".into(),
            state: VersionState::Ready,
            declared_coverage: Some(interval(0, 20)),
            bindings: vec![binding(1, 0, 10), binding(2, 10, 20)],
        }
    }

    #[test]
    fn intervals_are_checked_half_open_values_even_after_deserialization() {
        for (start, end) in [(-1, 1), (0, 0), (10, 9)] {
            assert_eq!(
                IntervalUs::new(start, end),
                Err(IdentityError::InvalidInterval)
            );
            assert!(
                serde_json::from_value::<IntervalUs>(serde_json::json!({
                    "start_us": start, "end_us": end
                }))
                .is_err()
            );
        }
        for invalid in [
            r#"{"start_us":0}"#,
            r#"{"end_us":10}"#,
            r#"{"start_us":0,"end_us":10,"unit":"frames"}"#,
            r#"{"start_us":0.0,"end_us":10}"#,
            r#"{"start_us":0,"end_us":9223372036854775808}"#,
        ] {
            assert!(
                serde_json::from_str::<IntervalUs>(invalid).is_err(),
                "{invalid}"
            );
        }
        let max = interval(0, i64::MAX);
        assert_eq!(max.duration_us(), i64::MAX);
        assert!(!interval(0, 10).overlaps(interval(10, 11)));
        assert!(interval(0, 10).overlaps(interval(9, 11)));
        assert_eq!(interval(10, 20).place_at(50).unwrap(), interval(50, 60));
        assert_eq!(max.place_at(1), Err(IdentityError::NormalizationOverflow));
        let json = serde_json::to_string(&max).unwrap();
        assert_eq!(serde_json::from_str::<IntervalUs>(&json).unwrap(), max);
    }

    #[test]
    fn tool_normalization_rounds_inward_and_retains_original_evidence() {
        let original = ToolSlice {
            start_ticks: 1,
            end_ticks: 4,
            time_base: ToolTimeBase {
                numerator: 1,
                denominator: 3,
            },
        };
        let result = normalize_tool_slice(original).unwrap();
        assert_eq!(result.original, original);
        assert_eq!(result.interval, interval(333_334, 1_333_333));
        assert!(result.start_rounded_up);
        assert!(result.end_rounded_down);
        let exact = normalize_tool_slice(ToolSlice {
            start_ticks: 0,
            end_ticks: 30,
            time_base: ToolTimeBase {
                numerator: 1,
                denominator: 30,
            },
        })
        .unwrap();
        assert_eq!(exact.interval, interval(0, 1_000_000));
        assert!(!exact.start_rounded_up && !exact.end_rounded_down);
    }

    #[test]
    fn normalization_rejects_invalid_empty_and_overflowed_ranges() {
        let input = ToolSlice {
            start_ticks: 0,
            end_ticks: 1,
            time_base: ToolTimeBase {
                numerator: 1,
                denominator: 1,
            },
        };
        for base in [
            ToolTimeBase {
                numerator: 0,
                denominator: 1,
            },
            ToolTimeBase {
                numerator: 1,
                denominator: 0,
            },
            ToolTimeBase {
                numerator: -1,
                denominator: 1,
            },
            ToolTimeBase {
                numerator: 1,
                denominator: -1,
            },
        ] {
            assert_eq!(
                normalize_tool_slice(ToolSlice {
                    time_base: base,
                    ..input
                }),
                Err(IdentityError::InvalidTimeBase)
            );
        }
        for (start_ticks, end_ticks) in [(-1, 1), (1, 1), (2, 1)] {
            assert_eq!(
                normalize_tool_slice(ToolSlice {
                    start_ticks,
                    end_ticks,
                    ..input
                }),
                Err(IdentityError::InvalidToolInterval)
            );
        }
        assert_eq!(
            normalize_tool_slice(ToolSlice {
                time_base: ToolTimeBase {
                    numerator: 1,
                    denominator: 1_000_001
                },
                ..input
            }),
            Err(IdentityError::EmptyNormalizedInterval)
        );
        assert_eq!(
            normalize_tool_slice(ToolSlice {
                end_ticks: i64::MAX,
                ..input
            }),
            Err(IdentityError::NormalizationOverflow)
        );
        assert_eq!(
            normalize_tool_slice(ToolSlice {
                end_ticks: i64::MAX,
                time_base: ToolTimeBase {
                    numerator: i64::MAX,
                    denominator: 1
                },
                ..input
            }),
            Err(IdentityError::NormalizationOverflow)
        );
    }

    #[test]
    fn rational_normalization_handles_large_exact_values_without_float_loss() {
        let result = normalize_tool_slice(ToolSlice {
            start_ticks: i64::MAX - 2,
            end_ticks: i64::MAX,
            time_base: ToolTimeBase {
                numerator: 1,
                denominator: 1_000_000,
            },
        })
        .unwrap();
        assert_eq!(result.interval, interval(i64::MAX - 2, i64::MAX));
        let result = normalize_tool_slice(ToolSlice {
            start_ticks: 9_000_000_000_000,
            end_ticks: 9_000_000_000_001,
            time_base: ToolTimeBase {
                numerator: i64::MAX,
                denominator: i64::MAX,
            },
        })
        .unwrap();
        assert_eq!(
            result.interval,
            interval(9_000_000_000_000_000_000, 9_000_000_000_001_000_000)
        );
    }

    #[test]
    fn inward_rounding_matches_independent_small_rational_oracle() {
        for numerator in 1_i64..8 {
            for denominator in 1_i64..31 {
                for start in 0_i64..10 {
                    let original = ToolSlice {
                        start_ticks: start,
                        end_ticks: start + 1,
                        time_base: ToolTimeBase {
                            numerator,
                            denominator,
                        },
                    };
                    let result = normalize_tool_slice(original).unwrap();
                    let a = start * numerator * 1_000_000;
                    let b = (start + 1) * numerator * 1_000_000;
                    assert_eq!(
                        result.interval.start_us(),
                        (a + denominator - 1) / denominator
                    );
                    assert_eq!(result.interval.end_us(), b / denominator);
                    assert_eq!(result.start_rounded_up, a % denominator != 0);
                    assert_eq!(result.end_rounded_down, b % denominator != 0);
                }
            }
        }
    }

    #[test]
    fn complete_versions_validate_independently_and_use_part_order() {
        let first = version();
        assert_eq!(validate_version(&first), Ok(()));
        let mut alternative = first.clone();
        alternative.version_id = "version-2".into();
        alternative.association_item_id = "version-item-2".into();
        alternative.bindings.reverse();
        assert_eq!(validate_version(&alternative), Ok(()));
        let encoded = serde_json::to_string(&first).unwrap();
        assert_eq!(
            serde_json::from_str::<VersionContract>(&encoded).unwrap(),
            first
        );
    }

    #[test]
    fn ready_versions_require_complete_consecutive_nonoverlapping_parts() {
        let mut value = version();
        value.bindings[1].part = 3;
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::NonconsecutiveParts)
        );
        value.bindings[1].part = 1;
        assert_eq!(validate_version(&value), Err(IdentityError::DuplicatePart));
        value.bindings[1].part = 0;
        assert_eq!(validate_version(&value), Err(IdentityError::InvalidPart));
        value = version();
        value.bindings[1] = binding(2, 9, 19);
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::OverlappingCoverage)
        );
        value.bindings[1] = binding(2, 11, 20);
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::IncompleteCoverage)
        );
        value.bindings[1] = binding(2, 10, 19);
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::IncompleteCoverage)
        );
        value = version();
        value.bindings.swap(0, 1);
        value.bindings[0].part = 1;
        value.bindings[1].part = 2;
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::IncompleteCoverage)
        );
        value = version();
        value.bindings.clear();
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::NonconsecutiveParts)
        );
        value = version();
        value.declared_coverage = None;
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::MissingCoverage)
        );
    }

    #[test]
    fn file_reuse_is_rejected_even_for_another_revision() {
        let mut value = version();
        value.bindings[1].file.file_id = value.bindings[0].file.file_id.clone();
        assert_ne!(
            value.bindings[1].file.revision,
            value.bindings[0].file.revision
        );
        assert_eq!(validate_version(&value), Err(IdentityError::RepeatedFile));
        value.state = VersionState::Draft;
        assert_eq!(validate_version(&value), Err(IdentityError::RepeatedFile));
    }

    #[test]
    fn bindings_require_unique_ids_and_exact_revision_evidence() {
        let mut value = version();
        value.bindings[1].binding_id = value.bindings[0].binding_id.clone();
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::DuplicateBinding)
        );
        value = version();
        value.bindings[1].association_id = value.bindings[0].association_id.clone();
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::DuplicateAssociation)
        );
        value = version();
        value.bindings[1].file.revision.clear();
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::InvalidIdentifier)
        );
        value = version();
        value.association_item_id = value.edition_item_id.clone();
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::InvalidVersionOwner)
        );
    }

    #[test]
    fn slice_placement_preserves_duration_and_requires_both_intervals() {
        let mut value = version();
        value.bindings[0].source = SourceExtent::Slice {
            interval: interval(1_000, 1_010),
        };
        assert_eq!(validate_version(&value), Ok(()));
        value.bindings[0].source = SourceExtent::Slice {
            interval: interval(1_000, 1_011),
        };
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::DurationMismatch)
        );
        value.bindings[0].timeline_interval = None;
        value.state = VersionState::Draft;
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::MissingPlacement)
        );
    }

    #[test]
    fn unmeasured_whole_files_remain_evidence_without_false_readiness() {
        let mut value = version();
        value.state = VersionState::Draft;
        value.declared_coverage = None;
        value.bindings[0].source = SourceExtent::WholeFile { duration_us: None };
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::UnknownDurationPlacement)
        );
        value.bindings[0].timeline_interval = None;
        assert_eq!(validate_version(&value), Ok(()));
        value.declared_coverage = Some(interval(0, 20));
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::UnmeasuredCoverage)
        );
        value.declared_coverage = None;
        value.state = VersionState::Ready;
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::UnmeasuredCoverage)
        );
        value.bindings[0].source = SourceExtent::WholeFile {
            duration_us: Some(0),
        };
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::InvalidInterval)
        );
    }

    fn segmentation() -> SegmentationSet {
        let file = ExactFileRevision {
            file_id: "two-episodes".into(),
            revision: "r1".into(),
        };
        SegmentationSet {
            contract_version: IDENTITY_CONTRACT_VERSION,
            segmentation_set_id: "episodes-set".into(),
            file: file.clone(),
            segments: vec![
                ActiveSegment {
                    binding_id: "episode-1".into(),
                    file: file.clone(),
                    source_interval: interval(0, 10),
                },
                ActiveSegment {
                    binding_id: "episode-2".into(),
                    file,
                    source_interval: interval(10, 20),
                },
            ],
        }
    }

    #[test]
    fn segmentation_is_scoped_and_nonoverlapping_at_exact_revision() {
        let mut set = segmentation();
        assert_eq!(validate_segmentation_set(&set), Ok(()));
        set.segments.reverse();
        assert_eq!(validate_segmentation_set(&set), Ok(()));
        set.segments[1].source_interval = interval(9, 11);
        assert_eq!(
            validate_segmentation_set(&set),
            Err(IdentityError::OverlappingCoverage)
        );
        set = segmentation();
        set.segments[1].file.revision = "r2".into();
        assert_eq!(
            validate_segmentation_set(&set),
            Err(IdentityError::MixedFileRevision)
        );
        set = segmentation();
        set.segments[1].binding_id = set.segments[0].binding_id.clone();
        assert_eq!(
            validate_segmentation_set(&set),
            Err(IdentityError::DuplicateBinding)
        );
        let mut editorial_context = segmentation();
        editorial_context.segmentation_set_id = "alternate-context".into();
        assert_eq!(validate_segmentation_set(&editorial_context), Ok(()));
    }

    #[test]
    fn unknown_equivalence_creates_a_distinct_provisional_timeline() {
        let mut request = TimelineAdmissionRequest {
            contract_version: IDENTITY_CONTRACT_VERSION,
            candidate_timeline_id: Some("candidate".into()),
            provisional_timeline_id: "new-provisional".into(),
            evidence: None,
        };
        assert_eq!(
            admit_timeline(&request),
            Ok(TimelineAdmission::Provisional {
                timeline_id: "new-provisional".into()
            })
        );
        request.provisional_timeline_id = "candidate".into();
        assert_eq!(
            admit_timeline(&request),
            Err(IdentityError::ProvisionalTimelineCollision)
        );
        for basis in [EquivalenceBasis::Declared, EquivalenceBasis::Verified] {
            let evidence = EquivalenceEvidence {
                basis,
                evidence_id: "recorded-evidence".into(),
            };
            request.evidence = Some(evidence.clone());
            assert_eq!(
                admit_timeline(&request),
                Ok(TimelineAdmission::Equivalent {
                    timeline_id: "candidate".into(),
                    evidence
                })
            );
        }
        request.evidence.as_mut().unwrap().evidence_id.clear();
        assert_eq!(
            admit_timeline(&request),
            Err(IdentityError::InvalidIdentifier)
        );
        request.evidence.as_mut().unwrap().evidence_id = "evidence".into();
        request.candidate_timeline_id = None;
        assert_eq!(
            admit_timeline(&request),
            Err(IdentityError::MissingEquivalentTimeline)
        );
    }

    #[test]
    fn unsupported_contracts_are_refused_before_any_decision() {
        let mut value = version();
        value.contract_version = 2;
        assert_eq!(
            validate_version(&value),
            Err(IdentityError::UnsupportedContract)
        );
        let mut set = segmentation();
        set.contract_version = 0;
        assert_eq!(
            validate_segmentation_set(&set),
            Err(IdentityError::UnsupportedContract)
        );
        let request = TimelineAdmissionRequest {
            contract_version: 2,
            candidate_timeline_id: None,
            provisional_timeline_id: "new".into(),
            evidence: None,
        };
        assert_eq!(
            admit_timeline(&request),
            Err(IdentityError::UnsupportedContract)
        );
    }

    fn alternative() -> VersionContract {
        let mut version = version();
        version.version_id = "version-2".into();
        version.association_item_id = "version-item-2".into();
        for binding in &mut version.bindings {
            binding.binding_id.push_str("-alternative");
            binding.association_id.push_str("-alternative");
            binding.file.file_id.push_str("-alternative");
        }
        version
    }

    fn evidence() -> EquivalenceEvidence {
        EquivalenceEvidence {
            basis: EquivalenceBasis::Declared,
            evidence_id: "explicit-membership-evidence".into(),
        }
    }

    fn choose(candidates: &[VersionCandidate<'_>]) -> Result<Option<usize>> {
        select_complete_version("edition-1", "timeline-1", interval(0, 20), candidates)
    }

    #[test]
    fn selection_never_combines_available_parts_across_versions() {
        let first = version();
        let second = alternative();
        let evidence = evidence();
        let first_one = [first.bindings[0].binding_id.clone()];
        let second_two = [second.bindings[1].binding_id.clone()];
        let mut candidates = [
            VersionCandidate {
                version: &first,
                equivalence: Some(&evidence),
                available_binding_ids: &first_one,
            },
            VersionCandidate {
                version: &second,
                equivalence: Some(&evidence),
                available_binding_ids: &second_two,
            },
        ];
        assert_eq!(choose(&candidates), Ok(None));
        let second_all = second
            .bindings
            .iter()
            .map(|b| b.binding_id.clone())
            .collect::<Vec<_>>();
        candidates[1].available_binding_ids = &second_all;
        assert_eq!(choose(&candidates), Ok(Some(1)));
        let first_all = first
            .bindings
            .iter()
            .map(|b| b.binding_id.clone())
            .collect::<Vec<_>>();
        candidates[0].available_binding_ids = &first_all;
        assert_eq!(choose(&candidates), Ok(Some(0)));
    }

    #[test]
    fn selection_rejects_unknown_equivalence_instead_of_using_duration_equality() {
        let first = version();
        let second = alternative();
        let evidence = evidence();
        let all = first
            .bindings
            .iter()
            .map(|b| b.binding_id.clone())
            .collect::<Vec<_>>();
        let candidates = [
            VersionCandidate {
                version: &first,
                equivalence: Some(&evidence),
                available_binding_ids: &all,
            },
            VersionCandidate {
                version: &second,
                equivalence: None,
                available_binding_ids: &[],
            },
        ];
        assert_eq!(first.declared_coverage, second.declared_coverage);
        // Admission checks all candidates even though the preferred one is usable.
        assert_eq!(
            choose(&candidates),
            Err(IdentityError::MissingEquivalenceEvidence)
        );
        let blank_evidence = EquivalenceEvidence {
            basis: EquivalenceBasis::Verified,
            evidence_id: " ".into(),
        };
        assert_eq!(
            choose(&[VersionCandidate {
                version: &first,
                equivalence: Some(&blank_evidence),
                available_binding_ids: &all
            }]),
            Err(IdentityError::InvalidIdentifier)
        );
    }

    #[test]
    fn selection_checks_scope_readiness_and_exact_availability_ids() {
        let mut first = version();
        let evidence = evidence();
        let unknown = ["another-version-binding".to_string()];
        assert_eq!(
            choose(&[VersionCandidate {
                version: &first,
                equivalence: Some(&evidence),
                available_binding_ids: &unknown
            }]),
            Err(IdentityError::InvalidAvailability)
        );
        let duplicate = [
            first.bindings[0].binding_id.clone(),
            first.bindings[0].binding_id.clone(),
        ];
        assert_eq!(
            choose(&[VersionCandidate {
                version: &first,
                equivalence: Some(&evidence),
                available_binding_ids: &duplicate
            }]),
            Err(IdentityError::InvalidAvailability)
        );
        first.state = VersionState::Draft;
        assert_eq!(
            choose(&[VersionCandidate {
                version: &first,
                equivalence: Some(&evidence),
                available_binding_ids: &[]
            }]),
            Err(IdentityError::VersionNotReady)
        );
        first.state = VersionState::Ready;
        first.edition_item_id = "different-edition".into();
        assert_eq!(
            choose(&[VersionCandidate {
                version: &first,
                equivalence: Some(&evidence),
                available_binding_ids: &[]
            }]),
            Err(IdentityError::VersionScopeMismatch)
        );
        first.edition_item_id = "edition-1".into();
        first.timeline_id = "different-cut".into();
        assert_eq!(
            choose(&[VersionCandidate {
                version: &first,
                equivalence: Some(&evidence),
                available_binding_ids: &[]
            }]),
            Err(IdentityError::VersionScopeMismatch)
        );
        first.timeline_id = "timeline-1".into();
        assert_eq!(
            select_complete_version(
                "edition-1",
                "timeline-1",
                interval(0, 21),
                &[VersionCandidate {
                    version: &first,
                    equivalence: Some(&evidence),
                    available_binding_ids: &[]
                }]
            ),
            Err(IdentityError::VersionScopeMismatch)
        );
    }

    #[test]
    fn selection_checks_dedicated_owners_and_global_binding_identity() {
        let first = version();
        let mut second = alternative();
        let evidence = evidence();
        let select = |second: &VersionContract| {
            choose(&[
                VersionCandidate {
                    version: &first,
                    equivalence: Some(&evidence),
                    available_binding_ids: &[],
                },
                VersionCandidate {
                    version: second,
                    equivalence: Some(&evidence),
                    available_binding_ids: &[],
                },
            ])
        };
        second.version_id = first.version_id.clone();
        assert_eq!(select(&second), Err(IdentityError::DuplicateVersion));
        second = alternative();
        second.association_item_id = first.association_item_id.clone();
        assert_eq!(select(&second), Err(IdentityError::DuplicateVersionOwner));
        second = alternative();
        second.bindings[0].binding_id = first.bindings[0].binding_id.clone();
        assert_eq!(select(&second), Err(IdentityError::DuplicateBinding));
        second = alternative();
        second.bindings[0].association_id = first.bindings[0].association_id.clone();
        assert_eq!(select(&second), Err(IdentityError::DuplicateAssociation));
    }

    #[test]
    fn ready_provisional_version_is_explicitly_selectable_without_equivalence() {
        let admission = admit_timeline(&TimelineAdmissionRequest {
            contract_version: IDENTITY_CONTRACT_VERSION,
            candidate_timeline_id: Some("known-timeline".into()),
            provisional_timeline_id: "unknown-encode-timeline".into(),
            evidence: None,
        })
        .unwrap();
        let TimelineAdmission::Provisional { timeline_id } = admission else {
            panic!("unknown equivalence must remain provisional");
        };
        let mut version = version();
        version.timeline_id = timeline_id;
        let all = version
            .bindings
            .iter()
            .map(|b| b.binding_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            select_explicit_version("version-1", &version, &all),
            Ok(true)
        );
        assert_eq!(
            select_explicit_version("version-1", &version, &all[..1]),
            Ok(false)
        );
        assert_eq!(
            select_explicit_version("different-version", &version, &all),
            Err(IdentityError::VersionScopeMismatch)
        );
        assert_eq!(
            select_complete_version(
                "edition-1",
                &version.timeline_id,
                interval(0, 20),
                &[VersionCandidate {
                    version: &version,
                    equivalence: None,
                    available_binding_ids: &all
                }]
            ),
            Err(IdentityError::MissingEquivalenceEvidence)
        );
        version.state = VersionState::Draft;
        assert_eq!(
            select_explicit_version("version-1", &version, &all),
            Err(IdentityError::VersionNotReady)
        );
    }
}
