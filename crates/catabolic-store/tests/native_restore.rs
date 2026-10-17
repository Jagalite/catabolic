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
    assert_eq!(report["already_published"], false);
    assert_eq!(report["durability"], "durable");
    assert_eq!(report["warnings"], json!([]));
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
    let substitute = std::env::var_os("CATABOLIC_TEST_RESTORE_SUBSTITUTE").is_some();
    let result =
        native_restore::restore(Path::new(&path), Path::new(&backup), quiescent(), &|at| {
            if substitute && at == native_restore::RENAME_WINDOW {
                substitute_staged(Path::new(&path), Path::new(&backup));
            }
            if at == stage {
                std::process::exit(86);
            }
            Ok(())
        });
    panic!("restore stage {stage} was not reached: {result:?}");
}

#[test]
fn crash_at_every_restore_stage_is_recoverable() {
    let stages = native_restore::RESTORE_STAGES;
    let position = |name: &str| stages.iter().position(|stage| *stage == name).unwrap();
    for stage in stages {
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
        let reached = |name: &str| position(stage) >= position(name);
        let committed = reached("renamed");
        // Staging begins after verification and survives a crash.
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
            assert_eq!(
                directory.join("catalog.sqlite3").exists(),
                reached("staged") && !committed,
                "{stage}"
            );
        }
        migration::validate(&connect(&path, true).unwrap(), true).unwrap();
        let read =
            |path: &PathBuf| -> Value { serde_json::from_slice(&fs::read(path).unwrap()).unwrap() };
        if committed {
            // The rename is the commit point: the restore is in effect.
            let published = read(record.as_ref().unwrap());
            assert_eq!(
                incarnation(&path),
                published["incarnation"].as_str().unwrap()
            );
            assert_eq!(items(&path, "later-item"), 0, "{stage}");
            let expected = if reached("finalized") {
                "published"
            } else {
                "publishing"
            };
            assert_eq!(published["status"], expected, "{stage}");
            // A rerun recognizes the published state and does not rotate again.
            let rotated = incarnation(&path);
            let rerun = restore(&path, &receipt).unwrap();
            assert_eq!(rerun["already_published"], true, "{stage}");
            assert_eq!(rerun["incarnation"], rotated.as_str(), "{stage}");
            assert_eq!(incarnation(&path), rotated, "{stage}");
            assert_eq!(
                read(record.as_ref().unwrap())["status"],
                "published",
                "{stage}"
            );
        } else {
            // Before the rename the live catalog is exactly as it was.
            assert_eq!(fs::read(&path).unwrap(), before, "{stage}");
            assert_eq!(incarnation(&path), live_incarnation, "{stage}");
            assert_eq!(items(&path, "later-item"), 1, "{stage}");
            if reached("rotated") {
                assert_eq!(
                    read(record.as_ref().unwrap())["status"],
                    "staged",
                    "{stage}"
                );
            }
            // Rerunning the restore completes it from any interrupted stage.
            let report = restore(&path, &receipt).unwrap();
            assert_eq!(report["already_published"], false, "{stage}");
            assert_eq!(incarnation(&path), report["incarnation"].as_str().unwrap());
        }
        assert_eq!(items(&path, "kept-item"), 1, "{stage}");
        assert_eq!(items(&path, "later-item"), 0, "{stage}");
        identity_manifest::require_preserved(&receipt["identity_manifest"], &manifest(&path))
            .unwrap();
        Store::open(&path, true, false).unwrap();
    }
}

