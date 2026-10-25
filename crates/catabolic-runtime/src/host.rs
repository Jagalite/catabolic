// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Versioned, synchronous, privileged native host boundary.
//!
//! Run on a host-owned blocking thread. This is an operator capability, not an
//! application-user authorization API: R4 supplies scoped admission separately.
//! Store access, arbitrary SQL and caller-controlled transactions are deliberately
//! absent. Calls reuse the same native session and never upgrade on open.
mod backup;
mod edition;
mod exact_source;
pub use exact_source::ExactSource;
mod generated;
mod lineage;
mod logical;
mod metadata;
mod named;
mod nfo_inventory;
#[cfg(test)]
mod observation_handoff_tests;
mod relationships;
mod selected;
mod source_event_bridge;
mod source_preparation;
use catabolic_core::catalog_host_lease::{AuthorizationLease, HostAuthorization};
use catabolic_store::embedding::Session;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Minted only after this retained owner actually receives an older-generation
/// refusal. It cannot be deserialized or constructed by embedding callers.
#[derive(Debug, serde::Serialize)]
pub struct StaleGenerationWitness {
    envelope: String,
    current_generation: i64,
}
impl StaleGenerationWitness {
    pub fn envelope(&self) -> &str {
        &self.envelope
    }
    pub fn current_generation(&self) -> i64 {
        self.current_generation
    }
}

pub const CONTRACT_VERSION: u32 = 1;
/// Explicit restart-admission contract; ordinary open_authorized does not infer
/// or enforce a persisted import handoff.
pub const IMPORT_RESTART_CONTRACT_VERSION: u32 = 1;
/// Reconstituted only from the trusted host's durable intent/checkpoint. No
/// caller-provided Catabolic receipt is accepted as evidence of activation.
#[derive(Clone, Debug)]
pub struct RecordedImportHandshake {
    pub activation_intent: catabolic_core::catalog_command::CommandEnvelope,
    pub checkpoint: catabolic_store::catalog_import::HostCutoverCheckpoint,
}
/// All partitions are independently bounded by the store import contract. The
/// host supplies this complete set from its durable frozen-source manifest, not
/// from an untrusted request or whichever receipts happen to be discoverable.
pub const PARTITIONED_IMPORT_RESTART_CONTRACT_VERSION: u32 = 2;
pub const MAX_IMPORT_RESTART_PARTITIONS: usize = 4096;
#[derive(Clone, Debug)]
pub struct RecordedImportSet {
    manifest_digest: String,
    partitions: Vec<RecordedImportHandshake>,
}
impl RecordedImportSet {
    pub fn from_durable_manifest(
        manifest_digest: String,
        expected_import_ids: &std::collections::BTreeSet<String>,
        partitions: Vec<RecordedImportHandshake>,
    ) -> Result<Self> {
        if manifest_digest.len() != 64
            || !manifest_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || partitions.is_empty()
            || partitions.len() > MAX_IMPORT_RESTART_PARTITIONS
            || expected_import_ids.len() != partitions.len()
        {
            return Err(invalid("invalid complete import partition manifest"));
        }
        let first = &partitions[0].checkpoint;
        let mut actual = std::collections::BTreeSet::new();
        for partition in &partitions {
            let c = &partition.checkpoint;
            if c.catalog_id != first.catalog_id
                || c.incarnation != first.incarnation
                || !actual.insert(c.import_id.clone())
                || partition.activation_intent.key().catalog_id() != c.catalog_id
                || partition.activation_intent.admission_incarnation() != c.incarnation
            {
                return Err(invalid("duplicate or nonuniform import partition identity"));
            }
        }
        if &actual != expected_import_ids {
            return Err(invalid("missing or unexpected import partition"));
        }
        Ok(Self {
            manifest_digest,
            partitions,
        })
    }
    pub fn manifest_digest(&self) -> &str {
        &self.manifest_digest
    }
    pub fn partition_count(&self) -> usize {
        self.partitions.len()
    }
}
/// A closed outcome contains no HostSession or authorization gate. Admission
/// exposes the same existing host API only after durable-owner reconciliation.
pub struct ImportRestartAdmission {
    decision: catabolic_store::catalog_import::Reconciliation,
    host: Option<HostSession>,
}
impl ImportRestartAdmission {
    pub fn decision(&self) -> catabolic_store::catalog_import::Reconciliation {
        self.decision
    }
    pub fn admitted_host(&mut self) -> Result<&mut HostSession> {
        self.host.as_mut().ok_or_else(|| {
            error(
                ErrorCode::RecoveryRequired,
                Retry::Reconcile,
                "import handoff disagreement keeps host admission closed",
            )
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCode {
    InvalidInput,
    UnsupportedCapability,
    UnsupportedSchema,
    BudgetExhausted,
    MissingTool,
    NotFound,
    Conflict,
    Busy,
    Cancelled,
    Timeout,
    RecoveryRequired,
    Internal,
}
impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::UnsupportedCapability => "unsupported_capability",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::BudgetExhausted => "budget_exhausted",
            Self::MissingTool => "missing_tool",
            Self::NotFound => "not_found_or_not_visible",
            Self::Conflict => "conflict",
            Self::Busy => "busy",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::RecoveryRequired => "recovery_required",
            Self::Internal => "internal_failure",
        }
    }
}
/// No error automatically authorizes a mutation retry with a new operation ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Retry {
    CorrectRequest,
    RetryRead,
    Reconcile,
    Reopen,
    Recover,
    DoNotRetry,
}
#[derive(Debug)]
pub struct HostError {
    pub code: ErrorCode,
    pub retry: Retry,
    pub detail: &'static str,
    diagnostic: Option<catabolic_store::Error>,
}
impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.detail)
    }
}
impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.diagnostic
            .as_ref()
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}
pub type Result<T> = std::result::Result<T, HostError>;
fn error(code: ErrorCode, retry: Retry, detail: &'static str) -> HostError {
    HostError {
        code,
        retry,
        detail,
        diagnostic: None,
    }
}
fn internal(cause: catabolic_store::Error) -> HostError {
    // Diagnostic source is for trusted operator logs, never a response detail.
    HostError {
        code: ErrorCode::Internal,
        retry: Retry::Reconcile,
        detail: "native owner failed; inspect operator diagnostics before retry",
        diagnostic: Some(cause),
    }
}
fn invalid(detail: &'static str) -> HostError {
    error(ErrorCode::InvalidInput, Retry::CorrectRequest, detail)
}

