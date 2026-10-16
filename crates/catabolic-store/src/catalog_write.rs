//! Narrow private native writer and index-maintenance admission. No public
//! Store/Connection getter, generic SQL, or arbitrary mutation callback exists.
use crate::{
    Error, Store,
    catalog_commands::{
        self, CatalogAuthority, CommandStoreErrorCode, ReceiptPolicy, TrustedCommandAuthority,
    },
    catalog_metadata_store::{MetadataAccess, MetadataAuthority, MetadataChange, MetadataMutation},
    catalog_scope::{
        LeasedRead, ReadBudget, SavedQueryRef, ScopeErrorCode, ScopeHandle, ScopedReadSession,
        TrustedHostReadGrant, TrustedSelection,
    },
    native_writer_vm::NativeWriterVmAdmission,
    operation_budget::{BudgetLimits, OperationBudget},
    selection::EvaluationFailure,
};
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility},
    catalog_host_lease::{AuthorizationLease, HostAuthorization},
    catalog_metadata::SourceKind,
};
use serde_json::json;
use std::{cell::Cell, collections::BTreeSet, path::Path, time::Duration};
/// Only this module can construct the seal, and only for an owned session
/// opened here. An arbitrary public Store can never opt into hook replacement.
pub(crate) struct WriterSeal(());
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteErrorCode {
    AuthorityDenied,
    WrongSession,
    InvalidRequest,
    Unsupported,
    Scope(ScopeErrorCode),
    Budget(EvaluationFailure),
    Command(CommandStoreErrorCode),
    Index(crate::catalog_search_index::SearchIndexErrorCode),
    StoreFailure,
}
#[derive(Debug)]
pub struct WriteError {
    pub code: WriteErrorCode,
}
type Result<T> = std::result::Result<T, WriteError>;
fn fail(code: WriteErrorCode) -> WriteError {
    WriteError { code }
}
fn budget_failure(work: &OperationBudget) -> WriteError {
    fail(
        work.failure()
            .map_or(WriteErrorCode::StoreFailure, WriteErrorCode::Budget),
    )
}
impl From<Error> for WriteError {
    fn from(_: Error) -> Self {
        fail(WriteErrorCode::StoreFailure)
    }
}
impl From<rusqlite::Error> for WriteError {
    fn from(_: rusqlite::Error) -> Self {
        fail(WriteErrorCode::StoreFailure)
    }
}
impl From<crate::catalog_scope::ScopeError> for WriteError {
    fn from(error: crate::catalog_scope::ScopeError) -> Self {
        fail(WriteErrorCode::Scope(error.code))
    }
}
impl std::error::Error for WriteError {}
impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.code)
    }
}
#[derive(Clone, Copy)]
pub struct WriteBudget {
    timeout: Duration,
    limits: BudgetLimits,
}
impl WriteBudget {
    pub fn new(
        timeout: Duration,
        items: usize,
        values: usize,
        bytes: usize,
        work: usize,
    ) -> Result<Self> {
        if timeout.is_zero()
            || timeout > Duration::from_secs(60)
            || items > 100000
            || values > 1000000
            || bytes > 64 * 1024 * 1024
            || work > 10000000
        {
            return Err(fail(WriteErrorCode::InvalidRequest));
        }
        Ok(Self {
            timeout,
            limits: BudgetLimits {
                items,
                values,
                bytes,
                work,
            },
        })
    }
    fn operation(self) -> Result<OperationBudget> {
        OperationBudget::new(self.timeout, self.limits)
            .map_err(|cause| fail(WriteErrorCode::Budget(cause)))
    }
}
/// Independent trusted write authority. Read grants/handles cannot be converted
/// into this type. Source privileges and editable fields are explicit host input.
pub struct TrustedMetadataWriteGrant {
    host: HostAuthorization,
    profile: String,
    selections: Vec<SavedQueryRef>,
    source: MetadataAuthority,
    fields: BTreeSet<String>,
    receipt_visibility: ReceiptVisibility,
}
impl TrustedMetadataWriteGrant {
    pub fn new(
        host: HostAuthorization,
        profile: String,
        selections: Vec<SavedQueryRef>,
        source: MetadataAuthority,
        editable_fields: Vec<String>,
        receipt_visibility: ReceiptVisibility,
    ) -> Result<Self> {
        if source.host_namespace != host.namespace()
            || !matches!(
                &source.access,
                MetadataAccess::Source {
                    kind: SourceKind::Local,
                    ..
                }
            )
            || selections.is_empty()
            || selections.len() > 16
        {
            return Err(fail(WriteErrorCode::AuthorityDenied));
        }
        crate::access_admin::validate_host_metadata_fields(&editable_fields)
            .map_err(|_| fail(WriteErrorCode::InvalidRequest))?;
        if editable_fields.is_empty() {
            return Err(fail(WriteErrorCode::AuthorityDenied));
        }
        Ok(Self {
            host,
            profile,
            selections,
            source,
            fields: editable_fields.into_iter().collect(),
            receipt_visibility,
        })
    }
}
pub struct MetadataWriteHandle {
    epoch: u64,
    data_version: i64,
    owner: String,
    scope: ScopeHandle,
    source: MetadataAuthority,
    fields: BTreeSet<String>,
    receipt_visibility: ReceiptVisibility,
}
/// Only an existing-item local metadata patch. Subject/source/kind come solely
/// from trusted admission; requests cannot select provider/scan/local privilege.
pub struct MetadataPatchRequest {
    pub operation_id: String,
    pub schema_fingerprint: String,
    pub expected_revision: i64,
    pub edits_json: String,
    pub evidence_json: String,
}
/// Kept crate-private so only the durable dispatcher can check this proof after
/// receipt replay and inside its BEGIN IMMEDIATE, before any new source mutation.
pub(crate) struct ScopedMetadataAdmission<'a> {
    writer_epoch: u64,
    handle: &'a MetadataWriteHandle,
    request: &'a MetadataPatchRequest,
}
impl ScopedMetadataAdmission<'_> {
    pub(crate) fn check(
        &self,
        store: &Store,
    ) -> std::result::Result<(), catalog_commands::CommandStoreError> {
        let denied = |message: &str| catalog_commands::CommandStoreError {
            code: CommandStoreErrorCode::AuthorityDenied,
            error: Error(message.into()),
        };
        let data_version: i64 = store
            .db
            .query_row("PRAGMA main.data_version", [], |r| r.get(0))?;
        if self.writer_epoch != self.handle.epoch || data_version != self.handle.data_version {
            return Err(denied("scoped writer snapshot changed"));
        }
        self.handle
            .scope
            .validate_write_member(store, &self.handle.source.subject)
            .map_err(|_| denied("scoped metadata membership changed"))?;
        if self.request.edits_json.len() > crate::catalog_metadata_store::MAX_RAW_MUTATION_BYTES
            || self.request.evidence_json.len()
                > crate::catalog_metadata_store::MAX_RAW_MUTATION_BYTES
        {
            return Err(denied("scoped metadata input exceeds bound"));
        }
        let edits = catabolic_core::decode_json(&self.request.edits_json)
            .map_err(|_| denied("invalid scoped metadata edits"))?;
        let fields = edits
            .as_object()
            .ok_or_else(|| denied("scoped edits require an object"))?;
        if fields
            .keys()
            .any(|field| !self.handle.fields.contains(field))
        {
            return Err(denied("scoped metadata field is not editable"));
        }
        Ok(())
    }
}
/// Global index maintenance is a distinct trusted capability, never a read/search
/// scope. Its status result is returned only through the privileged method below.
pub struct TrustedIndexMaintenance {
    host: HostAuthorization,
    catalog: String,
    incarnation: String,
}
impl TrustedIndexMaintenance {
    pub fn new(host: HostAuthorization, catalog: String, incarnation: String) -> Self {
        Self {
            host,
            catalog,
            incarnation,
        }
    }
}
pub struct IndexMaintenanceHandle {
    owner: String,
    grant: TrustedIndexMaintenance,
    generation: i64,
}
/// Current independent receipt-read authority for one operation, separate from
/// metadata write grants and saved-selection membership. Never Deserialize.
pub struct TrustedReceiptReadGrant {
    host: HostAuthorization,
    catalog: String,
    operation_id: String,
    visibility: ReceiptVisibility,
}
impl TrustedReceiptReadGrant {
    pub fn new(
        host: HostAuthorization,
        catalog: String,
        operation_id: String,
        visibility: ReceiptVisibility,
    ) -> Self {
        Self {
            host,
            catalog,
            operation_id,
            visibility,
        }
    }
}
// The fixed-COMMIT proof is qualified only for these exact finalizer sets.
// A future schema (including worker/finalizer additions) must be separately
// integrated and qualified before it can use this private writer owner.
// Schema36 adds guarded lifetime rows; schema37 adds guarded companion rows.
// Both add statement-time generation triggers, no additional COMMIT finalizers.
// Schema38 adds statement-time guards and one deferred receipt FK, which the
// receipt-last owner satisfies before COMMIT; it adds no COMMIT-time writes.
// Their separately qualified upgrade matrices preserve the same FTS preflush
// and fixed-COMMIT contract; arbitrary future additions remain refused.
fn qualified_store(store: &Store) -> Result<&Store> {
    if ![34, 35, 36, 37, 38].contains(&store.schema_version) {
        return Err(fail(WriteErrorCode::Unsupported));
    }
    let actual: u32 = store
        .db
        .pragma_query_value(None, "user_version", |r| r.get(0))?;
    if actual != store.schema_version {
        return Err(fail(WriteErrorCode::Unsupported));
    }
    Ok(store)
}

