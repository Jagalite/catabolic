//! Archived input plus permanent receipt facts, never an Activated result body.
//! Private descriptive stepping stone only: the current observed item revision
//! is NOT an original baseline. Motion must independently authenticate and compare
//! its immutable publication bridge before using any original baseline claim.
use super::*;
use crate::catalog_commands::TrustedCommandAuthority;
use crate::migration::offline_bootstrap::HostImportPart;

// Existing bounded serializer sequence ceiling; import batch counts use the
// tighter canonical commitment cohort.
const MAX_BATCHES: usize = 10_000;

pub(crate) struct ObservedArchivedLibrary {
    current: AuthenticatedImportedLibrary,
    catalog_id: String,
    incarnation: String,
    import_id: String,
    activation_operation_id: String,
    activation_digest: String,
    observed_generation: i64,
}
impl ObservedArchivedLibrary {
    pub(crate) fn definition(&self) -> &Value {
        self.current.definition()
    }
    pub(crate) fn current_item_revision(&self) -> i64 {
        self.current.item_revision()
    }
    pub(crate) fn catalog_id(&self) -> &str {
        &self.catalog_id
    }
    pub(crate) fn incarnation(&self) -> &str {
        &self.incarnation
    }
    pub(crate) fn import_id(&self) -> &str {
        &self.import_id
    }
    pub(crate) fn activation_operation_id(&self) -> &str {
        &self.activation_operation_id
    }
    pub(crate) fn activation_digest(&self) -> &str {
        &self.activation_digest
    }
    pub(crate) fn observed_generation(&self) -> i64 {
        self.observed_generation
    }
}

mod accounting;
mod commitment;

