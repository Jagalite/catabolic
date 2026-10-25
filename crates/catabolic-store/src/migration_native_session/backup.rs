//! Retained custody of the actual verified source40 backup image. A receipt or
//! pathname alone is not proof. This owner neither migrates nor restores bytes.
use super::allocation::{AllocationContext, ApplyMetadataLoan, CopiedImageReservation, PaidImageSqliteLoan};
#[cfg(test)]
use super::allocation::AllocationGuard;
use crate::{Error, Result, migration};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata, OpenOptions},
    io::{self, Read},
    os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const BUFFER_BYTES: usize = 64 * 1024;
const MAX_IMAGE_BYTES: u64 = super::allocation::MAX_COPIED_IMAGE_BYTES;
const ACCOUNTING_LIMIT: u64 = 256 * 1024 * 1024;
const METADATA_RESERVE: u64 = 32 * 1024 * 1024;
const _: () = assert!(METADATA_RESERVE == super::allocation::MIGRATION_SCRATCH);
fn fail(message: impl std::fmt::Display) -> Error {
    Error(format!("native_backup_custody_refused: {message}"))
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    device: u64,
    inode: u64,
    size: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
    nlink: u64,
    mode: u32,
}
impl Fingerprint {
    fn read(metadata: Metadata) -> Result<Self> {
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() == 0 {
            return Err(fail("backup is not a nonempty unique regular file"));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            mtime: metadata.mtime(),
            mtime_ns: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_ns: metadata.ctime_nsec(),
            nlink: metadata.nlink(),
            mode: metadata.mode(),
        })
    }
}

