// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Disposable schema-32 integration coverage. No user catalogs or network calls.
use catabolic_runtime::media;
use catabolic_store::{Store, media_mutation as storage, migration};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    sync::{Arc, atomic::AtomicBool},
};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    source: std::path::PathBuf,
}
impl Fixture {
    fn new(name: &str, content: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.db");
        let root = dir.path().join("source");
        fs::create_dir(&root).unwrap();
        let source = root.join(name);
        fs::write(&source, content).unwrap();
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let r = root.metadata().unwrap();
        let s = source.metadata().unwrap();
        store.transaction(|db|{
            db.execute("INSERT INTO locations VALUES ('source')",[])?;
            db.execute("INSERT INTO bindings VALUES ('default','source','source',?,?,?)",rusqlite_args![root.to_string_lossy().as_ref(),r.dev() as i64,r.ino() as i64])?;
            db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','source',1,1,'[]')",[])?;
            db.execute("INSERT INTO files VALUES ('file','source',?)",[name])?;
            db.execute("INSERT INTO observations VALUES ('default','file',?,?,?,?, 'present','scan')",rusqlite_args![s.size() as i64,s.mtime()*1_000_000_000+s.mtime_nsec(),s.dev() as i64,s.ino() as i64])?;
            db.execute("INSERT INTO items VALUES ('item','movie','{}')",[])?;
            db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('association','file','item','primary','explicit')",[])?;
            Ok(())
        }).unwrap();
        Self {
            _dir: dir,
            store,
            source,
        }
    }
    fn enqueue(&self, operation: &str) -> String {
        let result = media::enqueue(
            &self.store,
            "default",
            operation,
            &["file".into()],
            &json!({}),
            true,
            None,
        )
        .unwrap();
        assert_eq!(result["errors"], json!([]), "{result}");
        result["queued"][0].as_str().unwrap().into()
    }
    fn run(&mut self, id: &str) -> Value {
        media::run(
            &mut self.store,
            "default",
            &[id.into()],
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap()
    }
}
// Use SQLite's trait-object parameter arrays without adding a test dependency.
macro_rules! rusqlite_args { ($($value:expr),+ $(,)?) => { [$(&$value as &dyn rusqlite::ToSql),+] }; }
use rusqlite_args;

#[test]
fn durable_hash_baseline_and_attempt_are_atomic_and_python_shaped() {
    let mut f = Fixture::new("sample.txt", b"hello native\n");
    let before = fs::read(&f.source).unwrap();
    let id = f.enqueue("hash");
    assert_eq!(f.run(&id)["counts"]["complete"], 1);
    let job = storage::get_job(&f.store, "default", &id).unwrap();
    assert_eq!(job["state"], "complete");
    assert_eq!(job["result"]["digest"], catabolic_core::sha256(&before));
    let (facts,attempts,baselines):(i64,i64,i64)=f.store.db.query_row("SELECT (SELECT count(*) FROM file_facts),(SELECT count(*) FROM processing_attempts),(SELECT count(*) FROM content_baselines)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!((facts, attempts, baselines), (1, 1, 1));
    assert_eq!(fs::read(&f.source).unwrap(), before);
    let cached = media::enqueue(
        &f.store,
        "default",
        "hash",
        &["file".into()],
        &json!({}),
        false,
        None,
    )
    .unwrap();
    assert_eq!(cached["cached"], json!([id]));
    let id = f.enqueue("verify");
    assert_eq!(f.run(&id)["counts"]["complete"], 1);
}
#[test]
fn cancellation_takeover_and_expiry_fence_all_publication() {
    let f = Fixture::new("sample.txt", b"fenced");
    let id = f.enqueue("hash");
    let first = storage::start(&f.store, "default", &id, media::now()).unwrap();
    assert!(storage::start(&f.store, "default", &id, media::now()).is_err());
    let result = media::process(
        &storage::get_job(&f.store, "default", &id).unwrap(),
        &AtomicBool::new(false),
    );
    storage::cancel(&f.store, "default", &id).unwrap();
    assert!(
        storage::complete(&f.store, "default", &first, &result, media::now(), 30, None).is_err()
    );
    storage::retry(&f.store, "default", &id).unwrap();
    storage::release(&f.store, &first).unwrap();
    let second = storage::start(&f.store, "default", &id, media::now()).unwrap();
    assert_eq!(second.generation, first.generation + 1);
    assert_eq!(second.attempt, 2);
    assert!(
        storage::complete(&f.store, "default", &first, &result, media::now(), 30, None).is_err()
    );
    assert!(storage::heartbeat(&f.store, &second, second.lease_until).is_err());
    assert_eq!(
        storage::complete(
            &f.store,
            "default",
            &second,
            &result,
            media::now(),
            30,
            None
        )
        .unwrap(),
        "complete"
    );
    assert!(
        storage::complete(
            &f.store,
            "default",
            &second,
            &result,
            media::now(),
            30,
            None
        )
        .is_err()
    );
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM processing_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn source_replacement_and_symlink_never_publish_facts() {
    let mut f = Fixture::new("source.txt", b"old source");
    let id = f.enqueue("hash");
    fs::rename(&f.source, f.source.with_extension("old")).unwrap();
    fs::write(&f.source, b"new source").unwrap();
    assert_eq!(f.run(&id)["counts"]["changed"], 1);
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM file_facts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    fs::remove_file(&f.source).unwrap();
    std::os::unix::fs::symlink(f.source.with_extension("old"), &f.source).unwrap();
    assert_eq!(
        media::enqueue(
            &f.store,
            "default",
            "hash",
            &["file".into()],
            &json!({}),
            true,
            None
        )
        .unwrap()["complete"],
        false
    );
}
#[test]
fn immutable_analysis_definition_reuses_digest_and_rejects_options() {
    let f = Fixture::new("test", b"x");
    let value = json!({"kind":"analysis","operation":"hash","options":{"timeout":7}});
    let a = media::put_operation(&f.store, "checksum", &value).unwrap();
    let b = media::put_operation(&f.store, "checksum", &value).unwrap();
    assert_eq!(a, b);
    assert_eq!(a["revision"], 1);
    assert_eq!(
        a["digest"],
        catabolic_core::sha256(catabolic_core::encode(&a["definition"]))
    );
    assert!(media::operation_config("probe", &json!({"timeout":true})).is_err());
    assert!(
        media::operation_config(
            "probe",
            &json!({"required_streams":["audio","audio","video"]})
        )
        .is_err()
    );
    assert!(media::operation_config("invalid", &json!({})).is_err());
}
#[test]
fn real_wav_probe_decode_and_component_index() {
    // PCM mono 8 kHz WAV is deterministic and needs no fixture download/tool generation.
    let samples = 800u32;
    let mut wav = vec![];
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + samples * 2).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(samples * 2).to_le_bytes());
    wav.resize(44 + (samples * 2) as usize, 0);
    let mut f = Fixture::new("tone.wav", &wav);
    assert_eq!(
        media::tool_signature("probe")["available"],
        true,
        "ffprobe is required for this acceptance scenario"
    );
    assert_eq!(
        media::tool_signature("decode")["available"],
        true,
        "ffmpeg is required for this acceptance scenario"
    );
    let id = f.enqueue("probe");
    let result = f.run(&id);
    assert_eq!(
        result["counts"]["complete"],
        1,
        "{result}: {}",
        storage::get_job(&f.store, "default", &id).unwrap()
    );
    let job = storage::get_job(&f.store, "default", &id).unwrap();
    assert_eq!(
        job["result"]["summary"]["audio_codecs"],
        json!(["pcm_s16le"])
    );
    assert!(job["result"]["format"].get("filename").is_none());
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM component_occurrences", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let id = f.enqueue("decode");
    assert_eq!(f.run(&id)["counts"]["complete"], 1);
    assert_eq!(fs::read(&f.source).unwrap(), wav);
}
#[test]
fn zero_filled_header_and_bad_tool_result_never_become_success() {
    let mut f = Fixture::new("broken.mkv", &[0; 128]);
    let id = f.enqueue("probe");
    assert_eq!(f.run(&id)["counts"]["failed"], 1);
    let job = storage::get_job(&f.store, "default", &id).unwrap();
    assert_eq!(
        job["result"]["media_health"]["reason"],
        "zero_filled_header"
    );
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM file_facts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn text_segments_terms_and_failed_refresh_preserve_existing_facts() {
    let mut f = Fixture::new(
        "notes.txt",
        "Straße STRASSE foo_bar café\n第二行\n".as_bytes(),
    );
    let id = f.enqueue("text");
    assert_eq!(f.run(&id)["counts"]["complete"], 1);
    let terms = f
        .store
        .db
        .prepare("SELECT DISTINCT term FROM text_terms ORDER BY term")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(terms, vec!["bar", "café", "foo", "strasse", "第二行"]);
    let queued = media::enqueue(
        &f.store,
        "default",
        "text",
        &["file".into()],
        &json!({"max_bytes":1}),
        true,
        None,
    )
    .unwrap();
    let failed = queued["queued"][0].as_str().unwrap();
    assert_eq!(f.run(failed)["counts"]["partial"], 1);
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT job_id FROM file_facts WHERE operation='text'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        id
    );
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM text_segments", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn output_definitions_and_registration_preserve_curated_associations() {
    use catabolic_store::output_mutation as outputs;
    let f = Fixture::new("original.txt", b"source");
    let generated = f.source.with_file_name("external.txt");
    fs::write(&generated, b"external result").unwrap();
    let s = generated.metadata().unwrap();
    f.store.transaction(|db|{db.execute("INSERT INTO files VALUES ('derived','source','external.txt')",[])?;db.execute("INSERT INTO observations VALUES ('default','derived',?,?,?,?, 'present','scan')",rusqlite_args![s.size() as i64,s.mtime()*1_000_000_000+s.mtime_nsec(),s.dev() as i64,s.ino() as i64])?;db.execute("INSERT INTO item_files(id,file_id,item_id,role,metadata,origin,active) VALUES ('curated','derived','item','primary','{\"curated\": true}','explicit',1)",[])?;Ok(())}).unwrap();
    let definition = outputs::define(
        &f.store,
        "external",
        &json!({"purpose":"custom","file_metadata":{"from_definition":true}}),
    )
    .unwrap();
    let again = outputs::define(
        &f.store,
        "external",
        &json!({"purpose":"custom","file_metadata":{"from_definition":true}}),
    )
    .unwrap();
    assert_eq!(definition, again);
    assert_eq!(definition["definition"]["version"], 2);
    let result = outputs::register(
        &f.store,
        "default",
        "derived",
        "file",
        "item",
        definition["id"].as_str().unwrap(),
        &json!({"explicit":true}),
    )
    .unwrap();
    assert_eq!(result["origin"], "registered");
    let (active, metadata): (i64, String) = f
        .store
        .db
        .query_row(
            "SELECT active,metadata FROM item_files WHERE id='curated'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(active, 1);
    assert_eq!(metadata, "{\"curated\": true}");
    assert!(
        outputs::register(
            &f.store,
            "default",
            "file",
            "derived",
            "item",
            definition["id"].as_str().unwrap(),
            &json!({})
        )
        .is_err()
    );
}
