//! Native-created first refresh only. The controlled host owns the complete
//! constructed archive and supplies positive absence/original-file audits.
//! Actual create, closed scan, metadata, bootstrap, refresh and receipt owners
//! are exercised; this does not qualify a production Motion audit builder.
use super::*;

pub(super) fn install_empty(fixture: physical::Fixture) -> physical::Fixture {
    let fixture = fixture.install_candidate();
    let store = &fixture.store;
    let plan = store
        .transaction_detailed(|_| bootstrap::prepare_in(store, &[]))
        .unwrap();
    let command = physical::envelope(store, "empty-first-refresh-bootstrap", plan.payload());
    committed(
        commands::execute_candidate_for_test(
            store,
            &command,
            physical::authority(),
            physical::policy(),
            move |store, outer, _| bootstrap::apply_in(store, outer, plan),
        )
        .unwrap(),
    );
    fixture
}
fn ready_first() -> physical::Fixture {
    install_empty(physical::Fixture::new_first())
}
fn source_rows(store: &Store) -> Vec<Value> {
    crate::migration::rows(
        &store.db,
        "SELECT * FROM catalog_metadata_sources ORDER BY item_id,namespace,record_id",
        &[],
    )
    .unwrap()
}
fn assert_uninitialized(store: &Store) {
    let empty:bool=store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_metadata_sources WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_metadata_history WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_nfo_states WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_nfo_state_revisions WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM item_tags WHERE item_id=?1 AND source='motion.nfo') AND NOT EXISTS(SELECT 1 FROM catalog_metadata_permits)",[physical::WORK],|r|r.get(0)).unwrap();
    assert!(empty);
}

#[test]
fn actual_first_refresh_retains_ordered_root_origin_and_structure_then_replays_after_restart() {
    let fixture = ready_first();
    let request = request(
        &fixture,
        Workflow::DirectRefresh,
        [
            parsed(" Fresh Title ", Some("fresh-id")),
            parsed(" Fresh Title ", Some("fresh-id")),
        ],
    );
    let (command, result) = execute(&fixture, "first-refresh-ordered", &request);
    assert_eq!(result["counts"]["processed"], 2);
    let state = &result["states"][physical::WORK];
    assert_eq!(
        state["head"]["source"]["revision"], 1,
        "equal second publication changes only origin attribution"
    );
    assert_eq!(state["head"]["attribution"]["revision"], 2);
    assert_eq!(state["presence"]["public_revision"], 1);
    assert_eq!(state["presence"]["incarnation"], 1);
    assert_eq!(
        state["head"]["attribution"]["origin"]["file"]["file_id"],
        fixture.selection_part(2).file.file_id
    );
    assert_eq!(
        state["head"]["attribution"]["origin"]["provenance"]["kind"],
        "observed"
    );
    let legacy=crate::migration::rows(&fixture.store.db,"SELECT revision,fields_json,raw_mutation_json,command_operation_id FROM catalog_metadata_history WHERE item_id=? AND namespace=? ORDER BY revision",&[physical::WORK.to_owned().into(),metadata::LEGACY_NAMESPACE.to_owned().into()]).unwrap();
    assert_eq!(legacy.len(), 3);
    assert_eq!(
        legacy
            .iter()
            .map(|r| r["revision"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    let original: Value = serde_json::from_str(legacy[0]["fields_json"].as_str().unwrap()).unwrap();
    let reconciled: Value =
        serde_json::from_str(legacy[1]["fields_json"].as_str().unwrap()).unwrap();
    let structural: Value =
        serde_json::from_str(legacy[2]["fields_json"].as_str().unwrap()).unwrap();
    assert_eq!(original["title"], "Original");
    assert!(original.get("motion_catalog").is_some());
    assert!(reconciled.get("title").is_none());
    assert!(reconciled.get("motion_catalog").is_some());
    assert_eq!(structural, json!({}));
    assert!(
        legacy
            .iter()
            .all(|r| r["command_operation_id"] == command.key().operation_id())
    );
    assert_eq!(
        legacy[1]["raw_mutation_json"],
        legacy[2]["raw_mutation_json"]
    );
    let initial: Value =
        serde_json::from_str(legacy[0]["raw_mutation_json"].as_str().unwrap()).unwrap();
    assert_eq!(initial["contract_version"], 2);
    assert_eq!(initial["system_fields"], json!(["motion_catalog"]));
    let rows = source_rows(&fixture.store);
    let scan = rows
        .iter()
        .find(|r| r["namespace"] == "motion.scan" && r["record_id"] == "origin")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(scan["fields_json"].as_str().unwrap()).unwrap(),
        json!({"title":"Original"})
    );
    let evidence: Value = serde_json::from_str(scan["evidence_json"].as_str().unwrap()).unwrap();
    assert_eq!(evidence["owner"], "motion.receipt-origin.v1");
    assert_eq!(
        evidence["creation_intent_digest"],
        catabolic_core::sha256(fixture.creation.as_ref().unwrap().canonical_payload())
    );
    let metadata: String = fixture
        .store
        .db
        .query_row(
            "SELECT metadata FROM items WHERE id=?",
            [physical::WORK],
            |r| r.get(0),
        )
        .unwrap();
    let metadata: Value = serde_json::from_str(&metadata).unwrap();
    // First root deliberately follows native40 Json/Exact, not typed trim/lowercase.
    assert_eq!(metadata["title"], " Fresh Title ");
    assert_eq!(
        metadata["motion_catalog"]["external_ids"][0]["value"],
        "fresh-id"
    );
    let fixture = fixture.reopen_candidate();
    let before = crate::migration::snapshot(&fixture.store.db).unwrap();
    assert!(matches!(
        commands::execute_candidate_for_test(
            &fixture.store,
            &command,
            physical::authority(),
            physical::policy(),
            |_, _, _| panic!("receipt replay must not revalidate first absence or sidecars")
        )
        .unwrap(),
        CommandOutcome::Replay { .. }
    ));
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
            assert_eq!(current.head().attribution.as_ref().unwrap().revision, 2);
            assert!(current.origin_physical().is_none());
            Ok(())
        })
        .unwrap();
}

