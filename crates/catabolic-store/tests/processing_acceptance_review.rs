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
