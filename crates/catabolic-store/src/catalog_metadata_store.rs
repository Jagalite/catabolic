// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Item-only metadata v1 persistent owner. The durable command dispatcher owns
//! BEGIN IMMEDIATE, receipt replay/admission, rollback and COMMIT. This module
//! never begins/commits a transaction, writes receipts, invokes callbacks or does
//! external I/O. Its apply entrypoint is crate-private.
//!
//! Hosts supply current trusted authority separately from deserialized requests.
//! A source grant binds one exact item/namespace/record and kind; namespace text
//! never confers local, provider or scan privilege. Initialization and explicit
//! legacy reconciliation require distinct editorial grants. All commands carry
//! the outer catalog generation to reject phantom sources and stale snapshots.
use crate::Error;
use catabolic_core::{
    catalog_command::CommandEnvelope,
    catalog_metadata::{
        ContributionDocument, ContributionKey, ContributionPatch, FieldPolicy, FieldResolution,
        FieldSchema, MAX_METADATA_DOCUMENT_BYTES, MAX_METADATA_DOCUMENTS,
        MAX_METADATA_EVIDENCE_BYTES, MAX_METADATA_SNAPSHOT_BYTES, MetadataError,
        MetadataResolution, Normalization, SourceKind, ValueType, plan_legacy_reconciliation,
        plan_update, resolve,
    },
    decode_json, encode,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const METADATA_STORE_CONTRACT_VERSION: u32 = 1;
pub const LEGACY_NAMESPACE: &str = "catabolic.legacy_import.v1";
pub const LEGACY_RECORD_ID: &str = "items.metadata";
pub const MAX_RAW_MUTATION_BYTES: usize = 256 * 1024;

/// An embedding-host capability, never request data. Do not deserialize it.
#[derive(Clone, Debug)]
pub struct MetadataAuthority {
    pub host_namespace: String,
    pub subject: String,
    pub access: MetadataAccess,
}
#[derive(Clone, Debug)]
pub enum MetadataAccess {
    Initialize,
    Source {
        namespace: String,
        record_id: String,
        kind: SourceKind,
    },
    ReconcileLegacy,
}

/// Separately authorized current read scope for exactly one item. The host must
/// establish this authority, including current admission stamps, outside request
/// deserialization. This is not an ACL/lease implementation; R4 owns those scopes.
#[derive(Clone, Debug)]
pub struct MetadataReadAuthority {
    pub catalog_id: String,
    pub incarnation: String,
    pub subject: String,
    pub expected_generation: i64,
}
#[derive(Clone, Debug, Serialize)]
pub struct MetadataReadView {
    pub catalog_id: String,
    pub incarnation: String,
    pub generation: i64,
    pub subject: String,
    pub schema_fingerprint: String,
    pub metadata: Value,
    pub resolution: MetadataResolution,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyValueType {
    String,
    Boolean,
    Integer,
    Json,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyNormalization {
    Exact,
    TrimString,
    TrimAsciiLowercase,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyField {
    pub field: String,
    pub value_type: PolicyValueType,
    pub nullable: bool,
    pub normalization: PolicyNormalization,
}
fn field_schema(fields: &[PolicyField]) -> std::result::Result<FieldSchema, MetadataError> {
    FieldSchema::new(
        fields
            .iter()
            .map(|field| {
                (
                    field.field.clone(),
                    FieldPolicy {
                        value_type: match field.value_type {
                            PolicyValueType::String => ValueType::String,
                            PolicyValueType::Boolean => ValueType::Boolean,
                            PolicyValueType::Integer => ValueType::Integer,
                            PolicyValueType::Json => ValueType::Json,
                        },
                        nullable: field.nullable,
                        normalization: match field.normalization {
                            PolicyNormalization::Exact => Normalization::Exact,
                            PolicyNormalization::TrimString => Normalization::TrimString,
                            PolicyNormalization::TrimAsciiLowercase => {
                                Normalization::TrimAsciiLowercase
                            }
                        },
                    },
                )
            })
            .collect(),
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataMutation {
    pub contract_version: u32,
    pub change: MetadataChange,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum MetadataChange {
    /// Explicit, immutable per-item policies. Every existing metadata field must
    /// be covered and valid. No inference or rewrite occurs during migration.
    Initialize {
        subject: String,
        policies: Vec<PolicyField>,
    },
    Patch {
        subject: String,
        namespace: String,
        record_id: String,
        schema_fingerprint: String,
        expected_revision: i64,
        /// Raw JSON strings preserve the exact supplied edits and evidence in
        /// immutable history, before field normalization. Duplicate keys fail.
        edits_json: String,
        evidence_json: String,
    },
    ReconcileLegacy {
        subject: String,
        schema_fingerprint: String,
        expected_revision: i64,
        fields: Vec<String>,
    },
}
impl MetadataChange {
    fn subject(&self) -> &str {
        match self {
            Self::Initialize { subject, .. }
            | Self::Patch { subject, .. }
            | Self::ReconcileLegacy { subject, .. } => subject,
        }
    }
}

/// Selected where failures originate; diagnostics are never parsed by callers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataStoreErrorCode {
    AuthorityDenied,
    TransactionRequired,
    AmbientReadTransaction,
    StaleGeneration,
    CatalogMismatch,
    IncarnationMismatch,
    UnsupportedContract,
    SubjectMissing,
    RetiredSubject,
    AlreadyInitialized,
    NotInitialized,
    ManagedWriteBlocked,
    CorruptState,
    InvalidInput,
    Core(MetadataError),
    StoreFailure,
}
impl MetadataStoreErrorCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::AuthorityDenied => "catalog_metadata_authority_denied",
            Self::TransactionRequired => "catalog_metadata_transaction_required",
            Self::AmbientReadTransaction => "catalog_metadata_ambient_read_transaction",
            Self::StaleGeneration => "catalog_metadata_stale_generation",
            Self::CatalogMismatch => "catalog_metadata_catalog_mismatch",
            Self::IncarnationMismatch => "catalog_metadata_incarnation_mismatch",
            Self::UnsupportedContract => "catalog_metadata_unsupported_contract",
            Self::SubjectMissing => "catalog_metadata_subject_missing",
            Self::RetiredSubject => "catalog_metadata_retired_subject",
            Self::AlreadyInitialized => "catalog_metadata_already_initialized",
            Self::NotInitialized => "catalog_metadata_not_initialized",
            Self::ManagedWriteBlocked => "catalog_metadata_managed_write_blocked",
            Self::CorruptState => "catalog_metadata_corrupt_state",
            Self::InvalidInput => "catalog_metadata_invalid_input",
            Self::Core(error) => error.code(),
            Self::StoreFailure => "catalog_metadata_store_failure",
        }
    }
}
#[derive(Debug)]
pub struct MetadataStoreError {
    pub code: MetadataStoreErrorCode,
    pub error: Error,
}
impl std::fmt::Display for MetadataStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for MetadataStoreError {}
impl From<rusqlite::Error> for MetadataStoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self {
            code: MetadataStoreErrorCode::StoreFailure,
            error: error.into(),
        }
    }
}
impl From<serde_json::Error> for MetadataStoreError {
    fn from(error: serde_json::Error) -> Self {
        Self {
            code: MetadataStoreErrorCode::StoreFailure,
            error: error.into(),
        }
    }
}
impl From<MetadataError> for MetadataStoreError {
    fn from(error: MetadataError) -> Self {
        Self {
            code: MetadataStoreErrorCode::Core(error),
            error: Error(error.to_string()),
        }
    }
}
pub type Result<T> = std::result::Result<T, MetadataStoreError>;
fn fail(code: MetadataStoreErrorCode, message: impl Into<String>) -> MetadataStoreError {
    MetadataStoreError {
        code,
        error: Error(message.into()),
    }
}
fn corrupt(message: impl Into<String>) -> MetadataStoreError {
    fail(MetadataStoreErrorCode::CorruptState, message)
}
fn kind_sql(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Local => "local",
        SourceKind::Provider => "provider",
        SourceKind::Scan => "scan",
        SourceKind::LegacyImport => "legacy_import",
    }
}
fn stored_kind(raw: &str) -> Result<SourceKind> {
    match raw {
        "local" => Ok(SourceKind::Local),
        "provider" => Ok(SourceKind::Provider),
        "scan" => Ok(SourceKind::Scan),
        "legacy_import" => Ok(SourceKind::LegacyImport),
        _ => Err(corrupt("invalid stored metadata source kind")),
    }
}
fn legacy_key(subject: &str) -> std::result::Result<ContributionKey, MetadataError> {
    ContributionKey::new(subject, LEGACY_NAMESPACE, LEGACY_RECORD_ID)
}
fn check_fingerprint(schema: &FieldSchema, fingerprint: &str) -> Result<()> {
    if schema.fingerprint() != fingerprint {
        return Err(MetadataError::SchemaMismatch.into());
    }
    Ok(())
}
fn read_schema(db: &Connection, subject: &str) -> Result<FieldSchema> {
    let row: Option<(String, String)> = db.query_row(
        "SELECT schema_fingerprint,policies_json FROM catalog_metadata_subjects WHERE item_id=?1",
        [subject], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((fingerprint, raw)) = row else {
        return Err(fail(
            MetadataStoreErrorCode::NotInitialized,
            "metadata subject requires explicit initialization",
        ));
    };
    let policies: Vec<PolicyField> =
        serde_json::from_str(&raw).map_err(|_| corrupt("invalid stored metadata policies"))?;
    let schema = field_schema(&policies).map_err(|_| corrupt("invalid stored metadata schema"))?;
    if schema.fingerprint() != fingerprint {
        return Err(corrupt("stored metadata schema fingerprint mismatch"));
    }
    Ok(schema)
}
fn read_documents(
    db: &Connection,
    schema: &FieldSchema,
    subject: &str,
) -> Result<Vec<ContributionDocument>> {
    // Preflight aggregate raw bytes and per-row sizes before loading any source
    // text. The pure resolver then checks exact serialized envelope/snapshot size.
    let (count, bytes, fields_max, evidence_max): (i64, i64, i64, i64) = db.query_row(
        "SELECT count(*),coalesce(sum(length(CAST(fields_json AS BLOB))+length(CAST(evidence_json AS BLOB))),0),coalesce(max(length(CAST(fields_json AS BLOB))),0),coalesce(max(length(CAST(evidence_json AS BLOB))),0) FROM catalog_metadata_sources WHERE item_id=?1",
        [subject], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    )?;
    if count > MAX_METADATA_DOCUMENTS as i64 {
        return Err(MetadataError::SourceBudget.into());
    }
    if bytes > MAX_METADATA_SNAPSHOT_BYTES as i64 {
        return Err(MetadataError::SnapshotBudget.into());
    }
    if fields_max > MAX_METADATA_DOCUMENT_BYTES as i64 {
        return Err(MetadataError::DocumentBudget.into());
    }
    if evidence_max > MAX_METADATA_EVIDENCE_BYTES as i64 {
        return Err(MetadataError::EvidenceBudget.into());
    }
    // LIMIT detects an over-budget set without unbounded allocation. All current
    // heads, including empty documents, are loaded; there is no caller subset.
    let mut query = db.prepare("SELECT namespace,record_id,source_kind,revision,schema_fingerprint,fields_json,evidence_json FROM catalog_metadata_sources WHERE item_id=?1 ORDER BY namespace,record_id LIMIT ?2")?;
    let mut rows = query.query(params![subject, (MAX_METADATA_DOCUMENTS + 1) as i64])?;
    let mut documents = Vec::new();
    while let Some(row) = rows.next()? {
        if documents.len() == MAX_METADATA_DOCUMENTS {
            return Err(MetadataError::SourceBudget.into());
        }
        let fingerprint: String = row.get(4)?;
        if fingerprint != schema.fingerprint() {
            return Err(corrupt("stored source schema fingerprint mismatch"));
        }
        let key = ContributionKey::new(subject, row.get::<_, String>(0)?, row.get::<_, String>(1)?)
            .map_err(|_| corrupt("invalid stored source key"))?;
        let kind = stored_kind(&row.get::<_, String>(2)?)?;
        let document = ContributionDocument::from_json(
            schema,
            key,
            kind,
            row.get(3)?,
            &row.get::<_, String>(5)?,
            &row.get::<_, String>(6)?,
        )
        .map_err(|_| corrupt("invalid stored source document"))?;
        documents.push(document);
    }
    Ok(documents)
}
fn write_revision(
    db: &Connection,
    document: &ContributionDocument,
    raw_mutation: &str,
    command: &CommandEnvelope,
    admitted_at_us: i64,
) -> Result<()> {
    let key = document.key();
    let fields = serde_json::to_string(document.fields())?;
    let evidence = encode(document.evidence());
    db.execute("INSERT INTO catalog_metadata_history(item_id,namespace,record_id,revision,source_kind,schema_fingerprint,fields_json,evidence_json,raw_mutation_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)", params![key.subject(),key.namespace(),key.record_id(),document.revision(),kind_sql(document.kind()),document.schema_fingerprint(),fields,evidence,raw_mutation,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),admitted_at_us])?;
    db.execute("INSERT INTO catalog_metadata_sources(item_id,namespace,record_id,source_kind,revision,schema_fingerprint,fields_json,evidence_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(item_id,namespace,record_id) DO UPDATE SET revision=excluded.revision,fields_json=excluded.fields_json,evidence_json=excluded.evidence_json", params![key.subject(),key.namespace(),key.record_id(),kind_sql(document.kind()),document.revision(),document.schema_fingerprint(),fields,evidence])?;
    Ok(())
}
fn write_projection(db: &Connection, resolution: &MetadataResolution) -> Result<()> {
    let effective = Value::Object(
        resolution
            .fields
            .iter()
            .filter_map(|(field, resolved)| {
                if let FieldResolution::Resolved { value, .. } = resolved {
                    Some((field.clone(), value.clone()))
                } else {
                    None
                }
            })
            .collect(),
    );
    let projection = serde_json::to_string(resolution)?;
    let effective_json = encode(&effective);
    db.execute("INSERT INTO catalog_metadata_projection(item_id,schema_fingerprint,resolution_json,effective_json) VALUES(?1,?2,?3,?4) ON CONFLICT(item_id) DO UPDATE SET resolution_json=excluded.resolution_json,effective_json=excluded.effective_json",params![resolution.subject,resolution.schema_fingerprint,projection,effective_json])?;
    let old: String = db.query_row(
        "SELECT metadata FROM items WHERE id=?1",
        [&resolution.subject],
        |row| row.get(0),
    )?;
    let parsed = decode_json(&old).map_err(|_| corrupt("invalid existing item metadata"))?;
    // Keep exact original bytes whenever the effective value is unchanged.
    if parsed != effective {
        db.execute(
            "UPDATE items SET metadata=?2 WHERE id=?1",
            params![resolution.subject, effective_json],
        )?;
    }
    Ok(())
}

