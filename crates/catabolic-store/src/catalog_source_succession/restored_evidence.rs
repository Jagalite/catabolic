//! Fixed descriptive evidence read from the independently rotated readonly image.
//! No ordinary Store admission, caller SQL, mutation or ancestry grant escapes.
use super::*;
use crate::catalog_restore_lineage::AdmittedRestoreHistory;
use rusqlite::{
    Connection,
    types::{Value as SqlValue, ValueRef},
};
use serde_json::Value;

/// Immutable descriptive rows. This is not a command or source availability proof.
pub struct RestoredSourceEvidence {
    headers: Vec<Value>,
    facts: Vec<Value>,
}
impl RestoredSourceEvidence {
    pub fn headers(&self) -> &[Value] {
        &self.headers
    }
    pub fn facts(&self) -> &[Value] {
        &self.facts
    }
}
const MAX_ROWS: usize = 16_384;
pub(super) const HEADERS: &[&str] = &[
    "profile",
    "file_id",
    "scan_id",
    "size",
    "mtime_ns",
    "device",
    "inode",
    "status",
    "reason",
    "bytes_read",
];
pub(super) const FACTS: &[&str] = &[
    "profile",
    "file_id",
    "operation",
    "job_id",
    "snapshot",
    "data",
    "status",
];

// Closed internal UNION projection. These padding columns and discriminator
// never escape the reader. UNION ALL preserves duplicate facts; the outer
// ordering retains each old relation's exact ordering keys independently.
pub(super) const COMBINED_SELECT: &str = "SELECT 0 AS evidence_kind,profile,file_id,scan_id,size,mtime_ns,device,inode,status,reason,bytes_read,NULL AS operation,NULL AS job_id,NULL AS snapshot,NULL AS data FROM catalog_source_effective_headers UNION ALL SELECT 1,profile,file_id,NULL,NULL,NULL,NULL,NULL,status,NULL,NULL,operation,job_id,snapshot,data FROM catalog_source_effective_file_facts";
const HEADER_INDICES: &[usize] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
const FACT_INDICES: &[usize] = &[1, 2, 11, 12, 13, 14, 8];
const HEADER_PADDING: &[usize] = &[11, 12, 13, 14];
const FACT_PADDING: &[usize] = &[3, 4, 5, 6, 7, 9, 10];

fn row_kind(row: &rusqlite::Row<'_>, budget: &mut impl StructuralReadBudget) -> Result<usize> {
    budget.charge(BudgetCharge {
        values: 1,
        bytes: 8,
        work: 1,
        ..Default::default()
    })?;
    let kind = match row.get_ref(0)? {
        ValueRef::Integer(0) => 0,
        ValueRef::Integer(1) => 1,
        _ => return Err(refuse("restored evidence discriminator changed")),
    };
    let padding = if kind == 0 {
        HEADER_PADDING
    } else {
        FACT_PADDING
    };
    budget.charge(BudgetCharge {
        values: padding.len(),
        work: padding.len(),
        ..Default::default()
    })?;
    for index in padding {
        if !matches!(row.get_ref(*index)?, ValueRef::Null) {
            return Err(refuse("restored evidence padding changed"));
        }
    }
    Ok(kind)
}
fn projection(kind: usize) -> (&'static [&'static str], &'static [usize]) {
    if kind == 0 {
        (HEADERS, HEADER_INDICES)
    } else {
        (FACTS, FACT_INDICES)
    }
}

