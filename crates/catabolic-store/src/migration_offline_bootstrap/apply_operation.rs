//! Single-attempt borrowed migration and bootstrap conversion. The original
//! connection/leases/preservation stay caller-owned through errors and unwinds.
use super::*;
use crate::Error;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

const APPLY_FAILED: &str = "offline apply operation did not finish successfully";
// A returned dynamic Error can outlive the control scratch loan. It has its own
// additive reservation, distinct from the retained terminal failure message.
const ERROR_OUTPUT_STORAGE: usize = 64 * 1024;
const ERROR_OUTPUT_FAILED: &str = "offline apply error exceeded reserved output storage";
const ERROR_OUTPUT_FALLBACK_STORAGE: usize = ERROR_OUTPUT_FAILED.len() * 2;
pub(super) struct ApplyAdmission {
    backup_parent: Option<PathBuf>,
    deadline: Instant,
    failure: Error,
    // Constructed before any loan; moving the fallback on failure allocates
    // nothing. Its capacity is separate from the dynamic error's 64KiB share.
    output_failure: Option<Error>,
    _storage: native_session::NativeApplyStorage,
}
/// Minted only by the fixed controlled call below. Borrowing this witness keeps
/// the actual independently admitted64KiB output share and preallocated fallback
/// alive through the reduced copy phase; the caller still bounds the result
/// under its outer control loan. No scalar/control-brand constructor exists.
pub(in crate::migration) struct AdmittedControlApplyOutput<'a> {
    admission: &'a ApplyAdmission,
}
impl AdmittedControlApplyOutput<'_> {
    pub(in crate::migration) fn require_owner(&self, owner: &native_session::RetainedNativeUpgrade) -> Result<()> {
        if self.admission.output_failure.is_none() {
            return Err(Error("offline apply output fallback unavailable".into()));
        }
        self.admission._storage.require_owner(owner)
    }
}
enum ApplyState {
    Ready,
    Failed,
    Applied(native_session::BootstrapConversion),
}
/// Caller-owned stable custody. All producer work borrows this object; dropping
/// the request future alone need not drop it. An attempted failure cannot retry,
/// extract a reusable owner, publish a Store, or assert that rollback succeeded.
/// Completed preservation/verified-backup owners install inline. Existing
/// synchronous capture/backup producers still own provisional descriptors and
/// buffers locally and may drop them on failure/unwind. Backup publication
/// precedes live migration mutation; a failed attempt can leave backup files.
pub struct OfflineApplyOperation {
    owner: PreparedOfflineBootstrap,
    admission: ApplyAdmission,
    state: ApplyState,
}
/// No producer work started. Only a missing admission permits recovering the
/// unchanged prepared owner; this is not an attempted-failure recovery API.
pub struct OfflineApplyPreparationFailure {
    owner: PreparedOfflineBootstrap,
}
impl OfflineApplyPreparationFailure {
    pub fn into_prepared(self) -> PreparedOfflineBootstrap {
        self.owner
    }
}
impl std::fmt::Debug for OfflineApplyPreparationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("offline apply storage was not admitted")
    }
}
/// Opaque terminal custody retains the original connection, both leases,
/// approved plan, and completed acquired preservation/verified-backup owners
/// until explicitly dropped. Provisional producer-local descriptors/buffers
/// are not retained; backup publication precedes live migration mutation.
/// A postcommit failure may contain durable schema41, never fabricated schema40.
pub struct OfflineApplyFailure {
    _owner: PreparedOfflineBootstrap,
    admission: ApplyAdmission,
}
impl OfflineApplyFailure {
    pub fn error(&self) -> &Error {
        &self.admission.failure
    }
}
impl std::fmt::Debug for OfflineApplyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfflineApplyFailure")
            .field("error", &self.error())
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for OfflineApplyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.error(), f)
    }
}
impl std::error::Error for OfflineApplyFailure {}

