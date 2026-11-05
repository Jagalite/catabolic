// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Committed processing results against a disposable catalog and real source.
#![cfg(unix)]
use catabolic_store::{Store, media_mutation, migration, observation};
use rusqlite::params;
use serde_json::json;

fn fixture() -> (tempfile::TempDir, Store, String) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("a.txt"), b"original").unwrap();
    let db = root.path().join("catalog.sqlite3");
    migration::initialize(&db).unwrap();
    let store = Store::open(&db, true, false).unwrap();
    observation::execute(
        &store,
        "default",
        "bind",
        &json!({"source":"media","root":source}),
    )
    .unwrap();
    observation::execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
    let file: String = store
        .db
        .query_row("SELECT id FROM files WHERE path='a.txt'", [], |r| r.get(0))
        .unwrap();
    (root, store, file)
}
fn start(store: &Store, file: &str, op: &str, now: f64) -> media_mutation::Claim {
    let job = media_mutation::enqueue(store, "default", file, op, &json!({}), true, None).unwrap();
    media_mutation::start(store, "default", job["id"].as_str().unwrap(), now).unwrap()
}
#[test]
fn changed_inventory_never_publishes_facts_for_the_prior_snapshot() {
    let (_root, store, file) = fixture();
    let claim = start(&store, &file, "sniff", 100.0);
    store
        .transaction(|db| {
            db.execute(
                "UPDATE observations SET size=size+1 WHERE profile='default' AND file_id=?",
                [&file],
            )?;
            Ok(())
        })
        .unwrap();
    let state = media_mutation::complete(
        &store,
        "default",
        &claim,
        &json!({"state":"complete","data":{}}),
        101.0,
        5,
        None,
    )
    .unwrap();
    assert_eq!(state, "changed");
    let facts: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM file_facts WHERE profile='default' AND file_id=?",
            [&file],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(facts, 0);
}
#[test]
fn checksum_mismatch_preserves_baseline_and_invalidates_old_facts() {
    let (_root, store, file) = fixture();
    let original = "a".repeat(64);
    let changed = "b".repeat(64);
    let first = start(&store, &file, "hash", 100.0);
    assert_eq!(
        media_mutation::complete(
            &store,
            "default",
            &first,
            &json!({"state":"complete","data":{"digest":original,"size":8}}),
            101.0,
            5,
            None
        )
        .unwrap(),
        "complete"
    );
    let second = start(&store, &file, "verify", 102.0);
    assert_eq!(
        media_mutation::complete(
            &store,
            "default",
            &second,
            &json!({"state":"complete","data":{"digest":changed,"size":8}}),
            103.0,
            5,
            None
        )
        .unwrap(),
        "mismatch"
    );
    let (baseline,status):(String,String)=store.db.query_row("SELECT b.digest,f.status FROM content_baselines b JOIN file_facts f ON f.profile=b.profile AND f.file_id=b.file_id WHERE b.profile='default' AND b.file_id=? AND f.operation='hash'",params![file],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(baseline, original);
    assert_eq!(status, "invalidated");
}

#[test]
fn committed_retry_keeps_source_identity_and_fences_the_previous_attempt() {
    // The schema-32 production owner is deliberately exercised directly here;
    // newer native process-lifetime admission has its own cleanup qualification.
    for changed_source in [false, true] {
        let (root, store, file) = fixture();
        let path = store.path.clone();
        let first = start(&store, &file, "sniff", 100.0);
        let snapshot = first.snapshot.clone();
        let job = first.job_id.clone();
        assert_eq!(
            media_mutation::complete(
                &store,
                "default",
                &first,
                &json!({"state":"failed","data":{},"retryable":true}),
                101.0,
                5,
                None,
            )
            .unwrap(),
            "failed"
        );
        drop(store);
        let store = Store::open(&path, true, false).unwrap();
        let scheduled: (i64, f64) = store
            .db
            .query_row(
                "SELECT attempt,retry_after FROM processing_retry_queue WHERE job_id=?",
                [&job],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(scheduled, (1, 106.0));
        assert_eq!(
            media_mutation::prepare_run(
                &store,
                "default",
                Some(std::slice::from_ref(&job)),
                105.0,
                2,
                10
            )
            .unwrap(),
            0
        );
        if changed_source {
            std::fs::write(root.path().join("source/a.txt"), b"new source revision").unwrap();
            observation::execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        }
        drop(store);
        let store = Store::open(&path, true, false).unwrap();
        assert_eq!(
            media_mutation::prepare_run(
                &store,
                "default",
                Some(std::slice::from_ref(&job)),
                106.0,
                2,
                10
            )
            .unwrap(),
            1
        );
        drop(store);
        let store = Store::open(&path, true, false).unwrap();
        let second = media_mutation::start(&store, "default", &job, 107.0).unwrap();
        assert_eq!(second.attempt, 2);
        assert_eq!(second.generation, first.generation + 1);
        assert_eq!(second.snapshot, snapshot);
        assert_ne!(second.token, first.token);
        let result = json!({"state":"complete","data":{"media_type":"text"}});
        assert!(
            media_mutation::complete(&store, "default", &first, &result, 108.0, 5, None).is_err()
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM file_facts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let expected = if changed_source {
            "changed"
        } else {
            "complete"
        };
        assert_eq!(
            media_mutation::complete(&store, "default", &second, &result, 108.0, 5, None).unwrap(),
            expected
        );
        drop(store);
        let store = Store::open(&path, true, false).unwrap();
        let durable: (String, i64, i64, i64) = store.db.query_row(
            "SELECT state,attempts,(SELECT count(*) FROM processing_attempts WHERE job_id=j.id),(SELECT count(*) FROM processing_retry_queue WHERE job_id=j.id) FROM processing_jobs j WHERE id=?",
            [&job], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).unwrap();
        assert_eq!(durable, (expected.into(), 2, 2, 0));
        let facts: i64 = store
            .db
            .query_row("SELECT count(*) FROM file_facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(facts, i64::from(!changed_source));
        assert!(
            media_mutation::complete(&store, "default", &second, &result, 109.0, 5, None).is_err()
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM processing_attempts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM file_facts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            facts
        );
    }
}
