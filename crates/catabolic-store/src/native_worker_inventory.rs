//! Conservative native worker inventory. Durable state is never a process handle.
use crate::{Error, Result, Store, media_mutation, migration};
// Bound returned unresolved-owner evidence independently of reconciliation work.
const MAX_RECORDS: usize = 256;
// This is a bounded complete scan, not unlimited or resumable recovery. An owner
// beyond this work budget keeps admission closed even when all examined rows
// have positive witnesses. The extra SQL row distinguishes exhaustion from EOF.
const MAX_INSPECTED_OWNERS: usize = 4096;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnresolvedOwner {
    pub kind: String,
    pub id: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Inventory {
    pub native: bool,
    pub catalog_id: String,
    pub incarnation: Option<String>,
    pub witnessed_processing: usize,
    pub witnessed_observation: usize,
    pub unresolved: Vec<UnresolvedOwner>,
    pub truncated: bool,
}
impl Inventory {
    pub fn admits_new_work(&self) -> bool {
        !self.truncated && self.unresolved.is_empty()
    }
}
/// Inspect one held Store lifetime. Writable owners retain their writer lock;
/// read-only Store sessions retain their existing snapshot. This never changes
/// a claim, signals/adopts a PID, or reconstructs capacity from durable rows.
pub fn inspect(store: &Store) -> Result<Inventory> {
    if migration::is_supported_native_schema(store.schema_version) {
        store.validate_native_identity()?;
        if store.writable && !store.owns_writer_lock() {
            return Err(Error(
                "native worker inventory requires the current catalog owner".into(),
            ));
        }
    }
    let result = inspect_in(
        &store.db,
        store.schema_version,
        &store.database_id,
        store.catalog_incarnation.as_deref(),
    )?;
    if result.native {
        store.validate_native_identity()?;
    }
    Ok(result)
}
/// Shared read-only inventory on an already validated, held native connection.
pub(crate) fn inspect_in(
    db: &rusqlite::Connection,
    schema: u32,
    catalog: &str,
    incarnation: Option<&str>,
) -> Result<Inventory> {
    let native = migration::is_supported_native_schema(schema);
    let report = Inventory {
        native,
        catalog_id: catalog.into(),
        incarnation: incarnation.map(str::to_owned),
        witnessed_processing: 0,
        witnessed_observation: 0,
        unresolved: Vec::new(),
        truncated: false,
    };
    if !native {
        return Ok(report);
    }
    scan_in(db, schema, report, |kind, id| {
        current_cleanup_in(db, schema, catalog, incarnation, kind, id)
    })
}

/// Only retained migration custody with exact closed41 validation selects this
/// path. Ordinary unsupported41 inspection remains unchanged and nonnative.
pub(crate) fn inspect_pending41(
    admitted: &crate::migration::AdmittedPending41<'_>,
) -> Result<Inventory> {
    let db = admitted.database();
    let catalog = admitted.catalog_id();
    let incarnation = admitted.incarnation();
    let report = Inventory {
        native: true,
        catalog_id: catalog.into(),
        incarnation: Some(incarnation.into()),
        witnessed_processing: 0,
        witnessed_observation: 0,
        unresolved: Vec::new(),
        truncated: false,
    };
    scan_in(db, 41, report, |kind, id| {
        if kind == "processing" && media_mutation::cleanup_witness_for_pending41(admitted, id)? {
            Ok(CurrentCleanup::Processing)
        } else if kind == "observation"
            && crate::native_observation_companion::has_cleanup_witness_in(
                db,
                catalog,
                Some(incarnation),
                id,
            )?
        {
            Ok(CurrentCleanup::Observation)
        } else {
            Ok(CurrentCleanup::Unresolved)
        }
    })
}

/// Genuine completed-catalog custody, distinct from pending migration and
/// fixture admission. Construction is local to the exact-token scanner below.
pub(crate) struct AdmittedCompletedSource<'a> {
    store: &'a Store,
}
impl AdmittedCompletedSource<'_> {
    pub(crate) fn database(&self) -> &rusqlite::Connection {
        &self.store.db
    }
}
pub(crate) fn inspect_completed_source(
    store: &Store,
    admission: &crate::migration::CompletedSourceAdmission<'_, '_>,
    command: &catabolic_core::catalog_command::CommandEnvelope,
) -> Result<Inventory> {
    admission.check(store, command)?;
    inspect_completed_source_validated(store)
}
pub(crate) fn inspect_completed_source_preparation(
    store: &Store,
    admission: &crate::migration::CompletedSourcePreparationAdmission<'_, '_>,
    profile: &str,
    source: &str,
    candidate: &std::path::Path,
) -> Result<Inventory> {
    admission.check(store, profile, source, candidate)?;
    inspect_completed_source_validated(store)
}
pub(crate) fn inspect_completed_source_withdrawal(
    store: &Store,
    admission: &crate::migration::CompletedSourceWithdrawalAdmission<'_>,
    profile: &str,
    source: &str,
    motion_source: &str,
) -> Result<Inventory> {
    admission.check(store, profile, source, motion_source)?;
    inspect_completed_source_validated(store)
}
/// Fixed current-image admission, never pending/bootstrap or raw connection authority.
pub(crate) fn inspect_completed_current_image(
    store: &Store,
    admission: &crate::migration::CompletedCurrentImageAdmission<'_, '_>,
) -> Result<Inventory> {
    admission.check(store)?;
    inspect_completed_source_validated(store)
}
fn inspect_completed_source_validated(store: &Store) -> Result<Inventory> {
    if !store.owns_writer_lock()
        || store.db.is_autocommit()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
    {
        return Err(Error(
            "completed source inventory requires retained writer".into(),
        ));
    }
    let admitted = AdmittedCompletedSource { store };
    let report = Inventory {
        native: true,
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone(),
        witnessed_processing: 0,
        witnessed_observation: 0,
        unresolved: Vec::new(),
        truncated: false,
    };
    scan_in(&store.db, 41, report, |kind, id| {
        if kind == "processing"
            && media_mutation::cleanup_witness_for_completed_source(&admitted, id)?
        {
            Ok(CurrentCleanup::Processing)
        } else if kind == "observation"
            && crate::native_observation_companion::has_cleanup_witness_in(
                &store.db,
                &store.database_id,
                store.catalog_incarnation.as_deref(),
                id,
            )?
        {
            Ok(CurrentCleanup::Observation)
        } else {
            Ok(CurrentCleanup::Unresolved)
        }
    })
}

