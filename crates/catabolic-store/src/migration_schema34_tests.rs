//! Fixed schema34 preservation probes plus explicit latest-target runner tests.
//! Mutation of guards in corruption fixtures is
//! confined to these disposable databases; production schema validation remains
//! a separate requirement in the actual migration runner.
use super::*;

fn native33() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("populated33.sqlite3");
    initialize_native_at(&path, 33).unwrap();
    let db = connect(&path, true).unwrap();
    db.execute_batch(
        "BEGIN IMMEDIATE;
        INSERT INTO items VALUES('retained-a','video','{\"title\":\"old\"}');
        INSERT INTO items VALUES('retained-b','video','{\"title\":\"second\"}');
        INSERT INTO items VALUES('deleted-item','video','{}');
        UPDATE items SET metadata='{\"title\":\"current\",\"unknown\":null}' WHERE id='retained-a';
        DELETE FROM items WHERE id='deleted-item';
        INSERT INTO identities VALUES('imdb','tt-retained','retained-a');
        INSERT INTO identities VALUES('provider','secondary','retained-b');",
    )
    .unwrap();
    db.execute(
        "INSERT INTO identities VALUES(?,?,?)",
        params!["quoted\"é", "line\nvalue", "retained-a"],
    )
    .unwrap();
    db.execute("INSERT INTO catalog_command_receipts(catalog_id,host_namespace,operation_id,contract_version,canonicalization_version,payload_digest,admission_incarnation,expected_generation,committed_generation,committed_at_us,retain_until_us,result_state,result_json)
        SELECT catalog_id,'historical-host','retained-command',1,1,?,incarnation,0,generation,1,2,'full','{\"retained\":true}' FROM catalog_state WHERE id=1", ["a".repeat(64)]).unwrap();
    db.execute_batch("COMMIT").unwrap();
    validate(&db, true).unwrap();
    (root, path)
}
fn step34() -> &'static Migration {
    NATIVE_MIGRATIONS
        .iter()
        .find(|step| step.version == 34)
        .expect("schema34 resource required for these tests")
}
fn staged34(path: &Path) -> (Connection, Value) {
    let db = connect(path, true).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let before = snapshot(&db).unwrap();
    execute(&db, step34()).unwrap();
    record_history(&db, 34, false).unwrap();
    db.pragma_update(None, "user_version", 34).unwrap();
    crate::configure_native_connection(&db, 34).unwrap();
    preservation_schema34(&db, &before).unwrap();
    validate(&db, true).unwrap();
    (db, before)
}
fn quiescence() -> NativeUpgradeQuiescence {
    NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}