pub(crate) fn capture_restored_source_evidence_in(
    db: &Connection,
    proof: &AdmittedRestoreHistory<'_>,
    profile: &str,
    source: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<RestoredSourceEvidence> {
    capture_evidence_in(db, proof, None, profile, source, budget)
}

pub(crate) fn capture_restored_source_evidence_with_witness_in(
    db: &Connection,
    proof: &AdmittedRestoreHistory<'_>,
    witness: &RetainedRestoredSourceHistory,
    profile: &str,
    source: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<RestoredSourceEvidence> {
    capture_evidence_in(db, proof, Some(witness), profile, source, budget)
}
fn capture_evidence_in(
    db: &Connection,
    proof: &AdmittedRestoreHistory<'_>,
    witness: Option<&RetainedRestoredSourceHistory>,
    profile: &str,
    source: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<RestoredSourceEvidence> {
    let operation = crate::operation_budget::OperationBudget::current()
        .ok_or_else(|| refuse("restored source read requires inherited operation"))?;
    operation
        .charge(Default::default())
        .map_err(|e| refuse(&format!("restored source read budget: {e:?}")))?;
    history::charge_restore_check(proof, budget)?;
    proof.check(db)?;
    let selector_bytes = profile
        .len()
        .checked_add(source.len())
        .ok_or_else(|| refuse("restored selector size overflow"))?;
    budget.charge(BudgetCharge {
        bytes: selector_bytes,
        values: 2,
        work: 2 + selector_bytes.div_ceil(64),
        ..Default::default()
    })?;
    if let Some(witness) = witness {
        witness.require_selected_in(db, proof, profile, source, budget)?;
    } else {
        // Ordinary fixed reader retains its complete fresh history verifier.
        let binding = history::current_restored_binding_in(db, proof, profile, source, budget)?;
        if let Some(binding) = binding.as_ref() {
            require_current_binding_from(db, profile, source, binding, budget)?;
        }
    }
    // Parameters are copied only after admitting the bounded token-derived
    // origins and caller selectors. No origin string is interpolated into SQL.
    let count = proof.ordered_origins().count();
    if !(1..=33).contains(&count) {
        return Err(refuse("restored origin count exceeds closed bound"));
    }
    let bytes = proof.ordered_origins().try_fold(
        profile
            .len()
            .checked_add(source.len())
            .ok_or_else(|| refuse("restored selector size overflow"))?,
        |sum, origin| {
            sum.checked_add(origin.len())
                .ok_or_else(|| refuse("restored parameter size overflow"))
        },
    )?;
    budget.charge(BudgetCharge {
        bytes: bytes
            .checked_add((count + 2) * std::mem::size_of::<SqlValue>())
            .ok_or_else(|| refuse("restored parameter storage overflow"))?,
        values: count + 2,
        work: count + 2 + bytes.div_ceil(64),
        ..Default::default()
    })?;
    let mut args = Vec::with_capacity(count + 2);
    args.push(SqlValue::Text(profile.to_owned()));
    args.push(SqlValue::Text(source.to_owned()));
    for origin in proof.ordered_origins() {
        args.push(SqlValue::Text(origin.to_owned()));
    }
    let sql = current::restored_combined_select(MAX_ROWS, proof, budget)?;
    let evidence = combined_rows(db, &sql, &args, MAX_ROWS, budget)?;
    history::charge_restore_check(proof, budget)?;
    proof.check(db)?;
    operation
        .charge(Default::default())
        .map_err(|e| refuse(&format!("restored source read budget: {e:?}")))?;
    Ok(evidence)
}

// Private fixed caller only. Both executions retain the same immutable image
// snapshot. The COMPLETE batch is admitted before either output Vec or any
// owned row is allocated; this is not incremental/unchecked row sampling.
fn combined_rows(
    db: &Connection,
    sql: &str,
    args: &[SqlValue],
    limit: usize,
    budget: &mut impl StructuralReadBudget,
) -> Result<RestoredSourceEvidence> {
    if limit > MAX_ROWS {
        return Err(refuse("restored evidence limit exceeds closed cohort"));
    }
    // Keep explicit preparation-input work, separate from construction passes.
    budget.charge(BudgetCharge {
        work: 1 + sql.len().div_ceil(64),
        ..Default::default()
    })?;
    let mut statement = db.prepare(sql)?;
    if statement.column_count() != 15 {
        return Err(refuse("restored evidence projection changed"));
    }
    let mut counts = [0usize; 2];
    let mut count = 0usize;
    let mut last_kind = 0usize;
    {
        let mut rows = statement.query(rusqlite::params_from_iter(args))?;
        loop {
            budget.charge(BudgetCharge {
                work: 1,
                ..Default::default()
            })?;
            let Some(row) = rows.next()? else { break };
            if count == limit {
                return Err(refuse("restored evidence exceeds export row cohort"));
            }
            let kind = row_kind(row, budget)?;
            if kind < last_kind {
                return Err(refuse("restored evidence order changed"));
            }
            last_kind = kind;
            counts[kind] += 1;
            count += 1;
            let (columns, indices) = projection(kind);
            // Borrow and admit every scalar before any row/string/map copy.
            // Fixed 7/10-field maps reserve node slots and static key copies;
            // the output Vec gets an exact separate capacity reservation below.
            let mut bytes = 0usize;
            let mut work = columns.len();
            for (key, index) in columns.iter().zip(indices) {
                let scalar = match row.get_ref(*index)? {
                    ValueRef::Null => 0,
                    ValueRef::Integer(_) => 8,
                    ValueRef::Text(raw) => raw
                        .len()
                        .checked_mul(2)
                        .ok_or_else(|| refuse("restored evidence text overflow"))?,
                    _ => return Err(refuse("restored evidence has unsupported scalar storage")),
                };
                bytes = bytes
                    .checked_add(scalar)
                    .and_then(|n| n.checked_add(256 + key.len()))
                    .ok_or_else(|| refuse("restored evidence row size overflow"))?;
                work = work
                    .checked_add(1 + scalar.div_ceil(64))
                    .ok_or_else(|| refuse("restored evidence row work overflow"))?;
            }
            budget.charge(BudgetCharge {
                items: 1,
                values: columns.len() + 1,
                bytes,
                work,
                ..Default::default()
            })?;
            for index in indices {
                if let ValueRef::Text(raw) = row.get_ref(*index)? {
                    std::str::from_utf8(raw)
                        .map_err(|_| refuse("restored evidence is not UTF-8"))?;
                }
            }
        }
    }
    budget.charge(BudgetCharge {
        bytes: count
            .checked_mul(std::mem::size_of::<Value>())
            .ok_or_else(|| refuse("restored evidence vector overflow"))?,
        work: 2,
        ..Default::default()
    })?;
    let mut result = [Vec::with_capacity(counts[0]), Vec::with_capacity(counts[1])];
    let mut rows = statement.query(rusqlite::params_from_iter(args))?;
    last_kind = 0;
    loop {
        budget.charge(BudgetCharge {
            work: 1,
            ..Default::default()
        })?;
        let Some(row) = rows.next()? else { break };
        let kind = row_kind(row, budget)?;
        if kind < last_kind || result[kind].len() == counts[kind] {
            return Err(refuse("restored evidence changed after admission"));
        }
        last_kind = kind;
        let (columns, indices) = projection(kind);
        budget.charge(BudgetCharge {
            work: columns.len(),
            ..Default::default()
        })?;
        let mut object = serde_json::Map::new();
        for (key, index) in columns.iter().zip(indices) {
            let value = row.get_ref(*index)?;
            let text = match value {
                ValueRef::Text(raw) => raw.len(),
                _ => 0,
            };
            // Distinct from borrowed validation: admit actual conversion/copy
            // work before creating each String. Storage was reserved earlier.
            budget.charge(BudgetCharge {
                work: 1 + text.div_ceil(64),
                ..Default::default()
            })?;
            object.insert((*key).to_owned(), crate::migration::value(value));
        }
        result[kind].push(Value::Object(object));
    }
    if result[0].len() != counts[0] || result[1].len() != counts[1] {
        return Err(refuse("restored evidence changed after admission"));
    }
    let [headers, facts] = result;
    Ok(RestoredSourceEvidence { headers, facts })
}

#[cfg(test)]
#[path = "restored_evidence/tests.rs"]
mod tests;
