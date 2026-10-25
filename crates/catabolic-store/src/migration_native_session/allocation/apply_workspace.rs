//! Exact apply metadata workspace. This is not a budget credit or a new limit.
//! The original reservation remains charged while a single pinned-image phase
//! borrows it. The caller's retained outputs are independently capacity-checked.
use super::*;
use std::{marker::PhantomData, path::PathBuf, rc::Rc};

#[cfg(test)]
pub(in crate::migration::native_session) struct ApplyOuter<'a> {
    pub plan: &'a Value,
    pub info: &'a Value,
    pub before: &'a Value,
    pub receipt: &'a Value,
    pub default_parent: &'a PathBuf,
    pub directory: &'a PathBuf,
}
#[cfg(test)]
impl ApplyOuter<'_> {
    fn bytes(&self) -> Result<u64> {
        let mut bytes = 0;
        for value in [self.plan, self.info, self.before, self.receipt] {
            bytes = add(bytes, value_bytes(value)?)?;
        }
        for path in [self.default_parent, self.directory] {
            bytes = add(bytes, std::mem::size_of::<PathBuf>() as u64)?;
            bytes = add(bytes, path.capacity() as u64)?;
        }
        Ok(bytes)
    }
}

/// The only owned values surviving backup construction into a pinned image.
/// The approved plan and its tables are borrowed from the independently retained
/// original plan, not duplicated or charged again to this cohort.
pub(in crate::migration::native_session) struct ApplyRetainedOuter<'a> {
    pub receipt: &'a Value,
    pub database_id: &'a Value,
    pub directory: &'a PathBuf,
}
impl ApplyRetainedOuter<'_> {
    fn bytes(&self) -> Result<u64> {
        let mut bytes = value_bytes(self.receipt)?;
        bytes = add(bytes, value_bytes(self.database_id)?)?;
        bytes = add(bytes, std::mem::size_of::<PathBuf>() as u64)?;
        add(bytes, self.directory.capacity() as u64)
    }
}

enum BorrowedApplyOuter<'a> {
    #[cfg(test)]
    Construction(ApplyOuter<'a>),
    Retained(ApplyRetainedOuter<'a>),
}
impl BorrowedApplyOuter<'_> {
    fn bytes(&self) -> Result<u64> {
        match self {
            #[cfg(test)]
            Self::Construction(outer) => outer.bytes(),
            Self::Retained(outer) => outer.bytes(),
        }
    }
}

/// A lexical handoff, not a smaller plan multiplier or an allocation credit.
/// Construction keeps its original six-plan allowance. Only after its actual
/// plan/info/table/path temporaries have ended may a pinned-image phase retain
/// the independently measured immutable survivors instead. Replacement admission
/// always precedes release of the previous charge.
pub(in crate::migration::native_session) struct ApplyCopyReservation<'a> {
    context: &'a AllocationContext,
    source: &'a Connection,
    construction_bytes: u64,
    construction: Option<AllocationGuard>,
    retained: Option<AllocationGuard>,
}
impl<'a> ApplyCopyReservation<'a> {
    pub(in crate::migration::native_session) fn new(
        context: &'a AllocationContext,
        source: &'a Connection,
        plan: &Value,
    ) -> Result<Self> {
        context.retain_plan(plan, source)?;
        let construction_bytes = value_bytes(plan)?
            .checked_mul(6)
            .ok_or_else(|| Error("offline plan copy allocation overflow".into()))?;
        let construction = Some(context.reserve(source, construction_bytes)?);
        Ok(Self { context, source, construction_bytes, construction, retained: None })
    }
    pub(in crate::migration::native_session) fn retain_image_outputs(
        &mut self,
        outer: ApplyRetainedOuter<'_>,
        backup: &super::super::backup::VerifiedBackup,
    ) -> Result<()> {
        require_source(self.context, self.source)?;
        if self.construction.is_none() || self.retained.is_some() {
            return Err(refused());
        }
        let bytes = add(outer.bytes()?, backup.allocated_bytes()?)?;
        if bytes > self.construction_bytes {
            return Err(Error("offline apply retained outputs exceed construction reservation".into()));
        }
        // Admission may fail: in that case the original construction guard is
        // untouched, so the existing failure-manifest producer remains covered.
        self.retained = Some(self.context.reserve(self.source, bytes)?);
        drop(self.construction.take());
        #[cfg(test)]
        super::super::apply_plan_lifetime_tests::retained_phase(self.context, self.construction_bytes, bytes);
        Ok(())
    }
    pub(in crate::migration::native_session) fn restore_construction(&mut self) -> Result<()> {
        if self.construction.is_none() {
            #[cfg(test)]
            let _pressure = super::super::apply_plan_lifetime_tests::before_restore(self.context, self.source);
            self.construction = Some(self.context.reserve(self.source, self.construction_bytes)?);
            drop(self.retained.take());
            #[cfg(test)]
            super::super::apply_plan_lifetime_tests::restored_phase(self.context);
        }
        Ok(())
    }
    pub(in crate::migration::native_session) fn construction_is_held(&self) -> bool {
        self.construction.is_some()
    }
    pub(in crate::migration::native_session) fn guard(&self) -> &AllocationGuard {
        self.construction.as_ref().or(self.retained.as_ref()).expect("apply copy phase retains a guard")
    }
}

