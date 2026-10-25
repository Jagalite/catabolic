//! Fixed single-attempt completion promotion. The caller keeps the original
//! session inline while all producer work borrows it; no reopen or recovery sink.
use super::*;
use crate::Error;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

const COMPLETION_FAILED: &str = "offline completion operation did not finish successfully";

pub(super) struct CompletionAdmission {
    storage: native_session::CatalogCompletionStorage,
    deadline: Instant,
    failure: Error,
}
enum CompletionState {
    Ready,
    Failed,
    Completed { source_count: i64, generation: i64 },
}

/// Original bootstrap custody remains owned here across errors and unwinds.
/// A failed attempt cannot be retried or extracted into a reusable session.
/// Cancelling a request does not drop this caller-owned object; explicitly
/// dropping the operation or its opaque failure closes its original custody.
pub struct OfflineCompletionOperation<'plan> {
    owner: OfflineBootstrap<'plan>,
    admission: CompletionAdmission,
    state: CompletionState,
}
/// Borrowed facts measured by this exact retained operation. There is no public
/// constructor, owner escape or fresh verification here. Keep the operation
/// alive through the host's ACK uncertainty and all fallible reacquisition;
/// these measured facts do not by themselves acknowledge a host commit.
pub struct OfflineCompletionFacts<'operation> {
    storage: &'operation native_session::CatalogCompletionStorage,
    source_count: i64,
    generation: i64,
}
impl OfflineCompletionFacts<'_> {
    pub fn command(&self) -> &CommandEnvelope {
        self.storage.command()
    }
    pub fn command_key(&self) -> &CommandKey {
        self.command().key()
    }
    pub fn catalog_id(&self) -> &str {
        self.command_key().catalog_id()
    }
    pub fn incarnation(&self) -> &str {
        self.command().admission_incarnation()
    }
    pub fn payload_digest(&self) -> &str {
        self.command().digest().as_str()
    }
    pub fn inventory_digest(&self) -> &str {
        self.storage.inventory_digest()
    }
    pub fn transition_digest(&self) -> &str {
        self.storage.transition_digest()
    }
    pub fn state_digest(&self) -> &str {
        self.storage.state_digest()
    }
    pub fn committed_generation(&self) -> i64 {
        self.generation
    }
    pub fn source_count(&self) -> i64 {
        self.source_count
    }
}
/// No admission was present, so no producer attempt began. Recovery of this
/// unchanged owner is allowed; this is distinct from an attempted failure.
pub struct OfflineCompletionPreparationFailure<'plan> {
    owner: OfflineBootstrap<'plan>,
}
impl<'plan> OfflineCompletionPreparationFailure<'plan> {
    pub fn into_bootstrap(self) -> OfflineBootstrap<'plan> {
        self.owner
    }
}
impl std::fmt::Debug for OfflineCompletionPreparationFailure<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("offline completion storage was not admitted")
    }
}

/// Opaque terminal custody, not a retry, Complete, source grant, or Store escape.
/// The original Store (including poison), preservation and plan remain retained
/// until the caller deliberately drops this failure.
pub struct OfflineCompletionFailure<'plan> {
    _owner: OfflineBootstrap<'plan>,
    admission: CompletionAdmission,
}
impl OfflineCompletionFailure<'_> {
    pub fn error(&self) -> &Error {
        &self.admission.failure
    }
}
impl std::fmt::Debug for OfflineCompletionFailure<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfflineCompletionFailure")
            .field("error", &self.error())
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for OfflineCompletionFailure<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.error(), f)
    }
}
impl std::error::Error for OfflineCompletionFailure<'_> {}

