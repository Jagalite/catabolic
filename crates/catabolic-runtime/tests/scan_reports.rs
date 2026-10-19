// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Report projection only; disposable roots, not effect-owner qualification.
use catabolic_runtime::public_scan;
use catabolic_store::{Store, migration, observation};
use serde_json::{Value, json};
use std::fs;

fn fixture() -> (tempfile::TempDir, Store, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    let path = dir.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    observation::execute(
        &store,
        "default",
        "bind",
        &json!({"source":"media","root":source}),
    )
    .unwrap();
    (dir, store, source)
}
fn report(store: &mut Store, request: &Value) -> Value {
    public_scan::execute(store, "default", request).unwrap()
}
fn total(store: &Store, table: &str) -> i64 {
    store
        .db
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn completed_report_is_backed_by_durable_rows_and_actual_resource_samples() {
    let (_dir, mut store, source) = fixture();
    fs::write(source.join("note.txt"), "ordinary fixture\n").unwrap();
    let result = report(&mut store, &json!({}));
    let scan = &result["scans"][0];
    let evidence = &result["observations"][0];
    assert_eq!(evidence["report"], *scan);
    assert_eq!(evidence["reuse"], "created");
    assert_eq!(evidence["retry_state"], Value::Null);
    assert_eq!(
        evidence["coverage"]["execution"],
        evidence["guarantees"]["execution"]
    );
    assert_eq!(
        evidence["coverage"]["dirty_generation"],
        evidence["dirty_generation"]
    );
    assert!(evidence["guarantees"]["after"].as_f64().unwrap() > 0.0);
    assert!(
        evidence["guarantees"]["after"].as_f64().unwrap()
            < evidence["guarantees"]["minimum_start"].as_f64().unwrap()
    );
    assert_eq!(
        evidence["guarantees"]["minimum_start"],
        evidence["started_at"]
    );
    let progress = migration::rows(&store.db, "SELECT count(*) AS batches,sum(bytes) AS committed_bytes,max(committed_at) AS last_progress_at FROM observation_batches WHERE job_id=?", &[evidence["id"].as_str().unwrap().to_owned().into()]).unwrap();
    for key in ["batches", "committed_bytes", "last_progress_at"] {
        assert_eq!(scan["progress"][key], progress[0][key]);
    }
    assert_eq!(scan["batches"], 3);
    assert_eq!(scan["media_headers"]["invalid"], json!([]));
    assert_eq!(scan["media_headers"]["invalid_truncated"], false);
    assert_eq!(scan["suggested_actions"], json!([]));
    for key in ["temporary_bytes", "temporary_free_bytes", "elapsed_seconds"] {
        assert!(scan["progress"][key].as_f64().unwrap() > 0.0);
    }
    let durable: String = store
        .db
        .query_row(
            "SELECT report FROM observation_jobs WHERE id=?",
            [evidence["id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(catabolic_core::decode_json(&durable).unwrap(), *scan);
    assert_eq!(
        fs::read_to_string(source.join("note.txt")).unwrap(),
        "ordinary fixture\n"
    );
    for table in ["journal", "mappings", "processing_jobs", "watch_revisions"] {
        assert_eq!(total(&store, table), 0, "{table}");
    }
    let changes = store.db.total_changes();
    let resumed = report(&mut store, &json!({"request_id":evidence["request_id"]}));
    assert_eq!(resumed["observations"][0]["reuse"], "resumed");
    assert_eq!(resumed["scans"][0], *scan);
    assert_eq!(store.db.total_changes(), changes);
    let reused = observation::execute(
        &store,
        "default",
        "scan",
        &json!({"source":"media","reuse_completed":true,"max_age":3600}),
    )
    .unwrap();
    assert_eq!(reused["observations"][0]["reuse"], "completed");
    assert_eq!(reused["scans"][0], *scan);
    assert_eq!(total(&store, "observation_jobs"), 1);
    assert_eq!(total(&store, "observation_batches"), 3);
    observation::execute(
        &store,
        "default",
        "cancel",
        &json!({"request_id":evidence["request_id"]}),
    )
    .unwrap();
    let changes = store.db.total_changes();
    let cancelled = report(&mut store, &json!({"request_id":evidence["request_id"]}));
    assert_eq!(cancelled["complete"], false);
    assert_eq!(cancelled["observations"][0]["state"], "cancelled");
    assert_eq!(cancelled["observations"][0]["job_state"], "complete");
    assert_eq!(cancelled["observations"][0]["retry_state"], Value::Null);
    assert!(cancelled["observations"][0].get("reuse").is_none());
    assert_eq!(store.db.total_changes(), changes);
}

#[test]
fn invalid_header_details_are_sorted_bounded_and_backed_by_owner_checks() {
    let (_dir, mut store, source) = fixture();
    for number in (0..101).rev() {
        fs::write(source.join(format!("clip-{number:03}.wav")), [0; 64]).unwrap();
    }
    let result = report(&mut store, &json!({}));
    let scan = &result["scans"][0];
    assert_eq!(scan["media_headers"]["counts"]["invalid"], 101);
    assert_eq!(scan["media_headers"]["invalid_truncated"], true);
    let expected = migration::rows(&store.db, "SELECT h.file_id,f.path,h.reason FROM media_header_checks h JOIN files f ON f.id=h.file_id WHERE h.scan_id=? AND h.status='invalid' ORDER BY f.path LIMIT 100", &[scan["scan_id"].as_str().unwrap().to_owned().into()]).unwrap();
    assert_eq!(scan["media_headers"]["invalid"], json!(expected));
    assert_eq!(expected.len(), 100);
    assert_eq!(expected[0]["path"], "clip-000.wav");
    assert_eq!(expected[99]["path"], "clip-099.wav");
    assert!(
        expected
            .iter()
            .all(|row| row["reason"] == "zero_filled_header")
    );
    for number in 0..101 {
        assert_eq!(
            fs::read(source.join(format!("clip-{number:03}.wav"))).unwrap(),
            [0; 64]
        );
    }
}

#[test]
fn deferred_reuse_reports_actual_retry_and_continuation_request_without_new_batches() {
    let (_dir, mut store, source) = fixture();
    for number in 0..3 {
        fs::write(source.join(format!("note-{number}.txt")), "fixture").unwrap();
    }
    let request = json!({"budgets":{"total_entries":1}});
    let first = report(&mut store, &request);
    let scan = &first["scans"][0];
    let evidence = &first["observations"][0];
    assert_eq!(scan["complete"], false);
    assert_eq!(scan["deferred"], true);
    assert_eq!(evidence["reuse"], "created");
    assert_eq!(evidence["retry_state"], "explicit_continuation_required");
    assert_eq!(
        scan["suggested_actions"][0]["arguments"]["continue_request"],
        evidence["request_id"]
    );
    assert_eq!(scan["suggested_actions"][1]["arguments"]["name"], "media");
    let batches = total(&store, "observation_batches");
    let second = report(&mut store, &request);
    assert_eq!(second["observations"][0]["reuse"], "deferred");
    assert_eq!(
        second["observations"][0]["retry_state"],
        "explicit_continuation_required"
    );
    assert_eq!(second["scans"][0], *scan);
    assert_eq!(total(&store, "observation_jobs"), 1);
    assert_eq!(total(&store, "observation_batches"), batches);
}
