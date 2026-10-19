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
    tags: Option<&'a TagAuthority>,
    pins: Option<&'a PinAuthority>,
    logical_edits: Option<&'a LogicalEditAuthority>,
    imports: Option<&'a ImportAuthority>,
    import_activation: Option<&'a TrustedImportActivation>,
    import_typed: Option<&'a crate::catalog_import_typed::TypedAuthority>,
}
impl<'a> CatalogAuthority<'a> {
    pub fn new(base: TrustedCommandAuthority<'a>) -> Self {
        Self {
            base,
            metadata: None,
            tags: None,
            pins: None,
            logical_edits: None,
            imports: None,
            import_activation: None,
            import_typed: None,
        }
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
    ItemPut { item: Value },
    ItemPutBatch { items: Vec<Value> },
    Identity { mutation: IdentityMutation },
    Metadata { mutation: MetadataMutation },
    Tags { mutation: TagMutation },
    Pins { mutation: PinMutation },
    LogicalEdit { reviewed: ReviewedLogicalEdit },
    Import { mutation: ImportMutation },
}
impl Operation {
    fn parse(payload: &Value) -> Result<Self> {
        let operation: Self =
            serde_json::from_value(payload.clone()).map_err(|error| CommandStoreError {
                code: CommandStoreErrorCode::InvalidOperation,
                error: Error(error.to_string()),
            })?;
        let items = match &operation {
            Self::Import { .. }
            | Self::Identity { .. }
            | Self::Metadata { .. }
            | Self::Tags { .. }
            | Self::Pins { .. }
            | Self::LogicalEdit { .. } => return Ok(operation),
            Self::ItemPut { item } => std::slice::from_ref(item),
            Self::ItemPutBatch { items }
                if !items.is_empty() && items.len() <= MAX_ITEM_PUT_BATCH =>
            {
                items
            }
            Self::ItemPutBatch { .. } => {
                return fail(
                    CommandStoreErrorCode::InvalidOperation,
                    "item-put batch must contain 1..=128 items",
                );
            }
        };
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
    execute_with_authority_inner(store, command, grants, policy, None, None)
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
    )?;
    delivery.ok_or_else(|| Error("private receipt delivery was not prepared".into()).into())
}

fn execute_with_authority_inner(
    store: &Store,
    command: &CommandEnvelope,
    grants: CatalogAuthority<'_>,
    policy: ReceiptPolicy,
    admission: Option<&crate::catalog_write::ScopedMetadataAdmission<'_>>,
    mut delivery: Option<&mut Option<Value>>,
) -> Result<CommandOutcome> {
    let authority = grants.base;
    admit(store)?;
    check_scope(store, command.key(), authority.host_namespace)?;
    let outcome = store.transaction_detailed::<_, ExecutionError>(|db| {
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
                if store.schema_version < 34 && matches!(&operation, Operation::Metadata { .. } | Operation::Tags { .. } | Operation::Pins { .. } | Operation::LogicalEdit { .. }) {
                    return fail(CommandStoreErrorCode::UnsupportedSchema, "typed curation requires native schema34").map_err(Into::into);
                }
                let applied = operation.apply(store,command,grants,now_us()?)?;
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
                store.validate_native_identity()?;
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
                if authority.receipt_visibility == ReceiptVisibility::CommittedStatusOnly { CommandOutcome::CommittedRedacted } else { CommandOutcome::Committed { result: result.value().clone(),committed_generation,retain_until_us } }
            }
        };
        if let Some(slot) = delivery.as_deref_mut() {
            *slot = Some(prepare_delivery(&outcome)?);
        }
        Ok(outcome)
    });
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
