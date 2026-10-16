//! Owns Catabolic's SQLite lifecycle. Opening never creates or upgrades a catalog.
pub mod access;
pub mod access_admin;
pub mod admin_cli;
pub mod api_catalog;
pub mod api_events;
pub mod api_queries;
pub mod artifact_mutation;
pub mod catalog_commands;
pub mod catalog_identity;
pub mod catalog_identity_mutation;
pub mod catalog_import;
pub mod catalog_import_typed;
pub mod catalog_logical_edits;
pub mod catalog_metadata_store;
pub mod catalog_pins;
pub mod catalog_refresh;
pub mod catalog_scope;
pub mod catalog_search_index;
pub mod catalog_tag_mutation;
pub mod catalog_write;
pub mod component_mutation;
pub mod component_selection;
mod components;
pub mod consumers;
pub mod content_access;
pub mod curation;
pub mod definitions;
pub mod destination_mappings;
mod document;
pub mod embedding;
pub mod estimates;
pub mod export_bundle;
pub mod exports;
pub mod fallback_mutation;
pub mod fallback_projection;
pub mod fallback_publication;
pub mod fallback_resolution;
pub mod filesystem_mutation;
pub mod graphql;
mod graphql_input;
mod graphql_parse;
mod graphql_runtime_input;
pub mod hardlink_mutation;
pub mod http_mappings;
pub mod http_operations;
pub mod identity_manifest;
pub mod imports;
pub mod interchange;
mod introspection;
pub mod item_workflow;
pub mod layout;
pub mod layout_mutation;
pub mod legacy_refresh;
pub mod maintenance;
pub mod manifest;
pub mod mapping_history;
pub mod media_mutation;
pub mod migration;
mod native_admission;
mod native_writer_vm;
pub use native_admission::{NativeAdmissionLock, native_admission_exclusive};
pub mod mime;
pub mod mutation;
pub mod notifications;
pub mod observation;
pub mod output_mutation;
pub mod plans;
pub mod playback;
pub mod playback_cache;
pub mod private_secret;
pub mod processor_mutation;
pub mod programs;
pub mod projection_run;
pub mod public_components;
pub mod public_fallback;
pub mod public_layouts;
pub mod public_reconcile;
pub mod public_watchers;
mod publication;
pub mod publication_mappings;
pub mod query;
pub mod receipt_mutation;
pub mod remount;
pub mod rendition_mutation;
pub mod rules;
mod schema_versions;
pub mod selection;
mod source;
pub mod source_events;
pub mod sql;
pub mod tagging;
pub mod targets;
pub mod timestamps;
mod validation;
pub mod watcher;
pub mod webhooks;
pub mod work_inbox;

use rusqlite::{Connection, OpenFlags};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Stable exception class selected by the failing native check.
/// Diagnostic text is never used to infer this provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorClass {
    CatabolicError,
    TypeError,
}
impl ErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CatabolicError => "CatabolicError",
            Self::TypeError => "TypeError",
        }
    }
}

#[derive(Debug)]
pub struct Error(pub String);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self(e.to_string())
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self(e.to_string())
    }
}
impl From<catabolic_core::Error> for Error {
    fn from(e: catabolic_core::Error) -> Self {
        Self(e.to_string())
    }
}
pub type Result<T> = std::result::Result<T, Error>;

/// Additive failure details selected only by the actual failing native check.
/// Existing callers retain `Error(pub String)` and may deliberately erase details.
pub struct DetailedError {
    error: Error,
    provenance: ErrorProvenance,
}

enum ErrorProvenance {
    Ordinary,
    UnhashableSortDictionary,
    UnhashableSortList,
    MappingSubjectJson {
        raw: String,
    },
    MappingRuntime {
        class: &'static str,
        message: String,
    },
}

pub type DetailedResult<T> = std::result::Result<T, DetailedError>;

impl DetailedError {
    pub fn class(&self) -> ErrorClass {
        match self.provenance {
            ErrorProvenance::UnhashableSortDictionary | ErrorProvenance::UnhashableSortList => {
                ErrorClass::TypeError
            }
            _ => ErrorClass::CatabolicError,
        }
    }

