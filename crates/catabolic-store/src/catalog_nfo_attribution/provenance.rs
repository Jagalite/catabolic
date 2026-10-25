//! Read retained commit facts, including expired-result tombstones, without
//! resurrecting a result or changing receipt replay/restore semantics.
use crate::{Error, Result, Store, catalog_commands::TrustedCommandAuthority};
use catabolic_core::catalog_command::{CommandEnvelope, ReceiptVisibility};
use rusqlite::{OptionalExtension, params};

/// Descriptive, non-deserializable historical evidence. The later attribution
/// bootstrap must independently verify the source history and imported origin,
/// and repeat this check under its retained writer before applying any effect.
#[derive(Debug)]
pub struct VerifiedCommittedCommand {
    catalog_id: String,
    incarnation: String,
    host_namespace: String,
    operation_id: String,
    digest: String,
    observed_generation: i64,
    committed_generation: i64,
}
impl VerifiedCommittedCommand {
    pub fn committed_generation(&self) -> i64 {
        self.committed_generation
    }
    pub fn observed_generation(&self) -> i64 {
        self.observed_generation
    }
    pub fn matches(&self, command: &CommandEnvelope) -> bool {
        self.catalog_id == command.key().catalog_id()
            && self.incarnation == command.admission_incarnation()
            && self.host_namespace == command.key().host_namespace()
            && self.operation_id == command.key().operation_id()
            && self.digest == command.digest().as_str()
    }
}

fn refused(reason: &str) -> Error {
    Error(format!("catalog_nfo_provenance_refused: {reason}"))
}

/// Verify an exact committed command within an already retained transaction.
/// No transaction, progress handler, receipt write or operation execution is
/// installed here. Full-result read authority is required even though result
/// bytes are deliberately neither selected nor returned. Absence is not proof
/// of failure. An old-incarnation commit is not admitted after restore.
pub fn verify_committed_command_in(
    store: &Store,
    command: &CommandEnvelope,
    authority: TrustedCommandAuthority<'_>,
    expected_generation: i64,
) -> Result<VerifiedCommittedCommand> {
    if !matches!(store.schema_version, 40 | 41)
        || store.catalog_incarnation.is_none()
        || !store.writable
        || store.db.is_autocommit()
    {
        return Err(refused("native40/41 retained writer required"));
    }
    store.transaction_available()?;
    if authority.allow_new_commands
        || authority.receipt_visibility != ReceiptVisibility::FullResult
        || authority.host_namespace != command.key().host_namespace()
        || command.key().catalog_id() != store.database_id
    {
        return Err(refused("exact full receipt-only authority required"));
    }
    let (catalog, incarnation, generation): (String, String, i64) = store.db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if catalog != store.database_id
        || Some(incarnation.as_str()) != store.catalog_incarnation.as_deref()
        || incarnation != command.admission_incarnation()
        || generation != expected_generation
    {
        return Err(refused("catalog incarnation or current generation changed"));
    }
    store
        .catalog_operation_budget()?
        .charge(crate::operation_budget::BudgetCharge {
            bytes: command.canonical_payload().len().saturating_add(2048),
            items: 1,
            values: 12,
            work: command.canonical_payload().len() / 64 + 1,
            ..Default::default()
        })
        .map_err(|_| refused("proof budget exceeded"))?;
    // Select only bounded permanent facts, never a possibly expired/full body.
    type Row = (
        u32,
        u32,
        String,
        String,
        i64,
        i64,
        i64,
        i64,
        String,
        Option<i64>,
        bool,
    );
    let row: Option<Row> = store.db.query_row(
        "SELECT contract_version,canonicalization_version,payload_digest,admission_incarnation,expected_generation,committed_generation,committed_at_us,retain_until_us,result_state,expired_at_us,result_json IS NULL FROM catalog_command_receipts WHERE catalog_id=?1 AND host_namespace=?2 AND operation_id=?3",
        params![command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id()],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?)),
    ).optional()?;
    let Some((
        contract,
        canonical,
        digest,
        admitted,
        expected,
        committed,
        at,
        until,
        state,
        expired,
        body_absent,
    )) = row
    else {
        return Err(refused("committed command evidence absent"));
    };
    let valid_state = match state.as_str() {
        "full" => !body_absent && expired.is_none(),
        "expired" => body_absent && expired.is_some_and(|time| time >= until),
        _ => false,
    };
    if contract != command.contract_version()
        || canonical != command.digest().canonicalization_version()
        || digest != command.digest().as_str()
        || admitted != command.admission_incarnation()
        || expected != command.expected_generation()
        || expected < 0
        || committed < expected
        || committed > generation
        || at < 0
        || until < at
        || !valid_state
    {
        return Err(refused(
            "committed command facts do not match exact envelope",
        ));
    }
    Ok(VerifiedCommittedCommand {
        catalog_id: catalog,
        incarnation,
        host_namespace: authority.host_namespace.into(),
        operation_id: command.key().operation_id().into(),
        digest,
        observed_generation: generation,
        committed_generation: committed,
    })
}

#[cfg(test)]
mod tests;