/// No Clone/Deserialize and no File/Connection escape. The migration owner
/// retains this value until its final pre-COMMIT revalidation has succeeded.
pub(super) struct VerifiedBackup {
    file: File,
    path: PathBuf,
    fingerprint: Fingerprint,
    sha256: String,
}
impl VerifiedBackup {
    /// Descriptions come from the parent's actual SourcePreservation capture.
    /// All logical facts are independently read from this exact retained image;
    /// these arguments cannot mint migration, completion or restore authority.
    pub(super) fn open(
        path: &Path,
        expected_catalog: &str,
        expected_incarnation: &str,
        expected_full_ledger: &Value,
        expected_tables: &Value,
        expected_backup_sha256: &str,
    ) -> Result<Self> {
        Self::open_mode(
            None,
            None,
            path,
            expected_catalog,
            expected_incarnation,
            expected_full_ledger,
            expected_tables,
            expected_backup_sha256,
        )
    }
    pub(super) fn open_reserved(
        source: &Connection,
        allocation: &AllocationContext,
        path: &Path,
        expected_catalog: &str,
        expected_incarnation: &str,
        expected_full_ledger: &Value,
        expected_tables: &Value,
        expected_backup_sha256: &str,
    ) -> Result<Self> {
        Self::open_mode(
            Some((source, allocation)),
            None,
            path,
            expected_catalog,
            expected_incarnation,
            expected_full_ledger,
            expected_tables,
            expected_backup_sha256,
        )
    }
    pub(super) fn open_with_workspace(
        workspace: &mut ApplyMetadataLoan<'_, '_>,
        path: &Path,
        expected_catalog: &str,
        expected_incarnation: &str,
        expected_full_ledger: &Value,
        expected_tables: &Value,
        expected_backup_sha256: &str,
    ) -> Result<Self> {
        Self::open_mode(
            None,
            Some(workspace),
            path,
            expected_catalog,
            expected_incarnation,
            expected_full_ledger,
            expected_tables,
            expected_backup_sha256,
        )
    }
    fn open_mode(
        allocation: Option<(&Connection, &AllocationContext)>,
        workspace: Option<&mut ApplyMetadataLoan<'_, '_>>,
        path: &Path,
        expected_catalog: &str,
        expected_incarnation: &str,
        expected_full_ledger: &Value,
        expected_tables: &Value,
        expected_backup_sha256: &str,
    ) -> Result<Self> {
        if !(1..=256).contains(&expected_catalog.len())
            || expected_incarnation.len() != 32
            || !expected_incarnation
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || expected_backup_sha256.len() != 64
            || !expected_backup_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !expected_full_ledger.is_array()
            || !expected_tables.is_object()
        {
            return Err(fail("invalid expected source/backup description"));
        }
        // In the workspace path, construction/canonicalization still uses the
        // paid metadata phase while no copied-image scan exists. The actual
        // pinned path/SHA capacities are admitted into the separate copy share
        // by require_backup BEFORE image loading, not before these allocations.
        // Input path length alone is not a bound on canonicalization's peak.
        let path = crate::database_path(path)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        let fingerprint = Fingerprint::read(file.metadata()?)?;
        let owner = Self {
            file,
            path,
            fingerprint,
            sha256: expected_backup_sha256.into(),
        };
        let reserved = allocation.is_some() || workspace.is_some();
        let verify = |db: &Connection, image: Option<&PaidImageSqliteLoan<'_>>| {
            if reserved {
                super::allocation::catalog_preflight(db)?;
            }
            db.execute_batch("BEGIN")?;
            let info = migration::validate(&db, true)?;
            if info["schema"] != 40 {
                return Err(fail("backup is not exact closed40"));
            }
            let (catalog, incarnation): (String, String) = db.query_row(
                "SELECT catalog_id,incarnation FROM catalog_state WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if catalog != expected_catalog || incarnation != expected_incarnation {
                return Err(fail("backup source identity differs"));
            }
            let ledger = json!(migration::rows_with_image(
                &db,
                "SELECT version,filename,checksum,origin,applied_at FROM schema_migrations ORDER BY version",
                &[],
                image,
            )?);
            if &ledger != expected_full_ledger || &migration::snapshot_with_image(&db, image)? != expected_tables {
                return Err(fail(
                    "backup full ledger or preserved table snapshot differs",
                ));
            }
            db.execute_batch("ROLLBACK")?;
            Ok(())
        };
        if let Some(workspace) = workspace {
            owner.with_verified_image_workspace(false, workspace, |db, image| verify(db, Some(image)))?;
        } else {
            owner.with_verified_image_mode(false, allocation, |db| verify(db, None))?;
        }
        Ok(owner)
    }
    /// Private isolated image only. SQLite consumes the SAME retained descriptor
    /// whose exact bytes are hashed, never a second pathname open. The callback
    /// borrows an image with no lifecycle/Store/production schema authority.
    pub(super) fn with_verified_image<T>(
        &self,
        writable: bool,
        use_image: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        self.with_verified_image_mode(writable, None, use_image)
    }
    pub(super) fn with_verified_image_reserved<T>(
        &self,
        writable: bool,
        source: &Connection,
        allocation: &AllocationContext,
        use_image: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        self.with_verified_image_mode(writable, Some((source, allocation)), use_image)
    }
    /// Fixed apply-only callback. No owned callback result can carry metadata
    /// out of the loan; callers are the closed verification/rehearsal routines.
    /// The callback receives only its explicitly borrowed copied-image accounting
    /// token, not the metadata workspace loan or original source connection.
    pub(super) fn with_verified_image_workspace(
        &self,
        writable: bool,
        workspace: &mut ApplyMetadataLoan<'_, '_>,
        use_image: impl FnOnce(&Connection, &PaidImageSqliteLoan<'_>) -> Result<()>,
    ) -> Result<()> {
        self.with_verified_image_inner(writable, None, Some(workspace), |db, image| {
            use_image(db, image.ok_or_else(|| fail("paid copied image loan missing"))?)
        })
    }
    fn with_verified_image_mode<T>(
        &self,
        writable: bool,
        allocation: Option<(&Connection, &AllocationContext)>,
        use_image: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        self.with_verified_image_inner(writable, allocation, None, |db, _| use_image(db))
    }
    fn with_verified_image_inner<T>(
        &self,
        writable: bool,
        allocation: Option<(&Connection, &AllocationContext)>,
        workspace: Option<&mut ApplyMetadataLoan<'_, '_>>,
        use_image: impl FnOnce(&Connection, Option<&PaidImageSqliteLoan<'_>>) -> Result<T>,
    ) -> Result<T> {
        self.check_identity()?;
        if self.fingerprint.size > MAX_IMAGE_BYTES {
            return Err(fail("backup image exceeds explicit 56MiB cohort"));
        }
        let mut header = [0_u8; 20];
        self.file.read_exact_at(&mut header, 0)?;
        if &header[..16] != b"SQLite format 3\0" || header[18] != 1 || header[19] != 1 {
            return Err(fail("backup image is not rollback-journal SQLite format"));
        }
        self.check_identity()?;
        // Declare the reservation BEFORE SQLite: reverse local drop order
        // closes the actual copied connection before releasing its charge on
        // success, error, or unwind. The metadata guard stays charged throughout.
        let mut image_reservation = workspace
            .map(|workspace| WorkspaceImageReservation::new(workspace, self))
            .transpose()?;
        let mut db = Connection::open_in_memory()?;
        db.pragma_update(None, "cache_size", -2048)?;
        image_accounting_mode(
            &db,
            self.fingerprint.size,
            allocation,
            image_reservation.as_mut(),
        )?;
        let mut input = PositionalImage {
            file: &self.file,
            offset: 0,
            size: self.fingerprint.size,
            hash: Sha256::new(),
        };
        db.deserialize_read_exact(
            "main",
            &mut input,
            usize::try_from(self.fingerprint.size).map_err(fail)?,
            !writable,
        )?;
        if input.offset != self.fingerprint.size
            || format!("{:x}", input.hash.finalize()) != self.sha256
        {
            return Err(fail("consumed backup image digest differs"));
        }
        self.check_identity()?;
        image_accounting_mode(
            &db,
            self.fingerprint.size,
            allocation,
            image_reservation.as_mut(),
        )?;
        db.pragma_update(None, "temp_store", "MEMORY")?;
        crate::configure_native_connection(&db, 40)?;
        crate::components::install(&db)?;
        crate::catalog_source_succession::install(&db)?;
        // This is explicitly a private in-memory rehearsal, not the actual
        // source's rollback-journal admission or an exclusive file lock.
        if writable {
            let mode: String = db.query_row("PRAGMA journal_mode=MEMORY", [], |r| r.get(0))?;
            if mode != "memory" {
                return Err(fail("private image journal differs"));
            }
            let page_size: i64 = db.pragma_query_value(None, "page_size", |r| r.get(0))?;
            let page_size = u64::try_from(page_size)
                .ok()
                .filter(|size| *size > 0)
                .ok_or_else(|| fail("invalid image page size"))?;
            let max_pages = i64::try_from(MAX_IMAGE_BYTES / page_size).map_err(fail)?;
            db.pragma_update(None, "max_page_count", max_pages)?;
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        db.progress_handler(10000, Some(move || Instant::now() >= deadline))?;
        let result = {
            // Explicitly borrow the exact paid owner and actual copied database
            // only for this fixed callback. Mint performs the existing final
            // full page/SQLite checkpoint, with no lifetime gap before borrowing.
            // No ambient waiver exists. Revoke the terminal token before the
            // full postcallback page checkpoint. SQL growth between checkpoints
            // remains logical accounting, not a preemptive allocation bound.
            let image = image_reservation.as_ref().map(|paid| paid.accounting(&db)).transpose()?;
            use_image(&db, image.as_ref())
        };
        image_accounting_mode(
            &db,
            self.fingerprint.size,
            allocation,
            image_reservation.as_mut(),
        )?;
        self.revalidate()?;
        result
    }
    pub(super) fn allocated_bytes(&self) -> Result<u64> {
        (std::mem::size_of::<Self>() as u64)
            .checked_add(self.path.capacity() as u64)
            .and_then(|bytes| bytes.checked_add(self.sha256.capacity() as u64))
            .ok_or_else(|| fail("pinned backup framing allocation overflow"))
    }
    fn check_identity(&self) -> Result<()> {
        if Fingerprint::read(self.file.metadata()?)? != self.fingerprint
            || Fingerprint::read(std::fs::symlink_metadata(&self.path)?)? != self.fingerprint
        {
            return Err(fail("held or named backup fingerprint changed"));
        }
        Ok(())
    }
    /// Must be invoked immediately before the live migration COMMIT while the
    /// original catalog lifecycle/writer/path custody is also still retained.
    pub(super) fn revalidate(&self) -> Result<()> {
        self.check_identity()?;
        let digest = stream_sha256(&self.file, self.fingerprint.size)?;
        self.check_identity()?;
        if digest != self.sha256 {
            return Err(fail("actual backup bytes differ from verified image"));
        }
        Ok(())
    }
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
    #[cfg(test)]
    pub(super) fn path_for_test(&self) -> &Path {
        &self.path
    }
    /// Historical content commitment, not a present-backup-existence assertion.
    pub(super) fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// Positional reads do not mutate a shared file cursor. Each consumed byte is
/// hashed exactly once, in the order passed to SQLite's bounded image loader.
struct PositionalImage<'a> {
    file: &'a File,
    offset: u64,
    size: u64,
    hash: Sha256,
}
impl Read for PositionalImage<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let limit = output
            .len()
            .min(BUFFER_BYTES)
            .min((self.size - self.offset) as usize);
        if limit == 0 {
            return Ok(0);
        }
        let count = self.file.read_at(&mut output[..limit], self.offset)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "backup truncated",
            ));
        }
        self.hash.update(&output[..count]);
        self.offset += count as u64;
        Ok(count)
    }
}
// The fixed apply path retains four copied-image bytes plus copied SQLite
// high-water independently of its metadata workspace. Only an explicit token
// borrowing this exact paid owner and live database prevents charging that
// copied SQLite share again in its own row preflights. Foreign paths stay full.
struct WorkspaceImageReservation<'image, 'loan, 'source> {
    workspace: &'image mut ApplyMetadataLoan<'loan, 'source>,
    paid: CopiedImageReservation,
}
impl<'image, 'loan, 'source> WorkspaceImageReservation<'image, 'loan, 'source> {
    fn new(
        workspace: &'image mut ApplyMetadataLoan<'loan, 'source>,
        backup: &VerifiedBackup,
    ) -> Result<Self> {
        workspace.require_backup(backup)?;
        let paid = CopiedImageReservation::new(workspace.context(), workspace.source())?;
        Ok(Self { workspace, paid })
    }
    fn checkpoint(&self, db: &Connection, original_bytes: u64) -> Result<()> {
        self.paid.checkpoint(self.workspace.source(), db, original_bytes)
    }
    fn accounting<'a>(&'a self, db: &'a Connection) -> Result<PaidImageSqliteLoan<'a>> {
        self.paid.accounting(self.workspace.source(), db)
    }
    #[cfg(test)]
    fn charged_for_test(&self) -> u64 {
        self.paid.charged_for_test()
    }
}
fn image_accounting_mode(
    db: &Connection,
    original_bytes: u64,
    allocation: Option<(&Connection, &AllocationContext)>,
    workspace: Option<&mut WorkspaceImageReservation<'_, '_, '_>>,
) -> Result<()> {
    if let Some(workspace) = workspace {
        workspace.checkpoint(db, original_bytes)
    } else {
        image_accounting(db, original_bytes, allocation)
    }
}

