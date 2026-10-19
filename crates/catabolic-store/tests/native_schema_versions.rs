//! Version-evolution regression coverage. Schema 33 remains an explicitly selected
//! historical fixture when the latest native target advances.
use catabolic_core::decode_json;
use catabolic_store::{OpenErrorCode, Store, connect, migration, native_admission_exclusive};
use migration::{FIRST_NATIVE_SCHEMA_VERSION, NATIVE_SCHEMA_VERSION, SCHEMA_VERSION};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::Path};

fn assert_connection_prerequisites(db: &Connection) {
    for (pragma, expected) in [
        ("recursive_triggers", true),
        ("foreign_keys", true),
        ("read_uncommitted", false),
    ] {
        assert_eq!(
            db.pragma_query_value(None, pragma, |row| row.get::<_, bool>(0))
                .unwrap(),
            expected,
            "{pragma}"
        );
    }
}

fn assert_schema(path: &Path, version: u32) -> Value {
    let db = connect(path, false).unwrap();
    let info = migration::validate(&db, true).unwrap();
    assert_eq!(info["schema"], version);
    assert_eq!(info["history"].as_array().unwrap().len(), version as usize);
    for (entry, step) in info["history"]
        .as_array()
        .unwrap()
        .iter()
        .zip(migration::NATIVE_MIGRATIONS)
    {
        let expected = migration::description(step);
        for field in ["version", "filename", "checksum"] {
            assert_eq!(entry[field], expected[field]);
        }
    }
    info
}

#[test]
fn version_boundaries_and_signature_lookup_are_consistent() {
    assert_eq!(SCHEMA_VERSION, 32);
    assert_eq!(FIRST_NATIVE_SCHEMA_VERSION, 33);
    assert_eq!(migration::MIGRATIONS.len(), SCHEMA_VERSION as usize);
    assert_eq!(
        migration::NATIVE_MIGRATIONS.len(),
        NATIVE_SCHEMA_VERSION as usize
    );
    assert!(std::ptr::eq(
        migration::native_reference(),
        migration::native_reference_at(NATIVE_SCHEMA_VERSION).unwrap()
    ));
    assert_eq!(
        migration::native_reference_at(33).unwrap(),
        &decode_json(include_str!("../resources/native-schema-33.json")).unwrap()
    );
    for version in 0..=NATIVE_SCHEMA_VERSION + 1 {
        let supported = (FIRST_NATIVE_SCHEMA_VERSION..=NATIVE_SCHEMA_VERSION).contains(&version);
        assert_eq!(migration::is_supported_native_schema(version), supported);
        assert_eq!(migration::native_reference_at(version).is_some(), supported);
    }
    assert!(!migration::is_supported_native_schema(u32::MAX));
    assert!(migration::native_reference_at(u32::MAX).is_none());
    for (legacy, native) in migration::MIGRATIONS
        .iter()
        .zip(migration::NATIVE_MIGRATIONS)
    {
        assert_eq!(
            migration::description(legacy),
            migration::description(native)
        );
    }
}

#[test]
fn pinned_schema33_remains_openable_without_automatic_upgrade() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("frozen33.sqlite3");
    assert_eq!(
        migration::initialize_native_at(&path, 33).unwrap()["schema"],
        33
    );
    let before = assert_schema(&path, 33);
    let expected_pending = json!(
        migration::NATIVE_MIGRATIONS
            .iter()
            .skip(33)
            .map(migration::description)
            .collect::<Vec<_>>()
    );
    assert_eq!(before["pending_migrations"], expected_pending);
    assert_eq!(before["latest_schema"], NATIVE_SCHEMA_VERSION);
    for writable in [false, true] {
        let store = Store::open_typed(&path, writable, false).unwrap();
        assert_eq!(store.schema_version, 33);
        assert!(store.catalog_incarnation.is_some());
        assert_connection_prerequisites(&store.db);
        store.validate_native_identity().unwrap();
        assert!(native_admission_exclusive(&path).is_err());
        assert!(
            store
                .db
                .query_row("SELECT catabolic_catalog_id_v1('unused')", [], |row| {
                    row.get::<_, String>(0)
                })
                .is_err(),
            "cutover-only UDF must not leak into opened catalogs"
        );
    }
    assert_eq!(assert_schema(&path, 33), before);
    native_admission_exclusive(&path).unwrap();
}

