// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Version 1 durable catalog command envelopes and replay decisions. No I/O.
//!
//! The owning store MUST validate current authority before calling this module,
//! and supply its receipt lookup, visibility, incarnation, generation and storage
//! admission from one writer transaction. Visibility is a trusted owner decision,
//! never a client authorization flag. An Execute decision is only permission to
//! continue that transaction's validation: the mutation, projections, generation
//! and immutable receipt must commit atomically. It is not a durable result.
//!
//! Receipt lookup is by the complete key, without an incarnation filter. Known
//! receipts precede stale preconditions and restore fencing. Absent receipts from
//! an old incarnation cannot prove that the original command did not commit.
//! Compaction may remove a full result only after its retention deadline and all
//! holds have ended; its key and digest remain for the catalog lifetime. This
//! module neither schedules compaction nor provides a way to recycle a key.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const COMMAND_CONTRACT_VERSION: u32 = 1;
pub const COMMAND_CANONICALIZATION_VERSION: u32 = 1;
pub const MAX_COMMAND_IDENTIFIER_BYTES: usize = 256;
pub const MAX_COMMAND_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const MAX_COMMAND_RESULT_BYTES: usize = 4 * 1024 * 1024;
/// Payload plus four maximally escaped identifiers and fixed envelope fields.
pub const MAX_COMMAND_ENVELOPE_BYTES: usize =
    MAX_COMMAND_PAYLOAD_BYTES + 4 * MAX_COMMAND_IDENTIFIER_BYTES * 6 + 512;
pub const MAX_COMMAND_JSON_DEPTH: usize = 64;
pub const MAX_COMMAND_JSON_NODES: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandError {
    UnsupportedContract,
    UnsupportedCanonicalization,
    InvalidCatalogId,
    InvalidHostNamespace,
    InvalidOperationId,
    InvalidIncarnation,
    InvalidGeneration,
    InvalidJson,
    NonFiniteJson,
    PayloadBudget,
    ResultBudget,
    JsonDepthBudget,
    JsonNodeBudget,
    InvalidDigest,
    CatalogMismatch,
    HostNamespaceMismatch,
    ReceiptKeyMismatch,
}

impl CommandError {
    /// Stable machine code. Incidental diagnostics are not the error contract.
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedContract => "catalog_command_unsupported_contract",
            Self::UnsupportedCanonicalization => "catalog_command_unsupported_canonicalization",
            Self::InvalidCatalogId => "catalog_command_invalid_catalog_id",
            Self::InvalidHostNamespace => "catalog_command_invalid_host_namespace",
            Self::InvalidOperationId => "catalog_command_invalid_operation_id",
            Self::InvalidIncarnation => "catalog_command_invalid_incarnation",
            Self::InvalidGeneration => "catalog_command_invalid_generation",
            Self::InvalidJson => "catalog_command_invalid_json",
            Self::NonFiniteJson => "catalog_command_nonfinite_json",
            Self::PayloadBudget => "catalog_command_payload_budget",
            Self::ResultBudget => "catalog_command_result_budget",
            Self::JsonDepthBudget => "catalog_command_json_depth_budget",
            Self::JsonNodeBudget => "catalog_command_json_node_budget",
            Self::InvalidDigest => "catalog_command_invalid_digest",
            Self::CatalogMismatch => "catalog_command_catalog_mismatch",
            Self::HostNamespaceMismatch => "catalog_command_host_namespace_mismatch",
            Self::ReceiptKeyMismatch => "catalog_command_receipt_key_mismatch",
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for CommandError {}

pub type Result<T> = std::result::Result<T, CommandError>;

/// Exact, case-sensitive opaque identifiers; no trimming or Unicode normalization
/// changes a receipt key. Bounds apply to UTF-8 bytes, not character counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CommandKeyFields")]
pub struct CommandKey {
    catalog_id: String,
    host_namespace: String,
    operation_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandKeyFields {
    catalog_id: String,
    host_namespace: String,
    operation_id: String,
}

impl TryFrom<CommandKeyFields> for CommandKey {
    type Error = CommandError;

