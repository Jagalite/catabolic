//! Bounded, database-only snapshot export from a retained native owner.
//! No restore authority, media inventory, or service-drain proof is conveyed.
pub mod image_read;
pub(crate) mod inventory;

use crate::{
    Error, Result, Store,
    catalog_projection::TrustedProjectionAuthority,
    filesystem_mutation::{identity, open_at, open_root},
};
use rusqlite::{
    Connection,
    backup::{Backup, StepResult},
};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// Conservative export accounting cap, not a process-wide SQLite heap limit.
/// Four image reservations cover source/cache, target, serialization and reread
/// verification; an additional fixed reserve covers schema/statement metadata.
pub const ACCOUNTING_LIMIT: u64 = 256 * 1024 * 1024;
const METADATA_RESERVE: u64 = 32 * 1024 * 1024;
fn fail(message: &str) -> Error {
    Error(message.into())
}
fn charge(image: u64) -> Result<()> {
    charge_reserved(image, 0)
}
fn charge_reserved(image: u64, reserved: u64) -> Result<()> {
    if image == 0
        || image
            .checked_mul(4)
            .and_then(|n| n.checked_add(METADATA_RESERVE))
            .and_then(|n| n.checked_add(reserved))
            .is_none_or(|n| n > ACCOUNTING_LIMIT)
    {
        return Err(fail("native export exceeds bounded-memory cohort"));
    }
    Ok(())
}
// SQLite exposes connection allocation accounting, not a per-connection hard
// heap allocator. Reserve image copies in addition to observed cache/schema/SQL
// allocations, and never alter the process-global SQLite heap limit.
pub(crate) fn allocations(db: &Connection) -> Result<u64> {
    // SAFETY: this borrowed connection remains live on the owning thread.
    unsafe { allocations_handle(db.handle()) }
}
pub(crate) unsafe fn allocations_handle(handle: *mut rusqlite::ffi::sqlite3) -> Result<u64> {
    // SAFETY: forwarded unchanged from this function's live-handle contract.
    unsafe {
        allocation_status(
            handle,
            &[
                rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
                rusqlite::ffi::SQLITE_DBSTATUS_SCHEMA_USED,
                rusqlite::ffi::SQLITE_DBSTATUS_STMT_USED,
            ],
        )
    }
}
// Components are descriptive measurements, not permission to reuse them.
// Only the closed identity-ledger cursor proves schema stability for reuse.
pub(crate) fn schema_allocations(db: &Connection) -> Result<u64> {
    // SAFETY: the exclusively borrowed connection is live on its owning thread.
    unsafe { allocation_status(db.handle(), &[rusqlite::ffi::SQLITE_DBSTATUS_SCHEMA_USED]) }
}
pub(crate) fn dynamic_allocations(db: &Connection) -> Result<u64> {
    // SAFETY: the exclusively borrowed connection is live on its owning thread.
    unsafe {
        allocation_status(
            db.handle(),
            &[
                rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
                rusqlite::ffi::SQLITE_DBSTATUS_STMT_USED,
            ],
        )
    }
}
unsafe fn allocation_status(
    handle: *mut rusqlite::ffi::sqlite3,
    operations: &[i32],
) -> Result<u64> {
    let mut total = 0u64;
    for &op in operations {
        let mut current = 0;
        let mut high = 0;
        // SAFETY: the connection is live on its owning thread for this call.
        let result =
            unsafe { rusqlite::ffi::sqlite3_db_status(handle, op, &mut current, &mut high, 0) };
        if result != rusqlite::ffi::SQLITE_OK || current < 0 {
            return Err(fail("native export allocation accounting unavailable"));
        }
        total = total
            .checked_add(current as u64)
            .ok_or_else(|| fail("native export allocation overflow"))?;
    }
    Ok(total)
}

