use super::*;
use crate::{
    Error,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy,
        TrustedReceiptMaintenance,
    },
    catalog_metadata_store::{
        self as metadata, MetadataAccess, MetadataAuthority, PolicyField, PolicyNormalization,
        PolicyValueType,
    },
    catalog_nfo_attribution::verify_source_history_in,
    migration,
};
use catabolic_core::catalog_command::{CommandKey, ReceiptVisibility};
use serde_json::{Value, json};
use std::time::Duration;

const HOST: &str = "nfo-v1-created";
const SUBJECT: &str = "subject";
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
fn evidence() -> Value {
    json!({"owner":"motion.metadata.replace","request_digest":"a".repeat(64),"expected_aggregate_revision":11})
}
fn source_grant(kind: SourceKind) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::Source {
            namespace: "motion.nfo".into(),
            record_id: "document".into(),
            kind,
        },
    }
}
fn patch(
    store: &Store,
    id: &str,
    revision: i64,
    kind: SourceKind,
    edits: Value,
    evidence: Value,
) -> CommandEnvelope {
    let mutation = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Patch {
            subject: SUBJECT.into(),
            namespace: "motion.nfo".into(),
            record_id: "document".into(),
            schema_fingerprint: metadata::current_schema_fingerprint(&store.db, SUBJECT)
                .unwrap()
                .unwrap(),
            expected_revision: revision,
            edits_json: edits.to_string(),
            evidence_json: evidence.to_string(),
        },
    };
    let command = envelope(
        store,
        id,
        json!({"operation":"metadata","mutation":mutation}),
    );
    assert!(matches!(
        commands::execute_with_authority(
            store,
            &command,
            CatalogAuthority::new(authority(true)).with_metadata(&source_grant(kind)),
            policy()
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    command
}

struct Fixture {
    _root: tempfile::TempDir,
    store: Store,
    first: CommandEnvelope,
    later: Vec<CommandEnvelope>,
}
impl Fixture {
    fn new(later: bool) -> Self {
        Self::custom(
            json!({"title":{"op":"set","value":"  NFO TITLE  "},"plot":{"op":"set","value":[1,2]}}),
            evidence(),
            SourceKind::Provider,
            later,
        )
    }
    fn custom(edits: Value, supplied_evidence: Value, kind: SourceKind, later: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native39.db");
        migration::initialize_native_at(&path, 39).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let seed = envelope(
            &store,
            "seed",
            json!({"operation":"item_put","item":{"id":SUBJECT,"kind":"video","metadata":{}}}),
        );
        commands::execute(&store, &seed, authority(true), policy()).unwrap();
        let policies = vec![
            PolicyField {
                field: "title".into(),
                value_type: PolicyValueType::String,
                nullable: false,
                normalization: PolicyNormalization::TrimAsciiLowercase,
            },
            PolicyField {
                field: "plot".into(),
                value_type: PolicyValueType::Json,
                nullable: false,
                normalization: PolicyNormalization::Exact,
            },
            PolicyField {
                field: "motion_private".into(),
                value_type: PolicyValueType::Json,
                nullable: false,
                normalization: PolicyNormalization::Exact,
            },
        ];
        let initialize = envelope(
            &store,
            "initialize",
            json!({"operation":"metadata","mutation":MetadataMutation { contract_version:1,change:MetadataChange::Initialize { subject:SUBJECT.into(),policies } }}),
        );
        let initialization = MetadataAuthority {
            host_namespace: HOST.into(),
            subject: SUBJECT.into(),
            access: MetadataAccess::Initialize,
        };
        commands::execute_with_authority(
            &store,
            &initialize,
            CatalogAuthority::new(authority(true)).with_metadata(&initialization),
            policy(),
        )
        .unwrap();
        let first = patch(&store, "first", 0, kind, edits, supplied_evidence);
        let later = if later {
            vec![patch(
                &store,
                "second",
                1,
                kind,
                json!({"title":{"op":"set","value":" Later "},"plot":{"op":"remove"}}),
                evidence(),
            )]
        } else {
            vec![]
        };
        let before = source_rows(&store);
        let old_generation = generation(&store);
        let old_incarnation = store.catalog_incarnation.clone();
        drop(store);
        // Real migration owner, with no SQL metadata writes or trigger bypass.
        migration::upgrade_native(
            &path,
            &migration::plan_native(&path).unwrap(),
            migration::NativeUpgradeQuiescence {
                admission_stopped: true,
                readers_and_workers_stopped: true,
                exclusive_operational_ownership: true,
            },
            None,
            &|_, _, _| Ok(()),
        )
        .unwrap();
        let store = Store::open(&path, true, false).unwrap();
        assert_eq!(store.schema_version, 40);
        assert_eq!(store.catalog_incarnation, old_incarnation);
        assert_eq!(generation(&store), old_generation);
        assert_eq!(source_rows(&store), before);
        Self {
            _root: root,
            store,
            first,
            later,
        }
    }
    fn grant(&self) -> NfoCreatedSourceAuthority {
        NfoCreatedSourceAuthority {
            host_namespace: HOST.into(),
            subject: SUBJECT.into(),
            request_digest: "a".repeat(64),
            complete_absence_audit_digest: "b".repeat(64),
        }
    }
    fn verify(
        &self,
        command: &CommandEnvelope,
        grant: &NfoCreatedSourceAuthority,
    ) -> Result<VerifiedNfoBaseline> {
        self.store.transaction_detailed(|_| {
            verify_first_patch_in(
                &self.store,
                command,
                grant,
                authority(false),
                generation(&self.store),
            )
        })
    }
    fn baseline(&self) -> VerifiedNfoBaseline {
        self.verify(&self.first, &self.grant()).unwrap()
    }
    fn assert_history(&self, baseline: &VerifiedNfoBaseline) {
        self.store
            .transaction_detailed(|_| {
                let source = verify_source_history_in(
                    &self.store,
                    baseline,
                    &self.later,
                    authority(false),
                    generation(&self.store),
                )?;
                assert_eq!(
                    source.current_source().revision(),
                    1 + self.later.len() as i64
                );
                assert_eq!(source.automatic_identity(), None);
                assert_eq!(source.origin(), None);
                assert_eq!(
                    source.attribution_audit_digest(),
                    self.grant().complete_absence_audit_digest
                );
                Ok::<_, Error>(())
            })
            .unwrap();
    }
}
fn source_rows(store: &Store) -> Vec<(i64, String, String, String, String)> {
    store.db.prepare("SELECT revision,schema_fingerprint,fields_json,evidence_json,raw_mutation_json FROM catalog_metadata_history WHERE item_id=? AND namespace='motion.nfo' AND record_id='document' ORDER BY revision").unwrap()
        .query_map([SUBJECT],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap()
        .collect::<std::result::Result<Vec<_>,_>>().unwrap()
}

#[test]
fn actual_native39_first_patch_survives_official_upgrade_with_exact_v1_bytes() {
    let f = Fixture::new(false);
    let before = source_rows(&f.store);
    let baseline = f.baseline();
    assert_eq!(baseline.revision, 1);
    assert_eq!(
        serde_json::from_str::<Value>(&baseline.fields_json).unwrap(),
        json!({"title":"nfo title","plot":[1,2]})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&baseline.evidence_json).unwrap(),
        evidence()
    );
    let mutation: MetadataMutation =
        serde_json::from_value(f.first.payload()["mutation"].clone()).unwrap();
    assert_eq!(
        baseline.raw_mutation_json,
        serde_json::to_string(&mutation).unwrap()
    );
    assert_ne!(baseline.raw_mutation_json, f.first.payload().to_string());
    assert_eq!(baseline.command, f.first);
    f.assert_history(&baseline);
    assert_eq!(source_rows(&f.store), before);
}

#[test]
fn later_native39_patch_and_expired_receipts_preserve_attribution_without_replay() {
    let f = Fixture::new(true);
    let baseline = f.baseline();
    f.assert_history(&baseline);
    assert!(f.verify(&f.later[0], &f.grant()).is_err());
    assert!(
        commands::compact_receipts(
            &f.store,
            TrustedReceiptMaintenance {
                host_namespace: HOST,
                holder_namespace: "nfo-v1-maintenance"
            },
            100
        )
        .unwrap()
            > 0
    );
    let recovered = f.baseline();
    assert_eq!(recovered.raw_mutation_json, baseline.raw_mutation_json);
    f.assert_history(&recovered);
    for command in std::iter::once(&f.first).chain(&f.later) {
        assert!(matches!(
            commands::reconcile_receipt(&f.store, command, authority(false)).unwrap(),
            Some(CommandOutcome::ReceiptExpired)
        ));
    }
}

#[test]
fn empty_first_public_document_remains_present_at_revision_one() {
    let f = Fixture::custom(json!({}), evidence(), SourceKind::Provider, false);
    let baseline = f.baseline();
    assert_eq!(baseline.fields_json, "{}");
    assert_eq!(baseline.revision, 1);
    f.assert_history(&baseline);
}

#[test]
fn generic_native_remove_reserved_and_nonprovider_first_patches_are_not_motion_creation() {
    for (edits, kind) in [
        (json!({"title":{"op":"remove"}}), SourceKind::Provider),
        (
            json!({"motion_private":{"op":"set","value":"not-public"}}),
            SourceKind::Provider,
        ),
        (
            json!({"title":{"op":"set","value":"local"}}),
            SourceKind::Local,
        ),
    ] {
        let f = Fixture::custom(edits, evidence(), kind, false);
        assert!(f.verify(&f.first, &f.grant()).is_err());
    }
}

#[test]
fn closed_evidence_and_independent_complete_absence_audit_are_not_optional() {
    for supplied in [
        json!({"unrecognized":"owner"}),
        json!({"owner":"motion.metadata.replace","request_digest":"a".repeat(64),"expected_aggregate_revision":11,"external_id":"not-a-proof"}),
    ] {
        let f = Fixture::custom(
            json!({"title":{"op":"set","value":"title"}}),
            supplied,
            SourceKind::Provider,
            false,
        );
        assert!(f.verify(&f.first, &f.grant()).is_err());
    }
    let f = Fixture::new(false);
    for field in 0..4 {
        let mut grant = f.grant();
        match field {
            0 => grant.host_namespace = "wrong".into(),
            1 => grant.subject = "wrong".into(),
            2 => grant.request_digest = "c".repeat(64),
            _ => grant.complete_absence_audit_digest.clear(),
        }
        assert!(f.verify(&f.first, &grant).is_err());
    }
    f.baseline();
}

#[test]
fn same_key_tampering_and_other_real_receipts_do_not_prove_first_patch_history() {
    let f = Fixture::new(false);
    for (key, value) in [
        ("edits_json", json!("{}")),
        ("schema_fingerprint", json!("c".repeat(64))),
        ("evidence_json", json!("{}")),
    ] {
        let mut changed = f.first.payload().clone();
        changed["mutation"]["change"][key] = value;
        let command = CommandEnvelope::new(
            1,
            f.first.key().clone(),
            f.first.admission_incarnation(),
            f.first.expected_generation(),
            &changed.to_string(),
        )
        .unwrap();
        assert!(f.verify(&command, &f.grant()).is_err());
    }
    let unrelated = envelope(
        &f.store,
        "unrelated",
        json!({"operation":"item_put","item":{"id":"other","kind":"video","metadata":{}}}),
    );
    commands::execute(&f.store, &unrelated, authority(true), policy()).unwrap();
    assert!(f.verify(&unrelated, &f.grant()).is_err());
    f.assert_history(&f.baseline());
}

#[test]
fn first_patch_proof_requires_current_writer_generation_and_full_receipt_scope() {
    let f = Fixture::new(false);
    assert!(
        verify_first_patch_in(
            &f.store,
            &f.first,
            &f.grant(),
            authority(false),
            generation(&f.store)
        )
        .is_err()
    );
    for denied in [
        authority(true),
        TrustedCommandAuthority {
            receipt_visibility: ReceiptVisibility::CommittedStatusOnly,
            ..authority(false)
        },
        TrustedCommandAuthority {
            host_namespace: "wrong",
            ..authority(false)
        },
    ] {
        assert!(
            f.store
                .transaction_detailed::<_, Error>(|_| verify_first_patch_in(
                    &f.store,
                    &f.first,
                    &f.grant(),
                    denied,
                    generation(&f.store)
                ))
                .is_err()
        );
    }
    assert!(
        f.store
            .transaction_detailed::<_, Error>(|_| verify_first_patch_in(
                &f.store,
                &f.first,
                &f.grant(),
                authority(false),
                generation(&f.store) + 1
            ))
            .is_err()
    );
}