#[test]
fn actual_managed_first_refresh_uses_existing_typed_normalization_without_origin_rewrite() {
    let fixture = physical::Fixture::new_first();
    let init = MetadataInitializeV2 {
        contract_version: 2,
        subject: physical::WORK.into(),
        policies: vec![
            PolicyField {
                field: "motion_catalog".into(),
                value_type: PolicyValueType::Json,
                nullable: false,
                normalization: PolicyNormalization::Exact,
            },
            PolicyField {
                field: "title".into(),
                value_type: PolicyValueType::String,
                nullable: false,
                normalization: PolicyNormalization::TrimAsciiLowercase,
            },
        ],
        system_fields: ["motion_catalog".into()].into(),
    };
    let outer = physical::envelope(
        &fixture.store,
        "managed-first-root",
        json!({"operation":"metadata_initialize_v2","request":init}),
    );
    let init_grant = initialization();
    committed(
        commands::execute_with_authority(
            &fixture.store,
            &outer,
            CatalogAuthority::new(physical::authority())
                .with_metadata(&init_grant)
                .with_metadata_initialization(&init_grant),
            physical::policy(),
        )
        .unwrap(),
    );
    let fixture = install_empty(fixture);
    let retained = crate::migration::rows(
        &fixture.store.db,
        "SELECT * FROM catalog_metadata_history ORDER BY item_id,namespace,record_id,revision",
        &[],
    )
    .unwrap();
    let request = request(
        &fixture,
        Workflow::DirectRefresh,
        [parsed(" Typed Title ", None), unusable()],
    );
    let (_, result) = execute(&fixture, "managed-first-refresh", &request);
    assert_eq!(
        result["states"][physical::WORK]["head"]["source"]["document"]["contribution"]["fields"]["title"],
        "typed title"
    );
    let legacy=crate::migration::rows(&fixture.store.db,"SELECT * FROM catalog_metadata_history WHERE namespace=? ORDER BY item_id,namespace,record_id,revision",&[metadata::LEGACY_NAMESPACE.to_owned().into()]).unwrap();
    assert_eq!(
        legacy, retained,
        "managed legacy history is not a fresh-create origin grant"
    );
    assert!(
        !source_rows(&fixture.store)
            .iter()
            .any(|r| r["namespace"] == "motion.scan")
    );
}

