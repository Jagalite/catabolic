// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Persistent native identity owner. Only the durable command dispatcher may
//! apply a mutation, inside its writer transaction. This module never commits,
//! writes receipts, rehomes legacy rows, or infers editorial identity.
mod selected;
mod source_succession;
pub(crate) use selected::load_selected_in;
pub(crate) use source_succession::{
    PreparedSourceContexts, apply_source_contexts, prepare_source_contexts,
};
mod structural;
pub(crate) use structural::{
    StructuralContext, StructuralReadBudget, load_structural_context_in,
    load_structural_context_in_with_control_workspace,
    revalidate_structural_heads_in, source_succession_heads_cte,
};

use crate::{
    Error as StoreError, Store,
    catalog_identity::{CatalogManifest, ManifestVersion},
    components,
    migration::rows,
};
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey},
    catalog_identity::{
        ActiveSegment, EquivalenceBasis, EquivalenceEvidence, ExactFileRevision, IntervalUs,
        SegmentationSet, SourceExtent, VersionBinding, VersionContract, VersionState,
        validate_segmentation_set, validate_version,
    },
    encode, sha256,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Trusted host scope, supplied independently of request JSON. A host must
/// resolve the editorial context before admitting a command. New caller-chosen
/// context IDs cannot create a fresh authorization or evade overlap checks.
#[derive(Clone, Debug)]
pub struct IdentityAuthority {
    pub authority_namespace: String,
    pub context_keys: BTreeSet<String>,
    pub allow_declared_evidence: bool,
    pub allow_verified_evidence: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextChange {
    pub id: String,
    pub authority_namespace: String,
    pub context_key: String,
    /// None creates a context. Some requires exactly that retained head.
    pub expected_revision: Option<i64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineState {
    Provisional,
    Established,
}
impl TimelineState {
    fn sql(self) -> &'static str {
        match self {
            Self::Provisional => "provisional",
            Self::Established => "established",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineChange {
    pub timeline_id: String,
    pub edition_item_id: String,
    pub expected_revision: Option<i64>,
    pub identity_state: TimelineState,
    pub coverage: Option<IntervalUs>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityEvidence {
    pub id: String,
    pub basis: EquivalenceBasis,
    pub claims: Vec<EquivalenceClaim>,
    pub evidence: Value,
}
/// Explicit evidence binds exact version and timeline snapshots. A retained
/// declaration cannot be reused to authorize another encode or later revision.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EquivalenceClaim {
    pub edition_item_id: String,
    pub timeline_id: String,
    pub timeline_revision: i64,
    pub version_id: String,
    pub version_revision: i64,
    pub exact_files: Vec<ExactFileRevision>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionChange {
    pub expected_revision: Option<i64>,
    /// The exact timeline head used by the resulting version, including any
    /// timeline revision explicitly created in this command.
    pub expected_timeline_revision: i64,
    pub version: VersionContract,
    pub equivalence: Option<EquivalenceEvidence>,
    /// Complete expected binding heads. None explicitly claims a new binding;
    /// Some compares its last retained revision before appending the snapshot.
    pub expected_binding_revisions: BTreeMap<String, Option<i64>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityMutation {
    pub contract_version: u32,
    pub profile: String,
    pub context: ContextChange,
    pub timelines: Vec<TimelineChange>,
    pub versions: Vec<VersionChange>,
    pub evidence: Vec<IdentityEvidence>,
}

/// Origin-selected identity failures. Display text is diagnostic only and is
/// never parsed by the durable command dispatcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityErrorCode {
    InvalidInput,
    AuthorityDenied,
    Conflict,
    Unsupported,
    StoreFailure,
}
#[derive(Debug)]
pub struct IdentityError {
    pub code: IdentityErrorCode,
    pub error: StoreError,
}
impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for IdentityError {}
impl From<StoreError> for IdentityError {
    fn from(error: StoreError) -> Self {
        Self {
            code: IdentityErrorCode::StoreFailure,
            error,
        }
    }
}
impl From<rusqlite::Error> for IdentityError {
    fn from(error: rusqlite::Error) -> Self {
        StoreError::from(error).into()
    }
}
impl From<serde_json::Error> for IdentityError {
    fn from(error: serde_json::Error) -> Self {
        StoreError::from(error).into()
    }
}
impl From<IdentityError> for StoreError {
    fn from(error: IdentityError) -> Self {
        error.error
    }
}
pub type Result<T> = std::result::Result<T, IdentityError>;
fn failure(code: IdentityErrorCode, message: &str) -> IdentityError {
    IdentityError {
        code,
        error: StoreError(message.into()),
    }
}
fn fail(message: &str) -> IdentityError {
    failure(IdentityErrorCode::InvalidInput, message)
}
fn conflict(message: &str) -> IdentityError {
    failure(IdentityErrorCode::Conflict, message)
}
fn denied(message: &str) -> IdentityError {
    failure(IdentityErrorCode::AuthorityDenied, message)
}
fn unsupported(message: &str) -> IdentityError {
    failure(IdentityErrorCode::Unsupported, message)
}

fn id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(fail("catalog_identity_invalid_identifier"))
    } else {
        Ok(())
    }
}
fn checked<T>(value: catabolic_core::catalog_identity::Result<T>) -> Result<T> {
    value.map_err(|e| fail(e.code()))
}
fn bounds(interval: Option<IntervalUs>) -> (Option<i64>, Option<i64>) {
    (
        interval.map(IntervalUs::start_us),
        interval.map(IntervalUs::end_us),
    )
}
fn basis(value: EquivalenceBasis) -> &'static str {
    match value {
        EquivalenceBasis::Declared => "declared",
        EquivalenceBasis::Verified => "verified",
    }
}
fn next(expected: Option<i64>) -> Result<i64> {
    match expected {
        None => Ok(1),
        Some(v) if v > 0 => v
            .checked_add(1)
            .ok_or_else(|| conflict("catalog_identity_revision_exhausted")),
        _ => Err(conflict("catalog_identity_revision_conflict")),
    }
}
fn interval(row: &Value, start: &str, end: &str) -> Result<Option<IntervalUs>> {
    match (row[start].as_i64(), row[end].as_i64()) {
        (None, None) if row[start].is_null() && row[end].is_null() => Ok(None),
        (Some(a), Some(b)) => checked(IntervalUs::new(a, b)).map(Some),
        _ => Err(fail("catalog_identity_invalid_interval")),
    }
}
fn string(row: &Value, key: &str) -> Result<String> {
    row[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| fail("catalog_identity_invalid_record"))
}
fn int(row: &Value, key: &str) -> Result<i64> {
    row[key]
        .as_i64()
        .ok_or_else(|| fail("catalog_identity_invalid_record"))
}
fn permit(db: &Connection, kind: &str, scope: &str) -> Result<()> {
    db.execute("INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) SELECT ?,?,incarnation FROM catalog_state WHERE id=1",params![kind,scope])?;
    Ok(())
}

fn has_table(db: &Connection, name: &str) -> Result<bool> {
    Ok(db
        .prepare("SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?")?
        .exists([name])?)
}

/// Schema 33 keeps its original read paths. Schema 34 preserves all retained
/// heads and snapshots; only these live projections exclude retired versions.
fn live_versions(db: &Connection) -> Result<&'static str> {
    Ok(if has_table(db, "catalog_version_retirements")? {
        "catalog_live_versions"
    } else {
        "catalog_versions"
    })
}
fn live_bindings(db: &Connection) -> Result<&'static str> {
    Ok(if has_table(db, "catalog_version_retirements")? {
        "catalog_live_binding_revisions"
    } else {
        "catalog_active_binding_revisions"
    })
}

fn require_live_version(db: &Connection, version: &str) -> Result<()> {
    if has_table(db, "catalog_version_retirements")?
        && db
            .prepare("SELECT 1 FROM catalog_version_retirements WHERE version_id=?")?
            .exists([version])?
    {
        return Err(conflict("catalog_identity_retired_version"));
    }
    Ok(())
}

fn require_live_owner(db: &Connection, item: &str) -> Result<()> {
    if has_table(db, "catalog_logical_retirements")?
        && db
            .prepare("SELECT 1 FROM catalog_logical_retirements WHERE item_id=?")?
            .exists([item])?
    {
        return Err(conflict("catalog_identity_retired_owner"));
    }
    if has_table(db, "catalog_version_retirements")?
        && db
            .prepare("SELECT 1 FROM catalog_versions v JOIN catalog_version_retirements r ON r.version_id=v.id WHERE v.association_item_id=?")?
            .exists([item])?
    {
        return Err(conflict("catalog_identity_retired_owner"));
    }
    Ok(())
}

