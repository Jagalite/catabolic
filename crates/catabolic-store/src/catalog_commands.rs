// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Durable, database-only catalog commands. This is a privileged store owner,
//! not an authentication boundary: the embedding host must establish current
//! authority before supplying [`TrustedCommandAuthority`]. Never deserialize that
//! authority, receipt policy, or maintenance scope from an untrusted request.
//!
//! The public entry point owns a top-level writer transaction. Its operations are bounded item puts through the existing curation owner and
//! typed identity mutations through the native identity owner; it does
//! not accept arbitrary mutation callbacks or perform external effects. Known
//! receipts precede stale preconditions, operation parsing and storage admission.
//! Full results may expire, but command and hold keys are never recycled.

use crate::{
    Error, Store,
    catalog_identity_mutation::{self, IdentityAuthority, IdentityMutation},
    catalog_import::{self, ImportAuthority, ImportMutation, TrustedImportActivation},
    catalog_logical_edits::{self, LogicalEditAuthority, ReviewedLogicalEdit},
    catalog_metadata_store::{self, MetadataAuthority, MetadataMutation},
    catalog_pins::{self, PinAuthority, PinMutation},
    catalog_relationships::{self, RelationshipAuthority, RelationshipMutation},
    catalog_tag_mutation::{self, TagAuthority, TagMutation},
    curation,
    migration::is_supported_native_schema,
};
use catabolic_core::catalog_command::{
    CommandDecision, CommandEnvelope, CommandError, CommandKey, CommandResult, CommandSnapshot,
    PayloadDigest, ReceiptBody, ReceiptStorage, ReceiptVisibility, StoredReceipt, decide_command,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MAX_ITEM_PUT_BATCH: usize = 128;
pub const MAX_COMPACTION_BATCH: usize = 1024;
/// Logical accounting includes retained strings and fixed per-row charges.
/// It is an admission budget, not a SQLite file-size or free-space cap.
const RECEIPT_OVERHEAD_BYTES: u64 = 256;
const HOLD_OVERHEAD_BYTES: u64 = 128;

/// Current host authorization, resolved outside this privileged owner. A
/// status-only reconciler must set `allow_new_commands` to false. No receipt
/// body is returned when `receipt_visibility` is `CommittedStatusOnly`.
/// `FullResult` is an independent, current trusted receipt-read capability for
/// the entire retained result within this exact host namespace. It is not
/// inferred from mutation grants or from membership in the same host. The host
/// must choose status-only when the caller may not read any retained member.
/// Receipt replay intentionally precedes mutation parsing and mutation grants;
/// revoking write authority alone does not revoke this distinct read capability.
#[derive(Clone, Copy, Debug)]
pub struct TrustedCommandAuthority<'a> {
    pub host_namespace: &'a str,
    pub receipt_visibility: ReceiptVisibility,
    pub allow_new_commands: bool,
    pub identity_authority: Option<&'a IdentityAuthority>,
}

/// Current host grants for typed catalog commands. Never deserialize grants
/// from the request or infer them from a preview or its affected-member list.
#[derive(Clone, Copy, Debug)]
pub struct CatalogAuthority<'a> {
    base: TrustedCommandAuthority<'a>,
    metadata: Option<&'a MetadataAuthority>,
    metadata_initialization: Option<&'a MetadataAuthority>,
    metadata_schema: Option<&'a MetadataAuthority>,
    metadata_origin: Option<&'a catalog_metadata_store::VerifiedCreatedOrigin>,
    artwork_attach: Option<&'a catalog_metadata_store::ArtworkAttachAuthority>,
    tags: Option<&'a TagAuthority>,
    pins: Option<&'a PinAuthority>,
    logical_edits: Option<&'a LogicalEditAuthority>,
    imports: Option<&'a ImportAuthority>,
    import_activation: Option<&'a TrustedImportActivation>,
    import_typed: Option<&'a crate::catalog_import_typed::TypedAuthority>,
    discovery: Option<&'a crate::catalog_discovery::FreshMediaDiscoveryAuthority>,
    processing: Option<&'a crate::catalog_processing::ProcessingEnqueueAuthority>,
    relationships: Option<&'a RelationshipAuthority>,
    empty_edition: Option<&'a crate::catalog_edition_creation::EmptyEditionAuthority>,
    generated_output: Option<&'a crate::catalog_generated_output::GeneratedOutputAuthority>,
}
impl<'a> CatalogAuthority<'a> {
    pub fn new(base: TrustedCommandAuthority<'a>) -> Self {
        Self {
            base,
            metadata: None,
            metadata_initialization: None,
            metadata_schema: None,
            metadata_origin: None,
            artwork_attach: None,
            tags: None,
            pins: None,
            logical_edits: None,
            imports: None,
            import_activation: None,
            import_typed: None,
            discovery: None,
            processing: None,
            relationships: None,
            empty_edition: None,
            generated_output: None,
        }
    }
    pub fn with_empty_edition(
        mut self,
        grant: &'a crate::catalog_edition_creation::EmptyEditionAuthority,
    ) -> Self {
        self.empty_edition = Some(grant);
        self
    }
    pub fn with_relationships(mut self, grant: &'a RelationshipAuthority) -> Self {
        self.relationships = Some(grant);
        self
    }
    pub fn with_generated_output(
        mut self,
        grant: &'a crate::catalog_generated_output::GeneratedOutputAuthority,
    ) -> Self {
        self.generated_output = Some(grant);
        self
    }
    pub fn with_processing(
        mut self,
        grant: &'a crate::catalog_processing::ProcessingEnqueueAuthority,
    ) -> Self {
        self.processing = Some(grant);
        self
    }
    pub fn with_discovery(
        mut self,
        grant: &'a crate::catalog_discovery::FreshMediaDiscoveryAuthority,
    ) -> Self {
        self.discovery = Some(grant);
        self
    }
    pub fn with_import_typed(
        mut self,
        grant: &'a crate::catalog_import_typed::TypedAuthority,
    ) -> Self {
        self.import_typed = Some(grant);
        self
    }
    pub fn with_import_activation(mut self, grant: &'a TrustedImportActivation) -> Self {
        self.import_activation = Some(grant);
        self
    }
    pub fn with_imports(mut self, grant: &'a ImportAuthority) -> Self {
        self.imports = Some(grant);
        self
    }
    pub fn with_metadata(mut self, grant: &'a MetadataAuthority) -> Self {
        self.metadata = Some(grant);
        self
    }
    /// Separate initialization authority is required in addition to the source
    /// grant for atomic first-edit composition. Neither is inferred from payload.
    pub fn with_metadata_initialization(mut self, grant: &'a MetadataAuthority) -> Self {
        self.metadata_initialization = Some(grant);
        self
    }
    pub fn with_metadata_origin(
        mut self,
        grant: &'a catalog_metadata_store::VerifiedCreatedOrigin,
    ) -> Self {
        self.metadata_origin = Some(grant);
        self
    }
    pub fn with_metadata_schema(mut self, grant: &'a MetadataAuthority) -> Self {
        self.metadata_schema = Some(grant);
        self
    }
    pub fn with_artwork_attach(
        mut self,
        grant: &'a catalog_metadata_store::ArtworkAttachAuthority,
    ) -> Self {
        self.artwork_attach = Some(grant);
        self
    }
    pub fn with_tags(mut self, grant: &'a TagAuthority) -> Self {
        self.tags = Some(grant);
        self
    }
    pub fn with_logical_edits(mut self, grant: &'a LogicalEditAuthority) -> Self {
        self.logical_edits = Some(grant);
        self
    }
    pub fn with_pins(mut self, grant: &'a PinAuthority) -> Self {
        self.pins = Some(grant);
        self
    }
}
fn required_grant<T>(grant: Option<&T>) -> Result<&T> {
    grant.ok_or_else(|| CommandStoreError {
        code: CommandStoreErrorCode::AuthorityDenied,
        error: Error("typed catalog command requires current host authority".into()),
    })
}