/// Opaque saved definition identity. Constructed input is validated at admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryId(String);
impl QueryId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > 1024 || value.contains('\0') {
            return Err(invalid("query ID must be nonempty and at most 1024 bytes"));
        }
        Ok(Self(value))
    }
}
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ReadBudget {
    pub timeout: Duration,
    pub maximum_ids: usize,
}
impl ReadBudget {
    fn validate(self) -> Result<u64> {
        let millis = self.timeout.as_millis();
        if self.timeout < Duration::from_millis(1)
            || self.timeout > Duration::from_secs(60)
            || !(1..=100_000).contains(&self.maximum_ids)
        {
            return Err(invalid(
                "read budget requires 1..60000 milliseconds and 1..100000 IDs",
            ));
        }
        Ok(millis as u64)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lifecycle {
    Open,
    Draining,
    Closed,
}

/// Scan admission is selected by catalog schema, never by caller preference.
/// Prior schemas retain only their historical local-child supervision contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanAdmissionContract {
    RetainedChildV1,
    ExactCleanupV2,
}
/// Sessions are serialized by &mut self; no async operation holds a transaction.
/// The host must restrict possession of this privileged handle to trusted code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanHandle(String);

pub struct HostSession {
    session: Session,
    authorization: Option<HostAuthorization>,
    profile: String,
    writable: bool,
    state: Lifecycle,
    helper_ready: bool,
    worker: Option<(
        ScanHandle,
        crate::observation::SupervisedWorker<Box<dyn Send>>,
    )>,
    detached: bool,
    unavailable: bool,
}
impl HostSession {
    /// Frozen schema32 operator entrypoint. Native application hosts must pass
    /// the actual shared authorization gate to open_authorized.
    pub fn open(path: &Path, profile: &str, writable: bool, contract_version: u32) -> Result<Self> {
        Self::open_inner(path, profile, writable, contract_version, None)
    }
    /// The caller supplies its existing trusted gate, shared with scoped reads,
    /// private writes and import activation. Equal strings are never a substitute.
    pub fn open_authorized(
        path: &Path,
        profile: &str,
        writable: bool,
        contract_version: u32,
        authorization: HostAuthorization,
    ) -> Result<Self> {
        let _admission = authorization
            .acquire(authorization.revision())
            .map_err(|_| {
                error(
                    ErrorCode::Conflict,
                    Retry::DoNotRetry,
                    "host authorization is closed or unavailable",
                )
            })?;
        Self::open_inner(
            path,
            profile,
            writable,
            contract_version,
            Some(authorization),
        )
    }
    /// Versioned import restart entrypoint. Hosts resuming the import contract
    /// must use this factory, not ordinary open_authorized. It reads the actual
    /// receipt through the durable owner with new commands forcibly disabled,
    /// then creates a fresh common gate only after both recorded phases agree.
    /// The native writer lifetime is retained from checking through admission.
    pub fn open_after_import_cutover(
        path: &Path,
        profile: &str,
        restart_contract_version: u32,
        recorded: &RecordedImportHandshake,
        current_authority: catabolic_store::catalog_commands::TrustedCommandAuthority<'_>,
        next_authorization_revision: String,
        receipt_policy: catabolic_store::catalog_commands::ReceiptPolicy,
    ) -> Result<ImportRestartAdmission> {
        if restart_contract_version != IMPORT_RESTART_CONTRACT_VERSION {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                Retry::CorrectRequest,
                "unsupported import restart contract version",
            ));
        }
        if profile.is_empty() || profile.len() > 1024 || profile.contains('\0') {
            return Err(invalid("invalid restart profile"));
        }
        let mut session =
            Session::open_typed(path, true, false).map_err(|cause| internal(cause.into()))?;
        let store = session.store().map_err(internal)?;
        if !matches!(store.schema_version, 34..=40) {
            return Err(error(
                ErrorCode::UnsupportedSchema,
                Retry::DoNotRetry,
                "import restart is qualified only for native34 through native40",
            ));
        }
        let decision = catabolic_store::catalog_import::reconcile_persisted_activation(
            store,
            &recorded.activation_intent,
            &recorded.checkpoint,
            current_authority,
            receipt_policy,
        )
        .map_err(|cause| HostError {
            code: ErrorCode::RecoveryRequired,
            retry: Retry::Reconcile,
            detail: "durable import reconciliation did not admit this host",
            diagnostic: Some(cause.error),
        })?;
        if decision != catabolic_store::catalog_import::Reconciliation::AdmitCatabolicOnly {
            session.close().map_err(internal)?;
            return Ok(ImportRestartAdmission {
                decision,
                host: None,
            });
        }
        let authorization = HostAuthorization::new(
            current_authority.host_namespace.into(),
            next_authorization_revision,
        )
        .map_err(|_| invalid("invalid next authorization revision"))?;
        let host = Self::from_session(session, profile, true, Some(authorization))?;
        Ok(ImportRestartAdmission {
            decision,
            host: Some(host),
        })
    }
    /// Reconcile every manifest partition while retaining one native writer
    /// lifetime. No gate is constructed until all exact receipts admit native
    /// ownership. A partial/expired/restore-ambiguous set remains closed.
    pub fn open_after_import_cutovers(
        path: &Path,
        profile: &str,
        restart_contract_version: u32,
        recorded: &RecordedImportSet,
        expected_manifest_digest: &str,
        current_authority: catabolic_store::catalog_commands::TrustedCommandAuthority<'_>,
        next_authorization_revision: String,
        receipt_policy: catabolic_store::catalog_commands::ReceiptPolicy,
    ) -> Result<ImportRestartAdmission> {
        use catabolic_store::catalog_import::Reconciliation;
        if restart_contract_version != PARTITIONED_IMPORT_RESTART_CONTRACT_VERSION {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                Retry::CorrectRequest,
                "unsupported partitioned import restart contract version",
            ));
        }
        if recorded.manifest_digest != expected_manifest_digest
            || profile.is_empty()
            || profile.len() > 1024
            || profile.contains('\0')
        {
            return Err(invalid("partition manifest identity or profile mismatch"));
        }
        let mut session =
            Session::open_typed(path, true, false).map_err(|cause| internal(cause.into()))?;
        let store = session.store().map_err(internal)?;
        if !matches!(store.schema_version, 34..=40) {
            return Err(error(
                ErrorCode::UnsupportedSchema,
                Retry::DoNotRetry,
                "partitioned import restart requires native34 through native40",
            ));
        }
        let mut decision = Reconciliation::AdmitCatabolicOnly;
        for partition in &recorded.partitions {
            let next = catabolic_store::catalog_import::reconcile_persisted_activation(
                store,
                &partition.activation_intent,
                &partition.checkpoint,
                current_authority,
                receipt_policy,
            )
            .map_err(|cause| HostError {
                code: ErrorCode::RecoveryRequired,
                retry: Retry::Reconcile,
                detail: "partition receipt reconciliation keeps admission closed",
                diagnostic: Some(cause.error),
            })?;
            decision = match (decision, next) {
                (Reconciliation::KeepAdmissionClosed, _)
                | (_, Reconciliation::KeepAdmissionClosed) => Reconciliation::KeepAdmissionClosed,
                (Reconciliation::FinishHostActivation, _)
                | (_, Reconciliation::FinishHostActivation) => Reconciliation::FinishHostActivation,
                _ => Reconciliation::AdmitCatabolicOnly,
            };
        }
        if decision != Reconciliation::AdmitCatabolicOnly {
            session.close().map_err(internal)?;
            return Ok(ImportRestartAdmission {
                decision,
                host: None,
            });
        }
        let authorization = HostAuthorization::new(
            current_authority.host_namespace.into(),
            next_authorization_revision,
        )
        .map_err(|_| invalid("invalid partition restart authorization revision"))?;
        let host = Self::from_session(session, profile, true, Some(authorization))?;
        Ok(ImportRestartAdmission {
            decision,
            host: Some(host),
        })
    }
    fn open_inner(
        path: &Path,
        profile: &str,
        writable: bool,
        contract_version: u32,
        authorization: Option<HostAuthorization>,
    ) -> Result<Self> {
        if contract_version != CONTRACT_VERSION {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                Retry::CorrectRequest,
                "unsupported host contract version",
            ));
        }
        if profile.is_empty() || profile.len() > 1024 || profile.contains('\0') {
            return Err(invalid("invalid profile"));
        }
        let session = Session::open_typed(path, writable, false).map_err(|failure| {
            use catabolic_store::OpenErrorCode;
            match failure.code {
                OpenErrorCode::UnsupportedSchema => error(
                    ErrorCode::UnsupportedSchema,
                    Retry::CorrectRequest,
                    "catalog schema requires an explicitly supported upgrade",
                ),
                OpenErrorCode::BusyWriter => {
                    error(ErrorCode::Busy, Retry::Reopen, "catalog writer is busy")
                }
                OpenErrorCode::InvalidCatalog => error(
                    ErrorCode::InvalidInput,
                    Retry::CorrectRequest,
                    "catalog is missing or invalid",
                ),
                OpenErrorCode::Internal => internal(failure.error),
            }
        })?;
        Self::from_session(session, profile, writable, authorization)
    }
    fn from_session(
        session: Session,
        profile: &str,
        writable: bool,
        authorization: Option<HostAuthorization>,
    ) -> Result<Self> {
        if catabolic_store::migration::is_supported_native_schema(
            session.store().map_err(internal)?.schema_version,
        ) && authorization.is_none()
        {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                Retry::CorrectRequest,
                "native host requires the existing shared HostAuthorization gate",
            ));
        }
        let profile_exists = session
            .store()
            .map_err(internal)?
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM profiles WHERE id=?)",
                [profile],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|cause| internal(cause.into()))?;
        if !profile_exists {
            return Err(error(
                ErrorCode::NotFound,
                Retry::CorrectRequest,
                "profile is unavailable",
            ));
        }
        Ok(Self {
            session,
            authorization,
            profile: profile.into(),
            writable,
            state: Lifecycle::Open,
            helper_ready: false,
            worker: None,
            detached: false,
            unavailable: false,
        })
    }
    /// Clone the actual trusted gate for other native owners; never reconstruct
    /// it from namespace/revision strings. This does not acquire a new lease.
    pub fn authorization(&self) -> Result<HostAuthorization> {
        self.authorization.clone().ok_or_else(|| {
            error(
                ErrorCode::UnsupportedCapability,
                Retry::CorrectRequest,
                "legacy operator session has no shared authorization gate",
            )
        })
    }
    fn acquire_authorization(&self) -> Result<Option<AuthorizationLease>> {
        self.authorization
            .as_ref()
            .map(|host| {
                host.acquire(host.revision()).map_err(|_| {
                    error(
                        ErrorCode::Conflict,
                        Retry::DoNotRetry,
                        "host authorization is closed or unavailable",
                    )
                })
            })
            .transpose()
    }
    pub fn lifecycle(&self) -> Lifecycle {
        self.state
    }
    pub fn identity(&self) -> Result<Value> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        Ok(
            json!({"contract_version":CONTRACT_VERSION,"catalog_id":store.database_id,"schema_version":store.schema_version,"catalog_incarnation":store.catalog_incarnation,"profile":self.profile}),
        )
    }
    /// Capture descriptive current library facts through the existing writer host.
    /// Read-only composition is not admitted by this fixed adapter.
    pub fn library_scope(
        &self,
        library: &str,
        reference: &catabolic_store::catalog_scope::SavedQueryRef,
        sources: &[catabolic_store::catalog_source_succession::LibraryScopeSource],
    ) -> Result<catabolic_store::catalog_source_succession::LibraryScopeSnapshot> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        if !self.writable {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                Retry::CorrectRequest,
                "fixed library scope requires the existing writer host",
            ));
        }
        catabolic_store::catalog_source_succession::capture_host_library_scope(
            self.session.store().map_err(internal)?,
            &self.profile,
            library,
            reference,
            sources,
        )
        .map_err(internal)
    }
    /// Inspect a source binding through this exact authorized host owner. A host
    /// adapter must not inspect a second database connection and assume it is the
    /// same catalog incarnation or profile as the session executing a scan.
    pub fn source_binding(&self, source: &str) -> Result<Value> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        catabolic_store::query::name(source).map_err(|_| invalid("invalid source name"))?;
        catabolic_store::observation::binding(
            self.session.store().map_err(internal)?,
            &self.profile,
            "source",
            source,
        )
        .map_err(internal)
    }
    /// Read effective inventory scope through the same authorized source owner.
    pub fn source_observation_policy(&self, source: &str) -> Result<Value> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        catabolic_store::query::name(source).map_err(|_| invalid("invalid source name"))?;
        catabolic_store::observation::execute(
            self.session.store().map_err(internal)?,
            &self.profile,
            "scan_policy",
            &json!({"source":source}),
        )
        .map_err(internal)
    }
    /// Current physical observations through this retained host, never a second
    /// connection. The native owner returns one exact transactional snapshot.
    pub fn source_occurrences(&self, source: &str) -> Result<Value> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        catabolic_store::catalog_import_typed::capture_source_occurrences(
            self.session.store().map_err(internal)?,
            &self.profile,
            source,
        )
        .map_err(internal)
    }
    /// Register an explicitly host-assigned source with exact immutable root
    /// and inventory scope through this retained owner's shared authorization.
    pub fn register_source(
        &mut self,
        source: &str,
        root: &Path,
        exclusions: &[String],
    ) -> Result<Value> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        catabolic_store::catalog_import_typed::register_host_source(
            self.session.store().map_err(internal)?,
            &self.profile,
            source,
            root,
            exclusions,
        )
        .map_err(internal)
    }
    fn require_open(&self) -> Result<()> {
        if self.unavailable {
            return Err(error(
                ErrorCode::RecoveryRequired,
                Retry::Reopen,
                "host store is unavailable; drain its worker before reopening",
            ));
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|(_, worker)| !worker.is_finished())
        {
            return Err(error(
                ErrorCode::Busy,
                Retry::DoNotRetry,
                "scan worker is still active",
            ));
        }
        if self.state != Lifecycle::Open {
            return Err(error(
                ErrorCode::Conflict,
                Retry::Reopen,
                "host session is not admitting work",
            ));
        }
        Ok(())
    }
    /// Evaluate only a pre-existing saved selection. No raw SQL is accepted.
    /// Read-only hosts retain the original SQLite snapshot until close/reopen.
    /// Returns either the complete bounded selection or an explicit failure.
    pub fn select(
        &mut self,
        query: &QueryId,
        budget: ReadBudget,
        cancel: &Cancellation,
    ) -> Result<Value> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        let timeout_ms = budget.validate()?;
        if cancel.is_cancelled() {
            return Err(error(
                ErrorCode::Cancelled,
                Retry::RetryRead,
                "read cancelled before admission",
            ));
        }
        let started = Instant::now();
        let poll = cancel.clone();
        let mut read_failure = None;
        let result = catabolic_store::embedding::with_interrupt_check(
            move || poll.is_cancelled(),
            || {
                self.session.with_evaluation_typed(&self.profile, &json!({}), timeout_ms, budget.maximum_ids, |evaluation| {
                let selected = evaluation.select(&json!({"query_id":query.0,"profile":self.profile}))?;
                let response = json!({"contract_version":CONTRACT_VERSION,"complete":true,"entity":selected.entity,"ids":selected.ids,"report":selected.report});
                evaluation.account_value(&response)?;
                Ok(response)
            }).map_err(|failure| { read_failure = failure.failure; failure.error })
            },
        );
        // Revalidate immediately before delivery, even if the query completed.
        if cancel.is_cancelled() {
            return Err(error(
                ErrorCode::Cancelled,
                Retry::RetryRead,
                "read cancelled before delivery",
            ));
        }
        if started.elapsed() >= budget.timeout {
            return Err(error(
                ErrorCode::Timeout,
                Retry::RetryRead,
                "read deadline elapsed",
            ));
        }
        result.map_err(|cause| {
            use catabolic_store::selection::EvaluationFailure;
            match read_failure {
                Some(EvaluationFailure::MissingSavedQuery) => error(
                    ErrorCode::NotFound,
                    Retry::CorrectRequest,
                    "saved query is unavailable",
                ),
                Some(
                    EvaluationFailure::IdBudget
                    | EvaluationFailure::ByteBudget
                    | EvaluationFailure::WorkBudget,
                ) => error(
                    ErrorCode::BudgetExhausted,
                    Retry::CorrectRequest,
                    "read exceeded its declared resource budget",
                ),
                Some(EvaluationFailure::Timeout) => error(
                    ErrorCode::Timeout,
                    Retry::RetryRead,
                    "read deadline elapsed",
                ),
                Some(EvaluationFailure::Cancelled) => {
                    error(ErrorCode::Cancelled, Retry::RetryRead, "read was cancelled")
                }
                Some(EvaluationFailure::UnsupportedSelectionMode) => error(
                    ErrorCode::UnsupportedCapability,
                    Retry::CorrectRequest,
                    "saved query is not a supported selection",
                ),
                None => internal(cause),
            }
        })
    }
    /// Bounded legacy-v1 item mutation composition. This is not a durable R2
    /// command or a new metadata resolver. All members commit or roll back on the
    /// same existing writer, and admission refuses pending filesystem recovery.
    pub fn put_items(&mut self, items: &[Value], cancel: &Cancellation) -> Result<Vec<Value>> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        if !self.writable {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                Retry::Reopen,
                "session is read-only",
            ));
        }
        if items.is_empty() || items.len() > 256 || items.iter().any(|v| !v.is_object()) {
            return Err(invalid("item batch must contain 1..256 objects"));
        }
        let bytes: usize = items.iter().map(|v| catabolic_core::encode(v).len()).sum();
        if bytes > 1024 * 1024 {
            return Err(invalid("item batch exceeds one MiB"));
        }
        if cancel.is_cancelled() {
            return Err(error(
                ErrorCode::Cancelled,
                Retry::DoNotRetry,
                "mutation cancelled before admission",
            ));
        }
        let store = self.session.store().map_err(internal)?;
        let mut mutation_failure = None;
        let mut recovery_pending = false;
        let result = store.transaction(|db| {
            recovery_pending = db.query_row("SELECT EXISTS(SELECT 1 FROM journal)", [], |row| {
                row.get::<_, bool>(0)
            })?;
            if recovery_pending {
                return Err(catabolic_store::Error("catalog requires recovery".into()));
            }
            let mut output = Vec::with_capacity(items.len());
            for item in items {
                if cancel.is_cancelled() {
                    return Err(catabolic_store::Error("host mutation cancelled".into()));
                }
                output.push(
                    catabolic_store::curation::item_put_typed(store, item).map_err(|failure| {
                        mutation_failure = Some(failure.code);
                        failure.error
                    })?,
                );
            }
            if cancel.is_cancelled() {
                return Err(catabolic_store::Error("host mutation cancelled".into()));
            }
            Ok(output)
        });
        match result {
            Err(cause) if self.session.store().is_err() => {
                self.unavailable = true;
                self.state = Lifecycle::Draining;
                let mut failure = internal(cause);
                failure.retry = Retry::Reopen;
                Err(failure)
            }
            Ok(output) => Ok(output), // A post-commit cancellation cannot erase success.
            Err(_) if cancel.is_cancelled() => Err(error(
                ErrorCode::Cancelled,
                Retry::Reconcile,
                "mutation interrupted; reconcile before retry",
            )),
            Err(_) if recovery_pending => Err(error(
                ErrorCode::RecoveryRequired,
                Retry::Recover,
                "catalog requires recovery before mutation",
            )),
            Err(cause) => match mutation_failure {
                Some(catabolic_store::curation::ItemPutErrorCode::InvalidInput) => {
                    Err(invalid("invalid legacy-v1 item mutation"))
                }
                Some(catabolic_store::curation::ItemPutErrorCode::Conflict) => Err(error(
                    ErrorCode::Conflict,
                    Retry::CorrectRequest,
                    "item identity or kind conflicts with catalog state",
                )),
                _ => Err(internal(cause)),
            },
        }
    }
    /// Observe create-only admission facts in one native SQLite statement.
    /// The caller must persist this generation unchanged in its command; an
    /// absent item at another generation does not authorize an upsert.
    pub fn observe_item_for_create(&self, id: &str) -> Result<(bool, i64)> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        if id.is_empty() || id.len() > 256 {
            return Err(invalid("invalid item ID"));
        }
        self.session.store().map_err(internal)?.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM items WHERE id=?1), generation FROM catalog_state WHERE id=1",
            [id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(|cause| internal(cause.into()))
    }

    /// Bounded native facts used to decide which typed projection an item
    /// requires. This is a privileged host observation, not user authorization.
    /// Descendant IDs are returned only for requested work owners. `timeline_ids`
    /// is the complete native edition-owned set, including timelines without a
    /// version; a caller must not infer a default from version manifests alone.
    pub fn item_projection_shapes(
        &self,
        ids: &[String],
        allowed_metadata_fields: &[String],
    ) -> Result<Value> {
        use std::{cell::Cell, collections::BTreeSet, sync::atomic::AtomicU8};
        const MAX_FACTS: usize = 10_000;
        const MAX_BYTES: usize = 4 * 1024 * 1024;
        const MAX_VM_STEPS: usize = 10_000_000;
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        if ids.len() > MAX_FACTS
            || ids.iter().any(|id| id.is_empty() || id.len() > 256)
            || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
            || allowed_metadata_fields.len() > 64
            || allowed_metadata_fields
                .iter()
                .any(|field| field.is_empty() || field.len() > 256)
        {
            return Err(invalid("invalid item projection IDs"));
        }
        if catabolic_store::embedding::interrupt_requested() {
            return Err(error(
                ErrorCode::Cancelled,
                Retry::RetryRead,
                "item projection was cancelled",
            ));
        }
        let store = self.session.store().map_err(internal)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        // This host boundary owns the retained connection's VM hook. The store's
        // internal OperationBudget is not public; use the same deadline/VM-hook
        // mechanism here and remove it before transaction rollback on failure.
        let stopped = Arc::new(AtomicU8::new(0));
        let result = store.transaction(|db| {
            let stop = stopped.clone();
            let mut steps = 0usize;
            db.progress_handler(1000, Some(move || {
                steps = steps.saturating_add(1000);
                let reason = if catabolic_store::embedding::interrupt_requested() {
                    3
                } else if Instant::now() >= deadline {
                    2
                } else if steps > MAX_VM_STEPS {
                    1
                } else {
                    0
                };
                if reason != 0 {
                    stop.store(reason, Ordering::Relaxed);
                }
                reason != 0
            }))?;
            struct Reset<'a>(&'a rusqlite::Connection);
            impl Drop for Reset<'_> {
                fn drop(&mut self) {
                    let _ = self.0.progress_handler(0, None::<fn() -> bool>);
                }
            }
            let _reset = Reset(db);
            let facts = Cell::new(0usize);
            let bytes = Cell::new(0usize);
            let collect = |statement: &mut rusqlite::Statement<'_>, value: &str| -> catabolic_store::Result<Vec<String>> {
                let mut output = Vec::new();
                for row in statement.query_map([value], |row| row.get::<_, String>(0))? {
                    let value = row?;
                    facts.set(facts.get().saturating_add(1));
                    bytes.set(bytes.get().saturating_add(value.len()));
                    if facts.get() > MAX_FACTS || bytes.get() > MAX_BYTES {
                        stopped.store(1, Ordering::Relaxed);
                        return Err(catabolic_store::Error("item projection descendant budget exceeded".into()));
                    }
                    if Instant::now() >= deadline {
                        stopped.store(2, Ordering::Relaxed);
                        return Err(catabolic_store::Error("item projection deadline elapsed".into()));
                    }
                    output.push(value);
                }
                Ok(output)
            };
            let generation: i64 = db.query_row("SELECT generation FROM catalog_state WHERE id=1", [], |row| row.get(0))?;
            let encoded = serde_json::to_string(ids)?;
            let allowed = serde_json::to_string(allowed_metadata_fields)?;
            let mut statement = db.prepare("SELECT i.id, EXISTS(SELECT 1 FROM identities x WHERE x.item_id=i.id), EXISTS(SELECT 1 FROM item_files a WHERE a.item_id=i.id), EXISTS(SELECT 1 FROM item_relationships r WHERE r.source_id=i.id OR r.target_id=i.id), EXISTS(SELECT 1 FROM catalog_timelines t WHERE t.edition_item_id=i.id), EXISTS(SELECT 1 FROM catalog_versions v WHERE v.association_item_id=i.id), EXISTS(SELECT 1 FROM json_each(i.metadata) j WHERE j.key NOT IN (SELECT value FROM json_each(?2))), r.revision, EXISTS(SELECT 1 FROM catalog_logical_retirements l WHERE l.item_id=i.id), EXISTS(SELECT 1 FROM item_relationships r WHERE (r.source_id=i.id OR r.target_id=i.id) AND NOT (r.source_id=i.id AND r.kind='custom:motion_edition' AND r.active=1 AND EXISTS(SELECT 1 FROM items e WHERE e.id=r.target_id AND e.kind='custom:motion_edition'))) FROM items i LEFT JOIN catalog_entity_revisions r ON r.entity_kind='items' AND r.entity_key=json_array(i.id) AND r.deleted=0 WHERE i.id IN (SELECT value FROM json_each(?1)) ORDER BY i.id")?;
            let rows = statement.query_map([encoded, allowed], |r| Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?,r.get::<_,bool>(2)?,r.get::<_,bool>(3)?,r.get::<_,bool>(4)?,r.get::<_,bool>(5)?,r.get::<_,bool>(6)?,r.get::<_,i64>(7)?,r.get::<_,bool>(8)?,r.get::<_,bool>(9)?)))?;
            let mut shapes = Vec::new();
            for row in rows {
                let (id, identities, files, relationships, timelines, versions, mut metadata, revision, retired, mut unsupported_relationships) = row?;
                if metadata && store.schema_version == 40 {
                    // User metadata is descriptive, not a new structural owner.
                    // Only the typed registry may classify these extra keys;
                    // its current projection must still equal items.metadata.
                    if let Some(public_fields) = catabolic_store::catalog_metadata_store::registered_public_projection_fields(db, &id)
                        .map_err(|e| catabolic_store::Error(e.to_string()))?
                    {
                        let raw: String = db.query_row("SELECT metadata FROM items WHERE id=?", [&id], |row| row.get(0))?;
                        bytes.set(bytes.get().saturating_add(raw.len()));
                        if bytes.get() > MAX_BYTES { return Err(catabolic_store::Error("item metadata projection byte budget exceeded".into())); }
                        let value: Value = serde_json::from_str(&raw)?;
                        let fields = value.as_object().ok_or_else(||catabolic_store::Error("managed item metadata is not an object".into()))?;
                        metadata = fields.keys().any(|key| !allowed_metadata_fields.contains(key) && !public_fields.contains(key));
                    }
                }
                if unsupported_relationships {
                    let mut extra=db.prepare("SELECT json_object('id',r.id,'source_id',r.source_id,'target_id',r.target_id,'kind',r.kind,'position',r.position,'metadata',r.metadata,'active',r.active) FROM item_relationships r WHERE (r.source_id=?1 OR r.target_id=?1) AND NOT (r.source_id=?1 AND r.kind='custom:motion_edition' AND r.active=1 AND EXISTS(SELECT 1 FROM items e WHERE e.id=r.target_id AND e.kind='custom:motion_edition')) ORDER BY r.id LIMIT 10001")?;
                    let extra=collect(&mut extra,&id)?;
                    unsupported_relationships=false;
                    for raw in extra {
                        let edge:Value=serde_json::from_str(&raw)?;
                        if catabolic_store::catalog_work_relationship::decode_record(&edge)?.is_none() {
                            unsupported_relationships=true; break;
                        }
                        for endpoint in [edge["source_id"].as_str(),edge["target_id"].as_str()] {
                            let endpoint=endpoint.ok_or_else(||catabolic_store::Error("work edge endpoint missing".into()))?;
                            let supported:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM items i WHERE i.id=? AND i.kind IN ('video','custom:motion_audio') AND NOT EXISTS(SELECT 1 FROM catalog_logical_retirements r WHERE r.item_id=i.id))",[endpoint],|r|r.get(0))?;
                            unsupported_relationships|=!supported;
                        }
                    }
                }
                let mut editions = db.prepare("SELECT target_id FROM item_relationships WHERE source_id=? AND kind='custom:motion_edition' AND active=1 ORDER BY target_id LIMIT 10001")?;
                let edition_ids = collect(&mut editions, &id)?;
                let encoded_editions = serde_json::to_string(&edition_ids)?;
                let mut version_statement = db.prepare("SELECT DISTINCT v.target_id FROM item_relationships e JOIN item_relationships v ON v.source_id=e.target_id WHERE e.source_id=? AND e.kind='custom:motion_edition' AND e.active=1 AND v.kind='custom:motion_version' AND v.active=1 ORDER BY v.target_id LIMIT 10001")?;
                let version_ids = collect(&mut version_statement, &id)?;
                let encoded_versions = serde_json::to_string(&version_ids)?;
                let mut timeline_statement = db.prepare("SELECT id FROM catalog_timelines WHERE edition_item_id IN (SELECT value FROM json_each(?)) ORDER BY id LIMIT 10001")?;
                let timeline_ids = collect(&mut timeline_statement, &encoded_editions)?;

                // Compare exact parent/child pairs, not merely a union of IDs:
                // a version linked beneath the wrong edition must not pass.
                let mut edge_statement = db.prepare("SELECT json_array(e.target_id,v.target_id) FROM item_relationships e JOIN item_relationships v ON v.source_id=e.target_id WHERE e.source_id=? AND e.kind='custom:motion_edition' AND e.active=1 AND v.kind='custom:motion_version' AND v.active=1 ORDER BY e.target_id,v.target_id LIMIT 10001")?;
                let edge_pairs: BTreeSet<_> = collect(&mut edge_statement, &id)?.into_iter().collect();
                let mut owner_statement = db.prepare("SELECT json_array(t.edition_item_id,v.association_item_id) FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id WHERE t.edition_item_id IN (SELECT value FROM json_each(?)) AND NOT EXISTS(SELECT 1 FROM catalog_version_retirements x WHERE x.version_id=v.id) AND NOT EXISTS(SELECT 1 FROM catalog_logical_retirements x WHERE x.item_id=v.association_item_id) ORDER BY t.edition_item_id,v.association_item_id LIMIT 10001")?;
                let owner_pairs: BTreeSet<_> = collect(&mut owner_statement, &encoded_editions)?.into_iter().collect();
                let unsupported_children: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM item_relationships e JOIN item_relationships x ON x.source_id=e.target_id OR x.target_id=e.target_id WHERE e.source_id=?1 AND e.kind='custom:motion_edition' AND e.active=1 AND NOT ((x.source_id=?1 AND x.target_id=e.target_id AND x.kind='custom:motion_edition' AND x.active=1) OR (x.source_id=e.target_id AND x.kind='custom:motion_version' AND x.active=1 AND EXISTS(SELECT 1 FROM items v WHERE v.id=x.target_id AND v.kind='custom:motion_version'))))", [&id], |r| r.get(0))?;
                let unsupported_editions: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM json_each(?1) wanted LEFT JOIN items e ON e.id=wanted.value WHERE e.id IS NULL OR e.kind IS NOT 'custom:motion_edition' OR json_type(e.metadata) IS NOT 'object' OR json_type(e.metadata,'$.motion_edition') IS NOT 'object' OR json_extract(e.metadata,'$.motion_edition.contract_version') IS NOT 1 OR json_extract(e.metadata,'$.motion_edition.source_edition.item_id') IS NOT ?2 OR e.id IS NOT 'motion.edition:'||json_extract(e.metadata,'$.motion_edition.source_edition.id') OR EXISTS(SELECT 1 FROM json_each(e.metadata) j WHERE j.key!='motion_edition') OR EXISTS(SELECT 1 FROM identities x WHERE x.item_id=e.id) OR EXISTS(SELECT 1 FROM item_files a WHERE a.item_id=e.id) OR EXISTS(SELECT 1 FROM catalog_versions v WHERE v.association_item_id=e.id) OR EXISTS(SELECT 1 FROM catalog_logical_retirements l WHERE l.item_id=e.id))",
                    rusqlite::params![encoded_editions, id], |r| r.get(0),
                )?;
                let unsupported_versions: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM json_each(?1) wanted LEFT JOIN items v ON v.id=wanted.value WHERE v.id IS NULL OR v.kind IS NOT 'custom:motion_version' OR json_type(v.metadata) IS NOT 'object' OR json_type(v.metadata,'$.motion_version') IS NOT 'object' OR json_extract(v.metadata,'$.motion_version.contract_version') IS NOT 1 OR v.id IS NOT 'motion.version:'||json_extract(v.metadata,'$.motion_version.source_version.id') OR EXISTS(SELECT 1 FROM json_each(v.metadata) j WHERE j.key NOT IN ('motion_version','motion_probe','motion_generated_output')) OR (json_type(v.metadata,'$.motion_generated_output') IS NOT NULL AND (json_type(v.metadata,'$.motion_generated_output') IS NOT 'object' OR json_extract(v.metadata,'$.motion_version.source_version.origin') IS NOT 'generated')) OR EXISTS(SELECT 1 FROM identities x WHERE x.item_id=v.id) OR EXISTS(SELECT 1 FROM catalog_timelines t WHERE t.edition_item_id=v.id) OR EXISTS(SELECT 1 FROM catalog_logical_retirements l WHERE l.item_id=v.id) OR NOT EXISTS(SELECT 1 FROM catalog_versions cv WHERE cv.association_item_id=v.id AND NOT EXISTS(SELECT 1 FROM catalog_version_retirements x WHERE x.version_id=cv.id)) OR EXISTS(SELECT 1 FROM item_relationships x WHERE (x.source_id=v.id OR x.target_id=v.id) AND NOT (x.target_id=v.id AND x.kind='custom:motion_version' AND x.active=1 AND EXISTS(SELECT 1 FROM catalog_versions cv JOIN catalog_version_revisions r ON r.version_id=cv.id AND r.revision=cv.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id WHERE cv.association_item_id=v.id AND t.edition_item_id=x.source_id AND t.edition_item_id IN (SELECT value FROM json_each(?2))))) OR EXISTS(SELECT 1 FROM item_files a WHERE a.item_id=v.id AND (a.active!=1 OR a.role!='primary' OR json_type(a.metadata) IS NOT 'object' OR EXISTS(SELECT 1 FROM json_each(a.metadata)) OR NOT EXISTS(SELECT 1 FROM catalog_active_binding_revisions b WHERE b.association_item_id=v.id AND b.association_id=a.id AND b.file_id=a.file_id AND b.part=a.part AND b.profile=?3))) OR EXISTS(SELECT 1 FROM catalog_active_binding_revisions b WHERE b.association_item_id=v.id AND (b.profile!=?3 OR NOT EXISTS(SELECT 1 FROM item_files a WHERE a.id=b.association_id AND a.item_id=v.id AND a.active=1 AND a.role='primary' AND a.file_id=b.file_id AND a.part=b.part))))",
                    rusqlite::params![encoded_versions, encoded_editions, self.profile], |r| r.get(0),
                )?;
                let shape = json!({
                    "id": id, "item_revision": revision,
                    "requires_rich_projection": identities || files || relationships || timelines || versions || metadata || retired,
                    "logical_work_projection": relationships && edition_ids.is_empty() && !identities && !files && !timelines && !versions && !metadata && !retired && !unsupported_relationships,
                    "physical_work_projection": !edition_ids.is_empty() && relationships && !identities && !files && !timelines && !versions && !metadata && !retired && !unsupported_relationships && !unsupported_children && !unsupported_editions && !unsupported_versions && edge_pairs == owner_pairs,
                    "edition_ids": edition_ids, "version_ids": version_ids, "timeline_ids": timeline_ids,
                });
                bytes.set(bytes.get().saturating_add(serde_json::to_vec(&shape)?.len()));
                if bytes.get() > MAX_BYTES {
                    stopped.store(1, Ordering::Relaxed);
                    return Err(catabolic_store::Error("item projection byte budget exceeded".into()));
                }
                if Instant::now() >= deadline {
                    stopped.store(2, Ordering::Relaxed);
                    return Err(catabolic_store::Error("item projection deadline elapsed".into()));
                }
                shapes.push(shape);
            }
            let output = json!({"catalog_id":store.database_id,"incarnation":store.catalog_incarnation,"generation":generation,"items":shapes});
            if serde_json::to_vec(&output)?.len() > MAX_BYTES {
                stopped.store(1, Ordering::Relaxed);
                return Err(catabolic_store::Error("item projection byte budget exceeded".into()));
            }
            Ok(output)
        });
        match stopped.load(Ordering::Relaxed) {
            1 => Err(error(
                ErrorCode::BudgetExhausted,
                Retry::CorrectRequest,
                "item projection exceeded its resource budget",
            )),
            2 => Err(error(
                ErrorCode::Timeout,
                Retry::RetryRead,
                "item projection deadline elapsed",
            )),
            3 => Err(error(
                ErrorCode::Cancelled,
                Retry::RetryRead,
                "item projection was cancelled",
            )),
            _ => result.map_err(internal),
        }
    }

    /// Execute an already durable host intent on the retained native owner.
    /// The embedding host must establish current application authorization and
    /// receipt visibility before calling; this is not a request decoder.
    /// Disabled aggregate-issuance integration. The embedding must retain its
    /// Motion writer and this host owner through the complete membership join;
    /// a capture is neither a scoped read grant nor a playable manifest.
    pub fn capture_work_dependencies(
        &self,
        expected_generation: i64,
        work_id: &str,
    ) -> Result<catabolic_store::catalog_work_capture::CompleteWorkCapture> {
        let _lease = self.acquire_authorization()?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        catabolic_store::catalog_work_capture::capture(
            store,
            &catabolic_store::catalog_projection::TrustedProjectionAuthority {
                catalog_id: store.database_id.clone(),
                incarnation: store
                    .catalog_incarnation
                    .clone()
                    .ok_or_else(|| invalid("native incarnation missing"))?,
                expected_generation,
            },
            &self.profile,
            work_id,
        )
        .map_err(internal)
    }

    pub fn projection_unavailable_versions(
        &self,
        expected_generation: i64,
        version_ids: &[String],
    ) -> Result<Value> {
        let _lease = self.acquire_authorization()?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        catabolic_store::catalog_projection::unavailable_version_facts(
            store,
            &catabolic_store::catalog_projection::TrustedProjectionAuthority {
                catalog_id: store.database_id.clone(),
                incarnation: store
                    .catalog_incarnation
                    .clone()
                    .ok_or_else(|| invalid("native incarnation missing"))?,
                expected_generation,
            },
            &self.profile,
            version_ids,
        )
        .map_err(internal)
    }

    /// Privileged host startup reconciliation, never a mutation admission.
    pub fn reconcile_command_receipt(
        &mut self,
        command: &catabolic_core::catalog_command::CommandEnvelope,
    ) -> Result<Option<catabolic_store::catalog_commands::CommandOutcome>> {
        let lease = self.acquire_authorization()?;
        self.require_open()?;
        let lease =
            lease.ok_or_else(|| invalid("receipt reconciliation requires host authority"))?;
        if command.key().host_namespace() != lease.namespace() {
            return Err(invalid("receipt namespace differs from retained host"));
        }
        let store = self.session.store().map_err(internal)?;
        if command.key().catalog_id() != store.database_id
            || Some(command.admission_incarnation()) != store.catalog_incarnation.as_deref()
        {
            return Err(invalid(
                "receipt catalog incarnation differs from retained host",
            ));
        }
        catabolic_store::catalog_commands::reconcile_receipt(
            store,
            command,
            catabolic_store::catalog_commands::TrustedCommandAuthority {
                host_namespace: lease.namespace(),
                receipt_visibility: catabolic_core::catalog_command::ReceiptVisibility::FullResult,
                allow_new_commands: false,
                identity_authority: None,
            },
        )
        .map_err(|failure| internal(failure.error))
    }

    pub fn execute_command(
        &mut self,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        authority: catabolic_store::catalog_commands::TrustedCommandAuthority<'_>,
        policy: catabolic_store::catalog_commands::ReceiptPolicy,
    ) -> Result<catabolic_store::catalog_commands::CommandOutcome> {
        let lease = self.acquire_authorization()?;
        self.require_open()?;
        if !self.writable {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                Retry::CorrectRequest,
                "session is read-only",
            ));
        }
        let lease =
            lease.ok_or_else(|| invalid("durable commands require native host authorization"))?;
        if authority.host_namespace != lease.namespace()
            || command.key().host_namespace() != lease.namespace()
        {
            return Err(invalid(
                "command namespace differs from retained host authority",
            ));
        }
        catabolic_store::catalog_commands::execute(
            self.session.store().map_err(internal)?,
            command,
            authority,
            policy,
        )
        .map_err(|failure| internal(failure.error))
    }

    /// Dispatch plus non-forgeable evidence for a distinct durable retry key.
    /// A replay, expiry, restore ambiguity or a future expected generation never
    /// yields a witness. Persist the next intent before dispatching it.
    pub fn execute_command_observed(
        &mut self,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        authority: catabolic_store::catalog_commands::TrustedCommandAuthority<'_>,
        policy: catabolic_store::catalog_commands::ReceiptPolicy,
    ) -> Result<(
        catabolic_store::catalog_commands::CommandOutcome,
        Option<StaleGenerationWitness>,
    )> {
        let _lease = self.acquire_authorization()?;
        let outcome = self.execute_command(command, authority, policy)?;
        let witness = self.stale_generation_witness(command, &outcome)?;
        Ok((outcome, witness))
    }

    pub fn execute_catalog_command_observed(
        &mut self,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        authority: catabolic_store::catalog_commands::CatalogAuthority<'_>,
        policy: catabolic_store::catalog_commands::ReceiptPolicy,
    ) -> Result<(
        catabolic_store::catalog_commands::CommandOutcome,
        Option<StaleGenerationWitness>,
    )> {
        let _lease = self.acquire_authorization()?;
        let outcome = self.execute_catalog_command(command, authority, policy)?;
        let witness = self.stale_generation_witness(command, &outcome)?;
        Ok((outcome, witness))
    }

    fn stale_generation_witness(
        &self,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        outcome: &catabolic_store::catalog_commands::CommandOutcome,
    ) -> Result<Option<StaleGenerationWitness>> {
        if !matches!(
            outcome,
            catabolic_store::catalog_commands::CommandOutcome::StaleGeneration
        ) {
            return Ok(None);
        }
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        let generation = self.catalog_generation()?;
        if store.database_id != command.key().catalog_id()
            || store.catalog_incarnation.as_deref() != Some(command.admission_incarnation())
            || generation <= command.expected_generation()
        {
            return Ok(None);
        }
        Ok(Some(StaleGenerationWitness {
            envelope: command.canonical_payload().to_owned(),
            current_generation: generation,
        }))
    }

    /// Read one bounded page for a trusted host's disposable projection. This
    /// is not application authorization; never expose this operation as raw HTTP.
    /// Every page is fenced to the caller's unchanged retained-owner generation.
    pub fn projection_item_page(
        &self,
        expected_generation: i64,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Value> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        let authority = catabolic_store::catalog_projection::TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store
                .catalog_incarnation
                .clone()
                .ok_or_else(|| invalid("native projection required"))?,
            expected_generation,
        };
        catabolic_store::catalog_projection::item_page(store, &authority, after_id, limit)
            .map_err(internal)
    }

    /// Internal relationship facts. The application must independently retain
    /// current read authority for both endpoints before returning any row.
    pub fn projection_work_relationships(
        &self,
        expected_generation: i64,
        work_id: &str,
        limit: usize,
    ) -> Result<Value> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        let authority = catabolic_store::catalog_projection::TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store
                .catalog_incarnation
                .clone()
                .ok_or_else(|| invalid("native relationship projection required"))?,
            expected_generation,
        };
        catabolic_store::catalog_work_relationship::read_incident(store, &authority, work_id, limit)
            .map_err(internal)
    }
    pub fn projection_work_relationship(
        &self,
        expected_generation: i64,
        id: &str,
    ) -> Result<Value> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        let authority = catabolic_store::catalog_projection::TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store
                .catalog_incarnation
                .clone()
                .ok_or_else(|| invalid("native relationship projection required"))?,
            expected_generation,
        };
        catabolic_store::catalog_work_relationship::read_exact(store, &authority, id)
            .map_err(internal)
    }

    /// Internal native-authoritative version context for a disposable read
    /// model. The caller must retain its application fence and this owner.
    pub fn projection_version_manifest(
        &self,
        expected_generation: i64,
        version_ids: &[String],
    ) -> Result<Value> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        let authority = catabolic_store::catalog_projection::TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store
                .catalog_incarnation
                .clone()
                .ok_or_else(|| invalid("native projection required"))?,
            expected_generation,
        };
        catabolic_store::catalog_projection::version_manifest(
            store,
            &authority,
            &self.profile,
            version_ids,
        )
        .map_err(internal)
    }

    /// Typed durable owner; grants must be constructed from current host
    /// application authority, never deserialized from the catalog request.
    pub fn execute_catalog_command(
        &mut self,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        grants: catabolic_store::catalog_commands::CatalogAuthority<'_>,
        policy: catabolic_store::catalog_commands::ReceiptPolicy,
    ) -> Result<catabolic_store::catalog_commands::CommandOutcome> {
        let lease = self.acquire_authorization()?;
        self.require_open()?;
        if !self.writable {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                Retry::CorrectRequest,
                "session is read-only",
            ));
        }
        let lease =
            lease.ok_or_else(|| invalid("typed commands require native host authorization"))?;
        if command.key().host_namespace() != lease.namespace() {
            return Err(invalid(
                "command namespace differs from retained host authority",
            ));
        }
        catabolic_store::catalog_commands::execute_with_authority(
            self.session.store().map_err(internal)?,
            command,
            grants,
            policy,
        )
        .map_err(|failure| internal(failure.error))
    }

    /// Admit only an independently verified external completion. This does not
    /// execute a job or infer authority from the serialized command payload.
    pub fn execute_generated_output(
        &mut self,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        authority: &catabolic_store::catalog_generated_output::GeneratedOutputAuthority,
    ) -> Result<catabolic_store::catalog_commands::CommandOutcome> {
        self.execute_generated_output_observed(command, authority)
            .map(|(outcome, _)| outcome)
    }
    pub fn execute_generated_output_observed(
        &mut self,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        authority: &catabolic_store::catalog_generated_output::GeneratedOutputAuthority,
    ) -> Result<(
        catabolic_store::catalog_commands::CommandOutcome,
        Option<StaleGenerationWitness>,
    )> {
        use catabolic_store::catalog_commands::{
            CatalogAuthority, ReceiptPolicy, TrustedCommandAuthority,
        };
        let namespace = self.authorization()?.namespace().to_owned();
        if authority.host_namespace != namespace {
            return Err(invalid(
                "generated output authority belongs to another host",
            ));
        }
        self.execute_catalog_command_observed(
            command,
            CatalogAuthority::new(TrustedCommandAuthority {
                host_namespace: &namespace,
                receipt_visibility: catabolic_core::catalog_command::ReceiptVisibility::FullResult,
                allow_new_commands: true,
                identity_authority: None,
            })
            .with_generated_output(authority),
            ReceiptPolicy {
                retention: std::time::Duration::from_secs(30 * 24 * 60 * 60),
                max_receipts: 100_000,
                max_logical_bytes: 256 * 1024 * 1024,
                max_hold_records: 100_000,
            },
        )
    }

    /// Read the exact local source head without exposing unrelated metadata.
    /// The embedding caller must have current read/write scope for this subject.
    pub fn metadata_source_head(
        &self,
        subject: &str,
        namespace: &str,
        record_id: &str,
    ) -> Result<catabolic_store::catalog_metadata_store::MetadataSourceHead> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        let generation = store
            .db
            .query_row(
                "SELECT generation FROM catalog_state WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(|cause| internal(cause.into()))?;
        let authority = catabolic_store::catalog_metadata_store::MetadataReadAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store
                .catalog_incarnation
                .clone()
                .ok_or_else(|| invalid("native metadata required"))?,
            subject: subject.into(),
            expected_generation: generation,
        };
        catabolic_store::catalog_metadata_store::read_source_head(
            store, &authority, namespace, record_id,
        )
        .map_err(|failure| internal(failure.error))
    }

    /// Validate a host-retained selection reference on the same catalog owner.
    /// This checks freshness only; it does not grant application membership.
    pub fn validate_selection_reference(
        &self,
        reference: &catabolic_store::catalog_scope::SavedQueryRef,
    ) -> Result<()> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        catabolic_store::catalog_scope::validate_selection_reference(
            self.session.store().map_err(internal)?,
            &self.profile,
            reference,
        )
        .map_err(|_| {
            error(
                ErrorCode::Conflict,
                Retry::Reconcile,
                "trusted selection reference is unavailable or changed",
            )
        })
    }

    /// Append an immutable validated selection revision on the retained owner.
    /// Only trusted host-generated definitions belong here; application callers
    /// must never choose SQL, profile, or a saved-query grant.
    pub fn prepare_selection(&mut self, name: &str, definition: &Value) -> Result<Value> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        if !self.writable || self.authorization.is_none() {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                Retry::CorrectRequest,
                "selection preparation requires native writer authorization",
            ));
        }
        if definition["mode"].as_str() != Some("selection") {
            return Err(invalid(
                "host scope preparation requires a selection definition",
            ));
        }
        catabolic_store::definitions::query_put(
            self.session.store().map_err(internal)?,
            &self.profile,
            name,
            definition,
        )
        .map_err(internal)
    }

    /// Exact generation observed on the retained command owner, for recording a
    /// new durable intent. Replay must reuse the previously recorded generation.
    pub fn catalog_generation(&self) -> Result<i64> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        self.session
            .store()
            .map_err(internal)?
            .db
            .query_row(
                "SELECT generation FROM catalog_state WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(|cause| internal(cause.into()))
    }

    /// Register an exact native helper for this privileged host. Ambient PATH and
    /// the legacy environment override alone do not admit hosted scan work.
    pub fn register_helper(&mut self, path: &Path, digest: &str) -> Result<()> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(invalid("helper digest must be a lowercase SHA-256"));
        }
        crate::helper::register(path, Some(digest)).map_err(internal)?;
        self.helper_ready = true;
        Ok(())
    }
    /// Enroll a packaging-verified exact closed companion capability using this
    /// same host gate. An arbitrary helper path/digest never grants it implicitly.
    pub fn register_closed_observation_helper(
        &mut self,
        grant: &crate::helper::TrustedClosedObservationArtifact,
    ) -> Result<()> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        crate::helper::register_closed_observation(grant).map_err(internal)?;
        self.helper_ready = true;
        Ok(())
    }
    /// Report the exact scan admission contract; no API permits a schema37
    /// catalog to downgrade to the historical local-supervisor behavior.
    pub fn scan_admission_contract(&self) -> Result<ScanAdmissionContract> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        Ok(
            if self.session.store().map_err(internal)?.schema_version
                >= catabolic_store::native_observation_companion::SCHEMA_VERSION
            {
                ScanAdmissionContract::ExactCleanupV2
            } else {
                ScanAdmissionContract::RetainedChildV1
            },
        )
    }
    /// Report unresolved durable resource ownership for this exact catalog
    /// lifetime. This is evidence for refusal, never a reconstructed supervisor.
    pub fn worker_inventory(&self) -> Result<catabolic_store::native_worker_inventory::Inventory> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        catabolic_store::native_worker_inventory::inspect(self.session.store().map_err(internal)?)
            .map_err(internal)
    }
    /// Start one scan while transferring the host's resource permit into its
    /// supervisor. The permit remains held across every pending response.
    pub fn start_scan<P: Send + 'static>(&mut self, source: &str, permit: P) -> Result<ScanHandle> {
        let authorization = self.acquire_authorization()?;
        self.require_open()?;
        if !self.writable {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                Retry::Reopen,
                "scan requires a writable host",
            ));
        }
        if self.scan_admission_contract()? == ScanAdmissionContract::ExactCleanupV2
            && !self.worker_inventory()?.admits_new_work()
        {
            return Err(error(
                ErrorCode::RecoveryRequired,
                Retry::Reconcile,
                "native durable worker ownership requires exact cleanup before new scan admission",
            ));
        }
        if !self.helper_ready {
            return Err(error(
                ErrorCode::MissingTool,
                Retry::CorrectRequest,
                "register an exact native helper before scanning",
            ));
        }
        catabolic_store::query::name(source).map_err(|_| invalid("invalid source name"))?;
        let prepared = crate::observation::prepare_supervised(
            self.session.store_mut().map_err(internal)?,
            &self.profile,
            &json!({"source":source,"isolate":true}),
            Box::new((permit, authorization)) as Box<dyn Send>,
        )
        .map_err(internal)?;
        self.start_prepared(prepared)
    }
    /// Attach a new demand to another live host's exact retained scan owner.
    /// Both hosts must share the actual gate and catalog/profile. This is an
    /// explicit existing-work join, not a fresh scan or reconstructed supervisor.
    /// Unknown owners elsewhere in the inventory continue to block admission.
    pub fn join_scan_from<P: Send + 'static>(
        &mut self,
        owner: &HostSession,
        handle: &ScanHandle,
        permit: P,
    ) -> Result<ScanHandle> {
        let authorization = self.acquire_authorization()?.ok_or_else(|| {
            error(
                ErrorCode::UnsupportedCapability,
                Retry::CorrectRequest,
                "shared join requires native host authority",
            )
        })?;
        self.require_open()?;
        if !self.writable
            || self.scan_admission_contract()? != ScanAdmissionContract::ExactCleanupV2
        {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                Retry::CorrectRequest,
                "exact shared join requires schema37 writable host",
            ));
        }
        if !authorization.belongs_to(&owner.authorization()?) || self.profile != owner.profile {
            return Err(error(
                ErrorCode::Conflict,
                Retry::DoNotRetry,
                "shared join requires the same exact host authority and profile",
            ));
        }
        if owner.unavailable || owner.state != Lifecycle::Open {
            return Err(error(
                ErrorCode::RecoveryRequired,
                Retry::Reconcile,
                "shared scan owner is not admitting joined demands",
            ));
        }
        if !self.helper_ready {
            return Err(error(
                ErrorCode::MissingTool,
                Retry::CorrectRequest,
                "register the qualified closed helper before joining",
            ));
        }
        let (current, worker) = owner.worker.as_ref().ok_or_else(|| {
            error(
                ErrorCode::NotFound,
                Retry::DoNotRetry,
                "original scan owner is unavailable",
            )
        })?;
        if current != handle {
            return Err(error(
                ErrorCode::NotFound,
                Retry::DoNotRetry,
                "original scan handle is unavailable",
            ));
        }
        let shared = worker
            .shared_claim(self.session.store().map_err(internal)?)
            .map_err(|cause| {
                if cause.0 == "shared_observation_owner_not_admitted" {
                    error(
                        ErrorCode::Busy,
                        Retry::RetryRead,
                        "original scan has not admitted its durable demand yet",
                    )
                } else {
                    internal(cause)
                }
            })?;
        let inventory = self.worker_inventory()?;
        if inventory.truncated
            || inventory.unresolved.iter().any(|entry| {
                !(entry.kind == "observation" && entry.id == shared.owned.job_id
                    || entry.kind == "observation_companion"
                        && entry.id == shared.reservation_nonce)
            })
        {
            return Err(error(
                ErrorCode::RecoveryRequired,
                Retry::Reconcile,
                "unrelated unresolved worker ownership blocks shared join",
            ));
        }
        let prepared = crate::observation::prepare_shared_supervised(
            self.session.store_mut().map_err(internal)?,
            &self.profile,
            worker,
            Box::new((permit, Some(authorization))) as Box<dyn Send>,
        )
        .map_err(internal)?;
        self.start_prepared(prepared)
    }
    fn start_prepared(
        &mut self,
        prepared: crate::observation::PreparedObservation<Box<dyn Send>>,
    ) -> Result<ScanHandle> {
        // Release the parent writer BEFORE spawning. Reconnecting here would
        // compete with the child's bounded writer admission while doing a full
        // catalog validation. Normal observation polling reconnects after exit;
        // explicit demand cancellation keeps its separate bounded reconnect path.
        if self.session.detach().is_err() {
            self.unavailable = true;
            self.state = Lifecycle::Draining;
            let _ = self.session.close();
            return Err(error(
                ErrorCode::RecoveryRequired,
                Retry::Reconcile,
                "observation prepared but parent detach failed",
            ));
        }
        self.detached = true;
        match prepared.spawn() {
            Ok(worker) => {
                let handle = ScanHandle(uuid::Uuid::new_v4().to_string());
                self.worker = Some((handle.clone(), worker));
                Ok(handle)
            }
            Err(cause) => {
                // No Child exists on this branch. Retain unresolved durable
                // reservation evidence; never claim a helper-exit witness.
                self.detached = false;
                if self.session.reconnect().is_err() {
                    self.unavailable = true;
                    self.state = Lifecycle::Draining;
                    let _ = self.session.close();
                    return Err(error(
                        ErrorCode::RecoveryRequired,
                        Retry::Reconcile,
                        "observation spawn failed and catalog reconnect failed",
                    ));
                }
                self.state = Lifecycle::Draining;
                Err(HostError {
                    code: ErrorCode::RecoveryRequired,
                    retry: Retry::Reconcile,
                    detail: "observation spawn failed; unresolved reservation requires reconciliation",
                    diagnostic: Some(cause),
                })
            }
        }
    }
    pub fn active_scan(&self) -> Option<ScanHandle> {
        self.worker.as_ref().map(|(handle, _)| handle.clone())
    }
    /// Pending is explicit; it never releases the resource permit. This wait is
    /// bounded to 30s. Reconnect after exit can add the existing Store's 5s bound.
    pub fn poll_scan(&mut self, handle: &ScanHandle, wait: Duration) -> Result<Option<Value>> {
        if wait > Duration::from_secs(30) {
            return Err(invalid("scan wait exceeds 30 seconds"));
        }
        let (current, worker) = self.worker.as_mut().ok_or_else(|| {
            error(
                ErrorCode::NotFound,
                Retry::DoNotRetry,
                "scan handle is unavailable",
            )
        })?;
        if current != handle {
            return Err(error(
                ErrorCode::NotFound,
                Retry::DoNotRetry,
                "scan handle is unavailable",
            ));
        }
        let result = worker.wait_without_store(wait);
        if worker.is_finished() && self.detached {
            self.detached = false;
            if self.session.reconnect().is_err() {
                self.unavailable = true;
                self.state = Lifecycle::Draining;
                return Err(error(
                    ErrorCode::RecoveryRequired,
                    Retry::Reconcile,
                    "worker exited but catalog reconnect failed",
                ));
            }
        }
        if worker.is_finished() && !self.unavailable {
            worker
                .record_cleanup(self.session.store().map_err(internal)?)
                .map_err(internal)?;
        }
        result.map_err(internal)
    }
    /// Cancel only this demand. Other consumers' shared work is never killed.
    /// None means the child has not yet published its request identity.
    pub fn cancel_scan(&mut self, handle: &ScanHandle) -> Result<Option<Value>> {
        // Cancellation of a still-owned worker is part of its retained lease.
        // A later terminal-demand edit is a new admission and cannot bypass a
        // revoked shared gate merely because the old handle is retained.
        let _terminal_authorization = if self
            .worker
            .as_ref()
            .is_some_and(|(_, worker)| worker.is_finished())
        {
            self.acquire_authorization()?
        } else {
            None
        };
        let (current, worker) = self.worker.as_ref().ok_or_else(|| {
            error(
                ErrorCode::NotFound,
                Retry::DoNotRetry,
                "scan handle is unavailable",
            )
        })?;
        if current != handle {
            return Err(error(
                ErrorCode::NotFound,
                Retry::DoNotRetry,
                "scan handle is unavailable",
            ));
        }
        if self.unavailable {
            return Err(error(
                ErrorCode::RecoveryRequired,
                Retry::Recover,
                "catalog owner is unavailable",
            ));
        }
        if self.detached {
            if self.session.reconnect().is_err() {
                self.detached = false;
                self.unavailable = true;
                self.state = Lifecycle::Draining;
                return Err(error(
                    ErrorCode::RecoveryRequired,
                    Retry::Reconcile,
                    "catalog reconnect failed while cancelling demand",
                ));
            }
            self.detached = false;
        }
        let result = (|| {
            let store = self.session.store_mut()?;
            let Some(request_id) = worker.request_id(store)? else {
                return Ok(None);
            };
            catabolic_store::observation::execute_detached(
                store,
                &self.profile,
                "cancel",
                &json!({"request_id":request_id}),
            )
            .map(Some)
        })();
        if !worker.is_finished() {
            if self.session.detach().is_err() {
                let _ = self.session.close();
                self.unavailable = true;
                self.state = Lifecycle::Draining;
                return Err(error(
                    ErrorCode::RecoveryRequired,
                    Retry::Reconcile,
                    "demand cancellation requires reconciliation",
                ));
            }
            self.detached = true;
        }
        result.map_err(internal)
    }
    /// Stop admitting work. Explicit drain/close can then be retried safely.
    pub fn begin_drain(&mut self) {
        if self.state == Lifecycle::Open {
            self.state = Lifecycle::Draining;
        }
    }
    pub fn drain(&mut self, wait: Duration) -> Result<bool> {
        self.begin_drain();
        if let Some(handle) = self.active_scan() {
            let outcome = self.poll_scan(&handle, wait);
            if self
                .worker
                .as_ref()
                .is_some_and(|(_, worker)| !worker.is_finished())
            {
                outcome?;
                return Ok(false);
            }
            self.close()?;
            outcome?;
            return Ok(true);
        }
        self.close()?;
        Ok(true)
    }
    pub fn close(&mut self) -> Result<()> {
        self.begin_drain();
        if self
            .worker
            .as_ref()
            .is_some_and(|(_, worker)| !worker.is_finished())
        {
            return Err(error(
                ErrorCode::Busy,
                Retry::DoNotRetry,
                "drain the active scan before closing",
            ));
        }
        self.session.close().map_err(internal)?;
        self.detached = false;
        self.state = Lifecycle::Closed;
        Ok(())
    }
}
impl Drop for HostSession {
    fn drop(&mut self) {
        // Release the writer before the supervisor's blocking join fallback.
        // Explicit bounded drain is preferred; Drop never claims cancellation.
        let _ = self.session.close();
        self.worker.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Temporary relations isolate the shape classifier from mutation fixtures.
    // The production method still runs on an authorized native HostSession and
    // its retained connection/transaction, with the real VM and result budgets.
    fn projection_shape_fixture() -> (tempfile::TempDir, HostSession) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog");
        catabolic_store::migration::initialize_native_at(&path, 39).unwrap();
        let gate = HostAuthorization::new("shape-fixture".into(), "r1".into()).unwrap();
        let host =
            HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate).unwrap();
        host.session.store().unwrap().db.execute_batch(r#"
            CREATE TEMP TABLE items(id TEXT PRIMARY KEY,kind TEXT,metadata TEXT);
            CREATE TEMP TABLE identities(item_id TEXT);
            CREATE TEMP TABLE item_files(id TEXT,item_id TEXT,file_id TEXT,part INTEGER,active INTEGER,role TEXT,metadata TEXT);
            CREATE TEMP TABLE item_relationships(source_id TEXT,target_id TEXT,kind TEXT,active INTEGER,id TEXT DEFAULT 'fixture-edge',position INTEGER,metadata TEXT DEFAULT '{}');
            CREATE TEMP TABLE catalog_timelines(id TEXT,edition_item_id TEXT);
            CREATE TEMP TABLE catalog_versions(id TEXT,association_item_id TEXT,current_revision INTEGER);
            CREATE TEMP TABLE catalog_version_revisions(version_id TEXT,revision INTEGER,timeline_id TEXT);
            CREATE TEMP TABLE catalog_entity_revisions(entity_kind TEXT,entity_key TEXT,deleted INTEGER,revision INTEGER);
            CREATE TEMP TABLE catalog_logical_retirements(item_id TEXT);
            CREATE TEMP TABLE catalog_version_retirements(version_id TEXT);
            CREATE TEMP TABLE catalog_active_binding_revisions(association_item_id TEXT,association_id TEXT,file_id TEXT,part INTEGER,profile TEXT);
            INSERT INTO items VALUES('work','video','{"title":"Work","motion_catalog":{}}');
            INSERT INTO items VALUES('motion.edition:ed','custom:motion_edition','{"motion_edition":{"contract_version":1,"source_edition":{"id":"ed","item_id":"work"}}}');
            INSERT INTO items VALUES('motion.version:v','custom:motion_version','{"motion_version":{"contract_version":1,"source_version":{"id":"v"}}}');
            INSERT INTO catalog_entity_revisions VALUES('items','["work"]',0,1);
            INSERT INTO item_relationships(source_id,target_id,kind,active) VALUES('work','motion.edition:ed','custom:motion_edition',1);
            INSERT INTO item_relationships(source_id,target_id,kind,active) VALUES('motion.edition:ed','motion.version:v','custom:motion_version',1);
            INSERT INTO catalog_timelines VALUES('timeline','motion.edition:ed');
            INSERT INTO catalog_versions VALUES('v','motion.version:v',1);
            INSERT INTO catalog_version_revisions VALUES('v',1,'timeline');
            INSERT INTO item_files VALUES('association','motion.version:v','file',1,1,'primary','{}');
            INSERT INTO catalog_active_binding_revisions VALUES('motion.version:v','association','file',1,'default');
        "#).unwrap();
        (root, host)
    }
    fn work_shape(host: &HostSession) -> Value {
        host.item_projection_shapes(&["work".into()], &["title".into(), "motion_catalog".into()])
            .unwrap()["items"][0]
            .clone()
    }
    #[test]
    fn projection_shapes_account_for_valid_work_edges_without_inventing_physical_descendants() {
        let (_root, host) = projection_shape_fixture();
        let db = &host.session.store().unwrap().db;
        db.execute("INSERT INTO items VALUES('other','video','{}')", [])
            .unwrap();
        let metadata=json!({"motion_relationship":{"contract_version":1,"id":"edge","kind":"part_of","position":0}}).to_string();
        db.execute("INSERT INTO item_relationships(source_id,target_id,kind,active,id,metadata) VALUES('work','other','custom:motion_part_of',1,'edge',?)",[metadata]).unwrap();
        assert_eq!(work_shape(&host)["physical_work_projection"], true);
        db.execute("DELETE FROM item_relationships WHERE kind IN ('custom:motion_edition','custom:motion_version')",[]).unwrap();
        let shape = work_shape(&host);
        assert_eq!(shape["physical_work_projection"], false);
        assert_eq!(shape["logical_work_projection"], true);
        assert_eq!(shape["edition_ids"], json!([]));
        db.execute("UPDATE item_relationships SET active=0 WHERE id='edge'", [])
            .unwrap();
        assert_eq!(work_shape(&host)["logical_work_projection"], true);
        db.execute(
            "UPDATE item_relationships SET metadata='{}' WHERE id='edge'",
            [],
        )
        .unwrap();
        assert!(
            host.item_projection_shapes(
                &["work".into()],
                &["title".into(), "motion_catalog".into()]
            )
            .is_err()
        );
    }

