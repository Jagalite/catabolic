//! Actual typed-owner fixtures. No history/receipt rows or proof tokens are
//! fabricated, and no immutable trigger is disabled by these tests.
use super::super::{VerifiedNfoBaseline, verify_source_history_in};
use super::{NfoCreatedSourceAuthority, verify_created_no_origin_baseline_in};
use crate::{
    Error, Result, Store,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
        TrustedReceiptMaintenance,
    },
    catalog_metadata_store::{
        self as metadata, ContributionReplace, MetadataAccess, MetadataAuthority, MetadataChange,
        MetadataInitializeV2, MetadataMutation, PolicyField, PolicyNormalization, PolicyValueType,
    },
    catalog_tag_mutation::{TagAuthority, TagSubject},
    migration,
};
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility},
    catalog_metadata::SourceKind,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Duration};

const HOST: &str = "nfo-created-proof";
const SUBJECT: &str = "subject";
const NFO: &str = "motion.nfo";
const RECORD: &str = "document";

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
        retention: Duration::ZERO,
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
fn envelope(store: &Store, id: &str, payload: Value) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, HOST, id).unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        generation(store),
        &payload.to_string(),
    )
    .unwrap()
}
fn grant(access: MetadataAccess) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access,
    }
}
fn evidence() -> Value {
    json!({"owner":"motion.metadata.replace","request_digest":"a".repeat(64),"expected_aggregate_revision":7})
}

#[derive(Clone, Copy)]
enum Cohort {
    Empty,
    Legacy,
    V1,
    V2,
    V2NoExtension,
    CreatedOrigin,
}

