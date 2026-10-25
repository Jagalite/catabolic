use super::*;
use crate::{
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
        TrustedReceiptMaintenance,
    },
    catalog_import::{
        ActivationBinding, ImportAuthority, ImportBatch, ImportItem, ImportMutation,
        SnapshotManifest, TrustedImportActivation,
    },
    catalog_import_typed::{
        ImportedContribution, ImportedMetadata, ImportedSourceKind, TypedAuthority, TypedBatch,
    },
    catalog_metadata_store::{MetadataAccess, MetadataAuthority},
    catalog_nfo_attribution::ProofBudget,
    migration,
};
use catabolic_core::{
    catalog_command::ReceiptVisibility, catalog_host_lease::HostAuthorization,
    catalog_metadata::SourceKind,
};
use std::{collections::BTreeSet, time::Duration};
const SUBJECT: &str = "motion.library:library";
const IMPORT: &str = "library-part";
const SOURCE: &str = "motion.schema68.export.v4";
fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap()
}
fn base() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn envelope(store: &Store, mutation: &ImportMutation) -> CommandEnvelope {
    let step = match mutation {
        ImportMutation::Prepare { .. } => ImportStep::Prepare,
        ImportMutation::Stage { batch_index, .. } => ImportStep::Batch(*batch_index),
        ImportMutation::Validate { .. } => ImportStep::Validate,
        ImportMutation::Activate { .. } => ImportStep::Activate,
    };
    CommandEnvelope::new(
        1,
        CommandKey::new(
            &store.database_id,
            HOST,
            catalog_import::operation_id(IMPORT, step).unwrap(),
        )
        .unwrap(),
        store.catalog_incarnation.clone().unwrap(),
        generation(store),
        &json!({"operation":"import","mutation":mutation}).to_string(),
    )
    .unwrap()
}
pub(super) struct Fixture {
    pub(super) root: tempfile::TempDir,
    pub(super) store: Store,
    pub(super) activation: CommandEnvelope,
    pub(super) manifest: SnapshotManifest,
    pub(super) batches: Vec<ImportBatch>,
}
impl Fixture {
    pub(super) fn new() -> Self {
        Self::build(false, false)
    }
    pub(super) fn with_refused_activation() -> Self {
        Self::build(true, false)
    }
    pub(super) fn with_mixed_archive(refuse_activation: bool) -> Self {
        Self::build(refuse_activation, true)
    }
    fn build(refuse_activation: bool, mixed: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("library.db");
        migration::initialize_native_at(&path, 40).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let row = json!({"id":"library","revision":3,"name":"Imported library","kind":"mixed","language":""});
        let definition = json!({"contract_version":1,"id":"library","catalog_revision":3,"name":"Imported library","kind":"mixed","language":"","source_ids":["source"]});
        let batch = ImportBatch {
            items: vec![ImportItem {
                source_id: SUBJECT.into(),
                item_id: SUBJECT.into(),
                kind: "custom:motion_library".into(),
                legacy_metadata: json!({}),
            }],
            unresolved_evidence: vec![],
            typed: Some(TypedBatch {
                metadata: vec![ImportedMetadata {
                    contract_version: 2,
                    system_fields: vec!["motion_library".into()],
                    item_id: SUBJECT.into(),
                    policies: json!([{"field":"title","value_type":"json","nullable":false,"normalization":"exact"},{"field":"motion_library","value_type":"json","nullable":false,"normalization":"exact"}]),
                    contributions: vec![ImportedContribution {
                        namespace: "motion.structure".into(),
                        record_id: "source_library".into(),
                        source_revision: 3,
                        kind: ImportedSourceKind::Scan,
                        fields: json!({"title":"Imported library","motion_library":definition}),
                        evidence: json!({"source_table":"catalog_libraries","source_row":row,"meaning":"exact definition; source_ids do not grant access or native query authority"}),
                    }],
                }],
                ..Default::default()
            }),
        };
        let archived_batches = if mixed {
            vec![
                batch,
                ImportBatch {
                    items: vec![ImportItem {
                        source_id: "unrelated".into(),
                        item_id: "unrelated".into(),
                        kind: "custom:unrelated".into(),
                        legacy_metadata: json!({"title":"Unrelated genuine imported item"}),
                    }],
                    unresolved_evidence: vec![],
                    typed: None,
                },
            ]
        } else {
            vec![batch]
        };
        let manifest = SnapshotManifest {
            contract_version: 2,
            import_id: IMPORT.into(),
            source_namespace: SOURCE.into(),
            source_snapshot_id: "actual-library-snapshot".into(),
            batches: archived_batches
                .iter()
                .map(|batch| catalog_import::describe_batch(batch).unwrap())
                .collect(),
        };
        let archived_manifest = manifest.clone();
        let digest = catalog_import::manifest_digest(&manifest).unwrap();
        let import = ImportAuthority {
            host_namespace: HOST.into(),
            import_id: IMPORT.into(),
            source_namespace: SOURCE.into(),
        };
        let system = BTreeSet::from(["motion_library".into()]);
        let typed = TypedAuthority {
            metadata: vec![
                MetadataAuthority {
                    host_namespace: HOST.into(),
                    subject: SUBJECT.into(),
                    access: MetadataAccess::InitializeV2 {
                        system_fields: system.clone(),
                    },
                },
                MetadataAuthority {
                    host_namespace: HOST.into(),
                    subject: SUBJECT.into(),
                    access: MetadataAccess::SourceV2 {
                        namespace: "motion.structure".into(),
                        record_id: "source_library".into(),
                        kind: SourceKind::Scan,
                        system_fields: system,
                    },
                },
            ],
            ..Default::default()
        };
        let policy = ReceiptPolicy {
            retention: Duration::ZERO,
            max_receipts: 100,
            max_logical_bytes: 16 * 1024 * 1024,
            max_hold_records: 100,
        };
        let run = |mutation: &ImportMutation, activation: Option<&TrustedImportActivation>| {
            let command = envelope(&store, mutation);
            let mut authority = CatalogAuthority::new(base())
                .with_imports(&import)
                .with_import_typed(&typed);
            if let Some(activation) = activation {
                authority = authority.with_import_activation(activation);
            }
            commands::execute_with_authority(&store, &command, authority, policy).unwrap()
        };
        run(
            &ImportMutation::Prepare {
                manifest: manifest.clone(),
                input_digest: digest.clone(),
            },
            None,
        );
        for (index, batch) in archived_batches.iter().enumerate() {
            run(
                &ImportMutation::Stage {
                    import_id: IMPORT.into(),
                    input_digest: digest.clone(),
                    batch_index: index as u32,
                    batch: batch.clone(),
                },
                None,
            );
        }
        let validation = run(
            &ImportMutation::Validate {
                import_id: IMPORT.into(),
                input_digest: digest.clone(),
            },
            None,
        );
        let CommandOutcome::Committed { result, .. } = validation else {
            panic!("validation did not commit");
        };
        let mapping = result["mapping_digest"].as_str().unwrap().to_owned();
        let gate = HostAuthorization::new(HOST.into(), "library-freeze".into()).unwrap();
        let grant = TrustedImportActivation::after_drain(
            &gate,
            gate.begin_revocation().unwrap(),
            ActivationBinding {
                catalog_id: store.database_id.clone(),
                incarnation: store.catalog_incarnation.clone().unwrap(),
                import_id: IMPORT.into(),
                input_digest: digest.clone(),
                source_snapshot_id: manifest.source_snapshot_id,
                validation_generation: generation(&store),
                reference_ack_id: "actual-library-audit".into(),
                mapping_digest: mapping.clone(),
            },
        )
        .unwrap();
        let mutation = ImportMutation::Activate {
            import_id: IMPORT.into(),
            input_digest: digest,
            reference_ack_id: "actual-library-audit".into(),
            mapping_digest: if refuse_activation {
                "0".repeat(64)
            } else {
                mapping
            },
        };
        let activation = envelope(&store, &mutation);
        if refuse_activation {
            let before = generation(&store);
            let authority = CatalogAuthority::new(base())
                .with_imports(&import)
                .with_import_typed(&typed)
                .with_import_activation(&grant);
            assert!(
                commands::execute_with_authority(&store, &activation, authority, policy).is_err(),
                "real Activate must refuse a mapping different from its retained gate"
            );
            assert_eq!(generation(&store), before);
            let rows: i64 = store
                .db
                .query_row(
                    "SELECT count(*) FROM catalog_command_receipts WHERE operation_id=?",
                    [activation.key().operation_id()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(rows, 0);
        } else {
            run(&mutation, Some(&grant));
        }
        // Every successful import command owns its own reconciliation hold.
        // Release each through the ordinary40 maintenance owner before the
        // candidate installer; later expiry tests still use actual compaction.
        let mut released = 0;
        for step in [ImportStep::Prepare, ImportStep::Validate]
            .into_iter()
            .chain((0..archived_batches.len()).map(|i| ImportStep::Batch(i as u32)))
            .chain((!refuse_activation).then_some(ImportStep::Activate))
        {
            let key = CommandKey::new(
                &store.database_id,
                HOST,
                catalog_import::operation_id(IMPORT, step).unwrap(),
            )
            .unwrap();
            assert_eq!(
                commands::release_hold(
                    &store,
                    &key,
                    TrustedReceiptMaintenance {
                        host_namespace: HOST,
                        holder_namespace: catalog_import::IMPORT_HOLDER_NAMESPACE,
                    },
                    &catabolic_core::sha256(IMPORT.as_bytes()),
                )
                .unwrap(),
                commands::HoldOutcome::Released,
            );
            released += 1;
        }
        assert_eq!(
            released,
            archived_batches.len() + 2 + usize::from(!refuse_activation)
        );
        drop(store);
        crate::candidate41_test_support::install_candidate41_for_test(&path).unwrap();
        let store =
            crate::candidate41_test_support::open_candidate41_for_test(&path, true).unwrap();
        Self {
            root,
            store,
            activation,
            manifest: archived_manifest,
            batches: archived_batches,
        }
    }
    pub(super) fn verify(&self) -> Result<AuthenticatedImportedLibrary> {
        self.store.transaction_detailed(|_| {
            let mut budget = ProofBudget::new(self.store.catalog_operation_budget()?);
            validate_imported_library_in(&self.store, SUBJECT, &mut budget)
        })
    }
}
#[test]
fn genuine_imported_library_proves_exact_definition_and_activation_head() {
    let f = Fixture::new();
    let before = generation(&f.store);
    let proof = f.verify().unwrap();
    assert_eq!(proof.definition()["name"], "Imported library");
    assert!(proof.item_revision() > 0);
    assert_eq!(generation(&f.store), before);
}
#[test]
fn actual_import_receipt_expiry_refuses_without_reconstructing_payload() {
    let f = Fixture::new();
    f.verify().unwrap();
    // The exact import holds were released through their genuine owner. Compact
    // through the actual maintenance owner, not a fabricated expired row.
    let n = commands::compact_candidate_receipts_for_test(
        &f.store,
        TrustedReceiptMaintenance {
            host_namespace: HOST,
            holder_namespace: catalog_import::IMPORT_HOLDER_NAMESPACE,
        },
        100,
    )
    .unwrap();
    assert!(n > 0);
    let error = f.verify().err().unwrap().to_string();
    assert!(error.contains("expired import receipt"), "{error}");
}
#[test]
fn actual_import_with_trimmed_result_refuses_after_guards_are_restored() {
    let f = Fixture::new();
    f.verify().unwrap();
    let path = f.root.path().join("library.db");
    let operation = f.activation.key().operation_id().to_owned();
    drop(f.store);
    // Corruption fixture only: retain and restore the exact immutable receipt
    // trigger bytes. No production owner can use this setup capability.
    let db = rusqlite::Connection::open(&path).unwrap();
    let guard:String=db.query_row("SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='catalog_receipt_compaction_only'",[],|r|r.get(0)).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; DROP TRIGGER catalog_receipt_compaction_only;")
        .unwrap();
    db.execute("UPDATE catalog_command_receipts SET result_json=json_remove(result_json,'$.input_digest') WHERE operation_id=?",[operation]).unwrap();
    db.execute_batch(&guard).unwrap();
    db.execute_batch("COMMIT").unwrap();
    drop(db);
    let store = crate::candidate41_test_support::open_candidate41_for_test(&path, true).unwrap();
    let error = store
        .transaction_detailed::<_, Error>(|_| {
            let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
            validate_imported_library_in(&store, SUBJECT, &mut budget)
        })
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("trimmed or malformed import result"),
        "{error}"
    );
}
