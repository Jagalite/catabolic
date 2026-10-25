//! Fixed post-bootstrap source custody. The original Complete is measured once
//! before conversion; later source effects do not pretend pristine40 equality.
use super::*;
use std::sync::atomic::AtomicBool;

pub struct CompletedCatalogSession {
    // A failed explicit copied-connection close remains with the original
    // poisoned owner through operation failure and into_session, never a sink.
    failed_capture: std::cell::RefCell<Option<crate::catalog_restore_lineage::completed_image::FailedOwnedCapture>>,
    capture_admission: Option<current_image::CaptureStorageAdmission>,
    session: OfflineBootstrapSession,
    bootstrap: CommandEnvelope,
    inventory_digest: String,
    source_count: i64,
    committed_generation: i64,
    transition_digest: String,
}
/// Pre-admitted buffers for the fixed borrowed completion producer. This is
/// storage, not a Complete or a grant; only the private operation can publish it.
pub(in crate::migration) struct CatalogCompletionStorage {
    bootstrap: CommandEnvelope,
    inventory_digest: String,
    transition_digest: String,
    state_digest: String,
    _slots: allocation::AllocationGuard,
}
impl CatalogCompletionStorage {
    pub(in crate::migration) fn matches(&self, command: &CommandEnvelope) -> bool {
        self.bootstrap == *command
    }
    // Borrowed storage only. Publication is gated by the operation's successful
    // measurement state, not by calling these descriptive accessors.
    pub(in crate::migration) fn command(&self) -> &CommandEnvelope {
        &self.bootstrap
    }
    pub(in crate::migration) fn inventory_digest(&self) -> &str {
        &self.inventory_digest
    }
    pub(in crate::migration) fn transition_digest(&self) -> &str {
        &self.transition_digest
    }
    pub(in crate::migration) fn state_digest(&self) -> &str {
        &self.state_digest
    }
}
// Core envelopes expose immutable strings, not capacities. Two times their
// actual bytes is the conservative owned-string growth convention; the payload
// walk uses observable Vec/String capacities and the existing map-node charge.
// No allowance maxima, JSON parsing, serialization, or authority is involved.
pub(super) fn retained_command_bytes(command: &CommandEnvelope) -> Result<u64> {
    let mut bytes = allocation::value_bytes(command.payload())?
        .checked_add(std::mem::size_of::<CommandEnvelope>() as u64)
        .ok_or_else(|| Error("completed command retention overflow".into()))?;
    for text in [
        command.canonical_payload(),
        command.key().catalog_id(),
        command.key().host_namespace(),
        command.key().operation_id(),
        command.admission_incarnation(),
        command.digest().as_str(),
    ] {
        bytes = (text.len() as u64)
            .checked_mul(2)
            .and_then(|n| bytes.checked_add(n))
            .ok_or_else(|| Error("completed command retention overflow".into()))?;
    }
    Ok(bytes)
}
fn recovered_command_bytes(session: &OfflineBootstrapSession) -> Result<u64> {
    match &session.mode {
        BootstrapMode::Pending { .. } => Ok(0),
        BootstrapMode::CompletedReceiptOnly { command } => retained_command_bytes(command),
    }
}
/// Nonclone exact owner/envelope admission. Source effects still require their
/// own current actor, complete context, descriptor and worker proofs.
pub(crate) struct CompletedSourceAdmission<'a, 'workspace> {
    owner: &'a CompletedCatalogSession,
    command: &'a CommandEnvelope,
    control: Option<&'a BootstrapWorkspace<'workspace>>,
}
impl<'a, 'workspace> CompletedSourceAdmission<'a, 'workspace> {
    pub(crate) fn control_workspace(&self) -> Option<&'a BootstrapWorkspace<'workspace>> { self.control }
    pub(crate) fn check(&self, store: &Store, command: &CommandEnvelope) -> Result<()> {
        if !std::ptr::eq(store, &self.owner.session.store) || command != self.command {
            return Err(Error(
                "completed source admission owner/envelope differs".into(),
            ));
        }
        self.owner.validate_current_with_control_workspace(self.control)?;
        if command.key().catalog_id() != store.database_id
            || Some(command.admission_incarnation()) != store.catalog_incarnation.as_deref()
        {
            return Err(Error("completed source command identity differs".into()));
        }
        Ok(())
    }
}
impl OfflineBootstrapSession {
    #[cfg(test)]
    pub(in crate::migration) fn completion_store_for_test(&self) -> &Store {
        &self.store
    }
    #[cfg(test)]
    pub(in crate::migration) fn completion_custody_for_test(&self) -> (usize, usize, usize) {
        let (catalog, incarnation) = self.preservation.identity();
        // Identity-only inspection of the same live connection, never a call
        // through the pointer or a replacement owner.
        (
            unsafe { self.store.db.handle() } as usize,
            catalog.as_ptr() as usize,
            incarnation.as_ptr() as usize,
        )
    }
    #[cfg(test)]
    pub(in crate::migration) fn fill_completion_headroom_for_test(
        &self,
    ) -> Result<allocation::AllocationGuard> {
        let context = &self.store.offline_bootstrap.as_ref().unwrap().allocation;
        let held =
            context.retained_native_bytes(&self.store.db)? + context.external_retained_bytes()?;
        context.reserve(&self.store.db, allocation::LIMIT - held)
    }
    pub(in crate::migration) fn admit_catalog_completion_storage(
        &self,
        command: &CommandEnvelope,
        slots: u64,
    ) -> Result<CatalogCompletionStorage> {
        // PayloadDigest validates exactly 64 lowercase SHA-256 hex bytes. All
        // completion digests use that same contract, not an allowance maximum.
        let digest_bytes = command.digest().as_str().len();
        let retained = retained_command_bytes(command)?
            .checked_add(recovered_command_bytes(self)?)
            .and_then(|n| n.checked_add(std::mem::size_of::<CompletedCatalogSession>() as u64))
            .and_then(|n| n.checked_add(3 * digest_bytes as u64))
            .ok_or_else(|| Error("completed operation retention overflow".into()))?;
        self.retain_completed_allocation(retained)?;
        let slots = self
            .store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("completed allocation owner missing".into()))?
            .allocation
            .reserve(&self.store.db, slots)?;
        Ok(CatalogCompletionStorage {
            bootstrap: command.clone(),
            inventory_digest: String::with_capacity(digest_bytes),
            transition_digest: String::with_capacity(digest_bytes),
            state_digest: String::with_capacity(digest_bytes),
            _slots: slots,
        })
    }
    /// Exactly one existing Complete verification. Its existing ambient/default
    /// Store budget selection is unchanged; this does not manufacture a budget.
    pub(in crate::migration) fn measure_catalog_completion(
        &self,
        storage: &mut CatalogCompletionStorage,
    ) -> Result<(i64, i64)> {
        self.measure_catalog_completion_with_control_workspace(storage, None)
    }
    pub(in crate::migration) fn measure_catalog_completion_with_control_workspace(
        &self, storage: &mut CatalogCompletionStorage, control: Option<&BootstrapWorkspace<'_>>,
    ) -> Result<(i64, i64)> {
        let complete = self.complete_with_control_workspace(&storage.bootstrap, control)?;
        let digest_bytes = storage.bootstrap.digest().as_str().len();
        if complete.catalog_id() != storage.bootstrap.key().catalog_id()
            || complete.incarnation() != storage.bootstrap.admission_incarnation()
            || complete.command_key() != storage.bootstrap.key()
            || complete.payload_digest() != storage.bootstrap.digest().as_str()
        {
            return Err(Error("completed measured command differs".into()));
        }
        if complete.inventory_digest().len() != digest_bytes
            || complete.transition_digest().len() != digest_bytes
            || complete.state_digest().len() != digest_bytes
            || storage.inventory_digest.capacity() < digest_bytes
            || storage.transition_digest.capacity() < digest_bytes
            || storage.state_digest.capacity() < digest_bytes
        {
            return Err(Error("completed digest storage contract differs".into()));
        }
        storage.inventory_digest.clear();
        storage
            .inventory_digest
            .push_str(complete.inventory_digest());
        storage.transition_digest.clear();
        storage
            .transition_digest
            .push_str(complete.transition_digest());
        storage.state_digest.clear();
        storage.state_digest.push_str(complete.state_digest());
        let result = (complete.source_count(), complete.committed_generation());
        drop(complete);
        self.store.transaction_available_with_control_workspace(control)?;
        Ok(result)
    }
    /// Fixed move-only handoff. The operation publishes this only after its
    /// borrowed measurement and final host deadline/cancellation checkpoint.
    pub(in crate::migration) fn finish_catalog_completion(
        self,
        storage: CatalogCompletionStorage,
        source_count: i64,
        committed_generation: i64,
    ) -> CompletedCatalogSession {
        let CatalogCompletionStorage {
            bootstrap,
            inventory_digest,
            transition_digest,
            state_digest: _state_digest,
            _slots,
        } = storage;
        // The ACK-only state buffer is destroyed on return, without fallible
        // allocation or owner mutation. Its conservative retained high-water
        // charge is deliberately not lowered by this move-only handoff.
        CompletedCatalogSession {
            failed_capture: std::cell::RefCell::new(None),
            capture_admission: None,
            session: self,
            bootstrap,
            inventory_digest,
            source_count,
            committed_generation,
            transition_digest,
        }
    }
    pub(in crate::migration) fn into_completed_catalog(
        self,
        command: &CommandEnvelope,
    ) -> Result<CompletedCatalogSession> {
        let complete = self.complete(command)?;
        // Preflight the additional retained clone while Complete and any saved
        // receipt-only command still coexist. Keep it in the retained-buffer
        // high-water used by subsequent native phases, not a released scratch.
        let retained = retained_command_bytes(command)?
            .checked_add(recovered_command_bytes(&self)?)
            .and_then(|n| n.checked_add(std::mem::size_of::<CompletedCatalogSession>() as u64))
            .and_then(|n| {
                n.checked_add(
                    2 * (complete.inventory_digest().len() + complete.transition_digest().len())
                        as u64,
                )
            })
            .ok_or_else(|| Error("completed owner retention overflow".into()))?;
        self.retain_completed_allocation(retained)?;
        let inventory_digest = complete.inventory_digest().to_owned();
        let source_count = complete.source_count();
        let committed_generation = complete.committed_generation();
        let transition_digest = complete.transition_digest().to_owned();
        drop(complete);
        Ok(CompletedCatalogSession {
            failed_capture: std::cell::RefCell::new(None),
            capture_admission: None,
            session: self,
            bootstrap: command.clone(),
            inventory_digest,
            source_count,
            committed_generation,
            transition_digest,
        })
    }
}
impl CompletedCatalogSession {
    #[cfg(test)]
    pub(in crate::migration) fn completion_custody_for_test(&self) -> (usize, usize, usize) {
        self.session.completion_custody_for_test()
    }
    fn retained_completed_bytes(&self) -> Result<u64> {
        retained_command_bytes(&self.bootstrap)?
            .checked_add(recovered_command_bytes(&self.session)?)
            .and_then(|n| n.checked_add(self.inventory_digest.capacity() as u64))
            .and_then(|n| n.checked_add(self.transition_digest.capacity() as u64))
            .and_then(|n| n.checked_add(std::mem::size_of::<Self>() as u64))
            .ok_or_else(|| Error("completed owner retention overflow".into()))
    }
    /// Checkpointed native-owner logical retention, excluding the separately
    /// tracked host reservation. This does not decode or grant current proof.
    pub fn retained_allocations(&self) -> Result<u64> {
        self.session
            .retain_completed_allocation(self.retained_completed_bytes()?)?;
        self.session
            .store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("completed source allocation owner missing".into()))?
            .allocation
            .retained_native_bytes(&self.session.store.db)
    }
    /// The actual monotonic host high-water already held by this native owner.
    pub fn external_retained_reservation(&self) -> Result<u64> {
        self.session
            .store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("completed source allocation owner missing".into()))?
            .allocation
            .external_retained_bytes()
    }
    pub fn refresh_external_reservation(
        &mut self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        self.session.refresh_external_reservation(reservation)
    }
    fn validate_current(&self) -> Result<()> {
        self.validate_current_with_control_workspace(None)
    }
    fn validate_current_with_control_workspace(&self, control: Option<&BootstrapWorkspace<'_>>) -> Result<()> {
        self.session.store.validate_native_identity_with_control_workspace(control)?;
        let transition_matches = match control {
            Some(control) => {
                // Preserve the original transition-description identity check,
                // but explicitly use this same live metadata loan.
                self.session.store.validate_native_identity_with_control_workspace(Some(control))?;
                control.transition_digest()? == self.transition_digest
            }
            None => self.session.transition_description()?.digest() == self.transition_digest,
        };
        if !transition_matches {
            return Err(Error("completed source transition changed".into()));
        }
        // Top-level receipt routing has not installed its operation budget yet.
        // Genuine Complete and exclusive custody establish admission here; the
        // SAME check is repeated inside the writer before effects and at COMMIT.
        if self.session.store.db.is_autocommit() {
            return Ok(());
        }
        let historical = crate::catalog_nfo_attribution::verify_historical_bootstrap_receipt_in_with_control_workspace(
            &self.session.store,
            &self.bootstrap,
            control,
        )?;
        if historical.inventory_digest != self.inventory_digest
            || historical.source_count != self.source_count
            || historical.committed_generation != self.committed_generation
        {
            return Err(Error("completed source bootstrap receipt changed".into()));
        }
        Ok(())
    }
    pub fn execute_source_succession(
        &mut self,
        command: &CommandEnvelope,
        authority: TrustedCommandAuthority<'_>,
        policy: ReceiptPolicy,
        cancel: &AtomicBool,
    ) -> catalog_commands::Result<CommandOutcome> {
        let admission = CompletedSourceAdmission {
            owner: self,
            command,
            control: None,
        };
        crate::catalog_source_succession::execute_completed(
            &self.session.store,
            &admission,
            command,
            authority,
            policy,
            cancel,
        )
    }
}

