//! Logical aggregate admission for the retained offline owner, not a hard RSS
//! guarantee. External retention is descriptive and can only reduce headroom.
//! SQLite DBSTATUS, observable String/Vec capacities and conservative BTreeMap
//! node storage are counted; allocator fragmentation/stacks are not claimed.
#[path = "allocation/identity_ledger.rs"]
mod identity_ledger;
pub(super) use identity_ledger::{IdentityLedger, identity_ledger_rows, original_identity_digest};
#[path = "allocation/apply_workspace.rs"]
mod apply_workspace;
pub(super) use apply_workspace::{ApplyCopyReservation, ApplyMetadataLoan, ApplyMetadataWorkspace, ApplyRetainedOuter};
#[cfg(test)]
pub(super) use apply_workspace::ApplyOuter;
#[path = "allocation/control_workspace.rs"]
mod control_workspace;
pub use control_workspace::OfflineControlWorkspaceBacking;
pub(in crate::migration) use control_workspace::ControlWorkspaceLoan;
pub(super) use control_workspace::MetadataReservation;
#[path = "allocation/copied_image.rs"]
mod copied_image;
pub(in crate::migration) use copied_image::PaidImageSqliteLoan;
pub(super) use copied_image::{CopiedImageReservation, MAX_COPIED_IMAGE_BYTES};

use crate::{
    Error, Result,
    operation_budget::{AllocationAttachment, BudgetCharge, OperationBudget},
};
use rusqlite::Connection;
use serde_json::Value;
use std::sync::{Arc, Mutex};

pub(crate) const LIMIT: u64 = 256 * 1024 * 1024;
pub(super) const MIGRATION_SCRATCH: u64 = 32 * 1024 * 1024;
// A deliberately conservative logical allowance per map/node/item, including
// unobservable BTreeMap spare slots and the Value header. Not an allocator ABI.
const NODE_BYTES: u64 = 1024;
fn refused() -> Error {
    Error("offline aggregate allocation budget exhausted".into())
}
fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).filter(|n| *n <= LIMIT).ok_or_else(refused)
}

