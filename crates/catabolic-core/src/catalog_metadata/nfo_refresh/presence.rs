// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent public NFO presence and retained native source history.
//!
//! Motion refresh deletes a withdrawn public document and inserts its next
//! appearance at revision 1. That public revision is NOT a native history head.
//! Refresh skips identical document/identity writes; explicit metadata PUT does
//! not. Inputs here are already validated, normalized source values with any
//! identity collision resolved by the refresh/identity owner. NFO has no local
//! excluded tags. Equality assumes the adapter's canonical contribution encoding,
//! not semantic comparison of arbitrary retained JSON bytes.
//!
//! This descriptive contract provides no authorization, pin bypass, complete
//! lookup, schema grant, or receipt. The owner must independently establish those
//! facts, atomically persist every changed head with its sole receipt, and retain
//! the original exact request across restart. Recover an existing receipt before
//! attempting a new transition; do NOT recapture its expected stamps on retry.
//! Public revision alone retains legacy ABA ambiguity: a newly admitted PUT with
//! revision 1 can address a later incarnation. Only an already captured exact
//! internal stamp is fenced here. No stronger legacy HTTP CAS is claimed.

use super::*;

pub const PRESENCE_CONTRACT_VERSION: u32 = 1;

/// Zero is the never-present baseline. Withdrawal retains both counters; a
/// subsequent insertion advances incarnation and resets ONLY public_revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePresence {
    pub revision: i64,
    pub incarnation: i64,
    pub public_revision: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresenceSnapshot {
    pub contract_version: u32,
    pub fence: Fence,
    pub subject: String,
    /// Exact subject aggregate head, independent of global generation.
    pub aggregate_revision: i64,
    pub head: SubjectHead,
    pub presence: SourcePresence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresenceStamp {
    pub aggregate_revision: i64,
    /// Independent source, attribution, item, schema and pin preconditions.
    pub head: HeadStamp,
    pub presence: SourcePresence,
}
impl PresenceSnapshot {
    pub fn stamp(&self) -> PresenceStamp {
        PresenceStamp {
            aggregate_revision: self.aggregate_revision,
            head: self.head.stamp(),
            presence: self.presence.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PresenceChange {
    /// A successful, already-authorized refresh publication. Identical content
    /// and identity retain document revisions even if origin changes.
    Publish {
        contribution: Contribution,
        external_identity: Option<String>,
        origin: FileOrigin,
    },
    /// Only a prior refresh decision based on a revalidated absent sidecar may
    /// request this. Missing/unproven observations must never become withdrawal.
    Withdraw,
    /// Legacy PUT always advances a present document, including identical data.
    /// It leaves existing nfo_origins untouched; zero creates an absent source.
    PublicPut {
        expected_public_revision: i64,
        contribution: Contribution,
        external_identity: Option<String>,
    },
    /// Retired owners release only attribution/automatic claim, never rewrite
    /// their retained native or public document. Caller proves retirement/pins.
    ReleaseRetiredAttribution,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresenceRequest {
    pub contract_version: u32,
    pub fence: Fence,
    pub subject: String,
    pub expected: PresenceStamp,
    pub change: PresenceChange,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PresenceHeadEffect {
    Retain,
    Replace { head: SourcePresence },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PresenceEffect {
    Unchanged,
    Changed {
        document: DocumentEffect,
        presence: PresenceHeadEffect,
        attribution: AttributionHead,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresencePlan {
    pub contract_version: u32,
    pub expected: PresenceStamp,
    pub effect: PresenceEffect,
    /// This is predicted local state, NOT a committed generation or receipt.
    /// The native owner supplies its actual terminal fence after commit.
    pub final_snapshot: PresenceSnapshot,
}

/// Read projection only. None means GET omits the source entirely; an empty
/// present contribution still returns Some and accepts its positive revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicNfoDocument {
    pub revision: i64,
    pub contribution: Contribution,
    pub external_identity: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresenceError {
    UnsupportedContract,
    InvalidSnapshot,
    InvalidInput,
    StaleFence,
    StaleSubject,
    StaleAggregate,
    StaleSource,
    StalePresence,
    StaleAttribution,
    StaleHead,
    StalePublicRevision { expected: i64, actual: i64 },
    RetiredTarget,
    RevisionExhausted,
    Bounds(RefreshError),
}
impl From<RefreshError> for PresenceError {
    fn from(value: RefreshError) -> Self {
        Self::Bounds(value)
    }
}
impl std::fmt::Display for PresenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NFO presence transition refused: {self:?}")
    }
}
impl std::error::Error for PresenceError {}
type PresenceResult<T> = std::result::Result<T, PresenceError>;

pub fn public_nfo_document(
    snapshot: &PresenceSnapshot,
) -> PresenceResult<Option<PublicNfoDocument>> {
    validate_snapshot(snapshot)?;
    SizeBudget::default().charge(snapshot)?;
    Ok(match &snapshot.head.source {
        Some(DocumentHead {
            document:
                DocumentState::Present {
                    contribution,
                    external_identity,
                },
            ..
        }) => Some(PublicNfoDocument {
            revision: snapshot
                .presence
                .public_revision
                .ok_or(PresenceError::InvalidSnapshot)?,
            contribution: contribution.clone(),
            external_identity: external_identity.clone(),
        }),
        _ => None,
    })
}

pub fn plan_presence(
    snapshot: &PresenceSnapshot,
    request: &PresenceRequest,
) -> PresenceResult<PresencePlan> {
    validate_snapshot(snapshot)?;
    if request.contract_version != PRESENCE_CONTRACT_VERSION {
        return Err(PresenceError::UnsupportedContract);
    }
    validate_change(&request.change, &snapshot.head.system_fields)?;
    if request.fence != snapshot.fence {
        return Err(PresenceError::StaleFence);
    }
    if request.subject != snapshot.subject {
        return Err(PresenceError::StaleSubject);
    }
    let actual = snapshot.stamp();
    let expected = &request.expected;
    if expected.aggregate_revision != actual.aggregate_revision {
        return Err(PresenceError::StaleAggregate);
    }
    if expected.head.source_revision != actual.head.source_revision {
        return Err(PresenceError::StaleSource);
    }
    if expected.presence != actual.presence {
        return Err(PresenceError::StalePresence);
    }
    if expected.head.attribution_revision != actual.head.attribution_revision {
        return Err(PresenceError::StaleAttribution);
    }
    if expected.head != actual.head {
        return Err(PresenceError::StaleHead);
    }
    // Stale fences/stamps are rejected before their untrusted strings are
    // serialized. Every admitted string above equals an already-bounded fact.
    // Complete input admission precedes copying any retained document.
    let mut budget = SizeBudget::default();
    budget.charge(snapshot)?;
    budget.charge(request)?;

    let old_origin = snapshot
        .head
        .attribution
        .as_ref()
        .and_then(|a| a.origin.clone());
    let (document, origin, active_identity, force_write) = match &request.change {
        PresenceChange::Publish {
            contribution,
            external_identity,
            origin,
        } => {
            active(snapshot)?;
            (
                Some(DocumentState::Present {
                    contribution: contribution.clone(),
                    external_identity: external_identity.clone(),
                }),
                Some(origin.clone()),
                external_identity.clone(),
                false,
            )
        }
        PresenceChange::PublicPut {
            expected_public_revision,
            contribution,
            external_identity,
        } => {
            active(snapshot)?;
            let actual = snapshot.presence.public_revision.unwrap_or(0);
            if *expected_public_revision != actual {
                return Err(PresenceError::StalePublicRevision {
                    expected: *expected_public_revision,
                    actual,
                });
            }
            (
                Some(DocumentState::Present {
                    contribution: contribution.clone(),
                    external_identity: external_identity.clone(),
                }),
                old_origin,
                external_identity.clone(),
                true,
            )
        }
        PresenceChange::Withdraw => {
            active(snapshot)?;
            (
                snapshot
                    .head
                    .source
                    .as_ref()
                    .map(|_| DocumentState::Withdrawn),
                None,
                None,
                false,
            )
        }
        PresenceChange::ReleaseRetiredAttribution => {
            if !matches!(snapshot.head.subject_state, SubjectState::Retired { .. }) {
                return Err(PresenceError::InvalidInput);
            }
            (
                snapshot.head.source.as_ref().map(|s| s.document.clone()),
                None,
                None,
                false,
            )
        }
    };
    let document_changed =
        force_write || document.as_ref() != snapshot.head.source.as_ref().map(|s| &s.document);
    let attribution_changed = snapshot
        .head
        .attribution
        .as_ref()
        .is_some_and(|a| a.origin != origin || a.active_identity != active_identity);
    let mut next = snapshot.clone();
    let effect = if !document_changed && !attribution_changed {
        PresenceEffect::Unchanged
    } else {
        next.aggregate_revision = advance(snapshot.aggregate_revision)?;
        let document_effect = if document_changed {
            let revision = advance(snapshot.head.source.as_ref().map_or(0, |s| s.revision))?;
            let document = document.ok_or(PresenceError::InvalidSnapshot)?;
            next.presence.revision = advance(snapshot.presence.revision)?;
            next.presence.public_revision = match &document {
                DocumentState::Present { .. } => Some(match snapshot.presence.public_revision {
                    Some(revision) => advance(revision)?,
                    None => {
                        next.presence.incarnation = advance(snapshot.presence.incarnation)?;
                        1
                    }
                }),
                DocumentState::Withdrawn => None,
            };
            let head = DocumentHead { revision, document };
            next.head.source = Some(head.clone());
            DocumentEffect::Replace { head }
        } else {
            DocumentEffect::Retain
        };
        let attribution = AttributionHead {
            revision: advance(snapshot.head.attribution.as_ref().map_or(0, |a| a.revision))?,
            document_revision: next
                .head
                .source
                .as_ref()
                .ok_or(PresenceError::InvalidSnapshot)?
                .revision,
            origin,
            active_identity,
        };
        next.head.attribution = Some(attribution.clone());
        PresenceEffect::Changed {
            document: document_effect,
            presence: if document_changed {
                PresenceHeadEffect::Replace {
                    head: next.presence.clone(),
                }
            } else {
                PresenceHeadEffect::Retain
            },
            attribution,
        }
    };
    validate_snapshot(&next)?;
    let plan = PresencePlan {
        contract_version: PRESENCE_CONTRACT_VERSION,
        expected: actual,
        effect,
        final_snapshot: next,
    };
    SizeBudget::default().charge(&plan)?;
    Ok(plan)
}

fn advance(revision: i64) -> PresenceResult<i64> {
    revision
        .checked_add(1)
        .ok_or(PresenceError::RevisionExhausted)
}
fn active(snapshot: &PresenceSnapshot) -> PresenceResult<()> {
    if matches!(snapshot.head.subject_state, SubjectState::Retired { .. }) {
        return Err(PresenceError::RetiredTarget);
    }
    Ok(())
}
fn validate_change(change: &PresenceChange, reserved: &BTreeSet<String>) -> PresenceResult<()> {
    match change {
        PresenceChange::Publish {
            contribution: value,
            external_identity,
            origin,
        } => {
            contribution(value, reserved)?;
            if let Some(value) = external_identity {
                identity(value, MAX_IDENTITY_BYTES)?;
            }
            validate_origin(origin)?;
        }
        PresenceChange::PublicPut {
            expected_public_revision,
            contribution: value,
            external_identity,
        } => {
            if *expected_public_revision < 0 {
                return Err(PresenceError::InvalidInput);
            }
            contribution(value, reserved)?;
            // Public metadata PUT has its existing stricter 256-byte limit.
            if let Some(value) = external_identity {
                identity(value, 256)?;
            }
        }
        _ => {}
    }
    Ok(())
}
// Keep identities opaque: legacy PUT does not trim or normalize them.
fn identity(value: &str, maximum: usize) -> PresenceResult<()> {
    if value.is_empty() || value.len() > maximum {
        return Err(PresenceError::InvalidInput);
    }
    Ok(())
}

fn validate_origin(origin: &FileOrigin) -> PresenceResult<()> {
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
    Ok(())
}
fn validate_snapshot(snapshot: &PresenceSnapshot) -> PresenceResult<()> {
    if snapshot.contract_version != PRESENCE_CONTRACT_VERSION {
        return Err(PresenceError::UnsupportedContract);
    }
    id(&snapshot.subject)?;
    id(&snapshot.fence.catalog_id)?;
    id(&snapshot.fence.incarnation)?;
    if snapshot.fence.generation < 0 || snapshot.aggregate_revision < 0 {
        return Err(PresenceError::InvalidSnapshot);
    }
    let head = &snapshot.head;
    match head.subject_state {
        SubjectState::Active { item_revision } if item_revision > 0 => {}
        SubjectState::Retired {
            item_revision,
            retirement_revision,
        } if item_revision > 0 && retirement_revision > 0 => {}
        _ => return Err(PresenceError::InvalidSnapshot),
    }
    if let Some(value) = &head.schema_fingerprint {
        digest(value)?;
    }
    if head.system_fields.len() > MAX_METADATA_JSON_NODES {
        return Err(RefreshError::Metadata(MetadataError::JsonNodeBudget).into());
    }
    for name in &head.system_fields {
        field(name)?;
    }
    if let PinHead::Decision { revision, value } = &head.pin {
        if *revision < 1 {
            return Err(PresenceError::InvalidSnapshot);
        }
        if let Some(value) = value {
            identity(value, MAX_IDENTITY_BYTES)?;
        }
    }
    let presence = &snapshot.presence;
    match (&head.source, &head.attribution) {
        (None, None)
            if presence.revision == 0
                && presence.incarnation == 0
                && presence.public_revision.is_none() => {}
        (Some(source), Some(attribution))
            if source.revision > 0
                && attribution.revision > 0
                && attribution.document_revision == source.revision
                && presence.revision > 0
                && presence.incarnation > 0
                && presence.incarnation <= presence.revision =>
        {
            if let Some(origin) = &attribution.origin {
                validate_origin(origin)?;
            }
            if let Some(value) = &attribution.active_identity {
                identity(value, MAX_IDENTITY_BYTES)?;
            }
            match &source.document {
                DocumentState::Present {
                    contribution: value,
                    external_identity,
                } => {
                    contribution(value, &head.system_fields)?;
                    if let Some(value) = external_identity {
                        identity(value, MAX_IDENTITY_BYTES)?;
                    }
                    if presence.public_revision.is_none_or(|r| r < 1)
                        || (attribution.active_identity.is_some()
                            && attribution.active_identity != *external_identity)
                        || (matches!(head.subject_state, SubjectState::Active { .. })
                            && attribution.active_identity != *external_identity)
                    {
                        return Err(PresenceError::InvalidSnapshot);
                    }
                }
                DocumentState::Withdrawn
                    if presence.public_revision.is_none()
                        && attribution.origin.is_none()
                        && attribution.active_identity.is_none() => {}
                _ => return Err(PresenceError::InvalidSnapshot),
            }
        }
        _ => return Err(PresenceError::InvalidSnapshot),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