/// Exact descriptive preparation scope, independently re-proved on execution.
pub(crate) struct CompletedSourcePreparationAdmission<'a, 'workspace> {
    owner: &'a CompletedCatalogSession,
    profile: &'a str,
    source: &'a str,
    candidate: &'a Path,
    control: Option<&'a BootstrapWorkspace<'workspace>>,
}
impl<'a, 'workspace> CompletedSourcePreparationAdmission<'a, 'workspace> {
    pub(crate) fn control_workspace(&self) -> Option<&'a BootstrapWorkspace<'workspace>> { self.control }
    pub(crate) fn check(
        &self,
        store: &Store,
        profile: &str,
        source: &str,
        candidate: &Path,
    ) -> Result<()> {
        if !std::ptr::eq(store, &self.owner.session.store)
            || profile != self.profile
            || source != self.source
            || candidate != self.candidate
        {
            return Err(Error(
                "completed source preparation owner/scope differs".into(),
            ));
        }
        self.owner.validate_current_with_control_workspace(self.control)
    }
}
impl CompletedCatalogSession {
    pub fn prepare_source_succession(
        &mut self,
        profile: &str,
        source: &str,
        candidate_root: &Path,
        cancel: &AtomicBool,
    ) -> Result<crate::catalog_source_succession::SourceRenewalPreparation> {
        let admission = CompletedSourcePreparationAdmission {
            owner: self,
            profile,
            source,
            candidate: candidate_root,
            control: None,
        };
        crate::catalog_source_succession::prepare_completed(
            &self.session.store,
            &admission,
            profile,
            source,
            candidate_root,
            cancel,
        )
    }
}

