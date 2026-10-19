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
    let native = migration::is_supported_native_schema(store.schema_version);
    let mut report = Inventory {
        native,
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone(),
        witnessed_processing: 0,
        witnessed_observation: 0,
        unresolved: Vec::new(),
        truncated: false,
    };
    if !native {
        return Ok(report);
    }
    store.validate_native_identity()?;
    if store.writable && !store.owns_writer_lock() {
        return Err(Error(
            "native worker inventory requires the current catalog owner".into(),
        ));
    }
    let lifetimes = if store.schema_version >= media_mutation::NATIVE_CLEANUP_SCHEMA_VERSION {
        " UNION ALL SELECT 'execution_lifetime',job_id||':'||generation||':'||attempt FROM native_execution_lifetimes WHERE state='owned'"
    } else {
        ""
    };
    let companions = if store.schema_version >= crate::native_observation_companion::SCHEMA_VERSION
    {
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
    let mut statement = store.db.prepare(&sql)?;
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
        if kind == "processing" && media_mutation::has_cleanup_witness(&store.db, &id)? {
            report.witnessed_processing += 1;
        } else if kind == "observation"
            && crate::native_observation_companion::has_cleanup_witness(store, &id)?
        {
            report.witnessed_observation += 1;
        } else {
            if report.unresolved.len() == MAX_RECORDS {
                report.truncated = true;
                break;
            }
            report.unresolved.push(UnresolvedOwner { kind, id });
        }
    }
    drop(records);
    drop(statement);
    store.validate_native_identity()?;
    Ok(report)
}
