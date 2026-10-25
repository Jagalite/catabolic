//! Explicit test-only schema41 candidate, not registered runtime admission.
//! Every baseline comes from a real40 source/import owner and historical proof.
//! No immutable history, receipt, typed token or cleanup witness is fabricated.
//! The controlled test host owns the complete constructed archive and prepares
//! nonserde absence/root/origin grants from those facts. This does NOT qualify
//! the future Motion archived-root audit builder or infer absence from JSON.
use super::{
    NfoCreatedSourceAuthority, NfoImportAuthority, VerifiedNfoSource, bootstrap, permit,
    public_put, publication, state, verify_created_no_origin_baseline_in,
    verify_imported_no_origin_baseline_in, verify_source_history_in,
};
use crate::{
    Error, Result, Store, candidate41_test_support as candidate,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
    },
    catalog_import::{
        self, ActivationBinding, ImportAuthority, ImportBatch, ImportItem, ImportMutation,
        ImportStep, SnapshotManifest, TrustedImportActivation,
    },
    catalog_import_typed::{
        ImportedContribution, ImportedMetadata, ImportedSourceKind, TypedAuthority, TypedBatch,
    },
    catalog_metadata_store::{
        self as metadata, ContributionReplace, MetadataAccess, MetadataAuthority,
        MetadataInitializeV2, PolicyField, PolicyNormalization, PolicyValueType,
    },
    catalog_pins::{self, PinAuthority, PinBatch, PinChange, PinDecision, PinMutation},
    catalog_tag_mutation::{TagAuthority, TagSubject},
    migration,
};
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility},
    catalog_host_lease::HostAuthorization,
    catalog_metadata::{
        SourceKind,
        nfo_refresh::{DocumentState, SubjectState},
    },
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Duration};

