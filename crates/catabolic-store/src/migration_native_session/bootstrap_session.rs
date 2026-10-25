//! Private offline bootstrap only. Normal Store admission/dispatch remains40.
//! No mutable Store, connection, arbitrary callback or serialized grant escapes.
use super::*;
use crate::{
    Store,
    catalog_commands::{self, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority},
    catalog_nfo_attribution::bootstrap_entry::{self, Scope, SourceEvidence},
};
use catabolic_core::catalog_command::{CommandEnvelope, ReceiptVisibility};
use std::os::unix::fs::MetadataExt;
#[path = "bootstrap_session/host_evidence.rs"]
pub(super) mod host_evidence;
use host_evidence::HostSourceEvidence;
#[path = "bootstrap_session/apply_conversion.rs"]
mod apply_conversion;
pub(in crate::migration) use apply_conversion::{BootstrapConversion, NativeApplyStorage};
#[path = "bootstrap_session/control_workspace.rs"]
mod control_workspace;
pub(crate) use control_workspace::{BootstrapWorkspace, CurrentImageInventory};
pub use control_workspace::{OfflineOwnedTransition, OfflineRetainedResult, OfflineWorkspaceAdmissionRefused};

pub(crate) struct OfflineBootstrapIdentity {
    transition: transition::Transition,
    allocation: allocation::AllocationContext,
}
impl OfflineBootstrapIdentity {
    pub(crate) fn validate(&self, store: &Store) -> Result<()> {
        if store.schema_version != 41 || !store.writable || store.lock.is_none() {
            return Err(Error("offline bootstrap retained writer missing".into()));
        }
        self.validate_retained_parts(
            &store.db,
            &store.path,
            store.native_path_identity,
            store.native_admission.as_ref(),
            &store.database_id,
            store.catalog_incarnation.as_deref(),
        )
    }
    // Exactly the Store's offline identity validation, shared with borrowed
    // preconversion. The native owner statically owns its writer/lifecycle and
    // the fixed handoff sets writable/schema41 rather than accepting arguments.
    fn validate_retained_parts(
        &self,
        db: &Connection,
        path: &Path,
        path_identity: Option<(u64, u64)>,
        admission: Option<&crate::NativeAdmissionLock>,
        database_id: &str,
        catalog_incarnation: Option<&str>,
    ) -> Result<()> {
        self.validate_retained_parts_with_control_workspace(
            db, path, path_identity, admission, database_id, catalog_incarnation, None,
        )
    }
    fn validate_retained_parts_with_control_workspace(
        &self,
        db: &Connection,
        path: &Path,
        path_identity: Option<(u64, u64)>,
        admission: Option<&crate::NativeAdmissionLock>,
        database_id: &str,
        catalog_incarnation: Option<&str>,
        control: Option<&mut allocation::ControlWorkspaceLoan<'_>>,
    ) -> Result<()> {
        self.allocation.check(Some(db), 0)?;
        let _scope = self.allocation.enter()?;
        let _metadata = allocation::MetadataReservation::new(&self.allocation, db, control)?;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || path_identity != Some((metadata.dev(), metadata.ino()))
        {
            return Err(Error("offline bootstrap path replaced".into()));
        }
        admission
            .ok_or_else(|| Error("offline lifecycle custody missing".into()))?
            .validate_path()?;
        closed41::Closed41Target.validate_identity(db)?;
        let row =
            transition::read_in(db)?.ok_or_else(|| Error("offline transition missing".into()))?;
        if row != self.transition
            || database_id != row.catalog_id
            || catalog_incarnation != Some(row.source_incarnation.as_str())
        {
            return Err(Error("offline transition identity changed".into()));
        }
        let (incarnation, generation): (String, i64) = db.query_row(
            "SELECT incarnation,generation FROM catalog_state WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        // Only the actual original Control scratch selects the fixed streaming
        // producer. Ordinary callers preserve the exact materialized path.
        let streamed = if matches!(&_metadata, allocation::MetadataReservation::Control(_)) {
            Some(allocation::original_identity_digest(db, &_metadata)?)
        } else { None };
        let digest_matches = match streamed.as_ref() {
            Some(digest) => digest.as_str() == row.source_full_ledger_sha256,
            None => {
                let ledger = json!(allocation::identity_ledger_rows(db, allocation::IdentityLedger::Original)?);
                sha256(encode(&ledger)) == row.source_full_ledger_sha256
            }
        };
        if incarnation != row.source_incarnation
            || generation < row.migrated_generation
            || !digest_matches
        {
            return Err(Error(
                "offline retained ledger or current identity changed".into(),
            ));
        }
        self.allocation.check(Some(db), 0)?;
        Ok(())
    }
    pub(crate) fn allocation_operation(
        &self,
        store: &Store,
        budget: &crate::operation_budget::OperationBudget,
    ) -> Result<OfflineAllocationOperation> {
        let guard = self.allocation.operation(&store.db, budget)?;
        let scope = self.allocation.enter()?;
        Ok(OfflineAllocationOperation {
            _scope: scope,
            _guard: guard,
        })
    }
    pub(crate) fn allocation_before_commit(&self, store: &Store) -> Result<()> {
        store
            .catalog_operation_budget()?
            .check()
            .map_err(|cause| Error(format!("offline operation budget: {cause:?}")))?;
        self.allocation.check(Some(&store.db), 0)
    }
}

pub(crate) struct OfflineAllocationOperation {
    _scope: allocation::AllocationScope,
    _guard: allocation::AllocationGuard,
}

pub(crate) struct OfflineBootstrapSession {
    store: Store,
    // Store (and its database/locks) drops before retained recovery custody.
    mode: BootstrapMode,
    preservation: preservation::SourcePreservation,
    // Drop after the entire Store: its Transition and duplicate identity strings
    // must die before their dedicated allocation reservation is released.
    // None preserves the legacy from_owner path's existing accounting semantics.
    _conversion_strings: Option<allocation::AllocationGuard>,
}
enum BootstrapMode {
    Pending { backup: backup::VerifiedBackup },
    CompletedReceiptOnly { command: CommandEnvelope },
}
/// Recovery callers name the phase explicitly; a missing backup never grants
/// receipt-only admission or permission to execute new effects.
pub(in crate::migration) enum BootstrapRecovery<'a> {
    Pending { backup_path: &'a Path },
    CompletedReceiptOnly { command: &'a CommandEnvelope },
}
impl RetainedNativeUpgrade {
    pub(in crate::migration) fn into_bootstrap(self) -> Result<OfflineBootstrapSession> {
        let (mut owner, _) = self.pending_inventory()?;
        let backup = owner
            .verified_backup
            .take()
            .ok_or_else(|| Error("offline backup custody missing".into()))?;
        let preservation = owner
            .retained_preservation
            .take()
            .ok_or_else(|| Error("offline source preservation missing".into()))?;
        OfflineBootstrapSession::from_owner(owner, preservation, BootstrapMode::Pending { backup })
    }
    /// Reopen only through retained exclusive migration custody. This is not
    /// ordinary Store admission and never resumes effects from schema alone.
    pub(in crate::migration) fn recover_bootstrap(
        mut self,
        original_plan: &Value,
        recovery: BootstrapRecovery<'_>,
    ) -> Result<OfflineBootstrapSession> {
        let allocation = self.ensure_allocation()?;
        allocation.retain_plan(original_plan, &self.db)?;
        let _allocation_scope = allocation.enter()?;
        let copies = allocation::value_bytes(original_plan)?
            .checked_mul(6)
            .and_then(|n| n.checked_add(allocation::MIGRATION_SCRATCH))
            .ok_or_else(|| Error("offline recovery copies overflow".into()))?;
        let _allocation_scratch = allocation.reserve(&self.db, copies)?;
        self.admission.validate_path()?;
        require_native_journal(&self.db)?;
        if !self.db.is_autocommit() {
            return Err(Error(
                "offline recovery requires a fresh retained connection".into(),
            ));
        }
        // Ordinary connect intentionally does not configure unregistered41.
        // This private recovery owner establishes safe connection semantics
        // BEFORE its transaction; exact closed-resource/ledger/identity checks
        // below remain mandatory and do not admit ordinary Store41 access.
        self.db.pragma_update(None, "foreign_keys", true)?;
        self.db.pragma_update(None, "recursive_triggers", true)?;
        self.db.pragma_update(None, "read_uncommitted", false)?;
        self.db.execute_batch("BEGIN EXCLUSIVE")?;
        closed41::Closed41Target.validate(&self.db)?;
        let transition = transition::read_in(&self.db)?
            .ok_or_else(|| Error("offline recovery transition missing".into()))?;
        let preservation =
            preservation::SourcePreservation::recover_in(&self.db, original_plan, &transition)?;
        preservation.validate_path(&self.path)?;
        allocation.retain_preservation(preservation.allocated_bytes()?, &self.db)?;
        self.db.execute_batch("ROLLBACK")?;
        match recovery {
            BootstrapRecovery::Pending { backup_path } => {
                let backup = preservation.verify_backup(
                    &self.db,
                    &allocation,
                    backup_path,
                    &transition.verified_backup_image_sha256,
                )?;
                let mut owner = self;
                owner.verified_backup = Some(backup);
                owner.retained_preservation = Some(preservation);
                // Includes exact old-table preservation and positive worker
                // inventory/cleanup proof, not unsupported schema inspection.
                owner.into_bootstrap()
            }
            BootstrapRecovery::CompletedReceiptOnly { command } => {
                // The saved command lives with preservation, independently of
                // any later copied image. Admit both before cloning it.
                let retained = preservation
                    .allocated_bytes()?
                    .checked_add(completed_catalog::retained_command_bytes(command)?)
                    .ok_or_else(|| Error("recovered command retention overflow".into()))?;
                allocation.retain_preservation(retained, &self.db)?;
                let session = OfflineBootstrapSession::from_owner(
                    self,
                    preservation,
                    BootstrapMode::CompletedReceiptOnly {
                        command: command.clone(),
                    },
                )?;
                session.complete(command)?;
                closed41::Closed41Target.validate(&session.store.db)?;
                session.store.validate_native_identity()?;
                Ok(session)
            }
        }
    }
}
impl OfflineBootstrapSession {
    // Persistent source preservation and saved/completed command buffers share
    // one lifetime. Copied image/history storage has a separate additive slot.
    fn retain_completed_allocation(&self, additional: u64) -> Result<()> {
        let bytes = self
            .preservation
            .allocated_bytes()?
            .checked_add(additional)
            .ok_or_else(|| Error("completed preservation retention overflow".into()))?;
        self.store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("offline allocation missing".into()))?
            .allocation
            .retain_preservation(bytes, &self.store.db)
    }
    fn retain_image_allocation(&self, bytes: u64) -> Result<()> {
        self.store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("offline allocation missing".into()))?
            .allocation
            .retain_image(&self.store.db, bytes)
    }
    pub(in crate::migration) fn refresh_external_reservation(
        &self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        self.store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("offline allocation missing".into()))?
            .allocation
            .refresh(reservation, Some(&self.store.db))
    }
    fn from_owner(
        owner: RetainedNativeUpgrade,
        preservation: preservation::SourcePreservation,
        mode: BootstrapMode,
    ) -> Result<Self> {
        let transition = transition::read_in(&owner.db)?
            .ok_or_else(|| Error("offline transition missing".into()))?;
        preservation.validate_path(&owner.path)?;
        let metadata = fs::symlink_metadata(&owner.path)?;
        let RetainedNativeUpgrade {
            db,
            writer,
            admission,
            path,
            allocation,
            ..
        } = owner;
        let store = Store {
            offline_bootstrap: Some(OfflineBootstrapIdentity {
                transition: transition.clone(),
                allocation: allocation
                    .ok_or_else(|| Error("offline allocation context missing".into()))?,
            }),
            #[cfg(test)]
            candidate41: false,
            db,
            path,
            writable: true,
            schema_version: 41,
            database_id: transition.catalog_id,
            catalog_incarnation: Some(transition.source_incarnation),
            native_path_identity: Some((metadata.dev(), metadata.ino())),
            native_admission: Some(admission),
            lock: Some(writer),
            transaction_poisoned: std::cell::Cell::new(false),
            catalog_operation_budget: std::cell::RefCell::new(None),
        };
        store.validate_native_identity()?;
        Ok(Self {
            store,
            mode,
            preservation,
            _conversion_strings: None,
        })
    }
    fn backup(&self) -> Result<&backup::VerifiedBackup> {
        match &self.mode {
            BootstrapMode::Pending { backup } => Ok(backup),
            BootstrapMode::CompletedReceiptOnly { .. } => {
                Err(Error("completed offline recovery is receipt-only".into()))
            }
        }
    }
    pub(crate) fn prepare(&self, host: &str, evidence: &[SourceEvidence<'_>]) -> Result<Value> {
        self.backup()?.revalidate()?;
        self.store.transaction_detailed::<_, Error>(|db| {
            let generation: i64 =
                db.query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                    r.get(0)
                })?;
            let plan = bootstrap_entry::prepare_in(
                &self.store,
                Scope {
                    catalog_id: &self.store.database_id,
                    incarnation: self
                        .store
                        .catalog_incarnation
                        .as_deref()
                        .ok_or_else(|| Error("offline incarnation missing".into()))?,
                    host_namespace: host,
                    expected_generation: generation,
                },
                evidence,
                historical_authority(host),
            )?;
            Ok(plan.payload())
        })
    }
    pub(in crate::migration) fn prepare_host(
        &self,
        host: &str,
        descriptions: &[HostSourceEvidence<'_>],
    ) -> Result<Value> {
        self.prepare_host_with_control_workspace(host, descriptions, None)
    }
    pub(in crate::migration) fn prepare_host_with_control_workspace(
        &self, host: &str, descriptions: &[HostSourceEvidence<'_>],
        control: Option<&BootstrapWorkspace<'_>>,
    ) -> Result<Value> {
        if control.is_some() && !descriptions.is_empty() {
            return Err(Error("control workspace requires empty bootstrap evidence".into()));
        }
        self.backup()?.revalidate()?;
        self.store.transaction_detailed_with_control_workspace::<_, Error>(|db| {
            let generation: i64 =
                db.query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                    r.get(0)
                })?;
            host_evidence::with_evidence_in_with_control_workspace(&self.store, host, descriptions, |evidence| {
                let plan = bootstrap_entry::prepare_in_with_control_workspace(
                    &self.store,
                    Scope {
                        catalog_id: &self.store.database_id,
                        incarnation: self
                            .store
                            .catalog_incarnation
                            .as_deref()
                            .ok_or_else(|| Error("offline incarnation missing".into()))?,
                        host_namespace: host,
                        expected_generation: generation,
                    },
                    evidence,
                    historical_authority(host),
                    control,
                )?;
                Ok(plan.payload())
            }, control)
        }, control)
    }
    pub(in crate::migration) fn execute_host<'a>(
        &'a self,
        command: &'a CommandEnvelope,
        host: &'a str,
        descriptions: &'a [HostSourceEvidence<'a>],
        policy: ReceiptPolicy,
    ) -> catalog_commands::Result<CommandOutcome> {
        let execution = OfflineBootstrapExecution {
            session: self,
            command,
            host,
            evidence: Some(ExecutionEvidence::Host(descriptions)),
            control: None,
        };
        catalog_commands::execute_offline_bootstrap(&self.store, command, &execution, policy)
    }
    pub(in crate::migration) fn execute_empty_with_control_workspace(
        &self, command: &CommandEnvelope, host: &str, policy: ReceiptPolicy,
        control: &BootstrapWorkspace<'_>,
    ) -> catalog_commands::Result<CommandOutcome> {
        let execution = OfflineBootstrapExecution {
            session: self, command, host, evidence: Some(ExecutionEvidence::Host(&[])), control: Some(control),
        };
        catalog_commands::execute_offline_bootstrap(&self.store, command, &execution, policy)
    }
    /// Receipt-first lookup never requires source evidence or current files.
    pub(crate) fn reconcile(
        &self,
        command: &CommandEnvelope,
        host: &str,
    ) -> catalog_commands::Result<Option<CommandOutcome>> {
        self.reconcile_with_control_workspace(command, host, None)
    }
    pub(in crate::migration) fn reconcile_with_control_workspace(
        &self, command: &CommandEnvelope, host: &str,
        control: Option<&BootstrapWorkspace<'_>>,
    ) -> catalog_commands::Result<Option<CommandOutcome>> {
        let execution = OfflineBootstrapExecution {
            session: self,
            command,
            host,
            evidence: None,
            control,
        };
        catalog_commands::reconcile_offline_bootstrap_receipt(&self.store, command, &execution)
    }
    pub(crate) fn execute<'a>(
        &'a self,
        command: &'a CommandEnvelope,
        host: &'a str,
        evidence: &'a [SourceEvidence<'a>],
        policy: ReceiptPolicy,
    ) -> catalog_commands::Result<CommandOutcome> {
        let execution = OfflineBootstrapExecution {
            session: self,
            command,
            host,
            evidence: Some(ExecutionEvidence::Private(evidence)),
            control: None,
        };
        catalog_commands::execute_offline_bootstrap(&self.store, command, &execution, policy)
    }
}
/// Owner-issued completion tied to retained native custody. Descriptive getters
/// do not authorize normal41 admission, restore, or a Motion ACK by themselves.
pub(crate) struct OfflineBootstrapComplete<'a> {
    session: &'a OfflineBootstrapSession,
    transition_digest: String,
    command_key: catabolic_core::catalog_command::CommandKey,
    payload_digest: String,
    inventory_digest: String,
    committed_generation: i64,
    state_digest: String,
    source_count: i64,
}
impl OfflineBootstrapComplete<'_> {
    pub(crate) fn image_allocation(&self) -> Result<OfflineImageAllocation<'_>> {
        self.session.image_allocation()
    }
    /// Fixed completed-image consumer only. A borrowed completion cannot be
    /// attached to another Store or used after its retained source fence moves.
    pub(crate) fn validate_source(&self, store: &Store) -> Result<()> {
        if !std::ptr::eq(store, &self.session.store) {
            return Err(Error(
                "offline completed image names another source owner".into(),
            ));
        }
        store.validate_native_identity()?;
        self.validate_closed_copy(&store.db)
    }
    /// Closed-image structure/identity check, not Store admission or bootstrap
    /// authority. The fixed image consumer additionally proves COMPLETE typed
    /// source/copy/serialized inventory equality while source custody is held.
    pub(crate) fn validate_closed_copy(&self, db: &Connection) -> Result<()> {
        closed41::Closed41Target.validate(db)?;
        let identity: (String, String, i64) = db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let retained = transition::read_in(db)?
            .ok_or_else(|| Error("completed image lacks immutable transition".into()))?;
        if identity.0 != self.catalog_id()
            || identity.1 != self.incarnation()
            || identity.2 != self.committed_generation
            || retained.digest()? != self.transition_digest
        {
            return Err(Error(
                "completed image identity/generation/transition differs".into(),
            ));
        }
        Ok(())
    }
    pub(crate) fn catalog_id(&self) -> &str {
        &self.session.store.database_id
    }
    pub(crate) fn incarnation(&self) -> &str {
        self.session
            .store
            .catalog_incarnation
            .as_deref()
            .expect("validated offline incarnation")
    }
    pub(crate) fn transition_digest(&self) -> &str {
        &self.transition_digest
    }
    pub(crate) fn command_key(&self) -> &catabolic_core::catalog_command::CommandKey {
        &self.command_key
    }
    pub(crate) fn payload_digest(&self) -> &str {
        &self.payload_digest
    }
    pub(crate) fn inventory_digest(&self) -> &str {
        &self.inventory_digest
    }
    pub(crate) fn committed_generation(&self) -> i64 {
        self.committed_generation
    }
    pub(crate) fn state_digest(&self) -> &str {
        &self.state_digest
    }
    pub(crate) fn source_count(&self) -> i64 {
        self.source_count
    }
}
impl OfflineBootstrapSession {
    pub(crate) fn identity(&self) -> (&str, &str) {
        (
            &self.store.database_id,
            self.store
                .catalog_incarnation
                .as_deref()
                .expect("validated offline incarnation"),
        )
    }
    pub(crate) fn generation(&self) -> Result<i64> {
        self.generation_with_control_workspace(None)
    }
    pub(in crate::migration) fn generation_with_control_workspace(&self, control: Option<&BootstrapWorkspace<'_>>) -> Result<i64> {
        self.store.validate_native_identity_with_control_workspace(control)?;
        Ok(self
            .store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })?)
    }
    pub(crate) fn complete<'a>(
        &'a self,
        command: &CommandEnvelope,
    ) -> Result<OfflineBootstrapComplete<'a>> {
        self.complete_with_control_workspace(command, None)
    }
    fn complete_with_control_workspace<'a>(
        &'a self, command: &CommandEnvelope, control: Option<&BootstrapWorkspace<'_>>,
    ) -> Result<OfflineBootstrapComplete<'a>> {
        let host = command.key().host_namespace();
        let execution = OfflineBootstrapExecution {
            session: self,
            command,
            host,
            evidence: None,
            control,
        };
        execution.check(&self.store, command)?;
        let completion = self.store.transaction_detailed_with_control_workspace::<_, Error>(|_| {
            let completion = crate::catalog_nfo_attribution::verify_bootstrap_completion_in_with_control_workspace(
                &self.store,
                command,
                control,
            )?;
            self.preservation
                .verify_bootstrap_delta(&self.store.db, command, &completion)?;
            Ok(completion)
        }, control)?;
        let transition = &self
            .store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("offline identity missing".into()))?
            .transition;
        Ok(OfflineBootstrapComplete {
            session: self,
            transition_digest: match control { Some(control) => control.transition_digest()?, None => transition.digest()? },
            command_key: command.key().clone(),
            payload_digest: command.digest().as_str().into(),
            inventory_digest: completion.inventory_digest().into(),
            committed_generation: completion.committed_generation(),
            state_digest: completion.state_digest().into(),
            source_count: completion.source_count(),
        })
    }
}

