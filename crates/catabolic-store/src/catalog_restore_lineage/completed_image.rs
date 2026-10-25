//! Fixed consumed-bootstrap image capture. This is not normal41 admission or
//! pathname custody. The consuming facade retains the original native session.
use super::{check, fail};
#[path = "completed_image/owned_capture.rs"]
mod owned_capture;
pub(crate) use owned_capture::FailedOwnedCapture;
use crate::{Result, Store, migration::OfflineBootstrapComplete};
use rusqlite::{
    Connection,
    backup::{Backup, StepResult},
};
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

/// Only this owner can construct the separate validated image. No connection or
/// mutable bytes escape. The facade must retain the completed source session.
pub(crate) struct ValidatedCompletedImage {
    db: Connection,
    inventory: crate::catalog_backup::inventory::Audit,
    catalog: String,
    incarnation: String,
    transition: String,
    bytes: u64,
    // Returned output owns its coverage across caller checks and measurement.
    // Every payload above is destroyed before this guard on error/unwind.
    capture_retention: Option<crate::migration::OwnedImageAllocation>,
}

/// Only this module can construct a paid copy target, immediately after the
/// same copier's complete image/DBSTATUS accounting. It never brands the copy
/// as the original source or permits an arbitrary foreign-DB workspace loan.
pub(crate) struct PaidCurrentImageInventory<'a> {
    source: &'a Store,
    image: &'a Connection,
}
impl PaidCurrentImageInventory<'_> {
    pub(crate) fn source(&self) -> &Store { self.source }
    pub(crate) fn image(&self) -> &Connection { self.image }
    pub(crate) fn configure(&self) -> Result<()> { settings(self.image) }
    pub(crate) fn validate(&self, proof: &crate::migration::CurrentImageProof<'_, '_>) -> Result<()> {
        proof.validate_closed_copy(self.image)?;
        integrity(self.image)
    }
}

/// Fixed mutable attachment target, constructed only by the owned copier.
pub(crate) struct PaidCurrentImageAttachment<'a> {
    source: &'a Store,
    image: &'a mut Connection,
}
impl PaidCurrentImageAttachment<'_> {
    pub(crate) fn source(&self) -> &Store { self.source }
    pub(crate) fn attach_and_account(&mut self, data: rusqlite::serialize::OwnedData, phase: &mut crate::migration::OfflineImageAllocation<'_>, bytes: u64) -> Result<()> {
        self.image.deserialize(rusqlite::MAIN_DB, data, false)?;
        phase.admit(bytes.checked_add(crate::catalog_backup::allocations(self.image)?).ok_or_else(|| fail("current image allocation overflow"))?)
    }
}

/// Constructed only by the fixed owned copier while Backup holds the target
/// mutable borrow. No raw target identity can be supplied outside this module.
pub(crate) struct PaidCurrentBackup<'a, 'source, 'target> {
    source: &'a Store,
    copy: &'a Backup<'source, 'target>,
    target: *mut rusqlite::ffi::sqlite3,
}
impl PaidCurrentBackup<'_, '_, '_> {
    pub(crate) fn source(&self) -> &Store { self.source }
    pub(crate) fn step_and_account(&self, phase: &mut crate::migration::OfflineImageAllocation<'_>, bytes: u64) -> Result<StepResult> {
        let result = self.copy.step(64)?;
        // SAFETY: the private constructor borrows the live Backup and the exact
        // target handle it exclusively owns. The step returned before DBSTATUS.
        let sqlite = unsafe { crate::catalog_backup::allocations_handle(self.target)? };
        phase.admit(bytes.checked_add(sqlite).ok_or_else(|| fail("owned backup accounting overflow"))?)?;
        Ok(result)
    }
}

/// Closed variant conversion, never a copy or a borrowed-pointer cast.
pub(crate) fn take_owned_current_serialization(
    data: rusqlite::serialize::Data<'_>, expected: u64,
) -> Result<(rusqlite::serialize::OwnedData, u64)> {
    let len = u64::try_from(data.len()).map_err(|_| fail("current serialized length overflow"))?;
    if len != expected { return Err(fail("current serialized image length changed")); }
    match data {
        rusqlite::serialize::Data::Owned(data) => Ok((data, len)),
        rusqlite::serialize::Data::Shared(_) => Err(fail("current owned serializer returned shared storage")),
    }
}

fn size(db: &Connection) -> Result<u64> {
    let pages: i64 = db.pragma_query_value(None, "page_count", |r| r.get(0))?;
    let page: i64 = db.pragma_query_value(None, "page_size", |r| r.get(0))?;
    let bytes = u64::try_from(pages)
        .ok()
        .and_then(|n| u64::try_from(page).ok().and_then(|p| n.checked_mul(p)))
        .ok_or_else(|| fail("completed image size overflow"))?;
    if bytes == 0 || bytes > 56 * 1024 * 1024 {
        return Err(fail("completed image exceeds bounded cohort"));
    }
    Ok(bytes)
}
fn account_capture(
    bytes: u64,
    connections: &[&Connection],
    store: &Store,
    allocation: &mut crate::migration::OfflineImageAllocation<'_>,
    metadata: u64,
) -> Result<()> {
    // Source, backup, serialization and independent deserialization remain live
    // together. Two extra image charges cover SQLite growth/copy transients.
    // Two inventory phases have bounded Rust metadata; DBSTATUS is additional.
    // This is cumulative accounting, not a process-wide hard RSS ceiling.
    let mut total = bytes
        .checked_mul(6)
        .and_then(|n| n.checked_add(metadata))
        .ok_or_else(|| fail("completed image allocation overflow"))?;
    for db in connections {
        if std::ptr::eq(*db, &store.db) {
            continue;
        }
        total = total
            .checked_add(crate::catalog_backup::allocations(db)?)
            .ok_or_else(|| fail("completed image allocation overflow"))?;
    }
    if total > crate::catalog_backup::ACCOUNTING_LIMIT {
        return Err(fail("completed image allocation budget exhausted"));
    }
    allocation.admit(total)?;
    #[cfg(test)]
    allocation.record_capture_phase_for_test("copier_phase", bytes,
        total - bytes * 6 - metadata, metadata, total)?;
    Ok(())
}
fn account(
    bytes: u64, connections: &[&Connection], store: &Store,
    allocation: &mut crate::migration::OfflineImageAllocation<'_>,
) -> Result<()> {
    account_capture(bytes, connections, store, allocation, 64 * 1024 * 1024)
}
fn live(cancel: &AtomicBool, deadline: Instant) -> Result<()> {
    check(cancel)?;
    if Instant::now() >= deadline {
        return Err(fail("completed image deadline exceeded"));
    }
    Ok(())
}
fn settings(db: &Connection) -> Result<()> {
    db.pragma_update(None, "foreign_keys", true)?;
    db.pragma_update(None, "recursive_triggers", true)?;
    db.pragma_update(None, "read_uncommitted", false)?;
    db.pragma_update(None, "cache_size", -2048)?;
    Ok(())
}
fn integrity(db: &Connection) -> Result<()> {
    let value: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if value != "ok" || db.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(fail("completed image integrity failed"));
    }
    Ok(())
}