    #[test]
    fn projection_shapes_require_complete_native_owner_pairs_and_timelines() {
        let (_root, host) = projection_shape_fixture();
        assert_eq!(work_shape(&host)["physical_work_projection"], true);
        let db = &host.session.store().unwrap().db;
        db.execute_batch("INSERT INTO catalog_timelines VALUES('empty-cut','motion.edition:ed')")
            .unwrap();
        let shape = work_shape(&host);
        assert_eq!(shape["physical_work_projection"], true);
        assert_eq!(shape["timeline_ids"], json!(["empty-cut", "timeline"]));
        db.execute_batch("INSERT INTO catalog_versions VALUES('unlinked','unlinked-owner',1); INSERT INTO catalog_version_revisions VALUES('unlinked',1,'empty-cut')").unwrap();
        assert_eq!(work_shape(&host)["physical_work_projection"], false);
        db.execute_batch("DELETE FROM catalog_versions WHERE id='unlinked'; DELETE FROM catalog_version_revisions WHERE version_id='unlinked'; INSERT INTO catalog_version_retirements VALUES('v')").unwrap();
        assert_eq!(work_shape(&host)["physical_work_projection"], false);
    }
    #[test]
    fn projection_shapes_recognize_generated_marker_without_granting_publication() {
        let (_root, host) = projection_shape_fixture();
        let db = &host.session.store().unwrap().db;
        // Classification only: the Motion consumer must independently prove the
        // exact native receipt, output occurrence and generated provenance.
        db.execute_batch("UPDATE items SET metadata=json_set(metadata,'$.motion_generated_output',json('{}')) WHERE id='motion.version:v'").unwrap();
        assert_eq!(work_shape(&host)["physical_work_projection"], false);
        db.execute_batch("UPDATE items SET metadata=json_set(metadata,'$.motion_version.source_version.origin','generated') WHERE id='motion.version:v'").unwrap();
        assert_eq!(work_shape(&host)["physical_work_projection"], true);
        for marker in ["null", "1", "[]", "\"invalid\""] {
            db.execute("UPDATE items SET metadata=json_set(metadata,'$.motion_generated_output',json(?)) WHERE id='motion.version:v'",[marker]).unwrap();
            assert_eq!(work_shape(&host)["physical_work_projection"], false);
        }
        db.execute_batch("UPDATE items SET metadata=json_set(metadata,'$.motion_generated_output',json('{}'),'$.unknown_generated',json('{}')) WHERE id='motion.version:v'").unwrap();
        assert_eq!(work_shape(&host)["physical_work_projection"], false);
    }