    /// Raw evidence from the failed stored-subject decode, for diagnostic-only
    /// presentation. It is never part of Display, Debug or the source chain.
    pub fn mapping_subject_json(&self) -> Option<&str> {
        match &self.provenance {
            ErrorProvenance::MappingSubjectJson { raw } => Some(raw),
            _ => None,
        }
    }

    /// Source-selected CLI runtime diagnostic. Legacy Rust errors retain their
    /// original text; this versioned presentation never synthesizes a traceback.
    pub fn mapping_runtime_diagnostic(&self) -> Option<(&'static str, &str)> {
        match &self.provenance {
            ErrorProvenance::MappingRuntime { class, message } => Some((class, message)),
            _ => None,
        }
    }

    pub(crate) fn mapping_runtime(error: Error, class: &'static str, message: String) -> Self {
        Self {
            error,
            provenance: ErrorProvenance::MappingRuntime { class, message },
        }
    }

    pub fn error(&self) -> &Error {
        &self.error
    }

    /// Deliberately discard all boundary provenance, retaining the legacy error.
    pub fn into_error(self) -> Error {
        self.error
    }

    pub(crate) fn mapping_subject_parse(error: catabolic_core::Error, raw: &str) -> Self {
        Self {
            error: error.into(),
            provenance: ErrorProvenance::MappingSubjectJson { raw: raw.into() },
        }
    }

    pub(crate) fn unhashable_sort_list() -> Self {
        Self {
            error: Error("cannot use 'list' as a dict key (unhashable type: 'list')".into()),
            provenance: ErrorProvenance::UnhashableSortList,
        }
    }

    pub(crate) fn unhashable_sort_dictionary() -> Self {
        Self {
            error: Error("cannot use 'dict' as a dict key (unhashable type: 'dict')".into()),
            provenance: ErrorProvenance::UnhashableSortDictionary,
        }
    }
}
impl std::fmt::Debug for DetailedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DetailedError")
            .field("error", &self.error)
            .field("class", &self.class())
            .finish()
    }
}
impl std::fmt::Display for DetailedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for DetailedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
impl From<Error> for DetailedError {
    fn from(error: Error) -> Self {
        Self {
            error,
            provenance: ErrorProvenance::Ordinary,
        }
    }
}
impl From<DetailedError> for Error {
    fn from(error: DetailedError) -> Self {
        error.into_error()
    }
}
impl From<rusqlite::Error> for DetailedError {
    fn from(error: rusqlite::Error) -> Self {
        Error::from(error).into()
    }
}
impl From<std::io::Error> for DetailedError {
    fn from(error: std::io::Error) -> Self {
        Error::from(error).into()
    }
}
impl From<serde_json::Error> for DetailedError {
    fn from(error: serde_json::Error) -> Self {
        Error::from(error).into()
    }
}
impl From<catabolic_core::Error> for DetailedError {
    fn from(error: catabolic_core::Error) -> Self {
        Error::from(error).into()
    }
}

/// Stable admission causes for hosts opening an existing native catalog.
/// Codes are selected at the failing check, never from diagnostic text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenErrorCode {
    UnsupportedSchema,
    BusyWriter,
    InvalidCatalog,
    Internal,
}

/// An additive typed admission error that retains the legacy diagnostic.
#[derive(Debug)]
pub struct OpenError {
    pub code: OpenErrorCode,
    pub error: Error,
}
impl OpenError {
    pub(crate) fn new(code: OpenErrorCode, error: Error) -> Self {
        Self { code, error }
    }
    pub(crate) fn invalid(error: Error) -> Self {
        Self::new(OpenErrorCode::InvalidCatalog, error)
    }
    fn internal(error: Error) -> Self {
        Self::new(OpenErrorCode::Internal, error)
    }
}
impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for OpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
impl From<OpenError> for Error {
    fn from(error: OpenError) -> Self {
        error.error
    }
}
impl From<std::io::Error> for OpenError {
    fn from(error: std::io::Error) -> Self {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound
            | std::io::ErrorKind::NotADirectory
            | std::io::ErrorKind::InvalidInput => OpenErrorCode::InvalidCatalog,
            _ => OpenErrorCode::Internal,
        };
        Self::new(code, error.into())
    }
}
impl From<rusqlite::Error> for OpenError {
    fn from(error: rusqlite::Error) -> Self {
        let code = match &error {
            rusqlite::Error::SqliteFailure(cause, _) => match cause.code {
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                    OpenErrorCode::BusyWriter
                }
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase => {
                    OpenErrorCode::InvalidCatalog
                }
                // CANTOPEN can also mean exhausted OS resources or permissions;
                // only the earlier path checks establish a missing catalog.
                _ => OpenErrorCode::Internal,
            },
            rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::QueryReturnedNoRows => OpenErrorCode::InvalidCatalog,
            _ => OpenErrorCode::Internal,
        };
        Self::new(code, error.into())
    }
}
pub type OpenResult<T> = std::result::Result<T, OpenError>;

