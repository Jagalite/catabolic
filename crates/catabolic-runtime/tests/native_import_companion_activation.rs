//! Actual schema37 retained-child cleanup composed with the existing typed import.
//! No fixture-written seal, PID clearing, inferred duration, or external host audit.
#![cfg(unix)]
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility},
    catalog_host_lease::{HostAuthorization, LeaseError},
};
use catabolic_runtime::{
    helper,
    host::{CONTRACT_VERSION, HostSession},
};
use catabolic_store::{
    Store,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
    },
    catalog_identity_mutation::IdentityAuthority,
    catalog_import::{
        self as import, ActivationBinding, ImportAuthority, ImportBatch, ImportErrorCode,
        ImportMutation, ImportStep, SnapshotManifest, TrustedImportActivation,
    },
    catalog_import_typed::{self as typed, TypedAuthority},
    migration, native_observation_companion as companion, native_worker_inventory,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};
static RUN: Mutex<()> = Mutex::new(());
const HOST: &str = "closed-import-host";
fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap()
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 1000,
        max_hold_records: 1000,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}
fn authority() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn command(store: &Store, step: ImportStep, mutation: &ImportMutation) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(
            &store.database_id,
            HOST,
            import::operation_id("scanned-import", step).unwrap(),
        )
        .unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        generation(store),
        &json!({"operation":"import","mutation":mutation}).to_string(),
    )
    .unwrap()
}
fn execute(
    store: &Store,
    step: ImportStep,
    mutation: &ImportMutation,
    grants: &TypedAuthority,
    active: Option<&TrustedImportActivation>,
) -> commands::Result<CommandOutcome> {
    let grant = ImportAuthority {
        host_namespace: HOST.into(),
        import_id: "scanned-import".into(),
        source_namespace: "bounded-fixture".into(),
    };
    let mut admission = CatalogAuthority::new(authority())
        .with_imports(&grant)
        .with_import_typed(grants);
    if let Some(active) = active {
        admission = admission.with_import_activation(active);
    }
    commands::execute_with_authority(store, &command(store, step, mutation), admission, policy())
}
fn value(outcome: &CommandOutcome) -> &Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        _ => panic!("unexpected {outcome:?}"),
    }
}
struct Scanned {
    _root: tempfile::TempDir,
    path: PathBuf,
    gate: HostAuthorization,
    job: String,
    pid: i64,
    file: String,
}
fn scanned() -> Scanned {
    let binary = PathBuf::from(
        std::env::var_os("CATABOLIC_HOST_TEST_HELPER").expect("exact rebuilt37 helper required"),
    );
    // This is the already-qualified closed implementation, not an arbitrary
    // program made trusted merely by possessing a digest.
    let qualified = helper::TrustedClosedObservationArtifact::after_packaging_verification(
        &binary,
        &catabolic_core::sha256(fs::read(&binary).unwrap()),
        helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, 37).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("one.txt"), "same exact content\n").unwrap();
    fs::write(source.join("copy.txt"), "same exact content\n").unwrap();
    let stat = source.metadata().unwrap();
    let store = Store::open(&path, true, false).unwrap();
    // Explicit fresh unenrolled source fixture. No existing UUID requirement is
    // removed or disabled; production enrolled UUID policies remain unchanged.
    store
        .transaction(|db| {
            db.execute("INSERT INTO locations VALUES('media')", [])?;
            db.execute(
                "INSERT INTO bindings VALUES('default','source','media',?,?,?)",
                params![
                    source.to_str().unwrap(),
                    stat.dev() as i64,
                    stat.ino() as i64
                ],
            )?;
            Ok(())
        })
        .unwrap();
    drop(store);
    let gate = HostAuthorization::new(HOST.into(), "scan-freeze-1".into()).unwrap();
    let mut host =
        HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate.clone())
            .unwrap();
    host.register_closed_observation_helper(&qualified).unwrap();
    let handle = host.start_scan("media", ()).unwrap();
    let revoked = gate.begin_revocation().unwrap();
    assert_eq!(
        revoked.wait_drained(Duration::ZERO),
        Err(LeaseError::Timeout)
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(result) = host.poll_scan(&handle, Duration::from_millis(50)).unwrap() {
            assert_eq!(result["complete"], true);
            break;
        }
        assert!(Instant::now() < deadline);
    }
    revoked.wait_drained(Duration::ZERO).unwrap();
    host.close().unwrap();
    drop(host);
    let store = Store::open(&path, true, false).unwrap();
    let (job, pid): (String, i64) = store
        .db
        .query_row(
            "SELECT id,worker_pid FROM observation_jobs WHERE state='complete'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(companion::has_cleanup_witness(&store, &job).unwrap());
    let inventory = native_worker_inventory::inspect(&store).unwrap();
    assert_eq!(inventory.witnessed_observation, 1);
    assert!(inventory.admits_new_work());
    let file = store
        .db
        .query_row("SELECT id FROM files WHERE path='one.txt'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    drop(store);
    Scanned {
        _root: root,
        path,
        gate,
        job,
        pid,
        file,
    }
}
fn stage(store: &Store, file: &str) -> (ActivationBinding, TypedAuthority) {
    let grants = TypedAuthority {
        physical_files: [("default".into(), [file.into()].into())].into(),
        identity: Some(IdentityAuthority {
            authority_namespace: HOST.into(),
            context_keys: ["import-context".into()].into(),
            allow_declared_evidence: false,
            allow_verified_evidence: false,
        }),
        ..Default::default()
    };
    let occurrence =
        typed::capture_existing_occurrence(store, "source-occurrence", "default", file, &grants)
            .unwrap();
    // Preserve an unmeasured whole-file draft. The scan did not establish media
    // duration, readiness, interval coverage or editorial equivalence.
    let batch:ImportBatch=serde_json::from_value(json!({"items":[{"source_id":"edition-source","item_id":"edition","kind":"video","legacy_metadata":{}},{"source_id":"feature-source","item_id":"feature","kind":"video","legacy_metadata":{}}],"unresolved_evidence":[],"typed":{"metadata":[],"pins":[],"native":{"occurrences":[occurrence],"associations":[{"source_association_id":"source-association","source_occurrence_id":"source-occurrence","item_id":"feature","part":1}],"mutations":[{"contract_version":1,"profile":"default","context":{"id":"context-import","authority_namespace":HOST,"context_key":"import-context","expected_revision":null},"timelines":[{"timeline_id":"timeline-import","edition_item_id":"edition","expected_revision":null,"identity_state":"provisional","coverage":null}],"versions":[{"expected_revision":null,"expected_timeline_revision":1,"version":{"contract_version":1,"version_id":"version-import","association_item_id":"feature","edition_item_id":"edition","timeline_id":"timeline-import","state":"draft","declared_coverage":null,"bindings":[{"binding_id":"binding-import","association_id":"source-association","file":occurrence.exact,"part":1,"source":{"kind":"whole_file","duration_us":null},"timeline_interval":null}]},"equivalence":null,"expected_binding_revisions":{"binding-import":null}}],"evidence":[]}]}}})).unwrap();
    let manifest = SnapshotManifest {
        contract_version: 2,
        import_id: "scanned-import".into(),
        source_namespace: "bounded-fixture".into(),
        source_snapshot_id: "snapshot-1".into(),
        batches: vec![import::describe_batch(&batch).unwrap()],
    };
    let digest = import::manifest_digest(&manifest).unwrap();
    execute(
        store,
        ImportStep::Prepare,
        &ImportMutation::Prepare {
            manifest,
            input_digest: digest.clone(),
        },
        &grants,
        None,
    )
    .unwrap();
    execute(
        store,
        ImportStep::Batch(0),
        &ImportMutation::Stage {
            import_id: "scanned-import".into(),
            input_digest: digest.clone(),
            batch_index: 0,
            batch,
        },
        &grants,
        None,
    )
    .unwrap();
    let generation_before = generation(store);
    let validated = execute(
        store,
        ImportStep::Validate,
        &ImportMutation::Validate {
            import_id: "scanned-import".into(),
            input_digest: digest.clone(),
        },
        &grants,
        None,
    )
    .unwrap();
    assert_eq!(generation(store), generation_before);
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM items", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    (
        ActivationBinding {
            catalog_id: store.database_id.clone(),
            incarnation: store.catalog_incarnation.clone().unwrap(),
            import_id: "scanned-import".into(),
            input_digest: digest,
            source_snapshot_id: "snapshot-1".into(),
            validation_generation: generation(store),
            reference_ack_id: "bounded-fixture-audit".into(),
            mapping_digest: value(&validated)["mapping_digest"].as_str().unwrap().into(),
        },
        grants,
    )
}
fn activation(binding: &ActivationBinding) -> ImportMutation {
    ImportMutation::Activate {
        import_id: binding.import_id.clone(),
        input_digest: binding.input_digest.clone(),
        reference_ack_id: binding.reference_ack_id.clone(),
        mapping_digest: binding.mapping_digest.clone(),
    }
}
#[test]
#[ignore = "requires exact rebuilt37 CATABOLIC_HOST_TEST_HELPER; run explicitly"]
fn native37_actual_scanned_occurrence_activates_draft_identity_without_clearing_owner() {
    let _serial = RUN.lock().unwrap();
    let f = scanned();
    let store = Store::open(&f.path, true, false).unwrap();
    let (binding, grants) = stage(&store, &f.file);
    let active = TrustedImportActivation::after_drain(
        &f.gate,
        f.gate.begin_revocation().unwrap(),
        binding.clone(),
    )
    .unwrap();
    let mutation = activation(&binding);
    let intent = command(&store, ImportStep::Activate, &mutation);
    let outcome = execute(
        &store,
        ImportStep::Activate,
        &mutation,
        &grants,
        Some(&active),
    )
    .unwrap();
    assert_eq!(outcome.code(), "committed");
    let loaded = catabolic_store::catalog_identity_mutation::load_manifest(
        &store,
        "default",
        &["version-import".into()],
    )
    .unwrap();
    assert_eq!(loaded.versions.len(), 1);
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT worker_pid FROM observation_jobs WHERE id=?",
                [&f.job],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        f.pid
    );
    assert!(companion::has_cleanup_witness(&store, &f.job).unwrap());
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    drop(store);
    let store = Store::open(&f.path, true, false).unwrap();
    let replay = commands::execute(
        &store,
        &intent,
        TrustedCommandAuthority {
            allow_new_commands: false,
            ..authority()
        },
        policy(),
    )
    .unwrap();
    assert_eq!(replay.code(), "replay");
    assert_eq!(value(&outcome), value(&replay));
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM catalog_bindings", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        f.gate.acquire(f.gate.revision()).unwrap_err(),
        LeaseError::Revoked
    );
}
#[test]
#[ignore = "requires exact rebuilt37 CATABOLIC_HOST_TEST_HELPER; run explicitly"]
fn native37_observation_witness_never_waives_changed_claim_or_unrelated_work() {
    let _serial = RUN.lock().unwrap();
    for failure in [
        "changed-claim",
        "pending-processing",
        "terminal-processing",
        "unknown-api-owner",
    ] {
        let f = scanned();
        let store = Store::open(&f.path, true, false).unwrap();
        store.transaction(|db|{match failure {
            "changed-claim"=>{db.execute("UPDATE observation_jobs SET claim_token=? WHERE id=?",params![uuid::Uuid::new_v4().to_string(),f.job])?;}
            "pending-processing"|"terminal-processing"=>{db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,attempts) VALUES('unrelated','default',?,'probe','media','{}','{}','unrelated',?,?)",params![f.file,if failure=="pending-processing"{"queued"}else{"complete"},if failure=="pending-processing"{0}else{1}])?;}
            "unknown-api-owner"=>{db.execute("INSERT INTO api_worker_status VALUES('default',99999,'{}',0)",[])?;}
            _=>unreachable!()
        }Ok(())}).unwrap();
        assert_eq!(
            companion::has_cleanup_witness(&store, &f.job).unwrap(),
            failure != "changed-claim"
        );
        let (binding, grants) = stage(&store, &f.file);
        let active = TrustedImportActivation::after_drain(
            &f.gate,
            f.gate.begin_revocation().unwrap(),
            binding.clone(),
        )
        .unwrap();
        assert_eq!(
            execute(
                &store,
                ImportStep::Activate,
                &activation(&binding),
                &grants,
                Some(&active)
            )
            .unwrap_err()
            .code,
            commands::CommandStoreErrorCode::Import(ImportErrorCode::RecoveryRequired),
            "{failure}"
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM items", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .db
                .query_row(
                    "SELECT worker_pid FROM observation_jobs WHERE id=?",
                    [&f.job],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            f.pid
        );
    }
}
