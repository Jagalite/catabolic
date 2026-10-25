//! Caller-owned initial native acquisition, separate from the legacy consuming
//! prepare API. The split corridor retains native locks before source verification.
use super::*;
use crate::Error;
use std::{fs::File, sync::atomic::AtomicBool, time::Instant};

const FAILED: &str = "offline native preparation did not finish successfully";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfflinePreparationPhase {
    Inputs,
    /// Both real native leases are retained; SQLite has not been opened.
    SourceLocked,
    Failed,
    Prepared,
}
/// Admit before scheduling any work. Every successfully returned lock File and
/// source Connection is installed inline before metadata/flock/path validation
/// or connection setup. Errors/unwinds retain the actual partial ownership and
/// any still-open transaction; they never promise rollback, retry, or a grant.
///
/// Creating lock sidecars is a filesystem effect. READ_WRITE SQLite setup/read
/// can recover a hot journal before a plan exists. Existing engine-internal
/// provisional FDs, failed SQLite opens, libc path-normalization internals, partial plan
/// buffers and the independent in-memory reference remain producer-internal;
/// this API does not claim to retain all of them or byte-immutable preparation.
/// Deadline/cancellation checks are checkpoints, not preemption inside SQLite,
/// filesystem calls, hashing or planning.
/// Safe path normalization has an explicit input/output scratch-cohort refusal;
/// its unknown current-directory/canonicalization internals are not hard peak
/// bounded, and checking returned capacity is not preadmission of that output.
/// Normalization finishes before any native File or Connection is acquired.
/// The caller must retain genuine host authority separately: quiescence flags
/// are assertions, not an owned grant.
/// Dropping this operation or its terminal failure explicitly closes custody.
/// No initial Motion96 adoption or activation is performed by this operation.
pub struct OfflinePreparationOperation {
    owner: native_session::NativePreparation,
    deadline: Instant,
    phase: OfflinePreparationPhase,
    failure: Error,
}
/// Opaque terminal partial custody. No raw connection, extraction, retry or
/// replacement authority is exposed. A failed BEGIN/ROLLBACK is not interpreted
/// as success; successful sidecar creation or engine recovery is not undone.
pub struct OfflinePreparationFailure {
    operation: OfflinePreparationOperation,
}
impl std::fmt::Debug for OfflinePreparationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.operation.failure.0)
    }
}
impl std::fmt::Display for OfflinePreparationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.operation.failure.0)
    }
}
impl std::error::Error for OfflinePreparationFailure {}
impl OfflinePreparationOperation {
    /// No native File/Connection is acquired here. The caller's absolute deadline
    /// is captured now, never recreated from a duration when work is polled later.
    /// This admission can fail without consuming any preexisting native/host owner.
    pub fn admit(
        path: &Path,
        quiescence: NativeUpgradeQuiescence,
        reservation: OfflineAllocationReservation,
        deadline: Instant,
    ) -> Result<Self> {
        Self::admit_inner(path, quiescence, reservation, None, deadline)
    }
    /// Original admission of a fixed, affine 64MiB control workspace. The
    /// reservation is the residual host retention EXCLUDING that workspace:
    /// native admission adds it exactly once and binds the backing to the same
    /// retained allocation context. Later refreshes still take the FULL host
    /// reservation, including the workspace, and cannot register or replace it.
    pub fn admit_with_control_workspace(
        path: &Path,
        quiescence: NativeUpgradeQuiescence,
        residual_reservation: OfflineAllocationReservation,
        backing: &mut OfflineControlWorkspaceBacking,
        deadline: Instant,
    ) -> Result<Self> {
        Self::admit_inner(path, quiescence, residual_reservation, Some(backing), deadline)
    }
    fn admit_inner(
        path: &Path,
        quiescence: NativeUpgradeQuiescence,
        reservation: OfflineAllocationReservation,
        backing: Option<&mut OfflineControlWorkspaceBacking>,
        deadline: Instant,
    ) -> Result<Self> {
        let storage = (std::mem::size_of::<Self>()
            + std::mem::size_of::<Result<Self>>()
            + std::mem::size_of::<OfflinePreparationFailure>()
            + std::mem::size_of::<
                std::result::Result<PreparedOfflineBootstrap, OfflinePreparationFailure>,
            >()
            + std::mem::size_of::<PreparedOfflineBootstrap>()) as u64;
        let storage = storage
            .checked_add((FAILED.len() as u64) * 2)
            .ok_or_else(|| Error("offline preparation storage overflow".into()))?;
        let owner = match backing {
            Some(backing) => native_session::NativePreparation::admit_with_control_workspace(
                path, quiescence, reservation, storage, backing,
            )?,
            None => native_session::NativePreparation::admit(path, quiescence, reservation, storage)?,
        };
        // The fallback buffer is charged before construction; it survives failure.
        let failure = Error(FAILED.into());
        if failure.0.capacity() > FAILED.len() * 2 {
            return Err(Error("offline preparation fallback storage differs".into()));
        }
        Ok(Self {
            owner,
            deadline,
            phase: OfflinePreparationPhase::Inputs,
            failure,
        })
    }
    pub fn phase(&self) -> OfflinePreparationPhase {
        self.phase
    }
    /// Raise the measured host reservation before acquisition, using the same
    /// admitted context, retained slots and scratch. No file/connection is
    /// acquired or path resolved here; the original deadline never changes.
    ///
    /// Only Inputs may refresh. An attempted refresh error/unwind is terminal
    /// and retains this entire owner; callers must not discard it to retry.
    /// A deadline expiring at the exit checkpoint can leave the higher host
    /// reservation recorded in the failed owner; no accounting rollback is
    /// implied. Rejection of an already Failed/Prepared owner leaves its phase
    /// and custody unchanged.
    pub fn refresh_external_reservation(
        &mut self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        if self.phase != OfflinePreparationPhase::Inputs {
            return Err(Error(
                "offline native reservation refresh requires Inputs".into(),
            ));
        }
        self.phase = OfflinePreparationPhase::Failed;
        if Instant::now() >= self.deadline {
            return Err(Error(
                "offline native preparation deadline exhausted".into(),
            ));
        }
        self.owner.refresh_external_reservation(reservation)?;
        if Instant::now() >= self.deadline {
            return Err(Error(
                "offline native preparation deadline exhausted".into(),
            ));
        }
        self.phase = OfflinePreparationPhase::Inputs;
        Ok(())
    }
    /// Acquire the existing native lifecycle and writer leases, then stop BEFORE
    /// opening SQLite. The same operation, allocation context, scratch, counters,
    /// and original absolute deadline remain retained through `prepare_locked`.
    /// No lock is released/reacquired and no source path is resolved again.
    ///
    /// This excludes cooperating Store/CLI writers, not hostile same-UID writes
    /// or namespace replacement. The caller must retain its genuine staged-file
    /// owner and verify historical bytes under these leases. If that external
    /// verification fails or unwinds, the caller must retain terminal custody;
    /// this operation cannot observe code executed outside its methods.
    pub fn acquire_source_locks(&mut self, cancel: &AtomicBool) -> Result<()> {
        self.acquire_source_locks_inner(cancel, 0)
    }
    fn acquire_source_locks_inner(&mut self, cancel: &AtomicBool, fault: u8) -> Result<()> {
        if self.phase != OfflinePreparationPhase::Inputs {
            return Err(Error("offline native preparation already attempted".into()));
        }
        self.phase = OfflinePreparationPhase::Failed;
        self.owner
            .acquire_source_locks(self.deadline, cancel, fault)?;
        self.phase = OfflinePreparationPhase::SourceLocked;
        Ok(())
    }
    /// Forward increased host retention after locked source verification and
    /// host preflight, immediately before the fixed SQLite continuation. This
    /// uses the same unbound allocation context and keeps both actual leases.
    /// It performs no SQL, embedding poll, scan, or fresh authority check.
    ///
    /// Only SourceLocked is accepted. Attempted decrease, aggregate refusal,
    /// expiry or unwind is terminal and preserves custody. Deadline checks are
    /// entry/exit checkpoints, not preemption; no deadline is restarted. A
    /// successful update observed expired at exit remains charged in Failed.
    pub fn refresh_source_locked_reservation(
        &mut self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        if self.phase != OfflinePreparationPhase::SourceLocked {
            return Err(Error(
                "offline native reservation refresh requires SourceLocked".into(),
            ));
        }
        self.phase = OfflinePreparationPhase::Failed;
        if Instant::now() >= self.deadline {
            return Err(Error(
                "offline native preparation deadline exhausted".into(),
            ));
        }
        self.owner.refresh_source_locked_reservation(reservation)?;
        if Instant::now() >= self.deadline {
            return Err(Error(
                "offline native preparation deadline exhausted".into(),
            ));
        }
        self.phase = OfflinePreparationPhase::SourceLocked;
        Ok(())
    }
    /// Bind the genuine retained source descriptor to the already normalized
    /// native path under both leases, then run the fixed SQLite continuation.
    /// The File stays borrowed for this entire call; no copied caller-supplied
    /// device/inode assertion can authorize it. This identity constraint is not
    /// historical byte verification and is not an atomic FD-backed SQLite VFS.
    ///
    /// Any error or unwind after this attempt begins is terminal and retains the
    /// exact owner, including both leases and any acquired SQLite connection.
    /// Legacy prepare/refresh cannot restart a SourceLocked or failed owner.
    pub fn prepare_locked(&mut self, source: &File, cancel: &AtomicBool) -> Result<()> {
        self.prepare_locked_inner(source, cancel, 0)
    }
    fn prepare_locked_inner(
        &mut self,
        source: &File,
        cancel: &AtomicBool,
        fault: u8,
    ) -> Result<()> {
        if self.phase != OfflinePreparationPhase::SourceLocked {
            return Err(Error(
                "offline native preparation requires source locks".into(),
            ));
        }
        self.phase = OfflinePreparationPhase::Failed;
        self.owner
            .prepare_locked(source, self.deadline, cancel, fault)?;
        self.phase = OfflinePreparationPhase::Prepared;
        Ok(())
    }
    pub fn prepare(&mut self, cancel: &AtomicBool) -> Result<()> {
        self.prepare_inner(cancel, 0)
    }
    fn prepare_inner(&mut self, cancel: &AtomicBool, fault: u8) -> Result<()> {
        if self.phase != OfflinePreparationPhase::Inputs {
            return Err(Error("offline native preparation already attempted".into()));
        }
        self.phase = OfflinePreparationPhase::Failed;
        self.owner.prepare(self.deadline, cancel, fault)?;
        self.phase = OfflinePreparationPhase::Prepared;
        Ok(())
    }
    /// Move-only handoff. No query, callback, allocation, deadline reset or reopen.
    pub fn finish(
        mut self,
    ) -> std::result::Result<PreparedOfflineBootstrap, OfflinePreparationFailure> {
        if self.phase != OfflinePreparationPhase::Prepared {
            self.phase = OfflinePreparationPhase::Failed;
            return Err(OfflinePreparationFailure { operation: self });
        }
        let Self {
            owner,
            deadline,
            failure,
            ..
        } = self;
        match owner.finish() {
            Ok(native_session::NativePreparedBootstrap {
                owner,
                plan,
                identity,
            }) => Ok(PreparedOfflineBootstrap {
                owner,
                plan,
                identity: PreparedIdentity {
                    catalog: identity.catalog,
                    incarnation: identity.incarnation,
                    generation: identity.generation,
                    device: identity.device,
                    inode: identity.inode,
                    producer_deadline: Some(deadline),
                },
                apply_admission: None,
            }),
            Err(owner) => Err(OfflinePreparationFailure {
                operation: Self {
                    owner,
                    deadline,
                    failure,
                    phase: OfflinePreparationPhase::Failed,
                },
            }),
        }
    }
}
#[cfg(test)]
#[path = "preparation_operation/tests.rs"]
mod tests;