fn require_live_timeline(db: &Connection, timeline: &str) -> Result<()> {
    let owner: Option<String> = db
        .query_row(
            "SELECT edition_item_id FROM catalog_timelines WHERE id=?",
            [timeline],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(owner) = owner {
        require_live_owner(db, &owner)?;
    }
    Ok(())
}

fn require_live_targets(db: &Connection, mutation: &IdentityMutation) -> Result<()> {
    for timeline in &mutation.timelines {
        require_live_owner(db, &timeline.edition_item_id)?;
        require_live_timeline(db, &timeline.timeline_id)?;
    }
    for change in &mutation.versions {
        require_live_version(db, &change.version.version_id)?;
        require_live_owner(db, &change.version.association_item_id)?;
        require_live_owner(db, &change.version.edition_item_id)?;
        require_live_timeline(db, &change.version.timeline_id)?;
    }
    Ok(())
}

impl IdentityMutation {
    pub fn validate(&self) -> Result<()> {
        if self.contract_version != 1 {
            return Err(unsupported("catalog_identity_unsupported_contract"));
        }
        if self.timelines.len() > 64
            || self.versions.len() > 64
            || self.evidence.len() > 128
            || self
                .versions
                .iter()
                .map(|v| v.version.bindings.len())
                .sum::<usize>()
                > 1024
            || (self.timelines.is_empty() && self.versions.is_empty())
        {
            return Err(fail("catalog_identity_mutation_budget"));
        }
        for value in [
            &self.profile,
            &self.context.id,
            &self.context.authority_namespace,
            &self.context.context_key,
        ] {
            id(value)?;
        }
        next(self.context.expected_revision)?;
        let mut timelines = BTreeSet::new();
        let mut versions = BTreeSet::new();
        let mut owners = BTreeSet::new();
        let mut bindings = BTreeSet::new();
        let mut evidence = BTreeSet::new();
        for timeline in &self.timelines {
            id(&timeline.timeline_id)?;
            id(&timeline.edition_item_id)?;
            next(timeline.expected_revision)?;
            if !timelines.insert(&timeline.timeline_id) {
                return Err(fail("catalog_identity_duplicate_timeline"));
            }
        }
        for change in &self.versions {
            checked(validate_version(&change.version))?;
            next(change.expected_revision)?;
            for value in [
                &change.version.version_id,
                &change.version.association_item_id,
                &change.version.edition_item_id,
                &change.version.timeline_id,
            ] {
                id(value)?;
            }
            if change.expected_timeline_revision < 1
                || !versions.insert(&change.version.version_id)
                || !owners.insert(&change.version.association_item_id)
            {
                return Err(fail("catalog_identity_duplicate_version_owner"));
            }
            if change.expected_binding_revisions.len() != change.version.bindings.len() {
                return Err(fail("catalog_identity_binding_set_mismatch"));
            }
            for binding in &change.version.bindings {
                for value in [
                    &binding.binding_id,
                    &binding.association_id,
                    &binding.file.file_id,
                    &binding.file.revision,
                ] {
                    id(value)?;
                }
                if !bindings.insert(&binding.binding_id) {
                    return Err(fail("catalog_identity_duplicate_binding"));
                }
                next(
                    *change
                        .expected_binding_revisions
                        .get(&binding.binding_id)
                        .ok_or_else(|| fail("catalog_identity_binding_set_mismatch"))?,
                )?;
            }
            if let Some(e) = &change.equivalence {
                id(&e.evidence_id)?;
            }
        }
        for e in &self.evidence {
            id(&e.id)?;
            if e.claims.is_empty() || e.claims.len() > 64 {
                return Err(fail("catalog_identity_invalid_evidence"));
            }
            let mut claims = BTreeSet::new();
            for claim in &e.claims {
                for value in [
                    &claim.edition_item_id,
                    &claim.timeline_id,
                    &claim.version_id,
                ] {
                    id(value)?;
                }
                if claim.timeline_revision < 1
                    || claim.version_revision < 1
                    || claim.exact_files.len() > 1024
                    || !claims.insert((&claim.version_id, claim.version_revision))
                {
                    return Err(fail("catalog_identity_invalid_evidence"));
                }
                let mut files = BTreeSet::new();
                for file in &claim.exact_files {
                    id(&file.file_id)?;
                    id(&file.revision)?;
                    if !files.insert(&file.file_id) {
                        return Err(fail("catalog_identity_invalid_evidence"));
                    }
                }
            }
            if !e.evidence.is_object()
                || e.evidence.as_object().is_none_or(|o| o.is_empty())
                || !evidence.insert(&e.id)
            {
                return Err(fail("catalog_identity_invalid_evidence"));
            }
        }
        Ok(())
    }
}

fn authorize(m: &IdentityMutation, a: &IdentityAuthority, key: &CommandKey) -> Result<()> {
    if m.context.authority_namespace != a.authority_namespace
        || a.authority_namespace != key.host_namespace()
        || !a.context_keys.contains(&m.context.context_key)
    {
        return Err(denied("catalog_identity_context_not_authorized"));
    }
    for evidence in m.evidence.iter().map(|e| e.basis).chain(
        m.versions
            .iter()
            .filter_map(|v| v.equivalence.as_ref().map(|e| e.basis)),
    ) {
        if !match evidence {
            EquivalenceBasis::Declared => a.allow_declared_evidence,
            EquivalenceBasis::Verified => a.allow_verified_evidence,
        } {
            return Err(denied("catalog_identity_evidence_not_authorized"));
        }
    }
    Ok(())
}

/// The ordinary write seam. The dispatcher owns admission, generation fencing,
/// rollback, receipt-last insertion and commit. Never expose permits or this
/// function as a public capability.
pub(crate) fn apply_identity(
    store: &Store,
    payload: &Value,
    command: &CommandEnvelope,
    authority: &IdentityAuthority,
    admitted_at_us: i64,
) -> Result<Value> {
    let mutation: IdentityMutation = serde_json::from_value(payload.clone())
        .map_err(|_| fail("catalog_identity_invalid_mutation"))?;
    apply(
        &store.db,
        &mutation,
        command.key(),
        authority,
        admitted_at_us,
        None,
    )
}

/// Only this module can construct the proof that old context membership was
/// checked before retirement. It is scoped to one exact retained context head.
struct ValidatedContextMembership {
    context_id: String,
    revision: i64,
}

/// Replace live membership without mutating the retired identity snapshots.
/// The logical owner/dispatcher must fence command.expected_generation(),
/// incarnation and its reviewed graph before ANY creation writes. Those writes
/// legitimately advance generation before reaching this seam. This function
/// therefore rechecks exact context/version heads, not the original generation.
/// The caller owns rollback on every error and receipt-last commit; there is no
/// public validation bypass and no external effect or transaction boundary here.
pub(crate) fn apply_identity_replacement(
    store: &Store,
    mutation: &IdentityMutation,
    command: &CommandEnvelope,
    authority: &IdentityAuthority,
    admitted_at_us: i64,
    retirements: &[(String, i64)],
) -> Result<Value> {
    let db = &store.db;
    if !store.writable
        || db.is_autocommit()
        || db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
    {
        return Err(unsupported("catalog_identity_writer_transaction_required"));
    }
    if store.schema_version < 34 || !has_table(db, "catalog_version_retirements")? {
        return Err(unsupported("catalog_identity_retirement_schema_required"));
    }
    mutation.validate()?;
    authorize(mutation, authority, command.key())?;
    require_live_targets(db, mutation)?;
    let context = context_head(db, mutation)?
        .ok_or_else(|| conflict("catalog_identity_context_revision_conflict"))?;
    let membership = ValidatedContextMembership {
        context_id: mutation.context.id.clone(),
        revision: int(&context, "current_revision")?,
    };
    validate_context(db, &membership.context_id)?;
    if retirements.is_empty() || retirements.len() > 64 {
        return Err(fail("catalog_identity_retirement_budget"));
    }
    let mut seen = BTreeSet::new();
    for (version, revision) in retirements {
        id(version)?;
        if !seen.insert(version) {
            return Err(fail("catalog_identity_duplicate_retirement"));
        }
        require_live_version(db, version)?;
        let head: Option<(i64, String)> = db
            .query_row(
                "SELECT current_revision,segmentation_context_id FROM catalog_versions WHERE id=?",
                [version],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match head {
            Some((current, context)) if current == *revision && *revision > 0 => {
                if context != mutation.context.id {
                    return Err(conflict("catalog_identity_context_scope_mismatch"));
                }
            }
            _ => return Err(conflict("catalog_identity_version_revision_conflict")),
        }
    }
    // Validate the whole retirement set before inserting any marker. The owner
    // subsequently rebuilds complete live membership, retaining every unchanged
    // live version, and checks the new snapshot in both directions as usual.
    for (version, revision) in retirements {
        db.execute(
            "INSERT INTO catalog_version_retirements(version_id,version_revision,command_catalog_id,command_host_namespace,command_operation_id,recorded_at_us) VALUES(?,?,?,?,?,?)",
            params![version, revision, command.key().catalog_id(), command.key().host_namespace(), command.key().operation_id(), admitted_at_us],
        )?;
    }
    apply(
        db,
        mutation,
        command.key(),
        authority,
        admitted_at_us,
        Some(&membership),
    )
}

fn context_head(db: &Connection, m: &IdentityMutation) -> Result<Option<Value>> {
    let alias:Option<String>=db.query_row("SELECT id FROM catalog_segmentation_contexts WHERE authority_namespace=? AND context_key=?",params![m.context.authority_namespace,m.context.context_key],|r|r.get(0)).optional()?;
    if alias.is_some_and(|existing| existing != m.context.id) {
        return Err(conflict("catalog_identity_context_scope_mismatch"));
    }
    let context = rows(
        db,
        "SELECT * FROM catalog_segmentation_contexts WHERE id=?",
        &[m.context.id.clone().into()],
    )?
    .pop();
    if context
        .as_ref()
        .and_then(|r| r["current_revision"].as_i64())
        != m.context.expected_revision
    {
        return Err(conflict("catalog_identity_context_revision_conflict"));
    }
    if let Some(r) = &context
        && (r["authority_namespace"] != m.context.authority_namespace
            || r["context_key"] != m.context.context_key)
    {
        return Err(conflict("catalog_identity_context_scope_mismatch"));
    }
    Ok(context)
}

fn apply(
    db: &Connection,
    m: &IdentityMutation,
    key: &CommandKey,
    a: &IdentityAuthority,
    now: i64,
    validated_membership: Option<&ValidatedContextMembership>,
) -> Result<Value> {
    if db.is_autocommit() {
        return Err(unsupported("catalog_identity_writer_transaction_required"));
    }
    m.validate()?;
    authorize(m, a, key)?;
    require_live_targets(db, m)?;
    if !db
        .prepare("SELECT 1 FROM profiles WHERE id=?")?
        .exists([&m.profile])?
    {
        return Err(fail("catalog_identity_unknown_profile"));
    }
    let context = context_head(db, m)?;
    match validated_membership {
        Some(validated)
            if validated.context_id == m.context.id
                && Some(validated.revision) == m.context.expected_revision
                && context.is_some() => {}
        Some(_) => return Err(conflict("catalog_identity_context_revision_conflict")),
        None if context.is_some() => validate_context(db, &m.context.id)?,
        None => {}
    }
    let context_revision = next(m.context.expected_revision)?;
    permit(db, "context", &m.context.id)?;
    if context.is_none() {
        db.execute(
            "INSERT INTO catalog_segmentation_contexts VALUES(?,?,?,1)",
            params![
                m.context.id,
                m.context.authority_namespace,
                m.context.context_key
            ],
        )?;
    } else {
        db.execute("UPDATE catalog_segmentation_contexts SET current_revision=? WHERE id=? AND current_revision=?",params![context_revision,m.context.id,m.context.expected_revision])?;
    }
    db.execute(
        "INSERT INTO catalog_segmentation_revisions VALUES(?,?,?,?,?,?)",
        params![
            m.context.id,
            context_revision,
            now,
            key.catalog_id(),
            key.host_namespace(),
            key.operation_id()
        ],
    )?;
    for e in &m.evidence {
        if db
            .prepare("SELECT 1 FROM catalog_identity_evidence WHERE id=?")?
            .exists([&e.id])?
        {
            return Err(conflict("catalog_identity_evidence_id_conflict"));
        }
        db.execute(
            "INSERT INTO catalog_identity_evidence VALUES(?,?,?,?)",
            params![
                e.id,
                basis(e.basis),
                encode(&json!({"contract_version":1,"claims":e.claims,"evidence":e.evidence})),
                now
            ],
        )?;
    }
    for t in &m.timelines {
        apply_timeline(db, m, t, key, now)?;
    }
    let mut mappings = vec![];
    for change in &m.versions {
        mappings.push(apply_version(db, m, change, key, now)?);
    }
    // Every existing live version remains in the resulting context. Membership
    // is rebuilt from authoritative live heads, never from submitted segments.
    let active = load_context(db, &m.profile, &m.context.id)?;
    validate_active(db, &m.profile, &active)?;
    let sets = build_sets(&m.context.id, &active)?;
    for set in &sets {
        let present:Option<String>=db.query_row("SELECT id FROM catalog_segmentation_sets WHERE context_id=? AND profile=? AND file_id=? AND file_revision=?",params![m.context.id,m.profile,set.file.file_id,set.file.revision],|r|r.get(0)).optional()?;
        let set_id = if let Some(old) = present {
            old
        } else {
            db.execute(
                "INSERT INTO catalog_segmentation_sets VALUES(?,?,?,?,?)",
                params![
                    set.segmentation_set_id,
                    m.context.id,
                    m.profile,
                    set.file.file_id,
                    set.file.revision
                ],
            )?;
            set.segmentation_set_id.clone()
        };
        for segment in &set.segments {
            let revision: i64 = db.query_row(
                &format!(
                    "SELECT binding_revision FROM {} WHERE binding_id=?",
                    live_bindings(db)?
                ),
                [&segment.binding_id],
                |r| r.get(0),
            )?;
            db.execute(
                "INSERT INTO catalog_segment_members VALUES(?,?,?,?,?)",
                params![
                    m.context.id,
                    context_revision,
                    set_id,
                    segment.binding_id,
                    revision
                ],
            )?;
        }
    }
    validate_context(db, &m.context.id)?;
    // Plans made before a typed claim contain no native marker. Advance the
    // existing legacy invalidation token in this same transaction so they cannot
    // admit whole-file effects under their pre-claim snapshot.
    let epoch: i64 = db.query_row(
        "SELECT generation FROM fallback_epoch WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    let epoch = epoch
        .checked_add(1)
        .ok_or_else(|| conflict("catalog_identity_legacy_epoch_exhausted"))?;
    db.execute("UPDATE fallback_epoch SET generation=? WHERE id=1", [epoch])?;
    for t in &m.timelines {
        db.execute(
            "DELETE FROM catalog_mutation_permits WHERE scope_kind='timeline' AND scope_id=?",
            [&t.timeline_id],
        )?;
    }
    for v in &m.versions {
        db.execute(
            "DELETE FROM catalog_mutation_permits WHERE scope_kind='version' AND scope_id=?",
            [&v.version.version_id],
        )?;
    }
    db.execute(
        "DELETE FROM catalog_mutation_permits WHERE scope_kind='context' AND scope_id=?",
        [&m.context.id],
    )?;
    Ok(
        json!({"contract_version":1,"context_id":m.context.id,"context_revision":context_revision,"authority_namespace":m.context.authority_namespace,"context_key":m.context.context_key,"timelines":m.timelines.iter().map(|t|json!({"timeline_id":t.timeline_id,"revision":next(t.expected_revision).expect("validated revision"),"edition_item_id":t.edition_item_id,"identity_state":t.identity_state,"coverage":t.coverage})).collect::<Vec<_>>(),"versions":mappings,"evidence":m.evidence}),
    )
}

fn apply_timeline(
    db: &Connection,
    m: &IdentityMutation,
    t: &TimelineChange,
    key: &CommandKey,
    now: i64,
) -> Result<()> {
    require_live_owner(db, &t.edition_item_id)?;
    require_live_timeline(db, &t.timeline_id)?;
    if !db
        .prepare("SELECT 1 FROM items WHERE id=?")?
        .exists([&t.edition_item_id])?
    {
        return Err(fail("catalog_identity_missing_owner"));
    }
    let old = rows(
        db,
        "SELECT * FROM catalog_timelines WHERE id=?",
        &[t.timeline_id.clone().into()],
    )?
    .pop();
    if old.as_ref().and_then(|r| r["current_revision"].as_i64()) != t.expected_revision {
        return Err(conflict("catalog_identity_timeline_revision_conflict"));
    }
    if old
        .as_ref()
        .is_some_and(|r| r["edition_item_id"] != t.edition_item_id)
    {
        return Err(conflict("catalog_identity_timeline_owner_immutable"));
    }
    // Revising a shared timeline requires an explicit new version snapshot for
    // every active owner; it may not silently reinterpret unchanged versions.
    for row in rows(
        db,
        &format!(
            "SELECT v.id FROM {} v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision WHERE r.timeline_id=? LIMIT 65",
            live_versions(db)?
        ),
        &[t.timeline_id.clone().into()],
    )? {
        if !m
            .versions
            .iter()
            .any(|v| row["id"] == v.version.version_id && v.version.timeline_id == t.timeline_id)
        {
            return Err(fail("catalog_identity_timeline_membership_incomplete"));
        }
    }
    permit(db, "timeline", &t.timeline_id)?;
    let revision = next(t.expected_revision)?;
    if old.is_none() {
        db.execute(
            "INSERT INTO catalog_timelines VALUES(?,?,1)",
            params![t.timeline_id, t.edition_item_id],
        )?;
    } else {
        db.execute(
            "UPDATE catalog_timelines SET current_revision=? WHERE id=? AND current_revision=?",
            params![revision, t.timeline_id, t.expected_revision],
        )?;
    }
    let (start, end) = bounds(t.coverage);
    db.execute(
        "INSERT INTO catalog_timeline_revisions VALUES(?,?,?,?,?,?,?,?,?)",
        params![
            t.timeline_id,
            revision,
            t.identity_state.sql(),
            start,
            end,
            now,
            key.catalog_id(),
            key.host_namespace(),
            key.operation_id()
        ],
    )?;
    Ok(())
}

fn capture_revision(
    db: &Connection,
    profile: &str,
    file: &ExactFileRevision,
    now: i64,
) -> Result<Value> {
    let snapshot = components::occurrence(db, profile, &file.file_id)?;
    if components::revision(&snapshot) != file.revision {
        return Err(conflict("catalog_identity_stale_file_revision"));
    }
    let old:Option<String>=db.query_row("SELECT snapshot_json FROM catalog_exact_file_revisions WHERE profile=? AND file_id=? AND revision=?",params![profile,file.file_id,file.revision],|r|r.get(0)).optional()?;
    if let Some(old) = old {
        let mut retained: Value = serde_json::from_str(&old)?;
        let mut current = snapshot.clone();
        // Components v1 deliberately excludes ctime from revision identity.
        retained
            .as_object_mut()
            .ok_or_else(|| fail("catalog_identity_invalid_exact_revision"))?
            .remove("ctime_ns");
        current
            .as_object_mut()
            .ok_or_else(|| fail("catalog_identity_invalid_exact_revision"))?
            .remove("ctime_ns");
        if retained != current {
            return Err(conflict("catalog_identity_exact_revision_collision"));
        }
    } else {
        db.execute(
            "INSERT INTO catalog_exact_file_revisions VALUES(?,?,?,1,?,?)",
            params![profile, file.file_id, file.revision, encode(&snapshot), now],
        )?;
    }
    Ok(snapshot)
}

fn apply_version(
    db: &Connection,
    m: &IdentityMutation,
    c: &VersionChange,
    key: &CommandKey,
    now: i64,
) -> Result<Value> {
    let v = &c.version;
    require_live_version(db, &v.version_id)?;
    require_live_owner(db, &v.association_item_id)?;
    require_live_owner(db, &v.edition_item_id)?;
    require_live_timeline(db, &v.timeline_id)?;
    let old=rows(db,"SELECT v.*,r.timeline_id FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision WHERE v.id=?",&[v.version_id.clone().into()])?.pop();
    if old.as_ref().and_then(|r| r["current_revision"].as_i64()) != c.expected_revision {
        return Err(conflict("catalog_identity_version_revision_conflict"));
    }
    if old.as_ref().is_some_and(|r| {
        r["association_item_id"] != v.association_item_id
            || r["segmentation_context_id"] != m.context.id
            || r["timeline_id"] != v.timeline_id
    }) {
        return Err(conflict("catalog_identity_rebind_unsupported"));
    }
    let timeline=rows(db,"SELECT t.*,r.identity_state,r.coverage_start_us,r.coverage_end_us FROM catalog_timelines t JOIN catalog_timeline_revisions r ON r.timeline_id=t.id AND r.revision=t.current_revision WHERE t.id=?",&[v.timeline_id.clone().into()])?.pop().ok_or_else(||fail("catalog_identity_missing_timeline"))?;
    if timeline["current_revision"].as_i64() != Some(c.expected_timeline_revision) {
        return Err(conflict("catalog_identity_timeline_revision_conflict"));
    }
    if timeline["edition_item_id"] != v.edition_item_id
        || interval(&timeline, "coverage_start_us", "coverage_end_us")? != v.declared_coverage
    {
        return Err(fail("catalog_identity_timeline_coverage_mismatch"));
    }
    if timeline["identity_state"] == "provisional" && c.equivalence.is_some() {
        return Err(fail("catalog_identity_provisional_equivalence"));
    }
    validate_associations(db, &m.profile, v)?;
    if let Some(evidence) = &c.equivalence {
        validate_equivalence(
            db,
            v,
            next(c.expected_revision)?,
            c.expected_timeline_revision,
            evidence,
        )?;
    }
    permit(db, "version", &v.version_id)?;
    let revision = next(c.expected_revision)?;
    if old.is_none() {
        db.execute(
            "INSERT INTO catalog_versions VALUES(?,?,?,1)",
            params![v.version_id, v.association_item_id, m.context.id],
        )?;
    } else {
        db.execute(
            "UPDATE catalog_versions SET current_revision=? WHERE id=? AND current_revision=?",
            params![revision, v.version_id, c.expected_revision],
        )?;
    }
    let (start, end) = bounds(v.declared_coverage);
    db.execute(
        "INSERT INTO catalog_version_revisions VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            v.version_id,
            revision,
            v.timeline_id,
            c.expected_timeline_revision,
            match v.state {
                VersionState::Draft => "draft",
                VersionState::Ready => "ready",
            },
            start,
            end,
            c.equivalence.as_ref().map(|e| e.evidence_id.as_str()),
            c.equivalence.as_ref().map(|e| basis(e.basis)),
            now,
            key.catalog_id(),
            key.host_namespace(),
            key.operation_id()
        ],
    )?;
    let mut mappings = vec![];
    for b in &v.bindings {
        let prior=rows(db,"SELECT b.*,MAX(r.revision) AS revision FROM catalog_bindings b JOIN catalog_binding_revisions r ON r.binding_id=b.id WHERE b.id=? GROUP BY b.id",&[b.binding_id.clone().into()])?.pop();
        let expected = c.expected_binding_revisions[&b.binding_id];
        if prior.as_ref().and_then(|r| r["revision"].as_i64()) != expected {
            return Err(conflict("catalog_identity_binding_revision_conflict"));
        }
        if prior.as_ref().is_some_and(|r| {
            r["version_id"] != v.version_id || r["association_id"] != b.association_id
        }) {
            return Err(conflict("catalog_identity_binding_owner_immutable"));
        }
        let snapshot = capture_revision(db, &m.profile, &b.file, now)?;
        if prior.is_none() {
            if db
                .prepare("SELECT 1 FROM catalog_bindings WHERE association_id=?")?
                .exists([&b.association_id])?
            {
                return Err(conflict("catalog_identity_binding_owner_immutable"));
            }
            db.execute(
                "INSERT INTO catalog_bindings VALUES(?,?,?)",
                params![b.binding_id, v.version_id, b.association_id],
            )?;
        }
        let binding_revision = next(expected)?;
        let (source_start, source_end) = bounds(checked(b.source.interval())?);
        let (timeline_start, timeline_end) = bounds(b.timeline_interval);
        db.execute(
            "INSERT INTO catalog_binding_revisions VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                b.binding_id,
                binding_revision,
                m.profile,
                b.file.file_id,
                b.file.revision,
                b.part,
                match b.source {
                    SourceExtent::WholeFile { .. } => "whole_file",
                    SourceExtent::Slice { .. } => "slice",
                },
                source_start,
                source_end,
                timeline_start,
                timeline_end,
                Option::<String>::None,
                now,
                key.catalog_id(),
                key.host_namespace(),
                key.operation_id()
            ],
        )?;
        db.execute(
            "INSERT INTO catalog_version_bindings VALUES(?,?,?,?)",
            params![v.version_id, revision, b.binding_id, binding_revision],
        )?;
        let legacy = rows(
            db,
            "SELECT * FROM catalog_legacy_associations WHERE association_id=?",
            &[b.association_id.clone().into()],
        )?
        .pop();
        let original_association = rows(
            db,
            "SELECT * FROM item_files WHERE id=?",
            &[b.association_id.clone().into()],
        )?
        .pop()
        .ok_or_else(|| conflict("catalog_identity_binding_set_mismatch"))?;
        mappings.push(json!({"original_association":original_association,"binding_id":b.binding_id,"binding_revision":binding_revision,"association_id":b.association_id,"association_item_id":v.association_item_id,"file":b.file,"part":b.part,"source":b.source,"timeline_interval":b.timeline_interval,"profile":m.profile,"snapshot":snapshot,"legacy_original":legacy}));
    }
    Ok(
        json!({"version_id":v.version_id,"revision":revision,"timeline_id":v.timeline_id,"timeline_revision":c.expected_timeline_revision,"association_item_id":v.association_item_id,"edition_item_id":v.edition_item_id,"state":v.state,"declared_coverage":v.declared_coverage,"equivalence":c.equivalence,"bindings":mappings}),
    )
}

fn validate_equivalence(
    db: &Connection,
    version: &VersionContract,
    version_revision: i64,
    timeline_revision: i64,
    evidence: &EquivalenceEvidence,
) -> Result<()> {
    let raw: Option<String> = db
        .query_row(
            "SELECT evidence_json FROM catalog_identity_evidence WHERE id=? AND basis=?",
            params![evidence.evidence_id, basis(evidence.basis)],
            |r| r.get(0),
        )
        .optional()?;
    let body: Value = serde_json::from_str(
        &raw.ok_or_else(|| fail("catalog_identity_missing_equivalence_evidence"))?,
    )?;
    if body["contract_version"] != 1 {
        return Err(fail("catalog_identity_evidence_scope_mismatch"));
    }
    let claims: Vec<EquivalenceClaim> = serde_json::from_value(body["claims"].clone())
        .map_err(|_| fail("catalog_identity_evidence_scope_mismatch"))?;
    let expected: BTreeSet<_> = version
        .bindings
        .iter()
        .map(|b| (&b.file.file_id, &b.file.revision))
        .collect();
    if !claims.iter().any(|claim| {
        claim.edition_item_id == version.edition_item_id
            && claim.timeline_id == version.timeline_id
            && claim.timeline_revision == timeline_revision
            && claim.version_id == version.version_id
            && claim.version_revision == version_revision
            && claim.exact_files.len() == expected.len()
            && claim
                .exact_files
                .iter()
                .map(|f| (&f.file_id, &f.revision))
                .collect::<BTreeSet<_>>()
                == expected
    }) {
        return Err(fail("catalog_identity_evidence_scope_mismatch"));
    }
    Ok(())
}

fn validate_associations(db: &Connection, profile: &str, v: &VersionContract) -> Result<()> {
    validate_associations_with_occurrence(db, profile, v, true)
}
fn validate_associations_with_occurrence(
    db: &Connection,
    profile: &str,
    v: &VersionContract,
    current_occurrence: bool,
) -> Result<()> {
    checked(validate_version(v))?;
    require_live_version(db, &v.version_id)?;
    require_live_timeline(db, &v.timeline_id)?;
    for owner in [&v.association_item_id, &v.edition_item_id] {
        require_live_owner(db, owner)?;
        if !db
            .prepare("SELECT 1 FROM items WHERE id=?")?
            .exists([owner])?
        {
            return Err(fail("catalog_identity_missing_owner"));
        }
    }
    let active = rows(
        db,
        "SELECT id,file_id,part FROM item_files WHERE item_id=? AND role='primary' AND active=1 ORDER BY id LIMIT 1025",
        &[v.association_item_id.clone().into()],
    )?;
    if active.len() != v.bindings.len() {
        return Err(fail("catalog_identity_binding_set_mismatch"));
    }
    for binding in &v.bindings {
        if !active.iter().any(|r| {
            r["id"] == binding.association_id
                && r["file_id"] == binding.file.file_id
                && r["part"].as_u64() == Some(u64::from(binding.part))
        }) {
            return Err(fail("catalog_identity_binding_set_mismatch"));
        }
        if current_occurrence
            && components::revision(&components::occurrence(db, profile, &binding.file.file_id)?)
                != binding.file.revision
        {
            return Err(conflict("catalog_identity_stale_file_revision"));
        }
    }
    Ok(())
}

fn load_context(db: &Connection, profile: &str, context: &str) -> Result<Vec<ManifestVersion>> {
    let heads = rows(
        db,
        &format!(
            "SELECT v.id,v.association_item_id,v.current_revision,r.*,t.edition_item_id FROM {} v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id WHERE v.segmentation_context_id=? ORDER BY v.id LIMIT 65",
            live_versions(db)?
        ),
        &[context.to_owned().into()],
    )?;
    if heads.len() > 64 {
        return Err(fail("catalog_identity_manifest_budget"));
    }
    let mut result = vec![];
    let mut count = 0;
    for row in heads {
        let version_id = string(&row, "version_id")?;
        let records = rows(
            db,
            "SELECT b.association_id,r.* FROM catalog_version_bindings m JOIN catalog_bindings b ON b.id=m.binding_id JOIN catalog_binding_revisions r ON r.binding_id=m.binding_id AND r.revision=m.binding_revision WHERE m.version_id=? AND m.version_revision=? ORDER BY r.part LIMIT 1025",
            &[version_id.clone().into(), int(&row, "revision")?.into()],
        )?;
        count += records.len();
        if count > 1024 {
            return Err(fail("catalog_identity_manifest_budget"));
        }
        let mut bindings = vec![];
        for record in records {
            if record["profile"] != profile {
                return Err(fail("catalog_identity_profile_mismatch"));
            }
            let source_interval = interval(&record, "source_start_us", "source_end_us")?;
            bindings.push(VersionBinding {
                binding_id: string(&record, "binding_id")?,
                association_id: string(&record, "association_id")?,
                file: ExactFileRevision {
                    file_id: string(&record, "file_id")?,
                    revision: string(&record, "file_revision")?,
                },
                part: u32::try_from(int(&record, "part")?)
                    .map_err(|_| fail("catalog_identity_invalid_part"))?,
                source: match record["source_kind"].as_str() {
                    Some("whole_file") => SourceExtent::WholeFile {
                        duration_us: source_interval.map(IntervalUs::duration_us),
                    },
                    Some("slice") => SourceExtent::Slice {
                        interval: source_interval
                            .ok_or_else(|| fail("catalog_identity_invalid_interval"))?,
                    },
                    _ => return Err(fail("catalog_identity_invalid_record")),
                },
                timeline_interval: interval(&record, "timeline_start_us", "timeline_end_us")?,
            });
        }
        let equivalence = match row["equivalence_basis"].as_str() {
            None if row["equivalence_evidence_id"].is_null() => None,
            Some("declared") => Some(EquivalenceEvidence {
                basis: EquivalenceBasis::Declared,
                evidence_id: string(&row, "equivalence_evidence_id")?,
            }),
            Some("verified") => Some(EquivalenceEvidence {
                basis: EquivalenceBasis::Verified,
                evidence_id: string(&row, "equivalence_evidence_id")?,
            }),
            _ => return Err(fail("catalog_identity_invalid_record")),
        };
        result.push(ManifestVersion {
            version: VersionContract {
                contract_version: 1,
                version_id,
                association_item_id: string(&row, "association_item_id")?,
                edition_item_id: string(&row, "edition_item_id")?,
                timeline_id: string(&row, "timeline_id")?,
                state: match row["state"].as_str() {
                    Some("draft") => VersionState::Draft,
                    Some("ready") => VersionState::Ready,
                    _ => return Err(fail("catalog_identity_invalid_record")),
                },
                declared_coverage: interval(&row, "coverage_start_us", "coverage_end_us")?,
                bindings,
            },
            equivalence,
        });
    }
    Ok(result)
}

fn validate_active(db: &Connection, profile: &str, versions: &[ManifestVersion]) -> Result<()> {
    validate_active_selected(db, profile, versions, None)
}
fn validate_active_selected(
    db: &Connection,
    profile: &str,
    versions: &[ManifestVersion],
    selected: Option<&BTreeSet<String>>,
) -> Result<()> {
    let heads = live_versions(db)?;
    for entry in versions {
        validate_associations_with_occurrence(
            db,
            profile,
            &entry.version,
            selected.is_none_or(|ids| ids.contains(&entry.version.version_id)),
        )?;
        let row=rows(db,&format!("SELECT tr.*,t.current_revision,vr.timeline_revision FROM {heads} v JOIN catalog_version_revisions vr ON vr.version_id=v.id AND vr.revision=v.current_revision JOIN catalog_timelines t ON t.id=vr.timeline_id JOIN catalog_timeline_revisions tr ON tr.timeline_id=t.id AND tr.revision=vr.timeline_revision WHERE v.id=?"),&[entry.version.version_id.clone().into()])?.pop().ok_or_else(||fail("catalog_identity_missing_timeline"))?;
        if let Some(evidence) = &entry.equivalence {
            let revision: i64 = db.query_row(
                &format!("SELECT current_revision FROM {heads} WHERE id=?"),
                [&entry.version.version_id],
                |r| r.get(0),
            )?;
            validate_equivalence(
                db,
                &entry.version,
                revision,
                int(&row, "timeline_revision")?,
                evidence,
            )?;
        }
        if row["current_revision"] != row["timeline_revision"]
            || interval(&row, "coverage_start_us", "coverage_end_us")?
                != entry.version.declared_coverage
        {
            return Err(fail("catalog_identity_timeline_coverage_mismatch"));
        }
        let count:i64=db.query_row(&format!("SELECT COUNT(*) FROM {heads} v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision WHERE r.timeline_id=?"),[&entry.version.timeline_id],|r|r.get(0))?;
        if row["identity_state"] == "provisional" && (count > 1 || entry.equivalence.is_some()) {
            return Err(conflict("catalog_identity_provisional_timeline_collision"));
        }
        if count > 1 {
            let without_evidence:bool=db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {heads} v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision WHERE r.timeline_id=? AND r.equivalence_evidence_id IS NULL)"),[&entry.version.timeline_id],|r|r.get(0))?;
            if without_evidence {
                return Err(fail("catalog_identity_missing_equivalence_evidence"));
            }
        }
    }
    Ok(())
}
fn build_sets(context: &str, versions: &[ManifestVersion]) -> Result<Vec<SegmentationSet>> {
    let mut sets = BTreeMap::<(String, String), SegmentationSet>::new();
    for entry in versions {
        for binding in &entry.version.bindings {
            if let SourceExtent::Slice { interval } = binding.source {
                let key = (binding.file.file_id.clone(), binding.file.revision.clone());
                let set = sets.entry(key).or_insert_with(|| SegmentationSet {
                    contract_version: 1,
                    segmentation_set_id: format!(
                        "segment-{}",
                        sha256(encode(&json!([context, binding.file])))
                    ),
                    file: binding.file.clone(),
                    segments: vec![],
                });
                set.segments.push(ActiveSegment {
                    binding_id: binding.binding_id.clone(),
                    file: binding.file.clone(),
                    source_interval: interval,
                });
            }
        }
    }
    if sets.len() > 64 {
        return Err(fail("catalog_identity_manifest_budget"));
    }
    for set in sets.values() {
        checked(validate_segmentation_set(set))?;
    }
    Ok(sets.into_values().collect())
}
fn validate_context(db: &Connection, context: &str) -> Result<()> {
    // Both directions are required: missing unchanged members and stale extra
    // members are equally invalid. Snapshot membership is not caller input.
    let bindings = live_bindings(db)?;
    let mismatch:bool=db.query_row(&format!("SELECT EXISTS(SELECT binding_id,binding_revision FROM {bindings} WHERE segmentation_context_id=?1 AND source_kind='slice' EXCEPT SELECT m.binding_id,m.binding_revision FROM catalog_segment_members m JOIN catalog_segmentation_contexts c ON c.id=m.context_id AND c.current_revision=m.context_revision WHERE c.id=?1) OR EXISTS(SELECT m.binding_id,m.binding_revision FROM catalog_segment_members m JOIN catalog_segmentation_contexts c ON c.id=m.context_id AND c.current_revision=m.context_revision WHERE c.id=?1 EXCEPT SELECT binding_id,binding_revision FROM {bindings} WHERE segmentation_context_id=?1 AND source_kind='slice')"),[context],|r|r.get(0))?;
    if mismatch {
        return Err(conflict("catalog_identity_context_membership_mismatch"));
    }
    Ok(())
}