impl<'plan> OfflineBootstrap<'plan> {
    /// Storage-only admission while the original owner is borrowed. The exact
    /// command is copied once, and all SHA-256 metadata buffers are allocated
    /// before custody moves. Re-admission only tightens the original deadline.
    /// A retained apply's inherited deadline also bounds the first admission.
    pub fn admit_completion_operation(
        &mut self,
        command: &CommandEnvelope,
        deadline: Instant,
    ) -> Result<()> {
        let deadline = self
            .producer_deadline
            .map_or(deadline, |old| old.min(deadline));
        if let Some(admission) = self.completion_admission.as_mut() {
            if !admission.storage.matches(command) {
                return Err(Error("offline completion admitted command differs".into()));
            }
            admission.deadline = admission.deadline.min(deadline);
            return Ok(());
        }
        let bytes = [
            std::mem::size_of::<OfflineCompletionOperation<'plan>>(),
            std::mem::size_of::<
                std::result::Result<
                    OfflineCompletionOperation<'plan>,
                    OfflineCompletionPreparationFailure<'plan>,
                >,
            >(),
            std::mem::size_of::<
                std::result::Result<CompletedCatalogSession, OfflineCompletionFailure<'plan>>,
            >(),
            std::mem::size_of::<Result<(i64, i64)>>(),
            std::mem::size_of::<Result<native_session::CatalogCompletionStorage>>(),
            std::mem::size_of::<Result<()>>(),
            std::mem::size_of::<Option<OfflineCompletionFacts<'_>>>(),
            COMPLETION_FAILED.len(),
        ]
        .into_iter()
        .try_fold(0usize, |total, bytes| total.checked_add(bytes))
        .ok_or_else(|| Error("offline completion slot storage overflow".into()))?;
        let storage = self.session.admit_catalog_completion_storage(
            command,
            u64::try_from(bytes).map_err(|_| Error("offline completion slot overflow".into()))?,
        )?;
        self.completion_admission = Some(CompletionAdmission {
            storage,
            deadline,
            failure: Error(COMPLETION_FAILED.to_owned()),
        });
        Ok(())
    }

    /// Moves only: no polling, query, callback, allocation or verification.
    pub fn begin_completion(
        mut self,
    ) -> std::result::Result<
        OfflineCompletionOperation<'plan>,
        OfflineCompletionPreparationFailure<'plan>,
    > {
        let Some(admission) = self.completion_admission.take() else {
            return Err(OfflineCompletionPreparationFailure { owner: self });
        };
        Ok(OfflineCompletionOperation {
            owner: self,
            admission,
            state: CompletionState::Ready,
        })
    }