/// Exact test-admitted source owner, not a migration token or generic schema41
/// exemption. Construction validates the complete candidate and retained writer.
#[cfg(test)]
pub(crate) struct AdmittedSourceCandidate<'a> {
    store: &'a Store,
}
#[cfg(test)]
impl AdmittedSourceCandidate<'_> {
    pub(crate) fn database(&self) -> &rusqlite::Connection {
        &self.store.db
    }
}
#[cfg(test)]
pub(crate) fn inspect_source_candidate_for_test(store: &Store) -> Result<Inventory> {
    store.validate_candidate41_for_test()?;
    if !store.owns_writer_lock()
        || store.db.is_autocommit()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
    {
        return Err(Error(
            "source candidate inventory requires retained writer".into(),
        ));
    }
    let admitted = AdmittedSourceCandidate { store };
    let report = Inventory {
        native: true,
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone(),
        witnessed_processing: 0,
        witnessed_observation: 0,
        unresolved: Vec::new(),
        truncated: false,
    };
    scan_in(&store.db, 41, report, |kind, id| {
        if kind == "processing"
            && media_mutation::cleanup_witness_for_source_candidate(&admitted, id)?
        {
            Ok(CurrentCleanup::Processing)
        } else if kind == "observation"
            && crate::native_observation_companion::has_cleanup_witness_in(
                &store.db,
                &store.database_id,
                store.catalog_incarnation.as_deref(),
                id,
            )?
        {
            Ok(CurrentCleanup::Observation)
        } else {
            Ok(CurrentCleanup::Unresolved)
        }
    })
}