#[test]
fn all_supported_native_targets_validate_and_require_native_connection_settings() {
    let root = tempfile::tempdir().unwrap();
    for version in FIRST_NATIVE_SCHEMA_VERSION..=NATIVE_SCHEMA_VERSION {
        let path = root.path().join(format!("native-{version}.sqlite3"));
        assert_eq!(
            migration::initialize_native_at(&path, version).unwrap()["schema"],
            version
        );
        assert_schema(&path, version);
        for (pragma, unsafe_value) in [
            ("recursive_triggers", false),
            ("foreign_keys", false),
            ("read_uncommitted", true),
        ] {
            let db = connect(&path, true).unwrap();
            assert_connection_prerequisites(&db);
            db.pragma_update(None, pragma, unsafe_value).unwrap();
            let error = migration::validate_typed(&db, false).unwrap_err();
            assert_eq!(error.code, OpenErrorCode::InvalidCatalog);
            assert!(error.to_string().contains("connection requires"));

            let store = Store::open(&path, true, false).unwrap();
            store.db.pragma_update(None, pragma, unsafe_value).unwrap();
            assert!(
                store
                    .validate_native_identity()
                    .unwrap_err()
                    .to_string()
                    .contains("unsafe native catalog connection settings")
            );
        }
    }
}

#[test]
fn unsupported_initialization_targets_are_rejected_before_filesystem_effects() {
    let root = tempfile::tempdir().unwrap();
    let existing = root.path().join("existing.sqlite3");
    fs::write(&existing, b"untouched sentinel").unwrap();
    for target in [0, 1, 32, NATIVE_SCHEMA_VERSION + 1, u32::MAX] {
        for path in [
            root.path().join(format!("unsupported-{target}.sqlite3")),
            root.path().join("missing-parent/catalog.sqlite3"),
            existing.clone(),
        ] {
            let error = migration::initialize_native_at(&path, target).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("unsupported native schema target"),
                "{target}: {error}"
            );
        }
        assert_eq!(fs::read(&existing).unwrap(), b"untouched sentinel");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

#[test]
fn explicit_native_initialization_refuses_existing_files_without_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("existing.sqlite3");
    migration::initialize_native_at(&path, 33).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(migration::initialize_native_at(&path, 33).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_schema(&path, 33);
}

#[test]
fn recovery_remains_refused_for_every_supported_native_version() {
    let root = tempfile::tempdir().unwrap();
    for version in FIRST_NATIVE_SCHEMA_VERSION..=NATIVE_SCHEMA_VERSION {
        let path = root.path().join(format!("recovery-{version}.sqlite3"));
        migration::initialize_native_at(&path, version).unwrap();
        for writable in [false, true] {
            let error = match Store::open_typed(&path, writable, true) {
                Ok(_) => panic!("native recovery was admitted for schema {version}"),
                Err(error) => error,
            };
            assert_eq!(error.code, OpenErrorCode::UnsupportedSchema);
            assert!(
                error
                    .to_string()
                    .contains("separately qualified recovery admission")
            );
        }
        assert_schema(&path, version);
    }
}

#[test]
fn legacy_defaults_and_upgrade_stay_at_schema32() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("legacy.sqlite3");
    assert_eq!(migration::initialize(&path).unwrap()["schema"], 32);
    let before = assert_schema(&path, 32);
    assert_eq!(before["latest_schema"], 32);
    assert_eq!(before["pending_migrations"], json!([]));
    let report = migration::upgrade(&path, false, None, &|_, _, _| Ok(())).unwrap();
    assert_eq!(report["schema"], 32);
    assert_eq!(report["upgraded"], false);
    assert_eq!(assert_schema(&path, 32), before);
    let store = Store::open(&path, true, false).unwrap();
    assert!(store.catalog_incarnation.is_none());
    assert_eq!(store.schema_version, 32);
    assert!(
        !store
            .db
            .pragma_query_value(None, "recursive_triggers", |row| row.get::<_, bool>(0))
            .unwrap()
    );
    assert!(
        !store
            .db
            .prepare("SELECT 1 FROM sqlite_schema WHERE name='catalog_state'")
            .unwrap()
            .exists([])
            .unwrap()
    );
    drop(store);
    let latest = root.path().join("latest.sqlite3");
    assert_eq!(
        migration::initialize_native(&latest).unwrap()["schema"],
        NATIVE_SCHEMA_VERSION
    );
    assert_schema(&latest, NATIVE_SCHEMA_VERSION);
}

