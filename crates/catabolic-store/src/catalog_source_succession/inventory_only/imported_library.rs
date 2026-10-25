//! Private, deliberately full-receipt-only imported library lineage check.
//! This is not current source authority and is not an expiry-safe serving scope.
//! Original import results must remain available; no missing history is inferred.
use crate::catalog_identity_mutation::StructuralReadBudget;
use crate::catalog_import::{self, ImportReceipt, ImportStep};
use crate::operation_budget::BudgetCharge;
use crate::{Error, Result, Store};
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
use rusqlite::{params, types::ValueRef};
use serde_json::{Value, json};

pub(crate) mod archived;
mod lineage;

const HOST: &str = "motion.catalog.operator.v1";
const MAX_RECEIPT: usize = 256 * 1024;
fn fail(message: &str) -> Error {
    Error(format!("imported library lineage: {message}"))
}
fn require(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(fail(message)) }
}
fn charge(budget: &mut impl StructuralReadBudget, bytes: usize) -> Result<()> {
    budget.charge(BudgetCharge {
        bytes,
        values: 1,
        work: 1 + bytes.div_ceil(64),
        ..Default::default()
    })
}
fn prepare<'a>(
    store: &'a Store,
    sql: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<rusqlite::Statement<'a>> {
    budget.charge(BudgetCharge {
        work: 1 + sql.len().div_ceil(64),
        ..Default::default()
    })?;
    Ok(store.db.prepare(sql)?)
}
fn row_charge(row: &rusqlite::Row<'_>, budget: &mut impl StructuralReadBudget) -> Result<()> {
    let columns = row.as_ref().column_count();
    budget.charge(BudgetCharge {
        items: 1,
        values: columns,
        bytes: columns
            .checked_mul(32)
            .ok_or_else(|| fail("row charge overflow"))?,
        work: columns + 1,
    })
}
fn text<'a>(
    row: &'a rusqlite::Row<'_>,
    index: usize,
    budget: &mut impl StructuralReadBudget,
) -> Result<&'a str> {
    let ValueRef::Text(bytes) = row.get_ref(index)? else {
        return Err(fail("text storage class differs"));
    };
    require(bytes.len() <= MAX_RECEIPT, "text exceeds finite cohort")?;
    charge(budget, bytes.len())?;
    std::str::from_utf8(bytes).map_err(|_| fail("invalid UTF-8"))
}
fn value(
    row: &rusqlite::Row<'_>,
    index: usize,
    budget: &mut impl StructuralReadBudget,
) -> Result<Value> {
    let raw = text(row, index, budget)?;
    budget.raw_json(raw)?;
    catabolic_core::decode_json(raw).map_err(|_| fail("invalid canonical JSON value"))
}
fn encoded(value: &Value, budget: &mut impl StructuralReadBudget) -> Result<String> {
    // Serialize once through a counting writer before owning the canonical
    // encoder output; the borrowed value was admitted by its input reader.
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|n| *n <= MAX_RECEIPT)
                .ok_or_else(|| std::io::Error::other("library encoding exceeds cohort"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, value).map_err(|_| fail("encoding exceeds cohort"))?;
    charge(
        budget,
        count
            .0
            .checked_mul(4)
            .ok_or_else(|| fail("encoding overflow"))?,
    )?;
    Ok(catabolic_core::encode(value))
}
struct Receipt {
    result: ImportReceipt,
    expected: i64,
    committed: i64,
    time: i64,
}
fn full_receipt(
    store: &Store,
    operation: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<Receipt> {
    let mut statement = prepare(
        store,
        "SELECT result_state,result_json,contract_version,canonicalization_version,payload_digest,admission_incarnation,expected_generation,committed_generation,committed_at_us FROM main.catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?",
        budget,
    )?;
    let mut rows = statement.query(params![store.database_id, HOST, operation])?;
    let row = rows
        .next()?
        .ok_or_else(|| fail("required full import receipt missing"))?;
    require(
        text(row, 0, budget)? == "full",
        "expired import receipt is unsupported",
    )?;
    row_charge(row, budget)?;
    let raw = text(row, 1, budget)?;
    for _ in 0..6 {
        budget.raw_json(raw)?;
    }
    // Typed result plus mutation/envelope copies coexist. This is actual raw
    // input preflight, never a reservation based on a configured allowance.
    charge(
        budget,
        raw.len()
            .checked_mul(6)
            .ok_or_else(|| fail("receipt copies overflow"))?,
    )?;
    let result: ImportReceipt =
        serde_json::from_str(raw).map_err(|_| fail("trimmed or malformed import result"))?;
    let mutation = match &result {
        ImportReceipt::Prepared {
            manifest,
            input_digest,
            ..
        } => json!({"action":"prepare","manifest":manifest,"input_digest":input_digest}),
        ImportReceipt::Staged {
            import_id,
            input_digest,
            batch_index,
            batch,
            ..
        } => {
            json!({"action":"stage","import_id":import_id,"input_digest":input_digest,"batch_index":batch_index,"batch":batch})
        }
        ImportReceipt::ValidatedStaging {
            import_id,
            input_digest,
            ..
        } => json!({"action":"validate","import_id":import_id,"input_digest":input_digest}),
        ImportReceipt::Activated {
            import_id,
            input_digest,
            reference_ack_id,
            mapping_digest,
            ..
        } => {
            json!({"action":"activate","import_id":import_id,"input_digest":input_digest,"reference_ack_id":reference_ack_id,"mapping_digest":mapping_digest})
        }
    };
    let incarnation = text(row, 5, budget)?;
    let expected: i64 = row.get(6)?;
    let committed: i64 = row.get(7)?;
    let time: i64 = row.get(8)?;
    charge(budget, 128)?;
    let generation: i64 = store.db.query_row(
        "SELECT generation FROM main.catalog_state WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    require(
        row.get::<_, i64>(2)? == 1
            && row.get::<_, i64>(3)? == 1
            && Some(incarnation) == store.catalog_incarnation.as_deref()
            && expected >= 0
            && committed >= expected
            && committed <= generation
            && time >= 0,
        "receipt identity or generation differs",
    )?;
    let payload = encoded(&json!({"operation":"import","mutation":mutation}), budget)?;
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, HOST, operation)
            .map_err(|_| fail("invalid command key"))?,
        incarnation,
        expected,
        &payload,
    )
    .map_err(|_| fail("invalid import envelope"))?;
    require(
        command.digest().as_str() == text(row, 4, budget)?,
        "import envelope fingerprint differs",
    )?;
    Ok(Receipt {
        result,
        expected,
        committed,
        time,
    })
}

