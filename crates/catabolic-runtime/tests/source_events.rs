// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Real kernel intake and scheduler-generation checks on disposable directories.
#![cfg(any(target_os = "linux", target_os = "macos"))]
use catabolic_runtime::source_events::MonitorSession;
use catabolic_store::{Store, migration, observation, source_events, watcher};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    source: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        let source = root.path().join("source");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/one.txt"), "before").unwrap();
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"media","root":source}),
        )
        .unwrap();
        watcher::execute(&store,"default","put",&json!({"name":"inventory","definition":{"plan":{"kind":"observation","sources":["media"]},"events":true,"debounce_seconds":2,"maximum_delay_seconds":3}})).unwrap();
        watcher::execute(&store, "default", "enable", &json!({"name":"inventory"})).unwrap();
        drop(store);
        Self {
            _root: root,
            path,
            source,
        }
    }
    fn monitor(&self) -> MonitorSession {
        MonitorSession::new(&self.path, "default").unwrap()
    }
    fn generation(&self) -> i64 {
        Store::open(&self.path,false,false).unwrap().db.query_row("SELECT dirty_generation FROM observation_sources WHERE profile='default' AND source='media'",[],|r|r.get(0)).unwrap_or(0)
    }
    fn poll(&self, monitor: &mut MonitorSession) -> Value {
        monitor
            .poll(
                Duration::from_millis(100),
                &AtomicBool::new(false),
                &mut || {},
            )
            .unwrap()
    }
    fn changed(&self, monitor: &mut MonitorSession) -> Value {
        let start = self.generation();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let report = self.poll(monitor);
            if self.generation() > start {
                return report;
            }
            assert!(Instant::now() < deadline, "no kernel event: {report}");
        }
    }
    fn settle(&self, monitor: &mut MonitorSession) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let report = self.poll(monitor);
            if report["complete"] == true && report["invalidated"] == json!([]) {
                return;
            }
            assert!(Instant::now() < deadline, "not settled: {report}");
        }
    }
}
#[test]
fn real_recursive_modify_create_move_delete_dirty_without_inventing_inventory() {
    let f = Fixture::new();
    let mut monitor = f.monitor();
    let startup = f.poll(&mut monitor);
    assert_eq!(startup["invalidated"][0]["source"], "media");
    assert_eq!(startup["coverage"], "hints_only");
    f.settle(&mut monitor);
    fs::write(f.source.join("nested/one.txt"), "changed").unwrap();
    f.changed(&mut monitor);
    fs::create_dir(f.source.join("new")).unwrap();
    fs::write(f.source.join("new/two.txt"), "new").unwrap();
    f.changed(&mut monitor);
    f.settle(&mut monitor);
    fs::write(f.source.join("new/two.txt"), "updated subtree").unwrap();
    f.changed(&mut monitor);
    fs::rename(f.source.join("new"), f.source.join("renamed")).unwrap();
    f.changed(&mut monitor);
    f.settle(&mut monitor);
    fs::remove_file(f.source.join("renamed/two.txt")).unwrap();
    f.changed(&mut monitor);
    let store = Store::open(&f.path, false, false).unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM observations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM scans", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn policy_exclusions_are_live_and_generated_roots_never_subscribe() {
    let f = Fixture::new();
    fs::create_dir(f.source.join("ignored")).unwrap();
    fs::write(f.source.join("ignored/noise.txt"), "before").unwrap();
    {
        let store = Store::open(&f.path, true, false).unwrap();
        observation::execute(
            &store,
            "default",
            "scan_policy",
            &json!({"source":"media","apply":true,"policy":{"exclusions":["ignored"]}}),
        )
        .unwrap();
        // Only the eligibility table is needed for this source-event fixture.
        let generated = f._root.path().join("generated");
        fs::create_dir(&generated).unwrap();
        observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"generated","root":generated}),
        )
        .unwrap();
        store
            .db
            .execute(
                "INSERT INTO generated_locations(profile,location) VALUES ('default','generated')",
                [],
            )
            .unwrap();
        assert_eq!(
            source_events::snapshot(&store, "default")
                .unwrap()
                .sources
                .len(),
            1
        );
    }
    let mut monitor = f.monitor();
    f.poll(&mut monitor);
    f.settle(&mut monitor);
    let previous = f.generation();
    fs::write(f.source.join("ignored/noise.txt"), "excluded").unwrap();
    let report = f.poll(&mut monitor);
    assert_eq!(f.generation(), previous, "{report}");
    {
        let store = Store::open(&f.path, true, false).unwrap();
        observation::execute(
            &store,
            "default",
            "scan_policy",
            &json!({"source":"media","apply":true,"policy":{"exclusions":[]}}),
        )
        .unwrap();
    }
    f.poll(&mut monitor);
    f.settle(&mut monitor);
    fs::write(f.source.join("ignored/noise.txt"), "included").unwrap();
    f.changed(&mut monitor);
}
#[test]
fn kernel_wait_releases_writer_and_cancel_does_not_change_observation_demand() {
    let f = Fixture::new();
    let mut monitor = f.monitor();
    f.poll(&mut monitor);
    f.settle(&mut monitor);
    let cancel = Arc::new(AtomicBool::new(false));
    let child_cancel = cancel.clone();
    let handle = std::thread::spawn(move || {
        monitor.poll(Duration::from_secs(10), &child_cancel, &mut || {})
    });
    std::thread::sleep(Duration::from_millis(100));
    let store = Store::open(&f.path, true, false).unwrap();
    store
        .db
        .execute("INSERT INTO locations VALUES ('writer-progress')", [])
        .unwrap();
    drop(store);
    cancel.store(true, Ordering::Release);
    let now = Instant::now();
    assert!(
        handle
            .join()
            .unwrap()
            .unwrap_err()
            .0
            .contains("interrupted")
    );
    assert!(now.elapsed() < Duration::from_secs(1));
}
#[test]
fn busy_writer_retains_coalesced_batch_then_commits_one_generation() {
    let f = Fixture::new();
    let mut monitor = f.monitor();
    f.poll(&mut monitor);
    f.settle(&mut monitor);
    let before = f.generation();
    let writer = Store::open(&f.path, true, false).unwrap();
    for n in 0..100 {
        fs::write(f.source.join("nested/one.txt"), format!("{n}")).unwrap();
    }
    let busy = f.poll(&mut monitor);
    assert_eq!(busy["catalog_busy"], true);
    assert_eq!(busy["pending_sources"], json!(["media"]));
    assert_eq!(f.generation(), before);
    drop(writer);
    let done = f.poll(&mut monitor);
    assert_eq!(done["pending_sources"], json!([]));
    assert_eq!(f.generation(), before + 1);
}
#[test]
fn hints_preserve_debounce_maximum_delay_and_newer_pending_generation() {
    let f = Fixture::new();
    let mut monitor = f.monitor();
    f.poll(&mut monitor);
    f.settle(&mut monitor);
    let store = Store::open(&f.path, false, false).unwrap();
    let first = watcher::execute(&store, "default", "get", &json!({"name":"inventory"})).unwrap();
    drop(store);
    fs::write(f.source.join("nested/one.txt"), "next").unwrap();
    f.changed(&mut monitor);
    let store = Store::open(&f.path, false, false).unwrap();
    let latest = watcher::execute(&store, "default", "get", &json!({"name":"inventory"})).unwrap();
    assert_eq!(first["event_first"], latest["event_first"]);
    assert!(latest["pending_generation"].as_i64() > first["pending_generation"].as_i64());
    let time = latest["event_last"].as_f64().unwrap();
    assert!(!watcher::due(&latest, time + 0.1).unwrap());
    assert!(watcher::due(&latest, time + 3.0).unwrap());
}
#[test]
fn disconnected_root_remains_incomplete_and_reconnect_invalidates() {
    let f = Fixture::new();
    let mut monitor = f.monitor();
    f.poll(&mut monitor);
    f.settle(&mut monitor);
    let disconnected = f._root.path().join("detached");
    fs::rename(&f.source, &disconnected).unwrap();
    let report = f.changed(&mut monitor);
    assert_eq!(report["complete"], false);
    assert_eq!(report["reconnecting"], true);
    fs::rename(&disconnected, &f.source).unwrap();
    std::thread::sleep(Duration::from_millis(1050));
    f.poll(&mut monitor);
    f.settle(&mut monitor);
    fs::write(f.source.join("nested/one.txt"), "after reconnect").unwrap();
    f.changed(&mut monitor);
}
#[test]
fn stale_source_or_policy_batch_cannot_dirty_replacement_configuration() {
    let f = Fixture::new();
    let store = Store::open(&f.path, true, false).unwrap();
    let old = source_events::snapshot(&store, "default").unwrap();
    observation::execute(
        &store,
        "default",
        "scan_policy",
        &json!({"source":"media","apply":true,"policy":{"exclusions":["nested"]}}),
    )
    .unwrap();
    assert_eq!(
        source_events::invalidate(&store, &old, &["media".into()])
            .unwrap_err()
            .0,
        "stale_native_event_snapshot"
    );
}
#[cfg(target_os = "linux")]
#[test]
fn real_event_storm_is_bounded_lost_and_requires_reconciliation() {
    let f = Fixture::new();
    let mut monitor = f.monitor();
    f.poll(&mut monitor);
    f.settle(&mut monitor);
    // Each create/write/close delivers distinct kernel notifications. Intake is
    // capped at 4096 even where the host kernel queue is configured larger.
    for n in 0..6000 {
        fs::write(f.source.join(format!("storm-{n}")), "x").unwrap();
    }
    let report = f.changed(&mut monitor);
    assert_eq!(report["lost_events"], true);
    assert_eq!(report["complete"], false);
    assert_eq!(report["pending_sources"], json!([]));
    f.poll(&mut monitor);
    f.settle(&mut monitor);
    fs::write(f.source.join("nested/one.txt"), "after loss").unwrap();
    f.changed(&mut monitor);
}