fn check_read_admission(db: &Connection, authority: &MetadataReadAuthority) -> Result<()> {
    let (catalog, incarnation, generation): (String, String, i64) = db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if catalog != authority.catalog_id {
        return Err(fail(
            MetadataStoreErrorCode::CatalogMismatch,
            "metadata read names another catalog",
        ));
    }
    if incarnation != authority.incarnation {
        return Err(fail(
            MetadataStoreErrorCode::IncarnationMismatch,
            "metadata read admission incarnation changed",
        ));
    }
    if generation != authority.expected_generation {
        return Err(fail(
            MetadataStoreErrorCode::StaleGeneration,
            "metadata read generation changed",
        ));
    }
    Ok(())
}

/// Bounded connection-level metadata/provenance reader for a trusted native34
/// host. The public Store wrapper must validate native identity/admission before
/// and after this reader and repeat check_read_admission before returning. It
/// accepts no SQL or caller-selected source subset and does not persist changes.
/// Call on an admitted native34 connection with no ambient transaction; an old
/// SQLite snapshot could conceal a concurrent commit. Initial and final current
/// catalog/incarnation/generation checks fence every source/schema read. A race
/// returns a typed stale error, never a mixed view. A successful view is stamped
/// with the last checked generation; callers must re-admit before later use.
/// The exact authority subject is the only item whose data can be returned.
pub(crate) fn read_metadata_connection(
    db: &Connection,
    authority: &MetadataReadAuthority,
) -> Result<MetadataReadView> {
    if !db.is_autocommit() {
        return Err(fail(
            MetadataStoreErrorCode::AmbientReadTransaction,
            "metadata current read cannot use an ambient snapshot",
        ));
    }
    legacy_key(&authority.subject)?;
    if authority.expected_generation < 0 {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "metadata read generation must be nonnegative",
        ));
    }
    check_read_admission(db, authority)?;
    let schema = read_schema(db, &authority.subject)?;
    let documents = read_documents(db, &schema, &authority.subject)?;
    let resolution = resolve(&schema, &authority.subject, &documents)?;
    let metadata = Value::Object(
        resolution
            .fields
            .iter()
            .filter_map(|(field, resolved)| {
                if let FieldResolution::Resolved { value, .. } = resolved {
                    Some((field.clone(), value.clone()))
                } else {
                    None
                }
            })
            .collect(),
    );
    check_read_admission(db, authority)?;
    Ok(MetadataReadView {
        catalog_id: authority.catalog_id.clone(),
        incarnation: authority.incarnation.clone(),
        generation: authority.expected_generation,
        subject: authority.subject.clone(),
        schema_fingerprint: schema.fingerprint().into(),
        metadata,
        resolution,
    })
}

