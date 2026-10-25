//! Explicit fixed bootstrap borrowers and independently owned result retention.
//! No Store/TLS brand makes a later validator eligible. Every validator needs
//! this live same-Store loan; returned values keep their own additive guard.
use super::*;
use std::{cell::RefCell, marker::PhantomData, sync::atomic::AtomicBool};
#[path = "control_workspace/source_cache_policy.rs"]
mod source_cache_policy;
pub(in crate::migration) use source_cache_policy::SourceCachePolicy;
const OUTPUT_BYTES: u64 = 64 * 1024;
const ERROR_BYTES: u64 = 64 * 1024;
const OUTPUT_FALLBACK: &str = "offline bootstrap retained output exceeded its admitted bound";

/// No producer began. This fixed admission/refresh refusal owns no output
/// allocation; any temporary workspace loan has already ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfflineWorkspaceAdmissionRefused;
impl std::fmt::Display for OfflineWorkspaceAdmissionRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("offline bootstrap workspace admission refused")
    }
}
impl std::error::Error for OfflineWorkspaceAdmissionRefused {}

/// Actual result/error first, independently admitted guard last. Borrow only:
/// no extraction or Clone can separate the pair. The original accounting
/// context survives Native close, and every call owns a new reservation.
pub struct OfflineRetainedResult<T, E = Error> {
    result: std::result::Result<T, E>,
    _retention: allocation::AllocationGuard,
}
impl<T, E> OfflineRetainedResult<T, E> {
    pub fn result(&self) -> &std::result::Result<T, E> { &self.result }
}
impl<T: std::fmt::Debug, E: std::fmt::Debug> std::fmt::Debug for OfflineRetainedResult<T, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfflineRetainedResult").field("result", &self.result).finish_non_exhaustive()
    }
}

/// Owned bounded facts, with no borrow into the closing Native session.
#[derive(Debug)]
pub struct OfflineOwnedTransition {
    digest: String,
    catalog_id: String,
    incarnation: String,
    source_generation: i64,
    approved_plan_sha256: String,
}
impl OfflineOwnedTransition {
    pub fn digest(&self) -> &str { &self.digest }
    pub fn catalog_id(&self) -> &str { &self.catalog_id }
    pub fn incarnation(&self) -> &str { &self.incarnation }
    pub fn source_generation(&self) -> i64 { self.source_generation }
    pub fn approved_plan_sha256(&self) -> &str { &self.approved_plan_sha256 }
}

