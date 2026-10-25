//! Affine original control admission, not scalar credit or a thread-local grant.
//! The full 64MiB remains in external high-water throughout every loan. At most
//! 32MiB of actual host control retention can coexist with one native 32MiB
//! metadata phase; all independent images, copies and outputs remain additive.
use super::*;
use std::{marker::PhantomData, rc::Rc};

/// Fixed control workspace registered only by original offline preparation.
/// Construct before host control admission and retain the same owner through
/// apply and the fixed qualified empty-bootstrap producers. It is neither
/// Clone nor a numeric reservation/limit override. Ordinary
/// scalar admission and refresh cannot create or substitute this backing.
pub struct OfflineControlWorkspaceBacking {
    context: Option<AllocationContext>,
}
impl OfflineControlWorkspaceBacking {
    pub const BYTES: u64 = 64 * 1024 * 1024;
    pub const RETAINED_LIMIT: u64 = Self::BYTES - MIGRATION_SCRATCH;
    pub fn new() -> Self {
        Self { context: None }
    }
    pub(in crate::migration) fn admit(
        &mut self,
        residual: OfflineAllocationReservation,
    ) -> Result<AllocationContext> {
        if self.context.is_some() {
            return Err(Error("offline control workspace already bound".into()));
        }
        let full = OfflineAllocationReservation::new(add(residual.external_retained_bytes, Self::BYTES)?)?;
        let context = AllocationContext::new(full)?;
        context.0.lock().map_err(|_| refused())?.control_workspace = true;
        self.context = Some(context.clone());
        Ok(context)
    }
    pub(in crate::migration) fn loan<'a>(
        &'a mut self,
        context: &AllocationContext,
        source: &Connection,
    ) -> Result<ControlWorkspaceLoan<'a>> {
        let bound = self.context.as_ref().ok_or_else(refused)?;
        if !Arc::ptr_eq(&bound.0, &context.0) {
            return Err(refused());
        }
        let source = connection_identity(source);
        let mut state = context.0.lock().map_err(|_| refused())?;
        if !state.control_workspace || state.external < Self::BYTES
            || state.sqlite_handle != Some(source) || state.control_loan || state.control_scratch
        {
            return Err(refused());
        }
        AllocationContext::total(&state, 0)?;
        state.control_loan = true;
        drop(state);
        Ok(ControlWorkspaceLoan { backing: self, source, _thread: PhantomData })
    }
}
impl Default for OfflineControlWorkspaceBacking {
    fn default() -> Self { Self::new() }
}

/// One fixed apply or bootstrap producer owns this exclusive borrow. No public
/// callback receives it;
/// each internal phase must explicitly borrow a scratch reservation from it.
pub(in crate::migration) struct ControlWorkspaceLoan<'a> {
    backing: &'a mut OfflineControlWorkspaceBacking,
    source: usize,
    _thread: PhantomData<Rc<()>>,
}
impl ControlWorkspaceLoan<'_> {
    /// A refresh is descriptive, not a new scalar admission. Only a live loan
    /// of the original backing can construct this reservation without charging
    /// a second metadata arena; all actual owner components remain additive.
    pub(in crate::migration) fn refresh_external_reservation(
        &mut self,
        context: &AllocationContext,
        source: &Connection,
        external_retained_bytes: u64,
    ) -> Result<()> {
        let _scratch = self.scratch(context, source)?;
        context.refresh(OfflineAllocationReservation { external_retained_bytes }, Some(source))
    }
    fn scratch<'a>(
        &'a mut self,
        context: &AllocationContext,
        source: &Connection,
    ) -> Result<ControlScratch<'a>> {
        let bound = self.backing.context.as_ref().ok_or_else(refused)?;
        if !Arc::ptr_eq(&bound.0, &context.0) || self.source != connection_identity(source) {
            return Err(refused());
        }
        let mut state = context.0.lock().map_err(|_| refused())?;
        if !state.control_workspace || !state.control_loan || state.control_scratch
            || state.sqlite_handle != Some(self.source) || state.external < OfflineControlWorkspaceBacking::BYTES
        {
            return Err(refused());
        }
        AllocationContext::checkpoint(&mut state, source)?;
        AllocationContext::total(&state, 0)?;
        state.control_scratch = true;
        Ok(ControlScratch { context: context.clone(), _loan: PhantomData, _thread: PhantomData })
    }
}
impl Drop for ControlWorkspaceLoan<'_> {
    fn drop(&mut self) {
        if let Some(context) = &self.backing.context {
            if let Ok(mut state) = context.0.lock() {
                // The scratch's lifetime cannot outlive this exclusive loan.
                state.control_loan = false;
            }
        }
    }
}

