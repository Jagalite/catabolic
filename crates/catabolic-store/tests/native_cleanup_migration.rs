//! Additive cleanup migration: old data is retained and ownership is never backfilled.
use catabolic_store::{Store, migration};
#[test]
fn native35_upgrade_to_latest_preserves_every_prior_table_and_frozen_history() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, 35).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store
        .transaction(|db| {
            db.execute("INSERT INTO locations VALUES('retained-source')", [])?;
            Ok(())
        })
        .unwrap();
    let before = migration::snapshot(&store.db).unwrap();
    let history = migration::validate(&store.db, true).unwrap()["history"].clone();
    drop(store);
    let plan = migration::plan_native(&path).unwrap();
    assert_eq!(plan["from_schema"], 35);
    assert_eq!(plan["to_schema"], migration::NATIVE_SCHEMA_VERSION);
    let result = migration::upgrade_native(
        &path,
        &plan,
        migration::NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        },
        None,
        &|_, _, _| Ok(()),
    )
    .unwrap();
    let store = Store::open(&path, false, false).unwrap();
    let after = migration::snapshot(&store.db).unwrap();
    for (table, value) in before.as_object().unwrap() {
        if table != "schema_migrations" {
            assert_eq!(&after[table], value, "prior table {table}");
        }
    }
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM native_execution_lifetimes", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        &migration::validate(&store.db, true).unwrap()["history"]
            .as_array()
            .unwrap()[..35],
        history.as_array().unwrap()
    );
    let backup = catabolic_store::connect(
        std::path::Path::new(result["backup"].as_str().unwrap()),
        false,
    )
    .unwrap();
    assert_eq!(migration::snapshot(&backup).unwrap(), before);
}
#[test]
fn schema36_fresh_catalog_has_empty_lifetimes_and_exact_signature() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, 36).unwrap();
    let store = Store::open(&path, false, false).unwrap();
    assert_eq!(store.schema_version, 36);
    assert_eq!(migration::validate(&store.db, true).unwrap()["schema"], 36);
    assert_eq!(
        migration::inspect_native(&path, true).unwrap()["pending_work"]["native_execution_lifetimes"],
        0
    );
}
