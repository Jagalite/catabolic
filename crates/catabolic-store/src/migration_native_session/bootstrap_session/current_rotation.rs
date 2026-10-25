//! Fixed current-image rotation. Original post94 custody is never rotated.
use super::*;
use crate::catalog_identity_mutation::StructuralReadBudget;
use crate::catalog_restore_lineage::{
    AdmittedRestoreHistory,
    completed_image::{RotatedCompletedImage, RotationFault, ValidatedCompletedImage},
};
use crate::operation_budget::BudgetCharge;

pub struct CurrentRestoreHostAttestation {
    pub pair_sha256: String,
    pub motion_binding_sha256: String,
    pub catalog_id: String,
    pub incarnation: String,
    pub generation: i64,
    pub current_inventory_sha256: String,
    pub transition_digest: String,
}
pub(crate) struct CurrentImageRotationAdmission<'a> {
    owner: &'a CompletedCatalogSession,
    history: &'a CapturedSourceHistory,
    generation: i64,
    deadline: Instant,
}
impl CurrentImageRotationAdmission<'_> {
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    fn live(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            Err(Error("current rotation deadline exceeded".into()))
        } else {
            Ok(())
        }
    }

    pub(crate) fn image_allocation(&self) -> Result<OfflineImageAllocation<'_>> {
        self.owner.session.image_allocation()
    }
    pub(crate) fn validate_source(&self, store: &Store) -> Result<()> {
        self.live()?;
        if !std::ptr::eq(store, &self.owner.session.store) {
            return Err(Error("current rotation owner differs".into()));
        }
        self.owner.validate_current()?;
        self.validate_closed_copy(&store.db)
    }
    pub(crate) fn validate_closed_copy(&self, db: &Connection) -> Result<()> {
        closed41::Closed41Target.validate(db)?;
        self.validate_closed_fields(db)
    }
    pub(crate) fn validate_planning_source(
        &self,
        store: &Store,
        observer: &crate::catalog_source_succession::authorizer::PlannedReadObserver,
    ) -> Result<()> {
        self.live()?;
        if !std::ptr::eq(store, &self.owner.session.store) {
            return Err(Error("current planning owner differs".into()));
        }
        observer.check(&store.db)?;
        self.owner.validate_current()?;
        self.validate_planning_copy(&store.db, observer)
    }
    pub(crate) fn validate_planning_copy(
        &self,
        db: &Connection,
        observer: &crate::catalog_source_succession::authorizer::PlannedReadObserver,
    ) -> Result<()> {
        closed41::Closed41Target.validate_observed_plan(db, observer)?;
        self.validate_closed_fields(db)
    }
    fn validate_closed_fields(&self, db: &Connection) -> Result<()> {
        let row = transition::read_in(db)?
            .ok_or_else(|| Error("current rotation transition missing".into()))?;
        let identity: (String, String, i64) = db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if identity
            != (
                self.owner.session.store.database_id.clone(),
                self.owner
                    .session
                    .store
                    .catalog_incarnation
                    .clone()
                    .unwrap_or_default(),
                self.generation,
            )
            || row.digest()? != self.owner.transition_digest
        {
            return Err(Error("current rotation identity changed".into()));
        }
        Ok(())
    }
    pub(crate) fn validate_restored_copy(
        &self,
        db: &Connection,
        proof: &AdmittedRestoreHistory<'_>,
    ) -> Result<crate::catalog_source_succession::RetainedRestoredSourceHistory> {
        proof.check(db)?;
        closed41::Closed41Target.validate(db)?;
        let row =
            transition::read_in(db)?.ok_or_else(|| Error("restored transition missing".into()))?;
        if row.digest()? != self.owner.transition_digest
            || proof.transition_digest() != self.owner.transition_digest
            || proof.generation() != self.generation
        {
            return Err(Error("restored transition commitment changed".into()));
        }
        proof.authorize_historical(&row.source_incarnation, row.migrated_generation)?;
        let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(
            self.owner.session.store.catalog_operation_budget()?,
        );
        let receipt = crate::catalog_nfo_attribution::verify_restored_historical_receipt_in(
            db,
            proof,
            &self.owner.bootstrap,
            &mut budget,
        )?;
        if receipt.inventory_digest != self.owner.inventory_digest
            || receipt.source_count != self.owner.source_count
            || receipt.committed_generation != self.owner.committed_generation
        {
            return Err(Error("restored historical bootstrap differs".into()));
        }
        let restored = crate::catalog_source_succession::capture_restored_source_history_in(
            db,
            proof,
            &mut budget,
        )?;
        restored.into_read_witness(db, proof, self, &mut budget)
    }
    pub(crate) fn require_source_effect_equality(
        &self,
        effects: &[Value],
        budget: &mut crate::catalog_nfo_attribution::ProofBudget,
    ) -> Result<()> {
        self.live()?;
        if effects.len() != self.history.effects().len() {
            return Err(Error("restored source effect count differs".into()));
        }
        for (old, new) in self.history.effects().iter().zip(effects) {
            if !equal_effect(old, new, budget, 0)? {
                return Err(Error("restored source effect changed".into()));
            }
        }
        Ok(())
    }
}
/// Compare borrowed effect trees directly. No duplicate JSON serialization or
/// owned representation is manufactured merely to account comparison work.
fn equal_effect(
    a: &Value,
    b: &Value,
    budget: &mut crate::catalog_nfo_attribution::ProofBudget,
    depth: usize,
) -> Result<bool> {
    use crate::catalog_identity_mutation::StructuralReadBudget;
    use crate::operation_budget::BudgetCharge;
    if depth > 64 {
        return Err(Error("restored effect equality depth exceeded".into()));
    }
    let bytes = match (a, b) {
        (Value::String(a), Value::String(b)) => a.len().checked_add(b.len()),
        _ => Some(16),
    }
    .ok_or_else(|| Error("restored effect equality overflow".into()))?;
    StructuralReadBudget::charge(
        budget,
        BudgetCharge {
            bytes,
            values: 2,
            work: 2 + bytes.div_ceil(64),
            ..Default::default()
        },
    )?;
    match (a, b) {
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Ok(false);
            };
            for (a, b) in a.iter().zip(b) {
                if !equal_effect(a, b, budget, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::Object(a), Value::Object(b)) => {
            // Equal paired keys establish their own alignment. Do not assume
            // serde_json iteration order or allocate/sort an already aligned
            // representation. A late order difference retains ALL work already
            // spent and falls back to the complete order-independent comparison.
            if let Some(equal) = aligned_effect_entries(
                a.iter().map(|(key, value)| (key.as_str(), value)),
                b.iter().map(|(key, value)| (key.as_str(), value)),
                budget,
                depth,
            )? {
                return Ok(equal);
            }
            sorted_effect_entries_equal(
                a.iter().map(|(key, value)| (key.as_str(), value)),
                b.iter().map(|(key, value)| (key.as_str(), value)),
                budget,
                depth,
            )
        }
        _ => Ok(a == b),
    }
}
type EffectEntry<'a> = (&'a str, &'a Value);