    fn try_from(value: CommandKeyFields) -> Result<Self> {
        Self::new(value.catalog_id, value.host_namespace, value.operation_id)
    }
}

impl CommandKey {
    pub fn new(
        catalog_id: impl Into<String>,
        host_namespace: impl Into<String>,
        operation_id: impl Into<String>,
    ) -> Result<Self> {
        let key = Self {
            catalog_id: catalog_id.into(),
            host_namespace: host_namespace.into(),
            operation_id: operation_id.into(),
        };
        identifier(&key.catalog_id, CommandError::InvalidCatalogId)?;
        identifier(&key.host_namespace, CommandError::InvalidHostNamespace)?;
        identifier(&key.operation_id, CommandError::InvalidOperationId)?;
        Ok(key)
    }

    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }

    pub fn host_namespace(&self) -> &str {
        &self.host_namespace
    }

    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
}

/// The codec version is stored with every full receipt and permanent tombstone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PayloadDigestFields")]
pub struct PayloadDigest {
    canonicalization_version: u32,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PayloadDigestFields {
    canonicalization_version: u32,
    sha256: String,
}

impl TryFrom<PayloadDigestFields> for PayloadDigest {
    type Error = CommandError;

    fn try_from(value: PayloadDigestFields) -> Result<Self> {
        Self::new(value.canonicalization_version, value.sha256)
    }
}

impl PayloadDigest {
    pub fn new(canonicalization_version: u32, sha256: impl Into<String>) -> Result<Self> {
        if canonicalization_version != COMMAND_CANONICALIZATION_VERSION {
            return Err(CommandError::UnsupportedCanonicalization);
        }
        let sha256 = sha256.into();
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CommandError::InvalidDigest);
        }
        Ok(Self {
            canonicalization_version,
            sha256,
        })
    }

    pub const fn canonicalization_version(&self) -> u32 {
        self.canonicalization_version
    }

    pub fn as_str(&self) -> &str {
        &self.sha256
    }
}

/// Immutable validated input. Persist this original envelope in the host intent;
/// a retry must retain its original incarnation and preconditions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandEnvelope {
    contract_version: u32,
    key: CommandKey,
    admission_incarnation: String,
    expected_generation: i64,
    payload: Value,
    canonical_payload: String,
    digest: PayloadDigest,
}

impl CommandEnvelope {
    /// Parse raw JSON here, before any adapter can lose duplicate object keys.
    /// Operation-specific payload validation still belongs to its owning module.
    ///
    /// Canonicalization v1 uses Catabolic's sorted-key compact UTF-8 encoding,
    /// including its existing integer/float/negative-zero rules. It is not JCS
    /// and does not claim that mathematically equal JSON numbers are identical.
    /// The digest covers all semantic request fields, including the full key.
    pub fn new(
        contract_version: u32,
        key: CommandKey,
        admission_incarnation: impl Into<String>,
        expected_generation: i64,
        payload_json: &str,
    ) -> Result<Self> {
        if contract_version != COMMAND_CONTRACT_VERSION {
            return Err(CommandError::UnsupportedContract);
        }
        let admission_incarnation = admission_incarnation.into();
        identifier(&admission_incarnation, CommandError::InvalidIncarnation)?;
        generation(expected_generation)?;
        let (payload, _) = finite_json(
            payload_json,
            MAX_COMMAND_PAYLOAD_BYTES,
            CommandError::PayloadBudget,
        )?;
        let canonical_payload = crate::encode_with(
            &json!({
                "contract_version": contract_version,
                "canonicalization_version": COMMAND_CANONICALIZATION_VERSION,
                "key": key,
                "admission_incarnation": admission_incarnation,
                "expected_generation": expected_generation,
                "payload": payload,
            }),
            false,
            true,
        );
        if canonical_payload.len() > MAX_COMMAND_ENVELOPE_BYTES {
            return Err(CommandError::PayloadBudget);
        }
        let digest = PayloadDigest {
            canonicalization_version: COMMAND_CANONICALIZATION_VERSION,
            sha256: crate::sha256(canonical_payload.as_bytes()),
        };
        Ok(Self {
            contract_version,
            key,
            admission_incarnation,
            expected_generation,
            payload,
            canonical_payload,
            digest,
        })
    }

    pub const fn contract_version(&self) -> u32 {
        self.contract_version
    }

    pub fn key(&self) -> &CommandKey {
        &self.key
    }

    pub fn admission_incarnation(&self) -> &str {
        &self.admission_incarnation
    }

