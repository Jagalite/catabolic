// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded native snapshot staging through the existing catalog command owner.
//!
//! This is not a second catalog or an execution engine. Immutable command results
//! hold the manifest and batches, with atomic reconciliation holds. No staged row
//! enters items, identity, availability, selections or search. `ValidatedStaging`
//! proves integrity, logical-ID uniqueness and current destination noncollision,
//! not semantic import readiness or an external application's reference audit.
//! Activation uses existing curation and optional V2 typed owners with an explicit
//! trusted drained host freeze and durable reference acknowledgment. Unresolved
//! media/identity evidence and unqualified effects remain blocked. Unknown
//! evidence stays unresolved and is retained with
//! the command codec’s canonical JSON values; raw input whitespace is not stored.

use crate::{Error, Store, curation};
use catabolic_core::{
    catalog_command::CommandEnvelope,
    catalog_host_lease::{HostAuthorization, Revocation},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const IMPORT_CONTRACT_VERSION: u32 = 1;
pub const TYPED_IMPORT_CONTRACT_VERSION: u32 = 2;
/// Version three adds an explicitly empty snapshot. An empty source needs the
/// same frozen-source audit, drained activation and durable restart handshake as
/// a populated source; it must not invent a placeholder catalog item.
pub const EMPTY_IMPORT_CONTRACT_VERSION: u32 = 3;
fn supported(version: u32) -> bool {
    matches!(
        version,
        IMPORT_CONTRACT_VERSION | TYPED_IMPORT_CONTRACT_VERSION | EMPTY_IMPORT_CONTRACT_VERSION
    )
}
pub const MAX_IMPORT_BATCHES: usize = 16;
pub const MAX_IMPORT_BATCH_BYTES: usize = 64 * 1024;
pub const MAX_IMPORT_ITEMS_PER_BATCH: usize = 64;
pub const MAX_IMPORT_EVIDENCE_PER_BATCH: usize = 64;
pub const IMPORT_HOLDER_NAMESPACE: &str = "catabolic.import.v1";
pub(crate) const OPERATION_PREFIX: &str = "catalog-import-v1/";

/// Current trusted host grant. It is not deserializable and confers no authority
/// to activate, read unrelated receipts, scan sources or alter existing items.
#[derive(Clone, Debug)]
pub struct ImportAuthority {
    pub host_namespace: String,
    pub import_id: String,
    pub source_namespace: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportErrorCode {
    InvalidInput,
    UnsupportedContract,
    AuthorityDenied,
    SnapshotConflict,
    MissingStage,
    StageExpired,
    DuplicateSourceId,
    DestinationCollision,
    ActivationUnqualified,
    FreezeNotDrained,
    StaleValidation,
    RecoveryRequired,
    CorruptReceipt,
    StoreFailure,
}
impl ImportErrorCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "catalog_import_invalid_input",
            Self::UnsupportedContract => "catalog_import_unsupported_contract",
            Self::AuthorityDenied => "catalog_import_authority_denied",
            Self::SnapshotConflict => "catalog_import_snapshot_conflict",
            Self::MissingStage => "catalog_import_missing_stage",
            Self::StageExpired => "catalog_import_stage_expired",
            Self::DuplicateSourceId => "catalog_import_duplicate_source_id",
            Self::DestinationCollision => "catalog_import_destination_collision",
            Self::ActivationUnqualified => "catalog_import_activation_unqualified",
            Self::FreezeNotDrained => "catalog_import_freeze_not_drained",
            Self::StaleValidation => "catalog_import_stale_validation",
            Self::RecoveryRequired => "catalog_import_recovery_required",
            Self::CorruptReceipt => "catalog_import_corrupt_receipt",
            Self::StoreFailure => "catalog_import_store_failure",
        }
    }
}
#[derive(Debug)]
pub struct ImportError {
    pub code: ImportErrorCode,
    pub error: Error,
}
impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.code(), self.error)
    }
}
impl std::error::Error for ImportError {}
impl From<Error> for ImportError {
    fn from(error: Error) -> Self {
        Self {
            code: ImportErrorCode::StoreFailure,
            error,
        }
    }
}