/// Load a complete authoritative editorial context. User-supplied manifests do
/// not participate. Draft versions remain available for inspection and are
/// never silently promoted to playback-ready representations.
pub fn load_manifest(
    store: &Store,
    profile: &str,
    version_ids: &[String],
) -> crate::Result<CatalogManifest> {
    load_manifest_typed(store, profile, version_ids).map_err(Into::into)
}
fn load_manifest_typed(
    store: &Store,
    profile: &str,
    version_ids: &[String],
) -> Result<CatalogManifest> {
    store.transaction_available()?;
    if store.schema_version < 33 {
        return Err(unsupported("catalog_identity_native_schema_required"));
    }
    if version_ids.is_empty() || version_ids.len() > 64 {
        return Err(fail("catalog_identity_candidate_budget"));
    }
    let before: i64 =
        store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })?;
    let mut context = None;
    for version in version_ids {
        require_live_version(&store.db, version)?;
        let found: Option<String> = store
            .db
            .query_row(
                &format!(
                    "SELECT segmentation_context_id FROM {} WHERE id=?",
                    live_versions(&store.db)?
                ),
                [version],
                |r| r.get(0),
            )
            .optional()?;
        let found = found.ok_or_else(|| fail("catalog_identity_unknown_version"))?;
        if context.as_ref().is_some_and(|old| old != &found) {
            return Err(conflict("catalog_identity_context_scope_mismatch"));
        }
        context = Some(found);
    }
    let context = context.expect("nonempty candidates");
    validate_context(&store.db, &context)?;
    let versions = load_context(&store.db, profile, &context)?;
    validate_active(&store.db, profile, &versions)?;
    let segmentation_sets = build_sets(&context, &versions)?;
    let expected_epoch = crate::fallback_resolution::epoch(store)?;
    let after: i64 =
        store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })?;
    if before != after {
        return Err(conflict("catalog_identity_stale_generation"));
    }
    store.transaction_available()?;
    Ok(CatalogManifest {
        contract_version: 1,
        database_id: store.database_id.clone(),
        expected_epoch,
        versions,
        segmentation_sets,
    })
}