#[test]
fn populated_native33_latest_upgrade_preserves_state_old_index_history_and_receipts() {
    let (_root, path) = native33();
    let source = connect(&path, false).unwrap();
    let before = snapshot(&source).unwrap();
    let history = validate(&source, true).unwrap()["history"].clone();
    let identities = rows(
        &source,
        "SELECT namespace,value,item_id FROM identities ORDER BY namespace,value,item_id",
        &[],
    )
    .unwrap();
    let old_index = rows(&source, "SELECT entity_kind,entity_key,revision,deleted FROM catalog_entity_revisions ORDER BY entity_kind,entity_key", &[]).unwrap();
    assert!(old_index.iter().any(|row| row["deleted"] == 1));
    assert!(
        old_index
            .iter()
            .any(|row| row["revision"].as_i64().unwrap() > 1)
    );
    drop(source);
    let plan = plan_native(&path).unwrap();
    assert_eq!(plan["from_schema"], 33);
    assert_eq!(plan["to_schema"], NATIVE_SCHEMA_VERSION);
    assert_eq!(
        plan["migrations"],
        json!(
            NATIVE_MIGRATIONS
                .iter()
                .filter(|step| step.version > 33)
                .map(description)
                .collect::<Vec<_>>()
        )
    );
    let result = upgrade_native(&path, &plan, quiescence(), None, &|_, _, _| Ok(())).unwrap();
    assert_eq!(result["schema"], NATIVE_SCHEMA_VERSION);
    assert_eq!(result["applied"], plan["migrations"]);
    let upgraded = connect(&path, false).unwrap();
    preservation_schema34(&upgraded, &before).unwrap();
    let info = validate(&upgraded, true).unwrap();
    assert_eq!(
        &info["history"].as_array().unwrap()[..33],
        history.as_array().unwrap()
    );
    assert_eq!(rows(&upgraded, "SELECT entity_kind,entity_key,revision,deleted FROM catalog_entity_revisions WHERE entity_kind<>'catalog_legacy_identity_evidence' ORDER BY entity_kind,entity_key", &[]).unwrap(), old_index);
    assert_eq!(rows(&upgraded, "SELECT namespace,value,item_id FROM catalog_legacy_identity_evidence ORDER BY namespace,value,item_id", &[]).unwrap(), identities);
    assert_eq!(upgraded.query_row("SELECT count(*) FROM catalog_legacy_identity_evidence WHERE provenance!='legacy_import'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(upgraded.query_row("SELECT count(*) FROM catalog_entity_revisions WHERE entity_kind='catalog_legacy_identity_evidence' AND (revision!=1 OR deleted!=0)", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(
        snapshot(&upgraded).unwrap()["catalog_state"],
        before["catalog_state"]
    );
    assert_eq!(
        snapshot(&upgraded).unwrap()["catalog_command_receipts"],
        before["catalog_command_receipts"]
    );
    let backup = connect(Path::new(result["backup"].as_str().unwrap()), false).unwrap();
    assert_eq!(snapshot(&backup).unwrap(), before);
    assert_eq!(validate(&backup, true).unwrap()["history"], history);
}

#[test]
fn schema34_rejects_modified_deleted_or_additional_old_index_rows() {
    for damage in [
        "UPDATE catalog_entity_revisions SET revision=revision+1 WHERE entity_kind='items' AND entity_key=json_array('retained-a')",
        "UPDATE catalog_entity_revisions SET revision=revision+1,deleted=1 WHERE entity_kind='items' AND entity_key=json_array('retained-a')",
        "DROP TRIGGER catalog_entity_revisions_no_delete; DELETE FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array('deleted-item')",
        "INSERT INTO catalog_entity_revisions VALUES('unexpected-old-kind','[\"phantom\"]',1,0)",
    ] {
        let (_root, path) = native33();
        let (db, before) = staged34(&path);
        db.execute_batch(damage).unwrap();
        let error = preservation_schema34(&db, &before).unwrap_err();
        assert!(
            error.0.contains("changed existing entity index rows"),
            "{damage}: {error}"
        );
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(validate(&db, true).unwrap()["schema"], 33);
        assert_eq!(snapshot(&db).unwrap(), before);
    }
}

#[test]
fn schema34_requires_every_baseline_key_once_at_revision_one_and_not_deleted() {
    for damage in [
        "UPDATE catalog_entity_revisions SET revision=revision+1 WHERE entity_kind='catalog_legacy_identity_evidence' AND entity_key=json_array('imdb','tt-retained','retained-a')",
        "UPDATE catalog_entity_revisions SET revision=revision+1,deleted=1 WHERE entity_kind='catalog_legacy_identity_evidence' AND entity_key=json_array('imdb','tt-retained','retained-a')",
        "DROP TRIGGER catalog_entity_revisions_no_delete; DELETE FROM catalog_entity_revisions WHERE entity_kind='catalog_legacy_identity_evidence' AND entity_key=json_array('imdb','tt-retained','retained-a')",
        "INSERT INTO catalog_entity_revisions VALUES('catalog_legacy_identity_evidence','[\"phantom\",\"value\",\"retained-a\"]',1,0)",
        "INSERT INTO catalog_entity_revisions VALUES('catalog_legacy_identity_evidence','[ \"imdb\", \"tt-retained\", \"retained-a\" ]',1,0)",
    ] {
        let (_root, path) = native33();
        let (db, before) = staged34(&path);
        db.execute_batch(damage).unwrap();
        let error = preservation_schema34(&db, &before).unwrap_err();
        assert!(
            error.0.contains("baseline is not exact"),
            "{damage}: {error}"
        );
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(snapshot(&db).unwrap(), before);
    }
}

#[test]
fn schema34_requires_legacy_evidence_to_equal_the_complete_identity_snapshot() {
    for damage in [
        // Drop corruption-fixture guards so the preservation algorithm, rather
        // than the ordinary immutable-table trigger, is what rejects the fault.
        "DROP TRIGGER catalog_pin_legacy_evidence_no_delete; DELETE FROM catalog_legacy_identity_evidence WHERE namespace='imdb'",
        "DROP TRIGGER catalog_change_catalog_legacy_identity_evidence_insert; INSERT INTO catalog_legacy_identity_evidence VALUES('phantom','value','retained-a','legacy_import'); INSERT INTO catalog_entity_revisions VALUES('catalog_legacy_identity_evidence','[\"phantom\",\"value\",\"retained-a\"]',1,0)",
        "DROP TRIGGER catalog_pin_legacy_evidence_no_update; UPDATE catalog_legacy_identity_evidence SET item_id='retained-b' WHERE namespace='imdb'",
    ] {
        let (_root, path) = native33();
        let (db, before) = staged34(&path);
        db.execute_batch(damage).unwrap();
        let error = preservation_schema34(&db, &before).unwrap_err();
        assert!(
            error.0.contains("baseline is not exact"),
            "{damage}: {error}"
        );
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(snapshot(&db).unwrap(), before);
    }
}

#[test]
fn schema34_rejects_catalog_state_or_other_old_table_mutations() {
    for (damage, table) in [
        (
            "UPDATE catalog_state SET generation=generation+1 WHERE id=1",
            "catalog_state",
        ),
        (
            "INSERT INTO catalogs VALUES('not-a-backfill')",
            "catalog_state",
        ),
    ] {
        let (_root, path) = native33();
        let (db, before) = staged34(&path);
        db.execute_batch(damage).unwrap();
        let error = preservation_schema34(&db, &before).unwrap_err();
        assert!(error.0.contains(table), "{damage}: {error}");
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(snapshot(&db).unwrap(), before);
    }
}

#[test]
fn schema34_exception_is_not_available_to_ordinary_preservation() {
    let (_root, path) = native33();
    let (db, before) = staged34(&path);
    assert!(
        preservation(&db, &before)
            .unwrap_err()
            .0
            .contains("catalog_entity_revisions")
    );
    preservation_schema34(&db, &before).unwrap();
    let already34 = snapshot(&db).unwrap();
    assert!(
        preservation_schema34(&db, &already34)
            .unwrap_err()
            .0
            .contains("must be newly introduced")
    );
}

#[test]
fn schema34_rejects_preexisting_reserved_baseline_kind_without_losing_old_rows() {
    let (_root, path) = native33();
    let db = connect(&path, true).unwrap();
    db.execute("INSERT INTO catalog_entity_revisions VALUES('catalog_legacy_identity_evidence','[\"preexisting\"]',7,1)", []).unwrap();
    let before = snapshot(&db).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    execute(&db, step34()).unwrap();
    assert!(
        preservation_schema34(&db, &before)
            .unwrap_err()
            .0
            .contains("changed existing entity index rows")
    );
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(snapshot(&db).unwrap(), before);
}

#[test]
fn native33_latest_upgrade_fault_boundaries_restore_all_old_rows_and_state() {
    let mut boundaries = Vec::new();
    for version in 34..=NATIVE_SCHEMA_VERSION {
        boundaries.extend([("before_migration", version), ("after_migration", version)]);
    }
    boundaries.push(("before_commit", NATIVE_SCHEMA_VERSION));
    for role in ["rehearsal", "upgrade"] {
        for (boundary, fault_version) in &boundaries {
            let (_root, path) = native33();
            let before_bytes = fs::read(&path).unwrap();
            let before = snapshot(&connect(&path, false).unwrap()).unwrap();
            let plan = plan_native(&path).unwrap();
            let target = format!("{role}:{boundary}");
            let error = upgrade_native(&path, &plan, quiescence(), None, &|stage, _, version| {
                if stage == target && version == *fault_version {
                    Err(Error("injected latest-target interruption".into()))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert!(
                error.0.contains("injected latest-target interruption"),
                "{target}@{fault_version}: {error}"
            );
            let db = connect(&path, false).unwrap();
            assert_eq!(validate(&db, true).unwrap()["schema"], 33);
            assert_eq!(snapshot(&db).unwrap(), before, "{target}@{fault_version}");
            assert_eq!(
                fs::read(&path).unwrap(),
                before_bytes,
                "{target}@{fault_version}"
            );
        }
    }
}

#[test]
fn runner_rejects_old_index_corruption_in_real_rehearsal_and_application() {
    for role in ["rehearsal", "upgrade"] {
        let (_root, path) = native33();
        let plan = plan_native(&path).unwrap();
        let before = snapshot(&connect(&path, false).unwrap()).unwrap();
        let error = upgrade_native(&path, &plan, quiescence(), None, &|stage, db, version| {
            if stage == format!("{role}:before_migration") && version == 34 {
                db.execute("UPDATE catalog_entity_revisions SET revision=revision+1 WHERE entity_kind='items' AND entity_key=json_array('retained-a')", [])?;
            }
            Ok(())
        }).unwrap_err();
        assert!(
            error.0.contains("changed existing entity index rows"),
            "{role}: {error}"
        );
        let db = connect(&path, false).unwrap();
        assert_eq!(validate(&db, true).unwrap()["schema"], 33);
        assert_eq!(snapshot(&db).unwrap(), before);
    }
}

#[test]
fn schema34_crash_worker() {
    let Ok(path) = std::env::var("CATABOLIC_TEST_SCHEMA34_CRASH_DATABASE") else {
        return;
    };
    let stage = std::env::var("CATABOLIC_TEST_SCHEMA34_CRASH_STAGE").unwrap();
    let fault_version: u32 = std::env::var("CATABOLIC_TEST_NATIVE_CRASH_VERSION")
        .unwrap()
        .parse()
        .unwrap();
    let path = Path::new(&path);
    let plan = plan_native(path).unwrap();
    upgrade_native(path, &plan, quiescence(), None, &|observed, _, version| {
        if observed == stage && version == fault_version {
            std::process::exit(86);
        }
        Ok(())
    })
    .unwrap();
    panic!("latest-target crash checkpoint was not reached");
}

#[test]
fn populated_native33_process_exit_is_atomic_at_each_migration_and_latest_commit() {
    let mut boundaries: Vec<_> = (34..=NATIVE_SCHEMA_VERSION)
        .map(|version| ("after_migration", version))
        .collect();
    boundaries.extend([
        ("before_commit", NATIVE_SCHEMA_VERSION),
        ("after_commit", NATIVE_SCHEMA_VERSION),
    ]);
    for role in ["rehearsal", "upgrade"] {
        for (stage, fault_version) in &boundaries {
            let (_root, path) = native33();
            let before = snapshot(&connect(&path, false).unwrap()).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "migration::schema34_preservation_tests::schema34_crash_worker",
                    "--nocapture",
                ])
                .env("CATABOLIC_TEST_SCHEMA34_CRASH_DATABASE", &path)
                .env(
                    "CATABOLIC_TEST_NATIVE_CRASH_VERSION",
                    fault_version.to_string(),
                )
                .env(
                    "CATABOLIC_TEST_SCHEMA34_CRASH_STAGE",
                    format!("{role}:{stage}"),
                )
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(86), "{role}:{stage}@{fault_version}");
            let db = connect(&path, true).unwrap(); // Recovers a hot rollback journal.
            let committed = role == "upgrade" && *stage == "after_commit";
            assert_eq!(
                validate(&db, true).unwrap()["schema"],
                if committed { NATIVE_SCHEMA_VERSION } else { 33 }
            );
            if committed {
                preservation_schema34(&db, &before).unwrap();
            } else {
                assert_eq!(snapshot(&db).unwrap(), before);
            }
            drop(db);
            let plan = plan_native(&path).unwrap();
            assert_eq!(
                upgrade_native(&path, &plan, quiescence(), None, &|_, _, _| Ok(())).unwrap()["schema"],
                NATIVE_SCHEMA_VERSION
            );
        }
    }
}