#[cfg(test)]
#[path = "completed_catalog_tests.rs"]
mod tests;

impl CompletedCatalogSession {
    /// Purpose-specific permanent effect proof for host ACK recovery. This is
    /// not generic full-result replay and does not reopen current source files.
    pub fn source_succession_receipt(
        &self,
        command: &CommandEnvelope,
        authority: TrustedCommandAuthority<'_>,
    ) -> Result<Option<crate::catalog_source_succession::SourceRenewalReceipt>> {
        let admission = CompletedSourceAdmission {
            owner: self,
            command,
            control: None,
        };
        crate::catalog_source_succession::receipt_completed(
            &self.session.store,
            &admission,
            command,
            authority,
        )
    }
}

#[cfg(test)]
impl CompletedCatalogSession {
    pub(crate) fn compact_source_receipt_for_test(
        &self,
        command: &CommandEnvelope,
    ) -> catalog_commands::Result<usize> {
        let admission = CompletedSourceAdmission {
            owner: self,
            command,
            control: None,
        };
        catalog_commands::compact_completed_source_receipt_for_test(
            &self.session.store,
            command,
            &admission,
        )
    }
}

#[path = "completed_withdrawal.rs"]
mod completed_withdrawal;
pub use completed_withdrawal::CompletedSourceWithdrawal;
pub(crate) use completed_withdrawal::CompletedSourceWithdrawalAdmission;

