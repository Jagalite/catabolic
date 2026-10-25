//! Explicit offline transition and bootstrap custody, not normal schema41
//! admission. The host retains this owner through its durable intent/ACK write.
//! Evidence descriptions are borrowed assertions; native owners independently
//! validate exact historical commands, import bytes, and current complete state.
use super::{NativeUpgradeQuiescence, native_session};
pub use crate::catalog_restore_lineage::completed_image::{
    CompletedImageDescription, RestoreHostAttestation,
};
pub use crate::catalog_source_succession::{SourceRenewalPreparation, SourceRenewalReceipt};
use crate::{
    Result,
    catalog_commands::{self, CommandOutcome, ReceiptPolicy},
};
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
pub use native_session::{
    CompletedCatalogSession, CompletedFileEndpoint, CompletedLibraryScope, CompletedOfflineImage,
    CompletedSourceWithdrawal, CurrentCatalogImage, CurrentImageCaptureFailure,
    CurrentImageCaptureOperation, CurrentImageCapturePreparationFailure, CurrentImageCaptureWorkspace,
    CurrentRestoreHostAttestation, CurrentRotationFailure, CurrentRotationOperation,
    CurrentRotationPlanFacts, CurrentRotationPlanningOperation, CurrentRotationPreparationFailure,
    HostBaselineEvidence, HostImportAudit, HostImportOrigin, HostImportPart, HostSourceEvidence,
    OfflineAllocationReservation, OfflineControlWorkspaceBacking, OfflineTransition, OfflineOwnedTransition,
    OfflineRetainedResult, OfflineWorkspaceAdmissionRefused, RotatedCurrentCatalogImage,
    RotatedOfflineImage,
};
use serde_json::Value;
use std::{borrow::Cow, path::Path};
#[path = "migration_offline_bootstrap/completion_operation.rs"]
mod completion_operation;
pub use completion_operation::{
    OfflineCompletionFacts, OfflineCompletionFailure, OfflineCompletionOperation,
    OfflineCompletionPreparationFailure,
};

#[path = "migration_offline_bootstrap/apply_operation.rs"]
mod apply_operation;
pub(in crate::migration) use apply_operation::AdmittedControlApplyOutput;
pub use apply_operation::{
    OfflineApplyFailure, OfflineApplyOperation, OfflineApplyPreparationFailure,
};
#[path = "migration_offline_bootstrap/preparation_operation.rs"]
mod preparation_operation;
pub use preparation_operation::{
    OfflinePreparationFailure, OfflinePreparationOperation, OfflinePreparationPhase,
};