/// A checked fast path, not an ordering invariant. Production iterators come
/// from maps with unique keys. An equal key with a different value disproves
/// equality regardless of subsequent order; a different key requires fallback.
/// The explicit iterator seam also tests order divergence when serde_json's
/// configured map implementation normalizes insertion order.
fn aligned_effect_entries<'a, 'b>(
    left: impl ExactSizeIterator<Item = EffectEntry<'a>>,
    right: impl ExactSizeIterator<Item = EffectEntry<'b>>,
    budget: &mut crate::catalog_nfo_attribution::ProofBudget,
    depth: usize,
) -> Result<Option<bool>> {
    budget.charge(BudgetCharge {
        work: 1,
        ..Default::default()
    })?;
    if left.len() != right.len() {
        return Ok(Some(false));
    }
    for ((key, value), (other_key, other)) in left.zip(right) {
        if compare_effect_keys(key, other_key, budget)? != std::cmp::Ordering::Equal {
            return Ok(None);
        }
        if !equal_effect(value, other, budget, depth + 1)? {
            return Ok(Some(false));
        }
    }
    Ok(Some(true))
}

/// Complete fallback for any differing iteration order. The failed aligned
/// attempt is never refunded, and each array, comparison and move is admitted.
fn sorted_effect_entries_equal<'a, 'b>(
    a: impl ExactSizeIterator<Item = EffectEntry<'a>>,
    b: impl ExactSizeIterator<Item = EffectEntry<'b>>,
    budget: &mut crate::catalog_nfo_attribution::ProofBudget,
    depth: usize,
) -> Result<bool> {
    budget.charge(BudgetCharge {
        work: 1,
        ..Default::default()
    })?;
    if a.len() != b.len() {
        return Ok(false);
    }
    // Materialize only borrowed entries, never Value/key clones. Admit
    // BOTH arrays and the single swap temporary before either allocation.
    // No dependency on serde_json's map implementation or iteration order.
    let entries = a
        .len()
        .checked_mul(2)
        .ok_or_else(|| Error("restored effect entry overflow".into()))?;
    let bytes = entries
        .checked_add(1)
        .and_then(|n| n.checked_mul(std::mem::size_of::<EffectEntry<'_>>()))
        .ok_or_else(|| Error("restored effect entry bytes overflow".into()))?;
    StructuralReadBudget::charge(
        budget,
        BudgetCharge {
            bytes,
            values: entries,
            items: 2,
            work: entries
                .checked_add(bytes.div_ceil(64))
                .and_then(|n| n.checked_add(1))
                .ok_or_else(|| Error("restored effect entry work overflow".into()))?,
        },
    )?;
    let mut left = Vec::with_capacity(a.len());
    let mut right = Vec::with_capacity(b.len());
    for (key, value) in a {
        left.push((key, value));
    }
    for (key, value) in b {
        right.push((key, value));
    }
    sort_effect_entries(&mut left, budget)?;
    sort_effect_entries(&mut right, budget)?;
    for ((key, value), (other_key, other)) in left.iter().zip(&right) {
        if compare_effect_keys(key, other_key, budget)? != std::cmp::Ordering::Equal
            || !equal_effect(value, other, budget, depth + 1)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn compare_effect_keys(
    a: &str,
    b: &str,
    budget: &mut impl StructuralReadBudget,
) -> crate::Result<std::cmp::Ordering> {
    let bytes = a
        .len()
        .checked_add(b.len())
        .ok_or_else(|| Error("restored effect key bytes overflow".into()))?;
    budget.charge(BudgetCharge {
        bytes,
        values: 2,
        work: 2 + bytes.div_ceil(64),
        ..Default::default()
    })?;
    Ok(a.cmp(b))
}

/// Fallible insertion sort has no opaque comparator or allocation. Every actual
/// comparison and three-tuple swap is admitted before execution. Its quadratic
/// worst case remains bounded by the SAME cumulative proof budget; already
/// ordered inputs still pay all order checks, without assuming they are ordered.
fn sort_effect_entries(
    entries: &mut [EffectEntry<'_>],
    budget: &mut impl StructuralReadBudget,
) -> crate::Result<()> {
    for i in 1..entries.len() {
        let mut j = i;
        while j > 0 {
            if compare_effect_keys(entries[j - 1].0, entries[j].0, budget)?
                != std::cmp::Ordering::Greater
            {
                break;
            }
            let bytes = std::mem::size_of::<EffectEntry<'_>>()
                .checked_mul(3)
                .ok_or_else(|| Error("restored effect swap overflow".into()))?;
            budget.charge(BudgetCharge {
                bytes,
                work: 1 + bytes.div_ceil(64),
                ..Default::default()
            })?;
            entries.swap(j - 1, j);
            j -= 1;
        }
    }
    Ok(())
}

/// Independently verified bytes and readonly isolated image; no live session
/// for the new incarnation, normal startup, new command or installation grant.
pub struct RotatedCurrentCatalogImage {
    image: RotatedCompletedImage,
    history: CapturedSourceHistory,
    generation: i64,
    owner: CompletedCatalogSession,
}
pub struct CurrentRotationFailure {
    error: Error,
    owner: CompletedCatalogSession,
}
impl CurrentRotationFailure {
    pub fn error(&self) -> &Error {
        &self.error
    }
    pub fn into_session(self) -> CompletedCatalogSession {
        self.owner
    }
}
impl std::fmt::Debug for CurrentRotationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CurrentRotationFailure")
            .field("error", &self.error.to_string())
            .finish_non_exhaustive()
    }
}
/// Caller-owned affine operation. The original native owner never enters the
/// consuming copied-image stack. A caught unwind leaves this object Failed;
/// no retry, old-image reconstruction, global registry or Drop allocation exists.
#[path = "current_rotation/planning.rs"]
mod planning;
pub use planning::{CurrentRotationPlanFacts, CurrentRotationPlanningOperation};

pub struct CurrentRotationOperation {
    planning: Option<planning::PlanningAdmission>,
    _admission: RotationStorageAdmission,
    owner: CompletedCatalogSession,
    history: CapturedSourceHistory,
    generation: i64,
    deadline: Instant,
    state: RotationState,
}
enum RotationState {
    Ready(ValidatedCompletedImage),
    Failed,
    Rotated(RotatedCompletedImage),
}
pub(super) struct RotationStorageAdmission {
    deadline: Instant,
    _slots: crate::migration::native_session::allocation::AllocationGuard,
}
/// No storage admission was present. This is not a rotation attempt: the exact
/// unchanged original image remains owned here, without a new grant.
pub struct CurrentRotationPreparationFailure {
    image: CurrentCatalogImage,
}
impl CurrentRotationPreparationFailure {
    pub fn into_image(self) -> CurrentCatalogImage {
        self.image
    }
}
impl std::fmt::Debug for CurrentRotationPreparationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("current rotation storage was not admitted")
    }
}
impl CurrentCatalogImage {
    /// Reserve the actual fixed handoff slots while caller custody is still
    /// borrowed. This admits storage only, not rotation or deadline extension.
    pub fn admit_rotation_operation(&mut self, deadline: Instant) -> Result<()> {
        if self.rotation_admission.is_some() {
            return Err(Error("current rotation storage already admitted".into()));
        }
        // Stable caller slot, both public move-only Result handoffs, and
        // isolated producer input/output slots may coexist during transitions.
        let bytes = [
            std::mem::size_of::<CurrentRotationOperation>(),
            std::mem::size_of::<
                std::result::Result<CurrentRotationOperation, CurrentRotationPreparationFailure>,
            >(),
            std::mem::size_of::<
                std::result::Result<RotatedCurrentCatalogImage, CurrentRotationFailure>,
            >(),
            std::mem::size_of::<ValidatedCompletedImage>(),
            std::mem::size_of::<RotatedCompletedImage>(),
        ]
        .into_iter()
        .try_fold(0usize, |total, bytes| total.checked_add(bytes))
        .ok_or_else(|| Error("current rotation slot overflow".into()))?;
        let store = &self.owner.session.store;
        let slots = store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("current rotation allocation owner missing".into()))?
            .allocation
            .reserve(
                &store.db,
                u64::try_from(bytes).map_err(|_| Error("current rotation slot overflow".into()))?,
            )?;
        self.rotation_admission = Some(RotationStorageAdmission {
            deadline: deadline.min(Instant::now() + Duration::from_secs(60)),
            _slots: slots,
        });
        Ok(())
    }
    /// Moves only: no allocation, query, callback, polling or registry insertion
    /// can precede establishment of the caller-owned operation.
    pub fn begin_rotation(
        mut self,
    ) -> std::result::Result<CurrentRotationOperation, CurrentRotationPreparationFailure> {
        let Some(admission) = self.rotation_admission.take() else {
            return Err(CurrentRotationPreparationFailure { image: self });
        };
        let Self {
            image,
            history,
            generation,
            owner,
            ..
        } = self;
        Ok(CurrentRotationOperation {
            planning: None,
            deadline: admission.deadline,
            _admission: admission,
            owner,
            history,
            generation,
            state: RotationState::Ready(image),
        })
    }
    /// Standalone consuming convenience. Connected hosts must retain the
    /// borrowed operation itself across caught panics and cancellation.
    pub fn rotate_current(
        self,
        host: &CurrentRestoreHostAttestation,
        cancel: &AtomicBool,
    ) -> std::result::Result<RotatedCurrentCatalogImage, CurrentRotationFailure> {
        self.rotate_current_inner(host, cancel, false)
    }
    fn rotate_current_inner(
        mut self,
        host: &CurrentRestoreHostAttestation,
        cancel: &AtomicBool,
        fault: bool,
    ) -> std::result::Result<RotatedCurrentCatalogImage, CurrentRotationFailure> {
        if let Err(error) = self.admit_rotation_operation(Instant::now() + Duration::from_secs(60))
        {
            return Err(CurrentRotationFailure {
                error,
                owner: self.discard_copy(),
            });
        }
        let mut operation = match self.begin_rotation() {
            Ok(operation) => operation,
            Err(failure) => {
                return Err(CurrentRotationFailure {
                    error: Error("current rotation storage admission missing".into()),
                    owner: failure.into_image().discard_copy(),
                });
            }
        };
        match operation.rotate_with_fault(
            host,
            cancel,
            if fault {
                RotationFault::AfterRotation
            } else {
                RotationFault::None
            },
        ) {
            Ok(()) => operation.finish(),
            Err(error) => Err(operation.into_failure(error)),
        }
    }
}
impl CurrentRotationOperation {
    pub fn rotate(
        &mut self,
        host: &CurrentRestoreHostAttestation,
        cancel: &AtomicBool,
    ) -> Result<()> {
        self.rotate_with_fault(host, cancel, RotationFault::None)
    }
    fn rotate_with_fault(
        &mut self,
        host: &CurrentRestoreHostAttestation,
        cancel: &AtomicBool,
        fault: RotationFault,
    ) -> Result<()> {
        // Mark terminal before allocation checkpoints, BEGIN, or any embedding
        // poll. Only the isolated copy is moved into the fallible closure.
        if self.planning.is_some() || !matches!(self.state, RotationState::Ready(_)) {
            return Err(Error("current rotation operation already consumed".into()));
        }
        let RotationState::Ready(image) = std::mem::replace(&mut self.state, RotationState::Failed)
        else {
            return Err(Error("current rotation operation already consumed".into()));
        };
        self.live(cancel)?;
        let store = &self.owner.session.store;
        let rotated = store.transaction_detailed(|_| {
            self.live(cancel)?;
            if host.generation != self.generation {
                return Err(Error("current restore generation differs".into()));
            }
            let proof = CurrentImageRotationAdmission {
                owner: &self.owner,
                history: &self.history,
                generation: self.generation,
                deadline: self.deadline,
            };
            let rotated = image.rotate_current(store, &proof, host, cancel, fault)?;
            proof.validate_source(store)?;
            let retained = rotated
                .retained_allocations()?
                .checked_add(retained_history(&self.history)?)
                .ok_or_else(|| Error("current rotation allocation overflow".into()))?;
            self.owner.session.retain_image_allocation(retained)?;
            self.live(cancel)?;
            Ok(rotated)
        })?;
        // Generic source finish can poll after the closure's deadline check
        // before its read-only COMMIT. Unlike the directly checked isolated
        // copied COMMIT, that source COMMIT has no immediate deadline fence.
        // Final expiry or poison refuses output; it does not claim rollback of
        // an already acknowledged read-only COMMIT.
        store.transaction_available()?;
        self.live(cancel)?;
        self.state = RotationState::Rotated(rotated);
        Ok(())
    }
    fn live(&self, cancel: &AtomicBool) -> Result<()> {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) || Instant::now() >= self.deadline {
            Err(Error(
                "current rotation cancelled or deadline exceeded".into(),
            ))
        } else {
            Ok(())
        }
    }
    fn into_failure(self, error: Error) -> CurrentRotationFailure {
        let Self {
            owner,
            state,
            history,
            ..
        } = self;
        drop(state);
        drop(history);
        CurrentRotationFailure { error, owner }
    }
    pub fn finish(self) -> std::result::Result<RotatedCurrentCatalogImage, CurrentRotationFailure> {
        let Self {
            owner,
            history,
            generation,
            state,
            ..
        } = self;
        match state {
            RotationState::Rotated(image) => Ok(RotatedCurrentCatalogImage {
                image,
                history,
                generation,
                owner,
            }),
            _ => Err(CurrentRotationFailure {
                error: Error("current rotation did not complete; original owner retained".into()),
                owner,
            }),
        }
    }
}
impl RotatedCurrentCatalogImage {
    /// Actual retained rotated image/history plus checkpointed original-owner
    /// logical retention. External host reservation is deliberately excluded.
    pub fn retained_allocations(&self) -> Result<u64> {
        let bytes = self
            .image
            .retained_allocations()?
            .checked_add(retained_history(&self.history)?)
            .ok_or_else(|| Error("rotated current retention overflow".into()))?;
        self.owner.session.retain_image_allocation(bytes)?;
        self.owner.retained_allocations()
    }
    /// Descriptive monotonic host reservation retained by the original owner.
    /// This performs no proof decoding and grants no additional authority.
    pub fn external_retained_reservation(&self) -> Result<u64> {
        self.owner.external_retained_reservation()
    }
    pub fn refresh_external_reservation(
        &mut self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        let bytes = self
            .image
            .retained_allocations()?
            .checked_add(retained_history(&self.history)?)
            .ok_or_else(|| Error("rotated current retention overflow".into()))?;
        self.owner.session.retain_image_allocation(bytes)?;
        self.owner.refresh_external_reservation(reservation)
    }
    pub fn sha256(&self) -> &str {
        self.image.sha256()
    }
    pub fn len(&self) -> u64 {
        self.image.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn origin_json(&self) -> &str {
        self.image.origin_json()
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
    pub fn current_inventory_sha256(&self) -> &str {
        self.image.inventory_sha256()
    }
    pub fn source_effects(&self) -> &[Value] {
        self.history.effects()
    }
    /// Read fixed descriptive source rows from the isolated rotated snapshot.
    /// This does not admit an ordinary restored Store or authorize a mutation.
    pub fn current_source_evidence(
        &self,
        profile: &str,
        source: &str,
        cancel: &AtomicBool,
    ) -> Result<crate::catalog_source_succession::RestoredSourceEvidence> {
        self.owner.session.store.transaction_detailed(|_| {
            let admission = CurrentImageRotationAdmission {
                owner: &self.owner,
                history: &self.history,
                generation: self.generation,
                deadline: Instant::now() + Duration::from_secs(60),
            };
            admission.validate_source(&self.owner.session.store)?;
            let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(
                self.owner.session.store.catalog_operation_budget()?,
            );
            let evidence =
                self.image
                    .current_source_evidence(profile, source, &mut budget, cancel)?;
            admission.validate_source(&self.owner.session.store)?;
            Ok(evidence)
        })
    }
    pub fn copy_verified_to(
        &self,
        output: &mut impl std::io::Write,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let validate = || {
            self.owner.session.store.transaction_detailed(|_| {
                CurrentImageRotationAdmission {
                    owner: &self.owner,
                    history: &self.history,
                    generation: self.generation,
                    deadline: Instant::now() + Duration::from_secs(60),
                }
                .validate_source(&self.owner.session.store)
            })
        };
        validate()?;
        self.image.copy_verified_to(output, cancel)?;
        validate()
    }
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
}

#[cfg(test)]
mod tests {
    use super::super::super::tests::committed;
    use super::*;
    use std::time::Duration;
    #[test]
    fn borrowed_effect_equality_charges_sorted_key_comparisons() {
        use crate::operation_budget::{BudgetLimits, OperationBudget};
        let budget = || {
            crate::catalog_nfo_attribution::ProofBudget::new(
                OperationBudget::new(
                    Duration::from_secs(30),
                    BudgetLimits {
                        items: usize::MAX,
                        bytes: usize::MAX,
                        values: usize::MAX,
                        work: usize::MAX,
                    },
                )
                .unwrap(),
            )
        };
        let original = serde_json::json!({"key":"original","revision":1});
        assert!(equal_effect(&original, &original, &mut budget(), 0).unwrap());
        assert!(
            !equal_effect(
                &original,
                &serde_json::json!({"key":"changed","revision":1}),
                &mut budget(),
                0
            )
            .unwrap()
        );
        let mut keys = serde_json::Map::new();
        for i in 0..16 {
            keys.insert(format!("{}-{i:02}", "a".repeat(512)), Value::Null);
        }
        let long = Value::Object(keys);
        assert!(equal_effect(&long, &long, &mut budget(), 0).unwrap());
        let mut keys = serde_json::Map::new();
        for i in 0..16 {
            keys.insert(format!("{}-{i:02}", "a".repeat(32 * 1024)), Value::Null);
        }
        let long = Value::Object(keys);
        assert!(equal_effect(&long, &long, &mut budget(), 0).is_err());
    }
    #[test]
    fn borrowed_effect_sort_handles_unsorted_entries_and_charges_each_swap() {
        use crate::operation_budget::{BudgetLimits, OperationBudget};
        let shared = OperationBudget::new(
            Duration::from_secs(30),
            BudgetLimits {
                items: usize::MAX,
                bytes: usize::MAX,
                values: usize::MAX,
                work: usize::MAX,
            },
        )
        .unwrap();
        let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(shared.clone());
        let value = Value::Null;
        let mut entries = [("c", &value), ("a", &value), ("b", &value)];
        sort_effect_entries(&mut entries, &mut budget).unwrap();
        assert_eq!(entries.map(|entry| entry.0), ["a", "b", "c"]);
        let swap_bytes = 3 * std::mem::size_of::<EffectEntry<'_>>();
        assert_eq!(shared.usage().bytes, 6 + 2 * swap_bytes);
        assert_eq!(shared.usage().work, 9 + 2 * (1 + swap_bytes.div_ceil(64)));
        assert_eq!(shared.usage().values, 6);
    }

    #[test]
    fn borrowed_effect_sort_failure_is_sticky_before_comparison_or_move() {
        use crate::operation_budget::{BudgetLimits, OperationBudget};
        let shared = OperationBudget::new(
            Duration::from_secs(30),
            BudgetLimits {
                items: usize::MAX,
                bytes: usize::MAX,
                values: usize::MAX,
                work: 2,
            },
        )
        .unwrap();
        let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(shared.clone());
        let value = Value::Null;
        let mut entries = [("b", &value), ("a", &value)];
        assert!(sort_effect_entries(&mut entries, &mut budget).is_err());
        assert_eq!(entries.map(|entry| entry.0), ["b", "a"]);
        assert!(shared.failure().is_some());
        assert!(sort_effect_entries(&mut entries, &mut budget).is_err());
        assert_eq!(entries.map(|entry| entry.0), ["b", "a"]);
    }

    #[test]
    fn borrowed_effect_equality_preserves_keys_nested_values_and_array_order() {
        use crate::operation_budget::{BudgetLimits, OperationBudget};
        let shared = OperationBudget::new(
            Duration::from_secs(30),
            BudgetLimits {
                items: usize::MAX,
                bytes: usize::MAX,
                values: usize::MAX,
                work: usize::MAX,
            },
        )
        .unwrap();
        let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(shared);
        let original = serde_json::json!({"z":[1, {"a":true,"b":null}], "a":0});
        let reordered = serde_json::json!({"a":0, "z":[1, {"b":null,"a":true}]});
        assert!(equal_effect(&original, &reordered, &mut budget, 0).unwrap());
        for changed in [
            serde_json::json!({"y":[1, {"a":true,"b":null}], "a":0}),
            serde_json::json!({"z":[1, {"a":false,"b":null}], "a":0}),
            serde_json::json!({"z":[{"a":true,"b":null},1], "a":0}),
            serde_json::json!({"z":[1, {"a":true,"b":null}]}),
        ] {
            assert!(!equal_effect(&original, &changed, &mut budget, 0).unwrap());
        }
    }

    fn aligned_equality_budget(
        work: usize,
        bytes: usize,
    ) -> (
        crate::operation_budget::OperationBudget,
        crate::catalog_nfo_attribution::ProofBudget,
    ) {
        use crate::operation_budget::{BudgetLimits, OperationBudget};
        let shared = OperationBudget::new(
            Duration::from_secs(30),
            BudgetLimits {
                items: usize::MAX,
                values: usize::MAX,
                bytes,
                work,
            },
        )
        .unwrap();
        let local = crate::catalog_nfo_attribution::ProofBudget::new(shared.clone());
        (shared, local)
    }

    #[test]
    fn aligned_effect_equality_has_no_entry_arrays_and_keeps_exact_boundary() {
        let value = serde_json::json!({"a":1,"b":null});
        let (shared, mut budget) = aligned_equality_budget(16, 52);
        assert!(equal_effect(&value, &value, &mut budget, 0).unwrap());
        assert_eq!(shared.usage().work, 16);
        assert_eq!(shared.usage().bytes, 52);
        assert_eq!(shared.usage().items, 0); // no temporary entry Vec admissions
        let (shared, mut budget) = aligned_equality_budget(16, 51);
        assert!(equal_effect(&value, &value, &mut budget, 0).is_err());
        assert!(shared.failure().is_some());
        assert!(equal_effect(&Value::Null, &Value::Null, &mut budget, 0).is_err());
    }

    #[test]
    fn aligned_effect_equality_rejects_value_and_cardinality_changes() {
        let value = serde_json::json!({"a":1,"b":null});
        for changed in [
            serde_json::json!({"a":2,"b":null}),
            serde_json::json!({"a":1}),
            serde_json::json!({"a":1,"b":null,"c":true}),
        ] {
            let (_, mut budget) = aligned_equality_budget(usize::MAX, usize::MAX);
            assert!(!equal_effect(&value, &changed, &mut budget, 0).unwrap());
        }
    }

    #[test]
    fn aligned_effect_late_order_divergence_uses_complete_sorted_fallback() {
        // Explicit unique-key entries genuinely differ in order even when the
        // selected serde_json feature set normalizes map insertion order.
        let nested = serde_json::json!({"z":[1,{"b":null,"a":true}],"a":0});
        let reordered = serde_json::json!({"a":0,"z":[1,{"a":true,"b":null}]});
        let value = Value::Null;
        let left = [("a", &value), ("b", &nested), ("c", &value)];
        let right = [("a", &value), ("c", &value), ("b", &reordered)];
        let (shared, mut budget) = aligned_equality_budget(usize::MAX, usize::MAX);
        assert_eq!(
            aligned_effect_entries(left.into_iter(), right.into_iter(), &mut budget, 0).unwrap(),
            None
        );
        let spent = shared.usage();
        assert_eq!(spent.work, 10); // includes the successfully compared prefix
        assert!(
            sorted_effect_entries_equal(left.into_iter(), right.into_iter(), &mut budget, 0)
                .unwrap()
        );
        let (oracle_shared, mut oracle) = aligned_equality_budget(usize::MAX, usize::MAX);
        assert!(
            sorted_effect_entries_equal(left.into_iter(), right.into_iter(), &mut oracle, 0)
                .unwrap()
        );
        assert_eq!(shared.usage().work, spent.work + oracle_shared.usage().work);
        assert_eq!(
            shared.usage().bytes,
            spent.bytes + oracle_shared.usage().bytes
        );
        let changed = Value::Bool(true);
        let changed = [("a", &value), ("c", &value), ("b", &changed)];
        let (_, mut budget) = aligned_equality_budget(usize::MAX, usize::MAX);
        assert!(
            !sorted_effect_entries_equal(left.into_iter(), changed.into_iter(), &mut budget, 0)
                .unwrap()
        );
    }

    #[test]
    fn aligned_effect_fallback_preserves_exhausted_parent_and_spent_work() {
        let value = Value::Null;
        let left = [("a", &value), ("b", &value), ("c", &value)];
        let right = [("a", &value), ("c", &value), ("b", &value)];
        let (shared, mut budget) = aligned_equality_budget(10, usize::MAX);
        assert_eq!(
            aligned_effect_entries(left.into_iter(), right.into_iter(), &mut budget, 0).unwrap(),
            None
        );
        assert_eq!(shared.usage().work, 10);
        assert!(
            sorted_effect_entries_equal(left.into_iter(), right.into_iter(), &mut budget, 0)
                .is_err()
        );
        assert!(shared.failure().is_some());
        assert!(
            aligned_effect_entries(left.into_iter(), left.into_iter(), &mut budget, 0).is_err()
        );
    }

    fn host(image: &CurrentCatalogImage) -> CurrentRestoreHostAttestation {
        CurrentRestoreHostAttestation {
            pair_sha256: "a".repeat(64),
            motion_binding_sha256: "b".repeat(64),
            catalog_id: image.catalog_id().into(),
            incarnation: image.incarnation().into(),
            generation: image.generation(),
            current_inventory_sha256: image.current_inventory_sha256().into(),
            transition_digest: image.transition_digest().into(),
        }
    }
    fn admitted_operation(
        mut image: CurrentCatalogImage,
        deadline: Instant,
    ) -> CurrentRotationOperation {
        image.admit_rotation_operation(deadline).unwrap();
        image.begin_rotation().unwrap()
    }
    #[test]
    fn current_rotation_preserves_original_generation_history_and_exact_copy_scope() {
        let (_root, session, command) = committed(Duration::from_secs(3600));
        let owner = session.into_completed_catalog(&command).unwrap();
        let before = snapshot(&owner.session.store.db).unwrap();
        let image = owner.into_current_image(&AtomicBool::new(false)).unwrap();
        let attestation = host(&image);
        let measured = image.retained_allocations().unwrap();
        assert!(measured > 0);
        let previous_external = image.external_retained_reservation().unwrap();
        let mut operation = admitted_operation(image, Instant::now() + Duration::from_secs(60));
        operation
            .rotate(&attestation, &AtomicBool::new(false))
            .unwrap();
        let mut rotated = operation.finish().unwrap();
        assert_eq!(
            rotated.external_retained_reservation().unwrap(),
            previous_external
        );
        let retained = rotated.retained_allocations().unwrap();
        assert!(retained >= rotated.image.retained_allocations().unwrap());
        let refreshed_external = previous_external.checked_add(1024).unwrap();
        rotated
            .refresh_external_reservation(
                OfflineAllocationReservation::new(refreshed_external).unwrap(),
            )
            .unwrap();
        assert_eq!(
            rotated.external_retained_reservation().unwrap(),
            refreshed_external
        );
        assert!(
            rotated
                .refresh_external_reservation(
                    OfflineAllocationReservation::new(previous_external).unwrap(),
                )
                .is_err()
        );
        assert_eq!(
            rotated.external_retained_reservation().unwrap(),
            refreshed_external
        );
        assert_eq!(snapshot(&rotated.owner.session.store.db).unwrap(), before);
        assert_eq!(rotated.generation(), attestation.generation);
        let origin: Value = serde_json::from_str(rotated.origin_json()).unwrap();
        assert_eq!(origin["from_incarnation"], attestation.incarnation);
        assert_ne!(origin["to_incarnation"], attestation.incarnation);
        rotated
            .owner
            .session
            .store
            .transaction_detailed(|_| {
                let store = &rotated.owner.session.store;
                let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(
                    store.catalog_operation_budget()?,
                );
                rotated.image.verify_history_scope_for_test(
                    &store.db,
                    &attestation.incarnation,
                    &mut budget,
                )
            })
            .unwrap();
        let mut bytes = Vec::new();
        rotated
            .copy_verified_to(&mut bytes, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(catabolic_core::sha256(&bytes), rotated.sha256());
        rotated.image.verify_ended_snapshot_for_test().unwrap();
        let owner = rotated.discard_copy();
        assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
        assert!(owner.session.store.db.is_autocommit());
    }
    #[test]
    fn current_rotation_source_witness_rejects_foreign_stale_poison_and_ended_snapshot() {
        let (_root, session, command) = committed(Duration::from_secs(3600));
        let owner = session.into_completed_catalog(&command).unwrap();
        let before = snapshot(&owner.session.store.db).unwrap();
        let image = owner.into_current_image(&AtomicBool::new(false)).unwrap();
        let attestation = host(&image);
        let mut rotated = image
            .rotate_current(&attestation, &AtomicBool::new(false))
            .unwrap();
        let store = &rotated.owner.session.store;
        store
            .transaction_detailed(|_| -> Result<()> {
                let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(
                    store.catalog_operation_budget()?,
                );
                let admission = CurrentImageRotationAdmission {
                    owner: &rotated.owner,
                    history: &rotated.history,
                    generation: rotated.generation,
                    deadline: Instant::now() + Duration::from_secs(60),
                };
                rotated
                    .image
                    .verify_source_effect_tamper_for_test(&admission, &mut budget)?;
                // Existing genuine mutation negatives establish readonly storage;
                // new fixed-reader negatives exercise its retained source witness.
                rotated.image.verify_history_scope_for_test(
                    &store.db,
                    &attestation.incarnation,
                    &mut budget,
                )?;
                rotated
                    .image
                    .verify_source_witness_for_test(&store.db, &mut budget)
            })
            .unwrap();
        let owner = rotated.discard_copy();
        assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
        assert!(owner.session.store.db.is_autocommit());
    }
    #[test]
    fn current_rotation_accepts_exact_expired_bootstrap_history_without_rewriting_it() {
        let (_root, session, command) = committed(Duration::ZERO);
        assert_eq!(
            session
                .compact_receipt_for_test(&command, command.key().host_namespace())
                .unwrap(),
            1
        );
        let owner = session.into_completed_catalog(&command).unwrap();
        let before = snapshot(&owner.session.store.db).unwrap();
        let image = owner.into_current_image(&AtomicBool::new(false)).unwrap();
        let attestation = host(&image);
        let rotated = image
            .rotate_current(&attestation, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(snapshot(&rotated.owner.session.store.db).unwrap(), before);
    }
    #[test]
    fn current_rotation_wrong_generation_cancel_and_post_effect_failure_keep_source_custody() {
        let (_root, session, command) = committed(Duration::from_secs(3600));
        let mut owner = session.into_completed_catalog(&command).unwrap();
        let before = snapshot(&owner.session.store.db).unwrap();
        for mode in 0..3 {
            let image = owner.into_current_image(&AtomicBool::new(false)).unwrap();
            let mut attestation = host(&image);
            if mode == 0 {
                attestation.generation += 1;
            }
            let result =
                image.rotate_current_inner(&attestation, &AtomicBool::new(mode == 1), mode == 2);
            owner = match result {
                Err(failure) => failure.into_session(),
                Ok(_) => panic!("invalid rotation succeeded"),
            };
            assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
            assert!(owner.session.store.db.is_autocommit());
            owner.validate_current().unwrap();
        }
    }
    #[test]
    fn borrowed_rotation_keeps_original_on_entry_poll_and_copy_commit_unwinds() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        use std::sync::{Arc, atomic::Ordering};
        for mode in 0..3 {
            let (_root, session, command) = committed(Duration::from_secs(3600));
            let owner = session.into_completed_catalog(&command).unwrap();
            let before = snapshot(&owner.session.store.db).unwrap();
            let image = owner.into_current_image(&AtomicBool::new(false)).unwrap();
            let attestation = host(&image);
            let mut operation = admitted_operation(image, Instant::now() + Duration::from_secs(60));
            let raw = unsafe { operation.owner.session.store.db.handle() } as usize;
            let polled = Arc::new(AtomicBool::new(false));
            let seen = polled.clone();
            // The catch surrounds only a borrow, not the operation's custody.
            let result = catch_unwind(AssertUnwindSafe(|| {
                if mode == 0 {
                    crate::embedding::with_interrupt_check(
                        move || {
                            // Scalar-only observation of the retained actual source.
                            // Panic at the first poll after its BEGIN, not an FFI callback.
                            if unsafe { rusqlite::ffi::sqlite3_get_autocommit(raw as *mut _) } == 0
                                && !seen.swap(true, Ordering::SeqCst)
                            {
                                panic!("controlled original entry-poll unwind");
                            }
                            false
                        },
                        || operation.rotate(&attestation, &AtomicBool::new(false)),
                    )
                } else {
                    operation.rotate_with_fault(
                        &attestation,
                        &AtomicBool::new(false),
                        if mode == 1 {
                            RotationFault::PanicBeforeCommit
                        } else {
                            RotationFault::PanicAfterCommit
                        },
                    )
                }
            }));
            assert!(result.is_err(), "controlled unwind was not reached");
            if mode == 0 {
                assert!(polled.load(Ordering::SeqCst));
            }
            assert!(matches!(operation.state, RotationState::Failed));
            assert!(operation.owner.session.store.owns_writer_lock());
            assert!(operation.owner.session.store.db.is_autocommit());
            assert_eq!(snapshot(&operation.owner.session.store.db).unwrap(), before);
            assert!(
                operation
                    .rotate(&attestation, &AtomicBool::new(false))
                    .is_err()
            );
            let owner = match operation.finish() {
                Err(failure) => failure.into_session(),
                Ok(_) => panic!("failed rotation returned an image"),
            };
            assert!(owner.session.store.owns_writer_lock());
            owner.validate_current().unwrap();
            assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
        }
    }

    #[test]
    fn borrowed_rotation_expired_original_deadline_is_terminal_before_effect() {
        let (_root, session, command) = committed(Duration::from_secs(3600));
        let owner = session.into_completed_catalog(&command).unwrap();
        let before = snapshot(&owner.session.store.db).unwrap();
        let image = owner.into_current_image(&AtomicBool::new(false)).unwrap();
        let attestation = host(&image);
        let image = match image.begin_rotation() {
            Err(failure) => failure.into_image(),
            Ok(_) => panic!("unadmitted rotation handoff succeeded"),
        };
        assert_eq!(snapshot(&image.owner.session.store.db).unwrap(), before);
        let mut operation = admitted_operation(image, Instant::now());
        assert!(
            operation
                .rotate(&attestation, &AtomicBool::new(false))
                .is_err()
        );
        assert!(matches!(operation.state, RotationState::Failed));
        assert!(
            operation
                .rotate(&attestation, &AtomicBool::new(false))
                .is_err()
        );
        let owner = match operation.finish() {
            Err(failure) => failure.into_session(),
            Ok(_) => panic!("failed rotation returned an image"),
        };
        assert!(owner.session.store.owns_writer_lock());
        assert_eq!(snapshot(&owner.session.store.db).unwrap(), before);
        owner.validate_current().unwrap();
    }
}
