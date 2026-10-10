//! Owns Catabolic's SQLite lifecycle. Opening never creates or upgrades a catalog.
mod components;
mod document;
pub mod graphql;
mod introspection;
pub mod layout;
pub mod migration;
mod publication;
pub mod query;
pub mod selection;
mod source;
pub mod sql;
mod validation;

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

pub fn writer_lock(path: &Path) -> Result<File> {
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
    Ok(file)
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
    lock: Option<File>,
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
        })
    }
    pub fn transaction<T>(&self, operation: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
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
        match operation(&self.db) {
            Ok(value) => {
                let result = self.db.execute_batch(&if nested {
                    format!("RELEASE {savepoint}")
                } else {
                    "COMMIT".into()
                });
                if let Err(e) = result {
                    let _ = self.db.execute_batch(&if nested {
                        format!("ROLLBACK TO {savepoint}; RELEASE {savepoint}")
                    } else {
                        "ROLLBACK".into()
                    });
                    return Err(e.into());
                }
                Ok(value)
            }
            Err(e) => {
                self.db.execute_batch(&if nested {
                    format!("ROLLBACK TO {savepoint}; RELEASE {savepoint}")
                } else {
                    "ROLLBACK".into()
                })?;
                Err(e)
            }
        }
    }
    /// Release the connection and writer lock around slow external work, then
    /// validate the reopened catalog identity before allowing further work.
    pub fn detached<T>(&mut self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        if !self.db.is_autocommit() {
            return Err(Error("cannot detach an active transaction".into()));
        }
        let placeholder = Connection::open_in_memory()?;
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
    }
}