const HOST: &str = "nfo-candidate-tests";
const SUBJECT: &str = "nfo-subject";
const OTHER: &str = "other-subject";
const NFO: &str = "motion.nfo";
const RECORD: &str = "document";
const AGGREGATE: i64 = 7;
pub(super) fn authority(write: bool) -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: write,
        identity_authority: None,
    }
}
pub(super) fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_hold_records: 100,
        max_logical_bytes: 16 * 1024 * 1024,
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
pub(super) fn envelope(store: &Store, id: &str, payload: Value) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, HOST, id).unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        generation(store),
        &payload.to_string(),
    )
    .unwrap()
}
fn committed(outcome: CommandOutcome) -> Value {
    match outcome {
        CommandOutcome::Committed { result, .. } => result,
        other => panic!("not committed: {other:?}"),
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
fn evidence() -> Value {
    json!({"owner":"motion.metadata.replace","request_digest":"a".repeat(64),"expected_aggregate_revision":AGGREGATE})
}
fn source_grant() -> MetadataAuthority {
    grant(MetadataAccess::Source {
        namespace: NFO.into(),
        record_id: RECORD.into(),
        kind: SourceKind::Provider,
    })
}
fn initialization() -> MetadataAuthority {
    grant(MetadataAccess::InitializeV2 {
        system_fields: BTreeSet::new(),
    })
}
pub(super) fn put_request(
    store: &Store,
    expected: i64,
    fields: Value,
    names: &[&str],
) -> ContributionReplace {
    ContributionReplace {
        contract_version: 2,
        subject: SUBJECT.into(),
        namespace: NFO.into(),
        record_id: RECORD.into(),
        expected_schema_fingerprint: metadata::current_schema_fingerprint(&store.db, SUBJECT)
            .unwrap(),
        expected_revision: expected,
        fields_json: fields.to_string(),
        evidence_json: evidence().to_string(),
        tags: names.iter().map(|v| (*v).into()).collect(),
        excluded_tags: vec![],
        origin_creation_intent: None,
    }
}
fn put_command(
    store: &Store,
    id: &str,
    public: i64,
    request: &ContributionReplace,
) -> CommandEnvelope {
    envelope(
        store,
        id,
        json!({"operation":"nfo_public_put","contract_version":1,"expected_public_revision":public,"request":request}),
    )
}
pub(super) fn replace_40(
    store: &Store,
    id: &str,
    request: &ContributionReplace,
    kind: SourceKind,
) -> CommandEnvelope {
    let command = envelope(
        store,
        id,
        json!({"operation":"metadata_contribution_replace","request":request}),
    );
    let source = grant(MetadataAccess::Source {
        namespace: request.namespace.clone(),
        record_id: request.record_id.clone(),
        kind,
    });
    let init = initialization();
    let extension = grant(MetadataAccess::ExtendSchema);
    let tags = tags();
    committed(
        commands::execute_with_authority(
            store,
            &command,
            CatalogAuthority::new(authority(true))
                .with_metadata(&source)
                .with_metadata_initialization(&init)
                .with_metadata_schema(&extension)
                .with_tags(&tags),
            policy(),
        )
        .unwrap(),
    );
    command
}
fn apply_put(
    store: &Store,
    command: &CommandEnvelope,
    request: &ContributionReplace,
    public: i64,
    at: i64,
) -> Result<Value> {
    let source = source_grant();
    let init = initialization();
    let extension = grant(MetadataAccess::ExtendSchema);
    let tags = tags();
    let published = public_put::apply_existing_put_in(
        store, command, request, public, AGGREGATE, &source, &init, &extension, &tags, at,
    )?;
    Ok(
        json!({"operation":"nfo_public_put","subject":published.subject(),"source_revision":published.source_revision(),"attribution_revision":published.attribution_revision(),"presence":published.presence(),"schema_fingerprint":published.schema_fingerprint(),"item_revision":published.item_revision(),"observed_generation":published.observed_generation(),"source_proof_digest":published.source_proof_digest()}),
    )
}
fn execute_put(
    store: &Store,
    command: &CommandEnvelope,
    request: &ContributionReplace,
    public: i64,
) -> CommandOutcome {
    commands::execute_candidate_for_test(
        store,
        command,
        authority(true),
        policy(),
        |store, command, at| apply_put(store, command, request, public, at),
    )
    .unwrap()
}

struct Imported {
    manifest: SnapshotManifest,
    batches: Vec<ImportBatch>,
    activation: CommandEnvelope,
    grant: NfoImportAuthority,
    physical: Option<PhysicalArchive>,
}
struct PhysicalArchive {
    manifest: SnapshotManifest,
    batches: Vec<ImportBatch>,
    activation: CommandEnvelope,
    grant: super::NfoImportedOriginAuthority,
}
enum Baseline {
    Created(CommandEnvelope),
    Imported(Imported),
}
pub(super) struct Fixture {
    _root: tempfile::TempDir,
    pub(super) store: Store,
    baseline: Baseline,
}
impl Fixture {
    pub(super) fn created() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("candidate.db");
        migration::initialize_native_at(&path, 40).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        for subject in [SUBJECT, OTHER] {
            let command = envelope(
                &store,
                &format!("create-{subject}"),
                json!({"operation":"item_put","item":{"id":subject,"kind":"video","metadata":{}}}),
            );
            committed(commands::execute(&store, &command, authority(true), policy()).unwrap());
        }
        let init = MetadataInitializeV2 {
            contract_version: 2,
            subject: SUBJECT.into(),
            policies: vec![PolicyField {
                field: "title".into(),
                value_type: PolicyValueType::String,
                nullable: false,
                normalization: PolicyNormalization::TrimAsciiLowercase,
            }],
            system_fields: BTreeSet::new(),
        };
        let command = envelope(
            &store,
            "initialize",
            json!({"operation":"metadata_initialize_v2","request":init}),
        );
        let init_grant = initialization();
        committed(
            commands::execute_with_authority(
                &store,
                &command,
                CatalogAuthority::new(authority(true))
                    .with_metadata(&init_grant)
                    .with_metadata_initialization(&init_grant),
                policy(),
            )
            .unwrap(),
        );
        let request = put_request(
            &store,
            0,
            json!({"title":"  Original NFO  "}),
            &["old", "suppressed"],
        );
        let first = replace_40(&store, "first-nfo", &request, SourceKind::Provider);
        let mut local = put_request(&store, 0, json!({}), &[]);
        local.namespace = "motion.local".into();
        local.excluded_tags = vec!["suppressed".into()];
        replace_40(&store, "local-exclusion", &local, SourceKind::Local);
        Self {
            _root: root,
            store,
            baseline: Baseline::Created(first),
        }
    }
    pub(super) fn proof_in(&self) -> Result<VerifiedNfoSource> {
        let baseline = match &self.baseline {
            Baseline::Created(first) => verify_created_no_origin_baseline_in(
                &self.store,
                first,
                &NfoCreatedSourceAuthority {
                    host_namespace: HOST.into(),
                    subject: SUBJECT.into(),
                    request_digest: "a".repeat(64),
                    complete_absence_audit_digest: catabolic_core::sha256(json!({"domain":"candidate.complete-created-archive.v1","subject":SUBJECT,"first":first.digest().as_str(),"automatic_identities":[],"sidecar_origins":[]}).to_string()),
                },
                authority(false),
                generation(&self.store),
            )?,
            Baseline::Imported(archive) => {
                if let Some(physical) = &archive.physical {
                    let origin = super::verify_imported_origin_in(
                        &self.store,
                        &archive.activation,
                        &physical.manifest,
                        &physical.batches,
                        &physical.activation,
                        &physical.grant,
                        authority(false),
                        generation(&self.store),
                    )?;
                    super::verify_imported_origin_baseline_in(
                        &self.store,
                        SUBJECT,
                        &archive.manifest,
                        &archive.batches,
                        &archive.activation,
                        &archive.grant,
                        &origin,
                        authority(false),
                        generation(&self.store),
                    )?
                } else {
                    verify_imported_no_origin_baseline_in(
                        &self.store,
                        SUBJECT,
                        &archive.manifest,
                        &archive.batches,
                        &archive.activation,
                        &archive.grant,
                        authority(false),
                        generation(&self.store),
                    )?
                }
            }
        };
        verify_source_history_in(
            &self.store,
            &baseline,
            &[],
            authority(false),
            generation(&self.store),
        )
    }
    pub(super) fn install(mut self) -> Self {
        let before = migration::snapshot(&self.store.db).unwrap();
        let path = self.store.path.clone();
        drop(self.store);
        candidate::install_candidate41_for_test(&path).unwrap();
        assert!(!migration::is_supported_native_schema(41));
        assert!(
            Store::open(&path, true, false).is_err(),
            "candidate must not become production admission"
        );
        self.store = candidate::open_candidate41_for_test(&path, true).unwrap();
        assert_eq!(self.store.schema_version, 41);
        assert_eq!(
            self.store
                .db
                .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            41
        );
        migration::preservation(&self.store.db, &before).unwrap();
        self
    }
    pub(super) fn bootstrap(&self) -> CommandEnvelope {
        let plan = self
            .store
            .transaction_detailed(|_| {
                let source = self.proof_in()?;
                bootstrap::prepare_in(&self.store, &[source])
            })
            .unwrap();
        let command = envelope(&self.store, "bootstrap", plan.payload());
        let result = committed(
            commands::execute_candidate_for_test(
                &self.store,
                &command,
                authority(true),
                policy(),
                move |store, command, _| bootstrap::apply_in(store, command, plan),
            )
            .unwrap(),
        );
        assert_eq!(result["source_count"], 1);
        command
    }
    pub(super) fn ready() -> Self {
        let f = Self::created().install();
        f.bootstrap();
        f
    }
    fn reopen(mut self) -> Self {
        let path = self.store.path.clone();
        drop(self.store);
        self.store = candidate::open_candidate41_for_test(&path, true).unwrap();
        self
    }
    fn state(&self) -> state::CurrentNfoState {
        self.store
            .transaction_detailed(|_| {
                state::read_in(&self.store, SUBJECT, generation(&self.store), AGGREGATE)
            })
            .unwrap()
    }
}

#[test]
fn candidate_actual_owner_put_replay_restart_and_local_tag_suppression() {
    let f = Fixture::ready();
    let before = f.state();
    assert_eq!(before.presence().presence.public_revision, Some(1));
    let suppression = migration::rows(
        &f.store.db,
        "SELECT * FROM catalog_item_tag_suppressions",
        &[],
    )
    .unwrap();
    let request = put_request(
        &f.store,
        1,
        json!({"title":"  Changed NFO  ","added":17}),
        &["new", "suppressed"],
    );
    let command = put_command(&f.store, "put", 1, &request);
    let result = committed(execute_put(&f.store, &command, &request, 1));
    assert_eq!(result["source_revision"], 2);
    assert_eq!(result["attribution_revision"], 2);
    assert_eq!(result["presence"]["public_revision"], 2);
    assert_eq!(result["observed_generation"], generation(&f.store));
    let current = f.state();
    assert_eq!(
        current.native_source().unwrap().fields()["title"],
        "changed nfo"
    );
    assert_eq!(current.native_source().unwrap().fields()["added"], 17);
    let DocumentState::Present { contribution, .. } =
        &current.presence().head.source.as_ref().unwrap().document
    else {
        panic!("present source expected")
    };
    assert_eq!(
        contribution.tags,
        ["new".to_owned(), "suppressed".to_owned()].into()
    );
    assert_eq!(
        migration::rows(
            &f.store.db,
            "SELECT * FROM catalog_item_tag_suppressions",
            &[]
        )
        .unwrap(),
        suppression
    );
    let after = migration::snapshot(&f.store.db).unwrap();
    let replay = commands::execute_candidate_for_test(
        &f.store,
        &command,
        authority(false),
        policy(),
        |_, _, _| panic!("receipt-first replay must not invoke mutation"),
    )
    .unwrap();
    match replay {
        CommandOutcome::Replay { result: got, .. } => assert_eq!(got, result),
        other => panic!("{other:?}"),
    }
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), after);
    drop(current);
    drop(before);
    let f = f.reopen();
    assert_eq!(f.state().presence().presence.public_revision, Some(2));
    assert!(matches!(
        commands::execute_candidate_for_test(
            &f.store,
            &command,
            authority(false),
            policy(),
            |_, _, _| panic!("restart replay must not apply")
        )
        .unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), after);
}

