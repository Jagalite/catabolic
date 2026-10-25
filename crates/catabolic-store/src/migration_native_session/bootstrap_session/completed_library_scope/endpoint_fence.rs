//! One fixed read-only phase within one genuine completed WRITE transaction.
//! No facts survive the operation. Each call still freshly verifies scope,
//! complete source history, and the current endpoint. Only the second identical
//! scope recapture is replaced by continuity, never by a cross-call cache.
use super::*;
use crate::{
    catalog_identity_mutation::StructuralReadBudget,
    catalog_source_succession::{authorizer, scope_history},
    operation_budget::{BudgetCharge, OperationBudget},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
fn refused() -> Error {
    Error("completed endpoint read continuity refused".into())
}
fn handle(store: &Store) -> usize {
    // Identity only; never used to dereference or operate on a SQLite handle.
    unsafe { store.db.handle() as usize }
}
pub(super) struct EndpointReadFence<'a> {
    owner: &'a CompletedCatalogSession,
    handle: usize,
    operation: OperationBudget,
    changes: u64,
    main_cookie: i64,
    temp_cookie: i64,
    attempted: Arc<AtomicBool>,
    guard: Option<authorizer::Guard<'a>>,
}
impl<'a> EndpointReadFence<'a> {
    pub(super) fn new(
        owner: &'a CompletedCatalogSession,
        budget: &mut ProofBudget,
    ) -> Result<Self> {
        let store = &owner.session.store;
        let operation = store.catalog_operation_budget()?;
        if store.schema_version != 41
            || !store.owns_writer_lock()
            || store.transaction_poisoned.get()
            || store.db.is_autocommit()
            || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
            || !budget.uses_operation(&operation)
            || OperationBudget::current().is_none_or(|b| !b.same_owner(&operation))
        {
            return Err(refused());
        }
        scope_history::origins_guard_charge(budget)?;
        budget.charge(BudgetCharge {
            bytes: std::mem::size_of::<Self>()
                + 2 * std::mem::size_of::<usize>()
                + std::mem::size_of::<AtomicBool>(),
            values: 1,
            work: 4,
            ..Default::default()
        })?;
        let attempted = Arc::new(AtomicBool::new(false));
        let guard = authorizer::Guard::for_store(store)?;
        guard.replace_endpoint_reads(attempted.clone())?;
        // All setup polling precedes these captured facts; every subsequent
        // SQLite action is fenced before the first fresh scope verification.
        let changes = store.db.total_changes();
        let main_cookie = store
            .db
            .query_row("PRAGMA main.schema_version", [], |r| r.get(0))?;
        let temp_cookie = store
            .db
            .query_row("PRAGMA temp.schema_version", [], |r| r.get(0))?;
        let fence = Self {
            owner,
            handle: handle(store),
            operation,
            changes,
            main_cookie,
            temp_cookie,
            attempted,
            guard: Some(guard),
        };
        fence.check(owner, budget)?;
        Ok(fence)
    }
    fn check(&self, owner: &CompletedCatalogSession, budget: &mut ProofBudget) -> Result<()> {
        // Poll first, then inspect continuity. The fixed authorizer itself never
        // polls, reenters SQLite, or lends the Store to an embedding callback.
        budget.charge(BudgetCharge {
            work: 7,
            ..Default::default()
        })?;
        let store = &owner.session.store;
        if !std::ptr::eq(owner, self.owner)
            || handle(store) != self.handle
            || !budget.uses_operation(&self.operation)
            || OperationBudget::current().is_none_or(|b| !b.same_owner(&self.operation))
            || !store
                .catalog_operation_budget()?
                .same_owner(&self.operation)
            || store.transaction_poisoned.get()
            || !store.owns_writer_lock()
            || self.attempted.load(Ordering::Acquire)
        {
            return Err(refused());
        }
        let main_cookie: i64 = store
            .db
            .query_row("PRAGMA main.schema_version", [], |r| r.get(0))?;
        let temp_cookie: i64 = store
            .db
            .query_row("PRAGMA temp.schema_version", [], |r| r.get(0))?;
        if store.db.is_autocommit()
            || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
            || store.db.total_changes() != self.changes
            || main_cookie != self.main_cookie
            || temp_cookie != self.temp_cookie
            || self.attempted.load(Ordering::Acquire)
            || self.operation.failure().is_some()
        {
            return Err(refused());
        }
        // No user callback follows this final parent deadline check; check()
        // itself can poll, so its effects are enclosed by a second scalar fence.
        self.operation.check().map_err(|_| refused())?;
        if store.transaction_poisoned.get()
            || self.attempted.load(Ordering::Acquire)
            || self.operation.failure().is_some()
            || store.db.is_autocommit()
            || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
            || handle(store) != self.handle
            || store.db.total_changes() != self.changes
            || OperationBudget::current().is_none_or(|b| !b.same_owner(&self.operation))
        {
            return Err(refused());
        }
        Ok(())
    }
    pub(super) fn finish(
        mut self,
        owner: &CompletedCatalogSession,
        budget: &mut ProofBudget,
    ) -> Result<()> {
        self.check(owner, budget)?;
        self.guard.take().ok_or_else(refused)?.restore()
    }
}
#[cfg(test)]
#[path = "endpoint_fence/tests.rs"]
mod tests;

#[path = "endpoint_fence/runner.rs"]
mod runner;
pub(super) use runner::{EndpointOperation, EndpointOutput, run};

#[path = "endpoint_fence/fresh_facts.rs"]
mod fresh_facts;
pub(crate) use fresh_facts::EndpointFreshFacts;
#[cfg(test)]
pub(super) use fresh_facts::assert_fresh_facts_for_test;
