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
/// and every later stage reports a committed restore.
pub const RESTORE_STAGES: [&str; 10] = [
    "verified",
    "retained",
    "staged",
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
/// Checkpoint between the final pathname checks and the publication rename.
/// Only tests use it, to model a substitution in that unavoidable window.
pub const RENAME_WINDOW: &str = "rename_window";

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Read and verify a native backup directory: its manifest, the snapshot's
/// SHA-256, full validation, and the identity manifest fingerprint.
fn verified_backup(directory: &Path) -> Result<(Value, PathBuf, Value)> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let directory = directory.canonicalize()?;
    let mut raw = Vec::new();
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("manifest.json"))
        .map_err(|error| Error(format!("cannot read native backup manifest: {error}")))?
        .take(64 * 1024 * 1024)
        .read_to_end(&mut raw)?;
    let receipt = catabolic_core::decode_json_bytes(&raw)?;
    if receipt["contract"] != BACKUP_CONTRACT {
        return Err(Error(
            "not a native backup: manifest contract is not catabolic-native-backup-v1; restore legacy snapshots to a new path instead".into(),
        ));
    }
    let snapshot = directory.join("snapshot.sqlite3");
    if receipt["backup"].as_str() != snapshot.to_str() {
        return Err(Error(
            "native backup manifest names a different snapshot".into(),
        ));
    }
    let metadata = fs::symlink_metadata(&snapshot)?;
    if !metadata.is_file() || std::os::unix::fs::MetadataExt::nlink(&metadata) != 1 {
        return Err(Error(
            "native backup snapshot is not a unique regular file".into(),
        ));
    }
    if receipt["sha256"].as_str() != Some(sha256(fs::read(&snapshot)?).as_str()) {
        return Err(Error(
            "native backup snapshot does not match its manifest checksum".into(),
        ));
    }
    let db = connect(&snapshot, false)?;
    db.execute_batch("BEGIN")?;
    let info = migration::validate(&db, true)?;
    let identity = identity_manifest::manifest(&db)?;
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
    Ok((receipt, snapshot, identity))
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

fn file_identity(path: &Path) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(Error(format!(
            "{} is not a unique regular file",
            path.display()
        )));
    }
    Ok((metadata.dev(), metadata.ino()))
}

fn descriptor_identity(file: &fs::File) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

fn identity_json(identity: (u64, u64)) -> Value {
    json!([identity.0, identity.1])
}

/// Byte copy of a file that the caller holds an exclusive SQLite lock on, with
/// no journal present, so the copy is consistent and byte-identical.
fn byte_copy(source: &Path, target: &Path) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let bytes = fs::read(source)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(target)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    if sha256(fs::read(target)?) != sha256(&bytes) {
        return Err(Error(
            "retained copy of the live catalog does not match it".into(),
        ));
    }
    Ok(())
}