pub(crate) fn capture(
    store: &Store,
    complete: &OfflineBootstrapComplete<'_>,
    cancel: &AtomicBool,
) -> Result<ValidatedCompletedImage> {
    capture_inner(
        store,
        ImageEvidence::Pristine(complete),
        cancel,
        CaptureFault::None,
        None,
    )
}
#[cfg(test)]
pub(crate) fn capture_cancel_after_snapshot_for_test(
    store: &Store,
    complete: &OfflineBootstrapComplete<'_>,
    cancel: &AtomicBool,
) -> Result<ValidatedCompletedImage> {
    capture_inner(
        store,
        ImageEvidence::Pristine(complete),
        cancel,
        CaptureFault::CancelAfterSnapshot,
        None,
    )
}
#[derive(Clone, Copy)]
pub(crate) enum CaptureFault {
    #[cfg(test)]
    CancelBeforeVerifierCacheRetirement,
    #[cfg(test)]
    PanicBeforeVerifierCacheRetirement,
    #[cfg(test)]
    CancelAfterVerifierCacheRetirement,
    #[cfg(test)]
    PanicAfterVerifierCacheRetirement,
    #[cfg(test)]
    PressureBeforeVerifierCacheRetirement,
    #[cfg(test)]
    CancelAfterOwnedSerialize,
    #[cfg(test)]
    PanicAfterOwnedSerialize,
    #[cfg(test)]
    CancelAfterProducerClose,
    #[cfg(test)]
    PanicAfterProducerClose,
    #[cfg(test)]
    BusyProducerClose,
    #[cfg(test)]
    PanicAfterOwnedAttach,
    #[cfg(test)]
    DeniedOwnedAttach,
    #[cfg(test)]
    CancelAfterCopyReturn,
    #[cfg(test)]
    PanicAfterCopyReturn,
    #[cfg(test)]
    CancelAfterProofReturn,
    #[cfg(test)]
    PanicAfterProofReturn,
    None,
    #[cfg(test)]
    CancelAfterSnapshot,
    #[cfg(test)]
    CancelBeforeFinalSource,
    #[cfg(test)]
    CorruptCopy,
}

// Closed alternatives only: current evidence is minted by the SAME completed
// session, never by caller values or a schema-only admission override.
enum ImageEvidence<'a, 'owner, 'workspace> {
    Pristine(&'a OfflineBootstrapComplete<'owner>),
    Current(&'a crate::migration::CurrentImageProof<'owner, 'workspace>),
}
impl<'workspace> ImageEvidence<'_, '_, 'workspace> {
    fn control_workspace(&self) -> Option<&crate::migration::BootstrapWorkspace<'workspace>> {
        match self { Self::Pristine(_) => None, Self::Current(p) => p.control_workspace() }
    }
    fn metadata_bytes(&self) -> u64 { if self.control_workspace().is_some() { 0 } else { 64 * 1024 * 1024 } }

    fn validate_source(&self, store: &Store) -> Result<()> {
        match self {
            Self::Pristine(p) => p.validate_source(store),
            Self::Current(p) => p.validate_source(store),
        }
    }
    fn validate_closed_copy(&self, store: &Store, db: &Connection) -> Result<()> {
        match self {
            Self::Current(p) if p.control_workspace().is_some() => {
                p.control_workspace().ok_or_else(|| fail("current image workspace missing"))?
                    .validate_paid_current_image(&PaidCurrentImageInventory { source: store, image: db }, p)
            }
            Self::Pristine(p) => { p.validate_closed_copy(db)?; integrity(db) }
            Self::Current(p) => { p.validate_closed_copy(db)?; integrity(db) }
        }
    }
    fn validate_inventory(&self, audit: &crate::catalog_backup::inventory::Audit) -> Result<()> {
        match self {
            Self::Pristine(_) => Ok(()),
            Self::Current(p) => p.validate_inventory(audit),
        }
    }
    fn image_allocation(&self) -> Result<crate::migration::OfflineImageAllocation<'_>> {
        match self {
            Self::Pristine(p) => p.image_allocation(),
            Self::Current(p) => p.image_allocation(),
        }
    }
    fn catalog_id(&self) -> &str {
        match self {
            Self::Pristine(p) => p.catalog_id(),
            Self::Current(p) => p.catalog_id(),
        }
    }
    fn incarnation(&self) -> &str {
        match self {
            Self::Pristine(p) => p.incarnation(),
            Self::Current(p) => p.incarnation(),
        }
    }
    fn transition_digest(&self) -> &str {
        match self {
            Self::Pristine(p) => p.transition_digest(),
            Self::Current(p) => p.transition_digest(),
        }
    }
}
pub(crate) fn capture_current(
    store: &Store,
    proof: &mut crate::migration::CurrentImageProof<'_, '_>,
    cancel: &AtomicBool,
    deadline: Instant,
    fault: CaptureFault,
) -> Result<ValidatedCompletedImage> {
    if proof.control_workspace().is_some() {
        return owned_capture::capture(store, proof, cancel, deadline, fault);
    }
    capture_inner(
        store,
        ImageEvidence::Current(proof),
        cancel,
        fault,
        Some(deadline),
    )
}

fn capture_inventory(
    store: &Store, complete: &ImageEvidence<'_, '_, '_>, image: &Connection,
    bytes: u64, connections: &[&Connection],
    allocation: &mut crate::migration::OfflineImageAllocation<'_>,
    cancel: &AtomicBool, deadline: Instant,
) -> Result<crate::migration::CurrentImageInventory> {
    account_capture(bytes, connections, store, allocation, complete.metadata_bytes())?;
    match complete.control_workspace() {
        Some(control) => control.capture_paid_current_inventory(
            &PaidCurrentImageInventory { source: store, image }, cancel, deadline),
        None => Ok(crate::migration::CurrentImageInventory::ordinary(
            crate::catalog_backup::inventory::capture(image, cancel, deadline)?)),
    }
}

