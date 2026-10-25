use super::*;
use crate::{
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy,
        TrustedReceiptMaintenance,
    },
    catalog_import::{ActivationBinding, ImportAuthority, ImportItem, TrustedImportActivation},
    catalog_import_typed::{
        ImportedContribution, ImportedTagSet, TagImportAuthority, TypedAuthority, TypedBatch,
    },
    catalog_metadata_store::{MetadataAccess, MetadataAuthority},
    migration,
};
use catabolic_core::{
    catalog_command::{CommandKey, ReceiptVisibility},
    catalog_host_lease::HostAuthorization,
};
use std::time::Duration;
const HOST: &str = "nfo-import-proof";
const SUBJECT: &str = "nfo-subject";
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
            catalog_import::operation_id("nfo-part", step).unwrap(),
        )
        .unwrap(),
        store.catalog_incarnation.clone().unwrap(),
        generation(store),
        &json!({"operation":"import","mutation":mutation}).to_string(),
    )
    .unwrap()
}
fn result(outcome: &CommandOutcome) -> &Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        other => panic!("unexpected {other:?}"),
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    store: Store,
    manifest: SnapshotManifest,
    batches: Vec<ImportBatch>,
    activation: CommandEnvelope,
    grant: NfoImportAuthority,
}
impl Fixture {
    fn new(version: u32) -> Self {
        Self::with_identity(version, "tmdb:42")
    }
    fn with_identity(version: u32, external_identity: &str) -> Self {
        Self::with_document(
            version,
            external_identity,
            json!({"values":{"title":"NFO title"},"tags":[],"excluded_tags":[]}),
            None,
        )
    }
    fn with_document(
        version: u32,
        external_identity: &str,
        document: Value,
        raw_document: Option<String>,
    ) -> Self {
        Self::with_document_and_tags(version, external_identity, document, raw_document, vec![])
    }
    fn with_document_and_tags(
        version: u32,
        external_identity: &str,
        document: Value,
        raw_document: Option<String>,
        tags: Vec<ImportedTagSet>,
    ) -> Self {
        Self::with_tag_setup(
            version,
            external_identity,
            document,
            raw_document,
            tags,
            |_| {},
        )
    }
    fn with_tag_setup(
        version: u32,
        external_identity: &str,
        document: Value,
        raw_document: Option<String>,
        tags: Vec<ImportedTagSet>,
        setup: impl FnOnce(&Store),
    ) -> Self {
        let raw_document = raw_document.unwrap_or_else(|| document.to_string());
        let policies: Vec<Value> = document["values"]
            .as_object()
            .unwrap()
            .keys()
            .map(|field| {
                json!({"field":field,"value_type":if field == "title" { "string" } else { "json" },"nullable":false,"normalization":"exact"})
            })
            .collect();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("import.db");
        migration::initialize_native_at(&path, 40).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        setup(&store);
        let metadata = ImportedMetadata {
            contract_version: version,
            system_fields: vec![],
            item_id: SUBJECT.into(),
            policies: Value::Array(policies),
            contributions: vec![ImportedContribution {
                namespace: "motion.nfo".into(),
                record_id: "document".into(),
                source_revision: 9,
                kind: ImportedSourceKind::Provider,
                fields: document["values"].clone(),
                evidence: json!({"source_table":"metadata_documents","source":"nfo","external_id":external_identity,"updated_at":123,"document_json":raw_document,"document":document}),
            }],
        };
        let batches = vec![ImportBatch {
            items: vec![ImportItem {
                source_id: SUBJECT.into(),
                item_id: SUBJECT.into(),
                kind: "video".into(),
                legacy_metadata: json!({"title":"Original"}),
            }],
            unresolved_evidence: vec![],
            typed: Some(TypedBatch {
                metadata: vec![metadata],
                tags,
                ..Default::default()
            }),
        }];
        let manifest = SnapshotManifest {
            contract_version: 2,
            import_id: "nfo-part".into(),
            source_namespace: SOURCE.into(),
            source_snapshot_id: "frozen-nfo".into(),
            batches: batches
                .iter()
                .map(|b| catalog_import::describe_batch(b).unwrap())
                .collect(),
        };
        let input = catalog_import::manifest_digest(&manifest).unwrap();
        let import = ImportAuthority {
            host_namespace: HOST.into(),
            import_id: manifest.import_id.clone(),
            source_namespace: SOURCE.into(),
        };
        let typed = TypedAuthority {
            // Controlled fixture host grants only these constructed source sets.
            // No grant is inferred from the archived document's requested tags.
            tag_sources: batches[0]
                .typed
                .as_ref()
                .unwrap()
                .tags
                .iter()
                .map(|tags| TagImportAuthority {
                    host_namespace: HOST.into(),
                    namespace: tags.namespace.clone(),
                    item_ids: BTreeSet::from([SUBJECT.into()]),
                    allow_suppressions: false,
                })
                .collect(),
            metadata: vec![
                MetadataAuthority {
                    host_namespace: HOST.into(),
                    subject: SUBJECT.into(),
                    access: if version == 2 {
                        MetadataAccess::InitializeV2 {
                            system_fields: BTreeSet::new(),
                        }
                    } else {
                        MetadataAccess::Initialize
                    },
                },
                MetadataAuthority {
                    host_namespace: HOST.into(),
                    subject: SUBJECT.into(),
                    access: if version == 2 {
                        MetadataAccess::SourceV2 {
                            namespace: "motion.nfo".into(),
                            record_id: "document".into(),
                            kind: SourceKind::Provider,
                            system_fields: BTreeSet::new(),
                        }
                    } else {
                        MetadataAccess::Source {
                            namespace: "motion.nfo".into(),
                            record_id: "document".into(),
                            kind: SourceKind::Provider,
                        }
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
            if let Some(active) = activation {
                authority = authority.with_import_activation(active);
            }
            commands::execute_with_authority(&store, &command, authority, policy).unwrap()
        };
        run(
            &ImportMutation::Prepare {
                manifest: manifest.clone(),
                input_digest: input.clone(),
            },
            None,
        );
        run(
            &ImportMutation::Stage {
                import_id: manifest.import_id.clone(),
                input_digest: input.clone(),
                batch_index: 0,
                batch: batches[0].clone(),
            },
            None,
        );
        let validation = run(
            &ImportMutation::Validate {
                import_id: manifest.import_id.clone(),
                input_digest: input.clone(),
            },
            None,
        );
        let mapping = result(&validation)["mapping_digest"]
            .as_str()
            .unwrap()
            .to_owned();
        let gate = HostAuthorization::new(HOST.into(), "freeze-nfo".into()).unwrap();
        let active = TrustedImportActivation::after_drain(
            &gate,
            gate.begin_revocation().unwrap(),
            ActivationBinding {
                catalog_id: store.database_id.clone(),
                incarnation: store.catalog_incarnation.clone().unwrap(),
                import_id: manifest.import_id.clone(),
                input_digest: input.clone(),
                source_snapshot_id: manifest.source_snapshot_id.clone(),
                validation_generation: generation(&store),
                reference_ack_id: "nfo-audit".into(),
                mapping_digest: mapping.clone(),
            },
        )
        .unwrap();
        let mutation = ImportMutation::Activate {
            import_id: manifest.import_id.clone(),
            input_digest: input.clone(),
            reference_ack_id: "nfo-audit".into(),
            mapping_digest: mapping.clone(),
        };
        let activation = envelope(&store, &mutation);
        run(&mutation, Some(&active));
        let grant = NfoImportAuthority {
            host_namespace: HOST.into(),
            root_manifest_digest: Some("a".repeat(64)),
            import_id: manifest.import_id.clone(),
            input_digest: input,
            source_namespace: SOURCE.into(),
            reference_ack_id: "nfo-audit".into(),
            mapping_digest: mapping,
            origin_audit_digest: "b".repeat(64),
            no_origin_subjects: [SUBJECT.to_owned()].into(),
        };
        Self {
            _root: root,
            store,
            manifest,
            batches,
            activation,
            grant,
        }
    }
    fn verify(&self) -> Result<VerifiedNfoBaseline> {
        self.store.transaction_detailed(|_| {
            verify_imported_no_origin_baseline_in(
                &self.store,
                SUBJECT,
                &self.manifest,
                &self.batches,
                &self.activation,
                &self.grant,
                TrustedCommandAuthority {
                    allow_new_commands: false,
                    ..base()
                },
                generation(&self.store),
            )
        })
    }
    fn into_candidate(mut self) -> Self {
        let path = self._root.path().join("import.db");
        drop(self.store);
        crate::candidate41_test_support::install_candidate41_for_test(&path).unwrap();
        self.store =
            crate::candidate41_test_support::open_candidate41_for_test(&path, true).unwrap();
        self
    }
}
#[test]
fn actual_v1_and_v2_import_baselines_preserve_identity_after_result_expiry() {
    for version in [1, 2] {
        let f = Fixture::new(version);
        let baseline = f.verify().unwrap();
        assert_eq!(baseline.automatic_identity.as_deref(), Some("tmdb:42"));
        assert!(baseline.origin.is_none());
        assert_eq!(baseline.revision, 1);
        let names = baseline.imported_requested_names.as_ref().unwrap();
        assert!(names.tags().is_empty());
        assert_eq!(names.source_revision(), 9);
        assert_eq!(names.matching_record(), None);
        // The shared history reader also compares exact stored revision-one bytes.
        f.store
            .transaction_detailed::<_, Error>(|_| {
                super::super::history::verify_source_history_in(
                    &f.store,
                    &baseline,
                    &[],
                    TrustedCommandAuthority {
                        allow_new_commands: false,
                        ..base()
                    },
                    generation(&f.store),
                )
            })
            .unwrap();
        // Activation is held for host reconciliation even with zero retention.
        // Release only this exact acknowledged activation through its owner.
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
                holder_namespace: catalog_import::IMPORT_HOLDER_NAMESPACE,
            },
            100,
        )
        .unwrap();
        assert!(matches!(
            commands::reconcile_receipt(
                &f.store,
                &f.activation,
                TrustedCommandAuthority {
                    allow_new_commands: false,
                    ..base()
                }
            )
            .unwrap(),
            Some(CommandOutcome::ReceiptExpired)
        ));
        let after_expiry = f.verify().unwrap();
        assert_eq!(after_expiry.automatic_identity, baseline.automatic_identity);
        assert_eq!(
            after_expiry
                .imported_requested_names
                .as_ref()
                .unwrap()
                .digest(),
            names.digest()
        );
    }
}
#[test]
fn altered_frozen_bytes_or_missing_complete_origin_audit_cannot_mint_baseline() {
    let mut f = Fixture::new(2);
    f.grant.no_origin_subjects.clear();
    assert!(f.verify().is_err());
    f.grant.no_origin_subjects.insert(SUBJECT.into());
    f.batches[0].typed.as_mut().unwrap().metadata[0].contributions[0].evidence["external_id"] =
        json!("forged:identity");
    assert!(f.verify().is_err());
    f.manifest.batches[0] = catalog_import::describe_batch(&f.batches[0]).unwrap();
    f.grant.input_digest = catalog_import::manifest_digest(&f.manifest).unwrap();
    assert!(f.verify().is_err()); // Rehashing cannot change the committed activation.
}

#[test]
fn imported_identity_preserves_opaque_legacy_put_bytes_without_normalization() {
    let identity = " \nopaque\tidentity ";
    let f = Fixture::with_identity(2, identity);
    assert_eq!(
        f.verify().unwrap().automatic_identity.as_deref(),
        Some(identity)
    );
    let baseline = f.verify().unwrap();
    f.store
        .transaction_detailed(|_| {
            let current = super::super::history::verify_source_history_in(
                &f.store,
                &baseline,
                &[],
                TrustedCommandAuthority {
                    allow_new_commands: false,
                    ..base()
                },
                generation(&f.store),
            )?;
            assert_eq!(current.automatic_identity(), Some(identity));
            Ok::<_, crate::Error>(())
        })
        .unwrap();
}

fn container_depth(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1 + values.iter().map(container_depth).max().unwrap_or(0),
        Value::Object(fields) => 1 + fields.values().map(container_depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// Inspect the real owner's stored evidence and retained receipt. These fixtures
/// never insert, update or disable triggers on metadata history/receipt tables.
fn assert_activated_with_raw(f: &Fixture, expected_raw: &str) -> Value {
    assert!(matches!(
        commands::reconcile_receipt(
            &f.store,
            &f.activation,
            TrustedCommandAuthority {
                allow_new_commands: false,
                ..base()
            }
        )
        .unwrap(),
        Some(CommandOutcome::Replay { .. })
    ));
    let raw: String = f.store.db.query_row(
        "SELECT evidence_json FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo' AND record_id='document' AND revision=1",
        [SUBJECT],
        |row| row.get(0),
    ).unwrap();
    let evidence: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        evidence["source_evidence"]["document_json"].as_str(),
        Some(expected_raw)
    );
    evidence
}

#[test]
fn actual_import_with_deep_valid_document_keeps_envelope_and_document_depth_separate() {
    use catabolic_core::{
        catalog_command::MAX_COMMAND_JSON_DEPTH, catalog_metadata::MAX_METADATA_JSON_DEPTH,
    };
    for version in [1, 2] {
        let mut extension = json!(0);
        for _ in 0..24 {
            extension = json!([extension]);
        }
        let document = json!({
            "values":{"title":"NFO title","extension":extension},
            "tags":[],"excluded_tags":[],
        });
        let raw = document.to_string();
        assert_eq!(container_depth(&document["values"]), 25);
        assert_eq!(container_depth(&document), 26);
        let f = Fixture::with_document(version, "tmdb:42", document, None);
        let evidence = assert_activated_with_raw(&f, &raw);
        assert_eq!(container_depth(&evidence), 28);
        assert!(container_depth(&evidence) <= MAX_METADATA_JSON_DEPTH);
        let proof_input = serde_json::to_value((&f.manifest, &f.batches)).unwrap();
        assert_eq!(container_depth(&proof_input), 35);
        assert!(container_depth(&proof_input) > MAX_METADATA_JSON_DEPTH);
        assert!(container_depth(&proof_input) <= MAX_COMMAND_JSON_DEPTH);
        let baseline = f.verify().unwrap();
        assert_eq!(baseline.evidence_json, catabolic_core::encode(&evidence));
        f.store
            .transaction_detailed(|_| {
                let current = super::super::history::verify_source_history_in(
                    &f.store,
                    &baseline,
                    &[],
                    TrustedCommandAuthority {
                        allow_new_commands: false,
                        ..base()
                    },
                    generation(&f.store),
                )?;
                assert_eq!(current.current_source().evidence(), &evidence);
                assert_eq!(current.automatic_identity(), Some("tmdb:42"));
                Ok::<_, Error>(())
            })
            .unwrap();
    }
}

#[test]
fn activated_opaque_document_json_with_duplicate_keys_cannot_mint_baseline() {
    let document = json!({"values":{"title":"NFO title"},"tags":[],"excluded_tags":[]});
    let raw =
        r#"{"values":{"title":"discarded","title":"NFO title"},"tags":[],"excluded_tags":[]}"#;
    // Ordinary serde_json would silently collapse the first title and make the
    // raw archived bytes appear consistent with the separately decoded object.
    assert_eq!(serde_json::from_str::<Value>(raw).unwrap(), document);
    for version in [1, 2] {
        let f = Fixture::with_document(version, "tmdb:42", document.clone(), Some(raw.into()));
        assert_activated_with_raw(&f, raw);
        assert!(f.verify().is_err());
    }
}

#[test]
fn activated_opaque_document_json_cannot_hide_excess_depth_in_discarded_duplicate() {
    use catabolic_core::catalog_metadata::MAX_METADATA_JSON_DEPTH;
    let document = json!({"values":{"title":"NFO title"},"tags":[],"excluded_tags":[]});
    let nested = format!("{}0{}", "[".repeat(50), "]".repeat(50));
    let raw = format!(
        "{{\"values\":{{\"title\":{nested},\"title\":\"NFO title\"}},\"tags\":[],\"excluded_tags\":[]}}"
    );
    // This has 52 raw container levels, but permissive decoding discards the
    // entire deep value. Outer evidence sees only an ordinary opaque string.
    assert!(50 + 2 > MAX_METADATA_JSON_DEPTH);
    assert_eq!(serde_json::from_str::<Value>(&raw).unwrap(), document);
    for version in [1, 2] {
        let f = Fixture::with_document(version, "tmdb:42", document.clone(), Some(raw.clone()));
        let evidence = assert_activated_with_raw(&f, &raw);
        assert!(container_depth(&evidence) <= MAX_METADATA_JSON_DEPTH);
        assert!(f.verify().is_err());
    }
}

fn imported_tag_set(names: &[&str]) -> ImportedTagSet {
    ImportedTagSet {
        item_id: SUBJECT.into(),
        namespace: "motion.nfo".into(),
        source_revision: 9,
        tags: names.iter().map(|name| (*name).to_owned()).collect(),
        excluded_tags: vec![],
    }
}

/// Independent length-framed hash oracle for this exact revision-one fixture.
/// The assertion below detects omission of the requested-name proof from source
/// history; it neither fabricates a baseline nor changes any retained owner row.
fn expected_imported_source_digest(
    store: &Store,
    baseline: &VerifiedNfoBaseline,
    current_fingerprint: &str,
    observed_generation: i64,
    closure_digest: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let hash = |parts: &[&[u8]]| {
        let mut hash = Sha256::new();
        for part in parts {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part);
        }
        format!("{:x}", hash.finalize())
    };
    let identity = serde_json::to_string(&baseline.automatic_identity).unwrap();
    let origin = serde_json::to_string(&baseline.origin).unwrap();
    let physical = serde_json::to_string(&baseline.origin_physical).unwrap();
    let initial = hash(&[
        b"catabolic.nfo.source-history.v1",
        store.database_id.as_bytes(),
        store.catalog_incarnation.as_ref().unwrap().as_bytes(),
        &observed_generation.to_le_bytes(),
        baseline.subject.as_bytes(),
        baseline.attribution_audit_digest.as_bytes(),
        identity.as_bytes(),
        origin.as_bytes(),
        physical.as_bytes(),
        baseline
            .origin_proof_digest
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
        baseline.command.digest().as_str().as_bytes(),
        current_fingerprint.as_bytes(),
    ]);
    let names = baseline.imported_requested_names.as_ref().unwrap();
    let named = hash(&[
        b"catabolic.nfo.source-history.imported-requested-tags.v1",
        initial.as_bytes(),
        names.digest().as_bytes(),
    ]);
    let historical = hash(&[
        named.as_bytes(),
        &baseline.revision.to_le_bytes(),
        baseline.schema_fingerprint.as_bytes(),
        baseline.fields_json.as_bytes(),
        baseline.evidence_json.as_bytes(),
        baseline.raw_mutation_json.as_bytes(),
        baseline.command.digest().as_str().as_bytes(),
    ]);
    hash(&[
        b"catabolic.nfo.source-history.imported-tag-closure.v1",
        historical.as_bytes(),
        closure_digest.as_bytes(),
    ])
}

#[test]
fn activated_nonempty_imported_tags_bind_history_and_candidate_bootstrap_effects() {
    use super::super::{bootstrap, verify_source_history_in};
    for version in [1, 2] {
        let original = json!({
            "values":{"title":"NFO title"},
            "tags":["favorite","genre:drama"],"excluded_tags":[],
        });
        let f = Fixture::with_document_and_tags(
            version,
            "tmdb:42",
            original.clone(),
            None,
            vec![imported_tag_set(&["genre:drama", "favorite"])],
        );
        assert_activated_with_raw(&f, &original.to_string());
        let baseline = f.verify().unwrap();
        let names = baseline.imported_requested_names.as_ref().unwrap();
        assert_eq!(names.subject(), SUBJECT);
        assert_eq!(names.source_revision(), 9);
        assert_eq!(names.matching_record(), Some((0, 0)));
        assert_eq!(
            names.tags(),
            &BTreeSet::from(["favorite".into(), "genre:drama".into()])
        );
        let count: i64 = f.store.db.query_row(
            "SELECT count(*) FROM item_tags WHERE item_id=? AND source='motion.nfo' AND active=1",
            [SUBJECT], |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 2); // Both assertions came from the actual typed owner.

        // The explicit test-only migration/receipt corridor is not production41
        // registration. This now consumes complete authenticated import IDs and
        // permanent unchanged assertion heads, not general merge/replacement lineage.
        let f = f.into_candidate();
        let baseline = f.verify().unwrap();
        let (plan, source_digest) = f
            .store
            .transaction_detailed(|_| {
                let generation = generation(&f.store);
                let source = verify_source_history_in(
                    &f.store,
                    &baseline,
                    &[],
                    TrustedCommandAuthority {
                        allow_new_commands: false,
                        ..base()
                    },
                    generation,
                )?;
                let expected = expected_imported_source_digest(
                    &f.store,
                    &baseline,
                    source.current_schema().fingerprint(),
                    generation,
                    source.imported_tag_admission().unwrap().digest().unwrap(),
                );
                assert_eq!(source.proof_digest(), expected);
                let plan = bootstrap::prepare_in(&f.store, &[source])?;
                Ok::<_, Error>((plan, expected))
            })
            .unwrap();
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(&f.store.database_id, HOST, "imported-tags-bootstrap").unwrap(),
            f.store.catalog_incarnation.clone().unwrap(),
            generation(&f.store),
            &plan.payload().to_string(),
        )
        .unwrap();
        let outcome = commands::execute_candidate_for_test(
            &f.store,
            &command,
            base(),
            ReceiptPolicy {
                retention: Duration::from_secs(60),
                max_receipts: 100,
                max_logical_bytes: 16 * 1024 * 1024,
                max_hold_records: 100,
            },
            move |store, command, _| bootstrap::apply_in(store, command, plan),
        )
        .unwrap();
        assert!(matches!(outcome, CommandOutcome::Committed { .. }));
        let retained: String = f.store.db.query_row(
            "SELECT source_proof_digest FROM catalog_nfo_state_revisions WHERE item_id=? AND revision=1",
            [SUBJECT], |row| row.get(0),
        ).unwrap();
        assert_eq!(retained, source_digest);
    }
}

