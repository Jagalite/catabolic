// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Disposable real-FFmpeg coverage of durable generated-output publication.
use catabolic_runtime::{artifacts, rendering};
use catabolic_store::{
    Store, artifact_mutation as storage, media_mutation as claims, migration, observation,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    sync::{Arc, atomic::AtomicBool},
};
struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    source: std::path::PathBuf,
    recipe: String,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.db");
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let input = dir.path().join("input");
        fs::create_dir(&input).unwrap();
        let source = input.join("tone.wav");
        // One second of deterministic PCM, no fixture-generation subprocess.
        let samples = 16000u32;
        let size = samples * 2;
        let mut wav = fs::File::create(&source).unwrap();
        wav.write_all(b"RIFF").unwrap();
        wav.write_all(&(36 + size).to_le_bytes()).unwrap();
        wav.write_all(b"WAVEfmt ").unwrap();
        wav.write_all(&16u32.to_le_bytes()).unwrap();
        wav.write_all(&1u16.to_le_bytes()).unwrap();
        wav.write_all(&1u16.to_le_bytes()).unwrap();
        wav.write_all(&16000u32.to_le_bytes()).unwrap();
        wav.write_all(&32000u32.to_le_bytes()).unwrap();
        wav.write_all(&2u16.to_le_bytes()).unwrap();
        wav.write_all(&16u16.to_le_bytes()).unwrap();
        wav.write_all(b"data").unwrap();
        wav.write_all(&size.to_le_bytes()).unwrap();
        for n in 0..samples {
            let sample = if (n / 18) % 2 == 0 { 1000i16 } else { -1000i16 };
            wav.write_all(&sample.to_le_bytes()).unwrap();
        }
        wav.sync_all().unwrap();
        drop(wav);
        observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"input","root":input.to_str()}),
        )
        .unwrap();
        let st = source.metadata().unwrap();
        store.transaction(|db|{
            db.execute("INSERT INTO files VALUES ('file','input','tone.wav')",[])?;db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','input',1,1,'[]')",[])?;
            db.execute("INSERT INTO observations VALUES ('default','file',?,?,?,?, 'present','scan')",params![st.len() as i64,st.mtime()*1_000_000_000+st.mtime_nsec(),st.dev() as i64,st.ino() as i64])?;
            db.execute("INSERT INTO items VALUES ('item','recording','{}')",[])?;db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('association','file','item','primary','explicit')",[])?;Ok(())
        }).unwrap();
        let generated = dir.path().join("output");
        fs::create_dir(&generated).unwrap();
        storage::bind(&store, "default", "generated", generated.to_str().unwrap()).unwrap();
        let recipe = artifacts::recipe(
            &store,
            "flac",
            "audio-flac",
            &json!({"reserve_bytes":0}),
            None,
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        Self {
            dir,
            store,
            source,
            recipe,
        }
    }
    fn enqueue(&mut self) -> String {
        artifacts::enqueue(
            &mut self.store,
            "default",
            "file",
            &self.recipe,
            "generated",
            "item",
        )
        .unwrap()["job_id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn run(&mut self, id: &str) -> Value {
        artifacts::run(
            &mut self.store,
            "default",
            100,
            Some(&[id.into()]),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap()
    }
}
fn available() -> bool {
    let caps = rendering::capabilities().unwrap();
    let available = caps["presets"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["name"] == "audio-flac" && v["available"] == true);
    if !available {
        eprintln!("real-media test skipped: optional FFmpeg FLAC capability unavailable");
    }
    available
}
#[test]
fn real_media_render_publishes_validated_bytes_lineage_and_reuses_only_intact_outputs() {
    if !available() {
        return;
    }
    let mut f = Fixture::new();
    let source = fs::read(&f.source).unwrap();
    let id = f.enqueue();
    let result = f.run(&id);
    assert_eq!(result["complete"], true, "{result}");
    assert_eq!(result["completed"].as_array().unwrap().len(), 1);
    let a = storage::get(
        &f.store,
        "default",
        result["completed"][0]["artifact_id"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(a["state"], "ready");
    let path = f
        .dir
        .path()
        .join("output")
        .join(a["path"].as_str().unwrap());
    let bytes = fs::read(&path).unwrap();
    assert_eq!(
        catabolic_core::sha256(&bytes),
        a["sha256"].as_str().unwrap()
    );
    assert!(bytes.starts_with(b"fLaC"));
    assert_eq!(fs::read(&f.source).unwrap(), source);
    let output: Value = catabolic_store::output_mutation::get(
        &f.store,
        "default",
        result["completed"][0]["output_id"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(output["origin"], "generated");
    assert_eq!(output["source_file_id"], "file");
    assert_eq!(f.enqueue(), id);
    fs::write(path, b"corrupt").unwrap();
    assert_ne!(f.enqueue(), id);
}
#[test]
fn source_mutation_and_pre_cancelled_run_never_publish() {
    if !available() {
        return;
    }
    let mut f = Fixture::new();
    let id = f.enqueue();
    fs::write(&f.source, b"not the queued revision").unwrap();
    let result = f.run(&id);
    assert_eq!(result["complete"], false, "{result}");
    assert_eq!(
        claims::get_job(&f.store, "default", &id).unwrap()["state"],
        "failed"
    );
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM media_outputs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let mut f = Fixture::new();
    let id = f.enqueue();
    let result = artifacts::run(
        &mut f.store,
        "default",
        100,
        Some(std::slice::from_ref(&id)),
        Arc::new(AtomicBool::new(true)),
    )
    .unwrap();
    assert_eq!(result["complete"], false);
    assert_eq!(
        claims::get_job(&f.store, "default", &id).unwrap()["state"],
        "queued"
    );
}
#[test]
fn component_mux_requires_an_explicitly_admitted_package() {
    let mut f = Fixture::new();
    let recipe = artifacts::recipe(
        &f.store,
        "package",
        "component-mux",
        &json!({"reserve_bytes":0}),
        None,
    )
    .unwrap();
    assert!(
        artifacts::enqueue(
            &mut f.store,
            "default",
            "file",
            recipe["id"].as_str().unwrap(),
            "generated",
            "item"
        )
        .unwrap_err()
        .0
        .contains("explicitly selected package")
    );
}

// The parent integration test launches only this helper in a separate process.
// A crash hook exits before Rust destructors, exercising SQLite/claim recovery.
#[test]
fn artifact_crash_worker() {
    let Ok(path) = std::env::var("CATABOLIC_ARTIFACT_CRASH_DB") else {
        return;
    };
    let job = std::env::var("CATABOLIC_ARTIFACT_CRASH_JOB").unwrap();
    let mut store = Store::open(std::path::Path::new(&path), true, false).unwrap();
    artifacts::run_one(&mut store, "default", &job, &AtomicBool::new(false)).unwrap();
    panic!("the configured crash hook did not terminate the worker");
}
#[test]
#[cfg(debug_assertions)]
fn abrupt_worker_death_recovers_every_durable_publication_boundary() {
    if !available() {
        return;
    }
    // Concurrent Cargo invocations can unlink the currently running test
    // executable. Pin the loaded executable into a disposable owned location.
    let executor = tempfile::tempdir().unwrap();
    let executable = executor.path().join("artifact-crash-worker");
    #[cfg(target_os = "linux")]
    let loaded = std::path::PathBuf::from("/proc/self/exe");
    #[cfg(not(target_os = "linux"))]
    let loaded = std::env::current_exe().unwrap();
    fs::copy(loaded, &executable).unwrap();
    for stage in [
        "after_planned",
        "after_temporary",
        "after_intent",
        "before_rename",
        "after_rename_before_sync",
        "after_rename",
        "before_register_commit",
        "after_register_commit",
    ] {
        let mut f = Fixture::new();
        let id = f.enqueue();
        let path = f.store.path.clone();
        let status = f
            .store
            .detached(|| {
                Ok(std::process::Command::new(&executable)
                    .args(["--exact", "artifact_crash_worker", "--nocapture"])
                    .env("CATABOLIC_ARTIFACT_CRASH_DB", &path)
                    .env("CATABOLIC_ARTIFACT_CRASH_JOB", &id)
                    .env("CATABOLIC_TEST_ARTIFACT_CRASH_AT", stage)
                    .env_remove("CATABOLIC_TEST_ARTIFACT_FAILPOINT")
                    .output()?)
            })
            .unwrap();
        assert_eq!(
            status.status.code(),
            Some(86),
            "{stage}: stdout={} stderr={}",
            String::from_utf8_lossy(&status.stdout),
            String::from_utf8_lossy(&status.stderr)
        );
        let result = artifacts::recover(&mut f.store, "default", 100).unwrap();
        assert_eq!(result["complete"], true, "{stage}: {result}");
        let job = claims::get_job(&f.store, "default", &id).unwrap();
        let (artifact_id, state): (String, String) = f
            .store
            .db
            .query_row(
                "SELECT id,state FROM processing_artifacts WHERE job_id=?",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        if ["after_planned", "after_temporary"].contains(&stage) {
            assert_eq!(state, "interrupted", "{stage}");
            assert_eq!(job["state"], "failed");
        } else {
            assert_eq!(state, "ready", "{stage}");
            assert_eq!(job["state"], "complete");
            let artifact = storage::get(&f.store, "default", &artifact_id).unwrap();
            assert!(storage::usable(&mut f.store, "default", &artifact).unwrap());
        }
        let second = artifacts::recover(&mut f.store, "default", 100).unwrap();
        assert_eq!(second["recovered"], json!([]), "{stage}");
        let expected = if state == "ready" { 1 } else { 0 };
        assert_eq!(
            f.store
                .db
                .query_row("SELECT count(*) FROM media_outputs", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            expected,
            "{stage}"
        );
    }
}

fn probed_component(f: &mut Fixture) -> String {
    let queued = catabolic_runtime::media::enqueue(
        &f.store,
        "default",
        "probe",
        &["file".into()],
        &json!({}),
        true,
        None,
    )
    .unwrap();
    let id = queued["queued"][0].as_str().unwrap().to_owned();
    let result = catabolic_runtime::media::run(
        &mut f.store,
        "default",
        &[id],
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(result["counts"]["complete"], 1, "{result}");
    f.store
        .db
        .query_row(
            "SELECT id FROM component_occurrences WHERE file_id='file' ORDER BY stream_key LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
}
#[test]
fn reviewed_component_extraction_publishes_exact_stream_and_lineage() {
    assert!(
        available(),
        "FFmpeg FLAC capability required for component extraction acceptance"
    );
    let mut f = Fixture::new();
    let component = probed_component(&mut f);
    let source = fs::read(&f.source).unwrap();
    let plan = catabolic_runtime::component_packaging::extraction(
        &mut f.store,
        "default",
        &component,
        &f.recipe,
        "generated",
        false,
        None,
    )
    .unwrap();
    assert_eq!(
        plan["package"]["components"][0]["occurrence"]["kind"],
        "audio"
    );
    assert!(
        catabolic_runtime::component_packaging::extraction(
            &mut f.store,
            "default",
            &component,
            &f.recipe,
            "generated",
            true,
            Some("stale")
        )
        .is_err()
    );
    let job = catabolic_runtime::component_packaging::extraction(
        &mut f.store,
        "default",
        &component,
        &f.recipe,
        "generated",
        true,
        plan["plan_id"].as_str(),
    )
    .unwrap();
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM component_job_inputs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let result = f.run(job["job_id"].as_str().unwrap());
    assert_eq!(result["complete"], true, "{result}");
    let artifact = storage::get(
        &f.store,
        "default",
        result["completed"][0]["artifact_id"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(
        artifact["validation"]["output"]["streams"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        artifact["validation"]["component_signature"],
        plan["plan_id"]
    );
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT relationship FROM component_output_lineage",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "extracted_conversion"
    );
    assert_eq!(fs::read(&f.source).unwrap(), source);
    // The metadata-only reader works inside a read transaction without dropping
    // the snapshot or acquiring a writer, but never substitutes for full hash.
    f.store.db.execute_batch("BEGIN").unwrap();
    assert!(storage::usable_metadata(&f.store, "default", &artifact).unwrap());
    assert!(!f.store.db.is_autocommit());
    f.store.db.execute_batch("ROLLBACK").unwrap();
    let output = f
        .dir
        .path()
        .join("output")
        .join(artifact["path"].as_str().unwrap());
    let mut bytes = fs::read(&output).unwrap();
    bytes[0] ^= 1;
    fs::write(&output, &bytes).unwrap();
    assert!(!storage::usable_metadata(&f.store, "default", &artifact).unwrap());
}
#[test]
fn extraction_rejects_wrong_stream_and_changed_evidence_before_effects() {
    let mut f = Fixture::new();
    let component = probed_component(&mut f);
    let bad = artifacts::recipe(
        &f.store,
        "wrong",
        "audio-flac",
        &json!({"stream":1,"reserve_bytes":0}),
        None,
    )
    .unwrap();
    assert!(
        catabolic_store::component_mutation::extraction_plan(
            &f.store,
            "default",
            &component,
            bad["id"].as_str().unwrap()
        )
        .unwrap_err()
        .0
        .contains("different stream")
    );
    let plan = catabolic_store::component_mutation::extraction_plan(
        &f.store, "default", &component, &f.recipe,
    )
    .unwrap();
    let job = catabolic_runtime::component_packaging::extraction(
        &mut f.store,
        "default",
        &component,
        &f.recipe,
        "generated",
        true,
        plan["plan_id"].as_str(),
    )
    .unwrap();
    f.store
        .db
        .execute(
            r#"UPDATE item_files SET metadata='{"title":"changed"}' WHERE id='association'"#,
            [],
        )
        .unwrap();
    let result = f.run(job["job_id"].as_str().unwrap());
    assert_eq!(result["complete"], false);
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM processing_artifacts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
