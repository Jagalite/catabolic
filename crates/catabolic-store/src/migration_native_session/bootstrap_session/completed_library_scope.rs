//! Fixed current library facts retained by genuine completed native custody.
use super::*;
use crate::{
    catalog_nfo_attribution::ProofBudget,
    catalog_scope::SavedQueryRef,
    catalog_source_succession::{LibraryScopeSnapshot, LibraryScopeSource, library_scope},
};
#[path = "completed_library_scope/endpoint_fence.rs"]
mod endpoint_fence;

/// This borrow prevents source effects or custody transfer until the host has
/// finished using these descriptive facts. It is not a selection/read grant.
pub struct CompletedLibraryScope<'a> {
    owner: &'a CompletedCatalogSession,
    facts: LibraryScopeSnapshot,
}
impl CompletedCatalogSession {
    pub fn library_scope(
        &self,
        profile: &str,
        library: &str,
        reference: &SavedQueryRef,
        selectors: &[LibraryScopeSource],
    ) -> Result<CompletedLibraryScope<'_>> {
        self.validate_current()?;
        if !self.session.store.db.is_autocommit() {
            return Err(Error(
                "completed library scope requires its own fixed writer".into(),
            ));
        }
        let facts = self.session.store.transaction_detailed(|_| {
            self.validate_current()?;
            let mut budget = ProofBudget::new(self.session.store.catalog_operation_budget()?);
            library_scope::capture_in(
                &self.session.store,
                profile,
                library,
                reference,
                selectors,
                &mut budget,
            )
        })?;
        Ok(CompletedLibraryScope { owner: self, facts })
    }
}
impl CompletedLibraryScope<'_> {
    pub fn facts(&self) -> &LibraryScopeSnapshot {
        &self.facts
    }
    pub fn revalidate(&self) -> Result<()> {
        self.owner.validate_current()?;
        if !self.owner.session.store.db.is_autocommit() {
            return Err(Error(
                "completed library scope requires its own fixed writer".into(),
            ));
        }
        self.owner.session.store.transaction_detailed(|_| {
            self.owner.validate_current()?;
            let mut budget = ProofBudget::new(self.owner.session.store.catalog_operation_budget()?);
            library_scope::revalidate_in(&self.owner.session.store, &self.facts, &mut budget)
        })
    }
}

/// Exclusive completed-custody borrow for an authenticated endpoint
/// transformation. It grants neither membership nor physical file access.
pub struct CompletedFileEndpoint<'a> {
    owner: &'a CompletedCatalogSession,
    scope: &'a LibraryScopeSnapshot,
    facts: crate::catalog_source_succession::CurrentFileEndpoint,
}
impl CompletedLibraryScope<'_> {
    pub fn file_endpoint<'a>(
        &'a self,
        native_source: &str,
        request: &crate::catalog_source_succession::FileEndpointRequest,
    ) -> Result<CompletedFileEndpoint<'a>> {
        self.owner.validate_current()?;
        if !self.owner.session.store.db.is_autocommit() {
            return Err(Error(
                "completed file endpoint requires its own fixed writer".into(),
            ));
        }
        let endpoint_fence::EndpointOutput::Captured(facts) = endpoint_fence::run(
            self.owner,
            endpoint_fence::EndpointOperation::Capture {
                scope: &self.facts,
                source: native_source,
                request,
            },
        )?
        else {
            return Err(Error("endpoint capture phase mismatch".into()));
        };
        Ok(CompletedFileEndpoint {
            owner: self.owner,
            scope: &self.facts,
            facts,
        })
    }
}
impl CompletedFileEndpoint<'_> {
    pub fn facts(&self) -> &crate::catalog_source_succession::CurrentFileEndpoint {
        &self.facts
    }
    /// Recaptures scope, the complete source chain, and the actual current
    /// occurrence under the same genuine owner. Only the second scope scan is
    /// replaced by a fixed operation-local continuity fence, never a saved cache.
    pub fn revalidate(&self) -> Result<()> {
        self.owner.validate_current()?;
        if !self.owner.session.store.db.is_autocommit() {
            return Err(Error(
                "completed file endpoint requires its own fixed writer".into(),
            ));
        }
        match endpoint_fence::run(
            self.owner,
            endpoint_fence::EndpointOperation::Revalidate {
                scope: self.scope,
                facts: &self.facts,
            },
        )? {
            endpoint_fence::EndpointOutput::Revalidated => Ok(()),
            _ => Err(Error("endpoint revalidation phase mismatch".into())),
        }
    }
}

