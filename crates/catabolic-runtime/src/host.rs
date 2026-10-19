// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Versioned, synchronous, privileged native host boundary.
//!
//! Run on a host-owned blocking thread. This is an operator capability, not an
//! application-user authorization API: R4 supplies scoped admission separately.
//! Store access, arbitrary SQL and caller-controlled transactions are deliberately
//! absent. Calls reuse the same native session and never upgrade on open.
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
        if !matches!(store.schema_version, 34..=39) {
            return Err(error(
                ErrorCode::UnsupportedSchema,
                Retry::DoNotRetry,
                "import restart is qualified only for native34 through native39",
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
            json!({"contract_version":CONTRACT_VERSION,"catalog_id":store.database_id,"schema_version":store.schema_version}),
        )
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
        let started = crate::observation::start_supervised(
            self.session.store_mut().map_err(internal)?,
            &self.profile,
            &json!({"source":source,"isolate":true}),
            Box::new((permit, authorization)) as Box<dyn Send>,
        )
        .map_err(internal)?;
        self.retain_started(started)
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
        let started = crate::observation::start_shared_supervised(
            self.session.store_mut().map_err(internal)?,
            &self.profile,
            worker,
            Box::new((permit, Some(authorization))) as Box<dyn Send>,
        )
        .map_err(internal)?;
        self.retain_started(started)
    }
    fn retain_started(
        &mut self,
        started: crate::observation::SupervisedStart<Box<dyn Send>>,
    ) -> Result<ScanHandle> {
        let handle = ScanHandle(uuid::Uuid::new_v4().to_string());
        self.worker = Some((handle.clone(), started.worker));
        // The helper must not wait behind a Store retained between host calls.
        if started.reconnect_error.is_some() || self.session.detach().is_err() {
            self.unavailable = true;
            self.state = Lifecycle::Draining;
            let _ = self.session.close();
            return Err(error(
                ErrorCode::RecoveryRequired,
                Retry::Reconcile,
                "scan started but store reconnect failed; retain and drain active scan",
            ));
        }
        self.detached = true;
        Ok(handle)
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
