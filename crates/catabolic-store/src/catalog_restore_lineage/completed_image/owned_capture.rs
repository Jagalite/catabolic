//! Qualified current-only copier. The ordinary six-image path is unchanged.
//! Real overlap is reduced: one backup plus one owned serialization, then an
//! explicitly closed backup before that same allocation enters the verifier.
use super::*;

/// Explicit close failure remains attached to the poisoned original session.
/// Payloads die before their coverage, and before that session's Store/locks.
/// This is retained failure custody, not a verified-close or retry grant.
pub(crate) struct FailedOwnedCapture {
    #[cfg(test)]
    _pending: Option<PendingStatement>,
    _db: Connection,
    _coverage: crate::migration::OwnedImageAllocation,
}

#[cfg(test)]
struct PendingStatement(usize);
#[cfg(test)]
impl Drop for PendingStatement {
    fn drop(&mut self) {
        // SAFETY: this exact test-created statement has one owner, remains
        // associated with the retained copy, and is finalized before that copy.
        unsafe { rusqlite::ffi::sqlite3_finalize(self.0 as *mut rusqlite::ffi::sqlite3_stmt); }
    }
}
#[cfg(test)]
impl FailedOwnedCapture {
    pub(crate) fn retained_busy_copy_for_test(&self) -> bool {
        self._pending.is_some() && crate::catalog_backup::allocations(&self._db).is_ok_and(|n| n != 0)
    }
}

fn account(
    store: &Store, allocation: &mut crate::migration::OfflineImageAllocation<'_>,
    bytes: u64, copy: Option<&Connection>,
) -> Result<()> {
    let sqlite = copy.map(crate::catalog_backup::allocations).transpose()?.unwrap_or(0);
    // One whole image is admitted BEFORE Backup steps: it covers unobserved
    // next-step page payload growth. Once Backup ends, the identical reserve
    // covers the one separately owned serializer buffer, then the same buffer
    // owned by the verifier. DBSTATUS is always additional, never substituted
    // for serialized backing. There is never a second deserialize allocation.
    // SQLite metadata retains the original fixed metadata loan convention.
    let requested = bytes.checked_add(sqlite).ok_or_else(|| fail("owned current image allocation overflow"))?;
    allocation.admit(requested)?;
    #[cfg(test)]
    allocation.record_capture_phase_for_test("copier_phase", bytes, sqlite, 0, requested)?;
    Ok(())
}

// This is only a best-effort release of clean, purgeable pages on the
// original file-backed source. It neither lends accounting capacity nor
// changes SQL/transaction state. The fixed caller measures full DBSTATUS
// immediately afterward and still checks every later backup/cache growth.
fn release_snapshot_cache(db: &Connection) -> Result<()> {
    if db.transaction_state(Some("main"))? != rusqlite::TransactionState::Read {
        return Err(fail("owned capture cache retirement requires source READ"));
    }
    // Optional optimization only: no purgeability claim for memory/temporary
    // databases. Preserve their existing full accounting without attempting it.
    if db.path().map_or(true, str::is_empty) { return Ok(()); }
    db.release_memory()?;
    Ok(())
}

// Only the owned-deserialize verifier reaches this boundary, after complete
// validation and inventory D. Its memdb VFS retains the owned RESIZEABLE
// serialization separately from its clean, purgeable pager cache; it is not
// an ordinary :memory: pager and no READONLY storage flag is claimed. Public
// release_memory may retire clean pages in SQLite's shared global LRU too.
// No freed amount is credited: full accounting surrounds this call, and the
// unchanged output seal later measures the actual verifier again.
fn release_verified_cache(db: &Connection) -> Result<()> {
    if !db.is_autocommit()
        || db.transaction_state(Some("main"))? != rusqlite::TransactionState::None
    {
        return Err(fail("owned verifier cache retirement requires completed reads"));
    }
    db.release_memory()?;
    Ok(())
}

