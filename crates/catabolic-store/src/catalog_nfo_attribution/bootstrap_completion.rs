//! Closed, immediate post-bootstrap verification. No restored/future-state inference.
use super::{bootstrap_inventory, proof_budget::ProofBudget, state};
use crate::{Error, Result, Store, migration::BootstrapWorkspace, operation_budget::BudgetCharge};
use catabolic_core::catalog_command::CommandEnvelope;
use rusqlite::{Row, types::ValueRef};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) struct VerifiedBootstrapCompletion {
    inventory_digest: String,
    committed_generation: i64,
    state_digest: String,
    source_count: i64,
    generation_delta: i64,
}
impl VerifiedBootstrapCompletion {
    pub(crate) fn generation_delta(&self) -> i64 {
        self.generation_delta
    }
    pub(crate) fn inventory_digest(&self) -> &str {
        &self.inventory_digest
    }
    pub(crate) fn committed_generation(&self) -> i64 {
        self.committed_generation
    }
    pub(crate) fn state_digest(&self) -> &str {
        &self.state_digest
    }
    pub(crate) fn source_count(&self) -> i64 {
        self.source_count
    }
}
fn refused(message: &str) -> Error {
    Error(format!(
        "catalog_nfo_bootstrap_completion_refused: {message}"
    ))
}
fn charge_sql(budget: &mut ProofBudget, sql: &str) -> Result<()> {
    budget.charge(BudgetCharge {
        bytes: sql.len(),
        values: 1,
        work: 1,
        ..Default::default()
    })
}
// Inspect borrowed SQLite values before any owned row text/JSON allocation.
fn charge_row(budget: &mut ProofBudget, row: &Row<'_>, columns: usize) -> Result<()> {
    let mut bytes = 0usize;
    for column in 0..columns {
        if let ValueRef::Text(text) | ValueRef::Blob(text) = row.get_ref(column)? {
            if text.len() > 1024 {
                return Err(refused("oversized completion row field"));
            }
            bytes = bytes
                .checked_add(text.len())
                .ok_or_else(|| refused("row size overflow"))?;
        }
    }
    budget.charge(BudgetCharge {
        items: 1,
        bytes: bytes.saturating_mul(4),
        values: columns,
        work: columns + bytes / 64,
    })
}
fn hash_part(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

pub(crate) fn verify_in(
    store: &Store,
    command: &CommandEnvelope,
) -> Result<VerifiedBootstrapCompletion> {
    verify_in_with_control_workspace(store, command, None)
}

pub(crate) fn verify_in_with_control_workspace(
    store: &Store,
    command: &CommandEnvelope,
    control: Option<&BootstrapWorkspace<'_>>,
) -> Result<VerifiedBootstrapCompletion> {
    if store.schema_version != 41 || store.db.is_autocommit() || !store.writable {
        return Err(refused("retained schema41 writer transaction required"));
    }
    store.transaction_available_with_control_workspace(control)?;
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    let receipt = verify_receipt(store, command, true, &mut budget)?;
    let inventory_digest = receipt.inventory_digest;
    let source_count = receipt.source_count;
    let committed_generation = receipt.committed_generation;

    let inventory = bootstrap_inventory::capture_with_control_workspace(store, control)?;
    inventory.require_complete()?;
    if i64::try_from(inventory.subjects().len()).ok() != Some(source_count) {
        return Err(refused("complete inventory count differs"));
    }
    // Per-subject state readers keep their ordinary accounting and are never
    // reached by the fixed empty-bootstrap control workspace.
    if control.is_some() && source_count != 0 {
        return Err(refused("controlled bootstrap requires zero source count"));
    }
    let sql = "SELECT item_id,current_revision FROM catalog_nfo_states ORDER BY item_id";
    charge_sql(&mut budget, sql)?;
    let mut query = store.db.prepare(sql)?;
    let mut rows = query.query([])?;
    for subject in inventory.subjects().keys() {
        let row = rows
            .next()?
            .ok_or_else(|| refused("missing bootstrap state"))?;
        charge_row(&mut budget, row, 2)?;
        if row
            .get_ref(0)?
            .as_str()
            .map_err(|_| refused("invalid state key"))?
            != subject
            || row.get::<_, i64>(1)? != 1
        {
            return Err(refused("bootstrap state keyset/revision mismatch"));
        }
    }
    if rows.next()?.is_some() {
        return Err(refused("extra bootstrap state"));
    }
    drop(rows);
    drop(query);
    let sql = "SELECT item_id,revision,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_nfo_state_revisions ORDER BY item_id,revision";
    charge_sql(&mut budget, sql)?;
    let mut query = store.db.prepare(sql)?;
    let mut rows = query.query([])?;
    for subject in inventory.subjects().keys() {
        let row = rows
            .next()?
            .ok_or_else(|| refused("missing bootstrap revision"))?;
        charge_row(&mut budget, row, 5)?;
        if row
            .get_ref(0)?
            .as_str()
            .map_err(|_| refused("invalid revision key"))?
            != subject
            || row.get::<_, i64>(1)? != 1
            || row
                .get_ref(2)?
                .as_str()
                .map_err(|_| refused("invalid revision catalog"))?
                != command.key().catalog_id()
            || row
                .get_ref(3)?
                .as_str()
                .map_err(|_| refused("invalid revision host"))?
                != command.key().host_namespace()
            || row
                .get_ref(4)?
                .as_str()
                .map_err(|_| refused("invalid revision operation"))?
                != command.key().operation_id()
        {
            return Err(refused("bootstrap revision command mismatch"));
        }
    }
    if rows.next()?.is_some() {
        return Err(refused("extra bootstrap revision"));
    }
    drop(rows);
    drop(query);
    let generation_delta =
        verify_entity_closure(store, &mut budget, command, committed_generation)?;
    let mut hash = Sha256::new();
    hash_part(&mut hash, b"catabolic.nfo-bootstrap-completion.v1");
    hash_part(&mut hash, inventory_digest.as_bytes());
    hash_part(&mut hash, inventory.digest().as_bytes());
    for subject in inventory.subjects().keys() {
        // Zero is a descriptive constant, not a Motion aggregate revision grant.
        let current = state::read_in(store, subject, committed_generation, 0)?;
        let encoded = budget.serialized(
            &(
                subject,
                current.presence(),
                current.native_source(),
                current.origin_physical(),
                current.origin_proof_digest(),
                current.source_proof_digest(),
                current.attribution_audit_digest(),
            ),
            64,
        )?;
        hash_part(&mut hash, encoded.as_bytes());
    }
    Ok(VerifiedBootstrapCompletion {
        inventory_digest,
        committed_generation,
        state_digest: format!("{:x}", hash.finalize()),
        source_count,
        generation_delta,
    })
}

/// Compare both directions without copying any entity keys out of SQLite.
fn verify_entity_closure(
    store: &Store,
    budget: &mut ProofBudget,
    command: &CommandEnvelope,
    committed_generation: i64,
) -> Result<i64> {
    let count_sql = "SELECT (SELECT count(*) FROM catalog_nfo_states)+(SELECT count(*) FROM catalog_nfo_state_revisions)+(SELECT count(*) FROM catalog_nfo_identity_claims)+(SELECT count(*) FROM catalog_nfo_file_attributions)+(SELECT count(*) FROM catalog_nfo_bootstrap),(SELECT count(*) FROM catalog_entity_revisions WHERE entity_kind IN ('catalog_nfo_states','catalog_nfo_state_revisions','catalog_nfo_identity_claims','catalog_nfo_file_attributions','catalog_nfo_bootstrap'))";
    charge_sql(budget, count_sql)?;
    let (inserted, heads): (i64, i64) = store
        .db
        .query_row(count_sql, [], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let rows = inserted
        .checked_add(heads)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| refused("entity closure count overflow"))?;
    // Admit every prospective SQL key and comparison before the CTE runs.
    budget.charge(BudgetCharge {
        items: rows,
        bytes: rows
            .checked_mul(2048)
            .ok_or_else(|| refused("entity closure size overflow"))?,
        values: rows.saturating_mul(4),
        work: rows.saturating_mul(8),
    })?;
    if inserted != heads
        || command.expected_generation().checked_add(inserted) != Some(committed_generation)
    {
        return Err(refused("bootstrap entity count/generation delta differs"));
    }
    let orphan_sql = "SELECT EXISTS(SELECT 1 FROM catalog_nfo_identity_claims c WHERE c.state_revision!=1 OR NOT EXISTS(SELECT 1 FROM catalog_nfo_states s WHERE s.item_id=c.item_id AND s.current_revision=c.state_revision)) OR EXISTS(SELECT 1 FROM catalog_nfo_file_attributions f WHERE f.state_revision!=1 OR NOT EXISTS(SELECT 1 FROM catalog_nfo_states s WHERE s.item_id=f.item_id AND s.current_revision=f.state_revision))";
    charge_sql(budget, orphan_sql)?;
    if store
        .db
        .query_row(orphan_sql, [], |row| row.get::<_, bool>(0))?
    {
        return Err(refused(
            "identity/file attribution outside complete bootstrap states",
        ));
    }
    let sql = "WITH expected(kind,key) AS (SELECT 'catalog_nfo_states',json_array(item_id) FROM catalog_nfo_states UNION ALL SELECT 'catalog_nfo_state_revisions',json_array(item_id,revision) FROM catalog_nfo_state_revisions UNION ALL SELECT 'catalog_nfo_identity_claims',json_array(value) FROM catalog_nfo_identity_claims UNION ALL SELECT 'catalog_nfo_file_attributions',json_array(item_id) FROM catalog_nfo_file_attributions UNION ALL SELECT 'catalog_nfo_bootstrap',json_array(id) FROM catalog_nfo_bootstrap) SELECT EXISTS(SELECT 1 FROM expected e WHERE NOT EXISTS(SELECT 1 FROM catalog_entity_revisions r WHERE r.entity_kind=e.kind AND r.entity_key=e.key AND r.revision=1 AND r.deleted=0)) OR EXISTS(SELECT 1 FROM catalog_entity_revisions r WHERE r.entity_kind IN ('catalog_nfo_states','catalog_nfo_state_revisions','catalog_nfo_identity_claims','catalog_nfo_file_attributions','catalog_nfo_bootstrap') AND NOT EXISTS(SELECT 1 FROM expected e WHERE e.kind=r.entity_kind AND e.key=r.entity_key))";
    charge_sql(budget, sql)?;
    if store.db.query_row(sql, [], |row| row.get::<_, bool>(0))? {
        return Err(refused(
            "bootstrap entity heads differ from exact inserted rows",
        ));
    }
    Ok(inserted)
}

/// Historical immutable bootstrap receipt only. No present state, current
/// attribution, or permission to execute a source operation is inferred.
pub(crate) struct BootstrapReceipt {
    pub(crate) inventory_digest: String,
    pub(crate) source_count: i64,
    pub(crate) committed_generation: i64,
}
pub(crate) fn verify_historical_receipt_in(
    store: &Store,
    command: &CommandEnvelope,
) -> Result<BootstrapReceipt> {
    verify_historical_receipt_in_with_control_workspace(store, command, None)
}
pub(crate) fn verify_historical_receipt_in_with_control_workspace(
    store: &Store,
    command: &CommandEnvelope,
    control: Option<&BootstrapWorkspace<'_>>,
) -> Result<BootstrapReceipt> {
    if store.schema_version != 41 || !store.writable || store.db.is_autocommit() {
        return Err(refused("closed41 retained writer transaction required"));
    }
    store.transaction_available_with_control_workspace(control)?;
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    verify_receipt(store, command, false, &mut budget)
}
fn verify_receipt(
    store: &Store,
    command: &CommandEnvelope,
    immediate: bool,
    budget: &mut ProofBudget,
) -> Result<BootstrapReceipt> {
    verify_receipt_in(
        &store.db,
        &store.database_id,
        store.catalog_incarnation.as_deref(),
        command,
        immediate,
        budget,
        None,
    )
}
fn charge_restore_identity(
    budget: &mut ProofBudget,
    proof: &crate::catalog_restore_lineage::AdmittedRestoreHistory<'_>,
) -> Result<()> {
    let bytes = proof
        .catalog_id()
        .len()
        .checked_add(proof.incarnation().len())
        .and_then(|n| n.checked_mul(2))
        .and_then(|n| n.checked_add(16))
        .ok_or_else(|| refused("restore identity accounting overflow"))?;
    budget.charge(BudgetCharge {
        bytes,
        values: 6,
        work: 4 + bytes.div_ceil(64),
        ..Default::default()
    })
}
pub(crate) fn verify_restored_historical_receipt_in(
    db: &rusqlite::Connection,
    proof: &crate::catalog_restore_lineage::AdmittedRestoreHistory<'_>,
    command: &CommandEnvelope,
    budget: &mut ProofBudget,
) -> Result<BootstrapReceipt> {
    charge_restore_identity(budget, proof)?;
    proof.check(db)?;
    verify_receipt_in(
        db,
        proof.catalog_id(),
        Some(proof.incarnation()),
        command,
        false,
        budget,
        Some(proof),
    )
}
fn verify_receipt_in(
    db: &rusqlite::Connection,
    catalog: &str,
    incarnation: Option<&str>,
    command: &CommandEnvelope,
    immediate: bool,
    budget: &mut ProofBudget,
    history: Option<&crate::catalog_restore_lineage::AdmittedRestoreHistory<'_>>,
) -> Result<BootstrapReceipt> {
    let sql = "SELECT b.catalog_id,b.incarnation,b.inventory_digest,b.source_count,b.command_catalog_id,b.command_host_namespace,b.command_operation_id,r.contract_version,r.canonicalization_version,r.payload_digest,r.admission_incarnation,r.expected_generation,r.committed_generation,r.committed_at_us,r.retain_until_us,r.result_state,r.result_json,r.expired_at_us,c.catalog_id,c.incarnation,c.generation FROM catalog_nfo_bootstrap b JOIN catalog_command_receipts r ON r.catalog_id=b.command_catalog_id AND r.host_namespace=b.command_host_namespace AND r.operation_id=b.command_operation_id JOIN catalog_state c ON c.id=1 WHERE b.id=1 LIMIT 2";
    charge_sql(budget, sql)?;
    let mut query = db.prepare(sql)?;
    let mut rows = query.query([])?;
    let row = rows
        .next()?
        .ok_or_else(|| refused("missing bootstrap receipt"))?;
    charge_row(budget, row, 21)?;
    let text = |column| -> Result<&str> {
        row.get_ref(column)?
            .as_str()
            .map_err(|_| refused("non-text completion field"))
    };
    let digest = text(2)?;
    let source_count: i64 = row.get(3)?;
    let committed_generation: i64 = row.get(12)?;
    let committed_at: i64 = row.get(13)?;
    let retain_until: i64 = row.get(14)?;
    let historical_identity = if let Some(proof) = history {
        charge_restore_identity(budget, proof)?;
        let origin_work = proof.ordered_origins().try_fold(0usize, |n, v| {
            n.checked_add(1 + v.len().div_ceil(64))
                .ok_or_else(|| refused("restore origin accounting overflow"))
        })?;
        budget.charge(BudgetCharge {
            work: origin_work,
            ..Default::default()
        })?;
        proof.authorize_historical(text(1)?, committed_generation)?;
        proof.catalog_id() == catalog
            && text(19)? == proof.incarnation()
            && row.get::<_, i64>(20)? == proof.generation()
    } else {
        Some(text(1)?) == incarnation && text(19)? == text(1)?
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || source_count < 0
        || committed_generation < command.expected_generation()
        || committed_at < 0
        || retain_until < committed_at
        || text(0)? != catalog
        || text(18)? != catalog
        || !historical_identity
        || text(4)? != command.key().catalog_id()
        || text(4)? != text(0)?
        || text(5)? != command.key().host_namespace()
        || text(6)? != command.key().operation_id()
        || row.get::<_, i64>(7)? != i64::from(command.contract_version())
        || row.get::<_, i64>(8)? != i64::from(command.digest().canonicalization_version())
        || text(9)? != command.digest().as_str()
        || text(10)? != command.admission_incarnation()
        || text(10)? != text(1)?
        || row.get::<_, i64>(11)? != command.expected_generation()
        || row.get::<_, i64>(20)? < committed_generation
        || (immediate && row.get::<_, i64>(20)? != committed_generation)
    {
        return Err(refused("bootstrap envelope/identity/generation mismatch"));
    }
    // Fixed small payload/result objects and final owned digest descriptions.
    budget.charge(BudgetCharge {
        bytes: 2048,
        values: 32,
        work: 32,
        ..Default::default()
    })?;
    let expected_payload =
        json!({"operation":"nfo_bootstrap","contract_version":1,"inventory_digest":digest});
    if command.payload() != &expected_payload {
        return Err(refused("bootstrap payload mismatch"));
    }
    match text(15)? {
        "full" => {
            if !matches!(row.get_ref(17)?, ValueRef::Null) {
                return Err(refused("full receipt has expiry metadata"));
            }
            let result: Value =
                serde_json::from_str(text(16)?).map_err(|_| refused("invalid receipt result"))?;
            if result
                != json!({"contract_version":1,"source_count":source_count,"inventory_digest":digest})
            {
                return Err(refused("bootstrap receipt result mismatch"));
            }
        }
        "expired" => {
            if !matches!(row.get_ref(16)?, ValueRef::Null) || row.get::<_, i64>(17)? < retain_until
            {
                return Err(refused("invalid bootstrap tombstone"));
            }
        }
        _ => return Err(refused("unsupported receipt state")),
    }
    let inventory_digest = digest.to_owned();
    if rows.next()?.is_some() {
        return Err(refused("ambiguous bootstrap singleton"));
    }
    drop(rows);
    drop(query);

    Ok(BootstrapReceipt {
        inventory_digest,
        source_count,
        committed_generation,
    })
}