/// Host-owned bytes coexisting with native custody. This is not an authority,
/// an allocation limit override, or a claim that native measured host memory.
#[derive(Clone, Copy, Debug, Default)]
pub struct OfflineAllocationReservation {
    external_retained_bytes: u64,
}
impl OfflineAllocationReservation {
    pub fn new(external_retained_bytes: u64) -> Result<Self> {
        add(external_retained_bytes, MIGRATION_SCRATCH)?;
        Ok(Self {
            external_retained_bytes,
        })
    }
}
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct CapturePhaseForTest {
    pub(crate) name: &'static str,
    pub(crate) external: u64,
    pub(crate) temporary: u64,
    pub(crate) total: u64,
    // preparation, plan, preservation, persistent image, temporary, SQLite,
    // active cumulative operation attachment. Nothing is reset or rebased.
    pub(crate) native: [u64; 7],
    pub(crate) image_bytes: u64,
    pub(crate) copy_sqlite: u64,
    pub(crate) metadata: u64,
    pub(crate) requested: u64,
}
#[derive(Default)]
struct State {
    #[cfg(test)]
    capture_trace: [Option<CapturePhaseForTest>; 32],
    external: u64,
    // Set only by the original fixed workspace admission, never by refresh.
    control_workspace: bool,
    control_loan: bool,
    control_scratch: bool,
    preparation: u64,
    plan: u64,
    preservation: u64,
    image: u64,
    temporary: u64,
    sqlite: u64,
    sqlite_handle: Option<usize>,
    operation: Option<(OperationBudget, u64, usize)>,
}
#[derive(Clone, Default)]
pub(crate) struct AllocationContext(Arc<Mutex<State>>);
impl AllocationContext {
    pub(super) fn preparation_context_storage() -> u64 {
        // Logical Arc counters plus its actual Mutex<State> payload, not a
        // promise about allocator metadata or fragmentation.
        (std::mem::size_of::<Mutex<State>>() + 2 * std::mem::size_of::<usize>()) as u64
    }
    pub(super) fn new(reservation: OfflineAllocationReservation) -> Result<Self> {
        let owner = Self::default();
        owner.refresh(reservation, None)?;
        Ok(owner)
    }
    fn total(state: &State, additional: u64) -> Result<u64> {
        [
            state.external,
            state.preparation,
            state.plan,
            state.preservation,
            state.image,
            state.temporary,
            state.sqlite,
            state.operation.as_ref().map_or(0, |(_, bytes, _)| *bytes),
            additional,
        ]
        .into_iter()
        .try_fold(0, add)
    }
    #[cfg(test)]
    pub(in crate::migration) fn capture_trace_for_test(&self) -> Result<[Option<CapturePhaseForTest>; 32]> {
        Ok(self.0.lock().map_err(|_| refused())?.capture_trace)
    }
    #[cfg(test)]
    pub(super) fn retained_components_for_test(&self) -> Result<(u64, u64)> {
        let state = self.0.lock().map_err(|_| refused())?;
        Ok((state.preservation, state.image))
    }
    #[cfg(test)]
    pub(in crate::migration) fn external_retained_bytes_for_test(&self) -> Result<u64> {
        self.external_retained_bytes()
    }
    #[cfg(test)]
    pub(in crate::migration) fn temporary_bytes_for_test(&self) -> Result<u64> {
        Ok(self.0.lock().map_err(|_| refused())?.temporary)
    }
    #[cfg(test)]
    pub(in crate::migration) fn preparation_state_for_test(
        &self,
    ) -> Result<(usize, Option<usize>, u64)> {
        let state = self.0.lock().map_err(|_| refused())?;
        Ok((
            Arc::as_ptr(&self.0) as usize,
            state.sqlite_handle,
            state.preparation,
        ))
    }
    #[cfg(test)]
    pub(super) fn fill_unbound_for_test(&self) -> Result<AllocationGuard> {
        let held = {
            let state = self.0.lock().map_err(|_| refused())?;
            Self::total(&state, 0)?
        };
        self.reserve_unbound(LIMIT.checked_sub(held).ok_or_else(refused)?)
    }
    /// Fixed preparation storage admitted before acquisition. The high-water
    /// remains in this SAME context through Prepared, apply and Store transfers;
    /// it is not charged to the shared plan framing or released at finish.
    pub(super) fn retain_preparation_unbound(&self, bytes: u64) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| refused())?;
        if state.sqlite_handle.is_some() {
            return Err(refused());
        }
        Self::total(&state, bytes.saturating_sub(state.preparation))?;
        state.preparation = state.preparation.max(bytes);
        Ok(())
    }
    /// Only the partial preparation owner uses this before a source exists.
    /// Binding later preserves this reservation and all existing counters.
    pub(super) fn reserve_unbound(&self, bytes: u64) -> Result<AllocationGuard> {
        let mut state = self.0.lock().map_err(|_| refused())?;
        if state.sqlite_handle.is_some() {
            return Err(refused());
        }
        Self::total(&state, bytes)?;
        state.temporary = add(state.temporary, bytes)?;
        Ok(AllocationGuard {
            context: self.clone(),
            kind: GuardKind::Temporary(bytes),
            attachment: None,
        })
    }
    /// Descriptive retained host high-water; does not allocate or admit work.
    pub(super) fn external_retained_bytes(&self) -> Result<u64> {
        Ok(self.0.lock().map_err(|_| refused())?.external)
    }
    pub(super) fn bind_source(&self, db: &Connection) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| refused())?;
        // SAFETY: inspect only this borrowed live connection's identity,
        // without calling through or dereferencing the native pointer.
        let handle = unsafe { db.handle() } as usize;
        if state.sqlite_handle.is_some_and(|old| old != handle) {
            return Err(refused());
        }
        state.sqlite_handle = Some(handle);
        state.sqlite = crate::catalog_backup::allocations(db)?;
        Self::total(&state, 0)?;
        Ok(())
    }
    fn checkpoint(state: &mut State, db: &Connection) -> Result<u64> {
        let bytes = crate::catalog_backup::allocations(db)?;
        // SAFETY: read only the identity of this live, borrowed
        // connection. No FFI call, hook, or pointer dereference is performed.
        let handle = unsafe { db.handle() } as usize;
        match state.sqlite_handle {
            None => Err(refused()),
            Some(source) if source == handle => {
                state.sqlite = bytes;
                Ok(0)
            }
            Some(_) => Ok(bytes), // isolated reference/image, never replace source
        }
    }
    pub(crate) fn check(&self, db: Option<&Connection>, additional: u64) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| refused())?;
        let other = db
            .map(|db| Self::checkpoint(&mut state, db))
            .transpose()?
            .unwrap_or(0);
        Self::total(&state, add(other, additional)?)?;
        Ok(())
    }
    pub(super) fn refresh(
        &self,
        reservation: OfflineAllocationReservation,
        db: Option<&Connection>,
    ) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| refused())?;
        let other = db
            .map(|db| Self::checkpoint(&mut state, db))
            .transpose()?
            .unwrap_or(0);
        if reservation.external_retained_bytes < state.external {
            return Err(Error("offline external reservation cannot decrease".into()));
        }
        Self::total(
            &state,
            add(other, reservation.external_retained_bytes - state.external)?,
        )?;
        state.external = reservation.external_retained_bytes;
        Ok(())
    }
    pub(super) fn retain_image(&self, db: &Connection, bytes: u64) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| refused())?;
        let other = Self::checkpoint(&mut state, db)?;
        Self::total(&state, add(other, bytes.saturating_sub(state.image))?)?;
        state.image = state.image.max(bytes);
        Ok(())
    }
    /// Checkpointed logical native retention, excluding the caller's separately
    /// tracked host scalar. Conservative retained high-water charges remain.
    pub(super) fn retained_native_bytes(&self, db: &Connection) -> Result<u64> {
        let mut state = self.0.lock().map_err(|_| refused())?;
        let other = Self::checkpoint(&mut state, db)?;
        Self::total(&state, other)?
            .checked_sub(state.external)
            .ok_or_else(refused)
    }
    pub(super) fn image_reservation(&self, db: &Connection) -> Result<(AllocationGuard, u64)> {
        let retained = self.0.lock().map_err(|_| refused())?.image;
        Ok((self.reserve(db, 0)?, retained))
    }
    pub(super) fn retain_plan(&self, value: &Value, db: &Connection) -> Result<()> {
        // One retained plan plus bounded identity/transition copies derived
        // from it. Two whole actual-plan capacities conservatively cover those
        // smaller projections; 64 KiB covers fixed digests/paths/command keys.
        // This scales with admitted data, never an operation allowance.
        let bytes = value_bytes(value)?
            .checked_mul(3)
            .and_then(|n| n.checked_add(64 * 1024))
            .ok_or_else(refused)?;
        self.retain(bytes, db, true)
    }
    pub(super) fn retain_preservation(&self, bytes: u64, db: &Connection) -> Result<()> {
        self.retain(bytes, db, false)
    }
    fn retain(&self, bytes: u64, db: &Connection, plan: bool) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| refused())?;
        let other = Self::checkpoint(&mut state, db)?;
        let old = if plan { state.plan } else { state.preservation };
        Self::total(&state, add(other, bytes.saturating_sub(old))?)?;
        // Conservative high water survives transfer/recovery. No retained plan
        // is forgotten merely because an outer facade releases its copy.
        if plan {
            state.plan = old.max(bytes);
        } else {
            state.preservation = old.max(bytes);
        }
        Ok(())
    }
    pub(in crate::migration) fn reserve(
        &self,
        db: &Connection,
        bytes: u64,
    ) -> Result<AllocationGuard> {
        let mut state = self.0.lock().map_err(|_| refused())?;
        let other = Self::checkpoint(&mut state, db)?;
        Self::total(&state, add(other, bytes)?)?;
        state.temporary = add(state.temporary, bytes)?;
        Ok(AllocationGuard {
            context: self.clone(),
            kind: GuardKind::Temporary(bytes),
            attachment: None,
        })
    }
    pub(crate) fn operation(
        &self,
        db: &Connection,
        budget: &OperationBudget,
    ) -> Result<AllocationGuard> {
        {
            let mut state = self.0.lock().map_err(|_| refused())?;
            let other = Self::checkpoint(&mut state, db)?;
            Self::total(&state, other)?;
            match &mut state.operation {
                Some((active, _, count)) if active.same_owner(budget) => {
                    *count = count.checked_add(1).ok_or_else(refused)?;
                }
                Some(_) => return Err(Error("offline nested allocation operation changed".into())),
                None => state.operation = Some((budget.clone(), 0, 1)),
            }
        }
        let mut guard = AllocationGuard {
            context: self.clone(),
            kind: GuardKind::Operation,
            attachment: None,
        };
        guard.attachment = Some(budget.attach_offline_allocation(AllocationSink {
            context: self.clone(),
            budget: budget.allocation_identity(),
        })?);
        Ok(guard)
    }
}
/// Arithmetic only. Safe at existing UDF/progress charge sites: no SQLite,
/// filesystem access, polling callback or allocation occurs in admission.
#[derive(Clone)]
pub(crate) struct AllocationSink {
    context: AllocationContext,
    budget: usize,
}
impl AllocationSink {
    pub(crate) fn same(&self, other: &Self) -> bool {
        self.budget == other.budget && Arc::ptr_eq(&self.context.0, &other.context.0)
    }
    pub(crate) fn admit(&self, used: BudgetCharge) -> Result<()> {
        // Charge ACTUAL admitted buffers/nodes, never configured allowances.
        // Two encoded-byte copies cover ordinary geometric String growth;
        // node allowance includes map/collection spare slots. Existing local
        // owners still precharge any additional simultaneous explicit clones.
        let bytes = (used.bytes as u64)
            .checked_mul(2)
            .and_then(|bytes| {
                (used.values as u64)
                    .checked_add(used.items as u64)
                    .and_then(|n| n.checked_mul(NODE_BYTES))
                    .and_then(|n| bytes.checked_add(n))
            })
            .ok_or_else(refused)?;
        let mut state = self.context.0.lock().map_err(|_| refused())?;
        let (active, previous, _) = state.operation.as_ref().ok_or_else(refused)?;
        if active.allocation_identity() != self.budget {
            return Err(refused());
        }
        if bytes < *previous {
            return Err(refused());
        }
        AllocationContext::total(&state, bytes - *previous)?;
        state
            .operation
            .as_mut()
            .expect("checked active operation")
            .1 = bytes;
        Ok(())
    }
}

