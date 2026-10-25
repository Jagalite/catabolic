//! Affine proof retained only inside the independently validated immutable copy.
//! Complete prefix absence is history absence, not source existence or authority.
use super::*;
use crate::catalog_restore_lineage::AdmittedRestoreHistory;
use rusqlite::Connection;

struct Entry {
    profile: String,
    source: String,
    binding: Value,
    binding_allocations: u64,
}
pub(super) struct UnsealedRestoredHistory {
    handle: usize,
    changes: u64,
    entries: Vec<Entry>,
}
fn handle(db: &Connection) -> usize {
    // SAFETY: The borrowed Connection remains alive throughout this identity
    // observation. The pointer is never dereferenced or used to execute SQL.
    unsafe { db.handle() as usize }
}
impl UnsealedRestoredHistory {
    pub(super) fn new(
        db: &Connection,
        count: usize,
        b: &mut impl StructuralReadBudget,
    ) -> Result<Self> {
        if count > MAX_EXPORT_SOURCES {
            return Err(refuse("restored witness prefix bound"));
        }
        let bytes = count
            .checked_mul(std::mem::size_of::<Entry>())
            .ok_or_else(|| refuse("restored witness capacity overflow"))?;
        b.charge(BudgetCharge {
            bytes,
            values: count,
            work: 1,
            ..Default::default()
        })?;
        Ok(Self {
            handle: handle(db),
            changes: db.total_changes(),
            entries: Vec::with_capacity(count),
        })
    }
    pub(super) fn push(
        &mut self,
        profile: String,
        source: String,
        binding: Value,
        b: &mut impl StructuralReadBudget,
    ) -> Result<()> {
        if self.entries.len() == self.entries.capacity() {
            return Err(refuse("restored witness capacity changed"));
        }
        // All payload ownership moves from the already admitted complete verifier.
        // No String, Value tree, or map is cloned; charge the fixed owner movement.
        b.charge(BudgetCharge {
            work: 1 + std::mem::size_of::<Entry>().div_ceil(64),
            ..Default::default()
        })?;
        // Bindings are one flat SQL row, never decoded nested JSON. Admit both
        // this shape walk and the existing allocation walk before either runs.
        let fields = binding
            .as_object()
            .ok_or_else(|| refuse("restored binding is not a row"))?;
        let work = fields
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_mul(2))
            .ok_or_else(|| refuse("restored binding walk overflow"))?;
        b.charge(BudgetCharge {
            work,
            ..Default::default()
        })?;
        if fields
            .values()
            .any(|v| matches!(v, Value::Array(_) | Value::Object(_)))
        {
            return Err(refuse("restored binding contains nested payload"));
        }
        let binding_allocations = crate::migration::retained_value_allocation_bytes(&binding)?
            .checked_sub(std::mem::size_of::<Value>() as u64)
            .ok_or_else(|| refuse("restored binding allocation underflow"))?;
        // The root Value slot is already included in the Entry Vec capacity.
        // Capacity cannot change: the moved tree is private and only borrowed.
        self.entries.push(Entry {
            profile,
            source,
            binding,
            binding_allocations,
        });
        Ok(())
    }
}