impl PreparedOfflineBootstrap {
    /// Storage-only admission while the original owner is borrowed. The backup
    /// directory is fixed once; repeating admission can only tighten deadline.
    /// Pass the inherited absolute host deadline before queueing work, not a new
    /// duration. This neither acquires initial custody nor starts Store work.
    pub fn admit_apply_operation(
        &mut self,
        backup_parent: Option<&Path>,
        deadline: Instant,
    ) -> Result<()> {
        let deadline = self
            .identity
            .producer_deadline
            .map_or(deadline, |old| old.min(deadline));
        if let Some(admission) = self.apply_admission.as_mut() {
            if admission.backup_parent.as_deref() != backup_parent {
                return Err(Error(
                    "offline apply admitted backup directory differs".into(),
                ));
            }
            admission.deadline = admission.deadline.min(deadline);
            return Ok(());
        }
        let path_bytes = backup_parent.map_or(0, |path| path.as_os_str().len());
        let bytes =
            [
                std::mem::size_of::<OfflineApplyOperation>(),
                std::mem::size_of::<
                    std::result::Result<OfflineApplyOperation, OfflineApplyPreparationFailure>,
                >(),
                std::mem::size_of::<
                    std::result::Result<OfflineBootstrap<'static>, OfflineApplyFailure>,
                >(),
                std::mem::size_of::<Result<Value>>(),
                std::mem::size_of::<Result<native_session::NativeApplyStorage>>(),
                std::mem::size_of::<Result<()>>(),
                APPLY_FAILED.len(),
                ERROR_OUTPUT_STORAGE,
                ERROR_OUTPUT_FALLBACK_STORAGE,
                path_bytes,
                path_bytes,
            ]
            .into_iter()
            .try_fold(0usize, |total, bytes| total.checked_add(bytes))
            .ok_or_else(|| Error("offline apply slot storage overflow".into()))?;
        let storage = self.owner.admit_apply_storage(
            u64::try_from(bytes).map_err(|_| Error("offline apply slot overflow".into()))?,
        )?;
        let output_failure = Error(ERROR_OUTPUT_FAILED.to_owned());
        if output_failure.0.capacity() > ERROR_OUTPUT_FALLBACK_STORAGE {
            return Err(Error("offline apply output fallback capacity differs".into()));
        }
        self.apply_admission = Some(ApplyAdmission {
            backup_parent: backup_parent.map(Path::to_path_buf),
            deadline,
            failure: Error(APPLY_FAILED.to_owned()),
            output_failure: Some(output_failure),
            _storage: storage,
        });
        Ok(())
    }
    /// Synchronous move-only handoff: no query, callback, allocation or polling.
    pub fn begin_apply(
        mut self,
    ) -> std::result::Result<OfflineApplyOperation, OfflineApplyPreparationFailure> {
        let Some(admission) = self.apply_admission.take() else {
            return Err(OfflineApplyPreparationFailure { owner: self });
        };
        Ok(OfflineApplyOperation {
            owner: self,
            admission,
            state: ApplyState::Ready,
        })
    }
}