enum GuardKind {
    Temporary(u64),
    Operation,
}
pub(crate) struct AllocationGuard {
    context: AllocationContext,
    kind: GuardKind,
    attachment: Option<AllocationAttachment>,
}
impl AllocationGuard {
    #[cfg(test)]
    pub(super) fn record_capture_phase_for_test(&self, name: &'static str,
        image_bytes: u64, copy_sqlite: u64, metadata: u64, requested: u64,
    ) -> Result<()> {
        let mut state = self.context.0.lock().map_err(|_| refused())?;
        let phase = CapturePhaseForTest { name, external: state.external,
            temporary: state.temporary, total: AllocationContext::total(&state, 0)?,
            native: [state.preparation, state.plan, state.preservation, state.image,
                state.temporary, state.sqlite, state.operation.as_ref().map_or(0, |(_, n, _)| *n)],
            image_bytes, copy_sqlite, metadata, requested };
        if let Some(slot) = state.capture_trace.iter_mut().find(|slot| slot.is_none()) { *slot = Some(phase); }
        Ok(())
    }
    pub(super) fn require_same_source(&self, context: &AllocationContext, source: &Connection) -> Result<()> {
        // SAFETY: inspect only the live borrowed connection's identity; never
        // dereference its pointer or call through it.
        let handle = unsafe { source.handle() } as usize;
        if !Arc::ptr_eq(&self.context.0, &context.0)
            || !matches!(self.kind, GuardKind::Temporary(_))
            || context.0.lock().map_err(|_| refused())?.sqlite_handle != Some(handle)
        {
            return Err(refused());
        }
        Ok(())
    }