/// Explicit, host-configured limits. There is deliberately no implicit retention
/// or eviction default. Counts include permanent tombstones and released holds.
#[derive(Clone, Copy, Debug)]
pub struct ReceiptPolicy {
    pub retention: Duration,
    pub max_receipts: u64,
    pub max_logical_bytes: u64,
    pub max_hold_records: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ReceiptUsage {
    pub receipts: u64,
    pub hold_records: u64,
    pub logical_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandStoreErrorCode {
    AmbientTransaction,
    UnsupportedSchema,
    AuthorityDenied,
    InvalidOperation,
    OperationConflict,
    InvalidPolicy,
    InvalidHoldIdentifier,
    InvalidCompactionLimit,
    InvalidReceipt,
    Import(catalog_import::ImportErrorCode),
    Validation(CommandError),
    StoreFailure,
}
impl CommandStoreErrorCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::AmbientTransaction => "catalog_command_ambient_transaction",
            Self::UnsupportedSchema => "catalog_command_unsupported_schema",
            Self::AuthorityDenied => "catalog_command_authority_denied",
            Self::InvalidOperation => "catalog_command_invalid_operation",
            Self::OperationConflict => "catalog_command_operation_conflict",
            Self::InvalidPolicy => "catalog_command_invalid_policy",
            Self::InvalidHoldIdentifier => "catalog_command_invalid_hold_identifier",
            Self::InvalidCompactionLimit => "catalog_command_invalid_compaction_limit",
            Self::InvalidReceipt => "catalog_command_invalid_receipt",
            Self::Import(error) => error.code(),
            Self::Validation(error) => error.code(),
            Self::StoreFailure => "catalog_command_store_failure",
        }
    }
}
#[derive(Debug)]
pub struct CommandStoreError {
    pub code: CommandStoreErrorCode,
    pub error: Error,
}
impl std::fmt::Display for CommandStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.code(), self.error)
    }
}
impl std::error::Error for CommandStoreError {}
impl From<Error> for CommandStoreError {
    fn from(error: Error) -> Self {
        Self {
            code: CommandStoreErrorCode::StoreFailure,
            error,
        }
    }
}
impl From<rusqlite::Error> for CommandStoreError {
    fn from(error: rusqlite::Error) -> Self {
        Error::from(error).into()
    }
}
impl From<CommandError> for CommandStoreError {
    fn from(error: CommandError) -> Self {
        Self {
            code: CommandStoreErrorCode::Validation(error),
            error: Error(error.to_string()),
        }
    }
}
// A successful-looking storage refusal must not escape the transaction until
// rollback succeeds. Keep that control signal private and distinct from errors.
enum ExecutionError {
    Store(CommandStoreError),
    StorageExhausted,
}
impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => error.fmt(f),
            Self::StorageExhausted => f.write_str("receipt_storage_exhausted"),
        }
    }
}
impl From<CommandStoreError> for ExecutionError {
    fn from(error: CommandStoreError) -> Self {
        Self::Store(error)
    }
}
impl From<Error> for ExecutionError {
    fn from(error: Error) -> Self {
        Self::Store(error.into())
    }
}
impl From<rusqlite::Error> for ExecutionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.into())
    }
}
impl From<CommandError> for ExecutionError {
    fn from(error: CommandError) -> Self {
        Self::Store(error.into())
    }
}

