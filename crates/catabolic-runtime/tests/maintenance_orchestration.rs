// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! New orchestration only. Disposable roots; no underlying M6 qualification.
use catabolic_runtime::maintenance;
use catabolic_store::{Store, migration};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("note.txt"), "ordinary fixture\n").unwrap();
    let path = dir.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    let root = source.metadata().unwrap();
    store
        .transaction(|db| {
            db.execute("INSERT INTO locations VALUES ('media')", [])?;
            db.execute(
                "INSERT INTO bindings VALUES ('default','source','media',?,?,?)",
                rusqlite::params![
                    source.to_string_lossy(),
                    root.dev() as i64,
                    root.ino() as i64
                ],
            )?;
            Ok(())
        })
        .unwrap();
    (dir, store)
}

#[test]
fn cancellation_between_stages_does_not_start_later_owners() {
    for stop in ["preflight", "scan"] {
        let (_dir, mut store) = fixture();
        let cancel = Arc::new(AtomicBool::new(false));
        let callback = cancel.clone();
        let mut stages = vec![];
        let error = maintenance::run(
            &mut store,
            "default",
            &json!({"inventory_only":true,"process":"sniff","settle":0}),
            cancel,
            &mut |event| {
                stages.push(event["stage"].as_str().unwrap().to_owned());
                if event["stage"] == stop {
                    callback.store(true, Ordering::Release);
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.0, maintenance::INTERRUPTED);
        assert_eq!(stages.last().unwrap(), stop);
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM processing_jobs", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM watch_revisions", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            i64::from(stop == "scan")
        );
        assert!(store.db.is_autocommit());
        let path = store.path.clone();
        drop(store);
        assert!(Store::open(&path, true, false).is_ok());
    }
}

#[test]
fn read_session_refused_before_any_scan() {
    let (_dir, store) = fixture();
    let path = store.path.clone();
    drop(store);
    let mut reader = Store::open(&path, false, false).unwrap();
    let error = maintenance::run(
        &mut reader,
        "default",
        &json!({"inventory_only":true}),
        Arc::new(AtomicBool::new(false)),
        &mut |_| panic!("no progress before admission"),
    )
    .unwrap_err();
    assert_eq!(error.0, "maintenance requires the catalog writer lock");
}

#[test]
fn stable_revision_page_rolls_back_whole_page_on_constraint_failure() {
    let (_dir, store) = fixture();
    store.transaction(|db| {
        db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','media',1,2,'[]')",[])?;
        for id in ["a","b"] {
            db.execute("INSERT INTO files VALUES (?,'media',?)",[id,id])?;
            db.execute("INSERT INTO observations VALUES ('default',?,1,1,1,1,'present','scan')",[id])?;
        }
        db.execute_batch("CREATE TEMP TRIGGER fail_revision BEFORE INSERT ON watch_revisions WHEN NEW.file_id='b' BEGIN SELECT RAISE(ABORT,'fixture revision failure'); END;")?;
        Ok(())
    }).unwrap();
    let scan = json!({"location":"media","scan_id":"scan"});
    assert!(
        catabolic_store::maintenance::stable_page(&store, "default", &scan, "", 1000.0, 0).is_err()
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM watch_revisions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(store.db.is_autocommit());
    store
        .db
        .execute_batch("DROP TRIGGER fail_revision; BEGIN")
        .unwrap();
    assert_eq!(
        catabolic_store::maintenance::stable_page(&store, "default", &scan, "", 1000.0, 0)
            .unwrap()
            .0,
        vec!["a", "b"]
    );
    assert!(!store.db.is_autocommit());
    store.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM watch_revisions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn watch_revision_pages_obey_bounds_settling_and_transaction_pages() {
    let (_dir, store) = fixture();
    store.transaction(|db| {
        db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','media',1,3,'[]')",[])?;
        for id in ["a","b","c"] {
            db.execute("INSERT INTO files VALUES (?,'media',?)",[id,id])?;
            db.execute("INSERT INTO observations VALUES ('default',?,1,1,1,1,'present','scan')",[id])?;
        }
        Ok(())
    }).unwrap();
    let scan = json!({"location":"media","scan_id":"scan"});
    let page = |after: &str, now, settle, limit| {
        catabolic_store::maintenance::stable_page_with_limit(
            &store, "default", &scan, after, now, settle, limit,
        )
    };
    for limit in [0, 1001, usize::MAX] {
        assert_eq!(
            page("", 1000.0, 0, limit).unwrap_err().0,
            "batch must be 1..1000"
        );
    }
    let count = || {
        store
            .db
            .query_row("SELECT count(*) FROM watch_revisions", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
    };
    assert_eq!(count(), 0);
    assert_eq!(
        page("", 1000.0, 1, 1).unwrap(),
        (vec![], 1, Some("a".into()))
    );
    assert_eq!(
        page("a", 1000.0, 1, 1).unwrap(),
        (vec![], 1, Some("b".into()))
    );
    assert_eq!(count(), 2);
    let revision: String = store
        .db
        .query_row(
            "SELECT revision FROM watch_revisions WHERE file_id='b'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    store
        .db
        .execute("UPDATE observations SET size=2 WHERE file_id='b'", [])
        .unwrap();
    store.db.execute_batch("CREATE TEMP TRIGGER fail_revision BEFORE INSERT ON watch_revisions WHEN NEW.file_id='c' BEGIN SELECT RAISE(ABORT,'fixture page failure'); END;").unwrap();
    assert!(page("a", 1002.0, 1, 2).is_err());
    assert_eq!(count(), 2);
    let since: f64 = store
        .db
        .query_row(
            "SELECT stable_since FROM watch_revisions WHERE file_id='b'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(since, 1000.0);
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT revision FROM watch_revisions WHERE file_id='b'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        revision
    );
    store
        .db
        .execute_batch("DROP TRIGGER fail_revision; BEGIN")
        .unwrap();
    assert_eq!(
        page("b", 1002.0, 0, 1).unwrap(),
        (vec!["c".into()], 0, Some("c".into()))
    );
    assert_eq!(count(), 3);
    store.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(count(), 2);
    assert_eq!(page("", 1002.0, 1, 1).unwrap().0, vec!["a"]);
    assert_eq!(page("c", 1002.0, 0, 1).unwrap(), (vec![], 0, None));
    // Unchanged maintenance entrypoint still pages all three at once.
    assert_eq!(
        catabolic_store::maintenance::stable_page(&store, "default", &scan, "", 1002.0, 0)
            .unwrap()
            .0,
        vec!["a", "b", "c"]
    );
}

#[test]
fn typed_progress_failure_retains_stage_report_and_stops_later_owners() {
    for stop in ["preflight", "scan"] {
        for kind in [
            "OSError",
            "ValueError",
            "OperationalError",
            "CatabolicError",
        ] {
            let (_dir, mut store) = fixture();
            let mut stages = vec![];
            let report = maintenance::run_with_progress(
                &mut store,
                "default",
                &json!({"inventory_only":true,"process":"sniff","settle":0}),
                Arc::new(AtomicBool::new(false)),
                &mut |event| {
                    stages.push(event["stage"].as_str().unwrap().to_owned());
                    if event["stage"] == stop {
                        Err(maintenance::ProgressError {
                            message: "fixture text stream failure".into(),
                            kind: kind.into(),
                        })
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap();
            assert_eq!(report["complete"], false);
            assert_eq!(report["stopped_at"], stop);
            assert_eq!(
                report["errors"],
                json!([{"stage":stop,"message":"fixture text stream failure","type":kind}])
            );
            assert_eq!(stages.last().unwrap(), stop);
            assert_eq!(
                report["stages"].as_array().unwrap().len(),
                if stop == "scan" { 2 } else { 1 }
            );
            assert_eq!(
                store
                    .db
                    .query_row("SELECT count(*) FROM processing_jobs", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            assert_eq!(
                store
                    .db
                    .query_row("SELECT count(*) FROM files", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                i64::from(stop == "scan")
            );
            assert!(store.db.is_autocommit());
            let path = store.path.clone();
            drop(store);
            assert!(Store::open(&path, true, false).is_ok());
        }
    }
}
