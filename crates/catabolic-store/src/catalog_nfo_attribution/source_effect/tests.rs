//! Actual candidate-only transaction owners. These fixtures never register41,
//! manufacture source capabilities, receipts, history or cleanup witnesses.
use super::super::{
    NfoImportAuthority, bootstrap, candidate_tests as fixture, permit, public_put, state,
    verify_imported_no_origin_baseline_in, verify_source_history_in,
};
use crate::{
    Error, Result, Store, candidate41_test_support as candidate,
    catalog_commands::{self as commands, CatalogAuthority, CommandOutcome},
    catalog_import::{
        self as import, ActivationBinding, ImportAuthority, ImportBatch, ImportItem,
        ImportMutation, ImportStep, SnapshotManifest, TrustedImportActivation,
    },
    catalog_import_typed::{
        self as typed, ImportedContribution, ImportedMetadata, ImportedSourceKind, ImportedTagSet,
        TagImportAuthority, TypedAuthority, TypedBatch,
    },
    catalog_metadata_store::{
        self as metadata, ContributionReplace, MetadataAccess, MetadataAuthority, MetadataChange,
        MetadataMutation,
    },
    catalog_tag_mutation::{
        self as tags, SuppressionChange, TagAuthority, TagMerge, TagMutation, TagSubject,
    },
    migration,
};
use catabolic_core::{
    catalog_command::CommandEnvelope, catalog_host_lease::HostAuthorization,
    catalog_metadata::SourceKind,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
const HOST: &str = "nfo-candidate-tests";
const SUBJECT: &str = "nfo-subject";
const OTHER: &str = "other-subject";
fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap()
}
fn grant(namespace: &str, kind: SourceKind) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::Source {
            namespace: namespace.into(),
            record_id: "document".into(),
            kind,
        },
    }
}
fn init() -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::InitializeV2 {
            system_fields: BTreeSet::new(),
        },
    }
}
fn extension() -> MetadataAuthority {
    MetadataAuthority {
        access: MetadataAccess::ExtendSchema,
        ..init()
    }
}
fn tag_grant() -> TagAuthority {
    TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [
            TagSubject::Item(SUBJECT.into()),
            TagSubject::Item(OTHER.into()),
        ]
        .into(),
        allow_global_merge: true,
    }
}
fn committed(outcome: CommandOutcome) -> Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        other => panic!("unexpected {other:?}"),
    }
}
fn put(store: &Store, id: &str, revision: i64, names: &[&str]) -> CommandEnvelope {
    let request = fixture::put_request(store, revision, json!({"title":"new nfo"}), names);
    let command = fixture::envelope(
        store,
        id,
        json!({"operation":"nfo_public_put","contract_version":1,"expected_public_revision":revision,"request":request}),
    );
    committed(
        commands::execute_candidate_for_test(
            store,
            &command,
            fixture::authority(true),
            fixture::policy(),
            |store, command, time| {
                let published = public_put::apply_existing_put_in(
                    store,
                    command,
                    &request,
                    revision,
                    7,
                    &grant("motion.nfo", SourceKind::Provider),
                    &init(),
                    &extension(),
                    &tag_grant(),
                    time,
                )?;
                Ok(json!({"source_revision":published.source_revision()}))
            },
        )
        .unwrap(),
    );
    command
}
fn assign(store: &Store, namespace: &str, item: &str, remove: bool) -> Result<Value> {
    crate::tagging::assign(
        store,
        crate::tagging::Assignment {
            names: &[json!("old")],
            items: &[item.into()],
            files: &[],
            source: &json!(namespace),
            confidence: &Value::Null,
            note: &json!(""),
            remove,
        },
    )
}
#[test]
fn generic_namespace_guards_cover_absent_active_and_inactive_sources_not_operation_spelling() {
    let f = fixture::Fixture::ready();
    for subject in [SUBJECT, OTHER] {
        for remove in [false, true] {
            let before = migration::snapshot(&f.store.db).unwrap();
            assert!(
                assign(&f.store, "motion.nfo", subject, remove)
                    .unwrap_err()
                    .to_string()
                    .contains("attributed source")
            );
            assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
        }
    }
    put(&f.store, "deactivate-source-tags", 1, &[]);
    assert_eq!(f.store.db.query_row("SELECT active FROM item_tags WHERE item_id=? AND source='motion.nfo' AND tag_id=(SELECT tag_id FROM tag_names WHERE name='old')",[SUBJECT],|r|r.get::<_,i64>(0)).unwrap(),0);
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(assign(&f.store, "motion.nfo", SUBJECT, false).is_err());
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    // Source namespace, not operation ID or target item spelling, selects custody.
    let mutation = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Patch {
            subject: SUBJECT.into(),
            namespace: "motion.other".into(),
            record_id: "document".into(),
            schema_fingerprint: metadata::current_schema_fingerprint(&f.store.db, SUBJECT)
                .unwrap()
                .unwrap(),
            expected_revision: 0,
            edits_json: json!({"title":{"op":"set","value":"other"}}).to_string(),
            evidence_json: "{}".into(),
        },
    };
    let command = fixture::envelope(
        &f.store,
        "motion.nfo",
        json!({"operation":"metadata","mutation":mutation}),
    );
    committed(
        commands::execute_candidate_dispatch_for_test(
            &f.store,
            &command,
            CatalogAuthority::new(fixture::authority(true))
                .with_metadata(&grant("motion.other", SourceKind::Provider)),
            fixture::policy(),
        )
        .unwrap(),
    );
    let mutation = MetadataMutation {
        change: MetadataChange::Patch {
            subject: SUBJECT.into(),
            namespace: "motion.nfo".into(),
            record_id: "document".into(),
            schema_fingerprint: metadata::current_schema_fingerprint(&f.store.db, SUBJECT)
                .unwrap()
                .unwrap(),
            expected_revision: 2,
            edits_json: "{}".into(),
            evidence_json: "{}".into(),
        },
        contract_version: 1,
    };
    let command = fixture::envelope(
        &f.store,
        "ordinary-looking",
        json!({"operation":"metadata","mutation":mutation}),
    );
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(
        commands::execute_candidate_dispatch_for_test(
            &f.store,
            &command,
            CatalogAuthority::new(fixture::authority(true))
                .with_metadata(&grant("motion.nfo", SourceKind::Provider)),
            fixture::policy()
        )
        .is_err()
    );
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
}
#[test]
fn generic_metadata_permit_never_authorizes_nfo_assertion_or_source_effects() {
    let f = fixture::Fixture::ready();
    for sql in [
        "UPDATE item_tags SET active=0 WHERE source='motion.nfo'",
        "DELETE FROM item_tags WHERE source='motion.nfo'",
        "UPDATE catalog_metadata_sources SET fields_json='{}' WHERE namespace='motion.nfo'",
        "DELETE FROM catalog_metadata_sources WHERE namespace='motion.nfo'",
    ] {
        let before = migration::snapshot(&f.store.db).unwrap();
        f.store
            .transaction_detailed::<_, Error>(|db| {
                db.execute(
                    "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES (?,?)",
                    rusqlite::params![SUBJECT, f.store.catalog_incarnation],
                )?;
                let error = db.execute(sql, []).unwrap_err();
                assert!(error.to_string().contains("catalog_nfo_"), "{error}");
                Err::<(), _>(Error("intentional adversarial rollback".into()))
            })
            .unwrap_err();
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    }
}
fn merge(store: &Store, source: &str, target: &str) -> (CommandEnvelope, TagAuthority) {
    let source = crate::tagging::resolve_tag(&store.db, &json!(source)).unwrap();
    let target = crate::tagging::resolve_tag(&store.db, &json!(target)).unwrap();
    let grant = tag_grant();
    let preview = tags::preview_merge(store, &grant, HOST, &source, &target).unwrap();
    let mutation = TagMutation::Merge {
        contract_version: 1,
        merge: TagMerge {
            source_tag_id: source,
            target_tag_id: target,
            expected_source_revision: preview.body.affected.source_revision,
            expected_target_revision: preview.body.affected.target_revision,
            preview_digest: preview.digest,
            collisions: vec![],
        },
    };
    (
        fixture::envelope(
            store,
            "merge",
            json!({"operation":"tags","mutation":mutation}),
        ),
        grant,
    )
}
#[test]
fn merge_inactive_nfo_assertion_refuses_but_unrelated_merge_and_manual_suppression_work() {
    let f = fixture::Fixture::ready();
    put(&f.store, "remove-tags", 1, &[]);
    crate::tagging::put(&f.store, &json!("target"), None).unwrap();
    let (command, grant) = merge(&f.store, "old", "target");
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(
        commands::execute_candidate_dispatch_for_test(
            &f.store,
            &command,
            CatalogAuthority::new(fixture::authority(true)).with_tags(&grant),
            fixture::policy()
        )
        .is_err()
    );
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    crate::tagging::put(&f.store, &json!("independent"), None).unwrap();
    crate::tagging::assign(
        &f.store,
        crate::tagging::Assignment {
            names: &[json!("independent")],
            items: &[SUBJECT.into()],
            files: &[],
            source: &json!("manual"),
            confidence: &Value::Null,
            note: &json!(""),
            remove: false,
        },
    )
    .unwrap();
    let (command, grant) = merge(&f.store, "independent", "old");
    committed(
        commands::execute_candidate_dispatch_for_test(
            &f.store,
            &command,
            CatalogAuthority::new(fixture::authority(true)).with_tags(&grant),
            fixture::policy(),
        )
        .unwrap(),
    );
    let mutation = TagMutation::SetSuppression {
        contract_version: 1,
        changes: vec![SuppressionChange {
            subject: TagSubject::Item(SUBJECT.into()),
            canonical_tag_id: crate::tagging::resolve_tag(&f.store.db, &json!("old")).unwrap(),
            expected_revision: None,
            suppressed: true,
            reason: "independent same-host editorial decision".into(),
        }],
    };
    let command = fixture::envelope(
        &f.store,
        "suppress",
        json!({"operation":"tags","mutation":mutation}),
    );
    committed(
        commands::execute_candidate_dispatch_for_test(
            &f.store,
            &command,
            CatalogAuthority::new(fixture::authority(true)).with_tags(&tag_grant()),
            fixture::policy(),
        )
        .unwrap(),
    );
    put(&f.store, "restore-tag", 2, &["old"]);
    assert!(f.store.db.query_row("SELECT suppressed FROM catalog_tag_suppression_revisions WHERE subject_id=? AND tag_id=(SELECT tag_id FROM tag_names WHERE name='old') ORDER BY revision DESC LIMIT 1",[SUBJECT],|r|r.get::<_,bool>(0)).unwrap());
}
#[test]
fn non_nfo_schema_evolution_preserves_historical_nfo_source_and_current_read() {
    let f = fixture::Fixture::ready();
    let history = migration::rows(
        &f.store.db,
        "SELECT * FROM catalog_metadata_history WHERE namespace='motion.nfo'",
        &[],
    )
    .unwrap();
    let old = metadata::current_schema_fingerprint(&f.store.db, SUBJECT)
        .unwrap()
        .unwrap();
    let mut request = fixture::put_request(&f.store, 0, json!({"additional":17}), &[]);
    request.namespace = "motion.other".into();
    let command = fixture::envelope(
        &f.store,
        "non-nfo-extension",
        json!({"operation":"metadata_contribution_replace","request":request}),
    );
    committed(
        commands::execute_candidate_dispatch_for_test(
            &f.store,
            &command,
            CatalogAuthority::new(fixture::authority(true))
                .with_metadata(&grant("motion.other", SourceKind::Provider))
                .with_metadata_initialization(&init())
                .with_metadata_schema(&extension())
                .with_tags(&tag_grant()),
            fixture::policy(),
        )
        .unwrap(),
    );
    assert_ne!(
        metadata::current_schema_fingerprint(&f.store.db, SUBJECT)
            .unwrap()
            .unwrap(),
        old
    );
    assert_eq!(
        migration::rows(
            &f.store.db,
            "SELECT * FROM catalog_metadata_history WHERE namespace='motion.nfo'",
            &[]
        )
        .unwrap(),
        history
    );
    f.store
        .transaction_detailed::<_, Error>(|_| {
            let current = state::read_in(&f.store, SUBJECT, generation(&f.store), 7)?;
            assert_eq!(current.native_source().unwrap().schema_fingerprint(), old);
            assert_eq!(current.native_source().unwrap().revision(), 1);
            Ok(())
        })
        .unwrap();
}
#[test]
fn independent_source_barrier_survives_lost_token_even_if_generic_barrier_removed() {
    let f = fixture::Fixture::ready();
    let before = migration::snapshot(&f.store.db).unwrap();
    let request = fixture::put_request(&f.store, 1, json!({"title":"half"}), &["new"]);
    let command = fixture::envelope(
        &f.store,
        "half",
        json!({"operation":"nfo_public_put","contract_version":1,"expected_public_revision":1,"request":request}),
    );
    let attempted = std::cell::Cell::new(false);
    let result = f.store.transaction_detailed::<_, Error>(|db| {
        let current = state::read_in(&f.store, SUBJECT, command.expected_generation(), 7)?;
        let permit = permit::prepare_put_in(&f.store, current, &command, &request, 1)?;
        metadata::replace_nfo_contribution(
            &f.store,
            &request,
            &command,
            &grant("motion.nfo", SourceKind::Provider),
            &init(),
            &extension(),
            &tag_grant(),
            1,
            &permit,
        )
        .map_err(|e| Error(e.to_string()))?;
        drop(permit);
        db.execute(
            "DELETE FROM catalog_metadata_permits WHERE item_id=?",
            [SUBJECT],
        )?;
        assert!(
            db.prepare("PRAGMA foreign_key_check(catalog_nfo_source_effect_permits)")?
                .exists([])?
        );
        attempted.set(true);
        Ok(())
    });
    assert!(attempted.get());
    assert!(result.is_err());
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
}

