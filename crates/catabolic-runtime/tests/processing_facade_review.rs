// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent bounded integration checks for the new processing adapter.
use catabolic_runtime::public_processing;
use catabolic_store::{Store, migration};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::MetadataExt,
    sync::{Arc, atomic::AtomicBool},
};

#[test]
fn selection_admits_all_1001_real_files_across_batch_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    let root = source.metadata().unwrap();
    store.transaction(|db| {
        db.execute("INSERT INTO locations VALUES ('source')", [])?;
        db.execute("INSERT INTO bindings VALUES ('default','source','source',?,?,?)", rusqlite::params![source.to_string_lossy(),root.dev() as i64,root.ino() as i64])?;
        db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','source',1,1001,'[]')", [])?;
        for n in 0..1001 {
            let id = format!("file-{n:04}");
            let name = format!("{id}.txt");
            fs::write(source.join(&name), format!("disposable processing fixture {n}\n"))?;
            let stat = source.join(&name).metadata()?;
            db.execute("INSERT INTO files VALUES (?,'source',?)", rusqlite::params![id,name])?;
            db.execute("INSERT INTO observations VALUES ('default',?,?,?,?,?,'present','scan')", rusqlite::params![id,stat.size() as i64,stat.mtime()*1_000_000_000+stat.mtime_nsec(),stat.dev() as i64,stat.ino() as i64])?;
        }
        Ok(())
    }).unwrap();
    let selected = public_processing::enqueue_selection(
        store,
        "default",
        &json!({"language":"sql","query":"SELECT id AS file_id FROM files ORDER BY id DESC"}),
        &json!({"operation":"hash","options_json":"{\"timeout\":7}","refresh":false}),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(selected["complete"], true);
    assert_eq!(selected["next_after"], serde_json::Value::Null);
    assert_eq!(selected["errors"], json!([]));
    assert_eq!(selected["cached"], json!([]));
    let queued = selected["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1001);
    assert_eq!(
        queued
            .iter()
            .map(|id| id.as_str().unwrap())
            .collect::<BTreeSet<_>>()
            .len(),
        1001
    );
    // Reopening verifies both batches committed and the consumed writer was released.
    let store = Store::open(&path, true, false).unwrap();
    let count: i64 = store.db.query_row(
        "SELECT count(*) FROM processing_jobs WHERE profile='default' AND state='queued' AND operation='hash' AND json_extract(options,'$.timeout')=7",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 1001);
    let files: i64 = store
        .db
        .query_row(
            "SELECT count(DISTINCT file_id) FROM processing_jobs",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(files, 1001);
    for file in ["file-0000", "file-0999", "file-1000"] {
        let present: bool = store.db.query_row("SELECT EXISTS(SELECT 1 FROM processing_jobs WHERE file_id=? AND json_extract(snapshot,'$.path')=?)", rusqlite::params![file,format!("{file}.txt")], |row| row.get(0)).unwrap();
        assert!(present, "missing admission or snapshot at {file}");
    }
    // Repeating the same selection must aggregate both cache-hit batches too.
    let cached = public_processing::enqueue_selection(
        store,
        "default",
        &json!({"language":"sql","query":"SELECT id AS file_id FROM files"}),
        &json!({"operation":"hash","options":{"timeout":7},"refresh":false}),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(cached["complete"], true);
    assert_eq!(cached["queued"], json!([]));
    assert_eq!(cached["errors"], json!([]));
    assert_eq!(cached["cached"], selected["queued"]);
    let store = Store::open(&path, true, false).unwrap();
    let count: i64 = store
        .db
        .query_row("SELECT count(*) FROM processing_jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1001);
}