impl From<rusqlite::Error> for ImportError {
    fn from(error: rusqlite::Error) -> Self {
        Self {
            code: ImportErrorCode::StoreFailure,
            error: error.into(),
        }
    }
}
pub type Result<T> = std::result::Result<T, ImportError>;
fn fail<T>(code: ImportErrorCode, message: &str) -> Result<T> {
    Err(ImportError {
        code,
        error: Error(message.into()),
    })
}
fn invalid(message: &str) -> ImportError {
    ImportError {
        code: ImportErrorCode::InvalidInput,
        error: Error(message.into()),
    }
}
fn identifier(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn is_zero(value: &usize) -> bool {
    *value == 0
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn encoded<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_value(value)
        .map(|v| catabolic_core::encode_with(&v, false, true))
        .map_err(|_| invalid("snapshot cannot be encoded"))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchDescriptor {
    pub sha256: String,
    pub canonical_bytes: usize,
    pub items: usize,
    pub unresolved_evidence: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub typed_records: usize,
}

/// Describes one frozen input, not a claim that the source is actually frozen.
/// The host must retain its authoritative export and original command intents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifest {
    pub contract_version: u32,
    pub import_id: String,
    pub source_namespace: String,
    pub source_snapshot_id: String,
    pub batches: Vec<BatchDescriptor>,
}

/// Metadata is retained as legacy/unknown provenance. It is never promoted to a
/// provider contribution or local decision. Accepted identities belong in
/// unresolved evidence until their existing owner has a qualified import path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportItem {
    pub source_id: String,
    pub item_id: String,
    pub kind: String,
    pub legacy_metadata: Value,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Contribution,
    AcceptedDecision,
    PhysicalOccurrence,
    TimelineVersion,
    ApplicationReference,
}
/// Data is retained evidence, never interpreted as a path to open, a mapping,
/// available coverage, timeline equivalence or a passed reference audit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedEvidence {
    pub evidence_id: String,
    pub kind: EvidenceKind,
    pub data: Value,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportBatch {
    pub items: Vec<ImportItem>,
    pub unresolved_evidence: Vec<UnresolvedEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typed: Option<crate::catalog_import_typed::TypedBatch>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportStep {
    Prepare,
    Batch(u32),
    Validate,
    Activate,
}
/// Deterministic reserved keys bind an import ID even when callers attempt to
/// choose a fresh command identity. As with every command, persist and reuse the
/// original envelope on retry; rebuilding it with newer preconditions conflicts.
pub fn operation_id(import_id: &str, step: ImportStep) -> Result<String> {
    if !identifier(import_id) {
        return Err(invalid("invalid import ID"));
    }
    let suffix = match step {
        ImportStep::Prepare => "prepare".into(),
        ImportStep::Batch(index) => format!("batch/{index}"),
        ImportStep::Validate => "validate".into(),
        ImportStep::Activate => "activate".into(),
    };
    Ok(format!(
        "{OPERATION_PREFIX}{}/{suffix}",
        catabolic_core::sha256(import_id.as_bytes())
    ))
}

pub fn describe_batch(batch: &ImportBatch) -> Result<BatchDescriptor> {
    validate_batch(batch)?;
    let body = encoded(batch)?;
    Ok(BatchDescriptor {
        sha256: catabolic_core::sha256(body.as_bytes()),
        canonical_bytes: body.len(),
        items: batch.items.len(),
        unresolved_evidence: batch.unresolved_evidence.len(),
        typed_records: batch.typed.as_ref().map_or(0, |typed| typed.record_count()),
    })
}
pub fn manifest_digest(manifest: &SnapshotManifest) -> Result<String> {
    validate_manifest(manifest)?;
    Ok(catabolic_core::sha256(encoded(manifest)?.as_bytes()))
}
fn validate_manifest(manifest: &SnapshotManifest) -> Result<()> {
    if !supported(manifest.contract_version) {
        return fail(
            ImportErrorCode::UnsupportedContract,
            "unsupported snapshot contract",
        );
    }
    if !identifier(&manifest.import_id)
        || !identifier(&manifest.source_namespace)
        || !identifier(&manifest.source_snapshot_id)
        || manifest.batches.is_empty() && manifest.contract_version != EMPTY_IMPORT_CONTRACT_VERSION
        || manifest.batches.len() > MAX_IMPORT_BATCHES
    {
        return Err(invalid("manifest identifiers or batch bound invalid"));
    }
    for batch in &manifest.batches {
        if !digest(&batch.sha256)
            || batch.canonical_bytes == 0
            || batch.canonical_bytes > MAX_IMPORT_BATCH_BYTES
            || batch.items > MAX_IMPORT_ITEMS_PER_BATCH
            || batch.unresolved_evidence > MAX_IMPORT_EVIDENCE_PER_BATCH
            || batch.typed_records > 64
            || manifest.contract_version == 1 && batch.typed_records != 0
            || batch.items + batch.unresolved_evidence + batch.typed_records == 0
        {
            return Err(invalid("invalid bounded batch descriptor"));
        }
    }
    Ok(())
}
fn validate_batch(batch: &ImportBatch) -> Result<()> {
    if batch.items.len() > MAX_IMPORT_ITEMS_PER_BATCH
        || batch.unresolved_evidence.len() > MAX_IMPORT_EVIDENCE_PER_BATCH
        || batch.items.is_empty()
            && batch.unresolved_evidence.is_empty()
            && batch
                .typed
                .as_ref()
                .is_none_or(|typed| typed.record_count() == 0)
        || encoded(batch)?.len() > MAX_IMPORT_BATCH_BYTES
    {
        return Err(invalid("batch is empty or exceeds its bound"));
    }
    if let Some(typed) = &batch.typed {
        typed.validate()?;
    }
    let mut sources = BTreeSet::new();
    let mut destinations = BTreeSet::new();
    let mut evidence_ids = BTreeSet::new();
    for item in &batch.items {
        if !identifier(&item.source_id) || !identifier(&item.item_id) {
            return Err(invalid("invalid logical item ID"));
        }
        if !sources.insert(&item.source_id) {
            return fail(
                ImportErrorCode::DuplicateSourceId,
                "duplicate source logical ID",
            );
        }
        if !destinations.insert(&item.item_id) {
            return fail(
                ImportErrorCode::DestinationCollision,
                "multiple source items target one logical ID",
            );
        }
        curation::validate_item_input(&serde_json::json!({"id":item.item_id,"kind":item.kind,"metadata":item.legacy_metadata}))
            .map_err(|error| ImportError { code: ImportErrorCode::InvalidInput, error })?;
    }
    for evidence in &batch.unresolved_evidence {
        if !identifier(&evidence.evidence_id) || !evidence.data.is_object() {
            return Err(invalid("invalid unresolved evidence"));
        }
        if !evidence_ids.insert(&evidence.evidence_id) {
            return fail(
                ImportErrorCode::DuplicateSourceId,
                "duplicate unresolved evidence ID",
            );
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportMutation {
    Prepare {
        manifest: SnapshotManifest,
        input_digest: String,
    },
    Stage {
        import_id: String,
        input_digest: String,
        batch_index: u32,
        batch: ImportBatch,
    },
    Validate {
        import_id: String,
        input_digest: String,
    },
    Activate {
        import_id: String,
        input_digest: String,
        reference_ack_id: String,
        mapping_digest: String,
    },
}
impl ImportMutation {
    fn identity(&self) -> (&str, &str, ImportStep) {
        match self {
            Self::Prepare {
                manifest,
                input_digest,
            } => (&manifest.import_id, input_digest, ImportStep::Prepare),
            Self::Stage {
                import_id,
                input_digest,
                batch_index,
                ..
            } => (import_id, input_digest, ImportStep::Batch(*batch_index)),
            Self::Validate {
                import_id,
                input_digest,
            } => (import_id, input_digest, ImportStep::Validate),
            Self::Activate {
                import_id,
                input_digest,
                ..
            } => (import_id, input_digest, ImportStep::Activate),
        }
    }
    pub(crate) fn hold_id(&self) -> String {
        catabolic_core::sha256(self.identity().0.as_bytes())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalMapping {
    pub source_id: String,
    pub item_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportReceipt {
    Prepared {
        contract_version: u32,
        input_digest: String,
        manifest: SnapshotManifest,
    },
    Staged {
        contract_version: u32,
        import_id: String,
        input_digest: String,
        batch_index: u32,
        batch: ImportBatch,
    },
    ValidatedStaging {
        contract_version: u32,
        import_id: String,
        input_digest: String,
        validation_generation: i64,
        proposed_mappings: Vec<LogicalMapping>,
        unresolved_evidence: usize,
        activation_supported: bool,
        mapping_digest: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        proposed_typed_mappings: Vec<Value>,
    },
    Activated {
        contract_version: u32,
        catalog_id: String,
        incarnation: String,
        import_id: String,
        input_digest: String,
        source_snapshot_id: String,
        reference_ack_id: String,
        mapping_digest: String,
        freeze_revision: String,
        mappings: Vec<LogicalMapping>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        typed_results: Vec<Value>,
        /// Exact item heads captured atomically with activation, not inferred
        /// from a later restart snapshot after unrelated native mutations.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        item_revision_baselines: BTreeMap<String, i64>,
    },
}
fn read_receipt(
    store: &Store,
    command: &CommandEnvelope,
    import_id: &str,
    step: ImportStep,
) -> Result<ImportReceipt> {
    let key = operation_id(import_id, step)?;
    let row: Option<(String, Option<String>)> = store.db.query_row(
        "SELECT result_state,result_json FROM catalog_command_receipts WHERE catalog_id=?1 AND host_namespace=?2 AND operation_id=?3",
        params![command.key().catalog_id(), command.key().host_namespace(), key],
        |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
    match row {
        None => fail(
            ImportErrorCode::MissingStage,
            "required import stage has not committed",
        ),
        Some((state, _)) if state == "expired" => fail(
            ImportErrorCode::StageExpired,
            "required import receipt expired; never repeat or replace this import ID",
        ),
        Some((state, Some(body))) if state == "full" => {
            serde_json::from_str(&body).map_err(|_| ImportError {
                code: ImportErrorCode::CorruptReceipt,
                error: Error("invalid import receipt".into()),
            })
        }
        _ => fail(
            ImportErrorCode::CorruptReceipt,
            "invalid import receipt state",
        ),
    }
}
fn prepared(
    store: &Store,
    command: &CommandEnvelope,
    import_id: &str,
    input_digest: &str,
) -> Result<SnapshotManifest> {
    match read_receipt(store, command, import_id, ImportStep::Prepare)? {
        ImportReceipt::Prepared {
            contract_version,
            input_digest: stored_digest,
            manifest,
        } if supported(contract_version)
            && contract_version == manifest.contract_version
            && manifest.import_id == import_id
            && stored_digest == input_digest
            && manifest_digest(&manifest)? == input_digest =>
        {
            Ok(manifest)
        }
        _ => fail(
            ImportErrorCode::SnapshotConflict,
            "import ID is bound to a different snapshot",
        ),
    }
}
fn require_no_collision(store: &Store, batch: &ImportBatch) -> Result<()> {
    for item in &batch.items {
        if store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM items WHERE id=?1)",
            [&item.item_id],
            |row| row.get::<_, bool>(0),
        )? {
            return fail(
                ImportErrorCode::DestinationCollision,
                "requested logical item ID already exists; explicit reconciliation is required",
            );
        }
    }
    Ok(())
}

/// Borrowed proof of the complete descriptor-validated activation input. Only
/// this import owner constructs it after validate_staged; ordinary typed owner
/// grants or an individual caller batch cannot manufacture this proof.
pub(crate) struct ValidatedNfoImport<'a> {
    command: &'a CommandEnvelope,
    batches: &'a [crate::catalog_import_typed::TypedBatch],
}
impl<'a> ValidatedNfoImport<'a> {
    pub(crate) fn check(
        &self,
        command: &CommandEnvelope,
        batches: &[crate::catalog_import_typed::TypedBatch],
    ) -> crate::Result<()> {
        if command != self.command || !std::ptr::eq(batches, self.batches) {
            return Err(Error("complete validated import input differs".into()));
        }
        Ok(())
    }
    pub(crate) fn command(&self) -> &'a CommandEnvelope {
        self.command
    }
    pub(crate) fn source_entry(
        &self,
        subject: &str,
        namespace: &str,
        record: &str,
    ) -> crate::Result<ValidatedImportSource<'a>> {
        let mut entries = self
            .batches
            .iter()
            .flat_map(|b| &b.metadata)
            .filter(|m| m.item_id == subject)
            .flat_map(|m| {
                m.contributions
                    .iter()
                    .filter(|c| c.namespace == namespace && c.record_id == record)
                    .map(move |c| (m, c))
            });
        let (metadata, source) = entries
            .next()
            .ok_or_else(|| Error("validated source absent".into()))?;
        if entries.next().is_some() {
            return Err(Error("duplicate validated source".into()));
        }
        Ok(ValidatedImportSource {
            outer: self.command,
            subject: &metadata.item_id,
            contract_version: metadata.contract_version,
            source,
        })
    }
    pub(crate) fn source(
        &self,
        subject: &str,
    ) -> crate::Result<(
        &'a str,
        &'a crate::catalog_import_typed::ImportedContribution,
        Option<&'a crate::catalog_import_typed::ImportedTagSet>,
    )> {
        let mut sources = self
            .batches
            .iter()
            .flat_map(|b| &b.metadata)
            .filter(|m| m.item_id == subject)
            .flat_map(|m| {
                m.contributions
                    .iter()
                    .filter(|c| c.namespace == "motion.nfo")
                    .map(move |c| (m.item_id.as_str(), c))
            });
        let source = sources
            .next()
            .ok_or_else(|| Error("validated NFO metadata source absent".into()))?;
        if sources.next().is_some() {
            return Err(Error("duplicate validated NFO source".into()));
        }
        let mut tags = self
            .batches
            .iter()
            .flat_map(|b| &b.tags)
            .filter(|t| t.item_id == subject && t.namespace == "motion.nfo");
        let tag = tags.next();
        if tags.next().is_some() {
            return Err(Error("duplicate validated NFO tag set".into()));
        }
        Ok((source.0, source.1, tag))
    }
}

/// Exact borrowed declared source from a complete descriptor-validated import.
/// This is not an NFO source-effect permit or post-bootstrap system-write grant.
pub(crate) struct ValidatedImportSource<'a> {
    outer: &'a CommandEnvelope,
    subject: &'a str,
    contract_version: u32,
    source: &'a crate::catalog_import_typed::ImportedContribution,
}
impl ValidatedImportSource<'_> {
    fn check_in(
        &self,
        store: &Store,
        command: &CommandEnvelope,
        grant: &crate::catalog_metadata_store::MetadataAuthority,
    ) -> crate::Result<()> {
        use crate::catalog_metadata_store::MetadataAccess;
        let exact = matches!(&grant.access,MetadataAccess::Source{namespace,record_id,kind}|MetadataAccess::SourceV2{namespace,record_id,kind,..} if namespace==&self.source.namespace && record_id==&self.source.record_id && *kind==self.source.kind.owner());
        if store.schema_version != 41
            || store.db.is_autocommit()
            || command.key() != self.outer.key()
            || command.admission_incarnation() != self.outer.admission_incarnation()
            || grant.subject != self.subject
            || grant.host_namespace != self.outer.key().host_namespace()
            || !exact
            || store
                .db
                .prepare("SELECT 1 FROM catalog_nfo_bootstrap")?
                .exists([])?
        {
            return Err(Error(
                "validated original import source scope differs".into(),
            ));
        }
        Ok(())
    }
    pub(crate) fn authorize_v1_in(
        &self,
        store: &Store,
        command: &CommandEnvelope,
        mutation: &crate::catalog_metadata_store::MetadataMutation,
        grant: &crate::catalog_metadata_store::MetadataAuthority,
    ) -> crate::Result<()> {
        use crate::catalog_metadata_store::MetadataChange;
        self.check_in(store, command, grant)?;
        let MetadataChange::Patch {
            subject,
            namespace,
            record_id,
            expected_revision,
            edits_json,
            evidence_json,
            ..
        } = &mutation.change
        else {
            return Err(Error("validated import requires first source Patch".into()));
        };
        let mut budget =
            crate::catalog_nfo_attribution::ProofBudget::new(store.catalog_operation_budget()?);
        let fields = self
            .source
            .fields
            .as_object()
            .ok_or_else(|| Error("validated fields not object".into()))?;
        let edits = crate::catalog_nfo_attribution::source_effect::ImportEdits(fields);
        let evidence = crate::catalog_nfo_attribution::source_effect::ImportEvidence {
            import_operation_id: self.outer.key().operation_id(),
            source_evidence: &self.source.evidence,
            source_revision: self.source.source_revision,
        };
        budget.serialized(&edits, 64)?;
        budget.serialized(&evidence, 64)?;
        if self.contract_version != 1
            || mutation.contract_version != 1
            || subject != self.subject
            || namespace != &self.source.namespace
            || record_id != &self.source.record_id
            || *expected_revision != 0
            || edits_json != &serde_json::to_string(&edits)?
            || evidence_json != &serde_json::to_string(&evidence)?
        {
            return Err(Error("validated imported Patch differs".into()));
        }
        Ok(())
    }
    pub(crate) fn authorize_v2_in(
        &self,
        store: &Store,
        command: &CommandEnvelope,
        subject: &str,
        namespace: &str,
        record: &str,
        fields: &Value,
        evidence: &Value,
        grant: &crate::catalog_metadata_store::MetadataAuthority,
    ) -> crate::Result<()> {
        self.check_in(store, command, grant)?;
        if self.contract_version != 2
            || subject != self.subject
            || namespace != self.source.namespace
            || record != self.source.record_id
            || fields != &self.source.fields
            || evidence.as_object().map(|m| m.len()) != Some(3)
            || evidence.get("import_operation_id").and_then(Value::as_str)
                != Some(self.outer.key().operation_id())
            || evidence.get("source_revision").and_then(Value::as_i64)
                != Some(self.source.source_revision)
            || evidence.get("source_evidence") != Some(&self.source.evidence)
        {
            return Err(Error("validated imported source differs".into()));
        }
        Ok(())
    }
}

/// Only the command dispatcher may call this under its writer transaction.
/// Its receipt/hold writes and rollback are owned by that dispatcher.
pub(crate) fn apply(
    store: &Store,
    command: &CommandEnvelope,
    mutation: &ImportMutation,
    authority: &ImportAuthority,
    activation: Option<&TrustedImportActivation>,
    typed_authority: Option<&crate::catalog_import_typed::TypedAuthority>,
    admitted_at_us: i64,
) -> Result<Value> {
    let (import_id, input_digest, step) = mutation.identity();
    if store.db.is_autocommit() {
        return fail(
            ImportErrorCode::StoreFailure,
            "import requires command writer transaction",
        );
    }
    if store.schema_version < 34 {
        return fail(
            ImportErrorCode::UnsupportedContract,
            "import staging requires native schema34 or later",
        );
    }
    if !identifier(import_id)
        || !digest(input_digest)
        || command.key().operation_id() != operation_id(import_id, step)?
    {
        return Err(invalid(
            "import requires its deterministic operation ID and input digest",
        ));
    }
    if authority.host_namespace != command.key().host_namespace()
        || authority.import_id != import_id
    {
        return fail(
            ImportErrorCode::AuthorityDenied,
            "current host import grant mismatch",
        );
    }
    let manifest = match mutation {
        ImportMutation::Prepare { manifest, .. } => {
            if manifest_digest(manifest)? != input_digest {
                return fail(
                    ImportErrorCode::SnapshotConflict,
                    "snapshot digest does not match manifest",
                );
            }
            manifest.clone()
        }
        _ => prepared(store, command, import_id, input_digest)?,
    };
    if authority.source_namespace != manifest.source_namespace {
        return fail(
            ImportErrorCode::AuthorityDenied,
            "source namespace is not authorized",
        );
    }
    let contract_version = manifest.contract_version;
    let result = match mutation {
        ImportMutation::Prepare { .. } => ImportReceipt::Prepared {
            contract_version,
            input_digest: input_digest.into(),
            manifest,
        },
        ImportMutation::Stage {
            batch_index, batch, ..
        } => {
            let expected = manifest
                .batches
                .get(*batch_index as usize)
                .ok_or_else(|| invalid("batch index outside manifest"))?;
            if &describe_batch(batch)? != expected {
                return fail(
                    ImportErrorCode::SnapshotConflict,
                    "batch does not match frozen snapshot descriptor",
                );
            }
            require_no_collision(store, batch)?;
            ImportReceipt::Staged {
                contract_version,
                import_id: import_id.into(),
                input_digest: input_digest.into(),
                batch_index: *batch_index,
                batch: batch.clone(),
            }
        }
        ImportMutation::Validate { .. } => {
            let StagedContents {
                items,
                mappings,
                unresolved_evidence,
                typed_batches,
            } = validate_staged(store, command, &manifest, input_digest)?;
            let validated = ValidatedNfoImport {
                command,
                batches: &typed_batches,
            };
            let proposed_typed_mappings = crate::catalog_import_typed::rehearse_validated(
                store,
                command,
                &items,
                &typed_batches,
                typed_authority,
                admitted_at_us,
                &validated,
            )?;
            let digest = complete_mapping_digest(&mappings, &proposed_typed_mappings)?;
            ImportReceipt::ValidatedStaging {
                contract_version,
                import_id: import_id.into(),
                input_digest: input_digest.into(),
                validation_generation: command.expected_generation(),
                proposed_mappings: mappings,
                unresolved_evidence,
                activation_supported: unresolved_evidence == 0,
                mapping_digest: digest,
                proposed_typed_mappings,
            }
        }
        ImportMutation::Activate {
            reference_ack_id,
            mapping_digest: requested_mapping_digest,
            ..
        } => {
            let grant = activation.ok_or_else(|| ImportError { code: ImportErrorCode::ActivationUnqualified, error: Error("activation requires a current drained freeze and durable reference acknowledgment".into()) })?;
            let (validated_generation, validated_digest) =
                match read_receipt(store, command, import_id, ImportStep::Validate)? {
                    ImportReceipt::ValidatedStaging {
                        contract_version,
                        import_id: stored_id,
                        input_digest: stored_digest,
                        activation_supported: true,
                        unresolved_evidence: 0,
                        validation_generation,
                        mapping_digest,
                        ..
                    } if contract_version == manifest.contract_version
                        && stored_id == import_id
                        && stored_digest == input_digest =>
                    {
                        (validation_generation, mapping_digest)
                    }
                    _ => {
                        return fail(
                            ImportErrorCode::ActivationUnqualified,
                            "unresolved evidence or missing validated import blocks activation",
                        );
                    }
                };
            if validated_generation != command.expected_generation() {
                return fail(
                    ImportErrorCode::StaleValidation,
                    "catalog changed after validation; keep admission closed and reconcile",
                );
            }
            grant.check(
                command,
                &manifest,
                input_digest,
                reference_ack_id,
                requested_mapping_digest,
                validated_generation,
            )?;
            let StagedContents {
                items,
                mappings,
                unresolved_evidence,
                typed_batches,
            } = validate_staged(store, command, &manifest, input_digest)?;
            if unresolved_evidence != 0 || &validated_digest != requested_mapping_digest {
                return fail(
                    ImportErrorCode::SnapshotConflict,
                    "reference acknowledgment does not match complete validated mapping",
                );
            }
            require_settled_cutover(store, false)?;
            let imported_ids: BTreeSet<String> =
                items.iter().map(|item| item.item_id.clone()).collect();
            create_import_items(store, &items)?;
            let validated = ValidatedNfoImport {
                command,
                batches: &typed_batches,
            };
            let typed_results = crate::catalog_import_typed::apply_validated(
                store,
                command,
                &typed_batches,
                &imported_ids,
                typed_authority,
                admitted_at_us,
                &validated,
            )?;
            if complete_mapping_digest(&mappings, &typed_results)? != validated_digest {
                return fail(
                    ImportErrorCode::SnapshotConflict,
                    "activated typed mapping differs from reviewed validation",
                );
            }
            let mut item_revision_baselines = BTreeMap::new();
            for item in &items {
                let revision:i64=store.db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?) AND deleted=0",[&item.item_id],|r|r.get(0)).map_err(|e|ImportError{code:ImportErrorCode::StoreFailure,error:e.into()})?;
                item_revision_baselines.insert(item.item_id.clone(), revision);
            }
            ImportReceipt::Activated {
                item_revision_baselines,
                contract_version,
                typed_results,
                catalog_id: command.key().catalog_id().into(),
                incarnation: command.admission_incarnation().into(),
                import_id: import_id.into(),
                input_digest: input_digest.into(),
                source_snapshot_id: manifest.source_snapshot_id,
                reference_ack_id: reference_ack_id.clone(),
                mapping_digest: validated_digest,
                freeze_revision: grant.freeze_revision.clone(),
                mappings,
            }
        }
    };
    serde_json::to_value(result).map_err(|_| ImportError {
        code: ImportErrorCode::StoreFailure,
        error: Error("cannot encode import result".into()),
    })
}

