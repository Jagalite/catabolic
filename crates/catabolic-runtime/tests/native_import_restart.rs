//! Persisted trusted-host checkpoint/process-exit fixtures for import restart.
//! Fixture persistence is not a Motion checkpoint implementation or restore proof.
#![cfg(unix)]
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility},
    catalog_host_lease::HostAuthorization,
    catalog_metadata::SourceKind,
};
use catabolic_runtime::{
    helper,
    host::{
        CONTRACT_VERSION, Cancellation, ErrorCode, HostSession, IMPORT_RESTART_CONTRACT_VERSION,
        ImportRestartAdmission, QueryId, ReadBudget as HostReadBudget, RecordedImportHandshake,
    },
};
use catabolic_store::{
    Store,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
        TrustedReceiptMaintenance,
    },
    catalog_import::{
        self as import, ActivationBinding, HostCutoverCheckpoint, HostCutoverPhase,
        ImportAuthority, ImportBatch, ImportItem, ImportMutation, ImportStep, Reconciliation,
        SnapshotManifest, TrustedImportActivation,
    },
    catalog_metadata_store::{
        MetadataAccess, MetadataAuthority, MetadataChange, MetadataMutation, PolicyField,
        PolicyNormalization, PolicyValueType,
    },
    catalog_scope::{
        ReadBudget, SavedQueryRef, ScopeReadRequest, ScopedReadSession, TrustedHostReadGrant,
        TrustedSelection,
    },
    catalog_write::{
        MetadataPatchRequest, PrivateCatalogWriter, TrustedMetadataWriteGrant, WriteBudget,
    },
    curation, definitions, migration, native_observation_companion as companion,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
const HOST: &str = "composition-host";
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::ZERO,
        max_receipts: 1000,
        max_hold_records: 1000,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}
fn base() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap()
}
fn command(store: &Store, key: &str, payload: Value) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, HOST, key).unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        generation(store),
        &payload.to_string(),
    )
    .unwrap()
}
fn value(outcome: CommandOutcome) -> Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        _ => panic!("unexpected {outcome:?}"),
    }
}
fn import_grant() -> ImportAuthority {
    ImportAuthority {
        host_namespace: HOST.into(),
        import_id: "composition-import".into(),
        source_namespace: "bounded-fixture".into(),
    }
}
fn run_import(
    store: &Store,
    step: ImportStep,
    mutation: ImportMutation,
    active: Option<&TrustedImportActivation>,
) -> commands::Result<CommandOutcome> {
    let grant = import_grant();
    let mut authority = CatalogAuthority::new(base()).with_imports(&grant);
    if let Some(active) = active {
        authority = authority.with_import_activation(active);
    }
    commands::execute_with_authority(
        store,
        &command(
            store,
            &import::operation_id("composition-import", step).unwrap(),
            json!({"operation":"import","mutation":mutation}),
        ),
        authority,
        policy(),
    )
}
fn stage(store: &Store) -> ActivationBinding {
    let batch = ImportBatch {
        items: vec![ImportItem {
            source_id: "source-imported".into(),
            item_id: "imported".into(),
            kind: "video".into(),
            legacy_metadata: json!({}),
        }],
        unresolved_evidence: vec![],
        typed: None,
    };
    let manifest = SnapshotManifest {
        contract_version: 1,
        import_id: "composition-import".into(),
        source_namespace: "bounded-fixture".into(),
        source_snapshot_id: "snapshot-1".into(),
        batches: vec![import::describe_batch(&batch).unwrap()],
    };
    let digest = import::manifest_digest(&manifest).unwrap();
    run_import(
        store,
        ImportStep::Prepare,
        ImportMutation::Prepare {
            manifest,
            input_digest: digest.clone(),
        },
        None,
    )
    .unwrap();
    run_import(
        store,
        ImportStep::Batch(0),
        ImportMutation::Stage {
            import_id: "composition-import".into(),
            input_digest: digest.clone(),
            batch_index: 0,
            batch,
        },
        None,
    )
    .unwrap();
    let validated = value(
        run_import(
            store,
            ImportStep::Validate,
            ImportMutation::Validate {
                import_id: "composition-import".into(),
                input_digest: digest.clone(),
            },
            None,
        )
        .unwrap(),
    );
    ActivationBinding {
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone().unwrap(),
        import_id: "composition-import".into(),
        input_digest: digest,
        source_snapshot_id: "snapshot-1".into(),
        validation_generation: generation(store),
        reference_ack_id: "bounded-fixture-audit".into(),
        mapping_digest: validated["mapping_digest"].as_str().unwrap().into(),
    }
}
fn activate(
    store: &Store,
    gate: &HostAuthorization,
    binding: &ActivationBinding,
) -> commands::Result<CommandOutcome> {
    let active = TrustedImportActivation::after_drain(
        gate,
        gate.begin_revocation().unwrap(),
        binding.clone(),
    )
    .unwrap();
    run_import(
        store,
        ImportStep::Activate,
        ImportMutation::Activate {
            import_id: binding.import_id.clone(),
            input_digest: binding.input_digest.clone(),
            reference_ack_id: binding.reference_ack_id.clone(),
            mapping_digest: binding.mapping_digest.clone(),
        },
        Some(&active),
    )
}

