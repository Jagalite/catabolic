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

/// Durable restore stages, in order. `published` follows the atomic rename,
/// which is the single commit point of a restore.
pub const RESTORE_STAGES: [&str; 5] = ["verified", "retained", "staged", "rotated", "published"];

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

/// Rotate the staged copy's incarnation through the schema's restore-scoped
/// permit and verify that nothing else changed.
fn rotate(
    staged: &Path,
    snapshot_tables: &Value,
    identity: &Value,
    avoid: &[&str],
) -> Result<String> {
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
    let mut tables = migration::snapshot(&db)?;
    let after = identity_manifest::manifest(&db)?;
    drop(db);
    let mut expected = snapshot_tables.clone();
    for object in [&mut tables, &mut expected] {
        object
            .as_object_mut()
            .expect("snapshot object")
            .remove("catalog_state");
    }
    if tables != expected
        || after["catalog"]["incarnation"] != incarnation
        || after["catalog"]["generation"] != identity["catalog"]["generation"]
    {
        return Err(Error(
            "incarnation rotation changed restored catalog contents".into(),
        ));
    }
    identity_manifest::require_preserved(identity, &after)?;
    Ok(incarnation)
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

/// Controlled restore of a native catalog from a verified native backup.
///
/// The operator asserts quiescence exactly as for native upgrade and journal
/// conversion. The restore takes the native lifecycle lease, the Catabolic
/// writer lock and an exclusive SQLite lock on the live catalog, retains a
/// copy of the live catalog, stages the snapshot beside it with a fresh
/// incarnation, and publishes it by one atomic rename followed by a directory
/// fsync. Anything that fails before the rename leaves the live catalog
/// untouched. Old sessions, permits and receipts name the old incarnation.
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
    let live_file = {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(&target)?;
        (metadata.dev(), metadata.ino())
    };
    checkpoint("verified")?;

    let parent = target.parent().expect("database parent").to_path_buf();
    let name = target
        .file_name()
        .expect("database name")
        .to_string_lossy()
        .into_owned();
    let staging = parent.join(format!(".{name}.restore-{}", Uuid::new_v4().simple()));
    private_directory(&staging)?;
    let retained = staging.join("previous.sqlite3");
    migration::copy(&live, &retained)?;
    migration::fsync(&retained)?;
    migration::fsync(&staging)?;
    checkpoint("retained")?;

    let staged = staging.join("catalog.sqlite3");
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
    checkpoint("staged")?;

    let snapshot_incarnation = identity["catalog"]["incarnation"]
        .as_str()
        .unwrap_or_default();
    let incarnation = rotate(
        &staged,
        &snapshot_tables,
        &identity,
        &[previous_incarnation.as_str(), snapshot_incarnation],
    )?;
    if let Err(error) = require_worker_ownership_resolved(&staged) {
        // Nothing was published; the live catalog is untouched.
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    migration::fsync(&staged)?;
    let mut record = json!({
        "contract":RESTORE_CONTRACT, "status":"staged", "database":target,
        "database_id":identity["database_id"], "catalog_id":identity["catalog"]["catalog_id"],
        "previous_incarnation":previous_incarnation, "incarnation":incarnation,
        "backup":snapshot, "backup_sha256":receipt["sha256"], "retained":retained,
    });
    migration::manifest(&staging, &record)?;
    checkpoint("rotated")?;

    // Publish only if the live pathname, its lease and its sidecars are still
    // exactly what was verified; a stray journal would be applied to the new file.
    {
        use std::os::unix::fs::MetadataExt;
        admission.validate_path()?;
        let metadata = fs::symlink_metadata(&target)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || (metadata.dev(), metadata.ino()) != live_file
        {
            return Err(Error("live catalog was replaced during restore".into()));
        }
        for suffix in ["-journal", "-wal", "-shm"] {
            if fs::symlink_metadata(sidecar(&target, suffix)).is_ok() {
                return Err(Error(format!(
                    "live catalog has a {suffix} file; refusing to publish a restore over it"
                )));
            }
        }
    }
    fs::rename(&staged, &target)?;
    migration::fsync(&parent)?;
    drop(live);
    record["status"] = json!("published");
    migration::manifest(&staging, &record)?;
    checkpoint("published")?;

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
    Ok(json!({
        "database":target, "database_id":identity["database_id"],
        "catalog_id":identity["catalog"]["catalog_id"], "schema":identity["schema"],
        "previous_incarnation":previous_incarnation, "incarnation":incarnation,
        "generation":restored["catalog"]["generation"], "backup":snapshot,
        "backup_sha256":receipt["sha256"], "retained":retained,
        "restore_record":staging.join("manifest.json"), "restored":true,
    }))
}