#[test]
fn new_source_binding_reconnects_without_leaking_old_root_notifications() {
    let f = Fixture::new();
    let mut monitor = f.monitor();
    f.poll(&mut monitor);
    f.settle(&mut monitor);
    let new_root = f._root.path().join("replacement");
    fs::create_dir(&new_root).unwrap();
    {
        let store = Store::open(&f.path, true, false).unwrap();
        observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"media","root":new_root}),
        )
        .unwrap();
    }
    f.poll(&mut monitor);
    f.settle(&mut monitor);
    let generation = f.generation();
    fs::write(f.source.join("nested/one.txt"), "old root event").unwrap();
    f.poll(&mut monitor);
    assert_eq!(f.generation(), generation);
    fs::write(new_root.join("new.txt"), "new root event").unwrap();
    f.changed(&mut monitor);
}

#[test]
fn catalog_identity_change_ends_monitor_without_mutating_replacement() {
    let f = Fixture::new();
    let mut monitor = f.monitor();
    f.poll(&mut monitor);
    f.settle(&mut monitor);
    let replacement = f._root.path().join("replacement.db");
    migration::initialize(&replacement).unwrap();
    // Both catalogs and this pathname are private to this disposable fixture.
    fs::rename(&replacement, &f.path).unwrap();
    let error = monitor
        .poll(Duration::ZERO, &AtomicBool::new(false), &mut || {})
        .unwrap_err();
    assert_eq!(error.0, "catalog replaced during native monitoring");
    let store = Store::open(&f.path, false, false).unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM observation_sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn profile_generations_are_isolated_by_subscription_snapshot() {
    let f = Fixture::new();
    {
        let store = Store::open(&f.path, true, false).unwrap();
        store
            .db
            .execute("INSERT INTO profiles(id) VALUES ('other')", [])
            .unwrap();
        observation::execute(
            &store,
            "other",
            "bind",
            &json!({"source":"media","root":f.source}),
        )
        .unwrap();
    }
    let mut monitor = f.monitor();
    f.poll(&mut monitor);
    fs::write(f.source.join("nested/one.txt"), "default-only hint").unwrap();
    f.changed(&mut monitor);
    let store = Store::open(&f.path, false, false).unwrap();
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM observation_sources WHERE profile='other'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