fn historical_authority(host: &str) -> TrustedCommandAuthority<'_> {
    TrustedCommandAuthority {
        host_namespace: host,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: false,
        identity_authority: None,
    }
}
/// The only constructor is the retained session above. This is not a generic
/// operation/callback permit and cannot be decoded from a caller's description.
#[derive(Clone, Copy)]
enum ExecutionEvidence<'a> {
    Private(&'a [SourceEvidence<'a>]),
    Host(&'a [HostSourceEvidence<'a>]),
}
pub(crate) struct OfflineBootstrapExecution<'a, 'workspace> {
    session: &'a OfflineBootstrapSession,
    command: &'a CommandEnvelope,
    host: &'a str,
    evidence: Option<ExecutionEvidence<'a>>,
    control: Option<&'a BootstrapWorkspace<'workspace>>,
}
impl<'workspace> OfflineBootstrapExecution<'_, 'workspace> {
    pub(crate) fn control_workspace(&self) -> Option<&BootstrapWorkspace<'workspace>> { self.control }
    pub(crate) fn authority(&self) -> TrustedCommandAuthority<'_> {
        TrustedCommandAuthority {
            allow_new_commands: self.evidence.is_some()
                && matches!(&self.session.mode, BootstrapMode::Pending { .. }),
            ..historical_authority(self.host)
        }
    }
    pub(crate) fn check(&self, store: &Store, command: &CommandEnvelope) -> Result<()> {
        if let BootstrapMode::CompletedReceiptOnly { command: saved } = &self.session.mode {
            if command != saved {
                return Err(Error(
                    "completed offline recovery requires exact saved command".into(),
                ));
            }
        }

        if !std::ptr::eq(store, &self.session.store)
            || !store.db.is_autocommit()
            || command != self.command
            || command.key().host_namespace() != self.host
            || command.key().catalog_id() != store.database_id
        {
            return Err(Error(
                "offline bootstrap exact owner/envelope mismatch".into(),
            ));
        }
        store.validate_native_identity_with_control_workspace(self.control)?;
        store.transaction_available_with_control_workspace(self.control)?;
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            operation: String,
            contract_version: u32,
            inventory_digest: String,
        }
        let request: Request = <Request as serde::Deserialize>::deserialize(command.payload())
            .map_err(|_| Error("offline bootstrap payload invalid".into()))?;
        if request.operation != "nfo_bootstrap"
            || request.contract_version != 1
            || request.inventory_digest.len() != 64
            || !request
                .inventory_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error(
                "offline bootstrap operation or commitment invalid".into(),
            ));
        }
        Ok(())
    }
    pub(crate) fn apply_in(&self, store: &Store, command: &CommandEnvelope) -> Result<Value> {
        if !std::ptr::eq(store, &self.session.store)
            || command != self.command
            || store.db.is_autocommit()
        {
            return Err(Error(
                "offline bootstrap retained effect scope differs".into(),
            ));
        }
        self.session.backup()?.revalidate()?;
        let evidence = self
            .evidence
            .ok_or_else(|| Error("offline bootstrap evidence required for new effect".into()))?;
        let result = match evidence {
            ExecutionEvidence::Private(evidence) => bootstrap_entry::apply_in_with_control_workspace(
                store,
                command,
                evidence,
                historical_authority(self.host),
                self.control,
            )?,
            ExecutionEvidence::Host(descriptions) => {
                host_evidence::with_evidence_in_with_control_workspace(store, self.host, descriptions, |evidence| {
                    bootstrap_entry::apply_in_with_control_workspace(
                        store,
                        command,
                        evidence,
                        historical_authority(self.host),
                        self.control,
                    )
                }, self.control)?
            }
        };
        self.session.backup()?.revalidate()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use catabolic_core::catalog_command::CommandKey;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };
    pub(super) const HOST: &str = "offline-bootstrap-fixture";
    pub(super) fn fixture() -> (tempfile::TempDir, OfflineBootstrapSession) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        initialize_native_at(&path, 40).unwrap();
        let owner = RetainedNativeUpgrade::acquire(
            &path,
            NativeUpgradeQuiescence {
                admission_stopped: true,
                readers_and_workers_stopped: true,
                exclusive_operational_ownership: true,
            },
        )
        .unwrap();
        let (owner, plan) = owner.plan_closed41().unwrap();
        let (owner, _) = owner
            .apply_closed41(&plan, None, &|_, _, _| Ok(()))
            .unwrap();
        (root, owner.into_bootstrap().unwrap())
    }
    pub(super) fn fixture_with_registered_library() -> (tempfile::TempDir, OfflineBootstrapSession)
    {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        initialize_native_at(&path, 40).unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        crate::catalog_import_typed::register_host_source(
            &store,
            "default",
            "native-source",
            &source,
            &[],
        )
        .unwrap();
        crate::curation::item_put(&store,&json!({"id":"logical-library","kind":"custom:motion_library","metadata":{"title":"Library","motion_library":{"source_ids":["motion-source","motion-source"]}}})).unwrap();
        drop(store);

        let owner = RetainedNativeUpgrade::acquire(
            &path,
            NativeUpgradeQuiescence {
                admission_stopped: true,
                readers_and_workers_stopped: true,
                exclusive_operational_ownership: true,
            },
        )
        .unwrap();
        let (owner, plan) = owner.plan_closed41().unwrap();
        let (owner, _) = owner
            .apply_closed41(&plan, None, &|_, _, _| Ok(()))
            .unwrap();
        (root, owner.into_bootstrap().unwrap())
    }
    pub(super) fn policy() -> ReceiptPolicy {
        ReceiptPolicy {
            retention: Duration::from_secs(3600),
            max_receipts: 100,
            max_hold_records: 100,
            max_logical_bytes: 16 * 1024 * 1024,
        }
    }
    pub(super) fn command(session: &OfflineBootstrapSession, payload: Value) -> CommandEnvelope {
        let (catalog, incarnation) = session.identity();
        CommandEnvelope::new(
            1,
            CommandKey::new(catalog, HOST, "bootstrap").unwrap(),
            incarnation,
            session.generation().unwrap(),
            &payload.to_string(),
        )
        .unwrap()
    }
    #[test]
    fn actual_offline_bootstrap_commits_complete_and_replays_without_backup_availability() {
        let (_root, session) = fixture();
        let payload = session.prepare(HOST, &[]).unwrap();
        let command = command(&session, payload);
        assert!(session.reconcile(&command, HOST).unwrap().is_none());
        assert!(matches!(
            session.execute(&command, HOST, &[], policy()).unwrap(),
            CommandOutcome::Committed { .. }
        ));
        let complete = session.complete(&command).unwrap();
        assert_eq!(complete.source_count(), 0);
        assert_eq!(
            complete.inventory_digest(),
            command.payload()["inventory_digest"].as_str().unwrap()
        );
        assert_eq!(
            complete.committed_generation(),
            command.expected_generation() + 1
        );
        assert_eq!(complete.command_key(), command.key());
        let state_digest = complete.state_digest().to_owned();
        drop(complete);
        let backup_path = session.backup().unwrap().path_for_test().to_owned();
        fs::rename(&backup_path, backup_path.with_extension("held-but-renamed")).unwrap();
        assert!(session.prepare(HOST, &[]).is_err());
        assert!(matches!(
            session.reconcile(&command, HOST).unwrap(),
            Some(CommandOutcome::Replay { .. })
        ));
        assert!(matches!(
            session.execute(&command, HOST, &[], policy()).unwrap(),
            CommandOutcome::Replay { .. }
        ));
        assert_eq!(
            session.complete(&command).unwrap().state_digest(),
            state_digest
        );
        let path = session.store.path.clone();
        drop(session);
        assert!(
            Store::open(&path, true, false).is_err(),
            "Complete is not normal41 registration"
        );
    }
    #[test]
    fn wrong_bootstrap_commitment_rolls_back_every_table() {
        let (_root, session) = fixture();
        let mut payload = session.prepare(HOST, &[]).unwrap();
        payload["inventory_digest"] = json!("0".repeat(64));
        let command = command(&session, payload);
        let before = snapshot(&session.store.db).unwrap();
        assert!(session.execute(&command, HOST, &[], policy()).is_err());
        assert_eq!(snapshot(&session.store.db).unwrap(), before);
        assert!(session.complete(&command).is_err());
    }
    #[test]
    fn actual_post_effect_receipt_denial_rolls_back_bootstrap_and_retries() {
        let (_root, session) = fixture();
        let payload = session.prepare(HOST, &[]).unwrap();
        let command = command(&session, payload);
        let before = snapshot(&session.store.db).unwrap();
        let observed = Arc::new(AtomicBool::new(false));
        let flag = observed.clone();
        let guard =
            crate::catalog_source_succession::authorizer::Guard::for_store(&session.store).unwrap();
        guard
            .replace(move |context: rusqlite::hooks::AuthContext<'_>| {
                if context.database_name == Some("main")
                    && matches!(
                        context.action,
                        rusqlite::hooks::AuthAction::Insert {
                            table_name: "catalog_command_receipts"
                        }
                    )
                {
                    flag.store(true, Ordering::SeqCst);
                    rusqlite::hooks::Authorization::Deny
                } else {
                    rusqlite::hooks::Authorization::Allow
                }
            })
            .unwrap();
        assert!(session.execute(&command, HOST, &[], policy()).is_err());
        guard.restore().unwrap();
        assert!(observed.load(Ordering::SeqCst));
        assert_eq!(snapshot(&session.store.db).unwrap(), before);
        assert!(session.complete(&command).is_err());
        assert!(matches!(
            session.execute(&command, HOST, &[], policy()).unwrap(),
            CommandOutcome::Committed { .. }
        ));
        session.complete(&command).unwrap();
    }
    #[test]
    fn external_actual_allocation_failure_cannot_commit_swallowed_error() {
        use crate::operation_budget::{BudgetCharge, BudgetLimits, OperationBudget};
        let (_root, session) = fixture();
        // Use a mutable non-identity row: database_id is protected by schema33.
        assert_eq!(
            session
                .store
                .db
                .execute(
                    "INSERT INTO meta(key,value) VALUES('allocation_rollback_probe','before')",
                    []
                )
                .unwrap(),
            1
        );
        session
            .refresh_external_reservation(
                OfflineAllocationReservation::new(180 * 1024 * 1024).unwrap(),
            )
            .unwrap();
        let before: String = session
            .store
            .db
            .query_row(
                "SELECT value FROM meta WHERE key='allocation_rollback_probe'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let budget = OperationBudget::new(
            Duration::from_secs(60),
            BudgetLimits {
                items: 1_000_000,
                values: 1_000_000,
                bytes: 64 * 1024 * 1024,
                work: 1_000_000,
            },
        )
        .unwrap();
        let _scope = budget.enter().unwrap();
        let entered = std::cell::Cell::new(false);
        let wrote = std::cell::Cell::new(false);
        let result = session.store.transaction_detailed::<_, Error>(|db| {
            entered.set(true);
            assert_eq!(
                db.execute(
                    "UPDATE meta SET value='must-rollback' WHERE key='allocation_rollback_probe'",
                    []
                )?,
                1
            );
            wrote.set(true);
            let failure = budget
                .charge(BudgetCharge {
                    values: 100_000,
                    ..Default::default()
                })
                .unwrap_err();
            assert_eq!(format!("{failure:?}"), "ByteBudget");
            Ok(()) // deliberately swallow the aggregate admission refusal
        });
        assert!(
            wrote.get(),
            "genuine fixture write did not complete: entered={}, result={result:?}, budget_failure={:?}",
            entered.get(),
            budget.failure()
        );
        assert!(result.is_err());
        assert!(session.store.db.is_autocommit());
        assert_eq!(
            before,
            session
                .store
                .db
                .query_row(
                    "SELECT value FROM meta WHERE key='allocation_rollback_probe'",
                    [],
                    |r| { r.get::<_, String>(0) }
                )
                .unwrap()
        );
    }
}