fn metadata_half(
    store: &Store,
    command: &CommandEnvelope,
    request: &ContributionReplace,
) -> Result<permit::NfoWritePermit> {
    let current = state::read_in(store, SUBJECT, generation(store), AGGREGATE)?;
    let permit = permit::prepare_put_in(store, current, command, request, 1)?;
    metadata::replace_nfo_contribution(
        store,
        request,
        command,
        &source_grant(),
        &initialization(),
        &grant(MetadataAccess::ExtendSchema),
        &tags(),
        123,
        &permit,
    )
    .map_err(|e| Error(e.to_string()))?;
    Ok(permit)
}
#[derive(Clone, Copy, Debug)]
enum HalfFailure {
    DropPermit,
    DropVerified,
    SwallowVerify,
    SwallowPublish,
    SwallowLatePublish,
}
#[test]
fn actual_commit_refuses_dropped_tokens_and_swallowed_half_effect_errors() {
    for case in [
        HalfFailure::DropPermit,
        HalfFailure::DropVerified,
        HalfFailure::SwallowVerify,
        HalfFailure::SwallowPublish,
        HalfFailure::SwallowLatePublish,
    ] {
        let f = Fixture::ready();
        let before = migration::snapshot(&f.store.db).unwrap();
        let request = put_request(&f.store, 1, json!({"title":"changed"}), &["new"]);
        let command = put_command(&f.store, "incomplete", 1, &request);
        let attempted = std::cell::Cell::new(false);
        let outcome = f.store.transaction_detailed::<_, Error>(|db| {
            let permit = metadata_half(&f.store, &command, &request)?;
            match case {
                HalfFailure::DropPermit => drop(permit),
                HalfFailure::DropVerified => drop(permit.verify_applied_in(&f.store, &command)?),
                HalfFailure::SwallowVerify => {
                    let wrong = put_command(&f.store, "different-command", 1, &request);
                    assert!(permit.verify_applied_in(&f.store, &wrong).is_err());
                }
                HalfFailure::SwallowPublish => {
                    let verified = permit.verify_applied_in(&f.store, &command)?;
                    // Genuine unrelated item owner advances the writer generation;
                    // no NFO token, state/history row or receipt is fabricated.
                    crate::curation::item_put(
                        &f.store,
                        &json!({"id":OTHER,"kind":"video","metadata":{"title":"intervening"}}),
                    )?;
                    assert!(publication::publish_put_in(&f.store, verified).is_err());
                }
                HalfFailure::SwallowLatePublish => {
                    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
                    use std::sync::{
                        Arc,
                        atomic::{AtomicUsize, Ordering},
                    };
                    let verified = permit.verify_applied_in(&f.store, &command)?;
                    let expected_source = verified.source().revision();
                    let expected_attribution = verified
                        .plan()
                        .final_snapshot
                        .head
                        .attribution
                        .as_ref()
                        .unwrap()
                        .revision;
                    let post_metadata_generation = verified.post_metadata_generation();
                    let old_history_count: i64 = db.query_row(
                        "SELECT count(*) FROM catalog_nfo_state_revisions WHERE item_id=?",
                        [SUBJECT],
                        |row| row.get(0),
                    )?;
                    // Preserve the candidate baseline while injecting one denial.
                    // No SQL effect is injected: only the final barrier DELETE is
                    // denied, after the actual publication owner writes its state.
                    let denied = Arc::new(AtomicUsize::new(0));
                    let observed = denied.clone();
                    let reset = crate::catalog_source_succession::authorizer::Guard::for_store(&f.store)?;
                    reset.replace(move |context: AuthContext<'_>| {
                        if context.database_name == Some("main")
                            && matches!(
                                context.action,
                                AuthAction::Delete {
                                    table_name: "catalog_metadata_permits"
                                }
                            )
                        {
                            observed.fetch_add(1, Ordering::SeqCst);
                            Authorization::Deny
                        } else {
                            Authorization::Allow
                        }
                    })?;
                    let failed = publication::publish_put_in(&f.store, verified);
                    reset.restore()?;
                    assert!(failed.is_err());
                    assert!(denied.load(Ordering::SeqCst) > 0);
                    assert!(!db.is_autocommit());
                    let advanced: (i64, i64, i64) = db.query_row(
                        "SELECT s.current_revision,h.source_revision,(SELECT count(*) FROM catalog_nfo_state_revisions WHERE item_id=?1) FROM catalog_nfo_states s JOIN catalog_nfo_state_revisions h ON h.item_id=s.item_id AND h.revision=s.current_revision WHERE s.item_id=?1 AND h.command_operation_id=?2",
                        [SUBJECT, command.key().operation_id()],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )?;
                    assert_eq!(advanced, (expected_attribution, expected_source, old_history_count + 1));
                    assert!(generation(&f.store) > post_metadata_generation);
                    let barrier: (String, i64) = db.query_row(
                        "SELECT incarnation,barrier_id FROM catalog_metadata_permits WHERE item_id=?",
                        [SUBJECT],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    assert_eq!(barrier, (command.admission_incarnation().to_owned(), 1));
                    // Deliberately swallow the late failure. The shared check
                    // below proves real COMMIT still fails and rolls all of these
                    // already-written publication rows back with the metadata.
                }
            }
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM catalog_metadata_permits WHERE item_id=?",
                    [SUBJECT],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            let mut violations =
                db.prepare("PRAGMA foreign_key_check(catalog_metadata_permits)")?;
            assert!(violations.exists([])?);
            attempted.set(true);
            // Ok reaches Store's actual SQLite COMMIT, not the dispatcher's
            // earlier permit-empty validator. Its FK failure must roll back.
            Ok(())
        });
        assert!(attempted.get(), "did not reach COMMIT for {case:?}");
        let error = outcome.unwrap_err();
        assert!(
            error.0.to_ascii_lowercase().contains("foreign key"),
            "{case:?}: {error}"
        );
        assert!(f.store.db.is_autocommit());
        assert_eq!(
            migration::snapshot(&f.store.db).unwrap(),
            before,
            "{case:?}"
        );
        assert_eq!(f.state().native_source().unwrap().revision(), 1);
    }
}