pub const RECOVERABLE_SCHEMAS: &[u32] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 24, 25, 26, 28,
];

pub fn database_path(path: &Path) -> Result<PathBuf> {
    database_path_typed(path).map_err(Into::into)
}

fn database_path_typed(path: &Path) -> OpenResult<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let resolved = absolute
        .parent()
        .ok_or_else(|| OpenError::invalid(Error("database parent is missing".into())))?
        .canonicalize()?
        .join(
            absolute
                .file_name()
                .ok_or_else(|| OpenError::invalid(Error("database name is missing".into())))?,
        );
    let metadata = std::fs::symlink_metadata(&resolved).map_err(|cause| {
        let mut error = OpenError::from(cause);
        error.error = Error(format!(
            "database does not exist or is a symlink: {}; use init explicitly",
            resolved.display()
        ));
        error
    })?;
    if !metadata.file_type().is_file() {
        return Err(OpenError::invalid(Error(format!(
            "database does not exist or is a symlink: {}; use init explicitly",
            resolved.display()
        ))));
    }
    if metadata.nlink() != 1 {
        return Err(OpenError::invalid(Error(
            "hard-linked databases are unsupported; use one canonical database path".into(),
        )));
    }
    Ok(resolved)
}

/// The owning scope explicitly releases its advisory lock before closing.
/// Closing alone can retain a flock through an unrelated child's transient
/// fork/exec duplicate of the same open-file description, even with CLOEXEC.
/// The descriptor is intentionally private and the ownership is not clonable.
pub struct WriterLock(File, u32);
impl Drop for WriterLock {
    fn drop(&mut self) {
        // SAFETY: this object owns a live descriptor. Unlock only releases the
        // lock belonging to this open-file description, never another owner's.
        if std::process::id() == self.1 {
            // SAFETY: only the creating process explicitly releases its owned flock.
            unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

pub fn writer_lock(path: &Path) -> Result<WriterLock> {
    writer_lock_typed(path).map_err(Into::into)
}

fn writer_lock_typed(path: &Path) -> OpenResult<WriterLock> {
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(Path::new(&lock))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(OpenError::invalid(Error(
            "database lock must be a regular file with one link".into(),
        )));
    }
    // SAFETY: file owns a valid descriptor for the duration of this call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Err(OpenError::new(
                OpenErrorCode::BusyWriter,
                Error("another Catabolic writer or upgrade is using this database".into()),
            ));
        }
        return Err(error.into());
    }
    Ok(WriterLock(file, std::process::id()))
}

pub fn connect(path: &Path, writable: bool) -> Result<Connection> {
    connect_typed(path, writable).map_err(Into::into)
}

fn connect_typed(path: &Path, writable: bool) -> OpenResult<Connection> {
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let db = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    // Choose query TEMP storage before any transaction or caller-owned objects.
    // Per-query setup must preserve a caller's later temp_store choice: changing
    // it there can either fail in their transaction or delete their TEMP schema.
    db.pragma_update(None, "temp_store", "MEMORY")?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.busy_timeout(Duration::from_secs(5))?;
    let schema: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    configure_native_connection(&db, schema).map_err(OpenError::internal)?;
    components::install(&db).map_err(OpenError::internal)?;
    Ok(db)
}

