//! Bounded R6 import owner contracts. Disposable native34/35/36 SQLite catalogs. No Motion,
//! filesystem publication, real restore, interval activation or external audit.
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
use catabolic_store::{
    Store,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, CommandStoreErrorCode, ReceiptPolicy,
        TrustedCommandAuthority, TrustedReceiptMaintenance,
    },
    catalog_import::{
        self as import, BatchDescriptor, EvidenceKind, ImportAuthority, ImportBatch,
        ImportErrorCode, ImportItem, ImportMutation, ImportStep, SnapshotManifest,
        UnresolvedEvidence,
    },
    migration,
};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};

struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        Self::at(34)
    }
    fn at(schema: u32) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("import.db");
        migration::initialize_native_at(&path, schema).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        Self {
            _root: root,
            path,
            store,
        }
    }
    fn envelope(&self, mutation: &ImportMutation) -> CommandEnvelope {
        envelope(&self.store, mutation)
    }
    fn run(&self, mutation: &ImportMutation) -> CommandOutcome {
        run(&self.store, &self.envelope(mutation), policy()).unwrap()
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
fn count(store: &Store, table: &str) -> i64 {
    assert!(
        [
            "items",
            "files",
            "catalog_command_receipts",
            "catalog_receipt_holds",
            "catalog_search_documents"
        ]
        .contains(&table)
    );
    store
        .db
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
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
        host_namespace: "import-host",
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn grant() -> ImportAuthority {
    ImportAuthority {
        host_namespace: "import-host".into(),
        import_id: "import-1".into(),
        source_namespace: "fixture-source".into(),
    }
}
fn maintenance() -> TrustedReceiptMaintenance<'static> {
    TrustedReceiptMaintenance {
        host_namespace: "import-host",
        holder_namespace: import::IMPORT_HOLDER_NAMESPACE,
    }
}
fn envelope(store: &Store, mutation: &ImportMutation) -> CommandEnvelope {
    let (id, step) = match mutation {
        ImportMutation::Prepare { manifest, .. } => (&manifest.import_id, ImportStep::Prepare),
        ImportMutation::Stage {
            import_id,
            batch_index,
            ..
        } => (import_id, ImportStep::Batch(*batch_index)),
        ImportMutation::Validate { import_id, .. } => (import_id, ImportStep::Validate),
        ImportMutation::Activate { import_id, .. } => (import_id, ImportStep::Activate),
    };
    CommandEnvelope::new(
        1,
        CommandKey::new(
            &store.database_id,
            "import-host",
            import::operation_id(id, step).unwrap(),
        )
        .unwrap(),
        store.catalog_incarnation.clone().unwrap(),
        generation(store),
        &json!({"operation":"import","mutation":mutation}).to_string(),
    )
    .unwrap()
}
fn run(
    store: &Store,
    command: &CommandEnvelope,
    policy: ReceiptPolicy,
) -> commands::Result<CommandOutcome> {
    commands::execute_with_authority(
        store,
        command,
        CatalogAuthority::new(base()).with_imports(&grant()),
        policy,
    )
}
fn result(outcome: &CommandOutcome) -> &Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        _ => panic!("unexpected {outcome:?}"),
    }
}
fn item(id: &str) -> ImportItem {
    ImportItem {
        source_id: format!("source-{id}"),
        item_id: id.into(),
        kind: "video".into(),
        legacy_metadata: json!({"title":id}),
    }
}
fn batch(id: &str) -> ImportBatch {
    ImportBatch {
        items: vec![item(id)],
        unresolved_evidence: vec![],
        typed: None,
    }
}
fn snapshot(batches: &[ImportBatch]) -> (SnapshotManifest, String) {
    let manifest = SnapshotManifest {
        contract_version: 1,
        import_id: "import-1".into(),
        source_namespace: "fixture-source".into(),
        source_snapshot_id: "frozen-export-1".into(),
        batches: batches
            .iter()
            .map(|b| import::describe_batch(b).unwrap())
            .collect(),
    };
    let digest = import::manifest_digest(&manifest).unwrap();
    (manifest, digest)
}
fn prepare(f: &Fixture, batches: &[ImportBatch]) -> String {
    let (manifest, input_digest) = snapshot(batches);
    assert_eq!(
        f.run(&ImportMutation::Prepare {
            manifest,
            input_digest: input_digest.clone()
        })
        .code(),
        "committed"
    );
    input_digest
}
fn stage(digest: &str, index: u32, batch: ImportBatch) -> ImportMutation {
    ImportMutation::Stage {
        import_id: "import-1".into(),
        input_digest: digest.into(),
        batch_index: index,
        batch,
    }
}
fn validate(digest: &str) -> ImportMutation {
    ImportMutation::Validate {
        import_id: "import-1".into(),
        input_digest: digest.into(),
    }
}
fn error(f: &Fixture, mutation: &ImportMutation) -> ImportErrorCode {
    match run(&f.store, &f.envelope(mutation), policy())
        .unwrap_err()
        .code
    {
        CommandStoreErrorCode::Import(code) => code,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn prepare_stage_validate_never_enter_active_catalog_or_search() {
    for schema in [34, 35] {
        let f = Fixture::at(schema);
        let before = generation(&f.store);
        let batches = vec![batch("logical-a"), batch("logical-b")];
        let digest = prepare(&f, &batches);
        for (index, batch) in batches.into_iter().enumerate() {
            f.run(&stage(&digest, index as u32, batch));
        }
        let validated = f.run(&validate(&digest));
        let body = result(&validated);
        assert_eq!(body["phase"], "validated_staging");
        assert_eq!(body["activation_supported"], true);
        assert_eq!(
            body["proposed_mappings"][0],
            json!({"source_id":"source-logical-a","item_id":"logical-a"})
        );
        assert_eq!(body["proposed_mappings"].as_array().unwrap().len(), 2);
        assert_eq!(count(&f.store, "items"), 0);
        assert_eq!(count(&f.store, "files"), 0);
        if schema == 35 {
            assert_eq!(count(&f.store, "catalog_search_documents"), 0);
        }
        assert_eq!(generation(&f.store), before);
        assert_eq!(count(&f.store, "catalog_command_receipts"), 4);
        assert_eq!(count(&f.store, "catalog_receipt_holds"), 4);
    }
}
#[test]
fn lost_batch_reply_reopens_and_replays_original_result() {
    let f = Fixture::new();
    let digest = prepare(&f, &[batch("a"), batch("b")]);
    let command = f.envelope(&stage(&digest, 0, batch("a")));
    let first = run(&f.store, &command, policy()).unwrap();
    let Fixture { _root, path, store } = f;
    drop(store);
    let store = Store::open(&path, true, false).unwrap();
    let replay = run(&store, &command, policy()).unwrap();
    assert_eq!(replay.code(), "replay");
    assert_eq!(result(&first), result(&replay));
    let second = envelope(&store, &stage(&digest, 1, batch("b")));
    run(&store, &second, policy()).unwrap();
    run(&store, &envelope(&store, &validate(&digest)), policy()).unwrap();
    assert_eq!(count(&store, "catalog_command_receipts"), 4);
    drop(_root);
}
#[test]
fn import_id_and_batch_keys_cannot_be_rebound_or_replaced() {
    let f = Fixture::new();
    let (manifest, input_digest) = snapshot(&[batch("a")]);
    let prepared = f.envelope(&ImportMutation::Prepare {
        manifest,
        input_digest: input_digest.clone(),
    });
    run(&f.store, &prepared, policy()).unwrap();
    let (changed, changed_digest) = snapshot(&[batch("b")]);
    assert_eq!(
        f.run(&ImportMutation::Prepare {
            manifest: changed,
            input_digest: changed_digest.clone()
        }),
        CommandOutcome::Conflict
    );
    assert_eq!(
        error(&f, &stage(&changed_digest, 0, batch("b"))),
        ImportErrorCode::SnapshotConflict
    );
    let staged = f.envelope(&stage(&input_digest, 0, batch("a")));
    run(&f.store, &staged, policy()).unwrap();
    assert_eq!(
        f.run(&stage(&input_digest, 0, batch("b"))),
        CommandOutcome::Conflict
    );
    assert_eq!(count(&f.store, "catalog_command_receipts"), 2);
}
#[test]
fn incomplete_and_corrupt_batches_do_not_validate() {
    let f = Fixture::new();
    let digest = prepare(&f, &[batch("a"), batch("b")]);
    assert_eq!(error(&f, &validate(&digest)), ImportErrorCode::MissingStage);
    assert_eq!(
        error(&f, &stage(&digest, 0, batch("wrong"))),
        ImportErrorCode::SnapshotConflict
    );
    assert_eq!(
        error(&f, &stage(&digest, 2, batch("b"))),
        ImportErrorCode::InvalidInput
    );
    f.run(&stage(&digest, 0, batch("a")));
    assert_eq!(error(&f, &validate(&digest)), ImportErrorCode::MissingStage);
    assert_eq!(count(&f.store, "catalog_command_receipts"), 2);
}
#[test]
fn duplicates_across_batches_and_preexisting_logical_ids_refuse_mapping() {
    let f = Fixture::new();
    let batches = [batch("a"), batch("a")];
    let digest = prepare(&f, &batches);
    for (index, b) in batches.into_iter().enumerate() {
        f.run(&stage(&digest, index as u32, b));
    }
    assert_eq!(
        error(&f, &validate(&digest)),
        ImportErrorCode::DuplicateSourceId
    );
    let f = Fixture::new();
    let digest = prepare(&f, &[batch("a")]);
    catabolic_store::curation::item_put(&f.store, &json!({"id":"a","kind":"video","metadata":{}}))
        .unwrap();
    assert_eq!(
        error(&f, &stage(&digest, 0, batch("a"))),
        ImportErrorCode::DestinationCollision
    );
}
#[test]
fn phantom_destination_created_after_staging_is_detected_on_validation() {
    let f = Fixture::new();
    let digest = prepare(&f, &[batch("a")]);
    f.run(&stage(&digest, 0, batch("a")));
    let validation = f.envelope(&validate(&digest));
    catabolic_store::curation::item_put(&f.store, &json!({"id":"a","kind":"video","metadata":{}}))
        .unwrap();
    assert_eq!(
        run(&f.store, &validation, policy()).unwrap(),
        CommandOutcome::StaleGeneration
    );
    assert_eq!(
        error(&f, &validate(&digest)),
        ImportErrorCode::DestinationCollision
    );
    assert_eq!(count(&f.store, "catalog_command_receipts"), 2);
}
#[test]
fn unresolved_occurrences_decisions_and_references_are_retained_without_inference() {
    let f = Fixture::new();
    let mut b = batch("a");
    let evidence = [
        (
            "rename",
            EvidenceKind::PhysicalOccurrence,
            json!({"source":"disk","path":"new/a","previous_path":"old/a","revision":"r1","digest":"same"}),
        ),
        (
            "replacement",
            EvidenceKind::PhysicalOccurrence,
            json!({"source":"disk","path":"new/a","revision":"r2","digest":"different"}),
        ),
        (
            "duplicate",
            EvidenceKind::PhysicalOccurrence,
            json!({"source":"disk","path":"copy/a","revision":"r3","digest":"same"}),
        ),
        (
            "unavailable",
            EvidenceKind::PhysicalOccurrence,
            json!({"source":"offline","availability":"unknown","coverage":null}),
        ),
        (
            "decision",
            EvidenceKind::AcceptedDecision,
            json!({"source_id":"old-a","decision":"accepted","evidence":{}}),
        ),
        (
            "progress",
            EvidenceKind::ApplicationReference,
            json!({"target":"missing-timeline","position_us":123}),
        ),
    ];
    b.unresolved_evidence = evidence
        .into_iter()
        .map(|(id, kind, data)| UnresolvedEvidence {
            evidence_id: id.into(),
            kind,
            data,
        })
        .collect();
    let digest = prepare(&f, std::slice::from_ref(&b));
    let staged = f.run(&stage(&digest, 0, b.clone()));
    assert_eq!(result(&staged)["batch"], serde_json::to_value(&b).unwrap());
    let validated = f.run(&validate(&digest));
    assert_eq!(result(&validated)["unresolved_evidence"], 6);
    assert_eq!(result(&validated)["activation_supported"], false);
    assert_eq!(count(&f.store, "items"), 0);
    assert_eq!(count(&f.store, "files"), 0);
}
#[test]
fn activation_remains_fail_closed_after_validated_staging() {
    let f = Fixture::new();
    let digest = prepare(&f, &[batch("a")]);
    f.run(&stage(&digest, 0, batch("a")));
    f.run(&validate(&digest));
    let activate = ImportMutation::Activate {
        import_id: "import-1".into(),
        input_digest: digest,
        reference_ack_id: "audit-1".into(),
        mapping_digest: "0".repeat(64),
    };
    assert_eq!(error(&f, &activate), ImportErrorCode::ActivationUnqualified);
    assert_eq!(count(&f.store, "items"), 0);
    assert_eq!(count(&f.store, "catalog_command_receipts"), 3);
}
#[test]
fn mandatory_holds_are_atomic_and_storage_bounded() {
    let f = Fixture::new();
    let (manifest, input_digest) = snapshot(&[batch("a")]);
    let command = f.envelope(&ImportMutation::Prepare {
        manifest,
        input_digest,
    });
    assert_eq!(
        run(
            &f.store,
            &command,
            ReceiptPolicy {
                max_hold_records: 0,
                ..policy()
            }
        )
        .unwrap(),
        CommandOutcome::ReceiptStorageExhausted
    );
    assert_eq!(count(&f.store, "catalog_command_receipts"), 0);
    assert_eq!(count(&f.store, "catalog_receipt_holds"), 0);
    run(&f.store, &command, policy()).unwrap();
    assert_eq!(
        commands::compact_receipts(&f.store, maintenance(), 32).unwrap(),
        0
    );
    let usage = commands::receipt_usage(&f.store).unwrap();
    assert_eq!(usage.hold_records, 1);
    assert_eq!(
        run(
            &f.store,
            &command,
            ReceiptPolicy {
                max_logical_bytes: 0,
                max_hold_records: 0,
                ..policy()
            }
        )
        .unwrap()
        .code(),
        "replay"
    );
    let f = Fixture::new();
    let (manifest, input_digest) = snapshot(&[batch("a")]);
    let command = f.envelope(&ImportMutation::Prepare {
        manifest,
        input_digest,
    });
    assert_eq!(
        run(
            &f.store,
            &command,
            ReceiptPolicy {
                max_logical_bytes: usage.logical_bytes - 1,
                ..policy()
            }
        )
        .unwrap(),
        CommandOutcome::ReceiptStorageExhausted
    );
    assert_eq!(count(&f.store, "catalog_command_receipts"), 0);
    assert_eq!(count(&f.store, "catalog_receipt_holds"), 0);
}
#[test]
fn explicit_hold_release_and_compaction_fail_closed_without_key_reuse() {
    let f = Fixture::new();
    let (manifest, input_digest) = snapshot(&[batch("a")]);
    let command = f.envelope(&ImportMutation::Prepare {
        manifest,
        input_digest: input_digest.clone(),
    });
    run(&f.store, &command, policy()).unwrap();
    commands::release_hold(
        &f.store,
        command.key(),
        maintenance(),
        &catabolic_core::sha256("import-1"),
    )
    .unwrap();
    assert_eq!(
        commands::compact_receipts(&f.store, maintenance(), 32).unwrap(),
        1
    );
    assert_eq!(
        run(&f.store, &command, policy()).unwrap(),
        CommandOutcome::ReceiptExpired
    );
    assert_eq!(
        error(&f, &stage(&input_digest, 0, batch("a"))),
        ImportErrorCode::StageExpired
    );
    assert_eq!(count(&f.store, "catalog_command_receipts"), 1);
}
#[test]
fn receipt_and_hold_insertion_and_deferred_commit_failures_roll_back_everything() {
    for trigger in [
        "CREATE TEMP TRIGGER fail BEFORE INSERT ON catalog_command_receipts BEGIN SELECT RAISE(ABORT,'injected'); END;",
        "CREATE TEMP TRIGGER fail BEFORE INSERT ON catalog_receipt_holds BEGIN SELECT RAISE(ABORT,'injected'); END;",
        "CREATE TEMP TRIGGER fail AFTER INSERT ON catalog_receipt_holds BEGIN INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) SELECT 'context','injected',incarnation FROM catalog_state; END;",
    ] {
        let f = Fixture::new();
        let (manifest, input_digest) = snapshot(&[batch("a")]);
        let command = f.envelope(&ImportMutation::Prepare {
            manifest,
            input_digest,
        });
        f.store.db.execute_batch(trigger).unwrap();
        assert!(run(&f.store, &command, policy()).is_err());
        assert!(f.store.db.is_autocommit());
        assert_eq!(count(&f.store, "catalog_command_receipts"), 0);
        assert_eq!(count(&f.store, "catalog_receipt_holds"), 0);
        assert_eq!(count(&f.store, "items"), 0);
        f.store.db.execute_batch("DROP TRIGGER fail").unwrap();
        assert_eq!(
            run(&f.store, &command, policy()).unwrap().code(),
            "committed"
        );
    }
}
#[test]
fn current_import_grant_and_receipt_read_capabilities_are_independent() {
    let f = Fixture::new();
    let (manifest, input_digest) = snapshot(&[batch("a")]);
    let command = f.envelope(&ImportMutation::Prepare {
        manifest,
        input_digest,
    });
    assert_eq!(
        commands::execute(&f.store, &command, base(), policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::AuthorityDenied
    );
    let wrong = ImportAuthority {
        source_namespace: "another-source".into(),
        ..grant()
    };
    assert_eq!(
        commands::execute_with_authority(
            &f.store,
            &command,
            CatalogAuthority::new(base()).with_imports(&wrong),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::Import(ImportErrorCode::AuthorityDenied)
    );
    run(&f.store, &command, policy()).unwrap();
    assert_eq!(
        commands::execute(
            &f.store,
            &command,
            TrustedCommandAuthority {
                allow_new_commands: false,
                receipt_visibility: ReceiptVisibility::CommittedStatusOnly,
                ..base()
            },
            policy()
        )
        .unwrap(),
        CommandOutcome::CommittedRedacted
    );
    assert_eq!(
        commands::execute(
            &f.store,
            &command,
            TrustedCommandAuthority {
                allow_new_commands: false,
                ..base()
            },
            policy()
        )
        .unwrap()
        .code(),
        "replay"
    );
}
#[test]
fn deterministic_import_keys_cannot_be_used_by_generic_commands() {
    let f = Fixture::new();
    let key = CommandKey::new(
        &f.store.database_id,
        "import-host",
        import::operation_id("import-1", ImportStep::Prepare).unwrap(),
    )
    .unwrap();
    let command = CommandEnvelope::new(
        1,
        key,
        f.store.catalog_incarnation.clone().unwrap(),
        generation(&f.store),
        &json!({"operation":"item_put","item":{"id":"a","kind":"video","metadata":{}}}).to_string(),
    )
    .unwrap();
    assert_eq!(
        run(&f.store, &command, policy()).unwrap_err().code,
        CommandStoreErrorCode::InvalidOperation
    );
    assert_eq!(count(&f.store, "items"), 0);
    let (manifest, input_digest) = snapshot(&[batch("a")]);
    let command=CommandEnvelope::new(1,CommandKey::new(&f.store.database_id,"import-host","arbitrary-key").unwrap(),f.store.catalog_incarnation.clone().unwrap(),generation(&f.store),&json!({"operation":"import","mutation":{"action":"prepare","manifest":manifest,"input_digest":input_digest}}).to_string()).unwrap();
    assert_eq!(
        run(&f.store, &command, policy()).unwrap_err().code,
        CommandStoreErrorCode::Import(ImportErrorCode::InvalidInput)
    );
}
#[test]
fn simulated_restore_keeps_known_receipts_but_never_executes_absent_old_intents() {
    let f = Fixture::new();
    let (manifest, input_digest) = snapshot(&[batch("a")]);
    let known = f.envelope(&ImportMutation::Prepare {
        manifest,
        input_digest: input_digest.clone(),
    });
    run(&f.store, &known, policy()).unwrap();
    let absent = f.envelope(&stage(&input_digest, 0, batch("a")));
    let Fixture { _root, path, store } = f;
    drop(store);
    // Fixture-only incarnation rotation with every Store dropped. This does not
    // exercise or enable the prohibited real controlled-restore publication path.
    let db = catabolic_store::connect(&path, true).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) SELECT 'restore',catalog_id,incarnation FROM catalog_state; UPDATE catalog_state SET incarnation=lower(hex(randomblob(16))); DELETE FROM catalog_mutation_permits; COMMIT;").unwrap();
    drop(db);
    let store = Store::open(&path, true, false).unwrap();
    assert_eq!(run(&store, &known, policy()).unwrap().code(), "replay");
    assert_eq!(
        run(&store, &absent, policy()).unwrap(),
        CommandOutcome::OutcomeUnknownAfterRestore
    );
    assert_eq!(count(&store, "catalog_command_receipts"), 1);
    drop(_root);
}
#[test]
fn contract_and_resource_bounds_refuse_unsupported_or_empty_input() {
    let empty = ImportBatch {
        items: vec![],
        unresolved_evidence: vec![],
        typed: None,
    };
    assert_eq!(
        import::describe_batch(&empty).unwrap_err().code,
        ImportErrorCode::InvalidInput
    );
    let mut big = batch("a");
    big.items = vec![item("a"); 65];
    assert_eq!(
        import::describe_batch(&big).unwrap_err().code,
        ImportErrorCode::InvalidInput
    );
    let mut big = batch("a");
    big.items[0].legacy_metadata = json!({"title":"x".repeat(import::MAX_IMPORT_BATCH_BYTES)});
    assert_eq!(
        import::describe_batch(&big).unwrap_err().code,
        ImportErrorCode::InvalidInput
    );
    let (mut manifest, _) = snapshot(&[batch("a")]);
    manifest.contract_version = 3;
    assert_eq!(
        import::manifest_digest(&manifest).unwrap_err().code,
        ImportErrorCode::UnsupportedContract
    );
    manifest.contract_version = 1;
    manifest.batches = vec![
        BatchDescriptor {
            sha256: "0".repeat(64),
            canonical_bytes: 1,
            items: 1,
            unresolved_evidence: 0,
            typed_records: 0,
        };
        17
    ];
    assert_eq!(
        import::manifest_digest(&manifest).unwrap_err().code,
        ImportErrorCode::InvalidInput
    );
    let mut bad = batch("a");
    bad.items[0].kind = "made-up-kind".into();
    assert_eq!(
        import::describe_batch(&bad).unwrap_err().code,
        ImportErrorCode::InvalidInput
    );
    let f = Fixture::at(33);
    let (manifest, input_digest) = snapshot(&[batch("a")]);
    assert_eq!(
        error(
            &f,
            &ImportMutation::Prepare {
                manifest,
                input_digest
            }
        ),
        ImportErrorCode::UnsupportedContract
    );
}

#[test]
fn process_exit_before_commit_and_after_commit_before_reply_resume_exactly_once() {
    for mode in ["before-commit", "after-commit"] {
        let f = Fixture::new();
        let digest = prepare(&f, &[batch("a")]);
        let intent = f.envelope(&stage(&digest, 0, batch("a")));
        let Fixture { _root, path, store } = f;
        drop(store);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "import_exit_helper", "--ignored", "--nocapture"])
            .env("CATABOLIC_IMPORT_EXIT_DB", &path)
            .env("CATABOLIC_IMPORT_EXIT_MODE", mode)
            .env("CATABOLIC_IMPORT_EXIT_DIGEST", &digest)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(73));
        let store = Store::open(&path, true, false).unwrap();
        let committed = mode == "after-commit";
        assert_eq!(
            count(&store, "catalog_command_receipts"),
            if committed { 2 } else { 1 }
        );
        assert_eq!(
            count(&store, "catalog_receipt_holds"),
            if committed { 2 } else { 1 }
        );
        assert_eq!(
            run(&store, &intent, policy()).unwrap().code(),
            if committed { "replay" } else { "committed" }
        );
        assert_eq!(count(&store, "catalog_command_receipts"), 2);
        assert_eq!(count(&store, "catalog_receipt_holds"), 2);
        assert_eq!(count(&store, "items"), 0);
        drop(_root);
    }
}
#[test]
#[ignore = "process-exit helper; executed only through the crash/resume wrapper"]
fn import_exit_helper() {
    let path = std::env::var("CATABOLIC_IMPORT_EXIT_DB").expect("wrapper must supply database");
    let mode = std::env::var("CATABOLIC_IMPORT_EXIT_MODE").unwrap();
    let digest = std::env::var("CATABOLIC_IMPORT_EXIT_DIGEST").unwrap();
    let store = Store::open(std::path::Path::new(&path), true, false).unwrap();
    if mode == "before-commit" {
        store
            .db
            .create_scalar_function(
                "import_exit",
                0,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                |_| -> rusqlite::Result<i64> { std::process::exit(73) },
            )
            .unwrap();
        store.db.execute_batch("CREATE TEMP TRIGGER exit_before_commit AFTER INSERT ON catalog_receipt_holds BEGIN SELECT import_exit(); END;").unwrap();
    }
    assert_eq!(
        run(
            &store,
            &envelope(&store, &stage(&digest, 0, batch("a"))),
            policy()
        )
        .unwrap()
        .code(),
        "committed"
    );
    std::process::exit(73);
}
#[test]
fn concurrent_identical_stages_commit_once_with_one_hold() {
    let f = Fixture::new();
    let digest = prepare(&f, &[batch("a")]);
    let command = f.envelope(&stage(&digest, 0, batch("a")));
    let Fixture { _root, path, store } = f;
    drop(store);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let command = command.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                let store = loop {
                    match Store::open_typed(&path, true, false) {
                        Ok(store) => break store,
                        Err(error)
                            if error.code == catabolic_store::OpenErrorCode::BusyWriter
                                && std::time::Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("admission: {error:?}"),
                    }
                };
                run(&store, &command, policy()).unwrap()
            })
        })
        .collect();
    barrier.wait();
    let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        outcomes.iter().filter(|o| o.code() == "committed").count(),
        1
    );
    assert_eq!(outcomes.iter().filter(|o| o.code() == "replay").count(), 1);
    assert_eq!(result(&outcomes[0]), result(&outcomes[1]));
    let store = Store::open(&path, true, false).unwrap();
    assert_eq!(count(&store, "catalog_command_receipts"), 2);
    assert_eq!(count(&store, "catalog_receipt_holds"), 2);
    drop(_root);
}