// Private scanner: only the two admitted entry points above choose cleanup
// predicates. Bounds, domain query and accounting are shared without schema aliases.
fn scan_in(
    db: &rusqlite::Connection,
    schema: u32,
    mut report: Inventory,
    cleanup: impl Fn(&str, &str) -> Result<CurrentCleanup>,
) -> Result<Inventory> {
    let lifetimes = if schema >= media_mutation::NATIVE_CLEANUP_SCHEMA_VERSION {
        " UNION ALL SELECT 'execution_lifetime',job_id||':'||generation||':'||attempt FROM native_execution_lifetimes WHERE state='owned'"
    } else {
        ""
    };
    let companions = if schema >= crate::native_observation_companion::SCHEMA_VERSION {
        " UNION ALL SELECT 'observation_companion',reservation_nonce FROM native_observation_companions WHERE state!='quiescent'"
    } else {
        ""
    };
    // Terminal rows and absent/malformed PIDs are deliberately retained. Only
    // a supported exact processing/observation witness can discharge a claim.
    // Apply the response sample bound after witness validation, not before it.
    let sql = format!(
        "SELECT 'processing' AS kind,j.id FROM processing_jobs j LEFT JOIN execution_claims c ON c.job_id=j.id WHERE j.attempts>0 OR j.state='running' OR c.job_id IS NOT NULL UNION ALL SELECT 'observation',id FROM observation_jobs WHERE state!='queued' OR worker_pid IS NOT NULL OR claim_token IS NOT NULL UNION ALL SELECT 'watcher',id FROM watcher_runs UNION ALL SELECT 'watcher_active',coalesce(active_run,'') FROM watchers WHERE active_run IS NOT NULL UNION ALL SELECT 'fallback_probe',CAST(id AS TEXT) FROM fallback_probe_slots WHERE owner IS NOT NULL OR pid IS NOT NULL UNION ALL SELECT 'api_worker',profile FROM api_worker_status{lifetimes}{companions} LIMIT {}",
        MAX_INSPECTED_OWNERS + 1
    );
    let mut statement = db.prepare(&sql)?;
    let mut records = statement.query([])?;
    let mut examined = 0;
    while let Some(row) = records.next()? {
        if examined == MAX_INSPECTED_OWNERS {
            report.truncated = true;
            break;
        }
        examined += 1;
        let kind: String = row.get(0)?;
        let id: String = row.get(1)?;
        match cleanup(&kind, &id)? {
            CurrentCleanup::Processing => report.witnessed_processing += 1,
            CurrentCleanup::Observation => report.witnessed_observation += 1,
            CurrentCleanup::Unresolved => {
                if report.unresolved.len() == MAX_RECORDS {
                    report.truncated = true;
                    break;
                }
                report.unresolved.push(UnresolvedOwner { kind, id });
            }
        }
    }
    drop(records);
    drop(statement);
    Ok(report)
}

/// Current-incarnation cleanup only. Historical restore evidence is deliberately
/// not an outcome of this predicate and cannot satisfy ordinary admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CurrentCleanup {
    Processing,
    Observation,
    Unresolved,
}
pub(crate) fn current_cleanup_in(
    db: &rusqlite::Connection,
    schema: u32,
    catalog: &str,
    incarnation: Option<&str>,
    kind: &str,
    id: &str,
) -> Result<CurrentCleanup> {
    if kind == "processing" && media_mutation::has_cleanup_witness(db, id)? {
        Ok(CurrentCleanup::Processing)
    } else if kind == "observation"
        && schema >= crate::native_observation_companion::SCHEMA_VERSION
        && crate::native_observation_companion::has_cleanup_witness_in(
            db,
            catalog,
            incarnation,
            id,
        )?
    {
        Ok(CurrentCleanup::Observation)
    } else {
        Ok(CurrentCleanup::Unresolved)
    }
}

#[cfg(test)]
mod pending41_boundary_tests {
    use super::*;
    #[test]
    fn unregistered41_ordinary_inventory_remains_nonnative() {
        if migration::is_supported_native_schema(41) {
            return;
        }
        let db = rusqlite::Connection::open_in_memory().unwrap();
        // No owner tables: unchanged ordinary refusal must not run the41 scanner.
        let result = inspect_in(&db, 41, "opaque", Some("historical")).unwrap();
        assert!(!result.native);
        assert!(result.unresolved.is_empty());
        assert!(!result.truncated);
    }
}
