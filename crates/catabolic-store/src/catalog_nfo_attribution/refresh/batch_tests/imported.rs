//! Actual contract1/2 imported-first refresh. The controlled host owns the complete
//! constructed one-part original archive and its independently complete absence
//! audit. Current media is established by the normal closed-scan/typed-identity
//! fixture, not by historical Activate output. No production Motion audit or
//! sidecar parser is qualified by this native owner test.
use super::*;
use crate::{
    catalog_commands::TrustedReceiptMaintenance,
    catalog_import::{self, ImportBatch, ImportItem, SnapshotManifest},
    catalog_import_typed::{
        ImportedContribution, ImportedMetadata, ImportedSourceKind, TypedBatch,
    },
    catalog_nfo_attribution::first_source::{
        self,
        imported::{
            ImportedFirstPart, ImportedFirstPartCommitment, ImportedFirstRootAudit,
            tests::{Part, activate_for_host},
        },
    },
};

fn original_work(store: &Store) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    (
        crate::migration::rows(
            &store.db,
            "SELECT * FROM items WHERE id=?",
            &[physical::WORK.to_owned().into()],
        )
        .unwrap(),
        crate::migration::rows(
            &store.db,
            "SELECT * FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?)",
            &[physical::WORK.to_owned().into()],
        )
        .unwrap(),
        crate::migration::rows(
            &store.db,
            "SELECT * FROM catalog_metadata_history WHERE item_id=? ORDER BY namespace,record_id,revision",
            &[physical::WORK.to_owned().into()],
        )
        .unwrap(),
    )
}

fn ready_imported(version: u32) -> (physical::Fixture, Part) {
    let mut part = None;
    let mut original = None;
    let fixture = physical::Fixture::new_imported(|store| {
        let batch = ImportBatch {
            items: vec![ImportItem {
                source_id: "original-logical-work".into(),
                item_id: physical::WORK.into(),
                kind: "video".into(),
                legacy_metadata: json!({"title":" Original Imported Title "}),
            }],
            unresolved_evidence: vec![],
            typed: Some(TypedBatch {
                metadata: vec![ImportedMetadata {
                    contract_version: version,
                    item_id: physical::WORK.into(),
                    system_fields: if version == 2 {
                        vec!["motion_catalog".into()]
                    } else {
                        vec![]
                    },
                    policies: json!([
                        {"field":"motion_catalog","value_type":"json","nullable":false,"normalization":"exact"},
                        {"field":"title","value_type":"string","nullable":false,"normalization":"trim_ascii_lowercase"}
                    ]),
                    contributions: vec![ImportedContribution {
                        namespace: "motion.structure".into(),
                        record_id: "source_projection".into(),
                        source_revision: 4,
                        kind: ImportedSourceKind::Scan,
                        fields: json!({"motion_catalog":{
                            "contract_version":1,"item_id":physical::WORK,
                            "media_type":"movie","external_ids":[],"unrelated":"retained"
                        }}),
                        evidence: json!({"source_tables":["items","item_structure"]}),
                    }],
                }],
                ..Default::default()
            }),
        };
        let manifest = SnapshotManifest {
            contract_version: 2,
            import_id: "refresh-original-import".into(),
            source_namespace: "motion.schema68.export.v4".into(),
            source_snapshot_id: "refresh-original-complete-archive".into(),
            batches: vec![catalog_import::describe_batch(&batch).unwrap()],
        };
        part = Some(activate_for_host(
            store,
            manifest,
            vec![batch],
            physical::HOST,
        ));
        original = Some(original_work(store));
    });
    let part = part.unwrap();
    // Relationships, selected associations and context/version setup may advance
    // global generation, but cannot mutate the untouched imported WORK proof.
    assert_eq!(original_work(&fixture.store), original.unwrap());
    assert!(fixture.creation.is_none());
    commands::compact_receipts(
        &fixture.store,
        TrustedReceiptMaintenance {
            host_namespace: physical::HOST,
            holder_namespace: "refresh-import-maintenance",
        },
        100,
    )
    .unwrap();
    assert!(matches!(
        commands::reconcile_receipt(
            &fixture.store,
            &part.activation,
            crate::catalog_commands::TrustedCommandAuthority {
                allow_new_commands: false,
                ..physical::authority()
            },
        )
        .unwrap(),
        Some(CommandOutcome::ReceiptExpired)
    ));
    (super::first::install_empty(fixture), part)
}

