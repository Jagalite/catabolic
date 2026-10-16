// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Upgrade, interruption, resume and restore rehearsal on retained fixtures.
//!
//! Each fixture is copied, captured (whole-row snapshot and identity manifest),
//! upgraded to schema 32 through the legacy owner, planned, and then the native
//! upgrade is killed by process exit at every durable boundary. After each exit
//! the copy is reopened and checked, the upgrade is resumed with a fresh plan,
//! identities are compared, and the verified backup is restored to a new path.
//! Fixtures are only ever read; every write goes to a private copy.
//!
//! The retained fixtures live outside version control. Set
//! `CATABOLIC_REHEARSAL_FIXTURES` to a directory containing `legacy-fixtures/`
//! and `scale/fixtures/`; the default is the checkout's `.local-tests` copy.
use catabolic_store::{Store, connect, identity_manifest, migration};
use migration::{NATIVE_SCHEMA_VERSION, NativeUpgradeQuiescence, SCHEMA_VERSION};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const EXIT: i32 = 86;

fn fixtures() -> PathBuf {
    std::env::var_os("CATABOLIC_REHEARSAL_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local-tests/rust-migration/m0-completion")
        })
}
fn legacy_fixture(version: u32) -> PathBuf {
    let path = fixtures().join(format!(
        "legacy-fixtures/schema-{version:02}/before.sqlite3"
    ));
    assert!(
        path.is_file(),
        "retained schema-{version:02} fixture required at {}",
        path.display()
    );
    path
}
fn quiescence() -> NativeUpgradeQuiescence {
    NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}
/// A byte copy into a new private file; fixture permissions are not inherited.
fn private_copy(source: &Path, target: &Path) {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(target)
        .unwrap()
        .write_all(&fs::read(source).unwrap())
        .unwrap();
}
fn snapshot(path: &Path) -> Value {
    let db = connect(path, false).unwrap();
    db.execute_batch("BEGIN").unwrap();
    migration::snapshot(&db).unwrap()
}
fn manifest(path: &Path) -> Value {
    let mut value = identity_manifest::manifest_path(path).unwrap();
    value.as_object_mut().unwrap().remove("database");
    value
}
fn backup_directories(backups: &Path) -> Vec<PathBuf> {
    let mut directories = fs::read_dir(backups)
        .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_else(|_| Vec::new());
    directories.sort();
    directories
}
fn receipt(directory: &Path) -> Value {
    serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap()
}

/// Every durable boundary of a native upgrade from schema 32, in order. The
/// version is the one passed to the migration checkpoint at that boundary.
fn boundaries() -> Vec<(String, u32)> {
    let steps = || SCHEMA_VERSION + 1..=NATIVE_SCHEMA_VERSION;
    let mut boundaries = vec![("upgrade:after_backup".to_owned(), SCHEMA_VERSION)];
    for role in ["rehearsal", "upgrade"] {
        if role == "upgrade" {
            boundaries.push(("upgrade:after_rehearsal".into(), SCHEMA_VERSION));
        }
        for version in steps() {
            boundaries.push((format!("{role}:before_migration"), version));
            boundaries.push((format!("{role}:after_migration"), version));
        }
        boundaries.push((format!("{role}:before_commit"), NATIVE_SCHEMA_VERSION));
        boundaries.push((format!("{role}:after_commit"), NATIVE_SCHEMA_VERSION));
    }
    boundaries
}

/// Re-executed by `interrupt` in a child process. The approved plan is read
/// from disk exactly as an operator would supply it.
#[test]
fn rehearsal_exit_worker() {
    let Ok(database) = std::env::var("CATABOLIC_TEST_REHEARSAL_DATABASE") else {
        return;
    };
    let plan: Value = serde_json::from_slice(
        &fs::read(std::env::var("CATABOLIC_TEST_REHEARSAL_PLAN").unwrap()).unwrap(),
    )
    .unwrap();
    let backups = PathBuf::from(std::env::var("CATABOLIC_TEST_REHEARSAL_BACKUPS").unwrap());
    let stage = std::env::var("CATABOLIC_TEST_REHEARSAL_STAGE").unwrap();
    let version: u32 = std::env::var("CATABOLIC_TEST_REHEARSAL_VERSION")
        .unwrap()
        .parse()
        .unwrap();
    let result = migration::upgrade_native(
        Path::new(&database),
        &plan,
        quiescence(),
        Some(&backups),
        &|current, _, at| {
            if current == stage && at == version {
                std::process::exit(EXIT);
            }
            Ok(())
        },
    );
    panic!("rehearsal boundary {stage}:{version} was not reached: {result:?}");
}