#[cfg(test)]
#[path = "bootstrap_session/nonempty_tests.rs"]
mod nonempty_tests;

#[cfg(test)]
#[path = "bootstrap_session/expiry_tests.rs"]
mod expiry_tests;

#[cfg(test)]
#[path = "bootstrap_session/recovery_tests.rs"]
mod recovery_tests;

#[cfg(test)]
#[path = "bootstrap_session/imported_tests.rs"]
mod imported_tests;

/// Fixed copied-image owner; source connection and both locks remain retained.
/// No image connection or generic source mutation interface is exposed.
pub(crate) struct OfflineImageAllocation<'a> {
    db: &'a Connection,
    guard: super::allocation::AllocationGuard,
    retained: u64,
    peak: u64,
}
impl OfflineImageAllocation<'_> {
    #[cfg(test)]
    pub(crate) fn record_capture_phase_for_test(&self, name: &'static str,
        image_bytes: u64, copy_sqlite: u64, metadata: u64, requested: u64,
    ) -> Result<()> {
        self.guard.record_capture_phase_for_test(name, image_bytes, copy_sqlite, metadata, requested)
    }
    pub(crate) fn admit(&mut self, phase_total: u64) -> Result<()> {
        // Full image phase totals already include the held image. Its current
        // measured reservation remains live while only the excess grows here.
        let target = phase_total.saturating_sub(self.retained);
        self.guard.grow(self.db, target.saturating_sub(self.peak))?;
        self.peak = self.peak.max(target);
        Ok(())
    }
}
/// Same image reservation without a self-referential source borrow. Only the
/// exact retained SQLite handle can grow it; counters/context are never replaced.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OwnedImagePhase { Construction, OutputSealed, Committed }
pub(crate) struct OwnedImageAllocation {
    phase: OwnedImagePhase,
    handle: usize,
    guard: super::allocation::AllocationGuard,
    retained: u64,
    peak: u64,
}
impl OfflineImageAllocation<'_> {
    pub(crate) fn absorb_current_inventory(&mut self, inventory: &mut control_workspace::CurrentImageInventory) -> Result<()> {
        if let Some(guard) = inventory.retention.as_mut() {
            self.guard.merge_image_payload(guard, self.db)?;
        } else {
            return Err(Error("current image inventory has no retained coverage".into()));
        }
        Ok(())
    }
    pub(crate) fn into_owned(self) -> OwnedImageAllocation {
        OwnedImageAllocation {
            phase: OwnedImagePhase::Construction,
            // SAFETY: inspect only this borrowed live connection's identity;
            // no native pointer dereference or ownership transfer is performed.
            handle: unsafe { self.db.handle() } as usize,
            guard: self.guard,
            retained: self.retained,
            peak: self.peak,
        }
    }
}
impl OwnedImageAllocation {
    // Only the retained CurrentImageProof/ValidatedCompletedImage handoff uses
    // this scalar: it is measured from that exact private immutable payload.
    // The full measured image stays additive to any prior persistent category.
    fn seal_current_capture(&mut self, source: &Connection, bytes: u64) -> Result<()> {
        // SAFETY: compare only the identity of this borrowed live connection;
        // never dereference its handle or transfer ownership through it.
        if self.phase != OwnedImagePhase::Construction
            || self.handle != unsafe { source.handle() } as usize
        {
            return Err(Error("owned image allocation connection or phase differs".into()));
        }
        self.guard.seal_image_payload(source, bytes)?;
        self.phase = OwnedImagePhase::OutputSealed;
        Ok(())
    }
    pub(crate) fn commit_current_capture(&mut self, source: &Connection, history: &mut allocation::AllocationGuard, bytes: u64) -> Result<()> {
        if self.phase != OwnedImagePhase::OutputSealed || self.handle != unsafe { source.handle() } as usize {
            return Err(Error("owned image allocation connection differs".into()));
        }
        self.guard.commit_image_payload(history, source, bytes)?;
        self.phase = OwnedImagePhase::Committed;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn record_capture_phase_for_test(&self, name: &'static str) -> Result<()> {
        self.guard.record_capture_phase_for_test(name, 0, 0, 0, 0)
    }
    pub(crate) fn admit(&mut self, db: &Connection, phase_total: u64) -> Result<()> {
        // SAFETY: compare only the borrowed live connection's identity;
        // no call through or dereference of its native pointer is performed.
        if self.phase != OwnedImagePhase::Construction || self.handle != unsafe { db.handle() } as usize {
            return Err(Error("owned image allocation connection differs".into()));
        }
        let target = phase_total.saturating_sub(self.retained);
        self.guard.grow(db, target.saturating_sub(self.peak))?;
        self.peak = self.peak.max(target);
        Ok(())
    }
}
pub struct CompletedOfflineImage {
    image: crate::catalog_restore_lineage::completed_image::ValidatedCompletedImage,
    session: OfflineBootstrapSession,
    command: CommandEnvelope,
}
impl OfflineBootstrapSession {
    pub(crate) fn into_completed_image(
        self,
        command: &CommandEnvelope,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<CompletedOfflineImage> {
        let complete = self.complete(command)?;
        let mut image = crate::catalog_restore_lineage::completed_image::capture(
            &self.store,
            &complete,
            cancel,
        )?;
        drop(complete);
        self.retain_image_allocation(image.retained_allocations()?)?;
        image.release_capture_retention();
        Ok(CompletedOfflineImage {
            session: self,
            image,
            command: command.clone(),
        })
    }
    pub(in crate::migration) fn transition_description(&self) -> Result<OfflineTransition<'_>> {
        self.store.validate_native_identity()?;
        let row = &self
            .store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("offline identity missing".into()))?
            .transition;
        Ok(OfflineTransition {
            row,
            digest: row.digest()?,
        })
    }
}
/// Immutable, borrow-scoped transition description. This is not a write grant.
pub struct OfflineTransition<'a> {
    row: &'a transition::Transition,
    digest: String,
}
impl OfflineTransition<'_> {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn catalog_id(&self) -> &str {
        &self.row.catalog_id
    }
    pub fn incarnation(&self) -> &str {
        &self.row.source_incarnation
    }
    pub fn source_generation(&self) -> i64 {
        self.row.source_generation
    }
    pub fn migrated_generation(&self) -> i64 {
        self.row.migrated_generation
    }
    pub fn approved_plan_sha256(&self) -> &str {
        &self.row.approved_plan_sha256
    }
    pub fn source_full_ledger_sha256(&self) -> &str {
        &self.row.source_full_ledger_sha256
    }
    pub fn source_tables_sha256(&self) -> &str {
        &self.row.source_tables_sha256
    }
    pub fn verified_backup_image_sha256(&self) -> &str {
        &self.row.verified_backup_image_sha256
    }
    pub fn migration41_sql_sha256(&self) -> &str {
        &self.row.migration41_sql_sha256
    }
    pub fn closed41_reference_sha256(&self) -> &str {
        &self.row.closed41_reference_sha256
    }
}
#[cfg(test)]
#[path = "bootstrap_session/completed_image_tests.rs"]
mod completed_image_tests;