fn apply_imported(
    fixture: &physical::Fixture,
    part: &Part,
    outer: &CommandEnvelope,
    request: &RefreshRequest,
    custody: &dyn observation::RetainedRefreshCustody,
    time: i64,
) -> Result<Value> {
    apply_imported_configured(fixture, part, outer, request, custody, time, |_| {})
}
fn apply_imported_configured(
    fixture: &physical::Fixture,
    part: &Part,
    outer: &CommandEnvelope,
    request: &RefreshRequest,
    custody: &dyn observation::RetainedRefreshCustody,
    time: i64,
    configure: impl FnOnce(&mut RefreshAuthority<'_>),
) -> Result<Value> {
    let store = &fixture.store;
    assert!(!store.db.is_autocommit());
    let parts = [ImportedFirstPart {
        ordinal: 0,
        manifest: &part.manifest,
        batches: &part.batches,
        activation: &part.activation,
        reference_ack_id: "archive-ack",
        mapping_digest: &part.mapping,
    }];
    let audit = ImportedFirstRootAudit {
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone().unwrap(),
        generation: outer.expected_generation(),
        host_namespace: physical::HOST.into(),
        outer_digest: outer.digest().as_str().into(),
        root_import_id: part.manifest.import_id.clone(),
        root_manifest_digest: None,
        archived_audit_digest: catabolic_core::sha256(
            json!({
                "domain":"controlled complete imported original root audit",
                "subject":physical::WORK,"logical_source_id":"original-logical-work",
                "original_nfo_documents":[],"automatic_nfo_identities":[],
                "nfo_tags":[],"nfo_exclusions":[],"sidecar_origins":[],
                "input_digest":catalog_import::manifest_digest(&part.manifest).unwrap()
            })
            .to_string(),
        ),
        ordered_parts: vec![ImportedFirstPartCommitment {
            ordinal: 0,
            import_id: part.manifest.import_id.clone(),
            input_digest: catalog_import::manifest_digest(&part.manifest).unwrap(),
            activation_digest: part.activation.digest().as_str().into(),
            reference_ack_id: "archive-ack".into(),
            mapping_digest: part.mapping.clone(),
        }],
        subjects: [(physical::WORK.into(), "original-logical-work".into())].into(),
    };
    let current = state::read_in(store, physical::WORK, outer.expected_generation(), 7)?;
    let first = first_source::prepare_imported_in(
        store,
        outer,
        current,
        &parts,
        &audit,
        crate::catalog_commands::TrustedCommandAuthority {
            allow_new_commands: false,
            ..physical::authority()
        },
    )?;
    assert!(first.creation().is_err());
    assert!(first.origin().is_none());
    assert!(first.legacy_raw_json().is_none());
    apply_candidate_with_first(
        fixture,
        outer,
        request,
        custody,
        time,
        None,
        None,
        Some(&first),
        configure,
    )
}

#[test]
fn actual_imported_first_refresh_rolls_back_then_publishes_and_replays_after_reopen() {
    for version in [1, 2] {
        let (fixture, part) = ready_imported(version);
        let store = &fixture.store;
        let original = original_work(store);
        let original_fingerprint = metadata::current_schema_fingerprint(&store.db, physical::WORK)
            .unwrap()
            .unwrap();
        let original_schema = crate::migration::rows(
            &store.db,
            "SELECT * FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?",
            &[
                physical::WORK.to_owned().into(),
                original_fingerprint.clone().into(),
            ],
        )
        .unwrap();
        assert_eq!(original_schema[0]["contract_version"], version);
        let request = request(
            &fixture,
            Workflow::DirectRefresh,
            [
                parsed(" Fresh Imported NFO ", Some("imported-nfo-id")),
                unusable(),
            ],
        );
        let command = physical::envelope(
            store,
            "imported-first-refresh",
            json!({"operation":"nfo_refresh","request":request}),
        );
        let before = crate::migration::snapshot(&store.db).unwrap();
        let observed_write = Cell::new(false);
        let failing_custody = || {
            let written: bool = store.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM catalog_nfo_states WHERE item_id=?)",
                [physical::WORK],
                |r| r.get(0),
            )?;
            if written {
                observed_write.set(true);
                Err(Error(
                    "imported refresh terminal sidecar custody changed".into(),
                ))
            } else {
                Ok(())
            }
        };
        let failed = store.transaction_detailed::<(), Error>(|db| {
        let time = i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros()).unwrap();
        let error = apply_imported(&fixture, &part, &command, &request, &failing_custody, time).unwrap_err();
        assert!(error.0.contains("terminal sidecar custody changed"), "{error}");
        assert!(observed_write.get());
        let heads: (i64,i64,i64) = db.query_row(
            "SELECT s.revision,n.current_revision,(SELECT count(*) FROM catalog_metadata_permits WHERE item_id=s.item_id) FROM catalog_metadata_sources s JOIN catalog_nfo_states n ON n.item_id=s.item_id WHERE s.item_id=? AND s.namespace='motion.nfo' AND s.record_id='document'",
            [physical::WORK], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        )?;
        assert_eq!(heads, (1,1,1), "real source/state publication precedes terminal failure");
        let current_version: i64 = db.query_row("SELECT s.contract_version FROM catalog_metadata_schemas s JOIN catalog_metadata_schema_heads h ON h.item_id=s.item_id AND h.schema_fingerprint=s.schema_fingerprint WHERE s.item_id=?", [physical::WORK], |r|r.get(0))?;
        assert_eq!(current_version, 2, "v1 evolution occurs under the same retained barrier");
        assert_eq!(crate::migration::rows(db, "SELECT * FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?", &[physical::WORK.to_owned().into(),original_fingerprint.clone().into()])?, original_schema);
        assert!(db.execute_batch("COMMIT").is_err(), "swallowed failure must retain deferred batch barrier");
        Err(Error("expected rollback after refused COMMIT".into()))
    });
        assert!(failed.is_err());
        assert_eq!(crate::migration::snapshot(&store.db).unwrap(), before);
        assert_eq!(original_work(store), original);
        assert_eq!(
            metadata::current_schema_fingerprint(&store.db, physical::WORK)
                .unwrap()
                .as_deref(),
            Some(original_fingerprint.as_str())
        );

        let custody = || Ok(());
        let result = committed(
            commands::execute_candidate_for_test(
                store,
                &command,
                physical::authority(),
                physical::policy(),
                |_, outer, time| apply_imported(&fixture, &part, outer, &request, &custody, time),
            )
            .unwrap(),
        );
        let current_schema = crate::migration::rows(&store.db, "SELECT s.* FROM catalog_metadata_schemas s JOIN catalog_metadata_schema_heads h ON h.item_id=s.item_id AND h.schema_fingerprint=s.schema_fingerprint WHERE s.item_id=?", &[physical::WORK.to_owned().into()]).unwrap();
        assert_eq!(current_schema[0]["contract_version"], 2);
        assert_eq!(
            serde_json::from_str::<Value>(
                current_schema[0]["system_fields_json"].as_str().unwrap()
            )
            .unwrap(),
            json!(["motion_catalog"])
        );
        if version == 1 {
            assert_ne!(
                current_schema[0]["schema_fingerprint"],
                original_fingerprint
            );
        }
        assert_eq!(
            crate::migration::rows(
                &store.db,
                "SELECT * FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?",
                &[
                    physical::WORK.to_owned().into(),
                    original_fingerprint.clone().into()
                ]
            )
            .unwrap(),
            original_schema
        );
        assert_eq!(result["counts"]["processed"], 1);
        assert_eq!(result["counts"]["skipped"], 1);
        let state = &result["states"][physical::WORK];
        assert_eq!(state["head"]["source"]["revision"], 1);
        assert_eq!(state["head"]["attribution"]["revision"], 1);
        assert_eq!(
            state["head"]["attribution"]["active_identity"],
            "imported-nfo-id"
        );
        assert_eq!(
            state["head"]["attribution"]["origin"]["provenance"]["kind"],
            "observed"
        );
        assert_eq!(
            state["head"]["attribution"]["origin"]["file"]["file_id"],
            fixture.selection_part(1).file.file_id
        );
        assert_eq!(state["presence"]["incarnation"], 1);
        assert_eq!(state["presence"]["public_revision"], 1);
        let retained = crate::migration::rows(
        &store.db,
        "SELECT * FROM catalog_metadata_history WHERE item_id=? AND revision=1 AND namespace!='motion.nfo' ORDER BY namespace,record_id,revision",
        &[physical::WORK.to_owned().into()],
    ).unwrap();
        assert_eq!(
            retained, original.2,
            "exact imported revision-one history retained"
        );
        let marker: String = store
            .db
            .query_row(
                "SELECT metadata FROM items WHERE id=?",
                [physical::WORK],
                |r| r.get(0),
            )
            .unwrap();
        let marker: Value = serde_json::from_str(&marker).unwrap();
        // Imported legacy/local metadata intentionally outranks Provider NFO.
        // Publication must update its own normalized contribution, not erase the
        // independently retained effective-title owner.
        assert_eq!(marker["title"], " Original Imported Title ");
        let nfo_fields: String = store.db.query_row(
            "SELECT fields_json FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo' AND record_id='document'",
            [physical::WORK], |r| r.get(0),
        ).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&nfo_fields).unwrap()["title"],
            "fresh imported nfo"
        );
        assert_eq!(
            marker["motion_catalog"]["external_ids"],
            json!([{"provider":"nfo","value":"imported-nfo-id","namespace_version":""}])
        );
        assert_eq!(marker["motion_catalog"]["unrelated"], "retained");
        let tags = crate::migration::rows(&store.db, "SELECT t.name FROM item_tags a JOIN tags t ON t.id=a.tag_id WHERE a.item_id=? AND a.source='motion.nfo' AND a.active=1 ORDER BY t.name", &[physical::WORK.to_owned().into()]).unwrap();
        assert_eq!(tags, vec![json!({"name":"new"})]);
        let heads: (i64,i64,bool) = store.db.query_row(
        "SELECT (SELECT revision FROM catalog_metadata_sources WHERE item_id=?1 AND namespace=?2 AND record_id=?3),(SELECT revision FROM catalog_metadata_sources WHERE item_id=?1 AND namespace='motion.structure' AND record_id='source_projection'),NOT EXISTS(SELECT 1 FROM catalog_metadata_sources WHERE item_id=?1 AND namespace='motion.scan' AND record_id='origin')",
        params![physical::WORK,metadata::LEGACY_NAMESPACE,metadata::LEGACY_RECORD_ID],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).unwrap();
        assert_eq!(
            heads,
            (1, 2, true),
            "no created-title rewrite on imported ancestry"
        );
        store
            .transaction_detailed(|_| -> Result<()> {
                let current =
                    state::read_in(store, physical::WORK, physical::generation(store), 7)?;
                assert!(
                    current.origin_physical().is_none(),
                    "observed current origin is not an imported physical-origin grant"
                );
                assert_eq!(
                    current.native_source().unwrap().fields()["title"],
                    json!("fresh imported nfo")
                );
                Ok(())
            })
            .unwrap();

        let fixture = fixture.reopen_candidate();
        let before = crate::migration::snapshot(&fixture.store.db).unwrap();
        assert!(matches!(commands::execute_candidate_for_test(
        &fixture.store, &command, physical::authority(), physical::policy(),
        |_, _, _| panic!("receipt replay must not reconstruct imported absence or current sidecars"),
    ).unwrap(), CommandOutcome::Replay { result: replay, .. } if replay == result));
        assert_eq!(
            crate::migration::snapshot(&fixture.store.db).unwrap(),
            before
        );
        fixture
            .store
            .transaction_detailed(|_| -> Result<()> {
                let current = state::read_in(
                    &fixture.store,
                    physical::WORK,
                    physical::generation(&fixture.store),
                    7,
                )?;
                assert_eq!(current.head().source.as_ref().unwrap().revision, 1);
                assert_eq!(current.head().attribution.as_ref().unwrap().revision, 1);
                assert_eq!(
                    current
                        .head()
                        .attribution
                        .as_ref()
                        .unwrap()
                        .active_identity
                        .as_deref(),
                    Some("imported-nfo-id")
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn actual_imported_v1_first_refresh_requires_exact_independent_classification_grants() {
    let (fixture, part) = ready_imported(1);
    let store = &fixture.store;
    let request = request(
        &fixture,
        Workflow::DirectRefresh,
        [parsed("Classified", Some("imported-nfo-id")), unusable()],
    );
    let command = physical::envelope(
        store,
        "imported-classification-denied",
        json!({"operation":"nfo_refresh","request":request}),
    );
    let before = crate::migration::snapshot(&store.db).unwrap();
    for case in 0..8 {
        let reached_classification = Cell::new(false);
        let custody = || Ok(());
        let denied = commands::execute_candidate_for_test(
            store,
            &command,
            physical::authority(),
            physical::policy(),
            |_, outer, time| {
                apply_imported_configured(
                    &fixture,
                    &part,
                    outer,
                    &request,
                    &custody,
                    time,
                    |authority| {
                        reached_classification.set(true);
                        let subject = authority.subjects.get_mut(physical::WORK).unwrap();
                        match case {
                        0 | 1 => {
                            let MetadataAccess::InitializeV2 { system_fields } = &mut subject.initialization.access else { panic!("expected exact initialization grant") };
                            system_fields.insert(if case == 0 { "title" } else { "unknown_system" }.into());
                        }
                        2 => {
                            let MetadataAccess::InitializeV2 { system_fields } = &mut subject.initialization.access else { panic!("expected exact initialization grant") };
                            system_fields.clear();
                        }
                        3..=6 => {
                            let structure = subject.structural_sources.iter_mut().find(|grant| matches!(&grant.access,MetadataAccess::SourceV2 { namespace, .. } if namespace=="motion.structure")).unwrap();
                            match case {
                                3 => structure.host_namespace = "foreign-host".into(),
                                4 => structure.subject = "foreign-subject".into(),
                                5 | 6 => {
                                    let MetadataAccess::SourceV2 { system_fields, .. } = &mut structure.access else { unreachable!() };
                                    if case == 5 { system_fields.clear(); } else { system_fields.insert("title".into()); }
                                }
                                _ => unreachable!(),
                            }
                        }
                        7 => subject.structural_sources.retain(|grant| !matches!(&grant.access,MetadataAccess::SourceV2 { namespace, .. } if namespace=="motion.structure")),
                        _ => unreachable!(),
                    }
                    },
                )
            },
        );
        assert!(
            reached_classification.get(),
            "case {case} must independently prove imported absence before refusing classification"
        );
        assert!(
            denied.is_err(),
            "case {case} unexpectedly authorized classification"
        );
        assert_eq!(
            crate::migration::snapshot(&store.db).unwrap(),
            before,
            "case {case} must preserve all metadata/schema/history/state/receipt rows"
        );
    }
}

mod classification;
