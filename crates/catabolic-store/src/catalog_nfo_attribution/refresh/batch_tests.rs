//! Actual candidate receipt corridor over real closed-scan/typed-version owners.
//! Controlled host evidence binds this fixture's complete original selection;
//! this does not qualify the future Motion filesystem/audit adapter.
mod first;
mod imported;
mod retirement;

use super::observation::{self, tests as physical};
use super::*;
use crate::catalog_nfo_attribution::{
    NfoCreatedSourceAuthority, bootstrap, verify_created_no_origin_baseline_in,
    verify_source_history_in,
};
use crate::{
    catalog_commands::{self as commands, CatalogAuthority, CommandOutcome},
    catalog_metadata_store::{
        MetadataAccess, MetadataInitializeV2, MetadataSystemReplace, PolicyField,
        PolicyNormalization, PolicyValueType,
    },
    catalog_tag_mutation::{TagAuthority, TagSubject},
};
use catabolic_core::catalog_metadata::SourceKind;
use std::cell::Cell;

fn grant(access: MetadataAccess) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: physical::HOST.into(),
        subject: physical::WORK.into(),
        access,
    }
}
fn initialization() -> MetadataAuthority {
    grant(MetadataAccess::InitializeV2 {
        system_fields: ["motion_catalog".into()].into(),
    })
}
fn source() -> MetadataAuthority {
    grant(MetadataAccess::Source {
        namespace: "motion.nfo".into(),
        record_id: "document".into(),
        kind: SourceKind::Provider,
    })
}
fn structural() -> MetadataAuthority {
    grant(MetadataAccess::SourceV2 {
        namespace: "motion.catalog".into(),
        record_id: "work".into(),
        kind: SourceKind::Scan,
        system_fields: ["motion_catalog".into()].into(),
    })
}
fn local_structure() -> MetadataAuthority {
    grant(MetadataAccess::SourceV2 {
        namespace: "motion.local".into(),
        record_id: "document".into(),
        kind: SourceKind::Local,
        system_fields: ["motion_catalog".into()].into(),
    })
}
fn tag_grant() -> TagAuthority {
    TagAuthority {
        host_namespace: physical::HOST.into(),
        allowed_subjects: [TagSubject::Item(physical::WORK.into())].into(),
        allow_global_merge: false,
    }
}
fn committed(outcome: CommandOutcome) -> Value {
    match outcome {
        CommandOutcome::Committed { result, .. } => result,
        other => panic!("not committed: {other:?}"),
    }
}