pub type Result<T> = std::result::Result<T, CommandStoreError>;
fn fail<T>(code: CommandStoreErrorCode, message: &str) -> Result<T> {
    Err(CommandStoreError {
        code,
        error: Error(message.into()),
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CommandOutcome {
    Committed {
        result: Value,
        committed_generation: i64,
        retain_until_us: i64,
    },
    Replay {
        result: Value,
        committed_generation: i64,
        retain_until_us: i64,
    },
    CommittedRedacted,
    ReceiptExpired,
    Conflict,
    OutcomeUnknownAfterRestore,
    StaleGeneration,
    ReceiptStorageExhausted,
}
impl CommandOutcome {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Committed { .. } => "committed",
            Self::Replay { .. } => "replay",
            Self::CommittedRedacted => "committed_redacted",
            Self::ReceiptExpired => "receipt_expired",
            Self::Conflict => "conflict",
            Self::OutcomeUnknownAfterRestore => "outcome_unknown_after_restore",
            Self::StaleGeneration => "stale_generation",
            Self::ReceiptStorageExhausted => "receipt_storage_exhausted",
        }
    }
    pub const fn is_committed(&self) -> bool {
        matches!(
            self,
            Self::Committed { .. }
                | Self::Replay { .. }
                | Self::CommittedRedacted
                | Self::ReceiptExpired
        )
    }
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    EmptyEditionCreate {
        request: crate::catalog_edition_creation::EmptyEditionRequest,
    },
    ItemPut {
        item: Value,
    },
    ItemPutBatch {
        items: Vec<Value>,
    },
    ItemCreateBatch {
        items: Vec<Value>,
    },
    ProcessingEnqueue {
        request: crate::catalog_processing::EnqueueRequest,
    },
    GeneratedOutputAdmit {
        request: crate::catalog_generated_output::GeneratedOutputRequest,
    },
    Relationships {
        mutation: RelationshipMutation,
    },
    FreshMediaDiscovery {
        items: Vec<Value>,
        native: crate::catalog_import_typed::NativeIdentityBatch,
        relationships: Vec<crate::catalog_import_typed::ImportedRelationship>,
    },
    Identity {
        mutation: IdentityMutation,
    },
    Metadata {
        mutation: MetadataMutation,
    },
    MetadataInitializeV2 {
        request: catalog_metadata_store::MetadataInitializeV2,
    },
    MetadataSystemReplace {
        request: catalog_metadata_store::MetadataSystemReplace,
    },
    MetadataArtworkAttach {
        request: catalog_metadata_store::MetadataArtworkAttach,
    },
    MetadataContributionReplace {
        request: catalog_metadata_store::ContributionReplace,
    },
    MetadataInitializeAndPatch {
        subject: String,
        policies: Vec<catalog_metadata_store::PolicyField>,
        namespace: String,
        record_id: String,
        edits_json: String,
        evidence_json: String,
    },
    Tags {
        mutation: TagMutation,
    },
    Pins {
        mutation: PinMutation,
    },
    LogicalEdit {
        reviewed: ReviewedLogicalEdit,
    },
    Import {
        mutation: ImportMutation,
    },
}
impl Operation {
    fn parse(payload: &Value) -> Result<Self> {
        let operation: Self =
            serde_json::from_value(payload.clone()).map_err(|error| CommandStoreError {
                code: CommandStoreErrorCode::InvalidOperation,
                error: Error(error.to_string()),
            })?;
        let items = match &operation {
            Self::EmptyEditionCreate { .. }
            | Self::ProcessingEnqueue { .. }
            | Self::GeneratedOutputAdmit { .. }
            | Self::Relationships { .. }
            | Self::FreshMediaDiscovery { .. }
            | Self::Import { .. }
            | Self::Identity { .. }
            | Self::Metadata { .. }
            | Self::MetadataInitializeAndPatch { .. }
            | Self::MetadataInitializeV2 { .. }
            | Self::MetadataSystemReplace { .. }
            | Self::MetadataArtworkAttach { .. }
            | Self::MetadataContributionReplace { .. }
            | Self::Tags { .. }
            | Self::Pins { .. }
            | Self::LogicalEdit { .. } => return Ok(operation),
            Self::ItemPut { item } => std::slice::from_ref(item),
            Self::ItemPutBatch { items } | Self::ItemCreateBatch { items }
                if !items.is_empty() && items.len() <= MAX_ITEM_PUT_BATCH =>
            {
                items
            }
            Self::ItemPutBatch { .. } | Self::ItemCreateBatch { .. } => {
                return fail(
                    CommandStoreErrorCode::InvalidOperation,
                    "item-put batch must contain 1..=128 items",
                );
            }
        };
        if matches!(&operation, Self::ItemCreateBatch { .. }) {
            let mut ids = std::collections::BTreeSet::new();
            for item in items {
                if item
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(|id| !valid_identifier(id) || !ids.insert(id))
                {
                    return fail(
                        CommandStoreErrorCode::InvalidOperation,
                        "create batch requires explicit unique item IDs",
                    );
                }
            }
        }
        for item in items {
            let Some(object) = item.as_object() else {
                return fail(
                    CommandStoreErrorCode::InvalidOperation,
                    "item must be an object",
                );
            };
            if object
                .keys()
                .any(|key| !["id", "kind", "metadata", "identities"].contains(&key.as_str()))
                || object
                    .get("id")
                    .is_some_and(|value| value.as_str().is_none_or(|id| !valid_identifier(id)))
            {
                return fail(
                    CommandStoreErrorCode::InvalidOperation,
                    "unknown item field or invalid item ID",
                );
            }
        }
        Ok(operation)
    }
    fn apply(
        self,
        store: &Store,
        command: &CommandEnvelope,
        authority: CatalogAuthority<'_>,
        admitted_at_us: i64,
    ) -> Result<Value> {
        let put = |item: &Value| {
            curation::item_put_typed(store, item).map_err(|error| CommandStoreError {
                code: match error.code {
                    curation::ItemPutErrorCode::InvalidInput => {
                        CommandStoreErrorCode::InvalidOperation
                    }
                    curation::ItemPutErrorCode::Conflict => {
                        CommandStoreErrorCode::OperationConflict
                    }
                    curation::ItemPutErrorCode::Internal => CommandStoreErrorCode::StoreFailure,
                },
                error: error.error,
            })
        };
        match self {
            Self::EmptyEditionCreate { request } => crate::catalog_edition_creation::create(
                store,
                command,
                &request,
                required_grant(authority.empty_edition)?,
                admitted_at_us,
            ),
            Self::Relationships { mutation } => catalog_relationships::apply(
                store,
                command,
                &mutation,
                required_grant(authority.relationships)?,
            ),
            Self::GeneratedOutputAdmit { request } => crate::catalog_generated_output::admit(
                store,
                command,
                &request,
                required_grant(authority.generated_output)?,
                admitted_at_us,
            )
            .map_err(Into::into),
            Self::ProcessingEnqueue { request } => crate::catalog_processing::enqueue(
                store,
                command,
                &request,
                required_grant(authority.processing)?,
            )
            .map_err(Into::into),
            Self::FreshMediaDiscovery {
                items,
                native,
                relationships,
            } => {
                let discovery = required_grant(authority.discovery)?;
                let typed = crate::catalog_discovery::validate(
                    store,
                    command,
                    &items,
                    &native,
                    &relationships,
                    discovery,
                )?;
                let create = Self::parse(&json!({"operation":"item_create_batch","items":items}))?;
                let created = create.apply(store, command, authority, admitted_at_us)?;
                let mappings = crate::catalog_discovery::apply(
                    store,
                    command,
                    native,
                    relationships,
                    discovery,
                    &typed,
                    admitted_at_us,
                )?;
                // Baselines are captured after all typed effects, inside the
                // same durable command transaction, never reconstructed later.
                let mut item_revision_baselines = std::collections::BTreeMap::new();
                for id in &discovery.new_item_ids {
                    let revision: i64 = store.db.query_row(
                        "SELECT revision FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?1) AND deleted=0",
                        [id], |r| r.get(0),
                    )?;
                    item_revision_baselines.insert(id.clone(), revision);
                }
                let measured_originals: Vec<Value> = discovery.measured_originals.iter().map(|(version,proof)|json!({
                    "version_id":version,"file":proof.file,"observed_ctime_ns":proof.observed_ctime_ns,"duration_us":proof.duration_us,
                })).collect();
                Ok(
                    json!({"operation":"fresh_media_discovery","items":created["items"],"typed_results":mappings,"measured_originals":measured_originals,"item_revision_baselines":item_revision_baselines}),
                )
            }
            Self::Import { mutation } => catalog_import::apply(
                store,
                command,
                &mutation,
                required_grant(authority.imports)?,
                authority.import_activation,
                authority.import_typed,
                admitted_at_us,
            )
            .map_err(|error| CommandStoreError {
                code: CommandStoreErrorCode::Import(error.code),
                error: error.error,
            }),
            Self::Identity { mutation } => {
                let authority =
                    authority
                        .base
                        .identity_authority
                        .ok_or_else(|| CommandStoreError {
                            code: CommandStoreErrorCode::AuthorityDenied,
                            error: Error(
                                "native identity mutation requires current editorial authority"
                                    .into(),
                            ),
                        })?;
                let payload =
                    serde_json::to_value(mutation).map_err(|error| CommandStoreError {
                        code: CommandStoreErrorCode::InvalidOperation,
                        error: Error(error.to_string()),
                    })?;
                catalog_identity_mutation::apply_identity(
                    store,
                    &payload,
                    command,
                    authority,
                    admitted_at_us,
                )
                .map_err(|error| CommandStoreError {
                    code: match error.code {
                        catalog_identity_mutation::IdentityErrorCode::InvalidInput
                        | catalog_identity_mutation::IdentityErrorCode::Unsupported => {
                            CommandStoreErrorCode::InvalidOperation
                        }
                        catalog_identity_mutation::IdentityErrorCode::AuthorityDenied => {
                            CommandStoreErrorCode::AuthorityDenied
                        }
                        catalog_identity_mutation::IdentityErrorCode::Conflict => {
                            CommandStoreErrorCode::OperationConflict
                        }
                        catalog_identity_mutation::IdentityErrorCode::StoreFailure => {
                            CommandStoreErrorCode::StoreFailure
                        }
                    },
                    error: error.error,
                })
            }
            Self::MetadataInitializeV2 { request } => catalog_metadata_store::initialize_v2(
                store,
                &request,
                command,
                required_grant(authority.metadata_initialization)?,
                admitted_at_us,
            )
            .map_err(|error| CommandStoreError {
                code: if error.code
                    == catalog_metadata_store::MetadataStoreErrorCode::AuthorityDenied
                {
                    CommandStoreErrorCode::AuthorityDenied
                } else {
                    CommandStoreErrorCode::OperationConflict
                },
                error: error.error,
            }),
            Self::MetadataArtworkAttach { request } => catalog_metadata_store::attach_artwork(
                store,
                &request,
                command,
                required_grant(authority.artwork_attach)?,
                required_grant(authority.metadata)?,
                required_grant(authority.metadata_initialization)?,
                required_grant(authority.metadata_schema)?,
                authority.metadata_origin,
                admitted_at_us,
            )
            .map_err(|error| {
                use catalog_metadata_store::MetadataStoreErrorCode as C;
                CommandStoreError {
                    code: match error.code {
                        C::AuthorityDenied => CommandStoreErrorCode::AuthorityDenied,
                        C::InvalidInput | C::UnsupportedContract => {
                            CommandStoreErrorCode::InvalidOperation
                        }
                        C::StoreFailure
                        | C::CorruptState
                        | C::AmbientReadTransaction
                        | C::TransactionRequired => CommandStoreErrorCode::StoreFailure,
                        _ => CommandStoreErrorCode::OperationConflict,
                    },
                    error: error.error,
                }
            }),
            Self::MetadataSystemReplace { request } => catalog_metadata_store::replace_system(
                store,
                &request,
                command,
                required_grant(authority.metadata)?,
                required_grant(authority.metadata_initialization)?,
                required_grant(authority.metadata_schema)?,
                authority.metadata_origin,
                admitted_at_us,
            )
            .map_err(|error| CommandStoreError {
                code: if error.code
                    == catalog_metadata_store::MetadataStoreErrorCode::AuthorityDenied
                {
                    CommandStoreErrorCode::AuthorityDenied
                } else {
                    CommandStoreErrorCode::OperationConflict
                },
                error: error.error,
            }),
            Self::MetadataContributionReplace { request } => {
                catalog_metadata_store::replace_contribution(
                    store,
                    &request,
                    command,
                    required_grant(authority.metadata)?,
                    required_grant(authority.metadata_initialization)?,
                    required_grant(authority.metadata_schema)?,
                    required_grant(authority.tags)?,
                    authority.metadata_origin,
                    admitted_at_us,
                )
                .map_err(|error| {
                    use catalog_metadata_store::MetadataStoreErrorCode as C;
                    CommandStoreError {
                        code: match error.code {
                            C::AuthorityDenied => CommandStoreErrorCode::AuthorityDenied,
                            C::InvalidInput | C::UnsupportedContract => {
                                CommandStoreErrorCode::InvalidOperation
                            }
                            C::StoreFailure
                            | C::CorruptState
                            | C::AmbientReadTransaction
                            | C::TransactionRequired => CommandStoreErrorCode::StoreFailure,
                            _ => CommandStoreErrorCode::OperationConflict,
                        },
                        error: error.error,
                    }
                })
            }
            Self::MetadataInitializeAndPatch {
                subject,
                policies,
                namespace,
                record_id,
                edits_json,
                evidence_json,
            } => {
                let initialization_grant = required_grant(authority.metadata_initialization)?;
                let source_grant = required_grant(authority.metadata)?;
                // Execute existing owners under ONE outer receipt and transaction.
                let initialization = MetadataMutation {
                    contract_version: 1,
                    change: catalog_metadata_store::MetadataChange::Initialize {
                        subject: subject.clone(),
                        policies,
                    },
                };
                let initialization_command = CommandEnvelope::new(
                    command.contract_version(),
                    command.key().clone(),
                    command.admission_incarnation(),
                    command.expected_generation(),
                    &json!({"operation":"metadata","mutation":initialization}).to_string(),
                )?;
                let initialized = Self::Metadata {
                    mutation: initialization,
                }
                .apply(
                    store,
                    &initialization_command,
                    authority.with_metadata(initialization_grant),
                    admitted_at_us,
                )?;
                let fingerprint: String = store.db.query_row(
                    "SELECT schema_fingerprint FROM catalog_metadata_subjects WHERE item_id=?",
                    [&subject],
                    |r| r.get(0),
                )?;
                let mutation = MetadataMutation {
                    contract_version: 1,
                    change: catalog_metadata_store::MetadataChange::Patch {
                        subject,
                        namespace,
                        record_id,
                        schema_fingerprint: fingerprint,
                        expected_revision: 0,
                        edits_json,
                        evidence_json,
                    },
                };
                let generation: i64 = store.db.query_row(
                    "SELECT generation FROM catalog_state WHERE id=1",
                    [],
                    |r| r.get(0),
                )?;
                let inner = CommandEnvelope::new(
                    command.contract_version(),
                    command.key().clone(),
                    command.admission_incarnation(),
                    generation,
                    &json!({"operation":"metadata","mutation":mutation}).to_string(),
                )?;
                let patched = Self::Metadata { mutation }.apply(
                    store,
                    &inner,
                    authority.with_metadata(source_grant),
                    admitted_at_us,
                )?;
                Ok(
                    json!({"operation":"metadata_initialize_and_patch","initialization":initialized,"patch":patched}),
                )
            }
            Self::Metadata { mutation } => catalog_metadata_store::apply_metadata(
                &store.db,
                &mutation,
                command,
                required_grant(authority.metadata)?,
                admitted_at_us,
            )
            .map_err(|error| {
                use catalog_metadata_store::MetadataStoreErrorCode as C;
                CommandStoreError {
                    code: match error.code {
                        C::AuthorityDenied => CommandStoreErrorCode::AuthorityDenied,
                        C::Core(
                            catabolic_core::catalog_metadata::MetadataError::StaleSourceRevision {
                                ..
                            }
                            | catabolic_core::catalog_metadata::MetadataError::SourceKindMismatch
                            | catabolic_core::catalog_metadata::MetadataError::MultipleLocalSources
                            | catabolic_core::catalog_metadata::MetadataError::RevisionExhausted
                            | catabolic_core::catalog_metadata::MetadataError::SchemaMismatch,
                        ) => CommandStoreErrorCode::OperationConflict,
                        C::InvalidInput | C::UnsupportedContract | C::Core(_) => {
                            CommandStoreErrorCode::InvalidOperation
                        }
                        C::TransactionRequired
                        | C::AmbientReadTransaction
                        | C::CorruptState
                        | C::StoreFailure => CommandStoreErrorCode::StoreFailure,
                        _ => CommandStoreErrorCode::OperationConflict,
                    },
                    error: error.error,
                }
            }),
            Self::Tags { mutation } => {
                let context = catalog_tag_mutation::TagCommandContext {
                    catalog_id: command.key().catalog_id(),
                    host_namespace: command.key().host_namespace(),
                    operation_id: command.key().operation_id(),
                    expected_generation: command.expected_generation(),
                    expected_incarnation: command.admission_incarnation(),
                    recorded_at_us: admitted_at_us,
                };
                let result = catalog_tag_mutation::apply(
                    &store.db,
                    &context,
                    required_grant(authority.tags)?,
                    &mutation,
                )
                .map_err(|error| {
                    use catalog_tag_mutation::TagErrorCode as C;
                    CommandStoreError {
                        code: match error.code {
                            C::AuthorityDenied => CommandStoreErrorCode::AuthorityDenied,
                            C::InvalidInput | C::LimitExceeded => {
                                CommandStoreErrorCode::InvalidOperation
                            }
                            C::TransactionRequired | C::StoreFailure => {
                                CommandStoreErrorCode::StoreFailure
                            }
                            _ => CommandStoreErrorCode::OperationConflict,
                        },
                        error: error.error,
                    }
                })?;
                serde_json::to_value(result).map_err(|e| CommandStoreError {
                    code: CommandStoreErrorCode::StoreFailure,
                    error: Error(e.to_string()),
                })
            }
            Self::Pins { mutation } => {
                let result = catalog_pins::apply_pins(
                    store,
                    &mutation,
                    command,
                    required_grant(authority.pins)?,
                    admitted_at_us,
                )
                .map_err(|error| {
                    use catalog_pins::PinErrorCode as C;
                    CommandStoreError {
                        code: match error.code {
                            C::AuthorityDenied => CommandStoreErrorCode::AuthorityDenied,
                            C::InvalidInput | C::BudgetExceeded | C::UnsupportedSchema => {
                                CommandStoreErrorCode::InvalidOperation
                            }
                            C::WriterTransactionRequired | C::StoreFailure => {
                                CommandStoreErrorCode::StoreFailure
                            }
                            _ => CommandStoreErrorCode::OperationConflict,
                        },
                        error: error.error,
                    }
                })?;
                serde_json::to_value(result).map_err(|e| CommandStoreError {
                    code: CommandStoreErrorCode::StoreFailure,
                    error: Error(e.to_string()),
                })
            }
            Self::LogicalEdit { reviewed } => catalog_logical_edits::apply_logical_edit(
                store,
                &reviewed,
                command,
                required_grant(authority.logical_edits)?,
                authority.base.identity_authority,
                admitted_at_us,
            )
            .map_err(|error| {
                use catalog_logical_edits::LogicalEditErrorCode as C;
                CommandStoreError {
                    code: match error.code {
                        C::InvalidInput | C::Unsupported => CommandStoreErrorCode::InvalidOperation,
                        C::AuthorityDenied => CommandStoreErrorCode::AuthorityDenied,
                        C::Conflict => CommandStoreErrorCode::OperationConflict,
                        C::StoreFailure => CommandStoreErrorCode::StoreFailure,
                    },
                    error: error.error,
                }
            }),
            Self::ItemPut { item } => Ok(json!({"operation":"item_put", "item": put(&item)?})),
            Self::ItemCreateBatch { items } => {
                for item in &items {
                    let id = item["id"].as_str().expect("validated explicit create ID");
                    if store
                        .db
                        .prepare("SELECT 1 FROM items WHERE id=?")?
                        .exists([id])?
                    {
                        return fail(
                            CommandStoreErrorCode::OperationConflict,
                            "create-only item ID already exists",
                        );
                    }
                }
                let results: Result<Vec<_>> = items.iter().map(put).collect();
                Ok(json!({"operation":"item_create_batch", "items":results?}))
            }
            Self::ItemPutBatch { items } => {
                let results: Result<Vec<_>> = items.iter().map(put).collect();
                Ok(json!({"operation":"item_put_batch", "items": results?}))
            }
        }
    }
}

/// The same operation budget follows source work, derived flush, receipt
/// serialization and delivery. Charge before growing output buffers rather than
/// noticing a byte-limit failure only after an unbounded encode allocation.
pub(crate) fn budgeted_json<T: Serialize>(value: &T) -> crate::Result<String> {
    struct Writer {
        output: Vec<u8>,
        budget: Option<crate::operation_budget::OperationBudget>,
    }
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(budget) = &self.budget {
                budget
                    .charge(crate::operation_budget::BudgetCharge {
                        bytes: bytes.len(),
                        work: 1,
                        ..Default::default()
                    })
                    .map_err(|cause| {
                        std::io::Error::other(format!("operation budget: {cause:?}"))
                    })?;
            }
            self.output.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer {
        output: Vec::new(),
        budget: crate::operation_budget::OperationBudget::current(),
    };
    serde_json::to_writer(&mut writer, value)?;
    String::from_utf8(writer.output).map_err(|_| Error("invalid serialized UTF-8".into()))
}

pub(crate) fn charge_command_bytes(bytes: usize) -> crate::Result<()> {
    if let Some(budget) = crate::operation_budget::OperationBudget::current() {
        budget
            .charge(crate::operation_budget::BudgetCharge {
                bytes,
                work: 1,
                ..Default::default()
            })
            .map_err(|cause| Error(format!("operation budget: {cause:?}")))?;
    }
    Ok(())
}

pub(crate) fn prepare_delivery<T: Serialize>(value: &T) -> crate::Result<Value> {
    let encoded = budgeted_json(value)?;
    charge_command_bytes(encoded.len())?;
    Ok(serde_json::from_str(&encoded)?)
}

struct Receipt {
    stored: StoredReceipt,
    committed_generation: i64,
    retain_until_us: i64,
}
fn read_receipt(db: &Connection, key: &CommandKey) -> Result<Option<Receipt>> {
    let row = db.query_row(
        "SELECT canonicalization_version,payload_digest,result_state,result_json,committed_generation,retain_until_us,contract_version FROM catalog_command_receipts WHERE catalog_id=?1 AND host_namespace=?2 AND operation_id=?3",
        params![key.catalog_id(),key.host_namespace(),key.operation_id()],
        |row| {
            if let rusqlite::types::ValueRef::Text(body) = row.get_ref(3)? {
                // Copy, decode and canonicalize only after the shared budget
                // admits this stored receipt, including on read-only replay.
                charge_command_bytes(body.len().saturating_mul(3)).map_err(|cause| {
                    rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(cause))
                })?;
            }
            Ok((row.get::<_,u32>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,Option<String>>(3)?,row.get::<_,i64>(4)?,row.get::<_,i64>(5)?,row.get::<_,u32>(6)?))
        },
    ).optional()?;
    let Some((version, digest, state, body, committed_generation, retain_until_us, contract)) = row
    else {
        return Ok(None);
    };
    if contract != 1 || committed_generation < 0 || retain_until_us < 0 {
        return fail(
            CommandStoreErrorCode::InvalidReceipt,
            "invalid stored receipt metadata",
        );
    }
    let body = match state.as_str() {
        "full" => ReceiptBody::Full(CommandResult::new(body.as_deref().ok_or_else(|| {
            CommandStoreError {
                code: CommandStoreErrorCode::InvalidReceipt,
                error: Error("missing receipt body".into()),
            }
        })?)?),
        "expired" if body.is_none() => ReceiptBody::Expired,
        _ => {
            return fail(
                CommandStoreErrorCode::InvalidReceipt,
                "invalid stored receipt body state",
            );
        }
    };
    Ok(Some(Receipt {
        stored: StoredReceipt::new(key.clone(), PayloadDigest::new(version, digest)?, body),
        committed_generation,
        retain_until_us,
    }))
}