/// Prepared under exclusive lifecycle/writer custody. No generic checkpoint or
/// mutable connection escapes. Legacy initial prepare/prepare_reserved return
/// a plain Result and close custody on error. Use OfflinePreparationOperation
/// for retained initial acquisition, then the fixed borrowed apply operation.
pub struct PreparedOfflineBootstrap {
    owner: native_session::RetainedNativeUpgrade,
    plan: Value,
    identity: PreparedIdentity,
    apply_admission: Option<apply_operation::ApplyAdmission>,
}
struct PreparedIdentity {
    catalog: String,
    incarnation: String,
    generation: i64,
    device: u64,
    inode: u64,
    producer_deadline: Option<std::time::Instant>,
}
impl PreparedOfflineBootstrap {
    /// Fixed same-context accounting and DBSTATUS update; no SQL query,
    /// embedding poll, host proof scan or authorization grant. A fixed caller
    /// may use it between its final temporal fence and effect continuation.
    /// DBSTATUS has library-internal work: this promises no preemptive timing
    /// or constant-time execution. Reservation only increases; counters remain.
    pub fn refresh_external_reservation(
        &mut self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        self.owner.refresh_external_reservation(reservation)
    }
    pub fn prepare(path: &Path, quiescence: NativeUpgradeQuiescence) -> Result<Self> {
        Self::prepare_reserved(path, quiescence, OfflineAllocationReservation::default())
    }
    pub fn prepare_reserved(
        path: &Path,
        quiescence: NativeUpgradeQuiescence,
        reservation: OfflineAllocationReservation,
    ) -> Result<Self> {
        let owner =
            native_session::RetainedNativeUpgrade::acquire_reserved(path, quiescence, reservation)?;
        let (owner, plan) = owner.plan_closed41()?;
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(&owner.path)?;
        let (catalog, incarnation, generation): (String, String, i64) = owner.db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        owner.admission.validate_path()?;
        if plan["input"]["catalog_identity"]["catalog_id"].as_str() != Some(catalog.as_str())
            || plan["input"]["catalog_identity"]["incarnation"].as_str()
                != Some(incarnation.as_str())
            || plan["input"]["catalog_state"][0]["generation"].as_i64() != Some(generation)
            || plan["input"]["file_identity"]["device"].as_u64() != Some(metadata.dev())
            || plan["input"]["file_identity"]["inode"].as_u64() != Some(metadata.ino())
        {
            return Err(crate::Error(
                "prepared offline identity differs from approved plan".into(),
            ));
        }
        let identity = PreparedIdentity {
            catalog,
            incarnation,
            generation,
            device: metadata.dev(),
            inode: metadata.ino(),
            producer_deadline: None,
        };
        Ok(Self {
            owner,
            plan,
            identity,
            apply_admission: None,
        })
    }
    pub fn catalog_id(&self) -> &str {
        &self.identity.catalog
    }
    pub fn incarnation(&self) -> &str {
        &self.identity.incarnation
    }
    pub fn source_generation(&self) -> i64 {
        self.identity.generation
    }
    pub fn native_path(&self) -> &Path {
        &self.owner.path
    }
    pub fn file_identity(&self) -> (u64, u64) {
        (self.identity.device, self.identity.inode)
    }
    pub fn approved_plan(&self) -> &Value {
        &self.plan
    }
    /// Legacy plain-Result convenience still drops original custody on error or
    /// unwind. Use the admitted borrowed operation to retain a terminal failure.
    /// An admitted or retained-preparation owner cannot bypass its original
    /// deadline. This old plain-Result refusal still closes consumed custody.
    pub fn apply(self, backup_parent: Option<&Path>) -> Result<OfflineBootstrap<'static>> {
        if self.apply_admission.is_some() || self.identity.producer_deadline.is_some() {
            return Err(crate::Error("use admitted offline apply operation".into()));
        }
        let (owner, _) = self
            .owner
            .apply_closed41(&self.plan, backup_parent, &|_, _, _| Ok(()))?;
        Ok(OfflineBootstrap {
            session: owner.into_bootstrap()?,
            plan: Cow::Owned(self.plan),
            completion_admission: None,
            producer_deadline: None,
            #[cfg(test)]
            producer_exit_test: None,
        })
    }
}
/// Recovery phase is explicit. Completed receipt-only recovery cannot prepare or
/// execute a different command, even if its payload happens to be identical.
pub enum Recovery<'a> {
    Pending { backup_path: &'a Path },
    CompletedReceiptOnly { command: &'a CommandEnvelope },
}
pub struct OfflineBootstrap<'plan> {
    session: native_session::OfflineBootstrapSession,
    plan: Cow<'plan, Value>,
    completion_admission: Option<completion_operation::CompletionAdmission>,
    producer_deadline: Option<std::time::Instant>,
    #[cfg(test)]
    producer_exit_test: Option<ProducerExitTest>,
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProducerExitStage {
    Prepare,
    Reconcile,
    Execute,
}
#[cfg(test)]
struct ProducerExitTest {
    stage: ProducerExitStage,
    reached: std::cell::Cell<bool>,
}
impl<'plan> OfflineBootstrap<'plan> {
    #[cfg(test)]
    fn wait_for_producer_exit_expiry_for_test(&self, stage: ProducerExitStage) {
        let Some(hook) = self.producer_exit_test.as_ref() else {
            return;
        };
        if hook.stage != stage {
            return;
        }
        // This fixed hook runs only AFTER the real inner call returned Ok. It
        // invents no result or clock and neither replaces nor advances a budget.
        hook.reached.set(true);
        let deadline = self.producer_deadline.expect("marked exit-expiry fixture");
        while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
            std::thread::sleep(remaining);
        }
    }
    fn check_producer_deadline(&self, after_work: bool) -> Result<()> {
        if self
            .producer_deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Err(crate::Error(if after_work {
                "offline bootstrap deadline exhausted after work; a committed receipt may remain for separately authorized recovery"
            } else {
                "offline bootstrap inherited producer deadline exhausted"
            }.into()));
        }
        Ok(())
    }
    pub fn refresh_external_reservation(
        &mut self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        self.session.refresh_external_reservation(reservation)
    }
    pub fn recover(
        path: &Path,
        quiescence: NativeUpgradeQuiescence,
        approved_plan: &'plan Value,
        phase: Recovery<'_>,
    ) -> Result<Self> {
        Self::recover_reserved(
            path,
            quiescence,
            approved_plan,
            phase,
            OfflineAllocationReservation::default(),
        )
    }
    pub fn recover_reserved(
        path: &Path,
        quiescence: NativeUpgradeQuiescence,
        approved_plan: &'plan Value,
        phase: Recovery<'_>,
        reservation: OfflineAllocationReservation,
    ) -> Result<Self> {
        let owner =
            native_session::RetainedNativeUpgrade::acquire_reserved(path, quiescence, reservation)?;
        let phase = match phase {
            Recovery::Pending { backup_path } => {
                native_session::BootstrapRecovery::Pending { backup_path }
            }
            Recovery::CompletedReceiptOnly { command } => {
                native_session::BootstrapRecovery::CompletedReceiptOnly { command }
            }
        };
        let session = owner.recover_bootstrap(approved_plan, phase)?;
        Ok(Self {
            session,
            plan: Cow::Borrowed(approved_plan),
            completion_admission: None,
            producer_deadline: None,
            #[cfg(test)]
            producer_exit_test: None,
        })
    }
    /// Legacy consuming convenience. An admitted completion must use its fixed
    /// operation instead. This plain-Result API still closes custody on error;
    /// only the new completion operation retains an opaque failed owner.
    pub fn into_completed_image(
        self,
        command: &CommandEnvelope,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<CompletedOfflineImage> {
        self.require_unadmitted_completion()?;
        self.session.into_completed_image(command, cancel)
    }
    /// Legacy consuming convenience with the same error/drop boundary as before.
    /// It cannot bypass an already admitted completion's original deadline.
    pub fn into_completed_catalog(
        self,
        command: &CommandEnvelope,
    ) -> Result<CompletedCatalogSession> {
        self.require_unadmitted_completion()?;
        self.session.into_completed_catalog(command)
    }
    pub fn approved_plan(&self) -> &Value {
        &self.plan
    }
    /// Fixed host contract: the exact persistent control cohort is quiescent
    /// and no larger than OfflineControlWorkspaceBacking::RETAINED_LIMIT.
    /// Qualification is descriptive accounting, never temporal authority. Keep
    /// the opaque result intact; its additive reservation survives this owner.
    pub fn transition_with_control_workspace(
        &self, backing: &mut OfflineControlWorkspaceBacking,
    ) -> std::result::Result<OfflineRetainedResult<OfflineOwnedTransition>, OfflineWorkspaceAdmissionRefused> {
        let admission = self.session.admit_control_result::<OfflineOwnedTransition, crate::Error>()?;
        let control = self.session.control_workspace(backing)?;
        let result = self.session.owned_transition_with_control_workspace(&control);
        let retained = admission.finish(result);
        drop(control);
        Ok(retained)
    }
    pub fn generation_with_control_workspace(
        &self, backing: &mut OfflineControlWorkspaceBacking,
    ) -> std::result::Result<OfflineRetainedResult<i64>, OfflineWorkspaceAdmissionRefused> {
        let admission = self.session.admit_control_result::<i64, crate::Error>()?;
        let control = self.session.control_workspace(backing)?;
        let result = self.session.generation_with_control_workspace(Some(&control));
        let retained = admission.finish(result);
        drop(control);
        Ok(retained)
    }
    /// Only the fixed empty-evidence cohort is eligible. General evidence keeps
    /// the ordinary full metadata charge and unchanged public API.
    pub fn prepare_empty_with_control_workspace(
        &self, host: &str, backing: &mut OfflineControlWorkspaceBacking,
    ) -> std::result::Result<OfflineRetainedResult<Value>, OfflineWorkspaceAdmissionRefused> {
        let admission = self.session.admit_control_result::<Value, crate::Error>()?;
        let control = self.session.control_workspace(backing)?;
        let result = (|| {
            self.check_producer_deadline(false)?;
            let preparation = self.session.prepare_host_with_control_workspace(host, &[], Some(&control))?;
            #[cfg(test)]
            self.wait_for_producer_exit_expiry_for_test(ProducerExitStage::Prepare);
            self.check_producer_deadline(true)?;
            Ok(preparation)
        })();
        let retained = admission.finish(result);
        drop(control);
        Ok(retained)
    }
    pub fn reconcile_with_control_workspace(
        &self, command: &CommandEnvelope, host: &str, backing: &mut OfflineControlWorkspaceBacking,
    ) -> std::result::Result<OfflineRetainedResult<Option<CommandOutcome>, catalog_commands::CommandStoreError>, OfflineWorkspaceAdmissionRefused> {
        let admission = self.session.admit_control_result::<Option<CommandOutcome>, catalog_commands::CommandStoreError>()?;
        let control = self.session.control_workspace(backing)?;
        let result = (|| {
            self.check_producer_deadline(false)?;
            let outcome = self.session.reconcile_with_control_workspace(command, host, Some(&control))?;
            #[cfg(test)]
            self.wait_for_producer_exit_expiry_for_test(ProducerExitStage::Reconcile);
            self.check_producer_deadline(true)?;
            Ok(outcome)
        })();
        let retained = admission.finish(result);
        drop(control);
        Ok(retained)
    }
    pub fn execute_empty_with_control_workspace(
        &self, command: &CommandEnvelope, host: &str, policy: ReceiptPolicy,
        backing: &mut OfflineControlWorkspaceBacking,
    ) -> std::result::Result<OfflineRetainedResult<CommandOutcome, catalog_commands::CommandStoreError>, OfflineWorkspaceAdmissionRefused> {
        let admission = self.session.admit_control_result::<CommandOutcome, catalog_commands::CommandStoreError>()?;
        let control = self.session.control_workspace(backing)?;
        let result = (|| {
            self.check_producer_deadline(false)?;
            let outcome = self.session.execute_empty_with_control_workspace(command, host, policy, &control)?;
            #[cfg(test)]
            self.wait_for_producer_exit_expiry_for_test(ProducerExitStage::Execute);
            self.check_producer_deadline(true)?;
            Ok(outcome)
        })();
        let retained = admission.finish(result);
        drop(control);
        Ok(retained)
    }
    pub fn transition(&self) -> Result<OfflineTransition<'_>> {
        self.session.transition_description()
    }
    pub fn catalog_id(&self) -> &str {
        self.session.identity().0
    }
    pub fn incarnation(&self) -> &str {
        self.session.identity().1
    }
    pub fn generation(&self) -> Result<i64> {
        self.session.generation()
    }
    pub fn verified_backup_path(&self) -> Result<&Path> {
        self.session.verified_backup_path()
    }
    #[cfg(test)]
    pub(super) fn compact_receipt_for_test(
        &self,
        command: &CommandEnvelope,
        host: &str,
    ) -> catalog_commands::Result<usize> {
        self.session.compact_receipt_for_test(command, host)
    }

    /// Exact receipt lookup comes before the host prepares archive or file data.
    pub fn reconcile(
        &self,
        command: &CommandEnvelope,
        host: &str,
    ) -> catalog_commands::Result<Option<CommandOutcome>> {
        self.check_producer_deadline(false)?;
        let outcome = self.session.reconcile(command, host)?;
        #[cfg(test)]
        self.wait_for_producer_exit_expiry_for_test(ProducerExitStage::Reconcile);
        self.check_producer_deadline(true)?;
        Ok(outcome)
    }
    pub fn prepare(&self, host: &str, evidence: &[HostSourceEvidence<'_>]) -> Result<Value> {
        self.check_producer_deadline(false)?;
        let preparation = self.session.prepare_host(host, evidence)?;
        #[cfg(test)]
        self.wait_for_producer_exit_expiry_for_test(ProducerExitStage::Prepare);
        self.check_producer_deadline(true)?;
        Ok(preparation)
    }
    /// The retained native transaction repeats receipt-first and generation
    /// admission before converting any host descriptions or executing effects.
    /// Marked retained producers check their inherited deadline before and after
    /// this call, without changing Store's budget. Expiry after success returns
    /// an error but does NOT undo a committed receipt or claim rollback; this
    /// borrowed owner remains held and later recovery needs separate authority.
    pub fn execute<'a>(
        &'a self,
        command: &'a CommandEnvelope,
        host: &'a str,
        evidence: &'a [HostSourceEvidence<'a>],
        policy: ReceiptPolicy,
    ) -> catalog_commands::Result<CommandOutcome> {
        self.check_producer_deadline(false)?;
        let outcome = self.session.execute_host(command, host, evidence, policy)?;
        #[cfg(test)]
        self.wait_for_producer_exit_expiry_for_test(ProducerExitStage::Execute);
        self.check_producer_deadline(true)?;
        Ok(outcome)
    }
    pub fn complete(&self, command: &CommandEnvelope) -> Result<BootstrapComplete<'_>> {
        self.require_unadmitted_completion()?;
        Ok(BootstrapComplete {
            inner: self.session.complete(command)?,
        })
    }
}
/// Genuine measured completion, retaining native custody by borrowing its
/// session. Keep this borrow alive until the host's durable ACK commit settles.
pub struct BootstrapComplete<'a> {
    inner: native_session::OfflineBootstrapComplete<'a>,
}
impl BootstrapComplete<'_> {
    pub fn catalog_id(&self) -> &str {
        self.inner.catalog_id()
    }
    pub fn incarnation(&self) -> &str {
        self.inner.incarnation()
    }
    pub fn transition_digest(&self) -> &str {
        self.inner.transition_digest()
    }
    pub fn command_key(&self) -> &CommandKey {
        self.inner.command_key()
    }
    pub fn payload_digest(&self) -> &str {
        self.inner.payload_digest()
    }
    /// Combined complete inventory/effect commitment, not a separate audit hash.
    pub fn inventory_digest(&self) -> &str {
        self.inner.inventory_digest()
    }
    pub fn committed_generation(&self) -> i64 {
        self.inner.committed_generation()
    }
    pub fn state_digest(&self) -> &str {
        self.inner.state_digest()
    }
    pub fn source_count(&self) -> i64 {
        self.inner.source_count()
    }
}