pub(in crate::migration) trait RetainedOutput { fn retained_bytes(&self) -> Result<u64>; }
impl RetainedOutput for () { fn retained_bytes(&self) -> Result<u64> { Ok(0) } }
impl RetainedOutput for i64 { fn retained_bytes(&self) -> Result<u64> { Ok(0) } }
impl RetainedOutput for Value {
    fn retained_bytes(&self) -> Result<u64> { allocation::value_bytes(self) }
}
impl RetainedOutput for OfflineOwnedTransition {
    fn retained_bytes(&self) -> Result<u64> {
        [self.digest.capacity(), self.catalog_id.capacity(), self.incarnation.capacity(), self.approved_plan_sha256.capacity()]
            .into_iter().try_fold(0u64, |n, capacity| n.checked_add(capacity as u64)
                .ok_or_else(|| Error("offline transition capacity overflow".into())))
    }
}
impl RetainedOutput for CommandOutcome {
    fn retained_bytes(&self) -> Result<u64> {
        match self {
            Self::Committed { result, .. } | Self::Replay { result, .. } => allocation::value_bytes(result),
            _ => Ok(0),
        }
    }
}
impl RetainedOutput for crate::catalog_source_succession::SourceRenewalPreparation {
    fn retained_bytes(&self) -> Result<u64> {
        let strings = [&self.catalog_id, &self.incarnation, &self.profile, &self.source,
            &self.candidate_root, &self.content_commitment];
        let old = allocation::value_bytes(&self.old_binding)?;
        let new = allocation::value_bytes(&self.new_binding)?;
        strings.into_iter().try_fold(0u64, |n, text| n.checked_add(text.capacity() as u64)
            .ok_or_else(|| Error("source preparation capacity overflow".into())))?
            .checked_add(old)
            .and_then(|n| n.checked_add(new))
            .ok_or_else(|| Error("source preparation capacity overflow".into()))
    }
}
impl RetainedOutput for crate::catalog_source_succession::SourceRenewalReceipt {
    fn retained_bytes(&self) -> Result<u64> {
        completed_catalog::retained_command_bytes(self.command())?
            .checked_add(allocation::value_bytes(self.effect_description())?)
            .ok_or_else(|| Error("source receipt capacity overflow".into()))
    }
}
impl<T: RetainedOutput> RetainedOutput for Option<T> {
    fn retained_bytes(&self) -> Result<u64> { self.as_ref().map_or(Ok(0), RetainedOutput::retained_bytes) }
}
pub(in crate::migration) trait RetainedError {
    fn fallback() -> Self;
    fn capacity(&self) -> u64;
}
impl RetainedError for Error {
    fn fallback() -> Self { Error(OUTPUT_FALLBACK.into()) }
    fn capacity(&self) -> u64 { self.0.capacity() as u64 }
}
impl RetainedError for catalog_commands::CommandStoreError {
    fn fallback() -> Self {
        Self { code: catalog_commands::CommandStoreErrorCode::StoreFailure, error: Error::fallback() }
    }
    fn capacity(&self) -> u64 { self.error.0.capacity() as u64 }
}
pub(in crate::migration) struct RetainedAdmission<T, E> {
    fallback: E,
    _output: PhantomData<T>,
    retention: allocation::AllocationGuard,
}
impl<T: RetainedOutput, E: RetainedError> RetainedAdmission<T, E> {
    /// Only while the explicit loan is held. Oversized output is destroyed
    /// before releasing it, and the replacement fallback already exists.
    pub(in crate::migration) fn finish(self, result: std::result::Result<T, E>) -> OfflineRetainedResult<T, E> {
        let mut retained = OfflineRetainedResult { result, _retention: self.retention };
        // Declared after the wrapper: an unwind drops fallback before the
        // wrapper's guard, which separately prepays both live allocations.
        let fallback = self.fallback;
        let accepted = match &retained.result {
            Ok(value) => value.retained_bytes().is_ok_and(|bytes| bytes <= OUTPUT_BYTES),
            Err(error) => error.capacity() <= ERROR_BYTES,
        };
        if accepted {
            drop(fallback);
        } else {
            let oversized = std::mem::replace(&mut retained.result, Err(fallback));
            drop(oversized);
        }
        retained
    }
}