/// An injected failure after the rename never turns a committed restore into
/// an ordinary failure, and a rerun does not rotate the incarnation again.
#[test]
fn failures_after_publication_report_a_committed_restore() {
    for (stage, durability, warning) in [
        ("renamed", "uncertain", "failure after publication rename"),
        ("published", "durable", "failure after durable publication"),
        (
            "finalizing",
            "durable",
            "restore record could not be finalized",
        ),
    ] {
        let (_root, path, receipt) = backed_up();
        let report =
            native_restore::restore(&path, &backup_directory(&receipt), quiescent(), &|at| {
                if at == stage {
                    Err(catabolic_store::Error(format!("injected {at}")))
                } else {
                    Ok(())
                }
            })
            .unwrap();
        assert_eq!(report["restored"], true, "{stage}");
        assert_eq!(report["durability"], durability, "{stage}");
        let warnings = report["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|text| text.as_str().unwrap().contains(warning)),
            "{stage}: {warnings:?}"
        );
        let rotated = report["incarnation"].as_str().unwrap().to_owned();
        assert_eq!(incarnation(&path), rotated, "{stage}");
        let record: Value =
            serde_json::from_slice(&fs::read(report["restore_record"].as_str().unwrap()).unwrap())
                .unwrap();
        let expected = if stage == "finalizing" {
            "publishing"
        } else {
            "published"
        };
        assert_eq!(record["status"], expected, "{stage}");
        if stage != "finalizing" {
            assert_eq!(record["durability"], durability, "{stage}");
        }
        let rerun = restore(&path, &receipt).unwrap();
        assert_eq!(rerun["already_published"], true, "{stage}");
        assert_eq!(rerun["incarnation"], rotated.as_str(), "{stage}");
        assert_eq!(incarnation(&path), rotated, "{stage}");
        assert_eq!(items(&path, "later-item"), 0, "{stage}");
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
    assert_eq!(report["durability"], "durable");
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

/// Replace the staged pathname with an unrotated copy of the backup snapshot.
fn substitute_staged(path: &Path, backup: &Path) {
    let staged = staged_file(path);
    let substitute = staged.with_extension("substitute");
    fs::write(
        &substitute,
        fs::read(backup.join("snapshot.sqlite3")).unwrap(),
    )
    .unwrap();
    fs::rename(&substitute, &staged).unwrap();
}
fn record_of(path: &Path) -> Value {
    let staging = staging_directories(path);
    serde_json::from_slice(&fs::read(staging[0].join("manifest.json")).unwrap()).unwrap()
}

/// `publishing` precedes the final pathname checks, so a replacement made
/// there is refused before the rename.
#[test]
fn replacement_at_publishing_is_refused_before_the_rename() {
    let (_root, path, receipt) = backed_up();
    let before = fs::read(&path).unwrap();
    let error = native_restore::restore(&path, &backup_directory(&receipt), quiescent(), &|at| {
        if at == "publishing" {
            substitute_staged(&path, &backup_directory(&receipt));
        }
        Ok(())
    })
    .unwrap_err();
    assert!(error.0.contains("staged catalog was replaced"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(staging_directories(&path).is_empty());
}

/// A substitution after the final checks is caught by binding the rename to
/// the locked inode, and the previous catalog is put back byte for byte.
#[test]
fn substitution_in_the_rename_window_is_rolled_back() {
    let (_root, path, receipt) = backed_up();
    let before = fs::read(&path).unwrap();
    let live_incarnation = incarnation(&path);
    let error = native_restore::restore(&path, &backup_directory(&receipt), quiescent(), &|at| {
        if at == native_restore::RENAME_WINDOW {
            substitute_staged(&path, &backup_directory(&receipt));
        }
        Ok(())
    })
    .unwrap_err();
    assert!(error.0.contains("hostile substitution detected"), "{error}");
    assert!(
        error.0.contains("previous catalog was restored unchanged"),
        "{error}"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(incarnation(&path), live_incarnation);
    assert_eq!(record_of(&path)["status"], "rolled_back");
    migration::validate(&connect(&path, true).unwrap(), true).unwrap();
    // A clean restore then proceeds normally.
    let report = restore(&path, &receipt).unwrap();
    assert_eq!(report["already_published"], false);
    assert_eq!(items(&path, "later-item"), 0);
}

/// A crash during the rollback is classified on rerun: before the retained
/// catalog is renamed back the outcome needs reconciliation; after it the
/// rerun records the rollback and restores normally.
#[test]
fn crash_during_substitution_rollback_is_classified_on_rerun() {
    for stage in [
        native_restore::ROLLBACK_PENDING,
        native_restore::ROLLBACK_RENAMED,
    ] {
        let (_root, path, receipt) = backed_up();
        let before = fs::read(&path).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "restore_exit_worker", "--nocapture"])
            .env("CATABOLIC_TEST_RESTORE_DATABASE", &path)
            .env("CATABOLIC_TEST_RESTORE_BACKUP", backup_directory(&receipt))
            .env("CATABOLIC_TEST_RESTORE_STAGE", stage)
            .env("CATABOLIC_TEST_RESTORE_SUBSTITUTE", "1")
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(86), "{stage}");
        assert_eq!(record_of(&path)["status"], "rolling_back", "{stage}");
        if stage == native_restore::ROLLBACK_PENDING {
            assert_ne!(fs::read(&path).unwrap(), before, "{stage}");
            let held = fs::read(&path).unwrap();
            let error = restore(&path, &receipt).unwrap_err();
            assert!(
                error.0.contains("restore record reconciliation required")
                    && error.0.contains("unfinished substitution rollback"),
                "{error}"
            );
            assert_eq!(fs::read(&path).unwrap(), held, "{stage}");
        } else {
            assert_eq!(fs::read(&path).unwrap(), before, "{stage}");
            let report = restore(&path, &receipt).unwrap();
            assert_eq!(report["already_published"], false, "{stage}");
            let mut statuses = staging_directories(&path)
                .iter()
                .map(|directory| {
                    serde_json::from_slice::<Value>(
                        &fs::read(directory.join("manifest.json")).unwrap(),
                    )
                    .unwrap()["status"]
                        .clone()
                })
                .collect::<Vec<_>>();
            statuses.sort_by_key(|status| status.to_string());
            assert_eq!(statuses, vec![json!("published"), json!("rolled_back")]);
        }
    }
}

/// A rerun after an exit at `renamed` retries the directory fsync under the
/// held exclusion and reports and records durability only from its result.
#[test]
fn rerun_retries_the_publication_fsync_before_reporting_durability() {
    let (_root, path, receipt) = backed_up();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "restore_exit_worker", "--nocapture"])
        .env("CATABOLIC_TEST_RESTORE_DATABASE", &path)
        .env("CATABOLIC_TEST_RESTORE_BACKUP", backup_directory(&receipt))
        .env("CATABOLIC_TEST_RESTORE_STAGE", "renamed")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86));
    let rotated = incarnation(&path);
    let record_path = staging_directories(&path)[0].join("manifest.json");
    let record = || -> Value { serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap() };
    assert_eq!(record()["status"], "publishing");
    let failed = native_restore::restore(&path, &backup_directory(&receipt), quiescent(), &|at| {
        if at == native_restore::RERUN_FSYNC {
            Err(catabolic_store::Error("injected fsync failure".into()))
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(failed["already_published"], true);
    assert_eq!(failed["durability"], "uncertain");
    assert!(
        failed["warnings"][0]
            .as_str()
            .unwrap()
            .contains("retrying the directory fsync failed")
    );
    assert_eq!(record()["status"], "published");
    assert_eq!(record()["durability"], "uncertain");
    let retried = restore(&path, &receipt).unwrap();
    assert_eq!(retried["already_published"], true);
    assert_eq!(retried["durability"], "durable");
    assert_eq!(retried["warnings"], json!([]));
    assert_eq!(record()["durability"], "durable");
    assert_eq!(incarnation(&path), rotated);
}

/// A record that cannot be read or parsed is never treated as absent: the
/// restore refuses and requires reconciliation, leaving the catalog unchanged.
#[test]
fn unreadable_or_corrupt_restore_records_require_reconciliation() {
    let damages: [(&str, &dyn Fn(&Path) -> PathBuf); 4] = [
        ("unreadable record", &|record: &Path| {
            fs::set_permissions(record, fs::Permissions::from_mode(0o000)).unwrap();
            record.to_path_buf()
        }),
        ("corrupt record", &|record: &Path| {
            fs::write(record, b"{").unwrap();
            record.to_path_buf()
        }),
        ("unreadable staging directory", &|record: &Path| {
            let directory = record.parent().unwrap();
            fs::set_permissions(directory, fs::Permissions::from_mode(0o000)).unwrap();
            directory.to_path_buf()
        }),
        ("unlistable catalog directory", &|record: &Path| {
            let parent = record.parent().unwrap().parent().unwrap();
            fs::set_permissions(parent, fs::Permissions::from_mode(0o300)).unwrap();
            parent.to_path_buf()
        }),
    ];
    for (label, damage) in damages {
        let (_root, path, receipt) = backed_up();
        let report = restore(&path, &receipt).unwrap();
        let published = incarnation(&path);
        put(&path, "after-restore", "after-restore-item");
        let before = fs::read(&path).unwrap();
        let damaged = damage(Path::new(report["restore_record"].as_str().unwrap()));
        let error = restore(&path, &receipt).unwrap_err();
        // Restore permissions before any assertion so cleanup can proceed.
        let mode = if label == "unreadable record" {
            0o600
        } else {
            0o700
        };
        fs::set_permissions(&damaged, fs::Permissions::from_mode(mode)).unwrap();
        assert!(
            error.0.contains("restore record reconciliation required"),
            "{label}: {error}"
        );
        assert_eq!(fs::read(&path).unwrap(), before, "{label}");
        assert_eq!(incarnation(&path), published, "{label}");
        assert_eq!(items(&path, "after-restore-item"), 1, "{label}");
    }
}

/// A failed publication rename publishes nothing and removes only this run's
/// staging directory, leaving other staging directories alone.
#[test]
fn failed_publication_rename_removes_only_its_staging_directory() {
    let (_root, path, receipt) = backed_up();
    let earlier = path.parent().unwrap().join(format!(
        ".{}.restore-earlier",
        path.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir(&earlier).unwrap();
    let before = fs::read(&path).unwrap();
    let error = native_restore::restore(&path, &backup_directory(&receipt), quiescent(), &|at| {
        if at == native_restore::RENAME_WINDOW {
            Err(catabolic_store::Error("injected rename failure".into()))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert!(
        error
            .0
            .contains("publication rename failed; the live catalog is unchanged"),
        "{error}"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(items(&path, "later-item"), 1);
    assert_eq!(staging_directories(&path), vec![earlier.clone()]);
    // The live catalog is not locked afterwards: a restore then succeeds.
    let report = restore(&path, &receipt).unwrap();
    assert_eq!(report["already_published"], false);
    assert_eq!(items(&path, "later-item"), 0);
}

/// Valid JSON is not enough: a record missing any required field, carrying a
/// wrong type, or naming another database is refused, never skipped.
#[test]
fn incomplete_restore_records_require_reconciliation() {
    let (_root, path, receipt) = backed_up();
    let report = restore(&path, &receipt).unwrap();
    let record_path = PathBuf::from(report["restore_record"].as_str().unwrap());
    let original: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    let published = incarnation(&path);
    let before = fs::read(&path).unwrap();
    let mut cases: Vec<(String, Value)> = Vec::new();
    for field in [
        "contract",
        "status",
        "database",
        "database_id",
        "catalog_id",
        "previous_incarnation",
        "incarnation",
        "backup",
        "backup_sha256",
        "retained",
        "live_identity",
        "staged_identity",
        "retained_identity",
    ] {
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove(field);
        cases.push((format!("missing {field}"), missing));
        let mut mistyped = original.clone();
        mistyped[field] = json!(17);
        cases.push((format!("mistyped {field}"), mistyped));
    }
    let mut durability = original.clone();
    durability["durability"] = json!("maybe");
    cases.push(("unknown durability".into(), durability));
    let mut foreign = original.clone();
    foreign["database_id"] = json!("2ff334d9-68fc-44b9-aa39-e9635593ccaa");
    cases.push(("mismatched database id".into(), foreign));
    let mut moved = original.clone();
    moved["database"] = json!("/elsewhere/native.sqlite3");
    cases.push(("mismatched database path".into(), moved));
    for (label, record) in cases {
        fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        let error = restore(&path, &receipt).unwrap_err();
        assert!(
            error.0.contains("restore record reconciliation required")
                && error
                    .0
                    .contains("is not a complete restore record for this catalog"),
            "{label}: {error}"
        );
        assert_eq!(fs::read(&path).unwrap(), before, "{label}");
        assert_eq!(incarnation(&path), published, "{label}");
    }
    // The intact record is recognized again.
    fs::write(&record_path, serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(restore(&path, &receipt).unwrap()["already_published"], true);
}

/// Schema objects, migration history and sequences are not covered by table
/// digests; each is verified under the held lock before the rename.
#[test]
fn staged_schema_history_and_sequence_changes_are_refused() {
    let first = |db: &rusqlite::Connection, kind: &str| -> String {
        db.query_row(
            "SELECT name FROM sqlite_schema WHERE type=? AND sql IS NOT NULL ORDER BY name LIMIT 1",
            [kind],
            |row| row.get(0),
        )
        .unwrap()
    };
    let changes: [(&str, &dyn Fn(&rusqlite::Connection)); 5] = [
        ("dropped trigger", &|db| {
            db.execute_batch("DROP TRIGGER catalog_permit_current_incarnation")
                .unwrap();
        }),
        ("altered view", &|db| {
            let view = first(db, "view");
            db.execute_batch(&format!(
                "DROP VIEW \"{view}\"; CREATE VIEW \"{view}\" AS SELECT 1 AS altered"
            ))
            .unwrap();
        }),
        ("dropped index", &|db| {
            let index = first(db, "index");
            db.execute_batch(&format!("DROP INDEX \"{index}\""))
                .unwrap();
        }),
        ("edited history", &|db| {
            db.execute_batch("UPDATE schema_migrations SET applied_at='edited' WHERE version=1")
                .unwrap();
        }),
        ("edited sequence", &|db| {
            db.execute_batch("INSERT INTO sqlite_sequence(name,seq) VALUES('api_events',1000)")
                .unwrap();
        }),
    ];
    for (label, change) in changes {
        let (_root, path, receipt) = backed_up();
        let before = fs::read(&path).unwrap();
        let error =
            native_restore::restore(&path, &backup_directory(&receipt), quiescent(), &|at| {
                if at == "rotated" {
                    let db = rusqlite::Connection::open(staged_file(&path)).unwrap();
                    change(&db);
                }
                Ok(())
            })
            .unwrap_err();
        assert!(
            error
                .0
                .contains("staged catalog failed verification before publication")
                || error.0.contains("staged catalog changed after rotation"),
            "{label}: {error}"
        );
        assert_eq!(fs::read(&path).unwrap(), before, "{label}");
        assert_eq!(items(&path, "later-item"), 1, "{label}");
        assert!(staging_directories(&path).is_empty(), "{label}");
    }
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