#[test]
fn activated_archive_and_typed_tag_mismatch_refuses_imported_baseline() {
    for version in [1, 2] {
        for mismatch in ["names", "revision", "omission"] {
            let original = json!({
                "values":{"title":"NFO title"},"tags":["favorite"],"excluded_tags":[],
            });
            let mut imported = imported_tag_set(&["favorite"]);
            match mismatch {
                "names" => imported.tags = vec!["different".into()],
                "revision" => imported.source_revision += 1,
                "omission" => (),
                _ => unreachable!(),
            }
            let f = Fixture::with_document_and_tags(
                version,
                "tmdb:42",
                original.clone(),
                None,
                if mismatch == "omission" {
                    vec![]
                } else {
                    vec![imported]
                },
            );
            // Generic import legitimately admits these independently granted
            // records. Baseline verification, not a forged receipt or altered
            // immutable row, rejects the contradictory archived NFO provenance.
            assert_activated_with_raw(&f, &original.to_string());
            let error = f.verify().unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("catalog_nfo_imported_tags_refused")
            );
        }
    }
}

fn tagged_fixture(version: u32) -> Fixture {
    Fixture::with_document_and_tags(
        version,
        "tmdb:42",
        json!({"values":{"title":"NFO title"},"tags":["favorite"],"excluded_tags":[]}),
        None,
        vec![imported_tag_set(&["favorite"])],
    )
}
fn proof_policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::ZERO,
        max_receipts: 100,
        max_logical_bytes: 16 * 1024 * 1024,
        max_hold_records: 100,
    }
}
fn expire_mapping_result(f: &Fixture, step: ImportStep) {
    let key = CommandKey::new(
        &f.store.database_id,
        HOST,
        catalog_import::operation_id(&f.manifest.import_id, step).unwrap(),
    )
    .unwrap();
    let maintenance = TrustedReceiptMaintenance {
        host_namespace: HOST,
        holder_namespace: catalog_import::IMPORT_HOLDER_NAMESPACE,
    };
    assert_eq!(
        commands::release_hold(
            &f.store,
            &key,
            maintenance,
            &catabolic_core::sha256(f.manifest.import_id.as_bytes())
        )
        .unwrap(),
        commands::HoldOutcome::Released
    );
    commands::compact_receipts(&f.store, maintenance, 100).unwrap();
    let state:String=f.store.db.query_row(
        "SELECT result_state FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?",
        rusqlite::params![key.catalog_id(),key.host_namespace(),key.operation_id()],|r|r.get(0)).unwrap();
    assert_eq!(state, "expired");
}