fn replace(
    store: &Store,
    id: &str,
    namespace: &str,
    revision: i64,
    kind: SourceKind,
    fields: Value,
    evidence: Value,
    creation: Option<&CommandEnvelope>,
    systems: BTreeSet<String>,
) -> CommandEnvelope {
    let origin = creation.map(|command| {
        metadata::verify_created_origin(store, command, SUBJECT, authority(false)).unwrap()
    });
    let request = ContributionReplace {
        contract_version: 2,
        subject: SUBJECT.into(),
        namespace: namespace.into(),
        record_id: RECORD.into(),
        expected_schema_fingerprint: metadata::current_schema_fingerprint(&store.db, SUBJECT)
            .unwrap(),
        expected_revision: revision,
        fields_json: fields.to_string(),
        evidence_json: evidence.to_string(),
        tags: vec![],
        excluded_tags: vec![],
        origin_creation_intent: creation.map(|command| command.canonical_payload().into()),
    };
    let command = envelope(
        store,
        id,
        json!({"operation":"metadata_contribution_replace","request":request}),
    );
    let source = grant(MetadataAccess::Source {
        namespace: namespace.into(),
        record_id: RECORD.into(),
        kind,
    });
    let initialize = grant(MetadataAccess::InitializeV2 {
        system_fields: systems,
    });
    let extension = grant(MetadataAccess::ExtendSchema);
    let tags = TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [TagSubject::Item(SUBJECT.into())].into(),
        allow_global_merge: false,
    };
    let mut trusted = CatalogAuthority::new(authority(true))
        .with_metadata(&source)
        .with_metadata_initialization(&initialize)
        .with_metadata_schema(&extension)
        .with_tags(&tags);
    if let Some(origin) = &origin {
        trusted = trusted.with_metadata_origin(origin);
    }
    assert!(matches!(
        commands::execute_with_authority(store, &command, trusted, policy()).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    command
}

struct Fixture {
    _root: tempfile::TempDir,
    store: Store,
    first: CommandEnvelope,
    creation: Option<CommandEnvelope>,
}
impl Fixture {
    fn new(cohort: Cohort) -> Self {
        Self::with_evidence(cohort, SourceKind::Provider, evidence())
    }
    fn with_evidence(cohort: Cohort, kind: SourceKind, supplied_evidence: Value) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("created.db");
        migration::initialize_native_at(&path, 40).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let original = match cohort {
            Cohort::CreatedOrigin => {
                json!({"title":"Original","motion_catalog":{"contract_version":1,"item_id":SUBJECT,"kind":"video"}})
            }
            Cohort::Legacy => json!({"retained":{"seed":1}}),
            _ => json!({}),
        };
        let created = matches!(cohort, Cohort::CreatedOrigin);
        let seed = envelope(
            &store,
            "create",
            if created {
                json!({"operation":"item_create_batch","items":[{"id":SUBJECT,"kind":"video","metadata":original}]})
            } else {
                json!({"operation":"item_put","item":{"id":SUBJECT,"kind":"video","metadata":original}})
            },
        );
        assert!(matches!(
            commands::execute(&store, &seed, authority(true), policy()).unwrap(),
            CommandOutcome::Committed { .. }
        ));
        if matches!(cohort, Cohort::V1 | Cohort::V2 | Cohort::V2NoExtension) {
            let mut policies = vec![PolicyField {
                field: "title".into(),
                value_type: PolicyValueType::String,
                nullable: false,
                normalization: PolicyNormalization::TrimAsciiLowercase,
            }];
            if matches!(cohort, Cohort::V2NoExtension) {
                policies.push(PolicyField {
                    field: "extra".into(),
                    value_type: PolicyValueType::Json,
                    nullable: false,
                    normalization: PolicyNormalization::Exact,
                });
            }
            let (payload, access) = if matches!(cohort, Cohort::V1) {
                (
                    json!({"operation":"metadata","mutation":MetadataMutation { contract_version:1, change:MetadataChange::Initialize { subject:SUBJECT.into(), policies } }}),
                    MetadataAccess::Initialize,
                )
            } else {
                (
                    json!({"operation":"metadata_initialize_v2","request":MetadataInitializeV2 { contract_version:2, subject:SUBJECT.into(), policies, system_fields:BTreeSet::new() }}),
                    MetadataAccess::InitializeV2 {
                        system_fields: BTreeSet::new(),
                    },
                )
            };
            let initialize = envelope(&store, "initialize", payload);
            let initialization = grant(access);
            assert!(matches!(
                commands::execute_with_authority(
                    &store,
                    &initialize,
                    CatalogAuthority::new(authority(true))
                        .with_metadata(&initialization)
                        .with_metadata_initialization(&initialization),
                    policy()
                )
                .unwrap(),
                CommandOutcome::Committed { .. }
            ));
        }
        let creation = created.then_some(seed);
        let systems = if created {
            ["motion_catalog".into()].into()
        } else {
            BTreeSet::new()
        };
        let first = replace(
            &store,
            "first-nfo",
            NFO,
            0,
            kind,
            json!({"title":"  NFO Title  ","extra":7}),
            supplied_evidence,
            creation.as_ref(),
            systems,
        );
        Self {
            _root: root,
            store,
            first,
            creation,
        }
    }
    fn absence(&self) -> NfoCreatedSourceAuthority {
        NfoCreatedSourceAuthority {
            host_namespace: HOST.into(),
            subject: SUBJECT.into(),
            request_digest: "a".repeat(64),
            complete_absence_audit_digest: "b".repeat(64),
        }
    }
    fn verify(
        &self,
        first: &CommandEnvelope,
        grant: &NfoCreatedSourceAuthority,
    ) -> Result<VerifiedNfoBaseline> {
        self.store.transaction_detailed(|_| {
            verify_created_no_origin_baseline_in(
                &self.store,
                first,
                grant,
                authority(false),
                generation(&self.store),
            )
        })
    }
    fn baseline(&self) -> VerifiedNfoBaseline {
        self.verify(&self.first, &self.absence()).unwrap()
    }
    fn compact(&self) {
        assert!(
            commands::compact_receipts(
                &self.store,
                TrustedReceiptMaintenance {
                    host_namespace: HOST,
                    holder_namespace: "nfo-created-maintenance"
                },
                100
            )
            .unwrap()
                > 0
        );
    }
    fn assert_history(
        &self,
        baseline: &VerifiedNfoBaseline,
        later: &[CommandEnvelope],
        expected_revision: i64,
    ) {
        let before = generation(&self.store);
        self.store
            .transaction_detailed(|_| {
                let source = verify_source_history_in(
                    &self.store,
                    baseline,
                    later,
                    authority(false),
                    generation(&self.store),
                )?;
                assert_eq!(source.current_source().revision(), expected_revision);
                assert_eq!(source.automatic_identity(), None);
                assert_eq!(source.origin(), None);
                assert_eq!(source.origin_physical(), None);
                assert_eq!(source.origin_proof_digest(), None);
                Ok::<_, Error>(())
            })
            .unwrap();
        assert_eq!(generation(&self.store), before);
    }
}