fn capture_inner(
    store: &Store,
    complete: ImageEvidence<'_, '_, '_>,
    cancel: &AtomicBool,
    fault: CaptureFault,
    deadline: Option<Instant>,
) -> Result<ValidatedCompletedImage> {
    // Complete was obtained before this read snapshot, never inside a nested
    // effect transaction. Exact pointer validation prevents a same-ID substitute.
    complete.validate_source(store)?;
    let mut allocation = complete.image_allocation()?;
    let metadata = complete.metadata_bytes();
    if !store.db.is_autocommit() {
        return Err(fail("completed image ambient transaction"));
    }
    // This new complete-image export uses the existing native export60s
    // operation contract; no existing restore/image deadline is changed.
    let deadline = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(60));
    live(cancel, deadline)?;
    store.db.execute_batch("BEGIN")?;
    struct Snapshot<'a>(&'a Store, bool);
    impl Drop for Snapshot<'_> {
        fn drop(&mut self) {
            if self.1 {
                let _ = self.0.rollback_transaction(None);
            }
        }
    }
    let mut snapshot = Snapshot(store, true);
    let bytes = size(&store.db)?;
    let source = capture_inventory(store, &complete, &store.db, bytes,
        &[&store.db], &mut allocation, cancel, deadline)?;
    complete.validate_inventory(&source.audit)?;
    #[cfg(test)]
    if matches!(fault, CaptureFault::CancelAfterSnapshot) {
        cancel.store(true, std::sync::atomic::Ordering::Release);
    }
    #[cfg(not(test))]
    let _ = &fault;
    live(cancel, deadline)?;
    let mut db = Connection::open_in_memory()?;
    settings(&db)?;
    account_capture(bytes, &[&store.db, &db], store, &mut allocation, metadata)?;
    {
        // Same lexical allocation-counter pattern as the retained export owner.
        // SAFETY: db stays live and on this thread until Backup drops. No SQLite
        // operation runs concurrently; counters are sampled between step calls.
        let target_handle = unsafe { db.handle() };
        let copy = Backup::new(&store.db, &mut db)?;
        loop {
            live(cancel, deadline)?;
            // SAFETY: the previous step returned; the target's mutable borrow
            // remains held by copy and only DBSTATUS counters are read here.
            let target_allocations =
                unsafe { crate::catalog_backup::allocations_handle(target_handle)? };
            let charged = bytes
                .checked_mul(6)
                .and_then(|n| n.checked_add(metadata))
                .and_then(|n| n.checked_add(target_allocations));
            if charged.is_none_or(|n| n > crate::catalog_backup::ACCOUNTING_LIMIT) {
                return Err(fail("completed backup live allocation budget exhausted"));
            }
            allocation
                .admit(charged.ok_or_else(|| fail("completed image accounting overflow"))?)?;
            match copy.step(64)? {
                StepResult::Done => break,
                StepResult::More => {}
                StepResult::Busy | StepResult::Locked => {
                    return Err(fail("completed image snapshot unexpectedly busy"));
                }
                _ => return Err(fail("completed image unknown backup state")),
            }
        }
    }
    account_capture(
        bytes.max(size(&db)?),
        &[&store.db, &db],
        store,
        &mut allocation,
        metadata,
    )?;
    // AtomicBool is borrowed, so never extend its lifetime into SQLite's static
    // callback. Inventory/backup checks cancel directly; an individual SQLite
    // validation operation has cancellation latency bounded by this deadline.
    db.progress_handler(10000, Some(move || Instant::now() >= deadline))?;
    #[cfg(test)]
    if matches!(fault, CaptureFault::CorruptCopy) {
        db.execute_batch("CREATE TABLE unexpected_current_image_copy(id INTEGER)")?;
    }
    complete.validate_closed_copy(store, &db)?;
    let copied = capture_inventory(store, &complete, &db, bytes.max(size(&db)?),
        &[&store.db, &db], &mut allocation, cancel, deadline)?;
    if copied.audit != source.audit {
        return Err(fail("completed image full inventory differs"));
    }
    // C is no longer needed. Destroy its Audit before its independent guard.
    drop(copied);
    account_capture(
        bytes.max(size(&db)?),
        &[&store.db, &db],
        store,
        &mut allocation,
        metadata,
    )?;
    live(cancel, deadline)?;
    let serialized = db.serialize("main")?;
    let serialized_bytes =
        u64::try_from(serialized.len()).map_err(|_| fail("completed image length overflow"))?;
    account_capture(
        bytes.max(serialized_bytes),
        &[&store.db, &db],
        store,
        &mut allocation,
        metadata,
    )?;
    let mut verified = Connection::open_in_memory()?;
    settings(&verified)?;
    account_capture(
        bytes.max(serialized_bytes),
        &[&store.db, &db, &verified],
        store,
        &mut allocation,
        metadata,
    )?;
    verified.deserialize_read_exact(
        "main",
        std::io::Cursor::new(&*serialized),
        serialized.len(),
        false,
    )?;
    settings(&verified)?;
    verified.progress_handler(10000, Some(move || Instant::now() >= deadline))?;
    account_capture(
        bytes.max(serialized_bytes),
        &[&store.db, &db, &verified],
        store,
        &mut allocation,
        metadata,
    )?;
    complete.validate_closed_copy(store, &verified)?;
    {
        let verified_inventory = capture_inventory(store, &complete, &verified,
            bytes.max(serialized_bytes), &[&store.db, &db, &verified],
            &mut allocation, cancel, deadline)?;
        if verified_inventory.audit != source.audit {
            return Err(fail("completed serialized image inventory differs"));
        }
        // D and its guard end together, after the required full comparison.
    }
    account_capture(
        bytes.max(serialized_bytes),
        &[&store.db, &db, &verified],
        store,
        &mut allocation,
        metadata,
    )?;
    live(cancel, deadline)?;
    verified.progress_handler(0, None::<fn() -> bool>)?;
    store.rollback_transaction(None)?;
    snapshot.1 = false;
    // All comparisons above are complete. Data may borrow the backup DB,
    // so end its lifetime first; only the verified image remains needed.
    drop(serialized);
    drop(db);
    let mut image = ValidatedCompletedImage {
        db: verified,
        inventory: source.audit,
        catalog: complete.catalog_id().into(),
        incarnation: complete.incarnation().into(),
        transition: complete.transition_digest().into(),
        bytes: serialized_bytes,
        capture_retention: None,
    };
    // Admit remaining image ownership while the old peak guard still covers
    // it. Only successful additive admission releases the ended copier phase.
    // Historical proof/history guards remain independently live throughout.
    let mut final_allocation = complete.image_allocation()?;
    final_allocation.admit(image.retained_allocations()?)?;
    image.capture_retention = Some(final_allocation.into_owned());
    #[cfg(test)]
    image.record_capture_phase_for_test("copier_output_installed")?;
    // B has transferred into image and is covered by its new owning guard.
    drop(source.retention);
    drop(allocation);
    #[cfg(test)]
    if matches!(fault, CaptureFault::CancelBeforeFinalSource) {
        cancel.store(true, std::sync::atomic::Ordering::Release);
    }
    live(cancel, deadline)?;
    complete.validate_source(store)?;
    #[cfg(test)]
    image.record_capture_phase_for_test("final_source_validated")?;
    live(cancel, deadline)?;
    Ok(image)
}

