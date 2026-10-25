//! Upgrade-only quiescence qualification on the caller's existing locked
//! connection. Historical deferred coverage is not an outstanding worker, but
//! only the exact existing closed-companion witness can establish that fact.
use crate::{Error, Result, migration, native_observation_companion};
use rusqlite::Connection;
use serde_json::Value;

pub(crate) fn qualified_pending_work(db: &Connection, version: u32) -> Result<Value> {
    let pending = migration::native_pending_work(db, version)?;
    if !matches!(version, 39 | 40) {
        return Err(Error(
            "unsupported qualified native upgrade inventory".into(),
        ));
    }
    // The migration owner separately validates full schema/history and retains
    // admission + writer + SQLite locks. Bind witness checks to this connection.
    let (catalog, incarnation): (String, String) = db.query_row(
        "SELECT catalog_id,incarnation FROM catalog_state WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let inventory =
        crate::native_worker_inventory::inspect_in(db, version, &catalog, Some(&incarnation))?;
    qualify_in(db, pending, &catalog, &incarnation, inventory)
}

/// Actual41 inventory under exact retained migration proof, never schema40 alias.
pub(crate) fn qualified_pending_work41(
    admitted: &crate::migration::AdmittedPending41<'_>,
) -> Result<Value> {
    let pending = migration::native_pending_work(admitted.database(), 41)?;
    let inventory = crate::native_worker_inventory::inspect_pending41(admitted)?;
    qualify_in(
        admitted.database(),
        pending,
        admitted.catalog_id(),
        admitted.incarnation(),
        inventory,
    )
}

fn qualify_in(
    db: &Connection,
    mut pending: Value,
    catalog: &str,
    incarnation: &str,
    inventory: crate::native_worker_inventory::Inventory,
) -> Result<Value> {
    // Same complete bounded owner inventory used by normal host admission.
    // Never turn terminal state or a PID into cleanup authority.
    pending["unresolved_native_owners"] = serde_json::json!(inventory.unresolved.len());
    pending["truncated_native_owners"] = serde_json::json!(if inventory.truncated { 1 } else { 0 });
    let jobs = db.prepare("SELECT DISTINCT job_id FROM observation_scopes WHERE state NOT IN ('complete','excluded','registry_complete') LIMIT 4097")?
        .query_map([], |r| r.get::<_,String>(0))?
        .collect::<std::result::Result<Vec<_>,_>>()?;
    if jobs.len() > 4096 {
        return Err(Error(
            "qualified upgrade observation inventory exceeds bound".into(),
        ));
    }
    for job in &jobs {
        let eligible: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM observation_jobs j WHERE j.id=?1 AND j.state IN ('complete','unavailable','failed','stale') AND j.lease_until=0 AND j.completed_at IS NOT NULL AND NOT EXISTS(SELECT 1 FROM observation_scopes s WHERE s.job_id=j.id AND s.state NOT IN ('complete','excluded','registry_complete','deferred')))", [job], |r| r.get(0))?;
        if !eligible
            || !native_observation_companion::has_cleanup_witness_in(
                db,
                catalog,
                Some(incarnation),
                job,
            )?
        {
            // Retain the raw count, rather than manufacture settlement.
            return Ok(pending);
        }
    }
    pending["observation_scopes"] = serde_json::json!(0);
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_catalog_qualifies_but_terminal_unwitnessed_history_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inventory.db");
        migration::initialize_native_at(&path, 39).unwrap();
        let store = crate::Store::open(&path, true, false).unwrap();
        assert!(migration::work_is_settled(
            &qualified_pending_work(&store.db, 39).unwrap()
        ));
        store.transaction(|db| {
            db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,completed_at,lease_until) VALUES('terminal','default','media','fixture','[]',0,1,'failed',1,2,0)",[])?;
            db.execute("INSERT INTO observation_scopes VALUES('terminal','path','deferred','{}',2)",[])?;
            Ok(())
        }).unwrap();
        let qualified = qualified_pending_work(&store.db, 39).unwrap();
        assert_eq!(qualified["observation_scopes"], 1);
        assert_eq!(qualified["unresolved_native_owners"], 1);
        assert!(!migration::work_is_settled(&qualified));
        // Qualification never rewrites or discards incomplete-coverage history.
        assert_eq!(
            store
                .db
                .query_row("SELECT state FROM observation_scopes", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "deferred"
        );
        assert!(qualified_pending_work(&store.db, 41).is_err());
    }
    #[test]
    fn complete_scope_does_not_discharge_retained_unwitnessed_worker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worker.db");
        migration::initialize_native_at(&path, 39).unwrap();
        let store = crate::Store::open(&path, true, false).unwrap();
        store.transaction(|db| {
            db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,completed_at,lease_until,worker_pid) VALUES('terminal','default','media','fixture','[]',0,1,'complete',1,2,0,123)",[])?;
            db.execute("INSERT INTO observation_scopes VALUES('terminal','path','complete','{}',2)",[])?;
            Ok(())
        }).unwrap();
        let raw = migration::native_pending_work(&store.db, 39).unwrap();
        assert!(migration::work_is_settled(&raw));
        let qualified = qualified_pending_work(&store.db, 39).unwrap();
        assert_eq!(qualified["unresolved_native_owners"], 1);
        assert!(!migration::work_is_settled(&qualified));
        let existing = crate::native_worker_inventory::inspect(&store).unwrap();
        let shared = crate::native_worker_inventory::inspect_in(
            &store.db,
            39,
            &store.database_id,
            store.catalog_incarnation.as_deref(),
        )
        .unwrap();
        assert_eq!(existing, shared);
    }
}