fn ready() -> physical::Fixture {
    let fixture = physical::Fixture::new(false);
    let store = &fixture.store;
    let init = MetadataInitializeV2 {
        contract_version: 2,
        subject: physical::WORK.into(),
        policies: vec![
            PolicyField {
                field: "local_note".into(),
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
            PolicyField {
                field: "motion_catalog".into(),
                value_type: PolicyValueType::Json,
                nullable: false,
                normalization: PolicyNormalization::Exact,
            },
        ],
        system_fields: ["motion_catalog".into()].into(),
    };
    let command = physical::envelope(
        store,
        "refresh-init",
        json!({"operation":"metadata_initialize_v2","request":init}),
    );
    let initialization = initialization();
    let extension = grant(MetadataAccess::ExtendSchema);
    committed(
        commands::execute_with_authority(
            store,
            &command,
            CatalogAuthority::new(physical::authority())
                .with_metadata(&initialization)
                .with_metadata_initialization(&initialization),
            physical::policy(),
        )
        .unwrap(),
    );
    let marker=MetadataSystemReplace{contract_version:1,subject:physical::WORK.into(),namespace:"motion.catalog".into(),record_id:"work".into(),expected_schema_fingerprint:metadata::current_schema_fingerprint(&store.db,physical::WORK).unwrap(),expected_revision:0,
        fields_json:json!({"motion_catalog":{"contract_version":1,"item_id":physical::WORK,"media_type":"movie","external_ids":[],"unrelated":"preserved"}}).to_string(),evidence_json:r#"{"owner":"fixture original structural archive"}"#.into(),origin_creation_intent:None};
    let command = physical::envelope(
        store,
        "refresh-structure",
        json!({"operation":"metadata_system_replace","request":marker}),
    );
    let structure = structural();
    committed(
        commands::execute_with_authority(
            store,
            &command,
            CatalogAuthority::new(physical::authority())
                .with_metadata(&structure)
                .with_metadata_initialization(&initialization)
                .with_metadata_schema(&extension),
            physical::policy(),
        )
        .unwrap(),
    );
    // Actual mixed local/document source retains its independently owned user
    // field while the refresh patches only the granted structural field.
    let local = local_structure();
    let change=metadata::MetadataMutation {contract_version:1,change:metadata::MetadataChange::Patch {
        subject:physical::WORK.into(),namespace:"motion.local".into(),record_id:"document".into(),
        schema_fingerprint:metadata::current_schema_fingerprint(&store.db,physical::WORK).unwrap().unwrap(),expected_revision:0,
        edits_json:json!({"motion_catalog":{"op":"set","value":{"contract_version":1,"item_id":physical::WORK,"media_type":"movie","external_ids":[],"unrelated":"preserved"}},
            "local_note":{"op":"set","value":{"exact":[3,1,2]}}}).to_string(),
        evidence_json:r#"{"owner":"controlled mixed local structural proof"}"#.into(),
    }};
    let command = physical::envelope(
        store,
        "refresh-local-structure",
        json!({"operation":"metadata","mutation":change}),
    );
    committed(
        commands::execute_with_authority(
            store,
            &command,
            CatalogAuthority::new(physical::authority()).with_metadata(&local),
            physical::policy(),
        )
        .unwrap(),
    );
    let request=ContributionReplace{contract_version:2,subject:physical::WORK.into(),namespace:"motion.nfo".into(),record_id:"document".into(),expected_schema_fingerprint:metadata::current_schema_fingerprint(&store.db,physical::WORK).unwrap(),expected_revision:0,
        fields_json:r#"{"title":"Original"}"#.into(),evidence_json:json!({"owner":"motion.metadata.replace","request_digest":"a".repeat(64),"expected_aggregate_revision":7}).to_string(),tags:vec!["old".into()],excluded_tags:vec![],origin_creation_intent:None};
    let first = physical::envelope(
        store,
        "refresh-first-source",
        json!({"operation":"metadata_contribution_replace","request":request}),
    );
    let source = source();
    let tags = tag_grant();
    committed(
        commands::execute_with_authority(
            store,
            &first,
            CatalogAuthority::new(physical::authority())
                .with_metadata(&source)
                .with_metadata_initialization(&initialization)
                .with_metadata_schema(&extension)
                .with_tags(&tags),
            physical::policy(),
        )
        .unwrap(),
    );
    let fixture = fixture.install_candidate();
    let store = &fixture.store;
    let plan=store.transaction_detailed(|_| {
        let baseline=verify_created_no_origin_baseline_in(store,&first,&NfoCreatedSourceAuthority{
            host_namespace:physical::HOST.into(),subject:physical::WORK.into(),request_digest:"a".repeat(64),
            complete_absence_audit_digest:catabolic_core::sha256(json!({"domain":"refresh complete controlled creation archive","first":first.digest().as_str(),"automatic_identities":[],"sidecar_origins":[]}).to_string()),
        },crate::catalog_commands::TrustedCommandAuthority { allow_new_commands: false, ..physical::authority() },physical::generation(store))?;
        let source=verify_source_history_in(store,&baseline,&[],crate::catalog_commands::TrustedCommandAuthority { allow_new_commands: false, ..physical::authority() },physical::generation(store))?;
        bootstrap::prepare_in(store,&[source])
    }).unwrap();
    let command = physical::envelope(store, "refresh-bootstrap", plan.payload());
    committed(
        commands::execute_candidate_for_test(
            store,
            &command,
            physical::authority(),
            physical::policy(),
            move |store, command, _| bootstrap::apply_in(store, command, plan),
        )
        .unwrap(),
    );
    fixture
}

fn request(
    fixture: &physical::Fixture,
    mode: Workflow,
    sidecars: [SidecarObservation; 2],
) -> RefreshRequest {
    request_for(
        fixture,
        physical::WORK,
        [(physical::WORK.into(), 7)].into(),
        mode,
        sidecars,
    )
}
fn request_for(
    fixture: &physical::Fixture,
    subject: &str,
    aggregates: BTreeMap<String, i64>,
    mode: Workflow,
    sidecars: [SidecarObservation; 2],
) -> RefreshRequest {
    fixture
        .store
        .transaction_detailed(|_| -> Result<RefreshRequest> {
            let store = &fixture.store;
            let generation = physical::generation(store);
            let state = state::read_in(store, subject, generation, aggregates[subject])?;
            let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
            let scope = state.presence().fence.clone();
            let observations = sidecars
                .into_iter()
                .enumerate()
                .map(|(index, sidecar)| {
                    let mut selection = fixture.selection_part(index as u32 + 1);
                    selection.lineage.work_item_id = subject.into();
                    let capture =
                        observation::capture_current_in(store, &scope, &selection, &mut budget)?;
                    Ok(Observation {
                        expected_file: capture.current().clone(),
                        sidecar,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let heads = aggregates
                .iter()
                .map(|(id, aggregate)| {
                    Ok((
                        id.clone(),
                        state::read_in(store, id, generation, *aggregate)?
                            .head()
                            .stamp(),
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            Ok(RefreshRequest {
                contract_version: 1,
                workflow: mode,
                expected_aggregates: aggregates.clone(),
                refresh: RefreshRequestV2 {
                    contract_version: CONTRACT_VERSION_V2,
                    fence: scope,
                    subject: subject.into(),
                    expected_heads: heads,
                    observations,
                },
            })
        })
        .unwrap()
}
fn parsed(title: &str, id: Option<&str>) -> SidecarObservation {
    SidecarObservation::Parsed {
        contribution: Contribution {
            fields: [("title".into(), json!(title))].into(),
            tags: ["new".into()].into(),
        },
        external_identity: id.map(str::to_owned),
    }
}
fn unusable() -> SidecarObservation {
    SidecarObservation::Unusable {
        reason: "controlled malformed second sidecar".into(),
    }
}

fn apply_candidate(
    fixture: &physical::Fixture,
    outer: &CommandEnvelope,
    request: &RefreshRequest,
    custody: &dyn observation::RetainedRefreshCustody,
    time: i64,
) -> Result<Value> {
    apply_candidate_inner(fixture, outer, request, custody, time, None)
}
fn apply_candidate_inner(
    fixture: &physical::Fixture,
    outer: &CommandEnvelope,
    request: &RefreshRequest,
    custody: &dyn observation::RetainedRefreshCustody,
    time: i64,
    probe: Option<&mut super::apply::PendingProbe<'_>>,
) -> Result<Value> {
    apply_candidate_configured(fixture, outer, request, custody, time, probe, None, |_| {})
}
fn apply_candidate_configured(
    fixture: &physical::Fixture,
    outer: &CommandEnvelope,
    request: &RefreshRequest,
    custody: &dyn observation::RetainedRefreshCustody,
    time: i64,
    probe: Option<&mut super::apply::PendingProbe<'_>>,
    first_init_probe: Option<&mut super::apply::FirstInitProbe<'_>>,
    configure: impl FnOnce(&mut RefreshAuthority<'_>),
) -> Result<Value> {
    apply_candidate_with_first(
        fixture,
        outer,
        request,
        custody,
        time,
        probe,
        first_init_probe,
        None,
        configure,
    )
}
fn apply_candidate_with_first(
    fixture: &physical::Fixture,
    outer: &CommandEnvelope,
    request: &RefreshRequest,
    custody: &dyn observation::RetainedRefreshCustody,
    time: i64,
    probe: Option<&mut super::apply::PendingProbe<'_>>,
    first_init_probe: Option<&mut super::apply::FirstInitProbe<'_>>,
    imported_first: Option<&super::super::first_source::VerifiedFirstNfoAbsence>,
    configure: impl FnOnce(&mut RefreshAuthority<'_>),
) -> Result<Value> {
    let store = &fixture.store;
    let subject = request.refresh.subject.as_str();
    let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
    let mut observations = Vec::new();
    for (index, entry) in request.refresh.observations.iter().enumerate() {
        let mut selection = fixture.selection_part(index as u32 + 1);
        selection.lineage.work_item_id = subject.into();
        let captured = observation::capture_current_in(
            store,
            &request.refresh.fence,
            &selection,
            &mut budget,
        )?;
        observations.push(observation::ObservationAudit {
            observed_file: entry.expected_file.clone(),
            original_lineage: selection.lineage.clone(),
            original_file_provenance_digest: catabolic_core::sha256(
                "controlled complete original two-part closed scan archive",
            ),
            observation_digest: budget.serialized(entry, 64)?,
            terminal: observation::TerminalObservationAudit::Present {
                selection,
                current: captured.current().clone(),
                availability: captured.availability().clone(),
                source_state: SourceState::Enabled,
                native_binding: captured.native_binding().clone(),
                root_sidecar_audit_digest: budget
                    .serialized(&("controlled complete root-sidecar observation", entry), 64)?,
            },
        });
    }
    let audit = observation::RefreshObservationAudit {
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone().unwrap(),
        generation: physical::generation(store),
        host_namespace: physical::HOST.into(),
        subject: subject.into(),
        outer_digest: outer.digest().as_str().into(),
        request_digest: budget.serialized(&request.refresh, 64)?,
        complete_original_selection_digest: budget.serialized(
            &(
                "complete actual original two-part selection",
                &request.refresh.observations,
            ),
            64,
        )?,
        observations,
        custody,
    };
    let first = if let Some(creation) = fixture.creation.as_ref() {
        let current = state::read_in(store, subject, outer.expected_generation(), 7)?;
        if current.head().source.is_none() {
            Some(super::super::first_source::prepare_in(
                store,
                outer,
                current,
                creation,
                crate::catalog_commands::TrustedCommandAuthority {
                    allow_new_commands: false,
                    ..physical::authority()
                },
                &super::super::first_source::FirstNfoSourceAudit {
                    catalog_id: store.database_id.clone(),
                    incarnation: store.catalog_incarnation.clone().unwrap(),
                    generation: outer.expected_generation(),
                    host_namespace: physical::HOST.into(),
                    subject: subject.into(),
                    outer_digest: outer.digest().as_str().into(),
                    creation_digest: creation.digest().as_str().into(),
                    complete_absence_audit_digest: catabolic_core::sha256(
                        json!({
                        "domain":"controlled complete fresh original fixture subject archive",
                        "subject":subject,"creation":creation.digest().as_str(),
                        "subject_automatic_identities":[],"subject_sidecar_origins":[]})
                        .to_string(),
                    ),
                },
            )?)
        } else {
            None
        }
    } else {
        None
    };
    let scoped = |mut value: MetadataAuthority| {
        value.subject = subject.into();
        value
    };
    let mut target_tags = tag_grant();
    target_tags.allowed_subjects = [TagSubject::Item(subject.into())].into();
    let mut authority = RefreshAuthority {
        first_sources: imported_first
            .or(first.as_ref())
            .map(|f| [(subject.into(), f)].into())
            .unwrap_or_default(),
        workflow: request.workflow,
        observations: &audit,
        subjects: [(
            subject.into(),
            SubjectAuthority {
                aggregate_revision: 7,
                curation_audit_digest: catabolic_core::sha256(
                    "controlled unrestricted host with complete work membership",
                ),
                source: scoped(source()),
                initialization: scoped(initialization()),
                extension: scoped(grant(MetadataAccess::ExtendSchema)),
                tags: target_tags,
                structural_sources: vec![
                    scoped(structural()),
                    scoped(local_structure()),
                    scoped(grant(MetadataAccess::SourceV2 {
                        namespace: "motion.structure".into(),
                        record_id: "source_projection".into(),
                        kind: SourceKind::Scan,
                        system_fields: ["motion_catalog".into()].into(),
                    })),
                ],
                structural_legacy: fixture
                    .creation
                    .as_ref()
                    .map(|_| scoped(grant(MetadataAccess::ReconcileLegacy))),
            },
        )]
        .into(),
    };
    configure(&mut authority);
    let prepared = prepare_in(store, outer, request, &authority)?;
    if let Some(probe) = first_init_probe {
        super::apply::apply_with_first_init_probe(store, prepared, time, probe)
    } else if let Some(probe) = probe {
        super::apply::apply_with_probe(store, prepared, time, probe)
    } else {
        apply_in(store, prepared, time)
    }
}
fn execute(
    fixture: &physical::Fixture,
    id: &str,
    request: &RefreshRequest,
) -> (CommandEnvelope, Value) {
    let command = physical::envelope(
        &fixture.store,
        id,
        json!({"operation":"nfo_refresh","request":request}),
    );
    let custody = || Ok(());
    let result = commands::execute_candidate_for_test(
        &fixture.store,
        &command,
        physical::authority(),
        physical::policy(),
        |_, outer, time| apply_candidate(fixture, outer, request, &custody, time),
    )
    .unwrap_or_else(|error| panic!("refresh operation {id}: {error:?}"));
    (command, committed(result))
}

#[test]
fn actual_refresh_partial_direct_job_refusal_withdraw_reappear_replay_and_restart() {
    let fixture = ready();
    let request = request(
        &fixture,
        Workflow::MetadataJob,
        [parsed(" Changed ", Some("opaque/NFO")), unusable()],
    );
    let before = crate::migration::snapshot(&fixture.store.db).unwrap();
    let command = physical::envelope(
        &fixture.store,
        "incomplete-job",
        json!({"operation":"nfo_refresh","request":request}),
    );
    let custody = || Ok(());
    assert!(
        commands::execute_candidate_for_test(
            &fixture.store,
            &command,
            physical::authority(),
            physical::policy(),
            |_, outer, time| apply_candidate(&fixture, outer, &request, &custody, time)
        )
        .is_err()
    );
    assert_eq!(
        crate::migration::snapshot(&fixture.store.db).unwrap(),
        before
    );
    let mut request = request;
    request.workflow = Workflow::DirectRefresh;
    let (command, result) = execute(&fixture, "partial-direct", &request);
    assert_eq!(result["counts"]["processed"], 1);
    assert_eq!(result["counts"]["skipped"], 1);
    assert_eq!(
        result["states"][physical::WORK]["head"]["source"]["revision"],
        2
    );
    let snapshot = crate::migration::snapshot(&fixture.store.db).unwrap();
    assert!(matches!(
        commands::execute_candidate_for_test(
            &fixture.store,
            &command,
            physical::authority(),
            physical::policy(),
            |_, _, _| panic!("replay must not reconstruct stale observations")
        )
        .unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(
        crate::migration::snapshot(&fixture.store.db).unwrap(),
        snapshot
    );
    let fixture = fixture.reopen_candidate();
    let absent = self::request(
        &fixture,
        Workflow::DirectRefresh,
        [SidecarObservation::Absent, SidecarObservation::Absent],
    );
    let (_, withdrawn) = execute(&fixture, "withdraw", &absent);
    assert!(withdrawn["states"][physical::WORK]["presence"]["public_revision"].is_null());
    let reappear = self::request(
        &fixture,
        Workflow::DirectRefresh,
        [parsed(" Reappeared ", Some("opaque/NFO")), unusable()],
    );
    let (_, current) = execute(&fixture, "reappear", &reappear);
    assert_eq!(
        current["states"][physical::WORK]["presence"]["incarnation"],
        2
    );
    assert_eq!(
        current["states"][physical::WORK]["presence"]["public_revision"],
        1
    );
    let marker: String = fixture
        .store
        .db
        .query_row(
            "SELECT metadata FROM items WHERE id=?",
            [physical::WORK],
            |r| r.get(0),
        )
        .unwrap();
    let marker: Value = serde_json::from_str(&marker).unwrap();
    assert_eq!(marker["title"], "reappeared");
    assert_eq!(marker["local_note"], json!({"exact":[3,1,2]}));
    assert_eq!(
        marker["motion_catalog"]["external_ids"][0]["value"],
        "opaque/NFO"
    );
    assert_eq!(marker["motion_catalog"]["unrelated"], "preserved");
    // A newly reviewed manual pin may differ from the retained document ID.
    // Pure eligibility must retain that document/projection untouched rather
    // than invoking the strict protected-field replacement planner.
    decide_pin(&fixture, "different-manual-id");
    let source_before = crate::migration::rows(
        &fixture.store.db,
        "SELECT * FROM catalog_metadata_sources ORDER BY item_id,namespace,record_id",
        &[],
    )
    .unwrap();
    let nfo_before = crate::migration::rows(
        &fixture.store.db,
        "SELECT * FROM catalog_nfo_states ORDER BY item_id",
        &[],
    )
    .unwrap();
    let pinned = self::request(
        &fixture,
        Workflow::DirectRefresh,
        [parsed("Should not replace", Some("opaque/NFO")), unusable()],
    );
    let (_, result) = execute(&fixture, "pin-mismatched-retained-document", &pinned);
    assert_eq!(result["counts"]["processed"], 1);
    assert_eq!(
        crate::migration::rows(
            &fixture.store.db,
            "SELECT * FROM catalog_metadata_sources ORDER BY item_id,namespace,record_id",
            &[]
        )
        .unwrap(),
        source_before
    );
    assert_eq!(
        crate::migration::rows(
            &fixture.store.db,
            "SELECT * FROM catalog_nfo_states ORDER BY item_id",
            &[]
        )
        .unwrap(),
        nfo_before
    );
}

#[test]
fn swallowed_terminal_custody_error_after_real_refresh_writes_cannot_commit() {
    let fixture = ready();
    let store = &fixture.store;
    let request = request(
        &fixture,
        Workflow::DirectRefresh,
        [parsed("Changed", Some("new-id")), unusable()],
    );
    let command = physical::envelope(
        store,
        "terminal-refusal",
        json!({"operation":"nfo_refresh","request":request}),
    );
    let before = crate::migration::snapshot(&store.db).unwrap();
    let observed_effect = Cell::new(false);
    let custody = || {
        let revision:i64=store.db.query_row("SELECT revision FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo' AND record_id='document'",[physical::WORK],|r|r.get(0))?;
        if revision > 1 {
            observed_effect.set(true);
            Err(Error("terminal source lifecycle withdrawn".into()))
        } else {
            Ok(())
        }
    };
    let result = store.transaction_detailed::<(), Error>(|db| {
        let error = apply_candidate(&fixture, &command, &request, &custody, 123).unwrap_err();
        assert!(
            error.0.contains("terminal source lifecycle withdrawn"),
            "{error}"
        );
        assert!(observed_effect.get());
        assert!(
            db.execute_batch("COMMIT").is_err(),
            "swallowed terminal error must retain batch barrier"
        );
        Err(Error("expected rollback".into()))
    });
    assert!(result.is_err());
    assert_eq!(crate::migration::snapshot(&store.db).unwrap(), before);
}

fn decide_pin(fixture: &physical::Fixture, value: &str) {
    use crate::catalog_pins::{self, PinAuthority, PinBatch, PinChange, PinDecision, PinMutation};
    let store = &fixture.store;
    let grant = PinAuthority {
        host_namespace: physical::HOST.into(),
        readable: Default::default(),
        mutable: [("nfo".into(), [physical::WORK.into()].into())].into(),
    };
    let batch = PinBatch {
        contract_version: 1,
        changes: vec![PinChange {
            item_id: physical::WORK.into(),
            namespace: "nfo".into(),
            expected_revision: 0,
            decision: PinDecision::Set {
                value: value.into(),
            },
        }],
    };
    let preview = catalog_pins::preview_pins(store, &batch, &grant).unwrap();
    let mutation = PinMutation {
        batch,
        preview_digest: preview.digest,
    };
    let command = physical::envelope(
        store,
        "refresh-manual-pin",
        json!({"operation":"pins","mutation":mutation}),
    );
    committed(
        commands::execute_candidate_for_test(
            store,
            &command,
            physical::authority(),
            physical::policy(),
            |store, outer, time| {
                let result = catalog_pins::apply_pins(store, &mutation, outer, &grant, time)
                    .map_err(refused)?;
                Ok(serde_json::to_value(result)?)
            },
        )
        .unwrap(),
    );
}

#[test]
fn actual_pending_batch_rejects_wrong_key_subject_revision_generation_and_missing_barrier() {
    let fixture = ready();
    let store = &fixture.store;
    let request = request(
        &fixture,
        Workflow::DirectRefresh,
        [parsed("Changed", Some("pending-id")), unusable()],
    );
    let command = physical::envelope(
        store,
        "pending-negative-batch",
        json!({"operation":"nfo_refresh","request":request}),
    );
    let before = crate::migration::snapshot(&store.db).unwrap();
    let observed = Cell::new(false);
    let mut probe = |store: &Store,
                     actual: &RefreshReadPermit,
                     budget: &mut ProofBudget|
     -> Result<()> {
        observed.set(true);
        let head = &actual.written[physical::WORK];
        actual.authorize_state_read_in(
            store,
            physical::WORK,
            head.state_revision,
            head.source_revision,
            actual.expected_generation,
            budget,
        )?;
        assert!(
            actual
                .authorize_source_read_in(
                    store,
                    "another-subject",
                    head.source_revision,
                    actual.expected_generation,
                    budget
                )
                .is_err()
        );
        assert!(
            actual
                .authorize_source_read_in(
                    store,
                    physical::WORK,
                    head.source_revision + 1,
                    actual.expected_generation,
                    budget
                )
                .is_err()
        );
        assert!(
            actual
                .authorize_state_read_in(
                    store,
                    physical::WORK,
                    head.state_revision + 1,
                    head.source_revision,
                    actual.expected_generation,
                    budget
                )
                .is_err()
        );
        assert!(
            actual
                .authorize_state_read_in(
                    store,
                    physical::WORK,
                    head.state_revision,
                    head.source_revision,
                    actual.expected_generation + 1,
                    budget
                )
                .is_err()
        );
        let wrong = CommandEnvelope::new(
            1,
            catabolic_core::catalog_command::CommandKey::new(
                &store.database_id,
                physical::HOST,
                "different-pending-command",
            )
            .unwrap(),
            actual.outer.admission_incarnation(),
            actual.outer.expected_generation(),
            &actual.outer.payload().to_string(),
        )
        .unwrap();
        let wrong = RefreshReadPermit {
            outer: wrong,
            expected_generation: actual.expected_generation,
            subjects: actual.subjects.clone(),
            written: actual.written.clone(),
        };
        let error =
            state::read_pending_in(store, physical::WORK, actual.expected_generation, 7, &wrong)
                .unwrap_err();
        assert!(
            error.0.contains("receipt absent"),
            "wrong pending command must not bypass history: {error}"
        );
        // Adversarial barrier omission is confined to a savepoint. No source,
        // receipt or grant rows are fabricated, and rollback restores barrier.
        store.db.execute_batch("SAVEPOINT omitted_barrier")?;
        store.db.execute(
            "DELETE FROM catalog_metadata_permits WHERE item_id=?",
            [physical::WORK],
        )?;
        assert!(
            actual
                .authorize_state_read_in(
                    store,
                    physical::WORK,
                    head.state_revision,
                    head.source_revision,
                    actual.expected_generation,
                    budget
                )
                .is_err()
        );
        store
            .db
            .execute_batch("ROLLBACK TO omitted_barrier; RELEASE omitted_barrier")?;
        Err(Error("stop after actual pending owner verification".into()))
    };
    let custody = || Ok(());
    let result = store.transaction_detailed::<(), Error>(|db| {
        let error = apply_candidate_inner(
            &fixture,
            &command,
            &request,
            &custody,
            123,
            Some(&mut probe),
        )
        .unwrap_err();
        assert!(
            error
                .0
                .contains("stop after actual pending owner verification"),
            "{error}"
        );
        assert!(observed.get());
        assert!(
            db.execute_batch("COMMIT").is_err(),
            "dropping real pending permit after denial must not commit"
        );
        Err(Error(
            "expected rollback after pending owner refusal".into(),
        ))
    });
    assert!(result.is_err());
    assert_eq!(crate::migration::snapshot(&store.db).unwrap(), before);
}

#[test]
fn response_budget_overflow_after_real_refresh_writes_cannot_commit() {
    post_effect_budget_refusal(false);
}
#[test]
fn cached_commitment_overflow_after_real_refresh_writes_cannot_commit() {
    post_effect_budget_refusal(true);
}
fn post_effect_budget_refusal(commitment: bool) {
    let fixture = ready();
    let store = &fixture.store;
    let request = request(
        &fixture,
        Workflow::DirectRefresh,
        [parsed("Changed", Some("response-overflow")), unusable()],
    );
    let command = physical::envelope(
        store,
        "response-budget-refusal",
        json!({"operation":"nfo_refresh","request":request}),
    );
    let before = crate::migration::snapshot(&store.db).unwrap();
    let observed_effect = Cell::new(false);
    // Adversarial response is allocated by the test, not by the response owner.
    // Streaming refusal must occur before that owner allocates its output Value.
    let oversized = "x".repeat(catabolic_core::catalog_metadata::MAX_METADATA_SNAPSHOT_BYTES + 1);
    let mut probe = |store: &Store,
                     _: &RefreshReadPermit,
                     budget: &mut ProofBudget|
     -> Result<()> {
        let revision: i64 = store.db.query_row(
            "SELECT revision FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo' AND record_id='document'",
            [physical::WORK], |row| row.get(0),
        )?;
        assert!(revision > 1, "must exercise the real post-effect barrier");
        observed_effect.set(true);
        if commitment {
            super::commitment::AdmittedCommitment::capture(budget, &oversized)?;
        } else {
            super::apply::admitted_response(budget, &oversized)?;
        }
        panic!("oversized response or commitment must not be admitted");
    };
    let result = store.transaction_detailed::<(), Error>(|db| {
        let error = apply_candidate_inner(
            &fixture,
            &command,
            &request,
            &|| Ok(()),
            123,
            Some(&mut probe),
        )
        .unwrap_err();
        assert!(
            error.0.contains("catalog_nfo_proof_budget_refused"),
            "{error}"
        );
        assert!(
            observed_effect.get(),
            "post-effect probe was not reached: {error}"
        );
        assert!(
            db.execute_batch("COMMIT").is_err(),
            "response refusal must retain the complete batch barrier"
        );
        Err(Error(
            "expected rollback after response admission refusal".into(),
        ))
    });
    assert!(result.is_err());
    assert_eq!(crate::migration::snapshot(&store.db).unwrap(), before);
}
