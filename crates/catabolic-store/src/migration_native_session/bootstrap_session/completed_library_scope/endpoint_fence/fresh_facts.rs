//! Non-escaping fresh source headers inside the fixed endpoint WRITE corridor.
use super::*;

/// Not a cross-call cache or a source grant. Only this operation's genuine
/// fence can construct it, after complete fresh scope equality. It borrows the
/// fence and cannot outlive the guarded transaction or escape in endpoint facts.
pub(crate) struct EndpointFreshFacts<'fence, 'owner> {
    fence: &'fence EndpointReadFence<'owner>,
    scope: LibraryScopeSnapshot,
}
impl<'fence, 'owner> EndpointFreshFacts<'fence, 'owner> {
    pub(super) fn capture(
        fence: &'fence EndpointReadFence<'owner>,
        old: &LibraryScopeSnapshot,
        budget: &mut ProofBudget,
    ) -> Result<Self> {
        fence.check(fence.owner, budget)?;
        // The fresh scope's construction was already charged to the cumulative
        // parent AllocationSink, which remains attached until this operation
        // settles. Extending its lexical lifetime does not allocate another
        // scope or release that admission. Charge only the new wrapper/actual
        // bounded header moves; no repeated tree measurement or clone.
        budget.charge(BudgetCharge {
            bytes: std::mem::size_of::<&EndpointReadFence<'_>>(),
            work: 1 + (3 * std::mem::size_of::<Self>()).div_ceil(64),
            ..Default::default()
        })?;
        let scope = library_scope::capture_revalidated_in(&fence.owner.session.store, old, budget)?;
        fence.check(fence.owner, budget)?;
        Ok(Self { fence, scope })
    }
    pub(crate) fn scope(&self) -> &LibraryScopeSnapshot {
        &self.scope
    }
    pub(crate) fn check(&self, store: &Store, budget: &mut ProofBudget) -> Result<()> {
        if !std::ptr::eq(store, &self.fence.owner.session.store) {
            return Err(refused());
        }
        self.fence.check(self.fence.owner, budget)
    }
    pub(crate) fn require_source(
        &self,
        store: &Store,
        source: &str,
        budget: &mut ProofBudget,
    ) -> Result<()> {
        self.check(store, budget)?;
        let mut found = false;
        for entry in self.scope.sources() {
            budget.charge(BudgetCharge {
                work: 1 + entry.native_source().len().min(source.len()).div_ceil(64),
                ..Default::default()
            })?;
            if entry.native_source() == source {
                if found {
                    return Err(refused());
                }
                found = true;
            }
        }
        if !found {
            return Err(refused());
        }
        Ok(())
    }
}

#[cfg(test)]
pub(in super::super) fn assert_fresh_facts_for_test(
    scope: &CompletedLibraryScope<'_>,
    foreign: &CompletedCatalogSession,
    source: &str,
    request: &crate::catalog_source_succession::FileEndpointRequest,
) {
    let owner = scope.owner;
    let store = &owner.session.store;
    store
        .transaction_detailed::<_, Error>(|_| {
            owner.validate_current()?;
            let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
            let fence = EndpointReadFence::new(owner, &mut budget)?;
            let fresh = EndpointFreshFacts::capture(&fence, &scope.facts, &mut budget)?;
            assert_eq!(fresh.scope(), &scope.facts);
            assert!(fresh.check(&foreign.session.store, &mut budget).is_err());
            let mut wrong_operation =
                ProofBudget::new(crate::catalog_search_index::default_operation_budget()?);
            assert!(fresh.check(store, &mut wrong_operation).is_err());
            assert!(
                fresh
                    .require_source(store, "not-a-scope-source", &mut budget)
                    .is_err()
            );
            fresh.check(store, &mut budget)?;
            crate::catalog_source_succession::file_endpoint::assert_fresh_occurrence_parity_in(
                store,
                &fresh,
                source,
                &mut budget,
            )?;
            drop(fresh);
            fence.finish(owner, &mut budget)
        })
        .unwrap();
    // These are real attempted writes under the minted proof, not a fabricated
    // positive or an admitted scan. Neither a metadata nor physical tuple
    // change can leave a reusable fresh header after the denial is swallowed.
    for sql in [
        "UPDATE main.items SET metadata=json_set(metadata,'$.title','changed') WHERE kind='custom:motion_library'",
        "UPDATE main.observations SET mtime_ns=mtime_ns+1 WHERE status='present'",
        "ROLLBACK",
    ] {
        let reached = std::cell::Cell::new(false);
        let result: Result<()> = store.transaction_detailed(|_| {
            owner.validate_current()?;
            let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
            let fence = EndpointReadFence::new(owner, &mut budget)?;
            let fresh = EndpointFreshFacts::capture(&fence, &scope.facts, &mut budget)?;
            assert!(store.db.execute_batch(sql).is_err());
            assert_eq!(
                fresh.check(store, &mut budget).unwrap_err().to_string(),
                "completed endpoint read continuity refused"
            );
            let error = crate::catalog_source_succession::file_endpoint::capture_fresh_in(
                store,
                &fresh,
                source,
                request,
                &mut budget,
            )
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "completed endpoint read continuity refused"
            );
            reached.set(true);
            drop(fresh);
            drop(fence);
            Err(Error("rollback controlled fresh endpoint attempt".into()))
        });
        assert_eq!(
            result.unwrap_err().to_string(),
            "rollback controlled fresh endpoint attempt"
        );
        assert!(reached.get());
        assert!(store.db.is_autocommit());
        scope.revalidate().unwrap();
    }
}
