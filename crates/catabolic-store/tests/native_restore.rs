// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Verified native backups and controlled restore with incarnation rotation,
//! on disposable native catalogs.
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
use catabolic_store::{
    Store,
    catalog_commands::{self as commands, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority},
    connect, identity_manifest, migration, native_restore, native_worker_inventory,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn native() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.sqlite3");
    migration::initialize_native(&path).unwrap();
    (root, path)
}
fn manifest(path: &Path) -> Value {
    let mut value = identity_manifest::manifest_path(path).unwrap();
    value.as_object_mut().unwrap().remove("database");
    value
}

#[test]
fn native_backup_is_verified_private_and_bound_to_its_catalog() {
    let (root, path) = native();
    let before = fs::read(&path).unwrap();
    let backups = root.path().join("backups");
    fs::create_dir(&backups).unwrap();
    let receipt = native_restore::backup(&path, Some(&backups)).unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(receipt["contract"], native_restore::BACKUP_CONTRACT);
    assert_eq!(receipt["schema"], migration::NATIVE_SCHEMA_VERSION);
    let live = manifest(&path);
    assert_eq!(receipt["identity_manifest"], live);
    assert_eq!(receipt["catalog_id"], live["catalog"]["catalog_id"]);
    assert_eq!(receipt["incarnation"], live["catalog"]["incarnation"]);
    let snapshot = PathBuf::from(receipt["backup"].as_str().unwrap());
    assert_eq!(
        receipt["sha256"],
        catabolic_core::sha256(fs::read(&snapshot).unwrap())
    );
    assert_eq!(manifest(&snapshot)["fingerprint"], live["fingerprint"]);
    let directory = snapshot.parent().unwrap();
    assert_eq!(
        fs::metadata(directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let written: Value =
        serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(written, receipt);
    // The default parent is created next to the catalog.
    let default = native_restore::backup(&path, None).unwrap();
    assert!(
        Path::new(default["backup"].as_str().unwrap()).starts_with(
            fs::canonicalize(root.path())
                .unwrap()
                .join("native.sqlite3.backups")
        )
    );
}

#[test]
fn native_backup_refuses_legacy_catalogs_and_missing_parents() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy.sqlite3");
    migration::initialize(&legacy).unwrap();
    assert!(
        native_restore::backup(&legacy, None)
            .unwrap_err()
            .0
            .contains("native backup requires a native catalog")
    );
    assert!(!PathBuf::from(format!("{}.backups", legacy.display())).exists());
    let (_root, path) = native();
    assert!(
        native_restore::backup(&path, Some(&root.path().join("missing")))
            .unwrap_err()
            .0
            .contains("must be an existing directory")
    );
    // A live session does not block a consistent backup.
    let session = Store::open(&path, false, false).unwrap();
    native_restore::backup(&path, None).unwrap();
    drop(session);
    migration::validate(&connect(&path, false).unwrap(), true).unwrap();
}

fn quiescent() -> migration::NativeUpgradeQuiescence {
    migration::NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}
fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row(
            "SELECT generation FROM catalog_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap()
}
fn envelope(store: &Store, operation: &str, id: &str) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, "test-host", operation).unwrap(),
        store.catalog_incarnation.clone().unwrap(),
        generation(store),
        &json!({"operation":"item_put","item":{"id":id,"kind":"video","metadata":{"title":id}}})
            .to_string(),
    )
    .unwrap()
}
fn run(store: &Store, command: &CommandEnvelope) -> CommandOutcome {
    commands::execute(
        store,
        command,
        TrustedCommandAuthority {
            host_namespace: "test-host",
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: None,
        },
        ReceiptPolicy {
            retention: Duration::from_secs(3600),
            max_receipts: 1000,
            max_logical_bytes: 16 * 1024 * 1024,
            max_hold_records: 1000,
        },
    )
    .unwrap()
}
fn put(path: &Path, operation: &str, id: &str) {
    let store = Store::open(path, true, false).unwrap();
    let outcome = run(&store, &envelope(&store, operation, id));
    assert!(
        matches!(outcome, CommandOutcome::Committed { .. }),
        "{outcome:?}"
    );
}
fn items(path: &Path, id: &str) -> i64 {
    connect(path, false)
        .unwrap()
        .query_row("SELECT count(*) FROM items WHERE id=?", [id], |row| {
            row.get(0)
        })
        .unwrap()
}
fn incarnation(path: &Path) -> String {
    connect(path, false)
        .unwrap()
        .query_row(
            "SELECT incarnation FROM catalog_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap()
}
fn tables_without_state(path: &Path) -> Value {
    let db = connect(path, false).unwrap();
    db.execute_batch("BEGIN").unwrap();
    let mut tables = migration::snapshot(&db).unwrap();
    tables.as_object_mut().unwrap().remove("catalog_state");
    tables
}
fn staging_directories(path: &Path) -> Vec<PathBuf> {
    let prefix = format!(".{}.restore-", path.file_name().unwrap().to_string_lossy());
    let mut found = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|entry| {
            entry
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&prefix)
        })
        .collect::<Vec<_>>();
    found.sort();
    found
}
/// A catalog with one item in its backup and a second written afterwards.
fn backed_up() -> (tempfile::TempDir, PathBuf, Value) {
    let (root, path) = native();
    put(&path, "kept", "kept-item");
    let receipt = native_restore::backup(&path, None).unwrap();
    put(&path, "later", "later-item");
    (root, path, receipt)
}
fn backup_directory(receipt: &Value) -> PathBuf {
    Path::new(receipt["backup"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_path_buf()
}
fn restore(path: &Path, receipt: &Value) -> catabolic_store::Result<Value> {
    native_restore::restore(path, &backup_directory(receipt), quiescent(), &|_| Ok(()))
}

#[test]
fn restore_round_trip_publishes_the_backup_under_a_fresh_incarnation() {
    let (_root, path, receipt) = backed_up();
    let live_incarnation = incarnation(&path);
    let live_inode = fs::metadata(&path).unwrap().ino();
    let expected = tables_without_state(Path::new(receipt["backup"].as_str().unwrap()));
    let report = restore(&path, &receipt).unwrap();
    assert_eq!(report["restored"], true);
    assert_eq!(report["previous_incarnation"], live_incarnation);
    let rotated = report["incarnation"].as_str().unwrap();
    assert_eq!(rotated.len(), 32);
    assert_ne!(rotated, live_incarnation);
    assert_ne!(Some(rotated), receipt["incarnation"].as_str());
    assert_eq!(incarnation(&path), rotated);
    assert_eq!(report["generation"], receipt["generation"]);
    assert_ne!(fs::metadata(&path).unwrap().ino(), live_inode);
    assert_eq!(tables_without_state(&path), expected);
    assert_eq!(items(&path, "kept-item"), 1);
    assert_eq!(items(&path, "later-item"), 0);
    identity_manifest::require_preserved(&receipt["identity_manifest"], &manifest(&path)).unwrap();
    // The live catalog before the restore is retained, with its later write.
    let retained = Path::new(report["retained"].as_str().unwrap());
    assert_eq!(items(retained, "later-item"), 1);
    assert_eq!(incarnation(retained), live_incarnation);
    let record: Value =
        serde_json::from_slice(&fs::read(report["restore_record"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(record["status"], "published");
    assert_eq!(record["incarnation"], rotated);
    assert!(!PathBuf::from(format!("{}-journal", path.display())).exists());
    // Ordinary native admission opens the restored catalog normally.
    let store = Store::open(&path, true, false).unwrap();
    assert_eq!(store.catalog_incarnation.as_deref(), Some(rotated));
}

#[test]
fn stale_incarnation_sessions_permits_and_receipts_are_refused() {
    let (_root, path) = native();
    let store = Store::open(&path, true, false).unwrap();
    let committed = envelope(&store, "known", "known-item");
    let absent = envelope(&store, "absent", "never-written");
    let first = run(&store, &committed);
    let old = store.catalog_incarnation.clone().unwrap();
    drop(store);
    let receipt = native_restore::backup(&path, None).unwrap();

    // A live session holds shared admission, so the restore is refused.
    let session = Store::open(&path, false, false).unwrap();
    let before = fs::read(&path).unwrap();
    let error = restore(&path, &receipt).unwrap_err();
    assert!(
        error
            .0
            .contains("native catalog sessions or exclusive lifecycle operation are active"),
        "{error}"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    drop(session);

    let report = restore(&path, &receipt).unwrap();
    assert_eq!(report["previous_incarnation"], old);
    // A writer still holding the old incarnation cannot obtain a permit.
    let db = connect(&path, true).unwrap();
    let error = db
        .execute(
            "INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) SELECT 'version','stale',?",
            [&old],
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("catalog_stale_incarnation"),
        "{error}"
    );
    let error = db
        .execute_batch(&format!(
            "INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) SELECT 'restore',catalog_id,'{old}' FROM catalog_state"
        ))
        .unwrap_err();
    assert!(
        error.to_string().contains("catalog_stale_incarnation"),
        "{error}"
    );
    drop(db);
    // Commands admitted under the old incarnation: an unknown outcome is
    // reported as uncertain, and a committed one replays its original result.
    let store = Store::open(&path, true, false).unwrap();
    assert_ne!(store.catalog_incarnation.as_deref(), Some(old.as_str()));
    assert_eq!(
        run(&store, &absent),
        CommandOutcome::OutcomeUnknownAfterRestore
    );
    let CommandOutcome::Committed {
        result: original, ..
    } = first
    else {
        panic!("first command did not commit: {first:?}");
    };
    match run(&store, &committed) {
        CommandOutcome::Replay { result, .. } => assert_eq!(result, original),
        other => panic!("expected replay, got {other:?}"),
    }
    assert_eq!(items(&path, "never-written"), 0);
}

#[test]
fn mismatched_or_unverified_backups_are_refused_without_touching_the_catalog() {
    let (root, path, receipt) = backed_up();
    let before = fs::read(&path).unwrap();
    let check = |error: catabolic_store::Error, message: &str| {
        assert!(error.0.contains(message), "expected {message:?}: {error}");
        assert_eq!(fs::read(&path).unwrap(), before, "{message}");
        assert!(staging_directories(&path).is_empty(), "{message}");
    };
    check(
        native_restore::restore(
            &path,
            &backup_directory(&receipt),
            migration::NativeUpgradeQuiescence::default(),
            &|_| Ok(()),
        )
        .unwrap_err(),
        "explicit operator confirmation",
    );
    // Another catalog's genuine backup.
    let other = root.path().join("other.sqlite3");
    migration::initialize_native(&other).unwrap();
    let foreign = native_restore::backup(&other, None).unwrap();
    check(
        restore(&path, &foreign).unwrap_err(),
        "native backup belongs to another catalog",
    );
    // A snapshot whose bytes no longer match the manifest checksum.
    let tampered = native_restore::backup(&path, None).unwrap();
    let snapshot = Path::new(tampered["backup"].as_str().unwrap());
    let mut bytes = fs::read(snapshot).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(snapshot, bytes).unwrap();
    check(
        restore(&path, &tampered).unwrap_err(),
        "does not match its manifest checksum",
    );
    // A manifest whose identity evidence was edited.
    let edited = native_restore::backup(&path, None).unwrap();
    let mut forged = edited.clone();
    forged["identity_manifest"]["fingerprint"] = json!("0".repeat(64));
    fs::write(
        backup_directory(&edited).join("manifest.json"),
        serde_json::to_vec(&forged).unwrap(),
    )
    .unwrap();
    check(
        restore(&path, &edited).unwrap_err(),
        "does not match its manifest identity",
    );
    // A legacy upgrade backup is not a native backup.
    let legacy = root.path().join("legacy-backup");
    fs::create_dir(&legacy).unwrap();
    fs::write(legacy.join("manifest.json"), br#"{"status":"committed"}"#).unwrap();
    check(
        native_restore::restore(&path, &legacy, quiescent(), &|_| Ok(())).unwrap_err(),
        "not a native backup",
    );
    // A WAL live catalog must be converted first.
    connect(&path, true)
        .unwrap()
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
        .unwrap();
    let wal_bytes = fs::read(&path).unwrap();
    let error = restore(&path, &receipt).unwrap_err();
    assert!(error.0.contains("rollback-journal live catalog"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), wal_bytes);
    assert!(staging_directories(&path).is_empty());
}

/// Re-executed by `crash_at_every_restore_stage_is_recoverable`.
#[test]
fn restore_exit_worker() {
    let Ok(path) = std::env::var("CATABOLIC_TEST_RESTORE_DATABASE") else {
        return;
    };
    let backup = std::env::var("CATABOLIC_TEST_RESTORE_BACKUP").unwrap();
    let stage = std::env::var("CATABOLIC_TEST_RESTORE_STAGE").unwrap();
    let result =
        native_restore::restore(Path::new(&path), Path::new(&backup), quiescent(), &|at| {
            if at == stage {
                std::process::exit(86);
            }
            Ok(())
        });
    panic!("restore stage {stage} was not reached: {result:?}");
}

#[test]
fn crash_at_every_restore_stage_is_recoverable() {
    for stage in native_restore::RESTORE_STAGES {
        let (_root, path, receipt) = backed_up();
        let before = fs::read(&path).unwrap();
        let live_incarnation = incarnation(&path);
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "restore_exit_worker", "--nocapture"])
            .env("CATABOLIC_TEST_RESTORE_DATABASE", &path)
            .env("CATABOLIC_TEST_RESTORE_BACKUP", backup_directory(&receipt))
            .env("CATABOLIC_TEST_RESTORE_STAGE", stage)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(86), "{stage}");
        let reached = |name: &str| {
            native_restore::RESTORE_STAGES
                .iter()
                .position(|s| *s == stage)
                >= native_restore::RESTORE_STAGES
                    .iter()
                    .position(|s| *s == name)
        };
        // Staging begins after verification and is left for the operator.
        let staging = staging_directories(&path);
        assert_eq!(staging.len(), usize::from(reached("retained")), "{stage}");
        let record = staging
            .first()
            .map(|directory| directory.join("manifest.json"));
        if let Some(directory) = staging.first() {
            assert!(directory.join("previous.sqlite3").exists(), "{stage}");
            assert_eq!(
                record.as_ref().unwrap().exists(),
                reached("rotated"),
                "{stage}"
            );
        }
        migration::validate(&connect(&path, true).unwrap(), true).unwrap();
        if stage == "published" {
            // The rename is the commit point: the restore is in effect.
            let record: Value =
                serde_json::from_slice(&fs::read(record.unwrap()).unwrap()).unwrap();
            assert_eq!(record["status"], "published");
            assert_eq!(incarnation(&path), record["incarnation"].as_str().unwrap());
            assert_eq!(items(&path, "later-item"), 0);
        } else {
            // Before the rename the live catalog is exactly as it was.
            assert_eq!(fs::read(&path).unwrap(), before, "{stage}");
            assert_eq!(incarnation(&path), live_incarnation, "{stage}");
            assert_eq!(items(&path, "later-item"), 1, "{stage}");
            if reached("rotated") {
                let record: Value =
                    serde_json::from_slice(&fs::read(record.unwrap()).unwrap()).unwrap();
                assert_eq!(record["status"], "staged", "{stage}");
            }
        }
        // Rerunning the restore completes it from any interrupted stage.
        let report = restore(&path, &receipt).unwrap();
        assert_eq!(incarnation(&path), report["incarnation"].as_str().unwrap());
        assert_eq!(items(&path, "kept-item"), 1, "{stage}");
        assert_eq!(items(&path, "later-item"), 0, "{stage}");
        identity_manifest::require_preserved(&receipt["identity_manifest"], &manifest(&path))
            .unwrap();
        Store::open(&path, true, false).unwrap();
    }
}

fn staged_file(path: &Path) -> PathBuf {
    let staging = staging_directories(path);
    assert_eq!(staging.len(), 1);
    staging[0].join("catalog.sqlite3")
}

/// The staged copy is checked row for row and by file identity after
/// rotation, and is exclusively locked from then until the rename.
#[test]
fn staged_mutation_or_replacement_is_refused_before_publication() {
    let tamper = "INSERT INTO meta VALUES('restore-tamper','1')";
    let refusals: [(&str, &str, &dyn Fn(&Path)); 3] = [
        (
            "rotated",
            "staged catalog changed after rotation",
            &|staged: &Path| {
                connect(staged, true)
                    .unwrap()
                    .execute_batch(tamper)
                    .unwrap();
            },
        ),
        (
            "rotated",
            "staged catalog was replaced",
            &|staged: &Path| {
                let copy = staged.with_extension("copy");
                fs::write(&copy, fs::read(staged).unwrap()).unwrap();
                fs::rename(&copy, staged).unwrap();
            },
        ),
        (
            "protected",
            "staged catalog was replaced",
            &|staged: &Path| {
                let copy = staged.with_extension("copy");
                fs::write(&copy, fs::read(staged).unwrap()).unwrap();
                fs::remove_file(staged).unwrap();
                std::os::unix::fs::symlink(&copy, staged).unwrap();
            },
        ),
    ];
    for (stage, message, change) in refusals {
        let (_root, path, receipt) = backed_up();
        let before = fs::read(&path).unwrap();
        let error =
            native_restore::restore(&path, &backup_directory(&receipt), quiescent(), &|at| {
                if at == stage {
                    change(&staged_file(&path));
                }
                Ok(())
            })
            .unwrap_err();
        assert!(error.0.contains(message), "{stage}: {error}");
        assert_eq!(fs::read(&path).unwrap(), before, "{stage}");
        assert_eq!(items(&path, "later-item"), 1, "{stage}");
        assert!(staging_directories(&path).is_empty(), "{stage}");
    }
    // While protected, a writer cannot change the staged file at all.
    let (_root, path, receipt) = backed_up();
    let report = native_restore::restore(&path, &backup_directory(&receipt), quiescent(), &|at| {
        if at == "protected" {
            let mut writer = connect(&staged_file(&path), true);
            let refused = match writer.as_mut() {
                Ok(db) => db.execute_batch(tamper).unwrap_err().to_string(),
                Err(error) => error.to_string(),
            };
            assert!(refused.contains("database is locked"), "{refused}");
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(report["restored"], true);
    let count: i64 = connect(&path, false)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM meta WHERE key='restore-tamper'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// One observation job claimed by a native companion; `sealed` completes it
/// with the exact quiescent cleanup witness, otherwise it stays unfinished.
fn seed_observation_worker(path: &Path, sealed: bool) {
    use rusqlite::params;
    let store = Store::open(path, true, false).unwrap();
    store
        .transaction(|db| {
            let id = "00000000-0000-4000-8000-000000000001";
            let token = uuid::Uuid::new_v4().to_string();
            db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,lease_until,worker_pid,claim_token,guarded) VALUES(?,'default','media','fixture','[]',0,1,'complete',0,0,?,?,1)", params![id, std::process::id(), token])?;
            if sealed {
                let request = uuid::Uuid::new_v4().to_string();
                let nonce = uuid::Uuid::new_v4().simple().to_string();
                db.execute("INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,created_at) VALUES(?,'default','media','seed','{}',?,0)", params![request, id])?;
                db.execute("INSERT INTO native_observation_companions(reservation_nonce,catalog_id,incarnation,protocol_kind,protocol_version,helper_digest,profile,source,requester,args_digest) VALUES(?,?,?,'native_observation_no_child',1,?,'default','media','seed',?)", params![nonce, store.database_id, store.catalog_incarnation, DIGEST, DIGEST])?;
                db.execute("UPDATE native_observation_companions SET state='started',child_pid=? WHERE reservation_nonce=?", params![std::process::id(), nonce])?;
                db.execute("UPDATE native_observation_companions SET state='completed',request_id=?,demand_job_id=?,owned_job_id=?,owned_generation=1,owned_claim_token_digest=? WHERE reservation_nonce=?", params![request, id, id, catabolic_core::sha256(token), nonce])?;
                db.execute("UPDATE native_observation_companions SET state='quiescent',cleanup_kind='closed_child_exit_v1',sealed_at=CURRENT_TIMESTAMP WHERE reservation_nonce=?", [&nonce])?;
            }
            Ok(())
        })
        .unwrap();
    let inventory = native_worker_inventory::inspect(&store).unwrap();
    assert_eq!(inventory.admits_new_work(), sealed);
    assert_eq!(inventory.witnessed_observation, usize::from(sealed));
}

/// Cleanup witnesses name the incarnation that sealed them and cannot be
/// rebound, so a backup with retained worker ownership is refused before
/// publication, for completed and unfinished work alike.
#[test]
fn backups_with_retained_worker_ownership_are_refused() {
    for sealed in [true, false] {
        let (_root, path) = native();
        seed_observation_worker(&path, sealed);
        let receipt = native_restore::backup(&path, None).unwrap();
        let before = fs::read(&path).unwrap();
        let error = restore(&path, &receipt).unwrap_err();
        assert!(
            error.0.contains("retained worker ownership")
                && error
                    .0
                    .contains("observation:00000000-0000-4000-8000-000000000001"),
            "sealed={sealed}: {error}"
        );
        assert_eq!(fs::read(&path).unwrap(), before, "sealed={sealed}");
        assert!(staging_directories(&path).is_empty(), "sealed={sealed}");
        // The live catalog keeps admitting exactly what it admitted before.
        let store = Store::open(&path, false, false).unwrap();
        assert_eq!(
            native_worker_inventory::inspect(&store)
                .unwrap()
                .admits_new_work(),
            sealed
        );
    }
}