fn admit(store: &Store) -> Result<()> {
    if !store.db.is_autocommit() {
        return fail(
            CommandStoreErrorCode::AmbientTransaction,
            "durable command ownership requires a top-level transaction",
        );
    }
    if !is_supported_native_schema(store.schema_version) || store.catalog_incarnation.is_none() {
        return fail(
            CommandStoreErrorCode::UnsupportedSchema,
            "durable commands require explicit native schema admission",
        );
    }
    store.transaction_available()?;
    Ok(())
}
fn check_scope(store: &Store, key: &CommandKey, namespace: &str) -> Result<()> {
    if key.catalog_id() != store.database_id {
        return Err(CommandError::CatalogMismatch.into());
    }
    if key.host_namespace() != namespace {
        return Err(CommandError::HostNamespaceMismatch.into());
    }
    Ok(())
}
fn now_us() -> Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_micros()).ok())
        .ok_or_else(|| CommandStoreError {
            code: CommandStoreErrorCode::StoreFailure,
            error: Error("system clock outside receipt timestamp range".into()),
        })
}
fn valid_identifier(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn usage(db: &Connection) -> Result<ReceiptUsage> {
    let (receipts, receipt_bytes): (i64,i64) = db.query_row(
        "SELECT count(*),coalesce(sum(?1+length(CAST(catalog_id AS BLOB))+length(CAST(host_namespace AS BLOB))+length(CAST(operation_id AS BLOB))+length(CAST(payload_digest AS BLOB))+length(CAST(admission_incarnation AS BLOB))+coalesce(length(CAST(result_json AS BLOB)),0)),0) FROM catalog_command_receipts",
        [RECEIPT_OVERHEAD_BYTES as i64], |row| Ok((row.get(0)?,row.get(1)?)),
    )?;
    let (hold_records, hold_bytes): (i64,i64) = db.query_row(
        "SELECT count(*),coalesce(sum(?1+length(CAST(catalog_id AS BLOB))+length(CAST(host_namespace AS BLOB))+length(CAST(operation_id AS BLOB))+length(CAST(holder_namespace AS BLOB))+length(CAST(hold_id AS BLOB))),0) FROM catalog_receipt_holds",
        [HOLD_OVERHEAD_BYTES as i64], |row| Ok((row.get(0)?,row.get(1)?)),
    )?;
    let logical_bytes = receipt_bytes
        .checked_add(hold_bytes)
        .ok_or_else(|| CommandStoreError {
            code: CommandStoreErrorCode::StoreFailure,
            error: Error("receipt storage accounting overflow".into()),
        })?;
    Ok(ReceiptUsage {
        receipts: receipts as u64,
        hold_records: hold_records as u64,
        logical_bytes: logical_bytes as u64,
    })
}
fn fits_bytes(usage: ReceiptUsage, added: u64, policy: ReceiptPolicy) -> bool {
    usage
        .logical_bytes
        .checked_add(added)
        .is_some_and(|bytes| bytes <= policy.max_logical_bytes)
}
fn receipt_bytes(command: &CommandEnvelope, result_bytes: usize) -> u64 {
    RECEIPT_OVERHEAD_BYTES
        + (command.key().catalog_id().len()
            + command.key().host_namespace().len()
            + command.key().operation_id().len()
            + command.digest().as_str().len()
            + command.admission_incarnation().len()
            + result_bytes) as u64
}

/// Execute one validated envelope or return its original durable receipt. Payload
/// shapes are `{"operation":"item_put","item":{...}}` and
/// `{"operation":"item_put_batch","items":[{...}]}`, or
/// `{"operation":"identity","mutation":{...}}`. Identity operations require
/// separate trusted editorial authority. Item fields are the
/// existing curation `id`, `kind`, `metadata`, `identities` contract.
///
/// No `Committed` outcome escapes before COMMIT succeeds. If actual result
/// storage exceeds the remaining byte budget, the whole mutation rolls back.
/// Retrying that same envelope is safe, including after an unobserved reply.
/// Read an exact durable receipt without ever admitting a new command.
/// Absence is not proof of failure; callers must retain the original intent.
pub fn reconcile_receipt(
    store: &Store,
    command: &CommandEnvelope,
    authority: TrustedCommandAuthority<'_>,
) -> Result<Option<CommandOutcome>> {
    admit(store)?;
    reconcile_receipt_in(store, command, authority)
}

/// Receipt-only reconciliation for the privately retained offline bootstrap owner.
/// This path never asks the owner to reconstruct historical source evidence.
pub(crate) fn reconcile_offline_bootstrap_receipt(
    store: &Store,
    command: &CommandEnvelope,
    execution: &crate::migration::OfflineBootstrapExecution<'_, '_>,
) -> Result<Option<CommandOutcome>> {
    execution.check(store, command)?;
    let authority = TrustedCommandAuthority {
        allow_new_commands: false,
        ..execution.authority()
    };
    reconcile_receipt_in_with_control_workspace(store, command, authority, execution.control_workspace())
}

fn reconcile_receipt_in(
    store: &Store,
    command: &CommandEnvelope,
    authority: TrustedCommandAuthority<'_>,
) -> Result<Option<CommandOutcome>> {
    reconcile_receipt_in_with_control_workspace(store, command, authority, None)
}
fn reconcile_receipt_in_with_control_workspace(
    store: &Store, command: &CommandEnvelope, authority: TrustedCommandAuthority<'_>,
    control: Option<&crate::migration::BootstrapWorkspace<'_>>,
) -> Result<Option<CommandOutcome>> {
    check_scope(store, command.key(), authority.host_namespace)?;
    if authority.allow_new_commands {
        return fail(
            CommandStoreErrorCode::AuthorityDenied,
            "receipt-only authority required",
        );
    }
    store.transaction_detailed_with_control_workspace::<_, CommandStoreError>(|db| {
        let (incarnation, generation): (String, i64) = db.query_row(
            "SELECT incarnation,generation FROM catalog_state WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let receipt = read_receipt(db, command.key())?;
        let decision = decide_command(
            command,
            CommandSnapshot {
                catalog_id: &store.database_id,
                host_namespace: authority.host_namespace,
                incarnation: &incarnation,
                generation,
                receipt_storage: ReceiptStorage::Available,
            },
            receipt.as_ref().map(|r| &r.stored),
            authority.receipt_visibility,
        )?;
        Ok(match decision {
            CommandDecision::Replay { result } => {
                let receipt = receipt.as_ref().expect("replay requires receipt");
                Some(CommandOutcome::Replay {
                    result: result.clone(),
                    committed_generation: receipt.committed_generation,
                    retain_until_us: receipt.retain_until_us,
                })
            }
            CommandDecision::CommittedRedacted => Some(CommandOutcome::CommittedRedacted),
            CommandDecision::ReceiptExpired => Some(CommandOutcome::ReceiptExpired),
            CommandDecision::Conflict => Some(CommandOutcome::Conflict),
            CommandDecision::OutcomeUnknownAfterRestore => {
                Some(CommandOutcome::OutcomeUnknownAfterRestore)
            }
            CommandDecision::Execute | CommandDecision::StaleGeneration => None,
            CommandDecision::ReceiptStorageExhausted => {
                Some(CommandOutcome::ReceiptStorageExhausted)
            }
        })
    }, control)
}

pub fn execute(
    store: &Store,
    command: &CommandEnvelope,
    authority: TrustedCommandAuthority<'_>,
    policy: ReceiptPolicy,
) -> Result<CommandOutcome> {
    execute_with_authority(store, command, CatalogAuthority::new(authority), policy)
}

/// Execute typed schema34 curation with independently established host grants.
/// Shares the receipt-first transaction engine with [`execute`].
pub fn execute_with_authority(
    store: &Store,
    command: &CommandEnvelope,
    grants: CatalogAuthority<'_>,
    policy: ReceiptPolicy,
) -> Result<CommandOutcome> {
    execute_with_authority_inner(
        store,
        command,
        grants,
        policy,
        None,
        None,
        None,
        None,
        #[cfg(test)]
        None,
        #[cfg(test)]
        false,
    )
}

pub(crate) fn execute_scoped_metadata(
    store: &Store,
    command: &CommandEnvelope,
    grants: CatalogAuthority<'_>,
    policy: ReceiptPolicy,
    admission: &crate::catalog_write::ScopedMetadataAdmission<'_>,
) -> Result<Value> {
    let mut delivery = None;
    execute_with_authority_inner(
        store,
        command,
        grants,
        policy,
        Some(admission),
        Some(&mut delivery),
        None,
        None,
        #[cfg(test)]
        None,
        #[cfg(test)]
        false,
    )?;
    delivery.ok_or_else(|| Error("private command delivery was not prepared".into()).into())
}

pub(crate) fn reconcile_private_receipt(
    store: &Store,
    command: &CommandEnvelope,
    authority: TrustedCommandAuthority<'_>,
    policy: ReceiptPolicy,
) -> Result<Value> {
    let mut delivery = None;
    execute_with_authority_inner(
        store,
        command,
        CatalogAuthority::new(authority),
        policy,
        None,
        Some(&mut delivery),
        None,
        None,
        #[cfg(test)]
        None,
        #[cfg(test)]
        false,
    )?;
    delivery.ok_or_else(|| Error("private receipt delivery was not prepared".into()).into())
}

/// Fixed bootstrap execution under a nonconstructible retained offline owner.
/// Replay and expiry remain receipt-first and never invoke evidence reconstruction.
pub(crate) fn execute_offline_bootstrap(
    store: &Store,
    command: &CommandEnvelope,
    execution: &crate::migration::OfflineBootstrapExecution<'_, '_>,
    policy: ReceiptPolicy,
) -> Result<CommandOutcome> {
    execute_with_authority_inner(
        store,
        command,
        CatalogAuthority::new(execution.authority()),
        policy,
        None,
        None,
        Some(execution),
        None,
        #[cfg(test)]
        None,
        #[cfg(test)]
        false,
    )
}

/// Fixed source operation under genuine completed offline custody. This is
/// not normal41 dispatch and accepts no caller-provided mutation closure.
pub(crate) fn execute_completed_source(
    store: &Store,
    command: &CommandEnvelope,
    authority: TrustedCommandAuthority<'_>,
    policy: ReceiptPolicy,
    source: &mut crate::catalog_source_succession::SourceRenewalExecution<'_, '_>,
) -> Result<CommandOutcome> {
    execute_with_authority_inner(
        store,
        command,
        CatalogAuthority::new(authority),
        policy,
        None,
        None,
        None,
        Some(source),
        #[cfg(test)]
        None,
        #[cfg(test)]
        false,
    )
}

/// Test-only corridor for the independently admitted draft41 owner fixtures.
/// Production admission and operation dispatch remain closed. Replay, storage,
/// final permit checks, receipt insertion and COMMIT use the normal engine.
#[cfg(test)]
pub(crate) fn execute_candidate_for_test<F>(
    store: &Store,
    command: &CommandEnvelope,
    authority: TrustedCommandAuthority<'_>,
    policy: ReceiptPolicy,
    apply: F,
) -> Result<CommandOutcome>
where
    F: FnOnce(&Store, &CommandEnvelope, i64) -> crate::Result<Value>,
{
    let mut once = Some(apply);
    let mut callback = |store: &Store, command: &CommandEnvelope, time: i64| {
        let apply = once
            .take()
            .ok_or_else(|| Error("candidate owner invoked more than once".into()))?;
        apply(store, command, time)
    };
    execute_with_authority_inner(
        store,
        command,
        CatalogAuthority::new(authority),
        policy,
        None,
        None,
        None,
        None,
        Some(&mut callback),
        false,
    )
}

/// Exact candidate admission with the ordinary operation dispatcher, including
/// reserved import keys, validation/activation grants and actual receipt holds.
/// Unlike the callback hook this cannot substitute an arbitrary import effect.
#[cfg(test)]
pub(crate) fn execute_candidate_dispatch_for_test(
    store: &Store,
    command: &CommandEnvelope,
    grants: CatalogAuthority<'_>,
    policy: ReceiptPolicy,
) -> Result<CommandOutcome> {
    execute_with_authority_inner(
        store, command, grants, policy, None, None, None, None, None, true,
    )
}

fn execute_with_authority_inner<'workspace>(
    store: &Store,
    command: &CommandEnvelope,
    grants: CatalogAuthority<'_>,
    policy: ReceiptPolicy,
    admission: Option<&crate::catalog_write::ScopedMetadataAdmission<'_>>,
    mut delivery: Option<&mut Option<Value>>,
    offline: Option<&crate::migration::OfflineBootstrapExecution<'_, 'workspace>>,
    mut source: Option<&mut crate::catalog_source_succession::SourceRenewalExecution<'_, 'workspace>>,
    #[cfg(test)] mut candidate_apply: Option<
        &mut dyn FnMut(&Store, &CommandEnvelope, i64) -> crate::Result<Value>,
    >,
    #[cfg(test)] candidate_dispatch: bool,
) -> Result<CommandOutcome> {
    let control = offline.and_then(|execution| execution.control_workspace())
        .or_else(|| source.as_deref().and_then(|execution| execution.control_workspace()));
    let authority = grants.base;
    #[cfg(test)]
    let candidate = candidate_apply.is_some() || candidate_dispatch;
    #[cfg(not(test))]
    let candidate = false;
    if let Some(execution) = source.as_deref_mut() {
        if candidate || offline.is_some() || admission.is_some() || delivery.is_some() {
            return fail(
                CommandStoreErrorCode::AuthorityDenied,
                "source succession cannot combine admission corridors",
            );
        }
        execution.check(store, command)?;
    } else if let Some(execution) = offline {
        if candidate || admission.is_some() || delivery.is_some() {
            return fail(
                CommandStoreErrorCode::AuthorityDenied,
                "offline bootstrap cannot combine admission corridors",
            );
        }
        execution.check(store, command)?;
    } else if candidate {
        #[cfg(test)]
        {
            if !store.db.is_autocommit() || store.schema_version != 41 {
                return fail(
                    CommandStoreErrorCode::UnsupportedSchema,
                    "candidate41 requires top-level exact test admission",
                );
            }
            store.validate_candidate41_for_test()?;
            store.transaction_available()?;
            if candidate_apply.is_some()
                && command
                    .key()
                    .operation_id()
                    .starts_with(catalog_import::OPERATION_PREFIX)
            {
                return fail(
                    CommandStoreErrorCode::InvalidOperation,
                    "candidate owner cannot mint reserved import receipts",
                );
            }
        }
    } else {
        admit(store)?;
    }
    check_scope(store, command.key(), authority.host_namespace)?;
    let outcome = store.transaction_detailed_with_control_workspace::<_, ExecutionError>(|db| {
        if let Some(execution) = source.as_deref_mut() {
            execution.check_in(store, command)?;
        }
        let receipt = read_receipt(db,command.key())?;
        let (incarnation,generation): (String,i64) = db.query_row(
            "SELECT incarnation,generation FROM catalog_state WHERE id=1", [], |row| Ok((row.get(0)?,row.get(1)?)),
        )?;
        let decision = decide_command(command,CommandSnapshot {
            catalog_id: &store.database_id,
            host_namespace: authority.host_namespace,
            incarnation: &incarnation,
            generation,
            // Replay and stale decisions happen before storage is even counted.
            receipt_storage: ReceiptStorage::Available,
        },receipt.as_ref().map(|receipt| &receipt.stored),authority.receipt_visibility)?;
        let outcome = match decision {
            CommandDecision::Replay { result } => {
                let receipt = receipt.as_ref().expect("replay requires receipt");
                CommandOutcome::Replay { result: result.clone(),committed_generation: receipt.committed_generation,retain_until_us: receipt.retain_until_us }
            }
            CommandDecision::CommittedRedacted => CommandOutcome::CommittedRedacted,
            CommandDecision::ReceiptExpired => CommandOutcome::ReceiptExpired,
            CommandDecision::Conflict => CommandOutcome::Conflict,
            CommandDecision::OutcomeUnknownAfterRestore => CommandOutcome::OutcomeUnknownAfterRestore,
            CommandDecision::StaleGeneration => CommandOutcome::StaleGeneration,
            CommandDecision::ReceiptStorageExhausted => CommandOutcome::ReceiptStorageExhausted,
            CommandDecision::Execute => {
                if !authority.allow_new_commands {
                    return fail(CommandStoreErrorCode::AuthorityDenied, "current authority permits receipt reconciliation only").map_err(Into::into);
                }
                let usage = usage(db)?;
                if usage.receipts >= policy.max_receipts || !fits_bytes(usage,receipt_bytes(command,0),policy) {
                    return Err(ExecutionError::StorageExhausted);
                }
                let retention = i64::try_from(policy.retention.as_micros()).map_err(|_| CommandStoreError {
                    code: CommandStoreErrorCode::InvalidPolicy, error: Error("retention exceeds receipt timestamp range".into()),
                })?;
                if let Some(admission) = admission {
                    admission.check(store)?;
                }
                #[cfg(test)]
                let candidate_effect = if let Some(apply) = candidate_apply.as_deref_mut() {
                    Some(apply(store, command, now_us()?)?)
                } else { None };
                #[cfg(not(test))]
                let candidate_effect: Option<Value> = None;
                let (applied, import_hold) = if let Some(execution) = source.as_deref_mut() {
                    (execution.apply_in(store, command, now_us()?)?, None)
                } else if let Some(execution) = offline {
                    (execution.apply_in(store, command)?, None)
                } else if let Some(applied) = candidate_effect {
                    (applied, None)
                } else {
                let operation = Operation::parse(command.payload())?;
                // Reserve import IDs for the single bounded import owner. Do not
                // allow another operation to mint a misleading import checkpoint.
                if command.key().operation_id().starts_with(catalog_import::OPERATION_PREFIX) && !matches!(&operation, Operation::Import { .. }) {
                    return fail(CommandStoreErrorCode::InvalidOperation, "reserved import command key").map_err(Into::into);
                }
                let import_hold = match &operation {
                    Operation::Import { mutation } => Some(mutation.hold_id()),
                    _ => None,
                };
                if store.schema_version < 34 && matches!(&operation, Operation::Relationships { .. } | Operation::GeneratedOutputAdmit { .. } | Operation::FreshMediaDiscovery { .. } | Operation::Metadata { .. } | Operation::MetadataInitializeAndPatch { .. } | Operation::MetadataInitializeV2 { .. } | Operation::MetadataContributionReplace { .. } | Operation::MetadataSystemReplace { .. } | Operation::MetadataArtworkAttach { .. } | Operation::Tags { .. } | Operation::Pins { .. } | Operation::LogicalEdit { .. }) {
                    return fail(CommandStoreErrorCode::UnsupportedSchema, "typed curation requires native schema34").map_err(Into::into);
                }
                let applied = operation.apply(store,command,grants,now_us()?)?;
                (applied, import_hold)
                };
                let encoded = budgeted_json(&applied)?;
                // CommandResult retains decoded and canonical forms. Reserve
                // both before the bounded parser/canonicalizer allocates them.
                charge_command_bytes(encoded.len().saturating_mul(2))?;
                let result = CommandResult::new(&encoded)?;
                if store.schema_version >= 35 {
                    crate::catalog_search_index::flush_before_commit(db, &store.catalog_operation_budget()?)?;
                }
                if !fits_bytes(usage,receipt_bytes(command,result.canonical_json().len()),policy) {
                    // Returning Ok here would commit the unreceipted mutation.
                    return Err(ExecutionError::StorageExhausted);
                }
                store.validate_native_identity_with_control_workspace(control)?;
                if db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_mutation_permits)",[],|row| row.get::<_,bool>(0))? {
                    return fail(CommandStoreErrorCode::StoreFailure,"mutation permits remain before receipt commit").map_err(Into::into);
                }
                if store.schema_version >= 34 {
                    catalog_metadata_store::validate_metadata_permits_empty(db).map_err(|error| CommandStoreError { code: CommandStoreErrorCode::StoreFailure, error: error.error })?;
                    if db.prepare("SELECT 1 FROM catalog_logical_edit_permits LIMIT 1")?.exists([])? {
                        return fail(CommandStoreErrorCode::StoreFailure,"logical edit permits remain before receipt commit").map_err(Into::into);
                    }
                }
                let committed_generation: i64 = db.query_row("SELECT generation FROM catalog_state WHERE id=1",[],|row| row.get(0))?;
                let committed_at_us = now_us()?;
                let retain_until_us = committed_at_us.checked_add(retention).ok_or_else(|| CommandStoreError {
                    code: CommandStoreErrorCode::InvalidPolicy, error: Error("retention deadline exceeds timestamp range".into()),
                })?;
                db.execute("INSERT INTO catalog_command_receipts(catalog_id,host_namespace,operation_id,contract_version,canonicalization_version,payload_digest,admission_incarnation,expected_generation,committed_generation,committed_at_us,retain_until_us,result_state,result_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'full',?12)",params![
                    command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),
                    command.contract_version(),command.digest().canonicalization_version(),command.digest().as_str(),
                    command.admission_incarnation(),command.expected_generation(),committed_generation,
                    committed_at_us,retain_until_us,result.canonical_json(),
                ])?;
                // Staging data is the immutable receipt body. Its reconciliation
                // hold must commit atomically, with no expiry gap after staging.
                if let Some(hold_id) = import_hold {
                    db.execute("INSERT INTO catalog_receipt_holds(catalog_id,host_namespace,operation_id,holder_namespace,hold_id,acquired_at_us) VALUES(?1,?2,?3,?4,?5,?6)",params![command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id(),catalog_import::IMPORT_HOLDER_NAMESPACE,hold_id,committed_at_us])?;
                    let final_usage = self::usage(db)?;
                    if final_usage.hold_records > policy.max_hold_records || final_usage.logical_bytes > policy.max_logical_bytes {
                        return Err(ExecutionError::StorageExhausted);
                    }
                }
                if let Some(execution) = source.as_deref_mut() {
                    execution.terminal_in(store, command)?;
                }
                if authority.receipt_visibility == ReceiptVisibility::CommittedStatusOnly { CommandOutcome::CommittedRedacted } else { CommandOutcome::Committed { result: result.value().clone(),committed_generation,retain_until_us } }
            }
        };
        if let Some(slot) = delivery.as_deref_mut() {
            *slot = Some(prepare_delivery(&outcome)?);
        }
        Ok(outcome)
    }, control);
    match outcome {
        Ok(outcome) => Ok(outcome),
        Err(ExecutionError::StorageExhausted) => {
            let outcome = CommandOutcome::ReceiptStorageExhausted;
            if let Some(slot) = delivery {
                *slot = Some(prepare_delivery(&outcome)?);
            }
            Ok(outcome)
        }
        Err(ExecutionError::Store(error)) => Err(error),
    }
}