fn activate(digest: &str, mapping_digest: &str) -> ImportMutation {
    ImportMutation::Activate {
        import_id: "import-1".into(),
        input_digest: digest.into(),
        reference_ack_id: "audit-1".into(),
        mapping_digest: mapping_digest.into(),
    }
}
fn activation_binding(store: &Store, digest: &str, map: &str) -> import::ActivationBinding {
    import::ActivationBinding {
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone().unwrap(),
        import_id: "import-1".into(),
        input_digest: digest.into(),
        source_snapshot_id: "frozen-export-1".into(),
        validation_generation: generation(store),
        reference_ack_id: "audit-1".into(),
        mapping_digest: map.into(),
    }
}
fn activation_grant(store: &Store, digest: &str, map: &str) -> import::TrustedImportActivation {
    let gate = catabolic_core::catalog_host_lease::HostAuthorization::new(
        "import-host".into(),
        "freeze-1".into(),
    )
    .unwrap();
    let closed = gate.begin_revocation().unwrap();
    import::TrustedImportActivation::after_drain(
        &gate,
        closed,
        activation_binding(store, digest, map),
    )
    .unwrap()
}
fn run_activation(
    store: &Store,
    command: &CommandEnvelope,
    digest: &str,
    map: &str,
) -> commands::Result<CommandOutcome> {
    let gate = activation_grant(store, digest, map);
    commands::execute_with_authority(
        store,
        command,
        CatalogAuthority::new(base())
            .with_imports(&grant())
            .with_import_activation(&gate),
        policy(),
    )
}
fn fully_staged(f: &Fixture, batches: &[ImportBatch]) -> (String, String) {
    let digest = prepare(f, batches);
    for (i, b) in batches.iter().enumerate() {
        f.run(&stage(&digest, i as u32, b.clone()));
    }
    let validated = f.run(&validate(&digest));
    let map = result(&validated)["mapping_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    (digest, map)
}
#[test]
fn logical_activation_uses_curation_atomically_and_replays_without_a_new_freeze() {
    for schema in [34, 35] {
        let f = Fixture::at(schema);
        let (digest, map) = fully_staged(&f, &[batch("a"), batch("b")]);
        let command = f.envelope(&activate(&digest, &map));
        let before = generation(&f.store);
        let committed = run_activation(&f.store, &command, &digest, &map).unwrap();
        assert_eq!(committed.code(), "committed");
        assert_eq!(result(&committed)["phase"], "activated");
        assert_eq!(result(&committed)["reference_ack_id"], "audit-1");
        assert_eq!(count(&f.store, "items"), 2);
        assert!(generation(&f.store) > before);
        assert_eq!(count(&f.store, "catalog_command_receipts"), 5);
        assert_eq!(count(&f.store, "catalog_receipt_holds"), 5);
        if schema == 35 {
            assert_eq!(count(&f.store, "catalog_search_documents"), 2);
        }
        let replay = commands::execute(
            &f.store,
            &command,
            TrustedCommandAuthority {
                allow_new_commands: false,
                ..base()
            },
            policy(),
        )
        .unwrap();
        assert_eq!(replay.code(), "replay");
        assert_eq!(result(&committed), result(&replay));
        let metadata: String = f
            .store
            .db
            .query_row("SELECT metadata FROM items WHERE id='a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&metadata).unwrap(),
            json!({"title":"a"})
        );
    }
}
#[test]
fn actual_host_gate_must_be_closed_drained_and_identical() {
    use catabolic_core::catalog_host_lease::{HostAuthorization, LeaseError};
    let f = Fixture::new();
    let (digest, map) = fully_staged(&f, &[batch("a")]);
    let host = HostAuthorization::new("import-host".into(), "freeze-1".into()).unwrap();
    let worker = host.acquire("freeze-1").unwrap();
    let close = host.begin_revocation().unwrap();
    assert_eq!(
        import::TrustedImportActivation::after_drain(
            &host,
            close,
            activation_binding(&f.store, &digest, &map)
        )
        .unwrap_err()
        .code,
        ImportErrorCode::FreezeNotDrained
    );
    assert!(matches!(host.acquire("freeze-1"), Err(LeaseError::Revoked)));
    drop(worker);
    let fresh = HostAuthorization::new("import-host".into(), "freeze-1".into()).unwrap();
    assert_eq!(
        import::TrustedImportActivation::after_drain(
            &fresh,
            host.begin_revocation().unwrap(),
            activation_binding(&f.store, &digest, &map)
        )
        .unwrap_err()
        .code,
        ImportErrorCode::AuthorityDenied
    );
    let grant = import::TrustedImportActivation::after_drain(
        &host,
        host.begin_revocation().unwrap(),
        activation_binding(&f.store, &digest, &map),
    )
    .unwrap();
    drop(grant);
    assert!(matches!(host.acquire("freeze-1"), Err(LeaseError::Revoked)));
}
#[test]
fn activation_requires_exact_reference_ack_mapping_and_source_snapshot() {
    let f = Fixture::new();
    let (digest, map) = fully_staged(&f, &[batch("a")]);
    let command = f.envelope(&activate(&digest, &map));
    for field in ["ack", "source", "digest", "catalog", "incarnation"] {
        let gate = catabolic_core::catalog_host_lease::HostAuthorization::new(
            "import-host".into(),
            "freeze-1".into(),
        )
        .unwrap();
        let mut binding = activation_binding(&f.store, &digest, &map);
        match field {
            "ack" => binding.reference_ack_id = "wrong".into(),
            "source" => binding.source_snapshot_id = "wrong".into(),
            "digest" => binding.mapping_digest = "0".repeat(64),
            "catalog" => binding.catalog_id = "wrong".into(),
            _ => binding.incarnation = "wrong".into(),
        }
        let active = import::TrustedImportActivation::after_drain(
            &gate,
            gate.begin_revocation().unwrap(),
            binding,
        )
        .unwrap();
        let error = commands::execute_with_authority(
            &f.store,
            &command,
            CatalogAuthority::new(base())
                .with_imports(&grant())
                .with_import_activation(&active),
            policy(),
        )
        .unwrap_err();
        assert_eq!(
            error.code,
            CommandStoreErrorCode::Import(ImportErrorCode::AuthorityDenied)
        );
    }
    assert_eq!(count(&f.store, "items"), 0);
    assert_eq!(count(&f.store, "catalog_command_receipts"), 3);
}
#[test]
fn unresolved_evidence_cannot_be_activated_even_by_a_trusted_acknowledgment() {
    let f = Fixture::new();
    let mut b = batch("a");
    b.unresolved_evidence.push(UnresolvedEvidence {
        evidence_id: "timeline".into(),
        kind: EvidenceKind::TimelineVersion,
        data: json!({"start_us":0,"end_us":10}),
    });
    let (digest, map) = fully_staged(&f, &[b]);
    let command = f.envelope(&activate(&digest, &map));
    assert_eq!(
        run_activation(&f.store, &command, &digest, &map)
            .unwrap_err()
            .code,
        CommandStoreErrorCode::Import(ImportErrorCode::ActivationUnqualified)
    );
    assert_eq!(count(&f.store, "items"), 0);
}
#[test]
fn activation_rechecks_validation_generation_even_with_a_new_command_envelope() {
    let f = Fixture::new();
    let (digest, map) = fully_staged(&f, &[batch("a")]);
    catabolic_store::curation::item_put(
        &f.store,
        &json!({"id":"unrelated","kind":"video","metadata":{}}),
    )
    .unwrap();
    let command = f.envelope(&activate(&digest, &map));
    assert_eq!(
        run_activation(&f.store, &command, &digest, &map)
            .unwrap_err()
            .code,
        CommandStoreErrorCode::Import(ImportErrorCode::StaleValidation)
    );
    assert_eq!(count(&f.store, "items"), 1);
}
fn activation_rollback_includes_all_items_generation_search_receipt_and_hold(schema: u32) {
    for target in ["catalog_command_receipts", "catalog_receipt_holds"] {
        let f = Fixture::at(schema);
        let (digest, map) = fully_staged(&f, &[batch("a"), batch("b")]);
        let command = f.envelope(&activate(&digest, &map));
        let before = generation(&f.store);
        f.store.db.execute_batch(&format!("CREATE TEMP TRIGGER fail_activation BEFORE INSERT ON {target} WHEN NEW.operation_id LIKE '%/activate' BEGIN SELECT RAISE(ABORT,'injected'); END;")).unwrap();
        assert!(run_activation(&f.store, &command, &digest, &map).is_err());
        assert_eq!(count(&f.store, "items"), 0);
        if schema >= 35 {
            assert_eq!(count(&f.store, "catalog_search_documents"), 0);
        }
        assert_eq!(generation(&f.store), before);
        assert_eq!(count(&f.store, "catalog_command_receipts"), 4);
        assert_eq!(count(&f.store, "catalog_receipt_holds"), 4);
        f.store
            .db
            .execute_batch("DROP TRIGGER fail_activation")
            .unwrap();
        assert_eq!(
            run_activation(&f.store, &command, &digest, &map)
                .unwrap()
                .code(),
            "committed"
        );
    }
}
#[test]
fn activation_rollback_includes_all_items_generation_search_receipt_and_hold_native34() {
    activation_rollback_includes_all_items_generation_search_receipt_and_hold(34);
}

#[test]
fn activation_rollback_includes_all_items_generation_search_receipt_and_hold_native35() {
    activation_rollback_includes_all_items_generation_search_receipt_and_hold(35);
}

#[test]
fn activation_rollback_includes_all_items_generation_search_receipt_and_hold_native36() {
    activation_rollback_includes_all_items_generation_search_receipt_and_hold(36);
}

#[test]
fn restart_reconciliation_never_admits_on_mismatch_or_missing_ack() {
    use import::{HostCutoverCheckpoint, HostCutoverPhase as P, Reconciliation as R};
    let f = Fixture::new();
    let (digest, map) = fully_staged(&f, &[batch("a")]);
    let command = f.envelope(&activate(&digest, &map));
    let committed = run_activation(&f.store, &command, &digest, &map).unwrap();
    let receipt: import::ImportReceipt =
        serde_json::from_value(result(&committed).clone()).unwrap();
    let mut host = HostCutoverCheckpoint {
        catalog_id: f.store.database_id.clone(),
        incarnation: f.store.catalog_incarnation.clone().unwrap(),
        phase: P::Prepared,
        import_id: "import-1".into(),
        input_digest: digest,
        reference_ack_id: "audit-1".into(),
        mapping_digest: map,
    };
    let reconcile = |receipt: Option<&import::ImportReceipt>, host: &HostCutoverCheckpoint| {
        import::reconcile_activation(
            receipt,
            host,
            &f.store.database_id,
            f.store.catalog_incarnation.as_deref().unwrap(),
        )
    };
    assert_eq!(reconcile(Some(&receipt), &host), R::KeepAdmissionClosed);
    host.phase = P::Validated;
    assert_eq!(reconcile(Some(&receipt), &host), R::FinishHostActivation);
    host.phase = P::Activated;
    assert_eq!(reconcile(Some(&receipt), &host), R::AdmitCatabolicOnly);
    assert_eq!(reconcile(None, &host), R::KeepAdmissionClosed);
    assert_eq!(
        import::reconcile_activation(
            Some(&receipt),
            &host,
            &f.store.database_id,
            "new-incarnation"
        ),
        R::KeepAdmissionClosed
    );
    host.reference_ack_id = "unrelated".into();
    assert_eq!(reconcile(Some(&receipt), &host), R::KeepAdmissionClosed);
}
#[test]
fn process_exit_during_activation_and_after_commit_reconcile_without_duplicate_items() {
    for mode in ["before-commit", "after-commit"] {
        let f = Fixture::new();
        let (digest, map) = fully_staged(&f, &[batch("a"), batch("b")]);
        let intent = f.envelope(&activate(&digest, &map));
        let before = generation(&f.store);
        let Fixture { _root, path, store } = f;
        drop(store);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "import_activation_exit_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("CATABOLIC_IMPORT_EXIT_DB", &path)
            .env("CATABOLIC_IMPORT_EXIT_MODE", mode)
            .env("CATABOLIC_IMPORT_EXIT_DIGEST", &digest)
            .env("CATABOLIC_IMPORT_EXIT_MAP", &map)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(74));
        let store = Store::open(&path, true, false).unwrap();
        let committed = mode == "after-commit";
        assert_eq!(count(&store, "items"), if committed { 2 } else { 0 });
        if !committed {
            assert_eq!(generation(&store), before);
        }
        let outcome = if committed {
            run(&store, &intent, policy()).unwrap()
        } else {
            run_activation(&store, &intent, &digest, &map).unwrap()
        };
        assert_eq!(
            outcome.code(),
            if committed { "replay" } else { "committed" }
        );
        assert_eq!(count(&store, "items"), 2);
        assert_eq!(count(&store, "catalog_command_receipts"), 5);
        assert_eq!(count(&store, "catalog_receipt_holds"), 5);
        drop(_root);
    }
}
#[test]
#[ignore = "process-exit helper; executed by activation crash wrapper"]
fn import_activation_exit_helper() {
    let path = std::env::var("CATABOLIC_IMPORT_EXIT_DB").unwrap();
    let mode = std::env::var("CATABOLIC_IMPORT_EXIT_MODE").unwrap();
    let digest = std::env::var("CATABOLIC_IMPORT_EXIT_DIGEST").unwrap();
    let map = std::env::var("CATABOLIC_IMPORT_EXIT_MAP").unwrap();
    let store = Store::open(std::path::Path::new(&path), true, false).unwrap();
    if mode == "before-commit" {
        store
            .db
            .create_scalar_function(
                "import_exit",
                0,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                |_| -> rusqlite::Result<i64> { std::process::exit(74) },
            )
            .unwrap();
        store.db.execute_batch("CREATE TEMP TRIGGER activation_exit AFTER INSERT ON items BEGIN SELECT import_exit(); END;").unwrap();
    }
    let intent = envelope(&store, &activate(&digest, &map));
    assert_eq!(
        run_activation(&store, &intent, &digest, &map)
            .unwrap()
            .code(),
        "committed"
    );
    std::process::exit(74);
}