#[path = "bootstrap_session/completed_catalog.rs"]
mod completed_catalog;
pub(in crate::migration) use completed_catalog::CatalogCompletionStorage;
pub use completed_catalog::{
    CompletedCatalogSession, CompletedFileEndpoint, CompletedLibraryScope,
    CompletedSourceWithdrawal, CurrentCatalogImage, CurrentImageCaptureFailure,
    CurrentImageCaptureOperation, CurrentImageCapturePreparationFailure, CurrentImageCaptureWorkspace,
    CurrentRestoreHostAttestation, CurrentRotationFailure, CurrentRotationOperation,
    CurrentRotationPlanFacts, CurrentRotationPlanningOperation, CurrentRotationPreparationFailure,
    RotatedCurrentCatalogImage,
};
pub(crate) use completed_catalog::{
    CompletedCurrentImageAdmission, CompletedSourceAdmission, CompletedSourcePreparationAdmission,
    CompletedSourceWithdrawalAdmission, CurrentImageProof, CurrentImageRotationAdmission,
};

impl OfflineBootstrapSession {
    pub(in crate::migration) fn verified_backup_path(&self) -> Result<&Path> {
        match &self.mode {
            BootstrapMode::Pending { backup } => {
                backup.revalidate()?;
                Ok(backup.path())
            }
            BootstrapMode::CompletedReceiptOnly { .. } => {
                Err(Error("receipt-only recovery retains no backup".into()))
            }
        }
    }
    #[cfg(test)]
    pub(in crate::migration) fn compact_receipt_for_test(
        &self,
        command: &CommandEnvelope,
        host: &str,
    ) -> catalog_commands::Result<usize> {
        let execution = OfflineBootstrapExecution {
            session: self,
            command,
            host,
            evidence: None,
            control: None,
        };
        catalog_commands::compact_offline_bootstrap_receipt_for_test(
            &self.store,
            command,
            &execution,
        )
    }
}