/// Complete test database snapshot, including metadata, identity, every immutable
/// history, tag decisions and receipts. No fixture history/receipt is forged.
fn proof_database_snapshot(store: &Store) -> std::collections::BTreeMap<String, Vec<Vec<String>>> {
    let mut names=store.db.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
    let names = names
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let quoted = name.replace('"', "\"\"");
            let mut query = store
                .db
                .prepare(&format!("SELECT * FROM \"{quoted}\""))
                .unwrap();
            let columns = query.column_count();
            let mut rows = query
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get_ref(column).map(|v| format!("{v:?}")))
                        .collect::<std::result::Result<Vec<_>, _>>()
                })
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            rows.sort();
            (name, rows)
        })
        .collect()
}
fn assert_bootstrap_obligation(f: &Fixture, later: &[CommandEnvelope], message: &str) {
    let baseline = f.verify().unwrap();
    assert_eq!(baseline.automatic_identity.as_deref(), Some("tmdb:42"));
    let before = proof_database_snapshot(&f.store);
    let error = f
        .store
        .transaction_detailed(|_| {
            let source = super::super::verify_source_history_in(
                &f.store,
                &baseline,
                later,
                TrustedCommandAuthority {
                    allow_new_commands: false,
                    ..base()
                },
                generation(&f.store),
            )?;
            assert_eq!(source.automatic_identity(), Some("tmdb:42"));
            super::super::bootstrap::prepare_in(&f.store, &[source])
        })
        .unwrap_err();
    assert!(error.to_string().contains(message), "{error}");
    assert_eq!(proof_database_snapshot(&f.store), before);
}