    // Fixed copied-image ownership transitions only. No public credit or host
    // scalar enters these methods; both guards retain this exact source Arc.
    pub(super) fn merge_image_payload(&mut self, other: &mut Self, db: &Connection) -> Result<()> {
        self.require_same_source(&other.context, db)?;
        other.require_same_source(&self.context, db)?;
        let (GuardKind::Temporary(a), GuardKind::Temporary(b)) = (&mut self.kind, &mut other.kind) else { return Err(refused()); };
        let combined = add(*a, *b)?;
        *a = combined;
        *b = 0;
        Ok(())
    }
    /// One fixed construction-to-immutable-output transition. The caller has
    /// destroyed the producer/C/D cursors and measures the actual retained
    /// verifier, serialized backing, inventory and labels before this call.
    /// This never retires another guard, persistent high-water, or host bytes.
    pub(super) fn seal_image_payload(&mut self, db: &Connection, bytes: u64) -> Result<()> {
        self.require_same_source(&self.context, db)?;
        let GuardKind::Temporary(own) = &mut self.kind else { return Err(refused()); };
        if *own == 0 || bytes == 0 { return Err(refused()); }
        let mut state = self.context.0.lock().map_err(|_| refused())?;
        let other = AllocationContext::checkpoint(&mut state, db)?;
        if other != 0 { return Err(refused()); }
        // First prove that the construction phase is STILL covered. A smaller
        // output cannot rehabilitate a previously uncovered cache/phase peak.
        let old = AllocationContext::total(&state, 0)?;
        let temporary = state.temporary.checked_sub(*own)
            .and_then(|n| n.checked_add(bytes)).ok_or_else(refused)?;
        let next = old.checked_sub(*own).and_then(|n| n.checked_add(bytes))
            .ok_or_else(refused)?;
        if next > LIMIT { return Err(refused()); }
        state.temporary = temporary;
        *own = bytes;
        Ok(())
    }
    pub(super) fn commit_image_payload(&mut self, history: &mut Self, db: &Connection, bytes: u64) -> Result<()> {
        self.require_same_source(&history.context, db)?;
        history.require_same_source(&self.context, db)?;
        let (GuardKind::Temporary(own), GuardKind::Temporary(hist)) = (&mut self.kind, &mut history.kind) else { return Err(refused()); };
        if *own == 0 && *hist == 0 { return Err(refused()); }
        let mut state = self.context.0.lock().map_err(|_| refused())?;
        let other = AllocationContext::checkpoint(&mut state, db)?;
        if other != 0 { return Err(refused()); }
        let released = add(*own, *hist)?;
        let temporary = state.temporary.checked_sub(released).ok_or_else(refused)?;
        let image = state.image.max(bytes);
        // Validate the complete replacement before any ledger mutation. The
        // payloads stay owned by image/history while the same mutex transfers
        // their already-live coverage; failure leaves both guards unchanged.
        let old = AllocationContext::total(&state, 0)?;
        let next = old.checked_sub(released).and_then(|n| n.checked_add(image - state.image)).ok_or_else(refused)?;
        if next > LIMIT { return Err(refused()); }
        state.temporary = temporary;
        state.image = image;
        *own = 0;
        *hist = 0;
        Ok(())
    }

