//! Same-owner post-succession image capture. Historical bootstrap completion is
//! not replayed as a claim that the original pristine40 delta is still current.
use super::*;
use crate::catalog_backup::inventory::Audit;
use crate::catalog_restore_lineage::completed_image::{self, ValidatedCompletedImage};
use crate::catalog_source_succession::CapturedSourceHistory;
use std::time::{Duration, Instant};

pub(crate) struct CompletedCurrentImageAdmission<'a, 'workspace> {
    owner: &'a CompletedCatalogSession,
    control: Option<&'a BootstrapWorkspace<'workspace>>,
}
impl CompletedCurrentImageAdmission<'_, '_> {
    pub(crate) fn check(&self, store: &Store) -> Result<()> {
        if !std::ptr::eq(store, &self.owner.session.store) {
            return Err(Error("current image names another retained owner".into()));
        }
        self.owner.validate_current_with_control_workspace(self.control)
    }
}

/// Private immutable evidence for the fixed image copier, never a reopen grant.
pub(crate) struct CurrentImageProof<'a, 'workspace> {
    owner: &'a CompletedCatalogSession,
    generation: i64,
    inventory: Option<CurrentImageInventory>,
    history: CapturedSourceHistory,
    // Payloads precede their independently paid output guards.
    _history_retention: allocation::AllocationGuard,
    control: Option<&'a BootstrapWorkspace<'workspace>>,
}
impl<'a, 'workspace> CurrentImageProof<'a, 'workspace> {
    pub(crate) fn seal_image_retention(&self, guard: &mut OwnedImageAllocation, retained: u64) -> Result<()> {
        guard.seal_current_capture(&self.owner.session.store.db, retained)
    }
    pub(crate) fn commit_image_retention(&mut self, guard: &mut OwnedImageAllocation, retained: u64) -> Result<()> {
        guard.commit_current_capture(&self.owner.session.store.db, &mut self._history_retention, retained)
    }
    pub(crate) fn take_inventory(&mut self) -> Result<CurrentImageInventory> {
        self.inventory.take().ok_or_else(|| Error("current image inventory already moved".into()))
    }
    pub(crate) fn retain_failed_copy(&self, failed: completed_image::FailedOwnedCapture) {
        *self.owner.failed_capture.borrow_mut() = Some(failed);
    }
    pub(crate) fn control_workspace(&self) -> Option<&'a BootstrapWorkspace<'workspace>> { self.control }
    pub(crate) fn catalog_id(&self) -> &str {
        &self.owner.session.store.database_id
    }
    pub(crate) fn incarnation(&self) -> &str {
        // The actual admitted identity was checked in the writer before minting.
        self.owner
            .session
            .store
            .catalog_incarnation
            .as_deref()
            .unwrap_or("")
    }
    pub(crate) fn transition_digest(&self) -> &str {
        &self.owner.transition_digest
    }
    pub(crate) fn image_allocation(&self) -> Result<OfflineImageAllocation<'a>> {
        self.owner.session.image_allocation()
    }
    #[cfg(test)]
    pub(crate) fn verifier_retirement_pressure_for_test(&self) -> Result<allocation::AllocationGuard> {
        let store = &self.owner.session.store;
        let context = &store.offline_bootstrap.as_ref()
            .ok_or_else(|| Error("current image allocation owner missing".into()))?.allocation;
        let used = context.retained_native_bytes(&store.db)?
            .checked_add(context.external_retained_bytes()?)
            .ok_or_else(|| Error("retirement test pressure overflow".into()))?;
        context.reserve(&store.db, allocation::LIMIT.checked_sub(used)
            .ok_or_else(|| Error("retirement test pressure unavailable".into()))?)
    }
    pub(crate) fn checkpoint_source_allocation(&self, store: &Store) -> Result<()> {
        if !std::ptr::eq(store, &self.owner.session.store) {
            return Err(Error("current image source owner differs".into()));
        }
        store.offline_bootstrap.as_ref()
            .ok_or_else(|| Error("current image allocation owner missing".into()))?
            .allocation.check(Some(&store.db), 0)
    }
    pub(crate) fn validate_source(&self, store: &Store) -> Result<()> {
        if !std::ptr::eq(store, &self.owner.session.store) {
            return Err(Error("current image source owner differs".into()));
        }
        store.validate_native_identity_with_control_workspace(self.control)?;
        match self.control {
            Some(control) => control.validate_current_image_source(self),
            None => self.validate_closed_copy(&store.db),
        }
    }
    pub(crate) fn validate_closed_copy(&self, db: &Connection) -> Result<()> {
        closed41::Closed41Target.validate(db)?;
        let identity: (String, String, i64) = db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let row = transition::read_in(db)?
            .ok_or_else(|| Error("current image transition missing".into()))?;
        if identity
            != (
                self.catalog_id().to_owned(),
                self.incarnation().to_owned(),
                self.generation,
            )
            || row.digest()? != self.transition_digest()
        {
            return Err(Error(
                "current image identity/generation/transition changed".into(),
            ));
        }
        Ok(())
    }
    pub(crate) fn validate_inventory(&self, inventory: &Audit) -> Result<()> {
        if self.inventory.as_ref().is_none_or(|held| inventory != &held.audit) {
            return Err(Error(
                "current image complete inventory changed after proof".into(),
            ));
        }
        Ok(())
    }
}