pub(in crate::migration::native_session) struct ControlScratch<'a> {
    context: AllocationContext,
    _loan: PhantomData<&'a mut ()>,
    _thread: PhantomData<Rc<()>>,
}
impl Drop for ControlScratch<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.context.0.lock() {
            state.control_scratch = false;
        }
    }
}

/// Same full metadata reservation in ordinary/foreign/unreserved paths. Only
/// an explicit exact-context loan selects the fixed prepaid workspace instead.
pub(in crate::migration::native_session) enum MetadataReservation<'a> {
    Owned(AllocationGuard),
    Control(ControlScratch<'a>),
}
impl<'a> MetadataReservation<'a> {
    pub(in crate::migration::native_session) fn new(
        context: &AllocationContext,
        source: &Connection,
        loan: Option<&'a mut ControlWorkspaceLoan<'_>>,
    ) -> Result<Self> {
        match loan {
            Some(loan) => Ok(Self::Control(loan.scratch(context, source)?)),
            None => Ok(Self::Owned(context.reserve(source, MIGRATION_SCRATCH)?)),
        }
    }
    pub(in crate::migration::native_session) fn context(&self) -> &AllocationContext {
        match self { Self::Owned(guard) => &guard.context, Self::Control(guard) => &guard.context }
    }
    pub(in crate::migration::native_session) fn is_metadata(&self) -> bool {
        match self {
            Self::Owned(guard) => matches!(guard.kind, GuardKind::Temporary(MIGRATION_SCRATCH)),
            Self::Control(_) => true,
        }
    }
}
fn connection_identity(source: &Connection) -> usize {
    // SAFETY: read only this borrowed live connection's identity. No call
    // through, pointer dereference, or raw ownership transfer is performed.
    unsafe { source.handle() as usize }
}