    pub(in crate::migration) fn grow(&mut self, db: &Connection, bytes: u64) -> Result<()> {
        let GuardKind::Temporary(ref mut retained) = self.kind else {
            return Err(refused());
        };
        let mut state = self.context.0.lock().map_err(|_| refused())?;
        let other = AllocationContext::checkpoint(&mut state, db)?;
        AllocationContext::total(&state, add(other, bytes)?)?;
        state.temporary = add(state.temporary, bytes)?;
        *retained = add(*retained, bytes)?;
        Ok(())
    }
}
impl Drop for AllocationGuard {
    fn drop(&mut self) {
        drop(self.attachment.take());
        // Mutex poisoning remains fail closed. Dropping a lexical or retained
        // storage reservation never alters the real OperationBudget.
        if let Ok(mut state) = self.context.0.lock() {
            match self.kind {
                GuardKind::Temporary(bytes) => state.temporary -= bytes,
                GuardKind::Operation => {
                    if let Some((_, _, count)) = &mut state.operation {
                        *count -= 1;
                        if *count == 0 {
                            state.operation = None;
                        }
                    }
                }
            }
        }
    }
}
/// Borrowed preflight; no serialization buffer or cloned tree is needed.
pub(in crate::migration) fn value_bytes(value: &Value) -> Result<u64> {
    fn visit(v: &Value, depth: usize, nodes: &mut usize) -> Result<u64> {
        *nodes = nodes.checked_add(1).ok_or_else(refused)?;
        if depth > 128 || *nodes > 1_000_000 {
            return Err(refused());
        }
        let mut bytes = std::mem::size_of::<Value>() as u64;
        match v {
            Value::String(s) => bytes = add(bytes, s.capacity() as u64)?,
            Value::Array(a) => {
                bytes = add(
                    bytes,
                    (a.capacity() as u64)
                        .checked_mul(std::mem::size_of::<Value>() as u64)
                        .ok_or_else(refused)?,
                )?;
                for v in a {
                    bytes = add(bytes, visit(v, depth + 1, nodes)?)?;
                }
            }
            Value::Object(map) => {
                for (k, v) in map {
                    bytes = add(bytes, add(NODE_BYTES, k.capacity() as u64)?)?;
                    bytes = add(bytes, visit(v, depth + 1, nodes)?)?;
                }
            }
            _ => (),
        }
        Ok(bytes)
    }
    visit(value, 0, &mut 0)
}

// Thread-affine private migration scope. Only retained offline owners install
// it; ordinary migration rows retain their exact previous path.
thread_local! { static ACTIVE: std::cell::RefCell<Option<AllocationContext>> = const { std::cell::RefCell::new(None) }; }
pub(super) struct AllocationScope {
    previous: Option<AllocationContext>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl AllocationContext {
    pub(super) fn enter(&self) -> Result<AllocationScope> {
        ACTIVE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|active| !Arc::ptr_eq(&active.0, &self.0))
            {
                return Err(refused());
            }
            Ok(AllocationScope {
                previous: slot.replace(self.clone()),
                _thread: std::marker::PhantomData,
            })
        })
    }
}
impl Drop for AllocationScope {
    fn drop(&mut self) {
        ACTIVE.with(|slot| *slot.borrow_mut() = self.previous.take());
    }
}
pub(in crate::migration) fn current() -> Option<AllocationContext> {
    ACTIVE.with(|slot| slot.borrow().clone())
}
/// Same row remains borrowed until this preflight succeeds. The factor covers
/// typed values, doubled hex, worst-case canonical escaping and their live
/// encoder copies, rather than pretending wire length is owned storage.
pub(in crate::migration) fn row_reservation(
    db: &Connection,
    row: &rusqlite::Row<'_>,
    columns: usize,
) -> Result<Option<AllocationGuard>> {
    let Some(context) = current() else {
        return Ok(None);
    };
    let bytes = row_bytes(row, columns)?;
    Ok(Some(context.reserve(db, bytes)?))
}

