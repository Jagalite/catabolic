//! Cancelling one consumer must preserve another consumer’s shared demand.
use catabolic_store::{Store, migration};
use serde_json::json;

#[test]
fn cancelling_one_observation_demand_preserves_other_demand_and_job() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    migration::initialize_native_at(&path, 33).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store.transaction(|db| {
        db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,started_at,lease_until,worker_pid,guarded,execution,claim_token) VALUES('11111111-1111-4111-8111-111111111111','default','media','same','[]',0,1,'running',1,1,9999999999,?,1,'{}','claim')",[std::process::id()])?;
        for (id,requester) in [("demand-a","host-a"),("demand-b","host-b")] {
            db.execute("INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,created_at) VALUES(?,'default','media',?,'{}','11111111-1111-4111-8111-111111111111',1)",[id,requester])?;
        }
        Ok(())
    }).unwrap();
    catabolic_store::observation::execute(
        &store,
        "default",
        "cancel",
        &json!({"request_id":"demand-a"}),
    )
    .unwrap();
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT state FROM observation_requests WHERE id='demand-a'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "cancelled"
    );
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT state FROM observation_requests WHERE id='demand-b'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "active"
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT state FROM observation_jobs", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "running"
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT claim_token FROM observation_jobs", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        "claim"
    );
}