    pub(super) fn require_unadmitted_completion(&self) -> Result<()> {
        if self.completion_admission.is_some() || self.producer_deadline.is_some() {
            return Err(Error("use admitted offline completion operation".into()));
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn completion_admitted_deadline_for_test(&self) -> Option<Instant> {
        self.completion_admission
            .as_ref()
            .map(|admission| admission.deadline)
    }
}

impl<'plan> OfflineCompletionOperation<'plan> {
    /// Fixed accounting on the original allocation context and live connection,
    /// before or after successful measurement. This only samples SQLite
    /// DBSTATUS and monotonically refreshes the host reservation; no SQL,
    /// polling, verification, replacement budget or renewed deadline is used.
    /// Any refusal, expiry or unwind terminalizes the operation and its facts,
    /// retaining the original owner. Already failed operations cannot revive.
    pub fn refresh_external_reservation(
        &mut self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        if matches!(self.state, CompletionState::Failed) {
            return Err(Error(COMPLETION_FAILED.into()));
        }
        let prior = std::mem::replace(&mut self.state, CompletionState::Failed);
        if Instant::now() >= self.admission.deadline {
            return Err(Error("offline completion deadline exhausted".into()));
        }
        self.owner
            .session
            .refresh_external_reservation(reservation)?;
        if Instant::now() >= self.admission.deadline {
            return Err(Error("offline completion deadline exhausted".into()));
        }
        self.state = prior;
        Ok(())
    }
    /// Allocation-free view of the already measured facts, only after a
    /// successful complete(). No SQL, polling, deadline reset or silent rescan.
    /// Retain this operation until all fallible host ACK work has settled; only
    /// then use its move-only finish. This is not renewed temporal authority.
    pub fn measured_facts(&self) -> Option<OfflineCompletionFacts<'_>> {
        match &self.state {
            CompletionState::Completed {
                source_count,
                generation,
            } => Some(OfflineCompletionFacts {
                storage: &self.admission.storage,
                source_count: *source_count,
                generation: *generation,
            }),
            CompletionState::Ready | CompletionState::Failed => None,
        }
    }
    /// Exactly one borrowed Complete measurement. The inherited host deadline
    /// and cancellation are checked before and after it. These are checkpoints,
    /// not a new interrupt/deadline view inside existing Store verification;
    /// its existing ambient/default OperationBudget selection remains unchanged.
    pub fn complete(&mut self, cancel: &AtomicBool) -> Result<()> {
        self.complete_inner(cancel, 0)
    }

    pub fn complete_with_control_workspace(
        &mut self, cancel: &AtomicBool, backing: &mut OfflineControlWorkspaceBacking,
    ) -> std::result::Result<OfflineRetainedResult<()>, OfflineWorkspaceAdmissionRefused> {
        self.complete_with_control_workspace_inner(cancel, backing, 0)
    }
    fn complete_with_control_workspace_inner(
        &mut self, cancel: &AtomicBool, backing: &mut OfflineControlWorkspaceBacking, fault: u8,
    ) -> std::result::Result<OfflineRetainedResult<()>, OfflineWorkspaceAdmissionRefused> {
        // Claim before any fallible admission. Even a fixed admission refusal
        // cannot leave a reusable Ready completion or publish measured facts.
        if !matches!(self.state, CompletionState::Ready) {
            return Err(OfflineWorkspaceAdmissionRefused);
        }
        self.state = CompletionState::Failed;
        let output = self.owner.session.admit_control_result::<(), Error>()?;
        let control = self.owner.session.control_workspace(backing)?;
        let result = Self::complete_claimed(&self.owner, &mut self.admission, &mut self.state, cancel, fault, Some(&control));
        let retained = output.finish(result);
        drop(control);
        Ok(retained)
    }
    fn complete_inner(&mut self, cancel: &AtomicBool, fault: u8) -> Result<()> {
        if !matches!(self.state, CompletionState::Ready) {
            return Err(Error("offline completion operation already attempted".into()));
        }
        self.state = CompletionState::Failed;
        Self::complete_claimed(&self.owner, &mut self.admission, &mut self.state, cancel, fault, None)
    }
    fn complete_claimed(
        owner: &OfflineBootstrap<'_>, admission: &mut CompletionAdmission,
        state: &mut CompletionState, cancel: &AtomicBool, fault: u8,
        control: Option<&crate::migration::BootstrapWorkspace<'_>>,
    ) -> Result<()> {
        Self::check_host_deadline(admission.deadline, cancel)?;
        #[cfg(test)]
        if fault == 1 {
            panic!("controlled pre-measure completion unwind");
        }
        #[cfg(not(test))]
        let _ = fault;
        let (source_count, generation) = match control {
            Some(control) => owner.session.measure_catalog_completion_with_control_workspace(&mut admission.storage, Some(control))?,
            None => owner.session.measure_catalog_completion(&mut admission.storage)?,
        };
        #[cfg(test)]
        if fault == 2 {
            panic!("controlled post-measure completion unwind");
        }
        #[cfg(test)]
        if fault == 3 {
            cancel.store(true, Ordering::Release);
        }
        Self::check_host_deadline(admission.deadline, cancel)?;
        *state = CompletionState::Completed {
            source_count,
            generation,
        };
        Ok(())
    }

    fn check_host(&self, cancel: &AtomicBool) -> Result<()> {
        Self::check_host_deadline(self.admission.deadline, cancel)
    }
    fn check_host_deadline(deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(Error(
                "offline completion deadline exhausted or cancelled".into(),
            ));
        }
        Ok(())
    }