pub(in crate::migration::native_session) struct ApplyMetadataWorkspace<'source> {
    reservation: MetadataReservation<'source>,
    source: &'source Connection,
    loaned: bool,
    _thread: PhantomData<Rc<()>>,
}
impl<'source> ApplyMetadataWorkspace<'source> {
    pub(in crate::migration::native_session) fn new(
        context: &AllocationContext,
        source: &'source Connection,
    ) -> Result<Self> {
        Self::with_control_workspace(context, source, None)
    }
    pub(in crate::migration::native_session) fn with_control_workspace(
        context: &AllocationContext,
        source: &'source Connection,
        control: Option<&'source mut ControlWorkspaceLoan<'_>>,
    ) -> Result<Self> {
        require_source(context, source)?;
        Ok(Self {
            reservation: MetadataReservation::new(context, source, control)?,
            source,
            loaned: false,
            _thread: PhantomData,
        })
    }

    #[cfg(test)]
    pub(in crate::migration::native_session) fn loan<'loan>(
        &'loan mut self,
        context: &AllocationContext,
        source: &Connection,
        copies: &'loan AllocationGuard,
        outer: ApplyOuter<'loan>,
    ) -> Result<ApplyMetadataLoan<'loan, 'source>> {
        self.loan_inner(context, source, copies, BorrowedApplyOuter::Construction(outer))
    }

    pub(in crate::migration::native_session) fn loan_retained<'loan>(
        &'loan mut self,
        context: &AllocationContext,
        source: &Connection,
        copies: &'loan ApplyCopyReservation<'_>,
        outer: ApplyRetainedOuter<'loan>,
    ) -> Result<ApplyMetadataLoan<'loan, 'source>> {
        self.loan_inner(context, source, copies.guard(), BorrowedApplyOuter::Retained(outer))
    }

    fn loan_inner<'loan>(
        &'loan mut self,
        context: &AllocationContext,
        source: &Connection,
        copies: &'loan AllocationGuard,
        outer: BorrowedApplyOuter<'loan>,
    ) -> Result<ApplyMetadataLoan<'loan, 'source>> {
        require_source(context, source)?;
        if self.loaned
            || !Arc::ptr_eq(&context.0, &self.reservation.context().0)
            || !Arc::ptr_eq(&context.0, &copies.context.0)
            || connection_identity(source) != connection_identity(self.source)
            || !self.reservation.is_metadata()
        {
            return Err(refused());
        }
        let GuardKind::Temporary(copy_bytes) = copies.kind else {
            return Err(refused());
        };
        if outer.bytes()? > copy_bytes {
            return Err(Error(
                "offline apply retained outputs exceed separate copy reservation".into(),
            ));
        }
        context.check(Some(source), 0)?;
        self.loaned = true;
        Ok(ApplyMetadataLoan {
            workspace: self,
            copies,
            outer,
        })
    }
}

