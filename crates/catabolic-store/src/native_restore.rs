// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Verified native backups and controlled restore with incarnation rotation.
//!
//! A native backup is a consistent SQLite snapshot of a native catalog plus a
//! manifest binding its SHA-256, catalog identity, incarnation and identity
//! manifest. Controlled restore (below) publishes such a snapshot at the live
//! path under a fresh incarnation, so sessions, permits and receipts bound to
//! the previous incarnation are refused or reported as uncertain.
use crate::{Error, Result, connect, database_path, identity_manifest, migration};
use catabolic_core::sha256;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const BACKUP_CONTRACT: &str = "catabolic-native-backup-v1";

/// Create a private directory and make its entry durable.
fn private_directory(path: &Path) -> Result<()> {
    std::os::unix::fs::DirBuilderExt::mode(&mut fs::DirBuilder::new(), 0o700).create(path)?;
    migration::fsync(
        path.parent()
            .ok_or_else(|| Error("directory has no parent".into()))?,
    )
}

/// Take a verified backup of a native catalog. The snapshot is copied with
/// SQLite's backup API inside one read transaction, validated, digested and
/// fsynced before its manifest is written. `parent` must already exist,
/// except for the default `<catalog>.backups`, which is created once.
pub fn backup(path: &Path, parent: Option<&Path>) -> Result<Value> {
    let path = database_path(path)?;
    let db = connect(&path, false)?;
    db.execute_batch("BEGIN")?;
    let info = migration::validate(&db, true)?;
    let schema = info["schema"].as_u64().expect("validated schema") as u32;
    if !migration::is_supported_native_schema(schema) {
        return Err(Error(format!(
            "native backup requires a native catalog; schema {schema} is a legacy schema, use db upgrade backups instead"
        )));
    }
    let identity = identity_manifest::manifest(&db)?;
    let parent = match parent {
        Some(parent) => {
            let parent = std::path::absolute(parent)?;
            if !parent.is_dir() {
                return Err(Error(format!(
                    "backup parent {} must be an existing directory",
                    parent.display()
                )));
            }
            parent.canonicalize()?
        }
        None => {
            let parent = PathBuf::from(format!("{}.backups", path.display()));
            if !parent.exists() {
                private_directory(&parent)?;
            }
            parent.canonicalize()?
        }
    };
    let directory = parent.join(format!("native-v{schema}-{}", Uuid::new_v4().simple()));
    if path.starts_with(&directory) {
        return Err(Error(
            "backup directory cannot contain the active database".into(),
        ));
    }
    private_directory(&directory)?;
    let partial = directory.join("snapshot.sqlite3.partial");
    migration::copy(&db, &partial)?;
    drop(db);
    let verify = connect(&partial, false)?;
    verify.execute_batch("BEGIN")?;
    let copied = migration::validate(&verify, true)?;
    let copied_identity = identity_manifest::manifest(&verify)?;
    drop(verify);
    if copied["database_id"] != info["database_id"]
        || copied_identity["fingerprint"] != identity["fingerprint"]
    {
        return Err(Error(
            "native backup differs from the catalog it copied".into(),
        ));
    }
    migration::fsync(&partial)?;
    let saved = directory.join("snapshot.sqlite3");
    fs::rename(&partial, &saved)?;
    migration::fsync(&directory)?;
    let receipt = json!({
        "contract":BACKUP_CONTRACT, "database":path, "database_id":info["database_id"],
        "catalog_id":identity["catalog"]["catalog_id"], "incarnation":identity["catalog"]["incarnation"],
        "generation":identity["catalog"]["generation"], "schema":schema, "backup":saved,
        "sha256":sha256(fs::read(&saved)?), "identity_manifest":identity,
    });
    migration::manifest(&directory, &receipt)?;
    Ok(receipt)
}

pub const RESTORE_CONTRACT: &str = "catabolic-native-restore-v1";

/// Restore checkpoints, in order. The rename between `publishing` and
/// `renamed` is the single commit point: stages up to `publishing` precede it,
/// and every later stage belongs to a committed restore.
pub const RESTORE_STAGES: [&str; 10] = [
    "staged",
    "verified",
    "retained",
    "rotated",
    "protected",
    "publishing",
    "renamed",
    "published",
    "finalizing",
    "finalized",
];

/// Checkpoint on the rerun path, before retrying the directory fsync of an
/// already published restore.
pub const RERUN_FSYNC: &str = "rerun_fsync";
/// Checkpoint between the final checks and the publication rename. Only tests
/// use it, to model a substitution in that unavoidable window.
pub const RENAME_WINDOW: &str = "rename_window";
/// Checkpoint after the backup directory and its manifest were opened through
/// held descriptors and before the snapshot is opened. Only tests use it.
pub const BACKUP_OPENED: &str = "backup_opened";

/// One controlled restore at a time in this process. Lifecycle admission
/// excludes other processes; this also keeps a second restore in the same
/// process from opening and closing descriptors that could cancel the first
/// one's POSIX locks.
static RESTORE_IN_PROCESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn name_cstring(name: &str) -> Result<std::ffi::CString> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(Error(format!("invalid directory entry name {name:?}")));
    }
    std::ffi::CString::new(name)
        .map_err(|_| Error(format!("invalid directory entry name {name:?}")))
}