fn interrupt(database: &Path, plan: &Path, backups: &Path, stage: &str, version: u32) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "rehearsal_exit_worker", "--nocapture"])
        .env("CATABOLIC_TEST_REHEARSAL_DATABASE", database)
        .env("CATABOLIC_TEST_REHEARSAL_PLAN", plan)
        .env("CATABOLIC_TEST_REHEARSAL_BACKUPS", backups)
        .env("CATABOLIC_TEST_REHEARSAL_STAGE", stage)
        .env("CATABOLIC_TEST_REHEARSAL_VERSION", version.to_string())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(EXIT), "{stage}:{version}");
}

/// The schema-32 state every boundary starts from and is compared against.
struct Baseline {
    path: PathBuf,
    snapshot: Value,
    manifest: Value,
    original: Value,
}

/// Capture the fixture, then upgrade it to schema 32 with the legacy owner.
fn legacy_baseline(source: &Path, root: &Path) -> Baseline {
    let path = root.join("baseline.sqlite3");
    private_copy(source, &path);
    let original_snapshot = snapshot(&path);
    let original = manifest(&path);
    let from = original["schema"].as_u64().unwrap() as u32;
    let report = migration::upgrade(
        &path,
        false,
        Some(&root.join("legacy-backups")),
        &|_, _, _| Ok(()),
    )
    .unwrap();
    assert_eq!(report["schema"], SCHEMA_VERSION);
    assert_eq!(report["upgraded"], from < SCHEMA_VERSION);
    let db = connect(&path, false).unwrap();
    migration::preservation(&db, &original_snapshot).unwrap();
    drop(db);
    let manifest = manifest(&path);
    identity_manifest::require_preserved(&original, &manifest).unwrap();
    if from < SCHEMA_VERSION {
        // The legacy backup is the untouched original catalog.
        let backup = Path::new(report["backup"].as_str().unwrap());
        assert_eq!(snapshot(backup), original_snapshot);
        assert_eq!(
            receipt(backup.parent().unwrap())["sha256"],
            catabolic_core::sha256(fs::read(backup).unwrap())
        );
    }
    let info = migration::inspect_native(&path, true).unwrap();
    assert_eq!(info["work_settled"], true, "{}", info["pending_work"]);
    assert_eq!(info["journal_mode"], "delete");
    Baseline {
        snapshot: snapshot(&path),
        path,
        manifest,
        original,
    }
}

/// Restore a verified backup snapshot to a new path. The manifest checksum is
/// verified before and after the copy; the restored file is never the live one.
fn restore(directory: &Path, target: &Path, baseline: &Baseline) {
    let receipt = receipt(directory);
    let snapshot_path = Path::new(receipt["backup"].as_str().unwrap());
    assert_eq!(snapshot_path, directory.join("snapshot.sqlite3"));
    let bytes = fs::read(snapshot_path).unwrap();
    assert_eq!(receipt["sha256"], catabolic_core::sha256(&bytes));
    assert_eq!(receipt["database_id"], baseline.manifest["database_id"]);
    assert_eq!(receipt["from_schema"], SCHEMA_VERSION);
    assert_eq!(receipt["to_schema"], NATIVE_SCHEMA_VERSION);
    assert!(!target.exists());
    private_copy(snapshot_path, target);
    assert_eq!(
        receipt["sha256"],
        catabolic_core::sha256(fs::read(target).unwrap())
    );
    let inspected = migration::inspect(target, true).unwrap();
    assert_eq!(inspected["schema"], SCHEMA_VERSION);
    assert_eq!(inspected["database_id"], baseline.manifest["database_id"]);
    assert_eq!(inspected["pending_migrations"], serde_json::json!([]));
    assert_eq!(snapshot(target), baseline.snapshot);
    assert_eq!(
        manifest(target)["fingerprint"],
        baseline.manifest["fingerprint"]
    );
}

/// The latest-native-schema limitations that remain after a committed native upgrade.
/// Each is a specific refusal; none of them changes the upgraded catalog.
fn assert_post_upgrade_limitations(path: &Path) {
    let before = fs::read(path).unwrap();
    let recovery = Store::open(path, true, true).err().unwrap();
    assert!(
        recovery
            .0
            .contains("native catalog recovery requires separately qualified recovery admission"),
        "{recovery}"
    );
    let legacy = migration::upgrade(path, false, None, &|_, _, _| Ok(())).unwrap_err();
    assert!(
        legacy
            .0
            .contains("native catalogs cannot be downgraded by the legacy upgrade path"),
        "{legacy}"
    );
    let unsupported =
        migration::initialize_native_at(&path.with_extension("v38"), NATIVE_SCHEMA_VERSION + 1)
            .unwrap_err();
    assert!(unsupported.0.contains("unsupported native schema target"));
    // A repeated native upgrade is an acknowledged no-op, not a second backup.
    let plan = migration::plan_native(path).unwrap();
    assert_eq!(plan["migrations"], serde_json::json!([]));
    let repeated =
        migration::upgrade_native(path, &plan, quiescence(), None, &|_, _, _| Ok(())).unwrap();
    assert_eq!(repeated["upgraded"], false);
    assert_eq!(repeated["backup"], Value::Null);
    // Ordinary native admission still opens the upgraded catalog.
    let store = Store::open(path, true, false).unwrap();
    assert_eq!(store.schema_version, NATIVE_SCHEMA_VERSION);
    assert!(store.catalog_incarnation.is_some());
    drop(store);
    assert_eq!(fs::read(path).unwrap(), before);
}