/// Separate immutable copied image and complete authenticated native effects.
/// The original session remains locked until the copy and its proofs are dropped.
/// Only fixed isolated-copy rotation is available; no installation, general
/// dispatch or restored mutable admission is exposed.
pub struct CurrentCatalogImage {
    rotation_admission: Option<rotation::RotationStorageAdmission>,
    image: ValidatedCompletedImage,
    history: CapturedSourceHistory,
    generation: i64,
    owner: CompletedCatalogSession,
}
/// A failed copy retains the only admitted post94 session for retry/quarantine.
/// Dropping this value explicitly releases that custody; no effects are undone.
pub struct CurrentImageCaptureFailure {
    error: Error,
    owner: CompletedCatalogSession,
}
impl CurrentImageCaptureFailure {
    pub fn error(&self) -> &Error {
        &self.error
    }
    pub fn into_session(self) -> CompletedCatalogSession {
        self.owner
    }
}
impl std::fmt::Debug for CurrentImageCaptureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CurrentImageCaptureFailure")
            .field("error", &self.error.to_string())
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for CurrentImageCaptureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}
impl std::error::Error for CurrentImageCaptureFailure {}

const CAPTURE_FAILED: &str = "current image capture did not finish successfully";
pub(super) struct CaptureStorageAdmission {
    deadline: Instant,
    failure: Error,
    _slots: allocation::AllocationGuard,
}
enum CaptureState {
    Ready,
    WorkspaceClaimed,
    Failed,
    Captured(ValidatedCompletedImage, CapturedSourceHistory, i64),
}
/// Caller-owned original custody. All work borrows this stable operation;
/// dropping a request borrow does not drop its original Completed owner.
pub struct CurrentImageCaptureOperation {
    // Captured payloads and their embedded guards are destroyed before slots
    // and, last, the original Store and its writer/lifecycle locks.
    state: CaptureState,
    workspace_result: Option<control_workspace::RetainedAdmission<(), Error>>,
    admission: CaptureStorageAdmission,
    owner: CompletedCatalogSession,
}
/// One lexical original-backing loan, not a scalar discount or a reopen grant.
/// It borrows the backing only, so an enclosing owner need not self-reference.
/// Result admission is consumed with this token, including abandonment.
pub struct CurrentImageCaptureWorkspace<'a> {
    result: control_workspace::RetainedAdmission<(), Error>,
    loan: allocation::ControlWorkspaceLoan<'a>,
}
pub struct CurrentImageCapturePreparationFailure {
    owner: CompletedCatalogSession,
}
impl CurrentImageCapturePreparationFailure {
    pub fn into_session(self) -> CompletedCatalogSession {
        self.owner
    }
}
impl std::fmt::Debug for CurrentImageCapturePreparationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("current image capture storage was not admitted")
    }
}
impl CompletedCatalogSession {
    /// Storage admission only, while the original owner is still borrowed.
    /// The same guard remains retained through capture, unwind and handoff.
    /// The inherited host deadline is checked at source-proof checkpoints and
    /// passed explicitly to inventory/copy work. It does not shorten the Store's
    /// existing per-charge OperationBudget deadline inside source-proof segments.
    pub fn admit_current_image_operation(&mut self, deadline: Instant) -> Result<()> {
        if let Some(admission) = self.capture_admission.as_mut() {
            // Reuse the existing allocation guard. Re-admission can tighten,
            // but never replace storage or restart the inherited deadline.
            admission.deadline = admission.deadline.min(deadline);
            return Ok(());
        }
        let bytes =
            [
                std::mem::size_of::<CurrentImageCaptureOperation>(),
                std::mem::size_of::<CurrentImageCaptureWorkspace<'static>>(),
                std::mem::size_of::<std::result::Result<CurrentImageCaptureWorkspace<'static>, OfflineWorkspaceAdmissionRefused>>(),
                std::mem::size_of::<OfflineRetainedResult<()>>(),
                std::mem::size_of::<
                    std::result::Result<
                        CurrentImageCaptureOperation,
                        CurrentImageCapturePreparationFailure,
                    >,
                >(),
                std::mem::size_of::<
                    std::result::Result<CurrentCatalogImage, CurrentImageCaptureFailure>,
                >(),
                std::mem::size_of::<Result<(ValidatedCompletedImage, CapturedSourceHistory, i64)>>(
                ),
                CAPTURE_FAILED.len(),
            ]
            .into_iter()
            .try_fold(0usize, |n, x| n.checked_add(x))
            .ok_or_else(|| Error("capture handoff storage overflow".into()))?;
        let store = &self.session.store;
        let slots = store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("capture allocation owner missing".into()))?
            .allocation
            .reserve(
                &store.db,
                u64::try_from(bytes).map_err(|_| Error("capture slot overflow".into()))?,
            )?;
        self.capture_admission = Some(CaptureStorageAdmission {
            deadline: deadline.min(Instant::now() + Duration::from_secs(60)),
            failure: Error(CAPTURE_FAILED.to_owned()),
            _slots: slots,
        });
        Ok(())
    }
    /// Moves only; no query, allocation or callback during custody transfer.
    pub fn begin_current_image_capture(
        mut self,
    ) -> std::result::Result<CurrentImageCaptureOperation, CurrentImageCapturePreparationFailure>
    {
        let Some(admission) = self.capture_admission.take() else {
            return Err(CurrentImageCapturePreparationFailure { owner: self });
        };
        Ok(CurrentImageCaptureOperation {
            owner: self,
            admission,
            state: CaptureState::Ready,
            workspace_result: None,
        })
    }
}
impl CurrentImageCaptureOperation {
    /// Descriptive inherited deadline only; no restart or mutation authority.
    pub fn deadline(&self) -> Instant { self.admission.deadline }
    /// A claimed workspace alone is not an attempted Native capture.
    pub fn capture_attempted(&self) -> bool {
        matches!(self.state, CaptureState::Failed | CaptureState::Captured(..))
    }
    /// Must run before the host's final authentication fence. No result/error
    /// allocation is constructed by the later lexical loan-opening step.
    pub fn admit_control_workspace_result(&mut self) -> std::result::Result<(), OfflineWorkspaceAdmissionRefused> {
        if !matches!(self.state, CaptureState::Ready) || self.workspace_result.is_some() {
            return Err(OfflineWorkspaceAdmissionRefused);
        }
        self.workspace_result = Some(self.owner.session.admit_control_result()?);
        Ok(())
    }
    pub fn borrow_control_workspace<'a>(
        &mut self, backing: &'a mut allocation::OfflineControlWorkspaceBacking,
    ) -> std::result::Result<CurrentImageCaptureWorkspace<'a>, OfflineWorkspaceAdmissionRefused> {
        if !matches!(self.state, CaptureState::Ready) || self.workspace_result.is_none() {
            return Err(OfflineWorkspaceAdmissionRefused);
        }
        let store = &self.owner.session.store;
        let identity = store.offline_bootstrap.as_ref().ok_or(OfflineWorkspaceAdmissionRefused)?;
        // The fixed loan operation only checks original Arc/SQLite identity,
        // exclusive phase and existing aggregate totals. No query or reserve.
        let loan = backing.loan(&identity.allocation, &store.db)
            .map_err(|error| { drop(error); OfflineWorkspaceAdmissionRefused })?;
        let result = self.workspace_result.take().ok_or(OfflineWorkspaceAdmissionRefused)?;
        self.state = CaptureState::WorkspaceClaimed;
        Ok(CurrentImageCaptureWorkspace { result, loan })
    }
    pub fn capture_with_control_workspace(
        &mut self, cancel: &AtomicBool, workspace: CurrentImageCaptureWorkspace<'_>,
    ) -> OfflineRetainedResult<()> {
        let CurrentImageCaptureWorkspace { result, loan } = workspace;
        let control = BootstrapWorkspace::from_capture_loan(&self.owner.session.store, loan);
        // Split borrows keep the exact original Store stable through the call.
        let outcome = Self::capture_parts(&self.owner, &self.admission, &mut self.state,
            cancel, 0, Some(&control));
        // Finish while loan still lives. Oversized errors are destroyed here;
        // bounded original errors remain paired with the independent guard.
        result.finish(outcome)
    }
    pub fn capture(&mut self, cancel: &AtomicBool) -> Result<()> {
        self.capture_inner(cancel, 0)
    }
    fn capture_inner(&mut self, cancel: &AtomicBool, fault: u8) -> Result<()> {
        Self::capture_parts(&self.owner, &self.admission, &mut self.state, cancel, fault, None)
    }
    fn capture_parts(owner: &CompletedCatalogSession, admission: &CaptureStorageAdmission,
        state: &mut CaptureState, cancel: &AtomicBool, fault: u8,
        control: Option<&BootstrapWorkspace<'_>>,
    ) -> Result<()> {
        let ready = matches!((&*state, control.is_some()),
            (CaptureState::Ready, false) | (CaptureState::WorkspaceClaimed, true));
        if !ready {
            return Err(Error("current image capture operation already consumed".into()));
        }
        *state = CaptureState::Failed;
        #[cfg(test)]
        if fault == 1 {
            panic!("controlled pre-copy capture unwind");
        }
        #[cfg(not(test))]
        let _ = fault;
        let copy_fault = completed_image::CaptureFault::None;
        #[cfg(test)]
        let copy_fault = match fault {
            3 => completed_image::CaptureFault::CancelAfterCopyReturn,
            4 => completed_image::CaptureFault::PanicAfterCopyReturn,
            5 => completed_image::CaptureFault::CancelAfterProofReturn,
            6 => completed_image::CaptureFault::PanicAfterProofReturn,
            7 => completed_image::CaptureFault::CancelAfterOwnedSerialize,
            8 => completed_image::CaptureFault::PanicAfterOwnedSerialize,
            9 => completed_image::CaptureFault::CancelAfterProducerClose,
            10 => completed_image::CaptureFault::PanicAfterProducerClose,
            11 => completed_image::CaptureFault::BusyProducerClose,
            12 => completed_image::CaptureFault::PanicAfterOwnedAttach,
            13 => completed_image::CaptureFault::DeniedOwnedAttach,
            20 => completed_image::CaptureFault::CancelBeforeVerifierCacheRetirement,
            21 => completed_image::CaptureFault::PanicBeforeVerifierCacheRetirement,
            22 => completed_image::CaptureFault::CancelAfterVerifierCacheRetirement,
            23 => completed_image::CaptureFault::PanicAfterVerifierCacheRetirement,
            24 => completed_image::CaptureFault::PressureBeforeVerifierCacheRetirement,
            _ => copy_fault,
        };
        // A real source-cache working-set policy, never accounting credit. It
        // is installed before any copied output exists and spans the unchanged
        // outer source-availability fence. This guard borrows the stable owner;
        // later image/history locals die before restoration on error/unwind.
        let mut source_cache = match control {
            Some(control) => {
                // Fence only this new qualified work; ordinary capture keeps
                // its original entry check and error ordering unchanged.
                if cancel.load(std::sync::atomic::Ordering::Acquire)
                    || Instant::now() >= admission.deadline
                {
                    return Err(Error("current image capture deadline exhausted or cancelled".into()));
                }
                control_workspace::SourceCachePolicy::begin(
                    &owner.session.store, &admission._slots, control)?
            },
            None => None,
        };
        #[cfg(test)]
        if fault == 14 { cancel.store(true, std::sync::atomic::Ordering::Release); }
        #[cfg(test)]
        if fault == 15 { panic!("controlled current cache policy unwind"); }
        let (image, history, generation) = owner.capture_current_image_until(
            cancel,
            copy_fault,
            admission.deadline,
            control,
        )?;
        #[cfg(test)]
        if fault == 2 {
            panic!("controlled post-copy capture unwind");
        }
        // Generic read-only source finish may poll before its COMMIT. Refuse a
        // poisoned/expired result here, without claiming an acknowledged COMMIT
        // was rolled back. Policy restoration below is also fallible SQL; only
        // the final state assignment performs no check or callback.
        owner.session.store.transaction_available_with_control_workspace(control)?;
        #[cfg(test)]
        if fault == 16 { cancel.store(true, std::sync::atomic::Ordering::Release); }
        #[cfg(test)]
        if fault == 18 { panic!("controlled current availability unwind"); }
        if cancel.load(std::sync::atomic::Ordering::Acquire)
            || Instant::now() >= admission.deadline
        {
            return Err(Error(
                "current image capture deadline exhausted or cancelled".into(),
            ));
        }
        if let Some(cache) = source_cache.as_mut() {
            #[cfg(test)]
            if fault == 19 { cancel.store(true, std::sync::atomic::Ordering::Release); }
            #[cfg(test)]
            if fault == 17 { cache.finish_with_fault_for_test(true)?; }
            cache.finish()?;
            // Restoring this connection-local policy is SQL and can poll. Do
            // not publish after cancellation/deadline reached during restore.
            if cancel.load(std::sync::atomic::Ordering::Acquire)
                || Instant::now() >= admission.deadline
            {
                return Err(Error("current image capture deadline exhausted or cancelled".into()));
            }
        }
        *state = CaptureState::Captured(image, history, generation);
        Ok(())
    }
    /// Moves only into the caller's pre-admitted output slot. Failed capture
    /// returns only the original owner, never an image or healed Store.
    pub fn finish(self) -> std::result::Result<CurrentCatalogImage, CurrentImageCaptureFailure> {
        let Self {
            owner,
            admission,
            state,
            workspace_result: _,
        } = self;
        match state {
            CaptureState::Captured(image, history, generation) => Ok(CurrentCatalogImage {
                rotation_admission: None,
                image,
                history,
                generation,
                owner,
            }),
            _ => Err(CurrentImageCaptureFailure {
                error: admission.failure,
                owner,
            }),
        }
    }
}