/// Required connection semantics for every supported native schema; legacy stays frozen.
pub(crate) fn configure_native_connection(db: &Connection, schema: u32) -> Result<()> {
    if migration::is_supported_native_schema(schema) {
        db.pragma_update(None, "foreign_keys", true)?;
        db.pragma_update(None, "recursive_triggers", true)?;
        db.pragma_update(None, "read_uncommitted", false)?;
    }
    Ok(())
}

fn native_incarnation(db: &Connection, schema: u32, catalog_id: &str) -> Result<Option<String>> {
    if !migration::is_supported_native_schema(schema) {
        return Ok(None);
    }
    for (name, expected) in [
        ("foreign_keys", 1i64),
        ("recursive_triggers", 1),
        ("read_uncommitted", 0),
    ] {
        let actual: i64 = db.pragma_query_value(None, name, |row| row.get(0))?;
        if actual != expected {
            return Err(Error(
                "unsafe native catalog connection settings; reopen required".into(),
            ));
        }
    }
    let (identity, incarnation, generation): (String, String, i64) = db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if identity != catalog_id
        || generation < 0
        || incarnation.len() != 32
        || !incarnation
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(Error(
            "native catalog identity or incarnation is invalid".into(),
        ));
    }
    Ok(Some(incarnation))
}