/// Nonconstructible outside this owner. Shared references are synchronous
/// plumbing only; RefCell refuses nested metadata rather than lending twice.
pub(crate) struct BootstrapWorkspace<'a> {
    store: &'a Store,
    loan: RefCell<allocation::ControlWorkspaceLoan<'a>>,
}
impl BootstrapWorkspace<'_> {
    pub(crate) fn validate(&self, store: &Store) -> Result<()> {
        if !std::ptr::eq(self.store, store) || store.schema_version != 41 || !store.writable || store.lock.is_none() {
            return Err(Error("offline workspace Store differs".into()));
        }
        let identity = store.offline_bootstrap.as_ref().ok_or_else(|| Error("offline identity missing".into()))?;
        let mut loan = self.loan.try_borrow_mut().map_err(|_| Error("offline workspace already borrowed".into()))?;
        identity.validate_retained_parts_with_control_workspace(
            &store.db, &store.path, store.native_path_identity, store.native_admission.as_ref(),
            &store.database_id, store.catalog_incarnation.as_deref(), Some(&mut loan),
        )
    }
    fn owned_transition(&self) -> Result<OfflineOwnedTransition> {
        self.validate(self.store)?;
        let identity = self.store.offline_bootstrap.as_ref().ok_or_else(|| Error("offline identity missing".into()))?;
        let mut loan = self.loan.try_borrow_mut().map_err(|_| Error("offline workspace already borrowed".into()))?;
        let _metadata = allocation::MetadataReservation::new(&identity.allocation, &self.store.db, Some(&mut loan))?;
        let row = &identity.transition;
        Ok(OfflineOwnedTransition { digest: row.digest()?, catalog_id: row.catalog_id.clone(),
            incarnation: row.source_incarnation.clone(), source_generation: row.source_generation,
            approved_plan_sha256: row.approved_plan_sha256.clone() })
    }
    pub(super) fn transition_digest(&self) -> Result<String> {
        let identity = self.store.offline_bootstrap.as_ref().ok_or_else(|| Error("offline identity missing".into()))?;
        let mut loan = self.loan.try_borrow_mut().map_err(|_| Error("offline workspace already borrowed".into()))?;
        let _metadata = allocation::MetadataReservation::new(&identity.allocation, &self.store.db, Some(&mut loan))?;
        identity.transition.digest()
    }
}
impl OfflineBootstrapSession {
    pub(in crate::migration) fn admit_control_result<T: RetainedOutput, E: RetainedError>(
        &self,
    ) -> std::result::Result<RetainedAdmission<T, E>, OfflineWorkspaceAdmissionRefused> {
        let identity = self.store.offline_bootstrap.as_ref().ok_or(OfflineWorkspaceAdmissionRefused)?;
        let slots = (std::mem::size_of::<OfflineRetainedResult<T, E>>() + std::mem::size_of::<RetainedAdmission<T, E>>()) as u64;
        let mut retention = identity.allocation.reserve(&self.store.db,
            OUTPUT_BYTES + ERROR_BYTES + OUTPUT_FALLBACK.len() as u64 + slots)
            .map_err(|error| { drop(error); OfflineWorkspaceAdmissionRefused })?;
        let fallback = E::fallback();
        if fallback.capacity() > OUTPUT_FALLBACK.len() as u64 {
            retention.grow(&self.store.db, fallback.capacity() - OUTPUT_FALLBACK.len() as u64)
                .map_err(|error| { drop(error); OfflineWorkspaceAdmissionRefused })?;
        }
        Ok(RetainedAdmission { fallback, _output: PhantomData, retention })
    }
    pub(in crate::migration) fn control_workspace<'a>(
        &'a self, backing: &'a mut allocation::OfflineControlWorkspaceBacking,
    ) -> std::result::Result<BootstrapWorkspace<'a>, OfflineWorkspaceAdmissionRefused> {
        let identity = self.store.offline_bootstrap.as_ref().ok_or(OfflineWorkspaceAdmissionRefused)?;
        let loan = backing.loan(&identity.allocation, &self.store.db)
            .map_err(|error| { drop(error); OfflineWorkspaceAdmissionRefused })?;
        Ok(BootstrapWorkspace { store: &self.store, loan: RefCell::new(loan) })
    }
    pub(in crate::migration) fn owned_transition_with_control_workspace(&self, control: &BootstrapWorkspace<'_>) -> Result<OfflineOwnedTransition> {
        if !std::ptr::eq(&self.store, control.store) { return Err(Error("offline workspace Store differs".into())); }
        control.owned_transition()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

    fn original(source: &Connection) -> (allocation::OfflineControlWorkspaceBacking, allocation::AllocationContext) {
        let mut backing = allocation::OfflineControlWorkspaceBacking::new();
        let context = backing.admit(allocation::OfflineAllocationReservation::default()).unwrap();
        context.bind_source(source).unwrap();
        (backing, context)
    }
    fn admission<T, E: RetainedError>(source: &Connection, context: &allocation::AllocationContext) -> RetainedAdmission<T, E> {
        let retention = context.reserve(source, OUTPUT_BYTES + ERROR_BYTES + 4096).unwrap();
        RetainedAdmission { fallback: E::fallback(), _output: PhantomData, retention }
    }

    #[test]
    fn bounded_errors_preserve_pointer_capacity_and_command_code_after_source_close() {
        let source = Connection::open_in_memory().unwrap();
        let (mut backing, context) = original(&source);
        let admission = admission::<(), catalog_commands::CommandStoreError>(&source, &context);
        let loan = backing.loan(&context, &source).unwrap();
        let mut text = String::with_capacity(ERROR_BYTES as usize);
        text.push_str("unchanged exact native command error");
        let pointer = text.as_ptr();
        let retained = admission.finish(Err(catalog_commands::CommandStoreError {
            code: catalog_commands::CommandStoreErrorCode::AuthorityDenied,
            error: Error(text),
        }));
        drop(loan);
        assert!(!backing.state_for_test().1);
        drop(source);
        drop(backing);
        let error = retained.result().as_ref().unwrap_err();
        assert_eq!(error.code, catalog_commands::CommandStoreErrorCode::AuthorityDenied);
        assert_eq!(error.error.0.as_ptr(), pointer);
        assert_eq!(error.error.0.capacity(), ERROR_BYTES as usize);
        assert!(context.temporary_bytes_for_test().unwrap() >= OUTPUT_BYTES + ERROR_BYTES);
        drop(retained);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
    }

    #[test]
    fn oversized_value_and_error_move_existing_fallback_while_original_loan_is_held() {
        let source = Connection::open_in_memory().unwrap();
        let (mut backing, context) = original(&source);
        let value_admission = admission::<Value, Error>(&source, &context);
        let error_admission = admission::<(), Error>(&source, &context);
        let value_fallback = value_admission.fallback.0.as_ptr();
        let error_fallback = error_admission.fallback.0.as_ptr();
        let loan = backing.loan(&context, &source).unwrap();
        let mut oversized = String::with_capacity(OUTPUT_BYTES as usize + 1);
        oversized.push_str("small length does not conceal large capacity");
        let value = value_admission.finish(Ok(Value::String(oversized)));
        let mut oversized = String::with_capacity(ERROR_BYTES as usize + 1);
        oversized.push_str("small error with oversized backing allocation");
        let error = error_admission.finish(Err(Error(oversized)));
        assert_eq!(value.result().as_ref().unwrap_err().0.as_ptr(), value_fallback);
        assert_eq!(error.result().as_ref().unwrap_err().0.as_ptr(), error_fallback);
        assert_eq!(error.result().as_ref().unwrap_err().0, OUTPUT_FALLBACK);
        drop(loan);
        assert_eq!(backing.state_for_test(), (allocation::OfflineControlWorkspaceBacking::BYTES, false, false));
        let both = context.temporary_bytes_for_test().unwrap();
        drop(value);
        assert!(context.temporary_bytes_for_test().unwrap() < both);
        assert!(context.temporary_bytes_for_test().unwrap() >= OUTPUT_BYTES + ERROR_BYTES);
        drop(error);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
    }

    struct PanicCapacity {
        context: allocation::AllocationContext,
        dropped: Arc<AtomicBool>,
    }
    impl RetainedOutput for PanicCapacity {
        fn retained_bytes(&self) -> Result<u64> { panic!("controlled output capacity unwind") }
    }
    impl Drop for PanicCapacity {
        fn drop(&mut self) {
            assert!(self.context.temporary_bytes_for_test().unwrap() >= OUTPUT_BYTES + ERROR_BYTES);
            self.dropped.store(true, Ordering::Release);
        }
    }
    #[test]
    fn installed_result_drops_before_its_guard_on_capacity_check_unwind() {
        let source = Connection::open_in_memory().unwrap();
        let (mut backing, context) = original(&source);
        let dropped = Arc::new(AtomicBool::new(false));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let admission = admission::<PanicCapacity, Error>(&source, &context);
            let _loan = backing.loan(&context, &source).unwrap();
            let _retained = admission.finish(Ok(PanicCapacity { context: context.clone(), dropped: dropped.clone() }));
        }));
        assert!(result.is_err());
        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
        assert_eq!(backing.state_for_test(), (allocation::OfflineControlWorkspaceBacking::BYTES, false, false));
    }
    pub(super) fn session_with_original_backing() -> (tempfile::TempDir, OfflineBootstrapSession, allocation::OfflineControlWorkspaceBacking) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        initialize_native_at(&path, 40).unwrap();
        let mut backing = allocation::OfflineControlWorkspaceBacking::new();
        let context = backing.admit(allocation::OfflineAllocationReservation::default()).unwrap();
        let mut owner = RetainedNativeUpgrade::acquire(&path, NativeUpgradeQuiescence {
            admission_stopped: true, readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        }).unwrap();
        context.bind_source(&owner.db).unwrap();
        owner.allocation = Some(context);
        let (owner, plan) = owner.plan_closed41().unwrap();
        let (owner, _) = owner.apply_closed41(&plan, None, &|_, _, _| Ok(())).unwrap();
        (root, owner.into_bootstrap().unwrap(), backing)
    }
    #[test]
    fn explicit_bootstrap_workspace_refuses_foreign_store_and_nested_validator() {
        let (_root, session, mut backing) = session_with_original_backing();
        let (_foreign_root, foreign, mut foreign_backing) = session_with_original_backing();
        assert!(session.control_workspace(&mut foreign_backing).is_err());
        let control = session.control_workspace(&mut backing).unwrap();
        assert!(control.validate(&foreign.store).is_err());
        let held = control.loan.borrow_mut();
        assert!(control.validate(&session.store).is_err());
        drop(held);
        control.validate(&session.store).unwrap();
        control.validate(&session.store).unwrap();
        drop(control);
        assert_eq!(backing.state_for_test(), (allocation::OfflineControlWorkspaceBacking::BYTES, false, false));
        session.store.validate_native_identity().unwrap();
    }
    #[test]
    fn bootstrap_metadata_unwind_releases_both_borrows_without_releasing_external_charge() {
        let (_root, session, mut backing) = session_with_original_backing();
        let context = session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
        let before = context.temporary_bytes_for_test().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let control = session.control_workspace(&mut backing).unwrap();
            let mut loan = control.loan.borrow_mut();
            let _metadata = allocation::MetadataReservation::new(&context, &session.store.db, Some(&mut loan)).unwrap();
            panic!("controlled bootstrap metadata unwind");
        }));
        assert!(result.is_err());
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before);
        assert_eq!(backing.state_for_test(), (allocation::OfflineControlWorkspaceBacking::BYTES, false, false));
        let control = session.control_workspace(&mut backing).unwrap();
        control.validate(&session.store).unwrap();
    }

}

