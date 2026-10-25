//! Exact paid copied-image accounting. No ambient/TLS registration or generic
//! foreign-connection discount exists. Each consumer explicitly borrows the
//! reservation AND its actual live copied connection through a non-Clone loan.
use super::*;
use std::{cell::{Cell, RefCell}, marker::PhantomData, rc::Rc};

pub(in crate::migration::native_session) const MAX_COPIED_IMAGE_BYTES: u64 = 56 * 1024 * 1024;
struct PaidImage {
    guard: AllocationGuard,
    image_handle: Option<usize>,
    original_bytes: Option<u64>,
    image_highwater: u64,
    sqlite_highwater: u64,
    charged: u64,
}
/// Declared before the copied Connection. It retains the independently charged
/// four-image allowance and copied SQLite high-water until AFTER that database
/// closes, including error/unwind. A borrowed token never owns or drops it.
pub(in crate::migration::native_session) struct CopiedImageReservation {
    context: AllocationContext,
    source_handle: usize,
    paid: RefCell<PaidImage>,
    loaned: Cell<bool>,
    started: Cell<bool>,
    _thread: PhantomData<Rc<()>>,
}
impl CopiedImageReservation {
    pub(in crate::migration::native_session) fn new(
        context: &AllocationContext,
        source: &Connection,
    ) -> Result<Self> {
        require_context(context, source)?;
        Ok(Self {
            context: context.clone(),
            source_handle: identity(source),
            paid: RefCell::new(PaidImage {
                guard: context.reserve(source, 0)?,
                image_handle: None,
                original_bytes: None,
                image_highwater: 0,
                sqlite_highwater: 0,
                charged: 0,
            }),
            loaned: Cell::new(false),
            started: Cell::new(false),
            _thread: PhantomData,
        })
    }
    pub(in crate::migration::native_session) fn checkpoint(
        &self,
        source: &Connection,
        image: &Connection,
        original_bytes: u64,
    ) -> Result<()> {
        if self.loaned.get() {
            return Err(refused());
        }
        self.observe(source, image, original_bytes, true)
    }
    fn observe(
        &self,
        source: &Connection,
        image: &Connection,
        original_bytes: u64,
        check_pages: bool,
    ) -> Result<()> {
        require_context(&self.context, source)?;
        let source_handle = identity(source);
        let image_handle = identity(image);
        if source_handle != self.source_handle || image_handle == source_handle {
            return Err(refused());
        }
        let mut paid = self.paid.try_borrow_mut().map_err(|_| refused())?;
        if paid.image_handle.is_some_and(|held| held != image_handle)
            || paid.original_bytes.is_some_and(|held| held != original_bytes)
            || !Arc::ptr_eq(&paid.guard.context.0, &self.context.0)
            || !matches!(paid.guard.kind, GuardKind::Temporary(bytes) if bytes == paid.charged)
        {
            return Err(refused());
        }
        let image_highwater = if check_pages {
            let pages: i64 = image.pragma_query_value(None, "page_count", |row| row.get(0))?;
            let page_size: i64 = image.pragma_query_value(None, "page_size", |row| row.get(0))?;
            let pages = u64::try_from(pages).map_err(|_| refused())?;
            let page_size = u64::try_from(page_size).ok().filter(|n| *n > 0).ok_or_else(refused)?;
            let current = pages.checked_mul(page_size).ok_or_else(refused)?;
            paid.image_highwater.max(original_bytes).max(current)
        } else {
            // Row preflights retain SQLite growth only. Preserve the original
            // before/after callback page queries; do not add SQL to row loops.
            if paid.image_handle != Some(image_handle) || paid.image_highwater == 0 {
                return Err(refused());
            }
            paid.image_highwater
        };
        let sqlite_highwater = paid.sqlite_highwater.max(crate::catalog_backup::allocations(image)?);
        let retained = image_highwater.checked_mul(4)
            .and_then(|bytes| bytes.checked_add(sqlite_highwater)).ok_or_else(refused)?;
        let full = retained.checked_add(MIGRATION_SCRATCH).ok_or_else(refused)?;
        if original_bytes == 0 || image_highwater > MAX_COPIED_IMAGE_BYTES || full > LIMIT {
            return Err(Error("native_backup_custody_refused: backup image allocation budget exceeded".into()));
        }
        // Guard growth atomically checks and updates the SAME context. If the
        // later row admission fails, this paid high-water remains retained.
        let growth = retained.checked_sub(paid.charged).ok_or_else(refused)?;
        if growth != 0 {
            paid.guard.grow(source, growth)?;
        } else {
            self.context.check(Some(source), 0)?;
        }
        paid.image_handle = Some(image_handle);
        paid.original_bytes = Some(original_bytes);
        paid.image_highwater = image_highwater;
        paid.sqlite_highwater = sqlite_highwater;
        paid.charged = retained;
        Ok(())
    }
    pub(in crate::migration::native_session) fn accounting<'a>(
        &'a self,
        source: &'a Connection,
        image: &'a Connection,
    ) -> Result<PaidImageSqliteLoan<'a>> {
        // A mint attempt is terminal, including refusal and unwind. The token
        // borrows the live image, and its Drop revokes access without allowing a
        // later connection (including a recycled handle) to obtain another loan.
        if self.started.replace(true) {
            return Err(refused());
        }
        let original = self.paid.try_borrow().map_err(|_| refused())?
            .original_bytes.ok_or_else(refused)?;
        // Fuse the existing final pre-callback page/SQLite checkpoint with mint.
        // A stale earlier pointer/checkpoint alone can never authorize the loan.
        self.observe(source, image, original, true)?;
        self.loaned.set(true);
        Ok(PaidImageSqliteLoan { reservation: self, source, image, _thread: PhantomData })
    }
    #[cfg(test)]
    pub(in crate::migration::native_session) fn charged_for_test(&self) -> u64 {
        self.paid.borrow().charged
    }
}

