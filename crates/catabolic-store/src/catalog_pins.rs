// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Accepted identity decisions with DB-only mutations. Public reads validate
//! their Store lifecycle before and after reading. Legacy identities and provider
//! suggestions are evidence, never implicit accepted decisions or timeline proof.
//! The trusted host establishes authority independently of request JSON. Only the
//! central durable dispatcher may call apply_pins, in its admitted transaction;
//! it owns native session/lifetime checks, rollback, the receipt and commit.

use crate::{Error as StoreError, Store};
use catabolic_core::{catalog_command::CommandEnvelope, encode, sha256};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_PIN_BATCH: usize = 128;
pub const MAX_PIN_VALUE_BYTES: usize = 1024;
pub const MAX_PIN_HISTORY_PAGE: usize = 128;
pub const MAX_LEGACY_EVIDENCE_PAGE: usize = 128;

/// Privileged host material, intentionally not Deserialize. Mutation permission
/// includes read access to the same exact namespace/item pairs. No wildcard,
/// caller-chosen actor, inferred item authority, or namespace-only write grant.
#[derive(Clone, Debug)]
pub struct PinAuthority {
    pub host_namespace: String,
    pub readable: BTreeMap<String, BTreeSet<String>>,
    pub mutable: BTreeMap<String, BTreeSet<String>>,
}
impl PinAuthority {
    fn permits(&self, item: &str, namespace: &str, mutation: bool) -> bool {
        self.mutable
            .get(namespace)
            .is_some_and(|items| items.contains(item))
            || (!mutation
                && self
                    .readable
                    .get(namespace)
                    .is_some_and(|items| items.contains(item)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PinDecision {
    Set { value: String },
    Remove,
}
impl<'de> Deserialize<'de> for PinDecision {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // Serde's internally tagged unit variants otherwise ignore stray fields
        // even with deny_unknown_fields. An empty struct variant is strict.
        #[derive(Deserialize)]
        #[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
        enum Fields {
            Set { value: String },
            Remove {},
        }
        Ok(match Fields::deserialize(deserializer)? {
            Fields::Set { value } => Self::Set { value },
            Fields::Remove {} => Self::Remove,
        })
    }
}
impl PinDecision {
    fn value(&self) -> Option<&str> {
        match self {
            Self::Set { value } => Some(value),
            Self::Remove => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinChange {
    pub item_id: String,
    pub namespace: String,
    /// Zero means no decision has EVER existed. Removal tombstones require
    /// their positive retained revision when explicitly recreating a pin.
    /// Required in JSON: omitting it does not imply creation.
    pub expected_revision: i64,
    pub decision: PinDecision,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinBatch {
    pub contract_version: u32,
    pub changes: Vec<PinChange>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinMutation {
    pub batch: PinBatch,
    pub preview_digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinHead {
    pub revision: i64,
    /// None is an explicit retained removal, not absence of a decision.
    pub value: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinPreviewChange {
    pub change: PinChange,
    pub expected_item_revision: i64,
    pub before: Option<PinHead>,
    pub after: PinHead,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinPreview {
    pub contract_version: u32,
    pub catalog_id: String,
    pub incarnation: String,
    pub expected_generation: i64,
    pub changes: Vec<PinPreviewChange>,
    pub digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinApplyResult {
    pub operation: &'static str,
    pub preview_digest: String,
    pub decisions: Vec<PinPreviewChange>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinLookup {
    pub item_id: String,
    pub namespace: String,
    /// Only explicit pin records produce this field. Raw identities never do.
    pub decision: Option<PinHead>,
    /// Present only for an active explicit pin. Removed/never-pinned return None.
    pub accepted_value: Option<String>,
    /// Legacy evidence is fetched by the separate bounded provenance API.
    pub legacy_evidence_available: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinHistoryEntry {
    pub revision: i64,
    pub value: Option<String>,
    pub recorded_at_us: i64,
    pub command_catalog_id: String,
    pub command_host_namespace: String,
    pub command_operation_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinHistoryPage {
    pub entries: Vec<PinHistoryEntry>,
    pub next_after_revision: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LegacyIdentityEvidence {
    pub value: String,
    pub provenance: &'static str,
    /// Distinguishes the immutable snapshot from the current legacy tuple.
    pub present_in_legacy_table: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LegacyIdentityPage {
    pub entries: Vec<LegacyIdentityEvidence>,
    pub next_after_value: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinErrorCode {
    InvalidInput,
    BudgetExceeded,
    AuthorityDenied,
    UnsupportedSchema,
    WriterTransactionRequired,
    AmbientTransaction,
    RevisionConflict,
    DuplicateDecision,
    ValueOwned,
    NoChange,
    UnknownSubject,
    RetiredSubject,
    StalePreview,
    StoreFailure,
}
impl PinErrorCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "catalog_pin_invalid_input",
            Self::BudgetExceeded => "catalog_pin_budget_exceeded",
            Self::AuthorityDenied => "catalog_pin_authority_denied",
            Self::UnsupportedSchema => "catalog_pin_schema_34_required",
            Self::WriterTransactionRequired => "catalog_pin_writer_transaction_required",
            Self::AmbientTransaction => "catalog_pin_public_read_ambient_transaction",
            Self::RevisionConflict => "catalog_pin_revision_conflict",
            Self::DuplicateDecision => "catalog_pin_duplicate_decision",
            Self::ValueOwned => "catalog_pin_value_owned",
            Self::NoChange => "catalog_pin_no_change",
            Self::UnknownSubject => "catalog_pin_unknown_subject",
            Self::RetiredSubject => "catalog_pin_retired_subject",
            Self::StalePreview => "catalog_pin_stale_preview",
            Self::StoreFailure => "catalog_pin_store_failure",
        }
    }
}
#[derive(Debug)]
pub struct PinError {
    pub code: PinErrorCode,
    pub error: StoreError,
}
impl std::fmt::Display for PinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.code(), self.error)
    }
}
impl std::error::Error for PinError {}
impl From<StoreError> for PinError {
    fn from(error: StoreError) -> Self {
        Self {
            code: PinErrorCode::StoreFailure,
            error,
        }
    }
}
impl From<rusqlite::Error> for PinError {
    fn from(e: rusqlite::Error) -> Self {
        Self {
            code: PinErrorCode::StoreFailure,
            error: StoreError::from(e),
        }
    }
}
impl From<serde_json::Error> for PinError {
    fn from(e: serde_json::Error) -> Self {
        Self {
            code: PinErrorCode::StoreFailure,
            error: StoreError::from(e),
        }
    }
}
pub type Result<T> = std::result::Result<T, PinError>;
fn error(code: PinErrorCode) -> PinError {
    PinError {
        code,
        error: StoreError(code.code().into()),
    }
}
fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
fn require_schema(store: &Store) -> Result<()> {
    if store.schema_version < 34 {
        return Err(error(PinErrorCode::UnsupportedSchema));
    }
    Ok(())
}
fn authorize_pair(authority: &PinAuthority, item: &str, ns: &str, mutation: bool) -> Result<()> {
    if !identifier(&authority.host_namespace, 256) || !authority.permits(item, ns, mutation) {
        return Err(error(PinErrorCode::AuthorityDenied));
    }
    Ok(())
}
fn validate_pair(item: &str, namespace: &str) -> Result<()> {
    if !identifier(item, 256) || !identifier(namespace, 256) {
        return Err(error(PinErrorCode::InvalidInput));
    }
    Ok(())
}
fn validate_authorize(batch: &PinBatch, authority: &PinAuthority) -> Result<()> {
    if batch.changes.is_empty() || batch.changes.len() > MAX_PIN_BATCH {
        return Err(error(PinErrorCode::BudgetExceeded));
    }
    // Complete authority is checked before any DB read, including existence,
    // collision ownership, initial heads, generation, or partial preview data.
    for c in &batch.changes {
        authorize_pair(authority, &c.item_id, &c.namespace, true)?;
    }
    if batch.contract_version != 1 {
        return Err(error(PinErrorCode::InvalidInput));
    }
    let mut seen = BTreeSet::new();
    let mut proposed_values = BTreeSet::new();
    for c in &batch.changes {
        validate_pair(&c.item_id, &c.namespace)?;
        if c.expected_revision < 0 || c.expected_revision == i64::MAX {
            return Err(error(PinErrorCode::RevisionConflict));
        }
        if !seen.insert((&c.item_id, &c.namespace)) {
            return Err(error(PinErrorCode::DuplicateDecision));
        }
        if let Some(value) = c.decision.value() {
            if !identifier(value, MAX_PIN_VALUE_BYTES) {
                return Err(error(PinErrorCode::InvalidInput));
            }
            if !proposed_values.insert((&c.namespace, value)) {
                return Err(error(PinErrorCode::ValueOwned));
            }
        }
    }
    Ok(())
}
#[derive(PartialEq, Eq)]
struct Fence {
    catalog: String,
    incarnation: String,
    generation: i64,
}
fn fence(db: &Connection) -> Result<Fence> {
    Ok(db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |r| {
            Ok(Fence {
                catalog: r.get(0)?,
                incarnation: r.get(1)?,
                generation: r.get(2)?,
            })
        },
    )?)
}
fn head(db: &Connection, item: &str, namespace: &str) -> Result<Option<PinHead>> {
    Ok(db.query_row("SELECT current_revision,value FROM catalog_identity_pins WHERE item_id=? AND namespace=?",
        params![item, namespace], |r| Ok(PinHead { revision:r.get(0)?, value:r.get(1)? })).optional()?)
}

// Public reads validate the actual Store admission/lifetime on both sides of
// their database reads. The internal command mutation seam remains DB-only;
// its central dispatcher already owns that lifecycle validation.
fn begin_public_read(store: &Store) -> Result<()> {
    require_schema(store)?;
    if !store.db.is_autocommit() {
        return Err(error(PinErrorCode::AmbientTransaction));
    }
    store.transaction_available()?;
    Ok(())
}
fn finish_public_read(store: &Store, expected: &Fence) -> Result<()> {
    if !store.db.is_autocommit() {
        return Err(error(PinErrorCode::AmbientTransaction));
    }
    store.transaction_available()?;
    if &fence(&store.db)? != expected {
        return Err(error(PinErrorCode::StalePreview));
    }
    Ok(())
}

/// Read-only, deterministic, completely authorized preview. It fences related
/// rows and phantoms using the coarse catalog generation; no write transaction
/// is opened. The dispatcher still revalidates every precondition on apply.
pub fn preview_pins(
    store: &Store,
    batch: &PinBatch,
    authority: &PinAuthority,
) -> Result<PinPreview> {
    validate_authorize(batch, authority)?;
    begin_public_read(store)?;
    let preview = preview_pins_in_transaction(store, batch, authority)?;
    finish_public_read(
        store,
        &Fence {
            catalog: preview.catalog_id.clone(),
            incarnation: preview.incarnation.clone(),
            generation: preview.expected_generation,
        },
    )?;
    Ok(preview)
}

pub(crate) fn preview_pins_in_transaction(
    store: &Store,
    batch: &PinBatch,
    authority: &PinAuthority,
) -> Result<PinPreview> {
    validate_authorize(batch, authority)?;
    require_schema(store)?;
    let start = fence(&store.db)?;
    // The independently prepared logical edit owner preserves retired IDs.
    // Standalone pin preparation may omit that fragment; the integrated schema
    // signature must include it. Retired IDs never redirect to a survivor/child.
    let has_retirements: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='catalog_logical_retirements')",
        [], |r| r.get(0))?;
    let mut requested = batch.changes.clone();
    requested.sort_by(|a, b| (&a.item_id, &a.namespace).cmp(&(&b.item_id, &b.namespace)));
    let mut changes = Vec::with_capacity(requested.len());
    for change in requested {
        if has_retirements
            && store.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?)",
                [&change.item_id],
                |r| r.get::<_, bool>(0),
            )?
        {
            return Err(error(PinErrorCode::RetiredSubject));
        }
        let item_revision: Option<i64> = store.db.query_row(
            "SELECT e.revision FROM items i JOIN catalog_entity_revisions e ON e.entity_kind='items'
             AND e.entity_key=json_array(i.id) AND e.deleted=0 WHERE i.id=?", [&change.item_id], |r| r.get(0)).optional()?;
        let item_revision = item_revision.ok_or_else(|| error(PinErrorCode::UnknownSubject))?;
        let before = head(&store.db, &change.item_id, &change.namespace)?;
        if before.as_ref().map_or(0, |h| h.revision) != change.expected_revision {
            return Err(error(PinErrorCode::RevisionConflict));
        }
        if before.as_ref().and_then(|h| h.value.as_deref()) == change.decision.value() {
            return Err(error(PinErrorCode::NoChange));
        }
        if let Some(value) = change.decision.value() {
            let owned: bool = store.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM catalog_identity_pins WHERE namespace=? AND value=? AND item_id<>?)",
                params![change.namespace,value,change.item_id], |r| r.get(0))?;
            // Never disclose an unrelated owner, silently rehome it, or infer
            // permission from a second requested item. Release in a prior command.
            if owned {
                return Err(error(PinErrorCode::ValueOwned));
            }
        }
        let after = PinHead {
            revision: change.expected_revision + 1,
            value: change.decision.value().map(str::to_owned),
        };
        changes.push(PinPreviewChange {
            change,
            expected_item_revision: item_revision,
            before,
            after,
        });
    }
    if fence(&store.db)? != start {
        return Err(error(PinErrorCode::StalePreview));
    }
    let mut preview = PinPreview {
        contract_version: 1,
        catalog_id: start.catalog,
        incarnation: start.incarnation,
        expected_generation: start.generation,
        changes,
        digest: String::new(),
    };
    // Domain separation and an empty digest field make this self-contained and
    // canonical; serialized changes are sorted, object keys use core encoding.
    preview.digest = sha256(
        encode(
            &serde_json::json!({"domain":"catabolic.accepted-pins.preview.v1", "preview":preview}),
        )
        .as_bytes(),
    );
    Ok(preview)
}

/// Dispatcher-only mutation seam. The enclosing owner must roll back on ANY
/// error, insert the standard receipt last, and own commit. No independent
/// receipts, callbacks, transactions, network, filesystem, or timeline inference.
pub(crate) fn apply_pins(
    store: &Store,
    mutation: &PinMutation,
    command: &CommandEnvelope,
    authority: &PinAuthority,
    admitted_at_us: i64,
) -> Result<PinApplyResult> {
    validate_authorize(&mutation.batch, authority)?;
    if command.key().host_namespace() != authority.host_namespace {
        return Err(error(PinErrorCode::AuthorityDenied));
    }
    require_schema(store)?;
    if store.db.is_autocommit() {
        return Err(error(PinErrorCode::WriterTransactionRequired));
    }
    if admitted_at_us < 0
        || mutation.preview_digest.len() != 64
        || !mutation
            .preview_digest
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(error(PinErrorCode::InvalidInput));
    }
    let preview = preview_pins_in_transaction(store, &mutation.batch, authority)?;
    if preview.catalog_id != command.key().catalog_id()
        || preview.incarnation != command.admission_incarnation()
        || preview.expected_generation != command.expected_generation()
        || preview.digest != mutation.preview_digest
    {
        return Err(error(PinErrorCode::StalePreview));
    }
    for c in &preview.changes {
        store.db.execute("INSERT INTO catalog_identity_pin_revisions
            (item_id,namespace,revision,value,recorded_at_us,command_catalog_id,command_host_namespace,command_operation_id)
            VALUES(?,?,?,?,?,?,?,?)", params![c.change.item_id,c.change.namespace,c.after.revision,c.after.value,
            admitted_at_us,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id()])?;
        if c.before.is_none() {
            store.db.execute("INSERT INTO catalog_identity_pins(item_id,namespace,current_revision,value) VALUES(?,?,?,?)",
                params![c.change.item_id,c.change.namespace,c.after.revision,c.after.value])?;
        } else {
            let changed = store.db.execute(
                "UPDATE catalog_identity_pins SET current_revision=?,value=?
                WHERE item_id=? AND namespace=? AND current_revision=?",
                params![
                    c.after.revision,
                    c.after.value,
                    c.change.item_id,
                    c.change.namespace,
                    c.change.expected_revision
                ],
            )?;
            if changed != 1 {
                return Err(error(PinErrorCode::RevisionConflict));
            }
        }
    }
    Ok(PinApplyResult {
        operation: "identity_pins",
        preview_digest: preview.digest,
        decisions: preview.changes,
    })
}

/// Bounded native accepted lookup by exact authorized item/namespace. This API
/// never mistakes raw legacy tuples (including provider refreshes) for decisions.
pub fn lookup_pin(
    store: &Store,
    item: &str,
    namespace: &str,
    authority: &PinAuthority,
) -> Result<PinLookup> {
    authorize_pair(authority, item, namespace, false)?;
    validate_pair(item, namespace)?;
    begin_public_read(store)?;
    let start = fence(&store.db)?;
    let decision = head(&store.db, item, namespace)?;
    let legacy_evidence_available=store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_legacy_identity_evidence WHERE item_id=? AND namespace=?)",
        params![item,namespace], |r| r.get(0))?;
    finish_public_read(store, &start)?;
    Ok(PinLookup {
        item_id: item.into(),
        namespace: namespace.into(),
        accepted_value: decision.as_ref().and_then(|h| h.value.clone()),
        decision,
        legacy_evidence_available,
    })
}

/// History is independent of the current subject's existence. The host must
/// retain/resolve the authority for an archived subject before invoking this API.
pub fn pin_history(
    store: &Store,
    item: &str,
    namespace: &str,
    after_revision: i64,
    limit: usize,
    authority: &PinAuthority,
) -> Result<PinHistoryPage> {
    authorize_pair(authority, item, namespace, false)?;
    validate_pair(item, namespace)?;
    if after_revision < 0 || limit == 0 || limit > MAX_PIN_HISTORY_PAGE {
        return Err(error(PinErrorCode::BudgetExceeded));
    }
    begin_public_read(store)?;
    let start = fence(&store.db)?;
    let mut stmt=store.db.prepare("SELECT revision,value,recorded_at_us,command_catalog_id,command_host_namespace,command_operation_id
        FROM catalog_identity_pin_revisions WHERE item_id=? AND namespace=? AND revision>? ORDER BY revision LIMIT ?")?;
    let mut entries = stmt
        .query_map(
            params![item, namespace, after_revision, (limit + 1) as i64],
            |r| {
                Ok(PinHistoryEntry {
                    revision: r.get(0)?,
                    value: r.get(1)?,
                    recorded_at_us: r.get(2)?,
                    command_catalog_id: r.get(3)?,
                    command_host_namespace: r.get(4)?,
                    command_operation_id: r.get(5)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let next_after_revision = if entries.len() > limit {
        entries.pop();
        entries.last().map(|e| e.revision)
    } else {
        None
    };
    finish_public_read(store, &start)?;
    Ok(PinHistoryPage {
        entries,
        next_after_revision,
    })
}

/// Legacy tuples remain exact and separately visible as unknown-origin evidence.
/// Old over-budget values are preserved in storage but never allocated into an
/// unbounded response. Explicit migration/export tooling can handle those cases.
pub fn legacy_identity_evidence(
    store: &Store,
    item: &str,
    namespace: &str,
    after_value: Option<&str>,
    limit: usize,
    authority: &PinAuthority,
) -> Result<LegacyIdentityPage> {
    authorize_pair(authority, item, namespace, false)?;
    validate_pair(item, namespace)?;
    if limit == 0
        || limit > MAX_LEGACY_EVIDENCE_PAGE
        || after_value.is_some_and(|v| v.len() > MAX_PIN_VALUE_BYTES)
    {
        return Err(error(PinErrorCode::BudgetExceeded));
    }
    begin_public_read(store)?;
    let start = fence(&store.db)?;
    let mut stmt=store.db.prepare("SELECT CASE WHEN length(CAST(e.value AS BLOB))<=1024 THEN e.value END,
        EXISTS(SELECT 1 FROM identities i WHERE i.namespace=e.namespace AND i.value=e.value AND i.item_id=e.item_id)
        FROM catalog_legacy_identity_evidence e WHERE item_id=? AND namespace=? AND (? IS NULL OR value>?)
        ORDER BY value LIMIT ?")?;
    let rows = stmt.query_map(
        params![
            item,
            namespace,
            after_value,
            after_value,
            (limit + 1) as i64
        ],
        |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, bool>(1)?)),
    )?;
    let mut entries = Vec::new();
    for row in rows {
        let (value, present_in_legacy_table) = row?;
        let value = value.ok_or_else(|| error(PinErrorCode::BudgetExceeded))?;
        entries.push(LegacyIdentityEvidence {
            value,
            provenance: "legacy_import",
            present_in_legacy_table,
        });
    }
    let next_after_value = if entries.len() > limit {
        entries.pop();
        entries.last().map(|e| e.value.clone())
    } else {
        None
    };
    finish_public_read(store, &start)?;
    Ok(LegacyIdentityPage {
        entries,
        next_after_value,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AcceptedIdentityOwner {
    pub item_id: String,
    pub namespace: String,
    pub value: String,
    pub decision_revision: i64,
}

/// Resolve an accepted value only within a caller-supplied, fully authorized,
/// bounded item set. A raw legacy tuple or an owner outside that set does not
/// produce a result. This is not a global item-discovery or rehoming API.
pub fn accepted_identity_owner(
    store: &Store,
    namespace: &str,
    value: &str,
    candidate_item_ids: &[String],
    authority: &PinAuthority,
) -> Result<Option<AcceptedIdentityOwner>> {
    if candidate_item_ids.is_empty() || candidate_item_ids.len() > MAX_PIN_BATCH {
        return Err(error(PinErrorCode::BudgetExceeded));
    }
    for item in candidate_item_ids {
        authorize_pair(authority, item, namespace, false)?;
    }
    if !identifier(value, MAX_PIN_VALUE_BYTES) {
        return Err(error(PinErrorCode::InvalidInput));
    }
    let mut seen = BTreeSet::new();
    for item in candidate_item_ids {
        validate_pair(item, namespace)?;
        if !seen.insert(item) {
            return Err(error(PinErrorCode::InvalidInput));
        }
    }
    begin_public_read(store)?;
    let start = fence(&store.db)?;
    let candidates = encode(&serde_json::to_value(candidate_item_ids)?);
    let result = store.db.query_row(
        "SELECT item_id,current_revision FROM catalog_identity_pins WHERE namespace=? AND value=?
         AND item_id IN (SELECT value FROM json_each(?))",
        params![namespace,value,candidates],
        |r| Ok(AcceptedIdentityOwner {
            item_id:r.get(0)?, namespace:namespace.into(), value:value.into(), decision_revision:r.get(1)?,
        }),
    ).optional()?;
    finish_public_read(store, &start)?;
    Ok(result)
}
