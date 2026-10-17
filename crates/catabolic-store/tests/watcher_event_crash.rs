// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Watcher event admission and completion across an abrupt process exit (FS8).
//! The crash hook exists only in debug builds.
#![cfg(debug_assertions)]
use catabolic_store::{Store, definitions, migration, watcher};
use serde_json::{Value, json};
use std::path::Path;

fn run(store: &mut Store) -> Value {
    watcher::run(
        store,
        "default",
        "changes",
        "manual",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64(),
        &mut |_, request| panic!("query plans observe no sources here: {request}"),
        &mut |store, context| {
            let evaluated = watcher::evaluate(store, "default", &context.definition["plan"])?;
            watcher::complete_evaluation(
                store,
                "default",
                &context,
                &evaluated,
                &mut |_, _, _, _| Ok(Value::Null),
            )
        },
    )
    .unwrap()
}

fn notifications(store: &Store) -> i64 {
    store
        .db
        .query_row(
            "SELECT count(*) FROM consumer_events WHERE event='watcher_changed'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

// The parent test launches only this helper in a separate process. The crash
// hook exits before destructors, so no commit or rollback runs in-process.
#[test]
fn watcher_crash_worker() {
    let Ok(path) = std::env::var("CATABOLIC_WATCHER_CRASH_DB") else {
        return;
    };
    let mut store = Store::open(Path::new(&path), true, false).unwrap();
    run(&mut store);
    panic!("the configured crash hook did not terminate the worker");
}

/// Commits one changed evaluation's plan, then exits abruptly before watcher
/// completion and recovers the dead owner. Returns the retained planned run.
fn crash_before_completion() -> (tempfile::TempDir, std::path::PathBuf, Value) {
    let (root, path) = fixture();
    let crashed = crash_worker(&path);
    (root, path, crashed)
}

/// A disposable catalog with one item and an enabled-for-run event watcher.
fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    {
        let store = Store::open(&path, true, false).unwrap();
        store
            .db
            .execute(
                "INSERT INTO items(id,kind,metadata) VALUES('one','movie','{}')",
                [],
            )
            .unwrap();
        let query = definitions::query_put(
            &store,
            "default",
            "changes",
            &json!({"selection":{"language":"sql","query":"SELECT id AS item_id FROM items"}}),
        )
        .unwrap();
        watcher::execute(&store, "default", "put", &json!({"name":"changes","definition":{"plan":{"kind":"query","query_id":query["id"]},"reaction":"event"}})).unwrap();
    }
    (root, path)
}

/// Runs the watcher in a separate process that exits at the old crash window
/// (after planning, before event admission and completion), then recovers the
/// dead owner. Returns the watcher row as the crash left it.
fn crash_worker(path: &Path) -> Value {
    // Concurrent Cargo invocations can unlink the running test executable.
    let executor = tempfile::tempdir().unwrap();
    let executable = executor.path().join("watcher-crash-worker");
    #[cfg(target_os = "linux")]
    let loaded = std::path::PathBuf::from("/proc/self/exe");
    #[cfg(not(target_os = "linux"))]
    let loaded = std::env::current_exe().unwrap();
    std::fs::copy(loaded, &executable).unwrap();
    let output = std::process::Command::new(&executable)
        .args(["--exact", "watcher_crash_worker", "--nocapture"])
        .env("CATABOLIC_WATCHER_CRASH_DB", path)
        .env("CATABOLIC_TEST_WATCHER_CRASH_AT", "before_finish")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(86),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let store = Store::open(path, true, false).unwrap();
    let crashed = watcher::execute(&store, "default", "get", &json!({"name":"changes"})).unwrap();
    assert!(!crashed["active_run"].is_null(), "{crashed}");
    assert_eq!(crashed["pending_reaction"], crashed["active_run"]);
    let recovered = watcher::execute(&store, "default", "recover", &json!({})).unwrap();
    assert_eq!(recovered["recovered"], json!([crashed["active_run"]]));
    crashed
}

fn retry_admits_exactly_one(path: &Path) {
    let mut store = Store::open(path, true, false).unwrap();
    let retry = run(&mut store);
    assert_eq!(retry["complete"], true, "{retry}");
    assert_eq!(retry["evaluation"]["changed"], false, "{retry}");
    // The planned reaction is still delivered, exactly once.
    assert_eq!(notifications(&store), 1);
    let finished = watcher::execute(&store, "default", "get", &json!({"name":"changes"})).unwrap();
    assert!(finished["pending_reaction"].is_null(), "{finished}");
    assert!(finished["active_run"].is_null(), "{finished}");
    // A later unchanged run admits nothing further.
    let unchanged = run(&mut store);
    assert_eq!(unchanged["complete"], true, "{unchanged}");
    assert_eq!(notifications(&store), 1);
}

#[test]
fn event_notification_is_admitted_once_across_crash_before_completion() {
    let (_root, path, _) = crash_before_completion();
    retry_admits_exactly_one(&path);
}

#[test]
fn retry_deduplicates_event_committed_by_split_admission_protocol() {
    let (_root, path, crashed) = crash_before_completion();
    {
        // Reproduce the frozen Python (and earlier native) split protocol: the
        // planned run's event committed in its own transaction before the crash.
        let store = Store::open(&path, true, false).unwrap();
        store
            .transaction(|db| {
                catabolic_store::notifications::emit(
                    db,
                    "default",
                    "watcher_changed",
                    "info",
                    "changes",
                    &json!([crashed["pending_reaction"]]),
                )
            })
            .unwrap();
        assert_eq!(notifications(&store), 1);
    }
    retry_admits_exactly_one(&path);
}

/// Commits a split-protocol event under `run`, as frozen Python would have.
fn legacy_event(path: &Path, run: &Value) {
    let store = Store::open(path, true, false).unwrap();
    store
        .transaction(|db| {
            catabolic_store::notifications::emit(
                db,
                "default",
                "watcher_changed",
                "info",
                "changes",
                &json!([run]),
            )
        })
        .unwrap();
}

#[test]
fn legacy_commit_then_crashed_native_retry_then_retry_admits_once() {
    // Legacy run A commits its event, then crashes before finishing.
    let (_root, path, first) = crash_before_completion();
    legacy_event(&path, &first["pending_reaction"]);
    // Unchanged native retry B replaces pending_reaction, then crashes before
    // its event and completion transaction commits.
    let second = crash_worker(&path);
    assert_ne!(second["pending_reaction"], first["pending_reaction"]);
    let store = Store::open(&path, true, false).unwrap();
    assert_eq!(notifications(&store), 1);
    drop(store);
    retry_admits_exactly_one(&path);
}

#[test]
fn event_committed_under_an_unchanged_legacy_retry_identity_is_not_repeated() {
    // A crashes without an event; a legacy unchanged retry B emits under its
    // own run identity and crashes before finishing.
    let (_root, path, _) = crash_before_completion();
    let second = crash_worker(&path);
    legacy_event(&path, &second["pending_reaction"]);
    retry_admits_exactly_one(&path);
}

#[test]
fn completed_change_then_crashed_unchanged_run_then_unchanged_run_admits_nothing() {
    let (_root, path) = fixture();
    {
        // A evaluates a change, delivers it and completes.
        let mut store = Store::open(&path, true, false).unwrap();
        let first = run(&mut store);
        assert_eq!(first["complete"], true, "{first}");
        assert_eq!(first["evaluation"]["changed"], true, "{first}");
        assert_eq!(notifications(&store), 1);
    }
    // B evaluates unchanged with no pending reaction, persists its own
    // pending_reaction and crashes before completion.
    let second = crash_worker(&path);
    let store = Store::open(&path, true, false).unwrap();
    let planned: String = store
        .db
        .query_row(
            "SELECT result FROM watcher_runs WHERE id=?",
            [second["pending_reaction"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let planned: Value = serde_json::from_str(&planned).unwrap();
    assert_eq!(planned["evaluation"]["changed"], false, "{planned}");
    assert_eq!(planned["retry_of"], Value::Null, "{planned}");
    drop(store);
    // C is also unchanged: nothing new is notified, and the chain completes.
    let mut store = Store::open(&path, true, false).unwrap();
    let third = run(&mut store);
    assert_eq!(third["complete"], true, "{third}");
    assert_eq!(third["evaluation"]["changed"], false, "{third}");
    assert_eq!(notifications(&store), 1);
    let finished = watcher::execute(&store, "default", "get", &json!({"name":"changes"})).unwrap();
    assert!(finished["pending_reaction"].is_null(), "{finished}");
}

/// Rewrites one planned run's recorded result, as a damaged catalog might.
fn rewrite(path: &Path, run: &Value, expression: &str) {
    let store = Store::open(path, true, false).unwrap();
    let sql = format!("UPDATE watcher_runs SET result={expression} WHERE id=?1");
    assert_eq!(store.db.execute(&sql, [run.as_str().unwrap()]).unwrap(), 1);
}

/// A retry over an unresolvable chain fails closed, admits nothing and leaves
/// the reaction pending. Returns the number of run lookups the chain walk made
/// (a debug-only counter); the wall clock is only a generous hang guard.
fn retry_fails_closed(path: &Path) -> usize {
    let mut store = Store::open(path, true, false).unwrap();
    watcher::take_transition_lookups();
    let started = std::time::Instant::now();
    let retry = run(&mut store);
    let lookups = watcher::take_transition_lookups();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "hang guard"
    );
    assert_eq!(retry["complete"], false, "{retry}");
    assert_eq!(retry["error"], "watcher_transition_unresolved", "{retry}");
    assert_eq!(notifications(&store), 0);
    let row = watcher::execute(&store, "default", "get", &json!({"name":"changes"})).unwrap();
    assert!(!row["pending_reaction"].is_null(), "{row}");
    lookups
}

#[test]
fn self_linked_and_mutually_linked_chains_fail_closed_in_bounded_steps() {
    // Retry C -> A -> A: the repeat is detected before a third lookup.
    let (_root, path, first) = crash_before_completion();
    rewrite(
        &path,
        &first["pending_reaction"],
        "json_set(result,'$.evaluation.changed',json('false'),'$.retry_of',id)",
    );
    assert_eq!(retry_fails_closed(&path), 2);

    // Retry C -> B -> A -> B: the repeat is detected before a fourth lookup.
    let (_root, path, first) = crash_before_completion();
    let second = crash_worker(&path);
    rewrite(
        &path,
        &first["pending_reaction"],
        &format!(
            "json_set(result,'$.evaluation.changed',json('false'),'$.retry_of','{}')",
            second["pending_reaction"].as_str().unwrap()
        ),
    );
    assert_eq!(retry_fails_closed(&path), 3);
}

#[test]
fn missing_or_malformed_comparison_and_link_fields_fail_closed() {
    for expression in [
        "json_remove(result,'$.evaluation.changed')",
        "json_set(result,'$.evaluation.changed','yes')",
        "json_set(result,'$.evaluation.changed',json('false'),'$.retry_of',7)",
        "json_remove(json_set(result,'$.evaluation.changed',json('false')),'$.retry_of')",
        // The changed run that ends the walk has its link validated too.
        "json_remove(result,'$.retry_of')",
        "json_set(result,'$.retry_of',7)",
        "json_set(result,'$.retry_of',json('{}'))",
        "json_set(result,'$.phase','observing')",
        "json_set(result,'$.phase',3)",
        "'{not json'",
        "NULL",
    ] {
        let (_root, path, first) = crash_before_completion();
        rewrite(&path, &first["pending_reaction"], expression);
        // Retry C -> A: never more than the two runs in this chain.
        assert_eq!(retry_fails_closed(&path), 2, "{expression}");
    }
}