/// Trusted host statement recorded only after its reference audit and source
/// write freeze are durable. Not deserializable: application input cannot mint
/// this permission. The source snapshot and full proposed-map digest are exact.
#[derive(Clone, Debug)]
pub struct ActivationBinding {
    pub catalog_id: String,
    pub incarnation: String,
    pub import_id: String,
    pub input_digest: String,
    pub source_snapshot_id: String,
    pub validation_generation: i64,
    pub reference_ack_id: String,
    pub mapping_digest: String,
}
/// A closed, drained existing host admission gate held through command COMMIT.
/// The host must use this exact gate for every catalog/scanner admission and
/// retain worker leases for their actual lifetime. The gate alone cannot prove
/// that unrelated processes stopped or that an external reference audit ran.
/// Those are explicit host obligations, not facts inferred from request fields.
/// Dropping this value NEVER reopens the old gate.
#[derive(Debug)]
pub struct TrustedImportActivation {
    host_namespace: String,
    freeze_revision: String,
    freeze: Revocation,
    binding: ActivationBinding,
}
impl TrustedImportActivation {
    pub fn after_drain(
        host: &HostAuthorization,
        freeze: Revocation,
        binding: ActivationBinding,
    ) -> Result<Self> {
        if !freeze.belongs_to(host) {
            return fail(
                ImportErrorCode::AuthorityDenied,
                "freeze belongs to a different host gate",
            );
        }
        freeze
            .wait_drained(std::time::Duration::ZERO)
            .map_err(|_| ImportError {
                code: ImportErrorCode::FreezeNotDrained,
                error: Error("host gate still has admitted work or cannot prove drained".into()),
            })?;
        if [
            &binding.catalog_id,
            &binding.incarnation,
            &binding.import_id,
            &binding.source_snapshot_id,
            &binding.reference_ack_id,
        ]
        .iter()
        .any(|s| !identifier(s))
            || !digest(&binding.input_digest)
            || !digest(&binding.mapping_digest)
            || binding.validation_generation < 0
        {
            return Err(invalid("invalid trusted activation binding"));
        }
        Ok(Self {
            host_namespace: host.namespace().into(),
            freeze_revision: host.revision().into(),
            freeze,
            binding,
        })
    }
    fn check(
        &self,
        command: &CommandEnvelope,
        manifest: &SnapshotManifest,
        input_digest: &str,
        reference_ack_id: &str,
        mapping_digest: &str,
        generation: i64,
    ) -> Result<()> {
        self.freeze
            .wait_drained(std::time::Duration::ZERO)
            .map_err(|_| ImportError {
                code: ImportErrorCode::FreezeNotDrained,
                error: Error("activation freeze is not drained".into()),
            })?;
        let b = &self.binding;
        if self.host_namespace != command.key().host_namespace()
            || b.catalog_id != command.key().catalog_id()
            || b.incarnation != command.admission_incarnation()
            || b.import_id != manifest.import_id
            || b.input_digest != input_digest
            || b.source_snapshot_id != manifest.source_snapshot_id
            || b.validation_generation != generation
            || b.reference_ack_id != reference_ack_id
            || b.mapping_digest != mapping_digest
        {
            return fail(
                ImportErrorCode::AuthorityDenied,
                "activation grant, frozen source or durable acknowledgment does not match this exact command",
            );
        }
        Ok(())
    }
}