#[test]
fn candidate_wrong_source_authority_and_stale_public_cas_leave_every_table_unchanged() {
    let f = Fixture::ready();
    for stale in [false, true] {
        let before = migration::snapshot(&f.store.db).unwrap();
        let request = put_request(&f.store, 1, json!({"title":"denied"}), &[]);
        let public = if stale { 0 } else { 1 };
        let command = put_command(
            &f.store,
            if stale { "stale" } else { "wrong-source" },
            public,
            &request,
        );
        let result = commands::execute_candidate_for_test(
            &f.store,
            &command,
            authority(true),
            policy(),
            |store, command, at| {
                let source = if stale {
                    source_grant()
                } else {
                    grant(MetadataAccess::Source {
                        namespace: "motion.other".into(),
                        record_id: RECORD.into(),
                        kind: SourceKind::Provider,
                    })
                };
                public_put::apply_existing_put_in(
                    store,
                    command,
                    &request,
                    public,
                    AGGREGATE,
                    &source,
                    &initialization(),
                    &grant(MetadataAccess::ExtendSchema),
                    &tags(),
                    at,
                )?;
                Ok(json!({}))
            },
        );
        assert!(result.is_err());
        assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    }
}

fn activate_40(
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
    let command_for = |mutation: &ImportMutation| {
        let step = match mutation {
            ImportMutation::Prepare { .. } => ImportStep::Prepare,
            ImportMutation::Stage { batch_index, .. } => ImportStep::Batch(*batch_index),
            ImportMutation::Validate { .. } => ImportStep::Validate,
            ImportMutation::Activate { .. } => ImportStep::Activate,
        };
        envelope(
            store,
            &catalog_import::operation_id(id, step).unwrap(),
            json!({"operation":"import","mutation":mutation}),
        )
    };
    let run = |mutation: &ImportMutation, activation: Option<&TrustedImportActivation>| {
        let command = command_for(mutation);
        let mut auth = CatalogAuthority::new(authority(true))
            .with_imports(&grant)
            .with_import_typed(&typed);
        if let Some(activation) = activation {
            auth = auth.with_import_activation(activation);
        }
        committed(commands::execute_with_authority(store, &command, auth, policy()).unwrap())
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
    let validated = run(
        &ImportMutation::Validate {
            import_id: id.into(),
            input_digest: input.clone(),
        },
        None,
    );
    let mapping = validated["mapping_digest"].as_str().unwrap().to_owned();
    let gate = HostAuthorization::new(HOST.into(), format!("freeze-{id}")).unwrap();
    let activation = TrustedImportActivation::after_drain(
        &gate,
        gate.begin_revocation().unwrap(),
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
    let command = command_for(&mutation);
    run(&mutation, Some(&activation));
    (manifest, batches, command, mapping)
}

// Exactly the qualified helper lifecycle used by the earlier physical proof
// tests. Requires the parent's explicit rebuilt non-harness artifact; a digest
// alone does not establish packaging trust. No direct Store scan shortcut.
fn closed_scan_40(path: &std::path::Path, media: &std::path::Path) {
    use catabolic_runtime::{
        helper,
        host::{CONTRACT_VERSION, HostSession},
    };
    let binary = std::path::PathBuf::from(std::env::var_os("CATABOLIC_HOST_TEST_HELPER").expect(
        "CATABOLIC_HOST_TEST_HELPER must identify the qualified rebuilt non-harness helper",
    ));
    let artifact = helper::TrustedClosedObservationArtifact::after_packaging_verification(
        &binary,
        &catabolic_core::sha256(std::fs::read(&binary).unwrap()),
        helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
    )
    .unwrap();
    let gate = HostAuthorization::new(
        HOST.into(),
        format!("candidate-scan-{}", uuid::Uuid::new_v4()),
    )
    .unwrap();
    let mut host =
        HostSession::open_authorized(path, "default", true, CONTRACT_VERSION, gate.clone())
            .unwrap();
    host.register_source("media", media, &[]).unwrap();
    host.register_closed_observation_helper(&artifact).unwrap();
    let handle = host.start_scan("media", ()).unwrap();
    let revoked = gate.begin_revocation().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(report) = host.poll_scan(&handle, Duration::from_millis(50)).unwrap() {
            assert_eq!(report["complete"], true);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "closed candidate scan did not finish"
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
    pub(super) fn imported(with_origin: bool) -> Self {
        use crate::catalog_import_typed::{NativeIdentityBatch, capture_existing_occurrence};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("candidate.db");
        migration::initialize_native_at(&path, 40).unwrap();
        if with_origin {
            let media = root.path().join("media");
            std::fs::create_dir(&media).unwrap();
            std::fs::write(media.join("movie.mkv"), b"candidate original file").unwrap();
            closed_scan_40(&path, &media);
        }
        let store = Store::open(&path, true, false).unwrap();
        let other = envelope(
            &store,
            "create-other",
            json!({"operation":"item_put","item":{"id":OTHER,"kind":"video","metadata":{}}}),
        );
        committed(commands::execute(&store, &other, authority(true), policy()).unwrap());
        let occurrence = if with_origin {
            let file: String = store
                .db
                .query_row(
                    "SELECT id FROM files WHERE location='media' AND path='movie.mkv'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let typed = TypedAuthority {
                physical_files: [("default".into(), [file.clone()].into())].into(),
                ..Default::default()
            };
            Some((
                capture_existing_occurrence(&store, "motion-file:source", "default", &file, &typed)
                    .unwrap(),
                typed,
            ))
        } else {
            None
        };
        let document = json!({"values":{"title":"Imported NFO"},"tags":[],"excluded_tags":[]});
        let typed = TypedAuthority {
            metadata: vec![
                initialization(),
                grant(MetadataAccess::SourceV2 {
                    namespace: NFO.into(),
                    record_id: RECORD.into(),
                    kind: SourceKind::Provider,
                    system_fields: BTreeSet::new(),
                }),
            ],
            ..Default::default()
        };
        let (manifest, batches, activation, mapping) = activate_40(
            &store,
            "metadata-part",
            vec![ImportBatch {
                items: vec![ImportItem {
                    source_id: SUBJECT.into(),
                    item_id: SUBJECT.into(),
                    kind: "video".into(),
                    legacy_metadata: json!({"title":"Original"}),
                }],
                unresolved_evidence: vec![],
                typed: Some(TypedBatch {
                    metadata: vec![ImportedMetadata {
                        contract_version: 2,
                        system_fields: vec![],
                        item_id: SUBJECT.into(),
                        policies: json!([{"field":"title","value_type":"string","nullable":false,"normalization":"exact"}]),
                        contributions: vec![ImportedContribution {
                            namespace: NFO.into(),
                            record_id: RECORD.into(),
                            source_revision: 9,
                            kind: ImportedSourceKind::Provider,
                            fields: document["values"].clone(),
                            evidence: json!({"source_table":"metadata_documents","source":"nfo","external_id":"tmdb:42","document_json":document.to_string(),"document":document}),
                        }],
                    }],
                    ..Default::default()
                }),
            }],
            typed,
        );
        let audit_digest=catabolic_core::sha256(json!({"domain":"candidate.complete-origin-archive.v1","subject":SUBJECT,"metadata_activation":activation.digest().as_str(),"origin":occurrence.as_ref().map(|(physical,_)|physical),"sidecar_rows":if with_origin {vec!["old-motion-file"]} else {vec![]}}).to_string());
        let mut grant = NfoImportAuthority {
            host_namespace: HOST.into(),
            root_manifest_digest: None,
            import_id: manifest.import_id.clone(),
            input_digest: catalog_import::manifest_digest(&manifest).unwrap(),
            source_namespace: manifest.source_namespace.clone(),
            reference_ack_id: "audit-metadata-part".into(),
            mapping_digest: mapping,
            origin_audit_digest: audit_digest,
            no_origin_subjects: if with_origin {
                BTreeSet::new()
            } else {
                [SUBJECT.into()].into()
            },
        };
        let mut physical = occurrence.map(|(physical, typed)| {
            let (manifest, batches, activation, mapping) = activate_40(
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
            let origin_grant = super::NfoImportedOriginAuthority {
                host_namespace: HOST.into(),
                root_manifest_digest: grant.root_manifest_digest.clone(),
                metadata_import_id: grant.import_id.clone(),
                metadata_input_digest: grant.input_digest.clone(),
                physical_import_id: manifest.import_id.clone(),
                physical_input_digest: catalog_import::manifest_digest(&manifest).unwrap(),
                physical_mapping_digest: mapping,
                origin_audit_digest: grant.origin_audit_digest.clone(),
                subject: SUBJECT.into(),
                motion_file_id: "old-motion-file".into(),
                physical,
            };
            PhysicalArchive {
                manifest,
                batches,
                activation,
                grant: origin_grant,
            }
        });
        let root_digest=catabolic_core::sha256(json!({"domain":"candidate.complete-activated-root.v1","metadata_manifest":manifest,"metadata_batches":batches,"metadata_activation":activation.digest().as_str(),"physical_manifest":physical.as_ref().map(|p|&p.manifest),"physical_batches":physical.as_ref().map(|p|&p.batches),"physical_activation":physical.as_ref().map(|p|p.activation.digest().as_str())}).to_string());
        grant.root_manifest_digest = Some(root_digest.clone());
        if let Some(physical) = &mut physical {
            physical.grant.root_manifest_digest = Some(root_digest);
        }
        Self {
            _root: root,
            store,
            baseline: Baseline::Imported(Imported {
                manifest,
                batches,
                activation,
                grant,
                physical,
            }),
        }
    }
    pub(super) fn retire_40(&self) {
        use crate::catalog_logical_edits::{
            self as logical, LogicalChange, LogicalEditAuthority, LogicalEditRequest,
        };
        let grant = LogicalEditAuthority {
            host_namespace: HOST.into(),
            item_ids: [SUBJECT.into(), OTHER.into()].into(),
            new_item_ids: BTreeSet::new(),
        };
        let preview = logical::preview_logical_edit(
            &self.store,
            LogicalEditRequest {
                contract_version: 1,
                change: LogicalChange::Merge {
                    sources: vec![SUBJECT.into()],
                    survivor: OTHER.into(),
                },
                new_items: vec![],
                association_clones: vec![],
                contexts: vec![],
            },
            &grant,
            None,
        )
        .unwrap();
        let command = envelope(
            &self.store,
            "retire-before-bootstrap",
            json!({"operation":"logical_edit","reviewed":preview.reviewed}),
        );
        committed(
            commands::execute_with_authority(
                &self.store,
                &command,
                CatalogAuthority::new(authority(true)).with_logical_edits(&grant),
                policy(),
            )
            .unwrap(),
        );
    }
}
fn pin_grant(item: &str) -> PinAuthority {
    PinAuthority {
        host_namespace: HOST.into(),
        readable: Default::default(),
        mutable: [("nfo".into(), [item.into()].into())].into(),
    }
}
pub(super) fn pin(
    store: &Store,
    id: &str,
    item: &str,
    expected: i64,
    decision: PinDecision,
) -> commands::Result<CommandOutcome> {
    let grant = pin_grant(item);
    let batch = PinBatch {
        contract_version: 1,
        changes: vec![PinChange {
            item_id: item.into(),
            namespace: "nfo".into(),
            expected_revision: expected,
            decision,
        }],
    };
    let preview = catalog_pins::preview_pins(store, &batch, &grant).unwrap();
    let mutation = PinMutation {
        batch,
        preview_digest: preview.digest,
    };
    let command = envelope(store, id, json!({"operation":"pins","mutation":mutation}));
    if store.schema_version == 40 {
        commands::execute_with_authority(
            store,
            &command,
            CatalogAuthority::new(authority(true)).with_pins(&grant),
            policy(),
        )
    } else {
        commands::execute_candidate_for_test(
            store,
            &command,
            authority(true),
            policy(),
            |store, command, at| {
                let result = catalog_pins::apply_pins(store, &mutation, command, &grant, at)
                    .map_err(|e| Error(e.to_string()))?;
                Ok(serde_json::to_value(result)?)
            },
        )
    }
}

#[test]
fn automatic_identity_bootstrap_refuses_existing_other_manual_pin_without_effects() {
    let f = Fixture::imported(false);
    committed(
        pin(
            &f.store,
            "other-pin",
            OTHER,
            0,
            PinDecision::Set {
                value: "tmdb:42".into(),
            },
        )
        .unwrap(),
    );
    let f = f.install();
    let before = migration::snapshot(&f.store.db).unwrap();
    let result = f.store.transaction_detailed::<_, Error>(|_| {
        let source = f.proof_in()?;
        bootstrap::prepare_in(&f.store, &[source])?;
        Ok(())
    });
    assert!(result.unwrap_err().0.contains("manual Pins"));
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
}
#[test]
fn current_automatic_claim_blocks_other_pin_but_preserves_same_subject_decisions() {
    let f = Fixture::imported(false).install();
    f.bootstrap();
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(
        pin(
            &f.store,
            "other-pin-denied",
            OTHER,
            0,
            PinDecision::Set {
                value: "tmdb:42".into()
            }
        )
        .is_err()
    );
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    committed(
        pin(
            &f.store,
            "own-different-pin",
            SUBJECT,
            0,
            PinDecision::Set {
                value: "manual:choice".into(),
            },
        )
        .unwrap(),
    );
    let request = put_request(&f.store, 1, json!({"title":"Changed"}), &[]);
    let command = put_command(&f.store, "put-retains-identity", 1, &request);
    committed(execute_put(&f.store, &command, &request, 1));
    let current = f.state();
    assert_eq!(
        current
            .presence()
            .head
            .attribution
            .as_ref()
            .unwrap()
            .active_identity
            .as_deref(),
        Some("tmdb:42")
    );
    assert!(
        matches!(&current.presence().head.pin,catabolic_core::catalog_metadata::nfo_refresh::PinHead::Decision {revision:1,value:Some(value)} if value=="manual:choice")
    );
    committed(pin(&f.store, "own-pin-remove", SUBJECT, 1, PinDecision::Remove).unwrap());
    assert!(matches!(
        f.state().presence().head.pin,
        catabolic_core::catalog_metadata::nfo_refresh::PinHead::Decision {
            revision: 2,
            value: None
        }
    ));
}
#[test]
fn actual_physical_origin_is_retained_exactly_through_put_and_restart() {
    let f = Fixture::imported(true).install();
    f.bootstrap();
    let original = f.state();
    let physical = original.origin_physical().unwrap().clone();
    let proof = original.origin_proof_digest().unwrap().to_owned();
    let raw:(String,String)=f.store.db.query_row("SELECT origin_json,origin_physical_json FROM catalog_nfo_state_revisions WHERE item_id=? AND revision=1",[SUBJECT],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let request = put_request(&f.store, 1, json!({"title":"Changed"}), &["origin-tag"]);
    let command = put_command(&f.store, "origin-put", 1, &request);
    committed(execute_put(&f.store, &command, &request, 1));
    let next_raw:(String,String)=f.store.db.query_row("SELECT origin_json,origin_physical_json FROM catalog_nfo_state_revisions WHERE item_id=? AND revision=2",[SUBJECT],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(raw, next_raw);
    drop(original);
    let f = f.reopen();
    let current = f.state();
    assert_eq!(current.origin_physical(), Some(&physical));
    assert_eq!(current.origin_proof_digest(), Some(proof.as_str()));
    assert_eq!(
        current
            .presence()
            .head
            .attribution
            .as_ref()
            .unwrap()
            .active_identity
            .as_deref(),
        Some("tmdb:42")
    );
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT state_revision FROM catalog_nfo_file_attributions WHERE item_id=?",
                [SUBJECT],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
}
#[test]
fn retired_import_keeps_origin_document_and_manual_pin_but_no_automatic_claim() {
    let f = Fixture::imported(true);
    committed(
        pin(
            &f.store,
            "retained-manual",
            SUBJECT,
            0,
            PinDecision::Set {
                value: "manual:retained".into(),
            },
        )
        .unwrap(),
    );
    f.retire_40();
    let f = f.install();
    f.bootstrap();
    let state = f.state();
    assert!(matches!(
        state.presence().head.subject_state,
        SubjectState::Retired { .. }
    ));
    assert_eq!(
        state
            .presence()
            .head
            .attribution
            .as_ref()
            .unwrap()
            .active_identity,
        None
    );
    assert!(
        state
            .presence()
            .head
            .attribution
            .as_ref()
            .unwrap()
            .origin
            .is_some()
    );
    assert!(state.origin_physical().is_some());
    let DocumentState::Present {
        external_identity, ..
    } = &state.presence().head.source.as_ref().unwrap().document
    else {
        panic!("retained document missing")
    };
    assert_eq!(external_identity.as_deref(), Some("tmdb:42"));
    assert!(
        matches!(&state.presence().head.pin,catabolic_core::catalog_metadata::nfo_refresh::PinHead::Decision {revision:1,value:Some(value)} if value=="manual:retained")
    );
    let before = migration::snapshot(&f.store.db).unwrap();
    let request = put_request(&f.store, 1, json!({"title":"must not change"}), &[]);
    let command = put_command(&f.store, "retired-denied", 1, &request);
    assert!(
        commands::execute_candidate_for_test(
            &f.store,
            &command,
            authority(true),
            policy(),
            |store, command, at| apply_put(store, command, &request, 1, at)
        )
        .is_err()
    );
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    committed(
        pin(
            &f.store,
            "former-automatic-now-free",
            OTHER,
            0,
            PinDecision::Set {
                value: "tmdb:42".into(),
            },
        )
        .unwrap(),
    );
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT count(*) FROM catalog_nfo_identity_claims WHERE item_id=?",
                [SUBJECT],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

mod first_source;

mod retirement;

mod bootstrap_entry;

/// Original40-only lexical adapter. Existing authenticated archives remain owned
/// here while the consumer drops Store and uses the sealed migration owner.
/// This never installs candidate41 or exposes archive fields as mutable grants.
pub(crate) fn with_imported40_bootstrap_fixture<T>(
    use_fixture: impl FnOnce(
        tempfile::TempDir,
        Store,
        &'static str,
        &[super::bootstrap_entry::SourceEvidence<'_>],
    ) -> T,
) -> T {
    use super::bootstrap_entry::{
        BaselineEvidence, ImportOriginEvidence, ImportPart, SourceEvidence,
    };
    let Fixture {
        _root,
        store,
        baseline,
    } = Fixture::imported(true);
    let Baseline::Imported(archive) = baseline else {
        unreachable!()
    };
    let physical = archive
        .physical
        .as_ref()
        .expect("physical original40 archive");
    let evidence = [SourceEvidence {
        baseline: BaselineEvidence::Imported {
            subject: SUBJECT,
            metadata: ImportPart {
                manifest: &archive.manifest,
                batches: &archive.batches,
                activation: &archive.activation,
            },
            audit: &archive.grant,
            origin: ImportOriginEvidence::Physical {
                part: ImportPart {
                    manifest: &physical.manifest,
                    batches: &physical.batches,
                    activation: &physical.activation,
                },
                audit: &physical.grant,
            },
        },
        later_commands: &[],
    }];
    use_fixture(_root, store, HOST, &evidence)
}
