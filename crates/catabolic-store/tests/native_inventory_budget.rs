//! Inventory-budget fixtures model already validated immutable runtime seals.
//! They do not assert actual Child exit; process-proof tests are separate.
use catabolic_core::sha256;
use catabolic_store::{Store, migration, native_worker_inventory};
use rusqlite::params;

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn fixture() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, 37).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    (root, store)
}
fn job_id(index: usize) -> String {
    format!("00000000-0000-4000-8000-{index:012x}")
}
fn seed(store: &Store, start: usize, count: usize, sealed: bool) {
    // Keep synthetic fixture writes below the ordinary transaction work budget.
    for first in (start..start + count).step_by(128) {
        seed_chunk(store, first, (start + count - first).min(128), sealed);
    }
}
fn seed_chunk(store: &Store, start: usize, count: usize, sealed: bool) {
    store.transaction(|db| {
        let mut job = db.prepare("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,lease_until,worker_pid,claim_token,guarded) VALUES(?,'default','media','fixture','[]',0,?,'complete',0,0,?,?,1)")?;
        let mut request = db.prepare("INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,created_at) VALUES(?,'default','media',?,'{}',?,0)")?;
        let mut reserve = db.prepare("INSERT INTO native_observation_companions(reservation_nonce,catalog_id,incarnation,protocol_kind,protocol_version,helper_digest,profile,source,requester,args_digest) VALUES(?,?,?,'native_observation_no_child',1,?,'default','media',?,?)")?;
        let mut started = db.prepare("UPDATE native_observation_companions SET state='started',child_pid=? WHERE reservation_nonce=?")?;
        let mut completed = db.prepare("UPDATE native_observation_companions SET state='completed',request_id=?,demand_job_id=?,owned_job_id=?,owned_generation=?,owned_claim_token_digest=? WHERE reservation_nonce=?")?;
        let mut seal = db.prepare("UPDATE native_observation_companions SET state='quiescent',cleanup_kind='closed_child_exit_v1',sealed_at=CURRENT_TIMESTAMP WHERE reservation_nonce=?")?;
        for index in start..start+count {
            let id = job_id(index);
            let generation = i64::try_from(index+1).unwrap();
            let token = uuid::Uuid::new_v4().to_string();
            job.execute(params![id,generation,std::process::id(),token])?;
            if !sealed { continue; }
            let request_id = uuid::Uuid::new_v4().to_string();
            let requester = format!("seed:{index}");
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            request.execute(params![request_id,requester,id])?;
            reserve.execute(params![nonce,store.database_id,store.catalog_incarnation,DIGEST,requester,DIGEST])?;
            started.execute(params![std::process::id(),nonce])?;
            completed.execute(params![request_id,id,id,generation,sha256(token),nonce])?;
            seal.execute([&nonce])?;
        }
        Ok(())
    }).unwrap();
}
fn inspect_unchanged(store: &Store) -> native_worker_inventory::Inventory {
    let changes = store.db.total_changes();
    let generation: i64 = store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let report = native_worker_inventory::inspect(store).unwrap();
    assert_eq!(store.db.total_changes(), changes);
    assert_eq!(
        store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        generation
    );
    report
}
#[test]
fn more_than256_exact_witnesses_admit_only_after_full_reconciliation() {
    let (_root, store) = fixture();
    seed(&store, 0, 300, true);
    let report = inspect_unchanged(&store);
    assert_eq!(report.witnessed_observation, 300);
    assert!(report.unresolved.is_empty());
    assert!(!report.truncated);
    assert!(report.admits_new_work());
}
#[test]
fn unresolved_owner_after300_witnesses_is_not_skipped() {
    let (_root, store) = fixture();
    seed(&store, 0, 300, true);
    seed(&store, 300, 1, false);
    let report = inspect_unchanged(&store);
    assert_eq!(report.witnessed_observation, 300);
    assert!(!report.truncated);
    assert_eq!(
        report.unresolved,
        vec![native_worker_inventory::UnresolvedOwner {
            kind: "observation".into(),
            id: job_id(300)
        }]
    );
    assert!(!report.admits_new_work());
}
#[test]
fn malformed_late_claim_cannot_hide_behind_its_old_seal() {
    let (_root, store) = fixture();
    seed(&store, 0, 301, true);
    store
        .transaction(|db| {
            db.execute(
                "UPDATE observation_jobs SET claim_token='malformed' WHERE id=?",
                [job_id(300)],
            )?;
            Ok(())
        })
        .unwrap();
    let report = inspect_unchanged(&store);
    assert_eq!(report.witnessed_observation, 300);
    assert!(!report.truncated);
    assert_eq!(report.unresolved.len(), 1);
    assert_eq!(report.unresolved[0].id, job_id(300));
    assert!(!report.admits_new_work());
}
#[test]
fn raw_work_budget_exhaustion_refuses_even_when_every_examined_owner_is_witnessed() {
    let (_root, store) = fixture();
    seed(&store, 0, 4097, true);
    let report = inspect_unchanged(&store);
    assert_eq!(report.witnessed_observation, 4096);
    assert!(report.unresolved.is_empty());
    assert!(report.truncated);
    assert!(!report.admits_new_work());
}
#[test]
fn exact4096_owner_boundary_admits_after_confirmed_end_of_cursor() {
    let (_root, store) = fixture();
    seed(&store, 0, 4096, true);
    let report = inspect_unchanged(&store);
    assert_eq!(report.witnessed_observation, 4096);
    assert!(report.unresolved.is_empty());
    assert!(!report.truncated);
    assert!(report.admits_new_work());
}
#[test]
fn unresolved_response_sample_remains256_and_never_opens_admission() {
    let (_root, store) = fixture();
    seed(&store, 0, 257, false);
    let report = inspect_unchanged(&store);
    assert_eq!(report.unresolved.len(), 256);
    assert_eq!(report.witnessed_observation, 0);
    assert!(report.truncated);
    assert!(!report.admits_new_work());
}