/// Refuse if a path is no longer the exact file verified earlier, or if any
/// SQLite sidecar exists for it; a stray journal would apply to the new file.
fn require_exact_file(path: &Path, expected: (u64, u64), what: &str) -> Result<()> {
    if file_identity(path).ok() != Some(expected) {
        return Err(Error(format!(
            "{what} was replaced during restore; refusing to publish"
        )));
    }
    for suffix in ["-journal", "-wal", "-shm"] {
        if fs::symlink_metadata(sidecar(path, suffix)).is_ok() {
            return Err(Error(format!(
                "{what} has a {suffix} file; refusing to publish"
            )));
        }
    }
    Ok(())
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
        let complete = record[field]
            .as_array()
            .is_some_and(|pair| pair.len() == 2 && pair.iter().all(Value::is_u64));
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

/// A publication whose renamed file was not the verified, locked staged
/// catalog. Nothing is touched: the unexpected live file and its sidecars and
/// `previous.sqlite3` all stay where they are for the operator, and the record
/// says `substitution_detected`.
fn substitution_detected(staging: &Path, record: &mut Value) -> Error {
    record["status"] = json!("substitution_detected");
    let unrecorded = migration::manifest(staging, record)
        .err()
        .map(|error| format!(" (the record could not be updated: {error})"))
        .unwrap_or_default();
    reconciliation_required(format!(
        "hostile substitution detected: the file published at the live path was not the verified staged catalog{unrecorded}. The unexpected live file and its sidecars are preserved and nothing was rolled back. To recover, quiesce every user, check the live path for sidecars, verify {} against the record's previous_sha256 and retained_identity, and restore it over the live path by hand",
        record["retained"].as_str().unwrap_or_default()
    ))
}

/// Restore records for this catalog left by earlier runs, newest first. A
/// staging directory without a record never reached rotation and so cannot
/// have been published. Every other enumeration, read or parse failure is a
/// refusal rather than an absent record.
fn previous_records(target: &Path, database_id: &Value) -> Result<Vec<(PathBuf, Value)>> {
    let parent = target.parent().expect("database parent");
    let prefix = format!(
        ".{}.restore-",
        target.file_name().expect("database name").to_string_lossy()
    );
    let listing = |error: std::io::Error| {
        reconciliation_required(format!("cannot list {}: {error}", parent.display()))
    };
    let mut records = Vec::new();
    for entry in fs::read_dir(parent).map_err(listing)? {
        let entry = entry.map_err(listing)?;
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let path = entry.path().join("manifest.json");
        let raw = match fs::read(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(reconciliation_required(format!(
                    "cannot read {}: {error}",
                    path.display()
                )));
            }
        };
        let value: Value = serde_json::from_slice(&raw).map_err(|error| {
            reconciliation_required(format!("{} is not valid JSON: {error}", path.display()))
        })?;
        // The staging prefix names this catalog, so every record found here
        // must be complete and must identify exactly this catalog.
        if let Err(problem) = validate_record(&value, target, database_id) {
            return Err(reconciliation_required(format!(
                "{} is not a complete restore record for this catalog: {problem}",
                path.display()
            )));
        }
        records.push((path, value));
    }
    records.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(records)
}