/// Host-authorized maintenance scope. Namespaces are privileged identity inputs,
/// not a caller-selected mechanism for acquiring another host's retention hold.
#[derive(Clone, Copy, Debug)]
pub struct TrustedReceiptMaintenance<'a> {
    pub host_namespace: &'a str,
    pub holder_namespace: &'a str,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldOutcome {
    Acquired,
    AlreadyHeld,
    Released,
    AlreadyReleased,
    ReceiptNotFound,
    ReceiptExpired,
    HoldNotFound,
    ReceiptStorageExhausted,
}
fn hold_identity(authority: TrustedReceiptMaintenance<'_>, hold_id: &str) -> Result<()> {
    if !valid_identifier(authority.holder_namespace) || !valid_identifier(hold_id) {
        return fail(
            CommandStoreErrorCode::InvalidHoldIdentifier,
            "hold identifiers must be bounded nonempty text without controls",
        );
    }
    Ok(())
}

pub fn acquire_hold(
    store: &Store,
    key: &CommandKey,
    authority: TrustedReceiptMaintenance<'_>,
    hold_id: &str,
    policy: ReceiptPolicy,
) -> Result<HoldOutcome> {
    admit(store)?;
    check_scope(store, key, authority.host_namespace)?;
    hold_identity(authority, hold_id)?;
    store.transaction_detailed(|db| {
        let released = db.query_row("SELECT released_at_us FROM catalog_receipt_holds WHERE catalog_id=?1 AND host_namespace=?2 AND operation_id=?3 AND holder_namespace=?4 AND hold_id=?5",
            params![key.catalog_id(),key.host_namespace(),key.operation_id(),authority.holder_namespace,hold_id],|row| row.get::<_,Option<i64>>(0)).optional()?;
        if let Some(released) = released {
            return Ok(if released.is_some() { HoldOutcome::AlreadyReleased } else { HoldOutcome::AlreadyHeld });
        }
        let state: Option<String> = db.query_row("SELECT result_state FROM catalog_command_receipts WHERE catalog_id=?1 AND host_namespace=?2 AND operation_id=?3",params![key.catalog_id(),key.host_namespace(),key.operation_id()],|row| row.get(0)).optional()?;
        match state.as_deref() {
            None => return Ok(HoldOutcome::ReceiptNotFound),
            Some("expired") => return Ok(HoldOutcome::ReceiptExpired),
            Some("full") => (),
            _ => return fail(CommandStoreErrorCode::InvalidReceipt,"invalid receipt state"),
        }
        let usage = usage(db)?;
        let added = HOLD_OVERHEAD_BYTES + (key.catalog_id().len()+key.host_namespace().len()+key.operation_id().len()+authority.holder_namespace.len()+hold_id.len()) as u64;
        if usage.hold_records >= policy.max_hold_records || !fits_bytes(usage,added,policy) {
            return Ok(HoldOutcome::ReceiptStorageExhausted);
        }
        db.execute("INSERT INTO catalog_receipt_holds(catalog_id,host_namespace,operation_id,holder_namespace,hold_id,acquired_at_us) VALUES(?1,?2,?3,?4,?5,?6)",params![key.catalog_id(),key.host_namespace(),key.operation_id(),authority.holder_namespace,hold_id,now_us()?])?;
        Ok(HoldOutcome::Acquired)
    })
}