struct ImportFixture {
    _root: tempfile::TempDir,
    store: Store,
}
impl ImportFixture {
    fn empty() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fresh-import.db");
        migration::initialize_native_at(&path, 40).unwrap();
        candidate::install_candidate41_for_test(&path).unwrap();
        let store = candidate::open_candidate41_for_test(&path, true).unwrap();
        Self { _root: root, store }
    }
}
fn import_batch(version: u32) -> ImportBatch {
    let document =
        json!({"values":{"title":"Imported NFO"},"tags":["imported"],"excluded_tags":[]});
    ImportBatch {
        items: vec![ImportItem {
            source_id: "original-logical".into(),
            item_id: SUBJECT.into(),
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
                item_id: SUBJECT.into(),
                policies: json!([{"field":"title","value_type":"string","nullable":false,"normalization":"exact"},{"field":"motion_catalog","value_type":"json","nullable":false,"normalization":"exact"}]),
                contributions: vec![
                    ImportedContribution {
                        namespace: "motion.structure".into(),
                        record_id: "source_projection".into(),
                        source_revision: 3,
                        kind: ImportedSourceKind::Scan,
                        fields: json!({"motion_catalog":{"contract_version":1,"item_id":SUBJECT,"external_ids":[]}}),
                        evidence: json!({}),
                    },
                    ImportedContribution {
                        namespace: "motion.nfo".into(),
                        record_id: "document".into(),
                        source_revision: 7,
                        kind: ImportedSourceKind::Provider,
                        fields: document["values"].clone(),
                        evidence: json!({"source_table":"metadata_documents","source":"nfo","external_id":null,"document_json":document.to_string(),"document":document}),
                    },
                ],
            }],
            tags: vec![ImportedTagSet {
                item_id: SUBJECT.into(),
                namespace: "motion.nfo".into(),
                source_revision: 7,
                tags: vec!["imported".into()],
                excluded_tags: vec![],
            }],
            ..Default::default()
        }),
    }
}
fn typed_grants(batch: &ImportBatch) -> TypedAuthority {
    let mut grants = TypedAuthority::default();
    for m in &batch.typed.as_ref().unwrap().metadata {
        grants.metadata.push(MetadataAuthority {
            host_namespace: HOST.into(),
            subject: m.item_id.clone(),
            access: if m.contract_version == 2 {
                MetadataAccess::InitializeV2 {
                    system_fields: m.system_fields.iter().cloned().collect(),
                }
            } else {
                MetadataAccess::Initialize
            },
        });
        for c in &m.contributions {
            grants.metadata.push(MetadataAuthority {
                host_namespace: HOST.into(),
                subject: m.item_id.clone(),
                access: if m.contract_version == 2 {
                    MetadataAccess::SourceV2 {
                        namespace: c.namespace.clone(),
                        record_id: c.record_id.clone(),
                        kind: c.kind.owner(),
                        system_fields: m
                            .system_fields
                            .iter()
                            .filter(|name| c.fields.get(name.as_str()).is_some())
                            .cloned()
                            .collect(),
                    }
                } else {
                    MetadataAccess::Source {
                        namespace: c.namespace.clone(),
                        record_id: c.record_id.clone(),
                        kind: c.kind.owner(),
                    }
                },
            });
        }
    }
    for t in &batch.typed.as_ref().unwrap().tags {
        grants.tag_sources.push(TagImportAuthority {
            host_namespace: HOST.into(),
            namespace: t.namespace.clone(),
            item_ids: [t.item_id.clone()].into(),
            allow_suppressions: t.namespace == "motion.local",
        });
    }
    grants
}
struct Staging {
    manifest: SnapshotManifest,
    batches: Vec<ImportBatch>,
    input: String,
    grant: ImportAuthority,
    typed: TypedAuthority,
}
impl Staging {
    fn run(
        &self,
        store: &Store,
        step: ImportStep,
        mutation: ImportMutation,
        activation: Option<&TrustedImportActivation>,
    ) -> std::result::Result<(CommandEnvelope, Value), String> {
        let command = fixture::envelope(
            store,
            &import::operation_id(&self.manifest.import_id, step).unwrap(),
            json!({"operation":"import","mutation":mutation}),
        );
        let mut authority = CatalogAuthority::new(fixture::authority(true))
            .with_imports(&self.grant)
            .with_import_typed(&self.typed);
        if let Some(activation) = activation {
            authority = authority.with_import_activation(activation);
        }
        let value = commands::execute_candidate_dispatch_for_test(
            store,
            &command,
            authority,
            fixture::policy(),
        )
        .map_err(|e| e.to_string())?;
        Ok((command, committed(value)))
    }
    fn stage(store: &Store, id: &str, batch: ImportBatch, typed: TypedAuthority) -> Self {
        let manifest = SnapshotManifest {
            contract_version: 2,
            import_id: id.into(),
            source_namespace: "motion.schema68.export.v4".into(),
            source_snapshot_id: "frozen-archive".into(),
            batches: vec![import::describe_batch(&batch).unwrap()],
        };
        let input = import::manifest_digest(&manifest).unwrap();
        let grant = ImportAuthority {
            host_namespace: HOST.into(),
            import_id: id.into(),
            source_namespace: manifest.source_namespace.clone(),
        };
        let result = Self {
            manifest,
            batches: vec![batch],
            input,
            grant,
            typed,
        };
        result
            .run(
                store,
                ImportStep::Prepare,
                ImportMutation::Prepare {
                    manifest: result.manifest.clone(),
                    input_digest: result.input.clone(),
                },
                None,
            )
            .unwrap();
        result
            .run(
                store,
                ImportStep::Batch(0),
                ImportMutation::Stage {
                    import_id: id.into(),
                    input_digest: result.input.clone(),
                    batch_index: 0,
                    batch: result.batches[0].clone(),
                },
                None,
            )
            .unwrap();
        result
    }
    fn validate(&self, store: &Store) -> std::result::Result<Value, String> {
        self.run(
            store,
            ImportStep::Validate,
            ImportMutation::Validate {
                import_id: self.manifest.import_id.clone(),
                input_digest: self.input.clone(),
            },
            None,
        )
        .map(|(_, v)| v)
    }
    fn activate(&self, store: &Store) -> (CommandEnvelope, Value) {
        let validated = self.validate(store).unwrap();
        let mapping = validated["mapping_digest"].as_str().unwrap();
        let host = HostAuthorization::new(HOST.into(), "fresh-import-drain".into()).unwrap();
        let active = TrustedImportActivation::after_drain(
            &host,
            host.begin_revocation().unwrap(),
            ActivationBinding {
                catalog_id: store.database_id.clone(),
                incarnation: store.catalog_incarnation.clone().unwrap(),
                import_id: self.manifest.import_id.clone(),
                input_digest: self.input.clone(),
                source_snapshot_id: self.manifest.source_snapshot_id.clone(),
                validation_generation: generation(store),
                reference_ack_id: "archive-ack".into(),
                mapping_digest: mapping.into(),
            },
        )
        .unwrap();
        self.run(
            store,
            ImportStep::Activate,
            ImportMutation::Activate {
                import_id: self.manifest.import_id.clone(),
                input_digest: self.input.clone(),
                reference_ack_id: "archive-ack".into(),
                mapping_digest: mapping.into(),
            },
            Some(&active),
        )
        .unwrap()
    }
}
#[test]
fn actual_validated_prebootstrap_v1_v2_import_keeps_exact_wire_and_bootstraps() {
    for version in [1, 2] {
        let f = ImportFixture::empty();
        let batch = import_batch(version);
        let staged = Staging::stage(&f.store, "fresh-root", batch.clone(), typed_grants(&batch));
        let (activation, result) = staged.activate(&f.store);
        assert_eq!(
            f.store
                .db
                .query_row(
                    "SELECT count(*) FROM catalog_nfo_source_effect_permits",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert!(
            f.store
                .db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM catalog_receipt_holds WHERE operation_id=?)",
                    [activation.key().operation_id()],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap()
        );
        let audit = NfoImportAuthority {
            host_namespace: HOST.into(),
            root_manifest_digest: None,
            import_id: staged.manifest.import_id.clone(),
            input_digest: staged.input.clone(),
            source_namespace: staged.manifest.source_namespace.clone(),
            reference_ack_id: "archive-ack".into(),
            mapping_digest: result["mapping_digest"].as_str().unwrap().into(),
            origin_audit_digest: catabolic_core::sha256(
                json!({"fixture":"complete-archive","subjects":[SUBJECT],"sidecar_origins":[]})
                    .to_string(),
            ),
            no_origin_subjects: [SUBJECT.into()].into(),
        };
        let plan = f
            .store
            .transaction_detailed::<_, Error>(|_| {
                let baseline = verify_imported_no_origin_baseline_in(
                    &f.store,
                    SUBJECT,
                    &staged.manifest,
                    &staged.batches,
                    &activation,
                    &audit,
                    fixture::authority(false),
                    generation(&f.store),
                )?;
                let source = verify_source_history_in(
                    &f.store,
                    &baseline,
                    &[],
                    fixture::authority(false),
                    generation(&f.store),
                )?;
                bootstrap::prepare_in(&f.store, &[source])
            })
            .unwrap();
        let command = fixture::envelope(&f.store, "fresh-bootstrap", plan.payload());
        committed(
            commands::execute_candidate_for_test(
                &f.store,
                &command,
                fixture::authority(true),
                fixture::policy(),
                move |store, command, _| bootstrap::apply_in(store, command, plan),
            )
            .unwrap(),
        );
        let snapshot = migration::snapshot(&f.store.db).unwrap();
        assert!(matches!(
            commands::execute_candidate_dispatch_for_test(
                &f.store,
                &activation,
                CatalogAuthority::new(fixture::authority(false)),
                fixture::policy()
            )
            .unwrap(),
            CommandOutcome::Replay { .. }
        ));
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), snapshot);
        f.store
            .transaction_detailed::<_, Error>(|_| {
                let current = state::read_in(&f.store, SUBJECT, generation(&f.store), 7)?;
                assert_eq!(current.native_source().unwrap().revision(), 1);
                Ok(())
            })
            .unwrap();

        // A new, independently granted source is still not a bootstrap/claim
        // composition after the global bootstrap fence has been established.
        let mut later = import_batch(version);
        later.items[0].item_id = OTHER.into();
        later.items[0].source_id = "later-original-logical".into();
        let typed = later.typed.as_mut().unwrap();
        typed.metadata[0].item_id = OTHER.into();
        typed.metadata[0].contributions[0].fields["motion_catalog"]["item_id"] = json!(OTHER);
        typed.tags[0].item_id = OTHER.into();
        let grants = typed_grants(&later);
        let later = Staging::stage(&f.store, "post-bootstrap-root", later, grants);
        let before = migration::snapshot(&f.store.db).unwrap();
        assert!(
            later
                .validate(&f.store)
                .unwrap_err()
                .contains("pre-bootstrap")
        );
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    }
    // The generic typed owner has always normalized requested names. Keep that
    // input contract separate from strict archived requested-name proof; this
    // fixture does not claim that the noncanonical typed name is bootstrap proof.
    let f = ImportFixture::empty();
    let mut batch = import_batch(2);
    batch.typed.as_mut().unwrap().tags[0].tags = vec![" Imported ".into()];
    let grants = typed_grants(&batch);
    Staging::stage(&f.store, "normalized-import-root", batch, grants).activate(&f.store);
    assert_eq!(f.store.db.query_row(
        "SELECT t.name FROM item_tags a JOIN tags t ON t.id=a.tag_id WHERE a.item_id=? AND a.source='motion.nfo'",
        [SUBJECT], |r| r.get::<_, String>(0)).unwrap(), "imported");
}
#[test]
fn actual_tag_only_existing_item_import_refuses_nfo_but_allows_independent_source() {
    for namespace in ["motion.nfo", "motion.other"] {
        let f = fixture::Fixture::ready();
        let existing =
            typed::capture_existing_item(&f.store, HOST, SUBJECT, &[SUBJECT.into()].into())
                .unwrap();
        let batch = ImportBatch {
            items: vec![],
            unresolved_evidence: vec![],
            typed: Some(TypedBatch {
                tags: vec![ImportedTagSet {
                    item_id: SUBJECT.into(),
                    namespace: namespace.into(),
                    source_revision: 9,
                    tags: vec!["incoming".into()],
                    excluded_tags: vec![],
                }],
                ..Default::default()
            }),
        };
        let mut authority = typed_grants(&batch);
        authority.existing_items.insert(SUBJECT.into(), existing);
        let staged = Staging::stage(&f.store, "tag-only-root", batch, authority);
        let before = migration::snapshot(&f.store.db).unwrap();
        if namespace == "motion.nfo" {
            assert!(staged.validate(&f.store).unwrap_err().contains("tag-only"));
            assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
        } else {
            staged.activate(&f.store);
            assert_eq!(
                f.store
                    .db
                    .query_row(
                        "SELECT count(*) FROM item_tags WHERE item_id=? AND source='motion.other'",
                        [SUBJECT],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                1
            );
        }
    }
}

#[test]
fn legacy_item_put_cannot_rewrite_managed_nfo_projection_but_unchanged_and_unmanaged_work() {
    let f = fixture::Fixture::ready();
    let (kind, raw): (String, String) = f
        .store
        .db
        .query_row(
            "SELECT kind,metadata FROM items WHERE id=?",
            [SUBJECT],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let current: Value = serde_json::from_str(&raw).unwrap();
    let mut changed = current.clone();
    changed["motion_catalog"] = json!({"contract_version":1,"item_id":SUBJECT,
        "external_ids":[{"provider":"nfo","external_id":"unowned"}]});
    let before = migration::snapshot(&f.store.db).unwrap();
    let prior_generation = generation(&f.store);
    assert!(
        crate::curation::item_put(
            &f.store,
            &json!({
                "id":SUBJECT,"kind":kind,"metadata":changed,"identities":{}
            })
        )
        .unwrap_err()
        .to_string()
        .contains("typed metadata")
    );
    assert_eq!(generation(&f.store), prior_generation);
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    crate::curation::item_put(
        &f.store,
        &json!({
            "id":SUBJECT,"kind":kind,"metadata":current,"identities":{}
        }),
    )
    .unwrap();
    assert_eq!(
        f.store
            .db
            .query_row("SELECT metadata FROM items WHERE id=?", [SUBJECT], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        raw
    );
    crate::curation::item_put(&f.store, &json!({
        "id":"unmanaged-independent","kind":"video","metadata":{"title":"Independent"},"identities":{}
    })).unwrap();
    assert_eq!(f.store.db.query_row("SELECT json_extract(metadata,'$.title') FROM items WHERE id='unmanaged-independent'",[],
        |r|r.get::<_,String>(0)).unwrap(),"Independent");
}

mod system_replace;
