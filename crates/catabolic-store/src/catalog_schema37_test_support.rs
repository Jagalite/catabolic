//! Shared test-only, actual populated36→37 upgrade. All Store/host owners must be
//! closed by the fixture before this helper runs. No alternate migration engine.
use crate::{Store, migration};
use std::path::Path;

pub(super) fn upgrade(path: &Path) {
    assert_eq!(
        migration::NATIVE_SCHEMA_VERSION,
        37,
        "this receipt qualifies exact36→37; requalify a later target explicitly"
    );
    let before_store = Store::open(path, false, false).unwrap();
    assert_eq!(before_store.schema_version, 36);
    let before = migration::snapshot(&before_store.db).unwrap();
    let history = migration::validate(&before_store.db, true).unwrap()["history"].clone();
    drop(before_store);
    let plan = migration::plan_native(path).unwrap();
    assert_eq!(plan["from_schema"], 36);
    assert_eq!(plan["to_schema"], 37);
    let receipt = migration::upgrade_native(
        path,
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
    let after = Store::open(path, false, false).unwrap();
    assert_eq!(after.schema_version, 37);
    migration::preservation(&after.db, &before).unwrap();
    let validation = migration::validate(&after.db, true).unwrap();
    assert_eq!(validation["schema"], 37);
    assert_eq!(
        &validation["history"].as_array().unwrap()[..36],
        history.as_array().unwrap()
    );
    assert_eq!(
        after
            .db
            .query_row(
                "SELECT count(*) FROM native_observation_companions",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0,
        "migration cannot fabricate worker lifetime evidence"
    );
    let backup = crate::connect(Path::new(receipt["backup"].as_str().unwrap()), false).unwrap();
    assert_eq!(migration::snapshot(&backup).unwrap(), before);
}
