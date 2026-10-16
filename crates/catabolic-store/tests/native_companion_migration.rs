//! Targeted candidate37 migration preservation; not independent schema review.
use catabolic_store::{Store, migration, native_worker_inventory};
#[test]
fn schema37_fresh_has_exact_signature_and_no_invented_companions() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, 37).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    assert_eq!(migration::validate(&store.db, true).unwrap()["schema"], 37);
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM native_observation_companions",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        migration::inspect_native(&path, true).unwrap()["pending_work"]["native_observation_companions"],
        0
    );
    assert!(
        native_worker_inventory::inspect(&store)
            .unwrap()
            .admits_new_work()
    );
}
#[test]
fn upgrade36_preserves_every_prior_table_and_history_without_backfill() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, 36).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store.transaction(|db|{db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,lease_until,worker_pid,claim_token) VALUES('historical','default','old','old','[]',0,1,'complete',0,0,1,'historical-token')",[])?;Ok(())}).unwrap();
    let before = migration::snapshot(&store.db).unwrap();
    let history = migration::validate(&store.db, true).unwrap()["history"].clone();
    drop(store);
    let plan = migration::plan_native(&path).unwrap();
    assert_eq!(plan["from_schema"], 36);
    assert_eq!(plan["to_schema"], 38);
    migration::upgrade_native(
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
    let store = Store::open(&path, true, false).unwrap();
    let after = migration::snapshot(&store.db).unwrap();
    for (table, value) in before.as_object().unwrap() {
        if table != "schema_migrations" {
            assert_eq!(&after[table], value, "{table}");
        }
    }
    assert_eq!(
        &migration::validate(&store.db, true).unwrap()["history"]
            .as_array()
            .unwrap()[..36],
        history.as_array().unwrap()
    );
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM native_observation_companions",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        native_worker_inventory::inspect(&store)
            .unwrap()
            .unresolved
            .len(),
        1
    );
}
