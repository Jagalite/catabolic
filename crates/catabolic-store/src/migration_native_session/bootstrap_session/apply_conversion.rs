//! Borrowed validation before the fixed native-upgrade -> Store move. This is
//! not recovery: the original live db, locks, backup and preservation never reopen.
use super::*;

// Exact closed Transition bounds plus the Store's duplicate catalog/incarnation,
// using the existing 2x owned-string growth convention. This is an additive
// reservation on the existing owner, NOT another use of shared plan framing.
const CONVERSION_STRING_BYTES: u64 = 2 * (2 * 256 + 2 * 32 + 6 * 64);

pub(in crate::migration) struct NativeApplyStorage {
    _slots: allocation::AllocationGuard,
}
impl NativeApplyStorage {
    pub(in crate::migration) fn require_owner(&self, owner: &RetainedNativeUpgrade) -> Result<()> {
        let context = owner.allocation.as_ref()
            .ok_or_else(|| Error("offline apply output allocation missing".into()))?;
        self._slots.require_same_source(context, &owner.db)
    }
}
pub(in crate::migration) struct BootstrapConversion {
    identity: OfflineBootstrapIdentity,
    database_id: String,
    incarnation: String,
    path_identity: (u64, u64),
    // Last field: reservation outlives every conversion string even on unwind.
    strings: allocation::AllocationGuard,
}
impl RetainedNativeUpgrade {
    pub(in crate::migration) fn admit_apply_storage(&self, bytes: u64) -> Result<NativeApplyStorage> {
        let allocation = self
            .allocation
            .as_ref()
            .ok_or_else(|| Error("offline apply allocation owner missing".into()))?;
        let bytes = bytes
            .checked_add(std::mem::size_of::<BootstrapConversion>() as u64)
            .and_then(|n| n.checked_add(std::mem::size_of::<OfflineBootstrapSession>() as u64))
            .and_then(|n| n.checked_add(std::mem::size_of::<Result<BootstrapConversion>>() as u64))
            .and_then(|n| {
                n.checked_add(std::mem::size_of::<
                    std::result::Result<OfflineBootstrapSession, RetainedNativeUpgrade>,
                >() as u64)
            })
            .ok_or_else(|| Error("offline apply conversion storage overflow".into()))?;
        Ok(NativeApplyStorage {
            _slots: allocation.reserve(&self.db, bytes)?,
        })
    }

    pub(in crate::migration) fn prepare_bootstrap_conversion(&mut self) -> Result<BootstrapConversion> {
        self.prepare_bootstrap_conversion_inner(0)
    }
    pub(in crate::migration) fn prepare_bootstrap_conversion_inner(
        &mut self,
        fault: u8,
    ) -> Result<BootstrapConversion> {
        self.prepare_bootstrap_conversion_with_control_workspace(fault, None)
    }
    pub(in crate::migration) fn prepare_bootstrap_conversion_with_control_workspace(
        &mut self,
        fault: u8,
        mut control: Option<&mut allocation::ControlWorkspaceLoan<'_>>,
    ) -> Result<BootstrapConversion> {
        // Failure or unwind retains even an unsettled inventory transaction on
        // this original owner. Only the outer terminal operation can call this.
        self.pending_inventory_with_control_workspace(fault, control.as_deref_mut())?;
        let allocation = self
            .allocation
            .as_ref()
            .ok_or_else(|| Error("offline allocation context missing".into()))?
            .clone();
        let _scope = allocation.enter()?;
        // Only the independently precharged conversion survives this lexical
        // phase. Decoder/metadata temporaries die before the shared validator
        // takes the SAME 32MiB workspace sequentially, never nested.
        let conversion = {
            let _scratch = allocation::MetadataReservation::new(&allocation, &self.db, control.as_deref_mut())?;
            #[cfg(test)]
            let _headroom = if fault == 5 {
                Some(self.fill_apply_headroom_for_test()?)
            } else {
                None
            };
            // Admit before read_in constructs any retained Transition strings, and
            // before cloning the Store's two identity strings. Decoder scratch stays
            // covered separately above. This guard moves into the successful session
            // and never disappears at the operation's finish boundary.
            let strings = allocation.reserve(&self.db, CONVERSION_STRING_BYTES)?;
            let transition = transition::read_in(&self.db)?
                .ok_or_else(|| Error("offline transition missing".into()))?;
            self.retained_preservation
                .as_ref()
                .ok_or_else(|| Error("offline source preservation missing".into()))?
                .validate_path(&self.path)?;
            self.verified_backup
                .as_ref()
                .ok_or_else(|| Error("offline backup custody missing".into()))?
                .revalidate()?;
            let metadata = fs::symlink_metadata(&self.path)?;
            // One Transition and the Store pair own ten strings: <=960 logical
            // bytes, prepaid as 1920 above. Their actual capacities must fit that
            // dedicated reservation; other plan framing users remain unchanged.
            // Struct headers are included in pre-admitted NativeApplyStorage slots.
            let conversion = BootstrapConversion {
                database_id: transition.catalog_id.clone(),
                incarnation: transition.source_incarnation.clone(),
                identity: OfflineBootstrapIdentity { transition, allocation: allocation.clone() },
                path_identity: (metadata.dev(), metadata.ino()),
                strings,
            };
            conversion.check_retained_strings()?;
            conversion
        };
        #[cfg(test)]
        if fault == 3 {
            panic!("controlled preconversion identity unwind");
        }
        #[cfg(test)]
        if fault == 4 {
            // Test only: prove the actual shared validator rejects unsafe
            // connection state while the original owner remains retained.
            self.db.pragma_update(None, "read_uncommitted", true)?;
        }
        conversion.identity.validate_retained_parts_with_control_workspace(
            &self.db,
            &self.path,
            Some(conversion.path_identity),
            Some(&self.admission),
            &conversion.database_id,
            Some(&conversion.incarnation),
            control.as_deref_mut(),
        )?;
        if !self.db.is_autocommit() {
            return Err(Error("offline conversion has an unsettled transaction".into()));
        }
        Ok(conversion)
    }