fn inventory(
    store: &Store, proof: &crate::migration::CurrentImageProof<'_, '_>,
    db: &Connection, cancel: &AtomicBool, deadline: Instant,
) -> Result<crate::migration::CurrentImageInventory> {
    proof.control_workspace().ok_or_else(|| fail("owned current image workspace missing"))?
        .capture_paid_current_inventory(&PaidCurrentImageInventory { source: store, image: db }, cancel, deadline)
}

pub(super) fn capture(
    store: &Store, proof: &mut crate::migration::CurrentImageProof<'_, '_>,
    cancel: &AtomicBool, deadline: Instant, fault: CaptureFault,
) -> Result<ValidatedCompletedImage> {
    proof.validate_source(store)?;
    if !store.db.is_autocommit() { return Err(fail("completed image ambient transaction")); }
    let mut allocation = proof.image_allocation()?;
    live(cancel, deadline)?;
    store.db.execute_batch("BEGIN")?;
    struct Snapshot<'a>(&'a Store, bool);
    impl Drop for Snapshot<'_> {
        fn drop(&mut self) { if self.1 { let _ = self.0.rollback_transaction(None); } }
    }
    let mut snapshot = Snapshot(store, true);
    let bytes = size(&store.db)?;
    // Complete A=B comparison remains, but B is not the returned inventory.
    // Destroy B and its independent output guard before constructing a copy.
    {
        let source = inventory(store, proof, &store.db, cancel, deadline)?;
        proof.validate_inventory(&source.audit)?;
    }
    #[cfg(test)]
    if matches!(fault, CaptureFault::CancelAfterSnapshot) { cancel.store(true, std::sync::atomic::Ordering::Release); }
    live(cancel, deadline)?;
    // B and its cursor/output are gone. Keep this exact original READ snapshot
    // and all custody while asking SQLite to release unused source cache. No
    // reclaimed byte count is assumed: the following existing admission takes
    // a fresh full source measurement. Backup repopulation remains fully paid.
    release_snapshot_cache(&store.db)?;
    live(cancel, deadline)?;
    account(store, &mut allocation, bytes, None)?;
    let control = proof.control_workspace().ok_or_else(|| fail("owned current image workspace missing"))?;
    let mut db = Connection::open_in_memory()?;
    control.configure_owned_current_copy(&PaidCurrentImageInventory { source: store, image: &db }, &mut allocation, bytes)?;
    account(store, &mut allocation, bytes, Some(&db))?;
    {
        // SAFETY: same fixed Backup lifetime as the ordinary copier. Only
        // DBSTATUS is sampled while Backup holds the target mutable borrow.
        let handle = unsafe { db.handle() };
        let copy = Backup::new(&store.db, &mut db)?;
        loop {
            live(cancel, deadline)?;
            let sqlite = unsafe { crate::catalog_backup::allocations_handle(handle)? };
            allocation.admit(bytes.checked_add(sqlite).ok_or_else(|| fail("owned backup allocation overflow"))?)?;
            match control.step_owned_current_copy(&PaidCurrentBackup { source: store, copy: &copy, target: handle }, &mut allocation, bytes)? {
                StepResult::Done => break,
                StepResult::More => (),
                StepResult::Busy | StepResult::Locked => return Err(fail("completed image snapshot unexpectedly busy")),
                _ => return Err(fail("completed image unknown backup state")),
            }
        }
    }
    if size(&db)? != bytes { return Err(fail("owned backup image size changed")); }
    account(store, &mut allocation, bytes, Some(&db))?;
    db.progress_handler(10000, Some(move || Instant::now() >= deadline))?;
    #[cfg(test)]
    if matches!(fault, CaptureFault::CorruptCopy) { db.execute_batch("CREATE TABLE unexpected_current_image_copy(id INTEGER)")?; }
    control.validate_owned_current_copy(&PaidCurrentImageInventory { source: store, image: &db }, proof, &mut allocation, bytes)?;
    {
        let copied = control.inventory_owned_current_copy(&PaidCurrentImageInventory { source: store, image: &db }, &mut allocation, bytes, cancel, deadline)?;
        proof.validate_inventory(&copied.audit)?;
    }
    account(store, &mut allocation, bytes, Some(&db))?;
    live(cancel, deadline)?;
    // Full buffer + actual producer DBSTATUS are admitted before serialize.
    // MAIN_DB cannot fail schema-name conversion after OwnedData consumption.
    let (serialized, serialized_bytes) = proof.control_workspace().ok_or_else(|| fail("owned current image workspace missing"))?
        .serialize_paid_current_owned(&PaidCurrentImageInventory { source: store, image: &db }, bytes, &mut allocation)?;
    account(store, &mut allocation, bytes, Some(&db))?;
    #[cfg(test)]
    if matches!(fault, CaptureFault::CancelAfterOwnedSerialize) { cancel.store(true, std::sync::atomic::Ordering::Release); }
    #[cfg(test)]
    if matches!(fault, CaptureFault::PanicAfterOwnedSerialize) { panic!("owned serializer return unwind"); }
    live(cancel, deadline)?;
    #[cfg(test)]
    let pending = if matches!(fault, CaptureFault::BusyProducerClose) {
        let mut statement = std::ptr::null_mut();
        // SAFETY: fixed literal, live exact copied handle, output pointer owned
        // by PendingStatement. It is intentionally retained across real close
        // to induce SQLite's actual SQLITE_BUSY, never a fabricated outcome.
        let code = unsafe { rusqlite::ffi::sqlite3_prepare_v2(db.handle(), c"SELECT 1".as_ptr(), -1, &mut statement, std::ptr::null_mut()) };
        if code != rusqlite::ffi::SQLITE_OK || statement.is_null() { return Err(fail("close test statement preparation failed")); }
        Some(PendingStatement(statement as usize))
    } else { None };
    match db.close() {
        Ok(()) => (),
        Err((db, error)) => {
            // Do not drop the returned connection and pretend close succeeded.
            // The same original owner retains all real payloads and coverage.
            // OwnedData has no borrowed aliases and is not needed on a failed
            // close. Destroy that actual buffer now while full coverage lives;
            // keep the returned connection and conservative guard on the source.
            drop(serialized);
            proof.retain_failed_copy(FailedOwnedCapture {
                #[cfg(test)]
                _pending: pending,
                _db: db, _coverage: allocation.into_owned(),
            });
            // Stable failure custody is installed BEFORE rollback can poll or
            // unwind. Poison on every exit, but only after allowing cleanup.
            struct Poison<'a>(&'a Store);
            impl Drop for Poison<'_> { fn drop(&mut self) { self.0.poison_transaction_owner(); } }
            let poison = Poison(store);
            let cleanup = store.rollback_transaction(None);
            // Failed cleanup remains retained and poisoned, never retried as an
            // admitted operation or reported as a successful rollback.
            snapshot.1 = false;
            drop(error);
            drop(cleanup);
            drop(poison);
            return Err(fail("owned current image producer close failed"));
        }
    }
    #[cfg(test)]
    if matches!(fault, CaptureFault::CancelAfterProducerClose) { cancel.store(true, std::sync::atomic::Ordering::Release); }
    #[cfg(test)]
    if matches!(fault, CaptureFault::PanicAfterProducerClose) { panic!("owned producer close unwind"); }
    live(cancel, deadline)?;
    let mut verified = Connection::open_in_memory()?;
    control.configure_owned_current_copy(&PaidCurrentImageInventory { source: store, image: &verified }, &mut allocation, bytes)?;
    account(store, &mut allocation, bytes, Some(&verified))?;
    #[cfg(test)]
    if matches!(fault, CaptureFault::DeniedOwnedAttach) {
        verified.authorizer(Some(|_: rusqlite::hooks::AuthContext<'_>| rusqlite::hooks::Authorization::Deny))?;
    }
    control.attach_owned_current_copy(&mut PaidCurrentImageAttachment { source: store, image: &mut verified }, serialized, &mut allocation, bytes)?;
    #[cfg(test)]
    if matches!(fault, CaptureFault::PanicAfterOwnedAttach) { panic!("owned verifier attach unwind"); }
    control.configure_owned_current_copy(&PaidCurrentImageInventory { source: store, image: &verified }, &mut allocation, bytes)?;
    verified.progress_handler(10000, Some(move || Instant::now() >= deadline))?;
    account(store, &mut allocation, bytes, Some(&verified))?;
    control.validate_owned_current_copy(&PaidCurrentImageInventory { source: store, image: &verified }, proof, &mut allocation, bytes)?;
    {
        let verified_inventory = control.inventory_owned_current_copy(&PaidCurrentImageInventory { source: store, image: &verified }, &mut allocation, bytes, cancel, deadline)?;
        proof.validate_inventory(&verified_inventory.audit)?;
    }
    account(store, &mut allocation, bytes, Some(&verified))?;
    live(cancel, deadline)?;
    #[cfg(test)]
    allocation.record_capture_phase_for_test("verifier_retirement_enter", bytes,
        crate::catalog_backup::allocations(&verified)?, 0,
        bytes.checked_add(crate::catalog_backup::allocations(&verified)?)
            .ok_or_else(|| fail("owned verifier retirement allocation overflow"))?)?;
    #[cfg(test)]
    let _pressure = if matches!(fault, CaptureFault::PressureBeforeVerifierCacheRetirement) {
        Some(proof.verifier_retirement_pressure_for_test()?)
    } else { None };
    #[cfg(test)]
    let _pressure_vm = if _pressure.is_some() {
        // A real live source VM pushes the fresh total over the unchanged cap.
        // The pre-release check must refuse before public cache retirement.
        Some(store.db.prepare("SELECT 1, 2, 3, 4, 5, 6, 7, 8")?)
    } else { None };
    #[cfg(test)]
    if matches!(fault, CaptureFault::CancelBeforeVerifierCacheRetirement) {
        cancel.store(true, std::sync::atomic::Ordering::Release);
    }
    #[cfg(test)]
    if matches!(fault, CaptureFault::PanicBeforeVerifierCacheRetirement) {
        panic!("owned verifier pre-retirement unwind");
    }
    live(cancel, deadline)?;
    // Prove the entire live pre-retirement phase fits, including the original
    // source, owned backing, and actual verifier CACHE/SCHEMA/STMT categories.
    account(store, &mut allocation, bytes, Some(&verified))?;
    #[cfg(test)]
    allocation.record_capture_phase_for_test("verifier_retirement_prechecked", bytes,
        crate::catalog_backup::allocations(&verified)?, 0,
        bytes.checked_add(crate::catalog_backup::allocations(&verified)?)
            .ok_or_else(|| fail("owned verifier retirement allocation overflow"))?)?;
    release_verified_cache(&verified)?;
    // Construction high-water stays live; this does not lower any guard. The
    // exact seal below alone retires obsolete coverage after fresh full checks.
    account(store, &mut allocation, bytes, Some(&verified))?;
    #[cfg(test)]
    allocation.record_capture_phase_for_test("verifier_retirement_accounted", bytes,
        crate::catalog_backup::allocations(&verified)?, 0,
        bytes.checked_add(crate::catalog_backup::allocations(&verified)?)
            .ok_or_else(|| fail("owned verifier retirement allocation overflow"))?)?;
    #[cfg(test)]
    if matches!(fault, CaptureFault::CancelAfterVerifierCacheRetirement) {
        cancel.store(true, std::sync::atomic::Ordering::Release);
    }
    #[cfg(test)]
    if matches!(fault, CaptureFault::PanicAfterVerifierCacheRetirement) {
        panic!("owned verifier post-retirement unwind");
    }
    live(cancel, deadline)?;
    verified.progress_handler(0, None::<fn() -> bool>)?;
    store.rollback_transaction(None)?;
    snapshot.1 = false;
    // Preadmit exact string capacity convention before constructing strings.
    let labels = [proof.catalog_id(), proof.incarnation(), proof.transition_digest()];
    let label_bytes = labels.into_iter().try_fold(0u64, |n, s| n.checked_add((s.len() as u64).checked_mul(2)?))
        .ok_or_else(|| fail("owned image label allocation overflow"))?;
    allocation.admit(bytes.checked_add(crate::catalog_backup::allocations(&verified)?)
        .and_then(|n| n.checked_add(label_bytes)).ok_or_else(|| fail("owned image output allocation overflow"))?)?;
    let catalog = proof.catalog_id().to_owned();
    let incarnation = proof.incarnation().to_owned();
    let transition = proof.transition_digest().to_owned();
    // A's original independent guard moves into the same phase owner. No map
    // clone, second reservation, or unguarded output-return interval exists.
    let mut source = proof.take_inventory()?;
    allocation.absorb_current_inventory(&mut source)?;
    let mut image = ValidatedCompletedImage {
        db: verified, inventory: source.audit, catalog, incarnation, transition,
        bytes: serialized_bytes, capture_retention: Some(allocation.into_owned()),
    };
    drop(source.retention);
    // The backup is explicitly closed, serialization is owned by the verifier,
    // and C/D cursors and inventories are gone. Retire only obsolete temporary
    // construction headroom before the final original-source validation can
    // repopulate its cache. The exact immutable payload keeps its own guard;
    // history remains separately charged until the unchanged persistent commit.
    image.seal_current_output(proof)?;
    #[cfg(test)]
    image.record_capture_phase_for_test("copier_output_installed")?;
    #[cfg(test)]
    if matches!(fault, CaptureFault::CancelBeforeFinalSource) { cancel.store(true, std::sync::atomic::Ordering::Release); }
    live(cancel, deadline)?;
    proof.validate_source(store)?;
    // Check the complete final scanner working set BEFORE any retirement or
    // persistent guard transfer. A smaller later cache cannot hide this peak.
    proof.checkpoint_source_allocation(store)?;
    #[cfg(test)]
    image.record_capture_phase_for_test("final_source_validated")?;
    live(cancel, deadline)?;
    Ok(image)
}


#[cfg(test)]
mod tests {
    use super::*;
    fn sqlite_component(db: &Connection, operation: i32) -> u64 {
        let mut current = 0;
        let mut highwater = 0;
        // SAFETY: this exact live connection is exclusively owned by the test;
        // DBSTATUS only writes the two valid output integers, with reset=false.
        let code = unsafe { rusqlite::ffi::sqlite3_db_status(db.handle(), operation,
            &mut current, &mut highwater, 0) };
        assert_eq!(code, rusqlite::ffi::SQLITE_OK);
        u64::try_from(current).unwrap()
    }
    #[test]
    fn owned_capture_retires_only_actual_source_cache_and_preserves_read_custody() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.sqlite");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch("CREATE TABLE sample(value BLOB); WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<64) INSERT INTO sample SELECT zeroblob(8192) FROM n;").unwrap();
        }
        let before_file = std::fs::read(&path).unwrap();
        let db = Connection::open(&path).unwrap();
        db.execute_batch("BEGIN").unwrap();
        let read = || db.query_row("SELECT sum(length(hex(value))) FROM sample", [],
            |row| row.get::<_, i64>(0)).unwrap();
        let expected = read();
        assert_eq!(expected, 64 * 8192 * 2);
        let handle = unsafe { db.handle() };
        let cache_before = sqlite_component(&db, rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED);
        let schema_before = sqlite_component(&db, rusqlite::ffi::SQLITE_DBSTATUS_SCHEMA_USED);
        let statements_before = sqlite_component(&db, rusqlite::ffi::SQLITE_DBSTATUS_STMT_USED);
        // No SQL statement may be prepared/executed by the release helper.
        db.authorizer(Some(|_: rusqlite::hooks::AuthContext<'_>|
            rusqlite::hooks::Authorization::Deny)).unwrap();
        release_snapshot_cache(&db).unwrap();
        let cache_after = sqlite_component(&db, rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED);
        assert!(cache_after < cache_before);
        assert_eq!(sqlite_component(&db, rusqlite::ffi::SQLITE_DBSTATUS_SCHEMA_USED), schema_before);
        assert_eq!(sqlite_component(&db, rusqlite::ffi::SQLITE_DBSTATUS_STMT_USED), statements_before);
        assert_eq!(unsafe { db.handle() }, handle);
        assert!(!db.is_autocommit());
        assert_eq!(db.transaction_state(Some("main")).unwrap(), rusqlite::TransactionState::Read);
        db.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization>).unwrap();
        let foreign = Connection::open(&path).unwrap();
        foreign.busy_timeout(std::time::Duration::ZERO).unwrap();
        let blocked = foreign.execute_batch("BEGIN EXCLUSIVE").unwrap_err();
        assert!(matches!(blocked.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)));
        assert!(foreign.is_autocommit());
        assert_eq!(std::fs::read(&path).unwrap(), before_file);
        let after_total = crate::catalog_backup::allocations(&db).unwrap();
        // Later reads really repopulate cache. Accounting must use the new
        // measured value, not retain the smaller post-release measurement.
        assert_eq!(read(), expected);
        assert!(sqlite_component(&db, rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED) > cache_after);
        assert!(crate::catalog_backup::allocations(&db).unwrap() > after_total);
        assert_eq!(std::fs::read(&path).unwrap(), before_file);
        db.execute_batch("ROLLBACK").unwrap();
        foreign.execute_batch("BEGIN EXCLUSIVE; ROLLBACK").unwrap();
    }
    #[test]
    fn owned_capture_cache_retirement_skips_memory_and_refuses_write_or_unstarted_sources() {
        let memory = Connection::open_in_memory().unwrap();
        memory.execute_batch("CREATE TABLE sample(value); BEGIN; SELECT * FROM sample").unwrap();
        let memory_before = crate::catalog_backup::allocations(&memory).unwrap();
        release_snapshot_cache(&memory).unwrap();
        assert_eq!(crate::catalog_backup::allocations(&memory).unwrap(), memory_before);
        assert_eq!(memory.transaction_state(Some("main")).unwrap(), rusqlite::TransactionState::Read);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE sample(value); INSERT INTO sample VALUES(1)").unwrap();
        assert!(release_snapshot_cache(&db).is_err());
        assert!(db.is_autocommit());
        db.execute_batch("BEGIN IMMEDIATE; UPDATE sample SET value=2").unwrap();
        assert!(release_snapshot_cache(&db).is_err());
        assert_eq!(db.transaction_state(Some("main")).unwrap(), rusqlite::TransactionState::Write);
        assert_eq!(db.query_row("SELECT value FROM sample", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        db.execute_batch("ROLLBACK; BEGIN; SELECT * FROM sample").unwrap();
        release_snapshot_cache(&db).unwrap();
        release_snapshot_cache(&db).unwrap();
        assert_eq!(db.query_row("SELECT value FROM sample", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(db.transaction_state(Some("main")).unwrap(), rusqlite::TransactionState::Read);
        db.execute_batch("ROLLBACK").unwrap();
    }
    #[test]
    fn owned_current_serialization_transfers_buffer_and_refuses_shared_without_copy() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE sample(value TEXT); INSERT INTO sample VALUES('owned transfer')").unwrap();
        let (data, bytes) = {
            let data = db.serialize(rusqlite::MAIN_DB).unwrap();
            let len = data.len() as u64;
            take_owned_current_serialization(data, len).unwrap()
        };
        db.close().unwrap();
        let mut verified = Connection::open_in_memory().unwrap();
        verified.deserialize(rusqlite::MAIN_DB, data, false).unwrap();
        assert_eq!(verified.query_row("SELECT value FROM sample", [], |r| r.get::<_, String>(0)).unwrap(), "owned transfer");
        {
            let shared = verified.serialize(rusqlite::MAIN_DB).unwrap();
            assert!(matches!(&shared, rusqlite::serialize::Data::Shared(_)));
            let error = match take_owned_current_serialization(shared, bytes) {
                Err(error) => error,
                Ok(_) => panic!("shared backing was silently copied or rebranded"),
            };
            assert_eq!(error.to_string(), "current owned serializer returned shared storage");
        }
        assert_eq!(verified.query_row("SELECT count(*) FROM sample", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }
}

#[cfg(test)]
#[path = "verifier_cache_tests.rs"]
mod verifier_cache_tests;
