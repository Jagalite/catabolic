// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Independent adapter-only checks using disposable catalogs and Linux inotify.
#![cfg(target_os = "linux")]
use catabolic_runtime::source_events::MonitorSession;
use catabolic_store::{Store, migration, observation, source_events, watcher};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
    database: PathBuf,
    source: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("catalog.sqlite3");
        let source = root.path().join("source");
        fs::create_dir_all(source.join("ordinary")).unwrap();
        fs::write(source.join("ordinary/initial"), b"initial").unwrap();
        migration::initialize(&database).unwrap();
        let store = Store::open(&database, true, false).unwrap();
        observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"media", "root":source}),
        )
        .unwrap();
        watcher::execute(&store, "default", "put", &json!({"name":"watch", "definition":{"plan":{"kind":"observation", "sources":["media"]}, "events":true, "debounce_seconds":0}})).unwrap();
        watcher::execute(&store, "default", "enable", &json!({"name":"watch"})).unwrap();
        drop(store);
        Self {
            root,
            database,
            source,
        }
    }
    fn generation(&self, name: &str) -> i64 {
        Store::open(&self.database, false, false).unwrap().db.query_row(
            "SELECT dirty_generation FROM observation_sources WHERE profile='default' AND source=?", [name], |r| r.get(0)).unwrap_or(0)
    }
    fn monitor(&self) -> MonitorSession {
        MonitorSession::new(&self.database, "default").unwrap()
    }
    fn poll(&self, session: &mut MonitorSession) -> Value {
        session
            .poll(
                Duration::from_millis(25),
                &AtomicBool::new(false),
                &mut || {},
            )
            .unwrap()
    }
    fn settle(&self, session: &mut MonitorSession) {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let value = self.poll(session);
            if value["complete"] == true && value["invalidated"] == json!([]) {
                return;
            }
            assert!(Instant::now() < deadline, "adapter did not settle: {value}");
        }
    }
}

#[test]
fn nested_output_generated_and_policy_directories_do_not_register_or_invalidate() {
    let f = Fixture::new();
    for name in ["output", "generated", "excluded"] {
        fs::create_dir_all(f.source.join(name).join("deep")).unwrap();
        fs::write(f.source.join(name).join("deep/noise"), b"before").unwrap();
    }
    {
        let store = Store::open(&f.database, true, false).unwrap();
        store
            .db
            .execute("INSERT INTO locations VALUES ('generated')", [])
            .unwrap();
        store.db.execute("INSERT INTO bindings(profile,kind,owner,root,device,inode) SELECT profile,'source','generated',?,device,inode FROM bindings WHERE profile='default' AND kind='source' AND owner='media'", [f.source.join("generated").to_str().unwrap()]).unwrap();
        store
            .db
            .execute(
                "INSERT INTO generated_locations(profile,location) VALUES ('default','generated')",
                [],
            )
            .unwrap();
        // Fixture-only output eligibility row: source-event snapshot consumes the
        // binding contract; this does not exercise an output effect or publisher.
        store.db.execute("INSERT INTO bindings(profile,kind,owner,root,device,inode) SELECT profile,'output','excluded-output',?,device,inode FROM bindings WHERE profile='default' AND kind='source' AND owner='media'", [f.source.join("output").to_str().unwrap()]).unwrap();
        observation::execute(
            &store,
            "default",
            "scan_policy",
            &json!({"source":"media", "apply":true, "policy":{"exclusions":["excluded"]}}),
        )
        .unwrap();
    }
    let mut monitor = f.monitor();
    f.settle(&mut monitor);
    let before = f.generation("media");
    for name in ["output", "generated", "excluded"] {
        fs::write(f.source.join(name).join("deep/noise"), b"after").unwrap();
        fs::create_dir(f.source.join(name).join("new-child")).unwrap();
    }
    let result = f.poll(&mut monitor);
    assert_eq!(result["registrations"], 2, "{result}");
    assert_eq!(result["invalidated"], json!([]), "{result}");
    assert_eq!(f.generation("media"), before);
    assert_eq!(f.generation("generated"), 0);
    fs::write(f.source.join("ordinary/initial"), b"included").unwrap();
    assert_eq!(f.poll(&mut monitor)["invalidated"][0]["source"], "media");
}

#[test]
fn config_change_after_wait_snapshot_rejects_old_batch_and_retries_current_policy() {
    let f = Fixture::new();
    let mut monitor = f.monitor();
    f.settle(&mut monitor);
    let mut calls = 0;
    let mut after_config = 0;
    let result = monitor
        .poll(Duration::ZERO, &AtomicBool::new(false), &mut || {
            calls += 1;
            if calls == 2 {
                let store = Store::open(&f.database, true, false).unwrap();
                observation::execute(
                    &store,
                    "default",
                    "scan_policy",
                    &json!({"source":"media", "apply":true, "policy":{"exclusions":["ordinary"]}}),
                )
                .unwrap();
                drop(store);
                after_config = f.generation("media");
                fs::write(f.source.join("ordinary/initial"), b"event under old policy").unwrap();
            }
        })
        .unwrap();
    assert_eq!(result["complete"], false, "{result}");
    assert_eq!(result["invalidated"], json!([]));
    assert_eq!(result["pending_sources"], json!(["media"]));
    assert_eq!(f.generation("media"), after_config);
    f.settle(&mut monitor);
    assert_eq!(f.generation("media"), after_config + 1);
    let settled = f.generation("media");
    fs::write(
        f.source.join("ordinary/initial"),
        b"excluded under new policy",
    )
    .unwrap();
    assert_eq!(f.poll(&mut monitor)["invalidated"], json!([]));
    assert_eq!(f.generation("media"), settled);
}

