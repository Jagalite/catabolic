//! Pre-rotation historical worker closure. This does not seal cleanup, rotate an
//! incarnation or grant execution. Exact old witnesses remain untouched.
mod chain;
mod restored_history;
pub(crate) use restored_history::AdmittedRestoreHistory;
pub(crate) mod completed_image;
mod image;

use crate::{Error, Result, Store};
use rusqlite::{Connection, types::ValueRef};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct PreparedWorkerHistory {
    catalog: String,
    incarnation: String,
    owners: Vec<HistoricalOwner>,
}
struct HistoricalOwner {
    kind: String,
    id: String,
    tuple_sha256: String,
    origin_incarnation: String,
}
impl PreparedWorkerHistory {
    pub fn catalog_id(&self) -> &str {
        &self.catalog
    }
    pub fn origin_incarnation(&self) -> &str {
        &self.incarnation
    }
    pub fn owner_count(&self) -> usize {
        self.owners.len()
    }
}
fn fail(message: &str) -> Error {
    Error(message.into())
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(fail("restore worker closure cancelled"))
    } else {
        Ok(())
    }
}
fn field(hash: &mut Sha256, value: ValueRef<'_>) {
    let (tag, bytes): (u8, Vec<u8>) = match value {
        ValueRef::Null => (0, Vec::new()),
        ValueRef::Integer(v) => (1, v.to_be_bytes().to_vec()),
        ValueRef::Real(v) => (2, v.to_bits().to_be_bytes().to_vec()),
        ValueRef::Text(v) => (3, v.to_vec()),
        ValueRef::Blob(v) => (4, v.to_vec()),
    };
    hash.update([tag]);
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}
struct Budget {
    rows: u64,
    bytes: u64,
}
fn rows(
    db: &Connection,
    table: &str,
    predicate: &str,
    id: &str,
    budget: &mut Budget,
    cancel: &AtomicBool,
) -> Result<String> {
    check(cancel)?;
    // Table/predicate are fixed code, never caller-supplied SQL. The inspected
    // image already passed the native closed schema owner before this function.
    let columns = db
        .prepare(&format!("PRAGMA table_xinfo('{table}')"))?
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(6)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if columns.is_empty()
        || columns.len() > 64
        || columns
            .iter()
            .any(|(name, hidden)| name.len() > 256 || *hidden != 0)
    {
        return Err(fail("unsupported restore owner row layout"));
    }
    let selector = columns
        .iter()
        .map(|(name, _)| format!("\"{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(",");
    let cost = columns
        .iter()
        .map(|(name, _)| {
            format!(
                "coalesce(length(CAST(\"{}\" AS BLOB)),0)+16",
                name.replace('"', "\"\"")
            )
        })
        .collect::<Vec<_>>()
        .join("+");
    let (count, bytes): (i64, i64) = db.query_row(
        &format!("SELECT count(*),coalesce(sum({cost}),0) FROM {table} WHERE {predicate}"),
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let count = u64::try_from(count).map_err(|_| fail("negative restore row count"))?;
    let bytes = u64::try_from(bytes).map_err(|_| fail("negative restore row bytes"))?;
    budget.rows = budget
        .rows
        .checked_add(count)
        .ok_or_else(|| fail("restore owner row overflow"))?;
    budget.bytes = budget
        .bytes
        .checked_add(bytes)
        .ok_or_else(|| fail("restore owner byte overflow"))?;
    if budget.rows > 16384 || budget.bytes > 8 * 1024 * 1024 {
        return Err(fail("complete restore owner closure exceeds bound"));
    }
    let mut statement = db.prepare(&format!("SELECT {selector} FROM {table} WHERE {predicate}"))?;
    let mut query = statement.query([id])?;
    let mut hashes = Vec::new();
    while let Some(row) = query.next()? {
        check(cancel)?;
        let mut hash = Sha256::new();
        for index in 0..columns.len() {
            field(&mut hash, row.get_ref(index)?);
        }
        hashes.push(<[u8; 32]>::from(hash.finalize()));
    }
    hashes.sort_unstable();
    let mut hash = Sha256::new();
    hash.update(table.as_bytes());
    hash.update([0]);
    hash.update(selector.as_bytes());
    for row in hashes {
        hash.update(row);
    }
    Ok(format!("{:x}", hash.finalize()))
}
/// Only an independently validated native image calls this. It captures each
/// terminal owner's exact tuple separately, so unrelated new jobs cannot turn
/// historical closure into a fresh cleanup witness or invalidate other history.
pub(crate) fn prepare_in(
    db: &Connection,
    schema: u32,
    catalog: &str,
    incarnation: &str,
    cancel: &AtomicBool,
) -> Result<PreparedWorkerHistory> {
    check(cancel)?;
    let inventory =
        crate::native_worker_inventory::inspect_in(db, schema, catalog, Some(incarnation))?;
    if !inventory.native {
        return Err(fail(
            "restore historical closure requires a supported native owner; repeated restore ancestry is not yet admitted",
        ));
    }
    if !inventory.admits_new_work() {
        return Err(fail(
            "restore requires resolved pre-rotation worker ownership; retained unresolved evidence was not changed",
        ));
    }
    let owner_sql = "SELECT 'processing' AS kind,j.id AS id FROM processing_jobs j LEFT JOIN execution_claims c ON c.job_id=j.id WHERE j.attempts>0 OR j.state='running' OR c.job_id IS NOT NULL UNION ALL SELECT 'observation',id FROM observation_jobs WHERE state!='queued' OR worker_pid IS NOT NULL OR claim_token IS NOT NULL";
    let (count, bytes): (i64, i64) = db.query_row(
        &format!("SELECT count(*),coalesce(sum(length(CAST(id AS BLOB))),0) FROM ({owner_sql})"),
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if !(0..=4096).contains(&count) || !(0..=1024 * 1024).contains(&bytes) {
        return Err(fail("restore owner inventory exceeds complete bound"));
    }
    let owners = db
        .prepare(&format!("{owner_sql} ORDER BY 1,2"))?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if owners.len() > 4096 {
        return Err(fail("restore historical owner count exceeds bound"));
    }
    let mut result = Vec::new();
    let mut budget = Budget { rows: 0, bytes: 0 };
    for (kind, id) in owners {
        if id.len() > 256 {
            return Err(fail("restore owner identity exceeds bound"));
        }
        let tuple_sha256 = owner_tuple(db, &kind, &id, &mut budget, cancel)?;
        result.push(HistoricalOwner {
            kind,
            id,
            tuple_sha256,
            origin_incarnation: incarnation.into(),
        });
    }
    Ok(PreparedWorkerHistory {
        catalog: catalog.into(),
        incarnation: incarnation.into(),
        owners: result,
    })
}

fn owner_tuple(
    db: &Connection,
    kind: &str,
    id: &str,
    budget: &mut Budget,
    cancel: &AtomicBool,
) -> Result<String> {
    if !matches!(kind, "processing" | "observation") {
        return Err(fail("unsupported historical worker kind"));
    }
    let table = if kind == "processing" {
        "processing_jobs"
    } else {
        "observation_jobs"
    };
    let state: String = db.query_row(
        &format!("SELECT state FROM {table} WHERE id=?"),
        [id],
        |r| r.get(0),
    )?;
    let terminal = if kind == "processing" {
        matches!(
            state.as_str(),
            "complete" | "failed" | "cancelled" | "changed" | "mismatch"
        )
    } else {
        matches!(
            state.as_str(),
            "complete" | "failed" | "cancelled" | "stale" | "unavailable"
        )
    };
    if !terminal {
        return Err(fail(
            "restore owner is not an explicitly terminal witnessed claim",
        ));
    }

    let queries: &[(&str, &str)] = if kind == "processing" {
        &[
            ("processing_jobs", "id=?"),
            ("execution_claims", "job_id=?"),
            ("native_execution_lifetimes", "job_id=?"),
        ]
    } else {
        &[
            ("observation_jobs", "id=?"),
            ("native_observation_companions", "owned_job_id=?"),
            (
                "observation_requests",
                "id IN (SELECT request_id FROM native_observation_companions WHERE owned_job_id=?)",
            ),
        ]
    };
    let mut hash = Sha256::new();
    for (table, predicate) in queries {
        hash.update(rows(db, table, predicate, id, budget, cancel)?.as_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// This classification never satisfies ordinary execution cleanup. The caller
/// must already own a validated isolated staged image and complete chain.
enum RestoredOwner {
    CurrentCleanup(crate::native_worker_inventory::CurrentCleanup),
    AuthenticatedHistoricalClosure {
        origin_incarnation: String,
        tuple_sha256: String,
    },
}
fn classify_owner_in(
    admitted: &AdmittedRestoreOwner<'_>,
    schema: u32,
    catalog: &str,
    incarnation: &str,
    kind: &str,
    id: &str,
    chain: &chain::Chain,
    budget: &mut Budget,
    cancel: &AtomicBool,
) -> Result<RestoredOwner> {
    use crate::native_worker_inventory::{CurrentCleanup, current_cleanup_in};
    check(cancel)?;
    let db = admitted.database();
    let current = if kind == "processing"
        && crate::media_mutation::cleanup_witness_for_restore(admitted, id)?
    {
        CurrentCleanup::Processing
    } else {
        current_cleanup_in(db, schema, catalog, Some(incarnation), kind, id)?
    };
    if current != CurrentCleanup::Unresolved {
        return Ok(RestoredOwner::CurrentCleanup(current));
    }
    let tip = chain
        .origins
        .last()
        .filter(|o| o.catalog_id == catalog && o.to_incarnation == incarnation)
        .ok_or_else(|| fail("unresolved worker has no current authenticated restore ancestry"))?;
    let historical = chain
        .workers
        .get(&(tip.restore_id.clone(), kind.into(), id.into()))
        .ok_or_else(|| fail("new or unknown worker requires actual current cleanup"))?;
    let actual = owner_tuple(db, kind, id, budget, cancel)?;
    if historical.tuple_sha256 != actual {
        return Err(fail(
            "historical worker tuple changed; current cleanup required",
        ));
    }
    Ok(RestoredOwner::AuthenticatedHistoricalClosure {
        origin_incarnation: historical.origin_incarnation.clone(),
        tuple_sha256: actual,
    })
}

/// Nonconstructible outside this restore owner; borrowing keeps the admitted
/// Store, native lease and physical writer lock alive for the whole operation.
pub(crate) struct AdmittedRestoreOwner<'a> {
    store: Option<&'a Store>,
    db: &'a Connection,
    poisoned: Option<&'a std::cell::Cell<bool>>,
}
impl<'a> AdmittedRestoreOwner<'a> {
    fn acquire(store: &'a Store) -> Result<Self> {
        store.transaction_available()?;
        if store.schema_version != 41
            || store.catalog_incarnation.is_none()
            || !store.writable
            || !store.owns_writer_lock()
            || store.db.is_autocommit()
            || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
        {
            return Err(fail(
                "restore requires exact admitted candidate41 isolated writer",
            ));
        }
        // In current builds only the closed cfg(test) candidate owner can admit
        // this Store. Production native41 remains unregistered and unsupported.
        store.validate_native_identity()?;
        Ok(Self {
            store: Some(store),
            db: &store.db,
            poisoned: None,
        })
    }
    pub(crate) fn database(&self) -> &Connection {
        self.db
    }
    fn rollback(&self) -> Result<()> {
        if let Some(store) = self.store {
            return store.rollback_transaction(Some("paired_restore_rotation"));
        }
        let _cleanup = crate::native_writer_vm::cleanup(self.db);
        if self.db.is_autocommit() {
            return Ok(());
        }
        if let Err(error) = self
            .db
            .execute_batch("ROLLBACK TO paired_restore_rotation; RELEASE paired_restore_rotation")
        {
            let _ = self.db.execute_batch("ROLLBACK");
            if let Some(poisoned) = self.poisoned {
                poisoned.set(true);
            }
            let _ = self
                .db
                .authorizer(Some(|_: rusqlite::hooks::AuthContext<'_>| {
                    rusqlite::hooks::Authorization::Deny
                }));
            return Err(error.into());
        }
        Ok(())
    }
    fn validate_path(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let Some(store) = self.store else {
            if self.poisoned.is_some_and(|p| p.get()) {
                return Err(fail("isolated restore image cleanup is poisoned"));
            }
            return Ok(());
        };
        let metadata = std::fs::symlink_metadata(&store.path)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || store.native_path_identity != Some((metadata.dev(), metadata.ino()))
        {
            return Err(fail("staged restore pathname changed under retained owner"));
        }
        store
            .native_admission
            .as_ref()
            .ok_or_else(|| fail("staged restore admission lease missing"))?
            .validate_path()?;
        Ok(())
    }
}

fn prepare_candidate_in(
    admitted: &AdmittedRestoreOwner<'_>,
    catalog: &str,
    incarnation: &str,
    cancel: &AtomicBool,
) -> Result<PreparedWorkerHistory> {
    let db = admitted.database();
    let ancestry = chain::read(db, catalog, incarnation, cancel)?;
    // Exact candidate41 worker domains, including non-job owners. None of the
    // latter can be discharged by an old terminal job's historical proof.
    let sql = "SELECT 'processing' AS kind,j.id FROM processing_jobs j LEFT JOIN execution_claims c ON c.job_id=j.id WHERE j.attempts>0 OR j.state='running' OR c.job_id IS NOT NULL UNION ALL SELECT 'observation',id FROM observation_jobs WHERE state!='queued' OR worker_pid IS NOT NULL OR claim_token IS NOT NULL UNION ALL SELECT 'watcher',id FROM watcher_runs UNION ALL SELECT 'watcher_active',coalesce(active_run,'') FROM watchers WHERE active_run IS NOT NULL UNION ALL SELECT 'fallback_probe',CAST(id AS TEXT) FROM fallback_probe_slots WHERE owner IS NOT NULL OR pid IS NOT NULL UNION ALL SELECT 'api_worker',profile FROM api_worker_status UNION ALL SELECT 'execution_lifetime',job_id||':'||generation||':'||attempt FROM native_execution_lifetimes WHERE state='owned' UNION ALL SELECT 'observation_companion',reservation_nonce FROM native_observation_companions WHERE state!='quiescent'";
    let (count, bytes): (i64, i64) = db.query_row(
        &format!("SELECT count(*),coalesce(sum(length(CAST(id AS BLOB))),0) FROM ({sql})"),
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if !(0..=4096).contains(&count) || !(0..=1024 * 1024).contains(&bytes) {
        return Err(fail("complete restored worker closure exceeds bound"));
    }
    let mut statement = db.prepare(&format!("{sql} ORDER BY 1,2"))?;
    let mut query = statement.query([])?;
    let mut owners = Vec::new();
    let mut budget = Budget { rows: 0, bytes: 0 };
    while let Some(row) = query.next()? {
        check(cancel)?;
        let kind: String = row.get(0)?;
        let id: String = row.get(1)?;
        if id.len() > 256 || !matches!(kind.as_str(), "processing" | "observation") {
            return Err(fail("unresolved live or unsupported restore worker owner"));
        }
        let (origin_incarnation, tuple_sha256) = match classify_owner_in(
            admitted,
            41,
            catalog,
            incarnation,
            &kind,
            &id,
            &ancestry,
            &mut budget,
            cancel,
        )? {
            RestoredOwner::CurrentCleanup(current) => {
                debug_assert_ne!(
                    current,
                    crate::native_worker_inventory::CurrentCleanup::Unresolved
                );
                (
                    incarnation.into(),
                    owner_tuple(db, &kind, &id, &mut budget, cancel)?,
                )
            }
            RestoredOwner::AuthenticatedHistoricalClosure {
                origin_incarnation,
                tuple_sha256,
            } => (origin_incarnation, tuple_sha256),
        };
        owners.push(HistoricalOwner {
            kind,
            id,
            tuple_sha256,
            origin_incarnation,
        });
    }
    Ok(PreparedWorkerHistory {
        catalog: catalog.into(),
        incarnation: incarnation.into(),
        owners,
    })
}

// Raw lineage bytes are not an allocation bound. Reserve 64 MiB for decoded
// proof strings, typed column duplicates, canonical serialization, exact grant
// maps and bounded inventory maps; charge SQLite separately. This is accounting,
// not a process-wide RSS guarantee. Pair composition must additionally reserve
// its measured retained owners before exposing this currently private seam.
fn rotation_allocation(db: &Connection) -> Result<()> {
    let pages: i64 = db.pragma_query_value(None, "page_count", |r| r.get(0))?;
    let size: i64 = db.pragma_query_value(None, "page_size", |r| r.get(0))?;
    let image = u64::try_from(pages)
        .ok()
        .and_then(|p| u64::try_from(size).ok().and_then(|s| p.checked_mul(s)))
        .ok_or_else(|| fail("restore image size overflow"))?;
    let total = image
        .checked_mul(4)
        .and_then(|n| n.checked_add(64 * 1024 * 1024))
        .and_then(|n| n.checked_add(crate::catalog_backup::allocations(db).ok()?));
    if total.is_none_or(|n| n > 256 * 1024 * 1024) {
        return Err(fail("staged restore cumulative allocation cohort exceeded"));
    }
    Ok(())
}

/// Private staged-image effect only. The caller must discard/close this Store
/// after rotation: its original-incarnation ordinary session is intentionally
/// invalidated. Pair publication and Motion reconciliation are separate owners.
fn rotate_staged_in(
    store: &Store,
    pair_sha256: &str,
    motion_binding_sha256: &str,
    cancel: &AtomicBool,
) -> Result<chain::Origin> {
    rotate_staged_with(store, pair_sha256, motion_binding_sha256, cancel, || Ok(()))
}
/// Candidate owner fixture only: caller retains its genuine admitted Store and
/// BEGIN IMMEDIATE, then commits and drops the deliberately stale old owner.
/// Fixture digests are commitments for this test, not verified pair authority.
#[cfg(test)]
pub(crate) fn rotate_candidate_for_test(
    store: &Store,
    pair_sha256: &str,
    motion_binding_sha256: &str,
    cancel: &AtomicBool,
) -> Result<String> {
    Ok(rotate_staged_in(store, pair_sha256, motion_binding_sha256, cancel)?.to_incarnation)
}

fn rotate_staged_with(
    store: &Store,
    pair_sha256: &str,
    motion_binding_sha256: &str,
    cancel: &AtomicBool,
    after_rotation: impl FnOnce() -> Result<()>,
) -> Result<chain::Origin> {
    let admitted = AdmittedRestoreOwner::acquire(store)?;
    rotate_admitted_with(
        &admitted,
        pair_sha256,
        motion_binding_sha256,
        cancel,
        after_rotation,
    )
}
fn rotate_admitted_with(
    admitted: &AdmittedRestoreOwner<'_>,
    pair_sha256: &str,
    motion_binding_sha256: &str,
    cancel: &AtomicBool,
    after_rotation: impl FnOnce() -> Result<()>,
) -> Result<chain::Origin> {
    rotate_admitted_until(
        admitted,
        pair_sha256,
        motion_binding_sha256,
        cancel,
        None,
        after_rotation,
    )
}
fn rotate_admitted_until(
    admitted: &AdmittedRestoreOwner<'_>,
    pair_sha256: &str,
    motion_binding_sha256: &str,
    cancel: &AtomicBool,
    deadline: Option<std::time::Instant>,
    after_rotation: impl FnOnce() -> Result<()>,
) -> Result<chain::Origin> {
    use std::time::{Duration, Instant};
    // Only the fixed current-image producer supplies an inherited deadline.
    // Existing pristine/staged callers retain their original local policy.
    let live = || -> Result<()> {
        if let Some(deadline) = deadline {
            check(cancel)?;
            if Instant::now() >= deadline {
                return Err(fail("current rotation inherited deadline exceeded"));
            }
        }
        Ok(())
    };
    let inventory_deadline = || {
        let local = Instant::now() + Duration::from_secs(10);
        deadline.map_or(local, |d| d.min(local))
    };
    live()?;
    let db = admitted.database();
    rotation_allocation(db)?;
    let (catalog, incarnation, generation): (String, String, i64) = db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let before = crate::catalog_backup::inventory::capture(db, cancel, inventory_deadline())?;
    let old = prepare_candidate_in(&admitted, &catalog, &incarnation, cancel)?;
    let ancestry = chain::read(db, &catalog, &incarnation, cancel)?;
    rotation_allocation(db)?;
    let origin = chain::Origin {
        contract: 1,
        restore_id: uuid::Uuid::new_v4().to_string(),
        catalog_id: catalog.clone(),
        schema_version: 41,
        from_incarnation: incarnation.clone(),
        to_incarnation: uuid::Uuid::new_v4().simple().to_string(),
        parent_restore_id: ancestry.origins.last().map(|o| o.restore_id.clone()),
        pair_sha256: pair_sha256.into(),
        native_inventory_sha256: before.complete.clone(),
        motion_binding_sha256: motion_binding_sha256.into(),
    };
    drop(ancestry);
    let workers = old
        .owners
        .iter()
        .map(|o| chain::Worker {
            contract: 1,
            restore_id: origin.restore_id.clone(),
            owner_kind: o.kind.clone(),
            owner_id: o.id.clone(),
            origin_incarnation: o.origin_incarnation.clone(),
            tuple_sha256: o.tuple_sha256.clone(),
        })
        .collect::<Vec<_>>();
    check(cancel)?;
    live()?;
    db.execute_batch("SAVEPOINT paired_restore_rotation")?;
    struct Rollback<'a, 'b> {
        admitted: &'a AdmittedRestoreOwner<'b>,
        armed: bool,
    }
    impl Drop for Rollback<'_, '_> {
        fn drop(&mut self) {
            if self.armed {
                // The existing Store cleanup owner masks interruption safely
                // and poisons all SQL if scoped rollback cannot be established.
                let _ = self.admitted.rollback();
            }
        }
    }
    let mut rollback = Rollback {
        admitted,
        armed: true,
    };
    let result = (|| -> Result<()> {
        live()?;
        match deadline {
            Some(deadline) => chain::insert_exact_until(db, &origin, &workers, deadline, cancel)?,
            None => chain::insert_exact(db, &origin, &workers)?,
        }
        live()?;
        db.execute("INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) VALUES('restore',?,?)",rusqlite::params![catalog,incarnation])?;
        live()?;
        if db.execute("UPDATE catalog_state SET incarnation=? WHERE id=1 AND catalog_id=? AND incarnation=? AND generation=?",rusqlite::params![origin.to_incarnation,catalog,incarnation,generation])?!=1 {
            return Err(fail("staged restore identity changed before rotation"));
        }
        live()?;
        db.execute("DELETE FROM catalog_mutation_permits WHERE scope_kind='restore' AND scope_id=? AND incarnation=?",rusqlite::params![catalog,incarnation])?;
        after_rotation()?;
        rotation_allocation(db)?;
        let after = crate::catalog_backup::inventory::capture(db, cancel, inventory_deadline())?;
        if before.schema != after.schema
            || before.ledger != after.ledger
            || before.tables.len() != after.tables.len()
        {
            return Err(fail("staged restore changed schema or migration evidence"));
        }
        for (table, digest) in &before.tables {
            if ![
                "catalog_state",
                "catalog_restore_lineages",
                "catalog_restore_worker_history",
            ]
            .contains(&table.as_str())
                && after.tables.get(table) != Some(digest)
            {
                return Err(fail(
                    "staged restore changed retained data outside exact lineage",
                ));
            }
        }
        let actual: (String, String, i64) = db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if actual != (catalog.clone(), origin.to_incarnation.clone(), generation) {
            return Err(fail("staged restore identity result mismatch"));
        }
        let historical = prepare_candidate_in(&admitted, &catalog, &origin.to_incarnation, cancel)?;
        if historical.owners.len() != old.owners.len()
            || historical.owners.iter().zip(&old.owners).any(|(a, b)| {
                a.kind != b.kind
                    || a.id != b.id
                    || a.tuple_sha256 != b.tuple_sha256
                    || a.origin_incarnation != b.origin_incarnation
            })
        {
            return Err(fail(
                "staged restore changed exact historical worker closure",
            ));
        }
        admitted.validate_path()?;
        rotation_allocation(db)?;
        check(cancel)?;
        live()?;
        Ok(())
    })();
    if let Err(error) = result {
        admitted.rollback()?;
        rollback.armed = false;
        return Err(error);
    }
    live()?;
    db.execute_batch("RELEASE paired_restore_rotation")?;
    rollback.armed = false;
    Ok(origin)
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn candidate() -> (tempfile::TempDir, std::path::PathBuf, Store) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("staged.sqlite3");
        crate::migration::initialize_native_at(&path, 40).unwrap();
        crate::candidate41_test_support::install_candidate41_for_test(&path).unwrap();
        let store =
            crate::candidate41_test_support::open_candidate41_for_test(&path, true).unwrap();
        (root, path, store)
    }
    #[test]
    fn candidate_rotation_preserves_inventory_and_requires_new_owner_for_repeated_restore() {
        let (_root, path, store) = candidate();
        let original = store.catalog_incarnation.clone().unwrap();
        store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        let first = rotate_staged_in(
            &store,
            &"a".repeat(64),
            &"b".repeat(64),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(first.from_incarnation, original);
        assert!(store.validate_native_identity().is_err());
        assert!(
            rotate_staged_in(
                &store,
                &"a".repeat(64),
                &"b".repeat(64),
                &AtomicBool::new(false)
            )
            .is_err()
        );
        store.db.execute_batch("COMMIT").unwrap();
        drop(store);
        let reopened =
            crate::candidate41_test_support::open_candidate41_for_test(&path, true).unwrap();
        assert_eq!(
            reopened.catalog_incarnation.as_deref(),
            Some(first.to_incarnation.as_str())
        );
        reopened.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        let second = rotate_staged_in(
            &reopened,
            &"c".repeat(64),
            &"d".repeat(64),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            second.parent_restore_id.as_deref(),
            Some(first.restore_id.as_str())
        );
        assert_eq!(second.from_incarnation, first.to_incarnation);
        assert_ne!(second.to_incarnation, original);
        assert_eq!(
            chain::read(
                &reopened.db,
                &second.catalog_id,
                &second.to_incarnation,
                &AtomicBool::new(false)
            )
            .unwrap()
            .origins
            .len(),
            2
        );
        reopened.db.execute_batch("ROLLBACK").unwrap();
        reopened.validate_native_identity().unwrap();
    }
    #[test]
    fn invalid_commitment_and_cancel_leave_candidate_identity_and_lineage_unchanged() {
        let (_root, _path, store) = candidate();
        let original = store.catalog_incarnation.clone().unwrap();
        store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(
            rotate_staged_in(&store, "invalid", &"b".repeat(64), &AtomicBool::new(false)).is_err()
        );
        assert!(
            rotate_staged_in(
                &store,
                &"a".repeat(64),
                &"b".repeat(64),
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert_eq!(
            store
                .db
                .query_row::<String, _, _>(
                    "SELECT incarnation FROM catalog_state WHERE id=1",
                    [],
                    |r| r.get(0)
                )
                .unwrap(),
            original
        );
        assert_eq!(
            store
                .db
                .query_row::<i64, _, _>("SELECT count(*) FROM catalog_restore_lineages", [], |r| r
                    .get(0))
                .unwrap(),
            0
        );
        store.validate_native_identity().unwrap();
        store.db.execute_batch("ROLLBACK").unwrap();
    }
    #[test]
    fn post_rotation_failure_and_unwind_rollback_before_outer_commit() {
        for panic in [false, true] {
            let (_root, _path, store) = candidate();
            let original = store.catalog_incarnation.clone().unwrap();
            store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                rotate_staged_with(
                    &store,
                    &"a".repeat(64),
                    &"b".repeat(64),
                    &AtomicBool::new(false),
                    || {
                        assert_ne!(
                            store
                                .db
                                .query_row::<String, _, _>(
                                    "SELECT incarnation FROM catalog_state WHERE id=1",
                                    [],
                                    |r| r.get(0)
                                )
                                .unwrap(),
                            original
                        );
                        if panic {
                            panic!("actual staged post-rotation failure");
                        }
                        Err(fail("actual staged post-rotation verification refusal"))
                    },
                )
            }));
            if panic {
                assert!(outcome.is_err());
            } else {
                assert!(outcome.unwrap().is_err());
            }
            store.validate_native_identity().unwrap();
            store.db.execute_batch("COMMIT").unwrap();
            assert_eq!(
                store
                    .db
                    .query_row::<String, _, _>(
                        "SELECT incarnation FROM catalog_state WHERE id=1",
                        [],
                        |r| r.get(0)
                    )
                    .unwrap(),
                original
            );
            assert_eq!(
                store
                    .db
                    .query_row::<i64, _, _>(
                        "SELECT count(*) FROM catalog_restore_lineages",
                        [],
                        |r| r.get(0)
                    )
                    .unwrap(),
                0
            );
        }
    }
}