/// No Clone, serde, public constructor, mutable access, or independent read grant.
/// The sole owner is RestoredReadImage, which also owns the readonly-deserialized
/// Connection and active immutable snapshot. The sqlite handle survives moving
/// the Rust Connection wrapper. Fresh ancestry/READ checks still use that wrapper.
pub(crate) struct RetainedRestoredSourceHistory {
    history: UnsealedRestoredHistory,
    catalog: String,
    incarnation: String,
    generation: i64,
    transition: String,
}
impl CapturedSourceHistory {
    pub(crate) fn into_read_witness(
        mut self,
        db: &Connection,
        proof: &AdmittedRestoreHistory<'_>,
        admission: &crate::migration::CurrentImageRotationAdmission<'_>,
        budget: &mut crate::catalog_nfo_attribution::ProofBudget,
    ) -> Result<RetainedRestoredSourceHistory> {
        history::charge_restore_check(proof, budget)?;
        proof.check(db)?;
        let captured = self
            .restored
            .take()
            .ok_or_else(|| refuse("ordinary history cannot mint restored witness"))?;
        if captured.handle != handle(db) || captured.changes != db.total_changes() {
            return Err(refuse("restored witness capture owner changed"));
        }
        // The genuine private rotation admission, not a caller boolean/callback,
        // owns the original effects and must compare every exact effect first.
        admission.require_source_effect_equality(self.effects(), budget)?;
        let bytes = [
            proof.catalog_id().len(),
            proof.incarnation().len(),
            proof.transition_digest().len(),
        ]
        .into_iter()
        .try_fold(0usize, |n, v| {
            n.checked_add(v)
                .ok_or_else(|| refuse("restored witness identity overflow"))
        })?;
        budget.charge(BudgetCharge {
            bytes,
            values: 3,
            work: 3 + bytes.div_ceil(64),
            ..Default::default()
        })?;
        let witness = RetainedRestoredSourceHistory {
            history: captured,
            catalog: proof.catalog_id().to_owned(),
            incarnation: proof.incarnation().to_owned(),
            generation: proof.generation(),
            transition: proof.transition_digest().to_owned(),
        };
        witness.check(db, proof, budget)?;
        Ok(witness)
    }
}
impl RetainedRestoredSourceHistory {
    fn check(
        &self,
        db: &Connection,
        proof: &AdmittedRestoreHistory<'_>,
        b: &mut impl StructuralReadBudget,
    ) -> Result<()> {
        history::charge_restore_check(proof, b)?;
        proof.check(db)?;
        let bytes = self
            .catalog
            .len()
            .checked_add(self.incarnation.len())
            .and_then(|n| n.checked_add(self.transition.len()))
            .ok_or_else(|| refuse("restored witness identity overflow"))?;
        b.charge(BudgetCharge {
            work: 5 + bytes.div_ceil(64),
            ..Default::default()
        })?;
        if self.history.handle != handle(db)
            || self.history.changes != db.total_changes()
            || self.catalog != proof.catalog_id()
            || self.incarnation != proof.incarnation()
            || self.generation != proof.generation()
            || self.transition != proof.transition_digest()
        {
            return Err(refuse("restored source witness owner changed"));
        }
        Ok(())
    }
    pub(in crate::catalog_source_succession) fn require_selected_in(
        &self,
        db: &Connection,
        proof: &AdmittedRestoreHistory<'_>,
        profile: &str,
        source: &str,
        b: &mut impl StructuralReadBudget,
    ) -> Result<()> {
        self.check(db, proof, b)?;
        // At most 64 entries, already in the authenticated permanent-prefix order.
        // A complete miss means no permanent history, never an admitted binding.
        for entry in &self.history.entries {
            let bytes = entry
                .profile
                .len()
                .min(profile.len())
                .checked_add(entry.source.len().min(source.len()))
                .and_then(|n| n.checked_add(2))
                .ok_or_else(|| refuse("restored witness lookup overflow"))?;
            b.charge(BudgetCharge {
                work: 2 + bytes.div_ceil(64),
                ..Default::default()
            })?;
            if entry.profile == profile && entry.source == source {
                require_current_binding_from(db, profile, source, &entry.binding, b)?;
                break;
            }
        }
        self.check(db, proof, b)
    }
    pub(crate) fn retained_allocations(&self) -> Result<u64> {
        let mut bytes = (self.history.entries.capacity() as u64)
            .checked_mul(std::mem::size_of::<Entry>() as u64)
            .ok_or_else(|| refuse("restored witness allocation overflow"))?;
        for capacity in [
            self.catalog.capacity(),
            self.incarnation.capacity(),
            self.transition.capacity(),
        ] {
            bytes = bytes
                .checked_add(capacity as u64)
                .ok_or_else(|| refuse("restored witness allocation overflow"))?;
        }
        for entry in &self.history.entries {
            bytes = bytes
                .checked_add(entry.profile.capacity() as u64)
                .and_then(|n| n.checked_add(entry.source.capacity() as u64))
                .and_then(|n| n.checked_add(entry.binding_allocations))
                .ok_or_else(|| refuse("restored witness allocation overflow"))?;
        }
        Ok(bytes)
    }
}

#[cfg(test)]
impl RetainedRestoredSourceHistory {
    pub(crate) fn assert_owner_negatives_for_test(
        &mut self,
        db: &Connection,
        proof: &AdmittedRestoreHistory<'_>,
        foreign: &Connection,
        budget: &mut impl StructuralReadBudget,
    ) -> Result<()> {
        self.check(db, proof, budget)?;
        let foreign_error = self.check(foreign, proof, budget).unwrap_err();
        assert!(
            foreign_error
                .to_string()
                .contains("restore history requires exact immutable read snapshot")
        );
        // Controlled corruption of an actually minted private witness, not a
        // manufactured admission or permission to alter the immutable database.
        let generation = self.generation;
        self.generation = generation.checked_add(1).unwrap();
        let stale = self.check(db, proof, budget);
        self.generation = generation;
        assert!(
            stale
                .unwrap_err()
                .to_string()
                .contains("restored source witness owner changed")
        );
        let original_handle = self.history.handle;
        self.history.handle = handle(foreign);
        let swapped = self.check(db, proof, budget);
        self.history.handle = original_handle;
        assert!(
            swapped
                .unwrap_err()
                .to_string()
                .contains("restored source witness owner changed")
        );
        let retained = self.retained_allocations()?;
        let expected = self.history.entries.iter().try_fold(
            self.history.entries.capacity() as u64 * std::mem::size_of::<Entry>() as u64
                + self.catalog.capacity() as u64
                + self.incarnation.capacity() as u64
                + self.transition.capacity() as u64,
            |n, entry| -> Result<u64> {
                Ok(n + entry.profile.capacity() as u64
                    + entry.source.capacity() as u64
                    + crate::migration::retained_value_allocation_bytes(&entry.binding)?
                    - std::mem::size_of::<Value>() as u64)
            },
        )?;
        assert_eq!(retained, expected);
        self.check(db, proof, budget)
    }
}

#[cfg(test)]
impl CapturedSourceHistory {
    pub(crate) fn assert_effect_tamper_refused_for_test(
        mut self,
        db: &Connection,
        proof: &AdmittedRestoreHistory<'_>,
        admission: &crate::migration::CurrentImageRotationAdmission<'_>,
        budget: &mut crate::catalog_nfo_attribution::ProofBudget,
    ) -> Result<()> {
        assert!(
            self.effects.is_empty(),
            "fixed empty genuine rotation fixture"
        );
        budget.charge(BudgetCharge {
            bytes: std::mem::size_of::<Value>(),
            values: 1,
            work: 1,
            ..Default::default()
        })?;
        self.effects = vec![Value::Null];
        let error = match self.into_read_witness(db, proof, admission, budget) {
            Err(error) => error,
            Ok(_) => return Err(refuse("altered complete effect set minted a witness")),
        };
        assert!(
            error
                .to_string()
                .contains("restored source effect count differs")
        );
        proof.check(db)
    }
}
