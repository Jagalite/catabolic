//! Fixed partial native acquisition. No public connection, arbitrary SQL,
//! callback, replay or recovered authority is provided by these slots.
use super::*;
use crate::{NativeWriterAcquisition, native_admission::NativeAdmissionAcquisition};
use std::{
    fs::File,
    os::unix::fs::MetadataExt,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

pub(in crate::migration) struct NativePreparedIdentity {
    pub(in crate::migration) catalog: String,
    pub(in crate::migration) incarnation: String,
    pub(in crate::migration) generation: i64,
    pub(in crate::migration) device: u64,
    pub(in crate::migration) inode: u64,
}
pub(in crate::migration) struct NativePreparedBootstrap {
    pub(in crate::migration) owner: RetainedNativeUpgrade,
    pub(in crate::migration) plan: Value,
    pub(in crate::migration) identity: NativePreparedIdentity,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum PreparationState {
    Inputs,
    SourceLocked,
    Failed,
    Prepared,
}
pub(in crate::migration) struct NativePreparation {
    // Drop SQLite before writer and lifecycle leases, even for partial failure.
    db: Option<Connection>,
    writer: Option<NativeWriterAcquisition>,
    admission: Option<NativeAdmissionAcquisition>,
    input: PathBuf,
    path: Option<PathBuf>,
    plan: Option<Value>,
    metadata: Option<fs::Metadata>,
    // Derived only from an actually borrowed source File under both leases.
    source_metadata: Option<fs::Metadata>,
    identity: Option<NativePreparedIdentity>,
    allocation: allocation::AllocationContext,
    preparation_bytes: u64,
    state: PreparationState,
    // Stable scratch survives plan failure/retain_plan refusal, not just a call
    // frame. Successful finish releases it only after real scratch is gone and
    // the completed plan has its own existing durable high-water charge.
    _scratch: allocation::AllocationGuard,
    #[cfg(test)]
    _pressure: Option<allocation::AllocationGuard>,
}
fn same_identity(held: &fs::Metadata, named: &fs::Metadata) -> bool {
    held.is_file()
        && named.is_file()
        && held.nlink() == 1
        && named.nlink() == 1
        && held.dev() == named.dev()
        && held.ino() == named.ino()
}
fn same_source(held: &fs::Metadata, named: &fs::Metadata) -> bool {
    same_identity(held, named)
        && held.len() == named.len()
        && held.mtime() == named.mtime()
        && held.mtime_nsec() == named.mtime_nsec()
        && held.ctime() == named.ctime()
        && held.ctime_nsec() == named.ctime_nsec()
}
fn failed() -> Error {
    Error("offline native preparation state unavailable".into())
}
fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or_else(failed)
}
fn check(deadline: Instant, cancel: &AtomicBool) -> Result<()> {
    if Instant::now() >= deadline || cancel.load(Ordering::Acquire) {
        return Err(Error("offline native preparation interrupted".into()));
    }
    Ok(())
}
#[cfg(test)]
fn fault_at(wanted: u8, point: u8) {
    if wanted == point {
        panic!("controlled retained native preparation unwind at {point}");
    }
}
impl NativePreparation {
    pub(in crate::migration) fn admit(
        path: &Path,
        quiescence: NativeUpgradeQuiescence,
        reservation: OfflineAllocationReservation,
        outer_storage: u64,
    ) -> Result<Self> {
        Self::admit_inner(path, quiescence, reservation, outer_storage, None)
    }
    pub(in crate::migration) fn admit_with_control_workspace(
        path: &Path,
        quiescence: NativeUpgradeQuiescence,
        residual_reservation: OfflineAllocationReservation,
        outer_storage: u64,
        backing: &mut OfflineControlWorkspaceBacking,
    ) -> Result<Self> {
        Self::admit_inner(path, quiescence, residual_reservation, outer_storage, Some(backing))
    }
    fn admit_inner(
        path: &Path,
        quiescence: NativeUpgradeQuiescence,
        reservation: OfflineAllocationReservation,
        outer_storage: u64,
        backing: Option<&mut OfflineControlWorkspaceBacking>,
    ) -> Result<Self> {
        quiescence.require()?;
        // Exactly one context, created before any native File or Connection.
        // Only this original admission may register the fixed backing; scalar
        // refreshes preserve both its brand and the full external high-water.
        let allocation = match backing {
            Some(backing) => backing.admit(reservation)?,
            None => allocation::AllocationContext::new(reservation)?,
        };
        let input_bytes = (path.as_os_str().len() as u64)
            .checked_mul(2)
            .ok_or_else(failed)?;
        let storage = add(
            add(
                outer_storage,
                allocation::AllocationContext::preparation_context_storage(),
            )?,
            (std::mem::size_of::<Self>()
                + std::mem::size_of::<Result<Self>>()
                + std::mem::size_of::<NativePreparedBootstrap>()
                + std::mem::size_of::<std::result::Result<NativePreparedBootstrap, Self>>())
                as u64,
        )?;
        // Two identity strings obey the already-existing Transition identity
        // bounds; this dedicated charge does not borrow shared plan framing.
        let preparation_bytes = add(add(storage, input_bytes)?, 2 * (256 + 32))?;
        allocation.retain_preparation_unbound(preparation_bytes)?;
        let scratch = allocation.reserve_unbound(allocation::MIGRATION_SCRATCH)?;
        let input = path.to_owned();
        if input.capacity() as u64 > input_bytes {
            return Err(failed());
        }
        Ok(Self {
            db: None,
            writer: None,
            admission: None,
            input,
            path: None,
            plan: None,
            metadata: None,
            source_metadata: None,
            identity: None,
            allocation,
            preparation_bytes,
            state: PreparationState::Inputs,
            _scratch: scratch,
            #[cfg(test)]
            _pressure: None,
        })
    }
    pub(in crate::migration) fn refresh_external_reservation(
        &mut self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        if self.state != PreparationState::Inputs {
            return Err(failed());
        }
        // The same unbound context already owns the admitted slot/path storage
        // and scratch. No resolver, resource acquisition, binding or reset.
        self.allocation.refresh(reservation, None)
    }
    pub(in crate::migration) fn refresh_source_locked_reservation(
        &mut self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        if self.state != PreparationState::SourceLocked {
            return Err(failed());
        }
        self.state = PreparationState::Failed;
        // Same unbound context; both leases remain owned and SQLite is unopened.
        // No SQL, path resolution, embedding poll, binding or counter reset.
        self.allocation.refresh(reservation, None)?;
        self.state = PreparationState::SourceLocked;
        Ok(())
    }
    fn resolve_paths(&mut self) -> Result<()> {
        // Bound the known input-derived copies before the unchanged safe
        // resolver. Its absolute/canonicalize/join implementation still owns
        // provisional buffers internally; this is NOT a preallocation proof for
        // unknown canonical output or the library's internal workspace.
        let input_cohort = (self.input.capacity() as u64)
            .checked_mul(4)
            .ok_or_else(failed)?;
        if input_cohort > allocation::MIGRATION_SCRATCH {
            return Err(Error(
                "offline path preparation exceeds scratch cohort".into(),
            ));
        }
        self.path = Some(database_path(&self.input)?);
        let path = self.path.as_ref().ok_or_else(failed)?;
        if add(input_cohort, path.capacity() as u64)? > allocation::MIGRATION_SCRATCH {
            // This post-return refusal is not preadmission of arbitrary libc/
            // std output. Normalization still precedes all native acquisition.
            return Err(Error("offline resolved path exceeds scratch cohort".into()));
        }
        let path_len = path.as_os_str().len() as u64;
        let admission_bytes = add(path_len, NativeAdmissionAcquisition::SUFFIX.len() as u64)?
            .checked_mul(2)
            .ok_or_else(failed)?;
        let writer_bytes = add(path_len, NativeWriterAcquisition::SUFFIX.len() as u64)?
            .checked_mul(2)
            .ok_or_else(failed)?;
        self.preparation_bytes = add(
            self.preparation_bytes,
            add(path.capacity() as u64, add(admission_bytes, writer_bytes)?)?,
        )?;
        self.allocation
            .retain_preparation_unbound(self.preparation_bytes)?;
        // Both sidecar copies are charged BEFORE their constructors allocate.
        self.admission = Some(NativeAdmissionAcquisition::new(path));
        self.writer = Some(NativeWriterAcquisition::new(path));
        if self.admission.as_ref().ok_or_else(failed)?.path_capacity() as u64 > admission_bytes
            || self.writer.as_ref().ok_or_else(failed)?.path_capacity() as u64 > writer_bytes
        {
            return Err(failed());
        }
        Ok(())
    }
    pub(in crate::migration) fn prepare(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
        fault: u8,
    ) -> Result<()> {
        self.acquire_source_locks(deadline, cancel, fault)?;
        // Compatibility: ordinary preparation has no externally staged File.
        self.continue_locked(None, deadline, cancel, fault)
    }
    pub(in crate::migration) fn acquire_source_locks(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
        fault: u8,
    ) -> Result<()> {
        if self.state != PreparationState::Inputs {
            return Err(failed());
        }
        self.state = PreparationState::Failed;
        check(deadline, cancel)?;
        self.resolve_paths()?;
        check(deadline, cancel)?;
        self.admission.as_mut().ok_or_else(failed)?.open()?;
        #[cfg(test)]
        fault_at(fault, 1);
        check(deadline, cancel)?;
        self.admission.as_mut().ok_or_else(failed)?.lock()?;
        #[cfg(test)]
        fault_at(fault, 2);
        check(deadline, cancel)?;
        #[cfg(test)]
        if fault == 18 {
            let mut sidecar = self
                .path
                .as_ref()
                .ok_or_else(failed)?
                .as_os_str()
                .to_owned();
            sidecar.push(NativeAdmissionAcquisition::SUFFIX);
            let sidecar = PathBuf::from(sidecar);
            fs::rename(&sidecar, sidecar.with_extension("retained-original"))?;
            fs::write(sidecar, b"")?;
        }
        self.admission.as_ref().ok_or_else(failed)?.validate()?;
        check(deadline, cancel)?;
        self.writer.as_mut().ok_or_else(failed)?.open()?;
        #[cfg(test)]
        fault_at(fault, 3);
        check(deadline, cancel)?;
        self.writer.as_mut().ok_or_else(failed)?.lock()?;
        #[cfg(test)]
        fault_at(fault, 4);
        check(deadline, cancel)?;
        self.admission.as_ref().ok_or_else(failed)?.validate()?;
        check(deadline, cancel)?;
        #[cfg(test)]
        fault_at(fault, 19);
        #[cfg(not(test))]
        let _ = fault;
        self.state = PreparationState::SourceLocked;
        Ok(())
    }
    pub(in crate::migration) fn prepare_locked(
        &mut self,
        source: &File,
        deadline: Instant,
        cancel: &AtomicBool,
        fault: u8,
    ) -> Result<()> {
        self.continue_locked(Some(source), deadline, cancel, fault)
    }
    fn continue_locked(
        &mut self,
        source: Option<&File>,
        deadline: Instant,
        cancel: &AtomicBool,
        fault: u8,
    ) -> Result<()> {
        if self.state != PreparationState::SourceLocked {
            return Err(failed());
        }
        self.state = PreparationState::Failed;
        check(deadline, cancel)?;
        if !matches!(
            self.writer.as_ref(),
            Some(NativeWriterAcquisition::Locked(_))
        ) {
            return Err(failed());
        }
        self.admission.as_ref().ok_or_else(failed)?.validate()?;
        if let Some(source) = source {
            // Capture the actual retained descriptor, never a caller assertion.
            // Both leases precede these metadata reads and remain held through
            // open/planning/finish. A regular one-link native path must name it.
            self.source_metadata = Some(source.metadata()?);
            let held = self.source_metadata.as_ref().ok_or_else(failed)?;
            let named = fs::symlink_metadata(self.path.as_ref().ok_or_else(failed)?)?;
            if !same_source(held, &named) {
                return Err(Error(
                    "offline held source differs from locked native path".into(),
                ));
            }
            check(deadline, cancel)?;
            #[cfg(test)]
            fault_at(fault, 20);
        }
        check(deadline, cancel)?;
        #[cfg(test)]
        if fault == 17 {
            let path = self.path.as_ref().ok_or_else(failed)?;
            fs::rename(path, path.with_extension("preparation-original"))?;
            fs::create_dir(path)?;
        }
        self.db = Some(crate::open_connection_for_setup(
            self.path.as_deref().ok_or_else(failed)?,
            true,
        )?);
        #[cfg(test)]
        fault_at(fault, 5);
        #[cfg(test)]
        if fault == 16 {
            self._pressure = Some(self.allocation.fill_unbound_for_test()?);
        }
        check(deadline, cancel)?;
        let db = self.db.as_ref().ok_or_else(failed)?;
        // Binding may fail after installing its true handle/SQLite measurement;
        // that state and the exact Connection remain in this failed owner.
        self.allocation.bind_source(db)?;
        #[cfg(test)]
        fault_at(fault, 6);
        self.allocation.check(Some(db), 0)?;
        check(deadline, cancel)?;
        crate::configure_open_connection(db)?;
        #[cfg(test)]
        fault_at(fault, 7);
        check(deadline, cancel)?;
        self.allocation.check(Some(db), 0)?;
        let _scope = self.allocation.enter()?;
        check(deadline, cancel)?;
        db.execute_batch("BEGIN")?;
        #[cfg(test)]
        fault_at(fault, 8);
        check(deadline, cancel)?;
        self.plan =
            Some(closed41::Closed41Target.plan(db, self.path.as_deref().ok_or_else(failed)?)?);
        #[cfg(test)]
        fault_at(fault, 9);
        check(deadline, cancel)?;
        self.allocation
            .retain_plan(self.plan.as_ref().ok_or_else(failed)?, db)?;
        #[cfg(test)]
        fault_at(fault, 10);
        check(deadline, cancel)?;
        db.execute_batch("ROLLBACK")?;
        #[cfg(test)]
        fault_at(fault, 11);
        check(deadline, cancel)?;
        #[cfg(test)]
        if fault == 14 {
            // Real named-file identity mismatch, not a fabricated validation error.
            let path = self.path.as_ref().ok_or_else(failed)?;
            let moved = path.with_extension("preparation-original");
            fs::rename(path, &moved)?;
            fs::copy(&moved, path)?;
        }
        self.metadata = Some(fs::symlink_metadata(
            self.path.as_ref().ok_or_else(failed)?,
        )?);
        check(deadline, cancel)?;
        let metadata = self.metadata.as_ref().ok_or_else(failed)?;
        if let Some(source) = source {
            let held = source.metadata()?;
            let original = self.source_metadata.as_ref().ok_or_else(failed)?;
            // SQLite setup may recover a journal, so the continuation constrains
            // the same file identity, not an unchanged post-open byte image.
            if !same_identity(original, &held) || !same_identity(&held, metadata) {
                return Err(Error(
                    "offline held source identity changed during preparation".into(),
                ));
            }
            check(deadline, cancel)?;
        }
        let (catalog, incarnation, generation): (String, String, i64) = db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
            [],
            |row| {
                let catalog = row
                    .get_ref(0)?
                    .as_str()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                let incarnation = row
                    .get_ref(1)?
                    .as_str()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                if !(1..=256).contains(&catalog.len()) || incarnation.len() != 32 {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                let generation = row.get(2)?;
                Ok((catalog.to_owned(), incarnation.to_owned(), generation))
            },
        )?;
        self.identity = Some(NativePreparedIdentity {
            catalog,
            incarnation,
            generation,
            device: metadata.dev(),
            inode: metadata.ino(),
        });
        #[cfg(test)]
        fault_at(fault, 12);
        check(deadline, cancel)?;
        self.admission.as_ref().ok_or_else(failed)?.validate()?;
        let identity = self.identity.as_ref().ok_or_else(failed)?;
        let plan = self.plan.as_ref().ok_or_else(failed)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || identity.catalog.capacity() + identity.incarnation.capacity() > 2 * (256 + 32)
            || plan["input"]["catalog_identity"]["catalog_id"].as_str()
                != Some(identity.catalog.as_str())
            || plan["input"]["catalog_identity"]["incarnation"].as_str()
                != Some(identity.incarnation.as_str())
            || plan["input"]["catalog_state"][0]["generation"].as_i64() != Some(identity.generation)
            || plan["input"]["file_identity"]["device"].as_u64() != Some(identity.device)
            || plan["input"]["file_identity"]["inode"].as_u64() != Some(identity.inode)
        {
            return Err(Error(
                "prepared offline identity differs from approved plan".into(),
            ));
        }
        self.allocation.check(Some(db), 0)?;
        check(deadline, cancel)?;
        #[cfg(test)]
        fault_at(fault, 13);
        #[cfg(not(test))]
        let _ = fault;
        self.state = PreparationState::Prepared;
        Ok(())
    }
    /// No queries, allocations, polling, reopening, or fallible validation.
    /// A structural mismatch returns the actual complete partial owner unchanged.
    pub(in crate::migration) fn finish(self) -> std::result::Result<NativePreparedBootstrap, Self> {
        let Self {
            db: Some(db),
            writer: Some(NativeWriterAcquisition::Locked(writer)),
            admission: Some(NativeAdmissionAcquisition::Locked(admission)),
            path: Some(path),
            plan: Some(plan),
            identity: Some(identity),
            allocation,
            state: PreparationState::Prepared,
            ..
        } = self
        else {
            return Err(self);
        };
        Ok(NativePreparedBootstrap {
            owner: RetainedNativeUpgrade {
                db,
                writer,
                admission,
                path,
                verified_backup: None,
                retained_preservation: None,
                allocation: Some(allocation),
            },
            plan,
            identity,
        })
    }
    #[cfg(test)]
    pub(in crate::migration) fn custody_for_test(
        &self,
    ) -> (Option<usize>, Option<i32>, Option<i32>) {
        (
            // SAFETY: inspect only the identity of the retained live connection;
            // no call through or dereference of its native pointer is performed.
            self.db.as_ref().map(|db| unsafe { db.handle() } as usize),
            self.writer
                .as_ref()
                .and_then(NativeWriterAcquisition::descriptor_for_test),
            self.admission
                .as_ref()
                .and_then(NativeAdmissionAcquisition::descriptor_for_test),
        )
    }
    #[cfg(test)]
    pub(in crate::migration) fn connection_for_test(&self) -> Option<&Connection> {
        self.db.as_ref()
    }
    #[cfg(test)]
    pub(in crate::migration) fn allocation_for_test(&self) -> allocation::AllocationContext {
        self.allocation.clone()
    }
    #[cfg(test)]
    pub(in crate::migration) fn reservation_and_input_for_test(
        &self,
    ) -> Result<(u64, usize, usize)> {
        Ok((
            self.allocation.external_retained_bytes()?,
            self.input.as_os_str().as_encoded_bytes().as_ptr() as usize,
            self.input.capacity(),
        ))
    }
    #[cfg(test)]
    pub(in crate::migration) fn plan_for_test(&self) -> Option<&Value> {
        self.plan.as_ref()
    }
}