    /// Moves only into the already admitted public result slot. A failed or
    /// unattempted operation yields opaque original custody, never a new grant.
    pub fn finish(
        self,
    ) -> std::result::Result<CompletedCatalogSession, OfflineCompletionFailure<'plan>> {
        let Self {
            owner,
            admission,
            state,
        } = self;
        match state {
            CompletionState::Completed {
                source_count,
                generation,
            } => Ok(owner.session.finish_catalog_completion(
                admission.storage,
                source_count,
                generation,
            )),
            CompletionState::Ready | CompletionState::Failed => Err(OfflineCompletionFailure {
                _owner: owner,
                admission,
            }),
        }
    }
}

#[cfg(test)]
#[path = "completion_operation/tests.rs"]
mod tests;

#[cfg(test)]
mod control_workspace_tests {
    use super::*;
    use std::time::Duration;

    fn committed_with_control() -> (tempfile::TempDir, std::path::PathBuf, OfflineCompletionOperation<'static>, OfflineControlWorkspaceBacking) {
        let (root, path, mut owner, backing) = super::super::bootstrap_workspace_tests::migrated_with_control();
        let host = "controlled-completion-faults";
        let payload = owner.prepare(host, &[]).unwrap();
        let command = CommandEnvelope::new(1,
            CommandKey::new(owner.catalog_id(), host, "bootstrap").unwrap(),
            owner.incarnation(), owner.generation().unwrap(), &payload.to_string()).unwrap();
        owner.execute(&command, host, &[], ReceiptPolicy {
            retention: Duration::from_secs(3600), max_receipts: 100,
            max_hold_records: 100, max_logical_bytes: 16 * 1024 * 1024,
        }).unwrap();
        owner.admit_completion_operation(&command, Instant::now() + Duration::from_secs(60)).unwrap();
        (root, path, owner.begin_completion().unwrap(), backing)
    }
    #[test]
    fn controlled_completion_cancellation_retains_error_after_terminal_owner_close() {
        let (_root, path, mut operation, mut backing) = committed_with_control();
        let (context, _) = operation.owner.session.apply_conversion_allocation_for_test();
        let retained = operation.complete_with_control_workspace(&AtomicBool::new(true), &mut backing).unwrap();
        assert!(retained.result().is_err());
        assert!(operation.measured_facts().is_none());
        assert!(matches!(operation.state, CompletionState::Failed));
        assert!(operation.complete_with_control_workspace(&AtomicBool::new(false), &mut backing).is_err());
        assert!(crate::writer_lock(&path).is_err());
        assert!(!backing.state_for_test().1);
        let failure = operation.finish().err().expect("cancelled completion must retain custody");
        drop(failure);
        drop(crate::writer_lock(&path).unwrap());
        assert!(retained.result().as_ref().unwrap_err().0.contains("cancelled"));
        assert!(context.temporary_bytes_for_test().unwrap() >= 128 * 1024);
        drop(retained);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
    }
    #[test]
    fn controlled_completion_faults_keep_terminal_custody_and_release_workspace_on_unwind() {
        for fault in [1, 2, 3] {
            let (_root, path, mut operation, mut backing) = committed_with_control();
            let (context, _) = operation.owner.session.apply_conversion_allocation_for_test();
            let baseline = context.temporary_bytes_for_test().unwrap();
            let cancel = AtomicBool::new(false);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                operation.complete_with_control_workspace_inner(&cancel, &mut backing, fault)
            }));
            if fault == 3 {
                let retained = result.unwrap().unwrap();
                assert!(retained.result().is_err());
                drop(retained);
            } else {
                assert!(result.is_err());
            }
            assert!(matches!(operation.state, CompletionState::Failed));
            assert!(operation.measured_facts().is_none());
            assert_eq!(context.temporary_bytes_for_test().unwrap(), baseline);
            assert_eq!((backing.state_for_test().1, backing.state_for_test().2), (false, false));
            assert!(crate::writer_lock(&path).is_err());
            assert!(operation.complete_with_control_workspace(&cancel, &mut backing).is_err());
            let failure = operation.finish().err().expect("fault must retain failed owner");
            drop(failure);
            assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
        }
    }
}