// Conservative retained-allocation checks before/after loading and callback.
// This is NOT a peak-memory/RSS bound: callback temporary SQLite allocations may
// grow between checks. max_page_count separately limits persistent image growth.
fn image_accounting(
    db: &Connection,
    original_bytes: u64,
    allocation: Option<(&Connection, &AllocationContext)>,
) -> Result<()> {
    let pages: i64 = db.pragma_query_value(None, "page_count", |r| r.get(0))?;
    let page_size: i64 = db.pragma_query_value(None, "page_size", |r| r.get(0))?;
    let pages = u64::try_from(pages).map_err(|_| fail("negative image page count"))?;
    let page_size = u64::try_from(page_size)
        .ok()
        .filter(|size| *size > 0)
        .ok_or_else(|| fail("invalid image page size"))?;
    let current = pages
        .checked_mul(page_size)
        .ok_or_else(|| fail("image size overflow"))?;
    let bytes = original_bytes.max(current);
    let retained = bytes
        .checked_mul(4)
        .and_then(|v| v.checked_add(METADATA_RESERVE))
        .and_then(|v| v.checked_add(crate::catalog_backup::allocations(db).ok()?))
        .ok_or_else(|| fail("backup image allocation overflow"))?;
    if bytes == 0 || bytes > MAX_IMAGE_BYTES || retained > ACCOUNTING_LIMIT {
        return Err(fail("backup image allocation budget exceeded"));
    }
    if let Some((source, context)) = allocation {
        context.check(Some(source), retained)?;
    }
    Ok(())
}

