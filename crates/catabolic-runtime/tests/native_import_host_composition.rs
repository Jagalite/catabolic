//! Public HostSession/private-writer/import composition on disposable native35/36.
//! Controlled child evidence is distinct from real observation cleanup evidence.
#![cfg(unix)]
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility},
    catalog_host_lease::{HostAuthorization, LeaseError},
    catalog_metadata::SourceKind,
};
use catabolic_runtime::host::{CONTRACT_VERSION, ErrorCode, HostSession};
use catabolic_store::{
    Store,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
    },
    catalog_import::{
        self as import, ActivationBinding, ImportAuthority, ImportBatch, ImportErrorCode,
        ImportItem, ImportMutation, ImportStep, SnapshotManifest, TrustedImportActivation,
    },
    catalog_metadata_store::{
        MetadataAccess, MetadataAuthority, MetadataChange, MetadataMutation, PolicyField,
        PolicyNormalization, PolicyValueType,
    },
    catalog_scope::{ReadBudget, SavedQueryRef},
    catalog_write::{
        MetadataPatchRequest, PrivateCatalogWriter, TrustedMetadataWriteGrant, WriteBudget,
    },
    curation, definitions, migration,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};
static HELPER: Mutex<()> = Mutex::new(());
const HOST: &str = "composition-host";
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
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
fn blocked(gate: &HostAuthorization, binding: &ActivationBinding) {
    assert_eq!(
        TrustedImportActivation::after_drain(
            gate,
            gate.begin_revocation().unwrap(),
            binding.clone()
        )
        .unwrap_err()
        .code,
        ImportErrorCode::FreezeNotDrained
    );
}
struct Setup {
    root: tempfile::TempDir,
    path: PathBuf,
    query: SavedQueryRef,
    fingerprint: String,
    binding: ActivationBinding,
}
fn setup(schema: u32) -> Setup {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog");
    migration::initialize_native_at(&path, schema).unwrap();
    let store = Store::open(&path, true, false).unwrap();
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
    // This binding is used only to test drain admission before final validation.
    let binding = ActivationBinding {
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone().unwrap(),
        import_id: "composition-import".into(),
        input_digest: "0".repeat(64),
        source_snapshot_id: "snapshot-1".into(),
        validation_generation: generation(&store),
        reference_ack_id: "bounded-fixture-audit".into(),
        mapping_digest: "0".repeat(64),
    };
    Setup {
        root,
        path,
        query,
        fingerprint,
        binding,
    }
}
fn request(f: &Setup) -> MetadataPatchRequest {
    MetadataPatchRequest {
        operation_id: "admitted-write".into(),
        schema_fingerprint: f.fingerprint.clone(),
        expected_revision: 0,
        edits_json: r#"{"title":{"op":"set","value":"Committed while draining"}}"#.into(),
        evidence_json: "{}".into(),
    }
}
fn writer_grant(f: &Setup, gate: &HostAuthorization) -> TrustedMetadataWriteGrant {
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
fn write_budget() -> WriteBudget {
    WriteBudget::new(
        Duration::from_secs(10),
        10000,
        100000,
        16 * 1024 * 1024,
        1000000,
    )
    .unwrap()
}
fn alive(pid: i32) -> bool {
    // SAFETY: signal zero only observes the fixture-owned child.
    unsafe { libc::kill(pid, 0) == 0 }
}
#[test]
fn common_gate_retains_controlled_scan_and_committed_writer_delivery_native35_and36() {
    let _serial = HELPER.lock().unwrap();
    let tools = tempfile::tempdir().unwrap();
    let binary = tools.path().join("retained-child");
    fs::write(&binary,"#!/bin/sh\nprintf '%s' \"$$\" > \"$2.started\"\nn=0\nwhile [ ! -f \"$2.release\" ] && [ \"$n\" -lt 600 ]; do /bin/sleep 0.1; n=$((n+1)); done\nprintf '{\"complete\":true}'\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    for schema in [35, 36] {
        let f = setup(schema);
        let initial = HostAuthorization::new(HOST.into(), "shared-freeze-1".into()).unwrap();
        let mut host =
            HostSession::open_authorized(&f.path, "default", true, CONTRACT_VERSION, initial)
                .unwrap();
        let gate = host.authorization().unwrap();
        host.register_helper(&binary, &catabolic_core::sha256(fs::read(&binary).unwrap()))
            .unwrap();
        host.start_scan("media", ()).unwrap();
        let started = PathBuf::from(format!("{}.started", f.path.display()));
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid: i32 = loop {
            if let Ok(raw) = fs::read_to_string(&started)
                && let Ok(pid) = raw.parse()
            {
                break pid;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        };
        let mut writer = PrivateCatalogWriter::open(&f.path).unwrap();
        let lease = gate.acquire(gate.revision()).unwrap();
        assert!(lease.belongs_to(&host.authorization().unwrap()));
        let handle = writer
            .admit_metadata_write(
                writer_grant(&f, &gate),
                &lease,
                ReadBudget::new(Duration::from_secs(5), 1000).unwrap(),
            )
            .unwrap();
        let same_labels = HostAuthorization::new(HOST.into(), gate.revision().into()).unwrap();
        assert!(
            writer
                .execute_metadata_patch(
                    &handle,
                    same_labels.acquire(same_labels.revision()).unwrap(),
                    request(&f),
                    policy(),
                    write_budget()
                )
                .is_err()
        );
        blocked(&gate, &f.binding);
        assert!(alive(pid));
        assert_eq!(
            host.start_scan("media", ()).unwrap_err().code,
            ErrorCode::Conflict
        );
        let result = writer
            .execute_metadata_patch(&handle, lease, request(&f), policy(), write_budget())
            .unwrap();
        drop(writer);
        let durable = Store::open(&f.path, false, false).unwrap();
        assert_eq!(durable.db.query_row("SELECT count(*) FROM catalog_command_receipts WHERE operation_id='admitted-write'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        drop(durable);
        blocked(&gate, &f.binding);
        assert!(alive(pid));
        fs::write(format!("{}.release", f.path.display()), "release").unwrap();
        assert!(host.drain(Duration::from_secs(5)).unwrap());
        assert!(!alive(pid));
        // The child has exited and been reaped. Only the delivered writer result
        // still owns the common gate, and remains owned inside its callback.
        blocked(&gate, &f.binding);
        result.deliver(|value| {
            assert_eq!(value["status"], "committed");
            blocked(&gate, &f.binding);
        });
        gate.begin_revocation()
            .unwrap()
            .wait_drained(Duration::ZERO)
            .unwrap();
        drop(host);
        let store = Store::open(&f.path, true, false).unwrap();
        let binding = stage(&store);
        assert_eq!(
            activate(&store, &gate, &binding).unwrap().code(),
            "committed"
        );
        assert_eq!(
            gate.acquire(gate.revision()).unwrap_err(),
            LeaseError::Revoked
        );
        drop(store);
        assert!(
            HostSession::open_authorized(&f.path, "default", true, CONTRACT_VERSION, gate).is_err()
        );
    }
}
#[test]
#[ignore = "requires exact composition-build CATABOLIC_HOST_TEST_HELPER"]
fn real_scan_child_drain_does_not_erase_terminal_observation_ownership_native35_and36() {
    let _serial = HELPER.lock().unwrap();
    let binary = PathBuf::from(
        std::env::var_os("CATABOLIC_HOST_TEST_HELPER").expect("exact helper required"),
    );
    for schema in [35, 36] {
        let f = setup(schema);
        let source = f.root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("fixture.txt"), "bounded real scan\n").unwrap();
        let store = Store::open(&f.path, true, false).unwrap();
        catabolic_store::observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"media","root":source}),
        )
        .unwrap();
        drop(store);
        let mut host = HostSession::open_authorized(
            &f.path,
            "default",
            true,
            CONTRACT_VERSION,
            HostAuthorization::new(HOST.into(), "scan-freeze-1".into()).unwrap(),
        )
        .unwrap();
        let gate = host.authorization().unwrap();
        host.register_helper(&binary, &catabolic_core::sha256(fs::read(&binary).unwrap()))
            .unwrap();
        host.start_scan("media", ()).unwrap();
        blocked(&gate, &f.binding);
        assert!(host.drain(Duration::from_secs(20)).unwrap());
        gate.begin_revocation()
            .unwrap()
            .wait_drained(Duration::ZERO)
            .unwrap();
        drop(host);
        let store = Store::open(&f.path, true, false).unwrap();
        let retained:i64=store.db.query_row("SELECT count(*) FROM observation_jobs WHERE state='complete' AND worker_pid IS NOT NULL",[],|r|r.get(0)).unwrap();
        assert!(retained > 0);
        let binding = stage(&store);
        assert_eq!(
            activate(&store, &gate, &binding).unwrap_err().code,
            commands::CommandStoreErrorCode::Import(ImportErrorCode::RecoveryRequired)
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM items WHERE id='imported'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(store.db.query_row("SELECT count(*) FROM observation_jobs WHERE state='complete' AND worker_pid IS NOT NULL",[],|r|r.get::<_,i64>(0)).unwrap(),retained);
    }
}