pub fn release_hold(
    store: &Store,
    key: &CommandKey,
    authority: TrustedReceiptMaintenance<'_>,
    hold_id: &str,
) -> Result<HoldOutcome> {
    admit(store)?;
    check_scope(store, key, authority.host_namespace)?;
    hold_identity(authority, hold_id)?;
    store.transaction_detailed(|db| {
        let row = db.query_row("SELECT acquired_at_us,released_at_us FROM catalog_receipt_holds WHERE catalog_id=?1 AND host_namespace=?2 AND operation_id=?3 AND holder_namespace=?4 AND hold_id=?5",
            params![key.catalog_id(),key.host_namespace(),key.operation_id(),authority.holder_namespace,hold_id],|row| Ok((row.get::<_,i64>(0)?,row.get::<_,Option<i64>>(1)?))).optional()?;
        let Some((acquired,released)) = row else { return Ok(HoldOutcome::HoldNotFound); };
        if released.is_some() { return Ok(HoldOutcome::AlreadyReleased); }
        let now = now_us()?;
        if now < acquired { return fail(CommandStoreErrorCode::StoreFailure,"system clock predates hold acquisition"); }
        db.execute("UPDATE catalog_receipt_holds SET released_at_us=?6 WHERE catalog_id=?1 AND host_namespace=?2 AND operation_id=?3 AND holder_namespace=?4 AND hold_id=?5",params![key.catalog_id(),key.host_namespace(),key.operation_id(),authority.holder_namespace,hold_id,now])?;
        Ok(HoldOutcome::Released)
    })
}