    pub const fn expected_generation(&self) -> i64 {
        self.expected_generation
    }

    /// Decoded canonical payload. Canonically identical requests expose the same
    /// number tokens to the operation owner, rather than distinct raw spellings.
    pub fn payload(&self) -> &Value {
        &self.payload
    }

    /// Canonical bytes of the complete semantic envelope, not only its body.
    pub fn canonical_payload(&self) -> &str {
        &self.canonical_payload
    }

    pub fn digest(&self) -> &PayloadDigest {
        &self.digest
    }
}

/// Checked immutable result body reconstructed from the owning store's receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    value: Value,
    canonical_json: String,
}

impl CommandResult {
    pub fn new(result_json: &str) -> Result<Self> {
        let (value, canonical_json) = finite_json(
            result_json,
            MAX_COMMAND_RESULT_BYTES,
            CommandError::ResultBudget,
        )?;
        Ok(Self {
            value,
            canonical_json,
        })
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiptBody {
    Full(CommandResult),
    /// Permanent committed tombstone. The operation key must never be reused.
    Expired,
}

/// Only committed receipts belong here. An in-flight host intent is not a
/// receipt, and must never be represented by a fabricated empty result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredReceipt {
    key: CommandKey,
    digest: PayloadDigest,
    body: ReceiptBody,
}

impl StoredReceipt {
    pub fn new(key: CommandKey, digest: PayloadDigest, body: ReceiptBody) -> Self {
        Self { key, digest, body }
    }

    pub fn key(&self) -> &CommandKey {
        &self.key
    }

    pub fn digest(&self) -> &PayloadDigest {
        &self.digest
    }

    pub fn body(&self) -> &ReceiptBody {
        &self.body
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptVisibility {
    FullResult,
    /// Current authority permits reconciliation of status, but not the body.
    CommittedStatusOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptStorage {
    Available,
    /// The store refuses new commands rather than evicting permanent keys.
    Exhausted,
}

/// Trusted, same-transaction owner observations. These are not host-supplied
/// authorization claims. Validate current authority before constructing a call.
#[derive(Clone, Copy, Debug)]
pub struct CommandSnapshot<'a> {
    pub catalog_id: &'a str,
    pub host_namespace: &'a str,
    pub incarnation: &'a str,
    pub generation: i64,
    pub receipt_storage: ReceiptStorage,
}

/// A replay lends the original body; no recomputed IDs, revisions or mappings.
/// Redacted and expired states deliberately contain no affected-set information.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CommandDecision<'a> {
    Execute,
    Replay { result: &'a Value },
    CommittedRedacted,
    ReceiptExpired,
    Conflict,
    OutcomeUnknownAfterRestore,
    StaleGeneration,
    ReceiptStorageExhausted,
}

impl CommandDecision<'_> {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Replay { .. } => "replay",
            Self::CommittedRedacted => "committed_redacted",
            Self::ReceiptExpired => "receipt_expired",
            Self::Conflict => "conflict",
            Self::OutcomeUnknownAfterRestore => "outcome_unknown_after_restore",
            Self::StaleGeneration => "stale_generation",
            Self::ReceiptStorageExhausted => "receipt_storage_exhausted",
        }
    }

    /// Only these outcomes establish that this exact command already committed.
    pub const fn is_committed(&self) -> bool {
        matches!(
            self,
            Self::Replay { .. } | Self::CommittedRedacted | Self::ReceiptExpired
        )
    }
}