fn expected_status(stage: &str) -> &'static str {
    if stage == "upgrade:after_backup" || stage.starts_with("rehearsal:") {
        "backup_verified"
    } else {
        // An exit at upgrade:after_commit leaves the receipt one phase behind
        // the committed database; the schema version is authoritative.
        "rehearsed"
    }
}

/// Run the complete boundary matrix for one fixture.
fn rehearse(source: &Path) {
    let root = tempfile::tempdir().unwrap();
    let baseline = legacy_baseline(source, root.path());
    for (index, (stage, version)) in boundaries().into_iter().enumerate() {
        let label = format!("{}:{stage}:{version}", source.display());
        let case = root.path().join(format!("boundary-{index:02}"));
        fs::create_dir(&case).unwrap();
        let path = case.join("catalog.sqlite3");
        private_copy(&baseline.path, &path);
        let backups = case.join("backups");
        // The plan binds this copy's file identity, so each copy gets its own.
        let plan = case.join("plan.json");
        fs::write(
            &plan,
            serde_json::to_vec_pretty(&migration::plan_native(&path).unwrap()).unwrap(),
        )
        .unwrap();
        interrupt(&path, &plan, &backups, &stage, version);

        // Reopen writable first: SQLite rolls back a hot journal only then.
        let committed = stage == "upgrade:after_commit";
        let db = connect(&path, true).unwrap();
        let info = migration::validate(&db, true).unwrap();
        let expected = if committed {
            NATIVE_SCHEMA_VERSION
        } else {
            SCHEMA_VERSION
        };
        assert_eq!(info["schema"], expected, "{label}");
        migration::preservation(&db, &baseline.snapshot).unwrap();
        drop(db);
        let interrupted = manifest(&path);
        identity_manifest::require_preserved(&baseline.manifest, &interrupted).unwrap();
        if !committed {
            assert_eq!(
                interrupted["fingerprint"], baseline.manifest["fingerprint"],
                "{label}"
            );
        }
        let directories = backup_directories(&backups);
        assert_eq!(directories.len(), 1, "{label}");
        let interrupted_backup = &directories[0];
        let partial = receipt(interrupted_backup);
        assert_eq!(partial["status"], expected_status(&stage), "{label}");
        let plan_value: Value = serde_json::from_slice(&fs::read(&plan).unwrap()).unwrap();
        assert_eq!(partial["plan"], plan_value, "{label}");
        // A killed rehearsal leaves its private clone for the operator to remove.
        let leftovers = fs::read_dir(interrupted_backup)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("rehearsal-")
            })
            .count();
        assert_eq!(
            leftovers,
            usize::from(stage.starts_with("rehearsal:")),
            "{label}"
        );

        // Resume with a fresh plan; the interrupted plan no longer matches a
        // committed catalog and must not be reused for it.
        if committed {
            assert!(
                migration::upgrade_native(&path, &plan_value, quiescence(), None, &|_, _, _| {
                    Ok(())
                })
                .unwrap_err()
                .0
                .contains("plan or inputs changed"),
                "{label}"
            );
        }
        let report = migration::upgrade_native(
            &path,
            &migration::plan_native(&path).unwrap(),
            quiescence(),
            Some(&backups),
            &|_, _, _| Ok(()),
        )
        .unwrap();
        assert_eq!(report["schema"], NATIVE_SCHEMA_VERSION, "{label}");
        assert_eq!(report["upgraded"], !committed, "{label}");
        let db = connect(&path, false).unwrap();
        let resumed = migration::validate(&db, true).unwrap();
        assert_eq!(resumed["schema"], NATIVE_SCHEMA_VERSION);
        assert_eq!(
            &resumed["history"].as_array().unwrap()[..SCHEMA_VERSION as usize],
            baseline.manifest["history"].as_array().unwrap(),
            "{label}"
        );
        migration::preservation(&db, &baseline.snapshot).unwrap();
        drop(db);
        let upgraded = manifest(&path);
        identity_manifest::require_preserved(&baseline.manifest, &upgraded).unwrap();
        identity_manifest::require_preserved(&baseline.original, &upgraded).unwrap();
        assert_eq!(
            upgraded["catalog"]["catalog_id"],
            baseline.manifest["database_id"]
        );

        // Restore the backup belonging to the committed upgrade to a new path.
        let committed_backup = if committed {
            interrupted_backup.clone()
        } else {
            let backup = Path::new(report["backup"].as_str().unwrap());
            let directory = backup.parent().unwrap().to_path_buf();
            assert_eq!(receipt(&directory)["status"], "committed", "{label}");
            assert_eq!(backup_directories(&backups).len(), 2, "{label}");
            directory
        };
        restore(&committed_backup, &case.join("restored.sqlite3"), &baseline);
        if !committed {
            // The interrupted attempt's snapshot is equally restorable.
            restore(
                interrupted_backup,
                &case.join("restored-interrupted.sqlite3"),
                &baseline,
            );
        }
        if index == 0 {
            assert_post_upgrade_limitations(&path);
        }
        fs::remove_dir_all(&case).unwrap();
    }
}

