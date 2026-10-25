//! Lexical preparation and pre-effect passes inside fixed synchronous owners.
//! This is not a cache for caller-controlled Store/callback execution. The closed
//! owner retains the connection; the composed authorizer additionally denies
//! transaction/savepoint and attachment boundaries until consumed or unwound.
use super::*;
use crate::{catalog_nfo_attribution::ProofBudget, operation_budget::OperationBudget};
use catabolic_core::catalog_command::CommandEnvelope;
use rusqlite::hooks::{AuthAction, Authorization};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct RetainedSourcePass<'a, 'workspace> {
    control: Option<&'a crate::migration::BootstrapWorkspace<'workspace>>,
    store: &'a Store,
    cancel: &'a AtomicBool,
    operation: OperationBudget,
    changes: u64,
    cookie: i64,
    temp_cookie: i64,
    boundary_attempted: Arc<AtomicBool>,
    guard: Option<authorizer::Guard<'a>>,
    captured: CapturedSuccession,
}
impl<'a, 'workspace> RetainedSourcePass<'a, 'workspace> {
    // Only fixed source preparation/receipt owners construct this inner fence. No owned
    // proof, raw connection or arbitrary callback is exported by its facade.
    pub(super) fn new(
        store: &'a Store,
        profile: &str,
        source: &str,
        cancel: &'a AtomicBool,
        budget: &mut ProofBudget,
        control: Option<&'a crate::migration::BootstrapWorkspace<'workspace>>,
    ) -> Result<Self> {
        store.transaction_available_with_control_workspace(control)?;
        if store.schema_version != 41
            || store.db.is_autocommit()
            || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
        {
            return Err(refuse("pre-effect pass requires exact retained writer"));
        }
        let operation = store.catalog_operation_budget()?;
        if !budget.uses_operation(&operation)
            || OperationBudget::current().is_none_or(|active| !active.same_owner(&operation))
        {
            return Err(refuse("pre-effect operation owner differs"));
        }
        let boundary_attempted = Arc::new(AtomicBool::new(false));
        let attempted = boundary_attempted.clone();
        let guard = authorizer::Guard::for_store(store)?;
        guard.replace(move |context| {
            if matches!(
                context.action,
                AuthAction::Transaction { .. }
                    | AuthAction::Savepoint { .. }
                    | AuthAction::Attach { .. }
                    | AuthAction::Detach { .. }
            ) {
                attempted.store(true, Ordering::Release);
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        })?;
        budget.charge(BudgetCharge {
            work: 3,
            ..Default::default()
        })?;
        let changes = store.db.total_changes();
        let cookie = store
            .db
            .query_row("PRAGMA schema_version", [], |r| r.get(0))?;
        let temp_cookie = store
            .db
            .query_row("PRAGMA temp.schema_version", [], |r| r.get(0))?;
        let captured = capture_source_succession_in_with_control_workspace(store, profile, source, budget, control)?;
        let pass = Self {
            control,
            store,
            cancel,
            operation,
            changes,
            cookie,
            temp_cookie,
            boundary_attempted,
            guard: Some(guard),
            captured,
        };
        pass.check(store, budget)?;
        Ok(pass)
    }
    pub(crate) fn check(
        &self,
        store: &Store,
        budget: &mut ProofBudget,
    ) -> Result<&CapturedSuccession> {
        // Charge/poll before inspecting the retained fence. No host callback is
        // run while SQLite's authorizer executes, and none receives this Store.
        budget.charge(BudgetCharge {
            work: 5,
            ..Default::default()
        })?;
        if self.cancel.load(Ordering::Acquire) {
            return Err(refuse("content verification cancelled"));
        }
        if !std::ptr::eq(store, self.store)
            || !budget.uses_operation(&self.operation)
            || OperationBudget::current().is_none_or(|active| !active.same_owner(&self.operation))
            || !store
                .catalog_operation_budget()?
                .same_owner(&self.operation)
            || self.boundary_attempted.load(Ordering::Acquire)
        {
            return Err(refuse("pre-effect pass owner or boundary changed"));
        }
        store.transaction_available_with_control_workspace(self.control)?;
        let cookie: i64 = store
            .db
            .query_row("PRAGMA schema_version", [], |r| r.get(0))?;
        let temp_cookie: i64 = store
            .db
            .query_row("PRAGMA temp.schema_version", [], |r| r.get(0))?;
        if store.db.is_autocommit()
            || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
            || store.db.total_changes() != self.changes
            || cookie != self.cookie
            || temp_cookie != self.temp_cookie
            || self.boundary_attempted.load(Ordering::Acquire)
            || self.cancel.load(Ordering::Acquire)
        {
            return Err(refuse("pre-effect retained structure changed"));
        }
        Ok(&self.captured)
    }
    // Every caller performs its final typed check immediately before consuming.
    // No callback or database mutation occurs between that check and restoration.
    fn into_capture(mut self) -> Result<CapturedSuccession> {
        self.guard
            .take()
            .expect("owned source continuity authorizer")
            .restore()?;
        Ok(self.captured)
    }
}

pub(crate) struct PreEffectSourcePass<'a, 'workspace> {
    command: &'a CommandEnvelope,
    inner: RetainedSourcePass<'a, 'workspace>,
}
impl<'a, 'workspace> PreEffectSourcePass<'a, 'workspace> {
    pub(super) fn new(
        store: &'a Store,
        command: &'a CommandEnvelope,
        profile: &str,
        source: &str,
        cancel: &'a AtomicBool,
        budget: &mut ProofBudget,
    ) -> Result<Self> {
        Self::new_with_control_workspace(store, command, profile, source, cancel, budget, None)
    }
    pub(super) fn new_with_control_workspace(
        store: &'a Store,
        command: &'a CommandEnvelope,
        profile: &str,
        source: &str,
        cancel: &'a AtomicBool,
        budget: &mut ProofBudget,
        control: Option<&'a crate::migration::BootstrapWorkspace<'workspace>>,
    ) -> Result<Self> {
        if command.payload()["profile"].as_str() != Some(profile)
            || command.payload()["source"].as_str() != Some(source)
        {
            return Err(refuse("pre-effect source selector differs from envelope"));
        }
        let inner = RetainedSourcePass::new(store, profile, source, cancel, budget, control)?;
        if inner.captured.generation != command.expected_generation()
            || inner.captured.catalog_id != command.key().catalog_id()
            || inner.captured.incarnation != command.admission_incarnation()
        {
            return Err(refuse("fixed succession envelope differs"));
        }
        Ok(Self { command, inner })
    }
    pub(crate) fn check(
        &self,
        store: &Store,
        command: &CommandEnvelope,
        budget: &mut ProofBudget,
    ) -> Result<&CapturedSuccession> {
        let captured = self.inner.check(store, budget)?;
        if !std::ptr::eq(command, self.command) {
            return Err(refuse("pre-effect pass owner or boundary changed"));
        }
        Ok(captured)
    }
    pub(super) fn finish(
        self,
        store: &Store,
        command: &CommandEnvelope,
        budget: &mut ProofBudget,
    ) -> Result<CapturedSuccession> {
        self.check(store, command, budget)?;
        self.inner.into_capture()
    }
}

/// A separate descriptive preparation owner, never a command grant. The fixed
/// completed facade authenticates its admission before minting this pass. Its
/// captured generation and complete closure cannot change during the guarded
/// retained WRITE span. Only fixed descriptor custody borrows that capture.
/// No arbitrary callback is admitted by the production preparation API.
pub(super) struct SourcePreparationPass<'a, 'workspace> {
    inner: RetainedSourcePass<'a, 'workspace>,
    candidate: &'a std::path::Path,
}
impl<'a, 'workspace> SourcePreparationPass<'a, 'workspace> {
    pub(super) fn new(
        store: &'a Store,
        admission: &crate::migration::CompletedSourcePreparationAdmission<'a, 'workspace>,
        profile: &str,
        source: &str,
        candidate: &'a std::path::Path,
        cancel: &'a AtomicBool,
        budget: &mut ProofBudget,
    ) -> Result<Self> {
        admission.check(store, profile, source, candidate)?;
        Self::capture(store, profile, source, candidate, cancel, budget, admission.control_workspace())
    }
    // Private to this module; the production constructor above requires actual
    // completed-session admission. Tests use the exact same fence/descriptor
    // owner, without manufacturing a production admission token.
    fn capture(
        store: &'a Store,
        profile: &str,
        source: &str,
        candidate: &'a std::path::Path,
        cancel: &'a AtomicBool,
        budget: &mut ProofBudget,
        control: Option<&'a crate::migration::BootstrapWorkspace<'workspace>>,
    ) -> Result<Self> {
        Ok(Self {
            inner: RetainedSourcePass::new(store, profile, source, cancel, budget, control)?,
            candidate,
        })
    }
    #[cfg(test)]
    pub(super) fn for_test(
        store: &'a Store,
        profile: &str,
        source: &str,
        candidate: &'a std::path::Path,
        cancel: &'a AtomicBool,
        budget: &mut ProofBudget,
    ) -> Result<Self> {
        Self::capture(store, profile, source, candidate, cancel, budget, None)
    }
    pub(super) fn check(
        &self,
        store: &Store,
        profile: &str,
        source: &str,
        candidate: &std::path::Path,
        budget: &mut ProofBudget,
    ) -> Result<&CapturedSuccession> {
        let captured = self.inner.check(store, budget)?;
        if profile != captured.profile || source != captured.source || candidate != self.candidate {
            return Err(refuse("source preparation pass selector differs"));
        }
        Ok(captured)
    }
    pub(super) fn finish(
        self,
        store: &Store,
        profile: &str,
        source: &str,
        candidate: &std::path::Path,
        budget: &mut ProofBudget,
    ) -> Result<CapturedSuccession> {
        self.check(store, profile, source, candidate, budget)?;
        self.inner.into_capture()
    }
}

#[cfg(test)]
pub(crate) fn assert_fence_for_test(
    store: &Store,
    command: &CommandEnvelope,
    other: &Store,
    case: &str,
    budget: &mut ProofBudget,
) -> Result<()> {
    // A test-only enclosing savepoint restores real producer effects. It is
    // outside the pass; no production callback or mutable proof is exported.
    store.db.execute_batch("SAVEPOINT source_pass_fixture")?;
    let stop = AtomicBool::new(false);
    if case == "unwind" {
        let acquired = std::cell::Cell::new(false);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _pass = PreEffectSourcePass::new(store, command, "default", "media", &stop, budget)
                .unwrap();
            acquired.set(true);
            panic!("controlled source pass unwind");
        }));
        assert!(acquired.get(), "unwind must follow actual pass acquisition");
        assert!(outcome.is_err());
    } else {
        let pass = PreEffectSourcePass::new(store, command, "default", "media", &stop, budget)?;
        assert_eq!(pass.check(store, command, budget)?.files.len(), 2);
        match case {
            "unchanged" => {
                let captured = pass.finish(store, command, budget)?;
                assert_eq!(captured.files.len(), 2);
            }
            "write" | "write_rollback" => {
                assert_eq!(store.db.execute("UPDATE bindings SET inode=inode+1 WHERE profile='default' AND kind='source' AND owner='media'", [])?, 1);
                if case == "write_rollback" {
                    assert!(
                        store
                            .db
                            .execute_batch("ROLLBACK TO source_pass_fixture")
                            .is_err()
                    );
                }
                assert!(pass.check(store, command, budget).is_err());
                drop(pass);
            }
            "schema" => {
                store
                    .db
                    .execute_batch("CREATE TABLE source_pass_fixture_schema(id INTEGER)")?;
                assert!(pass.check(store, command, budget).is_err());
                drop(pass);
            }
            "temp_schema" => {
                let main: i64 = store
                    .db
                    .query_row("PRAGMA main.schema_version", [], |r| r.get(0))?;
                let changes = store.db.total_changes();
                store.db.execute_batch(
                    "CREATE TEMP VIEW bindings AS SELECT * FROM main.bindings WHERE 0",
                )?;
                assert_eq!(
                    store
                        .db
                        .query_row("PRAGMA main.schema_version", [], |r| r.get::<_, i64>(0))?,
                    main
                );
                assert_eq!(store.db.total_changes(), changes);
                assert_eq!(
                    store
                        .db
                        .query_row("SELECT count(*) FROM bindings", [], |r| r.get::<_, i64>(0))?,
                    0
                );
                assert!(pass.check(store, command, budget).is_err());
                drop(pass);
            }
            "commit" | "rollback" | "savepoint" | "attach" | "detach" => {
                let sql = match case {
                    "commit" => "COMMIT",
                    "rollback" => "ROLLBACK",
                    "attach" => "ATTACH DATABASE ':memory:' AS forbidden_pass_attachment",
                    "detach" => "DETACH DATABASE forbidden_pass_attachment",
                    _ => "SAVEPOINT forbidden_pass_boundary",
                };
                assert!(store.db.execute_batch(sql).is_err());
                assert!(
                    pass.check(store, command, budget).is_err(),
                    "caught boundary denial must be sticky"
                );
                drop(pass);
            }
            "owner" => {
                assert!(pass.check(other, command, budget).is_err());
                assert!(
                    PreEffectSourcePass::new(
                        store,
                        command,
                        "default",
                        "other-source",
                        &stop,
                        budget
                    )
                    .is_err()
                );
                let other_command = command.clone();
                assert!(pass.check(store, &other_command, budget).is_err());
                let different = OperationBudget::new(
                    std::time::Duration::from_secs(5),
                    crate::operation_budget::BudgetLimits {
                        items: 100,
                        values: 100,
                        bytes: 1024,
                        work: 100,
                    },
                )
                .expect("test operation limits");
                let mut different_budget = ProofBudget::new(different);
                assert!(pass.check(store, command, &mut different_budget).is_err());
                drop(pass);
            }
            "cancel" => {
                stop.store(true, Ordering::Release);
                assert!(pass.check(store, command, budget).is_err());
                drop(pass);
                stop.store(false, Ordering::Release);
            }
            _ => panic!("unknown fixed pass test"),
        }
    }
    store
        .db
        .execute_batch("ROLLBACK TO source_pass_fixture; RELEASE source_pass_fixture")?;
    // Guard restoration admits an actual ordinary producer, while preserving
    // the protected scoped-history baseline rather than clearing authorizers.
    store.db.execute_batch("SAVEPOINT source_pass_producer")?;
    crate::curation::item_put(
        store,
        &serde_json::json!({"id":"source-pass-restored-producer","kind":"video","metadata":{}}),
    )?;
    assert!(
        store
            .db
            .execute("DELETE FROM catalog_source_scope_heads", [])
            .is_err()
    );
    store
        .db
        .execute_batch("ROLLBACK TO source_pass_producer; RELEASE source_pass_producer")?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn assert_preparation_fence_for_test(
    store: &Store,
    candidate: &std::path::Path,
    other: &Store,
    case: &str,
    budget: &mut ProofBudget,
) -> Result<()> {
    // A test-only enclosing savepoint restores real producer effects. It is
    // outside the pass; no production callback or mutable proof is exported.
    store.db.execute_batch("SAVEPOINT source_pass_fixture")?;
    let stop = AtomicBool::new(false);
    if case == "unwind" {
        let acquired = std::cell::Cell::new(false);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _pass = SourcePreparationPass::for_test(
                store, "default", "media", candidate, &stop, budget,
            )
            .unwrap();
            acquired.set(true);
            panic!("controlled source pass unwind");
        }));
        assert!(acquired.get(), "unwind must follow actual pass acquisition");
        assert!(outcome.is_err());
    } else {
        let pass =
            SourcePreparationPass::for_test(store, "default", "media", candidate, &stop, budget)?;
        assert_eq!(
            pass.check(store, "default", "media", candidate, budget)?
                .files
                .len(),
            2
        );
        match case {
            "unchanged" => {
                let captured = pass.finish(store, "default", "media", candidate, budget)?;
                assert_eq!(captured.files.len(), 2);
            }
            "write" | "write_rollback" => {
                assert_eq!(store.db.execute("UPDATE bindings SET inode=inode+1 WHERE profile='default' AND kind='source' AND owner='media'", [])?, 1);
                if case == "write_rollback" {
                    assert!(
                        store
                            .db
                            .execute_batch("ROLLBACK TO source_pass_fixture")
                            .is_err()
                    );
                }
                assert!(
                    pass.check(store, "default", "media", candidate, budget)
                        .is_err()
                );
                drop(pass);
            }
            "schema" => {
                store
                    .db
                    .execute_batch("CREATE TABLE source_pass_fixture_schema(id INTEGER)")?;
                assert!(
                    pass.check(store, "default", "media", candidate, budget)
                        .is_err()
                );
                drop(pass);
            }
            "temp_schema" => {
                let main: i64 = store
                    .db
                    .query_row("PRAGMA main.schema_version", [], |r| r.get(0))?;
                let changes = store.db.total_changes();
                store.db.execute_batch(
                    "CREATE TEMP VIEW bindings AS SELECT * FROM main.bindings WHERE 0",
                )?;
                assert_eq!(
                    store
                        .db
                        .query_row("PRAGMA main.schema_version", [], |r| r.get::<_, i64>(0))?,
                    main
                );
                assert_eq!(store.db.total_changes(), changes);
                assert_eq!(
                    store
                        .db
                        .query_row("SELECT count(*) FROM bindings", [], |r| r.get::<_, i64>(0))?,
                    0
                );
                assert!(
                    pass.check(store, "default", "media", candidate, budget)
                        .is_err()
                );
                drop(pass);
            }
            "commit" | "rollback" | "savepoint" | "attach" | "detach" => {
                let sql = match case {
                    "commit" => "COMMIT",
                    "rollback" => "ROLLBACK",
                    "attach" => "ATTACH DATABASE ':memory:' AS forbidden_pass_attachment",
                    "detach" => "DETACH DATABASE forbidden_pass_attachment",
                    _ => "SAVEPOINT forbidden_pass_boundary",
                };
                assert!(store.db.execute_batch(sql).is_err());
                assert!(
                    pass.check(store, "default", "media", candidate, budget)
                        .is_err(),
                    "caught boundary denial must be sticky"
                );
                drop(pass);
            }
            "owner" => {
                assert!(
                    pass.check(other, "default", "media", candidate, budget)
                        .is_err()
                );
                assert!(
                    pass.check(store, "other-profile", "media", candidate, budget)
                        .is_err()
                );
                assert!(
                    pass.check(store, "default", "other-source", candidate, budget)
                        .is_err()
                );
                assert!(
                    pass.check(
                        store,
                        "default",
                        "media",
                        std::path::Path::new("different-root"),
                        budget
                    )
                    .is_err()
                );
                let different = OperationBudget::new(
                    std::time::Duration::from_secs(5),
                    crate::operation_budget::BudgetLimits {
                        items: 100,
                        values: 100,
                        bytes: 1024,
                        work: 100,
                    },
                )
                .expect("test operation limits");
                let mut different_budget = ProofBudget::new(different);
                assert!(
                    pass.check(store, "default", "media", candidate, &mut different_budget)
                        .is_err()
                );
                drop(pass);
            }
            "cancel" => {
                stop.store(true, Ordering::Release);
                assert!(
                    pass.check(store, "default", "media", candidate, budget)
                        .is_err()
                );
                drop(pass);
                stop.store(false, Ordering::Release);
            }
            _ => panic!("unknown fixed pass test"),
        }
    }
    store
        .db
        .execute_batch("ROLLBACK TO source_pass_fixture; RELEASE source_pass_fixture")?;
    // Guard restoration admits an actual ordinary producer, while preserving
    // the protected scoped-history baseline rather than clearing authorizers.
    store.db.execute_batch("SAVEPOINT source_pass_producer")?;
    crate::curation::item_put(
        store,
        &serde_json::json!({"id":"source-pass-restored-producer","kind":"video","metadata":{}}),
    )?;
    assert!(
        store
            .db
            .execute("DELETE FROM catalog_source_scope_heads", [])
            .is_err()
    );
    store
        .db
        .execute_batch("ROLLBACK TO source_pass_producer; RELEASE source_pass_producer")?;
    Ok(())
}