#[cfg(test)]
mod source_output_tests {
    use super::*;

    #[test]
    fn preparation_output_measures_deep_capacity_and_preserves_bounded_original_without_extraction() {
        let source = Connection::open_in_memory().unwrap();
        let mut backing = allocation::OfflineControlWorkspaceBacking::new();
        let context = backing.admit(OfflineAllocationReservation::default()).unwrap();
        context.bind_source(&source).unwrap();
        let make = |new_binding| crate::catalog_source_succession::SourceRenewalPreparation {
            catalog_id: "catalog".into(), incarnation: "incarnation".into(), expected_generation: 3,
            profile: "default".into(), source: "source".into(), candidate_root: "/candidate".into(),
            content_commitment: "a".repeat(64), old_binding: serde_json::json!({"root":"/old"}), new_binding,
        };
        let admit = || {
            let retention = context.reserve(&source, OUTPUT_BYTES + ERROR_BYTES + 4096).unwrap();
            RetainedAdmission::<crate::catalog_source_succession::SourceRenewalPreparation, Error> {
                fallback: Error::fallback(), _output: PhantomData, retention,
            }
        };
        let valid = make(serde_json::json!({"root":"/candidate"}));
        let pointer = valid.content_commitment.as_ptr();
        let capacity = valid.content_commitment.capacity();
        let valid_admission = admit();
        let oversized_admission = admit();
        let fallback = oversized_admission.fallback.0.as_ptr();
        let loan = backing.loan(&context, &source).unwrap();
        let retained = valid_admission.finish(Ok(valid));
        let mut spare = String::with_capacity(OUTPUT_BYTES as usize + 1);
        spare.push('x');
        let mut nested = serde_json::Map::new();
        nested.insert("nested".into(), Value::Array(vec![Value::String(spare)]));
        let oversized = oversized_admission.finish(Ok(make(Value::Object(nested))));
        assert_eq!(retained.result().as_ref().unwrap().content_commitment.as_ptr(), pointer);
        assert_eq!(retained.result().as_ref().unwrap().content_commitment.capacity(), capacity);
        match oversized.result() {
            Err(error) => assert_eq!(error.0.as_ptr(), fallback),
            Ok(_) => panic!("deep spare capacity must not escape its output bound"),
        }
        drop(loan);
        drop(source);
        drop(backing);
        assert!(context.temporary_bytes_for_test().unwrap() >= 2 * (OUTPUT_BYTES + ERROR_BYTES));
        assert_eq!(retained.result().as_ref().unwrap().content_commitment.as_ptr(), pointer);
        drop(oversized);
        assert!(context.temporary_bytes_for_test().unwrap() >= OUTPUT_BYTES + ERROR_BYTES);
        drop(retained);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
    }
}