fn check_host(deadline: Instant, cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
        return Err(Error(
            "offline apply deadline exhausted or cancelled".into(),
        ));
    }
    Ok(())
}
impl OfflineApplyOperation {
    /// Exactly one migration plus native-to-bootstrap validation, all borrowing
    /// the actual retained owner. Deadline/cancellation checks augment existing
    /// native checkpoints; no generic Store budget or counter is replaced.
    /// Raw SQL, copying and preservation verification are not preempted inside
    /// their existing calls; refusal occurs at the next fixed checkpoint.
    pub fn apply(&mut self, cancel: &AtomicBool) -> Result<()> {
        self.apply_inner(cancel, 0)
    }
    /// Borrow the original fixed control backing for this one apply call. The
    /// caller must keep its actual control owner exclusively borrowed, quiescent,
    /// and independently qualify its complete live retained capacity (including
    /// spare containers) at or below RETAINED_LIMIT. Native does not inspect or
    /// certify host storage. The original 64MiB accounts for two fixed 32MiB
    /// portions: retained host control and one sequential native metadata phase.
    /// Unbound/foreign backing fails. Hosts above the cohort must use ordinary
    /// apply, which retains its full, independent 32MiB metadata reservation.
    /// Returned errors retain their original text when capacity fits the
    /// independently admitted 64KiB output share. Larger errors are discarded
    /// before releasing the loan and replaced by a preallocated fixed fallback.
    pub fn apply_with_control_workspace(
        &mut self,
        cancel: &AtomicBool,
        backing: &mut OfflineControlWorkspaceBacking,
    ) -> Result<()> {
        self.apply_with_control_workspace_inner(cancel, backing, 0)
    }
    fn apply_with_control_workspace_inner(
        &mut self,
        cancel: &AtomicBool,
        backing: &mut OfflineControlWorkspaceBacking,
        fault: u8,
    ) -> Result<()> {
        if let Err(error) = self.begin_attempt() {
            return Err(self.bound_control_error_output(error));
        }
        let mut control = match self.owner.owner.control_workspace_loan(backing) {
            Ok(control) => control,
            Err(error) => return Err(self.bound_control_error_output(error)),
        };
        let output = AdmittedControlApplyOutput { admission: &self.admission };
        let result = Self::apply_attempt(
            &mut self.owner, &self.admission, &mut self.state, cancel, fault,
            Some((&mut control, &output)),
        );
        drop(output);
        // Bound or discard the original allocation BEFORE releasing the loan.
        // The retained admission's independent output reservation survives in
        // this operation and its opaque terminal failure, not in scratch.
        let result = result.map_err(|error| self.bound_control_error_output(error));
        drop(control);
        result
    }
    #[cfg(test)]
    pub(in crate::migration) fn apply_lifetime_owner_for_test(&self) -> &native_session::RetainedNativeUpgrade {
        &self.owner.owner
    }
    #[cfg(test)]
    pub(in crate::migration) fn apply_lifetime_require_output_owner_for_test(
        &self, owner: &native_session::RetainedNativeUpgrade,
    ) -> Result<()> {
        AdmittedControlApplyOutput { admission: &self.admission }.require_owner(owner)
    }
    #[cfg(test)]
    pub(in crate::migration) fn apply_lifetime_control_for_test(
        &mut self, cancel: &AtomicBool, backing: &mut OfflineControlWorkspaceBacking, fault: u8,
    ) -> Result<()> {
        self.apply_with_control_workspace_inner(cancel, backing, fault)
    }
    fn bound_control_error_output(&mut self, error: Error) -> Error {
        if error.0.capacity() <= ERROR_OUTPUT_STORAGE {
            return error;
        }
        // Do not format, clone, truncate, or allocate in custody-only failure.
        // The original oversized buffer dies while the caller holds its loan.
        drop(error);
        // Exhaustion is defensive only: a failed operation cannot attempt work
        // again. Even that case returns an allocation-free bounded empty error.
        self.admission.output_failure.take().unwrap_or(Error(String::new()))
    }
    fn apply_inner(&mut self, cancel: &AtomicBool, fault: u8) -> Result<()> {
        self.begin_attempt()?;
        Self::apply_attempt(&mut self.owner, &self.admission, &mut self.state, cancel, fault, None)
    }
    fn begin_attempt(&mut self) -> Result<()> {
        if !matches!(self.state, ApplyState::Ready) {
            return Err(Error("offline apply operation already attempted".into()));
        }
        // Terminal before any fallible call, checkpoint, query or test panic.
        self.state = ApplyState::Failed;
        Ok(())
    }
    fn apply_attempt(
        owner: &mut PreparedOfflineBootstrap,
        admission: &ApplyAdmission,
        state: &mut ApplyState,
        cancel: &AtomicBool,
        fault: u8,
        mut control: Option<(&mut native_session::allocation::ControlWorkspaceLoan<'_>, &AdmittedControlApplyOutput<'_>)>,
    ) -> Result<()> {
        let deadline = admission.deadline;
        check_host(deadline, cancel)?;
        #[cfg(test)]
        if fault == 1 {
            panic!("controlled pre-apply unwind");
        }
        let checkpoint = |stage: &str, _: &rusqlite::Connection, _: u32| {
            #[cfg(test)]
            {
                if (fault == 2 && stage == "upgrade:before_commit")
                    || (fault == 3 && stage == "upgrade:after_commit")
                    || (fault == 11 && stage == "upgrade:before_backup")
                {
                    panic!("controlled native apply checkpoint unwind");
                }
                if fault == 15 && stage == "rehearsal:before_commit" {
                    return Err(Error("exact original image failure".into()));
                }
                if fault == 16 && stage == "rehearsal:before_commit" {
                    panic!("controlled reduced copy phase unwind");
                }
                if fault == 17 && stage == "rehearsal:before_commit" {
                    let mut message = String::with_capacity(ERROR_OUTPUT_STORAGE + 1024);
                    message.push_str("controlled oversized image failure");
                    return Err(Error(message));
                }
                if fault == 14 && stage == "upgrade:before_backup" {
                    let mut message = String::with_capacity(ERROR_OUTPUT_STORAGE + 1024);
                    message.push_str("controlled oversized native apply error");
                    return Err(Error(message));
                }
                if fault == 4 && stage == "upgrade:before_commit" {
                    return Err(Error("controlled native precommit failure".into()));
                }
                if (fault == 5 && stage == "upgrade:after_commit")
                    || (fault == 12 && stage == "upgrade:commit_ready")
                {
                    cancel.store(true, Ordering::Release);
                }
            }
            #[cfg(not(test))]
            let _ = stage;
            check_host(deadline, cancel)
        };
        match control.as_mut() {
            Some((control, output)) => owner.owner.apply_closed41_with_admitted_control_output(
                &owner.plan, admission.backup_parent.as_deref(), &checkpoint, control, output,
            )?,
            None => owner.owner.apply_closed41_borrowed(
                &owner.plan, admission.backup_parent.as_deref(), &checkpoint,
            )?,
        };
        check_host(deadline, cancel)?;
        #[cfg(test)]
        let conversion = {
            let conversion_fault = match fault {
                6 => 1,
                7 => 2,
                8 => 3,
                9 => 4,
                13 => 5,
                _ => 0,
            };
            match control.as_mut().map(|(control, _)| &mut **control) {
                Some(control) => owner.owner.prepare_bootstrap_conversion_with_control_workspace(
                    conversion_fault, Some(control),
                )?,
                None => owner.owner.prepare_bootstrap_conversion_inner(conversion_fault)?,
            }
        };
        #[cfg(not(test))]
        let conversion = {
            let _ = fault;
            match control.as_mut().map(|(control, _)| &mut **control) {
                Some(control) => owner.owner.prepare_bootstrap_conversion_with_control_workspace(0, Some(control))?,
                None => owner.owner.prepare_bootstrap_conversion()?,
            }
        };
        #[cfg(test)]
        if fault == 10 {
            cancel.store(true, Ordering::Release);
        }
        check_host(deadline, cancel)?;
        *state = ApplyState::Applied(conversion);
        Ok(())
    }
    /// Synchronous fixed handoff. Failure keeps real original custody with no
    /// retry or grant; success moves it into OfflineBootstrap without any query,
    /// validation, allocation, callback, deadline reset or reopen.
    pub fn finish(self) -> std::result::Result<OfflineBootstrap<'static>, OfflineApplyFailure> {
        let Self {
            owner,
            admission,
            state,
        } = self;
        let ApplyState::Applied(conversion) = state else {
            return Err(OfflineApplyFailure {
                _owner: owner,
                admission,
            });
        };
        let PreparedOfflineBootstrap {
            owner,
            plan,
            identity,
            apply_admission,
        } = owner;
        match conversion.finish(owner) {
            Ok(session) => Ok(OfflineBootstrap {
                session,
                plan: Cow::Owned(plan),
                completion_admission: None,
                producer_deadline: Some(admission.deadline),
                #[cfg(test)]
                producer_exit_test: None,
            }),
            Err(owner) => Err(OfflineApplyFailure {
                _owner: PreparedOfflineBootstrap {
                    owner,
                    plan,
                    identity,
                    apply_admission,
                },
                admission,
            }),
        }
    }
}

#[cfg(test)]
#[path = "apply_operation/tests.rs"]
mod tests;
