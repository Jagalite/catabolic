//! Live retained migration custody only. This token is not a Store, bootstrap
//! completion, a crash-reopen proof, or an effect/cleanup-witness producer.
use super::*;

pub(crate) struct AdmittedPending41<'a> {
    db: &'a Connection,
    catalog: &'a str,
    incarnation: &'a str,
}
impl AdmittedPending41<'_> {
    pub(crate) fn database(&self) -> &Connection {
        self.db
    }
    pub(crate) fn catalog_id(&self) -> &str {
        self.catalog
    }
    pub(crate) fn incarnation(&self) -> &str {
        self.incarnation
    }
}
impl RetainedNativeUpgrade {
    /// Consuming owner, including error/unwind: no reusable session can survive
    /// an unproved rollback. Only exact live postmigration custody is admitted.
    pub(super) fn pending_inventory(mut self) -> Result<(Self, Value)> {
        let inventory = self.pending_inventory_borrowed()?;
        Ok((self, inventory))
    }
    pub(in crate::migration) fn pending_inventory_borrowed(&mut self) -> Result<Value> {
        self.pending_inventory_borrowed_inner(0)
    }
    pub(super) fn pending_inventory_borrowed_inner(&mut self, fault: u8) -> Result<Value> {
        self.pending_inventory_with_control_workspace(fault, None)
    }
    pub(super) fn pending_inventory_with_control_workspace(
        &mut self,
        fault: u8,
        control: Option<&mut allocation::ControlWorkspaceLoan<'_>>,
    ) -> Result<Value> {
        let allocation = self.ensure_allocation()?;
        let _scope = allocation.enter()?;
        let _scratch = allocation::MetadataReservation::new(&allocation, &self.db, control)?;
        let proof = self.retained_preservation.as_ref().ok_or_else(|| {
            Error("live migration preservation proof missing; recovery required".into())
        })?;
        let backup = self
            .verified_backup
            .as_ref()
            .ok_or_else(|| Error("live migration backup custody missing".into()))?;
        self.admission.validate_path()?;
        proof.validate_path(&self.path)?;
        require_native_journal(&self.db)?;
        proof.verify_committed_in(&self.db, backup.sha256())?;
        backup.revalidate()?;
        self.db.execute_batch("BEGIN EXCLUSIVE")?;
        #[cfg(test)]
        if fault == 1 {
            panic!("controlled pending-inventory transaction unwind");
        }
        #[cfg(test)]
        if fault == 2 {
            return Err(Error("controlled pending-inventory transaction failure".into()));
        }
        #[cfg(not(test))]
        let _ = fault;
        proof.verify_preserved_in(&self.db, backup.sha256())?;
        // Empty pending state is consistency only. The positive authority above
        // is the retained real migration and exact immutable transition proof.
        if self.db.prepare("SELECT 1 FROM catalog_nfo_bootstrap UNION ALL SELECT 1 FROM catalog_nfo_states UNION ALL SELECT 1 FROM catalog_nfo_state_revisions UNION ALL SELECT 1 FROM catalog_nfo_identity_claims UNION ALL SELECT 1 FROM catalog_nfo_file_attributions LIMIT 1")?.exists([])? {
            return Err(Error("offline pending bootstrap state is not empty".into()));
        }
        let (catalog, incarnation) = proof.identity();
        let admitted = AdmittedPending41 {
            db: &self.db,
            catalog,
            incarnation,
        };
        let inventory = crate::native_upgrade_inventory::qualified_pending_work41(&admitted)?;
        if inventory
            .as_object()
            .is_none_or(|counts| counts.values().any(|v| v.as_i64() != Some(0)))
        {
            return Err(Error("offline pending native work is unresolved".into()));
        }
        self.admission.validate_path()?;
        proof.validate_path(&self.path)?;
        backup.revalidate()?;
        proof.verify_preserved_in(&self.db, backup.sha256())?;
        self.db.execute_batch("ROLLBACK")?;
        Ok(inventory)
    }
}