pub(in crate::migration) fn row_bytes(row: &rusqlite::Row<'_>, columns: usize) -> Result<u64> {
    let mut bytes = (columns as u64)
        .checked_mul(NODE_BYTES * 4)
        .ok_or_else(refused)?;
    for i in 0..columns {
        let len = match row.get_ref(i)? {
            rusqlite::types::ValueRef::Text(v) | rusqlite::types::ValueRef::Blob(v) => {
                v.len() as u64
            }
            _ => 32,
        };
        bytes = add(bytes, len.checked_mul(24).ok_or_else(refused)?)?;
    }
    Ok(bytes)
}

/// Fixed metadata inputs that feed legacy non-streaming schema/ledger helpers.
/// Their worst-case row/string copies fit the separately retained32MiB phase
/// reserve. This is private offline admission, never normal migration policy.
pub(super) fn catalog_preflight(db: &Connection) -> Result<()> {
    let (rows, bytes): (i64, i64) = db.query_row("SELECT count(*),coalesce(sum(length(CAST(name AS BLOB))+length(CAST(tbl_name AS BLOB))+coalesce(length(CAST(sql AS BLOB)),0)),0) FROM sqlite_schema", [], |r| Ok((r.get(0)?,r.get(1)?)))?;
    let (history, history_bytes): (i64, i64) = db.query_row("SELECT count(*),coalesce(sum(length(CAST(filename AS BLOB))+length(CAST(checksum AS BLOB))+length(CAST(origin AS BLOB))+length(CAST(applied_at AS BLOB))),0) FROM schema_migrations", [], |r| Ok((r.get(0)?,r.get(1)?)))?;
    let identity_bytes: i64 = db.query_row("SELECT coalesce(sum(length(CAST(catalog_id AS BLOB))+length(CAST(incarnation AS BLOB))),0) FROM catalog_state", [], |r| r.get(0))?;
    let database_id_bytes: i64 = db.query_row(
        "SELECT coalesce(sum(length(CAST(value AS BLOB))),0) FROM meta WHERE key='database_id'",
        [],
        |r| r.get(0),
    )?;
    if !(0..=1024 * 1024).contains(&identity_bytes)
        || !(0..=1024 * 1024).contains(&database_id_bytes)
    {
        return Err(refused());
    }
    if !(0..=4096).contains(&rows)
        || !(0..=4 * 1024 * 1024).contains(&bytes)
        || !(0..=64).contains(&history)
        || !(0..=1024 * 1024).contains(&history_bytes)
    {
        return Err(refused());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_capture_guard_transfer_is_atomic_same_source_and_preserves_highwater() {
        let db = Connection::open_in_memory().unwrap();
        let foreign = Connection::open_in_memory().unwrap();
        let context = AllocationContext::new(OfflineAllocationReservation::new(1234).unwrap()).unwrap();
        context.bind_source(&db).unwrap();
        let other = AllocationContext::new(OfflineAllocationReservation::new(0).unwrap()).unwrap();
        other.bind_source(&foreign).unwrap();
        let mut image = context.reserve(&db, 4096).unwrap();
        let mut history = context.reserve(&db, 1024).unwrap();
        let mut inventory = context.reserve(&db, 512).unwrap();
        let mut stranger = other.reserve(&foreign, 512).unwrap();
        let before = context.temporary_bytes_for_test().unwrap();
        assert!(image.merge_image_payload(&mut stranger, &db).is_err());
        assert!(image.merge_image_payload(&mut inventory, &foreign).is_err());
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before);
        image.merge_image_payload(&mut inventory, &db).unwrap();
        drop(inventory);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before);
        assert!(image.commit_image_payload(&mut history, &db, LIMIT).is_err());
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before);
        assert_eq!(context.retained_components_for_test().unwrap().1, 0);
        image.commit_image_payload(&mut history, &db, 5632).unwrap();
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
        assert_eq!(context.retained_components_for_test().unwrap().1, 5632);
        assert_eq!(context.external_retained_bytes().unwrap(), 1234);
        assert!(image.commit_image_payload(&mut history, &db, 5632).is_err());
        assert!(image.commit_image_payload(&mut history, &db, 6000).is_err());
        drop(image); drop(history);
        assert_eq!(context.retained_components_for_test().unwrap().1, 5632);
    }
    #[test]
    fn current_capture_output_seal_retires_only_its_construction_guard() {
        let db = Connection::open_in_memory().unwrap();
        let context = AllocationContext::new(OfflineAllocationReservation::new(1234).unwrap()).unwrap();
        context.bind_source(&db).unwrap();
        context.retain_image(&db, 2048).unwrap();
        context.retain_preservation(512, &db).unwrap();
        let before_persistent = context.retained_components_for_test().unwrap();
        let mut image = context.reserve(&db, 8192).unwrap();
        let mut history = context.reserve(&db, 1024).unwrap();
        let other = context.reserve(&db, 256).unwrap();
        image.seal_image_payload(&db, 4096).unwrap();
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 4096 + 1024 + 256);
        assert_eq!(context.retained_components_for_test().unwrap(), before_persistent);
        assert_eq!(context.external_retained_bytes().unwrap(), 1234);
        // The immutable output keeps full measured coverage even alongside a
        // preexisting persistent image high-water. No scalar credit is reused.
        image.commit_image_payload(&mut history, &db, 5120).unwrap();
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 256);
        assert_eq!(context.retained_components_for_test().unwrap(), (512, 5120));
        context.check(Some(&db), 0).unwrap();
        drop(other); drop(history); drop(image);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
        assert_eq!(context.retained_components_for_test().unwrap(), (512, 5120));
    }

    #[test]
    fn current_capture_output_seal_refuses_foreign_oversize_and_uncovered_old_peak() {
        let db = Connection::open_in_memory().unwrap();
        let foreign = Connection::open_in_memory().unwrap();
        let context = AllocationContext::new(OfflineAllocationReservation::new(1234).unwrap()).unwrap();
        context.bind_source(&db).unwrap();
        let mut image = context.reserve(&db, 8192).unwrap();
        let before = context.temporary_bytes_for_test().unwrap();
        assert!(image.seal_image_payload(&foreign, 4096).is_err());
        assert!(image.seal_image_payload(&db, LIMIT).is_err());
        assert!(image.seal_image_payload(&db, 0).is_err());
        assert_eq!(context.temporary_bytes_for_test().unwrap(), before);
        let used = context.retained_native_bytes(&db).unwrap() + 1234;
        let pressure = context.reserve(&db, LIMIT - used).unwrap();
        let full = context.temporary_bytes_for_test().unwrap();
        // Real source SQLite growth, not a fabricated ledger mutation. The
        // smaller proposed image must NOT make an uncovered old phase legal.
        db.execute_batch("CREATE TABLE growth(value BLOB); INSERT INTO growth VALUES(zeroblob(16384))").unwrap();
        let error = image.seal_image_payload(&db, 1).unwrap_err();
        assert_eq!(error.to_string(), "offline aggregate allocation budget exhausted");
        assert_eq!(context.temporary_bytes_for_test().unwrap(), full);
        assert_eq!(context.retained_components_for_test().unwrap().1, 0);
        assert_eq!(context.external_retained_bytes().unwrap(), 1234);
        assert_eq!(db.query_row("SELECT length(value) FROM growth", [], |r| r.get::<_, i64>(0)).unwrap(), 16384);
        drop(pressure);
        image.seal_image_payload(&db, 4096).unwrap();
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 4096);
        drop(image);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
    }

    #[test]
    fn current_capture_output_seal_unwind_releases_payload_coverage_without_reset() {
        let db = Connection::open_in_memory().unwrap();
        let context = AllocationContext::new(OfflineAllocationReservation::new(1234).unwrap()).unwrap();
        context.bind_source(&db).unwrap();
        context.retain_image(&db, 2048).unwrap();
        let persistent = context.retained_components_for_test().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut image = context.reserve(&db, 8192).unwrap();
            let history = context.reserve(&db, 1024).unwrap();
            image.seal_image_payload(&db, 4096).unwrap();
            assert_eq!(context.temporary_bytes_for_test().unwrap(), 5120);
            let _keep_history = history;
            panic!("controlled immutable output retention unwind");
        }));
        assert!(result.is_err());
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
        assert_eq!(context.external_retained_bytes().unwrap(), 1234);
        assert_eq!(context.retained_components_for_test().unwrap(), persistent);
        context.check(Some(&db), 0).unwrap();
    }

    #[test]
    fn persistent_preservation_and_copy_remain_additive_at_limit() {
        let db = Connection::open_in_memory().unwrap();
        let context =
            AllocationContext::new(OfflineAllocationReservation::new(0).unwrap()).unwrap();
        context.bind_source(&db).unwrap();
        context.retain_preservation(1024, &db).unwrap();
        context.retain_image(&db, 2048).unwrap();
        let used = context.retained_native_bytes(&db).unwrap();
        let image = 2048 + LIMIT.checked_sub(used).unwrap();
        context.retain_image(&db, image).unwrap();
        assert_eq!(context.retained_native_bytes(&db).unwrap(), LIMIT);
        assert!(context.retain_preservation(1025, &db).is_err());
        assert_eq!(
            context.retained_components_for_test().unwrap(),
            (1024, image)
        );
        // Discard/retry cannot reclaim a conservative high-water and let the
        // next live command overwrite the independently retained image bytes.
        context.retain_image(&db, 0).unwrap();
        assert_eq!(
            context.retained_components_for_test().unwrap(),
            (1024, image)
        );
        assert!(context.retain_preservation(1025, &db).is_err());
    }
    #[test]
    fn large_allowance_with_tiny_actual_data_does_not_reserve_maxima() {
        use crate::operation_budget::{BudgetCharge, BudgetLimits};
        let db = Connection::open_in_memory().unwrap();
        let context =
            AllocationContext::new(OfflineAllocationReservation::new(128 * 1024 * 1024).unwrap())
                .unwrap();
        context.bind_source(&db).unwrap();
        let budget = OperationBudget::new(
            std::time::Duration::from_secs(5),
            BudgetLimits {
                items: 1_000_000,
                values: 1_000_000,
                bytes: 64 * 1024 * 1024,
                work: 1_000_000,
            },
        )
        .unwrap();
        let _guard = context.operation(&db, &budget).unwrap();
        budget
            .charge(BudgetCharge {
                values: 1,
                bytes: 1,
                ..Default::default()
            })
            .unwrap();
        assert!(
            budget
                .charge(BudgetCharge {
                    values: 200_000,
                    ..Default::default()
                })
                .is_err()
        );
        assert!(budget.charge(BudgetCharge::default()).is_err());
    }
    #[test]
    fn reservation_overflow_and_decrease_refuse() {
        assert!(OfflineAllocationReservation::new(u64::MAX).is_err());
        let owner =
            AllocationContext::new(OfflineAllocationReservation::new(1024).unwrap()).unwrap();
        assert_eq!(owner.external_retained_bytes().unwrap(), 1024);
        assert!(
            owner
                .refresh(OfflineAllocationReservation::default(), None)
                .is_err()
        );
        owner
            .refresh(OfflineAllocationReservation::new(2048).unwrap(), None)
            .unwrap();
        assert_eq!(owner.external_retained_bytes().unwrap(), 2048);
    }
    #[test]
    fn retained_capacity_and_simultaneous_copies_are_counted() {
        let mut s = String::with_capacity(1024 * 1024);
        s.push('x');
        let v = Value::String(s);
        assert!(value_bytes(&v).unwrap() >= 1024 * 1024);
        let db = Connection::open_in_memory().unwrap();
        let context = AllocationContext::default();
        context.bind_source(&db).unwrap();
        context.retain_plan(&v, &db).unwrap();
        let _one = context.reserve(&db, 128 * 1024 * 1024).unwrap();
        assert!(context.reserve(&db, 128 * 1024 * 1024).is_err());
    }
    #[test]
    fn nested_scratch_shares_only_exact_operation_without_reset() {
        use crate::operation_budget::{BudgetCharge, BudgetLimits};
        use std::time::Duration;
        let db = Connection::open_in_memory().unwrap();
        let context = AllocationContext::default();
        context.bind_source(&db).unwrap();
        let budget = || {
            OperationBudget::new(
                Duration::from_secs(5),
                BudgetLimits {
                    items: 2,
                    values: 3,
                    bytes: 4,
                    work: 5,
                },
            )
            .unwrap()
        };
        let first = budget();
        let _outer = context.operation(&db, &first).unwrap();
        {
            let _inner = context.operation(&db, &first).unwrap();
        }
        assert!(context.operation(&db, &budget()).is_err());
        first
            .charge(BudgetCharge {
                work: 5,
                ..Default::default()
            })
            .unwrap();
        assert!(
            first
                .charge(BudgetCharge {
                    work: 1,
                    ..Default::default()
                })
                .is_err()
        );
    }
    #[test]
    fn row_preflight_preserves_canonical_digest_and_refuses_large_cells() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE sample(a,b,c,d,e); INSERT INTO sample VALUES(NULL,7,1.25,'quote\"',x'00ff'); INSERT INTO sample VALUES('other',-4,2.5,'line',x'12');").unwrap();
        let columns: Vec<String> = ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let ordinary = crate::migration::digest_table(&db, "sample", &columns).unwrap();
        let context = AllocationContext::default();
        context.bind_source(&db).unwrap();
        {
            let _scope = context.enter().unwrap();
            assert_eq!(
                ordinary,
                crate::migration::digest_table(&db, "sample", &columns).unwrap()
            );
        }
        let constrained =
            AllocationContext::new(OfflineAllocationReservation::new(220 * 1024 * 1024).unwrap())
                .unwrap();
        constrained.bind_source(&db).unwrap();
        for cell in ["zeroblob(2000000)", "CAST(zeroblob(2000000) AS TEXT)"] {
            db.execute_batch(&format!(
                "DELETE FROM sample; INSERT INTO sample(a) VALUES({cell})"
            ))
            .unwrap();
            let before: i64 = db
                .query_row("SELECT length(CAST(a AS BLOB)) FROM sample", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let _scope = constrained.enter().unwrap();
            assert!(crate::migration::digest_table(&db, "sample", &columns).is_err());
            assert_eq!(
                before,
                db.query_row("SELECT length(CAST(a AS BLOB)) FROM sample", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap()
            );
        }
    }
}
