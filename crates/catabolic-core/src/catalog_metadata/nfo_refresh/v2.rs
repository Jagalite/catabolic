// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Partial-sidecar refresh is an explicit v2 contract, not relaxed v1 input.
//! These serializable observations describe facts; they grant no authority and
//! do not prove lookup completeness. The owner must independently establish the
//! complete original observation list, current authorization and all terminal
//! lookup/custody facts. Every effect and the sole receipt remain atomic.

use super::*;

pub const CONTRACT_VERSION_V2: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAvailability {
    Available,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    Enabled,
    Disabled,
}

/// Missing evidence is a refusal, not evidence that a previously observed file
/// disappeared. In particular, a failed authorization check cannot become a
/// harmless skipped observation or authorize a successful refresh receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingEvidence {
    Authorization,
    Ownership,
    SourceCustody,
    CompleteSelection,
    CompleteLookup,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileRevalidation {
    /// Current facts for the same stable file key and ordering identity. Changed
    /// subject/ownership, path, binding or revision are evidenced stale skips.
    Present {
        current: FileSnapshot,
        availability: FileAvailability,
        source_state: SourceState,
    },
    /// A complete, independently authorized terminal lookup found no such file.
    /// This is NOT SidecarObservation::Absent and MUST NOT withdraw anything.
    Missing {
        lookup_digest: String,
    },
    Unproven {
        reason: MissingEvidence,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevalidatedFile {
    /// Exact member of the original complete observation set, not a newly
    /// discovered replacement. New files are left for a later refresh attempt.
    pub observed_file: FileSnapshot,
    pub current: FileRevalidation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshSnapshotV2 {
    pub contract_version: u32,
    pub fence: Fence,
    pub subject: String,
    /// Complete ORIGINAL observation set, strictly ascending by order_key.
    /// The original set may be empty. Each member has terminal revalidation.
    pub files: Vec<RevalidatedFile>,
    /// Target plus all prior origins of the original files, including skipped
    /// files and retired owners. Each requires independent owner authorization.
    pub subjects: BTreeMap<String, SubjectHead>,
    /// Includes every active claim and every parsed incoming identity, even if
    /// its observation will be skipped. This is descriptive, not a grant.
    pub identity_claims: BTreeMap<String, IdentityClaim>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshRequestV2 {
    pub contract_version: u32,
    pub fence: Fence,
    pub subject: String,
    pub expected_heads: BTreeMap<String, HeadStamp>,
    /// Exactly one observation for each original file, in the same order.
    pub observations: Vec<Observation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum SkipReason {
    Unusable { detail: String },
    FileMissing,
    FileUnavailable,
    SourceDisabled,
    OwnershipChanged,
    BindingChanged,
    PathChanged,
    RevisionChanged,
}

/// Descriptive pre-normalization eligibility, never a grant or a predicted
/// write. Only Parsed observations need schema/value normalization before the
/// owner invokes the effect planner again with the normalized contribution.
/// Absent still runs per-prior-owner pin/retirement rules during planning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "eligibility", rename_all = "snake_case")]
pub enum EligibilityDispositionV2 {
    Skipped { reason: SkipReason },
    Absent,
    Parsed,
    TargetPinned { pin_revision: i64, value: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ObservationEligibilityV2 {
    pub file: FileKey,
    pub disposition: EligibilityDispositionV2,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationResultV2 {
    /// Processed counts an admitted call to the publication rule, not a write.
    /// In particular TargetPinned and an identical document are processed.
    Processed {
        disposition: ObservationDisposition,
        subjects: Vec<SubjectOutcome>,
    },
    /// No source, history, attribution, identity or pin effect for this row.
    Skipped { reason: SkipReason },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationOutcomeV2 {
    pub file: FileKey,
    pub result: ObservationResultV2,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshCounts {
    pub observed: usize,
    pub processed: usize,
    pub skipped: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshProgress {
    /// Empty and all-skipped batches must not be reported as a successful
    /// metadata refresh by a job adapter merely because planning returned Ok.
    NoObservationsProcessed,
    Processed,
}

/// Selected by the owner from its independently authorized operation, not a
/// caller flag. Neither mode changes existing v2 planning/wire semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshCompletionMode {
    DirectRefresh,
    MetadataJob,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshCompletionError {
    InvalidCounts,
    IncompleteJob { counts: RefreshCounts },
}
impl std::fmt::Display for RefreshCompletionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NFO refresh completion refused: {self:?}")
    }
}
impl std::error::Error for RefreshCompletionError {}

/// Check the counts of the freshly computed plan BEFORE any metadata, identity,
/// attribution or receipt effect. Direct refresh can process a partial set and
/// explicitly reports zero progress; a metadata job requires a nonempty fully
/// processed set. A pin no-op or unchanged document still counts as processed.
/// This checks completion policy, not custody, authorization or count provenance.
pub fn require_refresh_completion(
    mode: RefreshCompletionMode,
    counts: &RefreshCounts,
) -> std::result::Result<RefreshProgress, RefreshCompletionError> {
    if counts.processed.checked_add(counts.skipped) != Some(counts.observed) {
        return Err(RefreshCompletionError::InvalidCounts);
    }
    if mode == RefreshCompletionMode::MetadataJob
        && (counts.observed == 0 || counts.skipped != 0 || counts.processed != counts.observed)
    {
        return Err(RefreshCompletionError::IncompleteJob {
            counts: counts.clone(),
        });
    }
    Ok(if counts.processed == 0 {
        RefreshProgress::NoObservationsProcessed
    } else {
        RefreshProgress::Processed
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshPlanV2 {
    pub contract_version: u32,
    pub fence: Fence,
    pub progress: RefreshProgress,
    pub counts: RefreshCounts,
    pub observations: Vec<ObservationOutcomeV2>,
    pub effects: Vec<RefreshEffect>,
    pub final_heads: BTreeMap<String, SubjectHead>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshV2Error {
    Core(RefreshError),
    UnprovenObservation {
        file: FileKey,
        reason: MissingEvidence,
    },
}
impl From<RefreshError> for RefreshV2Error {
    fn from(error: RefreshError) -> Self {
        Self::Core(error)
    }
}
impl From<MetadataError> for RefreshV2Error {
    fn from(error: MetadataError) -> Self {
        Self::Core(error.into())
    }
}
impl std::fmt::Display for RefreshV2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NFO refresh v2 refused: {self:?}")
    }
}
impl std::error::Error for RefreshV2Error {}

type V2Result<T> = std::result::Result<T, RefreshV2Error>;

/// Preserve legacy partial-sidecar processing without making partial authority
/// acceptable. All supplied documents remain bounded, including skipped ones.
/// A refusal returns no plan/effects. Skips retain all previous attribution and
/// claims; only a revalidated Present+Absent sidecar can run withdrawal rules.
pub fn plan_refresh_v2(
    snapshot: &RefreshSnapshotV2,
    request: &RefreshRequestV2,
) -> V2Result<RefreshPlanV2> {
    let common_snapshot = validated_snapshot(snapshot, request)?;
    let mut planner = Planner::new(&common_snapshot);
    let mut observations = Vec::with_capacity(request.observations.len());
    let mut counts = RefreshCounts {
        observed: request.observations.len(),
        processed: 0,
        skipped: 0,
    };
    for (file, observation) in snapshot.files.iter().zip(&request.observations) {
        let result = if let Classification::Skipped(reason) = classify_observation(
            file,
            &observation.sidecar,
            &common_snapshot.subjects[&request.subject].pin,
        ) {
            planner.step()?;
            counts.skipped += 1;
            ObservationResultV2::Skipped { reason }
        } else {
            let processed = planner.observe(&request.subject, observation)?;
            counts.processed += 1;
            ObservationResultV2::Processed {
                disposition: processed.disposition,
                subjects: processed.subjects,
            }
        };
        let outcome = ObservationOutcomeV2 {
            file: observation.expected_file.file.clone(),
            result,
        };
        // Charge the v2 wrapper as well as the shared effect accounting. No
        // collection is truncated to make a plan fit its resource budget.
        planner.output.charge(&outcome)?;
        observations.push(outcome);
    }
    planner.output.charge(&planner.heads)?;
    let plan = RefreshPlanV2 {
        contract_version: CONTRACT_VERSION_V2,
        fence: snapshot.fence.clone(),
        progress: if counts.processed == 0 {
            RefreshProgress::NoObservationsProcessed
        } else {
            RefreshProgress::Processed
        },
        counts,
        observations,
        effects: planner.effects,
        final_heads: planner.heads,
    };
    SizeBudget::default().charge(&plan)?;
    Ok(plan)
}

/// Classify the complete ordered observation set without revision arithmetic
/// or speculative effects. In particular, an exhausted source revision does
/// not prevent the owner normalizing a Parsed contribution that may be equal
/// to the retained document. The eventual planner still enforces all revision
/// limits. No caller-supplied classification is accepted by that planner.
///
/// All input bounds, exact heads, closure and terminal custody checks are the
/// same as plan_refresh_v2, including skipped inputs. Unproven is always a
/// refusal, never a skip. The owner must preserve raw evidence separately and
/// replan after normalizing only Parsed contributions, without changing their
/// file facts or incoming identities.
pub fn classify_refresh_v2(
    snapshot: &RefreshSnapshotV2,
    request: &RefreshRequestV2,
) -> V2Result<Vec<ObservationEligibilityV2>> {
    let common = validated_snapshot(snapshot, request)?;
    let pin = &common.subjects[&request.subject].pin;
    let mut output = SizeBudget::default();
    let mut result = Vec::with_capacity(request.observations.len());
    for (file, observation) in snapshot.files.iter().zip(&request.observations) {
        let disposition = match classify_observation(file, &observation.sidecar, pin) {
            Classification::Skipped(reason) => EligibilityDispositionV2::Skipped { reason },
            Classification::Absent => EligibilityDispositionV2::Absent,
            Classification::Parsed => EligibilityDispositionV2::Parsed,
            Classification::TargetPinned { revision, value } => {
                EligibilityDispositionV2::TargetPinned {
                    pin_revision: revision,
                    value: value.into(),
                }
            }
        };
        let eligibility = ObservationEligibilityV2 {
            file: observation.expected_file.file.clone(),
            disposition,
        };
        output.charge(&eligibility)?;
        result.push(eligibility);
    }
    SizeBudget::default().charge(&result)?;
    Ok(result)
}

/// Both entry points validate the exact same complete input before considering
/// a single observation. This private common view does not relax v1 admission.
fn validated_snapshot(
    snapshot: &RefreshSnapshotV2,
    request: &RefreshRequestV2,
) -> V2Result<RefreshSnapshot> {
    preflight(snapshot, request)?;
    // Adapt only the private shared validation view. V1 public constructors,
    // version checks and wire representations still require contract 1.
    let common_snapshot = RefreshSnapshot {
        contract_version: CONTRACT_VERSION,
        fence: snapshot.fence.clone(),
        subject: snapshot.subject.clone(),
        files: snapshot
            .files
            .iter()
            .map(|file| file.observed_file.clone())
            .collect(),
        subjects: snapshot.subjects.clone(),
        identity_claims: snapshot.identity_claims.clone(),
    };
    let common_request = RefreshRequest {
        contract_version: CONTRACT_VERSION,
        fence: request.fence.clone(),
        subject: request.subject.clone(),
        expected_heads: request.expected_heads.clone(),
        observations: request.observations.clone(),
    };
    validate_input(&common_snapshot, &common_request, true)?;
    // Refuse any missing authority/completeness before planning any row, even
    // when its sidecar is unusable and would otherwise be skipped first.
    for file in &snapshot.files {
        if let FileRevalidation::Unproven { reason } = &file.current {
            return Err(RefreshV2Error::UnprovenObservation {
                file: file.observed_file.file.clone(),
                reason: reason.clone(),
            });
        }
    }
    Ok(common_snapshot)
}

enum Classification<'a> {
    Skipped(SkipReason),
    Absent,
    Parsed,
    TargetPinned { revision: i64, value: &'a str },
}

fn classify_observation<'a>(
    file: &RevalidatedFile,
    sidecar: &SidecarObservation,
    pin: &'a PinHead,
) -> Classification<'a> {
    if let Some(reason) = skip_reason(file, sidecar) {
        return Classification::Skipped(reason);
    }
    match sidecar {
        SidecarObservation::Absent => Classification::Absent,
        SidecarObservation::Parsed {
            external_identity, ..
        } => match target_pin_mismatch(pin, external_identity.as_deref()) {
            Some((revision, value)) => Classification::TargetPinned { revision, value },
            None => Classification::Parsed,
        },
        SidecarObservation::Unusable { .. } => unreachable!("unusable sidecars skip first"),
    }
}

fn preflight(snapshot: &RefreshSnapshotV2, request: &RefreshRequestV2) -> V2Result<()> {
    if snapshot.contract_version != CONTRACT_VERSION_V2
        || request.contract_version != CONTRACT_VERSION_V2
    {
        return Err(RefreshError::UnsupportedContract.into());
    }
    if snapshot
        .files
        .len()
        .saturating_add(snapshot.subjects.len())
        .saturating_add(snapshot.identity_claims.len())
        .saturating_add(request.observations.len())
        .saturating_add(request.expected_heads.len())
        > MAX_METADATA_JSON_NODES
    {
        return Err(MetadataError::JsonNodeBudget.into());
    }
    let mut incremental = SizeBudget::default();
    for file in &snapshot.files {
        validate_file(&file.observed_file)?;
        match &file.current {
            FileRevalidation::Present { current, .. } => {
                validate_file(current)?;
                // Different current rows cannot stand in for an exact lookup.
                if current.file != file.observed_file.file
                    || current.order_key != file.observed_file.order_key
                {
                    return Err(RefreshError::InvalidSnapshot.into());
                }
            }
            FileRevalidation::Missing { lookup_digest } => digest(lookup_digest)?,
            FileRevalidation::Unproven { .. } => {}
        }
        incremental.charge(file)?;
    }
    for head in snapshot.subjects.values() {
        if let Some(DocumentHead {
            document:
                DocumentState::Present {
                    contribution: value,
                    ..
                },
            ..
        }) = &head.source
        {
            contribution(value, &head.system_fields)?;
        }
        incremental.charge(head)?;
    }
    let target = snapshot
        .subjects
        .get(&snapshot.subject)
        .ok_or(RefreshError::InvalidSnapshot)?;
    for observation in &request.observations {
        validate_file(&observation.expected_file)?;
        match &observation.sidecar {
            SidecarObservation::Parsed {
                contribution: value,
                ..
            } => contribution(value, &target.system_fields)?,
            SidecarObservation::Unusable { reason } => id(reason)?,
            SidecarObservation::Absent => {}
        }
        incremental.charge(observation)?;
    }
    // The exact combined outer input, not just per-row serialization, has the
    // same byte/node ceiling as v1. This happens before cloning any closure.
    let mut input = SizeBudget::default();
    input.charge(snapshot)?;
    input.charge(request)?;
    Ok(())
}

fn validate_file(file: &FileSnapshot) -> Result<()> {
    id(&file.order_key)?;
    file_key(&file.file)?;
    id(&file.subject)?;
    id(&file.source_id)?;
    digest(&file.exact_revision)?;
    digest(&file.binding_fingerprint)?;
    digest(&file.ownership_fingerprint)?;
    if file.relative_path.is_empty()
        || file.relative_path.len() > 4096
        || file.relative_path.starts_with('/')
        || file
            .relative_path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        || file.relative_path.chars().any(char::is_control)
    {
        return Err(RefreshError::InvalidSnapshot);
    }
    Ok(())
}

fn skip_reason(file: &RevalidatedFile, sidecar: &SidecarObservation) -> Option<SkipReason> {
    // Legacy ordering: unusable sidecar skips before current-file publication.
    // Unproven current facts have already refused, rather than reaching here.
    if let SidecarObservation::Unusable { reason } = sidecar {
        return Some(SkipReason::Unusable {
            detail: reason.clone(),
        });
    }
    match &file.current {
        FileRevalidation::Missing { .. } => Some(SkipReason::FileMissing),
        FileRevalidation::Present {
            current,
            availability,
            source_state,
        } => {
            let observed = &file.observed_file;
            if *availability == FileAvailability::Unavailable {
                Some(SkipReason::FileUnavailable)
            } else if *source_state == SourceState::Disabled {
                Some(SkipReason::SourceDisabled)
            } else if current.subject != observed.subject
                || current.ownership_fingerprint != observed.ownership_fingerprint
            {
                Some(SkipReason::OwnershipChanged)
            } else if current.source_id != observed.source_id
                || current.binding_fingerprint != observed.binding_fingerprint
            {
                Some(SkipReason::BindingChanged)
            } else if current.relative_path != observed.relative_path {
                Some(SkipReason::PathChanged)
            } else if current.exact_revision != observed.exact_revision {
                Some(SkipReason::RevisionChanged)
            } else {
                None
            }
        }
        FileRevalidation::Unproven { .. } => {
            unreachable!("all missing evidence refused before planning")
        }
    }
}

#[cfg(test)]
mod tests;
