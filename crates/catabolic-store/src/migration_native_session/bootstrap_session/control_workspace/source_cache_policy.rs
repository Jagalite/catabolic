//! Fixed qualified-capture source working set. No scalar accounting credit,
//! hook replacement, durable setting, or arbitrary connection escapes here.
use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

const WORKING_SET_KIB: i32 = 1024;
const WORKING_SET_BYTES: u64 = WORKING_SET_KIB as u64 * 1024;

fn failed() -> Error { Error("current source cache policy unavailable".into()) }
fn file_backed(db: &Connection) -> bool { db.path().is_some_and(|path| !path.is_empty()) }

// SQLite's signed setting is pages when positive and KiB when negative. Work
// in widened checked arithmetic, including i32::MIN, and never enlarge a
// smaller caller setting. This is a soft cache target, not a memory guarantee.
fn needs_policy(old: i64, page: i64) -> Result<bool> {
    let old = i32::try_from(old).map_err(|_| failed())?;
    let page = u64::try_from(page).ok().filter(|value| *value != 0).ok_or_else(failed)?;
    let bytes = if old < 0 {
        u64::try_from(-i64::from(old)).map_err(|_| failed())?
            .checked_mul(1024).ok_or_else(failed)?
    } else {
        u64::try_from(old).map_err(|_| failed())?.checked_mul(page).ok_or_else(failed)?
    };
    Ok(bytes > WORKING_SET_BYTES)
}

// Private to the exact source loan. Every SQL VM remains alive across fresh
// source DBSTATUS checks. The fixed formatting buffer is metadata, not a
// substitute for actual CACHE_USED/SCHEMA_USED/STMT_USED.
fn source_context<'a>(control: &'a BootstrapWorkspace<'_>) -> Result<&'a allocation::AllocationContext> {
    Ok(&control.store.offline_bootstrap.as_ref().ok_or_else(failed)?.allocation)
}
fn checkpoint(control: &BootstrapWorkspace<'_>) -> Result<()> {
    source_context(control)?.check(Some(&control.store.db), 0)
}
fn read_pragma(control: &BootstrapWorkspace<'_>, page_size: bool) -> Result<i64> {
    let context = source_context(control)?;
    checkpoint(control)?;
    let mut loan = control.loan.try_borrow_mut().map_err(|_| failed())?;
    let _scratch = allocation::MetadataReservation::new(context, &control.store.db, Some(&mut loan))?;
    let sql = if page_size { "PRAGMA main.page_size" } else { "PRAGMA main.cache_size" };
    let mut statement = control.store.db.prepare(sql)?;
    checkpoint(control)?;
    let mut rows = statement.query([])?;
    let value = {
        let stepped = rows.next();
        let accounted = checkpoint(control);
        let row = stepped?.ok_or_else(failed)?;
        accounted?;
        let read = row.get::<_, i64>(0);
        let accounted = checkpoint(control);
        let value = read?;
        accounted?;
        value
    };
    let stepped = rows.next();
    let accounted = checkpoint(control);
    let extra = stepped?;
    accounted?;
    if extra.is_some() { return Err(failed()); }
    drop(rows);
    checkpoint(control)?;
    Ok(value)
}
fn set_pragma(control: &BootstrapWorkspace<'_>, value: i32) -> Result<()> {
    let context = source_context(control)?;
    checkpoint(control)?;
    let mut loan = control.loan.try_borrow_mut().map_err(|_| failed())?;
    let _scratch = allocation::MetadataReservation::new(context, &control.store.db, Some(&mut loan))?;
    // At most 35 ASCII bytes including the widest signed i32; no caller SQL.
    let sql = format!("PRAGMA main.cache_size={value}");
    // SQLite applies cache_size during PREPARE. The owning guard must already
    // be armed (or explicit restoration must already own poison custody).
    let mut statement = control.store.db.prepare(&sql)?;
    checkpoint(control)?;
    let executed = statement.execute([]);
    let accounted = checkpoint(control);
    executed?;
    accounted?;
    Ok(())
}

