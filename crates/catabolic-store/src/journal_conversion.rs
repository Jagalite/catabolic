// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Offline WAL to rollback-journal conversion before an explicit native upgrade.
//!
//! Native upgrade needs a real SQLite EXCLUSIVE lock, which WAL mode does not
//! provide against readers, so it refuses WAL catalogs. This is the separate,
//! offline operator step that precedes a fresh plan. It requires the same
//! explicit operator quiescence assertions as native upgrade, because a
//! process that has opened SQLite without reading holds no lock that any
//! probe can see. It then takes the native
//! lifecycle lease and the Catabolic writer lock, refuses while any other
//! process still has the WAL index open, then holds a real SQLite EXCLUSIVE
//! database lock from before the first snapshot until verification ends. It
//! verifies that every committed row, including committed WAL frames,
//! survives the conversion. It never converts into WAL: native WAL operation
//! is not a supported mode.
use crate::{Error, Result, database_path, migration, native_admission_exclusive, writer_lock};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

/// SQLite's unix VFS lock bytes in the `-shm` file: read marks, writer,
/// checkpointer, recovery and the dead-man switch every open connection
/// holds shared for as long as it has the WAL index mapped.
const SHM_LOCK_START: i64 = 120;
const SHM_LOCK_BYTES: i64 = 9;

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn busy() -> Error {
    Error("journal conversion refused: another connection has this database open; stop every Catabolic, Python and SQLite user of this catalog and retry".into())
}

/// Report whether another process holds any WAL-index lock. This must run
/// before this process opens its own SQLite connection: POSIX record locks
/// belong to the process, and closing this probe descriptor releases every
/// lock the process holds on the file. F_GETLK only reports other processes.
fn other_wal_users(path: &Path) -> Result<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(sidecar(path, "-shm"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    // SAFETY: an all-zero flock is a valid value for this plain C struct.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::F_WRLCK as _;
    lock.l_whence = libc::SEEK_SET as _;
    lock.l_start = SHM_LOCK_START as _;
    lock.l_len = SHM_LOCK_BYTES as _;
    // SAFETY: the descriptor is live and `lock` is a valid, writable flock.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut lock) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(lock.l_type != libc::F_UNLCK as libc::c_short)
}

fn journal_mode(db: &Connection) -> Result<String> {
    Ok(db.pragma_query_value(None, "journal_mode", |row| row.get(0))?)
}

fn locked(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// Validate and digest the catalog inside one read transaction.
fn state(db: &Connection) -> Result<(Value, Value)> {
    db.execute_batch("BEGIN")?;
    let result =
        migration::validate(db, true).and_then(|info| Ok((info, migration::snapshot(db)?)));
    db.execute_batch("ROLLBACK")?;
    result
}

/// Open the catalog in SQLite's EXCLUSIVE locking mode and take the database
/// lock before anything is read. In WAL mode this also keeps the WAL index in
/// private memory, so no other connection can read or write until this one
/// closes. Connection settings otherwise match `connect`.
pub(crate) fn connect_exclusive(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.pragma_update(None, "temp_store", "MEMORY")?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.busy_timeout(Duration::ZERO)?;
    let mode: String = db.query_row("PRAGMA locking_mode=EXCLUSIVE", [], |row| row.get(0))?;
    if mode != "exclusive" {
        return Err(busy());
    }
    // Exclusive locking mode retains the lock acquired here after COMMIT.
    db.execute_batch("BEGIN EXCLUSIVE; COMMIT")
        .map_err(|error| if locked(&error) { busy() } else { error.into() })?;
    let schema: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    crate::configure_native_connection(&db, schema)?;
    crate::components::install(&db)?;
    Ok(db)
}

/// Convert a WAL catalog to SQLite's DELETE rollback journal. A catalog that is
/// already in a rollback-journal mode is reported unchanged. Only `delete` is
/// an accepted target, and the operator must assert quiescence explicitly.
pub fn convert(
    path: &Path,
    target: &str,
    quiescence: migration::NativeUpgradeQuiescence,
) -> Result<Value> {
    convert_observed(path, target, quiescence, &|_| Ok(()))
}

/// `convert` with an observer called at `locked` (exclusion held, nothing read
/// yet) and `converted` (journal switched, before verification). An observer
/// error aborts the conversion; it cannot release the held lock early.
pub fn convert_observed(
    path: &Path,
    target: &str,
    quiescence: migration::NativeUpgradeQuiescence,
    observer: &dyn Fn(&str) -> Result<()>,
) -> Result<Value> {
    if target != "delete" {
        return Err(Error(format!(
            "unsupported journal conversion target {target:?}; only delete is supported and native catalogs are never converted to WAL"
        )));
    }
    // Locks cannot see an idle descriptor; refuse before any lock or open.
    quiescence.require()?;
    let path = database_path(path)?;
    let _admission = native_admission_exclusive(&path)?;
    let _writer = writer_lock(&path)?;
    if other_wal_users(&path)? {
        return Err(busy());
    }
    let db = connect_exclusive(&path)?;
    observer("locked")?;
    let from = journal_mode(&db)?;
    let (info, before) = state(&db)?;
    let report = json!({"database":path,"database_id":info["database_id"],"schema":info["schema"],"from_journal_mode":from});
    if from != "wal" {
        let mut report = report;
        report["journal_mode"] = json!(from);
        report["converted"] = json!(false);
        return Ok(report);
    }
    // TRUNCATE copies every committed frame into the database and empties the
    // log. The exclusive lock means no other connection can make it partial.
    let (blocked, _, _): (i64, i64, i64) = db
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|error| if locked(&error) { busy() } else { error.into() })?;
    if blocked != 0 {
        return Err(busy());
    }
    let mode: String = db
        .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
        .map_err(|error| if locked(&error) { busy() } else { error.into() })?;
    if mode != "delete" {
        return Err(busy());
    }
    observer("converted")?;
    let (_, after) = state(&db)?;
    if after != before {
        return Err(Error(
            "journal conversion changed catalog contents; inspect the catalog before any upgrade"
                .into(),
        ));
    }
    // Closing releases the exclusive lock, and only after verification.
    drop(db);
    if sidecar(&path, "-wal").exists() {
        return Err(Error(
            "journal conversion left a WAL file behind; another connection may still be open"
                .into(),
        ));
    }
    let mut report = report;
    report["journal_mode"] = json!(mode);
    report["converted"] = json!(true);
    Ok(report)
}
