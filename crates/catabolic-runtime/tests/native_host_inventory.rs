//! Reopened native hosts inventory recorded ownership without adopting PIDs.
use catabolic_runtime::host::{
    CONTRACT_VERSION, ErrorCode, HostSession, Retry, ScanAdmissionContract,
};
use catabolic_store::{Store, migration};
use rusqlite::params;
fn fixture(schema: u32) -> (tempfile::TempDir, std::path::PathBuf, Store) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, schema).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    store.transaction(|db|{db.execute_batch("INSERT INTO locations VALUES('source');INSERT INTO files VALUES('file','source','file.bin');")?;Ok(())}).unwrap();
    (root, path, store)
}
#[test]
fn inventory_preserves_unknown_claims_and_only_schema37_requires_qualified_admission() {
    for schema in [35, 36, 37] {
        for state in ["running", "failed"] {
            let (_root, path, store) = fixture(schema);
            store.transaction(|db|{db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,attempts) VALUES('job','default','file','hash','source','{}','{}','key',?,1)",[state])?;db.execute("INSERT INTO execution_claims VALUES('job',1,'old-token',?,1,0,'{}','{}')",[std::process::id()])?;Ok(())}).unwrap();
            let before = migration::snapshot(&store.db).unwrap();
            drop(store);
            let mut host = HostSession::open_authorized(
                &path,
                "default",
                true,
                CONTRACT_VERSION,
                catabolic_core::catalog_host_lease::HostAuthorization::new(
                    "host-fixture".into(),
                    "revision-1".into(),
                )
                .unwrap(),
            )
            .unwrap();
            let inventory = host.worker_inventory().unwrap();
            assert!(inventory.native);
            assert!(!inventory.admits_new_work());
            assert!(
                inventory
                    .unresolved
                    .iter()
                    .any(|owner| owner.kind == "processing" && owner.id == "job")
            );
            let failure = host.start_scan("source", ()).unwrap_err();
            if schema >= 37 {
                assert_eq!(
                    host.scan_admission_contract().unwrap(),
                    ScanAdmissionContract::ExactCleanupV2
                );
                assert_eq!(failure.code, ErrorCode::RecoveryRequired);
                assert_eq!(failure.retry, Retry::Reconcile);
            } else {
                assert_eq!(
                    host.scan_admission_contract().unwrap(),
                    ScanAdmissionContract::RetainedChildV1
                );
                assert_eq!(failure.code, ErrorCode::MissingTool);
            }
            assert!(host.active_scan().is_none());
            host.close().unwrap();
            let store = Store::open(&path, false, false).unwrap();
            assert_eq!(migration::snapshot(&store.db).unwrap(), before);
            // SAFETY: signal zero checks this still-running fixture process, never signals it.
            assert_eq!(unsafe { libc::kill(std::process::id() as i32, 0) }, 0);
        }
    }
}
#[test]
fn terminal_observation_rows_and_uncertain_probe_or_api_owners_are_not_cleanup() {
    for schema in [35, 36, 37] {
        for category in ["observation", "fallback_probe", "api_worker"] {
            let (_root, path, store) = fixture(schema);
            store.transaction(|db|{match category{
 "observation"=>{db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,worker_pid,claim_token) VALUES('past-scan','default','source','same','[]',0,1,'complete',1,NULL,'claim')",[])?;},
 "fallback_probe"=>{db.execute("INSERT OR REPLACE INTO fallback_probe_slots VALUES(1,'uncertain-owner',NULL)",[])?;},
 "api_worker"=>{db.execute("INSERT INTO api_worker_status(profile,worker_pid,capabilities,updated_at) VALUES('default',?,'{}',0)",[std::process::id()])?;},_=>unreachable!()};Ok(())}).unwrap();
            let before = migration::snapshot(&store.db).unwrap();
            drop(store);
            let mut host = HostSession::open_authorized(
                &path,
                "default",
                true,
                CONTRACT_VERSION,
                catabolic_core::catalog_host_lease::HostAuthorization::new(
                    "host-fixture".into(),
                    "revision-1".into(),
                )
                .unwrap(),
            )
            .unwrap();
            assert!(
                host.worker_inventory()
                    .unwrap()
                    .unresolved
                    .iter()
                    .any(|entry| entry.kind == category)
            );
            assert_eq!(
                host.start_scan("source", ()).unwrap_err().code,
                if schema >= 37 {
                    ErrorCode::RecoveryRequired
                } else {
                    ErrorCode::MissingTool
                }
            );
            host.close().unwrap();
            assert_eq!(
                migration::snapshot(&Store::open(&path, false, false).unwrap().db).unwrap(),
                before
            );
        }
    }
}
#[test]
fn inventory_overflow_is_explicit_and_does_not_admit_or_mutate() {
    let (_root, path, store) = fixture(37);
    store.transaction(|db|{for n in 0..257{db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,attempts) VALUES(?,'default','file','hash','source','{}','{}',?,'queued',1)",params![format!("job-{n}"),format!("key-{n}")])?;}Ok(())}).unwrap();
    let before = migration::snapshot(&store.db).unwrap();
    drop(store);
    let mut host = HostSession::open_authorized(
        &path,
        "default",
        true,
        CONTRACT_VERSION,
        catabolic_core::catalog_host_lease::HostAuthorization::new(
            "host-fixture".into(),
            "revision-1".into(),
        )
        .unwrap(),
    )
    .unwrap();
    let report = host.worker_inventory().unwrap();
    assert!(report.truncated);
    assert_eq!(report.unresolved.len(), 256);
    assert!(!report.admits_new_work());
    assert_eq!(
        host.start_scan("source", ()).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    host.close().unwrap();
    assert_eq!(
        migration::snapshot(&Store::open(&path, false, false).unwrap().db).unwrap(),
        before
    );
}

#[test]
fn native_ungated_constructor_cannot_bypass_the_shared_host_gate() {
    for schema in [35, 36, 37] {
        let (_root, path, store) = fixture(schema);
        drop(store);
        let error = match HostSession::open(&path, "default", true, CONTRACT_VERSION) {
            Ok(_) => panic!("ungated native host opened"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::UnsupportedCapability);
    }
}

#[test]
fn explicit36_upgrade_keeps_history_but_cannot_downgrade37_scan_admission() {
    let (_root, path, store) = fixture(36);
    store.transaction(|db|{db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,worker_pid,claim_token) VALUES('inherited','default','source','old','[]',0,1,'complete',0,NULL,'unwitnessed')",[])?;Ok(())}).unwrap();
    drop(store);
    let authority = catabolic_core::catalog_host_lease::HostAuthorization::new(
        "upgrade-fixture".into(),
        "r1".into(),
    )
    .unwrap();
    let mut before =
        HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, authority.clone())
            .unwrap();
    assert_eq!(
        before.scan_admission_contract().unwrap(),
        ScanAdmissionContract::RetainedChildV1
    );
    assert!(!before.worker_inventory().unwrap().admits_new_work());
    assert_eq!(
        before.start_scan("source", ()).unwrap_err().code,
        ErrorCode::MissingTool
    );
    before.close().unwrap();
    drop(before);
    let plan = migration::plan_native(&path).unwrap();
    assert_eq!(plan["to_schema"], 39);
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
    let mut after =
        HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, authority).unwrap();
    assert_eq!(
        after.scan_admission_contract().unwrap(),
        ScanAdmissionContract::ExactCleanupV2
    );
    assert_eq!(
        after.start_scan("source", ()).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    after.close().unwrap();
    let store = Store::open(&path, false, false).unwrap();
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
        store
            .db
            .query_row(
                "SELECT claim_token FROM observation_jobs WHERE id='inherited'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "unwitnessed"
    );
}