pub struct Store {
    pub db: Connection,
    pub path: PathBuf,
    pub writable: bool,
    pub schema_version: u32,
    pub database_id: String,
    pub catalog_incarnation: Option<String>,
    native_path_identity: Option<(u64, u64)>,
    native_admission: Option<NativeAdmissionLock>,
    lock: Option<WriterLock>,
    transaction_poisoned: std::cell::Cell<bool>,
    catalog_operation_budget: std::cell::RefCell<Option<operation_budget::OperationBudget>>,
}
impl Store {
    pub fn open(path: &Path, writable: bool, for_recovery: bool) -> Result<Self> {
        Self::open_typed(path, writable, for_recovery).map_err(Into::into)
    }
    /// Open the same native owner with structured admission failures.
    /// This performs no initialization or automatic schema upgrade.
    pub fn open_typed(path: &Path, writable: bool, for_recovery: bool) -> OpenResult<Self> {
        let path = database_path_typed(path)?;
        let lock = if writable {
            Some(writer_lock_typed(&path)?)
        } else {
            None
        };
        let mut db = connect_typed(&path, writable)?;
        if !writable {
            db.execute_batch("BEGIN")?;
        }
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version != migration::SCHEMA_VERSION
            && !migration::is_supported_native_schema(version)
            && !(for_recovery && RECOVERABLE_SCHEMAS.contains(&version))
        {
            return Err(OpenError::new(
                OpenErrorCode::UnsupportedSchema,
                Error(if (1..migration::SCHEMA_VERSION).contains(&version) {
                    format!(
                        "database schema {version} needs an upgrade to {}; run catabolic --db {} db upgrade",
                        migration::SCHEMA_VERSION,
                        path.display()
                    )
                } else {
                    format!(
                        "unsupported database schema {version}; no automatic migration was attempted"
                    )
                }),
            ));
        }
        if for_recovery && migration::is_supported_native_schema(version) {
            return Err(OpenError::new(
                OpenErrorCode::UnsupportedSchema,
                Error(
                    "native catalog recovery requires separately qualified recovery admission"
                        .into(),
                ),
            ));
        }
        let mut native_path_identity = None;
        let native_admission = if migration::is_supported_native_schema(version) {
            let guard = native_admission::native_admission_shared(&path)?;
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.is_file() || metadata.nlink() != 1 {
                return Err(OpenError::invalid(Error(
                    "native catalog pathname is not a unique regular file".into(),
                )));
            }
            native_path_identity = Some((metadata.dev(), metadata.ino()));
            // The first schema read happened before the lifecycle lease. Reopen
            // under it so a restore in that gap cannot admit a stale snapshot.
            drop(db);
            db = connect_typed(&path, writable)?;
            if !writable {
                db.execute_batch("BEGIN")?;
            }
            let reopened: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
            if reopened != version {
                return Err(OpenError::invalid(Error(
                    "catalog schema changed during admission; reopen required".into(),
                )));
            }
            Some(guard)
        } else {
            None
        };
        let info = migration::validate_typed(&db, false)?;
        let catalog_incarnation =
            native_incarnation(&db, version, info["database_id"].as_str().unwrap())
                .map_err(OpenError::invalid)?;
        if catalog_incarnation.is_some() {
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.is_file()
                || metadata.nlink() != 1
                || native_path_identity != Some((metadata.dev(), metadata.ino()))
            {
                return Err(OpenError::invalid(Error(
                    "native catalog pathname changed during admission".into(),
                )));
            }
        }
        Ok(Self {
            db,
            path,
            writable,
            schema_version: version,
            database_id: info["database_id"]
                .as_str()
                .expect("validated identity")
                .into(),
            catalog_incarnation,
            native_path_identity,
            native_admission,
            lock,
            transaction_poisoned: std::cell::Cell::new(false),
            catalog_operation_budget: std::cell::RefCell::new(None),
        })
    }
    pub(crate) fn catalog_operation_budget(&self) -> Result<operation_budget::OperationBudget> {
        self.catalog_operation_budget
            .borrow()
            .clone()
            .ok_or_else(|| Error("catalog_operation_budget_missing".into()))
    }
    pub(crate) fn transaction_available(&self) -> Result<()> {
        if self.writable && self.lock.is_none() {
            return Err(Error(
                "catalog session unavailable after detached execution".into(),
            ));
        }
        if self.transaction_poisoned.get() {
            return Err(Error(
                "native store unavailable after transaction cleanup failure".into(),
            ));
        }
        self.validate_native_identity()?;
        Ok(())
    }
    /// Fence native lifetime identity before reads, writes and commit. Ordinary
    /// opening does not rotate incarnation or treat replaced bytes as a reopen.
    pub fn validate_native_identity(&self) -> Result<()> {
        let Some(expected) = self.catalog_incarnation.as_deref() else {
            return Ok(());
        };
        let metadata = std::fs::symlink_metadata(&self.path)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || self.native_path_identity != Some((metadata.dev(), metadata.ino()))
        {
            return Err(Error(
                "native catalog pathname was replaced; reopen required".into(),
            ));
        }
        self.native_admission
            .as_ref()
            .ok_or_else(|| Error("native catalog admission lease missing".into()))?
            .validate_path()?;
        let current = native_incarnation(&self.db, self.schema_version, &self.database_id)?;
        if current.as_deref() != Some(expected) {
            return Err(Error(
                "native catalog incarnation changed; reopen required".into(),
            ));
        }
        Ok(())
    }
    fn poison_transaction_owner(&self) {
        self.transaction_poisoned.set(true);
        // Even direct db access must fail closed after an unprovable rollback.
        let _ = self
            .db
            .authorizer(Some(|_: rusqlite::hooks::AuthContext<'_>| {
                rusqlite::hooks::Authorization::Deny
            }));
    }
    pub(crate) fn rollback_transaction(&self, savepoint: Option<&str>) -> Result<()> {
        let _cleanup = native_writer_vm::cleanup(&self.db);
        // SQLITE_INTERRUPT, OR ROLLBACK and certain I/O failures can already
        // have rolled back every savepoint. Never replace the original error
        // with a spurious "no transaction is active" cleanup error.
        if self.db.is_autocommit() {
            return Ok(());
        }
        let sql = savepoint.map_or_else(
            || "ROLLBACK".into(),
            |name| format!("ROLLBACK TO {name}; RELEASE {name}"),
        );
        if let Err(error) = self.db.execute_batch(&sql) {
            // A missing/corrupt scope cannot be acknowledged as scoped rollback.
            // Retire the connection even if a best-effort full rollback succeeds.
            let _ = self.db.execute_batch("ROLLBACK");
            self.poison_transaction_owner();
            return Err(error.into());
        }
        Ok(())
    }
    pub(crate) fn begin_transaction(&self) -> Result<Option<String>> {
        self.transaction_available()?;
        if self.writable && self.lock.is_none() {
            return Err(Error(
                "catalog session unavailable after detached execution".into(),
            ));
        }
        if !self.writable {
            return Err(Error(
                "a read-only session cannot start a write transaction".into(),
            ));
        }
        let nested = !self.db.is_autocommit();
        if self.schema_version >= 35
            && (!nested || self.catalog_operation_budget.borrow().is_none())
        {
            let budget = operation_budget::OperationBudget::current()
                .map_or_else(catalog_search_index::default_operation_budget, Ok)?;
            budget
                .check()
                .map_err(|failure| Error(format!("operation budget: {failure:?}")))?;
            *self.catalog_operation_budget.borrow_mut() = Some(budget);
        }
        let savepoint = format!("nested_{}", uuid::Uuid::new_v4().simple());
        self.db.execute_batch(&if nested {
            format!("SAVEPOINT {savepoint}")
        } else {
            "BEGIN IMMEDIATE".into()
        })?;
        Ok(nested.then_some(savepoint))
    }
    pub(crate) fn finish_transaction<T>(
        &self,
        savepoint: Option<String>,
        outcome: Result<T>,
    ) -> Result<T> {
        self.finish_transaction_detailed(savepoint, outcome)
    }
    fn finish_transaction_detailed<T, E: From<Error> + std::fmt::Display>(
        &self,
        savepoint: Option<String>,
        outcome: std::result::Result<T, E>,
    ) -> std::result::Result<T, E> {
        match outcome {
            Ok(value) => {
                if let Err(error) = self.transaction_available() {
                    self.rollback_transaction(savepoint.as_deref())?;
                    return Err(error.into());
                }
                if self.db.is_autocommit() {
                    return Err(Error("native transaction was aborted before commit".into()).into());
                }
                if self.schema_version >= 35
                    && savepoint.is_none()
                    && let Err(error) = self.catalog_operation_budget().and_then(|budget| {
                        catalog_search_index::flush_before_commit(&self.db, &budget)
                    })
                {
                    self.rollback_transaction(savepoint.as_deref())?;
                    return Err(error.into());
                }
                // Keep the private writer's actual VM callback installed through
                // COMMIT. Poll after all derived/final-output work; top-level
                // COMMIT then reserves its fixed phase before durability.
                if let Some(budget) = native_writer_vm::current_budget(&self.db)
                    && let Err(cause) = budget.check()
                {
                    self.rollback_transaction(savepoint.as_deref())?;
                    return Err(Error(format!("operation budget: {cause:?}")).into());
                }
                let commit = savepoint
                    .as_ref()
                    .map_or_else(|| "COMMIT".into(), |name| format!("RELEASE {name}"));
                let commit_admission = if savepoint.is_none() {
                    match native_writer_vm::prepare_commit(&self.db) {
                        Ok(admission) => admission,
                        Err(error) => {
                            self.rollback_transaction(None)?;
                            return Err(error.into());
                        }
                    }
                } else {
                    None
                };
                let committed = self.db.execute_batch(&commit);
                let exceeded_commit_bound = commit_admission
                    .as_ref()
                    .is_some_and(|admission| !admission.within_prepaid_bound());
                drop(commit_admission);
                if let Err(error) = committed {
                    let _ = self.rollback_transaction(savepoint.as_deref());
                    if exceeded_commit_bound {
                        self.poison_transaction_owner();
                    }
                    return Err(Error::from(error).into());
                }
                if exceeded_commit_bound {
                    // The pinned fixed-program bound is qualification evidence.
                    // Retire an unexpectedly changed owner, but never erase an
                    // acknowledged durable COMMIT or pretend it rolled back.
                    self.poison_transaction_owner();
                }
                Ok(value)
            }
            Err(error) => {
                let rollback = self.rollback_transaction(savepoint.as_deref());
                match rollback {
                    Ok(()) => Err(error),
                    Err(cleanup) => {
                        Err(Error(format!("{error}; native rollback failed: {cleanup}")).into())
                    }
                }
            }
        }
    }
    pub fn transaction<T>(&self, operation: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.transaction_detailed(operation)
    }
    /// Preserve a private owner's typed failure through the shared transaction
    /// mechanism. Begin/commit/cleanup failures always come from Store::Error.
    pub(crate) fn transaction_detailed<T, E: From<Error> + std::fmt::Display>(
        &self,
        operation: impl FnOnce(&Connection) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E> {
        struct RollbackOnUnwind<'a> {
            store: &'a Store,
            scope: Option<String>,
            armed: bool,
        }
        impl Drop for RollbackOnUnwind<'_> {
            fn drop(&mut self) {
                if self.armed {
                    // Drop cannot replace the original panic with cleanup failure.
                    // rollback_transaction retires the owner if rollback fails.
                    let _ = self.store.rollback_transaction(self.scope.as_deref());
                }
            }
        }
        let scope = self.begin_transaction()?;
        let mut guard = RollbackOnUnwind {
            store: self,
            scope: scope.clone(),
            armed: true,
        };
        let budget = self.catalog_operation_budget.borrow().clone();
        let _budget_scope = budget
            .as_ref()
            .map(|budget| budget.enter())
            .transpose()
            .map_err(|failure| Error(format!("operation budget: {failure:?}")))?;
        let outcome = operation(&self.db);
        let result = self.finish_transaction_detailed(scope, outcome);
        guard.armed = false;
        result
    }
    /// Release the connection and writer lock around slow external work, then
    /// validate the reopened catalog identity before allowing further work.
    pub fn detached<T>(&mut self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        self.transaction_available()?;
        if self.writable && self.lock.is_none() {
            return Err(Error(
                "catalog session unavailable after detached execution".into(),
            ));
        }
        if !self.db.is_autocommit() {
            return Err(Error("cannot detach an active transaction".into()));
        }
        let placeholder = Connection::open_in_memory()?;
        // A failed reopen must leave an unusable connection, including for
        // callers accessing db directly. Never acknowledge placeholder writes.
        placeholder.authorizer(Some(|_: rusqlite::hooks::AuthContext<'_>| {
            rusqlite::hooks::Authorization::Deny
        }))?;
        drop(std::mem::replace(&mut self.db, placeholder));
        self.lock.take();
        let outcome = operation();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        if self.writable {
            loop {
                match writer_lock(&self.path) {
                    Ok(lock) => {
                        self.lock = Some(lock);
                        break;
                    }
                    Err(error)
                        if error.0.contains("another Catabolic writer")
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        let reopened = (|| {
            if let Some(admission) = &self.native_admission {
                admission.validate_path()?;
            }
            let path = database_path(&self.path)?;
            let db = connect(&path, self.writable)?;
            let info = migration::validate(&db, false)?;
            if info["database_id"] != self.database_id {
                return Err(Error("catalog replaced during detached execution".into()));
            }
            if self.catalog_incarnation.is_some() {
                let schema = info["schema"].as_u64().unwrap() as u32;
                let metadata = std::fs::symlink_metadata(&path)?;
                if schema != self.schema_version
                    || native_incarnation(&db, schema, &self.database_id)?
                        != self.catalog_incarnation
                    || self.native_path_identity != Some((metadata.dev(), metadata.ino()))
                {
                    return Err(Error(
                        "native catalog replaced during detached execution".into(),
                    ));
                }
            }
            Ok(db)
        })();
        match reopened {
            Ok(db) => {
                self.db = db;
                outcome
            }
            Err(error) => {
                self.lock.take();
                Err(error)
            }
        }
    }
    pub fn owns_writer_lock(&self) -> bool {
        self.lock.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writer_scope_releases_lock_despite_inherited_description() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        let owner = writer_lock(&path).unwrap();
        let inherited = owner.0.try_clone().unwrap();
        assert!(writer_lock(&path).is_err());
        drop(owner);
        let successor = writer_lock(&path).unwrap();
        // Closing an earlier open-file description cannot unlock this scope.
        drop(inherited);
        assert!(writer_lock(&path).is_err());
        drop(successor);
        assert!(writer_lock(&path).is_ok());
    }
    fn catalog() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        migration::initialize(&path).unwrap();
        (root, path)
    }
    #[test]
    fn nested_savepoints_rollback_only_the_failed_scope() {
        let (_root, path) = catalog();
        let store = Store::open(&path, true, false).unwrap();
        store
            .transaction(|db| {
                db.execute("INSERT INTO catalogs(id) VALUES('outer')", [])?;
                let failed = store.transaction(|db| -> Result<()> {
                    db.execute("INSERT INTO catalogs(id) VALUES('inner')", [])?;
                    Err(Error("injected nested failure".into()))
                });
                assert!(failed.is_err());
                assert_eq!(
                    db.query_row("SELECT count(*) FROM catalogs WHERE id='inner'", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                    0
                );
                Ok(())
            })
            .unwrap();
        assert!(store.db.is_autocommit());
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM catalogs WHERE id='outer'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        let failed = store.transaction(|db| -> Result<()> {
            db.execute("DELETE FROM catalogs WHERE id='outer'", [])?;
            Err(Error("outer failure".into()))
        });
        assert!(failed.is_err());
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM catalogs WHERE id='outer'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
    }
    #[test]
    fn readonly_snapshot_is_stable_and_refuses_transactions() {
        let (_root, path) = catalog();
        let writer = Store::open(&path, true, false).unwrap();
        writer
            .db
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        let reader = Store::open(&path, false, false).unwrap();
        writer
            .transaction(|db| {
                db.execute("INSERT INTO catalogs(id) VALUES('later')", [])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            reader
                .db
                .query_row("SELECT count(*) FROM catalogs WHERE id='later'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert!(reader.transaction(|_| Ok(())).is_err());
        drop(reader);
        assert_eq!(
            Store::open(&path, false, false)
                .unwrap()
                .db
                .query_row("SELECT count(*) FROM catalogs WHERE id='later'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
    }
    #[test]
    fn failed_detach_lock_or_reopen_disables_the_session() {
        for fail_lock in [false, true] {
            let (_root, path) = catalog();
            let mut writer = Store::open(&path, true, false).unwrap();
            assert!(
                writer
                    .detached(|| {
                        if fail_lock {
                            let lock_path = path.with_extension("sqlite3.lock");
                            std::fs::remove_file(&lock_path)?;
                            std::os::unix::fs::symlink(&path, lock_path)?;
                        } else {
                            std::fs::remove_file(&path)?;
                        }
                        Ok(())
                    })
                    .is_err()
            );
            assert!(!writer.owns_writer_lock());
            assert!(writer.transaction(|_| Ok(())).is_err());
            assert!(
                writer
                    .db
                    .execute_batch("CREATE TABLE ghost(value TEXT)")
                    .is_err()
            );
            assert!(writer.detached(|| Ok(())).is_err());
        }
    }
    #[test]
    fn detach_releases_writer_and_refuses_replaced_identity() {
        let (root, path) = catalog();
        let mut writer = Store::open(&path, true, false).unwrap();
        writer
            .detached(|| {
                let other = Store::open(&path, true, false)?;
                other.transaction(|db| {
                    db.execute("INSERT INTO catalogs(id) VALUES('detached')", [])?;
                    Ok(())
                })
            })
            .unwrap();
        assert!(writer.owns_writer_lock());
        writer.db.execute_batch("BEGIN").unwrap();
        assert!(
            writer
                .detached(|| Ok(()))
                .unwrap_err()
                .0
                .contains("active transaction")
        );
        writer.db.execute_batch("ROLLBACK").unwrap();
        let replacement = root.path().join("replacement.sqlite3");
        migration::initialize(&replacement).unwrap();
        assert!(
            writer
                .detached(|| {
                    std::fs::rename(&replacement, &path)?;
                    Ok(())
                })
                .unwrap_err()
                .0
                .contains("catalog replaced")
        );
        assert!(!writer.owns_writer_lock());
        assert!(
            writer
                .transaction(|db| {
                    db.execute_batch("CREATE TABLE ghost(value TEXT)")?;
                    Ok(())
                })
                .unwrap_err()
                .0
                .contains("unavailable")
        );
        assert!(
            writer
                .db
                .execute_batch("CREATE TABLE ghost(value TEXT)")
                .is_err()
        );
        assert!(
            writer
                .db
                .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
                .is_err()
        );
        assert!(
            writer
                .detached(|| Ok(()))
                .unwrap_err()
                .0
                .contains("unavailable")
        );
    }
}

pub mod plex_import;

pub mod plex_matching;

pub mod public_reads;

pub mod plex_metadata;

pub mod public_controls;

mod operation_budget;

#[cfg(test)]
mod catalog_schema37_test_support;

pub mod native_observation_companion;
pub mod native_worker_inventory;