/// The legacy resolver may represent only one ready, unshifted whole-file
/// binding. Slices, multipart version decisions, drafts and retired bindings
/// need the explicit native reader and cannot become whole-file effects.
pub(crate) fn legacy_association_admission(
    store: &Store,
    profile: &str,
    association: &str,
) -> crate::Result<()> {
    legacy_association_admission_typed(store, profile, association).map_err(Into::into)
}
fn legacy_association_admission_typed(
    store: &Store,
    profile: &str,
    association: &str,
) -> Result<()> {
    if store.schema_version < 33 {
        return Ok(());
    }
    store.transaction_available()?;
    let owner: Option<String> = store
        .db
        .query_row(
            "SELECT item_id FROM item_files WHERE id=?",
            [association],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(owner) = owner {
        require_live_owner(&store.db, &owner)?;
    }
    // Retained ownership is intentional: a retired typed association must never
    // fall through as an unclaimed legacy whole-file association.
    let owned: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_bindings WHERE association_id=?1) OR EXISTS(SELECT 1 FROM item_files a JOIN catalog_versions v ON v.association_item_id=a.item_id WHERE a.id=?1)",
        [association], |r|r.get(0),
    )?;
    if !owned {
        return Ok(());
    }
    let bindings = live_bindings(&store.db)?;
    let binding: Option<(String,String,String)> = store.db.query_row(
        &format!("SELECT r.file_id,r.file_revision,r.version_id FROM {bindings} r WHERE r.association_id=?1 AND r.profile=?2 AND r.state='ready' AND r.source_kind='whole_file' AND r.source_start_us=0 AND r.timeline_start_us=0 AND r.source_end_us=r.timeline_end_us AND (SELECT COUNT(*) FROM {bindings} all_parts WHERE all_parts.version_id=r.version_id)=1"),
        params![association,profile], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional()?;
    let Some((file, revision, version)) = binding else {
        return Err(unsupported("catalog_identity_legacy_binding_unsupported"));
    };
    let timeline: String = store.db.query_row(
        "SELECT r.timeline_id FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision WHERE v.id=?",
        [&version], |r| r.get(0),
    )?;
    require_live_timeline(&store.db, &timeline)?;
    if components::revision(&components::occurrence(&store.db, profile, &file)?) != revision {
        return Err(conflict("catalog_identity_stale_file_revision"));
    }
    Ok(())
}