#[test]
fn skipped_and_target_pinned_first_refresh_do_not_initialize_metadata() {
    let fixture = ready_first();
    let skipped = request(&fixture, Workflow::DirectRefresh, [unusable(), unusable()]);
    let (_, result) = execute(&fixture, "first-all-skipped", &skipped);
    assert_eq!(result["counts"]["processed"], 0);
    assert_uninitialized(&fixture.store);
    decide_pin(&fixture, "manual-id");
    let pinned = request(
        &fixture,
        Workflow::DirectRefresh,
        [parsed("ignored", Some("different-id")), unusable()],
    );
    let (_, result) = execute(&fixture, "first-target-pinned", &pinned);
    assert_eq!(result["counts"]["processed"], 1);
    assert_uninitialized(&fixture.store);
    let pin: String = fixture
        .store
        .db
        .query_row(
            "SELECT value FROM catalog_identity_pins WHERE item_id=? AND namespace='nfo'",
            [physical::WORK],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pin, "manual-id");
}

#[test]
fn swallowed_failure_after_real_first_initialization_cannot_commit_and_retry_is_receipt_safe() {
    let fixture = ready_first();
    let store = &fixture.store;
    let request = request(
        &fixture,
        Workflow::DirectRefresh,
        [parsed("First", Some("first-id")), unusable()],
    );
    let command = physical::envelope(
        store,
        "first-post-initialization-failure",
        json!({"operation":"nfo_refresh","request":request}),
    );
    let before = crate::migration::snapshot(&store.db).unwrap();
    let observed = Cell::new(false);
    let mut probe = |store: &Store| -> Result<()> {
        let initialized:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=?1) AND EXISTS(SELECT 1 FROM catalog_metadata_history WHERE item_id=?1 AND namespace='catabolic.legacy_import.v1' AND revision=1) AND NOT EXISTS(SELECT 1 FROM catalog_metadata_history WHERE item_id=?1 AND namespace='catabolic.legacy_import.v1' AND revision>1) AND NOT EXISTS(SELECT 1 FROM catalog_metadata_sources WHERE item_id=?1 AND namespace='motion.nfo') AND EXISTS(SELECT 1 FROM catalog_metadata_permits WHERE item_id=?1 AND incarnation=?2 AND barrier_id=1)",params![physical::WORK,command.admission_incarnation()],|r|r.get(0))?;
        assert!(
            initialized,
            "probe must follow actual initialization, before origin/provider effects"
        );
        observed.set(true);
        Err(Error(
            "controlled failure after first root persistence".into(),
        ))
    };
    let custody = || Ok(());
    let result = store.transaction_detailed::<(), Error>(|db| {
        let error = apply_candidate_configured(
            &fixture,
            &command,
            &request,
            &custody,
            123,
            None,
            Some(&mut probe),
            |_| {},
        )
        .unwrap_err();
        assert!(
            error
                .0
                .contains("controlled failure after first root persistence"),
            "{error}"
        );
        assert!(observed.get());
        assert!(
            db.execute_batch("COMMIT").is_err(),
            "swallowed post-init failure must retain deferred commit barrier"
        );
        Err(Error("expected rollback after refused COMMIT".into()))
    });
    assert!(result.is_err());
    assert_eq!(crate::migration::snapshot(&store.db).unwrap(), before);
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
    assert!(matches!(
        commands::execute_candidate_for_test(
            store,
            &command,
            physical::authority(),
            physical::policy(),
            |_, _, _| panic!("successful retry receipt must replay")
        )
        .unwrap(),
        CommandOutcome::Replay { .. }
    ));
}