#[path = "current_image.rs"]
mod current_image;
pub(crate) use current_image::{
    CompletedCurrentImageAdmission, CurrentImageProof, CurrentImageRotationAdmission,
};
pub use current_image::{
    CurrentCatalogImage, CurrentImageCaptureFailure, CurrentImageCaptureOperation,
    CurrentImageCapturePreparationFailure, CurrentImageCaptureWorkspace, CurrentRestoreHostAttestation, CurrentRotationFailure,
    CurrentRotationOperation, CurrentRotationPlanFacts, CurrentRotationPlanningOperation,
    CurrentRotationPreparationFailure, RotatedCurrentCatalogImage,
};

#[path = "completed_library_scope.rs"]
mod completed_library_scope;
pub use completed_library_scope::{CompletedFileEndpoint, CompletedLibraryScope};

pub(crate) use completed_library_scope::EndpointFreshFacts;

/// Fixed source94 continuation of the original control backing. The host must
/// freshly qualify its quiescent persistent cohort at RETAINED_LIMIT before
/// each purpose's final temporal fence. No qualification, host callback, await,
/// or new authentication scan occurs in these synchronous Native entry points.
impl CompletedCatalogSession {
    pub fn refresh_external_reservation_with_control_workspace(
        &mut self,
        forwarded: u64,
        backing: &mut allocation::OfflineControlWorkspaceBacking,
    ) -> std::result::Result<(), OfflineWorkspaceAdmissionRefused> {
        let identity = self.session.store.offline_bootstrap.as_ref().ok_or(OfflineWorkspaceAdmissionRefused)?;
        let mut loan = backing.loan(&identity.allocation, &self.session.store.db)
            .map_err(|error| { drop(error); OfflineWorkspaceAdmissionRefused })?;
        loan.refresh_external_reservation(&identity.allocation, &self.session.store.db, forwarded)
            .map_err(|error| { drop(error); OfflineWorkspaceAdmissionRefused })
    }
    pub fn prepare_source_succession_with_control_workspace(
        &mut self,
        profile: &str,
        source: &str,
        candidate_root: &Path,
        cancel: &AtomicBool,
        backing: &mut allocation::OfflineControlWorkspaceBacking,
    ) -> std::result::Result<OfflineRetainedResult<crate::catalog_source_succession::SourceRenewalPreparation>, OfflineWorkspaceAdmissionRefused> {
        let retained = self.session.admit_control_result::<crate::catalog_source_succession::SourceRenewalPreparation, Error>()?;
        let control = self.session.control_workspace(backing)?;
        let admission = CompletedSourcePreparationAdmission {
            owner: self, profile, source, candidate: candidate_root, control: Some(&control),
        };
        let result = crate::catalog_source_succession::prepare_completed(
            &self.session.store, &admission, profile, source, candidate_root, cancel,
        );
        let result = retained.finish(result);
        drop(control);
        Ok(result)
    }
    pub fn execute_source_succession_with_control_workspace(
        &mut self,
        command: &CommandEnvelope,
        authority: TrustedCommandAuthority<'_>,
        policy: ReceiptPolicy,
        cancel: &AtomicBool,
        backing: &mut allocation::OfflineControlWorkspaceBacking,
    ) -> std::result::Result<OfflineRetainedResult<CommandOutcome, catalog_commands::CommandStoreError>, OfflineWorkspaceAdmissionRefused> {
        let retained = self.session.admit_control_result::<CommandOutcome, catalog_commands::CommandStoreError>()?;
        let control = self.session.control_workspace(backing)?;
        let admission = CompletedSourceAdmission { owner: self, command, control: Some(&control) };
        let result = crate::catalog_source_succession::execute_completed(
            &self.session.store, &admission, command, authority, policy, cancel,
        );
        // A postcommit output refusal remains a retained error, never evidence
        // of rollback. Receipt reconciliation remains the authoritative route.
        let result = retained.finish(result);
        drop(control);
        Ok(result)
    }
    pub fn source_succession_receipt_with_control_workspace(
        &self,
        command: &CommandEnvelope,
        authority: TrustedCommandAuthority<'_>,
        backing: &mut allocation::OfflineControlWorkspaceBacking,
    ) -> std::result::Result<OfflineRetainedResult<Option<crate::catalog_source_succession::SourceRenewalReceipt>>, OfflineWorkspaceAdmissionRefused> {
        let retained = self.session.admit_control_result::<Option<crate::catalog_source_succession::SourceRenewalReceipt>, Error>()?;
        let control = self.session.control_workspace(backing)?;
        let admission = CompletedSourceAdmission { owner: self, command, control: Some(&control) };
        let result = crate::catalog_source_succession::receipt_completed(
            &self.session.store, &admission, command, authority,
        );
        let result = retained.finish(result);
        drop(control);
        Ok(result)
    }
}

#[cfg(test)]
#[path = "completed_catalog/source_workspace_tests.rs"]
mod source_workspace_tests;
