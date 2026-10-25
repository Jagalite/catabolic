// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Pure, bounded NFO refresh decisions. A snapshot is data, NOT authorization
//! or proof of completeness: the native owner must obtain the complete eligible
//! file list, all prior origins, relevant identity claims and independently
//! authorized heads in one fenced snapshot, then revalidate them on execution.
//! Apply every effect and the sole receipt in one transaction or apply none.
//! Accepted pins and retired metadata are never rewritten by this planner.

use super::{
    MAX_METADATA_DOCUMENT_BYTES, MAX_METADATA_IDENTIFIER_BYTES, MAX_METADATA_JSON_DEPTH,
    MAX_METADATA_JSON_NODES, MAX_METADATA_SNAPSHOT_BYTES, MAX_METADATA_V2_EXTERNAL_FIELDS,
    MAX_METADATA_V2_FIELD_BYTES, MetadataError,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

mod presence;
pub use presence::{
    PRESENCE_CONTRACT_VERSION, PresenceChange, PresenceEffect, PresenceError, PresenceHeadEffect,
    PresencePlan, PresenceRequest, PresenceSnapshot, PresenceStamp, PublicNfoDocument,
    SourcePresence, plan_presence, public_nfo_document,
};

mod v2;
pub use v2::{
    CONTRACT_VERSION_V2, EligibilityDispositionV2, FileAvailability, FileRevalidation,
    MissingEvidence, ObservationEligibilityV2, ObservationOutcomeV2, ObservationResultV2,
    RefreshCompletionError, RefreshCompletionMode, RefreshCounts, RefreshPlanV2, RefreshProgress,
    RefreshRequestV2, RefreshSnapshotV2, RefreshV2Error, RevalidatedFile, SkipReason, SourceState,
    classify_refresh_v2, plan_refresh_v2, require_refresh_completion,
};

pub const CONTRACT_VERSION: u32 = 1;
pub const MAX_IDENTITY_BYTES: usize = 1024;
pub const MAX_TAGS: usize = 100;
pub const MAX_TAG_BYTES: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fence {
    pub catalog_id: String,
    pub incarnation: String,
    pub generation: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileKey {
    pub profile: String,
    pub file_id: String,
}

/// Captured native facts, not caller assertions of filesystem access. The owner
/// establishes root custody and exact observation/binding/association digests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSnapshot {
    /// Stable, unique ascending adapter file identity; preserves legacy ordering
    /// even when imported native file IDs have a different lexical order.
    pub order_key: String,
    pub file: FileKey,
    pub subject: String,
    pub source_id: String,
    pub relative_path: String,
    pub exact_revision: String,
    pub binding_fingerprint: String,
    pub ownership_fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OriginProvenance {
    Observed {
        exact_revision: String,
        binding_fingerprint: String,
        ownership_fingerprint: String,
    },
    Imported {
        import_id: String,
        source_occurrence_id: String,
        mapping_digest: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileOrigin {
    pub file: FileKey,
    pub provenance: OriginProvenance,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contribution {
    pub fields: BTreeMap<String, Value>,
    pub tags: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DocumentState {
    Present {
        contribution: Contribution,
        external_identity: Option<String>,
    },
    /// Retained source revision, not deletion/recreation at revision zero.
    Withdrawn,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentHead {
    pub revision: i64,
    pub document: DocumentState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttributionHead {
    pub revision: i64,
    pub document_revision: i64,
    pub origin: Option<FileOrigin>,
    /// Automatic claim, deliberately separate from accepted identity pins.
    pub active_identity: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PinHead {
    NeverDecided,
    /// None is a retained explicit removal, not a never-existing decision.
    Decision {
        revision: i64,
        value: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubjectState {
    Active {
        item_revision: i64,
    },
    Retired {
        item_revision: i64,
        retirement_revision: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectHead {
    pub subject_state: SubjectState,
    pub schema_fingerprint: Option<String>,
    pub system_fields: BTreeSet<String>,
    pub source: Option<DocumentHead>,
    pub attribution: Option<AttributionHead>,
    pub pin: PinHead,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadStamp {
    pub subject_state: SubjectState,
    pub schema_fingerprint: Option<String>,
    pub source_revision: Option<i64>,
    pub attribution_revision: Option<i64>,
    pub pin: PinHead,
}
impl SubjectHead {
    pub fn stamp(&self) -> HeadStamp {
        HeadStamp {
            subject_state: self.subject_state.clone(),
            schema_fingerprint: self.schema_fingerprint.clone(),
            source_revision: self.source.as_ref().map(|head| head.revision),
            attribution_revision: self.attribution.as_ref().map(|head| head.revision),
            pin: self.pin.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdentityClaim {
    Available,
    Subject {
        subject: String,
    },
    /// Collision existence suffices; never disclose an unauthorized owner.
    OutsideClosure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshSnapshot {
    pub contract_version: u32,
    pub fence: Fence,
    pub subject: String,
    /// Complete eligible file set, strictly ordered by order_key.
    pub files: Vec<FileSnapshot>,
    /// Target plus every prior owner for these exact files, including retired
    /// owners. The database owner, not this serializable type, proves closure.
    pub subjects: BTreeMap<String, SubjectHead>,
    /// Exact relevant identity set: all active claims above plus every incoming
    /// identity. Available entries explicitly prove absence in the snapshot.
    pub identity_claims: BTreeMap<String, IdentityClaim>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SidecarObservation {
    Absent,
    Parsed {
        contribution: Contribution,
        /// The adapter supplies the parser's first external ID, without sorting
        /// or silently choosing a different provider after a collision.
        external_identity: Option<String>,
    },
    Unusable {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub expected_file: FileSnapshot,
    pub sidecar: SidecarObservation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshRequest {
    pub contract_version: u32,
    pub fence: Fence,
    pub subject: String,
    pub expected_heads: BTreeMap<String, HeadStamp>,
    pub observations: Vec<Observation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdentityDecision {
    NotSupplied,
    Claimed { value: String },
    Collision { value: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DocumentEffect {
    /// Includes origin-only moves and every retired-owner release. The store
    /// MUST NOT write metadata source/history/projection for this effect.
    Retain,
    Replace {
        head: DocumentHead,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectReason {
    Publish,
    Withdraw,
    ReleaseRetiredAttribution,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshEffect {
    pub subject: String,
    pub file: FileKey,
    /// For sequential effects on one subject, revisions refer to the preceding
    /// effect. Item/schema fences still name the original admitted snapshot.
    pub expected: HeadStamp,
    pub reason: EffectReason,
    pub document: DocumentEffect,
    pub attribution: AttributionHead,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubjectOutcome {
    Pinned {
        subject: String,
        pin_revision: i64,
        value: String,
    },
    Unchanged {
        subject: String,
    },
    Changed {
        effect_index: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationDisposition {
    Absent,
    Parsed { identity: IdentityDecision },
    TargetPinned { pin_revision: i64, value: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationOutcome {
    pub file: FileKey,
    pub disposition: ObservationDisposition,
    pub subjects: Vec<SubjectOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshPlan {
    pub contract_version: u32,
    pub fence: Fence,
    pub observations: Vec<ObservationOutcome>,
    pub effects: Vec<RefreshEffect>,
    pub final_heads: BTreeMap<String, SubjectHead>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshError {
    UnsupportedContract,
    EmptyObservations,
    InvalidSnapshot,
    InvalidInput,
    IncompleteObservations,
    UnusableObservation,
    StaleFence,
    StaleFile,
    StaleHead { subject: String },
    RetiredTarget,
    RevisionExhausted,
    WorkBudget,
    Metadata(MetadataError),
}
impl From<MetadataError> for RefreshError {
    fn from(value: MetadataError) -> Self {
        Self::Metadata(value)
    }
}
impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NFO refresh refused: {self:?}")
    }
}
impl std::error::Error for RefreshError {}
pub type Result<T> = std::result::Result<T, RefreshError>;

/// No effects escape on any failed precondition or budget. This does not replace
/// document schema validation, source authorization, current root custody or the
/// enclosing database owner's exact all-or-nothing transaction.
pub fn plan_refresh(snapshot: &RefreshSnapshot, request: &RefreshRequest) -> Result<RefreshPlan> {
    validate(snapshot, request)?;
    let mut input = SizeBudget::default();
    input.charge(snapshot)?;
    input.charge(request)?;
    let mut planner = Planner::new(snapshot);
    let mut observations = Vec::new();
    for observation in &request.observations {
        let outcome = planner.observe(&request.subject, observation)?;
        planner.output.charge(&outcome)?;
        observations.push(outcome);
    }
    planner.output.charge(&planner.heads)?;
    let plan = RefreshPlan {
        contract_version: CONTRACT_VERSION,
        fence: snapshot.fence.clone(),
        observations,
        effects: planner.effects,
        final_heads: planner.heads,
    };
    // Includes outer collection/field overhead, independently of input bounds.
    SizeBudget::default().charge(&plan)?;
    Ok(plan)
}

struct Planner {
    heads: BTreeMap<String, SubjectHead>,
    claims: BTreeMap<String, IdentityClaim>,
    origins: BTreeMap<FileKey, BTreeSet<String>>,
    effects: Vec<RefreshEffect>,
    output: SizeBudget,
    work: usize,
}
impl Planner {
    fn new(snapshot: &RefreshSnapshot) -> Self {
        let mut planner = Self {
            heads: snapshot.subjects.clone(),
            claims: snapshot.identity_claims.clone(),
            origins: BTreeMap::new(),
            effects: vec![],
            output: SizeBudget::default(),
            work: 0,
        };
        for (subject, head) in &planner.heads {
            if let Some(origin) = head.attribution.as_ref().and_then(|a| a.origin.as_ref()) {
                planner
                    .origins
                    .entry(origin.file.clone())
                    .or_default()
                    .insert(subject.clone());
            }
        }
        planner
    }
    fn observe(&mut self, subject: &str, observation: &Observation) -> Result<ObservationOutcome> {
        self.step()?;
        let file = &observation.expected_file;
        let mut outcomes = Vec::new();
        let disposition = match &observation.sidecar {
            SidecarObservation::Unusable { .. } => return Err(RefreshError::UnusableObservation),
            SidecarObservation::Absent => {
                self.withdraw(&file.file, None, &mut outcomes)?;
                ObservationDisposition::Absent
            }
            SidecarObservation::Parsed {
                contribution,
                external_identity,
            } => {
                if let Some((revision, value)) =
                    target_pin_mismatch(&self.heads[subject].pin, external_identity.as_deref())
                {
                    ObservationDisposition::TargetPinned {
                        pin_revision: revision,
                        value: value.into(),
                    }
                } else {
                    self.withdraw(&file.file, Some(subject), &mut outcomes)?;
                    let identity = self.identity(subject, external_identity.as_deref())?;
                    let accepted = match &identity {
                        IdentityDecision::Claimed { value } => Some(value.clone()),
                        _ => None,
                    };
                    let before = &self.heads[subject];
                    let document = DocumentState::Present {
                        contribution: contribution.clone(),
                        external_identity: accepted.clone(),
                    };
                    let source = if before
                        .source
                        .as_ref()
                        .is_some_and(|head| head.document == document)
                    {
                        before.source.clone().expect("existing equal document")
                    } else {
                        DocumentHead {
                            revision: advance(
                                before.source.as_ref().map_or(0, |head| head.revision),
                            )?,
                            document,
                        }
                    };
                    let origin = FileOrigin {
                        file: file.file.clone(),
                        provenance: OriginProvenance::Observed {
                            exact_revision: file.exact_revision.clone(),
                            binding_fingerprint: file.binding_fingerprint.clone(),
                            ownership_fingerprint: file.ownership_fingerprint.clone(),
                        },
                    };
                    outcomes.push(self.change(
                        subject,
                        &file.file,
                        source,
                        Some(origin),
                        accepted,
                        EffectReason::Publish,
                    )?);
                    ObservationDisposition::Parsed { identity }
                }
            }
        };
        let outcome = ObservationOutcome {
            file: file.file.clone(),
            disposition,
            subjects: outcomes,
        };
        Ok(outcome)
    }

    fn step(&mut self) -> Result<()> {
        self.work = self.work.checked_add(1).ok_or(RefreshError::WorkBudget)?;
        if self.work > MAX_METADATA_JSON_NODES {
            return Err(RefreshError::WorkBudget);
        }
        Ok(())
    }
    fn identity(&mut self, subject: &str, incoming: Option<&str>) -> Result<IdentityDecision> {
        self.step()?;
        let Some(value) = incoming else {
            return Ok(IdentityDecision::NotSupplied);
        };
        match self
            .claims
            .get(value)
            .ok_or(RefreshError::InvalidSnapshot)?
        {
            IdentityClaim::Available => Ok(IdentityDecision::Claimed {
                value: value.into(),
            }),
            IdentityClaim::Subject { subject: owner } if owner == subject => {
                Ok(IdentityDecision::Claimed {
                    value: value.into(),
                })
            }
            _ => Ok(IdentityDecision::Collision {
                value: value.into(),
            }),
        }
    }
    fn withdraw(
        &mut self,
        file: &FileKey,
        keep: Option<&str>,
        outcomes: &mut Vec<SubjectOutcome>,
    ) -> Result<()> {
        let owners = self.origins.get(file).cloned().unwrap_or_default();
        for subject in owners {
            self.step()?;
            if Some(subject.as_str()) == keep {
                continue;
            }
            let head = &self.heads[&subject];
            if let Some((revision, value)) = pinned(&head.pin) {
                outcomes.push(SubjectOutcome::Pinned {
                    subject: subject.clone(),
                    pin_revision: revision,
                    value: value.into(),
                });
                continue;
            }
            let current = head.source.as_ref().ok_or(RefreshError::InvalidSnapshot)?;
            let retired = matches!(head.subject_state, SubjectState::Retired { .. });
            let source = if retired {
                current.clone()
            } else {
                DocumentHead {
                    revision: advance(current.revision)?,
                    document: DocumentState::Withdrawn,
                }
            };
            let reason = if retired {
                EffectReason::ReleaseRetiredAttribution
            } else {
                EffectReason::Withdraw
            };
            outcomes.push(self.change(&subject, file, source, None, None, reason)?);
        }
        Ok(())
    }
    fn change(
        &mut self,
        subject: &str,
        file: &FileKey,
        source: DocumentHead,
        origin: Option<FileOrigin>,
        identity: Option<String>,
        reason: EffectReason,
    ) -> Result<SubjectOutcome> {
        self.step()?;
        let before = &self.heads[subject];
        if before.source.as_ref() == Some(&source)
            && before.attribution.as_ref().is_some_and(|a| {
                a.origin == origin
                    && a.active_identity == identity
                    && a.document_revision == source.revision
            })
        {
            return Ok(SubjectOutcome::Unchanged {
                subject: subject.into(),
            });
        }
        let attribution = AttributionHead {
            revision: advance(before.attribution.as_ref().map_or(0, |a| a.revision))?,
            document_revision: source.revision,
            origin,
            active_identity: identity,
        };
        let effect = RefreshEffect {
            subject: subject.into(),
            file: file.clone(),
            expected: before.stamp(),
            reason,
            document: if before.source.as_ref() == Some(&source) {
                DocumentEffect::Retain
            } else {
                DocumentEffect::Replace {
                    head: source.clone(),
                }
            },
            attribution: attribution.clone(),
        };
        self.output.charge(&effect)?;
        if let Some(old) = &before.attribution {
            if let Some(identity) = &old.active_identity {
                self.claims
                    .insert(identity.clone(), IdentityClaim::Available);
            }
            if let Some(origin) = &old.origin
                && let Some(owners) = self.origins.get_mut(&origin.file)
            {
                owners.remove(subject);
            }
        }
        if let Some(identity) = &attribution.active_identity {
            self.claims.insert(
                identity.clone(),
                IdentityClaim::Subject {
                    subject: subject.into(),
                },
            );
        }
        if let Some(origin) = &attribution.origin {
            self.origins
                .entry(origin.file.clone())
                .or_default()
                .insert(subject.into());
        }
        let head = self.heads.get_mut(subject).expect("validated subject");
        head.source = Some(source);
        head.attribution = Some(attribution);
        let index = self.effects.len();
        self.effects.push(effect);
        Ok(SubjectOutcome::Changed {
            effect_index: index,
        })
    }
}

fn advance(revision: i64) -> Result<i64> {
    revision
        .checked_add(1)
        .ok_or(RefreshError::RevisionExhausted)
}
fn pinned(head: &PinHead) -> Option<(i64, &str)> {
    match head {
        PinHead::Decision {
            revision,
            value: Some(value),
        } => Some((*revision, value)),
        _ => None,
    }
}
/// A target pin mismatch is processed without publication or prior-owner
/// withdrawal. Pins are not mutated by refresh, so the same predicate is safe
/// for pre-normalization eligibility and sequential effect planning.
fn target_pin_mismatch<'a>(head: &'a PinHead, incoming: Option<&str>) -> Option<(i64, &'a str)> {
    pinned(head).filter(|(_, value)| incoming != Some(*value))
}

fn id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_METADATA_IDENTIFIER_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(RefreshError::InvalidInput);
    }
    Ok(())
}
fn digest(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(RefreshError::InvalidInput);
    }
    Ok(())
}
fn external(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_IDENTITY_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(RefreshError::InvalidInput);
    }
    Ok(())
}
fn field(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_METADATA_V2_FIELD_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(MetadataError::InvalidField.into());
    }
    Ok(())
}
fn file_key(key: &FileKey) -> Result<()> {
    id(&key.profile)?;
    id(&key.file_id)
}

fn contribution(value: &Contribution, reserved: &BTreeSet<String>) -> Result<()> {
    if value.fields.len() > MAX_METADATA_V2_EXTERNAL_FIELDS {
        return Err(MetadataError::FieldBudget.into());
    }
    if value.tags.len() > MAX_TAGS {
        return Err(RefreshError::InvalidInput);
    }
    let mut nodes = 1usize;
    for (name, value) in &value.fields {
        field(name)?;
        if reserved.contains(name) {
            return Err(MetadataError::ReservedField.into());
        }
        // Bound recursive serialization before serializing any typed snapshot.
        let mut pending = vec![(value, 1usize)];
        while let Some((value, depth)) = pending.pop() {
            nodes += 1;
            if nodes > MAX_METADATA_JSON_NODES {
                return Err(MetadataError::JsonNodeBudget.into());
            }
            if depth > MAX_METADATA_JSON_DEPTH {
                return Err(MetadataError::JsonDepthBudget.into());
            }
            // Reject oversized scalar/key buffers before a serializer scans
            // their entire contents looking for escapes.
            if matches!(value, Value::String(text) if text.len() > MAX_METADATA_DOCUMENT_BYTES)
                || matches!(value, Value::Object(fields) if fields.keys().any(|key| key.len() > MAX_METADATA_DOCUMENT_BYTES))
            {
                return Err(MetadataError::DocumentBudget.into());
            }
            let children = match value {
                Value::Array(a) => a.len(),
                Value::Object(o) => o.len(),
                _ => 0,
            };
            if pending.len().saturating_add(children).saturating_add(nodes)
                > MAX_METADATA_JSON_NODES
            {
                return Err(MetadataError::JsonNodeBudget.into());
            }
            match value {
                Value::Array(a) => pending.extend(a.iter().map(|v| (v, depth + 1))),
                Value::Object(o) => pending.extend(o.values().map(|v| (v, depth + 1))),
                _ => {}
            }
        }
    }
    for tag in &value.tags {
        if tag.is_empty()
            || tag.len() > MAX_TAG_BYTES
            || tag.trim() != tag
            || tag.chars().any(char::is_control)
        {
            return Err(RefreshError::InvalidInput);
        }
    }
    let raw = encoded(
        &value.fields,
        MAX_METADATA_DOCUMENT_BYTES,
        MetadataError::DocumentBudget,
    )?;
    super::replacement_fields(&raw)?;
    Ok(())
}

fn validate(snapshot: &RefreshSnapshot, request: &RefreshRequest) -> Result<()> {
    validate_input(snapshot, request, false)
}

/// V2 alone permits an empty complete set and unusable sidecars. All document,
/// head, identity and closure checks remain common; v1 calls the strict branch.
fn validate_input(
    snapshot: &RefreshSnapshot,
    request: &RefreshRequest,
    partial_sidecars: bool,
) -> Result<()> {
    if snapshot.contract_version != CONTRACT_VERSION || request.contract_version != CONTRACT_VERSION
    {
        return Err(RefreshError::UnsupportedContract);
    }
    if !partial_sidecars && (snapshot.files.is_empty() || request.observations.is_empty()) {
        return Err(RefreshError::EmptyObservations);
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
    id(&snapshot.fence.catalog_id)?;
    id(&snapshot.fence.incarnation)?;
    id(&snapshot.subject)?;
    if snapshot.fence.generation < 0 {
        return Err(RefreshError::InvalidSnapshot);
    }
    if request.fence != snapshot.fence || request.subject != snapshot.subject {
        return Err(RefreshError::StaleFence);
    }
    if snapshot.files.len() != request.observations.len() {
        return Err(RefreshError::IncompleteObservations);
    }
    // Incremental checks stop before traversing a large collection of otherwise
    // valid individual documents. The caller also charges the exact two outer
    // serialized structures, including all field/key overhead.
    let mut incremental = SizeBudget::default();
    let mut files = BTreeSet::new();
    let mut previous: Option<&str> = None;
    for (file, observation) in snapshot.files.iter().zip(&request.observations) {
        id(&file.order_key)?;
        file_key(&file.file)?;
        id(&file.source_id)?;
        digest(&file.exact_revision)?;
        digest(&file.binding_fingerprint)?;
        digest(&file.ownership_fingerprint)?;
        if file.subject != snapshot.subject
            || file.relative_path.is_empty()
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
        if !files.insert(file.file.clone())
            || previous.is_some_and(|key| key >= file.order_key.as_str())
        {
            return Err(RefreshError::InvalidSnapshot);
        }
        previous = Some(&file.order_key);
        if observation.expected_file != *file {
            return Err(RefreshError::StaleFile);
        }
        if !partial_sidecars && matches!(observation.sidecar, SidecarObservation::Unusable { .. }) {
            return Err(RefreshError::UnusableObservation);
        }
        incremental.charge(file)?;
    }
    let target = snapshot
        .subjects
        .get(&snapshot.subject)
        .ok_or(RefreshError::InvalidSnapshot)?;
    if matches!(target.subject_state, SubjectState::Retired { .. }) {
        return Err(RefreshError::RetiredTarget);
    }
    if snapshot.subjects.len() != request.expected_heads.len() {
        return Err(RefreshError::InvalidSnapshot);
    }
    let mut identities = BTreeSet::new();
    for (subject, head) in &snapshot.subjects {
        id(subject)?;
        match head.subject_state {
            SubjectState::Active { item_revision } if item_revision > 0 => {}
            SubjectState::Retired {
                item_revision,
                retirement_revision,
            } if item_revision > 0 && retirement_revision > 0 => {}
            _ => return Err(RefreshError::InvalidSnapshot),
        }
        if let Some(fingerprint) = &head.schema_fingerprint {
            digest(fingerprint)?;
        }
        if head.system_fields.len() > MAX_METADATA_JSON_NODES {
            return Err(MetadataError::JsonNodeBudget.into());
        }
        for name in &head.system_fields {
            field(name)?;
        }
        if let PinHead::Decision { revision, value } = &head.pin {
            if *revision < 1 {
                return Err(RefreshError::InvalidSnapshot);
            }
            if let Some(value) = value {
                external(value)?;
            }
        }
        if request.expected_heads.get(subject) != Some(&head.stamp()) {
            return Err(RefreshError::StaleHead {
                subject: subject.clone(),
            });
        }
        match (&head.source, &head.attribution) {
            (None, None) => {}
            (Some(source), Some(attribution))
                if source.revision > 0
                    && attribution.revision > 0
                    && attribution.document_revision == source.revision =>
            {
                match &source.document {
                    DocumentState::Present {
                        contribution: document,
                        external_identity,
                    } => {
                        contribution(document, &head.system_fields)?;
                        if let Some(value) = external_identity {
                            external(value)?;
                        }
                        if attribution.active_identity.is_some()
                            && attribution.active_identity != *external_identity
                        {
                            return Err(RefreshError::InvalidSnapshot);
                        }
                        if matches!(head.subject_state, SubjectState::Active { .. })
                            && attribution.active_identity != *external_identity
                        {
                            return Err(RefreshError::InvalidSnapshot);
                        }
                    }
                    DocumentState::Withdrawn
                        if attribution.origin.is_none()
                            && attribution.active_identity.is_none() => {}
                    _ => return Err(RefreshError::InvalidSnapshot),
                }
                if let Some(origin) = &attribution.origin {
                    file_key(&origin.file)?;
                    match &origin.provenance {
                        OriginProvenance::Observed {
                            exact_revision,
                            binding_fingerprint,
                            ownership_fingerprint,
                        } => {
                            digest(exact_revision)?;
                            digest(binding_fingerprint)?;
                            digest(ownership_fingerprint)?;
                        }
                        OriginProvenance::Imported {
                            import_id,
                            source_occurrence_id,
                            mapping_digest,
                        } => {
                            id(import_id)?;
                            id(source_occurrence_id)?;
                            digest(mapping_digest)?;
                        }
                    }
                }
                if let Some(value) = &attribution.active_identity {
                    external(value)?;
                    identities.insert(value.clone());
                    if snapshot.identity_claims.get(value)
                        != Some(&IdentityClaim::Subject {
                            subject: subject.clone(),
                        })
                    {
                        return Err(RefreshError::InvalidSnapshot);
                    }
                }
            }
            _ => return Err(RefreshError::InvalidSnapshot),
        }
        if subject != &snapshot.subject
            && !head
                .attribution
                .as_ref()
                .and_then(|a| a.origin.as_ref())
                .is_some_and(|origin| files.contains(&origin.file))
        {
            return Err(RefreshError::InvalidSnapshot);
        }
        incremental.charge(head)?;
    }
    for observation in &request.observations {
        if let SidecarObservation::Parsed {
            contribution: document,
            external_identity,
        } = &observation.sidecar
        {
            contribution(document, &target.system_fields)?;
            if let Some(value) = external_identity {
                external(value)?;
                identities.insert(value.clone());
            }
        }
        incremental.charge(observation)?;
    }
    if identities.len() != snapshot.identity_claims.len() {
        return Err(RefreshError::InvalidSnapshot);
    }
    for value in identities {
        match snapshot
            .identity_claims
            .get(&value)
            .ok_or(RefreshError::InvalidSnapshot)?
        {
            IdentityClaim::Subject { subject }
                if snapshot
                    .subjects
                    .get(subject)
                    .and_then(|h| h.attribution.as_ref())
                    .and_then(|a| a.active_identity.as_ref())
                    != Some(&value) =>
            {
                return Err(RefreshError::InvalidSnapshot);
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Default)]
struct SizeBudget {
    bytes: usize,
    nodes: usize,
}
impl SizeBudget {
    fn charge<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let left = MAX_METADATA_SNAPSHOT_BYTES.saturating_sub(self.bytes);
        let raw = encoded(value, left, MetadataError::SnapshotBudget)?;
        let parsed = super::bounded_json(&raw, left, MetadataError::SnapshotBudget)?;
        let nodes = super::schema_evolution::json_nodes(&parsed)?;
        self.bytes = self
            .bytes
            .checked_add(raw.len())
            .ok_or(MetadataError::SnapshotBudget)?;
        self.nodes = self
            .nodes
            .checked_add(nodes)
            .ok_or(MetadataError::JsonNodeBudget)?;
        if self.nodes > MAX_METADATA_JSON_NODES {
            return Err(MetadataError::JsonNodeBudget.into());
        }
        Ok(())
    }
}
fn encoded<T: Serialize>(value: &T, maximum: usize, failure: MetadataError) -> Result<String> {
    struct Limited {
        bytes: Vec<u8>,
        maximum: usize,
        overflow: bool,
    }
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
                self.overflow = true;
                return Err(std::io::Error::other("NFO serialization budget"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Limited {
        bytes: Vec::new(),
        maximum,
        overflow: false,
    };
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(if writer.overflow {
            failure
        } else {
            MetadataError::InvalidJson
        }
        .into());
    }
    String::from_utf8(writer.bytes).map_err(|_| MetadataError::InvalidJson.into())
}

#[cfg(test)]
mod tests;
