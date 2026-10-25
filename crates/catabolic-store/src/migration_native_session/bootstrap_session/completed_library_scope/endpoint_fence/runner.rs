//! Fixed top-level endpoint transaction. Generic Store transaction APIs are
//! untouched. The observer precedes BEGIN and every post-BEGIN embedding poll.
use super::*;
use crate::catalog_source_succession::{CurrentFileEndpoint, FileEndpointRequest};
use authorizer::{EndpointBoundaries, EndpointBoundary};
pub(in super::super) enum EndpointOperation<'a> {
    #[cfg(test)]
    Probe {
        allocation_failure: bool,
        panic: bool,
    },
    Capture {
        scope: &'a LibraryScopeSnapshot,
        source: &'a str,
        request: &'a FileEndpointRequest,
    },
    Revalidate {
        scope: &'a LibraryScopeSnapshot,
        facts: &'a CurrentFileEndpoint,
    },
}
#[derive(Debug)]
pub(in super::super) enum EndpointOutput {
    Captured(CurrentFileEndpoint),
    Revalidated,
}
fn sql_boundary(
    store: &Store,
    state: &EndpointBoundaries,
    boundary: EndpointBoundary,
) -> Result<()> {
    struct Disarm<'a>(&'a EndpointBoundaries);
    impl Drop for Disarm<'_> {
        fn drop(&mut self) {
            self.0.disarm();
        }
    }
    let sql = match boundary {
        EndpointBoundary::Begin => "BEGIN IMMEDIATE",
        EndpointBoundary::Commit => "COMMIT",
        EndpointBoundary::Rollback => "ROLLBACK",
    };
    state.arm(boundary);
    let _disarm = Disarm(state);
    let result = store.db.execute_batch(sql);
    if !state.consumed() {
        state.attempted.store(true, Ordering::Release);
        return Err(refused());
    }
    result?;
    Ok(())
}
struct Cleanup<'a> {
    store: &'a Store,
    state: &'a EndpointBoundaries,
    armed: bool,
}
impl Cleanup<'_> {
    fn rollback(&self) -> Result<()> {
        let _cleanup = crate::native_writer_vm::cleanup(&self.store.db);
        if self.store.db.is_autocommit() {
            return Ok(());
        }
        if let Err(error) = sql_boundary(self.store, self.state, EndpointBoundary::Rollback) {
            let _ = sql_boundary(self.store, self.state, EndpointBoundary::Rollback);
            self.store.poison_transaction_owner();
            return Err(error);
        }
        Ok(())
    }
}
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.rollback();
        }
    }
}