#[test]
fn unsupported_open_versions_never_fall_back_to_a_native_signature() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("unknown.sqlite3");
    migration::initialize_native_at(&path, 33).unwrap();
    let db = connect(&path, true).unwrap();
    db.pragma_update(None, "user_version", NATIVE_SCHEMA_VERSION + 1)
        .unwrap();
    assert_eq!(
        migration::validate_typed(&db, false).unwrap_err().code,
        OpenErrorCode::UnsupportedSchema
    );
    drop(db);
    for for_recovery in [false, true] {
        let error = match Store::open_typed(&path, false, for_recovery) {
            Ok(_) => panic!("unknown future native schema was admitted"),
            Err(error) => error,
        };
        assert_eq!(error.code, OpenErrorCode::UnsupportedSchema);
    }
}

#[test]
fn native_plan_still_binds_latest_signature_and_target_limited_chain() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("frozen33.sqlite3");
    migration::initialize_native_at(&path, 33).unwrap();
    let plan = migration::plan_native(&path).unwrap();
    assert_eq!(plan["from_schema"], 33);
    assert_eq!(plan["to_schema"], NATIVE_SCHEMA_VERSION);
    assert_eq!(
        plan["native_schema_fingerprint"],
        catabolic_core::sha256(catabolic_core::encode(
            migration::native_reference_at(NATIVE_SCHEMA_VERSION).unwrap()
        ))
    );
    assert_eq!(
        plan["migrations"],
        json!(
            migration::NATIVE_MIGRATIONS
                .iter()
                .skip(33)
                .map(migration::description)
                .collect::<Vec<_>>()
        )
    );
    assert_eq!(
        plan["migration_resources"].as_array().unwrap().len(),
        NATIVE_SCHEMA_VERSION as usize
    );
}

#[test]
fn existing_durable_commands_commit_and_replay_on_pinned_schema33() {
    use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
    use catabolic_store::catalog_commands::{self, ReceiptPolicy, TrustedCommandAuthority};
    use std::time::Duration;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("commands33.sqlite3");
    migration::initialize_native_at(&path, 33).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, "historical-host", "pinned33-put").unwrap(),
        store.catalog_incarnation.clone().unwrap(),
        0,
        &json!({"operation":"item_put","item":{"id":"retained-item","kind":"video","metadata":{"title":"retained"}}}).to_string(),
    )
    .unwrap();
    let authority = TrustedCommandAuthority {
        host_namespace: "historical-host",
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    };
    let policy = ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 10,
        max_logical_bytes: 1024 * 1024,
        max_hold_records: 10,
    };
    assert_eq!(
        catalog_commands::execute(&store, &command, authority, policy)
            .unwrap()
            .code(),
        "committed"
    );
    drop(store);
    let store = Store::open(&path, true, false).unwrap();
    assert_eq!(
        catalog_commands::execute(&store, &command, authority, policy)
            .unwrap()
            .code(),
        "replay"
    );
    assert_eq!(catalog_commands::receipt_usage(&store).unwrap().receipts, 1);
    assert_eq!(store.schema_version, 33);
    assert_eq!(assert_schema(&path, 33)["schema"], 33);
}
