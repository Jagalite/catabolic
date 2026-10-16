// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Offline WAL to rollback-journal conversion. Other connections are real
//! child processes, because POSIX record locks do not conflict within one.
use catabolic_store::{Store, connect, journal_conversion, migration};
use migration::{NATIVE_SCHEMA_VERSION, NativeUpgradeQuiescence};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

fn catalog() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    migration::initialize(&path).unwrap();
    (root, path)
}
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.display()))
}
fn enable_wal(path: &Path) {
    let db = connect(path, true).unwrap();
    assert_eq!(
        db.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
}
fn count(path: &Path, id: &str) -> i64 {
    connect(path, false)
        .unwrap()
        .query_row("SELECT count(*) FROM catalogs WHERE id=?", [id], |row| {
            row.get(0)
        })
        .unwrap()
}

/// Child process: `commit` leaves committed WAL frames by exiting without
/// closing its connection; `hold` keeps an idle connection open until killed.
#[test]
fn journal_holder_worker() {
    let Ok(path) = std::env::var("CATABOLIC_TEST_JOURNAL_DATABASE") else {
        return;
    };
    let path = Path::new(&path);
    let db = connect(path, true).unwrap();
    match std::env::var("CATABOLIC_TEST_JOURNAL_MODE")
        .unwrap()
        .as_str()
    {
        "commit" => {
            db.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
                .unwrap();
            db.execute("INSERT INTO catalogs VALUES('committed-in-wal')", [])
                .unwrap();
            std::process::exit(0);
        }
        "hold" => {
            db.query_row("SELECT count(*) FROM catalogs", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
            fs::write(
                std::env::var("CATABOLIC_TEST_JOURNAL_READY").unwrap(),
                b"ready",
            )
            .unwrap();
            std::thread::sleep(Duration::from_secs(300));
            panic!("journal holder was not stopped");
        }
        other => panic!("unknown holder mode {other}"),
    }
}
fn worker(path: &Path, mode: &str, ready: Option<&Path>) -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "journal_holder_worker", "--nocapture"])
        .env("CATABOLIC_TEST_JOURNAL_DATABASE", path)
        .env("CATABOLIC_TEST_JOURNAL_MODE", mode);
    if let Some(ready) = ready {
        command.env("CATABOLIC_TEST_JOURNAL_READY", ready);
    }
    command.spawn().unwrap()
}

#[test]
fn unsupported_targets_are_refused_before_any_lock() {
    let (_root, path) = catalog();
    let before = fs::read(&path).unwrap();
    for target in ["wal", "truncate", "persist", "memory", "off", ""] {
        let error = journal_conversion::convert(&path, target).unwrap_err();
        assert!(
            error.0.contains("unsupported journal conversion target"),
            "{target}: {error}"
        );
    }
    assert!(!sidecar(&path, ".native-admission.lock").exists());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn rollback_journal_catalog_is_reported_unchanged() {
    let (_root, path) = catalog();
    let before = fs::read(&path).unwrap();
    let report = journal_conversion::convert(&path, "delete").unwrap();
    assert_eq!(report["converted"], false);
    assert_eq!(report["from_journal_mode"], "delete");
    assert_eq!(report["journal_mode"], "delete");
    assert_eq!(report["schema"], 32);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn committed_wal_frames_survive_conversion_and_unblock_native_upgrade() {
    let (_root, path) = catalog();
    assert!(worker(&path, "commit", None).wait().unwrap().success());
    assert!(fs::metadata(sidecar(&path, "-wal")).unwrap().len() > 0);
    let plan = migration::plan_native(&path).unwrap();
    let quiescence = NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    };
    assert!(
        migration::upgrade_native(&path, &plan, quiescence, None, &|_, _, _| Ok(()))
            .unwrap_err()
            .0
            .contains("found wal")
    );
    let report = journal_conversion::convert(&path, "delete").unwrap();
    assert_eq!(report["converted"], true);
    assert_eq!(report["from_journal_mode"], "wal");
    assert_eq!(report["journal_mode"], "delete");
    assert!(!sidecar(&path, "-wal").exists());
    assert_eq!(count(&path, "committed-in-wal"), 1);
    assert_eq!(
        migration::inspect_native(&path, true).unwrap()["journal_mode"],
        "delete"
    );
    // The earlier plan recorded WAL and is stale; a fresh plan is required.
    assert!(
        migration::upgrade_native(&path, &plan, quiescence, None, &|_, _, _| Ok(()))
            .unwrap_err()
            .0
            .contains("plan or inputs changed")
    );
    let report = migration::upgrade_native(
        &path,
        &migration::plan_native(&path).unwrap(),
        quiescence,
        None,
        &|_, _, _| Ok(()),
    )
    .unwrap();
    assert_eq!(report["schema"], NATIVE_SCHEMA_VERSION);
    assert_eq!(count(&path, "committed-in-wal"), 1);
}

#[test]
fn an_open_connection_in_another_process_refuses_conversion() {
    let (root, path) = catalog();
    enable_wal(&path);
    let ready = root.path().join("ready");
    let mut holder = worker(&path, "hold", Some(&ready));
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "holder did not open the catalog");
        assert!(holder.try_wait().unwrap().is_none(), "holder exited early");
        std::thread::sleep(Duration::from_millis(20));
    }
    let error = journal_conversion::convert(&path, "delete").unwrap_err();
    assert!(
        error
            .0
            .contains("another connection has this database open"),
        "{error}"
    );
    assert_eq!(
        migration::inspect_native(&path, false).unwrap()["journal_mode"],
        "wal"
    );
    holder.kill().unwrap();
    holder.wait().unwrap();
    let report = journal_conversion::convert(&path, "delete").unwrap();
    assert_eq!(report["converted"], true);
    assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
}

#[test]
fn catabolic_writers_and_native_sessions_refuse_conversion() {
    let (_root, path) = catalog();
    enable_wal(&path);
    let writer = Store::open(&path, true, false).unwrap();
    let error = journal_conversion::convert(&path, "delete").unwrap_err();
    assert!(
        error
            .0
            .contains("another Catabolic writer or upgrade is using this database"),
        "{error}"
    );
    drop(writer);

    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("native.sqlite3");
    migration::initialize_native(&native).unwrap();
    enable_wal(&native);
    let session = Store::open(&native, false, false).unwrap();
    let error = journal_conversion::convert(&native, "delete").unwrap_err();
    assert!(
        error
            .0
            .contains("native catalog sessions or exclusive lifecycle operation are active"),
        "{error}"
    );
    drop(session);
    let report = journal_conversion::convert(&native, "delete").unwrap();
    assert_eq!(report["converted"], true);
    assert_eq!(report["schema"], NATIVE_SCHEMA_VERSION);
    migration::validate(&connect(&native, false).unwrap(), true).unwrap();
}
