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