pub fn mapping_digest(mappings: &[LogicalMapping]) -> Result<String> {
    Ok(catabolic_core::sha256(
        encoded(
            &serde_json::json!({"contract_version":IMPORT_CONTRACT_VERSION,"mappings":mappings}),
        )?
        .as_bytes(),
    ))
}

struct StagedContents {
    items: Vec<ImportItem>,
    mappings: Vec<LogicalMapping>,
    unresolved_evidence: usize,
    typed_batches: Vec<crate::catalog_import_typed::TypedBatch>,
}
fn validate_staged(
    store: &Store,
    command: &CommandEnvelope,
    manifest: &SnapshotManifest,
    input_digest: &str,
) -> Result<StagedContents> {
    let mut sources = BTreeSet::new();
    let mut destinations = BTreeSet::new();
    let mut evidence_ids = BTreeSet::new();
    let mut mappings = Vec::new();
    let mut items = Vec::new();
    let mut typed_batches = Vec::new();
    for (index, descriptor) in manifest.batches.iter().enumerate() {
        let batch = match read_receipt(
            store,
            command,
            &manifest.import_id,
            ImportStep::Batch(index as u32),
        )? {
            ImportReceipt::Staged {
                contract_version,
                import_id: stored_id,
                input_digest: stored_digest,
                batch_index,
                batch,
            } if contract_version == manifest.contract_version
                && stored_id == manifest.import_id
                && stored_digest == input_digest
                && batch_index == index as u32
                && &describe_batch(&batch)? == descriptor =>
            {
                batch
            }
            _ => {
                return fail(
                    ImportErrorCode::SnapshotConflict,
                    "stored batch does not match manifest",
                );
            }
        };
        require_no_collision(store, &batch)?;
        if let Some(typed) = batch.typed {
            typed_batches.push(typed);
        }
        for item in batch.items {
            if !sources.insert(item.source_id.clone()) {
                return fail(
                    ImportErrorCode::DuplicateSourceId,
                    "duplicate source logical ID across batches",
                );
            }
            if !destinations.insert(item.item_id.clone()) {
                return fail(
                    ImportErrorCode::DestinationCollision,
                    "multiple source items target one logical ID across batches",
                );
            }
            mappings.push(LogicalMapping {
                source_id: item.source_id.clone(),
                item_id: item.item_id.clone(),
            });
            items.push(item);
        }
        for evidence in batch.unresolved_evidence {
            if !evidence_ids.insert(evidence.evidence_id) {
                return fail(
                    ImportErrorCode::DuplicateSourceId,
                    "duplicate unresolved evidence ID across batches",
                );
            }
        }
    }
    mappings.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    Ok(StagedContents {
        items,
        mappings,
        unresolved_evidence: evidence_ids.len(),
        typed_batches,
    })
}
// Admit only schema contracts shipped by this build. Additive supported native
// schemas retain the same exact companion witness; unknown future schemas do not.
fn supports_observation_cleanup(schema: u32) -> bool {
    schema >= crate::native_observation_companion::SCHEMA_VERSION
        && crate::migration::is_supported_native_schema(schema)
}