fn stream_sha256(file: &File, size: u64) -> Result<String> {
    let mut buffer = [0_u8; BUFFER_BYTES];
    let mut offset = 0_u64;
    let mut digest = Sha256::new();
    while offset < size {
        let wanted = usize::try_from((size - offset).min(BUFFER_BYTES as u64)).map_err(fail)?;
        let count = match file.read_at(&mut buffer[..wanted], offset) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Err(fail("backup truncated during streaming hash"));
        }
        digest.update(&buffer[..count]);
        offset = offset
            .checked_add(count as u64)
            .ok_or_else(|| fail("backup offset overflow"))?;
    }
    if file.read_at(&mut buffer[..1], size)? != 0 {
        return Err(fail("backup grew during streaming hash"));
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        _dir: tempfile::TempDir,
        path: PathBuf,
        catalog: String,
        incarnation: String,
        ledger: Value,
        tables: Value,
        hash: String,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("source.sqlite3");
            migration::initialize_native_at(&source, 40).unwrap();
            let db = crate::connect(&source, false).unwrap();
            db.execute_batch("BEGIN").unwrap();
            let (catalog, incarnation) = db
                .query_row(
                    "SELECT catalog_id,incarnation FROM catalog_state WHERE id=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            let ledger = json!(migration::rows(&db, "SELECT version,filename,checksum,origin,applied_at FROM schema_migrations ORDER BY version", &[]).unwrap());
            let tables = migration::snapshot(&db).unwrap();
            let path = dir.path().join("backup.sqlite3");
            migration::copy(&db, &path).unwrap();
            drop(db);
            let file = File::open(&path).unwrap();
            let hash = stream_sha256(&file, file.metadata().unwrap().len()).unwrap();
            Self {
                _dir: dir,
                path,
                catalog,
                incarnation,
                ledger,
                tables,
                hash,
            }
        }
        fn open(&self) -> Result<VerifiedBackup> {
            VerifiedBackup::open(
                &self.path,
                &self.catalog,
                &self.incarnation,
                &self.ledger,
                &self.tables,
                &self.hash,
            )
        }
    }
    struct WorkspaceFixture {
        fixture: Fixture,
        source: Connection,
        allocation: AllocationContext,
        plan: Value,
        info: Value,
        before: Value,
        receipt: Value,
        default_parent: PathBuf,
        directory: PathBuf,
    }
    impl WorkspaceFixture {
        fn new() -> Self {
            let fixture = Fixture::new();
            let source_path = fixture._dir.path().join("source.sqlite3");
            let source = crate::connect(&source_path, true).unwrap();
            source.execute_batch("BEGIN").unwrap();
            let plan = super::super::closed41::Closed41Target
                .plan(&source, &source_path)
                .unwrap();
            let mut info = migration::validate(&source, true).unwrap();
            info["pending_migrations"] = plan["migrations"].clone();
            source.execute_batch("ROLLBACK").unwrap();
            let before = plan["input"]["tables"].clone();
            let receipt = json!({"plan": plan, "tables_before": before, "backup": fixture.path});
            let allocation = AllocationContext::default();
            allocation.bind_source(&source).unwrap();
            let default_parent = fixture._dir.path().join("default-parent");
            let directory = fixture._dir.path().to_owned();
            Self {
                fixture,
                source,
                allocation,
                plan,
                info,
                before,
                receipt,
                default_parent,
                directory,
            }
        }
        fn outer(&self) -> super::super::allocation::ApplyOuter<'_> {
            super::super::allocation::ApplyOuter {
                plan: &self.plan,
                info: &self.info,
                before: &self.before,
                receipt: &self.receipt,
                default_parent: &self.default_parent,
                directory: &self.directory,
            }
        }
        fn copies(&self) -> AllocationGuard {
            self.allocation
                .reserve(
                    &self.source,
                    super::super::allocation::value_bytes(&self.plan)
                        .unwrap()
                        .checked_mul(6)
                        .unwrap(),
                )
                .unwrap()
        }
    }
    #[test]
    fn workspace_verifies_pinned_image_and_preserves_full_charge_fallback() {
        use super::super::allocation::{
            ApplyMetadataWorkspace, LIMIT, OfflineAllocationReservation,
        };
        let f = WorkspaceFixture::new();
        let copies = f.copies();
        let mut workspace = ApplyMetadataWorkspace::new(&f.allocation, &f.source).unwrap();
        let _scope = f.allocation.enter().unwrap();
        let baseline = f.allocation.temporary_bytes_for_test().unwrap();
        let owner = {
            let mut loan = workspace
                .loan(&f.allocation, &f.source, &copies, f.outer())
                .unwrap();
            VerifiedBackup::open_with_workspace(
                &mut loan,
                &f.fixture.path,
                &f.fixture.catalog,
                &f.fixture.incarnation,
                &f.fixture.ledger,
                &f.fixture.tables,
                &f.fixture.hash,
            )
            .unwrap()
        };
        assert_eq!(f.allocation.temporary_bytes_for_test().unwrap(), baseline);
        // Leave less than a second metadata allowance. Only the branded path
        // can reuse the existing one; old reserved and standalone APIs do not.
        let used = f.allocation.retained_native_bytes(&f.source).unwrap();
        let external = LIMIT
            .checked_sub(used)
            .unwrap()
            .checked_sub(METADATA_RESERVE - 1)
            .unwrap();
        f.allocation
            .refresh(
                OfflineAllocationReservation::new(external).unwrap(),
                Some(&f.source),
            )
            .unwrap();
        assert!(
            owner
                .with_verified_image_reserved(false, &f.source, &f.allocation, |_| Ok(()))
                .is_err()
        );
        assert!(
            VerifiedBackup::open_reserved(
                &f.source,
                &f.allocation,
                &f.fixture.path,
                &f.fixture.catalog,
                &f.fixture.incarnation,
                &f.fixture.ledger,
                &f.fixture.tables,
                &f.fixture.hash,
            )
            .is_err()
        );
        let mut loan = workspace
            .loan(&f.allocation, &f.source, &copies, f.outer())
            .unwrap();
        owner
            .with_verified_image_workspace(false, &mut loan, |_, _| {
                assert!(f.allocation.temporary_bytes_for_test().unwrap() > baseline);
                Ok(())
            })
            .unwrap();
        drop(loan);
        assert_eq!(f.allocation.temporary_bytes_for_test().unwrap(), baseline);
        // The unrelated standalone API still applies its original intrinsic cap.
        owner.with_verified_image(false, |_| Ok(())).unwrap();
    }
    #[test]
    fn workspace_image_charge_is_exact_grows_additively_and_survives_refusal_until_close() {
        use super::super::allocation::{
            ApplyMetadataWorkspace, LIMIT, OfflineAllocationReservation,
        };
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        };
        fn sampled_charge(db: &Connection, original: u64, highwaters: &mut (u64, u64)) -> u64 {
            let pages: i64 = db
                .pragma_query_value(None, "page_count", |r| r.get(0))
                .unwrap();
            let page_size: i64 = db
                .pragma_query_value(None, "page_size", |r| r.get(0))
                .unwrap();
            let current = u64::try_from(pages)
                .unwrap()
                .checked_mul(u64::try_from(page_size).unwrap())
                .unwrap();
            highwaters.0 = highwaters.0.max(original).max(current);
            highwaters.1 = highwaters.1.max(crate::catalog_backup::allocations(db).unwrap());
            highwaters.0.checked_mul(4).unwrap().checked_add(highwaters.1).unwrap()
        }
        struct CloseProbe {
            context: AllocationContext,
            observed_charge: Arc<AtomicU64>,
            closed: Arc<AtomicBool>,
        }
        impl Drop for CloseProbe {
            fn drop(&mut self) {
                self.observed_charge.store(
                    self.context.temporary_bytes_for_test().unwrap_or(u64::MAX),
                    Ordering::SeqCst,
                );
                self.closed.store(true, Ordering::SeqCst);
            }
        }
        let f = WorkspaceFixture::new();
        let owner = f.fixture.open().unwrap();
        let original = owner.fingerprint.size;
        let copies = f.copies();
        let mut workspace = ApplyMetadataWorkspace::new(&f.allocation, &f.source).unwrap();
        let baseline = f.allocation.temporary_bytes_for_test().unwrap();
        let mut loan = workspace
            .loan(&f.allocation, &f.source, &copies, f.outer())
            .unwrap();
        // Same declaration/drop order as production, with a genuine copied image.
        let reservation = WorkspaceImageReservation::new(&mut loan, &owner).unwrap();
        let mut db = Connection::open_in_memory().unwrap();
        db.pragma_update(None, "cache_size", -2048).unwrap();
        let mut input = File::open(&f.fixture.path).unwrap();
        db.deserialize_read_exact(
            "main",
            &mut input,
            usize::try_from(original).unwrap(),
            false,
        )
        .unwrap();
        crate::configure_native_connection(&db, 40).unwrap();
        crate::components::install(&db).unwrap();
        crate::catalog_source_succession::install(&db).unwrap();
        let closed = Arc::new(AtomicBool::new(false));
        let observed_charge = Arc::new(AtomicU64::new(0));
        let probe = CloseProbe {
            context: f.allocation.clone(),
            observed_charge: observed_charge.clone(),
            closed: closed.clone(),
        };
        db.create_scalar_function(
            "workspace_exact_charge_close_probe",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| {
                let _held_until_sqlite_close = &probe;
                Ok(1_i64)
            },
        )
        .unwrap();
        // Independent page and SQLite high-waters may peak at different times.
        let mut highwaters = (0, 0);
        let first = sampled_charge(&db, original, &mut highwaters);
        reservation.checkpoint(&db, original).unwrap();
        assert_eq!(sampled_charge(&db, original, &mut highwaters), first);
        assert_eq!(reservation.charged_for_test(), first);
        assert_eq!(
            f.allocation.temporary_bytes_for_test().unwrap(),
            baseline + first
        );

        db.execute_batch("CREATE TABLE workspace_image_growth(value BLOB)")
            .unwrap();
        db.execute(
            "INSERT INTO workspace_image_growth VALUES(zeroblob(?1))",
            [i64::try_from(original.checked_add(8192).unwrap()).unwrap()],
        )
        .unwrap();
        let grown = sampled_charge(&db, original, &mut highwaters);
        assert!(grown > first);
        assert!(grown.checked_add(METADATA_RESERVE).unwrap() < ACCOUNTING_LIMIT);
        reservation.checkpoint(&db, original).unwrap();
        assert_eq!(sampled_charge(&db, original, &mut highwaters), grown);
        assert_eq!(reservation.charged_for_test(), grown);
        let after_growth = f.allocation.temporary_bytes_for_test().unwrap();
        assert_eq!(after_growth, baseline + grown);
        assert_eq!(after_growth - (baseline + first), grown - first);

        // A real smaller image must not retire the already admitted high-water.
        db.execute_batch("DROP TABLE workspace_image_growth; VACUUM")
            .unwrap();
        let smaller = sampled_charge(&db, original, &mut (0, 0));
        assert!(smaller < grown);
        let after_shrink_charge = sampled_charge(&db, original, &mut highwaters);
        assert!(after_shrink_charge >= grown);
        reservation.checkpoint(&db, original).unwrap();
        assert_eq!(reservation.charged_for_test(), after_shrink_charge);
        let after_shrink = baseline + after_shrink_charge;
        assert_eq!(f.allocation.temporary_bytes_for_test().unwrap(), after_shrink);

        // Fill the unchanged aggregate cap, then grow the real image again.
        // This deliberately tests checkpointed logical accounting, not allocator
        // preemption: the SQL growth occurs before its next admission checkpoint.
        let used = f.allocation.retained_native_bytes(&f.source).unwrap();
        let external = LIMIT.checked_sub(used).unwrap();
        f.allocation
            .refresh(
                OfflineAllocationReservation::new(external).unwrap(),
                Some(&f.source),
            )
            .unwrap();
        db.execute_batch("CREATE TABLE workspace_image_growth(value BLOB)")
            .unwrap();
        db.execute(
            "INSERT INTO workspace_image_growth VALUES(zeroblob(?1))",
            [
                i64::try_from(original.checked_mul(2).unwrap().checked_add(16384).unwrap())
                    .unwrap(),
            ],
        )
        .unwrap();
        let refused_charge = sampled_charge(&db, original, &mut highwaters);
        assert!(refused_charge > after_shrink_charge);
        assert!(refused_charge.checked_add(METADATA_RESERVE).unwrap() < ACCOUNTING_LIMIT);
        let error = reservation.checkpoint(&db, original).unwrap_err();
        assert_eq!(error.0, "offline aggregate allocation budget exhausted");
        assert_eq!(reservation.charged_for_test(), after_shrink_charge);
        assert_eq!(
            f.allocation.temporary_bytes_for_test().unwrap(),
            after_shrink
        );
        assert!(!closed.load(Ordering::SeqCst));

        drop(db);
        assert!(closed.load(Ordering::SeqCst));
        assert_eq!(observed_charge.load(Ordering::SeqCst), after_shrink);
        assert_eq!(
            f.allocation.temporary_bytes_for_test().unwrap(),
            after_shrink
        );
        drop(reservation);
        assert_eq!(f.allocation.temporary_bytes_for_test().unwrap(), baseline);
        drop(loan);
        drop(workspace);
        drop(copies);
        assert_eq!(f.allocation.temporary_bytes_for_test().unwrap(), 0);
    }
    #[test]
    fn pinned_owner_framing_cannot_spend_the_metadata_share() {
        use super::super::allocation::{ApplyMetadataWorkspace, value_bytes};
        let f = WorkspaceFixture::new();
        let mut outer_bytes = 0_u64;
        for value in [&f.plan, &f.info, &f.before, &f.receipt] {
            outer_bytes = outer_bytes
                .checked_add(value_bytes(value).unwrap())
                .unwrap();
        }
        for path in [&f.default_parent, &f.directory] {
            outer_bytes = outer_bytes
                .checked_add(std::mem::size_of::<PathBuf>() as u64)
                .unwrap()
                .checked_add(path.capacity() as u64)
                .unwrap();
        }
        let copies = f.allocation.reserve(&f.source, outer_bytes).unwrap();
        let mut workspace = ApplyMetadataWorkspace::new(&f.allocation, &f.source).unwrap();
        let baseline = f.allocation.temporary_bytes_for_test().unwrap();
        let mut loan = workspace
            .loan(&f.allocation, &f.source, &copies, f.outer())
            .unwrap();
        let result = VerifiedBackup::open_with_workspace(
            &mut loan,
            &f.fixture.path,
            &f.fixture.catalog,
            &f.fixture.incarnation,
            &f.fixture.ledger,
            &f.fixture.tables,
            &f.fixture.hash,
        );
        assert!(result.is_err());
        drop(loan);
        assert_eq!(f.allocation.temporary_bytes_for_test().unwrap(), baseline);
    }
    #[test]
    fn copied_database_drops_while_image_charge_is_still_held_on_all_exits() {
        use super::super::allocation::ApplyMetadataWorkspace;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct CloseProbe {
            context: AllocationContext,
            baseline: u64,
            closed: Arc<AtomicBool>,
            charged_when_closed: Arc<AtomicBool>,
        }
        impl Drop for CloseProbe {
            fn drop(&mut self) {
                self.charged_when_closed.store(
                    self.context
                        .temporary_bytes_for_test()
                        .is_ok_and(|bytes| bytes > self.baseline),
                    Ordering::SeqCst,
                );
                self.closed.store(true, Ordering::SeqCst);
            }
        }
        let f = WorkspaceFixture::new();
        let owner = f.fixture.open().unwrap();
        let copies = f.copies();
        let mut workspace = ApplyMetadataWorkspace::new(&f.allocation, &f.source).unwrap();
        let baseline = f.allocation.temporary_bytes_for_test().unwrap();
        for exit in 0..3 {
            let closed = Arc::new(AtomicBool::new(false));
            let charged_when_closed = Arc::new(AtomicBool::new(false));
            let probe = CloseProbe {
                context: f.allocation.clone(),
                baseline,
                closed: closed.clone(),
                charged_when_closed: charged_when_closed.clone(),
            };
            let mut loan = workspace
                .loan(&f.allocation, &f.source, &copies, f.outer())
                .unwrap();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                owner.with_verified_image_workspace(false, &mut loan, |db, _| {
                    db.create_scalar_function(
                        "workspace_close_probe",
                        0,
                        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                        move |_| {
                            let _held_until_sqlite_close = &probe;
                            Ok(1_i64)
                        },
                    )?;
                    match exit {
                        0 => Ok(()),
                        1 => Err(fail("controlled image callback error")),
                        _ => panic!("controlled image callback unwind"),
                    }
                })
            }));
            match exit {
                0 => assert!(outcome.unwrap().is_ok()),
                1 => assert!(outcome.unwrap().is_err()),
                _ => assert!(outcome.is_err()),
            }
            drop(loan);
            assert!(closed.load(Ordering::SeqCst));
            assert!(charged_when_closed.load(Ordering::SeqCst));
            assert_eq!(f.allocation.temporary_bytes_for_test().unwrap(), baseline);
        }
        drop(workspace);
        drop(copies);
        assert_eq!(f.allocation.temporary_bytes_for_test().unwrap(), 0);
    }
    #[test]
    fn fixed_native_apply_runs_verification_and_rehearsal_with_real_outer_values() {
        let f = Fixture::new();
        let source = f._dir.path().join("source.sqlite3");
        let owner = super::super::RetainedNativeUpgrade::acquire(
            &source,
            migration::NativeUpgradeQuiescence {
                admission_stopped: true,
                readers_and_workers_stopped: true,
                exclusive_operational_ownership: true,
            },
        )
        .unwrap();
        let (owner, plan) = owner.plan_closed41().unwrap();
        let (owner, result) = owner
            .apply_closed41(&plan, None, &|_, _, _| Ok(()))
            .unwrap();
        assert_eq!(result["schema"], 41);
        assert!(owner.db.is_autocommit());
        assert!(owner.verified_backup.is_some());
        assert!(owner.retained_preservation.is_some());
        assert_eq!(
            owner
                .allocation
                .as_ref()
                .unwrap()
                .temporary_bytes_for_test()
                .unwrap(),
            0
        );
        assert!(crate::writer_lock(&source).is_err());
        assert!(crate::native_admission_exclusive(&source).is_err());
    }
    #[test]
    fn actual_closed40_image_checks_full_source_and_hash() {
        let f = Fixture::new();
        let owner = f.open().unwrap();
        owner.revalidate().unwrap();
        assert_eq!(owner.sha256(), f.hash);
        let mut wrong = f.ledger.clone();
        wrong[0]["applied_at"] = json!("changed");
        assert!(
            VerifiedBackup::open(
                &f.path,
                &f.catalog,
                &f.incarnation,
                &wrong,
                &f.tables,
                &f.hash
            )
            .is_err()
        );
        assert!(
            VerifiedBackup::open(
                &f.path,
                &f.catalog,
                &f.incarnation,
                &f.ledger,
                &f.tables,
                &"0".repeat(64)
            )
            .is_err()
        );
        assert!(
            VerifiedBackup::open(
                &f.path,
                "different-opaque-catalog",
                &f.incarnation,
                &f.ledger,
                &f.tables,
                &f.hash
            )
            .is_err()
        );
    }
    #[test]
    fn retained_backup_rejects_replacement_growth_and_byte_mutation() {
        for change in 0..3 {
            let f = Fixture::new();
            let owner = f.open().unwrap();
            match change {
                0 => {
                    let replacement = f.path.with_extension("replacement");
                    std::fs::copy(&f.path, &replacement).unwrap();
                    std::fs::rename(replacement, &f.path).unwrap();
                }
                1 => {
                    let file = OpenOptions::new().write(true).open(&f.path).unwrap();
                    file.set_len(file.metadata().unwrap().len() + 1).unwrap();
                }
                _ => {
                    let file = OpenOptions::new().write(true).open(&f.path).unwrap();
                    let mut byte = [0];
                    owner.file.read_at(&mut byte, 100).unwrap();
                    byte[0] ^= 1;
                    file.write_at(&byte, 100).unwrap();
                }
            }
            assert!(owner.revalidate().is_err());
        }
    }
    #[test]
    fn positional_image_cannot_validate_a_pathname_aba_substitute() {
        let original = Fixture::new();
        let other = Fixture::new();
        assert_ne!(original.catalog, other.catalog);
        let owner = original.open().unwrap();
        let retained_name = original.path.with_extension("retained");
        std::fs::rename(&original.path, &retained_name).unwrap();
        std::fs::copy(&other.path, &original.path).unwrap();
        // This is precisely the gap that a second pathname SQLite open could
        // exploit. The adapter feeds the original held inode despite the name.
        let mut input = PositionalImage {
            file: &owner.file,
            offset: 0,
            size: owner.fingerprint.size,
            hash: Sha256::new(),
        };
        let mut image = Connection::open_in_memory().unwrap();
        image
            .deserialize_read_exact("main", &mut input, owner.fingerprint.size as usize, true)
            .unwrap();
        std::fs::rename(&retained_name, &original.path).unwrap();
        let catalog: String = image
            .query_row("SELECT catalog_id FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(catalog, original.catalog);
        assert_eq!(format!("{:x}", input.hash.finalize()), owner.sha256);
        // A detected pathname/ctime change refuses; even if a platform leaves
        // that fingerprint unchanged, only the original bytes can be admitted.
        if let Ok(catalog) = owner.with_verified_image(false, |db| {
            Ok(
                db.query_row("SELECT catalog_id FROM catalog_state WHERE id=1", [], |r| {
                    r.get::<_, String>(0)
                })?,
            )
        }) {
            assert_eq!(catalog, original.catalog);
        }
    }
    #[test]
    fn oversized_backup_refuses_before_image_allocation() {
        let f = Fixture::new();
        OpenOptions::new()
            .write(true)
            .open(&f.path)
            .unwrap()
            .set_len(MAX_IMAGE_BYTES + 1)
            .unwrap();
        let error = match f.open() {
            Ok(_) => panic!("oversized image admitted"),
            Err(e) => e,
        };
        assert!(error.to_string().contains("56MiB cohort"));
    }
    #[test]
    fn writable_rehearsal_is_memory_only_and_does_not_change_backup() {
        let f = Fixture::new();
        let owner = f.open().unwrap();
        owner
            .with_verified_image(true, |db| {
                let mode: String = db.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
                assert_eq!(mode, "memory");
                db.execute_batch(
                    "BEGIN EXCLUSIVE; CREATE TABLE private_rehearsal_only(value TEXT); COMMIT",
                )?;
                Ok(())
            })
            .unwrap();
        owner.revalidate().unwrap();
        owner
            .with_verified_image(false, |db| {
                assert!(
                    !db.prepare("SELECT 1 FROM sqlite_schema WHERE name='private_rehearsal_only'")?
                        .exists([])?
                );
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn fixed_callback_passes_paid_image_explicitly_and_generic_rows_remain_full_charge() {
        use super::super::allocation::{ApplyMetadataWorkspace, LIMIT};
        let f = WorkspaceFixture::new();
        let owner = f.fixture.open().unwrap();
        let copies = f.copies();
        let mut workspace = ApplyMetadataWorkspace::new(&f.allocation, &f.source).unwrap();
        let _scope = f.allocation.enter().unwrap();
        let baseline = f.allocation.temporary_bytes_for_test().unwrap();
        let mut loan = workspace.loan(&f.allocation, &f.source, &copies, f.outer()).unwrap();
        owner.with_verified_image_workspace(false, &mut loan, |db, image| {
            let spare = 128 * 1024;
            assert!(crate::catalog_backup::allocations(db)? > spare);
            let held = f.allocation.retained_native_bytes(&f.source)?
                + f.allocation.external_retained_bytes()?;
            let fill = f.allocation.reserve(&f.source, LIMIT - held - spare)?;
            let before = f.allocation.temporary_bytes_for_test()?;
            assert!(migration::rows(db, "SELECT 1 AS n", &[]).is_err());
            let rows = migration::rows_with_image(db, "SELECT 1 AS n", &[], Some(image))?;
            assert_eq!(rows, vec![json!({"n": 1})]);
            assert!(f.allocation.temporary_bytes_for_test()? >= before);
            // The token cannot be reused for a merely foreign connection.
            let foreign = Connection::open_in_memory()?;
            assert!(migration::rows_with_image(&foreign, "SELECT 1 AS n", &[], Some(image)).is_err());
            drop(rows);
            drop(fill);
            Ok(())
        }).unwrap();
        drop(loan);
        assert_eq!(f.allocation.temporary_bytes_for_test().unwrap(), baseline);
    }

}