#[cfg(test)]
impl OfflineControlWorkspaceBacking {
    pub(in crate::migration) fn state_for_test(&self) -> (u64, bool, bool) {
        let state = self.context.as_ref().unwrap().0.lock().unwrap();
        (state.external, state.control_loan, state.control_scratch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered(source: &Connection) -> (OfflineControlWorkspaceBacking, AllocationContext) {
        let mut backing = OfflineControlWorkspaceBacking::new();
        let context = backing.admit(OfflineAllocationReservation::new(1234).unwrap()).unwrap();
        context.bind_source(source).unwrap();
        (backing, context)
    }
    #[test]
    fn original_admission_adds_fixed_workspace_once_and_refresh_never_rebrands() {
        let source = Connection::open_in_memory().unwrap();
        let (mut backing, context) = registered(&source);
        let full = OfflineControlWorkspaceBacking::BYTES + 1234;
        assert_eq!(backing.state_for_test(), (full, false, false));
        assert!(backing.admit(OfflineAllocationReservation::default()).is_err());
        assert_eq!(backing.state_for_test(), (full, false, false));
        context.refresh(OfflineAllocationReservation::new(full + 7).unwrap(), Some(&source)).unwrap();
        assert!(context.refresh(OfflineAllocationReservation::new(full).unwrap(), Some(&source)).is_err());
        assert_eq!(backing.state_for_test(), (full + 7, false, false));
        let loan = backing.loan(&context, &source).unwrap();
        drop(loan);
        assert_eq!(backing.state_for_test(), (full + 7, false, false));
    }
    #[test]
    fn scalar_high_water_is_never_workspace_authority() {
        let source = Connection::open_in_memory().unwrap();
        let context = AllocationContext::new(OfflineAllocationReservation::new(192 * 1024 * 1024).unwrap()).unwrap();
        context.bind_source(&source).unwrap();
        context.refresh(OfflineAllocationReservation::new(200 * 1024 * 1024).unwrap(), Some(&source)).unwrap();
        let mut unbound = OfflineControlWorkspaceBacking::new();
        assert!(unbound.loan(&context, &source).is_err());
        assert!(!context.0.lock().unwrap().control_workspace);
        let held = context.temporary_bytes_for_test().unwrap();
        let reservation = MetadataReservation::new(&context, &source, None).unwrap();
        assert_eq!(context.temporary_bytes_for_test().unwrap(), held + MIGRATION_SCRATCH);
        drop(reservation);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), held);
    }
    #[test]
    fn foreign_context_source_and_defensive_nested_loans_refuse() {
        let source = Connection::open_in_memory().unwrap();
        let foreign = Connection::open_in_memory().unwrap();
        let (mut backing, context) = registered(&source);
        let (_, other) = registered(&source);
        assert!(backing.loan(&other, &source).is_err());
        assert!(backing.loan(&context, &foreign).is_err());
        context.0.lock().unwrap().control_loan = true;
        assert!(backing.loan(&context, &source).is_err());
        context.0.lock().unwrap().control_loan = false;
        let mut loan = backing.loan(&context, &source).unwrap();
        assert!(loan.scratch(&other, &source).is_err());
        assert!(loan.scratch(&context, &foreign).is_err());
        // Safe Rust prevents borrowing the loan twice; inspect its independent
        // runtime overlap refusal without ever manufacturing aliased borrows.
        context.0.lock().unwrap().control_scratch = true;
        assert!(loan.scratch(&context, &source).is_err());
        context.0.lock().unwrap().control_scratch = false;
        for _ in 0..3 {
            let scratch = loan.scratch(&context, &source).unwrap();
            assert!(context.0.lock().unwrap().control_scratch);
            assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
            drop(scratch);
            assert!(!context.0.lock().unwrap().control_scratch);
        }
        drop(loan);
        assert_eq!(backing.state_for_test(), (OfflineControlWorkspaceBacking::BYTES + 1234, false, false));
    }
    #[test]
    fn explicit_loan_does_not_discharge_full_external_or_independent_outputs() {
        let source = Connection::open_in_memory().unwrap();
        let (mut backing, context) = registered(&source);
        let external = context.external_retained_bytes().unwrap();
        let native = context.retained_native_bytes(&source).unwrap();
        context.retain_image(&source, LIMIT - external - native).unwrap();
        assert!(MetadataReservation::new(&context, &source, None).is_err());
        let mut loan = backing.loan(&context, &source).unwrap();
        let scratch = loan.scratch(&context, &source).unwrap();
        assert_eq!(context.external_retained_bytes().unwrap(), external);
        assert!(context.reserve(&source, 1).is_err());
        assert!(context.retain_preservation(1, &source).is_err());
        drop(scratch);
        drop(loan);
        assert_eq!(context.external_retained_bytes().unwrap(), external);
        // Neither the retained brand nor dropping the original backing enables
        // implicit reuse: ordinary callers still need their full native guard.
        drop(backing);
        assert!(MetadataReservation::new(&context, &source, None).is_err());
    }
    #[test]
    fn unwind_releases_exclusive_access_without_releasing_external_charge() {
        let source = Connection::open_in_memory().unwrap();
        let (mut backing, context) = registered(&source);
        context.retain_preservation(4321, &source).unwrap();
        context.retain_image(&source, 8765).unwrap();
        let before = backing.state_for_test().0;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut loan = backing.loan(&context, &source).unwrap();
            let _scratch = loan.scratch(&context, &source).unwrap();
            panic!("controlled affine control workspace unwind");
        }));
        assert!(result.is_err());
        assert_eq!(backing.state_for_test(), (before, false, false));
        assert_eq!(context.retained_components_for_test().unwrap(), (4321, 8765));
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
        let mut loan = backing.loan(&context, &source).unwrap();
        let _scratch = loan.scratch(&context, &source).unwrap();
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    #[test]
    fn exact_live_control_refresh_crosses_only_constructor_floor_and_keeps_all_owners_additive() {
        let source = Connection::open_in_memory().unwrap();
        let foreign_source = Connection::open_in_memory().unwrap();
        let mut backing = OfflineControlWorkspaceBacking::new();
        let context = backing.admit(OfflineAllocationReservation::default()).unwrap();
        context.bind_source(&source).unwrap();
        let independent = context.reserve(&source, 512 * 1024).unwrap();
        let native = context.retained_native_bytes(&source).unwrap();
        let forwarded = LIMIT - native - 1024 * 1024;
        assert!(OfflineAllocationReservation::new(forwarded).is_err());
        {
            let mut loan = backing.loan(&context, &source).unwrap();
            assert!(loan.refresh_external_reservation(&context, &foreign_source, forwarded).is_err());
            loan.refresh_external_reservation(&context, &source, forwarded).unwrap();
            assert_eq!(context.external_retained_bytes().unwrap(), forwarded);
            assert!(loan.refresh_external_reservation(&context, &source, forwarded - 1).is_err());
            assert!(loan.refresh_external_reservation(&context, &source, LIMIT).is_err());
            assert_eq!(context.external_retained_bytes().unwrap(), forwarded);
            assert!(context.reserve(&source, 1024 * 1024 + 1).is_err());
        }
        assert_eq!(backing.state_for_test(), (forwarded, false, false));
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 512 * 1024);
        assert!(MetadataReservation::new(&context, &source, None).is_err());
        let mut absent = OfflineControlWorkspaceBacking::new();
        assert!(absent.loan(&context, &source).is_err());
        let mut other = OfflineControlWorkspaceBacking::new();
        let other_context = other.admit(OfflineAllocationReservation::default()).unwrap();
        other_context.bind_source(&source).unwrap();
        assert!(other.loan(&context, &source).is_err());
        drop(source);
        drop(backing);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 512 * 1024);
        drop(independent);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
    }
}