/// Decide from one transaction's observations, after current authorization.
///
/// Receipt consistency and payload conflict precede result visibility. Known
/// identical receipts are replayed even after generation/incarnation changes or
/// storage exhaustion. An absent old-incarnation command is never re-executed.
/// Execute does not waive entity/relation/membership validation or authorize any
/// network/filesystem effect; the owning store must enforce those constraints.
pub fn decide_command<'a>(
    command: &CommandEnvelope,
    snapshot: CommandSnapshot<'_>,
    receipt: Option<&'a StoredReceipt>,
    visibility: ReceiptVisibility,
) -> Result<CommandDecision<'a>> {
    identifier(snapshot.catalog_id, CommandError::InvalidCatalogId)?;
    identifier(snapshot.host_namespace, CommandError::InvalidHostNamespace)?;
    identifier(snapshot.incarnation, CommandError::InvalidIncarnation)?;
    generation(snapshot.generation)?;
    if command.key.catalog_id != snapshot.catalog_id {
        return Err(CommandError::CatalogMismatch);
    }
    if command.key.host_namespace != snapshot.host_namespace {
        return Err(CommandError::HostNamespaceMismatch);
    }
    if let Some(receipt) = receipt {
        if receipt.key != command.key {
            return Err(CommandError::ReceiptKeyMismatch);
        }
        if receipt.digest != command.digest {
            return Ok(CommandDecision::Conflict);
        }
        return Ok(match (&receipt.body, visibility) {
            (ReceiptBody::Expired, _) => CommandDecision::ReceiptExpired,
            (ReceiptBody::Full(_), ReceiptVisibility::CommittedStatusOnly) => {
                CommandDecision::CommittedRedacted
            }
            (ReceiptBody::Full(result), ReceiptVisibility::FullResult) => CommandDecision::Replay {
                result: result.value(),
            },
        });
    }
    if command.admission_incarnation != snapshot.incarnation {
        return Ok(CommandDecision::OutcomeUnknownAfterRestore);
    }
    if command.expected_generation != snapshot.generation {
        return Ok(CommandDecision::StaleGeneration);
    }
    if snapshot.receipt_storage == ReceiptStorage::Exhausted {
        return Ok(CommandDecision::ReceiptStorageExhausted);
    }
    Ok(CommandDecision::Execute)
}

fn identifier(value: &str, error: CommandError) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > MAX_COMMAND_IDENTIFIER_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(error);
    }
    Ok(())
}

fn generation(value: i64) -> Result<()> {
    if value < 0 {
        return Err(CommandError::InvalidGeneration);
    }
    Ok(())
}

fn finite_json(raw: &str, max_bytes: usize, budget: CommandError) -> Result<(Value, String)> {
    if raw.len() > max_bytes {
        return Err(budget);
    }
    // Reject extreme nesting before entering the shared recursive JSON parser.
    // This scan only enforces a budget; the parser still validates all grammar.
    check_nesting(raw)?;
    let value = crate::json::parse(raw, true).map_err(|_| CommandError::InvalidJson)?;
    check_nodes(&value)?;
    crate::validate_finite(&value).map_err(|_| CommandError::NonFiniteJson)?;
    let canonical = crate::encode_with(&value, false, true);
    if canonical.len() > max_bytes {
        return Err(budget);
    }
    // Operation owners must see exactly the semantics covered by the digest.
    // The shared codec normalizes float tokens; exposing the raw Number spelling
    // would let canonical-equivalent inputs mean different things downstream.
    let canonical_value =
        crate::json::parse(&canonical, true).map_err(|_| CommandError::InvalidJson)?;
    Ok((canonical_value, canonical))
}

