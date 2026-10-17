// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Schema-39 source ctimes reach playback admission and the worker's viewer
//! check unchanged. Admission pins the source snapshot, including the bound
//! ctime; the worker must compare that same ctime instead of dropping it, or
//! every viewer of a schema-39 catalog is rejected and valid encodes cancel.
use catabolic_runtime::artifacts;
use catabolic_store::{
    Store, access::Access, artifact_mutation, migration, migration::NativeUpgradeQuiescence,
    observation, playback,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

const TOOLS: &str = r#"{"fixture":"no tool execution during admission"}"#;

fn tools() -> Value {
    serde_json::from_str(TOOLS).unwrap()
}

fn scan(path: &Path) {
    let store = Store::open(path, true, false).unwrap();
    observation::execute(&store, "default", "scan", &json!({"source":"source"})).unwrap();
}

fn upgrade(path: &Path) {
    let plan = migration::plan_native(path).unwrap();
    let quiescence = NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    };
    migration::upgrade_native(path, &plan, quiescence, None, &|_, _, _| Ok(())).unwrap();
}

fn file_id(store: &Store) -> String {
    store
        .db
        .query_row("SELECT id FROM files WHERE path='sample.mkv'", [], |r| {
            r.get(0)
        })
        .unwrap()
}

/// A schema-32 catalog with a scanned source, an exposed playback operation and
/// one operator token; it is then upgraded to schema 39 and rescanned.
fn fixture(root: &Path) -> std::path::PathBuf {
    let path = root.join("catalog.db");
    let input = root.join("source");
    let output = root.join("output");
    fs::create_dir(&input).unwrap();
    fs::create_dir(&output).unwrap();
    fs::write(input.join("sample.mkv"), b"unchanged source").unwrap();
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    observation::execute(
        &store,
        "default",
        "bind",
        &json!({"source":"source","root":input.to_str().unwrap()}),
    )
    .unwrap();
    observation::execute(&store, "default", "scan", &json!({"source":"source"})).unwrap();
    let file = file_id(&store);
    store.transaction(|db| {
        db.execute("INSERT INTO items VALUES ('item','movie','{}')", [])?;
        db.execute("INSERT INTO item_files(id,file_id,item_id,role,metadata,active,origin) VALUES ('association',?,'item','primary','{}',1,'explicit')", [&file])?;
        db.execute("INSERT INTO api_sources VALUES ('default','source')", [])?;
        db.execute("INSERT INTO api_principals VALUES ('caller','default',1)", [])?;
        db.execute("INSERT INTO api_grants VALUES ('grant','caller',?,1,1)", [catabolic_core::encode(&json!({"actions":["*"],"operator":true}))])?;
        db.execute("INSERT INTO api_tokens VALUES ('token','caller','digest',?,4102444800,0)", [catabolic_core::encode(&json!(["grant"]))])?;
        Ok(())
    })
    .unwrap();
    artifact_mutation::bind(&store, "default", "generated", output.to_str().unwrap()).unwrap();
    let recipe = artifacts::recipe(
        &store,
        "playback",
        "h264-720p",
        &json!({"max_output_bytes":1_048_576,"reserve_bytes":0}),
        None,
    )
    .unwrap();
    store
        .transaction(|db| {
            db.execute(
                "INSERT INTO api_operations VALUES ('operation','default',?,'generated',1)",
                [recipe["id"].as_str()],
            )?;
            // Playback admission requires the operation's location exposed.
            db.execute("INSERT INTO api_sources VALUES ('default','generated')", [])?;
            Ok(())
        })
        .unwrap();
    drop(store);
    upgrade(&path);
    scan(&path);
    path
}