struct Fixture {
    root: tempfile::TempDir,
    path: PathBuf,
    query: SavedQueryRef,
    query_id: String,
    fingerprint: String,
}
fn setup(schema: u32) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, schema).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("one.txt"), "bounded restart source\n").unwrap();
    let stat = source.metadata().unwrap();
    let store = Store::open(&path, true, false).unwrap();
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
    curation::item_put(
        &store,
        &json!({"id":"editable","kind":"video","metadata":{}}),
    )
    .unwrap();
    let mutation = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Initialize {
            subject: "editable".into(),
            policies: vec![PolicyField {
                field: "title".into(),
                value_type: PolicyValueType::String,
                nullable: true,
                normalization: PolicyNormalization::Exact,
            }],
        },
    };
    let grant = MetadataAuthority {
        host_namespace: HOST.into(),
        subject: "editable".into(),
        access: MetadataAccess::Initialize,
    };
    commands::execute_with_authority(
        &store,
        &command(
            &store,
            "metadata-init",
            json!({"operation":"metadata","mutation":mutation}),
        ),
        CatalogAuthority::new(base()).with_metadata(&grant),
        policy(),
    )
    .unwrap();
    let row=definitions::query_put(&store,"default","editable",&json!({"version":1,"mode":"selection","selection":{"language":"sql","query":"SELECT item_id FROM catalog_items WHERE item_id='editable'"}})).unwrap();
    let query_id = row["id"].as_str().unwrap().to_owned();
    let query = SavedQueryRef::new(
        row["id"].as_str().unwrap().into(),
        row["revision"].as_i64().unwrap(),
        row["digest"].as_str().unwrap().into(),
    )
    .unwrap();
    let fingerprint = store
        .db
        .query_row(
            "SELECT schema_fingerprint FROM catalog_metadata_subjects WHERE item_id='editable'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    Fixture {
        root,
        path,
        query,
        query_id,
        fingerprint,
    }
}
fn activation_intent(store: &Store, binding: &ActivationBinding) -> CommandEnvelope {
    command(
        store,
        &import::operation_id("composition-import", ImportStep::Activate).unwrap(),
        json!({"operation":"import","mutation":ImportMutation::Activate {
        import_id:binding.import_id.clone(), input_digest:binding.input_digest.clone(),
        reference_ack_id:binding.reference_ack_id.clone(),mapping_digest:binding.mapping_digest.clone()}}),
    )
}
fn record(path: &Path, intent: &CommandEnvelope, binding: &ActivationBinding, phase: &str) {
    let record = json!({"version":1,"phase":phase,"intent":{
        "version":intent.contract_version(),"catalog_id":intent.key().catalog_id(),
        "host_namespace":intent.key().host_namespace(),"operation_id":intent.key().operation_id(),
        "incarnation":intent.admission_incarnation(),"generation":intent.expected_generation(),
        "payload":intent.payload().to_string(),"envelope_digest":intent.digest().as_str()},"checkpoint":{
        "catalog_id":binding.catalog_id,"incarnation":binding.incarnation,"import_id":binding.import_id,
        "input_digest":binding.input_digest,"reference_ack_id":binding.reference_ack_id,
        "mapping_digest":binding.mapping_digest}});
    let tmp = path.with_extension("pending");
    let mut file = fs::File::create(&tmp).unwrap();
    file.write_all(&serde_json::to_vec(&record).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    fs::rename(&tmp, path).unwrap();
    fs::File::open(path.parent().unwrap())
        .unwrap()
        .sync_all()
        .unwrap();
}
fn recorded(path: &Path) -> RecordedImportHandshake {
    let raw: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(raw["version"], 1);
    let i = &raw["intent"];
    let c = &raw["checkpoint"];
    let s = |v: &Value| v.as_str().unwrap().to_owned();
    let restored = RecordedImportHandshake {
        activation_intent: CommandEnvelope::new(
            i["version"].as_u64().unwrap() as u32,
            CommandKey::new(
                s(&i["catalog_id"]),
                s(&i["host_namespace"]),
                s(&i["operation_id"]),
            )
            .unwrap(),
            s(&i["incarnation"]),
            i["generation"].as_i64().unwrap(),
            i["payload"].as_str().unwrap(),
        )
        .unwrap(),
        checkpoint: HostCutoverCheckpoint {
            catalog_id: s(&c["catalog_id"]),
            incarnation: s(&c["incarnation"]),
            phase: match raw["phase"].as_str().unwrap() {
                "prepared" => HostCutoverPhase::Prepared,
                "validated" => HostCutoverPhase::Validated,
                "activated" => HostCutoverPhase::Activated,
                _ => panic!("unknown phase"),
            },
            import_id: s(&c["import_id"]),
            input_digest: s(&c["input_digest"]),
            reference_ack_id: s(&c["reference_ack_id"]),
            mapping_digest: s(&c["mapping_digest"]),
        },
    };
    assert_eq!(
        restored.activation_intent.digest().as_str(),
        i["envelope_digest"].as_str().unwrap()
    );
    restored
}
fn reopen(
    f: &Fixture,
    record: &RecordedImportHandshake,
    authority: TrustedCommandAuthority<'_>,
) -> catabolic_runtime::host::Result<ImportRestartAdmission> {
    HostSession::open_after_import_cutover(
        &f.path,
        "default",
        IMPORT_RESTART_CONTRACT_VERSION,
        record,
        authority,
        "fresh-restart-gate".into(),
        policy(),
    )
}
fn record_path(f: &Fixture) -> PathBuf {
    f.root.path().join("host-checkpoint.json")
}
fn crash(f: &Fixture, mode: &str, real_scan: bool) -> RecordedImportHandshake {
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "persisted_restart_exit_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("CATABOLIC_RESTART_DB", &f.path)
        .env("CATABOLIC_RESTART_RECORD", record_path(f))
        .env("CATABOLIC_RESTART_MODE", mode)
        .env(
            "CATABOLIC_RESTART_REAL_SCAN",
            if real_scan { "1" } else { "0" },
        )
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(77), "process did not reach {mode}");
    recorded(&record_path(f))
}
fn snapshot(path: &Path) -> Value {
    let store = Store::open(path, false, false).unwrap();
    let count = |table: &str| {
        store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
    };
    json!({"generation":generation(&store),"items":count("items"),"receipts":count("catalog_command_receipts"),"holds":count("catalog_receipt_holds"),"bindings":count("catalog_bindings")})
}
fn assert_closed(
    outcome: catabolic_runtime::host::Result<ImportRestartAdmission>,
    expected: Option<Reconciliation>,
) {
    match outcome {
        Ok(mut admission) => {
            if let Some(expected) = expected {
                assert_eq!(admission.decision(), expected);
            }
            // All three operation families require the withheld HostSession.
            for operation in ["bounded read", "bounded write", "scan"] {
                assert_eq!(
                    admission.admitted_host().err().unwrap().code,
                    ErrorCode::RecoveryRequired,
                    "{operation}"
                );
            }
        }
        Err(error) => {
            assert!(
                expected.is_none(),
                "unexpected reconciliation error: {error:?}"
            );
            assert_eq!(error.code, ErrorCode::RecoveryRequired);
        }
    }
}
fn qualified_helper() -> helper::TrustedClosedObservationArtifact {
    let binary = PathBuf::from(
        std::env::var_os("CATABOLIC_HOST_TEST_HELPER").expect("exact rebuilt37 helper required"),
    );
    helper::TrustedClosedObservationArtifact::after_packaging_verification(
        &binary,
        &catabolic_core::sha256(fs::read(&binary).unwrap()),
        helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
    )
    .unwrap()
}
fn scan_to_seal(path: &Path, gate: &HostAuthorization) {
    let mut host =
        HostSession::open_authorized(path, "default", true, CONTRACT_VERSION, gate.clone())
            .unwrap();
    host.register_closed_observation_helper(&qualified_helper())
        .unwrap();
    let handle = host.start_scan("media", ()).unwrap();
    let closed = gate.begin_revocation().unwrap();
    assert!(closed.wait_drained(Duration::ZERO).is_err());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(result) = host.poll_scan(&handle, Duration::from_millis(50)).unwrap() {
            assert_eq!(result["complete"], true);
            break;
        }
        assert!(Instant::now() < deadline);
    }
    closed.wait_drained(Duration::ZERO).unwrap();
    host.close().unwrap();
    drop(host);
    let store = Store::open(path, false, false).unwrap();
    let job: String = store
        .db
        .query_row(
            "SELECT id FROM observation_jobs ORDER BY rowid DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(companion::has_cleanup_witness(&store, &job).unwrap());
    assert!(
        catabolic_store::native_worker_inventory::inspect(&store)
            .unwrap()
            .admits_new_work()
    );
}
#[test]
#[ignore = "real process-exit helper; executed by wrappers"]
fn persisted_restart_exit_helper() {
    let path = PathBuf::from(std::env::var_os("CATABOLIC_RESTART_DB").unwrap());
    let target = PathBuf::from(std::env::var_os("CATABOLIC_RESTART_RECORD").unwrap());
    let mode = std::env::var("CATABOLIC_RESTART_MODE").unwrap();
    let real_scan = std::env::var("CATABOLIC_RESTART_REAL_SCAN").unwrap() == "1";
    let gate = HostAuthorization::new(HOST.into(), "pre-crash-gate".into()).unwrap();
    // This edge retains an already-recorded original intent, then loses the
    // process immediately after a real seal; it cannot imply activation.
    if mode == "after-seal" {
        let store = Store::open(&path, true, false).unwrap();
        let binding = stage(&store);
        let intent = activation_intent(&store, &binding);
        record(&target, &intent, &binding, "prepared");
        drop(store);
        scan_to_seal(&path, &gate);
        std::process::exit(77);
    }
    if real_scan {
        scan_to_seal(&path, &gate);
    }
    let store = Store::open(&path, true, false).unwrap();
    let binding = stage(&store);
    let intent = activation_intent(&store, &binding);
    record(&target, &intent, &binding, "validated");
    if mode == "before-activation" {
        std::process::exit(77);
    }
    assert_eq!(
        activate(&store, &gate, &binding).unwrap().code(),
        "committed"
    );
    if mode == "after-host-checkpoint" {
        record(&target, &intent, &binding, "activated");
    }
    std::process::exit(77);
}
fn exercise_bounded_host(f: &Fixture, mut admission: ImportRestartAdmission) {
    assert_eq!(admission.decision(), Reconciliation::AdmitCatabolicOnly);
    let host = admission.admitted_host().unwrap();
    let query = QueryId::new(f.query_id.clone()).unwrap();
    let budget = HostReadBudget {
        timeout: Duration::from_secs(5),
        maximum_ids: 16,
    };
    assert_eq!(
        host.select(&query, budget, &Cancellation::default())
            .unwrap()["ids"],
        json!(["editable"])
    );
    assert_eq!(
        host.put_items(
            &[json!({"id":"post-restart","kind":"video","metadata":{}})],
            &Cancellation::default()
        )
        .unwrap()
        .len(),
        1
    );
    // Older schemas use the pre-existing retained-child contract. This is
    // explicitly controlled-child evidence, not an exact companion seal.
    static TOOLS: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let binary = TOOLS
        .get_or_init(|| tempfile::tempdir().unwrap())
        .path()
        .join("controlled-scan");
    if !binary.exists() {
        fs::write(&binary, "#!/bin/sh\nprintf '{\"complete\":true}'\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    }
    host.register_helper(&binary, &catabolic_core::sha256(fs::read(&binary).unwrap()))
        .unwrap();
    let handle = host.start_scan("media", ()).unwrap();
    let gate = host.authorization().unwrap();
    let revoked = gate.begin_revocation().unwrap();
    assert!(revoked.wait_drained(Duration::ZERO).is_err());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(result) = host.poll_scan(&handle, Duration::from_millis(50)).unwrap() {
            assert_eq!(result["complete"], true);
            break;
        }
        assert!(Instant::now() < deadline);
    }
    revoked.wait_drained(Duration::ZERO).unwrap();
    assert_eq!(
        host.select(&query, budget, &Cancellation::default())
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        host.put_items(
            &[json!({"id":"refused","kind":"video","metadata":{}})],
            &Cancellation::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        host.start_scan("media", ()).unwrap_err().code,
        ErrorCode::Conflict
    );
    host.close().unwrap();
}
#[test]
fn persisted_process_exit_reopens_only_after_both_owners_agree_native34_through37() {
    for schema in 34..=37 {
        for mode in [
            "before-activation",
            "after-activation",
            "after-host-checkpoint",
        ] {
            let f = setup(schema);
            let record = crash(&f, mode, false);
            let before = snapshot(&f.path);

            let outcome = reopen(&f, &record, base());
            if mode == "after-host-checkpoint" {
                let mut admitted = outcome.unwrap();
                assert_eq!(admitted.decision(), Reconciliation::AdmitCatabolicOnly);
                assert_eq!(
                    admitted.admitted_host().unwrap().identity().unwrap()["schema_version"],
                    schema
                );
                assert_eq!(snapshot(&f.path), before);
                if schema < 37 {
                    exercise_bounded_host(&f, admitted);
                } else {
                    admitted.admitted_host().unwrap().close().unwrap();
                }
            } else {
                assert_closed(
                    outcome,
                    if mode == "after-activation" {
                        Some(Reconciliation::FinishHostActivation)
                    } else {
                        None
                    },
                );
                assert_eq!(snapshot(&f.path), before);
                if mode == "before-activation" {
                    let mut false_claim = record.clone();
                    false_claim.checkpoint.phase = HostCutoverPhase::Activated;
                    assert_closed(reopen(&f, &false_claim, base()), None);
                    assert_eq!(
                        snapshot(&f.path),
                        before,
                        "restart must never execute absent activation even when allow_new_commands=true"
                    );
                }
            }
        }
    }
}
#[test]
fn restart_disagreement_visibility_expiry_and_restore_uncertainty_stay_closed() {
    for schema in 34..=37 {
        let f = setup(schema);
        let original = crash(&f, "after-host-checkpoint", false);
        let before = snapshot(&f.path);
        for mismatch in [
            "catalog",
            "incarnation",
            "import",
            "input",
            "ack",
            "mapping",
            "prepared",
            "intent-generation",
            "intent-operation",
        ] {
            let mut r = original.clone();
            match mismatch {
                "catalog" => r.checkpoint.catalog_id = "different-catalog".into(),
                "incarnation" => r.checkpoint.incarnation = "different-incarnation".into(),
                "import" => r.checkpoint.import_id = "different-import".into(),
                "input" => r.checkpoint.input_digest = "0".repeat(64),
                "ack" => r.checkpoint.reference_ack_id = "different-ack".into(),
                "mapping" => r.checkpoint.mapping_digest = "0".repeat(64),
                "prepared" => r.checkpoint.phase = HostCutoverPhase::Prepared,
                "intent-generation" => {
                    r.activation_intent = CommandEnvelope::new(
                        1,
                        r.activation_intent.key().clone(),
                        r.activation_intent.admission_incarnation(),
                        r.activation_intent.expected_generation() + 1,
                        &r.activation_intent.payload().to_string(),
                    )
                    .unwrap()
                }
                "intent-operation" => {
                    r.activation_intent = CommandEnvelope::new(
                        1,
                        CommandKey::new(r.activation_intent.key().catalog_id(), HOST, "new-key")
                            .unwrap(),
                        r.activation_intent.admission_incarnation(),
                        r.activation_intent.expected_generation(),
                        &r.activation_intent.payload().to_string(),
                    )
                    .unwrap()
                }
                _ => unreachable!(),
            }
            assert_closed(reopen(&f, &r, base()), None);
            assert_eq!(snapshot(&f.path), before, "{schema}/{mismatch}");
        }
        assert_closed(
            reopen(
                &f,
                &original,
                TrustedCommandAuthority {
                    receipt_visibility: ReceiptVisibility::CommittedStatusOnly,
                    ..base()
                },
            ),
            Some(Reconciliation::KeepAdmissionClosed),
        );
        assert_closed(
            reopen(
                &f,
                &original,
                TrustedCommandAuthority {
                    host_namespace: "wrong-host",
                    ..base()
                },
            ),
            None,
        );
        assert_eq!(snapshot(&f.path), before);
        let store = Store::open(&f.path, true, false).unwrap();
        let maintenance = TrustedReceiptMaintenance {
            host_namespace: HOST,
            holder_namespace: import::IMPORT_HOLDER_NAMESPACE,
        };
        commands::release_hold(
            &store,
            original.activation_intent.key(),
            maintenance,
            &catabolic_core::sha256("composition-import"),
        )
        .unwrap();
        assert!(commands::compact_receipts(&store, maintenance, 32).unwrap() > 0);
        drop(store);
        assert_closed(
            reopen(&f, &original, base()),
            Some(Reconciliation::KeepAdmissionClosed),
        );
        // Fixture-only incarnation rotation, not actual restore publication.
        for mode in ["before-activation", "after-host-checkpoint"] {
            let f = setup(schema);
            let original = crash(&f, mode, false);
            let db = catabolic_store::connect(&f.path, true).unwrap();
            db.execute_batch("BEGIN IMMEDIATE; INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) SELECT 'restore',catalog_id,incarnation FROM catalog_state; UPDATE catalog_state SET incarnation=lower(hex(randomblob(16))); DELETE FROM catalog_mutation_permits; COMMIT;").unwrap();
            drop(db);
            let before = snapshot(&f.path);
            assert_closed(
                reopen(&f, &original, base()),
                Some(Reconciliation::KeepAdmissionClosed),
            );
            assert_eq!(snapshot(&f.path), before);
        }
    }
}
fn writer_grant(f: &Fixture, gate: &HostAuthorization) -> TrustedMetadataWriteGrant {
    TrustedMetadataWriteGrant::new(
        gate.clone(),
        "default".into(),
        vec![f.query.clone()],
        MetadataAuthority {
            host_namespace: HOST.into(),
            subject: "editable".into(),
            access: MetadataAccess::Source {
                namespace: "editorial".into(),
                record_id: "one".into(),
                kind: SourceKind::Local,
            },
        },
        vec!["title".into()],
        ReceiptVisibility::FullResult,
    )
    .unwrap()
}
#[test]
#[ignore = "requires exact rebuilt37 CATABOLIC_HOST_TEST_HELPER; run explicitly"]
fn native37_real_seal_and_persisted_handoff_admit_shared_reader_writer_scanner_only_after_ack() {
    for mode in [
        "after-seal",
        "before-activation",
        "after-activation",
        "after-host-checkpoint",
    ] {
        let f = setup(37);
        let record = crash(&f, mode, true);
        let before = snapshot(&f.path);
        let store = Store::open(&f.path, false, false).unwrap();
        let (job, pid): (String, i64) = store
            .db
            .query_row("SELECT id,worker_pid FROM observation_jobs", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(companion::has_cleanup_witness(&store, &job).unwrap());
        drop(store);
        let result = reopen(&f, &record, base());
        if mode != "after-host-checkpoint" {
            assert_closed(
                result,
                if mode == "after-activation" {
                    Some(Reconciliation::FinishHostActivation)
                } else {
                    None
                },
            );
            assert_eq!(snapshot(&f.path), before);
            continue;
        }
        let mut admission = result.unwrap();
        assert_eq!(admission.decision(), Reconciliation::AdmitCatabolicOnly);
        let host = admission.admitted_host().unwrap();
        let query = QueryId::new(f.query_id.clone()).unwrap();
        let budget = HostReadBudget {
            timeout: Duration::from_secs(5),
            maximum_ids: 16,
        };
        assert_eq!(
            host.select(&query, budget, &Cancellation::default())
                .unwrap()["ids"],
            json!(["editable"])
        );
        assert_eq!(
            host.put_items(
                &[json!({"id":"post-restart","kind":"video","metadata":{}})],
                &Cancellation::default()
            )
            .unwrap()
            .len(),
            1
        );
        let gate = host.authorization().unwrap();
        host.close().unwrap();
        let mut writer = PrivateCatalogWriter::open(&f.path).unwrap();
        let lease = gate.acquire(gate.revision()).unwrap();
        let handle = writer
            .admit_metadata_write(
                writer_grant(&f, &gate),
                &lease,
                ReadBudget::new(Duration::from_secs(5), 1000).unwrap(),
            )
            .unwrap();
        let write_result = writer
            .execute_metadata_patch(
                &handle,
                lease,
                MetadataPatchRequest {
                    operation_id: "restart-private-write".into(),
                    schema_fingerprint: f.fingerprint.clone(),
                    expected_revision: 0,
                    edits_json: r#"{"title":{"op":"set","value":"After persisted restart"}}"#
                        .into(),
                    evidence_json: "{}".into(),
                },
                policy(),
                WriteBudget::new(
                    Duration::from_secs(10),
                    10000,
                    100000,
                    16 * 1024 * 1024,
                    1000000,
                )
                .unwrap(),
            )
            .unwrap();
        drop(writer);
        let mut reader = ScopedReadSession::open(&f.path).unwrap();
        let read_lease = gate.acquire(gate.revision()).unwrap();
        let scope = reader
            .admit_host_scope(
                TrustedHostReadGrant::new(
                    gate.clone(),
                    "default".into(),
                    vec![TrustedSelection::new(f.query.clone(), vec!["title".into()]).unwrap()],
                    vec![],
                )
                .unwrap(),
                &read_lease,
                ReadBudget::new(Duration::from_secs(5), 1000).unwrap(),
            )
            .unwrap();
        let read_result = reader
            .read_host_scope(
                &scope,
                read_lease,
                ScopeReadRequest::GetItem("editable".into()),
                ReadBudget::new(Duration::from_secs(5), 1000).unwrap(),
            )
            .unwrap();
        drop(reader);
        // Continue with the same admitted gate; this is not a second restart.
        let mut host =
            HostSession::open_authorized(&f.path, "default", true, CONTRACT_VERSION, gate.clone())
                .unwrap();
        host.register_closed_observation_helper(&qualified_helper())
            .unwrap();
        let scan = host.start_scan("media", ()).unwrap();
        let revoked = gate.begin_revocation().unwrap();
        assert!(revoked.wait_drained(Duration::ZERO).is_err());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(result) = host.poll_scan(&scan, Duration::from_millis(50)).unwrap() {
                assert_eq!(result["complete"], true);
                break;
            }
            assert!(Instant::now() < deadline);
        }
        // The real scan has joined, but committed private-writer delivery still
        // owns the same gate. Neither source of lifetime can waive the other.
        assert!(revoked.wait_drained(Duration::ZERO).is_err());
        write_result.deliver(|value| {
            assert_eq!(value["status"], "committed");
            assert!(revoked.wait_drained(Duration::ZERO).is_err());
        });
        assert!(revoked.wait_drained(Duration::ZERO).is_err());
        read_result.deliver(|value| {
            assert_eq!(
                value["result"]["rows"][0][2],
                catabolic_core::encode(&json!({"title":"After persisted restart"}))
            );
            assert!(revoked.wait_drained(Duration::ZERO).is_err());
        });
        revoked.wait_drained(Duration::ZERO).unwrap();
        assert_eq!(
            host.select(&query, budget, &Cancellation::default())
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        assert_eq!(
            host.put_items(
                &[json!({"id":"refused","kind":"video","metadata":{}})],
                &Cancellation::default()
            )
            .unwrap_err()
            .code,
            ErrorCode::Conflict
        );
        assert_eq!(
            host.start_scan("media", ()).unwrap_err().code,
            ErrorCode::Conflict
        );
        assert!(PrivateCatalogWriter::open(&f.path).is_err());
        // Host still holds the writer lock. Closing it does not revive the gate.
        host.close().unwrap();
        drop(host);
        let mut writer = PrivateCatalogWriter::open(&f.path).unwrap();
        let foreign = HostAuthorization::new(HOST.into(), gate.revision().into()).unwrap();
        let mut reader = ScopedReadSession::open(&f.path).unwrap();
        assert!(
            reader
                .read_host_scope(
                    &scope,
                    foreign.acquire(foreign.revision()).unwrap(),
                    ScopeReadRequest::GetItem("editable".into()),
                    ReadBudget::new(Duration::from_secs(5), 1000).unwrap()
                )
                .is_err()
        );
        drop(reader);
        assert!(
            writer
                .admit_metadata_write(
                    writer_grant(&f, &gate),
                    &foreign.acquire(foreign.revision()).unwrap(),
                    ReadBudget::new(Duration::from_secs(5), 1000).unwrap()
                )
                .is_err()
        );
        drop(writer);
        let store = Store::open(&f.path, false, false).unwrap();
        assert_eq!(
            store
                .db
                .query_row(
                    "SELECT worker_pid FROM observation_jobs WHERE id=?",
                    [&job],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            pid
        );
        assert!(companion::has_cleanup_witness(&store, &job).unwrap());
        assert!(
            catabolic_store::native_worker_inventory::inspect(&store)
                .unwrap()
                .admits_new_work()
        );
        assert_eq!(store.db.query_row("SELECT count(*) FROM catalog_command_receipts WHERE operation_id='restart-private-write'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    }
}
#[test]
#[ignore = "requires exact rebuilt37 CATABOLIC_HOST_TEST_HELPER; run explicitly"]
fn native37_restart_rechecks_each_witness_and_unrelated_owner_without_mutation() {
    for failure in [
        "changed-claim",
        "pending-processing",
        "terminal-processing",
        "unknown-api-owner",
        "unwitnessed-observation",
    ] {
        let f = setup(37);
        let record = crash(&f, "after-host-checkpoint", true);
        let store = Store::open(&f.path, true, false).unwrap();
        let (job, file): (String, String) = store
            .db
            .query_row(
                "SELECT (SELECT id FROM observation_jobs LIMIT 1),(SELECT id FROM files LIMIT 1)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        store.transaction(|db| {match failure {
            "changed-claim"=>{db.execute("UPDATE observation_jobs SET claim_token=? WHERE id=?",params![uuid::Uuid::new_v4().to_string(),job])?;}
            "pending-processing"|"terminal-processing"=>{db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,attempts) VALUES('unrelated','default',?,'probe','media','{}','{}','unrelated',?,?)",params![file,if failure=="pending-processing"{"queued"}else{"complete"},if failure=="pending-processing"{0}else{1}])?;}
            "unknown-api-owner"=>{db.execute("INSERT INTO api_worker_status VALUES('default',99999,'{}',0)",[])?;}
            "unwitnessed-observation"=>{db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,worker_pid,claim_token) VALUES('other-scan','default','media','same','[]',0,2,'complete',1,NULL,'unwitnessed')",[])?;}
            _=>unreachable!()
        }Ok(())}).unwrap();
        assert_eq!(
            companion::has_cleanup_witness(&store, &job).unwrap(),
            failure != "changed-claim"
        );
        drop(store);
        let before = snapshot(&f.path);
        assert_closed(reopen(&f, &record, base()), None);
        assert_eq!(snapshot(&f.path), before, "{failure}");
    }
}

#[test]
fn restart_version_and_profile_are_checked_before_opening_a_catalog() {
    let f = setup(37);
    let record = crash(&f, "after-host-checkpoint", false);
    let missing = f.root.path().join("missing");
    let invalid = HostSession::open_after_import_cutover(
        &missing,
        "default",
        IMPORT_RESTART_CONTRACT_VERSION + 1,
        &record,
        base(),
        "next".into(),
        policy(),
    )
    .err()
    .unwrap();
    assert_eq!(invalid.code, ErrorCode::UnsupportedCapability);
    assert!(!missing.exists());
    let invalid = HostSession::open_after_import_cutover(
        &missing,
        "",
        IMPORT_RESTART_CONTRACT_VERSION,
        &record,
        base(),
        "next".into(),
        policy(),
    )
    .err()
    .unwrap();
    assert_eq!(invalid.code, ErrorCode::InvalidInput);
    assert!(!missing.exists());
}

#[test]
fn partition_restart_reconciles_every_durable_receipt_before_one_gate() {
    use catabolic_runtime::host::{PARTITIONED_IMPORT_RESTART_CONTRACT_VERSION, RecordedImportSet};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("partitioned.db");
    migration::initialize_native_at(&path, 39).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    let mut records = Vec::new();
    for id in ["partition-a", "partition-b"] {
        let grant = ImportAuthority {
            host_namespace: HOST.into(),
            import_id: id.into(),
            source_namespace: "partitioned-fixture".into(),
        };
        let manifest = SnapshotManifest {
            contract_version: import::EMPTY_IMPORT_CONTRACT_VERSION,
            import_id: id.into(),
            source_namespace: grant.source_namespace.clone(),
            source_snapshot_id: "complete-frozen-source".into(),
            batches: vec![],
        };
        let digest = import::manifest_digest(&manifest).unwrap();
        let execute = |step, mutation: ImportMutation| {
            let intent = command(
                &store,
                &import::operation_id(id, step).unwrap(),
                json!({"operation":"import","mutation":mutation}),
            );
            value(
                commands::execute_with_authority(
                    &store,
                    &intent,
                    CatalogAuthority::new(base()).with_imports(&grant),
                    policy(),
                )
                .unwrap(),
            )
        };
        execute(
            ImportStep::Prepare,
            ImportMutation::Prepare {
                manifest,
                input_digest: digest.clone(),
            },
        );
        let validated = execute(
            ImportStep::Validate,
            ImportMutation::Validate {
                import_id: id.into(),
                input_digest: digest.clone(),
            },
        );
        let map = validated["mapping_digest"].as_str().unwrap().to_owned();
        let binding = ActivationBinding {
            catalog_id: store.database_id.clone(),
            incarnation: store.catalog_incarnation.clone().unwrap(),
            import_id: id.into(),
            input_digest: digest.clone(),
            source_snapshot_id: "complete-frozen-source".into(),
            validation_generation: generation(&store),
            reference_ack_id: "aggregate-audit".into(),
            mapping_digest: map.clone(),
        };
        let gate = HostAuthorization::new(HOST.into(), format!("freeze-{id}")).unwrap();
        let active = TrustedImportActivation::after_drain(
            &gate,
            gate.begin_revocation().unwrap(),
            binding.clone(),
        )
        .unwrap();
        let intent = command(
            &store,
            &import::operation_id(id, ImportStep::Activate).unwrap(),
            json!({"operation":"import","mutation":ImportMutation::Activate{import_id:id.into(),input_digest:digest.clone(),reference_ack_id:binding.reference_ack_id.clone(),mapping_digest:map.clone()}}),
        );
        commands::execute_with_authority(
            &store,
            &intent,
            CatalogAuthority::new(base())
                .with_imports(&grant)
                .with_import_activation(&active),
            policy(),
        )
        .unwrap();
        records.push(RecordedImportHandshake {
            activation_intent: intent,
            checkpoint: HostCutoverCheckpoint {
                catalog_id: binding.catalog_id,
                incarnation: binding.incarnation,
                phase: HostCutoverPhase::Activated,
                import_id: id.into(),
                input_digest: digest,
                reference_ack_id: binding.reference_ack_id,
                mapping_digest: map,
            },
        });
    }
    drop(store);
    let ids = ["partition-a".into(), "partition-b".into()].into();
    let digest = catabolic_core::sha256(b"trusted complete two-partition manifest");
    assert!(
        RecordedImportSet::from_durable_manifest(digest.clone(), &ids, vec![records[0].clone()])
            .is_err()
    );
    assert!(
        RecordedImportSet::from_durable_manifest(
            digest.clone(),
            &ids,
            vec![records[0].clone(), records[0].clone()]
        )
        .is_err()
    );
    let complete =
        RecordedImportSet::from_durable_manifest(digest.clone(), &ids, records.clone()).unwrap();
    assert_eq!(complete.partition_count(), 2);
    let mut mixed = records.clone();
    mixed[1].checkpoint.incarnation = "different-incarnation".into();
    assert!(RecordedImportSet::from_durable_manifest(digest.clone(), &ids, mixed).is_err());
    let unexpected = ["partition-a".into(), "unexpected".into()].into();
    assert!(
        RecordedImportSet::from_durable_manifest(digest.clone(), &unexpected, records.clone())
            .is_err()
    );
    assert!(
        HostSession::open_after_import_cutovers(
            &path,
            "default",
            PARTITIONED_IMPORT_RESTART_CONTRACT_VERSION,
            &complete,
            &"0".repeat(64),
            base(),
            "wrong-manifest".into(),
            policy(),
        )
        .is_err()
    );
    let mut admission = HostSession::open_after_import_cutovers(
        &path,
        "default",
        PARTITIONED_IMPORT_RESTART_CONTRACT_VERSION,
        &complete,
        &digest,
        base(),
        "complete-gate".into(),
        policy(),
    )
    .unwrap();
    assert_eq!(admission.decision(), Reconciliation::AdmitCatabolicOnly);
    admission.admitted_host().unwrap().close().unwrap();
    drop(admission);
    records[1].checkpoint.phase = HostCutoverPhase::Validated;
    let incomplete =
        RecordedImportSet::from_durable_manifest(digest.clone(), &ids, records).unwrap();
    let mut admission = HostSession::open_after_import_cutovers(
        &path,
        "default",
        PARTITIONED_IMPORT_RESTART_CONTRACT_VERSION,
        &incomplete,
        &digest,
        base(),
        "closed-gate".into(),
        policy(),
    )
    .unwrap();
    assert_eq!(admission.decision(), Reconciliation::FinishHostActivation);
    assert!(admission.admitted_host().is_err());
}

#[test]
#[ignore = "real native helper qualification: restart retains terminal deferred scan history"]
fn native39_restart_allows_only_witnessed_terminal_deferred_history() {
    let f = setup(39);
    let record = crash(&f, "after-host-checkpoint", true);
    let store = Store::open(&f.path, true, false).unwrap();
    let job: String = store
        .db
        .query_row("SELECT id FROM observation_jobs LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert!(companion::has_cleanup_witness(&store, &job).unwrap());
    // Simulate retained incomplete coverage after a real, independently reaped
    // helper. This does not manufacture or modify its actual cleanup witness.
    store.transaction(|db|{db.execute("INSERT INTO observation_scopes(job_id,path,state,detail,updated_at) VALUES(?,'historical-deferred','deferred','{}',1)",[&job])?; Ok(())}).unwrap();
    drop(store);
    let before = snapshot(&f.path);
    let mut reopened = reopen(&f, &record, base()).unwrap();
    assert_eq!(reopened.decision(), Reconciliation::AdmitCatabolicOnly);
    assert!(reopened.admitted_host().is_ok());
    drop(reopened);
    assert_eq!(snapshot(&f.path), before);
    let store = Store::open(&f.path, true, false).unwrap();
    assert_eq!(store.db.query_row("SELECT state FROM observation_scopes WHERE job_id=? AND path='historical-deferred'",[&job],|r|r.get::<_,String>(0)).unwrap(),"deferred");
    store
        .transaction(|db| {
            db.execute(
                "UPDATE observation_jobs SET claim_token='different-owner' WHERE id=?",
                [&job],
            )?;
            Ok(())
        })
        .unwrap();
    drop(store);
    assert_closed(reopen(&f, &record, base()), None);
}

#[test]
fn native40_restart_requires_actual_activation_and_host_acknowledgment() {
    for mode in [
        "before-activation",
        "after-activation",
        "after-host-checkpoint",
    ] {
        let f = setup(40);
        let record = crash(&f, mode, false);
        let before = snapshot(&f.path);
        let outcome = reopen(&f, &record, base());
        if mode == "after-host-checkpoint" {
            let mut admission = outcome.unwrap();
            assert_eq!(admission.decision(), Reconciliation::AdmitCatabolicOnly);
            assert_eq!(
                admission.admitted_host().unwrap().identity().unwrap()["schema_version"],
                40
            );
            admission.admitted_host().unwrap().close().unwrap();
        } else {
            assert_closed(
                outcome,
                if mode == "after-activation" {
                    Some(Reconciliation::FinishHostActivation)
                } else {
                    None
                },
            );
        }
        assert_eq!(
            snapshot(&f.path),
            before,
            "restart never dispatches an absent activation"
        );
    }
}
