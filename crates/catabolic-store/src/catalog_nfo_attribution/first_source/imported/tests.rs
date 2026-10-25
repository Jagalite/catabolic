//! Real typed import/receipt owners followed by the explicitly unregistered41
//! installer and actual first-PUT composition. This controlled host owns the
//! entire constructed archive, including all four positive absence facts. These
//! tests do not qualify a production Motion archive audit builder.
use super::*;
use crate::{
    candidate41_test_support as candidate,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy,
        TrustedReceiptMaintenance,
    },
    catalog_import::{ActivationBinding, ImportAuthority, TrustedImportActivation},
    catalog_import_typed::{ImportedTagSet, TagImportAuthority, TypedAuthority, TypedBatch},
    catalog_metadata_store::{
        self as metadata, ContributionReplace, MetadataAccess, MetadataAuthority,
    },
    catalog_nfo_attribution::{bootstrap, first_source, public_put, state},
    catalog_pins::{self, PinAuthority, PinBatch, PinChange, PinDecision, PinMutation},
    catalog_tag_mutation::{SuppressionChange, TagAuthority, TagMutation, TagSubject},
    migration,
};
use catabolic_core::{
    catalog_command::{CommandKey, ReceiptVisibility},
    catalog_host_lease::HostAuthorization,
};
use std::time::Duration;
const HOST: &str = "imported-first-test";
const SUBJECT: &str = "first-work";
const OTHER: &str = "other-work";
const AGGREGATE: i64 = 7;
fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap()
}
fn authority(write: bool) -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: write,
        identity_authority: None,
    }
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
    }
}
fn envelope(store: &Store, id: &str, value: Value) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, HOST, id).unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        generation(store),
        &value.to_string(),
    )
    .unwrap()
}
fn result(outcome: CommandOutcome) -> Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        other => panic!("unexpected {other:?}"),
    }
}
fn grant(access: MetadataAccess) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access,
    }
}
fn tags() -> TagAuthority {
    TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [TagSubject::Item(SUBJECT.into())].into(),
        allow_global_merge: false,
    }
}
pub(in crate::catalog_nfo_attribution) struct Part {
    pub(in crate::catalog_nfo_attribution) manifest: SnapshotManifest,
    pub(in crate::catalog_nfo_attribution) batches: Vec<ImportBatch>,
    pub(in crate::catalog_nfo_attribution) activation: CommandEnvelope,
    pub(in crate::catalog_nfo_attribution) mapping: String,
}
struct Fixture {
    root: tempfile::TempDir,
    store: Store,
    parts: Vec<Part>,
}
impl Fixture {
    fn new(version: u32, part_count: usize) -> Self {
        Self::configured(version, part_count, |_, _| {})
    }
    fn configured(
        version: u32,
        part_count: usize,
        configure: impl Fn(usize, &mut ImportBatch),
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("imported-first.db");
        migration::initialize_native_at(&path, 40).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let mut parts = Vec::new();
        for index in 0..part_count {
            let subject = if index == 0 { SUBJECT } else { OTHER };
            let mut batch = ImportBatch {
                items: vec![ImportItem {
                    source_id: format!("logical-{subject}"),
                    item_id: subject.into(),
                    kind: "video".into(),
                    legacy_metadata: json!({}),
                }],
                unresolved_evidence: vec![],
                typed: Some(TypedBatch {
                    metadata: vec![ImportedMetadata {
                        contract_version: version,
                        system_fields: if version == 2 {
                            vec!["motion_catalog".into()]
                        } else {
                            vec![]
                        },
                        item_id: subject.into(),
                        policies: json!([
                            {"field":"motion_catalog","value_type":"json","nullable":false,"normalization":"exact"},
                            {"field":"title","value_type":"string","nullable":false,"normalization":"trim_ascii_lowercase"}
                        ]),
                        contributions: vec![ImportedContribution {
                            namespace: "motion.structure".into(),
                            record_id: "source_projection".into(),
                            source_revision: 4,
                            kind: ImportedSourceKind::Scan,
                            fields: json!({"motion_catalog":{"contract_version":1,"item_id":subject,"external_ids":[]}}),
                            evidence: json!({"source_tables":["items","item_structure"]}),
                        }],
                    }],
                    ..Default::default()
                }),
            };
            configure(index, &mut batch);
            let manifest = SnapshotManifest {
                contract_version: 2,
                import_id: format!("root-part-{index}"),
                source_namespace: "motion.schema68.export.v4".into(),
                source_snapshot_id: format!("root-snapshot-{index}"),
                batches: vec![catalog_import::describe_batch(&batch).unwrap()],
            };
            parts.push(activate(&store, manifest, vec![batch]));
        }
        Self { root, store, parts }
    }
    fn install(mut self) -> Self {
        let before = migration::snapshot(&self.store.db).unwrap();
        let path = self.root.path().join("imported-first.db");
        drop(self.store);
        candidate::install_candidate41_for_test(&path).unwrap();
        self.store = candidate::open_candidate41_for_test(&path, true).unwrap();
        migration::preservation(&self.store.db, &before).unwrap();
        assert!(!migration::is_supported_native_schema(41));
        self
    }
    fn ready(self) -> Self {
        let f = self.install();
        let plan = f
            .store
            .transaction_detailed(|_| bootstrap::prepare_in(&f.store, &[]))
            .unwrap();
        let command = envelope(&f.store, "imported-empty-bootstrap", plan.payload());
        result(
            commands::execute_candidate_for_test(
                &f.store,
                &command,
                authority(true),
                policy(),
                move |store, outer, _| bootstrap::apply_in(store, outer, plan),
            )
            .unwrap(),
        );
        f
    }
    fn parts(&self) -> Vec<ImportedFirstPart<'_>> {
        self.parts
            .iter()
            .enumerate()
            .map(|(index, p)| ImportedFirstPart {
                ordinal: index as u32,
                manifest: &p.manifest,
                batches: &p.batches,
                activation: &p.activation,
                reference_ack_id: "archive-ack",
                mapping_digest: &p.mapping,
            })
            .collect()
    }
    fn audit(&self, outer: &CommandEnvelope) -> ImportedFirstRootAudit {
        ImportedFirstRootAudit{catalog_id:self.store.database_id.clone(),incarnation:self.store.catalog_incarnation.clone().unwrap(),generation:outer.expected_generation(),host_namespace:HOST.into(),outer_digest:outer.digest().as_str().into(),root_import_id:"constructed-root".into(),root_manifest_digest:Some(catabolic_core::sha256("constructed-complete-root")),archived_audit_digest:catabolic_core::sha256(json!({"root":"constructed-root","subject":SUBJECT,"original_documents":[],"automatic_identities":[],"nfo_tags":[],"sidecar_origins":[]}).to_string()),ordered_parts:self.parts.iter().enumerate().map(|(index,p)|ImportedFirstPartCommitment{ordinal:index as u32,import_id:p.manifest.import_id.clone(),input_digest:catalog_import::manifest_digest(&p.manifest).unwrap(),activation_digest:p.activation.digest().as_str().into(),reference_ack_id:"archive-ack".into(),mapping_digest:p.mapping.clone()}).collect(),subjects:[(SUBJECT.into(),format!("logical-{SUBJECT}"))].into()}
    }
    fn request(&self) -> ContributionReplace {
        ContributionReplace{contract_version:2,subject:SUBJECT.into(),namespace:"motion.nfo".into(),record_id:"document".into(),expected_schema_fingerprint:metadata::current_schema_fingerprint(&self.store.db,SUBJECT).unwrap(),expected_revision:0,fields_json:json!({"title":" New NFO "}).to_string(),evidence_json:json!({"owner":"motion.metadata.replace","request_digest":catabolic_core::sha256("put"),"expected_aggregate_revision":AGGREGATE}).to_string(),tags:vec!["hidden".into()],excluded_tags:vec![],origin_creation_intent:None}
    }
    fn command(&self, id: &str, request: &ContributionReplace) -> CommandEnvelope {
        envelope(
            &self.store,
            id,
            json!({"operation":"nfo_public_put","contract_version":1,"expected_public_revision":0,"request":request}),
        )
    }
    fn verify(
        &self,
        outer: &CommandEnvelope,
        audit: &ImportedFirstRootAudit,
        parts: &[ImportedFirstPart<'_>],
    ) -> Result<()> {
        self.store.transaction_detailed(|_| {
            let current =
                state::read_in(&self.store, SUBJECT, outer.expected_generation(), AGGREGATE)?;
            let proof = verify_in(&self.store, outer, &current, parts, audit, authority(false))?;
            assert_eq!(proof.audit_digest().len(), 64);
            assert_eq!(proof.proof_digest().len(), 64);
            proof.validate_in(&self.store, outer, &current)
        })
    }
    fn apply(
        &self,
        outer: &CommandEnvelope,
        request: &ContributionReplace,
        rollback: bool,
    ) -> commands::Result<CommandOutcome> {
        self.apply_with_classification(outer, request, rollback, None)
    }
    fn apply_with_classification(
        &self,
        outer: &CommandEnvelope,
        request: &ContributionReplace,
        rollback: bool,
        classification: Option<BTreeSet<String>>,
    ) -> commands::Result<CommandOutcome> {
        let audit = self.audit(outer);
        let parts = self.parts();
        commands::execute_candidate_for_test(
            &self.store,
            outer,
            authority(true),
            policy(),
            |store, outer, at| {
                let current =
                    state::read_in(store, SUBJECT, outer.expected_generation(), AGGREGATE)?;
                let first = first_source::prepare_imported_in(
                    store,
                    outer,
                    current,
                    &parts,
                    &audit,
                    authority(false),
                )?;
                assert!(first.creation().is_err());
                assert!(first.origin().is_none());
                assert!(first.legacy_raw_json().is_none());
                // A title-only PUT does not authorize classification of retained
                // structural fields. Preserve the authenticated existing policy.
                let system_fields = classification.clone().unwrap_or_else(|| {
                    first
                        .current()
                        .schema()
                        .expect("managed imported schema")
                        .system_fields()
                        .clone()
                });
                let published = public_put::apply_first_put_in(
                    store,
                    outer,
                    request,
                    0,
                    first,
                    &grant(MetadataAccess::Source {
                        namespace: "motion.nfo".into(),
                        record_id: "document".into(),
                        kind: SourceKind::Provider,
                    }),
                    &grant(MetadataAccess::InitializeV2 { system_fields }),
                    &grant(MetadataAccess::ExtendSchema),
                    &tags(),
                    at,
                )?;
                if rollback {
                    return Err(Error(
                        "intentional error after actual imported publication".into(),
                    ));
                }
                Ok(
                    json!({"source_revision":published.source_revision(),"presence":published.presence(),"proof":published.source_proof_digest()}),
                )
            },
        )
    }
    fn expire_activation_results(&self) {
        for part in &self.parts {
            commands::compact_receipts(
                &self.store,
                TrustedReceiptMaintenance {
                    host_namespace: HOST,
                    holder_namespace: "imported-first-maintenance",
                },
                100,
            )
            .unwrap();
            let state:String=self.store.db.query_row("SELECT result_state FROM catalog_command_receipts WHERE catalog_id=? AND host_namespace=? AND operation_id=?",rusqlite::params![part.activation.key().catalog_id(),HOST,part.activation.key().operation_id()],|r|r.get(0)).unwrap();
            assert_eq!(state, "expired");
            assert!(matches!(
                commands::reconcile_receipt(&self.store, &part.activation, authority(false))
                    .unwrap(),
                Some(CommandOutcome::ReceiptExpired)
            ));
        }
    }
}
fn activate(store: &Store, manifest: SnapshotManifest, batches: Vec<ImportBatch>) -> Part {
    activate_for_host(store, manifest, batches, HOST)
}
/// Shared actual closed import fixture, not a constructor for verified tokens.
pub(in crate::catalog_nfo_attribution) fn activate_for_host(
    store: &Store,
    manifest: SnapshotManifest,
    batches: Vec<ImportBatch>,
    host: &str,
) -> Part {
    let command_authority = || TrustedCommandAuthority {
        host_namespace: host,
        ..authority(true)
    };
    let input = catalog_import::manifest_digest(&manifest).unwrap();
    let import = ImportAuthority {
        host_namespace: host.into(),
        import_id: manifest.import_id.clone(),
        source_namespace: manifest.source_namespace.clone(),
    };
    let mut typed = TypedAuthority::default();
    for metadata in batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|b| &b.metadata)
    {
        typed.metadata.push(MetadataAuthority {
            host_namespace: host.into(),
            subject: metadata.item_id.clone(),
            access: if metadata.contract_version == 2 {
                MetadataAccess::InitializeV2 {
                    system_fields: metadata.system_fields.iter().cloned().collect(),
                }
            } else {
                MetadataAccess::Initialize
            },
        });
        for source in &metadata.contributions {
            typed.metadata.push(MetadataAuthority {
                host_namespace: host.into(),
                subject: metadata.item_id.clone(),
                access: if metadata.contract_version == 2 {
                    MetadataAccess::SourceV2 {
                        namespace: source.namespace.clone(),
                        record_id: source.record_id.clone(),
                        kind: match source.kind {
                            ImportedSourceKind::Local => SourceKind::Local,
                            ImportedSourceKind::Provider => SourceKind::Provider,
                            ImportedSourceKind::Scan => SourceKind::Scan,
                        },
                        system_fields: metadata
                            .system_fields
                            .iter()
                            .filter(|name| source.fields.get(name.as_str()).is_some())
                            .cloned()
                            .collect(),
                    }
                } else {
                    MetadataAccess::Source {
                        namespace: source.namespace.clone(),
                        record_id: source.record_id.clone(),
                        kind: match source.kind {
                            ImportedSourceKind::Local => SourceKind::Local,
                            ImportedSourceKind::Provider => SourceKind::Provider,
                            ImportedSourceKind::Scan => SourceKind::Scan,
                        },
                    }
                },
            });
        }
    }
    for tags in batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|b| &b.tags)
    {
        typed.tag_sources.push(TagImportAuthority {
            host_namespace: host.into(),
            namespace: tags.namespace.clone(),
            item_ids: [tags.item_id.clone()].into(),
            allow_suppressions: tags.namespace == "motion.local",
        });
    }
    let run = |mutation: ImportMutation,
               step: ImportStep,
               active: Option<&TrustedImportActivation>| {
        let outer = CommandEnvelope::new(
            1,
            CommandKey::new(
                &store.database_id,
                host,
                &catalog_import::operation_id(&manifest.import_id, step).unwrap(),
            )
            .unwrap(),
            store.catalog_incarnation.as_deref().unwrap(),
            generation(store),
            &json!({"operation":"import","mutation":mutation}).to_string(),
        )
        .unwrap();
        let mut auth = CatalogAuthority::new(command_authority())
            .with_imports(&import)
            .with_import_typed(&typed);
        if let Some(active) = active {
            auth = auth.with_import_activation(active);
        }
        let mut import_policy = policy();
        import_policy.retention = Duration::ZERO;
        let result =
            result(commands::execute_with_authority(store, &outer, auth, import_policy).unwrap());
        (outer, result)
    };
    run(
        ImportMutation::Prepare {
            manifest: manifest.clone(),
            input_digest: input.clone(),
        },
        ImportStep::Prepare,
        None,
    );
    for (index, batch) in batches.iter().enumerate() {
        run(
            ImportMutation::Stage {
                import_id: manifest.import_id.clone(),
                input_digest: input.clone(),
                batch_index: index as u32,
                batch: batch.clone(),
            },
            ImportStep::Batch(index as u32),
            None,
        );
    }
    let (_, validation) = run(
        ImportMutation::Validate {
            import_id: manifest.import_id.clone(),
            input_digest: input.clone(),
        },
        ImportStep::Validate,
        None,
    );
    let mapping = validation["mapping_digest"].as_str().unwrap().to_owned();
    let gate =
        HostAuthorization::new(host.into(), format!("drain-{}", manifest.import_id)).unwrap();
    let active = TrustedImportActivation::after_drain(
        &gate,
        gate.begin_revocation().unwrap(),
        ActivationBinding {
            catalog_id: store.database_id.clone(),
            incarnation: store.catalog_incarnation.clone().unwrap(),
            import_id: manifest.import_id.clone(),
            input_digest: input.clone(),
            source_snapshot_id: manifest.source_snapshot_id.clone(),
            validation_generation: generation(store),
            reference_ack_id: "archive-ack".into(),
            mapping_digest: mapping.clone(),
        },
    )
    .unwrap();
    let (activation, _) = run(
        ImportMutation::Activate {
            import_id: manifest.import_id.clone(),
            input_digest: input,
            reference_ack_id: "archive-ack".into(),
            mapping_digest: mapping.clone(),
        },
        ImportStep::Activate,
        Some(&active),
    );
    // Official owner release, not cleanup SQL. Result expiry is independent of
    // the permanent exact Activate commitment subsequently used by the proof.
    assert_eq!(
        commands::release_hold(
            store,
            activation.key(),
            TrustedReceiptMaintenance {
                host_namespace: host,
                holder_namespace: catalog_import::IMPORT_HOLDER_NAMESPACE
            },
            &catabolic_core::sha256(manifest.import_id.as_bytes())
        )
        .unwrap(),
        commands::HoldOutcome::Released
    );
    Part {
        manifest,
        batches,
        activation,
        mapping,
    }
}