#[test]
fn initialized_v1_and_v2_first_sources_reconstruct_normalization_and_extension() {
    for cohort in [Cohort::V1, Cohort::V2, Cohort::V2NoExtension] {
        let f = Fixture::new(cohort);
        let baseline = f.baseline();
        let fields: Value = serde_json::from_str(&baseline.fields_json).unwrap();
        assert_eq!(fields, json!({"title":"nfo title","extra":7}));
        assert!(f.first.payload()["request"]["expected_schema_fingerprint"].is_string());
        assert_eq!(
            f.first.payload()["request"]["expected_schema_fingerprint"].as_str()
                == Some(baseline.schema_fingerprint.as_str()),
            matches!(cohort, Cohort::V2NoExtension),
        );
        assert_eq!(baseline.revision, 1);
        assert_eq!(baseline.source_kind, SourceKind::Provider);
        assert_eq!(baseline.command, f.first);
        assert_eq!(
            serde_json::from_str::<Value>(&baseline.evidence_json).unwrap(),
            evidence()
        );
        f.assert_history(&baseline, &[], 1);
    }
}

#[test]
fn first_initialization_reconstructs_empty_and_retained_legacy_roots() {
    for cohort in [Cohort::Empty, Cohort::Legacy] {
        let f = Fixture::new(cohort);
        assert!(f.first.payload()["request"]["expected_schema_fingerprint"].is_null());
        let baseline = f.baseline();
        assert_eq!(
            serde_json::from_str::<Value>(&baseline.fields_json).unwrap(),
            json!({"title":"  NFO Title  ","extra":7})
        );
        let original: String = f
            .store
            .db
            .query_row(
                "SELECT legacy_raw_json FROM catalog_metadata_subjects WHERE item_id=?",
                [SUBJECT],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&original).unwrap(),
            if matches!(cohort, Cohort::Legacy) {
                json!({"retained":{"seed":1}})
            } else {
                json!({})
            }
        );
        f.assert_history(&baseline, &[], 1);
    }
}