/// Compact at most `limit` due, unheld receipts in the authorized host namespace.
/// The clock is read by this owner; clients cannot submit a future expiry time.
/// Receipt keys, digests and released hold history remain permanent.
pub fn compact_receipts(
    store: &Store,
    authority: TrustedReceiptMaintenance<'_>,
    limit: usize,
) -> Result<usize> {
    admit(store)?;
    compact_receipts_admitted(store, authority, limit)
}

#[cfg(test)]
pub(crate) fn compact_candidate_receipts_for_test(
    store: &Store,
    authority: TrustedReceiptMaintenance<'_>,
    limit: usize,
) -> Result<usize> {
    if !store.db.is_autocommit() {
        return fail(
            CommandStoreErrorCode::AmbientTransaction,
            "candidate maintenance requires a top-level transaction",
        );
    }
    store.validate_candidate41_for_test()?;
    store.transaction_available()?;
    compact_receipts_admitted(store, authority, limit)
}

/// Test-only expiry qualification under the exact retained offline owner.
/// Uses the ordinary clock, due/hold checks and tombstone mutation unchanged.
#[cfg(test)]
pub(crate) fn compact_offline_bootstrap_receipt_for_test(
    store: &Store,
    command: &CommandEnvelope,
    execution: &crate::migration::OfflineBootstrapExecution<'_, '_>,
) -> Result<usize> {
    execution.check(store, command)?;
    compact_receipts_admitted(
        store,
        TrustedReceiptMaintenance {
            host_namespace: execution.authority().host_namespace,
            holder_namespace: "offline-bootstrap-expiry-test",
        },
        1,
    )
}