#[test]
fn sessions_keep_event_queues_catalogs_and_drops_independent() {
    let one = Fixture::new();
    let two = Fixture::new();
    let mut first = one.monitor();
    let mut second = two.monitor();
    one.settle(&mut first);
    two.settle(&mut second);
    let baseline_one = one.generation("media");
    let baseline_two = two.generation("media");
    fs::write(one.source.join("ordinary/initial"), b"first only").unwrap();
    assert_eq!(two.poll(&mut second)["invalidated"], json!([]));
    assert_eq!(two.generation("media"), baseline_two);
    assert_eq!(one.poll(&mut first)["invalidated"][0]["source"], "media");
    assert_eq!(one.generation("media"), baseline_one + 1);
    drop(first);
    fs::write(
        two.source.join("ordinary/initial"),
        b"second after unrelated drop",
    )
    .unwrap();
    assert_eq!(two.poll(&mut second)["invalidated"][0]["source"], "media");
    assert_eq!(two.generation("media"), baseline_two + 1);
}

#[test]
fn registration_budget_keeps_partial_coverage_incomplete() {
    let f = Fixture::new();
    for index in 0..16_500 {
        fs::create_dir(f.source.join(format!("directory-{index:05}"))).unwrap();
    }
    let mut monitor = f.monitor();
    let report = f.poll(&mut monitor);
    assert!(
        report["registrations"].as_u64().unwrap() <= 16_384,
        "{report}"
    );
    assert_eq!(report["unavailable_sources"], json!(["media"]));
    assert_eq!(report["complete"], false);
    assert_eq!(report["reconnecting"], true);
}

#[test]
fn moving_prepopulated_subtree_into_source_rebuilds_and_tracks_later_changes() {
    let f = Fixture::new();
    let incoming = f.root.path().join("incoming");
    fs::create_dir_all(incoming.join("deep/deeper")).unwrap();
    fs::write(incoming.join("deep/deeper/file"), b"initial").unwrap();
    let mut monitor = f.monitor();
    f.settle(&mut monitor);
    fs::rename(&incoming, f.source.join("incoming")).unwrap();
    let first = f.poll(&mut monitor);
    assert_eq!(first["reconnecting"], true, "{first}");
    assert_eq!(first["complete"], false);
    f.settle(&mut monitor);
    let before = f.generation("media");
    fs::write(
        f.source.join("incoming/deep/deeper/file"),
        b"new nested event",
    )
    .unwrap();
    let result = f.poll(&mut monitor);
    assert_eq!(result["invalidated"][0]["source"], "media");
    assert_eq!(f.generation("media"), before + 1);
}

#[test]
fn unknown_source_batch_rolls_back_all_names_and_duplicate_names_coalesce() {
    let f = Fixture::new();
    let store = Store::open(&f.database, true, false).unwrap();
    let snapshot = source_events::snapshot(&store, "default").unwrap();
    let before = f.generation("media");
    let error =
        source_events::invalidate(&store, &snapshot, &["media".into(), "zz-unknown".into()])
            .unwrap_err();
    assert_eq!(error.0, "unknown_native_event_source");
    assert_eq!(f.generation("media"), before);
    let report =
        source_events::invalidate(&store, &snapshot, &["media".into(), "media".into()]).unwrap();
    assert_eq!(report.len(), 1);
    assert_eq!(f.generation("media"), before + 1);
}

#[test]
#[ignore = "creates 200001 disposable empty entries to exercise setup exhaustion"]
fn entry_budget_exhaustion_cannot_restart_for_later_sources() {
    let f = Fixture::new();
    let oversized = f.root.path().join("oversized");
    fs::create_dir(&oversized).unwrap();
    for index in 0..200_001 {
        fs::File::create(oversized.join(format!("entry-{index:06}"))).unwrap();
    }
    {
        let store = Store::open(&f.database, true, false).unwrap();
        // The binding sort visits this source before ordinary "media".
        observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"aaa-oversized", "root":oversized}),
        )
        .unwrap();
    }
    let mut monitor = f.monitor();
    let report = f.poll(&mut monitor);
    let unavailable = report["unavailable_sources"].as_array().unwrap();
    assert!(unavailable.contains(&json!("aaa-oversized")), "{report}");
    assert!(
        unavailable.contains(&json!("media")),
        "the shared inspected-entry budget was exhausted by the earlier source: {report}"
    );
}

#[test]
fn intake_truncation_invalidates_quiet_sources_conservatively() {
    let f = Fixture::new();
    let quiet = f.root.path().join("quiet");
    fs::create_dir(&quiet).unwrap();
    {
        let store = Store::open(&f.database, true, false).unwrap();
        observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"quiet", "root":quiet}),
        )
        .unwrap();
    }
    let mut monitor = f.monitor();
    f.settle(&mut monitor);
    let before_media = f.generation("media");
    let before_quiet = f.generation("quiet");
    // Distinct create/write/close notifications exceed one 4096-event drain.
    for index in 0..2500 {
        fs::write(f.source.join(format!("event-{index:04}")), b"change").unwrap();
    }
    let report = f.poll(&mut monitor);
    assert_eq!(report["lost_events"], true, "{report}");
    assert_eq!(report["complete"], false);
    assert_eq!(report["invalidated"].as_array().unwrap().len(), 2);
    assert_eq!(f.generation("media"), before_media + 1);
    assert_eq!(f.generation("quiet"), before_quiet + 1);
    f.settle(&mut monitor);
    fs::write(quiet.join("after-reconnection"), b"new hint").unwrap();
    assert_eq!(f.poll(&mut monitor)["invalidated"][0]["source"], "quiet");
}