/// Descriptive trusted-host commitments, never native proof of a Motion ACK.
/// Native components are checked against the genuinely completed owned image;
/// the host remains responsible for authenticating the pair and Motion binding.
pub struct RestoreHostAttestation {
    pub pair_sha256: String,
    pub motion_binding_sha256: String,
    pub catalog_id: String,
    pub incarnation: String,
    pub completed_inventory_sha256: String,
    pub transition_digest: String,
}
/// Immutable observations for the host's independently authenticated96 owner.
pub struct CompletedImageDescription {
    pub catalog_id: String,
    pub incarnation: String,
    pub completed_inventory_sha256: String,
    pub transition_digest: String,
}
struct RotationHost<'a> {
    pair_sha256: &'a str,
    motion_binding_sha256: &'a str,
    catalog_id: &'a str,
    incarnation: &'a str,
    completed_inventory_sha256: &'a str,
    transition_digest: &'a str,
}
impl<'a> From<&'a RestoreHostAttestation> for RotationHost<'a> {
    fn from(h: &'a RestoreHostAttestation) -> Self {
        Self {
            pair_sha256: &h.pair_sha256,
            motion_binding_sha256: &h.motion_binding_sha256,
            catalog_id: &h.catalog_id,
            incarnation: &h.incarnation,
            completed_inventory_sha256: &h.completed_inventory_sha256,
            transition_digest: &h.transition_digest,
        }
    }
}
enum RotationEvidence<'a, 'b> {
    Pristine(&'a OfflineBootstrapComplete<'b>),
    Current(&'a crate::migration::CurrentImageRotationAdmission<'b>),
}
impl RotationEvidence<'_, '_> {
    fn validate_source(&self, store: &Store) -> Result<()> {
        match self {
            Self::Pristine(p) => p.validate_source(store),
            Self::Current(p) => p.validate_source(store),
        }
    }
    fn validate_closed_copy(&self, db: &Connection) -> Result<()> {
        match self {
            Self::Pristine(p) => p.validate_closed_copy(db),
            Self::Current(p) => p.validate_closed_copy(db),
        }
    }
    fn image_allocation(&self) -> Result<crate::migration::OfflineImageAllocation<'_>> {
        match self {
            Self::Pristine(p) => p.image_allocation(),
            Self::Current(p) => p.image_allocation(),
        }
    }
}
/// The only constructor replaces main with an owned SQLITE_DESERIALIZE_READONLY
/// image. SQLite's db_readonly reports the Btree open flags, not that memdb flag.
/// No mutable Connection, caller-selected readonly flag, or bytes escape.
pub(super) struct ImmutableDeserializedImage {
    db: Connection,
}
impl ImmutableDeserializedImage {
    fn deserialize(mut db: Connection, bytes: &[u8]) -> Result<Self> {
        db.deserialize_read_exact("main", std::io::Cursor::new(bytes), bytes.len(), true)?;
        Ok(Self { db })
    }
    pub(super) fn connection(&self) -> &Connection {
        &self.db
    }
}
struct RestoredReadImage {
    source_history: crate::catalog_source_succession::RetainedRestoredSourceHistory,
    poisoned: std::cell::Cell<bool>,
    db: ImmutableDeserializedImage,
    history: super::restored_history::RestoredHistoryState,
}
pub(crate) struct RotatedCompletedImage {
    current: Option<RestoredReadImage>,
    bytes: Vec<u8>,
    sha256: String,
    origin_json: String,
    inventory_sha256: String,
}
#[derive(Clone, Copy)]
pub(crate) enum RotationFault {
    None,
    AfterRotation,
    #[cfg(test)]
    PanicBeforeCommit,
    #[cfg(test)]
    PanicAfterCommit,
}