impl CompletedCatalogSession {
    pub fn into_current_image(
        self,
        cancel: &AtomicBool,
    ) -> std::result::Result<CurrentCatalogImage, CurrentImageCaptureFailure> {
        // The legacy convenience path may not discard an admitted deadline.
        // Return the same owner and marker for the fixed borrowed operation.
        if self.capture_admission.is_some() {
            return Err(CurrentImageCaptureFailure {
                error: Error("use admitted current image capture operation".into()),
                owner: self,
            });
        }
        match self.capture_current_image(cancel) {
            Ok((image, history, generation)) => Ok(CurrentCatalogImage {
                rotation_admission: None,
                image,
                history,
                generation,
                owner: self,
            }),
            Err(error) => Err(CurrentImageCaptureFailure { error, owner: self }),
        }
    }
    fn capture_current_image(
        &self,
        cancel: &AtomicBool,
    ) -> Result<(ValidatedCompletedImage, CapturedSourceHistory, i64)> {
        self.capture_current_image_with_fault(cancel, completed_image::CaptureFault::None)
    }
    fn capture_current_image_with_fault(
        &self,
        cancel: &AtomicBool,
        fault: completed_image::CaptureFault,
    ) -> Result<(ValidatedCompletedImage, CapturedSourceHistory, i64)> {
        self.capture_current_image_until(cancel, fault, Instant::now() + Duration::from_secs(60), None)
    }
    fn capture_current_image_until(
        &self,
        cancel: &AtomicBool,
        fault: completed_image::CaptureFault,
        deadline: Instant,
        control: Option<&BootstrapWorkspace<'_>>,
    ) -> Result<(ValidatedCompletedImage, CapturedSourceHistory, i64)> {
        let live = || -> Result<()> {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) || Instant::now() >= deadline {
                return Err(Error(
                    "current image cancelled or deadline exhausted".into(),
                ));
            }
            Ok(())
        };
        live()?;
        self.validate_current_with_control_workspace(control)?;
        let store = &self.session.store;
        let mut allocation = self.session.image_allocation()?;
        // Existing bounded inventory metadata convention. Actual proof charges
        // additionally attach to this same retained allocation owner in the writer.
        if control.is_none() { allocation.admit(64 * 1024 * 1024)?; }
        let admission = CompletedCurrentImageAdmission { owner: self, control };
        let mut proof = store.transaction_detailed_with_control_workspace(|_| {
            admission.check(store)?;
            live()?;
            let workers =
                crate::native_worker_inventory::inspect_completed_current_image(store, &admission)?;
            if !workers.admits_new_work() {
                return Err(Error("current image has unresolved native workers".into()));
            }
            let mut budget =
                crate::catalog_nfo_attribution::ProofBudget::new(store.catalog_operation_budget()?);
            let history =
                crate::catalog_source_succession::capture_complete_history_in_with_control_workspace(store, &mut budget, control)?;
            // The operation-budget allocation attachment still covers every
            // history Value while its independent guard is installed.
            let history_retention = store.offline_bootstrap.as_ref()
                .ok_or_else(|| Error("current image allocation missing".into()))?
                .allocation.reserve(&store.db, retained_history(&history)?)?;
            live()?;
            let generation =
                store
                    .db
                    .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                        r.get(0)
                    })?;
            let inventory = match control {
                Some(control) => control.capture_source_inventory(cancel, deadline)?,
                None => {
                    let audit = crate::catalog_backup::inventory::capture(&store.db, cancel, deadline)?;
                    let retention = store.offline_bootstrap.as_ref()
                        .ok_or_else(|| Error("current image allocation missing".into()))?
                        .allocation.reserve(&store.db, audit.retained_allocations()?)?;
                    CurrentImageInventory { audit, retention: Some(retention) }
                }
            };
            admission.check(store)?;
            live()?;
            Ok(CurrentImageProof {
                owner: self,
                generation,
                inventory: Some(inventory),
                history,
                _history_retention: history_retention,
                control,
            })
        }, control)?;
        let history_bytes = retained_history(&proof.history)?;
        #[cfg(test)]
        {
            let context = &store.offline_bootstrap.as_ref().unwrap().allocation;
            assert!(context.temporary_bytes_for_test()? >= history_bytes + proof.inventory.as_ref().unwrap().audit.retained_allocations()?);
            assert!(proof.inventory.as_ref().unwrap().retention.is_some());
            allocation.record_capture_phase_for_test("proof_return", 0, 0, 0, 0)?;
            if matches!(fault, completed_image::CaptureFault::CancelAfterProofReturn) {
                cancel.store(true, std::sync::atomic::Ordering::Release);
            }
            if matches!(fault, completed_image::CaptureFault::PanicAfterProofReturn) {
                panic!("controlled current proof return unwind");
            }
        }
        // Both output guards crossed the transaction return with their payloads.
        drop(allocation);
        let mut image = completed_image::capture_current(store, &mut proof, cancel, deadline, fault)?;
        #[cfg(test)]
        {
            assert!(image.has_capture_retention_for_test(), "copier returned an uncovered image");
            image.record_capture_phase_for_test("copier_return")?;
            if matches!(fault, completed_image::CaptureFault::CancelAfterCopyReturn) {
                cancel.store(true, std::sync::atomic::Ordering::Release);
            }
            if matches!(fault, completed_image::CaptureFault::PanicAfterCopyReturn) {
                panic!("controlled current copier return unwind");
            }
        }
        live()?;
        let retained = image
            .retained_allocations()?
            .checked_add(history_bytes)
            .ok_or_else(|| Error("current image retained allocation overflow".into()))?;
        if control.is_some() {
            image.commit_current_retention(&mut proof, retained)?;
        } else {
            self.session.retain_image_allocation(retained)?;
        }
        #[cfg(test)]
        image.record_capture_phase_for_test("persistent_handoff")?;
        image.release_capture_retention();
        // Persistent image/history retention is now established. The proof's
        // audit drops before its guards; move history only at the final handoff.
        let CurrentImageProof { generation, inventory, history,
            _history_retention, .. } = proof;
        drop(inventory);
        drop(_history_retention);
        Ok((image, history, generation))
    }
}
fn retained_history(history: &CapturedSourceHistory) -> Result<u64> {
    // The source owner preallocates a bounded record vector. Reserve its entire
    // export-cohort capacity; object/string/vector contents use actual capacities.
    let mut bytes = 256u64 * std::mem::size_of::<Value>() as u64;
    for effect in history.effects() {
        bytes = bytes
            .checked_add(allocation::value_bytes(effect)?)
            .ok_or_else(|| Error("current image history allocation overflow".into()))?;
    }
    Ok(bytes)
}
fn retained_audit(audit: &Audit) -> Result<u64> { audit.retained_allocations() }
impl CurrentCatalogImage {
    /// Dispose only the copied image and keep the original admitted session.
    /// The native allocation context retains its conservative high-water mark.
    pub fn discard_copy(self) -> CompletedCatalogSession {
        let Self {
            image,
            history,
            owner,
            ..
        } = self;
        drop(image);
        drop(history);
        owner
    }