/// No Clone, serialization, public constructor, amount, or context replacement.
/// Both the workspace and its separately charged immutable outputs remain
/// borrowed until the image and its additional allocation guard have retired.
pub(in crate::migration::native_session) struct ApplyMetadataLoan<'loan, 'source> {
    workspace: &'loan mut ApplyMetadataWorkspace<'source>,
    copies: &'loan AllocationGuard,
    outer: BorrowedApplyOuter<'loan>,
}
impl ApplyMetadataLoan<'_, '_> {
    pub(in crate::migration::native_session) fn context(&self) -> &AllocationContext {
        self.workspace.reservation.context()
    }
    pub(in crate::migration::native_session) fn source(&self) -> &Connection {
        self.workspace.source
    }
    pub(in crate::migration::native_session) fn require_backup(
        &self,
        backup: &super::super::backup::VerifiedBackup,
    ) -> Result<()> {
        require_source(self.context(), self.source())?;
        let GuardKind::Temporary(copy_bytes) = self.copies.kind else {
            return Err(refused());
        };
        // No free scalar/path allowance: the exact pinned owner's heap framing
        // is also covered by the exact separate reservation for this phase.
        if add(self.outer.bytes()?, backup.allocated_bytes()?)? > copy_bytes {
            return Err(Error(
                "offline apply retained backup exceeds separate copy reservation".into(),
            ));
        }
        self.context().check(Some(self.source()), 0)
    }
}
impl Drop for ApplyMetadataLoan<'_, '_> {
    fn drop(&mut self) {
        // The metadata guard never moved or stopped charging. Only exclusive
        // access is restored, including on error and unwind.
        self.workspace.loaned = false;
    }
}

