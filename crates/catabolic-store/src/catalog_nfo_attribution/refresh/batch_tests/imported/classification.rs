//! Genuine import→PUT→refresh ancestry, never direct immutable-history edits.
use super::*;
use crate::catalog_nfo_attribution::public_put;

fn first_put(fixture: &physical::Fixture, part: &Part) {
    let store = &fixture.store;
    let request = metadata::ContributionReplace {
        contract_version: 2,
        subject: physical::WORK.into(),
        namespace: "motion.nfo".into(),
        record_id: "document".into(),
        expected_schema_fingerprint: metadata::current_schema_fingerprint(
            &store.db,
            physical::WORK,
        )
        .unwrap(),
        expected_revision: 0,
        fields_json: json!({"title":" First PUT "}).to_string(),
        evidence_json: json!({"owner":"controlled.public.put"}).to_string(),
        tags: vec!["put-tag".into()],
        excluded_tags: vec![],
        origin_creation_intent: None,
    };
    let outer = physical::envelope(
        store,
        "ancestry-first-put",
        json!({"operation":"nfo_public_put","contract_version":1,"expected_public_revision":0,"request":request}),
    );
    committed(commands::execute_candidate_for_test(store,&outer,physical::authority(),physical::policy(),|store,outer,time| {
        let parts = [ImportedFirstPart {ordinal:0,manifest:&part.manifest,batches:&part.batches,activation:&part.activation,reference_ack_id:"archive-ack",mapping_digest:&part.mapping}];
        let audit = ImportedFirstRootAudit {
            catalog_id:store.database_id.clone(),incarnation:store.catalog_incarnation.clone().unwrap(),generation:outer.expected_generation(),host_namespace:physical::HOST.into(),outer_digest:outer.digest().as_str().into(),root_import_id:part.manifest.import_id.clone(),root_manifest_digest:None,
            archived_audit_digest:catabolic_core::sha256(json!({"domain":"controlled complete imported original root audit","subject":physical::WORK,"logical_source_id":"original-logical-work","original_nfo_documents":[],"automatic_nfo_identities":[],"nfo_tags":[],"nfo_exclusions":[],"sidecar_origins":[],"input_digest":catalog_import::manifest_digest(&part.manifest).unwrap()}).to_string()),
            ordered_parts:vec![ImportedFirstPartCommitment {ordinal:0,import_id:part.manifest.import_id.clone(),input_digest:catalog_import::manifest_digest(&part.manifest).unwrap(),activation_digest:part.activation.digest().as_str().into(),reference_ack_id:"archive-ack".into(),mapping_digest:part.mapping.clone()}],
            subjects:[(physical::WORK.into(),"original-logical-work".into())].into(),
        };
        let current=state::read_in(store,physical::WORK,outer.expected_generation(),7)?;
        let first=first_source::prepare_imported_in(store,outer,current,&parts,&audit,crate::catalog_commands::TrustedCommandAuthority {allow_new_commands:false,..physical::authority()})?;
        let published=public_put::apply_first_put_in(store,outer,&request,0,first,&source(),&grant(MetadataAccess::InitializeV2{system_fields:BTreeSet::new()}),&grant(MetadataAccess::ExtendSchema),&tag_grant(),time)?;
        Ok(json!({"source_revision":published.source_revision()}))
    }).unwrap());
    let current = store
        .transaction_detailed::<_, Error>(|_| {
            state::read_in(store, physical::WORK, physical::generation(store), 7)
        })
        .unwrap();
    assert_eq!(current.schema().unwrap().contract_version(), 2);
    assert!(
        !current
            .schema()
            .unwrap()
            .system_fields()
            .contains("motion_catalog")
    );
    assert_eq!(current.native_source().unwrap().revision(), 1);

    // A genuinely retained v2 public source must remain readable after later
    // marker classification. This is not a synthetic registry/history row.
    let local = metadata::ContributionReplace {
        namespace: "motion.local".into(),
        fields_json: json!({"rating":17}).to_string(),
        tags: vec![],
        expected_schema_fingerprint: metadata::current_schema_fingerprint(
            &store.db,
            physical::WORK,
        )
        .unwrap(),
        ..request
    };
    let outer = physical::envelope(
        store,
        "ancestry-local-v2",
        json!({"operation":"metadata_contribution_replace","request":local}),
    );
    let local_source = grant(MetadataAccess::Source {
        namespace: "motion.local".into(),
        record_id: "document".into(),
        kind: SourceKind::Local,
    });
    let initialize = grant(MetadataAccess::InitializeV2 {
        system_fields: BTreeSet::new(),
    });
    let extend = grant(MetadataAccess::ExtendSchema);
    let tags = tag_grant();
    committed(
        commands::execute_candidate_dispatch_for_test(
            store,
            &outer,
            CatalogAuthority::new(physical::authority())
                .with_metadata(&local_source)
                .with_metadata_initialization(&initialize)
                .with_metadata_schema(&extend)
                .with_tags(&tags),
            physical::policy(),
        )
        .unwrap(),
    );
}
fn retained_history(store: &Store) -> Vec<Value> {
    crate::migration::rows(&store.db,"SELECT * FROM catalog_metadata_history WHERE item_id=? ORDER BY namespace,record_id,revision",&[physical::WORK.to_owned().into()]).unwrap()
}
fn assert_history(store: &Store, history: &[Value]) {
    for row in history {
        assert_eq!(crate::migration::rows(&store.db,"SELECT * FROM catalog_metadata_history WHERE item_id=? AND namespace=? AND record_id=? AND revision=?",&[physical::WORK.to_owned().into(),row["namespace"].as_str().unwrap().to_owned().into(),row["record_id"].as_str().unwrap().to_owned().into(),row["revision"].as_i64().unwrap().into()]).unwrap(),vec![row.clone()]);
    }
}
#[test]
fn actual_original_v1_put_then_refresh_classifies_ancestry_rolls_back_retries_and_reads_old_v2() {
    let (fixture, part) = ready_imported(1);
    let original = crate::migration::rows(
        &fixture.store.db,
        "SELECT * FROM catalog_metadata_schemas WHERE item_id=?",
        &[physical::WORK.to_owned().into()],
    )
    .unwrap();
    first_put(&fixture, &part);
    let store = &fixture.store;
    let history = retained_history(store);
    let prior = metadata::current_schema_fingerprint(&store.db, physical::WORK)
        .unwrap()
        .unwrap();
    let local = crate::migration::rows(
        &store.db,
        "SELECT * FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.local'",
        &[physical::WORK.to_owned().into()],
    )
    .unwrap();
    let request = request(
        &fixture,
        Workflow::DirectRefresh,
        [
            parsed(" Ancestral Refresh ", Some("ancestral-id")),
            unusable(),
        ],
    );
    let command = physical::envelope(
        store,
        "ancestry-refresh",
        json!({"operation":"nfo_refresh","request":request}),
    );
    let before = crate::migration::snapshot(&store.db).unwrap();
    let written = Cell::new(false);
    let custody = || {
        let promoted:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_schema_heads h JOIN catalog_metadata_schemas s ON s.item_id=h.item_id AND s.schema_fingerprint=h.schema_fingerprint,json_each(s.system_fields_json) f WHERE h.item_id=? AND f.value='motion_catalog')",[physical::WORK],|r|r.get(0))?;
        if promoted {
            written.set(true);
            Err(Error("terminal ancestry custody failure".into()))
        } else {
            Ok(())
        }
    };
    let failure=store.transaction_detailed::<(),Error>(|db| {
        let time=i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros()).unwrap();
        let error=apply_candidate(&fixture,&command,&request,&custody,time).unwrap_err();
        assert!(error.0.contains("terminal ancestry custody failure"),"{error}");
        assert!(written.get());
        assert_ne!(metadata::current_schema_fingerprint(db,physical::WORK).unwrap().as_deref(),Some(prior.as_str()));
        assert_eq!(db.query_row("SELECT revision FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo'",[physical::WORK],|r|r.get::<_,i64>(0))?,2);
        assert!(db.prepare("PRAGMA foreign_key_check(catalog_metadata_permits)")?.exists([])?);
        Ok(()) // Swallow the actual late failure: deferred FK COMMIT must fail.
    });
    assert!(failure.is_err());
    assert_eq!(crate::migration::snapshot(&store.db).unwrap(), before);
    let custody = || Ok(());
    committed(
        commands::execute_candidate_for_test(
            store,
            &command,
            physical::authority(),
            physical::policy(),
            |_, outer, time| apply_candidate(&fixture, outer, &request, &custody, time),
        )
        .unwrap(),
    );
    assert_history(store, &history);
    assert_eq!(
        crate::migration::rows(
            &store.db,
            "SELECT * FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.local'",
            &[physical::WORK.to_owned().into()]
        )
        .unwrap(),
        local
    );
    assert_eq!(
        crate::migration::rows(
            &store.db,
            "SELECT * FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=?",
            &[
                physical::WORK.to_owned().into(),
                original[0]["schema_fingerprint"]
                    .as_str()
                    .unwrap()
                    .to_owned()
                    .into()
            ]
        )
        .unwrap(),
        original
    );
    let snapshot = metadata::read_contributions(
        store,
        &metadata::MetadataReadAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store.catalog_incarnation.clone().unwrap(),
            subject: physical::WORK.into(),
            expected_generation: physical::generation(store),
        },
    )
    .unwrap();
    assert_eq!(snapshot.metadata["rating"], 17);
    // Schema classification is not editorial reconciliation of imported legacy
    // title. The provider updates independently beneath that retained value.
    assert_eq!(snapshot.metadata["title"], " Original Imported Title ");
    let provider: (String, String) = store.db.query_row(
        "SELECT source_kind,fields_json FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo'",
        [physical::WORK], |r| Ok((r.get(0)?,r.get(1)?)),
    ).unwrap();
    assert_eq!(provider.0, "provider");
    let provider_fields: Value = serde_json::from_str(&provider.1).unwrap();
    assert_eq!(provider_fields["title"], "ancestral refresh");
    let state = store
        .transaction_detailed::<_, Error>(|_| {
            state::read_in(store, physical::WORK, physical::generation(store), 7)
        })
        .unwrap();
    assert!(
        state
            .schema()
            .unwrap()
            .system_fields()
            .contains("motion_catalog")
    );
    assert_eq!(
        state
            .head()
            .attribution
            .as_ref()
            .unwrap()
            .active_identity
            .as_deref(),
        Some("ancestral-id")
    );
    let fixture = fixture.reopen_candidate();
    let before = crate::migration::snapshot(&fixture.store.db).unwrap();
    assert!(matches!(
        commands::execute_candidate_for_test(
            &fixture.store,
            &command,
            physical::authority(),
            physical::policy(),
            |_, _, _| panic!("replay cannot reconstruct original classification authority")
        )
        .unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(
        crate::migration::snapshot(&fixture.store.db).unwrap(),
        before
    );
}

#[test]
fn actual_original_v1_ancestry_does_not_substitute_for_current_exact_contributor_grants() {
    let (fixture, part) = ready_imported(1);
    first_put(&fixture, &part);
    let store = &fixture.store;
    let request = request(
        &fixture,
        Workflow::DirectRefresh,
        [parsed("Denied", Some("ancestral-id")), unusable()],
    );
    let command = physical::envelope(
        store,
        "ancestry-grants-denied",
        json!({"operation":"nfo_refresh","request":request}),
    );
    let before = crate::migration::snapshot(&store.db).unwrap();
    for case in 0..8 {
        let custody = || Ok(());
        let denied = commands::execute_candidate_for_test(
            store,
            &command,
            physical::authority(),
            physical::policy(),
            |_, outer, time| {
                apply_candidate_configured(
                    &fixture,
                    outer,
                    &request,
                    &custody,
                    time,
                    None,
                    None,
                    |authority| {
                        let subject = authority.subjects.get_mut(physical::WORK).unwrap();
                        match case {
                    0..=2 => {
                        let MetadataAccess::InitializeV2 {system_fields}=&mut subject.initialization.access else {unreachable!()};
                        match case {0=>system_fields.clear(),1=>{system_fields.insert("title".into());},_=>{system_fields.insert("unknown_system".into());}}
                    },
                    3..=6 => {
                        let source=subject.structural_sources.iter_mut().find(|g|matches!(&g.access,MetadataAccess::SourceV2{namespace,..} if namespace=="motion.structure")).unwrap();
                        match case {3=>source.subject="foreign".into(),4=>source.host_namespace="foreign".into(),_=>{let MetadataAccess::SourceV2 {system_fields,..}=&mut source.access else {unreachable!()};if case==5 {system_fields.clear();}else{system_fields.insert("title".into());}}}
                    },
                    _=>subject.structural_sources.retain(|g|!matches!(&g.access,MetadataAccess::SourceV2{namespace,..} if namespace=="motion.structure")),
                }
                    },
                )
            },
        );
        assert!(denied.is_err(), "case {case}");
        assert_eq!(
            crate::migration::snapshot(&store.db).unwrap(),
            before,
            "case {case}"
        );
    }
}
