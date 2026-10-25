// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded, deterministic metadata contribution decisions. No I/O or authority.
//!
//! The store owner must authenticate source kinds and read current revisions in
//! one writer transaction. Commit the planned document, retained prior document,
//! rebuilt projection and generation together. These pure decisions are neither
//! durable writes nor authorization to write. Deletion must retain an empty
//! revisioned document: dropping its revision would permit stale recreation.
//!
//! A field schema is explicit, immutable and fingerprinted. Unknown fields fail
//! closed. This module supplies shape/normalization policies, not a media-domain
//! schema. Changing a schema requires an explicit migration, never an implicit
//! reinterpretation of retained documents. Json fields admit arbitrary bounded
//! JSON structure; nullable controls the field's top-level null, not nested JSON.
//!
//! Patch omission preserves a field, {"op":"remove"} removes its contribution,
//! and {"op":"set","value":null} is a value only for nullable fields. Removal
//! is not a suppression/tombstone over other sources. Tag suppression and accepted
//! identity decisions have separate owners and are not metadata field policies.
//!
//! Local values win. Without one, unreconciled legacy_import values retain their
//! exact effective value and attribution ahead of provider/scan evidence. Local
//! removal reveals them again. Only explicit legacy reconciliation releases that
//! hold; provider refresh and ordinary patches cannot relabel or remove it.
//! Equal normalized providers attribute all agreeing sources; disagreement stays
//! unresolved. Scan evidence is considered only without any stronger value.

use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const METADATA_CONTRACT_VERSION: u32 = 1;
pub const MAX_METADATA_DOCUMENTS: usize = 128;
pub const MAX_METADATA_FIELDS: usize = 64;
pub const MAX_METADATA_IDENTIFIER_BYTES: usize = 256;
pub const MAX_METADATA_FIELD_BYTES: usize = 64;
pub const MAX_METADATA_DOCUMENT_BYTES: usize = 64 * 1024;
pub const MAX_METADATA_EVIDENCE_BYTES: usize = 8 * 1024;
pub const MAX_METADATA_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_METADATA_JSON_DEPTH: usize = 32;
pub const MAX_METADATA_JSON_NODES: usize = 8192;

