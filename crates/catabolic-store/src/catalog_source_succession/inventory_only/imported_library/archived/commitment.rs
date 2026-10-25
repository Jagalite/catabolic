//! Private canonical fingerprints, never typed semantic admission. Called only
//! after exact permanent Activate success and whole-part allocation preflight.
//! The canonical contract deliberately matches catalog_import::encoded:
//! serde_json::to_value -> encode_with(false,true) -> SHA256.
use super::*;
use crate::catalog_import::{BatchDescriptor, ImportBatch, SnapshotManifest};
fn count(n: &mut usize, add: usize, budget: &mut impl StructuralReadBudget) -> Result<()> {
    budget.charge(BudgetCharge {
        work: 1,
        ..Default::default()
    })?;
    *n = n
        .checked_add(add)
        .ok_or_else(|| fail("typed record count overflow"))?;
    require(
        *n <= 64,
        "typed record count exceeds supported import cohort",
    )
}
pub(super) fn typed_count(
    batch: &ImportBatch,
    budget: &mut impl StructuralReadBudget,
) -> Result<usize> {
    let Some(t) = &batch.typed else { return Ok(0) };
    let mut n = 0;
    count(&mut n, t.logical_aliases.len(), budget)?;
    count(&mut n, t.relationships.len(), budget)?;
    for tags in &t.tags {
        count(&mut n, 1, budget)?;
        count(&mut n, tags.tags.len(), budget)?;
        count(&mut n, tags.excluded_tags.len(), budget)?;
    }
    count(&mut n, t.metadata.len(), budget)?;
    for m in &t.metadata {
        count(&mut n, m.contributions.len(), budget)?;
    }
    for p in &t.pins {
        count(&mut n, p.history.len(), budget)?;
    }
    if let Some(native) = &t.native {
        count(&mut n, native.occurrences.len(), budget)?;
        count(&mut n, native.associations.len(), budget)?;
        count(&mut n, native.mutations.len(), budget)?;
    }
    Ok(n)
}
pub(super) fn manifest_digest(manifest: &SnapshotManifest) -> Result<String> {
    let value = serde_json::to_value(manifest).map_err(|_| fail("manifest encoding refused"))?;
    let bytes = catabolic_core::encode_with(&value, false, true);
    Ok(catabolic_core::sha256(bytes.as_bytes()))
}
pub(super) fn describe(
    batch: &ImportBatch,
    budget: &mut impl StructuralReadBudget,
) -> Result<BatchDescriptor> {
    let typed_records = typed_count(batch, budget)?;
    let value = serde_json::to_value(batch).map_err(|_| fail("batch encoding refused"))?;
    let bytes = catabolic_core::encode_with(&value, false, true);
    Ok(BatchDescriptor {
        sha256: catabolic_core::sha256(bytes.as_bytes()),
        canonical_bytes: bytes.len(),
        items: batch.items.len(),
        unresolved_evidence: batch.unresolved_evidence.len(),
        typed_records,
    })
}
#[cfg(test)]
mod tests;
