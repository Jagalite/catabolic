//! Trusted-host scoped item reads and real authorized FTS. No external users.
//!
//! Trusted host code constructs grants from immutable saved-query references and
//! explicit field masks. Request IDs only look up data inside already admitted
//! scope. Public SQL is limited to approved saved reports and fully materialized
//! masked TEMP tables. Public GraphQL/tags/pins/files are deliberately not exposed
//! until their effective-state resolver adapters have equivalent coverage.
use crate::{
    Error, Store,
    selection::{Evaluation, EvaluationFailure},
    sql,
};
use catabolic_core::{
    catalog_host_lease::{AuthorizationLease, HostAuthorization},
    encode, sha256,
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::operation_budget::{BudgetCharge, BudgetLimits, OperationBudget};

const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_DEFINITION_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeErrorCode {
    InvalidGrant,
    InvalidRequest,
    InvalidCursor,
    StaleCursor,
    WrongSession,
    WrongAuthority,
    StaleScope,
    SelectionChanged,
    Unsupported,
    NotFoundOrNotVisible,
    AmbientTransaction,
    IdBudget,
    ByteBudget,
    WorkBudget,
    Timeout,
    Cancelled,
    QueryRejected,
    Catalog,
    SearchNotReady,
    MetadataNotInitialized,
}
#[derive(Debug)]
pub struct ScopeError {
    pub code: ScopeErrorCode,
}
type Result<T> = std::result::Result<T, ScopeError>;
fn fail(code: ScopeErrorCode) -> ScopeError {
    ScopeError { code }
}
impl From<crate::Error> for ScopeError {
    fn from(_: crate::Error) -> Self {
        fail(ScopeErrorCode::Catalog)
    }
}
impl From<rusqlite::Error> for ScopeError {
    fn from(_: rusqlite::Error) -> Self {
        fail(ScopeErrorCode::Catalog)
    }
}
impl From<catabolic_core::Error> for ScopeError {
    fn from(_: catabolic_core::Error) -> Self {
        fail(ScopeErrorCode::Catalog)
    }
}

/// An immutable saved revision reference supplied by trusted host admission.
/// A reference identifies a definition; it does not itself confer any authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedQueryRef {
    id: String,
    revision: i64,
    digest: String,
}
impl SavedQueryRef {
    pub fn new(id: String, revision: i64, digest: String) -> Result<Self> {
        if id.is_empty()
            || id.len() > 256
            || revision < 1
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(fail(ScopeErrorCode::InvalidGrant));
        }
        Ok(Self {
            id,
            revision,
            digest,
        })
    }
}
/// Search relations are independent trusted-host privileges. Metadata masks do
/// not imply identity, tag, or historical alias-path access. No request decoder.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct TrustedSearchFields {
    identity_namespaces: BTreeSet<String>,
    effective_tags: bool,
    logical_aliases: bool,
}
impl TrustedSearchFields {
    pub fn new(
        identity_namespaces: Vec<String>,
        effective_tags: bool,
        logical_aliases: bool,
    ) -> Result<Self> {
        if identity_namespaces.len() > 256
            || identity_namespaces
                .iter()
                .any(|n| n.is_empty() || n.len() > 256 || n.chars().any(char::is_control))
        {
            return Err(fail(ScopeErrorCode::InvalidGrant));
        }
        Ok(Self {
            identity_namespaces: identity_namespaces.into_iter().collect(),
            effective_tags,
            logical_aliases,
        })
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
struct MemberVisibility {
    metadata: BTreeSet<String>,
    search: TrustedSearchFields,
    relationships: TrustedRelationshipRead,
    memberships: BTreeSet<String>,
    provenance: TrustedProvenanceRead,
    file_facts: TrustedFileFactsRead,
    components: TrustedComponentsRead,
}
/// Construct only after the embedding host has established its own permissions.
/// Deliberately not Deserialize; there is no item-ID-list grant constructor.
#[derive(Clone)]
pub struct TrustedSelection {
    query: SavedQueryRef,
    fields: BTreeSet<String>,
    search: TrustedSearchFields,
    relationships: TrustedRelationshipRead,
    membership_read: bool,
    provenance: TrustedProvenanceRead,
    file_facts: TrustedFileFactsRead,
    components: TrustedComponentsRead,
}
impl TrustedSelection {
    pub fn with_components(mut self, fields: TrustedComponentsRead) -> Self {
        self.components = fields;
        self
    }
    pub fn with_file_facts(mut self, fields: TrustedFileFactsRead) -> Self {
        self.file_facts = fields;
        self
    }
    pub fn with_membership_read(mut self) -> Self {
        self.membership_read = true;
        self
    }
    pub fn with_provenance(mut self, fields: TrustedProvenanceRead) -> Self {
        self.provenance = fields;
        self
    }
    pub fn with_relationships(mut self, fields: TrustedRelationshipRead) -> Self {
        self.relationships = fields;
        self
    }
    pub fn with_search_fields(mut self, fields: TrustedSearchFields) -> Self {
        self.search = fields;
        self
    }
    pub fn new(query: SavedQueryRef, fields: Vec<String>) -> Result<Self> {
        crate::access_admin::validate_host_metadata_fields(&fields)
            .map_err(|_| fail(ScopeErrorCode::InvalidGrant))?;
        Ok(Self {
            query,
            fields: fields.into_iter().collect(),
            search: TrustedSearchFields::default(),
            relationships: TrustedRelationshipRead::default(),
            membership_read: false,
            provenance: TrustedProvenanceRead::default(),
            file_facts: TrustedFileFactsRead::default(),
            components: TrustedComponentsRead::default(),
        })
    }
}
/// No persisted principal/token is created or imported for the external host.
pub struct TrustedHostReadGrant {
    authority: HostAuthorization,
    profile: String,
    selections: Vec<TrustedSelection>,
    reports: Vec<SavedQueryRef>,
}
impl TrustedHostReadGrant {
    pub fn new(
        authority: HostAuthorization,
        profile: String,
        selections: Vec<TrustedSelection>,
        reports: Vec<SavedQueryRef>,
    ) -> Result<Self> {
        if profile.is_empty()
            || profile.len() > 256
            || !(1..=16).contains(&selections.len())
            || reports.len() > 16
        {
            return Err(fail(ScopeErrorCode::InvalidGrant));
        }
        let mut mask_bytes = 0usize;
        for selection in &selections {
            mask_bytes = mask_bytes.saturating_add(
                encode(&json!([
                    selection.search,
                    selection.relationships,
                    selection.membership_read,
                    selection.provenance,
                    selection.file_facts,
                    selection.components
                ]))
                .len(),
            );
            if mask_bytes > MAX_BYTES {
                return Err(fail(ScopeErrorCode::ByteBudget));
            }
            for field in &selection.fields {
                mask_bytes = mask_bytes.saturating_add(encode(&json!(field)).len() + 1);
                if mask_bytes > MAX_BYTES {
                    return Err(fail(ScopeErrorCode::ByteBudget));
                }
            }
        }
        Ok(Self {
            authority,
            profile,
            selections,
            reports,
        })
    }
}
#[derive(Clone, Copy)]
pub struct ReadBudget {
    timeout_ms: u64,
    maximum_ids: usize,
}
impl ReadBudget {
    pub fn new(timeout: Duration, maximum_ids: usize) -> Result<Self> {
        let millis = timeout.as_millis();
        if !(1..=60000).contains(&millis) || !(1..=100000).contains(&maximum_ids) {
            return Err(fail(ScopeErrorCode::InvalidRequest));
        }
        Ok(Self {
            timeout_ms: millis as u64,
            maximum_ids,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    schema: u32,
    data_version: i64,
    catalog: String,
    incarnation: String,
    generation: i64,
}
fn snapshot(store: &Store) -> Result<Snapshot> {
    store.validate_native_identity()?;
    let (catalog, incarnation, generation) = store.db.query_row(
        "SELECT catalog_id,incarnation,generation FROM main.catalog_state WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let data_version = store
        .db
        .query_row("PRAGMA main.data_version", [], |r| r.get(0))?;
    Ok(Snapshot {
        schema: store.schema_version,
        data_version,
        catalog,
        incarnation,
        generation,
    })
}
fn shared_charge(charge: BudgetCharge) -> Result<()> {
    if let Some(budget) = OperationBudget::current() {
        budget
            .charge(charge)
            .map_err(|cause| fail(evaluation_code(Some(cause))))?;
    }
    Ok(())
}
fn operation(budget: ReadBudget) -> Result<OperationBudget> {
    OperationBudget::new(
        Duration::from_millis(budget.timeout_ms),
        BudgetLimits {
            items: budget.maximum_ids.saturating_mul(4),
            values: 1_000_000,
            bytes: 16 * 1024 * 1024,
            work: 10_000_000,
        },
    )
    .map_err(|cause| fail(evaluation_code(Some(cause))))
}
/// Validate an existing trusted selection reference without disclosing records
/// or executing its selection. A valid reference is not an application grant.
pub fn validate_selection_reference(
    store: &Store,
    profile: &str,
    reference: &SavedQueryRef,
) -> Result<()> {
    let definition = query(store, profile, reference)?;
    if definition["mode"] != "selection" {
        return Err(fail(ScopeErrorCode::InvalidGrant));
    }
    Ok(())
}

fn query(store: &Store, profile: &str, reference: &SavedQueryRef) -> Result<Value> {
    let length:Option<i64>=store.db.query_row("SELECT length(CAST(definition AS BLOB)) FROM main.saved_queries WHERE id=? AND profile=?",params![reference.id,profile],|r|r.get(0)).optional()?;
    if let Some(length) = length {
        if length < 0 || length > MAX_DEFINITION_BYTES as i64 {
            return Err(fail(ScopeErrorCode::ByteBudget));
        }
        shared_charge(BudgetCharge {
            bytes: length as usize,
            values: 1,
            work: 1,
            ..Default::default()
        })?;
    }
    let row: Option<(i64, String, String, i64)> = store.db.query_row(
        "SELECT revision,digest,CASE WHEN length(CAST(definition AS BLOB))<=? THEN definition ELSE '' END,length(CAST(definition AS BLOB)) FROM main.saved_queries WHERE id=? AND profile=?",
        params![MAX_DEFINITION_BYTES as i64, reference.id, profile],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
    let Some((revision, digest, raw, bytes)) = row else {
        return Err(fail(ScopeErrorCode::SelectionChanged));
    };
    if bytes < 0 || bytes > MAX_DEFINITION_BYTES as i64 {
        return Err(fail(ScopeErrorCode::ByteBudget));
    }
    if revision != reference.revision || digest != reference.digest || sha256(&raw) != digest {
        return Err(fail(ScopeErrorCode::SelectionChanged));
    }
    Ok(catabolic_core::decode_json(&raw)?)
}
fn dependency_refs(
    store: &Store,
    profile: &str,
    reference: &SavedQueryRef,
    depth: usize,
    refs: &mut BTreeMap<String, SavedQueryRef>,
    bytes: &mut usize,
) -> Result<()> {
    if depth >= 16 || refs.len() >= 64 {
        return Err(fail(ScopeErrorCode::WorkBudget));
    }
    if let Some(existing) = refs.get(&reference.id) {
        return if existing == reference {
            Ok(())
        } else {
            Err(fail(ScopeErrorCode::SelectionChanged))
        };
    }
    let definition = query(store, profile, reference)?;
    *bytes = bytes.saturating_add(encode(&definition).len());
    if *bytes > MAX_BYTES {
        return Err(fail(ScopeErrorCode::ByteBudget));
    }
    refs.insert(reference.id.clone(), reference.clone());
    for id in definition["queries"].as_array().into_iter().flatten() {
        let id = id
            .as_str()
            .ok_or_else(|| fail(ScopeErrorCode::SelectionChanged))?;
        let child = store.db.query_row(
            "SELECT revision,digest FROM main.saved_queries WHERE id=? AND profile=?",
            params![id, profile],
            |r| {
                Ok(SavedQueryRef {
                    id: id.to_owned(),
                    revision: r.get(0)?,
                    digest: r.get(1)?,
                })
            },
        )?;
        dependency_refs(store, profile, &child, depth + 1, refs, bytes)?;
    }
    Ok(())
}
/// Sealed proof used only during complete membership evaluation, before public
/// materialization. It cannot be made from client-supplied selection JSON.
pub(crate) struct HostSelectionAdmission {
    state: Snapshot,
    profile: String,
    query: SavedQueryRef,
}
impl HostSelectionAdmission {
    pub(crate) fn query_id(&self) -> &str {
        &self.query.id
    }
    pub(crate) fn check(&self, store: &Store) -> crate::Result<()> {
        let checked = snapshot(store).and_then(|state| {
            if state != self.state {
                return Err(fail(ScopeErrorCode::StaleScope));
            }
            query(store, &self.profile, &self.query).map(|_| ())
        });
        checked.map_err(|_| Error("host_selection_admission_changed".into()))
    }
}
struct Capability {
    members: BTreeMap<String, MemberVisibility>,
    session: String,
    state: Snapshot,
    authority: HostAuthorization,
    profile: String,
    selections: Vec<TrustedSelection>,
    reports: BTreeMap<String, SavedQueryRef>,
    dependencies: BTreeMap<String, SavedQueryRef>,
    fingerprint: String,
    /// Per-admission identity and secret never originate in request JSON.
    capability_id: String,
    cursor_key: [u8; 32],
    membership_digest: String,
}
/// Private fields, no Deserialize, no conversion from caller IDs or JSON.
#[derive(Clone)]
pub struct ScopeHandle(Arc<Capability>);
impl ScopeHandle {
    pub(crate) fn host_authority(&self) -> &HostAuthorization {
        &self.0.authority
    }
    pub(crate) fn command_stamp(&self) -> (&str, &str, i64) {
        (
            &self.0.state.catalog,
            &self.0.state.incarnation,
            self.0.state.generation,
        )
    }
    pub(crate) fn validate_write_member(&self, store: &Store, subject: &str) -> Result<()> {
        if snapshot(store)? != self.0.state {
            return Err(fail(ScopeErrorCode::StaleScope));
        }
        for reference in self.0.dependencies.values() {
            query(store, &self.0.profile, reference)?;
        }
        if !self.0.members.contains_key(subject) {
            return Err(fail(ScopeErrorCode::NotFoundOrNotVisible));
        }
        Ok(())
    }
}

/// Request IDs/cursors only narrow an existing scope. No raw SQL/GraphQL variant.
pub enum ScopeReadRequest {
    GetItem(String),
    GetMemberships(String),
    GetMetadataProvenance(String),
    GetFileFacts(String),
    GetComponents(String),
    ItemsPage {
        cursor: Option<String>,
        limit: usize,
    },
    RelationshipsPage {
        item_id: String,
        cursor: Option<String>,
        limit: usize,
    },
    SavedReport {
        query_id: String,
        maximum_rows: usize,
    },
}
/// The payload remains private until a host delivery callback owns the lease.
/// Revocation drain cannot acknowledge effective revocation during this callback.
pub struct LeasedRead {
    value: Value,
    lease: AuthorizationLease,
}
impl LeasedRead {
    pub(crate) fn held(value: Value, lease: AuthorizationLease) -> Self {
        Self { value, lease }
    }
    pub fn deliver<T>(self, deliver: impl FnOnce(&Value) -> T) -> T {
        let result = deliver(&self.value);
        drop(self.lease);
        result
    }
}
/// Fresh-read owner. It never returns Store/Connection/legacy Session, and cannot
/// be constructed from an ambient caller-owned connection or transaction.
/// Legacy Store::open(false) and embedding::Session retain their old snapshots.
pub struct ScopedReadSession {
    store: Option<Store>,
    nonce: String,
    poisoned: bool,
}
impl ScopedReadSession {
    pub fn open(path: &Path) -> Result<Self> {
        let store = Store::open(path, false, false)?;
        if store.schema_version < 34 {
            return Err(fail(ScopeErrorCode::Unsupported));
        }
        // This is exactly the initial BEGIN owned by the Store just created here.
        store.db.execute_batch("ROLLBACK")?;
        snapshot(&store)?;
        Ok(Self {
            store: Some(store),
            nonce: uuid::Uuid::new_v4().to_string(),
            poisoned: false,
        })
    }
    /// Open the scoped read owner only for the exact catalog incarnation the
    /// embedding host already admitted. Checking another connection or merely
    /// checking the pathname first leaves a replacement race.
    pub fn open_expected(path: &Path, catalog_id: &str, incarnation: &str) -> Result<Self> {
        if catalog_id.is_empty() || incarnation.is_empty() {
            return Err(fail(ScopeErrorCode::InvalidGrant));
        }
        let mut session = Self::open(path)?;
        session.fresh(|_, current| {
            if current.catalog != catalog_id || current.incarnation != incarnation {
                return Err(fail(ScopeErrorCode::WrongAuthority));
            }
            Ok(())
        })?;
        Ok(session)
    }

    pub(crate) fn open_private_writer(path: &Path) -> Result<Self> {
        let store = Store::open(path, true, false)?;
        if store.schema_version < 34 {
            return Err(fail(ScopeErrorCode::Unsupported));
        }
        snapshot(&store)?;
        Ok(Self {
            store: Some(store),
            nonce: uuid::Uuid::new_v4().to_string(),
            poisoned: false,
        })
    }
    pub(crate) fn private_writer_store(&self) -> Result<&Store> {
        if self.poisoned {
            return Err(fail(ScopeErrorCode::Catalog));
        }
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
        if !store.writable {
            return Err(fail(ScopeErrorCode::Unsupported));
        }
        Ok(store)
    }
    fn fresh<T>(
        &mut self,
        action: impl FnOnce(&mut Option<Store>, &Snapshot) -> Result<T>,
    ) -> Result<T> {
        if self.poisoned {
            return Err(fail(ScopeErrorCode::Catalog));
        }
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
        if !store.db.is_autocommit() {
            return Err(fail(ScopeErrorCode::AmbientTransaction));
        }
        // Private read admission refuses lock contention promptly. Its saved
        // SQL/GraphQL owners restore this zero timeout after their own budgets.
        store.db.busy_timeout(Duration::ZERO)?;
        store.validate_native_identity()?;
        let query_only: bool = store
            .db
            .pragma_query_value(None, "query_only", |r| r.get(0))?;
        store.db.execute_batch("BEGIN")?;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let state = snapshot(self.store.as_ref().unwrap())?;
            action(&mut self.store, &state).map(|value| (value, state))
        }));
        let cleanup = self
            .store
            .as_ref()
            .ok_or_else(|| fail(ScopeErrorCode::Catalog))
            .and_then(|store| {
                // SQLITE_INTERRUPT may already have rolled back the complete
                // private read transaction (TEMP FTS population writes). Do not
                // mask its typed cancellation with "no transaction is active".
                if !store.db.is_autocommit() {
                    store.db.execute_batch("ROLLBACK")?;
                }
                store.db.pragma_update(None, "query_only", query_only)?;
                snapshot(store)
            });
        if cleanup.is_err() {
            self.poisoned = true;
        }
        match outcome {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(Err(error)) => {
                cleanup?;
                Err(error)
            }
            Ok(Ok((value, before))) => {
                if before != cleanup? {
                    return Err(fail(ScopeErrorCode::StaleScope));
                }
                Ok(value)
            }
        }
    }
    pub fn admit_host_scope(
        &mut self,
        grant: TrustedHostReadGrant,
        lease: &AuthorizationLease,
        budget: ReadBudget,
    ) -> Result<ScopeHandle> {
        if !lease.belongs_to(&grant.authority) {
            return Err(fail(ScopeErrorCode::WrongAuthority));
        }
        let work = operation(budget)?;
        let _work_guard = work
            .enter()
            .map_err(|cause| fail(evaluation_code(Some(cause))))?;
        let session = self.nonce.clone();
        self.fresh(move |slot, state| {
            let started = Instant::now();
            if crate::embedding::poll_interrupt() { return Err(fail(ScopeErrorCode::Cancelled)); }
            let store = slot.as_ref().unwrap();
            let mut dependencies = BTreeMap::new();
            let mut bytes = 0;
            for reference in grant.selections.iter().map(|s| &s.query).chain(grant.reports.iter()) {
                dependency_refs(store, &grant.profile, reference, 0, &mut dependencies, &mut bytes)?;
            }
            for selection in &grant.selections {
                if query(store, &grant.profile, &selection.query)?["mode"] != "selection" {
                    return Err(fail(ScopeErrorCode::Unsupported));
                }
            }
            for report in &grant.reports {
                let definition = query(store, &grant.profile, report)?;
                if definition["mode"] != "rows" || definition["selection"]["language"] != "sql" {
                    return Err(fail(ScopeErrorCode::Unsupported));
                }
            }
            let capability_id = uuid::Uuid::new_v4().to_string();
            let mut cursor_key = [0; 32];
            getrandom::fill(&mut cursor_key).map_err(|_| fail(ScopeErrorCode::Catalog))?;
            let fingerprint = sha256(encode(&json!({"capability":capability_id,"profile":grant.profile,
                "reports":grant.reports.iter().map(|q|json!([q.id,q.revision,q.digest])).collect::<Vec<_>>(),
                "session":session,"schema":state.schema,"catalog":state.catalog,
                "incarnation":state.incarnation,"generation":state.generation,
                "host":grant.authority.namespace(),"auth_revision":grant.authority.revision(),
                "selections":grant.selections.iter().map(|s|json!([s.query.id,s.query.revision,s.query.digest,s.fields,s.search,s.relationships,s.membership_read,s.provenance,s.file_facts,s.components])).collect::<Vec<_>>(),
                "dependencies":dependencies.values().map(|q|json!([q.id,q.revision,q.digest])).collect::<Vec<_>>() })));
            let mut cap = Capability { members: BTreeMap::new(), session, state: state.clone(), authority: grant.authority,
                profile: grant.profile, selections: grant.selections,
                reports: grant.reports.into_iter().map(|q|(q.id.clone(),q)).collect(), dependencies, fingerprint,
                capability_id, cursor_key, membership_digest: String::new() };
            // Complete bounded membership must succeed at admission; failures do
            // not become an empty authorized scope. Do not cache scoped outputs.
            let fields = membership(slot, &cap, ReadBudget { timeout_ms: remaining(started, budget)?, ..budget })?;
            cap.membership_digest = membership_digest(&fields)?;
            cap.members = fields;
            remaining(started, budget)?;
            Ok(ScopeHandle(Arc::new(cap)))
        })
    }
    pub fn search_host_scope(
        &mut self,
        handle: &ScopeHandle,
        lease: AuthorizationLease,
        request: ScopedSearchRequest,
        budget: ReadBudget,
    ) -> Result<LeasedRead> {
        let cap = &handle.0;
        if cap.session != self.nonce {
            return Err(fail(ScopeErrorCode::WrongSession));
        }
        if !lease.belongs_to(&cap.authority) {
            return Err(fail(ScopeErrorCode::WrongAuthority));
        }
        let work = operation(budget)?;
        let _work_guard = work
            .enter()
            .map_err(|cause| fail(evaluation_code(Some(cause))))?;
        let request = search::prepare(cap, request)?;
        let poisoned = Cell::new(false);
        let value = self.fresh(|slot, state| {
            let started = Instant::now();
            if state != &cap.state {
                return Err(fail(ScopeErrorCode::StaleScope));
            }
            for reference in cap.dependencies.values() {
                remaining(started, budget)?;
                query(slot.as_ref().unwrap(), &cap.profile, reference)?;
            }
            let fields = membership(
                slot,
                cap,
                ReadBudget {
                    timeout_ms: remaining(started, budget)?,
                    ..budget
                },
            )?;
            let current = membership_digest(&fields)?;
            if current != cap.membership_digest {
                return Err(fail(if request.has_cursor() {
                    ScopeErrorCode::StaleCursor
                } else {
                    ScopeErrorCode::StaleScope
                }));
            }
            search::execute(
                slot.as_ref().unwrap(),
                cap,
                &fields,
                request,
                started,
                budget,
                &poisoned,
            )
        });
        if poisoned.get() {
            self.poisoned = true;
        }
        let value = value?;
        work.check()
            .map_err(|cause| fail(evaluation_code(Some(cause))))?;
        Ok(LeasedRead { value, lease })
    }
    pub fn read_host_scope(
        &mut self,
        handle: &ScopeHandle,
        lease: AuthorizationLease,
        request: ScopeReadRequest,
        budget: ReadBudget,
    ) -> Result<LeasedRead> {
        let cap = &handle.0;
        if cap.session != self.nonce {
            return Err(fail(ScopeErrorCode::WrongSession));
        }
        if !lease.belongs_to(&cap.authority) {
            return Err(fail(ScopeErrorCode::WrongAuthority));
        }
        let work = operation(budget)?;
        let _work_guard = work
            .enter()
            .map_err(|cause| fail(evaluation_code(Some(cause))))?;
        let request = prepare_read_request(cap, request)?;
        let poisoned = Cell::new(false);
        let value = self.fresh(|slot, state| {
            let started = Instant::now();
            if state != &cap.state {
                return Err(fail(ScopeErrorCode::StaleScope));
            }
            for reference in cap.dependencies.values() {
                remaining(started, budget)?;
                query(slot.as_ref().unwrap(), &cap.profile, reference)?;
            }
            let fields = membership(
                slot,
                cap,
                ReadBudget {
                    timeout_ms: remaining(started, budget)?,
                    ..budget
                },
            )?;
            let current_membership = membership_digest(&fields)?;
            let cursor_membership = match &request {
                PreparedReadRequest::ItemsPage {
                    position: Some(position),
                    ..
                } => Some(position.membership.as_str()),
                PreparedReadRequest::Relationships(page) => page.membership(),
                _ => None,
            };
            if cursor_membership.is_some_and(|digest| digest != current_membership) {
                return Err(fail(ScopeErrorCode::StaleCursor));
            }
            // Saved queries can depend on receipts/derived rows that change
            // without catalog generation. Never silently expand or shrink the
            // membership or masks captured by this capability's admission.
            if current_membership != cap.membership_digest {
                return Err(fail(ScopeErrorCode::StaleScope));
            }
            if let PreparedReadRequest::Detail(request) = request {
                return details::read(
                    slot.as_ref().unwrap(),
                    cap,
                    &fields,
                    request,
                    started,
                    budget,
                    &poisoned,
                );
            }
            materialize(slot.as_ref().unwrap(), &fields, started, budget, &poisoned)?;
            read_materialized(slot.as_ref().unwrap(), cap, request, started, budget)
        });
        if poisoned.get() {
            self.poisoned = true;
        }
        let value = value?;
        work.check()
            .map_err(|cause| fail(evaluation_code(Some(cause))))?;
        Ok(LeasedRead { value, lease })
    }
}
const MAX_CURSOR_BYTES: usize = 4096;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemCursorPosition {
    after: String,
    membership: String,
}
enum PreparedReadRequest {
    Other(ScopeReadRequest),
    Relationships(relations::PreparedPage),
    Detail(details::Request),
    ItemsPage {
        position: Option<ItemCursorPosition>,
        limit: usize,
    },
}
fn cursor_scope(cap: &Capability, limit: usize) -> Value {
    json!({"contract_version":1,"surface":"native_scoped_items","order":"id_binary",
        "capability":cap.capability_id,"limit":limit})
}
fn prepare_read_request(
    cap: &Capability,
    request: ScopeReadRequest,
) -> Result<PreparedReadRequest> {
    let request = match request {
        ScopeReadRequest::GetComponents(item) => {
            return Ok(PreparedReadRequest::Detail(details::Request::Components(
                item,
            )));
        }
        ScopeReadRequest::GetFileFacts(item) => {
            return Ok(PreparedReadRequest::Detail(details::Request::FileFacts(
                item,
            )));
        }
        ScopeReadRequest::GetMemberships(item) => {
            return Ok(PreparedReadRequest::Detail(details::Request::Memberships(
                item,
            )));
        }
        ScopeReadRequest::GetMetadataProvenance(item) => {
            return Ok(PreparedReadRequest::Detail(details::Request::Provenance(
                item,
            )));
        }
        ScopeReadRequest::RelationshipsPage {
            item_id,
            cursor,
            limit,
        } => {
            return relations::prepare(cap, item_id, cursor, limit)
                .map(PreparedReadRequest::Relationships);
        }
        request => request,
    };
    let ScopeReadRequest::ItemsPage { cursor, limit } = request else {
        return Ok(PreparedReadRequest::Other(request));
    };
    if !(1..=1000).contains(&limit) {
        return Err(fail(ScopeErrorCode::InvalidRequest));
    }
    let position = match cursor {
        None => None,
        Some(cursor) => {
            if cursor.is_empty() || cursor.len() > MAX_CURSOR_BYTES {
                return Err(fail(ScopeErrorCode::InvalidCursor));
            }
            shared_charge(BudgetCharge {
                bytes: cursor.len(),
                values: 1,
                work: cursor.len() / 64 + 1,
                ..Default::default()
            })?;
            let after = crate::api_catalog::authenticated_cursor_after(
                &cap.cursor_key,
                &cap.fingerprint,
                &cursor_scope(cap, limit),
                Some(&cursor),
            )
            .map_err(|_| fail(ScopeErrorCode::InvalidCursor))?;
            let position: ItemCursorPosition =
                serde_json::from_str(&after).map_err(|_| fail(ScopeErrorCode::InvalidCursor))?;
            if position.after.is_empty()
                || position.after.len() > 1024
                || position.membership.len() != 64
                || !position
                    .membership
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            {
                return Err(fail(ScopeErrorCode::InvalidCursor));
            }
            Some(position)
        }
    };
    Ok(PreparedReadRequest::ItemsPage { position, limit })
}
fn membership_digest(fields: &BTreeMap<String, MemberVisibility>) -> Result<String> {
    // BTree ordering is the same deterministic item/mask ordering used by
    // admission and page materialization. Charge serialization and hash work.
    let canonical = encode(&json!(fields));
    if canonical.len() > MAX_BYTES {
        return Err(fail(ScopeErrorCode::ByteBudget));
    }
    shared_charge(BudgetCharge {
        bytes: canonical.len(),
        values: 1,
        work: canonical.len() / 64 + 1,
        ..Default::default()
    })?;
    Ok(sha256(canonical))
}
fn next_item_cursor(cap: &Capability, limit: usize, after: &str) -> Result<String> {
    if after.is_empty() || after.len() > 1024 {
        return Err(fail(ScopeErrorCode::ByteBudget));
    }
    let position = encode(&json!({"after":after,"membership":cap.membership_digest}));
    let cursor = crate::api_catalog::authenticated_cursor(
        &cap.cursor_key,
        &cap.fingerprint,
        &cursor_scope(cap, limit),
        &position,
    )
    .map_err(|_| fail(ScopeErrorCode::InvalidCursor))?;
    if cursor.len() > MAX_CURSOR_BYTES {
        return Err(fail(ScopeErrorCode::ByteBudget));
    }
    Ok(cursor)
}
fn evaluation_code(failure: Option<EvaluationFailure>) -> ScopeErrorCode {
    match failure {
        Some(EvaluationFailure::IdBudget) => ScopeErrorCode::IdBudget,
        Some(EvaluationFailure::ByteBudget) => ScopeErrorCode::ByteBudget,
        Some(EvaluationFailure::WorkBudget) => ScopeErrorCode::WorkBudget,
        Some(EvaluationFailure::Timeout) => ScopeErrorCode::Timeout,
        Some(EvaluationFailure::Cancelled) => ScopeErrorCode::Cancelled,
        Some(EvaluationFailure::MissingSavedQuery) => ScopeErrorCode::SelectionChanged,
        Some(EvaluationFailure::UnsupportedSelectionMode) => ScopeErrorCode::Unsupported,
        None => ScopeErrorCode::QueryRejected,
    }
}
fn remaining(started: Instant, budget: ReadBudget) -> Result<u64> {
    let shared = OperationBudget::current()
        .map(|work| work.remaining_ms())
        .transpose()
        .map_err(|cause| fail(evaluation_code(Some(cause))))?;
    if crate::embedding::poll_interrupt() {
        return Err(fail(ScopeErrorCode::Cancelled));
    }
    let left = Duration::from_millis(budget.timeout_ms)
        .saturating_sub(started.elapsed())
        .as_millis() as u64;
    if left == 0 {
        Err(fail(ScopeErrorCode::Timeout))
    } else {
        Ok(shared.map_or(left, |remaining| left.min(remaining)))
    }
}
fn membership(
    slot: &mut Option<Store>,
    cap: &Capability,
    budget: ReadBudget,
) -> Result<BTreeMap<String, MemberVisibility>> {
    let started = Instant::now();
    remaining(started, budget)?;
    let mut evaluation =
        Evaluation::unbound(&cap.profile, budget.timeout_ms, budget.maximum_ids, true);
    let mut fields: BTreeMap<String, MemberVisibility> = BTreeMap::new();
    let mut field_bytes = 0usize;
    let mut owner_failure = None;
    let result = evaluation.with_store(slot, |evaluation| {
        for selection in &cap.selections {
            let selected = evaluation.select_host_admitted(HostSelectionAdmission {
                state: cap.state.clone(),
                profile: cap.profile.clone(),
                query: selection.query.clone(),
            })?;
            if selected.entity != "item_id" {
                return Err(Error("host selection must return item_id".into()));
            }
            for id in selected.ids {
                if let Err(error) = remaining(started, budget) {
                    owner_failure = Some(error.code);
                    return Err(Error("host membership budget exhausted".into()));
                }
                if !fields.contains_key(&id) {
                    field_bytes = field_bytes.saturating_add(id.len()).saturating_add(4);
                }
                let visibility = fields.entry(id).or_default();
                for namespace in &selection.search.identity_namespaces {
                    if !visibility.search.identity_namespaces.contains(namespace) {
                        field_bytes = field_bytes.saturating_add(namespace.len() + 4);
                        if field_bytes > MAX_BYTES {
                            owner_failure = Some(ScopeErrorCode::ByteBudget);
                            return Err(Error("host search mask expansion exceeds budget".into()));
                        }
                        if let Err(error) = shared_charge(BudgetCharge {
                            bytes: namespace.len(),
                            values: 1,
                            work: 1,
                            ..Default::default()
                        }) {
                            owner_failure = Some(error.code);
                            return Err(Error("operation search mask budget exhausted".into()));
                        }
                        visibility
                            .search
                            .identity_namespaces
                            .insert(namespace.clone());
                    }
                }
                visibility.search.effective_tags |= selection.search.effective_tags;
                visibility.search.logical_aliases |= selection.search.logical_aliases;
                for (kind, allowed) in &selection.relationships.by_kind {
                    let target = visibility
                        .relationships
                        .by_kind
                        .entry(kind.clone())
                        .or_default();
                    field_bytes = field_bytes.saturating_add(kind.len() + 4);
                    if field_bytes > MAX_BYTES {
                        owner_failure = Some(ScopeErrorCode::ByteBudget);
                        return Err(Error("relationship mask expansion exceeds budget".into()));
                    }
                    for field in allowed {
                        if target.contains(field) {
                            continue;
                        }
                        field_bytes = field_bytes.saturating_add(field.len() + 4);
                        if field_bytes > MAX_BYTES {
                            owner_failure = Some(ScopeErrorCode::ByteBudget);
                            return Err(Error("relationship mask expansion exceeds budget".into()));
                        }
                        if let Err(error) = shared_charge(BudgetCharge {
                            bytes: field.len(),
                            values: 1,
                            work: 1,
                            ..Default::default()
                        }) {
                            owner_failure = Some(error.code);
                            return Err(Error(
                                "relationship mask operation budget exhausted".into(),
                            ));
                        }
                        target.insert(field.clone());
                    }
                }
                for field in &selection.provenance.fields {
                    if !visibility.provenance.fields.contains(field) {
                        field_bytes = field_bytes.saturating_add(field.len() + 4);
                        if field_bytes > MAX_BYTES {
                            owner_failure = Some(ScopeErrorCode::ByteBudget);
                            return Err(Error("provenance mask expansion exceeds budget".into()));
                        }
                        if let Err(error) = shared_charge(BudgetCharge {
                            bytes: field.len(),
                            values: 1,
                            work: 1,
                            ..Default::default()
                        }) {
                            owner_failure = Some(error.code);
                            return Err(Error("provenance operation budget exhausted".into()));
                        }
                        visibility.provenance.fields.insert(field.clone());
                    }
                }
                if selection.membership_read
                    && !visibility.memberships.contains(&selection.query.id)
                {
                    field_bytes = field_bytes.saturating_add(selection.query.id.len() + 4);
                    if field_bytes > MAX_BYTES {
                        owner_failure = Some(ScopeErrorCode::ByteBudget);
                        return Err(Error("membership label expansion exceeds budget".into()));
                    }
                    if let Err(error) = shared_charge(BudgetCharge {
                        bytes: selection.query.id.len(),
                        values: 1,
                        work: 1,
                        ..Default::default()
                    }) {
                        owner_failure = Some(error.code);
                        return Err(Error("membership label operation budget exhausted".into()));
                    }
                    visibility.memberships.insert(selection.query.id.clone());
                }
                for (role, allowed) in &selection.file_facts.by_role {
                    let target = visibility
                        .file_facts
                        .by_role
                        .entry(role.clone())
                        .or_default();
                    field_bytes = field_bytes.saturating_add(role.len() + allowed.len() * 32 + 4);
                    if field_bytes > MAX_BYTES {
                        owner_failure = Some(ScopeErrorCode::ByteBudget);
                        return Err(Error("file fact mask expansion exceeds budget".into()));
                    }
                    if let Err(error) = shared_charge(BudgetCharge {
                        bytes: role.len() + allowed.len() * 32,
                        values: allowed.len() + 1,
                        work: allowed.len() + 1,
                        ..Default::default()
                    }) {
                        owner_failure = Some(error.code);
                        return Err(Error("file fact mask operation budget exhausted".into()));
                    }
                    target.extend(allowed.iter().copied());
                }
                for (role, kinds) in &selection.components.by_role {
                    for (kind, allowed) in kinds {
                        let target = visibility
                            .components
                            .by_role
                            .entry(role.clone())
                            .or_default()
                            .entry(*kind)
                            .or_default();
                        let bytes = role.len() + allowed.len() * 32 + 16;
                        field_bytes = field_bytes.saturating_add(bytes);
                        if field_bytes > MAX_BYTES {
                            owner_failure = Some(ScopeErrorCode::ByteBudget);
                            return Err(Error("component mask expansion exceeds budget".into()));
                        }
                        if let Err(error) = shared_charge(BudgetCharge {
                            bytes,
                            values: allowed.len() + 1,
                            work: allowed.len() + 1,
                            ..Default::default()
                        }) {
                            owner_failure = Some(error.code);
                            return Err(Error("component mask operation budget exhausted".into()));
                        }
                        target.extend(allowed.iter().copied());
                    }
                }
                let wanted = &mut visibility.metadata;
                for field in &selection.fields {
                    if !wanted.contains(field) {
                        field_bytes = field_bytes
                            .saturating_add(encode(&json!(field)).len())
                            .saturating_add(2);
                        if field_bytes > MAX_BYTES {
                            owner_failure = Some(ScopeErrorCode::ByteBudget);
                            return Err(Error("host field mask expansion exceeds budget".into()));
                        }
                        if let Err(error) = shared_charge(BudgetCharge {
                            bytes: field.len(),
                            values: 1,
                            work: 1,
                            ..Default::default()
                        }) {
                            owner_failure = Some(error.code);
                            return Err(Error("operation field mask budget exhausted".into()));
                        }
                        wanted.insert(field.clone());
                    }
                }
            }
            evaluation.account_value(&json!(fields))?;
        }
        Ok(())
    });
    result.map_err(|_| {
        fail(owner_failure.unwrap_or_else(|| evaluation_code(evaluation.failure())))
    })?;
    Ok(fields)
}
fn materialize(
    store: &Store,
    fields: &BTreeMap<String, MemberVisibility>,
    started: Instant,
    budget: ReadBudget,
    poisoned: &Cell<bool>,
) -> Result<()> {
    let db = &store.db;
    let work = OperationBudget::current().ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
    let vm = search::OwnedVm::install(db, &work, poisoned)?;
    let result = (|| {
        // The only public backing table contains sanitized values. Every public
        // frozen view with a main-qualified source remains denied by the SQL owner.
        db.pragma_update(None, "query_only", false)?;
        db.execute_batch("DROP VIEW IF EXISTS temp.catalog_items; CREATE TEMP TABLE items(id TEXT PRIMARY KEY,kind TEXT NOT NULL,metadata TEXT NOT NULL); CREATE TEMP VIEW catalog_items AS SELECT id AS item_id,kind,catalog_title(metadata) AS title,catalog_year(metadata) AS year,metadata FROM temp.items;")?;
        let mut total = 0usize;
        for (id, mask) in fields {
            remaining(started, budget)?;
            let length: Option<i64> = db
                .query_row(
                    "SELECT length(CAST(metadata AS BLOB)) FROM main.items WHERE id=?",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(length) = length {
                if length < 0 || length > MAX_DEFINITION_BYTES as i64 {
                    return Err(fail(ScopeErrorCode::ByteBudget));
                }
                shared_charge(BudgetCharge {
                    items: 1,
                    bytes: length as usize,
                    values: 1,
                    work: 1,
                })?;
            }
            let row: Option<(String, String, i64)> = db.query_row(
            "SELECT kind,CASE WHEN length(CAST(metadata AS BLOB))<=? THEN metadata ELSE '' END,length(CAST(metadata AS BLOB)) FROM main.items WHERE id=?",
            params![MAX_DEFINITION_BYTES as i64, id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let Some((kind, raw, bytes)) = row else {
                continue;
            };
            // Charge raw bytes before decoding, even if a field mask would hide them.
            total = total
                .saturating_add(usize::try_from(bytes).unwrap_or(usize::MAX))
                .saturating_add(id.len())
                .saturating_add(kind.len());
            if bytes < 0 || bytes > MAX_DEFINITION_BYTES as i64 || total > MAX_BYTES {
                return Err(fail(ScopeErrorCode::ByteBudget));
            }
            let metadata = catabolic_core::decode_json(&raw)?;
            let masked: serde_json::Map<String, Value> = mask
                .metadata
                .iter()
                .filter_map(|key| metadata.get(key).map(|v| (key.clone(), v.clone())))
                .collect();
            db.execute(
                "INSERT INTO temp.items VALUES(?,?,?)",
                params![id, kind, encode(&json!(masked))],
            )?;
        }
        relations::materialize(store, fields, started, budget)?;
        db.pragma_update(None, "query_only", true)?;
        Ok(())
    })();
    vm.finish()?;
    if let Some(cause) = work.failure() {
        return Err(fail(evaluation_code(Some(cause))));
    }
    result
}

fn read_materialized(
    store: &Store,
    cap: &Capability,
    request: PreparedReadRequest,
    started: Instant,
    budget: ReadBudget,
) -> Result<Value> {
    let request = match request {
        PreparedReadRequest::Relationships(request) => {
            return relations::read(store, cap, request, started, budget);
        }
        request => request,
    };
    let mut left = remaining(started, budget)?;
    let mut page_limit = None;
    let (sql, bound, limit, item) = match request {
        PreparedReadRequest::Other(ScopeReadRequest::GetItem(id)) => {
            if id.is_empty() || id.len() > 1024 {
                return Err(fail(ScopeErrorCode::InvalidRequest));
            }
            (
                "SELECT id,kind,metadata FROM temp.items WHERE id=:id".into(),
                json!({"id":id}),
                1,
                true,
            )
        }
        PreparedReadRequest::ItemsPage { position, limit } => {
            page_limit = Some(limit);
            let after = position.map(|p| p.after);
            ("SELECT id,kind,metadata,(SELECT count(*) FROM temp.items) AS scoped_total FROM temp.items WHERE (:after IS NULL OR id COLLATE BINARY > :after) ORDER BY id COLLATE BINARY LIMIT :limit".into(),
             json!({"limit":limit+1,"after":after}), limit+1, false)
        }
        PreparedReadRequest::Other(ScopeReadRequest::SavedReport {
            query_id,
            maximum_rows,
        }) => {
            if !(1..=1000).contains(&maximum_rows) {
                return Err(fail(ScopeErrorCode::InvalidRequest));
            }
            let reference = cap
                .reports
                .get(&query_id)
                .ok_or_else(|| fail(ScopeErrorCode::NotFoundOrNotVisible))?;
            let definition = query(store, &cap.profile, reference)?;
            let selection = &definition["selection"];
            left = left
                .min(definition["timeout_ms"].as_u64().unwrap_or(5000))
                .min(selection["timeout_ms"].as_u64().unwrap_or(5000));
            (
                selection["query"]
                    .as_str()
                    .ok_or_else(|| fail(ScopeErrorCode::Unsupported))?
                    .to_owned(),
                selection.get("params").cloned().unwrap_or(json!({})),
                maximum_rows,
                false,
            )
        }
        PreparedReadRequest::Other(
            ScopeReadRequest::ItemsPage { .. }
            | ScopeReadRequest::RelationshipsPage { .. }
            | ScopeReadRequest::GetMemberships(_)
            | ScopeReadRequest::GetMetadataProvenance(_)
            | ScopeReadRequest::GetFileFacts(_)
            | ScopeReadRequest::GetComponents(_),
        )
        | PreparedReadRequest::Relationships(_)
        | PreparedReadRequest::Detail(_) => {
            return Err(fail(ScopeErrorCode::InvalidRequest));
        }
    };
    let bound = encode(&bound);
    let failure = Cell::new(None);
    let output = sql::execute_materialized_scope(
        store,
        &sql,
        &sql::Options {
            profile: &cap.profile,
            params: Some(&bound),
            max_rows: limit,
            timeout_ms: left,
            stable: true,
            http: true,
            ..Default::default()
        },
        &failure,
    );
    let code = match failure.get() {
        Some(sql::ExecutionFailure::RowBudget) => Some(ScopeErrorCode::IdBudget),
        Some(sql::ExecutionFailure::WorkBudget) => Some(ScopeErrorCode::WorkBudget),
        Some(sql::ExecutionFailure::ByteBudget) => Some(ScopeErrorCode::ByteBudget),
        Some(sql::ExecutionFailure::Timeout) => Some(ScopeErrorCode::Timeout),
        Some(sql::ExecutionFailure::Cancelled) => Some(ScopeErrorCode::Cancelled),
        None => None,
    };
    if let Some(code) = code {
        return Err(fail(code));
    }
    let mut output = output.map_err(|_| fail(ScopeErrorCode::QueryRejected))?;
    remaining(started, budget)?;
    if item && output["row_count"] == 0 {
        return Err(fail(ScopeErrorCode::NotFoundOrNotVisible));
    }
    let pagination = if let Some(limit) = page_limit {
        let rows = output["rows"]
            .as_array_mut()
            .ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
        let has_more = rows.len() > limit;
        if has_more {
            rows.pop();
        }
        let next_cursor = if has_more {
            let after = rows
                .last()
                .and_then(|r| r[0].as_str())
                .ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
            Some(next_item_cursor(cap, limit, after)?)
        } else {
            None
        };
        let count = rows.len();
        output["row_count"] = json!(count);
        output["max_rows"] = json!(limit);
        Some((has_more, next_cursor))
    } else {
        None
    };
    let mut value = json!({"scope":cap.fingerprint,"result":output});
    if let Some((has_more, next_cursor)) = pagination {
        value["has_more"] = json!(has_more);
        value["next_cursor"] = json!(next_cursor);
        let total: i64 = store
            .db
            .query_row("SELECT count(*) FROM temp.items", [], |row| row.get(0))?;
        value["scoped_total"] = json!(total);
    }
    let bytes = encode(&value).len();
    shared_charge(BudgetCharge {
        bytes,
        values: 1,
        ..Default::default()
    })?;
    if bytes > MAX_BYTES {
        return Err(fail(ScopeErrorCode::ByteBudget));
    }
    remaining(started, budget)?;
    Ok(value)
}

#[cfg(test)]
#[path = "catalog_scope_tests.rs"]
mod tests;

#[path = "catalog_scope_search.rs"]
mod search;
pub use search::ScopedSearchRequest;

#[path = "catalog_scope_relations.rs"]
mod relations;
pub use relations::TrustedRelationshipRead;

#[path = "catalog_scope_details.rs"]
mod details;
pub use details::TrustedProvenanceRead;

#[path = "catalog_scope_physical.rs"]
mod physical;
pub use physical::{FileFactField, TrustedFileFactsRead};

#[path = "catalog_scope_components.rs"]
mod component_reads;
pub use component_reads::{ComponentField, ComponentKind, TrustedComponentsRead};

mod library_selection;
pub(crate) use library_selection::{equal_saved_library_reference, validate_library_selection_in};