fn admit(path: &Path, key: &str) -> Value {
    let store = Store::open(path, true, false).unwrap();
    assert_eq!(store.schema_version, 39);
    let file = file_id(&store);
    let ctime: i64 = store
        .db
        .query_row(
            "SELECT ctime_ns FROM native_source_ctimes WHERE file_id=?",
            [&file],
            |r| r.get(0),
        )
        .unwrap();
    let revision = catabolic_store::content_access::revision_of(&store, "default", &file).unwrap();
    playback::worker_heartbeat(
        &store,
        "default",
        std::process::id(),
        &json!({"playback_hls":true,"tools":tools()}),
    )
    .unwrap();
    let mut access = Access::from_stored_token(store, "token").unwrap();
    let body = json!({"item_id":"item","source_file_id":file,"source_revision":revision,"operation_id":"operation"});
    playback::admit(&mut access, &body, key, 600, &tools()).unwrap();
    let store = access.into_store();
    let job_id: String = store
        .db
        .query_row(
            "SELECT job_id FROM api_playback_sessions WHERE idempotency_key=?",
            [key],
            |r| r.get(0),
        )
        .unwrap();
    let job = playback::job(&store, &job_id).unwrap();
    let pinned: Value = catabolic_core::decode_json(job["snapshot"].as_str().unwrap()).unwrap();
    // Admission pins the bound ctime, which equals the live ctime it checked.
    assert_eq!(pinned["ctime_ns"], json!(ctime));
    job
}

#[test]
fn schema39_playback_viewers_stay_authorized_until_the_source_changes() {
    let root = tempfile::tempdir().unwrap();
    let path = fixture(root.path());
    let job = admit(&path, "request-1");
    let store = Store::open(&path, false, false).unwrap();
    assert_eq!(playback::interested(&store, &job).unwrap(), 1);
    drop(store);
    // A ctime-only change is a possible content change: after the rescan the
    // bound ctime differs from the pinned one and the viewer is no longer
    // counted for this encode.
    let source = root.path().join("source/sample.mkv");
    let before = fs::metadata(&source).unwrap();
    for mode in [0o600, 0o644, 0o600] {
        fs::set_permissions(&source, fs::Permissions::from_mode(mode)).unwrap();
        use std::os::unix::fs::MetadataExt;
        let now = fs::metadata(&source).unwrap();
        if (now.ctime(), now.ctime_nsec()) != (before.ctime(), before.ctime_nsec()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    scan(&path);
    let store = Store::open(&path, false, false).unwrap();
    assert_eq!(playback::interested(&store, &job).unwrap(), 0);
}

#[test]
fn a_job_admitted_without_a_bound_ctime_keeps_legacy_normalization() {
    // Admission without a bound ctime pins the live ctime, while the current
    // snapshot has none: the worker compares without it, as before schema 39.
    let root = tempfile::tempdir().unwrap();
    let path = fixture(root.path());
    let store = Store::open(&path, true, false).unwrap();
    store
        .transaction(|db| {
            db.execute("DELETE FROM native_source_ctimes", [])?;
            Ok(())
        })
        .unwrap();
    drop(store);
    let store = Store::open(&path, true, false).unwrap();
    let file = file_id(&store);
    let snapshot = catabolic_store::content_access::snapshot(&store, "default", &file).unwrap();
    assert!(snapshot.get("ctime_ns").is_none());
    drop(store);
    let job = {
        let store = Store::open(&path, true, false).unwrap();
        let revision =
            catabolic_store::content_access::revision_of(&store, "default", &file).unwrap();
        playback::worker_heartbeat(
            &store,
            "default",
            std::process::id(),
            &json!({"playback_hls":true,"tools":tools()}),
        )
        .unwrap();
        let mut access = Access::from_stored_token(store, "token").unwrap();
        let body = json!({"item_id":"item","source_file_id":file,"source_revision":revision,"operation_id":"operation"});
        playback::admit(&mut access, &body, "request-legacy", 600, &tools()).unwrap();
        let store = access.into_store();
        let job_id: String = store
            .db
            .query_row(
                "SELECT job_id FROM api_playback_sessions WHERE idempotency_key='request-legacy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        playback::job(&store, &job_id).unwrap()
    };
    let store = Store::open(&path, false, false).unwrap();
    assert_eq!(playback::interested(&store, &job).unwrap(), 1);
    drop(store);
    // The first native scan records the ctime and rotates the content revision
    // once (schema section 12), so the session's pinned revision is stale.
    scan(&path);
    let store = Store::open(&path, false, false).unwrap();
    assert_eq!(playback::interested(&store, &job).unwrap(), 0);
}
