use catabolic_store::{connect, migration};
use serde_json::{Value, json};

fn quiet() -> migration::NativeUpgradeQuiescence {
    migration::NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}
fn fixture() -> (tempfile::TempDir, std::path::PathBuf, Value, String, String) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.sqlite3");
    migration::initialize_native_at(&path, 39).unwrap();
    let db = connect(&path, false).unwrap();
    let (catalog, incarnation) = db
        .query_row(
            "SELECT catalog_id,incarnation FROM catalog_state WHERE id=1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .unwrap();
    drop(db);
    let plan = migration::plan_native(&path).unwrap();
    (root, path, plan, catalog, incarnation)
}
fn resign(plan: &mut Value) {
    plan["input_fingerprint"] = json!(catabolic_core::sha256(catabolic_core::encode(
        &plan["input"]
    )));
    plan.as_object_mut().unwrap().remove("plan_fingerprint");
    plan["plan_fingerprint"] = json!(catabolic_core::sha256(catabolic_core::encode(plan)));
}
#[test]
fn exact39_to40_reconciliation_requires_positive_complete_preservation() {
    let (_root, path, plan, catalog, incarnation) = fixture();
    assert!(
        migration::with_reconciled_native_upgrade(
            &path,
            &plan,
            &catalog,
            &incarnation,
            quiet(),
            |_| Ok(())
        )
        .is_err()
    );
    migration::upgrade_native(&path, &plan, quiet(), None, &|_, _, _| Ok(())).unwrap();
    let proof = migration::with_reconciled_native_upgrade(
        &path,
        &plan,
        &catalog,
        &incarnation,
        quiet(),
        |proof| Ok(proof.proof().clone()),
    )
    .unwrap();
    assert_eq!(proof["from_schema"], 39);
    assert_eq!(proof["to_schema"], 40);
    assert_eq!(proof["plan_fingerprint"], plan["plan_fingerprint"]);
    assert!(
        migration::with_reconciled_native_upgrade(
            &path,
            &plan,
            "wrong",
            &incarnation,
            quiet(),
            |_| Ok(())
        )
        .is_err()
    );
    assert!(
        migration::with_reconciled_native_upgrade(
            &path,
            &plan,
            &catalog,
            "wrong",
            quiet(),
            |_| Ok(())
        )
        .is_err()
    );
    assert!(
        migration::with_reconciled_native_upgrade(
            &path,
            &plan,
            &catalog,
            &incarnation,
            Default::default(),
            |_| Ok(())
        )
        .is_err()
    );
    let mut pending = plan.clone();
    pending["input"]["pending_work"] = json!({});
    resign(&mut pending);
    assert!(
        migration::with_reconciled_native_upgrade(
            &path,
            &pending,
            &catalog,
            &incarnation,
            quiet(),
            |_| Ok(())
        )
        .is_err()
    );
    let mut omitted = plan.clone();
    omitted["input"]["tables"]
        .as_object_mut()
        .unwrap()
        .remove("profiles");
    resign(&mut omitted);
    assert!(
        migration::with_reconciled_native_upgrade(
            &path,
            &omitted,
            &catalog,
            &incarnation,
            quiet(),
            |_| Ok(())
        )
        .is_err()
    );
    let mut columns = plan.clone();
    columns["input"]["tables"]["profiles"]["columns"] = json!([]);
    resign(&mut columns);
    assert!(
        migration::with_reconciled_native_upgrade(
            &path,
            &columns,
            &catalog,
            &incarnation,
            quiet(),
            |_| Ok(())
        )
        .is_err()
    );
    let db = connect(&path, true).unwrap();
    db.execute(
        "INSERT INTO profiles(id) VALUES('after-unacknowledged-upgrade')",
        [],
    )
    .unwrap();
    drop(db);
    assert!(
        migration::with_reconciled_native_upgrade(
            &path,
            &plan,
            &catalog,
            &incarnation,
            quiet(),
            |_| Ok(())
        )
        .is_err()
    );
}

#[test]
fn upgrade_reconciliation_retains_real_native_exclusive_custody_through_ack() {
    let (_root, path, plan, catalog, incarnation) = fixture();
    migration::upgrade_native(&path, &plan, quiet(), None, &|_, _, _| Ok(())).unwrap();
    // Configure the independent connection before the owner's exclusive fence:
    // connect() reads user_version, which itself is correctly blocked inside it.
    // No transaction or lock is retained by this idle connection.
    let competitor = connect(&path, true).unwrap();
    competitor.busy_timeout(std::time::Duration::ZERO).unwrap();
    migration::with_reconciled_native_upgrade(
        &path,
        &plan,
        &catalog,
        &incarnation,
        quiet(),
        |_| {
            let error = competitor.execute_batch("BEGIN IMMEDIATE").unwrap_err();
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy)
            );
            Ok(())
        },
    )
    .unwrap();
    competitor
        .execute_batch("BEGIN IMMEDIATE;ROLLBACK")
        .unwrap();
}