fn check_nesting(raw: &str) -> Result<()> {
    let mut in_string = false;
    let mut escaped = false;
    let mut depth: usize = 0;
    for byte in raw.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > MAX_COMMAND_JSON_DEPTH {
                        return Err(CommandError::JsonDepthBudget);
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    Ok(())
}

fn check_nodes(value: &Value) -> Result<()> {
    let mut remaining = MAX_COMMAND_JSON_NODES;
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        remaining = remaining
            .checked_sub(1)
            .ok_or(CommandError::JsonNodeBudget)?;
        match value {
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> CommandKey {
        CommandKey::new("catalog", "host", "operation").unwrap()
    }

    fn command(payload: &str) -> CommandEnvelope {
        CommandEnvelope::new(1, key(), "incarnation-1", 7, payload).unwrap()
    }

    fn snapshot() -> CommandSnapshot<'static> {
        CommandSnapshot {
            catalog_id: "catalog",
            host_namespace: "host",
            incarnation: "incarnation-1",
            generation: 7,
            receipt_storage: ReceiptStorage::Available,
        }
    }

    fn receipt(command: &CommandEnvelope) -> StoredReceipt {
        StoredReceipt::new(
            command.key().clone(),
            command.digest().clone(),
            ReceiptBody::Full(
                CommandResult::new(
                    r#"{"item_id":"original-item","revision":8,"mappings":[["old","new"]]}"#,
                )
                .unwrap(),
            ),
        )
    }

    fn decide<'a>(
        command: &CommandEnvelope,
        snapshot: CommandSnapshot<'_>,
        receipt: Option<&'a StoredReceipt>,
    ) -> Result<CommandDecision<'a>> {
        decide_command(command, snapshot, receipt, ReceiptVisibility::FullResult)
    }

    #[test]
    fn canonical_envelope_has_frozen_bytes_and_digest() {
        let command = command(r#"{ "z": "Caf\u00e9", "a": [1, 1.000, -0.00] }"#);
        assert_eq!(
            command.canonical_payload(),
            r#"{"admission_incarnation":"incarnation-1","canonicalization_version":1,"contract_version":1,"expected_generation":7,"key":{"catalog_id":"catalog","host_namespace":"host","operation_id":"operation"},"payload":{"a":[1,1.0,-0.0],"z":"Café"}}"#,
        );
        assert_eq!(
            command.digest().as_str(),
            "23ea2093d0fb44b518e5a7f4a13bbc98daf0db885bd55fd54d0fddb53379f33b",
        );
        assert_eq!(command.contract_version(), COMMAND_CONTRACT_VERSION);
        assert_eq!(
            command.digest().canonicalization_version(),
            COMMAND_CANONICALIZATION_VERSION,
        );
        assert_eq!(command.admission_incarnation(), "incarnation-1");
        assert_eq!(command.expected_generation(), 7);
        assert_eq!(command.key().catalog_id(), "catalog");
        assert_eq!(command.key().host_namespace(), "host");
        assert_eq!(command.key().operation_id(), "operation");
    }

    #[test]
    fn canonicalization_uses_existing_number_and_object_semantics() {
        let first = command(r#"{"z":"Café","x":1e2,"a":1208925819614629174706176}"#);
        let second =
            command(r#" { "a": 1208925819614629174706176, "x": 100.00, "z": "Caf\u00e9" } "#);
        assert_eq!(first.canonical_payload(), second.canonical_payload());
        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.payload(), second.payload());
        assert_eq!(first.payload()["x"].to_string(), "100.0");
        assert_eq!(
            CommandResult::new("1e2").unwrap(),
            CommandResult::new("100.00").unwrap(),
        );
        assert_eq!(command("-0").digest(), command("0").digest());
        for (left, right) in [
            ("1", "1.0"),
            ("0.0", "-0.0"),
            ("[1,2]", "[2,1]"),
            ("1208925819614629174706176", "1208925819614629174706177"),
            (r#""é""#, r#""e\u0301""#),
        ] {
            assert_ne!(command(left).digest(), command(right).digest());
        }
        let sentinel = command(r#"{"$serde_json::private::Number":"1e400"}"#);
        assert!(sentinel.payload().is_object());
        assert_eq!(sentinel.payload()["$serde_json::private::Number"], "1e400");
        let result_sentinel =
            CommandResult::new(r#"{"$serde_json::private::Number":"1e400"}"#).unwrap();
        assert!(result_sentinel.value().is_object());
        assert_eq!(
            result_sentinel.canonical_json(),
            r#"{"$serde_json::private::Number":"1e400"}"#
        );
    }

    #[test]
    fn digest_binds_every_semantic_envelope_field() {
        let original = command(r#"{"operation":"merge","survivor":"one"}"#);
        let variants = [
            CommandEnvelope::new(
                1,
                key(),
                "incarnation-2",
                7,
                original.payload().to_string().as_str(),
            ),
            CommandEnvelope::new(
                1,
                key(),
                "incarnation-1",
                8,
                original.payload().to_string().as_str(),
            ),
            CommandEnvelope::new(
                1,
                key(),
                "incarnation-1",
                7,
                r#"{"operation":"split","survivor":"one"}"#,
            ),
            CommandEnvelope::new(
                1,
                CommandKey::new("other", "host", "operation").unwrap(),
                "incarnation-1",
                7,
                original.payload().to_string().as_str(),
            ),
            CommandEnvelope::new(
                1,
                CommandKey::new("catalog", "other", "operation").unwrap(),
                "incarnation-1",
                7,
                original.payload().to_string().as_str(),
            ),
            CommandEnvelope::new(
                1,
                CommandKey::new("catalog", "host", "other").unwrap(),
                "incarnation-1",
                7,
                original.payload().to_string().as_str(),
            ),
        ];
        for variant in variants {
            assert_ne!(original.digest(), variant.unwrap().digest());
        }
        assert_eq!(
            CommandEnvelope::new(2, key(), "incarnation-1", 7, "null"),
            Err(CommandError::UnsupportedContract),
        );
    }

    #[test]
    fn identifiers_are_bounded_exact_utf8_and_checked_on_deserialization() {
        for invalid in ["", " \t", "nul\0", "line\n", "delete\u{7f}", "next\u{85}"] {
            assert_eq!(
                CommandKey::new(invalid, "host", "operation"),
                Err(CommandError::InvalidCatalogId),
            );
            assert_eq!(
                CommandKey::new("catalog", invalid, "operation"),
                Err(CommandError::InvalidHostNamespace),
            );
            assert_eq!(
                CommandKey::new("catalog", "host", invalid),
                Err(CommandError::InvalidOperationId),
            );
            assert_eq!(
                CommandEnvelope::new(1, key(), invalid, 0, "null"),
                Err(CommandError::InvalidIncarnation),
            );
        }
        let max = "é".repeat(MAX_COMMAND_IDENTIFIER_BYTES / 2);
        assert!(CommandKey::new(&max, &max, &max).is_ok());
        assert_eq!(
            CommandKey::new(format!("{max}x"), "host", "operation"),
            Err(CommandError::InvalidCatalogId),
        );
        assert_ne!(
            CommandKey::new("catalog", " host ", "operation").unwrap(),
            key(),
        );
        let serialized = serde_json::to_string(&key()).unwrap();
        assert_eq!(
            serde_json::from_str::<CommandKey>(&serialized).unwrap(),
            key()
        );
        for raw in [
            r#"{"catalog_id":"catalog","host_namespace":"","operation_id":"operation"}"#,
            r#"{"catalog_id":"catalog","host_namespace":"host","operation_id":"operation","extra":1}"#,
            r#"{"catalog_id":"catalog","host_namespace":"host"}"#,
        ] {
            assert!(serde_json::from_str::<CommandKey>(raw).is_err());
        }
    }

    #[test]
    fn rejects_invalid_versions_digests_generations_and_json() {
        let digest = command("null").digest().clone();
        assert_eq!(
            serde_json::from_str::<PayloadDigest>(&serde_json::to_string(&digest).unwrap())
                .unwrap(),
            digest,
        );
        assert_eq!(
            PayloadDigest::new(0, digest.as_str()),
            Err(CommandError::UnsupportedCanonicalization)
        );
        for invalid in [
            "0".repeat(63),
            "0".repeat(65),
            "g".repeat(64),
            "A".repeat(64),
        ] {
            assert_eq!(
                PayloadDigest::new(1, invalid),
                Err(CommandError::InvalidDigest)
            );
        }
        assert_eq!(
            CommandEnvelope::new(1, key(), "incarnation-1", -1, "null"),
            Err(CommandError::InvalidGeneration)
        );
        assert!(CommandEnvelope::new(1, key(), "incarnation-1", i64::MAX, "null").is_ok());
        for raw in [
            "NaN",
            "Infinity",
            "-Infinity",
            "[1,]",
            "{} trailing",
            r#"{"x":1,"\u0078":2}"#,
            r#"{"x":{"y":1,"y":2}}"#,
        ] {
            assert_eq!(
                CommandEnvelope::new(1, key(), "incarnation-1", 7, raw),
                Err(CommandError::InvalidJson),
                "{raw}"
            );
            assert_eq!(CommandResult::new(raw), Err(CommandError::InvalidJson));
        }
        for raw in ["1e400", r#"{"nested":[-1e400]}"#] {
            assert_eq!(
                CommandEnvelope::new(1, key(), "incarnation-1", 7, raw),
                Err(CommandError::NonFiniteJson)
            );
            assert_eq!(CommandResult::new(raw), Err(CommandError::NonFiniteJson));
        }
    }

    #[test]
    fn json_byte_depth_and_node_limits_are_explicit() {
        let max_string = format!("\"{}\"", "x".repeat(MAX_COMMAND_PAYLOAD_BYTES - 2));
        assert!(CommandEnvelope::new(1, key(), "incarnation-1", 7, &max_string).is_ok());
        assert_eq!(
            CommandEnvelope::new(1, key(), "incarnation-1", 7, &format!("{max_string} ")),
            Err(CommandError::PayloadBudget)
        );
        let max_result = format!("\"{}\"", "x".repeat(MAX_COMMAND_RESULT_BYTES - 2));
        assert!(CommandResult::new(&max_result).is_ok());
        assert_eq!(
            CommandResult::new(&format!("{max_result} ")),
            Err(CommandError::ResultBudget)
        );
        let nested = |depth| format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        assert!(CommandResult::new(&nested(MAX_COMMAND_JSON_DEPTH)).is_ok());
        assert_eq!(
            CommandResult::new(&nested(MAX_COMMAND_JSON_DEPTH + 1)),
            Err(CommandError::JsonDepthBudget)
        );
        let nodes = |count| format!("[{}]", vec!["0"; count].join(","));
        assert!(CommandResult::new(&nodes(MAX_COMMAND_JSON_NODES - 1)).is_ok());
        assert_eq!(
            CommandResult::new(&nodes(MAX_COMMAND_JSON_NODES)),
            Err(CommandError::JsonNodeBudget)
        );
        // Delimiters and escaped quotes inside strings are not nesting.
        let brackets = json!({"string": "[\"\\".repeat(100)}).to_string();
        assert!(CommandResult::new(&brackets).is_ok());
    }

    #[test]
    fn absent_current_command_executes_only_against_current_generation_and_budget() {
        let command = command("null");
        assert_eq!(
            decide(&command, snapshot(), None),
            Ok(CommandDecision::Execute)
        );
        let mut observed = snapshot();
        observed.generation = 8;
        assert_eq!(
            decide(&command, observed, None),
            Ok(CommandDecision::StaleGeneration)
        );
        observed.generation = 7;
        observed.receipt_storage = ReceiptStorage::Exhausted;
        assert_eq!(
            decide(&command, observed, None),
            Ok(CommandDecision::ReceiptStorageExhausted)
        );
    }

    #[test]
    fn original_receipt_replay_precedes_stale_preconditions_restore_and_budget() {
        let command = command(r#"{"operation":"merge"}"#);
        let receipt = receipt(&command);
        let ReceiptBody::Full(original) = receipt.body() else {
            panic!("full receipt")
        };
        for generation in [0, 7, 8, i64::MAX] {
            for incarnation in ["incarnation-1", "restored-incarnation"] {
                for storage in [ReceiptStorage::Available, ReceiptStorage::Exhausted] {
                    let mut observed = snapshot();
                    observed.generation = generation;
                    observed.incarnation = incarnation;
                    observed.receipt_storage = storage;
                    let result = decide(&command, observed, Some(&receipt)).unwrap();
                    assert_eq!(
                        result,
                        CommandDecision::Replay {
                            result: original.value()
                        }
                    );
                    assert!(result.is_committed());
                    let CommandDecision::Replay { result } = result else {
                        unreachable!()
                    };
                    assert!(std::ptr::eq(result, original.value()));
                }
            }
        }
    }

    #[test]
    fn changed_envelope_conflicts_even_when_preconditions_match_or_body_is_hidden() {
        let original = command(r#"{"operation":"merge"}"#);
        let receipt = receipt(&original);
        let changed = command(r#"{"operation":"split"}"#);
        let changed_precondition =
            CommandEnvelope::new(1, key(), "incarnation-1", 8, r#"{"operation":"merge"}"#).unwrap();
        let changed_incarnation =
            CommandEnvelope::new(1, key(), "restored", 7, r#"{"operation":"merge"}"#).unwrap();
        for changed in [changed, changed_precondition, changed_incarnation] {
            for visibility in [
                ReceiptVisibility::FullResult,
                ReceiptVisibility::CommittedStatusOnly,
            ] {
                let result =
                    decide_command(&changed, snapshot(), Some(&receipt), visibility).unwrap();
                assert_eq!(result, CommandDecision::Conflict);
                assert!(!result.is_committed());
            }
        }
    }

    #[test]
    fn authorized_status_only_replay_never_contains_original_result() {
        let command = command("null");
        let receipt = receipt(&command);
        let result = decide_command(
            &command,
            snapshot(),
            Some(&receipt),
            ReceiptVisibility::CommittedStatusOnly,
        )
        .unwrap();
        assert_eq!(result, CommandDecision::CommittedRedacted);
        assert!(result.is_committed());
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({"status":"committed_redacted"})
        );
    }

    #[test]
    fn expired_tombstones_never_execute_or_hide_conflicting_reuse() {
        let original = command("null");
        let expired = StoredReceipt::new(
            original.key().clone(),
            original.digest().clone(),
            ReceiptBody::Expired,
        );
        for visibility in [
            ReceiptVisibility::FullResult,
            ReceiptVisibility::CommittedStatusOnly,
        ] {
            let mut observed = snapshot();
            observed.generation = 99;
            observed.incarnation = "restored";
            observed.receipt_storage = ReceiptStorage::Exhausted;
            let result = decide_command(&original, observed, Some(&expired), visibility).unwrap();
            assert_eq!(result, CommandDecision::ReceiptExpired);
            assert!(result.is_committed());
            assert_eq!(
                serde_json::to_value(result).unwrap(),
                json!({"status":"receipt_expired"})
            );
            assert_eq!(
                decide_command(&command("true"), observed, Some(&expired), visibility),
                Ok(CommandDecision::Conflict)
            );
        }
    }

    #[test]
    fn missing_pre_restore_receipt_is_unknown_even_when_generation_repeats() {
        let command = command("null");
        for generation in [0, 7, 8] {
            for storage in [ReceiptStorage::Available, ReceiptStorage::Exhausted] {
                let mut observed = snapshot();
                observed.incarnation = "restored";
                observed.generation = generation;
                observed.receipt_storage = storage;
                let result = decide(&command, observed, None).unwrap();
                assert_eq!(result, CommandDecision::OutcomeUnknownAfterRestore);
                assert!(!result.is_committed());
            }
        }
    }

    #[test]
    fn foreign_catalog_namespace_or_receipt_fails_before_body_disclosure() {
        let command = command("null");
        let receipt = receipt(&command);
        let mut observed = snapshot();
        observed.catalog_id = "other";
        assert_eq!(
            decide(&command, observed, Some(&receipt)),
            Err(CommandError::CatalogMismatch)
        );
        observed = snapshot();
        observed.host_namespace = "other";
        assert_eq!(
            decide(&command, observed, Some(&receipt)),
            Err(CommandError::HostNamespaceMismatch)
        );
        for foreign_key in [
            CommandKey::new("other", "host", "operation").unwrap(),
            CommandKey::new("catalog", "other", "operation").unwrap(),
            CommandKey::new("catalog", "host", "other").unwrap(),
        ] {
            let foreign = StoredReceipt::new(
                foreign_key,
                receipt.digest().clone(),
                receipt.body().clone(),
            );
            assert_eq!(
                decide(&command, snapshot(), Some(&foreign)),
                Err(CommandError::ReceiptKeyMismatch)
            );
        }
    }

    #[test]
    fn invalid_owner_snapshot_is_not_a_replay_or_permission_to_execute() {
        let command = command("null");
        let receipt = receipt(&command);
        for present in [None, Some(&receipt)] {
            let mut observed = snapshot();
            observed.generation = -1;
            assert_eq!(
                decide(&command, observed, present),
                Err(CommandError::InvalidGeneration)
            );
            observed = snapshot();
            observed.incarnation = "";
            assert_eq!(
                decide(&command, observed, present),
                Err(CommandError::InvalidIncarnation)
            );
            observed = snapshot();
            observed.catalog_id = "";
            assert_eq!(
                decide(&command, observed, present),
                Err(CommandError::InvalidCatalogId)
            );
            observed = snapshot();
            observed.host_namespace = "";
            assert_eq!(
                decide(&command, observed, present),
                Err(CommandError::InvalidHostNamespace)
            );
        }
    }

    #[test]
    fn stable_outcome_codes_match_serialized_status() {
        let body = json!({"item_id":"original"});
        let outcomes = [
            CommandDecision::Execute,
            CommandDecision::Replay { result: &body },
            CommandDecision::CommittedRedacted,
            CommandDecision::ReceiptExpired,
            CommandDecision::Conflict,
            CommandDecision::OutcomeUnknownAfterRestore,
            CommandDecision::StaleGeneration,
            CommandDecision::ReceiptStorageExhausted,
        ];
        for outcome in outcomes {
            assert_eq!(
                serde_json::to_value(&outcome).unwrap()["status"],
                outcome.code()
            );
        }
    }
}