/// Descriptive result, not a new execution/read grant. No public constructor.
pub(crate) struct AuthenticatedImportedLibrary {
    definition: Value,
    item_revision: i64,
}
impl AuthenticatedImportedLibrary {
    pub(crate) fn definition(&self) -> &Value {
        &self.definition
    }
    pub(crate) fn item_revision(&self) -> i64 {
        self.item_revision
    }
}

pub(crate) fn validate_imported_library_in(
    store: &Store,
    subject: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<AuthenticatedImportedLibrary> {
    require(
        store.schema_version == 41
            && !store.db.is_autocommit()
            && store.db.transaction_state(Some("main"))? == rusqlite::TransactionState::Write,
        "exact completed writer required",
    )?;
    store
        .catalog_operation_budget()?
        .check()
        .map_err(|_| fail("operation budget refused"))?;
    charge(budget, subject.len())?;
    let mut statement = prepare(
        store,
        "SELECT contract_version,policies_json,legacy_raw_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM main.catalog_metadata_subjects WHERE item_id=?",
        budget,
    )?;
    let mut rows = statement.query([subject])?;
    let row = rows
        .next()?
        .ok_or_else(|| fail("metadata subject missing"))?;
    require(
        row.get::<_, i64>(0)? == 2
            && text(row, 3, budget)? == store.database_id
            && text(row, 4, budget)? == HOST,
        "subject producer differs",
    )?;
    row_charge(row, budget)?;
    let policies = value(row, 1, budget)?;
    require(
        value(row, 2, budget)? == json!({}),
        "nonempty legacy metadata unsupported",
    )?;
    let activation = text(row, 5, budget)?.to_owned();
    let admitted: i64 = row.get(6)?;
    let receipt = full_receipt(store, &activation, budget)?;
    let ImportReceipt::Activated {
        contract_version,
        catalog_id,
        incarnation,
        import_id,
        input_digest,
        source_snapshot_id,
        mapping_digest,
        mappings,
        typed_results,
        item_revision_baselines,
        ..
    } = receipt.result
    else {
        return Err(fail("subject does not name activation"));
    };
    require(
        contract_version == 2
            && catalog_id == store.database_id
            && Some(incarnation.as_str()) == store.catalog_incarnation.as_deref()
            && admitted >= 0
            && admitted <= receipt.time
            && activation
                == catalog_import::operation_id(&import_id, ImportStep::Activate)
                    .map_err(|e| fail(&e.to_string()))?,
        "activation producer differs",
    )?;
    let preparation = full_receipt(
        store,
        &catalog_import::operation_id(&import_id, ImportStep::Prepare)
            .map_err(|e| fail(&e.to_string()))?,
        budget,
    )?;
    let ImportReceipt::Prepared {
        contract_version: prepared_version,
        input_digest: prepared_digest,
        manifest,
    } = preparation.result
    else {
        return Err(fail("prepare result differs"));
    };
    require(
        prepared_version == contract_version
            && manifest.contract_version == contract_version
            && manifest.import_id == import_id
            && manifest.source_namespace == "motion.schema68.export.v4"
            && prepared_digest == input_digest
            && manifest.source_snapshot_id == source_snapshot_id
            && catalog_import::manifest_digest(&manifest).map_err(|e| fail(&e.to_string()))?
                == input_digest
            && preparation.committed <= receipt.committed,
        "prepared manifest differs",
    )?;
    let validation = full_receipt(
        store,
        &catalog_import::operation_id(&import_id, ImportStep::Validate)
            .map_err(|e| fail(&e.to_string()))?,
        budget,
    )?;
    let ImportReceipt::ValidatedStaging {
        contract_version: version,
        import_id: validated_id,
        input_digest: validated_input,
        validation_generation,
        mapping_digest: validated_mapping,
        proposed_mappings,
        proposed_typed_mappings,
        activation_supported,
        ..
    } = validation.result
    else {
        return Err(fail("validation result differs"));
    };
    require(
        version == contract_version
            && validated_id == import_id
            && validated_input == input_digest
            && validated_mapping == mapping_digest
            && proposed_mappings == mappings
            && proposed_typed_mappings == typed_results
            && activation_supported
            && validation_generation == receipt.expected
            && validation.committed == receipt.expected,
        "validated activation commitment differs",
    )?;
    let mut selected = None;
    let mut source_item = None;
    let mut all_mappings = Vec::new();
    for (index, descriptor) in manifest.batches.iter().enumerate() {
        let staged = full_receipt(
            store,
            &catalog_import::operation_id(&import_id, ImportStep::Batch(index as u32))
                .map_err(|e| fail(&e.to_string()))?,
            budget,
        )?;
        let ImportReceipt::Staged {
            contract_version: version,
            import_id: id,
            input_digest: digest,
            batch_index,
            batch,
        } = staged.result
        else {
            return Err(fail("batch result differs"));
        };
        require(
            version == contract_version
                && id == import_id
                && digest == input_digest
                && batch_index as usize == index
                && staged.committed <= receipt.committed
                && &catalog_import::describe_batch(&batch).map_err(|e| fail(&e.to_string()))?
                    == descriptor,
            "staged batch differs",
        )?;
        for item in batch.items {
            if item.item_id == subject {
                require(source_item.is_none(), "duplicate library item")?;
                source_item = Some(item.clone());
            }
            all_mappings.push(catalog_import::LogicalMapping {
                source_id: item.source_id,
                item_id: item.item_id,
            });
        }
        if let Some(typed) = batch.typed {
            for metadata in typed.metadata {
                if metadata.item_id == subject {
                    require(selected.is_none(), "duplicate library metadata")?;
                    selected = Some(metadata);
                }
            }
        }
    }
    // Fixed insertion sort charges every actual length-sensitive comparison.
    // Error propagates immediately, rather than hiding sticky budget failure in
    // a comparator callback or assuming input-byte charges cover sorting.
    // Worst-case quadratic comparisons and element moves are both bounded by
    // the same sticky budget. Swapping moves three struct-sized values; the
    // existing heap buffers are not cloned or allocated again.
    for i in 1..all_mappings.len() {
        let mut j = i;
        while j > 0 {
            let bytes = all_mappings[j - 1]
                .source_id
                .len()
                .max(all_mappings[j].source_id.len());
            budget.charge(BudgetCharge {
                work: 2 + bytes.div_ceil(64),
                ..Default::default()
            })?;
            if all_mappings[j - 1].source_id <= all_mappings[j].source_id {
                break;
            }
            let moved = std::mem::size_of_val(&all_mappings[j])
                .checked_mul(3)
                .ok_or_else(|| fail("mapping movement charge overflow"))?;
            budget.charge(BudgetCharge {
                work: 1 + moved.div_ceil(64),
                ..Default::default()
            })?;
            all_mappings.swap(j - 1, j);
            j -= 1;
        }
    }
    require(
        all_mappings == mappings
            && catalog_import::complete_mapping_digest(&mappings, &typed_results)
                .map_err(|e| fail(&e.to_string()))?
                == mapping_digest,
        "complete activation mappings differ",
    )?;
    let item = source_item.ok_or_else(|| fail("imported library item absent"))?;
    let metadata = selected.ok_or_else(|| fail("imported library metadata absent"))?;
    let observed = lineage::validate_current_in(
        store,
        subject,
        &item,
        &metadata,
        &activation,
        &policies,
        admitted,
        budget,
    )?;
    require(
        item_revision_baselines.get(subject) == Some(&observed.item_revision),
        "current library item head differs from activation",
    )?;
    Ok(observed)
}

#[cfg(test)]
mod tests;