/// Independently paid inventory payload. Declaration order is intentional:
/// destroy the real map and strings before releasing their allocation guard.
pub(crate) struct CurrentImageInventory {
    pub(crate) audit: crate::catalog_backup::inventory::Audit,
    pub(crate) retention: Option<allocation::AllocationGuard>,
}
impl CurrentImageInventory {
    pub(crate) fn ordinary(audit: crate::catalog_backup::inventory::Audit) -> Self {
        Self { audit, retention: None }
    }
}
impl BootstrapWorkspace<'_> {
    pub(super) fn from_capture_loan<'a>(store: &'a Store, loan: allocation::ControlWorkspaceLoan<'a>) -> BootstrapWorkspace<'a> {
        BootstrapWorkspace { store, loan: RefCell::new(loan) }
    }
    pub(super) fn capture_source_inventory(
        &self, cancel: &AtomicBool, deadline: std::time::Instant,
    ) -> Result<CurrentImageInventory> {
        self.capture_inventory(&self.store.db, cancel, deadline)
    }
    /// Only the existing copier can construct this target, after accounting
    /// its actual copied DBSTATUS and admitted image phase. The scratch remains
    /// bound to the ORIGINAL Store/context, never the foreign image handle.
    pub(crate) fn capture_paid_current_inventory(
        &self, target: &crate::catalog_restore_lineage::completed_image::PaidCurrentImageInventory<'_>,
        cancel: &AtomicBool, deadline: std::time::Instant,
    ) -> Result<CurrentImageInventory> {
        if !std::ptr::eq(self.store, target.source()) {
            return Err(Error("current image inventory source differs".into()));
        }
        self.capture_inventory(target.image(), cancel, deadline)
    }
    pub(super) fn validate_current_image_source(
        &self, proof: &CurrentImageProof<'_, '_>,
    ) -> Result<()> {
        let identity = self.store.offline_bootstrap.as_ref()
            .ok_or_else(|| Error("current image source owner missing".into()))?;
        let mut loan = self.loan.try_borrow_mut()
            .map_err(|_| Error("current image source workspace already borrowed".into()))?;
        let _scratch = allocation::MetadataReservation::new(
            &identity.allocation, &self.store.db, Some(&mut loan),
        )?;
        proof.validate_closed_copy(&self.store.db)
    }
    pub(crate) fn validate_paid_current_image(
        &self, target: &crate::catalog_restore_lineage::completed_image::PaidCurrentImageInventory<'_>,
        proof: &CurrentImageProof<'_, '_>,
    ) -> Result<()> {
        if !std::ptr::eq(self.store, target.source()) {
            return Err(Error("current image validation source differs".into()));
        }
        let identity = self.store.offline_bootstrap.as_ref()
            .ok_or_else(|| Error("current image validation owner missing".into()))?;
        let mut loan = self.loan.try_borrow_mut()
            .map_err(|_| Error("current image validation workspace already borrowed".into()))?;
        let _scratch = allocation::MetadataReservation::new(
            &identity.allocation, &self.store.db, Some(&mut loan),
        )?;
        target.validate(proof)
    }
    /// Fixed original-source loan around the existing one-step backup call.
    /// The separate full-image growth reserve must already be live. All page
    /// payloads remain charged outside metadata; only bookkeeping uses scratch.
    pub(crate) fn step_owned_current_copy(
        &self, target: &crate::catalog_restore_lineage::completed_image::PaidCurrentBackup<'_, '_, '_>,
        phase: &mut OfflineImageAllocation<'_>, bytes: u64,
    ) -> Result<rusqlite::backup::StepResult> {
        if !std::ptr::eq(self.store, target.source()) { return Err(Error("current backup source differs".into())); }
        let identity = self.store.offline_bootstrap.as_ref().ok_or_else(|| Error("current image allocation missing".into()))?;
        let mut loan = self.loan.try_borrow_mut().map_err(|_| Error("current image metadata already borrowed".into()))?;
        let _scratch = allocation::MetadataReservation::new(&identity.allocation, &self.store.db, Some(&mut loan))?;
        target.step_and_account(phase, bytes)
    }
    pub(crate) fn configure_owned_current_copy(
        &self, target: &crate::catalog_restore_lineage::completed_image::PaidCurrentImageInventory<'_>,
        phase: &mut OfflineImageAllocation<'_>, bytes: u64,
    ) -> Result<()> {
        if !std::ptr::eq(self.store, target.source()) { return Err(Error("current image source differs".into())); }
        let identity = self.store.offline_bootstrap.as_ref().ok_or_else(|| Error("current image allocation missing".into()))?;
        let mut loan = self.loan.try_borrow_mut().map_err(|_| Error("current image metadata already borrowed".into()))?;
        let _scratch = allocation::MetadataReservation::new(&identity.allocation, &self.store.db, Some(&mut loan))?;
        target.configure()?;
        phase.admit(bytes.checked_add(crate::catalog_backup::allocations(target.image())?).ok_or_else(|| Error("current image allocation overflow".into()))?)
    }
    pub(crate) fn attach_owned_current_copy(
        &self, target: &mut crate::catalog_restore_lineage::completed_image::PaidCurrentImageAttachment<'_>, data: rusqlite::serialize::OwnedData,
        phase: &mut OfflineImageAllocation<'_>, bytes: u64,
    ) -> Result<()> {
        if !std::ptr::eq(self.store, target.source()) { return Err(Error("current image source differs".into())); }
        let identity = self.store.offline_bootstrap.as_ref().ok_or_else(|| Error("current image allocation missing".into()))?;
        let mut loan = self.loan.try_borrow_mut().map_err(|_| Error("current image metadata already borrowed".into()))?;
        let _scratch = allocation::MetadataReservation::new(&identity.allocation, &self.store.db, Some(&mut loan))?;
        target.attach_and_account(data, phase, bytes)
    }
    pub(crate) fn validate_owned_current_copy(
        &self, target: &crate::catalog_restore_lineage::completed_image::PaidCurrentImageInventory<'_>,
        proof: &CurrentImageProof<'_, '_>, phase: &mut OfflineImageAllocation<'_>, bytes: u64,
    ) -> Result<()> {
        if !std::ptr::eq(self.store, target.source()) { return Err(Error("current image source differs".into())); }
        let identity = self.store.offline_bootstrap.as_ref().ok_or_else(|| Error("current image allocation missing".into()))?;
        let mut loan = self.loan.try_borrow_mut().map_err(|_| Error("current image metadata already borrowed".into()))?;
        let _scratch = allocation::MetadataReservation::new(&identity.allocation, &self.store.db, Some(&mut loan))?;
        target.validate(proof)?;
        phase.admit(bytes.checked_add(crate::catalog_backup::allocations(target.image())?).ok_or_else(|| Error("current image allocation overflow".into()))?)
    }
    pub(crate) fn inventory_owned_current_copy(
        &self, target: &crate::catalog_restore_lineage::completed_image::PaidCurrentImageInventory<'_>,
        phase: &mut OfflineImageAllocation<'_>, bytes: u64, cancel: &AtomicBool, deadline: std::time::Instant,
    ) -> Result<CurrentImageInventory> {
        if !std::ptr::eq(self.store, target.source()) { return Err(Error("current image source differs".into())); }
        let identity = self.store.offline_bootstrap.as_ref().ok_or_else(|| Error("current image allocation missing".into()))?;
        let mut loan = self.loan.try_borrow_mut().map_err(|_| Error("current image metadata already borrowed".into()))?;
        let _scratch = allocation::MetadataReservation::new(&identity.allocation, &self.store.db, Some(&mut loan))?;
        let audit = crate::catalog_backup::inventory::capture(target.image(), cancel, deadline)?;
        phase.admit(bytes.checked_add(crate::catalog_backup::allocations(target.image())?).ok_or_else(|| Error("current image allocation overflow".into()))?)?;
        let retention = identity.allocation.reserve(&self.store.db, audit.retained_allocations()?)?;
        Ok(CurrentImageInventory { audit, retention: Some(retention) })
    }
    /// Fixed serializer only. The caller has already paid the separate full
    /// output buffer and exact copied DBSTATUS; this original-source loan covers
    /// only SQLite's page-count statement/metadata while serializing.
    pub(crate) fn serialize_paid_current_owned(
        &self, target: &crate::catalog_restore_lineage::completed_image::PaidCurrentImageInventory<'_>,
        expected: u64, phase: &mut OfflineImageAllocation<'_>,
    ) -> Result<(rusqlite::serialize::OwnedData, u64)> {
        if !std::ptr::eq(self.store, target.source()) { return Err(Error("current serialization source differs".into())); }
        let identity = self.store.offline_bootstrap.as_ref().ok_or_else(|| Error("current image allocation missing".into()))?;
        let mut loan = self.loan.try_borrow_mut().map_err(|_| Error("current image metadata already borrowed".into()))?;
        let _scratch = allocation::MetadataReservation::new(&identity.allocation, &self.store.db, Some(&mut loan))?;
        let data = target.image().serialize(rusqlite::MAIN_DB)?;
        let len = u64::try_from(data.len()).map_err(|_| Error("current serialized length overflow".into()))?;
        if len != expected { return Err(Error("current serialized image length changed".into())); }
        phase.admit(expected.checked_add(crate::catalog_backup::allocations(target.image())?).ok_or_else(|| Error("current serialization allocation overflow".into()))?)?;
        crate::catalog_restore_lineage::completed_image::take_owned_current_serialization(data, expected)
    }
    fn capture_inventory(
        &self, db: &Connection, cancel: &AtomicBool, deadline: std::time::Instant,
    ) -> Result<CurrentImageInventory> {
        let identity = self.store.offline_bootstrap.as_ref()
            .ok_or_else(|| Error("current image inventory owner missing".into()))?;
        let mut loan = self.loan.try_borrow_mut()
            .map_err(|_| Error("current image inventory workspace already borrowed".into()))?;
        let _scratch = allocation::MetadataReservation::new(
            &identity.allocation, &self.store.db, Some(&mut loan),
        )?;
        // Audit is declared after scratch, so a refused measurement/reservation
        // destroys the output before the producing scratch releases on unwind.
        let audit = crate::catalog_backup::inventory::capture(db, cancel, deadline)?;
        let bytes = audit.retained_allocations()?;
        let retention = identity.allocation.reserve(&self.store.db, bytes)?;
        Ok(CurrentImageInventory { audit, retention: Some(retention) })
    }
}


#[cfg(test)]
mod current_inventory_tests {
    use super::*;
    #[test]
    fn current_inventory_uses_exclusive_original_loan_and_separate_output_guard() {
        let (_root, session, mut backing) = super::tests::session_with_original_backing();
        let context = session.store.offline_bootstrap.as_ref().unwrap().allocation.clone();
        let original = context.preparation_state_for_test().unwrap();
        let external = context.external_retained_bytes().unwrap();
        let before = context.temporary_bytes_for_test().unwrap();
        let control = session.control_workspace(&mut backing).unwrap();
        let held = control.loan.borrow_mut();
        assert!(control.capture_source_inventory(&AtomicBool::new(false), std::time::Instant::now() + std::time::Duration::from_secs(60)).is_err());
        drop(held);
        let audit = control.capture_source_inventory(&AtomicBool::new(false), std::time::Instant::now() + std::time::Duration::from_secs(60)).unwrap();
        let bytes = audit.audit.retained_allocations().unwrap();
        assert!(audit.retention.is_some());
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before + bytes);
        assert_eq!(context.preparation_state_for_test().unwrap(), original);
        drop(control);
        assert_eq!(backing.state_for_test(), (external, false, false));
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before + bytes);
        drop(audit);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before);
    }
}