pub mod nfo_refresh;
mod schema_evolution;
pub use schema_evolution::{
    ContributionReplacement, MAX_METADATA_V2_EXTERNAL_FIELDS, MAX_METADATA_V2_FIELD_BYTES,
    METADATA_CONTRACT_VERSION_V2, plan_replace, plan_schema_extension, replacement_fields,
    revalidate_document, validate_schema_extension,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataError {
    InvalidIdentifier,
    InvalidField,
    UnknownField,
    ReservedField,
    DuplicateField,
    InvalidPolicy,
    EmptySchema,
    SchemaMismatch,
    InvalidRevision,
    RevisionExhausted,
    StaleSourceRevision { expected: i64, actual: i64 },
    SourceMismatch,
    SourceKindMismatch,
    SubjectMismatch,
    DuplicateSource,
    MultipleLocalSources,
    InvalidJson,
    NonFiniteJson,
    ExpectedObject,
    InvalidFieldValue,
    NullNotAllowed,
    InvalidOperation,
    LegacyReconciliationRequired,
    NotLegacyImport,
    EmptyReconciliation,
    FieldAbsent,
    DocumentBudget,
    EvidenceBudget,
    SnapshotBudget,
    FieldBudget,
    SourceBudget,
    JsonDepthBudget,
    JsonNodeBudget,
}

impl MetadataError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidIdentifier => "catalog_metadata_invalid_identifier",
            Self::InvalidField => "catalog_metadata_invalid_field",
            Self::UnknownField => "catalog_metadata_unknown_field",
            Self::ReservedField => "catalog_metadata_reserved_field",
            Self::DuplicateField => "catalog_metadata_duplicate_field",
            Self::InvalidPolicy => "catalog_metadata_invalid_policy",
            Self::EmptySchema => "catalog_metadata_empty_schema",
            Self::SchemaMismatch => "catalog_metadata_schema_mismatch",
            Self::InvalidRevision => "catalog_metadata_invalid_revision",
            Self::RevisionExhausted => "catalog_metadata_revision_exhausted",
            Self::StaleSourceRevision { .. } => "catalog_metadata_stale_source_revision",
            Self::SourceMismatch => "catalog_metadata_source_mismatch",
            Self::SourceKindMismatch => "catalog_metadata_source_kind_mismatch",
            Self::SubjectMismatch => "catalog_metadata_subject_mismatch",
            Self::DuplicateSource => "catalog_metadata_duplicate_source",
            Self::MultipleLocalSources => "catalog_metadata_multiple_local_sources",
            Self::InvalidJson => "catalog_metadata_invalid_json",
            Self::NonFiniteJson => "catalog_metadata_nonfinite_json",
            Self::ExpectedObject => "catalog_metadata_expected_object",
            Self::InvalidFieldValue => "catalog_metadata_invalid_field_value",
            Self::NullNotAllowed => "catalog_metadata_null_not_allowed",
            Self::InvalidOperation => "catalog_metadata_invalid_operation",
            Self::LegacyReconciliationRequired => "catalog_metadata_legacy_reconciliation_required",
            Self::NotLegacyImport => "catalog_metadata_not_legacy_import",
            Self::EmptyReconciliation => "catalog_metadata_empty_reconciliation",
            Self::FieldAbsent => "catalog_metadata_field_absent",
            Self::DocumentBudget => "catalog_metadata_document_budget",
            Self::EvidenceBudget => "catalog_metadata_evidence_budget",
            Self::SnapshotBudget => "catalog_metadata_snapshot_budget",
            Self::FieldBudget => "catalog_metadata_field_budget",
            Self::SourceBudget => "catalog_metadata_source_budget",
            Self::JsonDepthBudget => "catalog_metadata_json_depth_budget",
            Self::JsonNodeBudget => "catalog_metadata_json_node_budget",
        }
    }
}
impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for MetadataError {}
pub type Result<T> = std::result::Result<T, MetadataError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    String,
    Boolean,
    /// Lossless JSON integer, including integers outside i64/u64.
    Integer,
    /// Any finite bounded non-null JSON; nested structure is deliberately opaque.
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalization {
    /// Shared Catabolic canonical JSON, including its number-token semantics.
    Exact,
    TrimString,
    /// ASCII case normalization only; no claim of Unicode case folding.
    TrimAsciiLowercase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FieldPolicy {
    pub value_type: ValueType,
    pub nullable: bool,
    pub normalization: Normalization,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSchema {
    fields: BTreeMap<String, FieldPolicy>,
    fingerprint: String,
    contract_version: u32,
    system_fields: BTreeSet<String>,
}
impl FieldSchema {
    pub fn new(fields: Vec<(String, FieldPolicy)>) -> Result<Self> {
        if fields.is_empty() {
            return Err(MetadataError::EmptySchema);
        }
        if fields.len() > MAX_METADATA_FIELDS {
            return Err(MetadataError::FieldBudget);
        }
        let mut policies = BTreeMap::new();
        for (field, policy) in fields {
            field_name(&field)?;
            if policy.value_type != ValueType::String
                && policy.normalization != Normalization::Exact
            {
                return Err(MetadataError::InvalidPolicy);
            }
            if policies.insert(field, policy).is_some() {
                return Err(MetadataError::DuplicateField);
            }
        }
        let fingerprint = crate::sha256(crate::encode_with(
            &json!({"contract_version": METADATA_CONTRACT_VERSION, "fields": policies}),
            false,
            true,
        ));
        Ok(Self {
            fields: policies,
            fingerprint,
            contract_version: METADATA_CONTRACT_VERSION,
            system_fields: BTreeSet::new(),
        })
    }
    pub fn fields(&self) -> &BTreeMap<String, FieldPolicy> {
        &self.fields
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    fn normalize(&self, field: &str, value: Value, legacy: bool) -> Result<Value> {
        self.validate_field_name(field)?;
        let policy = self.fields.get(field).ok_or(MetadataError::UnknownField)?;
        if value.is_null() {
            return if policy.nullable {
                Ok(value)
            } else {
                Err(MetadataError::NullNotAllowed)
            };
        }
        let valid = match policy.value_type {
            ValueType::String => value.is_string(),
            ValueType::Boolean => value.is_boolean(),
            ValueType::Integer => value
                .as_number()
                .is_some_and(|n| !n.to_string().contains(['.', 'e', 'E'])),
            ValueType::Json => true,
        };
        if !valid {
            return Err(MetadataError::InvalidFieldValue);
        }
        if legacy {
            return Ok(value);
        }
        Ok(match policy.normalization {
            Normalization::Exact => value,
            Normalization::TrimString => {
                Value::String(value.as_str().expect("validated string").trim().to_owned())
            }
            Normalization::TrimAsciiLowercase => Value::String(
                value
                    .as_str()
                    .expect("validated string")
                    .trim()
                    .to_ascii_lowercase(),
            ),
        })
    }
}

/// Exact opaque source key. Source kind is trusted owner data, never inferred
/// from a namespace spelling or supplied unchecked by a public client.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ContributionKey {
    subject: String,
    namespace: String,
    record_id: String,
}
impl ContributionKey {
    pub fn new(
        subject: impl Into<String>,
        namespace: impl Into<String>,
        record_id: impl Into<String>,
    ) -> Result<Self> {
        let key = Self {
            subject: subject.into(),
            namespace: namespace.into(),
            record_id: record_id.into(),
        };
        for value in [&key.subject, &key.namespace, &key.record_id] {
            identifier(value)?;
        }
        Ok(key)
    }
    pub fn subject(&self) -> &str {
        &self.subject
    }
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    pub fn record_id(&self) -> &str {
        &self.record_id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Local,
    Provider,
    Scan,
    LegacyImport,
}

/// A validated immutable revision. Constructors/re-hydration are trusted-owner
/// APIs; they do not prove a revision exists. Refreshes must use plan_update.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ContributionDocument {
    schema_fingerprint: String,
    key: ContributionKey,
    kind: SourceKind,
    revision: i64,
    fields: BTreeMap<String, Value>,
    evidence: Value,
    #[serde(skip)]
    encoded_bytes: usize,
}
impl ContributionDocument {
    pub fn from_json(
        schema: &FieldSchema,
        key: ContributionKey,
        kind: SourceKind,
        revision: i64,
        fields_json: &str,
        evidence_json: &str,
    ) -> Result<Self> {
        Self::from_json_inner(
            schema,
            key,
            kind,
            revision,
            fields_json,
            evidence_json,
            false,
        )
    }
    /// Trusted-owner rehydration for explicit system fields. This is not a public
    /// request parser or an authorization to modify reserved contributions.
    pub fn from_json_with_system_fields(
        schema: &FieldSchema,
        key: ContributionKey,
        kind: SourceKind,
        revision: i64,
        fields_json: &str,
        evidence_json: &str,
    ) -> Result<Self> {
        Self::from_json_inner(
            schema,
            key,
            kind,
            revision,
            fields_json,
            evidence_json,
            true,
        )
    }
    fn from_json_inner(
        schema: &FieldSchema,
        key: ContributionKey,
        kind: SourceKind,
        revision: i64,
        fields_json: &str,
        evidence_json: &str,
        allow_system: bool,
    ) -> Result<Self> {
        revision_number(revision, false)?;
        let fields = bounded_json(
            fields_json,
            MAX_METADATA_DOCUMENT_BYTES,
            MetadataError::DocumentBudget,
        )?;
        let fields = fields.as_object().ok_or(MetadataError::ExpectedObject)?;
        schema.validate_field_set(fields.keys().map(String::as_str), allow_system)?;
        let fields = fields
            .iter()
            .map(|(field, value)| {
                Ok((
                    field.clone(),
                    schema.normalize(field, value.clone(), kind == SourceKind::LegacyImport)?,
                ))
            })
            .collect::<Result<_>>()?;
        let evidence = evidence(evidence_json)?;
        Self::validated(schema, key, kind, revision, fields, evidence)
    }
    fn validated(
        schema: &FieldSchema,
        key: ContributionKey,
        kind: SourceKind,
        revision: i64,
        fields: BTreeMap<String, Value>,
        evidence: Value,
    ) -> Result<Self> {
        schema.validate_field_set(fields.keys().map(String::as_str), true)?;
        let mut document = Self {
            schema_fingerprint: schema.fingerprint.clone(),
            key,
            kind,
            revision,
            fields,
            evidence,
            encoded_bytes: 0,
        };
        let value = serde_json::to_value(&document).expect("validated finite document");
        document.encoded_bytes = crate::encode_with(&value, false, true).len();
        if document.encoded_bytes > MAX_METADATA_DOCUMENT_BYTES {
            return Err(MetadataError::DocumentBudget);
        }
        if schema.contract_version == METADATA_CONTRACT_VERSION_V2 {
            schema_evolution::json_nodes(&value)?;
        }
        Ok(document)
    }
    pub fn key(&self) -> &ContributionKey {
        &self.key
    }
    pub const fn kind(&self) -> SourceKind {
        self.kind
    }
    pub const fn revision(&self) -> i64 {
        self.revision
    }
    pub fn fields(&self) -> &BTreeMap<String, Value> {
        &self.fields
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
    pub fn schema_fingerprint(&self) -> &str {
        &self.schema_fingerprint
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldEdit {
    Set(Value),
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributionPatch {
    schema_fingerprint: String,
    key: ContributionKey,
    kind: SourceKind,
    expected_revision: i64,
    fields: BTreeMap<String, FieldEdit>,
    evidence: Value,
}
impl ContributionPatch {
    /// expected_revision=0 means the complete source key must not exist.
    /// Evidence is replaced as one complete revisioned object, not merged.
    pub fn from_json(
        schema: &FieldSchema,
        key: ContributionKey,
        kind: SourceKind,
        expected_revision: i64,
        edits_json: &str,
        evidence_json: &str,
    ) -> Result<Self> {
        Self::from_json_inner(
            schema,
            key,
            kind,
            expected_revision,
            edits_json,
            evidence_json,
            false,
        )
    }
    /// Trusted-owner parser for operations on explicitly reserved system fields.
    pub fn from_json_with_system_fields(
        schema: &FieldSchema,
        key: ContributionKey,
        kind: SourceKind,
        expected_revision: i64,
        edits_json: &str,
        evidence_json: &str,
    ) -> Result<Self> {
        Self::from_json_inner(
            schema,
            key,
            kind,
            expected_revision,
            edits_json,
            evidence_json,
            true,
        )
    }
    fn from_json_inner(
        schema: &FieldSchema,
        key: ContributionKey,
        kind: SourceKind,
        expected_revision: i64,
        edits_json: &str,
        evidence_json: &str,
        allow_system: bool,
    ) -> Result<Self> {
        revision_number(expected_revision, true)?;
        if kind == SourceKind::LegacyImport {
            return Err(MetadataError::LegacyReconciliationRequired);
        }
        let edits = bounded_json(
            edits_json,
            MAX_METADATA_DOCUMENT_BYTES,
            MetadataError::DocumentBudget,
        )?;
        let edits = edits.as_object().ok_or(MetadataError::ExpectedObject)?;
        schema.validate_field_set(edits.keys().map(String::as_str), allow_system)?;
        let mut fields = BTreeMap::new();
        for (field, operation) in edits {
            schema.validate_field_name(field)?;
            if !schema.fields.contains_key(field) {
                return Err(MetadataError::UnknownField);
            }
            let operation = operation
                .as_object()
                .ok_or(MetadataError::InvalidOperation)?;
            let edit = match operation.get("op").and_then(Value::as_str) {
                Some("set") if operation.len() == 2 && operation.contains_key("value") => {
                    FieldEdit::Set(schema.normalize(field, operation["value"].clone(), false)?)
                }
                Some("remove") if operation.len() == 1 => FieldEdit::Remove,
                _ => return Err(MetadataError::InvalidOperation),
            };
            fields.insert(field.clone(), edit);
        }
        Ok(Self {
            schema_fingerprint: schema.fingerprint.clone(),
            key,
            kind,
            expected_revision,
            fields,
            evidence: evidence(evidence_json)?,
        })
    }
    pub fn key(&self) -> &ContributionKey {
        &self.key
    }
    pub const fn kind(&self) -> SourceKind {
        self.kind
    }
    pub fn schema_fingerprint(&self) -> &str {
        &self.schema_fingerprint
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
    pub fn edits(&self) -> &BTreeMap<String, FieldEdit> {
        &self.fields
    }
    pub const fn expected_revision(&self) -> i64 {
        self.expected_revision
    }
}

/// Check the latest source revision before constructing its next revision. The
/// caller must retain this check and commit in the same writer transaction.
pub fn plan_update(
    schema: &FieldSchema,
    current: Option<&ContributionDocument>,
    patch: &ContributionPatch,
) -> Result<ContributionDocument> {
    check_schema(schema, &patch.schema_fingerprint)?;
    if let Some(current) = current {
        check_schema(schema, &current.schema_fingerprint)?;
        if current.key != patch.key {
            return Err(MetadataError::SourceMismatch);
        }
        if current.kind != patch.kind {
            return Err(MetadataError::SourceKindMismatch);
        }
    }
    let actual = current.map_or(0, |document| document.revision);
    let revision = next_revision(patch.expected_revision, actual)?;
    let mut fields = current.map_or_else(BTreeMap::new, |document| document.fields.clone());
    for (field, edit) in &patch.fields {
        match edit {
            FieldEdit::Set(value) => {
                fields.insert(field.clone(), value.clone());
            }
            FieldEdit::Remove => {
                fields.remove(field);
            }
        }
    }
    ContributionDocument::validated(
        schema,
        patch.key.clone(),
        patch.kind,
        revision,
        fields,
        patch.evidence.clone(),
    )
}

/// Explicitly release named retained legacy values. The resulting source stays
/// legacy_import, including when empty; no provenance is relabeled. This does not
/// endorse a provider's value or create a local identity/metadata decision.
pub fn plan_legacy_reconciliation(
    schema: &FieldSchema,
    current: &ContributionDocument,
    expected_revision: i64,
    fields: &[String],
) -> Result<ContributionDocument> {
    check_schema(schema, &current.schema_fingerprint)?;
    if current.kind != SourceKind::LegacyImport {
        return Err(MetadataError::NotLegacyImport);
    }
    let revision = next_revision(expected_revision, current.revision)?;
    if fields.is_empty() {
        return Err(MetadataError::EmptyReconciliation);
    }
    schema.validate_field_set(fields.iter().map(String::as_str), true)?;
    let mut seen = BTreeSet::new();
    let mut retained = current.fields.clone();
    for field in fields {
        schema.validate_field_name(field)?;
        if !schema.fields.contains_key(field) {
            return Err(MetadataError::UnknownField);
        }
        if !seen.insert(field) {
            return Err(MetadataError::DuplicateField);
        }
        if retained.remove(field).is_none() {
            return Err(MetadataError::FieldAbsent);
        }
    }
    ContributionDocument::validated(
        schema,
        current.key.clone(),
        current.kind,
        revision,
        retained,
        current.evidence.clone(),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionOrigin {
    Local,
    RetainedLegacy,
    Provider,
    Scan,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceRevision {
    pub key: ContributionKey,
    pub kind: SourceKind,
    pub revision: i64,
}
impl From<&ContributionDocument> for SourceRevision {
    fn from(document: &ContributionDocument) -> Self {
        Self {
            key: document.key.clone(),
            kind: document.kind,
            revision: document.revision,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConflictCandidate {
    pub value: Value,
    pub sources: Vec<SourceRevision>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FieldResolution {
    Absent,
    Resolved {
        value: Value,
        origin: ResolutionOrigin,
        sources: Vec<SourceRevision>,
    },
    Conflict {
        origin: ResolutionOrigin,
        candidates: Vec<ConflictCandidate>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MetadataResolution {
    pub contract_version: u32,
    pub schema_fingerprint: String,
    pub subject: String,
    pub fields: BTreeMap<String, FieldResolution>,
    /// All documents consulted, including empty and shadowed ones. Sorted by
    /// exact key. This is a reproducible input list, not a concurrency guard for
    /// newly added documents; the store must also guard snapshot membership.
    pub inputs: Vec<SourceRevision>,
}

pub fn resolve(
    schema: &FieldSchema,
    subject: &str,
    documents: &[ContributionDocument],
) -> Result<MetadataResolution> {
    if schema.contract_version == METADATA_CONTRACT_VERSION_V2 {
        return schema_evolution::resolve_v2(schema, subject, documents);
    }
    identifier(subject)?;
    if documents.len() > MAX_METADATA_DOCUMENTS {
        return Err(MetadataError::SourceBudget);
    }
    let mut sorted = BTreeMap::new();
    let mut bytes = 0usize;
    let mut locals = 0usize;
    for document in documents {
        check_schema(schema, &document.schema_fingerprint)?;
        if document.key.subject != subject {
            return Err(MetadataError::SubjectMismatch);
        }
        if sorted.insert(&document.key, document).is_some() {
            return Err(MetadataError::DuplicateSource);
        }
        bytes = bytes
            .checked_add(document.encoded_bytes)
            .ok_or(MetadataError::SnapshotBudget)?;
        if bytes > MAX_METADATA_SNAPSHOT_BYTES {
            return Err(MetadataError::SnapshotBudget);
        }
        if document.kind == SourceKind::Local {
            locals += 1;
        }
    }
    // One catalog-local document per subject. Multiple local authors must share
    // its revision discipline, rather than race through distinct source keys.
    if locals > 1 {
        return Err(MetadataError::MultipleLocalSources);
    }
    let mut fields = BTreeMap::new();
    for field in schema.fields.keys() {
        let mut resolution = FieldResolution::Absent;
        for (kind, origin) in [
            (SourceKind::Local, ResolutionOrigin::Local),
            (SourceKind::LegacyImport, ResolutionOrigin::RetainedLegacy),
            (SourceKind::Provider, ResolutionOrigin::Provider),
            (SourceKind::Scan, ResolutionOrigin::Scan),
        ] {
            let mut groups: BTreeMap<String, ConflictCandidate> = BTreeMap::new();
            for document in sorted.values().filter(|document| document.kind == kind) {
                if let Some(value) = document.fields.get(field) {
                    let identity = crate::encode_with(value, false, true);
                    groups
                        .entry(identity)
                        .or_insert_with(|| ConflictCandidate {
                            value: value.clone(),
                            sources: Vec::new(),
                        })
                        .sources
                        .push(SourceRevision::from(*document));
                }
            }
            if groups.is_empty() {
                continue;
            }
            resolution = if groups.len() == 1 {
                let candidate = groups.into_values().next().expect("one candidate");
                FieldResolution::Resolved {
                    value: candidate.value,
                    origin,
                    sources: candidate.sources,
                }
            } else {
                FieldResolution::Conflict {
                    origin,
                    candidates: groups.into_values().collect(),
                }
            };
            break;
        }
        fields.insert(field.clone(), resolution);
    }
    Ok(MetadataResolution {
        contract_version: METADATA_CONTRACT_VERSION,
        schema_fingerprint: schema.fingerprint.clone(),
        subject: subject.to_owned(),
        fields,
        inputs: sorted
            .values()
            .map(|document| SourceRevision::from(*document))
            .collect(),
    })
}

fn identifier(value: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > MAX_METADATA_IDENTIFIER_BYTES
        || value.chars().any(char::is_control)
    {
        Err(MetadataError::InvalidIdentifier)
    } else {
        Ok(())
    }
}
fn field_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_METADATA_FIELD_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(MetadataError::InvalidField)
    } else {
        Ok(())
    }
}
fn revision_number(value: i64, allow_zero: bool) -> Result<()> {
    if value < if allow_zero { 0 } else { 1 } {
        Err(MetadataError::InvalidRevision)
    } else {
        Ok(())
    }
}
fn next_revision(expected: i64, actual: i64) -> Result<i64> {
    revision_number(expected, true)?;
    if expected != actual {
        return Err(MetadataError::StaleSourceRevision { expected, actual });
    }
    actual
        .checked_add(1)
        .ok_or(MetadataError::RevisionExhausted)
}
fn check_schema(schema: &FieldSchema, fingerprint: &str) -> Result<()> {
    if schema.fingerprint != fingerprint {
        Err(MetadataError::SchemaMismatch)
    } else {
        Ok(())
    }
}
fn evidence(raw: &str) -> Result<Value> {
    let value = bounded_json(
        raw,
        MAX_METADATA_EVIDENCE_BYTES,
        MetadataError::EvidenceBudget,
    )?;
    if !value.is_object() {
        return Err(MetadataError::ExpectedObject);
    }
    Ok(value)
}
fn bounded_json(raw: &str, max_bytes: usize, budget: MetadataError) -> Result<Value> {
    if raw.len() > max_bytes {
        return Err(budget);
    }
    // Enforce depth before entering the existing recursive parser. Quoted braces
    // do not count; parser remains responsible for all grammar and duplicates.
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for byte in raw.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > MAX_METADATA_JSON_DEPTH {
                        return Err(MetadataError::JsonDepthBudget);
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    let value = crate::json::parse(raw, true).map_err(|_| MetadataError::InvalidJson)?;
    let mut pending = vec![&value];
    let mut remaining = MAX_METADATA_JSON_NODES;
    while let Some(node) = pending.pop() {
        remaining = remaining
            .checked_sub(1)
            .ok_or(MetadataError::JsonNodeBudget)?;
        match node {
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            _ => {}
        }
    }
    crate::validate_finite(&value).map_err(|_| MetadataError::NonFiniteJson)?;
    let canonical = crate::encode_with(&value, false, true);
    if canonical.len() > max_bytes {
        return Err(budget);
    }
    crate::json::parse(&canonical, true).map_err(|_| MetadataError::InvalidJson)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(value_type: ValueType, nullable: bool, normalization: Normalization) -> FieldPolicy {
        FieldPolicy {
            value_type,
            nullable,
            normalization,
        }
    }
    fn schema() -> FieldSchema {
        FieldSchema::new(vec![
            (
                "title".into(),
                policy(ValueType::String, false, Normalization::TrimString),
            ),
            (
                "language".into(),
                policy(ValueType::String, true, Normalization::TrimAsciiLowercase),
            ),
            (
                "year".into(),
                policy(ValueType::Integer, false, Normalization::Exact),
            ),
            (
                "flag".into(),
                policy(ValueType::Boolean, false, Normalization::Exact),
            ),
            (
                "details".into(),
                policy(ValueType::Json, true, Normalization::Exact),
            ),
        ])
        .unwrap()
    }
    fn key(namespace: &str) -> ContributionKey {
        ContributionKey::new("item:1", namespace, "record:1").unwrap()
    }
    fn doc(namespace: &str, kind: SourceKind, revision: i64, fields: &str) -> ContributionDocument {
        ContributionDocument::from_json(
            &schema(),
            key(namespace),
            kind,
            revision,
            fields,
            r#"{"observation":"fixture"}"#,
        )
        .unwrap()
    }
    fn patch(namespace: &str, kind: SourceKind, revision: i64, edits: &str) -> ContributionPatch {
        ContributionPatch::from_json(
            &schema(),
            key(namespace),
            kind,
            revision,
            edits,
            r#"{"observation":"refresh"}"#,
        )
        .unwrap()
    }
    fn resolved(
        value: Value,
        origin: ResolutionOrigin,
        documents: &[&ContributionDocument],
    ) -> FieldResolution {
        FieldResolution::Resolved {
            value,
            origin,
            sources: documents
                .iter()
                .map(|document| SourceRevision::from(*document))
                .collect(),
        }
    }

    #[test]
    fn normalized_providers_agree_and_keep_every_attribution_and_revision() {
        let a = doc(
            "provider:a",
            SourceKind::Provider,
            3,
            r#"{"title":" A title ","language":" EN "}"#,
        );
        let b = doc(
            "provider:b",
            SourceKind::Provider,
            8,
            r#"{"title":"A title","language":"en"}"#,
        );
        let scan = doc("scan", SourceKind::Scan, 4, r#"{"title":"filename"}"#);
        let result = resolve(&schema(), "item:1", &[scan.clone(), b.clone(), a.clone()]).unwrap();
        assert_eq!(
            result.fields["title"],
            resolved(json!("A title"), ResolutionOrigin::Provider, &[&a, &b])
        );
        assert_eq!(
            result.fields["language"],
            resolved(json!("en"), ResolutionOrigin::Provider, &[&a, &b])
        );
        assert_eq!(
            result.inputs,
            vec![
                SourceRevision::from(&a),
                SourceRevision::from(&b),
                SourceRevision::from(&scan)
            ]
        );
        assert_eq!(result.fields["year"], FieldResolution::Absent);
    }

    #[test]
    fn conflicts_are_unresolved_and_do_not_fall_back_to_scan() {
        let a = doc("a", SourceKind::Provider, 1, r#"{"title":"Alpha"}"#);
        let b = doc("b", SourceKind::Provider, 2, r#"{"title":"Beta"}"#);
        let c = doc("c", SourceKind::Provider, 3, r#"{"title":" Alpha "}"#);
        let scan = doc("scan", SourceKind::Scan, 1, r#"{"title":"Alpha"}"#);
        let result = resolve(
            &schema(),
            "item:1",
            &[c.clone(), scan, b.clone(), a.clone()],
        )
        .unwrap();
        assert_eq!(
            result.fields["title"],
            FieldResolution::Conflict {
                origin: ResolutionOrigin::Provider,
                candidates: vec![
                    ConflictCandidate {
                        value: json!("Alpha"),
                        sources: vec![SourceRevision::from(&a), SourceRevision::from(&c)]
                    },
                    ConflictCandidate {
                        value: json!("Beta"),
                        sources: vec![SourceRevision::from(&b)]
                    },
                ],
            }
        );
        let json = serde_json::to_value(&result.fields["title"]).unwrap();
        assert_eq!(json["state"], "conflict");
        assert!(json.get("value").is_none());
    }

    #[test]
    fn explicit_local_value_wins_over_conflicting_providers() {
        let a = doc("a", SourceKind::Provider, 1, r#"{"title":"A"}"#);
        let b = doc("b", SourceKind::Provider, 1, r#"{"title":"B"}"#);
        let local = doc("local", SourceKind::Local, 5, r#"{"title":"Chosen"}"#);
        let result = resolve(&schema(), "item:1", &[a, b, local.clone()]).unwrap();
        assert_eq!(
            result.fields["title"],
            resolved(json!("Chosen"), ResolutionOrigin::Local, &[&local])
        );
    }

    #[test]
    fn null_omission_and_removal_have_distinct_request_and_resolution_semantics() {
        let provider = doc("provider", SourceKind::Provider, 1, r#"{"language":"EN"}"#);
        let local = doc(
            "local",
            SourceKind::Local,
            3,
            r#"{"language":null,"title":"Keep me"}"#,
        );
        let omitted = plan_update(
            &schema(),
            Some(&local),
            &patch("local", SourceKind::Local, 3, "{}"),
        )
        .unwrap();
        assert_eq!(omitted.fields(), local.fields());
        assert_eq!(omitted.revision(), 4);
        assert_eq!(omitted.evidence()["observation"], "refresh");
        let result = resolve(&schema(), "item:1", &[provider.clone(), omitted.clone()]).unwrap();
        assert_eq!(
            result.fields["language"],
            resolved(Value::Null, ResolutionOrigin::Local, &[&omitted])
        );
        let wire = serde_json::to_value(&result.fields["language"]).unwrap();
        assert_eq!(wire["state"], "resolved");
        assert!(wire.get("value").is_some_and(Value::is_null));
        let removed = plan_update(
            &schema(),
            Some(&omitted),
            &patch(
                "local",
                SourceKind::Local,
                4,
                r#"{"language":{"op":"remove"}}"#,
            ),
        )
        .unwrap();
        assert!(!removed.fields().contains_key("language"));
        assert_eq!(removed.fields()["title"], "Keep me");
        assert_eq!(
            resolve(&schema(), "item:1", &[provider.clone(), removed.clone()])
                .unwrap()
                .fields["language"],
            resolved(json!("en"), ResolutionOrigin::Provider, &[&provider])
        );
        assert_eq!(
            resolve(&schema(), "item:1", &[removed]).unwrap().fields["language"],
            FieldResolution::Absent
        );
        assert_eq!(local.fields()["language"], Value::Null); // original retained history unchanged
    }

    #[test]
    fn removal_reveals_provider_conflict_then_scan_when_all_stronger_values_absent() {
        let a = doc("a", SourceKind::Provider, 1, r#"{"title":"A"}"#);
        let b = doc("b", SourceKind::Provider, 1, r#"{"title":"B"}"#);
        let scan = doc("scan", SourceKind::Scan, 2, r#"{"title":"Scan"}"#);
        let local = doc("local", SourceKind::Local, 1, r#"{"title":"Local"}"#);
        let edit = r#"{"title":{"op":"remove"}}"#;
        let local = plan_update(
            &schema(),
            Some(&local),
            &patch("local", SourceKind::Local, 1, edit),
        )
        .unwrap();
        assert!(matches!(
            resolve(
                &schema(),
                "item:1",
                &[a.clone(), b.clone(), scan.clone(), local.clone()]
            )
            .unwrap()
            .fields["title"],
            FieldResolution::Conflict {
                origin: ResolutionOrigin::Provider,
                ..
            }
        ));
        let a = plan_update(
            &schema(),
            Some(&a),
            &patch("a", SourceKind::Provider, 1, edit),
        )
        .unwrap();
        let b = plan_update(
            &schema(),
            Some(&b),
            &patch("b", SourceKind::Provider, 1, edit),
        )
        .unwrap();
        let result = resolve(&schema(), "item:1", &[a, b, scan.clone(), local]).unwrap();
        assert_eq!(
            result.fields["title"],
            resolved(json!("Scan"), ResolutionOrigin::Scan, &[&scan])
        );
        assert_eq!(result.inputs.len(), 4); // empty sources remain revisioned inputs
    }

    #[test]
    fn stale_source_refreshes_fail_at_the_pure_decision_boundary() {
        for kind in [SourceKind::Provider, SourceKind::Local, SourceKind::Scan] {
            let original = doc("source", kind, 7, r#"{"title":"Original"}"#);
            let edit = patch(
                "source",
                kind,
                7,
                r#"{"title":{"op":"set","value":"Newer"}}"#,
            );
            let newer = plan_update(&schema(), Some(&original), &edit).unwrap();
            assert_eq!(newer.revision(), 8);
            assert_eq!(
                plan_update(&schema(), Some(&newer), &edit),
                Err(MetadataError::StaleSourceRevision {
                    expected: 7,
                    actual: 8
                })
            );
            assert_eq!(newer.fields()["title"], "Newer");
            assert_eq!(original.fields()["title"], "Original");
        }
    }

    #[test]
    fn missing_and_present_source_revision_preconditions_do_not_alias() {
        let create = patch(
            "provider",
            SourceKind::Provider,
            0,
            r#"{"title":{"op":"set","value":"New"}}"#,
        );
        let first = plan_update(&schema(), None, &create).unwrap();
        assert_eq!(first.revision(), 1);
        assert_eq!(
            plan_update(&schema(), Some(&first), &create),
            Err(MetadataError::StaleSourceRevision {
                expected: 0,
                actual: 1
            })
        );
        assert_eq!(
            plan_update(
                &schema(),
                None,
                &patch("provider", SourceKind::Provider, 1, "{}")
            ),
            Err(MetadataError::StaleSourceRevision {
                expected: 1,
                actual: 0
            })
        );
        let empty = plan_update(
            &schema(),
            Some(&first),
            &patch(
                "provider",
                SourceKind::Provider,
                1,
                r#"{"title":{"op":"remove"}}"#,
            ),
        )
        .unwrap();
        assert!(empty.fields().is_empty());
        assert_eq!(empty.revision(), 2);
        assert_eq!(
            plan_update(&schema(), Some(&empty), &create),
            Err(MetadataError::StaleSourceRevision {
                expected: 0,
                actual: 2
            })
        );
    }

    #[test]
    fn updates_cannot_target_another_source_or_relabel_provenance() {
        let current = doc("provider", SourceKind::Provider, 1, "{}");
        assert_eq!(
            plan_update(
                &schema(),
                Some(&current),
                &patch("other", SourceKind::Provider, 1, "{}")
            ),
            Err(MetadataError::SourceMismatch)
        );
        assert_eq!(
            plan_update(
                &schema(),
                Some(&current),
                &patch("provider", SourceKind::Local, 1, "{}")
            ),
            Err(MetadataError::SourceKindMismatch)
        );
        let legacy = doc("legacy", SourceKind::LegacyImport, 1, r#"{"title":"Old"}"#);
        assert_eq!(
            plan_update(
                &schema(),
                Some(&legacy),
                &patch("legacy", SourceKind::Provider, 1, "{}")
            ),
            Err(MetadataError::SourceKindMismatch)
        );
    }

    #[test]
    fn retained_legacy_preserves_exact_effective_value_until_explicit_reconciliation() {
        let legacy = doc(
            "legacy",
            SourceKind::LegacyImport,
            9,
            r#"{"title":" Legacy spacing ","language":"EN"}"#,
        );
        let provider = doc(
            "provider",
            SourceKind::Provider,
            2,
            r#"{"title":"Provider","language":"en"}"#,
        );
        let result = resolve(&schema(), "item:1", &[provider.clone(), legacy.clone()]).unwrap();
        assert_eq!(
            result.fields["title"],
            resolved(
                json!(" Legacy spacing "),
                ResolutionOrigin::RetainedLegacy,
                &[&legacy]
            )
        );
        assert_eq!(
            result.fields["language"],
            resolved(json!("EN"), ResolutionOrigin::RetainedLegacy, &[&legacy])
        );
        let refreshed = plan_update(
            &schema(),
            Some(&provider),
            &patch(
                "provider",
                SourceKind::Provider,
                2,
                r#"{"title":{"op":"set","value":"Refreshed"}}"#,
            ),
        )
        .unwrap();
        assert_eq!(
            resolve(&schema(), "item:1", &[refreshed.clone(), legacy.clone()])
                .unwrap()
                .fields["title"],
            result.fields["title"]
        );
        assert_eq!(
            ContributionPatch::from_json(
                &schema(),
                key("legacy"),
                SourceKind::LegacyImport,
                9,
                r#"{"title":{"op":"remove"}}"#,
                "{}"
            ),
            Err(MetadataError::LegacyReconciliationRequired)
        );
        assert_eq!(
            plan_legacy_reconciliation(&schema(), &legacy, 8, &["title".into()]),
            Err(MetadataError::StaleSourceRevision {
                expected: 8,
                actual: 9
            })
        );
        let reconciled =
            plan_legacy_reconciliation(&schema(), &legacy, 9, &["title".into()]).unwrap();
        assert_eq!(reconciled.kind(), SourceKind::LegacyImport);
        assert_eq!(reconciled.revision(), 10);
        assert_eq!(reconciled.evidence(), legacy.evidence());
        let result = resolve(
            &schema(),
            "item:1",
            &[refreshed.clone(), reconciled.clone()],
        )
        .unwrap();
        assert_eq!(
            result.fields["title"],
            resolved(
                json!("Refreshed"),
                ResolutionOrigin::Provider,
                &[&refreshed]
            )
        );
        assert_eq!(
            result.fields["language"],
            resolved(
                json!("EN"),
                ResolutionOrigin::RetainedLegacy,
                &[&reconciled]
            )
        );
        assert_eq!(legacy.fields()["title"], " Legacy spacing ");
    }

    #[test]
    fn explicit_local_choice_does_not_implicitly_reconcile_legacy() {
        let legacy = doc(
            "legacy",
            SourceKind::LegacyImport,
            1,
            r#"{"title":"Legacy"}"#,
        );
        let local = doc("local", SourceKind::Local, 1, r#"{"title":"Local"}"#);
        assert_eq!(
            resolve(&schema(), "item:1", &[legacy.clone(), local.clone()])
                .unwrap()
                .fields["title"],
            resolved(json!("Local"), ResolutionOrigin::Local, &[&local])
        );
        let local = plan_update(
            &schema(),
            Some(&local),
            &patch(
                "local",
                SourceKind::Local,
                1,
                r#"{"title":{"op":"remove"}}"#,
            ),
        )
        .unwrap();
        assert_eq!(
            resolve(&schema(), "item:1", &[legacy.clone(), local])
                .unwrap()
                .fields["title"],
            resolved(
                json!("Legacy"),
                ResolutionOrigin::RetainedLegacy,
                &[&legacy]
            )
        );
    }

    #[test]
    fn agreeing_legacy_remains_legacy_and_conflicting_legacy_is_never_arbitrarily_chosen() {
        let a = doc(
            "legacy:a",
            SourceKind::LegacyImport,
            1,
            r#"{"title":"Equal"}"#,
        );
        let b = doc(
            "legacy:b",
            SourceKind::LegacyImport,
            2,
            r#"{"title":"Equal"}"#,
        );
        let provider = doc("provider", SourceKind::Provider, 1, r#"{"title":"Equal"}"#);
        assert_eq!(
            resolve(
                &schema(),
                "item:1",
                &[a.clone(), b.clone(), provider.clone()]
            )
            .unwrap()
            .fields["title"],
            resolved(json!("Equal"), ResolutionOrigin::RetainedLegacy, &[&a, &b])
        );
        let c = doc(
            "legacy:c",
            SourceKind::LegacyImport,
            1,
            r#"{"title":"Other"}"#,
        );
        assert!(matches!(
            resolve(&schema(), "item:1", &[a, b, c, provider])
                .unwrap()
                .fields["title"],
            FieldResolution::Conflict {
                origin: ResolutionOrigin::RetainedLegacy,
                ..
            }
        ));
    }

    #[test]
    fn scan_disagreement_is_explicit_and_never_input_order_dependent() {
        let a = doc("scan:a", SourceKind::Scan, 1, r#"{"title":"A"}"#);
        let b = doc("scan:b", SourceKind::Scan, 3, r#"{"title":"B"}"#);
        let forward = resolve(&schema(), "item:1", &[a.clone(), b.clone()]).unwrap();
        let reverse = resolve(&schema(), "item:1", &[b, a]).unwrap();
        assert_eq!(forward, reverse);
        assert!(matches!(
            forward.fields["title"],
            FieldResolution::Conflict {
                origin: ResolutionOrigin::Scan,
                ..
            }
        ));
    }

    #[test]
    fn deterministic_resolution_and_serialization_across_all_input_permutations() {
        let documents = [
            doc(
                "a",
                SourceKind::Provider,
                1,
                r#"{"title":"Alpha","year":2026}"#,
            ),
            doc(
                "b",
                SourceKind::Provider,
                4,
                r#"{"title":"Beta","language":null}"#,
            ),
            doc(
                "c",
                SourceKind::Provider,
                9,
                r#"{"title":"Alpha","language":"EN"}"#,
            ),
            doc("d", SourceKind::Scan, 5, r#"{"flag":true}"#),
        ];
        let baseline = resolve(&schema(), "item:1", &documents).unwrap();
        let baseline_wire = serde_json::to_string(&baseline).unwrap();
        for a in 0..4 {
            for b in 0..4 {
                for c in 0..4 {
                    for d in 0..4 {
                        if BTreeSet::from([a, b, c, d]).len() != 4 {
                            continue;
                        }
                        let permutation = [a, b, c, d].map(|index| documents[index].clone());
                        let result = resolve(&schema(), "item:1", &permutation).unwrap();
                        assert_eq!(result, baseline);
                        assert_eq!(serde_json::to_string(&result).unwrap(), baseline_wire);
                    }
                }
            }
        }
    }

    #[test]
    fn exact_json_uses_shared_number_semantics_and_preserves_literal_private_sentinel() {
        let a = doc(
            "a",
            SourceKind::Provider,
            1,
            r#"{"details":{"x":1e2,"n":1208925819614629174706176,"$serde_json::private::Number":"1e400"}}"#,
        );
        let b = doc(
            "b",
            SourceKind::Provider,
            1,
            r#"{"details":{"$serde_json::private::Number":"1e400","n":1208925819614629174706176,"x":100.0}}"#,
        );
        assert!(
            matches!(resolve(&schema(), "item:1", &[a.clone(), b]).unwrap().fields["details"], FieldResolution::Resolved { ref sources, .. } if sources.len() == 2)
        );
        let integer = doc(
            "c",
            SourceKind::Provider,
            1,
            r#"{"details":{"x":100,"n":1208925819614629174706176,"$serde_json::private::Number":"1e400"}}"#,
        );
        assert!(matches!(
            resolve(&schema(), "item:1", &[a, integer]).unwrap().fields["details"],
            FieldResolution::Conflict { .. }
        ));
    }

    #[test]
    fn null_and_field_shape_policy_are_explicit() {
        for (raw, expected) in [
            (r#"{"title":null}"#, MetadataError::NullNotAllowed),
            (r#"{"title":true}"#, MetadataError::InvalidFieldValue),
            (r#"{"year":1.0}"#, MetadataError::InvalidFieldValue),
            (r#"{"flag":1}"#, MetadataError::InvalidFieldValue),
            (r#"{"unknown":"x"}"#, MetadataError::UnknownField),
        ] {
            assert_eq!(
                ContributionDocument::from_json(
                    &schema(),
                    key("p"),
                    SourceKind::Provider,
                    1,
                    raw,
                    "{}"
                ),
                Err(expected)
            );
        }
        assert_eq!(
            ContributionPatch::from_json(
                &schema(),
                key("l"),
                SourceKind::Local,
                0,
                r#"{"title":{"op":"set","value":null}}"#,
                "{}"
            ),
            Err(MetadataError::NullNotAllowed)
        );
        let nullable = patch(
            "local",
            SourceKind::Local,
            0,
            r#"{"language":{"op":"set","value":null}}"#,
        );
        assert_eq!(nullable.edits()["language"], FieldEdit::Set(Value::Null));
        let integer = doc(
            "p",
            SourceKind::Provider,
            1,
            r#"{"year":1208925819614629174706176}"#,
        );
        assert_eq!(
            integer.fields()["year"].to_string(),
            "1208925819614629174706176"
        );
    }

    #[test]
    fn malformed_or_ambiguous_patch_operations_fail_closed() {
        for raw in [
            r#"{"title":null}"#,
            r#"{"title":"new"}"#,
            r#"{"title":{"op":"set"}}"#,
            r#"{"title":{"op":"remove","value":null}}"#,
            r#"{"title":{"op":"set","value":"new","ignored":true}}"#,
            r#"{"title":{"op":"replace","value":"new"}}"#,
        ] {
            assert_eq!(
                ContributionPatch::from_json(&schema(), key("l"), SourceKind::Local, 0, raw, "{}"),
                Err(MetadataError::InvalidOperation),
                "{raw}"
            );
        }
        assert_eq!(
            ContributionPatch::from_json(
                &schema(),
                key("l"),
                SourceKind::Local,
                0,
                r#"{"unknown":{"op":"remove"}}"#,
                "{}"
            ),
            Err(MetadataError::UnknownField)
        );
        assert_eq!(
            ContributionDocument::from_json(
                &schema(),
                key("p"),
                SourceKind::Provider,
                1,
                "[]",
                "{}"
            ),
            Err(MetadataError::ExpectedObject)
        );
        assert_eq!(
            ContributionDocument::from_json(
                &schema(),
                key("p"),
                SourceKind::Provider,
                1,
                "{}",
                "null"
            ),
            Err(MetadataError::ExpectedObject)
        );
    }

    #[test]
    fn duplicate_object_keys_and_nonfinite_values_are_rejected_before_lossy_parsing() {
        for raw in [
            r#"{"title":"A","title":"B"}"#,
            r#"{"title":"A","\u0074itle":"B"}"#,
            r#"{"details":{"x":1,"x":2}}"#,
        ] {
            assert_eq!(
                ContributionDocument::from_json(
                    &schema(),
                    key("p"),
                    SourceKind::Provider,
                    1,
                    raw,
                    "{}"
                ),
                Err(MetadataError::InvalidJson)
            );
        }
        assert_eq!(
            ContributionPatch::from_json(
                &schema(),
                key("l"),
                SourceKind::Local,
                0,
                r#"{"title":{"op":"set","op":"remove","value":"x"}}"#,
                "{}"
            ),
            Err(MetadataError::InvalidJson)
        );
        assert_eq!(
            ContributionDocument::from_json(
                &schema(),
                key("p"),
                SourceKind::Provider,
                1,
                r#"{"details":1e400}"#,
                "{}"
            ),
            Err(MetadataError::NonFiniteJson)
        );
        assert_eq!(
            ContributionDocument::from_json(
                &schema(),
                key("p"),
                SourceKind::Provider,
                1,
                "{}",
                r#"{"x":1,"x":2}"#
            ),
            Err(MetadataError::InvalidJson)
        );
    }

    #[test]
    fn revision_and_identifier_bounds_are_enforced() {
        for revision in [i64::MIN, -1, 0] {
            assert_eq!(
                ContributionDocument::from_json(
                    &schema(),
                    key("p"),
                    SourceKind::Provider,
                    revision,
                    "{}",
                    "{}"
                ),
                Err(MetadataError::InvalidRevision)
            );
        }
        assert_eq!(
            ContributionPatch::from_json(&schema(), key("p"), SourceKind::Provider, -1, "{}", "{}"),
            Err(MetadataError::InvalidRevision)
        );
        let final_revision = doc("p", SourceKind::Provider, i64::MAX, "{}");
        assert_eq!(
            plan_update(
                &schema(),
                Some(&final_revision),
                &patch("p", SourceKind::Provider, i64::MAX, "{}")
            ),
            Err(MetadataError::RevisionExhausted)
        );
        for identifier in [
            "".into(),
            " \t ".into(),
            "a\0b".into(),
            "é".repeat(MAX_METADATA_IDENTIFIER_BYTES / 2 + 1),
        ] {
            assert_eq!(
                ContributionKey::new("item", &identifier, "record"),
                Err(MetadataError::InvalidIdentifier)
            );
            assert_eq!(
                ContributionKey::new(&identifier, "provider", "record"),
                Err(MetadataError::InvalidIdentifier)
            );
            assert_eq!(
                ContributionKey::new("item", "provider", &identifier),
                Err(MetadataError::InvalidIdentifier)
            );
        }
        assert_ne!(key("provider"), key("Provider"));
        assert_ne!(key("provider"), key(" provider")); // opaque: no key normalization
    }

    #[test]
    fn schema_is_bounded_deterministic_and_cannot_reinterpret_existing_documents() {
        let field_policy = policy(ValueType::String, false, Normalization::Exact);
        assert_eq!(FieldSchema::new(vec![]), Err(MetadataError::EmptySchema));
        assert_eq!(
            FieldSchema::new(vec![("a".into(), field_policy), ("a".into(), field_policy)]),
            Err(MetadataError::DuplicateField)
        );
        for field in ["".into(), " a".into(), "a\nb".into(), "é".repeat(33)] {
            assert_eq!(
                FieldSchema::new(vec![(field, field_policy)]),
                Err(MetadataError::InvalidField)
            );
        }
        assert_eq!(
            FieldSchema::new(vec![(
                "a".into(),
                policy(ValueType::Boolean, false, Normalization::TrimString)
            )]),
            Err(MetadataError::InvalidPolicy)
        );
        assert_eq!(
            FieldSchema::new(
                (0..=MAX_METADATA_FIELDS)
                    .map(|index| (format!("f{index}"), field_policy))
                    .collect()
            ),
            Err(MetadataError::FieldBudget)
        );
        let mut policies: Vec<_> = schema().fields.into_iter().collect();
        let forward = FieldSchema::new(policies.clone()).unwrap();
        policies.reverse();
        assert_eq!(FieldSchema::new(policies.clone()).unwrap(), forward);
        policies
            .iter_mut()
            .find(|(name, _)| name == "title")
            .unwrap()
            .1
            .normalization = Normalization::Exact;
        let changed = FieldSchema::new(policies).unwrap();
        let document = doc("p", SourceKind::Provider, 1, r#"{"title":"x"}"#);
        assert_eq!(
            resolve(&changed, "item:1", std::slice::from_ref(&document)),
            Err(MetadataError::SchemaMismatch)
        );
        assert_eq!(
            plan_update(
                &changed,
                Some(&document),
                &patch("p", SourceKind::Provider, 1, "{}")
            ),
            Err(MetadataError::SchemaMismatch)
        );
    }

    #[test]
    fn snapshot_rejects_wrong_subject_duplicate_source_and_multiple_local_documents() {
        let document = doc("p", SourceKind::Provider, 1, "{}");
        assert_eq!(
            resolve(&schema(), "item:2", std::slice::from_ref(&document)),
            Err(MetadataError::SubjectMismatch)
        );
        assert_eq!(
            resolve(&schema(), "item:1", &[document.clone(), document]),
            Err(MetadataError::DuplicateSource)
        );
        let a = doc("local:a", SourceKind::Local, 1, r#"{"title":"a"}"#);
        let b = doc("local:b", SourceKind::Local, 2, r#"{"language":"en"}"#);
        assert_eq!(
            resolve(&schema(), "item:1", &[a, b]),
            Err(MetadataError::MultipleLocalSources)
        );
        assert_eq!(
            resolve(&schema(), "", &[]),
            Err(MetadataError::InvalidIdentifier)
        );
    }

    #[test]
    fn reconciliation_requires_exact_named_existing_fields_and_keeps_empty_revision() {
        let legacy = doc("legacy", SourceKind::LegacyImport, 1, r#"{"title":"old"}"#);
        for (fields, expected) in [
            (vec![], MetadataError::EmptyReconciliation),
            (vec!["unknown".into()], MetadataError::UnknownField),
            (vec!["language".into()], MetadataError::FieldAbsent),
            (
                vec!["title".into(), "title".into()],
                MetadataError::DuplicateField,
            ),
        ] {
            assert_eq!(
                plan_legacy_reconciliation(&schema(), &legacy, 1, &fields),
                Err(expected)
            );
        }
        let empty = plan_legacy_reconciliation(&schema(), &legacy, 1, &["title".into()]).unwrap();
        assert!(empty.fields().is_empty());
        assert_eq!(empty.revision(), 2);
        assert_eq!(empty.kind(), SourceKind::LegacyImport);
        assert_eq!(
            plan_legacy_reconciliation(&schema(), &empty, 1, &["title".into()]),
            Err(MetadataError::StaleSourceRevision {
                expected: 1,
                actual: 2
            })
        );
        assert_eq!(
            plan_legacy_reconciliation(
                &schema(),
                &doc("p", SourceKind::Provider, 1, "{}"),
                1,
                &["title".into()]
            ),
            Err(MetadataError::NotLegacyImport)
        );
    }

    #[test]
    fn byte_depth_node_field_source_and_snapshot_budgets_fail_closed() {
        let make = |raw: &str| {
            ContributionDocument::from_json(&schema(), key("p"), SourceKind::Provider, 1, raw, "{}")
        };
        assert_eq!(
            make(&" ".repeat(MAX_METADATA_DOCUMENT_BYTES + 1)),
            Err(MetadataError::DocumentBudget)
        );
        let evidence = format!(
            r#"{{"data":"{}"}}"#,
            "x".repeat(MAX_METADATA_EVIDENCE_BYTES)
        );
        assert_eq!(
            ContributionDocument::from_json(
                &schema(),
                key("p"),
                SourceKind::Provider,
                1,
                "{}",
                &evidence
            ),
            Err(MetadataError::EvidenceBudget)
        );
        let deep = format!(
            "{{\"details\":{}0{}}}",
            "[".repeat(MAX_METADATA_JSON_DEPTH),
            "]".repeat(MAX_METADATA_JSON_DEPTH)
        );
        assert_eq!(make(&deep), Err(MetadataError::JsonDepthBudget));
        let nodes = format!(
            r#"{{"details":[{}]}}"#,
            vec!["0"; MAX_METADATA_JSON_NODES].join(",")
        );
        assert_eq!(make(&nodes), Err(MetadataError::JsonNodeBudget));
        let fields = format!(
            "{{{}}}",
            (0..=MAX_METADATA_FIELDS)
                .map(|index| format!(r#""f{index}":0"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(make(&fields), Err(MetadataError::FieldBudget));
        let sources = (0..=MAX_METADATA_DOCUMENTS)
            .map(|index| doc(&format!("p{index}"), SourceKind::Provider, 1, "{}"))
            .collect::<Vec<_>>();
        assert_eq!(
            resolve(&schema(), "item:1", &sources),
            Err(MetadataError::SourceBudget)
        );
        let raw = format!(r#"{{"title":"{}"}}"#, "x".repeat(32 * 1024));
        let large = (0..65)
            .map(|index| doc(&format!("p{index}"), SourceKind::Provider, 1, &raw))
            .collect::<Vec<_>>();
        assert_eq!(
            resolve(&schema(), "item:1", &large),
            Err(MetadataError::SnapshotBudget)
        );
    }

    #[test]
    fn enclosing_document_overhead_and_patch_growth_count_towards_byte_budget() {
        let raw = format!(
            r#"{{"title":"{}"}}"#,
            "x".repeat(MAX_METADATA_DOCUMENT_BYTES - 20)
        );
        assert!(raw.len() < MAX_METADATA_DOCUMENT_BYTES);
        assert_eq!(
            ContributionDocument::from_json(
                &schema(),
                key("p"),
                SourceKind::Provider,
                1,
                &raw,
                "{}"
            ),
            Err(MetadataError::DocumentBudget)
        );
        let current = doc(
            "p",
            SourceKind::Provider,
            1,
            &format!(r#"{{"title":"{}"}}"#, "x".repeat(40 * 1024)),
        );
        let edit = patch(
            "p",
            SourceKind::Provider,
            1,
            &format!(
                r#"{{"language":{{"op":"set","value":"{}"}}}}"#,
                "y".repeat(30 * 1024)
            ),
        );
        assert_eq!(
            plan_update(&schema(), Some(&current), &edit),
            Err(MetadataError::DocumentBudget)
        );
        assert_eq!(current.revision(), 1);
    }

    #[test]
    fn quoted_brackets_do_not_consume_depth_and_ascii_casefold_does_not_claim_unicode_equivalence()
    {
        let raw = serde_json::to_string(&json!({"title": "[\"\\{".repeat(50)})).unwrap();
        assert!(
            ContributionDocument::from_json(
                &schema(),
                key("p"),
                SourceKind::Provider,
                1,
                &raw,
                "{}"
            )
            .is_ok()
        );
        let a = doc("a", SourceKind::Provider, 1, r#"{"language":"ÉN"}"#);
        let b = doc("b", SourceKind::Provider, 1, r#"{"language":"én"}"#);
        assert!(matches!(
            resolve(&schema(), "item:1", &[a, b]).unwrap().fields["language"],
            FieldResolution::Conflict { .. }
        ));
    }

    #[test]
    fn exact_limits_are_admitted_and_source_record_id_is_part_of_identity() {
        assert!(ContributionKey::new("x".repeat(MAX_METADATA_IDENTIFIER_BYTES), "p", "r").is_ok());
        let fields = (0..MAX_METADATA_FIELDS)
            .map(|index| {
                (
                    format!("f{index}"),
                    policy(ValueType::Json, true, Normalization::Exact),
                )
            })
            .collect();
        assert_eq!(
            FieldSchema::new(fields).unwrap().fields().len(),
            MAX_METADATA_FIELDS
        );
        let documents = (0..MAX_METADATA_DOCUMENTS)
            .map(|index| {
                ContributionDocument::from_json(
                    &schema(),
                    ContributionKey::new("item:1", "provider", format!("record:{index:03}"))
                        .unwrap(),
                    SourceKind::Provider,
                    1,
                    r#"{"title":"Same"}"#,
                    "{}",
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let result = resolve(&schema(), "item:1", &documents).unwrap();
        assert_eq!(result.inputs.len(), MAX_METADATA_DOCUMENTS);
        assert!(
            matches!(result.fields["title"], FieldResolution::Resolved { ref sources, .. } if sources.len() == MAX_METADATA_DOCUMENTS)
        );
        let raw = format!(
            "{{\"details\":{}0{}}}",
            "[".repeat(MAX_METADATA_JSON_DEPTH - 1),
            "]".repeat(MAX_METADATA_JSON_DEPTH - 1)
        );
        assert!(
            ContributionDocument::from_json(
                &schema(),
                key("p"),
                SourceKind::Provider,
                1,
                &raw,
                "{}"
            )
            .is_ok()
        );
    }

    #[test]
    fn snapshot_membership_and_empty_sources_are_visible_in_resolution_inputs() {
        let empty = doc("empty", SourceKind::Provider, 6, "{}");
        let result = resolve(&schema(), "item:1", std::slice::from_ref(&empty)).unwrap();
        assert!(
            result
                .fields
                .values()
                .all(|field| *field == FieldResolution::Absent)
        );
        assert_eq!(result.inputs, vec![SourceRevision::from(&empty)]);
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["fields"]["title"], json!({"state":"absent"}));
        assert_eq!(wire["contract_version"], METADATA_CONTRACT_VERSION);
        let update = patch("empty", SourceKind::Provider, 6, "{}");
        assert_eq!(update.key(), empty.key());
        assert_eq!(update.kind(), SourceKind::Provider);
        assert_eq!(update.schema_fingerprint(), schema().fingerprint());
        assert_eq!(update.evidence()["observation"], "refresh");
    }
}