    #[cfg(test)]
    pub(in crate::migration) fn apply_custody_for_test(&self) -> (usize, Option<(usize, usize)>) {
        (
            // SAFETY: inspect only the retained live connection's identity;
            // never call through or dereference its native pointer.
            unsafe { self.db.handle() } as usize,
            self.retained_preservation.as_ref().map(|preservation| {
                let (catalog, incarnation) = preservation.identity();
                (catalog.as_ptr() as usize, incarnation.as_ptr() as usize)
            }),
        )
    }
    #[cfg(test)]
    pub(in crate::migration) fn apply_has_backup_for_test(&self) -> bool {
        self.verified_backup.is_some()
    }
    #[cfg(test)]
    pub(in crate::migration) fn fill_apply_headroom_for_test(
        &self,
    ) -> Result<allocation::AllocationGuard> {
        let context = self.allocation.as_ref().unwrap();
        let held = context.retained_native_bytes(&self.db)? + context.external_retained_bytes()?;
        context.reserve(&self.db, allocation::LIMIT - held)
    }
}
impl BootstrapConversion {
    fn check_retained_strings(&self) -> Result<()> {
        let row = &self.identity.transition;
        let bytes = [
            &row.catalog_id,
            &row.source_incarnation,
            &row.approved_plan_sha256,
            &row.source_full_ledger_sha256,
            &row.source_tables_sha256,
            &row.verified_backup_image_sha256,
            &row.migration41_sql_sha256,
            &row.closed41_reference_sha256,
            &self.database_id,
            &self.incarnation,
        ]
        .into_iter()
        .try_fold(0usize, |total, text| total.checked_add(text.capacity()))
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| Error("offline conversion identity capacity overflow".into()))?;
        if bytes > CONVERSION_STRING_BYTES {
            return Err(Error("offline conversion exceeds reserved identity storage".into()));
        }
        Ok(())
    }
    /// No queries, validation, polling, clones, allocations or panic branches.
    /// A missing private custody component fails closed with the EXACT original
    /// owner, not reconstructed or reopened custody. The public failure remains
    /// opaque. Successful construction transfers the original objects verbatim.
    pub(in crate::migration) fn finish(
        self,
        owner: RetainedNativeUpgrade,
    ) -> std::result::Result<OfflineBootstrapSession, RetainedNativeUpgrade> {
        let RetainedNativeUpgrade {
            db,
            writer,
            admission,
            path,
            verified_backup: Some(backup),
            retained_preservation: Some(preservation),
            allocation: _,
        } = owner else {
            return Err(owner);
        };
        let Self { identity, database_id, incarnation, path_identity, strings } = self;
        Ok(OfflineBootstrapSession {
            store: Store {
                offline_bootstrap: Some(identity),
                #[cfg(test)]
                candidate41: false,
                db,
                path,
                writable: true,
                schema_version: 41,
                database_id,
                catalog_incarnation: Some(incarnation),
                native_path_identity: Some(path_identity),
                native_admission: Some(admission),
                lock: Some(writer),
                transaction_poisoned: std::cell::Cell::new(false),
                catalog_operation_budget: std::cell::RefCell::new(None),
            },
            mode: BootstrapMode::Pending { backup },
            preservation,
            _conversion_strings: Some(strings),
        })
    }
}

#[cfg(test)]
impl OfflineBootstrapSession {
    pub(in crate::migration) fn apply_conversion_allocation_for_test(
        &self,
    ) -> (allocation::AllocationContext, u64) {
        assert!(self._conversion_strings.is_some());
        (
            self.store.offline_bootstrap.as_ref().unwrap().allocation.clone(),
            CONVERSION_STRING_BYTES,
        )
    }
}
