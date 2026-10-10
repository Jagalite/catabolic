//! Real SQLite rollback at every frozen migration boundary, not a mocked store.
use catabolic_store::{Error, RECOVERABLE_SCHEMAS, Store, connect, migration};
use std::{fs, path::PathBuf};
fn fixture(version: u32) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../.local-tests/rust-migration/m0-completion/legacy-fixtures/schema-{version:02}/before.sqlite3"))
}
#[test]
fn every_migration_boundary_rolls_back_the_whole_chain() {
    let initial = fs::read(fixture(1)).expect("retained schema-1 qualification fixture required");
    for role in ["rehearsal", "upgrade"] {
        for stage in ["before_migration", "after_migration"] {
            for version in 2..=32 {
                let temporary = tempfile::tempdir().unwrap();
                let path = temporary.path().join("catalog.sqlite3");
                fs::write(&path, &initial).unwrap();
                let target = format!("{role}:{stage}");
                let result = migration::upgrade(&path, false, None, &|stage, _, at| {
                    if stage == target && at == version {
                        Err(Error("injected boundary".into()))
                    } else {
                        Ok(())
                    }
                });
                assert!(
                    result.unwrap_err().0.contains("injected boundary"),
                    "{target}:{version}"
                );
                assert_eq!(fs::read(&path).unwrap(), initial, "{target}:{version}");
                assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 1);
            }
        }
    }
}
#[test]
fn recovery_is_an_exact_reviewed_set() {
    for version in 1..=32 {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("catalog.sqlite3");
        fs::copy(fixture(version), &path).unwrap();
        assert_eq!(
            Store::open(&path, false, true).is_ok(),
            version == 32 || RECOVERABLE_SCHEMAS.contains(&version),
            "schema {version}"
        );
        assert_eq!(
            Store::open(&path, false, false).is_ok(),
            version == 32,
            "schema {version}"
        );
    }
}
#[test]
fn committed_wal_is_in_backup_and_edits_after_backup_cancel_upgrade() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("catalog.sqlite3");
    fs::copy(fixture(14), &path).unwrap();
    let writer = connect(&path, true).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer
        .execute("INSERT INTO catalogs(id) VALUES('wal')", [])
        .unwrap();
    let result = migration::upgrade(&path, false, None, &|_, _, _| Ok(())).unwrap();
    let backup = connect(
        std::path::Path::new(result["backup"].as_str().unwrap()),
        false,
    )
    .unwrap();
    assert_eq!(
        backup
            .query_row("SELECT id FROM catalogs WHERE id='wal'", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    drop(writer);
    drop(backup);
    let path = temporary.path().join("edited.sqlite3");
    fs::copy(fixture(14), &path).unwrap();
    let result = migration::upgrade(&path, false, None, &|stage, _, _| {
        if stage == "upgrade:after_backup" {
            let other = connect(&path, true)?;
            other.execute("INSERT INTO catalogs(id) VALUES('concurrent')", [])?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().0.contains("changed after backup"));
    let db = connect(&path, false).unwrap();
    assert_eq!(
        db.query_row("SELECT id FROM catalogs WHERE id='concurrent'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "concurrent"
    );
    assert_eq!(migration::validate(&db, true).unwrap()["schema"], 14);
}

#[test]
fn sqlite_full_at_actual_upgrade_rolls_back() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("catalog.sqlite3");
    fs::copy(fixture(14), &path).unwrap();
    let before = fs::read(&path).unwrap();
    let result = migration::upgrade(&path, false, None, &|stage, db, _| {
        if stage == "upgrade:after_rehearsal" {
            let pages: i64 = db.pragma_query_value(None, "page_count", |r| r.get(0))?;
            db.pragma_update(None, "max_page_count", pages)?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().0.contains("full"));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 14);
}

#[test]
fn migration_crash_worker() {
    let Ok(path) = std::env::var("CATABOLIC_TEST_CRASH_DATABASE") else {
        return;
    };
    let target = std::env::var("CATABOLIC_TEST_CRASH_STAGE").unwrap();
    let version: u32 = std::env::var("CATABOLIC_TEST_CRASH_VERSION")
        .unwrap()
        .parse()
        .unwrap();
    migration::upgrade(std::path::Path::new(&path), false, None, &|stage, _, at| {
        if stage == target && at == version {
            std::process::exit(86);
        }
        Ok(())
    })
    .unwrap();
    panic!("crash checkpoint was not reached");
}

#[test]
fn process_exit_is_atomic_before_and_after_commit() {
    for role in ["rehearsal", "upgrade"] {
        for (stage, version) in [
            ("after_migration", 2),
            ("after_migration", 16),
            ("after_migration", 32),
            ("before_commit", 32),
            ("after_commit", 32),
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let path = temporary.path().join("catalog.sqlite3");
            fs::copy(fixture(1), &path).unwrap();
            let before = migration::snapshot(&connect(&path, false).unwrap()).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "migration_crash_worker", "--nocapture"])
                .env("CATABOLIC_TEST_CRASH_DATABASE", &path)
                .env("CATABOLIC_TEST_CRASH_STAGE", format!("{role}:{stage}"))
                .env("CATABOLIC_TEST_CRASH_VERSION", version.to_string())
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(86), "{role}:{stage}:{version}");
            let db = connect(&path, true).unwrap(); // writable SQLite recovers the hot rollback journal
            let info = migration::validate(&db, true).unwrap();
            let expected = if role == "upgrade" && stage == "after_commit" {
                32
            } else {
                1
            };
            assert_eq!(info["schema"], expected);
            migration::preservation(&db, &before).unwrap();
            drop(db);
            migration::upgrade(&path, false, None, &|_, _, _| Ok(())).unwrap();
            assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
        }
    }
}

#[test]
fn post_commit_observer_failure_reports_committed_with_warning() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("catalog.sqlite3");
    fs::copy(fixture(31), &path).unwrap();
    let result = migration::upgrade(&path, false, None, &|stage, _, _| {
        if stage == "upgrade:after_commit" {
            Err(Error("observer unavailable".into()))
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(result["upgraded"], true);
    assert!(
        result["warnings"][0]
            .as_str()
            .unwrap()
            .contains("committed")
    );
    assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
    let directory = std::path::Path::new(result["backup"].as_str().unwrap())
        .parent()
        .unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["status"], "committed");
}