#[test]
fn observation_cleanup_schema_admission_rejects_unknown_contracts() {
    assert!(!supports_observation_cleanup(36));
    assert!(supports_observation_cleanup(37));
    assert!(supports_observation_cleanup(39));
    assert!(!supports_observation_cleanup(
        crate::migration::NATIVE_SCHEMA_VERSION + 1
    ));
    assert!(!supports_observation_cleanup(u32::MAX));
}

fn require_settled_cutover(store: &Store, activated_restart: bool) -> Result<()> {
    // Reuse the upgrade owner's conservative inventory, without executing an
    // upgrade or restore. Reject rather than recovering any external effect.
    let mut pending = crate::migration::native_pending_work(&store.db, store.schema_version)
        .map_err(|error| ImportError {
            code: ImportErrorCode::StoreFailure,
            error,
        })?;
    // Restart after an exact retained activation receipt is not an initial
    // import: historical incomplete scan coverage is not live worker custody.
    // Preserve every scope row and every other pending-work/cleanup check.
    if activated_restart && pending["observation_scopes"].as_i64().unwrap_or(0) > 0 {
        let jobs = store.db.prepare("SELECT DISTINCT job_id FROM observation_scopes WHERE state NOT IN ('complete','excluded','registry_complete') LIMIT 4097")?
            .query_map([], |r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        if jobs.len() > 4096 {
            return fail(
                ImportErrorCode::RecoveryRequired,
                "historical observation scope inventory exceeds restart bound",
            );
        }
        for job in jobs {
            let eligible:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM observation_jobs j WHERE j.id=?1 AND j.state IN ('complete','unavailable','failed','stale') AND j.lease_until=0 AND j.completed_at IS NOT NULL AND NOT EXISTS(SELECT 1 FROM observation_scopes s WHERE s.job_id=j.id AND s.state NOT IN ('complete','excluded','registry_complete','deferred')))",[&job],|r|r.get(0))?;
            if !eligible
                || !supports_observation_cleanup(store.schema_version)
                || !crate::native_observation_companion::has_cleanup_witness(store, &job).map_err(
                    |error| ImportError {
                        code: ImportErrorCode::StoreFailure,
                        error,
                    },
                )?
            {
                return fail(
                    ImportErrorCode::RecoveryRequired,
                    "incomplete observation scope has live or unqualified restart ownership",
                );
            }
        }
        pending["observation_scopes"] = serde_json::json!(0);
    }
    if !crate::migration::work_is_settled(&pending) {
        return fail(
            ImportErrorCode::RecoveryRequired,
            "pending durable work blocks import activation",
        );
    }
    // Complete bounded owner reconciliation is required in addition to the
    // pending queue inventory. Truncation or an unrelated owner never becomes
    // settled merely because one observation has a positive companion witness.
    let inventory =
        crate::native_worker_inventory::inspect(store).map_err(|error| ImportError {
            code: ImportErrorCode::StoreFailure,
            error,
        })?;
    if !inventory.admits_new_work() {
        return fail(
            ImportErrorCode::RecoveryRequired,
            "unresolved or truncated native ownership inventory blocks import activation",
        );
    }
    let watchers: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM watcher_runs WHERE worker_pid IS NOT NULL)",
        [],
        |r| r.get(0),
    )?;
    if watchers {
        return fail(
            ImportErrorCode::RecoveryRequired,
            "retained watcher ownership blocks import activation",
        );
    }
    // Only the exact qualified schema37 companion protocol can discharge a
    // retained observation pointer. This never clears a PID or derives a seal
    // from terminal state, elapsed time, process liveness, or a shared demand.
    let mut statement = store
        .db
        .prepare("SELECT id FROM observation_jobs WHERE worker_pid IS NOT NULL")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let job: String = row.get(0)?;
        if !supports_observation_cleanup(store.schema_version)
            || !crate::native_observation_companion::has_cleanup_witness(store, &job).map_err(
                |error| ImportError {
                    code: ImportErrorCode::StoreFailure,
                    error,
                },
            )?
        {
            return fail(
                ImportErrorCode::RecoveryRequired,
                "unqualified retained observation ownership blocks import activation",
            );
        }
    }
    drop(rows);
    drop(statement);
    crate::migration::require_native_journal(&store.db).map_err(|error| ImportError {
        code: ImportErrorCode::ActivationUnqualified,
        error,
    })?;
    Ok(())
}