pub(crate) fn validate_archived_library_in(
    store: &Store,
    subject: &str,
    part: HostImportPart<'_>,
    authority: TrustedCommandAuthority<'_>,
    budget: &mut impl StructuralReadBudget,
) -> Result<ObservedArchivedLibrary> {
    validate_impl(store, subject, part, authority, budget, false)
}
pub(crate) fn validate_mixed_archived_library_in(
    store: &Store,
    subject: &str,
    part: HostImportPart<'_>,
    authority: TrustedCommandAuthority<'_>,
    budget: &mut impl StructuralReadBudget,
) -> Result<ObservedArchivedLibrary> {
    validate_impl(store, subject, part, authority, budget, true)
}
fn validate_impl(
    store: &Store,
    subject: &str,
    part: HostImportPart<'_>,
    authority: TrustedCommandAuthority<'_>,
    budget: &mut impl StructuralReadBudget,
    mixed: bool,
) -> Result<ObservedArchivedLibrary> {
    require(
        store.schema_version == 41
            && store.writable
            && !store.db.is_autocommit()
            && store.db.transaction_state(Some("main"))? == rusqlite::TransactionState::Write,
        "exact completed writer required",
    )?;
    store
        .catalog_operation_budget()?
        .check()
        .map_err(|_| fail("operation budget refused"))?;
    require(
        !subject.is_empty() && subject.len() <= 256 && authority.host_namespace == HOST,
        "archive subject or receipt authority differs",
    )?;
    if !mixed {
        accounting::require_library_only(part, budget)?;
    }
    let generation: i64 = prepare(
        store,
        "SELECT generation FROM main.catalog_state WHERE id=1",
        budget,
    )?
    .query_row([], |row| row.get(0))?;
    charge(budget, 32)?;
    let committed = crate::catalog_nfo_attribution::verify_committed_command_in(
        store,
        part.activation,
        authority,
        generation,
    )?;
    let manifest = part.manifest;
    let payload = part.activation.payload();
    let mutation = &payload["mutation"];
    require(
        !manifest.import_id.is_empty() && manifest.import_id.len() <= 256,
        "archive import identifier exceeds cohort",
    )?;
    // operation_id allocates the Activate suffix, hexadecimal SHA text, and
    // formatted key before mixed canonical preflight. Three times each output
    // length covers requested geometric String growth plus the old buffer at a
    // growth boundary; these are named fixed outputs, not a generic JSON factor.
    let key_len = catalog_import::OPERATION_PREFIX
        .len()
        .checked_add(64 + 1 + "activate".len())
        .ok_or_else(|| fail("operation key accounting overflow"))?;
    let operation_bytes = 64usize
        .checked_add(key_len)
        .and_then(|n| n.checked_mul(3))
        .and_then(|n| n.checked_add("activate".len() + 3 * std::mem::size_of::<String>() + 32))
        .ok_or_else(|| fail("operation key accounting overflow"))?;
    let operation_work = manifest
        .import_id
        .len()
        .checked_mul(3)
        .and_then(|n| n.checked_add(64 + key_len * 2))
        .ok_or_else(|| fail("operation key work overflow"))?;
    budget.charge(BudgetCharge {
        bytes: operation_bytes,
        values: 3,
        work: 6 + operation_work.div_ceil(64),
        ..Default::default()
    })?;
    require(
        manifest.contract_version == 2
            && manifest.source_namespace == "motion.schema68.export.v4"
            && manifest.batches.len() == part.batches.len()
            && payload.as_object().is_some_and(|m| m.len() == 2)
            && payload["operation"] == "import"
            && mutation.as_object().is_some_and(|m| m.len() == 5)
            && mutation["action"] == "activate"
            && mutation["import_id"].as_str() == Some(manifest.import_id.as_str())
            && mutation["reference_ack_id"].as_str().is_some_and(|s| {
                !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
            })
            && mutation["mapping_digest"].as_str().is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            && part.activation.key().operation_id()
                == catalog_import::operation_id(&manifest.import_id, ImportStep::Activate)
                    .map_err(|e| fail(&e.to_string()))?,
        "exact archived Activate identity differs",
    )?;
    // Both cohorts authenticate the successful permanent Activate before this
    // canonical-only path. The library-only wrapper additionally retains its
    // exact all-part shape restriction above; the selected strict tail is shared.
    accounting::preflight_mixed(part, subject, budget)?;
    let digest = commitment::manifest_digest(manifest)?;
    require(
        mutation["input_digest"].as_str() == Some(digest.as_str()),
        "archive manifest fingerprint differs",
    )?;
    let mut item = None;
    let mut metadata = None;
    for (batch, descriptor) in part.batches.iter().zip(&manifest.batches) {
        let actual = commitment::describe(batch, budget)?;
        require(&actual == descriptor, "archive batch descriptor differs")?;
        for candidate in &batch.items {
            charge(
                budget,
                candidate
                    .item_id
                    .len()
                    .checked_add(subject.len())
                    .ok_or_else(|| fail("archive comparison overflow"))?,
            )?;
            if candidate.item_id == subject {
                require(item.replace(candidate).is_none(), "duplicate library item")?;
            }
        }
        if let Some(typed) = &batch.typed {
            for candidate in &typed.metadata {
                charge(
                    budget,
                    candidate
                        .item_id
                        .len()
                        .checked_add(subject.len())
                        .ok_or_else(|| fail("archive comparison overflow"))?,
                )?;
                if candidate.item_id == subject {
                    require(
                        metadata.replace(candidate).is_none(),
                        "duplicate library metadata",
                    )?;
                }
            }
        }
    }
    if mixed {
        require(
            part.batches
                .iter()
                .all(|batch| batch.unresolved_evidence.is_empty()),
            "successful Activate cannot retain unresolved evidence",
        )?;
    }
    let item = item.ok_or_else(|| fail("imported library item absent"))?;
    let metadata = metadata.ok_or_else(|| fail("imported library metadata absent"))?;
    let mut statement = prepare(
        store,
        "SELECT contract_version,policies_json,legacy_raw_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM main.catalog_metadata_subjects WHERE item_id=?",
        budget,
    )?;
    let mut rows = statement.query([subject])?;
    let row = rows
        .next()?
        .ok_or_else(|| fail("metadata subject missing"))?;
    row_charge(row, budget)?;
    require(
        row.get::<_, i64>(0)? == 2
            && text(row, 3, budget)? == store.database_id
            && text(row, 4, budget)? == HOST
            && text(row, 5, budget)? == part.activation.key().operation_id()
            && value(row, 2, budget)? == json!({}),
        "subject producer differs",
    )?;
    let policies = value(row, 1, budget)?;
    let admitted: i64 = row.get(6)?;
    require(rows.next()?.is_none(), "duplicate metadata subject")?;
    let receipt_time: i64 = prepare(store,
        "SELECT committed_at_us FROM main.catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?", budget)?
        .query_row(params![store.database_id, HOST, part.activation.key().operation_id()], |r| r.get(0))?;
    charge(budget, 32)?;
    require(
        admitted >= 0 && admitted <= receipt_time && committed.committed_generation() <= generation,
        "subject admission differs from permanent Activate",
    )?;
    let current = super::lineage::validate_current_in(
        store,
        subject,
        item,
        metadata,
        part.activation.key().operation_id(),
        &policies,
        admitted,
        budget,
    )?;
    let strings = [
        store.database_id.as_str(),
        part.activation.admission_incarnation(),
        manifest.import_id.as_str(),
        part.activation.key().operation_id(),
        part.activation.digest().as_str(),
    ];
    let bytes = strings
        .iter()
        .try_fold(0usize, |n, s| n.checked_add(s.len()))
        .ok_or_else(|| fail("observed description overflow"))?;
    charge(budget, bytes)?;
    Ok(ObservedArchivedLibrary {
        current,
        catalog_id: strings[0].to_owned(),
        incarnation: strings[1].to_owned(),
        import_id: strings[2].to_owned(),
        activation_operation_id: strings[3].to_owned(),
        activation_digest: strings[4].to_owned(),
        observed_generation: generation,
    })
}

#[cfg(test)]
mod tests;