fn account(source: &Connection, target: &Connection, image: u64, reserved: u64) -> Result<()> {
    charge_reserved(image, reserved)?;
    let total = image
        .checked_mul(3)
        .and_then(|n| n.checked_add(METADATA_RESERVE))
        .and_then(|n| n.checked_add(reserved))
        .and_then(|n| n.checked_add(allocations(source).ok()?))
        .and_then(|n| n.checked_add(allocations(target).ok()?));
    if total.is_none_or(|n| n > ACCOUNTING_LIMIT) {
        return Err(fail("native export live allocation budget exhausted"));
    }
    Ok(())
}
fn account_verifier(
    source: &Connection,
    target: &Connection,
    verifier: &Connection,
    bytes: u64,
    reserved: u64,
) -> Result<()> {
    let cumulative = bytes
        .checked_mul(4)
        .and_then(|n| n.checked_add(METADATA_RESERVE))
        .and_then(|n| n.checked_add(reserved))
        .and_then(|n| n.checked_add(allocations(source).ok()?))
        .and_then(|n| n.checked_add(allocations(target).ok()?))
        .and_then(|n| n.checked_add(allocations(verifier).ok()?));
    if cumulative.is_none_or(|n| n > ACCOUNTING_LIMIT) {
        return Err(fail("native export verifier allocation budget exhausted"));
    }
    Ok(())
}
// SQLite INTEGER is signed. Refuse negative values rather than wrapping size,
// count or filesystem identity into a larger unsigned budget/identity.
fn nonnegative(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(column)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
}
fn optional_nonnegative(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(column)?
        .map(|value| {
            u64::try_from(value)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
        })
        .transpose()
}
fn image_size(db: &Connection) -> Result<u64> {
    let pages: u64 = db.pragma_query_value(None, "page_count", |r| nonnegative(r, 0))?;
    let size: u64 = db.pragma_query_value(None, "page_size", |r| nonnegative(r, 0))?;
    pages
        .checked_mul(size)
        .ok_or_else(|| fail("native export size overflow"))
}
fn check(store: &Store, expected: &TrustedProjectionAuthority) -> Result<()> {
    store.transaction_available()?;
    if !store.writable || !store.owns_writer_lock() || store.catalog_incarnation.is_none() {
        return Err(fail(
            "native export requires retained admitted writer owner",
        ));
    }
    let actual: (String, String, i64) = store.db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if actual
        != (
            expected.catalog_id.clone(),
            expected.incarnation.clone(),
            expected.expected_generation,
        )
        || actual.0 != store.database_id
        || Some(&actual.1) != store.catalog_incarnation.as_ref()
    {
        return Err(fail("native export identity or generation changed"));
    }
    Ok(())
}
fn live(cancelled: &AtomicBool, deadline: Instant) -> Result<()> {
    if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
        return Err(fail("native export cancelled or deadline exceeded"));
    }
    Ok(())
}
struct Snapshot<'a>(&'a Store);
impl Drop for Snapshot<'_> {
    fn drop(&mut self) {
        let _ = self.0.rollback_transaction(None);
    }
}

/// Opaque owner of an anonymous database-only artifact. Dropping closes the
/// descriptor; the kernel reclaims its unnamed inode without pathname cleanup.
/// This is NOT a durable backup. A pair publisher must copy and fsync both
/// verified databases before atomically publishing its own paired manifest.
pub struct VerifiedExport {
    parent: File,
    file: File,
    parent_path: PathBuf,
    catalog_id: String,
    incarnation: String,
    generation: i64,
    schema: u32,
    bytes: u64,
    sha256: String,
    audit: inventory::Audit,
    retained_allocations: u64,
}
impl VerifiedExport {
    /// Measured source connection allocations retained after temporary copies drop.
    /// Valid only while the caller keeps the same native owner idle and retained.
    pub fn retained_allocations(&self) -> u64 {
        self.retained_allocations
    }
    /// Descriptor identity, not a pathname grant. A companion must independently
    /// open its parent and compare the actual held descriptor before creation.
    pub fn parent_identity(&self) -> Result<(u64, u64)> {
        self.check_paths()?;
        Ok(identity(&self.parent.metadata()?))
    }
    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
    pub fn schema_version(&self) -> u32 {
        self.schema
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    /// Complete schema/typed-row inventory, including all receipt/ownership and
    /// migration tables. This is evidence only, never restore authority.
    pub fn inventory_sha256(&self) -> &str {
        &self.audit.complete
    }
    pub fn schema_sha256(&self) -> &str {
        &self.audit.schema
    }
    pub fn migration_ledger_sha256(&self) -> &str {
        &self.audit.ledger
    }
    /// Copy into the pair publisher's unpublished stage. The publisher MUST
    /// discard that stage on any error, and owns its fsync/publication protocol.
    /// No raw or writable descriptor is exposed. Exclusive borrow prevents
    /// concurrent shared-offset readers, and validation covers the actual copy.
    pub fn copy_verified_to(
        &mut self,
        destination: &mut impl Write,
        cancelled: &AtomicBool,
    ) -> Result<()> {
        self.check_paths()?;
        let deadline = Instant::now() + Duration::from_secs(60);
        self.file.seek(SeekFrom::Start(0))?;
        let mut digest = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            live(cancelled, deadline)?;
            let n = self.file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes = bytes
                .checked_add(n as u64)
                .ok_or_else(|| fail("native export size overflow"))?;
            if bytes > self.bytes {
                return Err(fail("native export artifact grew"));
            }
            digest.update(&buffer[..n]);
            destination.write_all(&buffer[..n])?;
        }
        if bytes != self.bytes || format!("{:x}", digest.finalize()) != self.sha256 {
            return Err(fail("native export artifact changed"));
        }
        self.check_paths()?;
        live(cancelled, deadline)
    }
    fn check_paths(&self) -> Result<()> {
        let current = open_root(
            self.parent_path
                .to_str()
                .ok_or_else(|| fail("invalid export parent"))?,
        )?;
        if identity(&current.metadata()?) != identity(&self.parent.metadata()?)
            || self.file.metadata()?.nlink() != 0
            || !self.file.metadata()?.is_file()
        {
            return Err(fail(
                "native export destination replaced or artifact linked",
            ));
        }
        Ok(())
    }
}
fn hash_file(file: &mut File) -> Result<(u64, String)> {
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes = bytes
            .checked_add(n as u64)
            .ok_or_else(|| fail("export size overflow"))?;
        charge(bytes)?;
        digest.update(&buffer[..n]);
    }
    Ok((bytes, format!("{:x}", digest.finalize())))
}

