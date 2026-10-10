//! Owns Catabolic's SQLite lifecycle. Opening never creates or upgrades a catalog.
mod components;
pub mod graphql;
pub mod migration;
pub mod query;
pub mod sql;

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
    pub fn owns_writer_lock(&self) -> bool {
        self.lock.is_some()
    }
}