#[test]
fn actual_v1_v2_complete_imported_root_first_put_survives_expiry_replay_and_restart() {
    for version in [1, 2] {
        let mut f = Fixture::configured(version, 2, |index, batch| {
            if index == 0 {
                batch.items[0].legacy_metadata = json!({"title":" Original Legacy "});
            }
        });
        f.expire_activation_results();
        f = f.ready();
        let original = migration::rows(
            &f.store.db,
            "SELECT * FROM catalog_metadata_history ORDER BY item_id,namespace,record_id,revision",
            &[],
        )
        .unwrap();
        let request = f.request();
        let outer = f.command("first-imported-put", &request);
        f.verify(&outer, &f.audit(&outer), &f.parts()).unwrap();
        if version == 1 {
            let before = migration::snapshot(&f.store.db).unwrap();
            let error = f
                .apply_with_classification(
                    &outer,
                    &request,
                    false,
                    Some(["motion_catalog".into()].into()),
                )
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("NFO PUT reclassified system fields")
            );
            assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
        }
        let value = result(f.apply(&outer, &request, false).unwrap());
        assert_eq!(value["source_revision"], 1);
        assert_eq!(value["presence"]["public_revision"], 1);
        let raw:String=f.store.db.query_row("SELECT fields_json FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo'",[SUBJECT],|r|r.get(0)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&raw).unwrap(),
            json!({"title":"new nfo"})
        );
        for row in original {
            let actual=migration::rows(&f.store.db,"SELECT * FROM catalog_metadata_history WHERE item_id=? AND namespace=? AND record_id=? AND revision=?",&[row["item_id"].as_str().unwrap().to_owned().into(),row["namespace"].as_str().unwrap().to_owned().into(),row["record_id"].as_str().unwrap().to_owned().into(),row["revision"].as_i64().unwrap().into()]).unwrap();
            assert_eq!(actual, vec![row]);
        }
        let no_origin:bool=f.store.db.query_row("SELECT r.document_identity IS NULL AND r.origin_json IS NULL AND r.origin_physical_json IS NULL FROM catalog_nfo_states s JOIN catalog_nfo_state_revisions r ON r.item_id=s.item_id AND r.revision=s.current_revision WHERE s.item_id=?",[SUBJECT],|r|r.get(0)).unwrap();
        assert!(no_origin);
        let path = f.root.path().join("imported-first.db");
        drop(f.store);
        f.store = candidate::open_candidate41_for_test(&path, true).unwrap();
        let before = migration::snapshot(&f.store.db).unwrap();
        assert!(matches!(
            commands::execute_candidate_for_test(
                &f.store,
                &outer,
                authority(true),
                policy(),
                |_, _, _| panic!(
                    "replay must not reconstruct an absence token after first publication"
                )
            )
            .unwrap(),
            CommandOutcome::Replay { .. }
        ));
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    }
}
#[test]
fn actual_imported_first_publication_error_rolls_back_and_same_envelope_can_retry() {
    let f = Fixture::new(2, 1).ready();
    let request = f.request();
    let outer = f.command("rollback-imported-first", &request);
    let mut single = f.audit(&outer);
    single.root_manifest_digest = None;
    single.root_import_id = f.parts[0].manifest.import_id.clone();
    f.verify(&outer, &single, &f.parts()).unwrap();
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(f.apply(&outer, &request, true).is_err());
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    assert_eq!(
        result(f.apply(&outer, &request, false).unwrap())["presence"]["public_revision"],
        1
    );
}
#[test]
fn exact_root_part_order_mapping_and_positive_audit_fences_refuse_without_writes() {
    let f = Fixture::new(2, 2).ready();
    let request = f.request();
    let outer = f.command("audit-refusals", &request);
    let before = migration::snapshot(&f.store.db).unwrap();
    let wrong_mapping = "f".repeat(64);
    let original_activation = &f.parts[0].activation;
    let mut false_payload = original_activation.payload().clone();
    false_payload["mutation"]["mapping_digest"] = json!(wrong_mapping);
    let false_activation = CommandEnvelope::new(
        1,
        original_activation.key().clone(),
        original_activation.admission_incarnation(),
        original_activation.expected_generation(),
        &false_payload.to_string(),
    )
    .unwrap();
    for case in 0..17 {
        let mut audit = f.audit(&outer);
        let mut parts = f.parts();
        match case {
            0 => audit.catalog_id.push('x'),
            1 => audit.incarnation.push('x'),
            2 => audit.generation += 1,
            3 => audit.host_namespace.push('x'),
            4 => audit.outer_digest = "0".repeat(64),
            5 => audit.archived_audit_digest.clear(),
            6 => {
                audit
                    .subjects
                    .insert(SUBJECT.into(), "wrong-logical-source".into());
            }
            7 => audit.ordered_parts[0].activation_digest = "0".repeat(64),
            8 => audit.ordered_parts[0].mapping_digest = "0".repeat(64),
            9 => {
                parts.pop();
            }
            10 => parts.swap(0, 1),
            11 => audit.ordered_parts.swap(0, 1),
            12 => {
                audit
                    .subjects
                    .insert("omitted-subject".into(), "missing-source".into());
            }
            13 => {
                parts[0].activation = &false_activation;
                parts[0].mapping_digest = &wrong_mapping;
                audit.ordered_parts[0].activation_digest =
                    false_activation.digest().as_str().into();
                audit.ordered_parts[0].mapping_digest = wrong_mapping.clone();
            }
            14 => {
                audit.subjects.remove(SUBJECT);
            }
            15 => {
                audit.root_manifest_digest = None;
            }
            16 => {
                audit.root_manifest_digest = None;
                parts.truncate(1);
                audit.ordered_parts.truncate(1);
                audit.root_import_id = "not-the-single-import-id".into();
            }
            _ => unreachable!(),
        }
        assert!(f.verify(&outer, &audit, &parts).is_err(), "case {case}");
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    }
}
#[test]
fn actual_root_foreign_audited_part_identity_and_marker_owner_obligations_are_not_omitted() {
    for case in 0..4 {
        let f = Fixture::configured(2, 2, |index, batch| {
            if index != 1 {
                return;
            }
            let metadata = &mut batch.typed.as_mut().unwrap().metadata[0];
            match case {
                0 => {
                    metadata.contributions[0].fields["motion_catalog"]["external_ids"] =
                        json!([{"provider":"nfo","value":"old"}])
                }
                1 => metadata.contributions[0].fields["motion_catalog"]["item_id"] = json!("wrong"),
                2 => {
                    metadata.contributions[0].fields["motion_catalog"]["contract_version"] =
                        json!(2)
                }
                3 => {
                    let mut extra = metadata.contributions[0].clone();
                    extra.namespace = "motion.extra".into();
                    metadata.contributions.push(extra);
                }
                _ => unreachable!(),
            }
        })
        .ready();
        let request = f.request();
        let outer = f.command("foreign-root-obligation", &request);
        let mut audit = f.audit(&outer);
        audit
            .subjects
            .insert(OTHER.into(), format!("logical-{OTHER}"));
        let before = migration::snapshot(&f.store.db).unwrap();
        assert!(f.verify(&outer, &audit, &f.parts()).is_err());
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    }
}
#[test]
fn actual_original_nfo_tags_block_empty_bootstrap_without_fabricated_absence() {
    let f = Fixture::configured(2, 1, |_, batch| {
        batch.typed.as_mut().unwrap().tags.push(ImportedTagSet {
            item_id: SUBJECT.into(),
            namespace: "motion.nfo".into(),
            source_revision: 1,
            tags: vec!["original".into()],
            excluded_tags: vec![],
        })
    })
    .install();
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(
        f.store
            .transaction_detailed(|_| bootstrap::prepare_in(&f.store, &[]))
            .is_err()
    );
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
}
#[test]
fn actual_structural_source_equal_effective_rewrite_is_not_untouched_import() {
    let f = Fixture::new(2, 1);
    let schema = metadata::current_schema_fingerprint(&f.store.db, SUBJECT)
        .unwrap()
        .unwrap();
    let fields = f.parts[0].batches[0].typed.as_ref().unwrap().metadata[0].contributions[0]
        .fields
        .clone();
    for expected in [1, 2] {
        let mutation = MetadataMutation {
            contract_version: 1,
            change: MetadataChange::Patch {
                subject: SUBJECT.into(),
                namespace: "motion.structure".into(),
                record_id: "source_projection".into(),
                schema_fingerprint: schema.clone(),
                expected_revision: expected,
                edits_json: json!({"motion_catalog":{"op":"set","value":fields["motion_catalog"]}})
                    .to_string(),
                evidence_json: json!({"same_effective":"not_original"}).to_string(),
            },
        };
        let outer = envelope(
            &f.store,
            &format!("structure-rewrite-{expected}"),
            json!({"operation":"metadata","mutation":mutation}),
        );
        result(
            commands::execute_with_authority(
                &f.store,
                &outer,
                CatalogAuthority::new(authority(true)).with_metadata(&grant(
                    MetadataAccess::SourceV2 {
                        namespace: "motion.structure".into(),
                        record_id: "source_projection".into(),
                        kind: SourceKind::Scan,
                        system_fields: ["motion_catalog".into()].into(),
                    },
                )),
                policy(),
            )
            .unwrap(),
        );
    }
    let f = f.ready();
    let request = f.request();
    let outer = f.command("evolved-import", &request);
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(f.verify(&outer, &f.audit(&outer), &f.parts()).is_err());
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
}
#[test]
fn independent_manual_pin_and_local_suppression_survive_first_imported_put() {
    let f = Fixture::new(2, 1);
    let pin_grant = PinAuthority {
        host_namespace: HOST.into(),
        readable: Default::default(),
        mutable: [("nfo".into(), [SUBJECT.into()].into())].into(),
    };
    let batch = PinBatch {
        contract_version: 1,
        changes: vec![PinChange {
            item_id: SUBJECT.into(),
            namespace: "nfo".into(),
            expected_revision: 0,
            decision: PinDecision::Set {
                value: "manual-choice".into(),
            },
        }],
    };
    let preview = catalog_pins::preview_pins(&f.store, &batch, &pin_grant).unwrap();
    let mutation = PinMutation {
        batch,
        preview_digest: preview.digest,
    };
    let outer = envelope(
        &f.store,
        "manual-pin",
        json!({"operation":"pins","mutation":mutation}),
    );
    result(
        commands::execute_with_authority(
            &f.store,
            &outer,
            CatalogAuthority::new(authority(true)).with_pins(&pin_grant),
            policy(),
        )
        .unwrap(),
    );
    crate::tagging::put(&f.store, &json!("hidden"), None).unwrap();
    let tag = crate::tagging::resolve_tag(&f.store.db, &json!("hidden")).unwrap();
    let mutation = TagMutation::SetSuppression {
        contract_version: 1,
        changes: vec![SuppressionChange {
            subject: TagSubject::Item(SUBJECT.into()),
            canonical_tag_id: tag,
            expected_revision: None,
            suppressed: true,
            reason: "independent local suppression".into(),
        }],
    };
    let outer = envelope(
        &f.store,
        "local-suppression",
        json!({"operation":"tags","mutation":mutation}),
    );
    result(
        commands::execute_with_authority(
            &f.store,
            &outer,
            CatalogAuthority::new(authority(true)).with_tags(&tags()),
            policy(),
        )
        .unwrap(),
    );
    let f = f.ready();
    let retained = migration::rows(
        &f.store.db,
        "SELECT * FROM catalog_item_tag_suppressions ORDER BY item_id,tag_id",
        &[],
    )
    .unwrap();
    let request = f.request();
    let outer = f.command("pinned-first-import", &request);
    result(f.apply(&outer, &request, false).unwrap());
    assert_eq!(
        migration::rows(
            &f.store.db,
            "SELECT * FROM catalog_item_tag_suppressions ORDER BY item_id,tag_id",
            &[]
        )
        .unwrap(),
        retained
    );
    f.store
        .transaction_detailed(|_| -> Result<()> {
            let current = state::read_in(&f.store, SUBJECT, generation(&f.store), AGGREGATE)?;
            assert!(matches!(&current.head().pin, catabolic_core::catalog_metadata::nfo_refresh::PinHead::Decision { value: Some(value), .. } if value == "manual-choice"));
            assert!(
                current
                    .head()
                    .attribution
                    .as_ref()
                    .unwrap()
                    .active_identity
                    .is_none()
            );
            Ok(())
        })
        .unwrap();
}
#[test]
fn complete_root_audit_admission_is_cumulative_without_per_part_reset() {
    let f = Fixture::new(2, 1).ready();
    let request = f.request();
    let outer = f.command("root-budget", &request);
    let mut audit = f.audit(&outer);
    for index in 0..9000 {
        audit
            .subjects
            .insert(format!("extra-{index}"), format!("logical-extra-{index}"));
    }
    let error = f.verify(&outer, &audit, &f.parts()).unwrap_err();
    assert!(error.0.contains("budget"), "{error:?}");
}