fn os_error() -> Error {
    std::io::Error::last_os_error().into()
}

/// What `fstatat(..., AT_SYMLINK_NOFOLLOW)` reports for one directory entry.
struct Entry {
    identity: (u64, u64),
    regular: bool,
    directory: bool,
    links: u64,
}

/// A directory held open with `O_DIRECTORY|O_NOFOLLOW`. Every create, open,
/// rename, unlink and fsync in restore goes through such a descriptor, so a
/// pathname swapped after it was opened is never followed. Its pathname is
/// kept only for SQLite, which opens files by name; callers recheck that the
/// name still resolves to the held directory around every such open.
struct Dir {
    file: fs::File,
    identity: (u64, u64),
    path: PathBuf,
}

impl Dir {
    fn adopt(fd: libc::c_int, path: PathBuf) -> Result<Dir> {
        use std::os::fd::FromRawFd;
        // SAFETY: the caller passes a freshly opened descriptor it owns.
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let identity = descriptor_identity(&file)?;
        Ok(Dir {
            file,
            identity,
            path,
        })
    }

    fn open(path: &Path) -> Result<Dir> {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| Error(format!("invalid directory path {}", path.display())))?;
        // SAFETY: raw is a valid NUL-terminated path.
        let fd = unsafe {
            libc::open(
                raw.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(Error(format!(
                "cannot open directory {} without following a symbolic link: {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        Dir::adopt(fd, path.to_path_buf())
    }

    fn fd(&self) -> libc::c_int {
        use std::os::fd::AsRawFd;
        self.file.as_raw_fd()
    }

    fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    fn open_dir_at(&self, name: &str) -> Result<Dir> {
        let raw = name_cstring(name)?;
        // SAFETY: valid directory descriptor and NUL-terminated entry name.
        let fd = unsafe {
            libc::openat(
                self.fd(),
                raw.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(os_error());
        }
        Dir::adopt(fd, self.child(name))
    }

    fn create_dir_at(&self, name: &str) -> Result<Dir> {
        let raw = name_cstring(name)?;
        // SAFETY: valid directory descriptor and NUL-terminated entry name.
        if unsafe { libc::mkdirat(self.fd(), raw.as_ptr(), 0o700) } != 0 {
            return Err(os_error());
        }
        self.fsync()?;
        self.open_dir_at(name)
    }

    fn open_file_at(&self, name: &str) -> std::io::Result<fs::File> {
        use std::os::fd::FromRawFd;
        let raw = name_cstring(name).map_err(|error| std::io::Error::other(error.0))?;
        // SAFETY: valid directory descriptor and NUL-terminated entry name.
        let fd = unsafe {
            libc::openat(
                self.fd(),
                raw.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh descriptor we own.
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }

    fn create_file_at(&self, name: &str) -> Result<fs::File> {
        use std::os::fd::FromRawFd;
        let raw = name_cstring(name)?;
        // SAFETY: valid directory descriptor and NUL-terminated entry name.
        let fd = unsafe {
            libc::openat(
                self.fd(),
                raw.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(os_error());
        }
        // SAFETY: openat returned a fresh descriptor we own.
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }

    fn entry(&self, name: &str) -> Option<Entry> {
        let raw = name_cstring(name).ok()?;
        // SAFETY: an all-zero stat is a valid output buffer.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid descriptor, name and output buffer.
        if unsafe {
            libc::fstatat(
                self.fd(),
                raw.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return None;
        }
        let kind = stat.st_mode & libc::S_IFMT;
        Some(Entry {
            identity: (stat.st_dev as u64, stat.st_ino as u64),
            regular: kind == libc::S_IFREG,
            directory: kind == libc::S_IFDIR,
            links: stat.st_nlink as u64,
        })
    }

    fn rename_at(&self, from: &str, to: &Dir, name: &str) -> Result<()> {
        let (from, name) = (name_cstring(from)?, name_cstring(name)?);
        // SAFETY: valid descriptors and NUL-terminated entry names.
        if unsafe { libc::renameat(self.fd(), from.as_ptr(), to.fd(), name.as_ptr()) } != 0 {
            return Err(os_error());
        }
        Ok(())
    }

    fn unlink_at(&self, name: &str, directory: bool) -> Result<()> {
        let raw = name_cstring(name)?;
        let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
        // SAFETY: valid descriptor and NUL-terminated entry name.
        if unsafe { libc::unlinkat(self.fd(), raw.as_ptr(), flags) } != 0 {
            return Err(os_error());
        }
        Ok(())
    }

    fn fsync(&self) -> Result<()> {
        Ok(self.file.sync_all()?)
    }

    /// The held descriptor is unchanged and its pathname still resolves to
    /// it, so SQLite opening a file by name below it reaches this directory.
    fn require_unchanged(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let held = descriptor_identity(&self.file)?;
        let named = fs::symlink_metadata(&self.path)
            .ok()
            .filter(|metadata| metadata.file_type().is_dir())
            .map(|metadata| (metadata.dev(), metadata.ino()));
        if held != self.identity || named != Some(self.identity) {
            return Err(Error(format!(
                "directory {} was replaced during restore; refusing to continue",
                self.path.display()
            )));
        }
        Ok(())
    }

    /// The entry `name` is still exactly the held child directory.
    fn require_child(&self, name: &str, child: &Dir) -> Result<()> {
        match self.entry(name) {
            Some(entry) if entry.directory && entry.identity == child.identity => Ok(()),
            _ => Err(Error(format!(
                "staging directory {} was replaced during restore; refusing to continue",
                child.path.display()
            ))),
        }
    }
}

/// Write `manifest.json` inside a held directory: a new private temporary file
/// written and fsynced through its descriptor, renamed within the directory,
/// and the directory fsynced. Every failure is an error.
fn write_record(parent: &Dir, staging_name: &str, staging: &Dir, record: &Value) -> Result<()> {
    use std::io::Write;
    parent.require_child(staging_name, staging)?;
    let temporary = format!("manifest-{}.tmp", Uuid::new_v4().simple());
    let mut file = staging.create_file_at(&temporary)?;
    file.write_all(serde_json::to_string_pretty(record)?.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    parent.require_child(staging_name, staging)?;
    staging.rename_at(&temporary, staging, "manifest.json")?;
    staging.fsync()
}

/// Remove this run's unpublished staging directory, entry by entry through
/// its held descriptor. Anything unexpected is left for the operator.
fn remove_staging(parent: &Dir, staging_name: &str, staging: &Dir) {
    if staging.require_unchanged().is_err() || parent.require_child(staging_name, staging).is_err()
    {
        return;
    }
    if let Ok(entries) = fs::read_dir(&staging.path) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // unlinkat removes a symbolic link itself and never follows it.
            if staging.entry(&name).is_some_and(|entry| !entry.directory) {
                let _ = staging.unlink_at(&name, false);
            }
        }
    }
    if parent.require_child(staging_name, staging).is_ok() {
        let _ = parent.unlink_at(staging_name, true);
        let _ = parent.fsync();
    }
}

fn fresh_incarnation(avoid: &[&str]) -> String {
    loop {
        let candidate = Uuid::new_v4().simple().to_string();
        if !avoid.contains(&candidate.as_str()) {
            return candidate;
        }
    }
}

/// Everything a published catalog must keep exactly: whole-row table digests,
/// every schema object (tables, indexes, views and triggers), migration
/// history, AUTOINCREMENT sequences and the identity manifest. Table digests
/// alone exclude schema definitions, history and sqlite_sequence.
fn complete_state(db: &rusqlite::Connection) -> Result<Value> {
    let sequences = if db
        .prepare("SELECT 1 FROM sqlite_schema WHERE type='table' AND name='sqlite_sequence'")?
        .exists([])?
    {
        json!(migration::rows(
            db,
            "SELECT name,seq FROM sqlite_sequence ORDER BY name,seq",
            &[]
        )?)
    } else {
        Value::Null
    };
    Ok(json!({
        "tables":migration::snapshot(db)?,
        "schema":migration::rows(db, "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name,tbl_name", &[])?,
        "history":migration::rows(db, "SELECT version,filename,checksum,origin,applied_at FROM schema_migrations ORDER BY version", &[])?,
        "sequences":sequences,
        "identity":identity_manifest::manifest(db)?,
    }))
}

/// Rotate the staged copy's incarnation through the schema's restore-scoped
/// permit, verify that nothing else changed, and return the incarnation and
/// the complete state of the rotated file.
fn rotate(
    staged: &Path,
    snapshot_tables: &Value,
    identity: &Value,
    avoid: &[&str],
) -> Result<(String, Value)> {
    let incarnation = fresh_incarnation(avoid);
    let db = connect(staged, true)?;
    db.execute_batch("BEGIN IMMEDIATE")?;
    let rotated = (|| -> Result<()> {
        db.execute(
            "INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) SELECT 'restore',catalog_id,incarnation FROM catalog_state WHERE id=1",
            [],
        )?;
        db.execute(
            "UPDATE catalog_state SET incarnation=? WHERE id=1",
            [&incarnation],
        )?;
        db.execute("DELETE FROM catalog_mutation_permits", [])?;
        db.execute_batch("COMMIT")?;
        Ok(())
    })();
    if rotated.is_err() && !db.is_autocommit() {
        db.execute_batch("ROLLBACK")?;
    }
    rotated?;
    db.execute_batch("BEGIN")?;
    migration::validate(&db, true)?;
    let state = complete_state(&db)?;
    drop(db);
    let tables = &state["tables"];
    let after = &state["identity"];
    let mut unchanged = tables.clone();
    let mut expected = snapshot_tables.clone();
    for object in [&mut unchanged, &mut expected] {
        object
            .as_object_mut()
            .expect("snapshot object")
            .remove("catalog_state");
    }
    if unchanged != expected
        || after["catalog"]["incarnation"] != incarnation
        || after["catalog"]["generation"] != identity["catalog"]["generation"]
    {
        return Err(Error(
            "incarnation rotation changed restored catalog contents".into(),
        ));
    }
    identity_manifest::require_preserved(identity, after)?;
    Ok((incarnation, state))
}

/// Cleanup witnesses for completed native workers name the incarnation that
/// sealed them, and the schema forbids rebinding them. Re-attesting them under
/// the new incarnation would mint evidence no runtime owner produced, so a
/// snapshot whose restored copy would carry unresolved worker ownership is
/// refused before publication, whether the work had completed or not.
fn require_worker_ownership_resolved(staged: &Path) -> Result<()> {
    let store = crate::Store::open(staged, false, false)?;
    let inventory = crate::native_worker_inventory::inspect(&store)?;
    drop(store);
    if !inventory.admits_new_work() {
        let owners = inventory
            .unresolved
            .iter()
            .map(|owner| format!("{}:{}", owner.kind, owner.id))
            .collect::<Vec<_>>();
        return Err(Error(format!(
            "native backup has retained worker ownership that cannot be carried across incarnation rotation (cleanup evidence is bound to the backup's incarnation and is not re-attested): {}{}; restore a backup taken with no retained worker ownership",
            owners.join(", "),
            if inventory.truncated {
                " (truncated)"
            } else {
                ""
            }
        )));
    }
    Ok(())
}

fn descriptor_identity(file: &fs::File) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

fn durable_identity(volume: &str, identity: (u64, u64)) -> Value {
    json!({"volume":volume, "inode":identity.1})
}

/// Identities of the live catalog and whichever of its sidecars exist.
fn live_identities(parent: &Dir, target_name: &str) -> Vec<(u64, u64)> {
    ["", "-journal", "-wal", "-shm"]
        .iter()
        .filter_map(|suffix| parent.entry(&format!("{target_name}{suffix}")))
        .map(|entry| entry.identity)
        .collect()
}

/// Open a backup file through the held backup directory without following a
/// symbolic link, and refuse anything but a unique regular file that is not
/// the live catalog, one of its sidecars or a hard link to either.
fn open_backup_file(backup: &Dir, name: &str, parent: &Dir, target_name: &str) -> Result<fs::File> {
    use std::os::unix::fs::MetadataExt;
    let file = backup.open_file_at(name).map_err(|error| {
        Error(format!(
            "cannot open native backup file {} without following a symbolic link: {error}",
            backup.child(name).display()
        ))
    })?;
    let metadata = file.metadata()?;
    if live_identities(parent, target_name).contains(&(metadata.dev(), metadata.ino())) {
        return Err(Error(format!(
            "native backup file {} is the live catalog or one of its sidecars; refusing to restore",
            backup.child(name).display()
        )));
    }
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(Error(format!(
            "native backup file {} is not a unique regular file; refusing to restore",
            backup.child(name).display()
        )));
    }
    Ok(file)
}

fn is_hex(value: &Value, length: usize) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == length
            && text
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

/// Every field a restore record must carry, with its type, and the catalog
/// it must identify. Returns the first problem found.
fn validate_record(
    record: &Value,
    target: &Path,
    database_id: &Value,
) -> std::result::Result<(), String> {
    let object = record
        .as_object()
        .ok_or_else(|| "the record is not an object".to_owned())?;
    if record["contract"] != RESTORE_CONTRACT {
        return Err("contract is not catabolic-native-restore-v1".into());
    }
    if !matches!(
        record["status"].as_str(),
        Some("staged" | "publishing" | "published" | "substitution_detected" | "abandoned")
    ) {
        return Err("status is missing or unknown".into());
    }
    for field in [
        "database",
        "database_id",
        "catalog_id",
        "backup",
        "retained",
    ] {
        if !record[field].is_string() {
            return Err(format!("{field} is missing or not a string"));
        }
    }
    for field in ["previous_incarnation", "incarnation"] {
        if !is_hex(&record[field], 32) {
            return Err(format!("{field} is missing or not an incarnation"));
        }
    }
    for field in ["backup_sha256", "previous_sha256"] {
        if !is_hex(&record[field], 64) {
            return Err(format!("{field} is missing or not a SHA-256"));
        }
    }
    for field in ["live_identity", "staged_identity", "retained_identity"] {
        let complete = record[field].as_object().is_some_and(|identity| {
            identity.len() == 2
                && record[field]["volume"]
                    .as_str()
                    .is_some_and(|volume| !volume.is_empty())
                && record[field]["inode"].is_u64()
        });
        if !complete {
            return Err(format!("{field} is missing or not a file identity"));
        }
    }
    if object.contains_key("durability")
        && !matches!(record["durability"].as_str(), Some("durable" | "uncertain"))
    {
        return Err("durability is not durable or uncertain".into());
    }
    if record["database"] != json!(target) {
        return Err("it names a different database path".into());
    }
    if record["database_id"] != *database_id {
        return Err("it names a different database ID".into());
    }
    Ok(())
}

/// Refusal used whenever an existing restore record cannot be checked.
fn reconciliation_required(detail: String) -> Error {
    Error(format!(
        "restore record reconciliation required: {detail}. An earlier restore of this catalog cannot be checked, so another restore could rotate an already published catalog again and discard writes made since; inspect, repair or remove the record by hand before restoring"
    ))
}

/// A failure after the publication rename. The restore is committed; a rerun
/// with the same backup recognizes it and never rotates again.
fn committed_failure(incarnation: &str, step: &str, error: impl std::fmt::Display) -> Error {
    Error(format!(
        "restore committed: the catalog was published under incarnation {incarnation}, but {step} failed: {error}. Rerun the same restore to confirm durability; it recognizes the published state and does not rotate the incarnation again"
    ))
}

/// One restore record found beside the catalog, with its held directory.
struct Record {
    name: String,
    directory: Dir,
    value: Value,
}

/// Restore records for this catalog left by earlier runs. Every staging
/// directory and record is opened through held descriptors without following
/// symbolic links. A staging directory without a record never reached
/// rotation and so cannot have been published; every other enumeration, open,
/// read or parse failure is a refusal rather than an absent record.
fn previous_records(
    parent: &Dir,
    target: &Path,
    target_name: &str,
    database_id: &Value,
) -> Result<Vec<Record>> {
    use std::io::Read;
    let prefix = format!(".{target_name}.restore-");
    let listing = |error: std::io::Error| {
        reconciliation_required(format!("cannot list {}: {error}", parent.path.display()))
    };
    let mut names = Vec::new();
    for entry in fs::read_dir(&parent.path).map_err(listing)? {
        let name = entry
            .map_err(listing)?
            .file_name()
            .to_string_lossy()
            .into_owned();
        if name.starts_with(&prefix) {
            names.push(name);
        }
    }
    // The listing came from the pathname; it must have been the held directory.
    parent
        .require_unchanged()
        .map_err(|error| reconciliation_required(error.0))?;
    names.sort();
    let mut records = Vec::new();
    for name in names {
        let directory = parent.open_dir_at(&name).map_err(|error| {
            reconciliation_required(format!(
                "cannot open {} without following a symbolic link: {error}",
                parent.child(&name).display()
            ))
        })?;
        let path = directory.child("manifest.json");
        let mut file = match directory.open_file_at("manifest.json") {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(reconciliation_required(format!(
                    "cannot read {}: {error}",
                    path.display()
                )));
            }
        };
        let regular = file
            .metadata()
            .map(|metadata| {
                use std::os::unix::fs::MetadataExt;
                metadata.file_type().is_file() && metadata.nlink() == 1
            })
            .unwrap_or(false);
        if !regular {
            return Err(reconciliation_required(format!(
                "{} is not a unique regular file",
                path.display()
            )));
        }
        let mut raw = Vec::new();
        file.read_to_end(&mut raw).map_err(|error| {
            reconciliation_required(format!("cannot read {}: {error}", path.display()))
        })?;
        let value: Value = serde_json::from_slice(&raw).map_err(|error| {
            reconciliation_required(format!("{} is not valid JSON: {error}", path.display()))
        })?;
        if let Err(problem) = validate_record(&value, target, database_id) {
            return Err(reconciliation_required(format!(
                "{} is not a complete restore record for this catalog: {problem}",
                path.display()
            )));
        }
        records.push(Record {
            name,
            directory,
            value,
        });
    }
    Ok(records)
}

/// Read the backup manifest through the held backup directory.
fn read_backup_manifest(backup: &Dir, parent: &Dir, target_name: &str) -> Result<Value> {
    use std::io::Read;
    let mut raw = Vec::new();
    open_backup_file(backup, "manifest.json", parent, target_name)?
        .take(64 * 1024 * 1024)
        .read_to_end(&mut raw)?;
    let receipt = catabolic_core::decode_json_bytes(&raw)?;
    if receipt["contract"] != BACKUP_CONTRACT {
        return Err(Error(
            "not a native backup: manifest contract is not catabolic-native-backup-v1; restore legacy snapshots to a new path instead".into(),
        ));
    }
    Ok(receipt)
}

/// Controlled restore of a native catalog from a verified native backup.
///
/// The operator asserts quiescence exactly as for native upgrade and journal
/// conversion. Restore takes process-wide and cross-process lifecycle
/// exclusion first, then reads the backup only through a held directory
/// descriptor into a private staged copy, which alone is verified and
/// published. Every file operation runs through held directory descriptors;
/// SQLite, which opens files by name, is only pointed at names whose
/// directories are rechecked around each open. The live catalog is held under
/// an exclusive SQLite lock from verification to the rename, the staged copy
/// from its final verification to the rename, and the rename itself is bound
/// to the locked staged inode. Anything that fails before the rename leaves
/// the live catalog untouched and removes the staging directory; any failure
/// after it is reported as a committed restore that a rerun confirms.
pub fn restore(
    target: &Path,
    backup_directory: &Path,
    quiescence: migration::NativeUpgradeQuiescence,
    checkpoint: &dyn Fn(&str) -> Result<()>,
) -> Result<Value> {
    quiescence.require()?;
    let target = database_path(target)?;
    // Lifecycle exclusion comes before anything in the backup is read.
    let _in_process = match RESTORE_IN_PROCESS.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            return Err(Error(
                "another controlled restore is running in this process".into(),
            ));
        }
    };
    let admission = crate::native_admission_exclusive(&target)?;
    let _writer = crate::writer_lock(&target)?;
    let target_name = target
        .file_name()
        .expect("database name")
        .to_string_lossy()
        .into_owned();
    let parent = Dir::open(target.parent().expect("database parent")).map_err(|error| {
        reconciliation_required(format!("cannot open the catalog directory: {error}"))
    })?;
    let live_entry = parent
        .entry(&target_name)
        .filter(|entry| entry.regular && entry.links == 1)
        .ok_or_else(|| Error("live catalog is not a unique regular file".into()))?;
    let live_file = live_entry.identity;

    // The backup is read only through this held directory descriptor; its
    // pathname is resolved once, before it is opened, and never used again.
    let backup_path = std::path::absolute(backup_directory)?.canonicalize()?;
    let backup = Dir::open(&backup_path)?;
    let snapshot_name = "snapshot.sqlite3";
    let snapshot_path = backup_path.join(snapshot_name);
    let receipt = read_backup_manifest(&backup, &parent, &target_name)?;
    checkpoint(BACKUP_OPENED)?;
    if receipt["backup"].as_str() != snapshot_path.to_str() {
        return Err(Error(
            "native backup manifest names a different snapshot".into(),
        ));
    }
    let mut snapshot_file = open_backup_file(&backup, snapshot_name, &parent, &target_name)?;

    // Records are read and classified before ANY SQLite access to the live
    // path: opening it could replay and delete a hot journal, altering a
    // preserved unexpected file and destroying evidence.
    let mut records = previous_records(&parent, &target, &target_name, &receipt["database_id"])?;
    let volume = crate::source::volume_uuid(&parent.file).ok_or_else(|| {
        Error(
            "controlled restore requires a filesystem that reports a volume UUID, so that restore records stay valid across reboots".into(),
        )
    })?;
    let live_json = durable_identity(&volume, live_file);
    for record in &mut records {
        let path = record.directory.child("manifest.json");
        match record.value["status"].as_str() {
            Some("publishing") if record.value["staged_identity"] == live_json => {}
            Some("publishing") if record.value["live_identity"] == live_json => {
                // The rename never happened.
                record.value["status"] = json!("abandoned");
                write_record(&parent, &record.name, &record.directory, &record.value)?;
            }
            Some("publishing") => {
                return Err(reconciliation_required(format!(
                    "{} records a publication whose outcome cannot be determined: the live catalog is neither the previous nor the staged file",
                    path.display()
                )));
            }
            Some("substitution_detected") => {
                return Err(reconciliation_required(format!(
                    "{} records a detected substitution of the published catalog; verify {} against its previous_sha256 and retained_identity and restore it by hand",
                    path.display(),
                    record.value["retained"].as_str().unwrap_or_default()
                )));
            }
            _ => {}
        }
    }

    // Held until after the rename: no SQLite user can read or write meanwhile.
    parent.require_unchanged()?;
    let live = crate::journal_conversion::connect_exclusive(&target)?;
    if parent.entry(&target_name).map(|entry| entry.identity) != Some(live_file) {
        return Err(Error("live catalog was replaced during restore".into()));
    }
    let journal: String = live.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if journal == "wal" {
        return Err(Error(
            "controlled restore requires a rollback-journal live catalog; run db convert-journal --to delete first".into(),
        ));
    }
    live.execute_batch("BEGIN")?;
    let current = migration::validate(&live, false)?;
    let current_identity = identity_manifest::manifest(&live)?;
    live.execute_batch("ROLLBACK")?;
    if current["database_id"] != receipt["database_id"]
        || current_identity["catalog"]["catalog_id"] != receipt["catalog_id"]
    {
        return Err(Error(
            "native backup belongs to another catalog; refusing to restore it over this one".into(),
        ));
    }
    let previous_incarnation = current_identity["catalog"]["incarnation"]
        .as_str()
        .ok_or_else(|| Error("live catalog has no native incarnation".into()))?
        .to_owned();
    // A record whose rotated incarnation is the live one, published at this
    // very inode, proves that this backup was already published; never rotate
    // it a second time.
    if let Some(mut record) = records.into_iter().find(|record| {
        record.value["incarnation"] == previous_incarnation.as_str()
            && matches!(
                record.value["status"].as_str(),
                Some("publishing" | "published")
            )
    }) {
        let path = record.directory.child("manifest.json");
        if record.value["staged_identity"] != live_json {
            return Err(reconciliation_required(format!(
                "{} names the live incarnation, but the live catalog is not the file it published",
                path.display()
            )));
        }
        if record.value["backup_sha256"] != receipt["sha256"] {
            return Err(Error(
                "the live catalog was published by a restore of a different backup; inspect it before restoring again".into(),
            ));
        }
        // An earlier run may have stopped before or during the directory
        // fsync. Retry it under the held exclusion; only its success makes
        // the publication durable, and its failure is an error.
        let retried = checkpoint(RERUN_FSYNC).and_then(|()| parent.fsync());
        let durability = json!(if retried.is_ok() {
            "durable"
        } else {
            "uncertain"
        });
        if record.value["status"] != "published" || record.value["durability"] != durability {
            record.value["status"] = json!("published");
            record.value["durability"] = durability.clone();
            write_record(&parent, &record.name, &record.directory, &record.value).map_err(
                |error| {
                    committed_failure(
                        &previous_incarnation,
                        "finalizing the restore record",
                        error,
                    )
                },
            )?;
        }
        if let Err(error) = retried {
            return Err(committed_failure(
                &previous_incarnation,
                "retrying the publication directory fsync",
                error,
            ));
        }
        return Ok(json!({
            "database":target, "database_id":receipt["database_id"],
            "catalog_id":receipt["catalog_id"], "schema":receipt["schema"],
            "previous_incarnation":record.value["previous_incarnation"], "incarnation":previous_incarnation,
            "backup":snapshot_path, "backup_sha256":receipt["sha256"], "retained":record.value["retained"],
            "restore_record":path, "restored":true, "already_published":true,
            "durability":"durable", "warnings":[],
        }));
    }

    let staging_name = format!(".{target_name}.restore-{}", Uuid::new_v4().simple());
    let staging = parent.create_dir_at(&staging_name)?;
    let staged_name = "catalog.sqlite3";
    let staged_path = staging.child(staged_name);
    // Every SQLite open by name in the staging directory is bracketed by this
    // check: the directories are the held ones and the staged file is the one
    // created through the held descriptor.
    let staged_check = |staged_file: (u64, u64)| -> Result<()> {
        parent.require_unchanged()?;
        parent.require_child(&staging_name, &staging)?;
        staging.require_unchanged()?;
        match staging.entry(staged_name) {
            Some(entry) if entry.regular && entry.links == 1 && entry.identity == staged_file => {
                Ok(())
            }
            _ => Err(Error(
                "staged catalog was replaced during restore; refusing to publish".into(),
            )),
        }
    };
    let prepared = (|| -> Result<(String, Value, PathBuf, rusqlite::Connection, fs::File)> {
        // Copy the snapshot from the held backup descriptor into a private
        // file created through the held staging descriptor; verify only that.
        let mut staged = staging.create_file_at(staged_name)?;
        let staged_file = descriptor_identity(&staged)?;
        {
            use sha2::{Digest, Sha256};
            use std::io::{Read, Write};
            let mut digest = Sha256::new();
            let mut buffer = vec![0u8; 1024 * 1024];
            loop {
                let read = snapshot_file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
                staged.write_all(&buffer[..read])?;
            }
            staged.sync_all()?;
            if receipt["sha256"].as_str() != Some(format!("{:x}", digest.finalize()).as_str()) {
                return Err(Error(
                    "native backup snapshot does not match its manifest checksum".into(),
                ));
            }
        }
        staging.fsync()?;
        checkpoint("staged")?;

        staged_check(staged_file)?;
        let (identity, snapshot_tables) = {
            let db = connect(&staged_path, false)?;
            db.execute_batch("BEGIN")?;
            let info = migration::validate(&db, true)?;
            let identity = identity_manifest::manifest(&db)?;
            let tables = migration::snapshot(&db)?;
            drop(db);
            let schema = info["schema"].as_u64().expect("validated schema") as u32;
            if !migration::is_supported_native_schema(schema)
                || receipt["schema"] != schema
                || receipt["database_id"] != info["database_id"]
                || receipt["identity_manifest"] != identity
                || receipt["catalog_id"] != identity["catalog"]["catalog_id"]
                || receipt["incarnation"] != identity["catalog"]["incarnation"]
            {
                return Err(Error(
                    "native backup snapshot does not match its manifest identity".into(),
                ));
            }
            identity_manifest::validate(&identity).map_err(Error)?;
            (identity, tables)
        };
        staged_check(staged_file)?;
        checkpoint("verified")?;

        // Copied through the live connection with SQLite's backup API. No
        // separate descriptor may ever be opened and closed on the live (or
        // staged) file while a connection holds it: on POSIX, closing any
        // descriptor drops every fcntl lock this process holds on the inode.
        let retained_name = "previous.sqlite3";
        let retained = staging.child(retained_name);
        staged_check(staged_file)?;
        migration::copy(&live, &retained)?;
        staged_check(staged_file)?;
        let (retained_file, previous_sha256) = {
            use std::io::Read;
            let mut file = staging.open_file_at(retained_name)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            file.sync_all()?;
            (descriptor_identity(&file)?, sha256(&bytes))
        };
        if staging.entry(retained_name).map(|entry| entry.identity) != Some(retained_file) {
            return Err(Error("retained copy was replaced during restore".into()));
        }
        staging.fsync()?;
        checkpoint("retained")?;

        let snapshot_incarnation = identity["catalog"]["incarnation"]
            .as_str()
            .unwrap_or_default();
        staged_check(staged_file)?;
        let (incarnation, rotated_state) = rotate(
            &staged_path,
            &snapshot_tables,
            &identity,
            &[previous_incarnation.as_str(), snapshot_incarnation],
        )?;
        staged_check(staged_file)?;
        require_worker_ownership_resolved(&staged_path)?;
        staged_check(staged_file)?;
        staged.sync_all()?;
        let record = json!({
            "contract":RESTORE_CONTRACT, "status":"staged", "database":target,
            "database_id":identity["database_id"], "catalog_id":identity["catalog"]["catalog_id"],
            "previous_incarnation":previous_incarnation, "incarnation":incarnation,
            "backup":snapshot_path, "backup_sha256":receipt["sha256"], "retained":retained,
            "live_identity":durable_identity(&volume, live_file),
            "staged_identity":durable_identity(&volume, staged_file),
            "retained_identity":durable_identity(&volume, retained_file),
            "previous_sha256":previous_sha256,
        });
        write_record(&parent, &staging_name, &staging, &record)?;
        checkpoint("rotated")?;

        // From here to the rename the staged file is exclusively locked, and
        // it must be exactly the rotated file, row for row and object for
        // object. The descriptor `staged` stays open until after the staged
        // connection closes, and pins the inode the rename must publish.
        staged_check(staged_file)?;
        let protected = crate::journal_conversion::connect_exclusive(&staged_path)?;
        staged_check(staged_file)?;
        protected.execute_batch("BEGIN")?;
        // The complete native validation (schema signature, history checksums,
        // integrity, foreign keys and native state) and the complete state
        // comparison both run under the held lock, immediately before the rename.
        let verified = migration::validate(&protected, true)
            .map_err(|error| {
                Error(format!(
                    "staged catalog failed verification before publication: {error}"
                ))
            })
            .and_then(|_| complete_state(&protected));
        protected.execute_batch("ROLLBACK")?;
        let state = verified?;
        if state != rotated_state || state["identity"]["catalog"]["incarnation"] != incarnation {
            return Err(Error(
                "staged catalog changed after rotation; refusing to publish".into(),
            ));
        }
        checkpoint("protected")?;
        checkpoint("publishing")?;
        // The final checks follow every observable checkpoint; both connections
        // and the staged descriptor stay open through them and the rename.
        staged_check(staged_file)?;
        admission.validate_path()?;
        if parent.entry(&target_name).map(|entry| entry.identity) != Some(live_file) {
            return Err(Error(
                "live catalog was replaced during restore; refusing to publish".into(),
            ));
        }
        for suffix in ["-journal", "-wal", "-shm"] {
            if parent.entry(&format!("{target_name}{suffix}")).is_some() {
                return Err(Error(format!(
                    "live catalog has a {suffix} file; refusing to publish"
                )));
            }
        }
        Ok((incarnation, record, retained, protected, staged))
    })();
    let (incarnation, mut record, retained, protected, staged) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            // Nothing was published; the live catalog is untouched.
            remove_staging(&parent, &staging_name, &staging);
            return Err(error);
        }
    };

    // A failed rename published nothing: close both connections and remove
    // only this run's unpublished staging directory. The record says
    // `publishing` first, so a crash here is classified on rerun by identity.
    record["status"] = json!("publishing");
    let staged_file = descriptor_identity(&staged)?;
    if let Err(error) = write_record(&parent, &staging_name, &staging, &record)
        .and_then(|()| checkpoint(RENAME_WINDOW))
        .and_then(|()| parent.require_child(&staging_name, &staging))
        .and_then(|()| staging.rename_at(staged_name, &parent, &target_name))
    {
        drop(protected);
        drop(live);
        remove_staging(&parent, &staging_name, &staging);
        return Err(Error(format!(
            "publication rename failed; the live catalog is unchanged: {error}"
        )));
    }
    // Bind the rename to the locked inode. Any check leaves a window before
    // the rename; a substituted file is never reported as published, and
    // never overwritten automatically.
    if parent.entry(&target_name).map(|entry| entry.identity) != Some(staged_file) {
        drop(protected);
        drop(staged);
        drop(live);
        record["status"] = json!("substitution_detected");
        let unrecorded = write_record(&parent, &staging_name, &staging, &record)
            .err()
            .map(|error| format!(" (the record could not be updated: {error})"))
            .unwrap_or_default();
        return Err(reconciliation_required(format!(
            "hostile substitution detected: the file published at the live path was not the verified staged catalog{unrecorded}. The unexpected live file and its sidecars are preserved and nothing was rolled back. To recover, quiesce every user, check the live path for sidecars, verify {} against the record's previous_sha256 and retained_identity, and restore it over the live path by hand",
            retained.display()
        )));
    }
    drop(protected);
    drop(staged);
    drop(live);

    // The single commit point has passed. Every failure from here on is a
    // committed restore that a rerun confirms.
    checkpoint("renamed")
        .map_err(|error| committed_failure(&incarnation, "the step after the rename", error))?;
    parent.fsync().map_err(|error| {
        committed_failure(&incarnation, "the publication directory fsync", error)
    })?;
    checkpoint("published").map_err(|error| {
        committed_failure(&incarnation, "the step after the directory fsync", error)
    })?;
    record["status"] = json!("published");
    record["durability"] = json!("durable");
    checkpoint("finalizing")
        .and_then(|()| write_record(&parent, &staging_name, &staging, &record))
        .map_err(|error| committed_failure(&incarnation, "finalizing the restore record", error))?;
    checkpoint("finalized")
        .map_err(|error| committed_failure(&incarnation, "the step after finalizing", error))?;
    let restored = (|| -> Result<Value> {
        let db = connect(&target, false)?;
        db.execute_batch("BEGIN")?;
        migration::validate(&db, true)?;
        let restored = identity_manifest::manifest(&db)?;
        drop(db);
        if restored["catalog"]["incarnation"] != incarnation {
            return Err(Error(
                "published catalog does not carry the rotated incarnation".into(),
            ));
        }
        identity_manifest::require_preserved(&receipt["identity_manifest"], &restored)?;
        Ok(restored)
    })()
    .map_err(|error| committed_failure(&incarnation, "post-publication verification", error))?;
    Ok(json!({
        "database":target, "database_id":receipt["database_id"],
        "catalog_id":receipt["catalog_id"], "schema":receipt["schema"],
        "previous_incarnation":previous_incarnation, "incarnation":incarnation,
        "generation":restored["catalog"]["generation"], "backup":snapshot_path,
        "backup_sha256":receipt["sha256"], "retained":retained,
        "restore_record":staging.child("manifest.json"), "restored":true,
        "already_published":false, "durability":"durable", "warnings":[],
    }))
}