#[test]
fn pending_work_retained_workers_and_wal_activation_stay_fail_closed() {
    for setup in [
        "INSERT INTO fallback_probe_slots(id,owner,pid) VALUES(987,'retained-owner',99999)",
        "INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,lease_until,worker_pid) VALUES('retained','default','source','{}','[]',0,1,'complete',0,0,99999)",
        "PRAGMA journal_mode=WAL",
    ] {
        let f = Fixture::new();
        // Establish pending-work evidence before the final generation is
        // validated, so this exercises recovery admission, not staleness.
        f.store.db.execute_batch(setup).unwrap();
        let (digest, map) = fully_staged(&f, &[batch("a")]);
        let command = f.envelope(&activate(&digest, &map));
        let code = run_activation(&f.store, &command, &digest, &map)
            .unwrap_err()
            .code;
        assert_eq!(
            code,
            CommandStoreErrorCode::Import(if setup.starts_with("PRAGMA") {
                ImportErrorCode::ActivationUnqualified
            } else {
                ImportErrorCode::RecoveryRequired
            })
        );
        assert_eq!(count(&f.store, "items"), 0);
        assert_eq!(count(&f.store, "catalog_command_receipts"), 3);
    }
}

#[test]
fn activation_hold_budget_exhaustion_rolls_back_applied_items() {
    let f = Fixture::new();
    let (digest, map) = fully_staged(&f, &[batch("a")]);
    let command = f.envelope(&activate(&digest, &map));
    let before = generation(&f.store);
    let frozen = activation_grant(&f.store, &digest, &map);
    let outcome = commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(base())
            .with_imports(&grant())
            .with_import_activation(&frozen),
        ReceiptPolicy {
            max_hold_records: 3,
            ..policy()
        },
    )
    .unwrap();
    assert_eq!(outcome, CommandOutcome::ReceiptStorageExhausted);
    assert_eq!(count(&f.store, "items"), 0);
    assert_eq!(generation(&f.store), before);
    assert_eq!(count(&f.store, "catalog_command_receipts"), 3);
    assert_eq!(count(&f.store, "catalog_receipt_holds"), 3);
    assert_eq!(
        commands::execute_with_authority(
            &f.store,
            &command,
            CatalogAuthority::new(base())
                .with_imports(&grant())
                .with_import_activation(&frozen),
            policy()
        )
        .unwrap()
        .code(),
        "committed"
    );
}