pub(in super::super) fn run(
    owner: &CompletedCatalogSession,
    operation: EndpointOperation<'_>,
) -> Result<EndpointOutput> {
    let store = &owner.session.store;
    if !store.db.is_autocommit()
        || store.schema_version != 41
        || !store.writable
        || !store.owns_writer_lock()
        || store.transaction_poisoned.get()
    {
        return Err(refused());
    }
    let offline = store.offline_bootstrap.as_ref().ok_or_else(refused)?;
    // Exactly the parent selected by begin_transaction, never a reset local
    // operation. All prelude polling occurs before this operation's WRITE.
    let parent = OperationBudget::current()
        .map_or_else(crate::catalog_search_index::default_operation_budget, Ok)?;
    let _prelude = parent
        .enter()
        .map_err(|e| Error(format!("operation budget: {e:?}")))?;
    let mut budget = ProofBudget::new(parent.clone());
    let origins_bytes = scope_history::origins_guard_charge(&mut budget)?;
    let fixed = std::mem::size_of::<EndpointReadFence<'_>>()
        .checked_add(std::mem::size_of::<EndpointBoundaries>())
        .and_then(|n| {
            n.checked_add(std::mem::size_of::<AtomicBool>() + 4 * std::mem::size_of::<usize>())
        })
        .ok_or_else(refused)?;
    budget.charge(BudgetCharge {
        bytes: fixed,
        values: 2,
        work: 4,
        ..Default::default()
    })?;
    let retained = origins_bytes.checked_add(fixed).ok_or_else(refused)?;
    let _hook_storage = offline
        .allocation
        .reserve(&store.db, u64::try_from(retained).map_err(|_| refused())?)?;
    let attempted = Arc::new(AtomicBool::new(false));
    let boundaries = Arc::new(EndpointBoundaries::new(attempted.clone()));
    // Preserve the deny-all probe. No proof or original WRITE exists yet.
    let hook = authorizer::Guard::for_store(store)?;
    if !store.db.is_autocommit() || store.transaction_poisoned.get() {
        return Err(refused());
    }
    hook.replace_endpoint_operation(boundaries.clone())?;
    // Declared before cleanup: attachment and hook survive error/unwind rollback.
    let mut _allocation = None;
    let mut cleanup = Cleanup {
        store,
        state: &boundaries,
        armed: true,
    };
    let outcome = (|| {
        store.transaction_available()?;
        if !store.db.is_autocommit()
            || !store.owns_writer_lock()
            || attempted.load(Ordering::Acquire)
        {
            return Err(refused());
        }
        parent
            .check()
            .map_err(|e| Error(format!("operation budget: {e:?}")))?;
        *store.catalog_operation_budget.borrow_mut() = Some(parent.clone());
        sql_boundary(store, &boundaries, EndpointBoundary::Begin)?;
        // Preserve the original enter poll and allocation attachment order, now
        // observed. No expected transaction slot remains armed during either call.
        let _active = parent
            .enter()
            .map_err(|e| Error(format!("operation budget: {e:?}")))?;
        #[cfg(test)]
        if matches!(
            &operation,
            EndpointOperation::Probe {
                allocation_failure: true,
                ..
            }
        ) {
            // Exercise a genuine pre-closure aggregate refusal, not a fake budget error.
            let _impossible = offline.allocation.reserve(&store.db, 256 * 1024 * 1024)?;
        }
        _allocation = Some(offline.allocation_operation(store, &parent)?);
        let fence = EndpointReadFence {
            owner,
            handle: handle(store),
            operation: parent.clone(),
            changes: store.db.total_changes(),
            main_cookie: store
                .db
                .query_row("PRAGMA main.schema_version", [], |r| r.get(0))?,
            temp_cookie: store
                .db
                .query_row("PRAGMA temp.schema_version", [], |r| r.get(0))?,
            attempted: attempted.clone(),
            guard: None,
        };
        fence.check(owner, &mut budget)?;
        owner.validate_current()?;
        let result = match operation {
            #[cfg(test)]
            EndpointOperation::Probe { panic, .. } => {
                if panic {
                    panic!("controlled fixed endpoint unwind");
                }
                EndpointOutput::Revalidated
            }
            EndpointOperation::Capture {
                scope,
                source,
                request,
            } => {
                let fresh = EndpointFreshFacts::capture(&fence, scope, &mut budget)?;
                EndpointOutput::Captured(
                    crate::catalog_source_succession::file_endpoint::capture_fresh_in(
                        store,
                        &fresh,
                        source,
                        request,
                        &mut budget,
                    )?,
                )
            }
            EndpointOperation::Revalidate { scope, facts } => {
                let fresh = EndpointFreshFacts::capture(&fence, scope, &mut budget)?;
                crate::catalog_source_succession::file_endpoint::revalidate_fresh_in(
                    store,
                    &fresh,
                    facts,
                    &mut budget,
                )?;
                EndpointOutput::Revalidated
            }
        };
        fence.check(owner, &mut budget)?;
        owner.validate_current()?;
        // Same generic finish preconditions, flush, allocation and fixed COMMIT VM
        // admission. An unexpected dirty flush cannot write through the observer.
        store.transaction_available()?;
        if store.db.is_autocommit() {
            return Err(refused());
        }
        crate::catalog_search_index::flush_before_commit(
            &store.db,
            &store.catalog_operation_budget()?,
        )?;
        offline.allocation_before_commit(store)?;
        if let Some(current) = crate::native_writer_vm::current_budget(&store.db) {
            current
                .check()
                .map_err(|e| Error(format!("operation budget: {e:?}")))?;
        }
        fence.check(owner, &mut budget)?;
        let commit = crate::native_writer_vm::prepare_commit(&store.db)?;
        let committed = sql_boundary(store, &boundaries, EndpointBoundary::Commit);
        let exceeded = commit.as_ref().is_some_and(|c| !c.within_prepaid_bound());
        drop(commit);
        if exceeded {
            store.poison_transaction_owner();
        }
        committed?;
        // A durable read-only COMMIT is not claimed to roll back. Unlike a generic
        // durable mutation result, an endpoint proof must not escape a poisoned owner.
        if exceeded
            || store.transaction_poisoned.get()
            || attempted.load(Ordering::Acquire)
            || parent.failure().is_some()
            || !store.db.is_autocommit()
        {
            return Err(refused());
        }
        Ok(result)
    })();
    let result = match outcome {
        Ok(result) => Ok(result),
        Err(error) => match cleanup.rollback() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(Error(format!("{error}; native rollback failed: {cleanup}"))),
        },
    };
    cleanup.armed = false;
    hook.restore()?;
    result
}