#[cfg(test)]
impl CompletedLibraryScope<'_> {
    pub(crate) fn assert_terminal_corruption_refused_for_test(
        &self,
        source: &str,
        request: &crate::catalog_source_succession::FileEndpointRequest,
    ) {
        // Controlled negative mutation only: this is NOT an admitted scan or a
        // production Completed mutation API. Every fault rolls back, including
        // assertion unwind. No native owner or connection leaves this method.
        let selectors: Vec<_> = self
            .facts
            .sources()
            .iter()
            .map(|s| {
                LibraryScopeSource::new(s.motion_source_id().into(), s.native_source().into())
                    .unwrap()
            })
            .collect();
        for ctime in [true, false] {
            self.owner.validate_current().unwrap();
            let reached = std::cell::Cell::new(false);
            let result: Result<()> = self.owner.session.store.transaction_detailed(|db| {
                self.owner.validate_current()?;
                let changed = if ctime {
                    db.execute("UPDATE main.native_source_ctimes SET ctime_ns=ctime_ns+1 WHERE profile=? AND file_id=?",
                        rusqlite::params![self.facts.profile(),request.exact().file_id])?
                } else {
                    db.execute("UPDATE main.observations SET observation_origin='scan',succession_source_id=NULL,succession_revision=NULL,scan_id=(SELECT id FROM main.scans WHERE profile=?1 AND location=?3 ORDER BY id LIMIT 1) WHERE profile=?1 AND file_id=?2",
                        rusqlite::params![self.facts.profile(),request.exact().file_id,source])?
                };
                assert_eq!(changed,1);
                let store=&self.owner.session.store;
                let mut budget=ProofBudget::new(store.catalog_operation_budget()?);
                let current=library_scope::capture_in(store,self.facts.profile(),self.facts.library_id(),self.facts.query(),&selectors,&mut budget)?;
                let error=crate::catalog_source_succession::file_endpoint::capture_in(store,&current,source,request,&mut budget).unwrap_err();
                assert_eq!(error.to_string(),if ctime {
                    "source file endpoint terminal ctime differs"
                } else { "source file endpoint terminal succession provenance differs" });
                reached.set(true);
                Err(Error("rollback controlled endpoint fault".into()))
            });
            assert_eq!(
                result.unwrap_err().to_string(),
                "rollback controlled endpoint fault"
            );
            assert!(reached.get(), "the intended endpoint fence was not reached");
            self.revalidate().unwrap();
        }
    }
    #[cfg(test)]
    pub(crate) fn assert_history_corruption_refused_for_test(
        &self,
        source: &str,
        request: &crate::catalog_source_succession::FileEndpointRequest,
        expected_revision: i64,
    ) {
        let selectors: Vec<_> = self
            .facts
            .sources()
            .iter()
            .map(|s| {
                LibraryScopeSource::new(s.motion_source_id().into(), s.native_source().into())
                    .unwrap()
            })
            .collect();
        for receipt_fault in [false, true] {
            self.owner.validate_current().unwrap();
            let reached = std::cell::Cell::new(false);
            let result: Result<()> = self.owner.session.store.transaction_detailed(|db| {
                let store = &self.owner.session.store;
                self.owner.validate_current()?;
                // Select the actual latest cohort at this one- or two-hop checkpoint.
                let revision: i64 = db.query_row("SELECT max(revision) FROM main.catalog_source_successions WHERE profile=? AND source_id=?", rusqlite::params![self.facts.profile(), source], |r| r.get(0))?;
                assert_eq!(revision, expected_revision);
                // Controlled corruption of a genuine two-file cohort.
                // Restore every byte-identical guard before invoking the real
                // endpoint owner. The entire fault is rolled back on all exits.
                let reset = crate::catalog_source_succession::authorizer::Guard::for_store(store)?;
                db.authorizer(Some(|_: rusqlite::hooks::AuthContext<'_>| rusqlite::hooks::Authorization::Allow))?;
                let table = if receipt_fault { "catalog_command_receipts" } else { "catalog_source_succession_files" };
                let guards: Vec<(String, String)> = db.prepare("SELECT name,sql FROM main.sqlite_schema WHERE type='trigger' AND tbl_name=? ORDER BY name")?
                    .query_map([table], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_,_>>()?;
                assert!(!guards.is_empty());
                for (name, _) in &guards {
                    db.execute_batch(&format!("DROP TRIGGER \"{}\"", name.replace('"', "\"\"")))?;
                }
                let changed = if receipt_fault {
                    // This check occurs after every selected/sibling file decode
                    // in the selected cohort, but before Collector::observe.
                    db.execute("UPDATE main.catalog_command_receipts SET payload_digest=printf('%064d',0) WHERE (catalog_id,host_namespace,operation_id) IN (SELECT catalog_id,command_host_namespace,command_operation_id FROM main.catalog_source_successions WHERE profile=? AND source_id=? AND revision=?)", rusqlite::params![self.facts.profile(), source, revision])?
                } else {
                    // A malformed sibling cannot be ignored merely because the
                    // selected file's decoded payload was privately retained.
                    db.execute("UPDATE main.catalog_source_succession_files SET old_occurrence_json='{}' WHERE profile=?1 AND source_id=?2 AND succession_revision=?4 AND file_id=(SELECT max(file_id) FROM main.catalog_source_succession_files WHERE profile=?1 AND source_id=?2 AND succession_revision=?4 AND file_id<>?3)", rusqlite::params![self.facts.profile(), source, request.exact().file_id, revision])?
                };
                assert_eq!(changed, 1);
                for (_, sql) in &guards { db.execute_batch(sql)?; }
                reset.restore()?;
                self.owner.validate_current()?;
                let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
                let current = library_scope::capture_in(store, self.facts.profile(), self.facts.library_id(), self.facts.query(), &selectors, &mut budget)?;
                let error = crate::catalog_source_succession::file_endpoint::capture_in(store, &current, source, request, &mut budget).unwrap_err();
                assert!(error.to_string().contains(if receipt_fault {
                    "permanent command provenance differs"
                } else { "complete permanent effect differs" }), "{error}");
                reached.set(true);
                Err(Error("rollback controlled endpoint history corruption".into()))
            });
            assert_eq!(
                result.unwrap_err().to_string(),
                "rollback controlled endpoint history corruption"
            );
            assert!(reached.get());
            self.revalidate().unwrap();
        }
    }
    pub(crate) fn assert_foreign_completed_owner_refused_for_test(
        &self,
        foreign: &CompletedCatalogSession,
    ) {
        foreign.validate_current().unwrap();
        let error = foreign
            .session
            .store
            .transaction_detailed(|_| {
                foreign.validate_current()?;
                let mut budget =
                    ProofBudget::new(foreign.session.store.catalog_operation_budget()?);
                library_scope::revalidate_in(&foreign.session.store, &self.facts, &mut budget)
            })
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "fixed native library scope differs or is unsupported"
        );
        self.revalidate().unwrap();
    }
}

pub(crate) use endpoint_fence::EndpointFreshFacts;

#[cfg(test)]
impl CompletedLibraryScope<'_> {
    pub(crate) fn assert_fresh_endpoint_facts_for_test(
        &self,
        foreign: &CompletedCatalogSession,
        source: &str,
        request: &crate::catalog_source_succession::FileEndpointRequest,
    ) {
        endpoint_fence::assert_fresh_facts_for_test(self, foreign, source, request)
    }
}