#[test]
fn boundary_matrix_covers_every_durable_checkpoint() {
    let boundaries = boundaries();
    let steps = (NATIVE_SCHEMA_VERSION - SCHEMA_VERSION) as usize;
    assert_eq!(boundaries.len(), 1 + 2 * (2 * steps + 2) + 1);
    assert_eq!(boundaries.first().unwrap().0, "upgrade:after_backup");
    assert_eq!(boundaries.last().unwrap().0, "upgrade:after_commit");
    assert!(boundaries.contains(&("upgrade:after_rehearsal".into(), SCHEMA_VERSION)));
    for role in ["rehearsal", "upgrade"] {
        for version in SCHEMA_VERSION + 1..=NATIVE_SCHEMA_VERSION {
            for boundary in ["before_migration", "after_migration"] {
                assert!(boundaries.contains(&(format!("{role}:{boundary}"), version)));
            }
        }
    }
}

#[test]
fn legacy_schema_01_rehearsal() {
    rehearse(&legacy_fixture(1));
}

#[test]
fn legacy_schema_16_rehearsal() {
    rehearse(&legacy_fixture(16));
}

#[test]
fn legacy_schema_32_rehearsal() {
    rehearse(&legacy_fixture(32));
}

/// Gated for time: each of the 26 boundaries copies, backs up, rehearses and
/// resumes the whole catalog. `CATABOLIC_REHEARSAL_SCALE_FIXTURE` may select
/// another retained catalog, such as `database-100000`.
#[test]
#[ignore = "scale rehearsal: run explicitly with --ignored scale_database_rehearsal"]
fn scale_database_rehearsal() {
    let path = std::env::var_os("CATABOLIC_REHEARSAL_SCALE_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| fixtures().join("scale/fixtures/database-10000/catalog.sqlite3"));
    assert!(
        path.is_file(),
        "scale fixture required at {}",
        path.display()
    );
    rehearse(&path);
}

/// Refusals before any backup: WAL mode and a live Catabolic writer. Neither
/// changes the catalog, its journal mode or creates a backup directory.
#[test]
fn pre_upgrade_refusals_leave_schema_32_untouched() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    private_copy(&legacy_fixture(32), &path);
    let backups = root.path().join("backups");

    let writer = Store::open(&path, true, false).unwrap();
    let plan = migration::plan_native(&path).unwrap();
    let busy =
        migration::upgrade_native(
            &path,
            &plan,
            quiescence(),
            Some(&backups),
            &|_, _, _| Ok(()),
        )
        .unwrap_err();
    assert!(
        busy.0
            .contains("another Catabolic writer or upgrade is using this database"),
        "{busy}"
    );
    drop(writer);

    let db = connect(&path, true).unwrap();
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    drop(db);
    let snapshot = snapshot(&path);
    let plan = migration::plan_native(&path).unwrap();
    assert_eq!(plan["input"]["journal_mode"], "wal");
    let wal =
        migration::upgrade_native(
            &path,
            &plan,
            quiescence(),
            Some(&backups),
            &|_, _, _| Ok(()),
        )
        .unwrap_err();
    assert!(
        wal.0
            .contains("native upgrade requires rollback-journal mode for a real SQLite exclusive lock; found wal"),
        "{wal}"
    );
    assert_eq!(
        migration::inspect_native(&path, true).unwrap()["journal_mode"],
        "wal"
    );
    assert_eq!(
        migration::inspect(&path, true).unwrap()["schema"],
        SCHEMA_VERSION
    );
    assert_eq!(self::snapshot(&path), snapshot);
    assert!(!backups.exists());
}