/// Controlled restore of a native catalog from a verified native backup.
///
/// The operator asserts quiescence exactly as for native upgrade and journal
/// conversion. The restore takes the native lifecycle lease, the Catabolic
/// writer lock and an exclusive SQLite lock on the live catalog, retains a
/// copy of it, stages the snapshot beside it with a fresh incarnation, holds
/// an exclusive SQLite lock on the staged file while verifying its identity
/// and whole-row digest, and publishes it by one atomic rename followed by a
/// directory fsync. Anything that fails before the rename leaves the live
/// catalog untouched and removes the staging directory. After the rename the
/// restore is committed: later problems are returned as warnings on a
/// committed result, and a rerun recognizes the published state.
pub fn restore(
    target: &Path,
    backup_directory: &Path,
    quiescence: migration::NativeUpgradeQuiescence,
    checkpoint: &dyn Fn(&str) -> Result<()>,
) -> Result<Value> {
    quiescence.require()?;
    let (receipt, snapshot, identity) = verified_backup(backup_directory)?;
    let snapshot_tables = {
        let db = connect(&snapshot, false)?;
        db.execute_batch("BEGIN")?;
        migration::snapshot(&db)?
    };
    let target = database_path(target)?;
    let admission = crate::native_admission_exclusive(&target)?;
    let _writer = crate::writer_lock(&target)?;
    // Held until after the rename: no SQLite user can read or write meanwhile.
    let live = crate::journal_conversion::connect_exclusive(&target)?;
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
    if current["database_id"] != identity["database_id"]
        || current_identity["catalog"]["catalog_id"] != identity["catalog"]["catalog_id"]
    {
        return Err(Error(
            "native backup belongs to another catalog; refusing to restore it over this one".into(),
        ));
    }
    let previous_incarnation = current_identity["catalog"]["incarnation"]
        .as_str()
        .ok_or_else(|| Error("live catalog has no native incarnation".into()))?
        .to_owned();
    let live_file = file_identity(&target)?;
    let live_json = identity_json(live_file);
    // Records left at `publishing` are classified by the live file's inode:
    // the previous file or the staged file. Anything else, and any detected
    // substitution, is an outcome that requires operator reconciliation.
    let mut records = previous_records(&target, &identity["database_id"])?;
    for (record_path, record) in &mut records {
        let directory = record_path
            .parent()
            .expect("record directory")
            .to_path_buf();
        match record["status"].as_str() {
            Some("publishing") if record["staged_identity"] == live_json => {}
            Some("publishing") if record["live_identity"] == live_json => {
                // The rename never happened.
                record["status"] = json!("abandoned");
                migration::manifest(&directory, record)?;
            }
            Some("publishing") => {
                return Err(reconciliation_required(format!(
                    "{} records a publication whose outcome cannot be determined: the live catalog is neither the previous nor the staged file",
                    record_path.display()
                )));
            }
            Some("substitution_detected") => {
                return Err(reconciliation_required(format!(
                    "{} records a detected substitution of the published catalog; verify {} against its previous_sha256 and retained_identity and restore it by hand",
                    record_path.display(),
                    record["retained"].as_str().unwrap_or_default()
                )));
            }
            _ => {}
        }
    }
    // A record whose rotated incarnation is the live one, published at this
    // very inode, proves that this backup was already published; never rotate
    // it a second time.
    if let Some((record_path, mut record)) = records.into_iter().find(|(_, record)| {
        record["incarnation"] == previous_incarnation.as_str()
            && matches!(record["status"].as_str(), Some("publishing" | "published"))
    }) {
        if record["staged_identity"] != live_json {
            return Err(reconciliation_required(format!(
                "{} names the live incarnation, but the live catalog is not the file it published",
                record_path.display()
            )));
        }
        if record["backup_sha256"] != receipt["sha256"] {
            return Err(Error(
                "the live catalog was published by a restore of a different backup; inspect it before restoring again".into(),
            ));
        }
        // An earlier run may have stopped before or during the directory
        // fsync. Retry it under the held exclusion; only its success makes
        // the publication durable.
        let mut warnings = Vec::new();
        let durable = match checkpoint(RERUN_FSYNC)
            .and_then(|()| migration::fsync(target.parent().expect("database parent")))
        {
            Ok(()) => true,
            Err(error) => {
                warnings.push(format!(
                    "publication rename may not be durable; retrying the directory fsync failed: {error}"
                ));
                false
            }
        };
        let durability = json!(if durable { "durable" } else { "uncertain" });
        if record["status"] != "published" || record["durability"] != durability {
            record["status"] = json!("published");
            record["durability"] = durability.clone();
            if let Err(error) =
                migration::manifest(record_path.parent().expect("record directory"), &record)
            {
                warnings.push(format!("restore record could not be finalized: {error}"));
            }
        }
        return Ok(json!({
            "database":target, "database_id":identity["database_id"],
            "catalog_id":identity["catalog"]["catalog_id"], "schema":identity["schema"],
            "previous_incarnation":record["previous_incarnation"], "incarnation":previous_incarnation,
            "backup":snapshot, "backup_sha256":receipt["sha256"], "retained":record["retained"],
            "restore_record":record_path, "restored":true, "already_published":true,
            "durability":durability, "warnings":warnings,
        }));
    }
    checkpoint("verified")?;

    let parent = target.parent().expect("database parent").to_path_buf();
    let name = target
        .file_name()
        .expect("database name")
        .to_string_lossy()
        .into_owned();
    let staging = parent.join(format!(".{name}.restore-{}", Uuid::new_v4().simple()));
    private_directory(&staging)?;
    let staged = staging.join("catalog.sqlite3");
    let prepared = (|| -> Result<(String, Value, PathBuf, rusqlite::Connection, fs::File)> {
        let retained = staging.join("previous.sqlite3");
        // The exclusive lock is held and no journal exists, so a byte copy is
        // consistent, and a substitution rollback restores identical bytes.
        require_exact_file(&target, live_file, "live catalog")?;
        byte_copy(&target, &retained)?;
        migration::fsync(&staging)?;
        checkpoint("retained")?;

        {
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            let bytes = fs::read(&snapshot)?;
            if receipt["sha256"].as_str() != Some(sha256(&bytes).as_str()) {
                return Err(Error(
                    "native backup snapshot changed during restore".into(),
                ));
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&staged)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        migration::fsync(&staging)?;
        let staged_file = file_identity(&staged)?;
        checkpoint("staged")?;

        let snapshot_incarnation = identity["catalog"]["incarnation"]
            .as_str()
            .unwrap_or_default();
        let (incarnation, rotated_state) = rotate(
            &staged,
            &snapshot_tables,
            &identity,
            &[previous_incarnation.as_str(), snapshot_incarnation],
        )?;
        require_worker_ownership_resolved(&staged)?;
        migration::fsync(&staged)?;
        let record = json!({
            "contract":RESTORE_CONTRACT, "status":"staged", "database":target,
            "database_id":identity["database_id"], "catalog_id":identity["catalog"]["catalog_id"],
            "previous_incarnation":previous_incarnation, "incarnation":incarnation,
            "backup":snapshot, "backup_sha256":receipt["sha256"], "retained":retained,
            "live_identity":identity_json(live_file), "staged_identity":identity_json(staged_file),
            "retained_identity":identity_json(file_identity(&retained)?),
            "previous_sha256":sha256(fs::read(&retained)?),
        });
        migration::manifest(&staging, &record)?;
        checkpoint("rotated")?;

        // From here to the rename the staged file is exclusively locked, and
        // it must be exactly the rotated file, row for row.
        require_exact_file(&staged, staged_file, "staged catalog")?;
        // Held through the rename: the published pathname must be this inode.
        let staged_descriptor = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&staged)?
        };
        if descriptor_identity(&staged_descriptor)? != staged_file {
            return Err(Error(
                "staged catalog was replaced during restore; refusing to publish".into(),
            ));
        }
        let protected = crate::journal_conversion::connect_exclusive(&staged)?;
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
        require_exact_file(&staged, staged_file, "staged catalog")?;
        admission.validate_path()?;
        require_exact_file(&target, live_file, "live catalog")?;
        Ok((incarnation, record, retained, protected, staged_descriptor))
    })();
    let (incarnation, mut record, retained, protected, staged_descriptor) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            // Nothing was published; the live catalog is untouched.
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };

    // The single commit point. Every result from here on reports a committed
    // restore; durability is uncertain until the directory fsync succeeds.
    // A failed rename published nothing: close both connections and remove
    // only this run's unpublished staging directory. The record says
    // `publishing` first, so a crash here is classified on rerun by inode.
    record["status"] = json!("publishing");
    if let Err(error) = migration::manifest(&staging, &record)
        .and_then(|()| checkpoint(RENAME_WINDOW))
        .and_then(|()| fs::rename(&staged, &target).map_err(Error::from))
    {
        drop(protected);
        drop(live);
        let _ = fs::remove_dir_all(&staging);
        return Err(Error(format!(
            "publication rename failed; the live catalog is unchanged: {error}"
        )));
    }
    // Bind the rename to the locked inode. Any pathname check leaves a window
    // before the rename; a substituted file is never reported as published,
    // and never overwritten automatically.
    let locked = descriptor_identity(&staged_descriptor).ok();
    if file_identity(&target).ok().is_none() || file_identity(&target).ok() != locked {
        drop(protected);
        drop(staged_descriptor);
        drop(live);
        return Err(substitution_detected(&staging, &mut record));
    }
    drop(protected);
    drop(staged_descriptor);
    drop(live);
    let mut warnings = Vec::new();
    let mut durable = match checkpoint("renamed") {
        Ok(()) => true,
        Err(error) => {
            warnings.push(format!("failure after publication rename: {error}"));
            false
        }
    };
    if durable {
        if let Err(error) = migration::fsync(&parent) {
            warnings.push(format!(
                "publication rename may not be durable; the directory fsync failed: {error}"
            ));
            durable = false;
        }
    }
    if durable {
        if let Err(error) = checkpoint("published") {
            warnings.push(format!("failure after durable publication: {error}"));
        }
    }
    record["status"] = json!("published");
    record["durability"] = json!(if durable { "durable" } else { "uncertain" });
    if let Err(error) =
        checkpoint("finalizing").and_then(|()| migration::manifest(&staging, &record))
    {
        warnings.push(format!("restore record could not be finalized: {error}"));
    } else if let Err(error) = checkpoint("finalized") {
        warnings.push(format!(
            "failure after finalizing the restore record: {error}"
        ));
    }
    let verified = (|| -> Result<Value> {
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
        identity_manifest::require_preserved(&identity, &restored)?;
        Ok(restored)
    })();
    let generation = match verified {
        Ok(restored) => restored["catalog"]["generation"].clone(),
        Err(error) => {
            warnings.push(format!("post-publication verification failed: {error}"));
            Value::Null
        }
    };
    Ok(json!({
        "database":target, "database_id":identity["database_id"],
        "catalog_id":identity["catalog"]["catalog_id"], "schema":identity["schema"],
        "previous_incarnation":previous_incarnation, "incarnation":incarnation,
        "generation":generation, "backup":snapshot,
        "backup_sha256":receipt["sha256"], "retained":retained,
        "restore_record":staging.join("manifest.json"), "restored":true,
        "already_published":false,
        "durability":if durable { "durable" } else { "uncertain" }, "warnings":warnings,
    }))
}
