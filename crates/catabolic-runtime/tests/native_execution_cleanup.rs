//! Schema36 exact-owner cleanup witnesses, exercised through the real Pool driver.
#![cfg(unix)]
use catabolic_runtime::public_processing;
use catabolic_store::{
    Store,
    media_mutation::{self, AdmissionErrorCode},
    migration,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    sync::{Arc, atomic::AtomicBool},
};
struct Fixture {
    _root: tempfile::TempDir,
    store: Store,
    job: String,
}
impl Fixture {
    fn new(schema: u32) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog");
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.bin"), b"owned builtin source").unwrap();
        migration::initialize_native_at(&path, schema).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let r = source.metadata().unwrap();
        let f = source.join("file.bin").metadata().unwrap();
        store.transaction(|db| {
            db.execute("INSERT INTO locations VALUES('source')",[])?;
            db.execute("INSERT INTO bindings VALUES('default','source','source',?,?,?)",params![source.to_string_lossy(),r.dev() as i64,r.ino() as i64])?;
            db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES('scan','default','source',1,1,'[]')",[])?;
            db.execute("INSERT INTO files VALUES('file','source','file.bin')",[])?;
            db.execute("INSERT INTO observations VALUES('default','file',?,?,?,?,'present','scan')",params![f.len() as i64,f.mtime()*1_000_000_000+f.mtime_nsec(),f.dev() as i64,f.ino() as i64])?;Ok(())
        }).unwrap();
        // A real builtin timeout completes its actual worker without a child.
        let result = media_mutation::enqueue(
            &store,
            "default",
            "file",
            "hash",
            &json!({"timeout":0.0}),
            false,
            None,
        )
        .unwrap();
        Self {
            _root: root,
            store,
            job: result["id"].as_str().unwrap().into(),
        }
    }
    fn run(&mut self) -> Value {
        public_processing::execute(
            &mut self.store,
            "default",
            "run",
            &json!({"job_ids":[self.job],"workers":1,"per_device":1,"limit":1,"_refresh":false}),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap()
    }
    fn quiescent(&self) -> i64 {
        self.store.db.query_row("SELECT count(*) FROM native_execution_lifetimes WHERE state='quiescent' AND capacity_released=1 AND cleanup_kind='builtin_no_child_v1'",[],|r|r.get(0)).unwrap()
    }
}
#[test]
fn schema36_same_owner_completed_builtin_retries_after_exact_cleanup_while_host_pid_lives() {
    let mut f = Fixture::new(36);
    assert_eq!(f.run()["counts"]["timeout"], 1);
    assert_eq!(f.quiescent(), 1);
    // SAFETY: signal zero checks this current fixture process without signaling.
    assert_eq!(unsafe { libc::kill(std::process::id() as i32, 0) }, 0);
    assert_eq!(
        media_mutation::retry_admitted(&f.store, "default", &f.job).unwrap()["queued"],
        f.job
    );
    assert_eq!(f.run()["counts"]["timeout"], 1);
    assert_eq!(f.quiescent(), 2);
    let generation: i64 = f
        .store
        .db
        .query_row(
            "SELECT generation FROM execution_claims WHERE job_id=?",
            [&f.job],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(generation, 2);
    assert_eq!(
        migration::validate(&f.store.db, true).unwrap()["schema"],
        36
    );
}
#[test]
fn schema36_witness_survives_reopen_without_reconstructing_a_pid_owner() {
    let mut f = Fixture::new(36);
    f.run();
    let path = f.store.path.clone();
    drop(f.store);
    f.store = Store::open(&path, true, false).unwrap();
    media_mutation::retry_admitted(&f.store, "default", &f.job).unwrap();
    assert_eq!(f.run()["counts"]["timeout"], 1);
    assert_eq!(f.quiescent(), 2);
}
#[test]
fn schema36_old_terminal_claim_without_private_witness_is_still_refused_atomically() {
    let f = Fixture::new(36);
    f.store.transaction(|db| {db.execute("UPDATE processing_jobs SET state='failed',attempts=1 WHERE id=?",[&f.job])?;db.execute("INSERT INTO execution_claims SELECT id,1,'old-token',?,1,0,snapshot,options FROM processing_jobs WHERE id=?",params![std::process::id(),f.job])?;Ok(())}).unwrap();
    let before = migration::snapshot(&f.store.db).unwrap();
    assert_eq!(
        media_mutation::retry_admitted(&f.store, "default", &f.job)
            .unwrap_err()
            .code,
        AdmissionErrorCode::BusyWorker
    );
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    assert_eq!(f.quiescent(), 0);
}
#[test]
fn schema36_exact_claim_token_and_inputs_cannot_borrow_a_prior_cleanup_witness() {
    for assignment in [
        "token='replacement'",
        "snapshot='{}'",
        "options='{}'",
        "generation=generation+1",
        "attempt=attempt+1",
    ] {
        let mut f = Fixture::new(36);
        f.run();
        f.store
            .transaction(|db| {
                db.execute(
                    &format!("UPDATE execution_claims SET {assignment} WHERE job_id=?"),
                    [&f.job],
                )?;
                Ok(())
            })
            .unwrap();
        let before = migration::snapshot(&f.store.db).unwrap();
        assert_eq!(
            media_mutation::retry_admitted(&f.store, "default", &f.job)
                .unwrap_err()
                .code,
            AdmissionErrorCode::BusyWorker
        );
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    }
}
#[test]
fn schema36_sealed_witness_is_immutable_and_historical_claims_are_not_backfilled() {
    let mut f = Fixture::new(36);
    f.run();
    let before = migration::snapshot(&f.store.db).unwrap();
    for sql in [
        "DELETE FROM native_execution_lifetimes",
        "UPDATE native_execution_lifetimes SET runtime_owner_nonce='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'",
        "UPDATE native_execution_lifetimes SET state='owned',cleanup_kind=NULL,capacity_released=0,sealed_at=NULL",
    ] {
        assert!(
            f.store
                .transaction(|db| {
                    db.execute(sql, [])?;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    }
    assert_eq!(
        migration::inspect_native(&f.store.path, false).unwrap()["pending_work"]["native_execution_lifetimes"],
        0
    );
}
#[test]
fn schema35_remains_without_synthesized_cleanup_evidence() {
    let mut f = Fixture::new(35);
    f.run();
    assert_eq!(
        media_mutation::retry_admitted(&f.store, "default", &f.job)
            .unwrap_err()
            .code,
        AdmissionErrorCode::BusyWorker
    );
    assert!(!f.store.db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='native_execution_lifetimes')",[],|r|r.get::<_,bool>(0)).unwrap());
}

#[test]
fn schema36_query_surfaces_cannot_mint_cleanup_or_expose_http_owner_nonces() {
    use catabolic_store::sql;
    let mut f = Fixture::new(36);
    f.run();
    let before = migration::snapshot(&f.store.db).unwrap();
    for http in [false, true] {
        let options = sql::Options {
            http,
            ..Default::default()
        };
        for query in [
            "UPDATE native_execution_lifetimes SET state='quiescent',cleanup_kind='builtin_no_child_v1',capacity_released=1,sealed_at=CURRENT_TIMESTAMP",
            "DELETE FROM native_execution_lifetimes",
            "INSERT INTO native_execution_lifetimes SELECT * FROM native_execution_lifetimes",
        ] {
            assert!(
                sql::execute_store(&f.store, Some(query), &options).is_err(),
                "{query}"
            );
        }
    }
    let options = sql::Options {
        http: true,
        ..Default::default()
    };
    for query in [
        "SELECT runtime_owner_nonce FROM native_execution_lifetimes",
        "SELECT count(*) FROM native_execution_lifetimes",
    ] {
        assert!(
            sql::execute_store(&f.store, Some(query), &options).is_err(),
            "{query}"
        );
    }
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
}

#[test]
fn schema37_reopened_host_admits_exact_builtin_witness_and_refuses_a_changed_claim() {
    use catabolic_runtime::host::{CONTRACT_VERSION, ErrorCode, HostSession};
    for alter in [false, true] {
        let mut f = Fixture::new(37);
        f.run();
        if alter {
            f.store
                .transaction(|db| {
                    db.execute(
                        "UPDATE execution_claims SET token='new-owner' WHERE job_id=?",
                        [&f.job],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        let path = f.store.path.clone();
        drop(f.store);
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
        if alter {
            assert!(!report.admits_new_work());
            assert_eq!(
                host.start_scan("source", ()).unwrap_err().code,
                ErrorCode::RecoveryRequired
            );
        } else {
            assert!(report.admits_new_work());
            assert_eq!(report.witnessed_processing, 1);
            assert_eq!(
                host.start_scan("source", ()).unwrap_err().code,
                ErrorCode::MissingTool
            );
        }
        host.close().unwrap();
    }
}