#[test]
fn exact_title_origin_side_effects_do_not_become_nfo_file_attribution() {
    let f = Fixture::new(Cohort::CreatedOrigin);
    let baseline = f.baseline();
    assert_eq!(
        f.first.payload()["request"]["origin_creation_intent"],
        f.creation.as_ref().unwrap().canonical_payload()
    );
    let (revision, raw): (i64,String) = f.store.db.query_row("SELECT revision,fields_json FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.scan' AND record_id='origin'", [SUBJECT], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(revision, 1);
    assert_eq!(
        serde_json::from_str::<Value>(&raw).unwrap(),
        json!({"title":"Original"})
    );
    let legacy: String = f.store.db.query_row("SELECT fields_json FROM catalog_metadata_sources WHERE item_id=? AND namespace=? AND record_id=?", [SUBJECT,metadata::LEGACY_NAMESPACE,metadata::LEGACY_RECORD_ID], |r|r.get(0)).unwrap();
    let legacy: Value = serde_json::from_str(&legacy).unwrap();
    assert!(legacy.get("title").is_none());
    assert!(legacy.get("motion_catalog").is_some());
    f.assert_history(&baseline, &[], 1);
}

#[test]
fn receipt_compaction_does_not_destroy_baseline_proof_or_revive_replay() {
    for cohort in [
        Cohort::V1,
        Cohort::V2,
        Cohort::V2NoExtension,
        Cohort::Empty,
        Cohort::Legacy,
        Cohort::CreatedOrigin,
    ] {
        let f = Fixture::new(cohort);
        let before = f.baseline();
        let generation = generation(&f.store);
        f.compact();
        let after = f.baseline();
        assert_eq!(before.fields_json, after.fields_json);
        assert_eq!(before.evidence_json, after.evidence_json);
        assert_eq!(before.raw_mutation_json, after.raw_mutation_json);
        assert_eq!(before.schema_fingerprint, after.schema_fingerprint);
        f.assert_history(&after, &[], 1);
        assert!(matches!(
            commands::reconcile_receipt(&f.store, &f.first, authority(false)).unwrap(),
            Some(CommandOutcome::ReceiptExpired)
        ));
        if let Some(creation) = &f.creation {
            assert!(matches!(
                commands::reconcile_receipt(&f.store, creation, authority(false)).unwrap(),
                Some(CommandOutcome::ReceiptExpired)
            ));
        }
        assert_eq!(generation, self::generation(&f.store));
    }
}

#[test]
fn independent_absence_audit_and_exact_request_binding_are_required() {
    let f = Fixture::new(Cohort::V2);
    for field in 0..5 {
        let mut grant = f.absence();
        match field {
            0 => grant.host_namespace = "wrong-host".into(),
            1 => grant.subject = "different-subject".into(),
            2 => grant.request_digest = "c".repeat(64),
            3 => grant.complete_absence_audit_digest.clear(),
            _ => grant.complete_absence_audit_digest = "not-a-digest".into(),
        }
        assert!(f.verify(&f.first, &grant).is_err());
    }
    f.baseline();
}

#[test]
fn a_real_receipt_does_not_prove_altered_raw_fields_or_origin_intent() {
    let f = Fixture::new(Cohort::CreatedOrigin);
    for field in ["fields_json", "evidence_json", "origin_creation_intent"] {
        let mut changed = f.first.payload().clone();
        changed["request"][field] = match field {
            "fields_json" => json!("{\"title\":\"forged\"}"),
            "evidence_json" => json!("{}"),
            _ => Value::Null,
        };
        let tampered = CommandEnvelope::new(
            1,
            f.first.key().clone(),
            f.first.admission_incarnation(),
            f.first.expected_generation(),
            &changed.to_string(),
        )
        .unwrap();
        assert!(f.verify(&tampered, &f.absence()).is_err());
    }
    let missing = CommandEnvelope::new(
        1,
        CommandKey::new(&f.store.database_id, HOST, "missing-command").unwrap(),
        f.first.admission_incarnation(),
        f.first.expected_generation(),
        &f.first.payload().to_string(),
    )
    .unwrap();
    assert!(f.verify(&missing, &f.absence()).is_err());
    f.assert_history(&f.baseline(), &[], 1);
}

#[test]
fn legitimate_later_and_other_source_commands_cannot_replace_revision_one_proof() {
    let f = Fixture::new(Cohort::V2);
    let first = f.baseline();
    let other = replace(
        &f.store,
        "other-source",
        "motion.other",
        0,
        SourceKind::Provider,
        json!({"another":true}),
        evidence(),
        None,
        BTreeSet::new(),
    );
    assert!(f.verify(&other, &f.absence()).is_err());
    let later = replace(
        &f.store,
        "second-nfo",
        NFO,
        1,
        SourceKind::Provider,
        json!({"title":" Later ","later":true}),
        evidence(),
        None,
        BTreeSet::new(),
    );
    assert!(f.verify(&later, &f.absence()).is_err());
    let retained = f.baseline();
    assert_eq!(retained.fields_json, first.fields_json);
    assert_eq!(retained.schema_fingerprint, first.schema_fingerprint);
    f.assert_history(&retained, &[later], 2);
}

#[test]
fn committed_unrecognized_evidence_or_nonprovider_kind_does_not_mint_a_baseline() {
    let mut malformed = Vec::new();
    for key in ["owner", "request_digest", "expected_aggregate_revision"] {
        let mut value = evidence();
        value.as_object_mut().unwrap().remove(key);
        malformed.push(value);
    }
    let mut wrong_owner = evidence();
    wrong_owner["owner"] = json!("another-owner");
    malformed.push(wrong_owner);
    let mut extra = evidence();
    extra["external_id"] = json!("unproven:identity");
    malformed.push(extra);
    let mut wrong_revision = evidence();
    wrong_revision["expected_aggregate_revision"] = json!("7");
    malformed.push(wrong_revision);
    for supplied in malformed {
        let f = Fixture::with_evidence(Cohort::V2, SourceKind::Provider, supplied);
        assert!(f.verify(&f.first, &f.absence()).is_err());
    }
    let f = Fixture::with_evidence(Cohort::V2, SourceKind::Local, evidence());
    assert!(f.verify(&f.first, &f.absence()).is_err());
}

#[test]
fn current_retained_writer_full_result_scope_and_generation_are_required() {
    let f = Fixture::new(Cohort::V2);
    assert!(
        verify_created_no_origin_baseline_in(
            &f.store,
            &f.first,
            &f.absence(),
            authority(false),
            generation(&f.store)
        )
        .is_err()
    );
    for denied in [
        authority(true),
        TrustedCommandAuthority {
            host_namespace: "wrong-host",
            ..authority(false)
        },
        TrustedCommandAuthority {
            receipt_visibility: ReceiptVisibility::CommittedStatusOnly,
            ..authority(false)
        },
    ] {
        assert!(
            f.store
                .transaction_detailed::<_, Error>(|_| verify_created_no_origin_baseline_in(
                    &f.store,
                    &f.first,
                    &f.absence(),
                    denied,
                    generation(&f.store)
                ))
                .is_err()
        );
    }
    assert!(
        f.store
            .transaction_detailed::<_, Error>(|_| verify_created_no_origin_baseline_in(
                &f.store,
                &f.first,
                &f.absence(),
                authority(false),
                generation(&f.store) + 1
            ))
            .is_err()
    );
    f.assert_history(&f.baseline(), &[], 1);
}
