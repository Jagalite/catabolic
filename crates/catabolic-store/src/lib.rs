//! Owns Catabolic's SQLite lifecycle. Opening never creates or upgrades a catalog.
pub mod access;
pub mod api_catalog;
pub mod api_events;
pub mod api_queries;
pub mod artifact_mutation;
pub mod catalog_refresh;
pub mod component_mutation;
pub mod component_selection;
mod components;
pub mod consumers;
pub mod content_access;
pub mod curation;
pub mod definitions;
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
pub mod hardlink_mutation;
pub mod http_mappings;
pub mod http_operations;
pub mod imports;
pub mod interchange;
mod introspection;
pub mod item_workflow;
pub mod layout;
pub mod layout_mutation;
pub mod manifest;
pub mod media_mutation;
pub mod migration;
pub mod mime;
pub mod mutation;
pub mod notifications;
pub mod observation;
pub mod output_mutation;
pub mod plans;
pub mod playback;
pub mod playback_cache;
pub mod processor_mutation;
pub mod programs;
mod publication;
pub mod query;
pub mod receipt_mutation;
pub mod remount;
pub mod rendition_mutation;
pub mod rules;
pub mod selection;
mod source;
pub mod source_events;
pub mod sql;
pub mod tagging;
pub mod targets;
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

pub const RECOVERABLE_SCHEMAS: &[u32] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 24, 25, 26, 28,
];

pub fn database_path(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let resolved = absolute
        .parent()
        .ok_or_else(|| Error("database parent is missing".into()))?
        .canonicalize()?
        .join(
            absolute
                .file_name()
                .ok_or_else(|| Error("database name is missing".into()))?,
        );
    let metadata = std::fs::symlink_metadata(&resolved).map_err(|_| {
        Error(format!(
            "database does not exist or is a symlink: {}; use init explicitly",
            resolved.display()
        ))
    })?;
    if !metadata.file_type().is_file() {
        return Err(Error(format!(
            "database does not exist or is a symlink: {}; use init explicitly",
            resolved.display()
        )));
    }
    if metadata.nlink() != 1 {
        return Err(Error(
            "hard-linked databases are unsupported; use one canonical database path".into(),
        ));
    }
    Ok(resolved)
}

/// The owning scope explicitly releases its advisory lock before closing.
/// Closing alone can retain a flock through an unrelated child's transient
/// fork/exec duplicate of the same open-file description, even with CLOEXEC.
/// The descriptor is intentionally private and the ownership is not clonable.
pub struct WriterLock(File);
impl Drop for WriterLock {
    fn drop(&mut self) {
        // SAFETY: this object owns a live descriptor. Unlock only releases the
        // lock belonging to this open-file description, never another owner's.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub fn writer_lock(path: &Path) -> Result<WriterLock> {
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
        return Err(Error(
            "database lock must be a regular file with one link".into(),
        ));
    }
    // SAFETY: file owns a valid descriptor for the duration of this call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Err(Error(
                "another Catabolic writer or upgrade is using this database".into(),
            ));
        }
        return Err(error.into());
    }
    Ok(WriterLock(file))
}

pub fn connect(path: &Path, writable: bool) -> Result<Connection> {
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let db = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.busy_timeout(Duration::from_secs(5))?;
    components::install(&db)?;
    Ok(db)
}

pub struct Store {
    pub db: Connection,
    pub path: PathBuf,
    pub writable: bool,
    pub schema_version: u32,
    pub database_id: String,
    lock: Option<WriterLock>,
    transaction_poisoned: std::cell::Cell<bool>,
}
impl Store {
    pub fn open(path: &Path, writable: bool, for_recovery: bool) -> Result<Self> {
        let path = database_path(path)?;
        let lock = if writable {
            Some(writer_lock(&path)?)
        } else {
            None
        };
        let db = connect(&path, writable)?;
        if !writable {
            db.execute_batch("BEGIN")?;
        }
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version != migration::SCHEMA_VERSION
            && !(for_recovery && RECOVERABLE_SCHEMAS.contains(&version))
        {
            return Err(Error(
                if (1..migration::SCHEMA_VERSION).contains(&version) {
                    format!(
                        "database schema {version} needs an upgrade to {}; run catabolic --db {} db upgrade",
                        migration::SCHEMA_VERSION,
                        path.display()
                    )
                } else {
                    format!(
                        "unsupported database schema {version}; no automatic migration was attempted"
                    )
                },
            ));
        }
        let info = migration::validate(&db, false)?;
        Ok(Self {
            db,
            path,
            writable,
            schema_version: version,
            database_id: info["database_id"]
                .as_str()
                .expect("validated identity")
                .into(),
            lock,
            transaction_poisoned: std::cell::Cell::new(false),
        })
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
        match outcome {
            Ok(value) => {
                self.transaction_available()?;
                if self.db.is_autocommit() {
                    return Err(Error("native transaction was aborted before commit".into()));
                }
                let commit = savepoint
                    .as_ref()
                    .map_or_else(|| "COMMIT".into(), |name| format!("RELEASE {name}"));
                if let Err(error) = self.db.execute_batch(&commit) {
                    let _ = self.rollback_transaction(savepoint.as_deref());
                    return Err(error.into());
                }
                Ok(value)
            }
            Err(error) => {
                let rollback = self.rollback_transaction(savepoint.as_deref());
                match rollback {
                    Ok(()) => Err(error),
                    Err(cleanup) => {
                        Err(Error(format!("{error}; native rollback failed: {cleanup}")))
                    }
                }
            }
        }
    }
    pub fn transaction<T>(&self, operation: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
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
        let outcome = operation(&self.db);
        let result = self.finish_transaction(scope, outcome);
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
            let path = database_path(&self.path)?;
            let db = connect(&path, self.writable)?;
            let info = migration::validate(&db, false)?;
            if info["database_id"] != self.database_id {
                return Err(Error("catalog replaced during detached execution".into()));
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