    pub fn catalog_id(&self) -> &str {
        &self.owner.session.store.database_id
    }
    pub fn incarnation(&self) -> &str {
        self.owner
            .session
            .store
            .catalog_incarnation
            .as_deref()
            .unwrap_or("")
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
    pub fn current_inventory_sha256(&self) -> &str {
        self.image.inventory_sha256()
    }
    pub fn transition_digest(&self) -> &str {
        &self.owner.transition_digest
    }
    pub fn source_effects(&self) -> &[Value] {
        self.history.effects()
    }
    /// Actual copied buffers and checkpointed native-owner logical retention.
    /// Host external reservation is deliberately excluded, not counted twice.
    pub fn retained_allocations(&self) -> Result<u64> {
        let bytes = self
            .image
            .retained_allocations()?
            .checked_add(retained_history(&self.history)?)
            .ok_or_else(|| Error("current image allocation overflow".into()))?;
        self.owner.session.retain_image_allocation(bytes)?;
        self.owner.retained_allocations()
    }
    /// Descriptive monotonic host reservation already retained by this owner.
    /// Reading it performs no validation allocation and grants no new work.
    pub fn external_retained_reservation(&self) -> Result<u64> {
        self.owner
            .session
            .store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("current image allocation owner missing".into()))?
            .allocation
            .external_retained_bytes()
    }
    /// Continue the same original host control backing after capture. The
    /// exclusive refresh loan never lends copied image or history storage.
    /// The host must freshly qualify its retained cohort before this call;
    /// original deadline and terminal phase checks remain with its producer.
    /// All image bytes and actual copied SQLite usage remain additive.
    pub fn refresh_external_reservation_with_control_workspace(
        &mut self,
        forwarded: u64,
        backing: &mut allocation::OfflineControlWorkspaceBacking,
    ) -> std::result::Result<(), control_workspace::OfflineWorkspaceAdmissionRefused> {
        self.owner.refresh_external_reservation_with_control_workspace(forwarded, backing)?;
        // The original-source loan has ended. Measure the actual immutable copy
        // and complete history, preserving their existing persistent high-water.
        self.retained_allocations().map(|_| ())
            .map_err(|error| { drop(error); control_workspace::OfflineWorkspaceAdmissionRefused })
    }
    pub fn refresh_external_reservation(
        &mut self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        self.owner.refresh_external_reservation(reservation)?;
        let retained = self
            .image
            .retained_allocations()?
            .checked_add(retained_history(&self.history)?)
            .ok_or_else(|| Error("current image retained allocation overflow".into()))?;
        self.owner.session.retain_image_allocation(retained)
    }
}

#[cfg(test)]
#[path = "current_image_tests.rs"]
mod tests;

#[path = "current_rotation.rs"]
mod rotation;
pub(crate) use rotation::CurrentImageRotationAdmission;
pub use rotation::{
    CurrentRestoreHostAttestation, CurrentRotationFailure, CurrentRotationOperation,
    CurrentRotationPlanFacts, CurrentRotationPlanningOperation, CurrentRotationPreparationFailure,
    RotatedCurrentCatalogImage,
};
