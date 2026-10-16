//! Opt-in native cutover uses the existing migration owner without changing the
//! frozen schema-32 path. These tests intentionally exercise real SQLite locks.
use catabolic_store::{Error, connect, migration};
use migration::{NATIVE_SCHEMA_VERSION, NativeUpgradeQuiescence, SCHEMA_VERSION};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    migration::initialize(&path).unwrap();
    (root, path)
}
fn quiescence() -> NativeUpgradeQuiescence {
    NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}
fn upgrade(path: &Path) -> Value {
    migration::upgrade_native(
        path,
        &migration::plan_native(path).unwrap(),
        quiescence(),
        None,
        &|_, _, _| Ok(()),
    )
    .unwrap()
}
fn seed_job(path: &Path, state: &str) {
    let db = connect(path, true).unwrap();
    db.execute_batch("INSERT INTO profiles VALUES('other'); INSERT INTO locations VALUES('source'); INSERT INTO files VALUES('file','source','file.bin')").unwrap();
    db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state) VALUES('job','other','file','probe','source','{}','{}','key',?)",[state]).unwrap();
}
#[test]
fn targets_are_explicit_and_legacy_resources_stay_frozen() {
    assert_eq!(SCHEMA_VERSION, 32);
    assert_eq!(NATIVE_SCHEMA_VERSION, 37);
    assert_eq!(migration::MIGRATIONS.len(), 32);
    assert_eq!(
        migration::NATIVE_MIGRATIONS.len(),
        NATIVE_SCHEMA_VERSION as usize
    );
    for (index, (legacy, native)) in migration::MIGRATIONS
        .iter()
        .zip(migration::NATIVE_MIGRATIONS)
        .enumerate()
    {
        assert_eq!(legacy.version as usize, index + 1);
        assert_eq!(
            migration::description(legacy),
            migration::description(native)
        );
    }
    let (root, path) = fixture();
    assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
    assert_eq!(
        migration::inspect(&path, true).unwrap()["pending_migrations"],
        json!([])
    );
    assert_eq!(
        migration::plan_native(&path).unwrap()["migrations"]
            .as_array()
            .unwrap()
            .len(),
        (NATIVE_SCHEMA_VERSION - SCHEMA_VERSION) as usize
    );
    let native = root.path().join("native.sqlite3");
    assert_eq!(
        migration::initialize_native(&native).unwrap()["schema"],
        NATIVE_SCHEMA_VERSION
    );
    assert_eq!(
        migration::validate(&connect(&native, false).unwrap(), true).unwrap()["history"]
            .as_array()
            .unwrap()
            .len(),
        NATIVE_SCHEMA_VERSION as usize
    );
    assert!(
        migration::upgrade(&native, false, None, &|_, _, _| Ok(()))
            .unwrap_err()
            .0
            .contains("cannot be downgraded")
    );
}
#[test]
fn native_upgrade_preserves_old_rows_history_and_verified_backup() {
    let (_root, path) = fixture();
    seed_job(&path, "complete");
    let db = connect(&path, true).unwrap();
    db.execute(
        "INSERT INTO items VALUES('existing-item','video','{\"title\":\"retained\"}')",
        [],
    )
    .unwrap();
    let before = migration::snapshot(&db).unwrap();
    let history = migration::validate(&db, true).unwrap()["history"].clone();
    drop(db);
    let report = upgrade(&path);
    assert_eq!(report["schema"], NATIVE_SCHEMA_VERSION);
    let db = connect(&path, false).unwrap();
    migration::preservation(&db, &before).unwrap();
    let after = migration::validate(&db, true).unwrap();
    assert_eq!(
        &after["history"].as_array().unwrap()[..32],
        history.as_array().unwrap()
    );
    let backup = connect(Path::new(report["backup"].as_str().unwrap()), false).unwrap();
    assert_eq!(migration::snapshot(&backup).unwrap(), before);
    assert_eq!(
        migration::validate(&backup, true).unwrap()["history"],
        history
    );
    let receipt: Value = serde_json::from_slice(
        &fs::read(
            Path::new(report["backup"].as_str().unwrap())
                .parent()
                .unwrap()
                .join("manifest.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["status"], "committed");
    assert_eq!(
        receipt["plan"]["plan_fingerprint"],
        report["plan_fingerprint"]
    );
    assert_eq!(
        receipt["quiescence"]["legacy_shutdown_automatically_verified"],
        false
    );
}
#[test]
fn native_upgrade_requires_operator_quiescence_and_exact_plan() {
    let (_root, path) = fixture();
    let plan = migration::plan_native(&path).unwrap();
    let before = fs::read(&path).unwrap();
    let error = migration::upgrade_native(
        &path,
        &plan,
        NativeUpgradeQuiescence::default(),
        None,
        &|_, _, _| Ok(()),
    )
    .unwrap_err();
    assert!(error.0.contains("operator confirmation"));
    assert_eq!(fs::read(&path).unwrap(), before);
    for field in [
        "plan_fingerprint",
        "input_fingerprint",
        "native_schema_fingerprint",
    ] {
        let mut corrupt = plan.clone();
        corrupt[field] = json!("tampered");
        assert!(
            migration::upgrade_native(&path, &corrupt, quiescence(), None, &|_, _, _| Ok(()))
                .unwrap_err()
                .0
                .contains("plan or inputs changed")
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let mut corrupt = plan.clone();
    corrupt["migrations"][0]["checksum"] = json!("tampered");
    assert!(
        migration::upgrade_native(&path, &corrupt, quiescence(), None, &|_, _, _| Ok(())).is_err()
    );
    assert!(!PathBuf::from(format!("{}.backups", path.display())).exists());
    connect(&path, true)
        .unwrap()
        .execute(
            "UPDATE schema_migrations SET applied_at='changed' WHERE version=1",
            [],
        )
        .unwrap();
    assert!(
        migration::upgrade_native(&path, &plan, quiescence(), None, &|_, _, _| Ok(()))
            .unwrap_err()
            .0
            .contains("plan or inputs changed")
    );
}
#[test]
fn pending_work_in_other_profiles_and_expired_claims_block_cutover() {
    for state in ["queued", "running", "future-unknown-state"] {
        let (_root, path) = fixture();
        seed_job(&path, state);
        let plan = migration::plan_native(&path).unwrap();
        assert_eq!(plan["input"]["pending_work"]["processing_jobs"], 1);
        let error = migration::upgrade_native(&path, &plan, quiescence(), None, &|_, _, _| Ok(()))
            .unwrap_err();
        assert!(error.0.contains("pending catalog work"));
        assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
    }
    let (_root, path) = fixture();
    seed_job(&path, "complete");
    connect(&path, true)
        .unwrap()
        .execute(
            "INSERT INTO execution_claims VALUES('job',1,'expired',999999,1,0,'{}','{}')",
            [],
        )
        .unwrap();
    let plan = migration::plan_native(&path).unwrap();
    assert_eq!(plan["input"]["pending_work"]["execution_claims"], 1);
    assert!(
        migration::upgrade_native(&path, &plan, quiescence(), None, &|_, _, _| Ok(())).is_err()
    );
}
#[test]
fn data_or_history_changed_after_backup_cancels_native_upgrade() {
    for sql in [
        "INSERT INTO catalogs VALUES('concurrent')",
        "UPDATE schema_migrations SET applied_at='concurrent' WHERE version=1",
    ] {
        let (_root, path) = fixture();
        let plan = migration::plan_native(&path).unwrap();
        let result = migration::upgrade_native(&path, &plan, quiescence(), None, &|stage, _, _| {
            if stage == "upgrade:after_backup" {
                connect(&path, true)?.execute_batch(sql)?;
            }
            Ok(())
        });
        assert!(result.unwrap_err().0.contains("plan or inputs changed"));
        assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
    }
}
#[test]
fn real_sqlite_reader_blocks_native_exclusive_application() {
    let (_root, path) = fixture();
    let plan = migration::plan_native(&path).unwrap();
    let reader = RefCell::new(None);
    let result = migration::upgrade_native(&path, &plan, quiescence(), None, &|stage, db, _| {
        if stage == "upgrade:after_rehearsal" {
            let other = connect(&path, false)?;
            other.execute_batch("BEGIN")?;
            other.query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0))?;
            *reader.borrow_mut() = Some(other);
            db.busy_timeout(Duration::from_millis(20))?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().0.contains("locked"));
    drop(reader);
    assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
    assert_eq!(upgrade(&path)["schema"], NATIVE_SCHEMA_VERSION);
}
#[test]
fn wal_mode_is_explicitly_refused_without_mode_change_or_data_loss() {
    let (_root, path) = fixture();
    let writer = connect(&path, true).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer
        .execute("INSERT INTO catalogs VALUES('committed-wal')", [])
        .unwrap();
    let plan = migration::plan_native(&path).unwrap();
    let error =
        migration::upgrade_native(&path, &plan, quiescence(), None, &|_, _, _| Ok(())).unwrap_err();
    assert!(error.0.contains("found wal"));
    assert_eq!(
        writer
            .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    assert_eq!(
        writer
            .query_row(
                "SELECT count(*) FROM catalogs WHERE id='committed-wal'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(!PathBuf::from(format!("{}.backups", path.display())).exists());
}
#[test]
fn native_rehearsal_and_application_failures_roll_back_old_catalog() {
    for role in ["rehearsal", "upgrade"] {
        for boundary in ["before_migration", "after_migration", "before_commit"] {
            let (_root, path) = fixture();
            let plan = migration::plan_native(&path).unwrap();
            let before = fs::read(&path).unwrap();
            let target = format!("{role}:{boundary}");
            let error = migration::upgrade_native(
                &path,
                &plan,
                quiescence(),
                None,
                &|stage, _, version| {
                    if stage == target && version == NATIVE_SCHEMA_VERSION {
                        Err(Error("injected native boundary".into()))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
            assert!(
                error.0.contains("injected native boundary"),
                "{target}: {error}"
            );
            assert_eq!(fs::read(&path).unwrap(), before, "{target}");
            assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
        }
    }
}
#[test]
fn native_post_commit_observer_failure_reports_committed() {
    let (_root, path) = fixture();
    let plan = migration::plan_native(&path).unwrap();
    let report = migration::upgrade_native(&path, &plan, quiescence(), None, &|stage, _, _| {
        if stage == "upgrade:after_commit" {
            Err(Error("observer failed".into()))
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(report["upgraded"], true);
    assert!(
        report["warnings"][0]
            .as_str()
            .unwrap()
            .contains("committed")
    );
    assert_eq!(
        migration::inspect_native(&path, true).unwrap()["schema"],
        NATIVE_SCHEMA_VERSION
    );
}

#[test]
fn native_signature_and_last_migration_checksum_are_validated() {
    for damage in [
        "ALTER TABLE catalog_state ADD COLUMN unexpected TEXT",
        "UPDATE schema_migrations SET checksum='0000000000000000000000000000000000000000000000000000000000000000' WHERE version=33",
        "UPDATE schema_migrations SET checksum='0000000000000000000000000000000000000000000000000000000000000000' WHERE version=34",
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native.sqlite3");
        migration::initialize_native(&path).unwrap();
        let db = connect(&path, true).unwrap();
        db.execute_batch(damage).unwrap();
        assert!(migration::validate(&db, false).is_err());
    }
}

#[test]
fn completed_repair_history_is_preserved_without_becoming_pending_work() {
    let (_root, path) = fixture();
    connect(&path, true)
        .unwrap()
        .execute(
            "INSERT INTO remount_repairs(id,profile,plan) VALUES('historical','default','{}')",
            [],
        )
        .unwrap();
    let plan = migration::plan_native(&path).unwrap();
    assert_eq!(plan["work_settled"], true);
    assert_eq!(upgrade(&path)["schema"], NATIVE_SCHEMA_VERSION);
    assert_eq!(
        connect(&path, false)
            .unwrap()
            .query_row(
                "SELECT plan FROM remount_repairs WHERE id='historical'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "{}"
    );
}

#[test]
fn replacing_catalog_after_plan_is_detected_even_with_identical_bytes() {
    let (root, path) = fixture();
    let plan = migration::plan_native(&path).unwrap();
    let result = migration::upgrade_native(&path, &plan, quiescence(), None, &|stage, _, _| {
        if stage == "upgrade:after_backup" {
            let replacement = root.path().join("replacement.sqlite3");
            fs::write(&replacement, fs::read(&path)?)?;
            fs::rename(replacement, &path)?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().0.contains("plan or inputs changed"));
    assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
}

#[test]
fn native_migration_crash_worker() {
    let Ok(path) = std::env::var("CATABOLIC_TEST_NATIVE_CRASH_DATABASE") else {
        return;
    };
    let target = std::env::var("CATABOLIC_TEST_NATIVE_CRASH_STAGE").unwrap();
    let path = Path::new(&path);
    migration::upgrade_native(
        path,
        &migration::plan_native(path).unwrap(),
        quiescence(),
        None,
        &|stage, _, version| {
            if stage == target && version == NATIVE_SCHEMA_VERSION {
                std::process::exit(86);
            }
            Ok(())
        },
    )
    .unwrap();
    panic!("native crash checkpoint was not reached");
}

#[test]
fn native_process_exit_is_atomic_around_the_real_commit() {
    for role in ["rehearsal", "upgrade"] {
        for stage in ["after_migration", "before_commit", "after_commit"] {
            let (_root, path) = fixture();
            seed_job(&path, "complete");
            let before = migration::snapshot(&connect(&path, false).unwrap()).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "native_migration_crash_worker", "--nocapture"])
                .env("CATABOLIC_TEST_NATIVE_CRASH_DATABASE", &path)
                .env(
                    "CATABOLIC_TEST_NATIVE_CRASH_STAGE",
                    format!("{role}:{stage}"),
                )
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(86), "{role}:{stage}");
            let db = connect(&path, true).unwrap();
            let info = migration::validate(&db, true).unwrap();
            let expected = if role == "upgrade" && stage == "after_commit" {
                NATIVE_SCHEMA_VERSION
            } else {
                32
            };
            assert_eq!(info["schema"], expected, "{role}:{stage}");
            migration::preservation(&db, &before).unwrap();
            drop(db);
            assert_eq!(upgrade(&path)["schema"], NATIVE_SCHEMA_VERSION);
        }
    }
}

#[test]
fn verified_backup_binds_the_plan_before_any_post_backup_observer() {
    let (root, path) = fixture();
    let plan = migration::plan_native(&path).unwrap();
    let backups = root.path().join("backups");
    let error = migration::upgrade_native(
        &path,
        &plan,
        quiescence(),
        Some(&backups),
        &|stage, _, _| {
            if stage == "upgrade:after_backup" {
                let directory = fs::read_dir(&backups)?.next().unwrap()?.path();
                let receipt: Value =
                    serde_json::from_slice(&fs::read(directory.join("manifest.json"))?)?;
                assert_eq!(receipt["status"], "backup_verified");
                assert_eq!(receipt["plan"], plan);
                return Err(Error("stop after verified backup".into()));
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(error.0.contains("stop after verified backup"));
    assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
}

#[test]
fn plan_fingerprint_covers_autoincrement_state() {
    let (_root, path) = fixture();
    let db = connect(&path, true).unwrap();
    db.execute("INSERT INTO api_events(profile,resource_type,resource_id,state) VALUES('default','job','historical','complete')",[]).unwrap();
    let plan = migration::plan_native(&path).unwrap();
    db.execute(
        "UPDATE sqlite_sequence SET seq=seq+100 WHERE name='api_events'",
        [],
    )
    .unwrap();
    assert!(
        migration::upgrade_native(&path, &plan, quiescence(), None, &|_, _, _| Ok(()))
            .unwrap_err()
            .0
            .contains("plan or inputs changed")
    );
}

#[test]
fn legacy_uuid_spellings_get_canonical_native_receipt_identity_without_meta_rewrite() {
    let canonical = "2ff334d9-68fc-44b9-aa39-e9635593ccaa";
    for original in [
        canonical.to_uppercase(),
        canonical.replace('-', ""),
        format!("{{{canonical}}}"),
        format!("urn:uuid:{canonical}"),
    ] {
        let (_root, path) = fixture();
        let db = connect(&path, true).unwrap();
        db.execute(
            "UPDATE meta SET value=? WHERE key='database_id'",
            [&original],
        )
        .unwrap();
        let before = migration::snapshot(&db).unwrap();
        let history = migration::validate(&db, true).unwrap()["history"].clone();
        drop(db);
        let plan = migration::plan_native(&path).unwrap();
        let report =
            migration::upgrade_native(&path, &plan, quiescence(), None, &|stage, db, version| {
                if stage.ends_with(":after_migration") && version == 33 {
                    assert!(
                        db.query_row("SELECT catabolic_catalog_id_v1(?)", [canonical], |row| {
                            row.get::<_, String>(0)
                        })
                        .is_err(),
                        "migration-only UDF must be removed from the migration connection"
                    );
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(report["database_id"], canonical);
        let db = connect(&path, true).unwrap();
        migration::preservation(&db, &before).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT value FROM meta WHERE key='database_id'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            original
        );
        assert_eq!(
            db.query_row(
                "SELECT catalog_id FROM catalog_state WHERE id=1",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            canonical
        );
        assert_eq!(
            &migration::validate(&db, true).unwrap()["history"]
                .as_array()
                .unwrap()[..32],
            history.as_array().unwrap()
        );
        db.execute("INSERT INTO catalog_command_receipts(catalog_id,host_namespace,operation_id,contract_version,canonicalization_version,payload_digest,admission_incarnation,expected_generation,committed_generation,committed_at_us,retain_until_us,result_state,result_json) SELECT ?,'test-host','identity-test',1,1,?,incarnation,generation,generation,0,1,'full','{}' FROM catalog_state WHERE id=1", rusqlite::params![canonical, "0".repeat(64)]).unwrap();
        assert_eq!(db.query_row("SELECT catalog_id FROM catalog_command_receipts WHERE operation_id='identity-test'", [], |row| row.get::<_,String>(0)).unwrap(),canonical);
        assert!(
            db.query_row("SELECT catabolic_catalog_id_v1(?)", [&original], |row| {
                row.get::<_, String>(0)
            })
            .is_err(),
            "migration-only UDF must be removed"
        );
    }
}

#[test]
fn native_validation_requires_trigger_and_foreign_key_connection_prerequisites() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.sqlite3");
    migration::initialize_native(&path).unwrap();
    for (pragma, unsafe_value) in [
        ("recursive_triggers", false),
        ("foreign_keys", false),
        ("read_uncommitted", true),
    ] {
        let db = connect(&path, true).unwrap();
        assert!(migration::validate(&db, false).is_ok());
        db.pragma_update(None, pragma, unsafe_value).unwrap();
        assert!(
            migration::validate(&db, false)
                .unwrap_err()
                .0
                .contains("connection requires")
        );
    }
}

#[test]
fn replaced_admission_sidecar_refuses_native_upgrade_before_commit() {
    for boundary in ["upgrade:after_rehearsal", "upgrade:before_commit"] {
        let (_root, path) = fixture();
        let plan = migration::plan_native(&path).unwrap();
        let result = migration::upgrade_native(&path, &plan, quiescence(), None, &|stage, _, _| {
            if stage == boundary {
                let sidecar =
                    std::path::PathBuf::from(format!("{}.native-admission.lock", path.display()));
                let replacement = sidecar.with_extension("replacement");
                fs::write(&replacement, b"")?;
                fs::rename(replacement, sidecar)?;
            }
            Ok(())
        });
        assert!(
            result
                .unwrap_err()
                .0
                .contains("admission lock was replaced")
        );
        assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
    }
}

#[test]
fn replaced_catalog_at_commit_boundary_refuses_acknowledgement() {
    let (root, path) = fixture();
    let original = fs::read(&path).unwrap();
    let plan = migration::plan_native(&path).unwrap();
    let result = migration::upgrade_native(&path, &plan, quiescence(), None, &|stage, _, _| {
        if stage == "upgrade:before_commit" {
            let replacement = root.path().join("replacement-before-commit.sqlite3");
            fs::write(&replacement, &original)?;
            fs::rename(replacement, &path)?;
        }
        Ok(())
    });
    assert!(
        result
            .unwrap_err()
            .0
            .contains("native catalog replaced during upgrade")
    );
    assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
}