struct PoisonUnlessRestored<'a> { store: &'a Store, armed: bool }
impl Drop for PoisonUnlessRestored<'_> {
    fn drop(&mut self) { if self.armed { self.store.poison_transaction_owner(); } }
}
#[derive(Clone, Copy)]
enum RestoreFault { None, #[cfg(test)] Panic }

pub(in crate::migration) struct SourceCachePolicy<'a, 'workspace> {
    control: &'a BootstrapWorkspace<'workspace>,
    original: i32,
    pending: bool,
    finished: bool,
    #[cfg(test)]
    restoration_attempts: usize,
}
impl<'a, 'workspace> SourceCachePolicy<'a, 'workspace> {
    pub(in crate::migration) fn begin(
        store: &Store, admission: &allocation::AllocationGuard,
        control: &'a BootstrapWorkspace<'workspace>,
    ) -> Result<Option<Self>> {
        if !std::ptr::eq(store, control.store) || store.schema_version != 41
            || !store.writable || store.lock.is_none() || store.transaction_poisoned.get()
            || !store.db.is_autocommit()
            || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::None
        { return Err(failed()); }
        admission.require_same_source(source_context(control)?, &store.db)?;
        // Always prove the old live source/guards fit before changing anything.
        checkpoint(control)?;
        if !file_backed(&store.db) { return Ok(None); }
        let original = read_pragma(control, false)?;
        let page = read_pragma(control, true)?;
        if !needs_policy(original, page)? { return Ok(None); }
        let policy = Self { control, original: i32::try_from(original).map_err(|_| failed())?,
            pending: true, finished: false,
            #[cfg(test)] restoration_attempts: 0,
        };
        // Armed before prepare: even a failed assignment/readback may have
        // changed SQLite's policy. Drop restores or poisons the original owner.
        set_pragma(control, -WORKING_SET_KIB)?;
        if read_pragma(control, false)? != i64::from(-WORKING_SET_KIB) { return Err(failed()); }
        checkpoint(control)?;
        Ok(Some(policy))
    }
    fn restore(&mut self, fault: RestoreFault) -> Result<()> {
        if self.finished || !self.pending { return Err(failed()); }
        let mut poison = PoisonUnlessRestored { store: self.control.store, armed: true };
        // Consume BEFORE preparing restore SQL. An uncertain failure cannot be
        // retried by Drop, and a panic still poisons without replacing itself.
        self.pending = false;
        self.finished = true;
        #[cfg(test)]
        { self.restoration_attempts += 1; }
        #[cfg(test)]
        if matches!(fault, RestoreFault::Panic) { panic!("controlled current cache restore unwind"); }
        #[cfg(not(test))]
        let _ = fault;
        set_pragma(self.control, self.original)?;
        if read_pragma(self.control, false)? != i64::from(self.original)
            || !self.control.store.db.is_autocommit()
            || self.control.store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::None
        { return Err(failed()); }
        poison.armed = false;
        // Exact original policy is verified. A later aggregate refusal does
        // not require another restore or pretend restoration itself failed.
        checkpoint(self.control)?;
        Ok(())
    }
    pub(in crate::migration) fn finish(&mut self) -> Result<()> { self.restore(RestoreFault::None) }
    #[cfg(test)]
    pub(in crate::migration) fn finish_with_fault_for_test(&mut self, panic: bool) -> Result<()> {
        self.restore(if panic { RestoreFault::Panic } else { RestoreFault::None })
    }
    #[cfg(test)]
    pub(in crate::migration) fn restoration_attempts_for_test(&self) -> usize { self.restoration_attempts }
}
impl Drop for SourceCachePolicy<'_, '_> {
    fn drop(&mut self) {
        if self.pending {
            // Preserve the primary error/panic. Restoration uses existing hooks
            // and accounting, never suppresses cancellation or heals poison.
            // restore's own guard poisons only while exact restoration is
            // unverified. A later accounting refusal must not reclassify that
            // verified restoration or trigger a second cleanup attempt.
            let _ = catch_unwind(AssertUnwindSafe(|| self.restore(RestoreFault::None)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_policy_comparison_preserves_smaller_settings_without_overflow() {
        for value in [0, -1, -1024, 1, 256] { assert!(!needs_policy(value, 4096).unwrap()); }
        for value in [-1025, -2048, 257, i64::from(i32::MIN), i64::from(i32::MAX)] {
            assert!(needs_policy(value, 4096).unwrap());
        }
        assert!(needs_policy(i64::MAX, 4096).is_err());
        assert!(needs_policy(i64::MIN, 4096).is_err());
        assert!(needs_policy(1, 0).is_err());
        assert!(needs_policy(1, -4096).is_err());
        assert!(needs_policy(i64::from(i32::MAX), i64::MAX).is_err());
    }
    #[test]
    fn cache_policy_file_predicate_never_claims_memory_or_temporary_purgeability() {
        let memory = Connection::open_in_memory().unwrap();
        let temporary = Connection::open("").unwrap();
        let root = tempfile::tempdir().unwrap();
        let file = Connection::open(root.path().join("cache.sqlite")).unwrap();
        assert!(!file_backed(&memory));
        assert!(!file_backed(&temporary));
        assert!(file_backed(&file));
    }
}