/// Copies a closed, admitted native database while retaining its writer/lifetime
/// owner and one read snapshot. Ambient effect transactions are rejected.
/// Caller independently owns service drain and Motion writer custody.
pub fn export(
    store: &Store,
    expected: &TrustedProjectionAuthority,
    parent_path: &Path,
    cancelled: &AtomicBool,
) -> Result<VerifiedExport> {
    export_reserved(store, expected, parent_path, cancelled, 0)
}
/// Additional retained allocation reservation only tightens the fixed cohort.
/// This number conveys no ownership or drain authority.
pub fn export_reserved(
    store: &Store,
    expected: &TrustedProjectionAuthority,
    parent_path: &Path,
    cancelled: &AtomicBool,
    reserved: u64,
) -> Result<VerifiedExport> {
    export_inner_reserved(store, expected, parent_path, cancelled, reserved, |_, _| {
        Ok(())
    })
}
#[cfg(test)]
fn export_inner(
    store: &Store,
    expected: &TrustedProjectionAuthority,
    parent_path: &Path,
    cancelled: &AtomicBool,
    mut before_write: impl FnMut(&Path, usize) -> Result<()>,
) -> Result<VerifiedExport> {
    export_inner_reserved(store, expected, parent_path, cancelled, 0, before_write)
}
fn export_inner_reserved(
    store: &Store,
    expected: &TrustedProjectionAuthority,
    parent_path: &Path,
    cancelled: &AtomicBool,
    reserved: u64,
    mut before_write: impl FnMut(&Path, usize) -> Result<()>,
) -> Result<VerifiedExport> {
    let deadline = Instant::now() + Duration::from_secs(60);
    live(cancelled, deadline)?;
    check(store, expected)?;
    if !store.db.is_autocommit() {
        return Err(fail("native export refuses ambient transaction"));
    }
    // Open every component no-follow before any allocation or artifact creation.
    let parent_path = std::path::absolute(parent_path)?;
    let parent = open_root(
        parent_path
            .to_str()
            .ok_or_else(|| fail("invalid export parent"))?,
    )?;
    let parent_path = parent_path.canonicalize()?;
    if identity(
        &open_root(
            parent_path
                .to_str()
                .ok_or_else(|| fail("invalid export parent"))?,
        )?
        .metadata()?,
    ) != identity(&parent.metadata()?)
    {
        return Err(fail("native export parent changed"));
    }
    if store.path.starts_with(&parent_path) {
        return Err(fail("export parent contains active catalog"));
    }
    let (count, bytes): (u64, u64) = store.db.query_row(
        "SELECT count(*),coalesce(sum(length(CAST(root AS BLOB))),0) FROM bindings",
        [],
        |r| Ok((nonnegative(r, 0)?, nonnegative(r, 1)?)),
    )?;
    if count > 10000 || bytes > 4 * 1024 * 1024 {
        return Err(fail("export root inventory exceeds bound"));
    }
    let mut statement = store.db.prepare("SELECT root,device,inode FROM bindings")?;
    let bindings = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                optional_nonnegative(r, 1)?,
                optional_nonnegative(r, 2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (root, _, _) in &bindings {
        let root = Path::new(root);
        if parent_path.starts_with(root) || root.starts_with(&parent_path) {
            return Err(fail("export parent overlaps source or output"));
        }
    }
    let mut ancestor = parent.try_clone()?;
    loop {
        let id = identity(&ancestor.metadata()?);
        if bindings
            .iter()
            .any(|(_, dev, ino)| *dev == Some(id.0) && *ino == Some(id.1))
        {
            return Err(fail("export parent aliases source or output"));
        }
        let next = open_at(&ancestor, "..", true)?;
        if identity(&next.metadata()?) == id {
            break;
        }
        ancestor = next;
    }
    store.db.execute_batch("BEGIN")?;
    let snapshot = Snapshot(store);
    check(store, expected)?;
    let bytes = image_size(&store.db)?;
    charge_reserved(bytes, reserved)?;
    let audit = inventory::capture(&store.db, cancelled, deadline)?;
    let mut target = Connection::open_in_memory()?;
    target.pragma_update(None, "foreign_keys", true)?;
    target.pragma_update(None, "recursive_triggers", true)?;
    target.pragma_update(None, "cache_size", -2048)?;
    {
        account(&store.db, &target, bytes, reserved)?;
        // SAFETY: target lives until after Backup is dropped; all calls are synchronous.
        let target_handle = unsafe { target.handle() };
        let backup = Backup::new(&store.db, &mut target)?;
        loop {
            live(cancelled, deadline)?;
            charge_reserved(image_size(&store.db)?, reserved)?;
            // SAFETY: backup.step has returned; target remains alive and is not
            // concurrently accessed. Only allocation counters are inspected.
            let observed = unsafe { allocations_handle(target_handle)? };
            let used = bytes
                .checked_mul(3)
                .and_then(|n| n.checked_add(METADATA_RESERVE))
                .and_then(|n| n.checked_add(reserved))
                .and_then(|n| n.checked_add(observed))
                .and_then(|n| n.checked_add(allocations(&store.db).ok()?));
            if used.is_none_or(|n| n > ACCOUNTING_LIMIT) {
                return Err(fail("native export live allocation budget exhausted"));
            }
            match backup.step(128)? {
                StepResult::Done => break,
                StepResult::More => {}
                _ => return Err(fail("native export backup busy")),
            }
        }
    }
    live(cancelled, deadline)?;
    account(&store.db, &target, bytes, reserved)?;
    if image_size(&target)? != bytes {
        return Err(fail("native export image size changed"));
    }
    target.progress_handler(10000, Some(move || Instant::now() >= deadline))?;
    let verified = crate::migration::validate(&target, true)?;
    if verified["database_id"].as_str() != Some(store.database_id.as_str())
        || verified["schema"].as_u64() != Some(store.schema_version as u64)
    {
        return Err(fail("native export copied schema or identity mismatch"));
    }
    let copied: (String, String, i64) = target.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if copied
        != (
            expected.catalog_id.clone(),
            expected.incarnation.clone(),
            expected.expected_generation,
        )
    {
        return Err(fail("native export copied lifetime mismatch"));
    }
    account(&store.db, &target, bytes, reserved)?;
    if inventory::capture(&target, cancelled, deadline)? != audit {
        return Err(fail("native export backup logical inventory mismatch"));
    }
    account(&store.db, &target, bytes, reserved)?;
    let data = target.serialize("main")?;
    charge_reserved(data.len() as u64, reserved)?;
    account(&store.db, &target, data.len() as u64, reserved)?;
    if data.len() as u64 != bytes {
        return Err(fail("native export serialized length mismatch"));
    }
    // Verify the actual serialized image, not only the in-memory source of it.
    // This also refuses a WAL-dependent image that is not independently usable.
    let mut image = Connection::open_in_memory()?;
    image.pragma_update(None, "cache_size", -2048)?;
    account_verifier(&store.db, &target, &image, bytes, reserved)?;
    image.deserialize_read_exact("main", std::io::Cursor::new(&*data), data.len(), true)?;
    image.pragma_update(None, "foreign_keys", true)?;
    image.pragma_update(None, "recursive_triggers", true)?;
    image.progress_handler(10000, Some(move || Instant::now() >= deadline))?;
    account_verifier(&store.db, &target, &image, bytes, reserved)?;
    crate::migration::validate(&image, true)?;
    account_verifier(&store.db, &target, &image, bytes, reserved)?;
    if inventory::capture(&image, cancelled, deadline)? != audit
        || inventory::capture(&store.db, cancelled, deadline)? != audit
    {
        return Err(fail("native export serialized logical inventory mismatch"));
    }
    account_verifier(&store.db, &target, &image, bytes, reserved)?;
    drop(image);
    check(store, expected)?;
    live(cancelled, deadline)?;
    before_write(&parent_path, 0)?;
    if identity(&open_root(parent_path.to_str().unwrap())?.metadata()?)
        != identity(&parent.metadata()?)
    {
        return Err(fail("native export parent replaced"));
    }
    // Linux/filesystem-specific bounded cohort. O_TMPFILE|O_EXCL creates a
    // unique anonymous inode which cannot later be linked. There is deliberately
    // no named-file fallback, directory adoption, or check-then-unlink cleanup.
    #[cfg(not(target_os = "linux"))]
    return Err(fail(
        "native anonymous export requires Linux O_TMPFILE support",
    ));
    #[cfg(target_os = "linux")]
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            c".".as_ptr(),
            libc::O_TMPFILE | libc::O_EXCL | libc::O_RDWR | libc::O_CLOEXEC,
            0o600,
        )
    };
    #[cfg(target_os = "linux")]
    if fd < 0 {
        return Err(fail(&format!(
            "native anonymous export unsupported or unavailable: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: successful openat returned a fresh descriptor owned only here.
    #[cfg(target_os = "linux")]
    let file = unsafe { File::from_raw_fd(fd) };
    #[cfg(not(target_os = "linux"))]
    let file: File = unreachable!();
    let mut receipt = VerifiedExport {
        parent,
        file,
        parent_path,
        catalog_id: expected.catalog_id.clone(),
        incarnation: expected.incarnation.clone(),
        generation: expected.expected_generation,
        schema: store.schema_version,
        bytes,
        sha256: format!("{:x}", Sha256::digest(&*data)),
        audit,
        retained_allocations: 0,
    };
    for (index, chunk) in data.chunks(65536).enumerate() {
        live(cancelled, deadline)?;
        receipt.file.write_all(chunk)?;
        before_write(&receipt.parent_path, index + 1)?;
    }
    receipt.file.sync_all()?;
    receipt.parent.sync_all()?;
    receipt.check_paths()?;
    receipt.file.seek(SeekFrom::Start(0))?;
    let (actual_bytes, actual_digest) = hash_file(&mut receipt.file)?;
    if actual_bytes != receipt.bytes || actual_digest != receipt.sha256 {
        return Err(fail("native anonymous export copy verification failed"));
    }
    check(store, expected)?;
    live(cancelled, deadline)?;
    store.db.execute_batch("ROLLBACK")?;
    std::mem::forget(snapshot);
    receipt.retained_allocations = allocations(&store.db)?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        tempfile::TempDir,
        Store,
        TrustedProjectionAuthority,
        PathBuf,
    ) {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("catalog.sqlite3");
        crate::migration::initialize_native_at(&db, 40).unwrap();
        let store = Store::open(&db, true, false).unwrap();
        let generation = store
            .db
            .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
            .unwrap();
        let authority = TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store.catalog_incarnation.clone().unwrap(),
            expected_generation: generation,
        };
        let destination = root.path().join("exports");
        std::fs::create_dir(&destination).unwrap();
        (root, store, authority, destination)
    }
    #[test]
    fn verified_copy_is_usable_and_drop_leaves_no_named_artifact() {
        let (root, store, authority, destination) = fixture();
        let mut receipt =
            export(&store, &authority, &destination, &AtomicBool::new(false)).unwrap();
        assert_eq!(receipt.generation(), authority.expected_generation);
        assert_eq!(receipt.inventory_sha256().len(), 64);
        assert_eq!(receipt.schema_sha256().len(), 64);
        assert_eq!(receipt.migration_ledger_sha256().len(), 64);
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        let path = root.path().join("test-copy.sqlite3");
        let mut copy = File::create(&path).unwrap();
        receipt
            .copy_verified_to(&mut copy, &AtomicBool::new(false))
            .unwrap();
        copy.sync_all().unwrap();
        drop(copy);
        let inspected = image_read::inspect(
            File::open(&path).unwrap(),
            receipt.bytes(),
            0,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(inspected.catalog_id(), receipt.catalog_id());
        assert_eq!(inspected.worker_history().unwrap().owner_count(), 0);
        assert_eq!(
            inspected.worker_history().unwrap().origin_incarnation(),
            receipt.incarnation()
        );
        assert_eq!(inspected.incarnation(), receipt.incarnation());
        assert_eq!(inspected.generation(), receipt.generation());
        assert_eq!(inspected.sha256(), receipt.sha256());
        assert_eq!(inspected.inventory_sha256(), receipt.inventory_sha256());
        assert_eq!(
            inspected.migration_ledger_sha256(),
            receipt.migration_ledger_sha256()
        );
        assert!(
            image_read::inspect(
                File::open(&path).unwrap(),
                receipt.bytes(),
                u64::MAX,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert!(
            image_read::inspect(
                File::open(&path).unwrap(),
                receipt.bytes(),
                0,
                &AtomicBool::new(true)
            )
            .is_err()
        );
        let db = crate::connect(&path, false).unwrap();
        crate::migration::validate(&db, true).unwrap();
        let actual = inventory::capture(
            &db,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(actual.complete, receipt.inventory_sha256());
        assert_eq!(actual.ledger, receipt.migration_ledger_sha256());
        assert_eq!(actual.schema, receipt.schema_sha256());
        assert_eq!(
            db.query_row("SELECT incarnation FROM catalog_state", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            authority.incarnation
        );
        assert!(store.owns_writer_lock());
        assert!(store.db.is_autocommit());
        drop(receipt);
        assert_eq!(std::fs::read_dir(destination).unwrap().count(), 0);
    }
    #[test]
    fn symlink_overlap_ambient_transaction_and_wrong_generation_refuse() {
        let (root, store, mut authority, destination) = fixture();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&destination, &alias).unwrap();
        let cancel = AtomicBool::new(false);
        assert!(export(&store, &authority, &alias, &cancel).is_err());
        assert!(export(&store, &authority, root.path(), &cancel).is_err());
        store.db.execute_batch("BEGIN").unwrap();
        assert!(export(&store, &authority, &destination, &cancel).is_err());
        assert!(!store.db.is_autocommit());
        store.db.execute_batch("ROLLBACK").unwrap();
        authority.expected_generation += 1;
        assert!(export(&store, &authority, &destination, &cancel).is_err());
        assert_eq!(std::fs::read_dir(destination).unwrap().count(), 0);
    }
    #[test]
    fn parent_replacement_and_cancel_fail_before_artifact_publication() {
        let (root, store, authority, destination) = fixture();
        let moved = root.path().join("moved");
        assert!(
            export_inner(
                &store,
                &authority,
                &destination,
                &AtomicBool::new(false),
                |path, phase| {
                    if phase != 0 {
                        return Ok(());
                    }
                    std::fs::rename(path, &moved)?;
                    std::fs::create_dir(path)?;
                    Ok(())
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(moved).unwrap().count(), 0);
        assert!(store.db.is_autocommit());
        assert!(export(&store, &authority, &destination, &AtomicBool::new(true)).is_err());
    }
    #[test]
    fn parent_replacement_never_removes_unowned_names() {
        let (root, store, authority, destination) = fixture();
        let mut receipt =
            export(&store, &authority, &destination, &AtomicBool::new(false)).unwrap();
        std::fs::rename(&destination, root.path().join("old-parent")).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("unrelated"), b"keep").unwrap();
        assert!(
            receipt
                .copy_verified_to(&mut Vec::new(), &AtomicBool::new(false))
                .is_err()
        );
        drop(receipt);
        assert_eq!(
            std::fs::read(destination.join("unrelated")).unwrap(),
            b"keep"
        );
    }
    #[test]
    fn cancellation_after_copy_cleans_staging_and_releases_read_snapshot() {
        let (_root, store, authority, destination) = fixture();
        let cancelled = AtomicBool::new(false);
        assert!(
            export_inner(&store, &authority, &destination, &cancelled, |_, _| {
                cancelled.store(true, Ordering::Release);
                Ok(())
            })
            .is_err()
        );
        assert_eq!(std::fs::read_dir(destination).unwrap().count(), 0);
        assert!(store.db.is_autocommit());
        assert!(store.owns_writer_lock());
    }
    #[test]
    fn cancellation_after_first_written_chunk_cleans_owned_artifact() {
        let (_root, store, authority, destination) = fixture();
        let cancelled = AtomicBool::new(false);
        assert!(
            export_inner(&store, &authority, &destination, &cancelled, |_, phase| {
                if phase > 0 {
                    cancelled.store(true, Ordering::Release);
                }
                Ok(())
            })
            .is_err()
        );
        assert_eq!(std::fs::read_dir(destination).unwrap().count(), 0);
        assert!(store.db.is_autocommit());
    }
    #[test]
    fn same_inode_corruption_refuses_descriptor_disclosure() {
        let (_root, store, authority, destination) = fixture();
        let mut receipt =
            export(&store, &authority, &destination, &AtomicBool::new(false)).unwrap();
        let mut file = receipt.file.try_clone().unwrap();
        file.seek(SeekFrom::Start(100)).unwrap();
        file.write_all(b"corrupt").unwrap();
        file.sync_all().unwrap();
        assert!(
            receipt
                .copy_verified_to(&mut Vec::new(), &AtomicBool::new(false))
                .is_err()
        );
        drop(receipt);
        assert_eq!(std::fs::read_dir(destination).unwrap().count(), 0);
    }
    #[test]
    fn source_mutation_during_export_never_returns_a_mixed_snapshot() {
        let (_root, store, authority, destination) = fixture();
        assert!(export_inner(&store,&authority,&destination,&AtomicBool::new(false), |_, phase| {
            if phase==0 {
                crate::curation::item_put(&store,&serde_json::json!({"id":"during-export","kind":"video","metadata":{"title":"not in captured image"}}))?;
            }
            Ok(())
        }).is_err());
        assert!(store.db.is_autocommit());
        assert_eq!(
            store
                .db
                .query_row(
                    "SELECT count(*) FROM items WHERE id='during-export'",
                    [],
                    |r| nonnegative(r, 0)
                )
                .unwrap(),
            0
        );
        assert_eq!(std::fs::read_dir(destination).unwrap().count(), 0);
        check(&store, &authority).unwrap();
    }
    #[test]
    fn signed_integer_decoders_refuse_negative_storage_without_wrapping() {
        let db = Connection::open_in_memory().unwrap();
        assert_eq!(
            db.query_row("SELECT 0", [], |r| nonnegative(r, 0)).unwrap(),
            0
        );
        assert!(
            db.query_row("SELECT -1", [], |r| nonnegative(r, 0))
                .is_err()
        );
        assert_eq!(
            db.query_row("SELECT NULL", [], |r| optional_nonnegative(r, 0))
                .unwrap(),
            None
        );
        assert!(
            db.query_row("SELECT -1", [], |r| optional_nonnegative(r, 0))
                .is_err()
        );
        assert!(
            db.query_row("SELECT '1'", [], |r| nonnegative(r, 0))
                .is_err()
        );
    }
    #[test]
    fn retained_pair_reservation_refuses_before_artifact_and_does_not_change_owner() {
        let (_root, store, authority, parent) = fixture();
        assert!(
            export_reserved(
                &store,
                &authority,
                &parent,
                &AtomicBool::new(false),
                ACCOUNTING_LIMIT
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
        assert!(store.db.is_autocommit());
        check(&store, &authority).unwrap();
        let receipt = export_reserved(
            &store,
            &authority,
            &parent,
            &AtomicBool::new(false),
            1024 * 1024,
        )
        .unwrap();
        assert_eq!(
            receipt.parent_identity().unwrap(),
            identity(&std::fs::metadata(&parent).unwrap())
        );
        assert!(receipt.retained_allocations() > 0);
        drop(receipt);
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
        assert!(charge_reserved(4096, u64::MAX).is_err());
    }
    #[test]
    fn export_accounting_refuses_growth_and_overflow() {
        assert!(charge(4096).is_ok());
        assert!(charge((ACCOUNTING_LIMIT - METADATA_RESERVE) / 4 + 1).is_err());
        assert!(charge(u64::MAX).is_err());
    }
}