    #[test]
    fn projection_shapes_reject_unrepresented_descendant_facts() {
        let (_root, host) = projection_shape_fixture();
        let db = &host.session.store().unwrap().db;
        for (add, remove) in [
            (
                "UPDATE items SET metadata=json_set(metadata,'$.private',1) WHERE id='motion.edition:ed'",
                "UPDATE items SET metadata=json_remove(metadata,'$.private') WHERE id='motion.edition:ed'",
            ),
            (
                "UPDATE items SET metadata=json_set(metadata,'$.private',1) WHERE id='motion.version:v'",
                "UPDATE items SET metadata=json_remove(metadata,'$.private') WHERE id='motion.version:v'",
            ),
            (
                "INSERT INTO identities VALUES('motion.edition:ed')",
                "DELETE FROM identities",
            ),
            (
                "INSERT INTO identities VALUES('motion.version:v')",
                "DELETE FROM identities",
            ),
            (
                "INSERT INTO item_files VALUES('extra','motion.version:v','other',2,1,'subtitle','{}')",
                "DELETE FROM item_files WHERE id='extra'",
            ),
            (
                "UPDATE item_files SET metadata='{\"private\":1}'",
                "UPDATE item_files SET metadata='{}'",
            ),
            (
                "INSERT INTO item_relationships(source_id,target_id,kind,active) VALUES('motion.version:v','other','related',1)",
                "DELETE FROM item_relationships WHERE source_id='motion.version:v'",
            ),
            (
                "INSERT INTO catalog_logical_retirements VALUES('motion.edition:ed')",
                "DELETE FROM catalog_logical_retirements",
            ),
        ] {
            db.execute_batch(add).unwrap();
            assert_eq!(
                work_shape(&host)["physical_work_projection"],
                false,
                "{add}"
            );
            db.execute_batch(remove).unwrap();
            assert_eq!(
                work_shape(&host)["physical_work_projection"],
                true,
                "{remove}"
            );
        }
    }
    #[test]
    fn projection_shapes_bound_descendants_and_restore_connection_after_refusal() {
        let (_root, host) = projection_shape_fixture();
        let db = &host.session.store().unwrap().db;
        db.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10001) INSERT INTO item_relationships(source_id,target_id,kind,active) SELECT 'work','edition-'||x,'custom:motion_edition',1 FROM n").unwrap();
        let failure = host
            .item_projection_shapes(&["work".into()], &["title".into(), "motion_catalog".into()])
            .unwrap_err();
        assert_eq!(failure.code, ErrorCode::BudgetExhausted);
        assert!(db.is_autocommit());
        db.execute_batch("DELETE FROM item_relationships WHERE target_id LIKE 'edition-%'")
            .unwrap();
        assert_eq!(work_shape(&host)["physical_work_projection"], true);
    }
    #[test]
    fn cancellation_after_first_member_rolls_back_all_members() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        catabolic_store::migration::initialize(&path).unwrap();
        let mut host = HostSession::open(&path, "default", true, CONTRACT_VERSION).unwrap();
        let cancel = Cancellation::default();
        let triggered = cancel.clone();
        host.session
            .store()
            .unwrap()
            .db
            .create_scalar_function(
                "host_fixture_cancel",
                0,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                move |_| {
                    triggered.cancel();
                    Ok(1)
                },
            )
            .unwrap();
        host.session.store().unwrap().db.execute_batch("CREATE TEMP TRIGGER host_cancel_after_insert AFTER INSERT ON main.items BEGIN SELECT host_fixture_cancel(); END;").unwrap();
        let failure = host
            .put_items(
                &[
                    json!({"id":"first","kind":"movie","metadata":{}}),
                    json!({"id":"second","kind":"movie","metadata":{}}),
                ],
                &cancel,
            )
            .unwrap_err();
        assert!(cancel.is_cancelled());
        assert_eq!(failure.code, ErrorCode::Cancelled);
        let count: i64 = host
            .session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        assert!(host.session.store().unwrap().db.is_autocommit());
    }
    #[test]
    fn native_host_requires_explicit_authority_and_preserves_legacy_operator_open() {
        for schema in [35, 36] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("catalog");
            catabolic_store::migration::initialize_native_at(&path, schema).unwrap();
            let error = match HostSession::open(&path, "default", true, CONTRACT_VERSION) {
                Ok(_) => panic!("native gate was omitted"),
                Err(error) => error,
            };
            assert_eq!(error.code, ErrorCode::UnsupportedCapability);
            let gate = HostAuthorization::new("fixture".into(), "r1".into()).unwrap();
            gate.begin_revocation().unwrap();
            assert!(
                HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate)
                    .is_err()
            );
        }
    }
    #[test]
    fn authorized_native_write_retains_the_same_gate_through_commit() {
        use catabolic_core::catalog_host_lease::LeaseError;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog");
        catabolic_store::migration::initialize_native_at(&path, 36).unwrap();
        let gate = HostAuthorization::new("host".into(), "revision".into()).unwrap();
        let mut host =
            HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate.clone())
                .unwrap();
        let observed = Arc::new(AtomicBool::new(false));
        let marker = observed.clone();
        let same = gate.clone();
        host.session
            .store()
            .unwrap()
            .db
            .create_scalar_function(
                "host_fixture_revoke",
                0,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                move |_| {
                    let revoked = same.begin_revocation().unwrap();
                    assert_eq!(
                        revoked.wait_drained(Duration::ZERO),
                        Err(LeaseError::Timeout)
                    );
                    marker.store(true, Ordering::SeqCst);
                    Ok(1)
                },
            )
            .unwrap();
        host.session.store().unwrap().db.execute_batch("CREATE TEMP TRIGGER gate_during_write AFTER INSERT ON main.items BEGIN SELECT host_fixture_revoke(); END;").unwrap();
        host.put_items(
            &[json!({"id":"committed","kind":"movie","metadata":{}})],
            &Cancellation::default(),
        )
        .unwrap();
        assert!(observed.load(Ordering::SeqCst));
        gate.begin_revocation()
            .unwrap()
            .wait_drained(Duration::ZERO)
            .unwrap();
        assert_eq!(
            host.put_items(
                &[json!({"id":"rejected","kind":"movie","metadata":{}})],
                &Cancellation::default()
            )
            .unwrap_err()
            .code,
            ErrorCode::Conflict
        );
        assert_eq!(host.identity().unwrap_err().code, ErrorCode::Conflict);
        assert_eq!(
            host.select(
                &QueryId::new("saved").unwrap(),
                ReadBudget {
                    timeout: Duration::from_secs(1),
                    maximum_ids: 1
                },
                &Cancellation::default()
            )
            .unwrap_err()
            .code,
            ErrorCode::Conflict
        );
        assert_eq!(
            host.register_helper(Path::new("/missing-helper"), &"a".repeat(64))
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        assert_eq!(
            host.start_scan("media", ()).unwrap_err().code,
            ErrorCode::Conflict
        );
        assert_eq!(
            host.session
                .store()
                .unwrap()
                .db
                .query_row("SELECT count(*) FROM items", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn pending_recovery_refuses_mutation_before_item_write() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        catabolic_store::migration::initialize(&path).unwrap();
        let mut host = HostSession::open(&path, "default", true, CONTRACT_VERSION).unwrap();
        host.session.store().unwrap().db.execute_batch("INSERT INTO journal(id,profile,catalog,path,kind) VALUES ('pending','default','global','','prepare');").unwrap();
        let failure = host
            .put_items(
                &[json!({"id":"blocked","kind":"movie","metadata":{}})],
                &Cancellation::default(),
            )
            .unwrap_err();
        assert_eq!(failure.code, ErrorCode::RecoveryRequired);
        let count: i64 = host
            .session
            .store()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
    #[test]
    fn rollback_failure_overrides_validation_and_conflict_classification() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
        for conflict in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("catalog.db");
            catabolic_store::migration::initialize(&path).unwrap();
            let mut host = HostSession::open(&path, "default", true, CONTRACT_VERSION).unwrap();
            host.put_items(
                &[json!({"id":"existing","kind":"movie","metadata":{}})],
                &Cancellation::default(),
            )
            .unwrap();
            host.session
                .store()
                .unwrap()
                .db
                .authorizer(Some(|context: AuthContext<'_>| {
                    if matches!(
                        context.action,
                        AuthAction::Transaction {
                            operation: TransactionOperation::Rollback
                        } | AuthAction::Savepoint {
                            operation: TransactionOperation::Rollback,
                            ..
                        }
                    ) {
                        Authorization::Deny
                    } else {
                        Authorization::Allow
                    }
                }))
                .unwrap();
            let rejected = if conflict {
                json!({"id":"existing","kind":"episode","metadata":{}})
            } else {
                json!({"id":"invalid","kind":"not_a_kind","metadata":{}})
            };
            let failure = host
                .put_items(&[rejected], &Cancellation::default())
                .unwrap_err();
            assert_eq!(failure.code, ErrorCode::Internal);
            assert_eq!(failure.retry, Retry::Reopen);
            assert!(host.unavailable);
            assert_eq!(host.lifecycle(), Lifecycle::Draining);
            host.close().unwrap();
            assert!(HostSession::open(&path, "default", true, CONTRACT_VERSION).is_ok());
        }
    }
}