#[test]
fn compacted_activation_uses_authenticated_full_validation_mapping_without_replay() {
    for version in [1, 2] {
        let f = tagged_fixture(version);
        expire_mapping_result(&f, ImportStep::Activate);
        assert!(matches!(
            commands::reconcile_receipt(
                &f.store,
                &f.activation,
                TrustedCommandAuthority {
                    allow_new_commands: false,
                    ..base()
                }
            )
            .unwrap(),
            Some(CommandOutcome::ReceiptExpired)
        ));
        let baseline = f.verify().unwrap();
        assert!(baseline.imported_tag_mapping.is_some());
        let f = f.into_candidate();
        let baseline = f.verify().unwrap();
        f.store
            .transaction_detailed(|_| {
                let source = super::super::verify_source_history_in(
                    &f.store,
                    &baseline,
                    &[],
                    TrustedCommandAuthority {
                        allow_new_commands: false,
                        ..base()
                    },
                    generation(&f.store),
                )?;
                source.imported_tag_admission().unwrap().require()?;
                super::super::bootstrap::prepare_in(&f.store, &[source])?;
                Ok::<_, Error>(())
            })
            .unwrap();
        // Historical mapping authentication did not revive the activation body.
        let absent: bool = f
            .store
            .db
            .query_row(
                "SELECT result_json IS NULL FROM catalog_command_receipts WHERE operation_id=?",
                [f.activation.key().operation_id()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(absent);
    }
}

#[test]
fn compacted_complete_mapping_preserves_identity_but_blocks_nonempty_tag_bootstrap() {
    for version in [1, 2] {
        let f = tagged_fixture(version);
        expire_mapping_result(&f, ImportStep::Activate);
        expire_mapping_result(&f, ImportStep::Validate);
        let baseline = f.verify().unwrap();
        assert_eq!(baseline.automatic_identity.as_deref(), Some("tmdb:42"));
        assert!(baseline.imported_tag_mapping.is_none());
        let f = f.into_candidate();
        assert_bootstrap_obligation(&f, &[], "complete imported tag mapping unavailable");
    }
}

#[test]
fn unjournaled_assignment_and_away_back_heads_cannot_bootstrap_imported_tags() {
    for mode in ["remove", "away_back", "extra", "oversize_vocabulary"] {
        let f = tagged_fixture(2);
        if mode == "oversize_vocabulary" {
            crate::tagging::rename(&f.store, &json!("favorite"), &json!("x".repeat(101))).unwrap();
            let f = f.into_candidate();
            assert_bootstrap_obligation(&f, &[], "imported assertion transition remains unproved");
            continue;
        }
        let names = if mode == "extra" {
            crate::tagging::put(&f.store, &json!("new"), None).unwrap();
            vec![json!("new")]
        } else {
            vec![json!("favorite")]
        };
        let items = vec![SUBJECT.to_owned()];
        let source = json!("motion.nfo");
        let note = if mode == "away_back" {
            // Return every writable assertion value to the exact original; the
            // permanent revision still records that this was not unchanged.
            json!({"import_operation_id":f.activation.key().operation_id(),"source_revision":9})
                .to_string()
        } else {
            String::new()
        };
        let assign = |remove| {
            crate::tagging::assign(
                &f.store,
                crate::tagging::Assignment {
                    names: &names,
                    items: &items,
                    files: &[],
                    source: &source,
                    confidence: &Value::Null,
                    note: &json!(note),
                    remove,
                },
            )
            .unwrap()
        };
        assign(mode != "extra");
        if mode == "away_back" {
            assign(false);
        }
        let f = f.into_candidate();
        assert_bootstrap_obligation(&f, &[], "imported assertion transition remains unproved");
    }
}

#[test]
fn reused_vocabulary_ids_and_independent_local_suppression_preserve_imported_assertions() {
    use crate::catalog_tag_mutation::{SuppressionChange, TagAuthority, TagMutation, TagSubject};
    let f = Fixture::with_tag_setup(
        2,
        "tmdb:42",
        json!({"values":{"title":"NFO title"},"tags":["favorite"],"excluded_tags":[]}),
        None,
        vec![imported_tag_set(&["favorite"])],
        |store| {
            crate::tagging::put(store, &json!("favorite"), Some(&json!("preexisting"))).unwrap();
        },
    );
    let tag = crate::tagging::resolve_tag(&f.store.db, &json!("favorite")).unwrap();
    assert_eq!(uuid::Uuid::parse_str(&tag).unwrap().get_version_num(), 4);
    let mutation = TagMutation::SetSuppression {
        contract_version: 1,
        changes: vec![SuppressionChange {
            subject: TagSubject::Item(SUBJECT.into()),
            canonical_tag_id: tag,
            expected_revision: None,
            suppressed: true,
            reason: "independent local decision".into(),
        }],
    };
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(&f.store.database_id, HOST, "local-suppression").unwrap(),
        f.store.catalog_incarnation.as_deref().unwrap(),
        generation(&f.store),
        &json!({"operation":"tags","mutation":mutation}).to_string(),
    )
    .unwrap();
    let grant = TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [TagSubject::Item(SUBJECT.into())].into(),
        allow_global_merge: false,
    };
    assert!(matches!(
        commands::execute_with_authority(
            &f.store,
            &command,
            CatalogAuthority::new(base()).with_tags(&grant),
            proof_policy()
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    // The authenticated original IDs survive canonical rename and removal of
    // the old requested alias; a new vocabulary row may now reuse that spelling.
    crate::tagging::rename(&f.store, &json!("favorite"), &json!("renamed")).unwrap();
    crate::tagging::alias(&f.store, &json!("renamed"), &json!("favorite"), true).unwrap();
    crate::tagging::put(&f.store, &json!("favorite"), None).unwrap();
    let f = f.into_candidate();
    let before = proof_database_snapshot(&f.store);
    let baseline = f.verify().unwrap();
    assert!(baseline.imported_tag_mapping.is_some());
    f.store
        .transaction_detailed(|_| {
            let source = super::super::verify_source_history_in(
                &f.store,
                &baseline,
                &[],
                TrustedCommandAuthority {
                    allow_new_commands: false,
                    ..base()
                },
                generation(&f.store),
            )?;
            source.imported_tag_admission().unwrap().require()?;
            super::super::bootstrap::prepare_in(&f.store, &[source])?;
            Ok::<_, Error>(())
        })
        .unwrap();
    assert_eq!(proof_database_snapshot(&f.store), before);
}

fn replace_tags(
    f: &Fixture,
    operation: &str,
    record: &str,
    expected: i64,
    tags: &[&str],
) -> CommandEnvelope {
    execute_replace_tags(f, operation, record, expected, tags, None)
}
fn execute_replace_tags(
    f: &Fixture,
    operation: &str,
    record: &str,
    expected: i64,
    tags: &[&str],
    expected_error: Option<&str>,
) -> CommandEnvelope {
    use crate::catalog_metadata_store::{self as metadata, ContributionReplace};
    use crate::catalog_tag_mutation::{TagAuthority, TagSubject};
    let request = ContributionReplace {
        contract_version: 2,
        subject: SUBJECT.into(),
        namespace: "motion.nfo".into(),
        record_id: record.into(),
        expected_schema_fingerprint: metadata::current_schema_fingerprint(&f.store.db, SUBJECT)
            .unwrap(),
        expected_revision: expected,
        fields_json: "{}".into(),
        evidence_json: "{}".into(),
        tags: tags.iter().map(|s| (*s).into()).collect(),
        excluded_tags: vec![],
        origin_creation_intent: None,
    };
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(&f.store.database_id, HOST, operation).unwrap(),
        f.store.catalog_incarnation.as_deref().unwrap(),
        generation(&f.store),
        &json!({"operation":"metadata_contribution_replace","request":request}).to_string(),
    )
    .unwrap();
    let grant = |access| MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access,
    };
    let source = grant(MetadataAccess::Source {
        namespace: "motion.nfo".into(),
        record_id: record.into(),
        kind: SourceKind::Provider,
    });
    let initialize = grant(MetadataAccess::InitializeV2 {
        system_fields: Default::default(),
    });
    let extend = grant(MetadataAccess::ExtendSchema);
    let tags = TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [TagSubject::Item(SUBJECT.into())].into(),
        allow_global_merge: false,
    };
    let outcome = commands::execute_with_authority(
        &f.store,
        &command,
        CatalogAuthority::new(base())
            .with_metadata(&source)
            .with_metadata_initialization(&initialize)
            .with_metadata_schema(&extend)
            .with_tags(&tags),
        proof_policy(),
    );
    if let Some(message) = expected_error {
        let error = outcome.unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    } else {
        assert!(matches!(outcome.unwrap(), CommandOutcome::Committed { .. }));
    }
    command
}
fn assert_accounted_bootstrap(f: &Fixture, later: &[CommandEnvelope]) {
    let baseline = f.verify().unwrap();
    let before = proof_database_snapshot(&f.store);
    f.store
        .transaction_detailed(|_| {
            let source = super::super::verify_source_history_in(
                &f.store,
                &baseline,
                later,
                TrustedCommandAuthority {
                    allow_new_commands: false,
                    ..base()
                },
                generation(&f.store),
            )?;
            assert_eq!(source.automatic_identity(), Some("tmdb:42"));
            source.imported_tag_admission().unwrap().require()?;
            super::super::bootstrap::prepare_in(&f.store, &[source])?;
            Ok::<_, Error>(())
        })
        .unwrap();
    assert_eq!(proof_database_snapshot(&f.store), before);
}
fn assertion_head(f: &Fixture, name: &str) -> (String, bool, i64, String, String, String) {
    f.store.db.query_row(
        "SELECT a.id,a.active,e.revision,a.note,a.created_at,a.updated_at FROM item_tags a JOIN tag_names n ON n.tag_id=a.tag_id JOIN catalog_entity_revisions e ON e.entity_kind='item_tags' AND e.entity_key=json_array(a.id) WHERE a.item_id=? AND a.source='motion.nfo' AND n.name=?",
        rusqlite::params![SUBJECT,name], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    ).unwrap()
}
fn metadata_time(f: &Fixture, revision: i64, command: &CommandEnvelope) -> i64 {
    f.store.db.query_row(
        "SELECT admitted_at_us FROM catalog_metadata_history WHERE item_id=? AND namespace='motion.nfo' AND record_id='document' AND revision=? AND command_catalog_id=? AND command_host_namespace=? AND command_operation_id=?",
        rusqlite::params![SUBJECT,revision,command.key().catalog_id(),command.key().host_namespace(),command.key().operation_id()],|r|r.get(0),
    ).unwrap()
}
fn expected_time(at: i64) -> String {
    chrono::DateTime::from_timestamp_micros(at)
        .unwrap()
        .to_rfc3339()
}
#[test]
fn later_empty_replacements_are_recorded_and_accounted_not_unchanged() {
    let f = Fixture::new(2);
    let first = replace_tags(&f, "empty-first", "document", 1, &[]);
    let second = replace_tags(&f, "empty-identical", "document", 2, &[]);
    let count: i64 = f
        .store
        .db
        .query_row(
            "SELECT count(*) FROM item_tags WHERE item_id=? AND source='motion.nfo'",
            [SUBJECT],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    let histories:i64=f.store.db.query_row("SELECT count(*) FROM catalog_metadata_tag_history WHERE item_id=? AND namespace='motion.nfo'",[SUBJECT],|r|r.get(0)).unwrap();
    assert_eq!(histories, 2); // Explicit identical PUT is not receipt replay.
    let f = f.into_candidate();
    assert_accounted_bootstrap(&f, &[first, second]);
}
#[test]
fn imported_assertions_replay_empty_reappearance_identical_put_and_receipt_noop() {
    for version in [1, 2] {
        let f = tagged_fixture(version);
        let imported = assertion_head(&f, "favorite");
        assert_eq!(imported.2, 1);
        // Typed import passes this exact admitted time to BOTH owners. Receipt
        // commit time is not used to reconstruct any assertion timestamp.
        let at = expected_time(metadata_time(&f, 1, &f.activation));
        assert_eq!((&imported.4, &imported.5), (&at, &at));
        let empty = replace_tags(&f, "remove-tags", "document", 1, &[]);
        let removed = assertion_head(&f, "favorite");
        assert_eq!((&removed.0, removed.1, removed.2), (&imported.0, false, 2));
        assert_eq!(removed.3, imported.3);
        assert_eq!(removed.5, expected_time(metadata_time(&f, 2, &empty)));
        let reappear = replace_tags(&f, "reappear-tags", "document", 2, &["favorite"]);
        let reappeared = assertion_head(&f, "favorite");
        assert_eq!(
            (&reappeared.0, reappeared.1, reappeared.2),
            (&imported.0, true, 3)
        );
        let identical = replace_tags(&f, "identical-tags", "document", 3, &["favorite"]);
        let final_head = assertion_head(&f, "favorite");
        assert_eq!(
            (&final_head.0, final_head.1, final_head.2),
            (&imported.0, true, 5)
        );
        assert_eq!(final_head.4, imported.4);
        assert_eq!(
            final_head.5,
            expected_time(metadata_time(&f, 4, &identical))
        );
        let before = proof_database_snapshot(&f.store);
        assert!(matches!(
            commands::execute_with_authority(
                &f.store,
                &identical,
                CatalogAuthority::new(base()),
                proof_policy(),
            )
            .unwrap(),
            CommandOutcome::Replay { .. }
        ));
        assert_eq!(proof_database_snapshot(&f.store), before);
        let f = f.into_candidate();
        assert_accounted_bootstrap(&f, &[empty, reappear, identical]);
    }
}
#[test]
fn repeated_stable_aliases_keep_exact_owner_writes_history_and_counters() {
    let f = tagged_fixture(2);
    crate::tagging::alias(&f.store, &json!("favorite"), &json!("fav"), false).unwrap();
    crate::tagging::put(&f.store, &json!("new"), None).unwrap();
    crate::tagging::alias(&f.store, &json!("new"), &json!("newalias"), false).unwrap();
    let old = assertion_head(&f, "favorite");
    let favorite_id = crate::tagging::resolve_tag(&f.store.db, &json!("favorite")).unwrap();
    let first = replace_tags(
        &f,
        "alias-tags",
        "document",
        1,
        &["newalias", "favorite", "new", "fav"],
    );
    assert_eq!(assertion_head(&f, "favorite").2, 3); // +2, not +3 for the alias.
    assert_eq!(assertion_head(&f, "new").2, 1); // INSERT then exact UPDATE no-op.
    let (before,after,at):(String,String,i64)=f.store.db.query_row(
        "SELECT before_json,after_json,admitted_at_us FROM catalog_metadata_tag_history WHERE item_id=? AND namespace='motion.nfo' AND record_id='document' AND revision=2",
        [SUBJECT],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).unwrap();
    assert_eq!(
        before,
        json!({"assertions":[[old.0,favorite_id,1,old.3,null]],"suppressions":[]}).to_string()
    );
    assert_eq!(
        after,
        json!({"tags":["fav","favorite","new","newalias"],"excluded_tags":[]}).to_string()
    );
    assert_eq!(at, metadata_time(&f, 2, &first));
    // Suppressions can legitimately advance between these provider writes.
    // Their before snapshots are evidence, not NFO-owned transition state.
    use crate::catalog_tag_mutation::{SuppressionChange, TagAuthority, TagMutation, TagSubject};
    let mutation = TagMutation::SetSuppression {
        contract_version: 1,
        changes: vec![SuppressionChange {
            subject: TagSubject::Item(SUBJECT.into()),
            canonical_tag_id: favorite_id,
            expected_revision: None,
            suppressed: true,
            reason: "local independent choice".into(),
        }],
    };
    let local = CommandEnvelope::new(
        1,
        CommandKey::new(&f.store.database_id, HOST, "between-provider-tags").unwrap(),
        f.store.catalog_incarnation.as_deref().unwrap(),
        generation(&f.store),
        &json!({"operation":"tags","mutation":mutation}).to_string(),
    )
    .unwrap();
    let grant = TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [TagSubject::Item(SUBJECT.into())].into(),
        allow_global_merge: false,
    };
    assert!(matches!(
        commands::execute_with_authority(
            &f.store,
            &local,
            CatalogAuthority::new(base()).with_tags(&grant),
            proof_policy()
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let second = replace_tags(
        &f,
        "alias-tags-again",
        "document",
        2,
        &["fav", "favorite", "new", "newalias"],
    );
    assert_eq!(assertion_head(&f, "favorite").2, 5);
    assert_eq!(assertion_head(&f, "new").2, 3);
    // Presentation rename reserves the new canonical name but retains every
    // original requested alias unchanged. It is not an assertion transition.
    let retained = assertion_head(&f, "favorite");
    crate::tagging::rename(&f.store, &json!("favorite"), &json!("renamed")).unwrap();
    assert_eq!(assertion_head(&f, "renamed"), retained);
    for alias in ["fav", "favorite"] {
        assert_eq!(
            crate::tagging::resolve_tag(&f.store.db, &json!(alias)).unwrap(),
            crate::tagging::resolve_tag(&f.store.db, &json!("renamed")).unwrap()
        );
        let stamp:(i64,bool)=f.store.db.query_row(
            "SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='tag_names' AND entity_key=json_array(?)",
            [alias],|r|Ok((r.get(0)?,r.get(1)?)),
        ).unwrap();
        assert_eq!(stamp, (1, false));
    }
    let f = f.into_candidate();
    assert_accounted_bootstrap(&f, &[first, second]);
}
#[test]
fn recorded_v1_patch_preserves_imported_tags_across_v2_replacement() {
    use crate::catalog_metadata_store::{self as metadata, MetadataChange, MetadataMutation};
    let f = tagged_fixture(1);
    let before = assertion_head(&f, "favorite");
    let mutation=MetadataMutation {
        contract_version:1,
        change:MetadataChange::Patch {
            subject:SUBJECT.into(),namespace:"motion.nfo".into(),record_id:"document".into(),
            schema_fingerprint:metadata::current_schema_fingerprint(&f.store.db,SUBJECT).unwrap().unwrap(),
            expected_revision:1,
            edits_json:json!({"title":{"op":"set","value":"changed title"}}).to_string(),
            evidence_json:json!({"owner":"motion.metadata.replace","request_digest":"a".repeat(64),"expected_aggregate_revision":1}).to_string(),
        },
    };
    let patch = CommandEnvelope::new(
        1,
        CommandKey::new(&f.store.database_id, HOST, "v1-preserves-tags").unwrap(),
        f.store.catalog_incarnation.as_deref().unwrap(),
        generation(&f.store),
        &json!({"operation":"metadata","mutation":mutation}).to_string(),
    )
    .unwrap();
    let source = MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::Source {
            namespace: "motion.nfo".into(),
            record_id: "document".into(),
            kind: SourceKind::Provider,
        },
    };
    assert!(matches!(
        commands::execute_with_authority(
            &f.store,
            &patch,
            CatalogAuthority::new(base()).with_metadata(&source),
            proof_policy()
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    assert_eq!(assertion_head(&f, "favorite"), before);
    let replacement = replace_tags(&f, "after-v1", "document", 2, &["favorite"]);
    assert_eq!(assertion_head(&f, "favorite").2, 3);
    let f = f.into_candidate();
    assert_accounted_bootstrap(&f, &[patch, replacement]);
}
#[test]
fn recorded_replacements_do_not_hide_unjournaled_away_back_or_rebound_aliases() {
    for mode in ["away_back", "rebound_alias"] {
        let f = tagged_fixture(2);
        let put = replace_tags(&f, "before-unknown", "document", 1, &["favorite"]);
        let mut later = vec![put];
        if mode == "away_back" {
            let original = assertion_head(&f, "favorite");
            for remove in [true, false] {
                crate::tagging::assign(
                    &f.store,
                    crate::tagging::Assignment {
                        names: &[json!("favorite")],
                        items: &[SUBJECT.into()],
                        files: &[],
                        source: &json!("motion.nfo"),
                        confidence: &Value::Null,
                        note: &json!(original.3),
                        remove,
                    },
                )
                .unwrap();
            }
        } else {
            crate::tagging::rename(&f.store, &json!("favorite"), &json!("renamed")).unwrap();
            crate::tagging::alias(&f.store, &json!("renamed"), &json!("favorite"), true).unwrap();
            crate::tagging::put(&f.store, &json!("favorite"), None).unwrap();
        }
        if mode == "away_back" {
            // A subsequent legitimate PUT restores exact timestamp/note and
            // active values, but cannot erase the unjournaled +2 entity counter.
            later.push(replace_tags(
                &f,
                "after-unknown",
                "document",
                2,
                &["favorite"],
            ));
            assert_eq!(assertion_head(&f, "favorite").2, 7);
        }
        let f = f.into_candidate();
        assert_bootstrap_obligation(
            &f,
            &later,
            if mode == "away_back" {
                "imported assertion transition remains unproved"
            } else {
                "historical requested-name mapping remains unproved"
            },
        );
    }
}
#[test]
fn foreign_record_replacement_refuses_before_history_or_assertion_mutation() {
    let f = tagged_fixture(2);
    let before = proof_database_snapshot(&f.store);
    // The real metadata owner forbids this namespace-scoped ambiguity. No
    // fabricated immutable foreign-record history is used as a fixture.
    execute_replace_tags(
        &f,
        "other-record",
        "other-record",
        0,
        &["favorite"],
        Some("namespace-scoped tags require one contribution record per source"),
    );
    assert_eq!(proof_database_snapshot(&f.store), before);
    let f = f.into_candidate();
    assert_accounted_bootstrap(&f, &[]);
}
#[test]
fn missing_or_extra_recorded_source_proofs_do_not_authorize_tag_bootstrap() {
    let f = tagged_fixture(2);
    let first = replace_tags(&f, "history-first", "document", 1, &[]);
    let second = replace_tags(&f, "history-second", "document", 2, &["favorite"]);
    let f = f.into_candidate();
    let baseline = f.verify().unwrap();
    for later in [
        vec![first.clone()],
        vec![first.clone(), second.clone(), second.clone()],
        vec![second.clone(), first.clone()],
    ] {
        let before = proof_database_snapshot(&f.store);
        assert!(
            f.store
                .transaction_detailed(|_| {
                    super::super::verify_source_history_in(
                        &f.store,
                        &baseline,
                        &later,
                        TrustedCommandAuthority {
                            allow_new_commands: false,
                            ..base()
                        },
                        generation(&f.store),
                    )
                    .map(|_| ())
                })
                .is_err()
        );
        assert_eq!(proof_database_snapshot(&f.store), before);
    }
    assert_accounted_bootstrap(&f, &[first, second]);
}

#[test]
fn unrelated_output_tampering_fails_complete_committed_mapping_digest() {
    // Candidate bytes come from the actual full Activated owner result. Alter
    // only a local decoded candidate, never immutable receipt/history rows.
    for part in ["logical", "typed"] {
        let f = tagged_fixture(2);
        let baseline = f.verify().unwrap();
        assert!(baseline.imported_tag_mapping.is_some());
        let replay = commands::reconcile_receipt(
            &f.store,
            &f.activation,
            TrustedCommandAuthority {
                allow_new_commands: false,
                ..base()
            },
        )
        .unwrap()
        .unwrap();
        let catalog_import::ImportReceipt::Activated {
            mut mappings,
            mut typed_results,
            ..
        } = serde_json::from_value(result(&replay).clone()).unwrap()
        else {
            panic!("actual activation required");
        };
        if part == "logical" {
            mappings[0].source_id.push_str("-changed");
        } else {
            typed_results.push(json!({"owner":"unrelated-output","value":1}));
        }
        let before = proof_database_snapshot(&f.store);
        let error = f
            .store
            .transaction_detailed(|_| {
                let mut budget = ProofBudget::new(f.store.catalog_operation_budget()?);
                super::imported_tag_mapping::authenticate_mapping(
                    &mappings,
                    &typed_results,
                    &f.grant.mapping_digest,
                    &mut budget,
                )
            })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("complete import mapping digest mismatch")
        );
        assert_eq!(proof_database_snapshot(&f.store), before);
    }
}