#[test]
fn first_refresh_missing_or_wrong_independent_grants_refuse_without_effects() {
    let fixture = ready_first();
    let request = request(
        &fixture,
        Workflow::DirectRefresh,
        [parsed("First", Some("first-id")), unusable()],
    );
    let before = crate::migration::snapshot(&fixture.store.db).unwrap();
    for case in 0..5 {
        let outer = physical::envelope(
            &fixture.store,
            &format!("first-wrong-grant-{case}"),
            json!({"operation":"nfo_refresh","request":request}),
        );
        let custody = || Ok(());
        let result = commands::execute_candidate_for_test(
            &fixture.store,
            &outer,
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
                        if case == 0 {
                            authority.first_sources.clear();
                            return;
                        }
                        let grants = authority.subjects.get_mut(physical::WORK).unwrap();
                        match case {
                    1=>grants.source.access=MetadataAccess::Source {namespace:"motion.local".into(),record_id:"document".into(),kind:SourceKind::Provider},
                    2=>grants.initialization.access=MetadataAccess::InitializeV2 {system_fields:BTreeSet::new()},
                    3=>grants.structural_legacy=None,
                    _=>grants.structural_sources.retain(|grant|!matches!(&grant.access,MetadataAccess::SourceV2 {namespace,..} if namespace=="motion.structure")),
                }
                    },
                )
            },
        );
        assert!(
            result.is_err(),
            "independent grant case {case} unexpectedly applied"
        );
        assert_eq!(
            crate::migration::snapshot(&fixture.store.db).unwrap(),
            before,
            "grant case {case} changed retained state"
        );
    }
}