impl CompletedOfflineImage {
    pub fn refresh_external_reservation(
        &mut self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        self.session
            .retain_image_allocation(self.image.retained_allocations()?)?;
        self.session.refresh_external_reservation(reservation)
    }
    pub fn description(
        &self,
    ) -> crate::catalog_restore_lineage::completed_image::CompletedImageDescription {
        self.image.description()
    }
    /// Pair and Motion binding hashes are explicit trusted-host attestations;
    /// native identity, transition and completed inventory are checked here.
    pub fn rotate(
        self,
        attestation: &crate::catalog_restore_lineage::completed_image::RestoreHostAttestation,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<RotatedOfflineImage> {
        let Self {
            image,
            session,
            command,
        } = self;
        let complete = session.complete(&command)?;
        let image = image.rotate(&session.store, &complete, attestation, cancel)?;
        drop(complete);
        session.retain_image_allocation(image.retained_allocations()?)?;
        Ok(RotatedOfflineImage {
            image,
            session,
            command,
        })
    }
}
/// Verified rotated bytes, never an install/path grant. The separate image
/// drops before original source custody; original native catalog stays intact.
pub struct RotatedOfflineImage {
    image: crate::catalog_restore_lineage::completed_image::RotatedCompletedImage,
    session: OfflineBootstrapSession,
    command: CommandEnvelope,
}
impl RotatedOfflineImage {
    pub fn refresh_external_reservation(
        &mut self,
        reservation: OfflineAllocationReservation,
    ) -> Result<()> {
        self.session
            .retain_image_allocation(self.image.retained_allocations()?)?;
        self.session.refresh_external_reservation(reservation)
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
    pub fn inventory_sha256(&self) -> &str {
        self.image.inventory_sha256()
    }
    /// May leave partial output on cancellation/I/O failure. No publication or
    /// replacement is authorized by this byte-copy operation.
    pub fn copy_verified_to(
        &self,
        output: &mut impl std::io::Write,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<()> {
        let complete = self.session.complete(&self.command)?;
        complete.validate_source(&self.session.store)?;
        self.image.copy_verified_to(output, cancel)?;
        complete.validate_source(&self.session.store)?;
        Ok(())
    }
}

impl OfflineBootstrapSession {
    pub(crate) fn image_allocation(&self) -> Result<OfflineImageAllocation<'_>> {
        let db = &self.store.db;
        let (guard, retained) = self
            .store
            .offline_bootstrap
            .as_ref()
            .ok_or_else(|| Error("offline allocation missing".into()))?
            .allocation
            .image_reservation(db)?;
        Ok(OfflineImageAllocation {
            db,
            guard,
            retained,
            peak: 0,
        })
    }
}

pub(crate) use completed_catalog::EndpointFreshFacts;
