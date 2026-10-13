// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Actual durable probe ownership and detached resolver boundaries. A sleeping
//! executable injects only an external-process fault; all slot, timeout, reap,
//! source checks, catalog writes and epoch decisions use production owners.
use catabolic_runtime::fallback;
use catabolic_store::{
    Store, definitions, fallback_mutation, fallback_resolution, migration, selection::Evaluation,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    time::{Duration, Instant},
};

fn fixture() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("sample.bin"), b"actual disposable source").unwrap();
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    let r = source.metadata().unwrap();
    let f = source.join("sample.bin").metadata().unwrap();
    store.transaction(|db|{
        db.execute("INSERT INTO locations VALUES ('source')",[])?;
        db.execute("INSERT INTO bindings VALUES ('default','source','source',?,?,?)",rusqlite::params![source.to_str(),r.dev() as i64,r.ino() as i64])?;
        db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','source',1,1,'[]')",[])?;
        db.execute("INSERT INTO files VALUES ('file','source','sample.bin')",[])?;
        db.execute("INSERT INTO observations VALUES ('default','file',?,?,?,?,'present','scan')",rusqlite::params![f.len() as i64,f.mtime()*1_000_000_000+f.mtime_nsec(),f.dev() as i64,f.ino() as i64])?;
        db.execute("INSERT INTO items VALUES ('item','movie','{}')",[])?;
        db.execute("INSERT INTO item_files(id,file_id,item_id,role,metadata,active,origin) VALUES ('association','file','item','primary','{}',1,'explicit')",[])?;
        Ok(())
    }).unwrap();
    (root, store)
}
#[test]
fn live_process_keeps_every_durable_probe_slot() {
    let (_root, store) = fixture();
    let path = store.path.clone();
    let database = store.database_id.clone();
    store
        .transaction(|db| {
            db.execute(
                "UPDATE fallback_probe_slots SET owner='live',pid=?",
                [std::process::id()],
            )?;
            Ok(())
        })
        .unwrap();
    let count: i64 = store
        .db
        .query_row("SELECT count(*) FROM fallback_probe_slots", [], |r| {
            r.get(0)
        })
        .unwrap();
    drop(store);
    assert!(
        fallback_resolution::reserve_probe(&path, &database)
            .unwrap()
            .is_none()
    );
    let store = Store::open(&path, false, false).unwrap();
    let retained: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM fallback_probe_slots WHERE owner='live' AND pid=?",
            [std::process::id()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retained, count);
}
#[test]
fn sleeping_child_times_out_and_is_reaped_before_slot_reuse() {
    let (root, store) = fixture();
    let path = store.path.clone();
    let database = store.database_id.clone();
    drop(store);
    let helper = root.path().join("sleeping-helper");
    let pidfile = root.path().join("helper.pid");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec /bin/sleep 30\n",
            pidfile.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    catabolic_runtime::helper::register(&helper, None).unwrap();
    let before = Instant::now();
    // Give the shell fixture time to publish its PID under host startup load.
    // The timeout, three-second wall bound, reaping and slot checks remain real.
    let result = fallback::probe(&path, "default", &database, &json!([{}]), 1000).unwrap();
    assert_eq!(result, json!({"usable":false,"reason":"probe_timeout"}));
    assert!(
        before.elapsed() < Duration::from_secs(3),
        "bounded helper exceeded deadline"
    );
    let pid: i64 = fs::read_to_string(pidfile).unwrap().parse().unwrap();
    assert!(
        !fallback_resolution::process_alive(pid),
        "timed-out child is still alive"
    );
    let store = Store::open(&path, false, false).unwrap();
    let occupied: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM fallback_probe_slots WHERE owner IS NOT NULL OR pid IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(occupied, 0);
}
#[test]
fn real_catalog_change_during_detached_probe_rejects_receipt() {
    let (_root, store) = fixture();
    let query = definitions::query_put(
        &store,
        "default",
        "primary",
        &json!({"selection":{"language":"sql","query":"SELECT id AS file_id FROM files"}}),
    )
    .unwrap();
    let policy = fallback_mutation::put(
        &store,
        "default",
        "default",
        &json!({"fallbacks":[{"name":"primary","query_id":query["id"]}]}),
    )
    .unwrap();
    let mut evaluation = Evaluation::new(store, "default", 5000, 10000, false).unwrap();
    let mut called = false;
    let mut probe = |path: &std::path::Path,
                     profile: &str,
                     database: &str,
                     snapshots: &serde_json::Value,
                     _: u64| {
        called = true;
        // This writer can be acquired only after the parent owner is detached.
        let writer = Store::open(path, true, false)?;
        writer.transaction(|db| {
            db.execute(
                "UPDATE items SET metadata='{\"title\":\"changed\"}' WHERE id='item'",
                [],
            )?;
            Ok(())
        })?;
        drop(writer);
        fallback::probe_check(
            &json!({"database":path,"database_id":database,"profile":profile,"snapshots":snapshots}),
        )
    };
    let result = {
        let mut resolver = fallback_resolution::Resolver::new(
            fallback_resolution::Context::Local(&mut evaluation),
            &mut probe,
            None,
        );
        resolver.resolve(
            &[json!({"item_id":"item"})],
            policy["id"].as_str().unwrap(),
            "",
            &BTreeMap::new(),
            false,
        )
    };
    assert!(called);
    assert_eq!(result.unwrap_err().0, "stale_resolution_plan");
    assert!(evaluation.store().writable);
}