pub struct PrivateCatalogWriter {
    inner: ScopedReadSession,
    nonce: String,
    poisoned: Cell<bool>,
    seal: WriterSeal,
    epoch: u64,
}
impl PrivateCatalogWriter {
    pub fn open(path: &Path) -> Result<Self> {
        let writer = Self {
            inner: ScopedReadSession::open_private_writer(path)?,
            nonce: uuid::Uuid::new_v4().to_string(),
            poisoned: Cell::new(false),
            seal: WriterSeal(()),
            epoch: 0,
        };
        writer.store()?;
        Ok(writer)
    }
    fn store(&self) -> Result<&Store> {
        if self.poisoned.get() {
            return Err(fail(WriteErrorCode::StoreFailure));
        }
        qualified_store(self.inner.private_writer_store()?)
    }
    pub fn admit_metadata_write(
        &mut self,
        grant: TrustedMetadataWriteGrant,
        lease: &AuthorizationLease,
        budget: ReadBudget,
    ) -> Result<MetadataWriteHandle> {
        if !lease.belongs_to(&grant.host) {
            return Err(fail(WriteErrorCode::AuthorityDenied));
        }
        let before: i64 = self
            .store()?
            .db
            .query_row("PRAGMA main.data_version", [], |r| r.get(0))?;
        let selections = grant
            .selections
            .into_iter()
            .map(|query| TrustedSelection::new(query, vec![]))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let scope = self.inner.admit_host_scope(
            TrustedHostReadGrant::new(grant.host, grant.profile, selections, vec![])?,
            lease,
            budget,
        )?;
        let after: i64 = self
            .store()?
            .db
            .query_row("PRAGMA main.data_version", [], |r| r.get(0))?;
        if before != after {
            return Err(fail(WriteErrorCode::Scope(ScopeErrorCode::StaleScope)));
        }
        Ok(MetadataWriteHandle {
            epoch: self.epoch,
            data_version: after,
            owner: self.nonce.clone(),
            scope,
            source: grant.source,
            fields: grant.fields,
            receipt_visibility: grant.receipt_visibility,
        })
    }
    pub fn execute_metadata_patch(
        &mut self,
        handle: &MetadataWriteHandle,
        lease: AuthorizationLease,
        request: MetadataPatchRequest,
        policy: ReceiptPolicy,
        budget: WriteBudget,
    ) -> Result<LeasedRead> {
        if handle.owner != self.nonce {
            return Err(fail(WriteErrorCode::WrongSession));
        }
        if !lease.belongs_to(handle.scope.host_authority()) {
            return Err(fail(WriteErrorCode::AuthorityDenied));
        }
        if request
            .edits_json
            .len()
            .saturating_add(request.evidence_json.len())
            > crate::catalog_metadata_store::MAX_RAW_MUTATION_BYTES
            || request.schema_fingerprint.len() > 256
            || request.operation_id.len() > 256
        {
            return Err(fail(WriteErrorCode::InvalidRequest));
        }
        let work = budget.operation()?;
        let _budget = work
            .enter()
            .map_err(|cause| fail(WriteErrorCode::Budget(cause)))?;
        catalog_commands::charge_command_bytes(
            request
                .edits_json
                .len()
                .saturating_add(request.evidence_json.len())
                .saturating_mul(2),
        )
        .map_err(|_| budget_failure(&work))?;
        let (catalog, incarnation, generation) = handle.scope.command_stamp();
        let MetadataAccess::Source {
            namespace,
            record_id,
            ..
        } = &handle.source.access
        else {
            return Err(fail(WriteErrorCode::AuthorityDenied));
        };
        let mutation = MetadataMutation {
            contract_version: 1,
            change: MetadataChange::Patch {
                subject: handle.source.subject.clone(),
                namespace: namespace.clone(),
                record_id: record_id.clone(),
                schema_fingerprint: request.schema_fingerprint.clone(),
                expected_revision: request.expected_revision,
                edits_json: request.edits_json.clone(),
                evidence_json: request.evidence_json.clone(),
            },
        };
        let key = CommandKey::new(catalog, lease.namespace(), &request.operation_id)
            .map_err(|_| fail(WriteErrorCode::InvalidRequest))?;
        let payload =
            catalog_commands::budgeted_json(&json!({"operation":"metadata","mutation":mutation}))
                .map_err(|_| budget_failure(&work))?;
        catalog_commands::charge_command_bytes(payload.len().saturating_mul(2))
            .map_err(|_| budget_failure(&work))?;
        let command = CommandEnvelope::new(1, key, incarnation, generation, &payload)
            .map_err(|_| fail(WriteErrorCode::InvalidRequest))?;
        let next_epoch = self
            .epoch
            .checked_add(1)
            .ok_or_else(|| fail(WriteErrorCode::InvalidRequest))?;
        let store = self.store()?;
        let vm = NativeWriterVmAdmission::install(&store.db, &work, &self.poisoned, &self.seal)
            .map_err(|_| budget_failure(&work))?;
        let authority = CatalogAuthority::new(TrustedCommandAuthority {
            host_namespace: lease.namespace(),
            receipt_visibility: handle.receipt_visibility,
            allow_new_commands: true,
            identity_authority: None,
        })
        .with_metadata(&handle.source);
        let changes_before = store.db.total_changes();
        let outcome = catalog_commands::execute_scoped_metadata(
            store,
            &command,
            authority,
            policy,
            &ScopedMetadataAdmission {
                writer_epoch: self.epoch,
                handle,
                request: &request,
            },
        );
        let changed = store.db.total_changes() != changes_before;
        vm.finish()?;
        let outcome = outcome.map_err(|error| {
            fail(
                work.failure()
                    .map_or(WriteErrorCode::Command(error.code), WriteErrorCode::Budget),
            )
        })?;
        if changed {
            self.epoch = next_epoch;
        }
        Ok(LeasedRead::held(outcome, lease))
    }
    pub fn reconcile_receipt(
        &mut self,
        grant: TrustedReceiptReadGrant,
        lease: AuthorizationLease,
        command: &CommandEnvelope,
        policy: ReceiptPolicy,
        budget: WriteBudget,
    ) -> Result<LeasedRead> {
        if !lease.belongs_to(&grant.host)
            || command.key().catalog_id() != grant.catalog
            || command.key().operation_id() != grant.operation_id
            || command.key().host_namespace() != lease.namespace()
        {
            return Err(fail(WriteErrorCode::AuthorityDenied));
        }
        let work = budget.operation()?;
        let _budget = work
            .enter()
            .map_err(|cause| fail(WriteErrorCode::Budget(cause)))?;
        let store = self.store()?;
        let vm = NativeWriterVmAdmission::install(&store.db, &work, &self.poisoned, &self.seal)
            .map_err(|_| budget_failure(&work))?;
        let outcome = catalog_commands::reconcile_private_receipt(
            store,
            command,
            TrustedCommandAuthority {
                host_namespace: lease.namespace(),
                receipt_visibility: grant.visibility,
                allow_new_commands: false,
                identity_authority: None,
            },
            policy,
        );
        vm.finish()?;
        let outcome = outcome.map_err(|error| {
            fail(
                work.failure()
                    .map_or(WriteErrorCode::Command(error.code), WriteErrorCode::Budget),
            )
        })?;
        Ok(LeasedRead::held(outcome, lease))
    }
    pub fn admit_index_maintenance(
        &mut self,
        grant: TrustedIndexMaintenance,
        lease: &AuthorizationLease,
    ) -> Result<IndexMaintenanceHandle> {
        if !lease.belongs_to(&grant.host) {
            return Err(fail(WriteErrorCode::AuthorityDenied));
        }
        let store = self.store()?;
        store.validate_native_identity()?;
        if store.schema_version < 35 {
            return Err(fail(WriteErrorCode::Unsupported));
        }
        if store.database_id != grant.catalog
            || store.catalog_incarnation.as_deref() != Some(grant.incarnation.as_str())
        {
            return Err(fail(WriteErrorCode::AuthorityDenied));
        }
        if !store.db.is_autocommit() {
            return Err(fail(WriteErrorCode::StoreFailure));
        }
        let generation =
            store
                .db
                .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                    r.get(0)
                })?;
        Ok(IndexMaintenanceHandle {
            owner: self.nonce.clone(),
            grant,
            generation,
        })
    }
    pub fn rebuild_index(
        &mut self,
        handle: &IndexMaintenanceHandle,
        lease: AuthorizationLease,
        budget: WriteBudget,
    ) -> Result<LeasedRead> {
        if handle.owner != self.nonce {
            return Err(fail(WriteErrorCode::WrongSession));
        }
        if !lease.belongs_to(&handle.grant.host) {
            return Err(fail(WriteErrorCode::AuthorityDenied));
        }
        let work = budget.operation()?;
        let _budget = work
            .enter()
            .map_err(|cause| fail(WriteErrorCode::Budget(cause)))?;
        let next_epoch = self
            .epoch
            .checked_add(1)
            .ok_or_else(|| fail(WriteErrorCode::InvalidRequest))?;
        let store = self.store()?;
        let vm = NativeWriterVmAdmission::install(&store.db, &work, &self.poisoned, &self.seal)
            .map_err(|_| budget_failure(&work))?;
        let result = store.transaction_detailed::<_, WriteError>(|db| {
            store.validate_native_identity()?;
            let generation: i64 =
                db.query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                    r.get(0)
                })?;
            if generation != handle.generation
                || store.database_id != handle.grant.catalog
                || store.catalog_incarnation.as_deref() != Some(handle.grant.incarnation.as_str())
            {
                return Err(fail(WriteErrorCode::AuthorityDenied));
            }
            if crate::native_writer_vm::current_budget(db).is_none() {
                return Err(fail(WriteErrorCode::StoreFailure));
            }
            let status = crate::catalog_search_index::rebuild_connection(db, &work)
                .map_err(|error| fail(WriteErrorCode::Index(error.code)))?;
            catalog_commands::prepare_delivery(&status).map_err(Into::into)
        });
        vm.finish()?;
        let result = result
            .map_err(|error| fail(work.failure().map_or(error.code, WriteErrorCode::Budget)))?;
        self.epoch = next_epoch;
        Ok(LeasedRead::held(result, lease))
    }
}
#[cfg(test)]
#[path = "catalog_write_tests.rs"]
mod tests;