pub(in crate::migration) struct PaidImageSqliteLoan<'a> {
    reservation: &'a CopiedImageReservation,
    source: &'a Connection,
    image: &'a Connection,
    _thread: PhantomData<Rc<()>>,
}
impl PaidImageSqliteLoan<'_> {
    fn refresh(&self, image: &Connection) -> Result<()> {
        if !self.reservation.loaned.get() || identity(image) != identity(self.image) {
            return Err(refused());
        }
        let original = self.reservation.paid.try_borrow().map_err(|_| refused())?
            .original_bytes.ok_or_else(refused)?;
        self.reservation.observe(self.source, self.image, original, false)
    }
    pub(in crate::migration) fn reserve(&self, db: &Connection, bytes: u64) -> Result<AllocationGuard> {
        self.refresh(db)?;
        // The image is already independently charged by the exact held owner.
        // Reserve ONLY the new row buffer through the original source context.
        self.reservation.context.reserve(self.source, bytes)
    }
    pub(in crate::migration) fn grow(
        &self,
        guard: &mut AllocationGuard,
        db: &Connection,
        bytes: u64,
    ) -> Result<()> {
        if !Arc::ptr_eq(&guard.context.0, &self.reservation.context.0)
            || !matches!(guard.kind, GuardKind::Temporary(_))
        {
            return Err(refused());
        }
        self.refresh(db)?;
        guard.grow(self.source, bytes)
    }
    pub(in crate::migration) fn check(&self, db: &Connection, additional: u64) -> Result<()> {
        self.refresh(db)?;
        self.reservation.context.check(Some(self.source), additional)
    }
}
impl Drop for PaidImageSqliteLoan<'_> {
    fn drop(&mut self) {
        self.reservation.loaned.set(false);
    }
}
fn require_context(context: &AllocationContext, source: &Connection) -> Result<()> {
    if current().is_some_and(|active| !Arc::ptr_eq(&active.0, &context.0))
        || context.0.lock().map_err(|_| refused())?.sqlite_handle != Some(identity(source))
    {
        return Err(refused());
    }
    Ok(())
}
fn identity(db: &Connection) -> usize {
    // SAFETY: inspect only the borrowed live connection's identity. No pointer
    // dereference, call through the pointer, or ownership transfer is performed.
    unsafe { db.handle() as usize }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connections() -> (Connection, Connection, AllocationContext) {
        let source = Connection::open_in_memory().unwrap();
        let image = Connection::open_in_memory().unwrap();
        image.execute_batch("CREATE TABLE image_rows(n INTEGER); INSERT INTO image_rows VALUES(1)").unwrap();
        let context = AllocationContext::default();
        context.bind_source(&source).unwrap();
        (source, image, context)
    }
    fn statement_sql() -> String {
        format!("SELECT {}", (0..256).map(|n| format!("{n} AS column_{n}")).collect::<Vec<_>>().join(","))
    }
    fn fill(context: &AllocationContext, source: &Connection, remaining: u64) -> AllocationGuard {
        let held = context.retained_native_bytes(source).unwrap() + context.external_retained_bytes().unwrap();
        context.reserve(source, LIMIT - held - remaining).unwrap()
    }

    #[test]
    fn only_explicit_exact_image_loan_uses_its_independently_paid_sqlite() {
        let (source, image, context) = connections();
        let unbound = CopiedImageReservation::new(&context, &source).unwrap();
        assert!(unbound.accounting(&source, &image).is_err());
        unbound.checkpoint(&source, &image, 4096).unwrap();
        assert!(unbound.accounting(&source, &image).is_err());
        drop(unbound);
        let paid = CopiedImageReservation::new(&context, &source).unwrap();
        paid.checkpoint(&source, &image, 4096).unwrap();
        let before = context.temporary_bytes_for_test().unwrap();
        let loan = paid.accounting(&source, &image).unwrap();
        let full = fill(&context, &source, 0);
        assert!(context.reserve(&image, 0).is_err(), "generic image preflight must keep full foreign charge");
        let row = loan.reserve(&image, 0).unwrap();
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before + match full.kind { GuardKind::Temporary(bytes) => bytes, _ => unreachable!() });
        assert!(loan.reserve(&image, 1).is_err());
        assert_eq!(context.0.lock().unwrap().sqlite_handle, Some(identity(&source)));
        drop(row);
        drop(full);
        drop(loan);
        assert!(paid.accounting(&source, &image).is_err());
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before);
        drop(paid);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
    }
    #[test]
    fn two_live_images_retain_independent_charges_and_cannot_exchange_tokens() {
        let (source, first_image, context) = connections();
        let second_image = Connection::open_in_memory().unwrap();
        second_image.execute_batch("CREATE TABLE second(value BLOB); INSERT INTO second VALUES(zeroblob(32768))").unwrap();
        let first = CopiedImageReservation::new(&context, &source).unwrap();
        let second = CopiedImageReservation::new(&context, &source).unwrap();
        first.checkpoint(&source, &first_image, 4096).unwrap();
        second.checkpoint(&source, &second_image, 4096).unwrap();
        let first_loan = first.accounting(&source, &first_image).unwrap();
        let second_loan = second.accounting(&source, &second_image).unwrap();
        let first_charge = first.charged_for_test();
        let second_charge = second.charged_for_test();
        assert!(first_charge > 0 && second_charge > 0);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), first_charge + second_charge);
        assert!(first_loan.reserve(&second_image, 0).is_err());
        assert!(second_loan.reserve(&first_image, 0).is_err());
        let first_row = first_loan.reserve(&first_image, 31).unwrap();
        let second_row = second_loan.reserve(&second_image, 47).unwrap();
        assert_eq!(context.temporary_bytes_for_test().unwrap(), first_charge + second_charge + 78);
        drop(first_row);
        drop(first_loan);
        drop(first_image);
        drop(first);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), second_charge + 47);
        second_loan.check(&second_image, 0).unwrap();
        drop(second_row);
        drop(second_loan);
        drop(second_image);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), second_charge);
        drop(second);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
    }
    #[test]
    fn foreign_context_source_image_row_guard_and_nested_loans_refuse() {
        let (source, image, context) = connections();
        let foreign = Connection::open_in_memory().unwrap();
        let other = AllocationContext::default();
        other.bind_source(&source).unwrap();
        assert!(CopiedImageReservation::new(&context, &foreign).is_err());
        let paid = CopiedImageReservation::new(&context, &source).unwrap();
        assert!(paid.checkpoint(&source, &source, 4096).is_err());
        paid.checkpoint(&source, &image, 4096).unwrap();
        assert!(paid.checkpoint(&source, &foreign, 4096).is_err());
        assert!(paid.checkpoint(&foreign, &image, 4096).is_err());
        assert!(paid.checkpoint(&source, &image, 8192).is_err());
        let loan = paid.accounting(&source, &image).unwrap();
        assert!(paid.accounting(&source, &image).is_err());
        assert!(paid.checkpoint(&source, &image, 4096).is_err());
        assert!(loan.reserve(&foreign, 0).is_err());
        assert!(loan.check(&foreign, 0).is_err());
        let mut wrong_guard = other.reserve(&source, 0).unwrap();
        assert!(loan.grow(&mut wrong_guard, &image, 1).is_err());
        {
            let _foreign_scope = other.enter().unwrap();
            assert!(loan.check(&image, 0).is_err());
            assert!(CopiedImageReservation::new(&context, &source).is_err());
        }
        loan.check(&image, 0).unwrap();
        drop(loan);
        assert!(paid.accounting(&source, &image).is_err());
    }
    fn page_bytes(db: &Connection) -> u64 {
        let pages: i64 = db.pragma_query_value(None, "page_count", |row| row.get(0)).unwrap();
        let size: i64 = db.pragma_query_value(None, "page_size", |row| row.get(0)).unwrap();
        u64::try_from(pages).unwrap().checked_mul(u64::try_from(size).unwrap()).unwrap()
    }
    fn cache_used(db: &Connection) -> u64 {
        let mut current = 0;
        let mut highwater = 0;
        // SAFETY: the live borrowed connection stays on this thread; SQLite
        // writes only to the two valid local integer output pointers.
        let rc = unsafe {
            rusqlite::ffi::sqlite3_db_status(
                db.handle(), rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
                &mut current, &mut highwater, 0,
            )
        };
        assert_eq!(rc, rusqlite::ffi::SQLITE_OK);
        u64::try_from(current).unwrap()
    }
    #[test]
    fn mint_rechecks_current_pages_and_cannot_be_repeated_after_drop() {
        let (source, image, context) = connections();
        let paid = CopiedImageReservation::new(&context, &source).unwrap();
        paid.checkpoint(&source, &image, 4096).unwrap();
        let old_pages = paid.paid.borrow().image_highwater;
        image.execute_batch("CREATE TABLE grown(value BLOB); INSERT INTO grown VALUES(zeroblob(262144))").unwrap();
        let current_pages = page_bytes(&image);
        assert!(current_pages > old_pages);
        let loan = paid.accounting(&source, &image).unwrap();
        let held = paid.paid.borrow();
        assert_eq!(held.image_highwater, current_pages);
        assert_eq!(held.charged, 4 * current_pages + held.sqlite_highwater);
        let charged = held.charged;
        drop(held);
        drop(loan);
        assert!(paid.accounting(&source, &image).is_err());
        assert_eq!(context.temporary_bytes_for_test().unwrap(), charged);
    }
    #[test]
    fn growing_cache_is_retained_during_loan_and_pages_at_final_checkpoint() {
        let (source, image, context) = connections();
        let paid = CopiedImageReservation::new(&context, &source).unwrap();
        paid.checkpoint(&source, &image, 4096).unwrap();
        let loan = paid.accounting(&source, &image).unwrap();
        let baseline = paid.charged_for_test();
        let old_pages = paid.paid.borrow().image_highwater;
        let old_sqlite = paid.paid.borrow().sqlite_highwater;
        let old_cache = cache_used(&image);
        image.execute_batch("CREATE TABLE growing_cache(value BLOB); INSERT INTO growing_cache VALUES(zeroblob(524288))").unwrap();
        assert!(cache_used(&image) > old_cache);
        let new_sqlite = crate::catalog_backup::allocations(&image).unwrap();
        assert!(new_sqlite > old_sqlite);
        let row = loan.reserve(&image, 31).unwrap();
        let retained = baseline + new_sqlite - old_sqlite;
        assert_eq!(paid.charged_for_test(), retained);
        assert_eq!(paid.paid.borrow().image_highwater, old_pages);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), retained + 31);
        drop(row);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), retained);
        drop(loan);
        let new_pages = page_bytes(&image);
        assert!(new_pages > old_pages);
        paid.checkpoint(&source, &image, 4096).unwrap();
        assert_eq!(paid.paid.borrow().image_highwater, new_pages);
        assert_eq!(paid.charged_for_test(), retained + 4 * (new_pages - old_pages));
        assert!(paid.accounting(&source, &image).is_err());
    }
    #[test]
    fn observed_statement_growth_stays_charged_before_rows_and_never_shrinks() {
        let (source, image, context) = connections();
        let paid = CopiedImageReservation::new(&context, &source).unwrap();
        paid.checkpoint(&source, &image, 4096).unwrap();
        let loan = paid.accounting(&source, &image).unwrap();
        let baseline = paid.charged_for_test();
        let old_sqlite = paid.paid.borrow().sqlite_highwater;
        let sql = statement_sql();
        let statement = image.prepare(&sql).unwrap();
        let new_sqlite = crate::catalog_backup::allocations(&image).unwrap();
        assert!(new_sqlite > old_sqlite);
        let growth = new_sqlite - old_sqlite;
        let mut row = loan.reserve(&image, 100).unwrap();
        assert_eq!(paid.charged_for_test(), baseline + growth);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), baseline + growth + 100);
        loan.grow(&mut row, &image, 23).unwrap();
        assert_eq!(context.temporary_bytes_for_test().unwrap(), baseline + growth + 123);
        drop(statement);
        loan.check(&image, 0).unwrap();
        assert_eq!(paid.charged_for_test(), baseline + growth);
        drop(row);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), baseline + growth);
        drop(loan);
        paid.checkpoint(&source, &image, 4096).unwrap();
        assert_eq!(paid.charged_for_test(), baseline + growth);
    }
    #[test]
    fn sqlite_growth_survives_independent_row_refusal_and_growth_refusal_is_atomic() {
        let (source, image, context) = connections();
        let paid = CopiedImageReservation::new(&context, &source).unwrap();
        paid.checkpoint(&source, &image, 4096).unwrap();
        let loan = paid.accounting(&source, &image).unwrap();
        let old = paid.charged_for_test();
        let old_sqlite = paid.paid.borrow().sqlite_highwater;
        let sql = statement_sql();
        let statement = image.prepare(&sql).unwrap();
        let growth = crate::catalog_backup::allocations(&image).unwrap() - old_sqlite;
        assert!(growth > 0);
        let full = fill(&context, &source, growth);
        assert!(loan.reserve(&image, 1).is_err());
        assert_eq!(paid.charged_for_test(), old + growth, "image growth remains paid despite later row failure");
        let held = context.temporary_bytes_for_test().unwrap();
        let more_sql = format!("{sql} UNION ALL {sql}");
        let another = image.prepare(&more_sql).unwrap();
        assert!(loan.check(&image, 0).is_err());
        assert_eq!(paid.charged_for_test(), old + growth);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), held);
        drop(another);
        drop(statement);
        drop(full);
        loan.check(&image, 0).unwrap();
        assert_eq!(paid.charged_for_test(), old + growth);
    }
    #[test]
    fn unwind_revokes_token_without_releasing_paid_image_or_external_highwater() {
        let (source, image, context) = connections();
        context.refresh(OfflineAllocationReservation::new(1234).unwrap(), Some(&source)).unwrap();
        let paid = CopiedImageReservation::new(&context, &source).unwrap();
        paid.checkpoint(&source, &image, 4096).unwrap();
        let charged = paid.charged_for_test();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let loan = paid.accounting(&source, &image).unwrap();
            let _row = loan.reserve(&image, 77).unwrap();
            panic!("controlled paid copied-image unwind");
        }));
        assert!(result.is_err());
        assert!(!paid.loaned.get());
        assert_eq!(paid.charged_for_test(), charged);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), charged);
        assert_eq!(context.external_retained_bytes().unwrap(), 1234);
        assert!(paid.accounting(&source, &image).is_err());
        paid.checkpoint(&source, &image, 4096).unwrap();
        assert_eq!(context.temporary_bytes_for_test().unwrap(), charged);
    }
}