#[cfg(test)]
mod retirement_seam_tests {
    use super::*;

    /// A small behavioral harness, deliberately independent of historical
    /// migration SQL. Full native constraints belong to integration coverage.
    fn fixture() -> Store {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE profiles(id TEXT PRIMARY KEY);
             INSERT INTO profiles VALUES('default');
             CREATE TABLE items(id TEXT PRIMARY KEY);
             INSERT INTO items VALUES('edition'),('old-owner'),('new-owner'),('keep-owner'),('keep-edition');
             CREATE TABLE item_files(id TEXT,item_id TEXT,role TEXT,active INTEGER,file_id TEXT,part INTEGER);
             CREATE TABLE catalog_state(id INTEGER,incarnation TEXT,generation INTEGER);
             INSERT INTO catalog_state VALUES(1,'incarnation',23);
             CREATE TABLE fallback_epoch(id INTEGER,generation INTEGER);
             INSERT INTO fallback_epoch VALUES(1,1);
             CREATE TABLE catalog_mutation_permits(scope_kind TEXT,scope_id TEXT,incarnation TEXT);
             CREATE TABLE catalog_segmentation_contexts(id TEXT PRIMARY KEY,authority_namespace TEXT,context_key TEXT,current_revision INTEGER);
             INSERT INTO catalog_segmentation_contexts VALUES('context','host','editorial',1);
             CREATE TABLE catalog_segmentation_revisions(context_id TEXT,revision INTEGER,recorded_at_us INTEGER,command_catalog_id TEXT,command_host_namespace TEXT,command_operation_id TEXT);
             INSERT INTO catalog_segmentation_revisions VALUES('context',1,0,'catalog','host','seed');
             CREATE TABLE catalog_timelines(id TEXT PRIMARY KEY,edition_item_id TEXT,current_revision INTEGER);
             INSERT INTO catalog_timelines VALUES('timeline','edition',1),('keep-timeline','keep-edition',1);
             CREATE TABLE catalog_timeline_revisions(timeline_id TEXT,revision INTEGER,identity_state TEXT,coverage_start_us INTEGER,coverage_end_us INTEGER,recorded_at_us INTEGER,command_catalog_id TEXT,command_host_namespace TEXT,command_operation_id TEXT);
             INSERT INTO catalog_timeline_revisions VALUES('timeline',1,'provisional',NULL,NULL,0,'catalog','host','seed'),('keep-timeline',1,'provisional',NULL,NULL,0,'catalog','host','seed');
             CREATE TABLE catalog_versions(id TEXT PRIMARY KEY,association_item_id TEXT,segmentation_context_id TEXT,current_revision INTEGER);
             INSERT INTO catalog_versions VALUES('old-version','old-owner','context',1),('keep-version','keep-owner','context',1);
             CREATE TABLE catalog_version_revisions(version_id TEXT,revision INTEGER,timeline_id TEXT,timeline_revision INTEGER,state TEXT,coverage_start_us INTEGER,coverage_end_us INTEGER,equivalence_evidence_id TEXT,equivalence_basis TEXT,recorded_at_us INTEGER,command_catalog_id TEXT,command_host_namespace TEXT,command_operation_id TEXT);
             INSERT INTO catalog_version_revisions VALUES('old-version',1,'timeline',1,'draft',NULL,NULL,NULL,NULL,0,'catalog','host','seed'),('keep-version',1,'keep-timeline',1,'draft',NULL,NULL,NULL,NULL,0,'catalog','host','seed');
             CREATE TABLE catalog_bindings(id TEXT PRIMARY KEY,version_id TEXT,association_id TEXT);
             INSERT INTO catalog_bindings VALUES('old-binding','old-version','old-association');
             CREATE TABLE catalog_binding_revisions(binding_id TEXT,revision INTEGER,profile TEXT,file_id TEXT,file_revision TEXT,part INTEGER,source_kind TEXT,source_start_us INTEGER,source_end_us INTEGER,timeline_start_us INTEGER,timeline_end_us INTEGER,legacy_origin_id TEXT,recorded_at_us INTEGER,command_catalog_id TEXT,command_host_namespace TEXT,command_operation_id TEXT);
             INSERT INTO catalog_binding_revisions VALUES('old-binding',1,'default','file','exact',1,'slice',0,10,0,10,NULL,0,'catalog','host','seed');
             CREATE TABLE catalog_version_bindings(version_id TEXT,version_revision INTEGER,binding_id TEXT,binding_revision INTEGER);
             INSERT INTO catalog_version_bindings VALUES('old-version',1,'old-binding',1);
             CREATE TABLE catalog_identity_evidence(id TEXT,basis TEXT,evidence_json TEXT,recorded_at_us INTEGER);
             CREATE TABLE catalog_segmentation_sets(id TEXT,context_id TEXT,profile TEXT,file_id TEXT,file_revision TEXT);
             INSERT INTO catalog_segmentation_sets VALUES('old-set','context','default','file','exact');
             CREATE TABLE catalog_segment_members(context_id TEXT,context_revision INTEGER,set_id TEXT,binding_id TEXT,binding_revision INTEGER);
             INSERT INTO catalog_segment_members VALUES('context',1,'old-set','old-binding',1);
             CREATE VIEW catalog_active_binding_revisions AS
               SELECT r.*,m.binding_revision,b.version_id,b.association_id,v.segmentation_context_id,vr.state
               FROM catalog_versions v
               JOIN catalog_version_revisions vr ON vr.version_id=v.id AND vr.revision=v.current_revision
               JOIN catalog_version_bindings m ON m.version_id=v.id AND m.version_revision=v.current_revision
               JOIN catalog_bindings b ON b.id=m.binding_id
               JOIN catalog_binding_revisions r ON r.binding_id=m.binding_id AND r.revision=m.binding_revision;
             CREATE TABLE catalog_version_retirements(version_id TEXT PRIMARY KEY,version_revision INTEGER,command_catalog_id TEXT,command_host_namespace TEXT,command_operation_id TEXT,recorded_at_us INTEGER);
             CREATE TABLE catalog_logical_retirements(item_id TEXT PRIMARY KEY);
             CREATE VIEW catalog_live_versions AS SELECT v.* FROM catalog_versions v WHERE NOT EXISTS(SELECT 1 FROM catalog_version_retirements r WHERE r.version_id=v.id);
             CREATE VIEW catalog_live_binding_revisions AS SELECT b.* FROM catalog_active_binding_revisions b WHERE NOT EXISTS(SELECT 1 FROM catalog_version_retirements r WHERE r.version_id=b.version_id);",
        )
        .unwrap();
        Store {
            offline_bootstrap: None,
            candidate41: false,
            db,
            path: std::path::PathBuf::new(),
            writable: true,
            schema_version: 34,
            database_id: "catalog".into(),
            catalog_incarnation: Some("incarnation".into()),
            native_path_identity: None,
            native_admission: None,
            lock: None,
            transaction_poisoned: std::cell::Cell::new(false),
            catalog_operation_budget: std::cell::RefCell::new(None),
        }
    }

    fn mutation() -> IdentityMutation {
        IdentityMutation {
            contract_version: 1,
            profile: "default".into(),
            context: ContextChange {
                id: "context".into(),
                authority_namespace: "host".into(),
                context_key: "editorial".into(),
                expected_revision: Some(1),
            },
            timelines: vec![],
            versions: vec![VersionChange {
                expected_revision: None,
                expected_timeline_revision: 1,
                version: VersionContract {
                    contract_version: 1,
                    version_id: "new-version".into(),
                    association_item_id: "new-owner".into(),
                    edition_item_id: "edition".into(),
                    timeline_id: "timeline".into(),
                    state: VersionState::Draft,
                    declared_coverage: None,
                    bindings: vec![],
                },
                equivalence: None,
                expected_binding_revisions: BTreeMap::new(),
            }],
            evidence: vec![],
        }
    }

    fn command() -> CommandEnvelope {
        // The reviewed generation can precede logical-owner creation writes.
        CommandEnvelope::new(
            1,
            CommandKey::new("catalog", "host", "replace").unwrap(),
            "incarnation",
            0,
            "{}",
        )
        .unwrap()
    }

    fn authority() -> IdentityAuthority {
        IdentityAuthority {
            authority_namespace: "host".into(),
            context_keys: ["editorial".into()].into(),
            allow_declared_evidence: true,
            allow_verified_evidence: false,
        }
    }

    fn replace(store: &Store, mutation: &IdentityMutation) -> Result<Value> {
        apply_identity_replacement(
            store,
            mutation,
            &command(),
            &authority(),
            123,
            &[("old-version".into(), 1)],
        )
    }

    fn scalar(store: &Store, sql: &str) -> i64 {
        store.db.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn replacement_preserves_history_and_unchanged_live_members() {
        let store = fixture();
        store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        let result = replace(&store, &mutation()).unwrap();
        assert_eq!(result["context_revision"], 2);
        let active = load_context(&store.db, "default", "context").unwrap();
        assert_eq!(
            active
                .iter()
                .map(|v| v.version.version_id.as_str())
                .collect::<Vec<_>>(),
            ["keep-version", "new-version"]
        );
        validate_context(&store.db, "context").unwrap();
        assert_eq!(
            scalar(
                &store,
                "SELECT COUNT(*) FROM catalog_segment_members WHERE context_revision=1"
            ),
            1
        );
        assert_eq!(
            scalar(
                &store,
                "SELECT COUNT(*) FROM catalog_segment_members WHERE context_revision=2"
            ),
            0
        );
        assert_eq!(
            scalar(
                &store,
                "SELECT COUNT(*) FROM catalog_binding_revisions WHERE binding_id='old-binding'"
            ),
            1
        );
        assert_eq!(
            scalar(
                &store,
                "SELECT COUNT(*) FROM catalog_version_revisions WHERE version_id='old-version' AND revision=1 AND state='draft'"
            ),
            1
        );
        assert_eq!(
            scalar(
                &store,
                "SELECT COUNT(*) FROM catalog_version_retirements WHERE version_id='old-version' AND version_revision=1 AND command_catalog_id='catalog' AND command_host_namespace='host' AND command_operation_id='replace' AND recorded_at_us=123"
            ),
            1
        );
        assert_eq!(
            scalar(&store, "SELECT COUNT(*) FROM catalog_mutation_permits"),
            0
        );
        store.db.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn late_failure_rolls_back_retirement_and_complete_replacement() {
        let store = fixture();
        store.db.execute_batch("CREATE TEMP TRIGGER reject_epoch BEFORE UPDATE ON fallback_epoch BEGIN SELECT RAISE(ABORT,'injected_late_failure'); END; BEGIN IMMEDIATE;").unwrap();
        let error = replace(&store, &mutation()).unwrap_err();
        assert_eq!(error.code, IdentityErrorCode::StoreFailure);
        assert!(error.to_string().contains("injected_late_failure"));
        assert_eq!(
            scalar(&store, "SELECT COUNT(*) FROM catalog_version_retirements"),
            1
        );
        assert_eq!(
            scalar(
                &store,
                "SELECT COUNT(*) FROM catalog_versions WHERE id='new-version'"
            ),
            1
        );
        assert_eq!(
            scalar(
                &store,
                "SELECT current_revision FROM catalog_segmentation_contexts"
            ),
            2
        );
        store.db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            scalar(&store, "SELECT COUNT(*) FROM catalog_version_retirements"),
            0
        );
        assert_eq!(
            scalar(
                &store,
                "SELECT COUNT(*) FROM catalog_versions WHERE id='new-version'"
            ),
            0
        );
        assert_eq!(
            scalar(
                &store,
                "SELECT current_revision FROM catalog_segmentation_contexts"
            ),
            1
        );
        assert_eq!(
            scalar(&store, "SELECT COUNT(*) FROM catalog_mutation_permits"),
            0
        );
        validate_context(&store.db, "context").unwrap();
    }

    #[test]
    fn old_membership_must_be_valid_before_retirement_can_hide_it() {
        let store = fixture();
        store
            .db
            .execute_batch("DELETE FROM catalog_segment_members; BEGIN IMMEDIATE")
            .unwrap();
        let error = replace(&store, &mutation()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "catalog_identity_context_membership_mismatch"
        );
        assert_eq!(
            scalar(&store, "SELECT COUNT(*) FROM catalog_version_retirements"),
            0
        );
        assert_eq!(
            scalar(
                &store,
                "SELECT current_revision FROM catalog_segmentation_contexts"
            ),
            1
        );
    }

    #[test]
    fn replacement_requires_exact_context_and_version_heads() {
        let store = fixture();
        store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut changed = mutation();
        changed.context.expected_revision = Some(2);
        assert_eq!(
            replace(&store, &changed).unwrap_err().to_string(),
            "catalog_identity_context_revision_conflict"
        );
        let error = apply_identity_replacement(
            &store,
            &mutation(),
            &command(),
            &authority(),
            123,
            &[("old-version".into(), 2)],
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "catalog_identity_version_revision_conflict"
        );
        let error = apply_identity_replacement(
            &store,
            &mutation(),
            &command(),
            &authority(),
            123,
            &[("old-version".into(), 1), ("old-version".into(), 1)],
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "catalog_identity_duplicate_retirement");
        store.db.execute("UPDATE catalog_versions SET segmentation_context_id='other-context' WHERE id='keep-version'", []).unwrap();
        let error = apply_identity_replacement(
            &store,
            &mutation(),
            &command(),
            &authority(),
            123,
            &[("keep-version".into(), 1)],
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "catalog_identity_context_scope_mismatch");
        assert_eq!(
            scalar(&store, "SELECT COUNT(*) FROM catalog_version_retirements"),
            0
        );
    }

    #[test]
    fn replacement_requires_schema34_and_a_writer_transaction() {
        let mut store = fixture();
        assert_eq!(
            replace(&store, &mutation()).unwrap_err().to_string(),
            "catalog_identity_writer_transaction_required"
        );
        store.db.execute_batch("BEGIN DEFERRED").unwrap();
        assert_eq!(
            replace(&store, &mutation()).unwrap_err().to_string(),
            "catalog_identity_writer_transaction_required"
        );
        store.db.execute_batch("ROLLBACK; BEGIN IMMEDIATE").unwrap();
        store.schema_version = 33;
        assert_eq!(
            replace(&store, &mutation()).unwrap_err().to_string(),
            "catalog_identity_retirement_schema_required"
        );
        assert_eq!(
            scalar(&store, "SELECT COUNT(*) FROM catalog_version_retirements"),
            0
        );
    }

    #[test]
    fn retirement_rejects_id_owner_and_timeline_resurrection() {
        let store = fixture();
        store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        replace(&store, &mutation()).unwrap();
        assert_eq!(
            require_live_version(&store.db, "old-version")
                .unwrap_err()
                .to_string(),
            "catalog_identity_retired_version"
        );
        assert_eq!(
            require_live_owner(&store.db, "old-owner")
                .unwrap_err()
                .to_string(),
            "catalog_identity_retired_owner"
        );
        let mut again = mutation();
        again.context.expected_revision = Some(2);
        assert_eq!(
            replace(&store, &again).unwrap_err().to_string(),
            "catalog_identity_retired_version"
        );
        store
            .db
            .execute(
                "INSERT INTO catalog_logical_retirements VALUES('edition')",
                [],
            )
            .unwrap();
        assert_eq!(
            require_live_timeline(&store.db, "timeline")
                .unwrap_err()
                .to_string(),
            "catalog_identity_retired_owner"
        );
        assert_eq!(
            require_live_targets(&store.db, &mutation())
                .unwrap_err()
                .to_string(),
            "catalog_identity_retired_owner"
        );
    }

    #[test]
    fn replacement_keeps_explicit_equivalence_validation() {
        let store = fixture();
        store.db.execute_batch("UPDATE catalog_version_revisions SET timeline_id='timeline' WHERE version_id='keep-version'; UPDATE catalog_timeline_revisions SET identity_state='established' WHERE timeline_id='timeline'; BEGIN IMMEDIATE").unwrap();
        let error = replace(&store, &mutation()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "catalog_identity_missing_equivalence_evidence"
        );
        store.db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            scalar(&store, "SELECT COUNT(*) FROM catalog_version_retirements"),
            0
        );
    }

    #[test]
    fn readers_refuse_retired_candidates_and_legacy_fallthrough() {
        let mut store = fixture();
        store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        replace(&store, &mutation()).unwrap();
        store.db.execute_batch("COMMIT").unwrap();
        // This synthetic reader has no disk identity or writer lock to fence.
        store.writable = false;
        store.catalog_incarnation = None;
        assert_eq!(
            load_manifest_typed(&store, "default", &["old-version".into()])
                .unwrap_err()
                .to_string(),
            "catalog_identity_retired_version"
        );
        assert_eq!(
            legacy_association_admission_typed(&store, "default", "old-association")
                .unwrap_err()
                .to_string(),
            "catalog_identity_legacy_binding_unsupported"
        );
        store
            .db
            .execute(
                "INSERT INTO item_files VALUES('old-association','old-owner','primary',1,'file',1)",
                [],
            )
            .unwrap();
        assert_eq!(
            legacy_association_admission_typed(&store, "default", "old-association")
                .unwrap_err()
                .to_string(),
            "catalog_identity_retired_owner"
        );
    }

    #[test]
    fn legacy_unclaimed_association_cannot_bypass_logical_retirement() {
        let mut store = fixture();
        store.writable = false;
        store.catalog_incarnation = None;
        store.db.execute_batch("INSERT INTO catalog_logical_retirements VALUES('new-owner'); INSERT INTO item_files VALUES('unclaimed','new-owner','primary',1,'file',1)").unwrap();
        assert_eq!(
            legacy_association_admission_typed(&store, "default", "unclaimed")
                .unwrap_err()
                .to_string(),
            "catalog_identity_retired_owner"
        );
    }

    #[test]
    fn schema33_uses_unchanged_live_read_paths() {
        let store = fixture();
        store.db.execute_batch("DROP VIEW catalog_live_binding_revisions; DROP VIEW catalog_live_versions; DROP TABLE catalog_version_retirements; DROP TABLE catalog_logical_retirements;").unwrap();
        assert_eq!(live_versions(&store.db).unwrap(), "catalog_versions");
        assert_eq!(
            live_bindings(&store.db).unwrap(),
            "catalog_active_binding_revisions"
        );
        require_live_version(&store.db, "old-version").unwrap();
        require_live_owner(&store.db, "old-owner").unwrap();
        validate_context(&store.db, "context").unwrap();
    }
}