fn typed_metadata_pin_batch() -> ImportBatch {
    use catabolic_store::catalog_import_typed::*;
    let mut b = batch("a");
    b.items[0].legacy_metadata = json!({});
    b.typed = Some(TypedBatch {
        native: None,
        metadata: vec![ImportedMetadata {
            item_id: "a".into(),
            policies: json!([{"field":"title","value_type":"string","nullable":true,"normalization":"exact"}]),
            contributions: vec![
                ImportedContribution {
                    namespace: "provider-a".into(),
                    record_id: "record".into(),
                    source_revision: 7,
                    kind: ImportedSourceKind::Provider,
                    fields: json!({"title":"Provider A"}),
                    evidence: json!({"observed":"original"}),
                },
                ImportedContribution {
                    namespace: "provider-b".into(),
                    record_id: "record".into(),
                    source_revision: 12,
                    kind: ImportedSourceKind::Provider,
                    fields: json!({"title":"Provider B"}),
                    evidence: json!({}),
                },
            ],
        }],
        pins: vec![ImportedPin {
            item_id: "a".into(),
            namespace: "imdb".into(),
            history: vec![
                ImportedPinRevision {
                    source_revision: 4,
                    decision: catabolic_store::catalog_pins::PinDecision::Set {
                        value: "tt-imported".into(),
                    },
                },
                ImportedPinRevision {
                    source_revision: 8,
                    decision: catabolic_store::catalog_pins::PinDecision::Remove,
                },
            ],
        }],
    });
    b
}
fn typed_grants() -> catabolic_store::catalog_import_typed::TypedAuthority {
    use catabolic_store::{
        catalog_metadata_store::{MetadataAccess, MetadataAuthority},
        catalog_pins::PinAuthority,
    };
    catabolic_store::catalog_import_typed::TypedAuthority {
        physical_files: Default::default(),
        identity: None,
        metadata: vec![
            MetadataAuthority {
                host_namespace: "import-host".into(),
                subject: "a".into(),
                access: MetadataAccess::Initialize,
            },
            MetadataAuthority {
                host_namespace: "import-host".into(),
                subject: "a".into(),
                access: MetadataAccess::Source {
                    namespace: "provider-a".into(),
                    record_id: "record".into(),
                    kind: catabolic_core::catalog_metadata::SourceKind::Provider,
                },
            },
            MetadataAuthority {
                host_namespace: "import-host".into(),
                subject: "a".into(),
                access: MetadataAccess::Source {
                    namespace: "provider-b".into(),
                    record_id: "record".into(),
                    kind: catabolic_core::catalog_metadata::SourceKind::Provider,
                },
            },
        ],
        pins: Some(PinAuthority {
            host_namespace: "import-host".into(),
            readable: Default::default(),
            mutable: [("imdb".into(), ["a".into()].into())].into(),
        }),
    }
}
fn stage_typed(f: &Fixture, b: &ImportBatch) -> String {
    let manifest = SnapshotManifest {
        contract_version: 2,
        import_id: "import-1".into(),
        source_namespace: "fixture-source".into(),
        source_snapshot_id: "frozen-export-1".into(),
        batches: vec![import::describe_batch(b).unwrap()],
    };
    let digest = import::manifest_digest(&manifest).unwrap();
    f.run(&ImportMutation::Prepare {
        manifest,
        input_digest: digest.clone(),
    });
    f.run(&stage(&digest, 0, b.clone()));
    digest
}
fn run_typed(
    f: &Fixture,
    mutation: &ImportMutation,
    grants: &catabolic_store::catalog_import_typed::TypedAuthority,
    active: Option<&import::TrustedImportActivation>,
) -> commands::Result<CommandOutcome> {
    let import_grant = grant();
    let mut authority = CatalogAuthority::new(base())
        .with_imports(&import_grant)
        .with_import_typed(grants);
    // The import grant is owned for the whole call, rather than reconstructed
    // by mutation content. Other owner grants are independently supplied above.

    if let Some(active) = active {
        authority = authority.with_import_activation(active);
    }
    commands::execute_with_authority(&f.store, &f.envelope(mutation), authority, policy())
}
fn typed_metadata_and_pin_histories_rehearse_without_visibility_then_activate_atomically(
    schema: u32,
) {
    let f = Fixture::at(schema);
    let digest = stage_typed(&f, &typed_metadata_pin_batch());
    let grants = typed_grants();
    let before = generation(&f.store);
    let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
    let map = result(&validated)["mapping_digest"].as_str().unwrap();
    assert_eq!(result(&validated)["contract_version"], 2);
    assert_eq!(
        result(&validated)["proposed_typed_mappings"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(count(&f.store, "items"), 0);
    assert_eq!(generation(&f.store), before);
    for table in [
        "catalog_metadata_subjects",
        "catalog_identity_pins",
        "catalog_identity_pin_revisions",
    ] {
        let n: i64 = f
            .store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
    let active = activation_grant(&f.store, &digest, map);
    let committed = run_typed(&f, &activate(&digest, map), &grants, Some(&active)).unwrap();
    assert_eq!(result(&committed)["contract_version"], 2);
    assert_eq!(
        result(&committed)["typed_results"],
        result(&validated)["proposed_typed_mappings"]
    );
    let metadata: String = f
        .store
        .db
        .query_row("SELECT metadata FROM items WHERE id='a'", [], |r| r.get(0))
        .unwrap();
    assert!(
        serde_json::from_str::<Value>(&metadata)
            .unwrap()
            .get("title")
            .is_none()
    );
    let (revision,value):(i64,Option<String>)=f.store.db.query_row("SELECT current_revision,value FROM catalog_identity_pins WHERE item_id='a' AND namespace='imdb'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!((revision, value), (2, None));
    let evidence:String=f.store.db.query_row("SELECT evidence_json FROM catalog_metadata_history WHERE item_id='a' AND namespace='provider-a' AND revision=1",[],|r|r.get(0)).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&evidence).unwrap()["source_revision"],
        7
    );
    assert_eq!(count(&f.store, "catalog_command_receipts"), 4);
    assert_eq!(count(&f.store, "catalog_receipt_holds"), 4);
}
#[test]
fn typed_metadata_and_pin_histories_rehearse_without_visibility_then_activate_atomically_native34()
{
    typed_metadata_and_pin_histories_rehearse_without_visibility_then_activate_atomically(34);
}

#[test]
fn typed_metadata_and_pin_histories_rehearse_without_visibility_then_activate_atomically_native35()
{
    typed_metadata_and_pin_histories_rehearse_without_visibility_then_activate_atomically(35);
}

#[test]
fn typed_metadata_and_pin_histories_rehearse_without_visibility_then_activate_atomically_native36()
{
    typed_metadata_and_pin_histories_rehearse_without_visibility_then_activate_atomically(36);
}

#[test]
fn typed_rehearsal_requires_independent_source_and_pin_grants_and_rolls_back() {
    let f = Fixture::new();
    let digest = stage_typed(&f, &typed_metadata_pin_batch());
    let mut grants = typed_grants();
    grants.metadata.pop();
    assert_eq!(
        run_typed(&f, &validate(&digest), &grants, None)
            .unwrap_err()
            .code,
        CommandStoreErrorCode::Import(ImportErrorCode::AuthorityDenied)
    );
    assert_eq!(count(&f.store, "items"), 0);
    assert_eq!(count(&f.store, "catalog_command_receipts"), 2);
    let mut grants = typed_grants();
    grants.pins = None;
    assert_eq!(
        run_typed(&f, &validate(&digest), &grants, None)
            .unwrap_err()
            .code,
        CommandStoreErrorCode::Import(ImportErrorCode::AuthorityDenied)
    );
    assert_eq!(count(&f.store, "items"), 0);
    assert_eq!(
        run_typed(&f, &validate(&digest), &typed_grants(), None)
            .unwrap()
            .code(),
        "committed"
    );
}
#[test]
fn typed_owner_schema_errors_and_source_kind_spoofs_never_validate() {
    use catabolic_store::catalog_import_typed::ImportedSourceKind;
    for spoof in [false, true] {
        let f = Fixture::new();
        let mut b = typed_metadata_pin_batch();
        let c = &mut b.typed.as_mut().unwrap().metadata[0].contributions[0];
        if spoof {
            c.kind = ImportedSourceKind::Local;
        } else {
            c.fields = json!({"title":42});
        }
        let digest = stage_typed(&f, &b);
        assert!(run_typed(&f, &validate(&digest), &typed_grants(), None).is_err());
        assert_eq!(count(&f.store, "items"), 0);
        assert_eq!(count(&f.store, "catalog_command_receipts"), 2);
    }
}
fn typed_activation_receipt_failure_rolls_back_metadata_decisions_and_search(schema: u32) {
    let f = Fixture::at(schema);
    let digest = stage_typed(&f, &typed_metadata_pin_batch());
    let grants = typed_grants();
    let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
    let map = result(&validated)["mapping_digest"].as_str().unwrap();
    let before = generation(&f.store);
    let active = activation_grant(&f.store, &digest, map);
    f.store.db.execute_batch("CREATE TEMP TRIGGER typed_receipt_fail BEFORE INSERT ON catalog_command_receipts WHEN NEW.operation_id LIKE '%/activate' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(run_typed(&f, &activate(&digest, map), &grants, Some(&active)).is_err());
    assert_eq!(count(&f.store, "items"), 0);
    if schema >= 35 {
        assert_eq!(count(&f.store, "catalog_search_documents"), 0);
    }
    assert_eq!(generation(&f.store), before);
    let pins: i64 = f
        .store
        .db
        .query_row(
            "SELECT count(*) FROM catalog_identity_pin_revisions",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pins, 0);
}

fn seed_physical_snapshot(f: &Fixture) {
    use std::os::unix::fs::MetadataExt;
    let source = f._root.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    catabolic_store::observation::execute(
        &f.store,
        "default",
        "bind",
        &json!({"source":"media","root":source}),
    )
    .unwrap();
    for (file, path) in [("physical", "media.mkv"), ("duplicate", "copy.mkv")] {
        let p = source.join(path);
        std::fs::write(&p, b"same-content-native-import-fixture").unwrap();
        let m = p.metadata().unwrap();
        f.store.transaction(|db| {db.execute("INSERT OR IGNORE INTO scans(id,profile,location,complete,observed,errors) VALUES('fixture-scan','default','media',1,2,'[]')",[])?;db.execute("INSERT INTO files(id,location,path) VALUES(?1,'media',?2)",rusqlite::params![file,path])?;db.execute("INSERT INTO observations VALUES('default',?1,?2,?3,?4,?5,'present','fixture-scan')",rusqlite::params![file,m.len() as i64,m.mtime()*1_000_000_000+m.mtime_nsec(),m.dev() as i64,m.ino() as i64])?;Ok(())}).unwrap();
    }
}
fn native_typed_fixture(
    f: &Fixture,
    overlap: bool,
) -> (
    ImportBatch,
    catabolic_store::catalog_import_typed::TypedAuthority,
) {
    use catabolic_store::catalog_identity_mutation::IdentityAuthority;
    use catabolic_store::catalog_import_typed::*;
    let grants = TypedAuthority {
        physical_files: [(
            "default".into(),
            ["physical".into(), "duplicate".into()].into(),
        )]
        .into(),
        identity: Some(IdentityAuthority {
            authority_namespace: "import-host".into(),
            context_keys: ["editorial-import".into()].into(),
            allow_declared_evidence: true,
            allow_verified_evidence: false,
        }),
        ..Default::default()
    };
    let physical =
        capture_existing_occurrence(&f.store, "source-physical", "default", "physical", &grants)
            .unwrap();
    let mut b = batch("edition");
    b.items.extend([item("episode-one"), item("episode-two")]);
    let timelines:Vec<Value>=["one","two"].into_iter().map(|n|json!({"timeline_id":format!("timeline-{n}"),"edition_item_id":"edition","expected_revision":null,"identity_state":"provisional","coverage":{"start_us":0,"end_us":10_000_000}})).collect();
    let versions:Vec<Value>=[("one",0), ("two",if overlap{5_000_000}else{10_000_000})].into_iter().map(|(n,start)|json!({"expected_revision":null,"expected_timeline_revision":1,"version":{"contract_version":1,"version_id":format!("version-{n}"),"association_item_id":format!("episode-{n}"),"edition_item_id":"edition","timeline_id":format!("timeline-{n}"),"state":"ready","declared_coverage":{"start_us":0,"end_us":10_000_000},"bindings":[{"binding_id":format!("binding-{n}"),"association_id":format!("source-association-{n}"),"file":physical.exact,"part":1,"source":{"kind":"slice","interval":{"start_us":start,"end_us":start+10_000_000}},"timeline_interval":{"start_us":0,"end_us":10_000_000}}]},"equivalence":null,"expected_binding_revisions":{format!("binding-{n}"):null}})).collect();
    let mutation = json!({"contract_version":1,"profile":"default","context":{"id":"context-import","authority_namespace":"import-host","context_key":"editorial-import","expected_revision":null},"timelines":timelines,"versions":versions,"evidence":[]});
    b.typed = Some(TypedBatch {
        native: Some(NativeIdentityBatch {
            occurrences: vec![physical],
            associations: ["one", "two"]
                .into_iter()
                .map(|n| ImportedAssociation {
                    source_association_id: format!("source-association-{n}"),
                    source_occurrence_id: "source-physical".into(),
                    item_id: format!("episode-{n}"),
                    part: 1,
                })
                .collect(),
            mutations: vec![mutation],
        }),
        ..Default::default()
    });
    (b, grants)
}
fn typed_native_identity_rehearses_and_activates_exact_revision_two_episode_bindings(schema: u32) {
    let f = Fixture::at(schema);
    seed_physical_snapshot(&f);
    let (b, grants) = native_typed_fixture(&f, false);
    let digest = stage_typed(&f, &b);
    let before = generation(&f.store);
    let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
    assert_eq!(count(&f.store, "items"), 0);
    assert_eq!(generation(&f.store), before);
    let map = result(&validated)["mapping_digest"].as_str().unwrap();
    let active = activation_grant(&f.store, &digest, map);
    let committed = run_typed(&f, &activate(&digest, map), &grants, Some(&active)).unwrap();
    assert_eq!(
        result(&committed)["typed_results"],
        result(&validated)["proposed_typed_mappings"]
    );
    let loaded = catabolic_store::catalog_identity_mutation::load_manifest(
        &f.store,
        "default",
        &["version-one".into(), "version-two".into()],
    )
    .unwrap();
    assert_eq!(loaded.versions.len(), 2);
    let rows: i64 = f
        .store
        .db
        .query_row("SELECT count(*) FROM catalog_bindings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 2);
    assert_eq!(count(&f.store, "files"), 2);
}
#[test]
fn typed_activation_receipt_failure_rolls_back_metadata_decisions_and_search_native34() {
    typed_activation_receipt_failure_rolls_back_metadata_decisions_and_search(34);
}

#[test]
fn typed_activation_receipt_failure_rolls_back_metadata_decisions_and_search_native35() {
    typed_activation_receipt_failure_rolls_back_metadata_decisions_and_search(35);
}

#[test]
fn typed_activation_receipt_failure_rolls_back_metadata_decisions_and_search_native36() {
    typed_activation_receipt_failure_rolls_back_metadata_decisions_and_search(36);
}

#[test]
fn typed_native_identity_rehearses_and_activates_exact_revision_two_episode_bindings_native34() {
    typed_native_identity_rehearses_and_activates_exact_revision_two_episode_bindings(34);
}

#[test]
fn typed_native_identity_rehearses_and_activates_exact_revision_two_episode_bindings_native35() {
    typed_native_identity_rehearses_and_activates_exact_revision_two_episode_bindings(35);
}

#[test]
fn typed_native_identity_rehearses_and_activates_exact_revision_two_episode_bindings_native36() {
    typed_native_identity_rehearses_and_activates_exact_revision_two_episode_bindings(36);
}

#[test]
fn typed_identity_overlap_missing_authority_and_revision_change_refuse_whole_snapshot() {
    for failure in ["overlap", "authority", "replacement"] {
        let f = Fixture::new();
        seed_physical_snapshot(&f);
        let (b, mut grants) = native_typed_fixture(&f, failure == "overlap");
        let digest = stage_typed(&f, &b);
        if failure == "authority" {
            grants.identity = None;
        }
        if failure == "replacement" {
            f.store
                .transaction(|db| {
                    db.execute(
                        "UPDATE observations SET size=size+1 WHERE file_id='physical'",
                        [],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        assert!(
            run_typed(&f, &validate(&digest), &grants, None).is_err(),
            "{failure}"
        );
        assert_eq!(count(&f.store, "items"), 0);
        let bindings: i64 = f
            .store
            .db
            .query_row("SELECT count(*) FROM catalog_bindings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bindings, 0);
        assert_eq!(count(&f.store, "catalog_command_receipts"), 2);
    }
}
#[test]
fn typed_exact_occurrence_maps_keep_duplicates_distinct_and_unavailable_unknown() {
    use catabolic_store::catalog_import_typed::*;
    let f = Fixture::new();
    seed_physical_snapshot(&f);
    let (_, grants) = native_typed_fixture(&f, false);
    f.store
        .transaction(|db| {
            db.execute(
                "UPDATE observations SET status='missing' WHERE file_id='duplicate'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let mut b = batch("a");
    b.typed = Some(TypedBatch {
        native: Some(NativeIdentityBatch {
            occurrences: vec![
                capture_existing_occurrence(&f.store, "original", "default", "physical", &grants)
                    .unwrap(),
                capture_existing_occurrence(&f.store, "copy", "default", "duplicate", &grants)
                    .unwrap(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    });
    let digest = stage_typed(&f, &b);
    let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
    let mappings = result(&validated)["proposed_typed_mappings"]
        .as_array()
        .unwrap();
    assert_eq!(mappings.len(), 2);
    assert_ne!(mappings[0]["file_id"], mappings[1]["file_id"]);
    assert_eq!(mappings[1]["observed_status"], "missing");
    assert_eq!(mappings[1]["source_availability"], "unknown");
    let map = result(&validated)["mapping_digest"].as_str().unwrap();
    let active = activation_grant(&f.store, &digest, map);
    run_typed(&f, &activate(&digest, map), &grants, Some(&active)).unwrap();
    assert_eq!(count(&f.store, "files"), 2);
}

#[test]
fn typed_real_observation_owner_preserves_rename_replacement_duplicates_and_unavailability() {
    use catabolic_store::catalog_import_typed::*;
    let f = Fixture::new();
    let source = f._root.path().join("real-source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("original.txt"), b"equal content").unwrap();
    std::fs::write(source.join("copy.txt"), b"equal content").unwrap();
    catabolic_store::observation::execute(
        &f.store,
        "default",
        "bind",
        &json!({"source":"media","root":source}),
    )
    .unwrap();
    let scan = || {
        catabolic_store::observation::execute(
            &f.store,
            "default",
            "scan",
            &json!({"source":"media","reuse_completed":false}),
        )
        .unwrap()
    };
    assert_eq!(scan()["complete"], true);
    let id_at = |path: &str| -> String {
        f.store
            .db
            .query_row(
                "SELECT id FROM files WHERE location='media' AND path=?1",
                [path],
                |r| r.get(0),
            )
            .unwrap()
    };
    let original = id_at("original.txt");
    let copy = id_at("copy.txt");
    assert_ne!(original, copy);
    let mut grants = TypedAuthority {
        physical_files: [("default".into(), [original.clone(), copy.clone()].into())].into(),
        ..Default::default()
    };
    let original_map =
        capture_existing_occurrence(&f.store, "source-original", "default", &original, &grants)
            .unwrap();
    let copy_map =
        capture_existing_occurrence(&f.store, "source-copy", "default", &copy, &grants).unwrap();
    assert_ne!(original_map.exact.file_id, copy_map.exact.file_id);
    std::fs::rename(source.join("original.txt"), source.join("renamed.txt")).unwrap();
    assert_eq!(scan()["complete"], true);
    let renamed = id_at("renamed.txt");
    grants
        .physical_files
        .get_mut("default")
        .unwrap()
        .insert(renamed.clone());
    assert_ne!(renamed, original);
    let renamed_before =
        capture_existing_occurrence(&f.store, "source-renamed", "default", &renamed, &grants)
            .unwrap();
    std::fs::write(
        source.join("renamed.txt"),
        b"new same-path replacement of a different length",
    )
    .unwrap();
    assert_eq!(scan()["complete"], true);
    let renamed_after =
        capture_existing_occurrence(&f.store, "source-renamed", "default", &renamed, &grants)
            .unwrap();
    assert_eq!(renamed_after.exact.file_id, renamed_before.exact.file_id);
    assert_ne!(renamed_after.exact.revision, renamed_before.exact.revision);
    let mut stale = batch("a");
    stale.typed = Some(TypedBatch {
        native: Some(NativeIdentityBatch {
            occurrences: vec![renamed_before],
            ..Default::default()
        }),
        ..Default::default()
    });
    let stale_digest = stage_typed(&f, &stale);
    assert!(run_typed(&f, &validate(&stale_digest), &grants, None).is_err());
    assert_eq!(count(&f.store, "items"), 0);
    let retained = count(&f.store, "files");
    std::fs::rename(&source, f._root.path().join("offline-source")).unwrap();
    let unavailable = scan();
    assert_eq!(unavailable["complete"], false);
    assert_eq!(unavailable["scans"][0]["availability"], "unavailable");
    assert_eq!(count(&f.store, "files"), retained);
    // Real source work remains retained. This fixture never clears a PID/claim
    // or invents a cleanup witness to make an import activation pass.
    let owned: i64 = f
        .store
        .db
        .query_row(
            "SELECT count(*) FROM observation_jobs WHERE worker_pid IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(owned > 0);
}

#[test]
fn typed_identity_requires_every_association_to_have_one_exact_binding() {
    let f = Fixture::new();
    seed_physical_snapshot(&f);
    let (mut b, grants) = native_typed_fixture(&f, false);
    b.typed
        .as_mut()
        .unwrap()
        .native
        .as_mut()
        .unwrap()
        .mutations
        .clear();
    let digest = stage_typed(&f, &b);
    assert!(run_typed(&f, &validate(&digest), &grants, None).is_err());
    assert_eq!(count(&f.store, "items"), 0);
    let n: i64 = f
        .store
        .db
        .query_row("SELECT count(*) FROM item_files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}
#[test]
fn typed_activation_lost_reply_replays_without_new_owner_grants_or_history() {
    let f = Fixture::new();
    let digest = stage_typed(&f, &typed_metadata_pin_batch());
    let grants = typed_grants();
    let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
    let map = result(&validated)["mapping_digest"].as_str().unwrap();
    let mutation = activate(&digest, map);
    let intent = f.envelope(&mutation);
    let frozen = activation_grant(&f.store, &digest, map);
    let first = run_typed(&f, &mutation, &grants, Some(&frozen)).unwrap();
    let before = generation(&f.store);
    let Fixture { _root, path, store } = f;
    drop(store);
    let store = Store::open(&path, true, false).unwrap();
    let replay = commands::execute(
        &store,
        &intent,
        TrustedCommandAuthority {
            allow_new_commands: false,
            ..base()
        },
        policy(),
    )
    .unwrap();
    assert_eq!(replay.code(), "replay");
    assert_eq!(result(&first), result(&replay));
    assert_eq!(generation(&store), before);
    let revisions: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM catalog_identity_pin_revisions",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(revisions, 2);
    drop(_root);
}
#[test]
fn version_one_canonical_shapes_stay_frozen_and_refuse_typed_batches() {
    let ordinary = batch("a");
    let value = serde_json::to_value(&ordinary).unwrap();
    assert!(value.get("typed").is_none());
    assert!(
        serde_json::to_value(import::describe_batch(&ordinary).unwrap())
            .unwrap()
            .get("typed_records")
            .is_none()
    );
    let b = typed_metadata_pin_batch();
    let manifest = SnapshotManifest {
        contract_version: 1,
        import_id: "import-1".into(),
        source_namespace: "fixture-source".into(),
        source_snapshot_id: "frozen-export-1".into(),
        batches: vec![import::describe_batch(&b).unwrap()],
    };
    assert_eq!(
        import::manifest_digest(&manifest).unwrap_err().code,
        ImportErrorCode::InvalidInput
    );
}

fn typed_process_exit_preserves_owner_atomicity(schema: u32) {
    for mode in ["before-commit", "after-commit"] {
        let f = Fixture::at(schema);
        let digest = stage_typed(&f, &typed_metadata_pin_batch());
        let grants = typed_grants();
        let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
        let map = result(&validated)["mapping_digest"]
            .as_str()
            .unwrap()
            .to_owned();
        let intent = f.envelope(&activate(&digest, &map));
        let before = generation(&f.store);
        let Fixture { _root, path, store } = f;
        drop(store);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "typed_import_exit_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("CATABOLIC_IMPORT_EXIT_DB", &path)
            .env("CATABOLIC_IMPORT_EXIT_MODE", mode)
            .env("CATABOLIC_IMPORT_EXIT_DIGEST", &digest)
            .env("CATABOLIC_IMPORT_EXIT_MAP", &map)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(75));
        let store = Store::open(&path, true, false).unwrap();
        let committed = mode == "after-commit";
        assert_eq!(count(&store, "items"), if committed { 1 } else { 0 });
        for (table, expected) in [
            ("catalog_metadata_subjects", 1),
            ("catalog_identity_pin_revisions", 2),
        ] {
            let rows: i64 = store
                .db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, if committed { expected } else { 0 });
        }
        if !committed {
            assert_eq!(generation(&store), before);
        }
        let outcome = if committed {
            commands::execute(
                &store,
                &intent,
                TrustedCommandAuthority {
                    allow_new_commands: false,
                    ..base()
                },
                policy(),
            )
            .unwrap()
        } else {
            let active = activation_grant(&store, &digest, &map);
            commands::execute_with_authority(
                &store,
                &intent,
                CatalogAuthority::new(base())
                    .with_imports(&grant())
                    .with_import_typed(&grants)
                    .with_import_activation(&active),
                policy(),
            )
            .unwrap()
        };
        assert_eq!(
            outcome.code(),
            if committed { "replay" } else { "committed" }
        );
        assert_eq!(count(&store, "catalog_command_receipts"), 4);
        assert_eq!(count(&store, "catalog_receipt_holds"), 4);
        let rows: i64 = store
            .db
            .query_row(
                "SELECT count(*) FROM catalog_identity_pin_revisions",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 2);
        drop(_root);
    }
}
#[test]
fn typed_process_exit_preserves_owner_atomicity_native34() {
    typed_process_exit_preserves_owner_atomicity(34);
}
#[test]
fn typed_process_exit_preserves_owner_atomicity_native35() {
    typed_process_exit_preserves_owner_atomicity(35);
}
#[test]
fn typed_process_exit_preserves_owner_atomicity_native36() {
    typed_process_exit_preserves_owner_atomicity(36);
}
#[test]
#[ignore = "process-exit helper; executed six times by typed crash wrappers"]
fn typed_import_exit_helper() {
    let path = std::env::var("CATABOLIC_IMPORT_EXIT_DB").unwrap();
    let mode = std::env::var("CATABOLIC_IMPORT_EXIT_MODE").unwrap();
    let digest = std::env::var("CATABOLIC_IMPORT_EXIT_DIGEST").unwrap();
    let map = std::env::var("CATABOLIC_IMPORT_EXIT_MAP").unwrap();
    let store = Store::open(std::path::Path::new(&path), true, false).unwrap();
    if mode == "before-commit" {
        store
            .db
            .create_scalar_function(
                "typed_import_exit",
                0,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                |_| -> rusqlite::Result<i64> { std::process::exit(75) },
            )
            .unwrap();
        store.db.execute_batch("CREATE TEMP TRIGGER typed_exit AFTER INSERT ON catalog_identity_pin_revisions BEGIN SELECT typed_import_exit(); END;").unwrap();
    }
    let intent = envelope(&store, &activate(&digest, &map));
    let active = activation_grant(&store, &digest, &map);
    assert_eq!(
        commands::execute_with_authority(
            &store,
            &intent,
            CatalogAuthority::new(base())
                .with_imports(&grant())
                .with_import_typed(&typed_grants())
                .with_import_activation(&active),
            policy()
        )
        .unwrap()
        .code(),
        "committed"
    );
    std::process::exit(75);
}

#[test]
fn typed_metadata_and_pin_histories_rehearse_without_visibility_then_activate_atomically_native37()
{
    typed_metadata_and_pin_histories_rehearse_without_visibility_then_activate_atomically(37);
}
#[test]
fn typed_native_identity_rehearses_and_activates_exact_revision_two_episode_bindings_native37() {
    typed_native_identity_rehearses_and_activates_exact_revision_two_episode_bindings(37);
}
#[test]
fn typed_activation_receipt_failure_rolls_back_metadata_decisions_and_search_native37() {
    typed_activation_receipt_failure_rolls_back_metadata_decisions_and_search(37);
}
#[test]
fn activation_rollback_includes_all_items_generation_search_receipt_and_hold_native37() {
    activation_rollback_includes_all_items_generation_search_receipt_and_hold(37);
}
#[test]
fn typed_process_exit_preserves_owner_atomicity_native37() {
    typed_process_exit_preserves_owner_atomicity(37);
}

fn mixed_grants() -> catabolic_store::catalog_import_typed::TypedAuthority {
    let mut grants = typed_grants();
    for metadata in &mut grants.metadata {
        metadata.subject = "episode-one".into();
    }
    grants.pins.as_mut().unwrap().mutable = [("imdb".into(), ["episode-one".into()].into())].into();
    grants.physical_files = [(
        "default".into(),
        ["physical".into(), "duplicate".into()].into(),
    )]
    .into();
    grants.identity = Some(
        catabolic_store::catalog_identity_mutation::IdentityAuthority {
            authority_namespace: "import-host".into(),
            context_keys: ["editorial-import".into()].into(),
            allow_declared_evidence: true,
            allow_verified_evidence: false,
        },
    );
    grants
}
fn mixed_batches(f: &Fixture, split: bool) -> Vec<ImportBatch> {
    use catabolic_store::catalog_import_typed::TypedBatch;
    let (mut native, _) = native_typed_fixture(f, false);
    native
        .items
        .iter_mut()
        .find(|i| i.item_id == "episode-one")
        .unwrap()
        .legacy_metadata = json!({});
    let mut metadata = typed_metadata_pin_batch().typed.unwrap();
    metadata.metadata[0].item_id = "episode-one".into();
    metadata.pins[0].item_id = "episode-one".into();
    if split {
        let child_items = native.items.split_off(1);
        let identities = native.typed.take().unwrap();
        native.typed = Some(metadata);
        vec![
            native,
            ImportBatch {
                items: child_items,
                unresolved_evidence: vec![],
                typed: Some(identities),
            },
        ]
    } else {
        let identities = native.typed.take().unwrap().native;
        native.typed = Some(TypedBatch {
            native: identities,
            ..metadata
        });
        vec![native]
    }
}
fn stage_typed_batches(f: &Fixture, batches: &[ImportBatch]) -> String {
    let manifest = SnapshotManifest {
        contract_version: 2,
        import_id: "import-1".into(),
        source_namespace: "fixture-source".into(),
        source_snapshot_id: "frozen-export-1".into(),
        batches: batches
            .iter()
            .map(|b| import::describe_batch(b).unwrap())
            .collect(),
    };
    let digest = import::manifest_digest(&manifest).unwrap();
    f.run(&ImportMutation::Prepare {
        manifest,
        input_digest: digest.clone(),
    });
    for (index, batch) in batches.iter().enumerate() {
        f.run(&stage(&digest, index as u32, batch.clone()));
    }
    digest
}
fn mixed_counts(store: &Store) -> Vec<i64> {
    [
        "items",
        "catalog_metadata_subjects",
        "catalog_metadata_history",
        "catalog_identity_pin_revisions",
        "catalog_timelines",
        "catalog_versions",
        "catalog_bindings",
    ]
    .iter()
    .map(|table| {
        store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    })
    .collect()
}
#[test]
fn mixed_metadata_pin_and_binding_activation_rolls_back_late_native_failure_single_and_cross_batch()
{
    for schema in [34, 35, 36, 37] {
        for split in [false, true] {
            let f = Fixture::at(schema);
            seed_physical_snapshot(&f);
            let batches = mixed_batches(&f, split);
            let digest = stage_typed_batches(&f, &batches);
            let grants = mixed_grants();
            let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
            let map = result(&validated)["mapping_digest"].as_str().unwrap();
            assert_eq!(mixed_counts(&f.store), vec![0; 7]);
            let before = generation(&f.store);
            let active = activation_grant(&f.store, &digest, map);
            f.store.db.execute_batch("CREATE TEMP TRIGGER fail_mixed_binding AFTER INSERT ON catalog_bindings BEGIN SELECT RAISE(ABORT,'late native failure'); END;").unwrap();
            assert!(run_typed(&f, &activate(&digest, map), &grants, Some(&active)).is_err());
            assert_eq!(mixed_counts(&f.store), vec![0; 7]);
            assert_eq!(generation(&f.store), before);
            if schema >= 35 {
                assert_eq!(count(&f.store, "catalog_search_documents"), 0);
            }
            assert_eq!(
                count(&f.store, "catalog_command_receipts"),
                batches.len() as i64 + 2
            );
            assert_eq!(
                count(&f.store, "catalog_receipt_holds"),
                batches.len() as i64 + 2
            );
            f.store
                .db
                .execute_batch("DROP TRIGGER fail_mixed_binding")
                .unwrap();
            let committed = run_typed(&f, &activate(&digest, map), &grants, Some(&active)).unwrap();
            assert_eq!(
                result(&committed)["typed_results"],
                result(&validated)["proposed_typed_mappings"]
            );
            assert_eq!(mixed_counts(&f.store), vec![3, 1, 3, 2, 2, 2, 2]);
            assert_eq!(
                count(&f.store, "catalog_command_receipts"),
                batches.len() as i64 + 3
            );
            assert_eq!(
                count(&f.store, "catalog_receipt_holds"),
                batches.len() as i64 + 3
            );
        }
    }
}
#[test]
fn mixed_native_binding_process_exit_resumes_or_replays_all_owner_histories() {
    for schema in [34, 35, 36, 37] {
        for mode in ["before-commit", "after-commit"] {
            let f = Fixture::at(schema);
            seed_physical_snapshot(&f);
            let batches = mixed_batches(&f, false);
            let digest = stage_typed_batches(&f, &batches);
            let grants = mixed_grants();
            let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
            let map = result(&validated)["mapping_digest"]
                .as_str()
                .unwrap()
                .to_owned();
            let intent = f.envelope(&activate(&digest, &map));
            let before = generation(&f.store);
            let Fixture { _root, path, store } = f;
            drop(store);
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "mixed_native_exit_helper",
                    "--ignored",
                    "--nocapture",
                ])
                .env("CATABOLIC_IMPORT_EXIT_DB", &path)
                .env("CATABOLIC_IMPORT_EXIT_MODE", mode)
                .env("CATABOLIC_IMPORT_EXIT_DIGEST", &digest)
                .env("CATABOLIC_IMPORT_EXIT_MAP", &map)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(76));
            let store = Store::open(&path, true, false).unwrap();
            let committed = mode == "after-commit";
            assert_eq!(
                mixed_counts(&store),
                if committed {
                    vec![3, 1, 3, 2, 2, 2, 2]
                } else {
                    vec![0; 7]
                }
            );
            if !committed {
                assert_eq!(generation(&store), before);
            }
            let outcome = if committed {
                commands::execute(
                    &store,
                    &intent,
                    TrustedCommandAuthority {
                        allow_new_commands: false,
                        ..base()
                    },
                    policy(),
                )
                .unwrap()
            } else {
                let active = activation_grant(&store, &digest, &map);
                commands::execute_with_authority(
                    &store,
                    &intent,
                    CatalogAuthority::new(base())
                        .with_imports(&grant())
                        .with_import_typed(&grants)
                        .with_import_activation(&active),
                    policy(),
                )
                .unwrap()
            };
            assert_eq!(
                outcome.code(),
                if committed { "replay" } else { "committed" }
            );
            assert_eq!(mixed_counts(&store), vec![3, 1, 3, 2, 2, 2, 2]);
            assert_eq!(count(&store, "catalog_command_receipts"), 4);
            assert_eq!(count(&store, "catalog_receipt_holds"), 4);
            drop(_root);
        }
    }
}
#[test]
#[ignore = "real exit helper, executed eight times by the mixed native-binding wrapper"]
fn mixed_native_exit_helper() {
    let path = std::env::var("CATABOLIC_IMPORT_EXIT_DB").unwrap();
    let mode = std::env::var("CATABOLIC_IMPORT_EXIT_MODE").unwrap();
    let digest = std::env::var("CATABOLIC_IMPORT_EXIT_DIGEST").unwrap();
    let map = std::env::var("CATABOLIC_IMPORT_EXIT_MAP").unwrap();
    let store = Store::open(std::path::Path::new(&path), true, false).unwrap();
    if mode == "before-commit" {
        store
            .db
            .create_scalar_function(
                "mixed_native_exit",
                0,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                |_| -> rusqlite::Result<i64> { std::process::exit(76) },
            )
            .unwrap();
        store.db.execute_batch("CREATE TEMP TRIGGER mixed_exit AFTER INSERT ON catalog_bindings BEGIN SELECT mixed_native_exit(); END;").unwrap();
    }
    let active = activation_grant(&store, &digest, &map);
    let intent = envelope(&store, &activate(&digest, &map));
    assert_eq!(
        commands::execute_with_authority(
            &store,
            &intent,
            CatalogAuthority::new(base())
                .with_imports(&grant())
                .with_import_typed(&mixed_grants())
                .with_import_activation(&active),
            policy()
        )
        .unwrap()
        .code(),
        "committed"
    );
    std::process::exit(76);
}

#[path = "native_import_support/contribution_semantics.rs"]
mod contribution_semantics;
#[path = "native_import_support/representations.rs"]
mod representations;
