//! Receipt-owned admission with real source and generated-root identities.
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
use catabolic_store::{
    Store, artifact_mutation,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
    },
    catalog_processing::{EnqueueRequest, ProcessingEnqueueAuthority},
    media_mutation, migration, public_components,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt, time::Duration};
struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    request: EnqueueRequest,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("native.db");
        migration::initialize_native_at(&db, 39).unwrap();
        let store = Store::open(&db, true, false).unwrap();
        let input = dir.path().join("input");
        let output = dir.path().join("output");
        fs::create_dir(&input).unwrap();
        fs::create_dir(&output).unwrap();
        fs::write(input.join("input.wav"), b"source evidence").unwrap();
        let r = input.metadata().unwrap();
        let st = input.join("input.wav").metadata().unwrap();
        store.transaction(|db| {
            db.execute("INSERT INTO locations VALUES ('input')",[])?;
            db.execute("INSERT INTO bindings VALUES ('default','source','input',?,?,?)",params![input.to_str(),r.dev() as i64,r.ino() as i64])?;
            db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','input',1,1,'[]')",[])?;
            db.execute("INSERT INTO files VALUES ('file','input','input.wav')",[])?;
            db.execute("INSERT INTO observations VALUES ('default','file',?,?,?,?,'present','scan')",params![st.len() as i64,st.mtime()*1_000_000_000+st.mtime_nsec(),st.dev() as i64,st.ino() as i64])?;
            db.execute("INSERT INTO items VALUES ('item','recording','{}')",[])?;
            db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('association','file','item','primary','explicit')",[])?;Ok(())
        }).unwrap();
        let binding =
            artifact_mutation::bind(&store, "default", "generated", output.to_str().unwrap())
                .unwrap();
        let recipe=artifact_mutation::recipe(&store,"audio",&json!({"version":1,"preset":"audio-flac","timeout":3600,"reserve_bytes":0,"max_output_bytes":1048576}),"primary",None).unwrap();
        let snapshot = media_mutation::snapshot(&store, "default", "file").unwrap();
        let request = EnqueueRequest {
            profile: "default".into(),
            source: catabolic_core::catalog_identity::ExactFileRevision {
                file_id: "file".into(),
                revision: public_components::revision(&snapshot).unwrap(),
            },
            source_ctime_ns: st.ctime() * 1_000_000_000 + st.ctime_nsec(),
            item_id: "item".into(),
            recipe_id: recipe["id"].as_str().unwrap().into(),
            recipe_digest: recipe["digest"].as_str().unwrap().into(),
            output_location: "generated".into(),
            output_binding: binding,
            capabilities: json!({"presets":[{"name":"audio-flac","available":true}],"tools":{}}),
        };
        Self {
            _dir: dir,
            store,
            request,
        }
    }
    fn generation(&self) -> i64 {
        self.store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn envelope(&self, key: &str, request: &EnqueueRequest) -> CommandEnvelope {
        CommandEnvelope::new(
            1,
            CommandKey::new(&self.store.database_id, "processing-test", key).unwrap(),
            self.store.catalog_incarnation.as_deref().unwrap(),
            self.generation(),
            &json!({"operation":"processing_enqueue","request":request}).to_string(),
        )
        .unwrap()
    }
    fn grant(&self) -> ProcessingEnqueueAuthority {
        ProcessingEnqueueAuthority {
            host_namespace: "processing-test".into(),
            request: self.request.clone(),
        }
    }
    fn count(&self, table: &str) -> i64 {
        self.store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}
fn base() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: "processing-test",
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 1 << 20,
    }
}
fn result(outcome: &CommandOutcome) -> &Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        _ => panic!("not committed"),
    }
}
#[test]
fn processing_enqueue_receipt_replay_conflict_and_atomic_rollback() {
    let f = Fixture::new();
    let grant = f.grant();
    let intent = f.envelope("one", &f.request);
    let committed = commands::execute_with_authority(
        &f.store,
        &intent,
        CatalogAuthority::new(base()).with_processing(&grant),
        policy(),
    )
    .unwrap();
    assert_eq!(committed.code(), "committed");
    assert_eq!(f.count("processing_jobs"), 1);
    assert_eq!(f.count("processing_artifacts"), 0);
    let replay = commands::execute_with_authority(
        &f.store,
        &intent,
        CatalogAuthority::new(base()),
        policy(),
    )
    .unwrap();
    assert_eq!(replay.code(), "replay");
    assert_eq!(result(&replay), result(&committed));
    assert_eq!(f.count("processing_jobs"), 1);
    let mut changed = f.request.clone();
    changed.item_id = "different".into();
    let conflict = f.envelope("one", &changed);
    assert_eq!(
        commands::execute_with_authority(
            &f.store,
            &conflict,
            CatalogAuthority::new(base()),
            policy()
        )
        .unwrap()
        .code(),
        "conflict"
    );
    let f = Fixture::new();
    let grant = f.grant();
    let intent = f.envelope("rollback", &f.request);
    f.store.db.execute_batch("CREATE TEMP TRIGGER reject_receipt BEFORE INSERT ON catalog_command_receipts BEGIN SELECT RAISE(ABORT,'test receipt failure'); END;").unwrap();
    assert!(
        commands::execute_with_authority(
            &f.store,
            &intent,
            CatalogAuthority::new(base()).with_processing(&grant),
            policy()
        )
        .is_err()
    );
    assert_eq!(f.count("processing_jobs"), 0);
    assert_eq!(f.count("catalog_command_receipts"), 0);
}
#[test]
fn processing_enqueue_refuses_stale_or_ungranted_exact_facts() {
    for failure in [
        "grant",
        "source",
        "ctime",
        "recipe",
        "binding",
        "capabilities",
    ] {
        let f = Fixture::new();
        let mut request = f.request.clone();
        let mut grant = f.grant();
        match failure {
            "grant" => grant.host_namespace = "wrong".into(),
            "source" => request.source.revision = "wrong".into(),
            "ctime" => request.source_ctime_ns += 1,
            "recipe" => request.recipe_digest = "wrong".into(),
            "binding" => request.output_binding["root"] = json!("/wrong"),
            "capabilities" => request.capabilities["presets"][0]["available"] = json!(false),
            _ => unreachable!(),
        }
        if failure != "grant" {
            grant.request = request.clone();
        }
        let intent = f.envelope(failure, &request);
        assert!(
            commands::execute_with_authority(
                &f.store,
                &intent,
                CatalogAuthority::new(base()).with_processing(&grant),
                policy()
            )
            .is_err(),
            "{failure}"
        );
        assert_eq!(f.count("processing_jobs"), 0);
    }
    let f = Fixture::new();
    let intent = f.envelope("stale", &f.request);
    let grant = f.grant();
    catabolic_store::curation::item_put(
        &f.store,
        &json!({"id":"unrelated","kind":"video","metadata":{}}),
    )
    .unwrap();
    assert_eq!(
        commands::execute_with_authority(
            &f.store,
            &intent,
            CatalogAuthority::new(base()).with_processing(&grant),
            policy()
        )
        .unwrap()
        .code(),
        "stale_generation"
    );
    assert_eq!(f.count("processing_jobs"), 0);
}
