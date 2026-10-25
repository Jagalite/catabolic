//! Planning-only retained transaction corridor. No mutation or COMMIT authority.
use super::*;
use crate::catalog_restore_lineage::completed_image::PlannedCurrentRotation;
use crate::catalog_source_succession::authorizer::{EndpointBoundary, PlannedReadObserver};
use crate::operation_budget::OperationBudget;

pub(super) struct PlanningAdmission {
    _storage: allocation::AllocationGuard,
    source: PlannedReadObserver,
    copy: PlannedReadObserver,
    budget: OperationBudget,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Ready,
    Failed,
    Planned,
    Settled,
}
/// Owns the entire original rotation operation. Neither its legacy rotation nor
/// its legacy finish can be reached after this move-only handoff.
pub struct CurrentRotationPlanningOperation {
    operation: CurrentRotationOperation,
    admission: PlanningAdmission,
    attachment: Option<allocation::AllocationGuard>,
    image_allocation: Option<OwnedImageAllocation>,
    plan: Option<PlannedCurrentRotation>,
    phase: Phase,
    #[cfg(test)]
    final_poll: Option<std::sync::Arc<AtomicBool>>,
}
/// Borrowed historical plan facts only. These do not renew temporal authority.
pub struct CurrentRotationPlanFacts<'a> {
    pub restore_id: &'a str,
    pub to_incarnation: &'a str,
    pub origin_json: &'a str,
    pub generation: i64,
    pub transition_digest: &'a str,
}
impl CurrentRotationOperation {
    /// All new owner/hook storage is admitted while original custody is borrowed.
    pub fn admit_planning(&mut self) -> Result<()> {
        if self.planning.is_some() || !matches!(self.state, RotationState::Ready(_)) {
            return Err(Error("rotation planning admission unavailable".into()));
        }
        self.live(&AtomicBool::new(false))?;
        let store = &self.owner.session.store;
        if !store.db.is_autocommit() {
            return Err(Error("planning cannot adopt outer transaction".into()));
        }
        let budget = OperationBudget::current()
            .map_or_else(crate::catalog_search_index::default_operation_budget, Ok)?;
        let _scope = budget
            .enter()
            .map_err(|e| Error(format!("planning budget: {e:?}")))?;
        let mut proof = crate::catalog_nfo_attribution::ProofBudget::new(budget.clone());
        let origins =
            crate::catalog_source_succession::scope_history::origins_guard_charge(&mut proof)?;
        let bytes = std::mem::size_of::<CurrentRotationPlanningOperation>()
            .checked_mul(3)
            .and_then(|n| n.checked_add(origins.checked_mul(2)?))
            .and_then(|n| n.checked_add(PlannedReadObserver::fixed_storage_bytes()?))
            .ok_or_else(|| Error("planning storage overflow".into()))?;
        let storage = store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("planning allocation missing".into()))?
            .allocation
            .reserve(&store.db, bytes as u64)?;
        // Preserve the existing deny-all admission probe before any original WRITE.
        if store.transaction_poisoned.get() || !store.owns_writer_lock() {
            return Err(Error("planning original owner unavailable".into()));
        }
        store.db.query_row("SELECT 1", [], |_| Ok(()))?;
        let source = PlannedReadObserver::prepared(true);
        let copy = PlannedReadObserver::prepared(false);
        self.planning = Some(PlanningAdmission {
            _storage: storage,
            source,
            copy,
            budget,
        });
        Ok(())
    }
    /// No query, allocation, callback or deadline refresh occurs here.
    pub fn into_planning(mut self) -> std::result::Result<CurrentRotationPlanningOperation, Self> {
        let Some(admission) = self.planning.take() else {
            return Err(self);
        };
        Ok(CurrentRotationPlanningOperation {
            operation: self,
            admission,
            attachment: None,
            image_allocation: None,
            plan: None,
            phase: Phase::Ready,
            #[cfg(test)]
            final_poll: None,
        })
    }
}
impl CurrentRotationPlanningOperation {
    pub fn plan(
        &mut self,
        host: &CurrentRestoreHostAttestation,
        cancel: &AtomicBool,
    ) -> Result<()> {
        self.plan_inner(host, cancel, 0)
    }
    fn plan_inner(
        &mut self,
        host: &CurrentRestoreHostAttestation,
        cancel: &AtomicBool,
        fault: u8,
    ) -> Result<()> {
        #[cfg(not(test))]
        let _ = fault;
        if self.phase != Phase::Ready {
            return Err(Error("rotation planning already attempted".into()));
        }
        self.phase = Phase::Failed;
        self.operation.live(cancel)?;
        let store = &self.operation.owner.session.store;
        let RotationState::Ready(image) = &self.operation.state else {
            return Err(Error("planning image missing".into()));
        };
        if !store.db.is_autocommit()
            || !image.planning_db().is_autocommit()
            || store.transaction_poisoned.get()
        {
            return Err(Error("planning transaction unavailable".into()));
        }
        // Both observers precede BEGIN and every subsequent entry/attachment poll.
        self.admission.source.install(&store.db)?;
        self.admission.copy.install(image.planning_db())?;
        let _budget = self
            .admission
            .budget
            .enter()
            .map_err(|e| Error(format!("planning budget: {e:?}")))?;
        *store.catalog_operation_budget.borrow_mut() = Some(self.admission.budget.clone());
        let allocation = &store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("planning allocation missing".into()))?
            .allocation;
        self.attachment = Some(allocation.operation(&store.db, &self.admission.budget)?);
        let _allocation = allocation.enter()?;
        store.transaction_available()?;
        self.admission.source.check(&store.db)?;
        self.admission
            .source
            .boundary(&store.db, EndpointBoundary::Begin)?;
        self.admission
            .copy
            .boundary(image.planning_db(), EndpointBoundary::Begin)?;
        // Retain the normal post-BEGIN entry poll, now already observed.
        let _entered = self
            .admission
            .budget
            .enter()
            .map_err(|e| Error(format!("planning budget: {e:?}")))?;
        self.image_allocation = Some(
            self.operation
                .owner
                .session
                .image_allocation()?
                .into_owned(),
        );
        let proof = CurrentImageRotationAdmission {
            owner: &self.operation.owner,
            history: &self.operation.history,
            generation: self.operation.generation,
            deadline: self.operation.deadline,
        };
        let plan = image.prepare_current_rotation(
            store,
            &proof,
            host,
            cancel,
            &self.admission.source,
            &self.admission.copy,
            self.image_allocation
                .as_mut()
                .ok_or_else(|| Error("planning reservation missing".into()))?,
        )?;
        #[cfg(test)]
        if let Some(phase) = &self.final_poll {
            phase.store(true, std::sync::atomic::Ordering::Release);
        }
        self.admission
            .budget
            .charge(BudgetCharge::default())
            .map_err(|e| Error(format!("planning final budget: {e:?}")))?;
        // The final poll may reenter. Verify sticky observers AFTER that poll.
        self.admission.source.check(&store.db)?;
        self.admission.copy.check(image.planning_db())?;
        self.operation.live(cancel)?;
        self.plan = Some(plan);
        #[cfg(test)]
        if fault == 1 {
            panic!("controlled retained-plan construction unwind");
        }
        self.phase = Phase::Planned;
        Ok(())
    }
    pub fn facts(&self) -> Option<CurrentRotationPlanFacts<'_>> {
        if self.phase != Phase::Planned {
            return None;
        }
        let RotationState::Ready(image) = &self.operation.state else {
            return None;
        };
        if self
            .operation
            .owner
            .session
            .store
            .transaction_poisoned
            .get()
            || self.admission.budget.failure().is_some()
            || self
                .admission
                .source
                .check(&self.operation.owner.session.store.db)
                .is_err()
            || self.admission.copy.check(image.planning_db()).is_err()
        {
            return None;
        }
        let plan = self.plan.as_ref()?;
        Some(CurrentRotationPlanFacts {
            restore_id: plan.restore_id(),
            to_incarnation: plan.to_incarnation(),
            origin_json: plan.origin_json(),
            generation: plan.generation(),
            transition_digest: &self.operation.owner.transition_digest,
        })
    }
    /// Cleanup borrows the retained owner. Failure leaves the whole owner here;
    /// it never returns a merely poisoned session while observer state is live.
    pub fn settle(&mut self) -> Result<()> {
        self.settle_inner(0)
    }
    fn settle_inner(&mut self, fault: u8) -> Result<()> {
        #[cfg(not(test))]
        let _ = fault;
        if self.phase == Phase::Settled {
            return Ok(());
        }
        self.phase = Phase::Failed;
        let store = &self.operation.owner.session.store;
        let RotationState::Ready(image) = &self.operation.state else {
            return Err(Error("planning image missing".into()));
        };
        if store.transaction_poisoned.get() && self.admission.source.installed() {
            return Err(Error(
                "poisoned planning observer cannot restore authority".into(),
            ));
        }
        #[cfg(test)]
        if fault == 1 {
            return Err(Error(
                "synthetic planning pre-rollback cleanup boundary".into(),
            ));
        }
        if self.admission.copy.installed() {
            let _cleanup = crate::native_writer_vm::cleanup(image.planning_db());
            if !image.planning_db().is_autocommit() {
                self.admission
                    .copy
                    .boundary(image.planning_db(), EndpointBoundary::Rollback)?;
            }
            self.admission.copy.restore(image.planning_db(), false)?;
        }
        if self.admission.source.installed() {
            let _cleanup = crate::native_writer_vm::cleanup(&store.db);
            if !store.db.is_autocommit() {
                if let Err(error) = self
                    .admission
                    .source
                    .boundary(&store.db, EndpointBoundary::Rollback)
                {
                    store.poison_transaction_owner();
                    return Err(error);
                }
            }
            #[cfg(test)]
            if fault == 2 {
                store.poison_transaction_owner();
                return Err(Error(
                    "synthetic planning pre-restoration cleanup boundary".into(),
                ));
            }
            if let Err(error) = self.admission.source.restore(&store.db, true) {
                store.poison_transaction_owner();
                return Err(error);
            }
        }
        self.plan = None;
        self.image_allocation = None;
        self.attachment = None;
        self.phase = Phase::Settled;
        Ok(())
    }
    /// Moves only. Unsettled ownership returns intact; a settled plan never
    /// reconstructs an old reusable image or permits a second attempt.
    pub fn finish(self) -> std::result::Result<CompletedCatalogSession, Self> {
        if self.phase != Phase::Settled {
            return Err(self);
        }
        Ok(self.operation.owner)
    }
}

#[cfg(test)]
#[path = "planning_tests.rs"]
mod tests;