/// State read by the trusted host from its own durable intent, never an
/// application-provided acknowledgment. The host must retain its write freeze
/// until this exact handshake permits creating a fresh Catabolic-only gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCutoverPhase {
    Prepared,
    Validated,
    Activated,
}
#[derive(Clone, Debug)]
pub struct HostCutoverCheckpoint {
    pub catalog_id: String,
    pub incarnation: String,
    pub phase: HostCutoverPhase,
    pub import_id: String,
    pub input_digest: String,
    pub reference_ack_id: String,
    pub mapping_digest: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconciliation {
    KeepAdmissionClosed,
    FinishHostActivation,
    AdmitCatabolicOnly,
}
/// Pure restart decision. It never opens a gate, edits host references or rolls
/// back imported rows. A redacted/expired/missing receipt must remain closed.
pub fn reconcile_activation(
    receipt: Option<&ImportReceipt>,
    host: &HostCutoverCheckpoint,
    current_catalog_id: &str,
    current_incarnation: &str,
) -> Reconciliation {
    let Some(ImportReceipt::Activated {
        contract_version,
        catalog_id,
        incarnation,
        import_id,
        input_digest,
        reference_ack_id,
        mapping_digest,
        ..
    }) = receipt
    else {
        return Reconciliation::KeepAdmissionClosed;
    };
    if !supported(*contract_version)
        || catalog_id != current_catalog_id
        || incarnation != current_incarnation
        || catalog_id != &host.catalog_id
        || incarnation != &host.incarnation
        || import_id != &host.import_id
        || input_digest != &host.input_digest
        || reference_ack_id != &host.reference_ack_id
        || mapping_digest != &host.mapping_digest
    {
        return Reconciliation::KeepAdmissionClosed;
    }
    match host.phase {
        HostCutoverPhase::Prepared => Reconciliation::KeepAdmissionClosed,
        HostCutoverPhase::Validated => Reconciliation::FinishHostActivation,
        HostCutoverPhase::Activated => Reconciliation::AdmitCatabolicOnly,
    }
}

pub(crate) fn complete_mapping_digest(
    mappings: &[LogicalMapping],
    typed_mappings: &[Value],
) -> Result<String> {
    if typed_mappings.is_empty() {
        return mapping_digest(mappings);
    }
    Ok(catabolic_core::sha256(encoded(&serde_json::json!({"contract_version":2,"mappings":mappings,"typed_mappings":typed_mappings}))?.as_bytes()))
}
pub(crate) fn create_import_items(store: &Store, items: &[ImportItem]) -> Result<()> {
    for item in items {
        curation::item_put_typed(store, &serde_json::json!({"id":item.item_id,"kind":item.kind,"metadata":item.legacy_metadata}))
            .map_err(|error| ImportError { code: match error.code { curation::ItemPutErrorCode::InvalidInput => ImportErrorCode::InvalidInput, curation::ItemPutErrorCode::Conflict => ImportErrorCode::DestinationCollision, curation::ItemPutErrorCode::Internal => ImportErrorCode::StoreFailure }, error: error.error })?;
    }
    Ok(())
}

/// Reconcile only the actual durable activation receipt under independently
/// supplied current read authority. The original intent comes from trusted host
/// persistence; it is not proof of commit and can never authorize a new command.
/// Missing, expired, redacted and restore-unknown outcomes remain closed.
/// A positive result also rechecks the existing complete cutover inventory in
/// the held Store lifetime; this never waives a worker or journal requirement.
pub fn reconcile_persisted_activation(
    store: &Store,
    activation_intent: &CommandEnvelope,
    checkpoint: &HostCutoverCheckpoint,
    current: crate::catalog_commands::TrustedCommandAuthority<'_>,
    policy: crate::catalog_commands::ReceiptPolicy,
) -> Result<Reconciliation> {
    use crate::catalog_commands::{self as commands, CommandOutcome, TrustedCommandAuthority};
    if !store.writable || !store.owns_writer_lock() || !store.db.is_autocommit() {
        return fail(
            ImportErrorCode::AuthorityDenied,
            "restart reconciliation requires the held native writer owner",
        );
    }
    if [
        &checkpoint.catalog_id,
        &checkpoint.incarnation,
        &checkpoint.import_id,
        &checkpoint.reference_ack_id,
    ]
    .iter()
    .any(|s| !identifier(s))
        || !digest(&checkpoint.input_digest)
        || !digest(&checkpoint.mapping_digest)
    {
        return Err(invalid("invalid recorded host checkpoint"));
    }
    let expected = serde_json::json!({"operation":"import","mutation":ImportMutation::Activate {
        import_id:checkpoint.import_id.clone(),input_digest:checkpoint.input_digest.clone(),reference_ack_id:checkpoint.reference_ack_id.clone(),mapping_digest:checkpoint.mapping_digest.clone(),
    }});
    if activation_intent.key().operation_id()
        != operation_id(&checkpoint.import_id, ImportStep::Activate)?
        || activation_intent.key().catalog_id() != checkpoint.catalog_id
        || activation_intent.payload() != &expected
    {
        return Ok(Reconciliation::KeepAdmissionClosed);
    }
    let outcome = commands::execute(
        store,
        activation_intent,
        TrustedCommandAuthority {
            host_namespace: current.host_namespace,
            receipt_visibility: current.receipt_visibility,
            allow_new_commands: false,
            identity_authority: None,
        },
        policy,
    )
    .map_err(|cause| ImportError {
        code: ImportErrorCode::AuthorityDenied,
        error: Error(format!(
            "durable activation receipt is unavailable: {cause}"
        )),
    })?;
    let CommandOutcome::Replay { result, .. } = outcome else {
        return Ok(Reconciliation::KeepAdmissionClosed);
    };
    let receipt: ImportReceipt = serde_json::from_value(result).map_err(|_| ImportError {
        code: ImportErrorCode::CorruptReceipt,
        error: Error("durable activation receipt has invalid shape".into()),
    })?;
    let decision = reconcile_activation(
        Some(&receipt),
        checkpoint,
        &store.database_id,
        store.catalog_incarnation.as_deref().unwrap_or(""),
    );
    if decision == Reconciliation::AdmitCatabolicOnly {
        store.transaction_detailed(|_| require_settled_cutover(store, true))?;
    }
    Ok(decision)
}

#[test]
fn deferred_history_never_waives_initial_cutover_or_unwitnessed_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("deferred.db");
    crate::migration::initialize_native_at(&path, 39).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store.transaction(|db|{
        db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,completed_at,lease_until) VALUES('terminal','default','media','fixture','[]',0,1,'failed',1,2,0)",[])?;
        db.execute("INSERT INTO observation_scopes VALUES('terminal','path','deferred','{}',2)",[])?;Ok(())
    }).unwrap();
    assert!(require_settled_cutover(&store, false).is_err());
    assert!(require_settled_cutover(&store, true).is_err());
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM observation_scopes", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