#[cfg(test)]
#[path = "migration_offline_bootstrap_tests.rs"]
mod tests;

impl OfflineBootstrap<'static> {
    /// Consumes already retained host journal bytes; native recovery performs
    /// the identical bounded fingerprint/resource/preservation checks before
    /// this owner retains the plan. No self-referential host bundle is needed.
    pub fn recover_owned(
        path: &Path,
        quiescence: NativeUpgradeQuiescence,
        approved_plan: Value,
        phase: Recovery<'_>,
    ) -> Result<Self> {
        Self::recover_owned_reserved(
            path,
            quiescence,
            approved_plan,
            phase,
            OfflineAllocationReservation::default(),
        )
    }
    pub fn recover_owned_reserved(
        path: &Path,
        quiescence: NativeUpgradeQuiescence,
        approved_plan: Value,
        phase: Recovery<'_>,
        reservation: OfflineAllocationReservation,
    ) -> Result<Self> {
        let owner =
            native_session::RetainedNativeUpgrade::acquire_reserved(path, quiescence, reservation)?;
        let phase = match phase {
            Recovery::Pending { backup_path } => {
                native_session::BootstrapRecovery::Pending { backup_path }
            }
            Recovery::CompletedReceiptOnly { command } => {
                native_session::BootstrapRecovery::CompletedReceiptOnly { command }
            }
        };
        let session = owner.recover_bootstrap(&approved_plan, phase)?;
        Ok(Self {
            session,
            plan: Cow::Owned(approved_plan),
            completion_admission: None,
            producer_deadline: None,
            #[cfg(test)]
            producer_exit_test: None,
        })
    }
}

// Descriptive fixed isolated-image rows, never a restored Store grant.
pub use crate::catalog_source_succession::RestoredSourceEvidence;

pub use crate::catalog_source_succession::{
    CurrentFileEndpoint, FileEndpointRequest, LibraryScopeSnapshot, LibraryScopeSource,
    LibraryScopeSourceFacts,
};

#[cfg(test)]
#[path = "migration_offline_bootstrap/bootstrap_workspace_tests.rs"]
mod bootstrap_workspace_tests;