/// Must be invoked only by the existing durable dispatcher after replay handling
/// and before receipt insertion, inside its top-level writer transaction. Every
/// error must escape that transaction; it is forbidden to catch an owner error
/// and commit partial work. Deferred permits also prevent accidental late commit.
pub(crate) fn apply_metadata(
    db: &Connection,
    mutation: &MetadataMutation,
    command: &CommandEnvelope,
    authority: &MetadataAuthority,
    admitted_at_us: i64,
) -> Result<Value> {
    if db.is_autocommit() {
        return Err(fail(
            MetadataStoreErrorCode::TransactionRequired,
            "metadata apply requires the outer writer transaction",
        ));
    }
    if mutation.contract_version != METADATA_STORE_CONTRACT_VERSION {
        return Err(fail(
            MetadataStoreErrorCode::UnsupportedContract,
            "unsupported metadata store contract",
        ));
    }
    let subject = mutation.change.subject();
    if authority.subject != subject || authority.host_namespace != command.key().host_namespace() {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "metadata authority does not cover this host and subject",
        ));
    }
    // Validate identifiers even for initialization, before SQL or grant matching.
    legacy_key(subject)?;
    let (catalog, incarnation, generation): (String, String, i64) = db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if command.key().catalog_id() != catalog {
        return Err(fail(
            MetadataStoreErrorCode::CatalogMismatch,
            "command names another catalog",
        ));
    }
    if command.admission_incarnation() != incarnation {
        return Err(fail(
            MetadataStoreErrorCode::IncarnationMismatch,
            "command names another incarnation",
        ));
    }
    if command.expected_generation() != generation {
        return Err(fail(
            MetadataStoreErrorCode::StaleGeneration,
            "metadata snapshot generation is stale",
        ));
    }
    if admitted_at_us < 0 {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "invalid admitted timestamp",
        ));
    }
    let expected_payload = json!({"operation":"metadata","mutation":mutation});
    if command.payload() != &expected_payload {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "metadata mutation does not match the command payload",
        ));
    }
    let raw_mutation = serde_json::to_string(mutation)?;
    if raw_mutation.len() > MAX_RAW_MUTATION_BYTES {
        return Err(fail(
            MetadataStoreErrorCode::InvalidInput,
            "metadata mutation exceeds raw history budget",
        ));
    }
    let existing: Option<String> = db
        .query_row("SELECT metadata FROM items WHERE id=?1", [subject], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(existing) = existing else {
        return Err(fail(
            MetadataStoreErrorCode::SubjectMissing,
            "metadata subject is not an existing item",
        ));
    };
    // Logical retirement retains this historical subject and all its source
    // keys. No alias/survivor is followed and no authority is transferred. The
    // presence check supports this isolated fragment/older schemas only; native34
    // admission must validate the known logical-retirement schema signature.
    let retirement_installed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='catalog_logical_retirements')", [], |row| row.get(0),
    )?;
    if retirement_installed
        && db.query_row(
            "SELECT EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=?1)",
            [subject],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Err(fail(
            MetadataStoreErrorCode::RetiredSubject,
            "retired logical items retain historical metadata but cannot accept new contributions",
        ));
    }
    let (schema, next, documents, initialization) = match &mutation.change {
        MetadataChange::Initialize { policies, .. } => {
            if !matches!(authority.access, MetadataAccess::Initialize) {
                return Err(fail(
                    MetadataStoreErrorCode::AuthorityDenied,
                    "initialization requires an explicit initialization grant",
                ));
            }
            if db.query_row(
                "SELECT EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=?1)",
                [subject],
                |row| row.get::<_, bool>(0),
            )? {
                return Err(fail(
                    MetadataStoreErrorCode::AlreadyInitialized,
                    "metadata schema is immutable after initialization",
                ));
            }
            let schema = field_schema(policies)?;
            let legacy = ContributionDocument::from_json(
                &schema,
                legacy_key(subject)?,
                SourceKind::LegacyImport,
                1,
                &existing,
                r#"{"origin":"unknown","import":"items.metadata"}"#,
            )?;
            (
                schema,
                legacy,
                Vec::new(),
                Some(serde_json::to_string(policies)?),
            )
        }
        MetadataChange::Patch {
            namespace,
            record_id,
            schema_fingerprint,
            expected_revision,
            edits_json,
            evidence_json,
            ..
        } => {
            let kind = match &authority.access {
                MetadataAccess::Source {
                    namespace: trusted_namespace,
                    record_id: trusted_record,
                    kind,
                } if trusted_namespace == namespace
                    && trusted_record == record_id
                    && *kind != SourceKind::LegacyImport
                    && namespace != LEGACY_NAMESPACE =>
                {
                    *kind
                }
                _ => {
                    return Err(fail(
                        MetadataStoreErrorCode::AuthorityDenied,
                        "source authority does not cover this exact key and kind",
                    ));
                }
            };
            let schema = read_schema(db, subject)?;
            check_fingerprint(&schema, schema_fingerprint)?;
            let key = ContributionKey::new(subject, namespace, record_id)?;
            let documents = read_documents(db, &schema, subject)?;
            let current = documents.iter().find(|document| document.key() == &key);
            let patch = ContributionPatch::from_json(
                &schema,
                key,
                kind,
                *expected_revision,
                edits_json,
                evidence_json,
            )?;
            let next = plan_update(&schema, current, &patch)?;
            (schema, next, documents, None)
        }
        MetadataChange::ReconcileLegacy {
            schema_fingerprint,
            expected_revision,
            fields,
            ..
        } => {
            if !matches!(authority.access, MetadataAccess::ReconcileLegacy) {
                return Err(fail(
                    MetadataStoreErrorCode::AuthorityDenied,
                    "legacy reconciliation requires an explicit editorial grant",
                ));
            }
            let schema = read_schema(db, subject)?;
            check_fingerprint(&schema, schema_fingerprint)?;
            let documents = read_documents(db, &schema, subject)?;
            let key = legacy_key(subject)?;
            let legacy = documents
                .iter()
                .find(|document| document.key() == &key)
                .ok_or_else(|| corrupt("initialized subject has no legacy import head"))?;
            let next = plan_legacy_reconciliation(&schema, legacy, *expected_revision, fields)?;
            (schema, next, documents, None)
        }
    };
    let mut documents = documents;
    documents.retain(|document| document.key() != next.key());
    documents.push(next.clone());
    // Resolve the full proposed snapshot before taking a permit or mutating.
    let resolution = resolve(&schema, subject, &documents)?;
    db.execute(
        "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES(?1,?2)",
        params![subject, incarnation],
    )?;
    if let Some(policies) = initialization {
        db.execute("INSERT INTO catalog_metadata_subjects(item_id,contract_version,schema_fingerprint,policies_json,legacy_raw_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) VALUES(?1,1,?2,?3,?4,?5,?6,?7,?8)",params![subject,schema.fingerprint(),policies,existing,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),admitted_at_us])?;
    }
    write_revision(db, &next, &raw_mutation, command, admitted_at_us)?;
    write_projection(db, &resolution)?;
    db.execute(
        "DELETE FROM catalog_metadata_permits WHERE item_id=?1",
        [subject],
    )?;
    Ok(
        json!({"operation":"metadata","subject":subject,"schema_fingerprint":schema.fingerprint(),"source":{"namespace":next.key().namespace(),"record_id":next.key().record_id(),"kind":next.kind(),"revision":next.revision()},"resolution":resolution}),
    )
}

