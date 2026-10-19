//! Candidate37 targeted store protocol tests. Direct seals below model the
//! runtime transaction only; they are not evidence of actual process exit.
use catabolic_store::{
    Error, Store, migration, native_observation_companion as companion, native_worker_inventory,
    sql,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt};
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn fixture() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, 37).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("one.txt"), "one").unwrap();
    let m = fs::metadata(&source).unwrap();
    // Fixture has no enrolled UUID. No production validation policy is changed.
    store
        .transaction(|db| {
            db.execute("INSERT INTO locations VALUES('media')", [])?;
            db.execute(
                "INSERT INTO bindings VALUES('default','source','media',?,?,?)",
                params![source.to_str().unwrap(), m.dev() as i64, m.ino() as i64],
            )?;
            Ok(())
        })
        .unwrap();
    (root, store)
}
fn args(requester: &str) -> Value {
    json!({"source":"media","requester":requester,"max_age":3600})
}
fn reserve(store: &Store, requester: &str) -> companion::Reservation {
    companion::reserve(
        store,
        "default",
        "media",
        requester,
        DIGEST,
        &args(requester),
    )
    .unwrap()
}
fn completed(store: &mut Store, requester: &str) -> companion::Record {
    let reservation = reserve(store, requester);
    companion::execute_closed(store, &reservation, DIGEST, &args(requester)).unwrap();
    companion::read(store, &reservation.reservation_nonce)
        .unwrap()
        .unwrap()
}
fn seal_sql(store: &Store, record: &companion::Record) -> catabolic_store::Result<()> {
    assert!(companion::completion_matches(store, record)?);
    let r = &record.reservation;
    assert_eq!(store.db.execute("UPDATE native_observation_companions SET state='quiescent',cleanup_kind='closed_child_exit_v1',sealed_at=CURRENT_TIMESTAMP WHERE reservation_nonce=? AND catalog_id=? AND incarnation=? AND protocol_kind='native_observation_no_child' AND protocol_version=1 AND helper_digest=? AND profile=? AND source=? AND requester=? AND args_digest=? AND state='completed' AND child_pid=? AND request_id=? AND demand_job_id=? AND owned_job_id IS ? AND owned_generation IS ? AND owned_claim_token_digest IS ? AND cleanup_kind IS NULL AND sealed_at IS NULL", params![r.reservation_nonce,r.catalog_id,r.incarnation,r.helper_digest,r.profile,r.source,r.requester,r.args_digest,record.child_pid,record.request_id,record.demand_job_id,record.owned.as_ref().map(|c|&c.job_id),record.owned.as_ref().map(|c|c.generation),record.owned.as_ref().map(|c|&c.claim_token_digest)])?,1);
    Ok(())
}
fn job(store: &Store, id: &str, state: &str, pid: Option<i64>, claim: Option<&str>) {
    store.transaction(|db| {
        db.execute("INSERT OR IGNORE INTO observation_sources VALUES('default','media',0)",[])?;
        db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,lease_until,worker_pid,claim_token,guarded) VALUES(?,'default','media','old','[]',0,99,?,0,0,?,?,1)",params![id,state,pid,claim])?;
        Ok(())
    }).unwrap();
}
#[test]
fn successful_closed_return_is_completed_but_unresolved_until_exact_seal() {
    let (_root, mut store) = fixture();
    let record = completed(&mut store, "first");
    assert_eq!(record.state, "completed");
    assert_eq!(record.child_pid, Some(std::process::id()));
    let owned = record.owned.as_ref().unwrap();
    assert!(!companion::has_cleanup_witness(&store, &owned.job_id).unwrap());
    let inventory = native_worker_inventory::inspect(&store).unwrap();
    assert_eq!(inventory.unresolved.len(), 2);
    assert_eq!(
        migration::inspect_native(&store.path, false).unwrap()["pending_work"]["native_observation_companions"],
        1
    );
    store.transaction(|_| seal_sql(&store, &record)).unwrap();
    assert!(companion::has_cleanup_witness(&store, &owned.job_id).unwrap());
    let inventory = native_worker_inventory::inspect(&store).unwrap();
    assert_eq!(inventory.witnessed_observation, 1);
    assert!(inventory.admits_new_work());
    let path = store.path.clone();
    drop(store);
    let reopened = Store::open(&path, true, false).unwrap();
    assert!(
        native_worker_inventory::inspect(&reopened)
            .unwrap()
            .admits_new_work()
    );
}
#[test]
fn reservation_rejects_wrong_nonce_helper_binding_and_arguments_without_admission() {
    let (_root, mut store) = fixture();
    let r = reserve(&store, "request");
    let before = migration::snapshot(&store.db).unwrap();
    for changed in [
        "nonce",
        "helper",
        "catalog",
        "incarnation",
        "profile",
        "source",
        "requester",
        "args",
        "protocol",
    ] {
        let mut wrong = r.clone();
        match changed {
            "nonce" => wrong.reservation_nonce = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            "helper" => {
                wrong.helper_digest =
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()
            }
            "catalog" => wrong.catalog_id = uuid::Uuid::new_v4().to_string(),
            "incarnation" => wrong.incarnation = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            "profile" => wrong.profile = "wrong".into(),
            "source" => wrong.source = "wrong".into(),
            "requester" => wrong.requester = "wrong".into(),
            "args" => {
                wrong.args_digest =
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()
            }
            _ => wrong.protocol_version = 2,
        }
        assert!(
            companion::execute_closed(&mut store, &wrong, DIGEST, &args("request")).is_err(),
            "{changed}"
        );
        assert_eq!(migration::snapshot(&store.db).unwrap(), before, "{changed}");
    }
    assert!(
        companion::execute_closed(
            &mut store,
            &r,
            DIGEST,
            &json!({"source":"media","requester":"request","max_age":0})
        )
        .is_err()
    );
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
}
#[test]
fn reserved_and_completed_identities_and_seals_are_immutable() {
    let (_root, mut store) = fixture();
    let r = reserve(&store, "request");
    for statement in [
        "UPDATE native_observation_companions SET requester='changed'",
        "UPDATE native_observation_companions SET reservation_nonce='bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'",
        "UPDATE native_observation_companions SET helper_digest='bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'",
        "UPDATE native_observation_companions SET state='quiescent',cleanup_kind='closed_child_exit_v1',sealed_at=CURRENT_TIMESTAMP",
        "DELETE FROM native_observation_companions",
        "INSERT OR REPLACE INTO native_observation_companions SELECT * FROM native_observation_companions",
    ] {
        assert!(
            store
                .transaction(|db| {
                    db.execute(statement, [])?;
                    Ok(())
                })
                .is_err(),
            "{statement}"
        );
    }
    companion::execute_closed(&mut store, &r, DIGEST, &args("request")).unwrap();
    let record = companion::read(&store, &r.reservation_nonce)
        .unwrap()
        .unwrap();
    for statement in [
        "UPDATE native_observation_companions SET state='quiescent',cleanup_kind='closed_child_exit_v1',sealed_at=CURRENT_TIMESTAMP,owned_generation=999",
        "UPDATE native_observation_companions SET state='quiescent',cleanup_kind='closed_child_exit_v1',sealed_at=CURRENT_TIMESTAMP,child_pid=1",
        "UPDATE native_observation_companions SET state='quiescent',cleanup_kind='closed_child_exit_v1',sealed_at=CURRENT_TIMESTAMP,owned_claim_token_digest='bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'",
    ] {
        assert!(
            store
                .transaction(|db| {
                    db.execute(statement, [])?;
                    Ok(())
                })
                .is_err()
        );
    }
    store.transaction(|_| seal_sql(&store, &record)).unwrap();
    assert!(store.transaction(|db|{db.execute("UPDATE native_observation_companions SET state='completed',cleanup_kind=NULL,sealed_at=NULL",[])?;Ok(())}).is_err());
    assert!(
        store
            .transaction(|db| {
                db.execute(
                    "UPDATE native_observation_companions SET sealed_at='changed'",
                    [],
                )?;
                Ok(())
            })
            .is_err()
    );
}
#[test]
fn runtime_seal_transaction_rollback_does_not_mint_witness_or_generation() {
    let (_root, mut store) = fixture();
    let record = completed(&mut store, "request");
    let before = migration::snapshot(&store.db).unwrap();
    let failed: catabolic_store::Result<()> = store.transaction(|_| {
        seal_sql(&store, &record)?;
        Err(Error("injected seal rollback".into()))
    });
    assert!(failed.is_err());
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
    assert!(!companion::has_cleanup_witness(&store, &record.owned.unwrap().job_id).unwrap());
}
#[test]
fn shared_completed_demand_never_discharges_original_owner() {
    let (_root, mut store) = fixture();
    let original = completed(&mut store, "original");
    let shared = completed(&mut store, "shared");
    assert!(shared.owned.is_none());
    assert_eq!(shared.demand_job_id, original.demand_job_id);
    store.transaction(|_| seal_sql(&store, &shared)).unwrap();
    assert!(
        !companion::has_cleanup_witness(&store, original.demand_job_id.as_ref().unwrap()).unwrap()
    );
    let report = native_worker_inventory::inspect(&store).unwrap();
    assert_eq!(report.witnessed_observation, 0);
    assert_eq!(report.unresolved.len(), 2);
}
#[test]
fn malformed_or_changed_current_claim_never_reuses_a_positive_seal() {
    for field in [
        "claim_token=NULL",
        "claim_token='not-a-claim'",
        "claim_token='00000000-0000-0000-0000-000000000000'",
        "claim_token='11111111-1111-4111-8111-111111111111'",
        "generation=0",
        "generation='malformed'",
        "worker_pid=0",
        "worker_pid='bad'",
        "worker_pid=2147483648",
    ] {
        let (_root, mut store) = fixture();
        let record = completed(&mut store, "request");
        store.transaction(|_| seal_sql(&store, &record)).unwrap();
        store
            .transaction(|db| {
                db.execute(&format!("UPDATE observation_jobs SET {field}"), [])?;
                Ok(())
            })
            .unwrap();
        assert!(
            !companion::has_cleanup_witness(&store, record.demand_job_id.as_ref().unwrap())
                .unwrap(),
            "{field}"
        );
        assert!(
            !native_worker_inventory::inspect(&store)
                .unwrap()
                .admits_new_work()
        );
    }
}
#[test]
fn child_operation_error_and_unspawned_reservation_stay_unresolved() {
    let (_root, mut store) = fixture();
    let bad = json!({"source":"media","requester":"bad","budgets":{"total_entries":0}});
    let r = companion::reserve(&store, "default", "media", "bad", DIGEST, &bad).unwrap();
    assert!(companion::execute_closed(&mut store, &r, DIGEST, &bad).is_err());
    assert_eq!(
        companion::read(&store, &r.reservation_nonce)
            .unwrap()
            .unwrap()
            .state,
        "started"
    );
    reserve(&store, "never-spawned");
    let report = native_worker_inventory::inspect(&store).unwrap();
    assert_eq!(report.unresolved.len(), 2);
    assert!(!report.admits_new_work());
}
#[test]
fn uuid_checking_and_continuation_are_refused_before_reservation_without_policy_changes() {
    let (_root, store) = fixture();
    store
        .transaction(|db| {
            db.execute(
                "INSERT INTO binding_volumes VALUES('default','source','media','enrolled')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let before = migration::snapshot(&store.db).unwrap();
    assert!(
        companion::reserve(&store, "default", "media", "uuid", DIGEST, &args("uuid"))
            .unwrap_err()
            .0
            .contains("uuid_subprocess")
    );
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
    for key in [
        "continue_id",
        "continue_request",
        "request_id",
        "scopes",
        "crash_after_scope",
    ] {
        let mut input = args("request");
        input[key] = Value::Null;
        assert!(
            companion::reserve(&store, "default", "media", "request", DIGEST, &input)
                .unwrap_err()
                .0
                .contains("arguments")
        );
    }
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
}
#[test]
fn closed_recovery_does_not_rewrite_unwitnessed_expired_guarded_owner() {
    let (_root, mut store) = fixture();
    let id = uuid::Uuid::new_v4().to_string();
    job(
        &store,
        &id,
        "running",
        Some(i64::from(i32::MAX)),
        Some("unwitnessed"),
    );
    let before = migration::rows(
        &store.db,
        "SELECT * FROM observation_jobs WHERE id=?",
        &[id.clone().into()],
    )
    .unwrap();
    let record = completed(&mut store, "queued-demand");
    assert!(record.owned.is_none());
    assert_eq!(
        migration::rows(
            &store.db,
            "SELECT * FROM observation_jobs WHERE id=?",
            &[id.clone().into()]
        )
        .unwrap(),
        before
    );
    assert!(!companion::has_cleanup_witness(&store, &id).unwrap());
}
#[test]
fn historical_terminal_and_arbitrary_pid_rows_stay_unresolved() {
    for state in ["complete", "failed", "unavailable", "stale"] {
        let (_root, store) = fixture();
        job(&store, "historical", state, Some(1), Some("equal-label"));
        assert!(!companion::has_cleanup_witness(&store, "historical").unwrap());
        assert_eq!(
            native_worker_inventory::inspect(&store)
                .unwrap()
                .unresolved
                .len(),
            1
        );
    }
}
#[test]
fn bounded_http_sql_cannot_read_companion_identity_or_nonce() {
    let (_root, store) = fixture();
    reserve(&store, "private");
    for query in [
        "SELECT * FROM native_observation_companions",
        "SELECT count(*) FROM native_observation_companions",
        "SELECT reservation_nonce FROM native_observation_companions",
    ] {
        assert!(
            sql::execute_store(
                &store,
                Some(query),
                &sql::Options {
                    http: true,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
}

#[test]
fn changed_demand_binding_refuses_completion_and_seal() {
    let (_root, mut store) = fixture();
    let record = completed(&mut store, "request");
    store
        .transaction(|db| {
            db.execute("UPDATE observation_requests SET requester='changed'", [])?;
            Ok(())
        })
        .unwrap();
    assert!(!companion::completion_matches(&store, &record).unwrap());
    assert!(store.transaction(|db| {
        db.execute("UPDATE native_observation_companions SET state='quiescent',cleanup_kind='closed_child_exit_v1',sealed_at=CURRENT_TIMESTAMP", [])?;
        Ok(())
    }).is_err());
    assert_eq!(
        companion::read(&store, &record.reservation.reservation_nonce)
            .unwrap()
            .unwrap()
            .state,
        "completed"
    );
}

#[test]
fn implicit_retry_cannot_overwrite_an_unwitnessed_old_claim() {
    let (_root, mut store) = fixture();
    let record = completed(&mut store, "request");
    store
        .transaction(|db| {
            db.execute(
                "UPDATE observation_jobs SET state='failed',report='{\"resume_progress\":true}'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let before = migration::rows(&store.db, "SELECT * FROM observation_jobs", &[]).unwrap();
    let reservation = reserve(&store, "request");
    let error =
        companion::execute_closed(&mut store, &reservation, DIGEST, &args("request")).unwrap_err();
    assert_eq!(error.0, "observation_owner_reconciliation_required");
    assert_eq!(
        migration::rows(&store.db, "SELECT * FROM observation_jobs", &[]).unwrap(),
        before
    );
    assert!(
        !companion::has_cleanup_witness(&store, record.demand_job_id.as_ref().unwrap()).unwrap()
    );
}

fn shared_args(requester: &str, shared: &companion::SharedClaim) -> Value {
    json!({"source":"media","requester":requester,"_native_shared_claim":shared,
        "after":9_000_000_000_000_000.0,"require_complete":true})
}
fn reserve_shared(
    store: &Store,
    requester: &str,
    shared: &companion::SharedClaim,
) -> companion::Reservation {
    companion::reserve(
        store,
        "default",
        "media",
        requester,
        DIGEST,
        &shared_args(requester, shared),
    )
    .unwrap()
}
#[test]
fn shared_started_owner_attaches_exact_running_or_complete_job_without_replacement() {
    for state in ["running", "complete"] {
        let (_root, mut store) = fixture();
        let original = reserve(&store, "original");
        store.transaction(|db|{db.execute("UPDATE native_observation_companions SET state='started',child_pid=? WHERE reservation_nonce=?",params![std::process::id(),original.reservation_nonce])?;Ok(())}).unwrap();
        // Build an ordinary owner fixture, then retain the protocol's started
        // state to model its in-flight durable binding. No process proof claimed.
        catabolic_store::observation::execute_detached(
            &mut store,
            "default",
            "scan",
            &args("original"),
        )
        .unwrap();
        store
            .transaction(|db| {
                db.execute("UPDATE observation_jobs SET state=?", [state])?;
                Ok(())
            })
            .unwrap();
        let shared =
            companion::capture_shared_claim(&store, &original, std::process::id()).unwrap();
        let before_jobs =
            migration::rows(&store.db, "SELECT * FROM observation_jobs", &[]).unwrap();
        let old_guarantees: String = store
            .db
            .query_row(
                "SELECT guarantees FROM observation_requests WHERE requester='original'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let joined = reserve_shared(&store, "joined", &shared);
        let result =
            companion::execute_closed(&mut store, &joined, DIGEST, &shared_args("joined", &shared))
                .unwrap();
        assert_eq!(result["observations"][0]["id"], shared.owned.job_id);
        assert_eq!(result["observations"][0]["job_state"], state);
        assert_eq!(
            migration::rows(&store.db, "SELECT * FROM observation_jobs", &[]).unwrap(),
            before_jobs
        );
        let new_guarantees: String = store
            .db
            .query_row(
                "SELECT guarantees FROM observation_requests WHERE requester='joined'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(new_guarantees, old_guarantees);
        let completion = companion::read(&store, &joined.reservation_nonce)
            .unwrap()
            .unwrap();
        assert!(completion.owned.is_none());
        assert_eq!(completion.demand_job_id, Some(shared.owned.job_id.clone()));
        store
            .transaction(|_| seal_sql(&store, &completion))
            .unwrap();
        assert!(!companion::has_cleanup_witness(&store, &shared.owned.job_id).unwrap());
        assert_eq!(
            companion::read(&store, &original.reservation_nonce)
                .unwrap()
                .unwrap()
                .state,
            "started"
        );
    }
}
#[test]
fn shared_completed_owner_attaches_without_freshness_or_ownership_claim() {
    let (_root, mut store) = fixture();
    let original = completed(&mut store, "original");
    let shared =
        companion::capture_shared_claim(&store, &original.reservation, std::process::id()).unwrap();
    let joined = reserve_shared(&store, "joined", &shared);
    companion::execute_closed(&mut store, &joined, DIGEST, &shared_args("joined", &shared))
        .unwrap();
    let completion = companion::read(&store, &joined.reservation_nonce)
        .unwrap()
        .unwrap();
    assert!(completion.owned.is_none());
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM observation_jobs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM observation_requests", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        companion::read(&store, &original.reservation.reservation_nonce)
            .unwrap()
            .unwrap(),
        original
    );
    assert!(!companion::has_cleanup_witness(&store, &shared.owned.job_id).unwrap());
}
#[test]
fn shared_changed_current_claim_or_demand_refuses_without_new_jobs_or_requests() {
    for changed in [
        "claim_token='11111111-1111-4111-8111-111111111111'",
        "generation=generation+1",
        "worker_pid=worker_pid+1",
        "state='failed'",
        "profile='other'",
        "source='other'",
    ] {
        let (_root, mut store) = fixture();
        let original = completed(&mut store, "original");
        let shared =
            companion::capture_shared_claim(&store, &original.reservation, std::process::id())
                .unwrap();
        let joined = reserve_shared(&store, "joined", &shared);
        store
            .transaction(|db| {
                db.execute(&format!("UPDATE observation_jobs SET {changed}"), [])?;
                Ok(())
            })
            .unwrap();
        let before = migration::snapshot(&store.db).unwrap();
        assert!(
            companion::validate_shared_claim(&store, &joined, &shared).is_err(),
            "{changed}"
        );
        assert!(
            companion::execute_closed(&mut store, &joined, DIGEST, &shared_args("joined", &shared))
                .is_err(),
            "{changed}"
        );
        assert_eq!(migration::snapshot(&store.db).unwrap(), before, "{changed}");
    }
    let (_root, mut store) = fixture();
    let original = completed(&mut store, "original");
    let shared =
        companion::capture_shared_claim(&store, &original.reservation, std::process::id()).unwrap();
    let joined = reserve_shared(&store, "joined", &shared);
    store
        .transaction(|db| {
            db.execute("UPDATE observation_requests SET requester='different'", [])?;
            Ok(())
        })
        .unwrap();
    let before = migration::snapshot(&store.db).unwrap();
    assert!(
        companion::execute_closed(&mut store, &joined, DIGEST, &shared_args("joined", &shared))
            .is_err()
    );
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
}
#[test]
fn shared_nonce_pid_and_lifetime_are_exact_read_only_identity_checks() {
    let (_root, mut store) = fixture();
    let original = completed(&mut store, "original");
    let shared =
        companion::capture_shared_claim(&store, &original.reservation, std::process::id()).unwrap();
    let before = migration::snapshot(&store.db).unwrap();
    assert!(
        companion::capture_shared_claim(&store, &original.reservation, std::process::id() + 1)
            .is_err()
    );
    for key in [
        "catalog",
        "incarnation",
        "helper",
        "profile",
        "source",
        "nonce",
    ] {
        let mut context = original.reservation.clone();
        match key {
            "catalog" => context.catalog_id = uuid::Uuid::new_v4().to_string(),
            "incarnation" => context.incarnation = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            "helper" => {
                context.helper_digest =
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()
            }
            "profile" => context.profile = "other".into(),
            "source" => context.source = "other".into(),
            _ => context.reservation_nonce = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        }
        assert!(
            companion::capture_shared_claim(&store, &context, std::process::id()).is_err(),
            "{key}"
        );
        if key != "nonce" {
            assert!(
                companion::validate_shared_claim(&store, &context, &shared).is_err(),
                "{key}"
            );
        }
    }
    let mut wrong = shared.clone();
    wrong.reservation_nonce = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
    assert!(companion::validate_shared_claim(&store, &original.reservation, &wrong).is_err());
    wrong = shared.clone();
    wrong.child_pid += 1;
    assert!(companion::validate_shared_claim(&store, &original.reservation, &wrong).is_err());
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
}
#[test]
fn shared_startup_and_malformed_private_argument_never_create_work() {
    let (_root, store) = fixture();
    let original = reserve(&store, "original");
    let before = migration::snapshot(&store.db).unwrap();
    assert_eq!(
        companion::capture_shared_claim(&store, &original, std::process::id())
            .unwrap_err()
            .0,
        "shared_observation_owner_not_admitted"
    );
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
    store
        .transaction(|db| {
            db.execute(
                "UPDATE native_observation_companions SET state='started',child_pid=?",
                [std::process::id()],
            )?;
            Ok(())
        })
        .unwrap();
    let before = migration::snapshot(&store.db).unwrap();
    assert_eq!(
        companion::capture_shared_claim(&store, &original, std::process::id())
            .unwrap_err()
            .0,
        "shared_observation_owner_not_admitted"
    );
    for value in [
        Value::Null,
        json!({"child_pid":std::process::id()}),
        json!(true),
    ] {
        let input = json!({"source":"media","requester":"invalid","_native_shared_claim":value});
        assert!(companion::reserve(&store, "default", "media", "invalid", DIGEST, &input).is_err());
    }
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
}
