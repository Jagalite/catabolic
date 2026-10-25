use super::*;
use crate::operation_budget::BudgetCharge;
use crate::{
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy,
        TrustedReceiptMaintenance,
    },
    catalog_import::{ActivationBinding, ImportAuthority, ImportItem, TrustedImportActivation},
    catalog_import_typed::{
        ImportedContribution, ImportedMetadata, ImportedSourceKind, NativeIdentityBatch,
        TypedAuthority, TypedBatch, capture_existing_occurrence,
    },
    catalog_metadata_store::{MetadataAccess, MetadataAuthority},
    migration,
};
use catabolic_core::{
    catalog_command::MAX_COMMAND_JSON_DEPTH,
    catalog_metadata::{MAX_METADATA_JSON_NODES, MAX_METADATA_SNAPSHOT_BYTES},
};
use catabolic_core::{
    catalog_command::{CommandKey, ReceiptVisibility},
    catalog_host_lease::HostAuthorization,
    catalog_metadata::SourceKind,
};
use catabolic_runtime::{
    helper,
    host::{CONTRACT_VERSION, HostSession},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    time::{Duration, Instant},
};
const HOST: &str = "nfo-physical-proof";
fn authority() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: false,
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
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::ZERO,
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}
fn envelope(store: &Store, id: &str, mutation: &ImportMutation) -> CommandEnvelope {
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
            catalog_import::operation_id(id, step).unwrap(),
        )
        .unwrap(),
        store.catalog_incarnation.clone().unwrap(),
        generation(store),
        &json!({"operation":"import","mutation":mutation}).to_string(),
    )
    .unwrap()
}
fn activate(
    store: &Store,
    id: &str,
    batches: Vec<ImportBatch>,
    typed: TypedAuthority,
) -> (SnapshotManifest, Vec<ImportBatch>, CommandEnvelope, String) {
    let manifest = SnapshotManifest {
        contract_version: 2,
        import_id: id.into(),
        source_namespace: "motion.schema68.export.v4".into(),
        source_snapshot_id: format!("frozen-{id}"),
        batches: batches
            .iter()
            .map(|batch| catalog_import::describe_batch(batch).unwrap())
            .collect(),
    };
    let input = catalog_import::manifest_digest(&manifest).unwrap();
    let grant = ImportAuthority {
        host_namespace: HOST.into(),
        import_id: id.into(),
        source_namespace: manifest.source_namespace.clone(),
    };
    let run = |mutation: &ImportMutation, active: Option<&TrustedImportActivation>| {
        let command = envelope(store, id, mutation);
        let mut trusted = CatalogAuthority::new(TrustedCommandAuthority {
            allow_new_commands: true,
            ..authority()
        })
        .with_imports(&grant)
        .with_import_typed(&typed);
        if let Some(active) = active {
            trusted = trusted.with_import_activation(active);
        }
        commands::execute_with_authority(store, &command, trusted, policy()).unwrap()
    };
    run(
        &ImportMutation::Prepare {
            manifest: manifest.clone(),
            input_digest: input.clone(),
        },
        None,
    );
    for (index, batch) in batches.iter().enumerate() {
        run(
            &ImportMutation::Stage {
                import_id: id.into(),
                input_digest: input.clone(),
                batch_index: index as u32,
                batch: batch.clone(),
            },
            None,
        );
    }
    let result = run(
        &ImportMutation::Validate {
            import_id: id.into(),
            input_digest: input.clone(),
        },
        None,
    );
    let value = match result {
        CommandOutcome::Committed { result, .. } => result,
        other => panic!("{other:?}"),
    };
    let mapping = value["mapping_digest"].as_str().unwrap().to_owned();
    let host = HostAuthorization::new(HOST.into(), format!("freeze-{id}")).unwrap();
    let active = TrustedImportActivation::after_drain(
        &host,
        host.begin_revocation().unwrap(),
        ActivationBinding {
            catalog_id: store.database_id.clone(),
            incarnation: store.catalog_incarnation.clone().unwrap(),
            import_id: id.into(),
            input_digest: input.clone(),
            source_snapshot_id: manifest.source_snapshot_id.clone(),
            validation_generation: generation(store),
            reference_ack_id: format!("audit-{id}"),
            mapping_digest: mapping.clone(),
        },
    )
    .unwrap();
    let mutation = ImportMutation::Activate {
        import_id: id.into(),
        input_digest: input,
        reference_ack_id: format!("audit-{id}"),
        mapping_digest: mapping.clone(),
    };
    let command = envelope(store, id, &mutation);
    assert!(matches!(
        run(&mutation, Some(&active)),
        CommandOutcome::Committed { .. }
    ));
    (manifest, batches, command, mapping)
}
struct Fixture {
    root: tempfile::TempDir,
    store: Store,
    metadata: CommandEnvelope,
    metadata_manifest: SnapshotManifest,
    metadata_batches: Vec<ImportBatch>,
    metadata_mapping: String,
    manifest: SnapshotManifest,
    batches: Vec<ImportBatch>,
    activation: CommandEnvelope,
    grant: NfoImportedOriginAuthority,
}
// This fixture uses the same qualified, non-harness helper as runtime lifecycle
// tests. A digest identifies that packaged artifact; it does not confer trust on
// arbitrary binaries. Parent test preflight supplies the exact rebuilt helper.
static CLOSED_SCAN: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn closed_scan(path: &Path, fresh_root: Option<&Path>) {
    let _serial = CLOSED_SCAN.lock().unwrap();
    let binary = std::path::PathBuf::from(
        std::env::var_os("CATABOLIC_HOST_TEST_HELPER")
            .expect("CATABOLIC_HOST_TEST_HELPER must name the qualified rebuilt native helper"),
    );
    let qualified = helper::TrustedClosedObservationArtifact::after_packaging_verification(
        &binary,
        &catabolic_core::sha256(std::fs::read(&binary).unwrap()),
        helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
    )
    .unwrap();
    let gate = HostAuthorization::new(
        HOST.into(),
        format!("nfo-proof-scan-{}", uuid::Uuid::new_v4()),
    )
    .unwrap();
    let mut host =
        HostSession::open_authorized(path, "default", true, CONTRACT_VERSION, gate.clone())
            .unwrap();
    if let Some(root) = fresh_root {
        host.register_source("media", root, &[]).unwrap();
    }
    host.register_closed_observation_helper(&qualified).unwrap();
    let handle = host.start_scan("media", ()).unwrap();
    let revoked = gate.begin_revocation().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(report) = host.poll_scan(&handle, Duration::from_millis(50)).unwrap() {
            assert_eq!(report["complete"], true);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "closed observation did not finish"
        );
    }
    revoked.wait_drained(Duration::ZERO).unwrap();
    host.close().unwrap();
    drop(host);
    let store = Store::open(path, true, false).unwrap();
    let inventory = crate::native_worker_inventory::inspect(&store).unwrap();
    assert!(inventory.admits_new_work(), "{inventory:?}");
    assert!(inventory.witnessed_observation >= 1);
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native.db");
        migration::initialize_native_at(&path, 40).unwrap();
        let media = root.path().join("media");
        std::fs::create_dir(&media).unwrap();
        std::fs::write(media.join("movie.mkv"), b"original observation").unwrap();
        closed_scan(&path, Some(&media));
        let store = Store::open(&path, true, false).unwrap();
        let file: String = store
            .db
            .query_row(
                "SELECT id FROM files WHERE location='media' AND path='movie.mkv'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let typed = TypedAuthority {
            physical_files: [("default".into(), [file].into())].into(),
            ..Default::default()
        };
        let native_file = typed.physical_files["default"].iter().next().unwrap();
        let physical = capture_existing_occurrence(
            &store,
            "motion-file:source",
            "default",
            native_file,
            &typed,
        )
        .unwrap();
        let document = json!({"values":{"title":"NFO"},"tags":[],"excluded_tags":[]});
        let metadata_grants = TypedAuthority {
            metadata: vec![
                MetadataAuthority {
                    host_namespace: HOST.into(),
                    subject: "subject".into(),
                    access: MetadataAccess::InitializeV2 {
                        system_fields: Default::default(),
                    },
                },
                MetadataAuthority {
                    host_namespace: HOST.into(),
                    subject: "subject".into(),
                    access: MetadataAccess::SourceV2 {
                        namespace: "motion.nfo".into(),
                        record_id: "document".into(),
                        kind: SourceKind::Provider,
                        system_fields: Default::default(),
                    },
                },
            ],
            ..Default::default()
        };
        let (metadata_manifest, metadata_batches, metadata, metadata_mapping) = activate(
            &store,
            "metadata-part",
            vec![ImportBatch {
                items: vec![ImportItem {
                    source_id: "subject".into(),
                    item_id: "subject".into(),
                    kind: "video".into(),
                    legacy_metadata: json!({"title":"Imported"}),
                }],
                unresolved_evidence: vec![],
                typed: Some(TypedBatch {
                    metadata: vec![ImportedMetadata {
                        contract_version: 2,
                        system_fields: vec![],
                        item_id: "subject".into(),
                        policies: json!([{"field":"title","value_type":"string","nullable":false,"normalization":"exact"}]),
                        contributions: vec![ImportedContribution {
                            namespace: "motion.nfo".into(),
                            record_id: "document".into(),
                            source_revision: 9,
                            kind: ImportedSourceKind::Provider,
                            fields: document["values"].clone(),
                            evidence: json!({"source_table":"metadata_documents","source":"nfo","external_id":"tmdb:42","document_json":document.to_string(),"document":document}),
                        }],
                    }],
                    ..Default::default()
                }),
            }],
            metadata_grants,
        );
        let (manifest, batches, activation, mapping) = activate(
            &store,
            "physical-part",
            vec![ImportBatch {
                items: vec![],
                unresolved_evidence: vec![],
                typed: Some(TypedBatch {
                    native: Some(NativeIdentityBatch {
                        occurrences: vec![physical.clone()],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            }],
            typed,
        );
        let grant = NfoImportedOriginAuthority {
            host_namespace: HOST.into(),
            root_manifest_digest: Some("a".repeat(64)),
            metadata_import_id: metadata_manifest.import_id.clone(),
            metadata_input_digest: catalog_import::manifest_digest(&metadata_manifest).unwrap(),
            physical_import_id: manifest.import_id.clone(),
            physical_input_digest: catalog_import::manifest_digest(&manifest).unwrap(),
            physical_mapping_digest: mapping,
            origin_audit_digest: "b".repeat(64),
            subject: "subject".into(),
            motion_file_id: "old-motion-file".into(),
            physical,
        };
        Self {
            root,
            store,
            metadata,
            metadata_manifest,
            metadata_batches,
            metadata_mapping,
            manifest,
            batches,
            activation,
            grant,
        }
    }
    fn rescan(mut self) -> Self {
        let path = self.store.path.clone();
        drop(self.store);
        closed_scan(&path, None);
        self.store = Store::open(&path, true, false).unwrap();
        self
    }
    fn verify(&self) -> Result<VerifiedImportedOrigin> {
        self.store.transaction_detailed(|_| {
            verify_imported_origin_in(
                &self.store,
                &self.metadata,
                &self.manifest,
                &self.batches,
                &self.activation,
                &self.grant,
                authority(),
                generation(&self.store),
            )
        })
    }
}
#[test]
fn exact_activated_physical_part_retains_historical_revision_after_path_changes_and_compaction() {
    let f = Fixture::new();
    let proof = f.verify().unwrap();
    assert_eq!(proof.physical(), &f.grant.physical);
    assert_eq!(proof.motion_file_id(), "old-motion-file");
    assert_eq!(proof.origin().file.file_id, f.grant.physical.exact.file_id);
    // A historical attribution is not invalidated or rebound by today's path.
    std::fs::rename(
        f.root.path().join("media/movie.mkv"),
        f.root.path().join("media/moved.mkv"),
    )
    .unwrap();
    let f = f.rescan();
    assert_eq!(f.verify().unwrap().physical().exact, proof.physical().exact);
    assert_eq!(
        commands::release_hold(
            &f.store,
            f.activation.key(),
            TrustedReceiptMaintenance {
                host_namespace: HOST,
                holder_namespace: catalog_import::IMPORT_HOLDER_NAMESPACE,
            },
            &catabolic_core::sha256(f.manifest.import_id.as_bytes()),
        )
        .unwrap(),
        commands::HoldOutcome::Released
    );
    commands::compact_receipts(
        &f.store,
        TrustedReceiptMaintenance {
            host_namespace: HOST,
            holder_namespace: "nfo-physical-maintenance",
        },
        100,
    )
    .unwrap();
    assert_eq!(f.verify().unwrap().proof_digest(), proof.proof_digest());
    assert!(matches!(
        commands::reconcile_receipt(&f.store, &f.activation, authority()).unwrap(),
        Some(CommandOutcome::ReceiptExpired)
    ));
}
#[test]
fn origin_proof_rejects_wrong_parts_mapping_revision_and_missing_complete_root() {
    let mut f = Fixture::new();
    f.verify().unwrap();
    let original = f.grant.physical.clone();
    f.grant.physical.exact.revision = "changed".into();
    assert!(f.verify().is_err());
    f.grant.physical = original;
    f.grant.root_manifest_digest = None;
    assert!(f.verify().is_err());
    f.grant.root_manifest_digest = Some("a".repeat(64));
    let original = f.grant.physical_mapping_digest.clone();
    f.grant.physical_mapping_digest = "c".repeat(64);
    assert!(f.verify().is_err());
    f.grant.physical_mapping_digest = original;
    f.batches[0]
        .typed
        .as_mut()
        .unwrap()
        .native
        .as_mut()
        .unwrap()
        .occurrences
        .clear();
    assert!(f.verify().is_err());
}

#[test]
fn exact_origin_composes_with_metadata_baseline_and_complete_source_history() {
    use super::super::{
        NfoImportAuthority, verify_imported_no_origin_baseline_in,
        verify_imported_origin_baseline_in, verify_source_history_in,
    };
    let f = Fixture::new();
    let mut grant = NfoImportAuthority {
        host_namespace: HOST.into(),
        root_manifest_digest: f.grant.root_manifest_digest.clone(),
        import_id: f.metadata_manifest.import_id.clone(),
        input_digest: catalog_import::manifest_digest(&f.metadata_manifest).unwrap(),
        source_namespace: f.metadata_manifest.source_namespace.clone(),
        reference_ack_id: "audit-metadata-part".into(),
        mapping_digest: f.metadata_mapping.clone(),
        origin_audit_digest: f.grant.origin_audit_digest.clone(),
        no_origin_subjects: Default::default(),
    };
    let verify = |grant: &NfoImportAuthority| {
        f.store.transaction_detailed(|_| {
            let origin = verify_imported_origin_in(
                &f.store,
                &f.metadata,
                &f.manifest,
                &f.batches,
                &f.activation,
                &f.grant,
                authority(),
                generation(&f.store),
            )?;
            assert!(
                verify_imported_no_origin_baseline_in(
                    &f.store,
                    "subject",
                    &f.metadata_manifest,
                    &f.metadata_batches,
                    &f.metadata,
                    grant,
                    authority(),
                    generation(&f.store)
                )
                .is_err()
            );
            let baseline = verify_imported_origin_baseline_in(
                &f.store,
                "subject",
                &f.metadata_manifest,
                &f.metadata_batches,
                &f.metadata,
                grant,
                &origin,
                authority(),
                generation(&f.store),
            )?;
            let current = verify_source_history_in(
                &f.store,
                &baseline,
                &[],
                authority(),
                generation(&f.store),
            )?;
            assert_eq!(current.automatic_identity(), Some("tmdb:42"));
            assert_eq!(current.origin(), Some(origin.origin()));
            assert_eq!(current.origin_physical(), Some(origin.physical()));
            assert_eq!(current.origin_proof_digest(), Some(origin.proof_digest()));
            Ok::<_, Error>(())
        })
    };
    verify(&grant).unwrap();
    grant.origin_audit_digest = "c".repeat(64);
    assert!(verify(&grant).is_err());
    grant.origin_audit_digest = f.grant.origin_audit_digest.clone();
    grant.root_manifest_digest = Some("d".repeat(64));
    assert!(verify(&grant).is_err());
}

fn proof_budget(bytes: usize) -> OperationBudget {
    OperationBudget::new(
        Duration::from_secs(30),
        crate::operation_budget::BudgetLimits {
            items: 100,
            values: usize::MAX,
            bytes,
            work: usize::MAX,
        },
    )
    .unwrap()
}

#[test]
fn physical_proof_stream_counts_values_and_preserves_exact_hash() {
    let value = json!({"key[{\"": ["escaped\\\"},[]", 12, true, null], "empty": {}});
    let raw = serde_json::to_vec(&value).unwrap();
    let budget = proof_budget(usize::MAX);
    assert_eq!(
        bounded_hash(budget.clone(), &value).unwrap(),
        format!("{:x}", Sha256::digest(&raw))
    );
    let used = budget.usage();
    assert_eq!(used.bytes, raw.len());
    assert_eq!(used.values, 7); // root, array, four scalar values, empty object.
    assert_eq!(used.work, raw.len() / 64 + 7 + 1);
}

#[test]
fn physical_proof_stream_refuses_local_depth_nodes_work_and_bytes() {
    let mut value = json!(null);
    for _ in 0..MAX_COMMAND_JSON_DEPTH {
        value = json!([value]);
    }
    assert!(bounded_hash(proof_budget(usize::MAX), &value).is_ok());
    value = json!([value]);
    assert!(bounded_hash(proof_budget(usize::MAX), &value).is_err());
    assert!(bounded_hash(proof_budget(usize::MAX), &vec![0; MAX_METADATA_JSON_NODES]).is_err());
    assert!(
        bounded_hash(
            proof_budget(usize::MAX),
            &"x".repeat(MAX_METADATA_JSON_NODES * 64)
        )
        .is_err()
    );
    assert!(
        bounded_hash(
            proof_budget(usize::MAX),
            &"x".repeat(MAX_METADATA_SNAPSHOT_BYTES + 1)
        )
        .is_err()
    );
}

#[test]
fn physical_proof_stream_charges_existing_shared_budget_incrementally() {
    let budget = proof_budget(32);
    budget
        .charge(BudgetCharge {
            bytes: 28,
            ..Default::default()
        })
        .unwrap();
    assert!(bounded_hash(budget.clone(), &json!([1, 2, 3, 4, 5])).is_err());
    assert!(budget.failure().is_some());
    // A successful prefix was charged before the rejected chunk, never a full
    // result hash. The failed charge itself cannot consume beyond the limit.
    assert!(budget.usage().bytes > 28);
    assert!(budget.usage().bytes <= 32);
    assert!(bounded_hash(budget, &json!(null)).is_err());
}