#[test]
fn actual_reparent_withdrawal_and_first_publication_are_atomic_at_terminal_custody() {
    const TARGET: &str = "new-target";
    let fixture = physical::Fixture::new_first();
    let creation = physical::envelope(
        &fixture.store,
        "create-refresh-reparent-target",
        json!({"operation":"item_create_batch","items":[{"id":TARGET,"kind":"video","metadata":{
            "title":"New original","motion_catalog":{"contract_version":1,"item_id":TARGET,"kind":"video","external_ids":[]}
        }}]}),
    );
    committed(
        commands::execute(
            &fixture.store,
            &creation,
            physical::authority(),
            physical::policy(),
        )
        .unwrap(),
    );
    let mut fixture = install_empty(fixture);
    let original = request(
        &fixture,
        Workflow::DirectRefresh,
        [parsed("Prior owner", Some("shared-id")), unusable()],
    );
    execute(&fixture, "refresh-before-actual-reparent", &original);
    let old_edge: String = fixture.store.db.query_row(
        "SELECT id FROM item_relationships WHERE source_id=? AND target_id='edition' AND kind='custom:motion_edition' AND active=1",
        [physical::WORK], |r| r.get(0),
    ).unwrap();
    // Reparent through the real generic relationship owner. No attribution,
    // assertion, native source, or permanent revision is inserted by the test.
    crate::curation::dispatch(
        &fixture.store,
        "default",
        "relationship-disable",
        &json!({"id":old_edge}),
    )
    .unwrap();
    crate::curation::relate(
        &fixture.store,
        &json!({"source":TARGET,"target":"edition","kind":"custom:motion_edition"}),
    )
    .unwrap();
    fixture.creation = Some(creation);
    let target_item_revision:i64=fixture.store.db.query_row(
        "SELECT revision FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?) AND deleted=0",
        [TARGET], |r|r.get(0),
    ).unwrap();
    assert_eq!(
        target_item_revision, 1,
        "new relationships must not fabricate a fresh item head"
    );
    let request = request_for(
        &fixture,
        TARGET,
        [(physical::WORK.into(), 7), (TARGET.into(), 7)].into(),
        Workflow::DirectRefresh,
        [parsed("New owner", Some("shared-id")), unusable()],
    );
    let command = physical::envelope(
        &fixture.store,
        "refresh-actual-reparent-first",
        json!({"operation":"nfo_refresh","request":request}),
    );
    // The controlled host owns both constructed work memberships, not just
    // the request's target. It independently grants curation of the old owner.
    let with_prior_owner = |authority: &mut RefreshAuthority<'_>| {
        authority.subjects.insert(physical::WORK.into(),SubjectAuthority {
            aggregate_revision:7,
            curation_audit_digest:catabolic_core::sha256(json!({"domain":"complete controlled reparent memberships","prior":physical::WORK,"target":TARGET,"all_libraries":[]}).to_string()),
            source:source(),initialization:initialization(),extension:grant(MetadataAccess::ExtendSchema),tags:tag_grant(),
            structural_sources:vec![structural(),local_structure(),grant(MetadataAccess::SourceV2{
                namespace:"motion.structure".into(),record_id:"source_projection".into(),kind:SourceKind::Scan,
                system_fields:["motion_catalog".into()].into(),
            })],
            structural_legacy:Some(grant(MetadataAccess::ReconcileLegacy)),
        });
    };
    let store = &fixture.store;
    let before = crate::migration::snapshot(&store.db).unwrap();
    let reached_terminal = Cell::new(false);
    let custody = || -> Result<()> {
        let target_exists:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo' AND record_id='document')",[TARGET],|r|r.get(0))?;
        if !target_exists {
            return Ok(());
        }
        let old_withdrawn:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_nfo_states s JOIN catalog_nfo_state_revisions h ON h.item_id=s.item_id AND h.revision=s.current_revision WHERE s.item_id=? AND h.source_revision=2 AND h.public_revision IS NULL AND h.origin_json IS NULL AND h.active_identity IS NULL)",[physical::WORK],|r|r.get(0))?;
        let target_published:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_nfo_states s JOIN catalog_nfo_state_revisions h ON h.item_id=s.item_id AND h.revision=s.current_revision WHERE s.item_id=? AND h.source_revision=1 AND h.public_revision=1 AND h.active_identity='shared-id')",[TARGET],|r|r.get(0))?;
        let barriers:i64=store.db.query_row("SELECT count(*) FROM catalog_metadata_permits WHERE item_id IN (?,?) AND incarnation=? AND barrier_id=1",params![physical::WORK,TARGET,command.admission_incarnation()],|r|r.get(0))?;
        assert!(
            old_withdrawn && target_published,
            "terminal check must follow both real subject effects"
        );
        assert_eq!(barriers, 2);
        reached_terminal.set(true);
        Err(Error(
            "controlled terminal custody loss after reparent effects".into(),
        ))
    };
    let result = store.transaction_detailed::<(), Error>(|db| {
        let error = apply_candidate_configured(
            &fixture,
            &command,
            &request,
            &custody,
            123,
            None,
            None,
            &with_prior_owner,
        )
        .unwrap_err();
        assert!(
            error.0.contains("controlled terminal custody loss"),
            "{error}"
        );
        assert!(reached_terminal.get());
        assert!(
            db.execute_batch("COMMIT").is_err(),
            "both pending subject barriers must reject a swallowed terminal error"
        );
        Err(Error("expected rollback of both subjects".into()))
    });
    assert!(result.is_err());
    assert_eq!(crate::migration::snapshot(&store.db).unwrap(), before);
    let custody = || Ok(());
    let result = committed(
        commands::execute_candidate_for_test(
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
                    &with_prior_owner,
                )
            },
        )
        .unwrap(),
    );
    assert_eq!(
        result["states"][physical::WORK]["presence"]["public_revision"],
        Value::Null
    );
    assert_eq!(
        result["states"][physical::WORK]["head"]["source"]["revision"],
        2
    );
    assert_eq!(result["states"][TARGET]["presence"]["public_revision"], 1);
    assert_eq!(result["states"][TARGET]["head"]["source"]["revision"], 1);
    let claim: String = store
        .db
        .query_row(
            "SELECT item_id FROM catalog_nfo_identity_claims WHERE value='shared-id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(claim, TARGET);
    let owners = crate::migration::rows(
        &store.db,
        "SELECT item_id FROM catalog_nfo_file_attributions ORDER BY item_id",
        &[],
    )
    .unwrap();
    assert_eq!(owners, vec![json!({"item_id":TARGET})]);
    let retained = crate::migration::snapshot(&store.db).unwrap();
    assert!(matches!(
        commands::execute_candidate_for_test(
            store,
            &command,
            physical::authority(),
            physical::policy(),
            |_, _, _| panic!("reparent replay must not repeat withdrawal or first initialization")
        )
        .unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(crate::migration::snapshot(&store.db).unwrap(), retained);
}