#[cfg(test)]
pub(crate) fn compact_completed_source_receipt_for_test(
    store: &Store,
    command: &CommandEnvelope,
    admission: &crate::migration::CompletedSourceAdmission<'_, '_>,
) -> Result<usize> {
    admission.check(store, command)?;
    if !store.db.is_autocommit() {
        return fail(
            CommandStoreErrorCode::AmbientTransaction,
            "completed maintenance requires top-level owner",
        );
    }
    compact_receipts_admitted_source(
        store,
        TrustedReceiptMaintenance {
            host_namespace: command.key().host_namespace(),
            holder_namespace: "completed-source-expiry-test",
        },
        1,
        Some((admission, command)),
    )
}

fn compact_receipts_admitted(
    store: &Store,
    authority: TrustedReceiptMaintenance<'_>,
    limit: usize,
) -> Result<usize> {
    compact_receipts_admitted_source(store, authority, limit, None)
}
fn compact_receipts_admitted_source(
    store: &Store,
    authority: TrustedReceiptMaintenance<'_>,
    limit: usize,
    source: Option<(
        &crate::migration::CompletedSourceAdmission<'_, '_>,
        &CommandEnvelope,
    )>,
) -> Result<usize> {
    if !valid_identifier(authority.host_namespace) || !valid_identifier(authority.holder_namespace)
    {
        return fail(
            CommandStoreErrorCode::InvalidHoldIdentifier,
            "invalid maintenance namespace",
        );
    }
    if limit == 0 || limit > MAX_COMPACTION_BATCH {
        return fail(
            CommandStoreErrorCode::InvalidCompactionLimit,
            "compaction batch must contain 1..=1024 receipts",
        );
    }
    store.transaction_detailed(|db| {
        if let Some((admission, command)) = source {
            admission.check(store, command)?;
        }
        let now = now_us()?;
        Ok(db.execute("UPDATE catalog_command_receipts SET result_state='expired',result_json=NULL,expired_at_us=?1 WHERE rowid IN (SELECT r.rowid FROM catalog_command_receipts r WHERE r.catalog_id=?2 AND r.host_namespace=?3 AND r.result_state='full' AND r.retain_until_us<=?1 AND NOT EXISTS (SELECT 1 FROM catalog_receipt_holds h WHERE h.catalog_id=r.catalog_id AND h.host_namespace=r.host_namespace AND h.operation_id=r.operation_id AND h.released_at_us IS NULL) ORDER BY r.retain_until_us,r.operation_id LIMIT ?4)",params![now,store.database_id,authority.host_namespace,limit as i64])?)
    })
}

/// Privileged catalog-wide logical storage accounting under the same writer
/// admission as maintenance. It reveals no historical result bodies.
pub fn receipt_usage(store: &Store) -> Result<ReceiptUsage> {
    admit(store)?;
    store.transaction_detailed(usage)
}