impl ValidatedCompletedImage {
    // Private qualified-copier handoff only. This borrows the actual immutable
    // image rather than accepting a caller's allocation-credit declaration.
    fn seal_current_output(&mut self, proof: &crate::migration::CurrentImageProof<'_, '_>) -> Result<()> {
        let retained = self.retained_allocations()?;
        proof.seal_image_retention(self.capture_retention.as_mut()
            .ok_or_else(|| fail("capture output guard missing"))?, retained)
    }
    pub(crate) fn commit_current_retention(&mut self, proof: &mut crate::migration::CurrentImageProof<'_, '_>, retained: u64) -> Result<()> {
        proof.commit_image_retention(self.capture_retention.as_mut().ok_or_else(|| fail("capture output guard missing"))?, retained)
    }
    /// Persistent original-session image coverage must already be installed.
    pub(crate) fn release_capture_retention(&mut self) { self.capture_retention.take(); }
    #[cfg(test)]
    pub(crate) fn has_capture_retention_for_test(&self) -> bool { self.capture_retention.is_some() }
    #[cfg(test)]
    pub(crate) fn record_capture_phase_for_test(&self, name: &'static str) -> Result<()> {
        self.capture_retention.as_ref().ok_or_else(|| fail("capture output guard missing"))?
            .record_capture_phase_for_test(name)
    }
    pub(crate) fn inventory_sha256(&self) -> &str {
        &self.inventory.complete
    }
    pub(crate) fn retained_allocations(&self) -> Result<u64> {
        let mut total = self
            .bytes
            .checked_add(crate::catalog_backup::allocations(&self.db)?)
            .ok_or_else(|| fail("retained image allocation overflow"))?;
        for text in [
            &self.catalog,
            &self.incarnation,
            &self.transition,
            &self.inventory.schema,
            &self.inventory.complete,
            &self.inventory.ledger,
        ] {
            total = total
                .checked_add(text.capacity() as u64)
                .ok_or_else(|| fail("retained image allocation overflow"))?;
        }
        for (key, value) in &self.inventory.tables {
            total = total
                .checked_add(1024)
                .and_then(|n| n.checked_add(key.capacity() as u64))
                .and_then(|n| n.checked_add(value.capacity() as u64))
                .ok_or_else(|| fail("retained image allocation overflow"))?;
        }
        Ok(total)
    }
    pub(crate) fn description(&self) -> CompletedImageDescription {
        CompletedImageDescription {
            catalog_id: self.catalog.clone(),
            incarnation: self.incarnation.clone(),
            completed_inventory_sha256: self.inventory.complete.clone(),
            transition_digest: self.transition.clone(),
        }
    }
    /// Fixed isolated-image effect. No write touches the source Store, and
    /// consuming self prevents a second rotation through the same owner.
    pub(crate) fn rotate(
        self,
        store: &Store,
        complete: &OfflineBootstrapComplete<'_>,
        host: &RestoreHostAttestation,
        cancel: &AtomicBool,
    ) -> Result<RotatedCompletedImage> {
        self.rotate_inner(
            store,
            RotationEvidence::Pristine(complete),
            host.into(),
            cancel,
            RotationFault::None,
        )
    }
    #[cfg(test)]
    pub(crate) fn fail_after_rotation_for_test(
        self,
        store: &Store,
        complete: &OfflineBootstrapComplete<'_>,
        host: &RestoreHostAttestation,
        cancel: &AtomicBool,
    ) -> Result<RotatedCompletedImage> {
        self.rotate_inner(
            store,
            RotationEvidence::Pristine(complete),
            host.into(),
            cancel,
            RotationFault::AfterRotation,
        )
    }
    pub(crate) fn rotate_current(
        self,
        store: &Store,
        proof: &crate::migration::CurrentImageRotationAdmission<'_>,
        host: &crate::migration::offline_bootstrap::CurrentRestoreHostAttestation,
        cancel: &AtomicBool,
        fault: RotationFault,
    ) -> Result<RotatedCompletedImage> {
        self.rotate_inner(
            store,
            RotationEvidence::Current(proof),
            RotationHost {
                pair_sha256: &host.pair_sha256,
                motion_binding_sha256: &host.motion_binding_sha256,
                catalog_id: &host.catalog_id,
                incarnation: &host.incarnation,
                completed_inventory_sha256: &host.current_inventory_sha256,
                transition_digest: &host.transition_digest,
            },
            cancel,
            fault,
        )
    }
    fn rotate_inner(
        self,
        store: &Store,
        complete: RotationEvidence<'_, '_>,
        host: RotationHost<'_>,
        cancel: &AtomicBool,
        fault: RotationFault,
    ) -> Result<RotatedCompletedImage> {
        complete.validate_source(store)?;
        let mut allocation = complete.image_allocation()?;
        account(
            self.bytes.max(size(&self.db)?),
            &[&store.db, &self.db],
            store,
            &mut allocation,
        )?;
        complete.validate_closed_copy(&self.db)?;
        let digest = |v: &str| {
            v.len() == 64
                && v.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if !digest(&host.pair_sha256)
            || !digest(&host.motion_binding_sha256)
            || host.catalog_id != self.catalog
            || host.incarnation != self.incarnation
            || host.completed_inventory_sha256 != self.inventory.complete
            || host.transition_digest != self.transition
        {
            return Err(fail(
                "restore host attestation differs from completed native image",
            ));
        }
        let deadline = match &complete {
            RotationEvidence::Current(proof) => proof.deadline(),
            RotationEvidence::Pristine(_) => Instant::now() + Duration::from_secs(60),
        };
        live(cancel, deadline)?;
        account(
            self.bytes.max(size(&self.db)?),
            &[&store.db, &self.db],
            store,
            &mut allocation,
        )?;
        if crate::catalog_backup::inventory::capture(&self.db, cancel, deadline)? != self.inventory
        {
            return Err(fail("completed native image changed before rotation"));
        }
        let generation: i64 =
            self.db
                .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                    r.get(0)
                })?;
        if matches!(complete, RotationEvidence::Current(_))
            && crate::catalog_backup::inventory::capture(&store.db, cancel, deadline)?
                != self.inventory
        {
            return Err(fail("current original image changed before rotation"));
        }
        let poisoned = std::cell::Cell::new(false);
        if matches!(complete, RotationEvidence::Current(_)) {
            live(cancel, deadline)?;
        }
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        struct ImageTransaction<'a>(&'a Connection, bool);
        impl Drop for ImageTransaction<'_> {
            fn drop(&mut self) {
                if self.1 {
                    let _cleanup = crate::native_writer_vm::cleanup(self.0);
                    let _ = self.0.execute_batch("ROLLBACK");
                }
            }
        }
        let mut transaction = ImageTransaction(&self.db, true);
        let admitted = super::AdmittedRestoreOwner {
            store: None,
            db: &self.db,
            poisoned: Some(&poisoned),
        };
        let origin = super::rotate_admitted_until(
            &admitted,
            &host.pair_sha256,
            &host.motion_binding_sha256,
            cancel,
            match complete {
                RotationEvidence::Current(_) => Some(deadline),
                RotationEvidence::Pristine(_) => None,
            },
            || {
                if matches!(fault, RotationFault::AfterRotation) {
                    Err(fail("injected completed image rotation failure"))
                } else {
                    Ok(())
                }
            },
        )?;
        account(
            self.bytes.max(size(&self.db)?),
            &[&store.db, &self.db],
            store,
            &mut allocation,
        )?;
        let rotated = crate::catalog_backup::inventory::capture(&self.db, cancel, deadline)?;
        live(cancel, deadline)?;
        #[cfg(test)]
        if matches!(fault, RotationFault::PanicBeforeCommit) {
            panic!("controlled isolated rotation pre-COMMIT unwind");
        }
        self.db.execute_batch("COMMIT")?;
        transaction.1 = false;
        drop(transaction);
        #[cfg(test)]
        if matches!(fault, RotationFault::PanicAfterCommit) {
            panic!("controlled isolated rotation post-COMMIT unwind");
        }
        let image_bytes = size(&self.db)?;
        account(
            self.bytes.max(image_bytes),
            &[&store.db, &self.db],
            store,
            &mut allocation,
        )?;
        let serialized = self.db.serialize("main")?;
        let length =
            u64::try_from(serialized.len()).map_err(|_| fail("rotated image size overflow"))?;
        account(
            self.bytes.max(length),
            &[&store.db, &self.db],
            store,
            &mut allocation,
        )?;
        let verifier = Connection::open_in_memory()?;
        settings(&verifier)?;
        account(
            self.bytes.max(length),
            &[&store.db, &self.db, &verifier],
            store,
            &mut allocation,
        )?;
        let immutable = ImmutableDeserializedImage::deserialize(verifier, &serialized)?;
        let verifier = immutable.connection();
        settings(&verifier)?;
        verifier.progress_handler(10000, Some(move || Instant::now() >= deadline))?;
        account(
            self.bytes.max(length),
            &[&store.db, &self.db, &verifier],
            store,
            &mut allocation,
        )?;
        integrity(&verifier)?;
        let verified = crate::catalog_backup::inventory::capture(&verifier, cancel, deadline)?;
        if verified != rotated
            || verified.schema != self.inventory.schema
            || verified.ledger != self.inventory.ledger
        {
            return Err(fail(
                "rotated completed image independent inventory differs",
            ));
        }
        let (catalog, incarnation, after_generation): (String, String, i64) = verifier.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if catalog != self.catalog
            || incarnation != origin.to_incarnation
            || after_generation != generation
        {
            return Err(fail("rotated completed image identity differs"));
        }
        let ancestry = super::chain::read(&verifier, &catalog, &incarnation, cancel)?;
        if ancestry.origins.last() != Some(&origin) {
            return Err(fail("rotated completed image lineage differs"));
        }
        account(
            self.bytes.max(length),
            &[&store.db, &self.db, &verifier],
            store,
            &mut allocation,
        )?;
        live(cancel, deadline)?;
        complete.validate_source(store)?;
        let sha256 = catabolic_core::sha256(&*serialized);
        let bytes = serialized.to_vec();
        let current = if let RotationEvidence::Current(proof) = &complete {
            let history = super::restored_history::RestoredHistoryState::from_rotation(
                &self.inventory,
                &verified,
                &origin,
                ancestry,
                generation,
                &self.transition,
            )?;
            // Fixed view support only; mutation permit remains default-deny.
            crate::catalog_source_succession::install(&verifier)?;
            verifier.execute_batch("BEGIN")?;
            let source_history =
                proof.validate_restored_copy(&verifier, &history.admit(&immutable)?)?;
            live(cancel, deadline)?;
            verifier.progress_handler(0, None::<fn() -> bool>)?;
            Some(RestoredReadImage {
                source_history,
                poisoned: std::cell::Cell::new(false),
                db: immutable,
                history,
            })
        } else {
            None
        };
        Ok(RotatedCompletedImage {
            current,
            bytes,
            sha256,
            origin_json: serde_json::to_string(&origin)?,
            inventory_sha256: verified.complete,
        })
    }
}
impl RotatedCompletedImage {
    pub(crate) fn current_source_evidence(
        &self,
        profile: &str,
        source: &str,
        budget: &mut crate::catalog_nfo_attribution::ProofBudget,
        cancel: &AtomicBool,
    ) -> Result<crate::catalog_source_succession::RestoredSourceEvidence> {
        check(cancel)?;
        let image = self
            .current
            .as_ref()
            .ok_or_else(|| fail("not a current restored image"))?;
        if image.poisoned.get() {
            return Err(fail("restored image read owner poisoned"));
        }
        let operation = crate::operation_budget::OperationBudget::current()
            .ok_or_else(|| fail("restored image read requires retained operation"))?;
        // This private copied connection has no caller-installed hooks. Charge
        // its VM to the SAME operation as the retained source writer, never a
        // fresh local allowance. The callback performs no database operation:
        // charge intentionally preserves the thread-local embedding interrupt
        // bridge, polling the host outside the budget mutex before accounting.
        // The separately borrowed cancel flag is checked around this read; it
        // is not captured by SQLite's static callback.
        image.poisoned.set(true);
        image.db.connection().progress_handler(
            1,
            Some(move || {
                operation
                    .charge(crate::operation_budget::BudgetCharge {
                        work: 1,
                        ..Default::default()
                    })
                    .is_err()
            }),
        )?;
        image.poisoned.set(false);
        struct Vm<'a> {
            image: &'a RestoredReadImage,
            armed: bool,
        }
        impl Vm<'_> {
            fn finish(mut self) -> Result<()> {
                self.armed = false;
                if let Err(error) = self
                    .image
                    .db
                    .connection()
                    .progress_handler(0, None::<fn() -> bool>)
                {
                    self.image.poisoned.set(true);
                    return Err(error.into());
                }
                Ok(())
            }
        }
        impl Drop for Vm<'_> {
            fn drop(&mut self) {
                if self.armed
                    && self
                        .image
                        .db
                        .connection()
                        .progress_handler(0, None::<fn() -> bool>)
                        .is_err()
                {
                    self.image.poisoned.set(true);
                }
            }
        }
        let vm = Vm { image, armed: true };
        let result = (|| {
            let proof = image.history.admit(&image.db)?;
            let result =
                crate::catalog_source_succession::capture_restored_source_evidence_with_witness_in(
                    image.db.connection(),
                    &proof,
                    &image.source_history,
                    profile,
                    source,
                    budget,
                )?;
            check(cancel)?;
            proof.check(image.db.connection())?;
            Ok(result)
        })();
        vm.finish()?;
        result
    }

    pub(crate) fn retained_allocations(&self) -> Result<u64> {
        let current = if let Some(current) = &self.current {
            size(current.db.connection())?
                .checked_add(crate::catalog_backup::allocations(current.db.connection())?)
                .and_then(|n| n.checked_add(current.history.retained_allocations().ok()?))
                .and_then(|n| n.checked_add(current.source_history.retained_allocations().ok()?))
                .ok_or_else(|| fail("retained restored image allocation overflow"))?
        } else {
            0
        };
        [
            self.bytes.capacity(),
            self.sha256.capacity(),
            self.origin_json.capacity(),
            self.inventory_sha256.capacity(),
        ]
        .into_iter()
        .try_fold(current, |n, bytes| {
            n.checked_add(bytes as u64)
                .ok_or_else(|| fail("retained image allocation overflow"))
        })
    }
    #[cfg(test)]
    pub(crate) fn verify_history_scope_for_test(
        &self,
        foreign: &Connection,
        original: &str,
        budget: &mut impl crate::catalog_identity_mutation::StructuralReadBudget,
    ) -> Result<()> {
        let current = self
            .current
            .as_ref()
            .ok_or_else(|| fail("missing restored read image"))?;
        let proof = current.history.admit(&current.db)?;
        // Separate genuine immutable copy in autocommit: this cannot pass by
        // merely reporting a nested-BEGIN error from the retained snapshot.
        let independent =
            ImmutableDeserializedImage::deserialize(Connection::open_in_memory()?, &self.bytes)?;
        assert!(independent.connection().is_autocommit());
        require_readonly_for_test(independent.connection().execute_batch("BEGIN IMMEDIATE"))?;
        assert!(independent.connection().is_autocommit());
        // Readonly memdb storage, not a mutable connection with query_only set.
        assert!(
            !current
                .db
                .connection()
                .query_row("PRAGMA query_only", [], |r| r.get::<_, bool>(0))?
        );
        let changes = current.db.connection().total_changes();
        for sql in [
            "INSERT INTO main.catalog_state SELECT * FROM main.catalog_state WHERE 0",
            "UPDATE main.catalog_state SET generation=generation WHERE 0",
            "CREATE TABLE main.restore_readonly_probe(value INTEGER)",
        ] {
            require_readonly_for_test(current.db.connection().execute_batch(sql))?;
            proof.check(current.db.connection())?;
        }
        assert_eq!(current.db.connection().total_changes(), changes);
        assert_eq!(
            catabolic_core::sha256(&*current.db.connection().serialize("main")?),
            self.sha256
        );
        if proof.check(foreign).is_ok()
            || proof.origin_rank(original)? >= proof.origin_rank(proof.incarnation())?
            || proof
                .require_forward_origins(proof.incarnation(), original)
                .is_ok()
            || proof.authorize_historical("not-an-origin", 0).is_ok()
            || proof
                .authorize_historical(
                    original,
                    proof
                        .generation()
                        .checked_add(1)
                        .ok_or_else(|| fail("generation overflow"))?,
                )
                .is_ok()
        {
            return Err(fail(
                "restored history scope negative unexpectedly admitted",
            ));
        }
        // Exercise the actual fixed reader under the real inherited operation,
        // not just the token primitive. Even a genuine original catalog
        // connection is not the immutable rotated snapshot bound by this token.
        let error = match crate::catalog_source_succession::capture_restored_source_evidence_in(
            foreign, &proof, "default", "media", budget,
        ) {
            Err(error) => error,
            Ok(_) => return Err(fail("foreign restored source reader unexpectedly admitted")),
        };
        if !error
            .to_string()
            .contains("restore history requires exact immutable read snapshot")
        {
            return Err(fail(
                "foreign reader refused before its exact connection check",
            ));
        }
        proof.check(current.db.connection())?;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn verify_source_effect_tamper_for_test(
        &self,
        admission: &crate::migration::CurrentImageRotationAdmission<'_>,
        budget: &mut crate::catalog_nfo_attribution::ProofBudget,
    ) -> Result<()> {
        let current = self
            .current
            .as_ref()
            .ok_or_else(|| fail("missing restored read image"))?;
        let proof = current.history.admit(&current.db)?;
        let captured = crate::catalog_source_succession::capture_restored_source_history_in(
            current.db.connection(),
            &proof,
            budget,
        )?;
        captured.assert_effect_tamper_refused_for_test(
            current.db.connection(),
            &proof,
            admission,
            budget,
        )
    }
    #[cfg(test)]
    pub(crate) fn verify_source_witness_for_test(
        &mut self,
        foreign: &Connection,
        budget: &mut crate::catalog_nfo_attribution::ProofBudget,
    ) -> Result<()> {
        {
            let current = self
                .current
                .as_mut()
                .ok_or_else(|| fail("missing restored read image"))?;
            let proof = current.history.admit(&current.db)?;
            current.source_history.assert_owner_negatives_for_test(
                current.db.connection(),
                &proof,
                foreign,
                budget,
            )?;
            let foreign_error = match crate::catalog_source_succession::capture_restored_source_evidence_with_witness_in(
                foreign, &proof, &current.source_history, "default", "media", budget,
            ) {
                Err(error) => error,
                Ok(_) => return Err(fail("foreign witness reader unexpectedly admitted")),
            };
            assert!(
                foreign_error
                    .to_string()
                    .contains("restore history requires exact immutable read snapshot")
            );
        }
        let current = self.current.as_ref().unwrap();
        current.poisoned.set(true);
        let poisoned =
            self.current_source_evidence("default", "media", budget, &AtomicBool::new(false));
        current.poisoned.set(false);
        let error = match poisoned {
            Err(error) => error,
            Ok(_) => return Err(fail("poisoned source reader unexpectedly admitted")),
        };
        assert!(
            error
                .to_string()
                .contains("restored image read owner poisoned")
        );
        // The fully enumerated empty prefix set establishes history absence only.
        // The fixed SQL reader must independently produce an empty result.
        let empty = self.current_source_evidence(
            "default",
            "not-a-source",
            budget,
            &AtomicBool::new(false),
        )?;
        assert!(empty.headers().is_empty() && empty.facts().is_empty());
        let current = self.current.as_ref().unwrap();
        let proof = current.history.admit(&current.db)?;
        current.db.connection().execute_batch("COMMIT")?;
        let ended =
            crate::catalog_source_succession::capture_restored_source_evidence_with_witness_in(
                current.db.connection(),
                &proof,
                &current.source_history,
                "default",
                "media",
                budget,
            );
        let error = match ended {
            Err(error) => error,
            Ok(_) => return Err(fail("ended witness reader unexpectedly admitted")),
        };
        assert!(
            error
                .to_string()
                .contains("restore history requires exact immutable read snapshot")
        );
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn verify_ended_snapshot_for_test(&self) -> Result<()> {
        let current = self
            .current
            .as_ref()
            .ok_or_else(|| fail("missing restored read image"))?;
        let proof = current.history.admit(&current.db)?;
        // COMMIT is not a write to immutable storage. It deliberately ends this
        // test-only snapshot; the fixed public image API exposes no such SQL.
        current.db.connection().execute_batch("COMMIT")?;
        assert!(current.db.connection().is_autocommit());
        assert!(proof.check(current.db.connection()).is_err());
        assert!(current.history.admit(&current.db).is_err());
        Ok(())
    }
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
    pub(crate) fn len(&self) -> u64 {
        self.bytes.len() as u64
    }
    pub(crate) fn origin_json(&self) -> &str {
        &self.origin_json
    }
    pub(crate) fn inventory_sha256(&self) -> &str {
        &self.inventory_sha256
    }
    pub(crate) fn copy_verified_to(
        &self,
        output: &mut impl std::io::Write,
        cancel: &AtomicBool,
    ) -> Result<()> {
        check(cancel)?;
        if catabolic_core::sha256(&self.bytes) != self.sha256 {
            return Err(fail("retained rotated image digest differs"));
        }
        for chunk in self.bytes.chunks(64 * 1024) {
            check(cancel)?;
            output.write_all(chunk)?;
        }
        check(cancel)
    }
}

#[cfg(test)]
fn require_readonly_for_test(result: rusqlite::Result<()>) -> Result<()> {
    match result {
        Err(rusqlite::Error::SqliteFailure(error, _))
            if error.extended_code & 0xff == rusqlite::ffi::SQLITE_READONLY =>
        {
            Ok(())
        }
        other => Err(fail(&format!(
            "immutable storage did not refuse SQLITE_READONLY: {other:?}"
        ))),
    }
}

/// Private prepared rows, not mutation authority or a historical output proof.
pub(crate) struct PlannedCurrentRotation {
    origin: super::chain::Origin,
    _workers: Vec<super::chain::Worker>,
    _rows: super::chain::PreparedInsertion,
    generation: i64,
}
impl PlannedCurrentRotation {
    pub(crate) fn origin_json(&self) -> &str {
        &self._rows.origin_json
    }
    pub(crate) fn restore_id(&self) -> &str {
        &self.origin.restore_id
    }
    pub(crate) fn to_incarnation(&self) -> &str {
        &self.origin.to_incarnation
    }
    pub(crate) fn generation(&self) -> i64 {
        self.generation
    }
}
impl ValidatedCompletedImage {
    // Private fixed owner access only. No connection escapes the crate facade.
    pub(crate) fn planning_db(&self) -> &Connection {
        &self.db
    }
    pub(crate) fn prepare_current_rotation(
        &self,
        store: &Store,
        proof: &crate::migration::CurrentImageRotationAdmission<'_>,
        host: &crate::migration::offline_bootstrap::CurrentRestoreHostAttestation,
        cancel: &AtomicBool,
        source_observer: &crate::catalog_source_succession::authorizer::PlannedReadObserver,
        copy_observer: &crate::catalog_source_succession::authorizer::PlannedReadObserver,
        allocation: &mut crate::migration::OwnedImageAllocation,
    ) -> Result<PlannedCurrentRotation> {
        let deadline = proof.deadline();
        live(cancel, deadline)?;
        let total = self
            .bytes
            .max(size(&self.db)?)
            .checked_mul(6)
            .and_then(|n| n.checked_add(64 * 1024 * 1024))
            .and_then(|n| {
                crate::catalog_backup::allocations(&self.db)
                    .ok()
                    .and_then(|k| n.checked_add(k))
            })
            .ok_or_else(|| fail("planned rotation allocation overflow"))?;
        allocation.admit(&store.db, total)?;
        proof.validate_planning_source(store, source_observer)?;
        proof.validate_planning_copy(&self.db, copy_observer)?;
        let digest = |v: &str| {
            v.len() == 64
                && v.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if !digest(&host.pair_sha256)
            || !digest(&host.motion_binding_sha256)
            || host.catalog_id != self.catalog
            || host.incarnation != self.incarnation
            || host.current_inventory_sha256 != self.inventory.complete
            || host.transition_digest != self.transition
        {
            return Err(fail("planned host attestation differs from exact image"));
        }
        if crate::catalog_backup::inventory::capture(&self.db, cancel, deadline)? != self.inventory
            || crate::catalog_backup::inventory::capture(&store.db, cancel, deadline)?
                != self.inventory
        {
            return Err(fail("planned current image inventory changed"));
        }
        let generation: i64 =
            self.db
                .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                    r.get(0)
                })?;
        if generation != host.generation {
            return Err(fail("planned generation differs"));
        }
        let poisoned = std::cell::Cell::new(false);
        let admitted = super::AdmittedRestoreOwner {
            store: None,
            db: &self.db,
            poisoned: Some(&poisoned),
        };
        let old = super::prepare_candidate_in(&admitted, &self.catalog, &self.incarnation, cancel)?;
        let ancestry = super::chain::read(&self.db, &self.catalog, &self.incarnation, cancel)?;
        let origin = super::chain::Origin {
            contract: 1,
            restore_id: uuid::Uuid::new_v4().to_string(),
            catalog_id: self.catalog.clone(),
            schema_version: 41,
            from_incarnation: self.incarnation.clone(),
            to_incarnation: uuid::Uuid::new_v4().simple().to_string(),
            parent_restore_id: ancestry.origins.last().map(|o| o.restore_id.clone()),
            pair_sha256: host.pair_sha256.clone(),
            native_inventory_sha256: self.inventory.complete.clone(),
            motion_binding_sha256: host.motion_binding_sha256.clone(),
        };
        drop(ancestry);
        let workers = old
            .owners
            .iter()
            .map(|o| super::chain::Worker {
                contract: 1,
                restore_id: origin.restore_id.clone(),
                owner_kind: o.kind.clone(),
                owner_id: o.id.clone(),
                origin_incarnation: o.origin_incarnation.clone(),
                tuple_sha256: o.tuple_sha256.clone(),
            })
            .collect::<Vec<_>>();
        let rows = super::chain::prepare_exact(&self.db, &origin, &workers)?;
        live(cancel, deadline)?;
        allocation.admit(
            &store.db,
            total
                .checked_add(crate::catalog_backup::allocations(&self.db)?)
                .ok_or_else(|| fail("planned allocation overflow"))?,
        )?;
        Ok(PlannedCurrentRotation {
            origin,
            _workers: workers,
            _rows: rows,
            generation,
        })
    }
}
