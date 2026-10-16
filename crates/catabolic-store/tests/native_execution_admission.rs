//! Native lease expiry is never process-exit proof. These tests deliberately
//! retain uncertain ownership rather than invent a destructive recovery path.
#![cfg(unix)]
use catabolic_store::{
    Store,
    media_mutation::{self, AdmissionErrorCode},
    migration,
};
use rusqlite::params;
use std::process::{Child, Command};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn catalog(version: u32) -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    if version == 32 {
        migration::initialize(&path).unwrap();
    } else {
        migration::initialize_native_at(&path, version).unwrap();
    }
    let store = Store::open(&path, true, false).unwrap();
    store.transaction(|db| {
        db.execute_batch("INSERT INTO locations VALUES('source'); INSERT INTO files VALUES('file','source','file.txt');")?;
        Ok(())
    }).unwrap();
    (root, store)
}
fn job(store: &Store, id: &str, state: &str, attempts: i64, pid: Option<i64>) {
    store.transaction(|db| {
        db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,attempts) VALUES(?,'default','file','probe','source','{}','{}',?,?,?)",params![id,id,state,attempts])?;
        if let Some(pid) = pid {
            db.execute("INSERT INTO execution_claims VALUES(?,1,'old-token',?,1,1,'{}','{}')",params![id,pid])?;
        }
        Ok(())
    }).unwrap();
}
fn prepare(store: &Store) -> Result<usize, media_mutation::AdmissionError> {
    media_mutation::prepare_run_admitted(store, "default", None, 2.0, 3, 100)
}
#[test]
fn native_expired_live_worker_refuses_prepare_and_direct_start_without_signaling() {
    let child = OwnedChild(Command::new("/bin/sleep").arg("30").spawn().unwrap());
    for schema in [33, 34, 35, 36] {
        let (_root, store) = catalog(schema);
        job(&store, "job", "running", 1, Some(i64::from(child.0.id())));
        let before = migration::snapshot(&store.db).unwrap();
        let error = prepare(&store).unwrap_err();
        assert_eq!(error.code, AdmissionErrorCode::BusyWorker);
        assert_eq!(error.job_id.as_deref(), Some("job"));
        assert_eq!(
            media_mutation::start_admitted(&store, "default", "job", 2.0)
                .unwrap_err()
                .code,
            AdmissionErrorCode::BusyWorker
        );
        assert_eq!(migration::snapshot(&store.db).unwrap(), before);
        // SAFETY: signal zero only observes this fixture-owned child.
        assert_eq!(unsafe { libc::kill(child.0.id() as i32, 0) }, 0);
    }
}
#[test]
fn verified_direct_child_exit_still_requires_missing_descendant_and_birth_identity_reconciliation()
{
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    let pid = child.id();
    assert!(child.wait().unwrap().success());
    // SAFETY: signal zero only tests absence after our retained Child was reaped.
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let (_root, store) = catalog(34);
    job(&store, "job", "running", 1, Some(i64::from(pid)));
    let before = migration::snapshot(&store.db).unwrap();
    assert_eq!(
        prepare(&store).unwrap_err().code,
        AdmissionErrorCode::ReconciliationRequired
    );
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
}
#[test]
fn unrelated_live_pid_is_refusal_evidence_never_adopted_as_the_recorded_worker() {
    let child = OwnedChild(Command::new("/bin/sleep").arg("30").spawn().unwrap());
    let (_root, store) = catalog(34);
    // There is no birth identity tying this deliberately unrelated process to
    // the claim. A reused PID is indistinguishable and must be refused too.
    job(&store, "job", "queued", 1, Some(i64::from(child.0.id())));
    let before = migration::snapshot(&store.db).unwrap();
    assert_eq!(
        media_mutation::start_admitted(&store, "default", "job", 2.0)
            .unwrap_err()
            .code,
        AdmissionErrorCode::BusyWorker
    );
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
    // SAFETY: signal zero only observes this fixture-owned child.
    assert_eq!(unsafe { libc::kill(child.0.id() as i32, 0) }, 0);
}
#[test]
fn missing_invalid_and_out_of_range_claims_require_reconciliation() {
    for pid in [None, Some(0), Some(-1), Some(i64::from(u32::MAX))] {
        let (_root, store) = catalog(34);
        job(&store, "job", "running", 1, pid);
        let before = migration::snapshot(&store.db).unwrap();
        assert_eq!(
            prepare(&store).unwrap_err().code,
            AdmissionErrorCode::ReconciliationRequired
        );
        assert_eq!(
            media_mutation::start_admitted(&store, "default", "job", 2.0)
                .unwrap_err()
                .code,
            AdmissionErrorCode::ReconciliationRequired
        );
        assert_eq!(migration::snapshot(&store.db).unwrap(), before);
    }
}
#[test]
fn native_refusal_preserves_due_retry_intents_generations_and_open_transaction_state() {
    let (_root, store) = catalog(34);
    job(
        &store,
        "a-retry",
        "failed",
        1,
        Some(i64::from(std::process::id())),
    );
    job(&store, "z-running", "running", 1, None);
    store.transaction(|db| {
        db.execute("INSERT INTO processing_retry_queue(job_id,profile,attempt,retry_after) VALUES('a-retry','default',1,1)",[])?;
        Ok(())
    }).unwrap();
    let before = migration::snapshot(&store.db).unwrap();
    assert_eq!(
        prepare(&store).unwrap_err().code,
        AdmissionErrorCode::BusyWorker
    );
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
    assert!(store.db.is_autocommit());
    store
        .transaction(|db| {
            db.execute(
                "UPDATE processing_jobs SET error='still usable' WHERE id='z-running'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
}
#[test]
fn first_native_admission_is_allowed_and_legacy_expired_requeue_remains_unchanged() {
    let (_root, store) = catalog(34);
    job(&store, "fresh", "queued", 0, None);
    let claim = media_mutation::start_admitted(&store, "default", "fresh", 2.0).unwrap();
    assert_eq!(claim.attempt, 1);
    assert_eq!(claim.worker_pid, std::process::id());
    let (_root, store) = catalog(32);
    job(
        &store,
        "legacy",
        "running",
        1,
        Some(i64::from(std::process::id())),
    );
    assert_eq!(prepare(&store).unwrap(), 0);
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT state FROM processing_jobs WHERE id='legacy'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "queued"
    );
    assert!(media_mutation::start_admitted(&store, "default", "legacy", 2.0).is_ok());
}

#[test]
fn native_watcher_recovery_and_direct_run_refuse_unproven_prior_ownership() {
    use catabolic_store::watcher;
    for pid in [
        Some(i64::from(std::process::id())),
        None,
        Some(i64::from(i32::MAX)),
    ] {
        let (_root, store) = catalog(34);
        store.transaction(|db| {
            db.execute("INSERT INTO watcher_definitions VALUES('definition','default','watch',1,'{}','digest',1)",[])?;
            db.execute("INSERT INTO watchers(profile,name,definition_id,active_run,lease_until) VALUES('default','watch','definition','run',0)",[])?;
            db.execute("INSERT INTO watcher_runs(id,profile,watcher,definition_id,generation,trigger,scheduled_at,started_at,state,worker_pid) VALUES('run','default','watch','definition',1,'manual',1,1,'observing',?)",[pid])?;Ok(())
        }).unwrap();
        let before = migration::snapshot(&store.db).unwrap();
        let error = watcher::require_native_admission(&store, "default").unwrap_err();
        assert_eq!(
            error.code,
            if pid == Some(i64::from(std::process::id())) {
                AdmissionErrorCode::BusyWorker
            } else {
                AdmissionErrorCode::ReconciliationRequired
            }
        );
        assert!(watcher::execute(&store, "default", "recover", &serde_json::json!({})).is_err());
        let mut store = store;
        assert!(
            watcher::run(
                &mut store,
                "default",
                "watch",
                "manual",
                2.0,
                &mut |_, _| panic!("must not observe"),
                &mut |_, _| panic!("must not react")
            )
            .is_err()
        );
        assert_eq!(migration::snapshot(&store.db).unwrap(), before);
    }
}
#[test]
fn native_probe_slots_do_not_reclaim_absent_or_uncertain_recorded_owners() {
    use catabolic_store::fallback_resolution;
    let (_root, store) = catalog(34);
    store
        .transaction(|db| {
            db.execute(
                "UPDATE fallback_probe_slots SET owner='unreconciled',pid=2147483647",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let path = store.path.clone();
    let identity = store.database_id.clone();
    let before = migration::snapshot(&store.db).unwrap();
    drop(store);
    assert!(
        fallback_resolution::reserve_probe(&path, &identity)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        migration::snapshot(&Store::open(&path, false, false).unwrap().db).unwrap(),
        before
    );
}