/// Native validation and the durable dispatcher must invoke this before receipt
/// commit, and native open/validation must invoke it on persisted databases.
/// The deferred FK is the final COMMIT barrier even if the hook is missed.
pub(crate) fn validate_metadata_permits_empty(db: &Connection) -> Result<()> {
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_metadata_permits)",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(corrupt("metadata mutation permits remain at validation"));
    }
    Ok(())
}

/// Existing item_put integration: resolve the actual item ID first, then call
/// this inside its writer transaction and use the returned bytes in the upsert.
/// Schema 32/33 (no metadata tables), unmanaged subjects and new items preserve
/// existing behavior. Managed unchanged values retain the exact stored bytes;
/// changed values fail closed with a typed conflict before writing.
pub(crate) fn legacy_item_put_metadata(
    db: &Connection,
    subject: &str,
    proposed: &Value,
) -> Result<String> {
    let installed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='catalog_metadata_subjects')",[],|row| row.get(0))?;
    if !installed {
        return Ok(encode(proposed));
    }
    let current: Option<String> = db.query_row("SELECT i.metadata FROM items i JOIN catalog_metadata_subjects s ON s.item_id=i.id WHERE i.id=?1",[subject],|row| row.get(0)).optional()?;
    let Some(current) = current else {
        return Ok(encode(proposed));
    };
    if decode_json(&current).map_err(|_| corrupt("managed item metadata is invalid"))? != *proposed
    {
        return Err(fail(
            MetadataStoreErrorCode::ManagedWriteBlocked,
            "managed metadata changes require a typed metadata command",
        ));
    }
    Ok(current)
}