fn connection_identity(source: &Connection) -> usize {
    // SAFETY: inspect identity only. The shared borrow keeps this connection
    // alive; no call through or dereference of its native pointer is performed.
    unsafe { source.handle() as usize }
}
fn require_source(context: &AllocationContext, source: &Connection) -> Result<()> {
    if context.0.lock().map_err(|_| refused())?.sqlite_handle != Some(connection_identity(source)) {
        return Err(refused());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Outer {
        plan: Value,
        info: Value,
        before: Value,
        receipt: Value,
        default_parent: PathBuf,
        directory: PathBuf,
    }
    impl Outer {
        fn new() -> Self {
            Self {
                plan: json!({"plan": "approved"}),
                info: json!({"schema": 40}),
                before: json!({"tables": []}),
                receipt: json!({"backup": "held.sqlite"}),
                default_parent: PathBuf::from("default-parent"),
                directory: PathBuf::from("saved-directory"),
            }
        }
        fn borrow(&self) -> ApplyOuter<'_> {
            ApplyOuter {
                plan: &self.plan,
                info: &self.info,
                before: &self.before,
                receipt: &self.receipt,
                default_parent: &self.default_parent,
                directory: &self.directory,
            }
        }
    }
    fn context(db: &Connection) -> AllocationContext {
        let context = AllocationContext::default();
        context.bind_source(db).unwrap();
        context
    }
    #[test]
    fn workspace_requires_exact_context_source_and_separate_copy_guard() {
        let db = Connection::open_in_memory().unwrap();
        let foreign = Connection::open_in_memory().unwrap();
        let context = context(&db);
        let other_context = {
            let other = AllocationContext::default();
            other.bind_source(&db).unwrap();
            other
        };
        let outer = Outer::new();
        let copies = context
            .reserve(&db, outer.borrow().bytes().unwrap())
            .unwrap();
        let foreign_copies = other_context
            .reserve(&db, outer.borrow().bytes().unwrap())
            .unwrap();
        let mut workspace = ApplyMetadataWorkspace::new(&context, &db).unwrap();
        let held = context.temporary_bytes_for_test().unwrap();
        assert!(
            workspace
                .loan(&other_context, &db, &copies, outer.borrow())
                .is_err()
        );
        assert!(
            workspace
                .loan(&context, &foreign, &copies, outer.borrow())
                .is_err()
        );
        assert!(
            workspace
                .loan(&context, &db, &foreign_copies, outer.borrow())
                .is_err()
        );
        assert!(ApplyMetadataWorkspace::new(&context, &foreign).is_err());
        assert_eq!(context.temporary_bytes_for_test().unwrap(), held);
        // Safe Rust also prevents a second mutable borrow while a loan exists.
        // Exercise the defensive overlap check without manufacturing aliases.
        workspace.loaned = true;
        assert!(
            workspace
                .loan(&context, &db, &copies, outer.borrow())
                .is_err()
        );
        workspace.loaned = false;
        let loan = workspace
            .loan(&context, &db, &copies, outer.borrow())
            .unwrap();
        assert_eq!(loan.context().temporary_bytes_for_test().unwrap(), held);
        drop(loan);
        assert!(!workspace.loaned);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), held);
    }
    #[test]
    fn outer_capacity_refuses_without_spending_metadata_workspace() {
        let db = Connection::open_in_memory().unwrap();
        let context = context(&db);
        let mut outer = Outer::new();
        let copies = context
            .reserve(&db, outer.borrow().bytes().unwrap())
            .unwrap();
        let mut workspace = ApplyMetadataWorkspace::new(&context, &db).unwrap();
        let mut large = String::with_capacity(1024 * 1024);
        large.push('x');
        outer.receipt = Value::String(large);
        let held = context.temporary_bytes_for_test().unwrap();
        assert!(
            workspace
                .loan(&context, &db, &copies, outer.borrow())
                .is_err()
        );
        assert!(!workspace.loaned);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), held);
    }
    #[test]
    fn workspace_unwind_restores_loan_without_resetting_operation_or_retention() {
        use crate::operation_budget::{BudgetCharge, BudgetLimits};
        use std::time::Duration;
        let db = Connection::open_in_memory().unwrap();
        let context = context(&db);
        context
            .refresh(OfflineAllocationReservation::new(1234).unwrap(), Some(&db))
            .unwrap();
        context.retain_preservation(4321, &db).unwrap();
        context.retain_image(&db, 8765).unwrap();
        let budget = OperationBudget::new(
            Duration::from_secs(60),
            BudgetLimits {
                items: 8,
                values: 8,
                bytes: 1024,
                work: 8,
            },
        )
        .unwrap();
        let _operation = context.operation(&db, &budget).unwrap();
        budget
            .charge(BudgetCharge {
                work: 7,
                ..Default::default()
            })
            .unwrap();
        let deadline = budget.deadline();
        let usage = budget.usage();
        let outer = Outer::new();
        let copies = context
            .reserve(&db, outer.borrow().bytes().unwrap())
            .unwrap();
        let mut workspace = ApplyMetadataWorkspace::new(&context, &db).unwrap();
        let held = context.temporary_bytes_for_test().unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _loan = workspace
                .loan(&context, &db, &copies, outer.borrow())
                .unwrap();
            panic!("controlled workspace unwind");
        }));
        assert!(outcome.is_err());
        assert!(!workspace.loaned);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), held);
        assert_eq!(context.external_retained_bytes().unwrap(), 1234);
        assert_eq!(
            context.retained_components_for_test().unwrap(),
            (4321, 8765)
        );
        assert_eq!(budget.usage(), usage);
        assert_eq!(budget.deadline(), deadline);
        let _loan = workspace
            .loan(&context, &db, &copies, outer.borrow())
            .unwrap();
        budget
            .charge(BudgetCharge {
                work: 1,
                ..Default::default()
            })
            .unwrap();
        assert!(
            budget
                .charge(BudgetCharge {
                    work: 1,
                    ..Default::default()
                })
                .is_err()
        );
    }
    #[test]
    fn fixed_control_scratch_keeps_nested_backup_metadata_and_copies_separate() {
        let db = Connection::open_in_memory().unwrap();
        let mut backing = OfflineControlWorkspaceBacking::new();
        let context = backing.admit(OfflineAllocationReservation::default()).unwrap();
        context.bind_source(&db).unwrap();
        let outer = Outer::new();
        let copy_bytes = outer.borrow().bytes().unwrap();
        let copies = context.reserve(&db, copy_bytes).unwrap();
        let full = context.external_retained_bytes().unwrap();
        let mut control = backing.loan(&context, &db).unwrap();
        {
            let mut workspace = ApplyMetadataWorkspace::with_control_workspace(
                &context, &db, Some(&mut control),
            ).unwrap();
            for _ in 0..2 {
                let metadata = workspace.loan(&context, &db, &copies, outer.borrow()).unwrap();
                assert_eq!(metadata.context().temporary_bytes_for_test().unwrap(), copy_bytes);
                // The existing nested image path may borrow metadata, but its
                // copied image remains an independent additive reservation.
                let image = metadata.context().reserve(&db, 4096).unwrap();
                assert_eq!(context.temporary_bytes_for_test().unwrap(), copy_bytes + 4096);
                assert_eq!(context.external_retained_bytes().unwrap(), full);
                drop(image);
                drop(metadata);
            }
        }
        drop(control);
        assert_eq!(backing.state_for_test(), (full, false, false));
        assert_eq!(context.temporary_bytes_for_test().unwrap(), copy_bytes);
        drop(copies);
        assert_eq!(context.temporary_bytes_for_test().unwrap(), 0);
    }

}