#[cfg(test)]
#[path = "catalog_metadata_store_tests.rs"]
mod tests;

// Integration snippet: append inside catalog_metadata_store.rs when adopting
// into the real Store crate. The isolated harness has no Store lifecycle.
// This wrapper must be compiled/tested against real native admission centrally.
/// Current metadata/provenance for one explicitly authorized native subject.
pub fn read_metadata(
    store: &crate::Store,
    authority: &MetadataReadAuthority,
) -> Result<MetadataReadView> {
    if store.schema_version < 34 || store.catalog_incarnation.is_none() {
        return Err(fail(
            MetadataStoreErrorCode::UnsupportedContract,
            "metadata read requires admitted native schema34 or newer",
        ));
    }
    store
        .transaction_available()
        .map_err(|error| MetadataStoreError {
            code: MetadataStoreErrorCode::StoreFailure,
            error,
        })?;
    let result = read_metadata_connection(&store.db, authority)?;
    store
        .transaction_available()
        .map_err(|error| MetadataStoreError {
            code: MetadataStoreErrorCode::StoreFailure,
            error,
        })?;
    check_read_admission(&store.db, authority)?;
    Ok(result)
}

/// R4's private owner holds a fresh read transaction and exact host lease. This
/// seam shares the existing metadata resolver; it never treats a caller-selected
/// source subset as authoritative. Only current allowed-field provenance leaves
/// the seam. Document evidence and whole-schema/input fingerprints stay private.
pub(crate) fn read_metadata_provenance_in_snapshot(
    db: &Connection,
    authority: &MetadataReadAuthority,
    fields: &std::collections::BTreeSet<String>,
    budget: &crate::operation_budget::OperationBudget,
) -> Result<Value> {
    if db.is_autocommit() {
        return Err(fail(
            MetadataStoreErrorCode::TransactionRequired,
            "scoped provenance requires its owned snapshot",
        ));
    }
    legacy_key(&authority.subject)?;
    check_read_admission(db, authority)?;
    let policy_bytes: Option<i64> = db.query_row(
        "SELECT length(CAST(policies_json AS BLOB)) FROM main.catalog_metadata_subjects WHERE item_id=?", [&authority.subject], |r|r.get(0),
    ).optional()?;
    let Some(policy_bytes) = policy_bytes else {
        return Err(fail(
            MetadataStoreErrorCode::NotInitialized,
            "metadata provenance requires explicit initialization",
        ));
    };
    if !(0..=MAX_METADATA_DOCUMENT_BYTES as i64).contains(&policy_bytes) {
        return Err(MetadataError::DocumentBudget.into());
    }
    let (count, bytes): (i64,i64) = db.query_row(
        "SELECT count(*),coalesce(sum(length(CAST(fields_json AS BLOB))+length(CAST(evidence_json AS BLOB))+length(CAST(namespace AS BLOB))+length(CAST(record_id AS BLOB))),0) FROM main.catalog_metadata_sources WHERE item_id=?", [&authority.subject], |r|Ok((r.get(0)?,r.get(1)?)),
    )?;
    if !(0..=MAX_METADATA_DOCUMENTS as i64).contains(&count) {
        return Err(MetadataError::SourceBudget.into());
    }
    if !(0..=MAX_METADATA_SNAPSHOT_BYTES as i64).contains(&bytes) {
        return Err(MetadataError::SnapshotBudget.into());
    }
    budget
        .charge(crate::operation_budget::BudgetCharge {
            items: count as usize,
            bytes: bytes as usize + policy_bytes as usize,
            values: (count as usize + 1) * catabolic_core::catalog_metadata::MAX_METADATA_FIELDS,
            work: bytes as usize / 64
                + count as usize * catabolic_core::catalog_metadata::MAX_METADATA_FIELDS
                + 1,
        })
        .map_err(|_| {
            fail(
                MetadataStoreErrorCode::StoreFailure,
                "scoped metadata operation budget exhausted",
            )
        })?;
    let schema = read_schema(db, &authority.subject)?;
    let documents = read_documents(db, &schema, &authority.subject)?;
    let mut resolution = resolve(&schema, &authority.subject, &documents)?;
    resolution.fields.retain(|field, _| fields.contains(field));
    let contributions = documents.iter().filter_map(|document| {
        let visible: serde_json::Map<String,Value> = document.fields().iter()
            .filter(|(field,_)| fields.contains(*field)).map(|(field,value)|(field.clone(),value.clone())).collect();
        if visible.is_empty() { None } else {
            Some(json!({"source":document.key(),"kind":document.kind(),"revision":document.revision(),"fields":visible}))
        }
    }).collect::<Vec<_>>();
    check_read_admission(db, authority)?;
    Ok(
        json!({"contract_version":resolution.contract_version,"item_id":authority.subject,
        "fields":resolution.fields,"scoped_total":contributions.len(),"contributions":contributions}),
    )
}
