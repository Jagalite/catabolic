use super::*;
use crate::{
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy,
        TrustedReceiptMaintenance,
    },
    catalog_metadata_store::{
        self as metadata, MetadataAccess, MetadataAuthority, MetadataChange, MetadataMutation,
        PolicyField, PolicyNormalization, PolicyValueType,
    },
    catalog_tag_mutation::{TagAuthority, TagSubject},
    migration,
};
use catabolic_core::catalog_command::ReceiptVisibility;
use catabolic_core::catalog_metadata::nfo_refresh::{FileKey, OriginProvenance};
use std::time::Duration;

const HOST: &str = "nfo-history-test";
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
        max_receipts: 1000,
        max_logical_bytes: 64 * 1024 * 1024,
        max_hold_records: 100,
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
fn current_fingerprint(store: &Store) -> String {
    current_schema_fingerprint(&store.db, SUBJECT)
        .unwrap()
        .unwrap()
}
fn run_patch(store: &Store, id: &str, expected_revision: i64, title: &str) -> CommandEnvelope {
    run_patch_with_evidence(store, id, expected_revision, title, json!({"patch":id}))
}
fn run_patch_with_evidence(
    store: &Store,
    id: &str,
    expected_revision: i64,
    title: &str,
    evidence: Value,
) -> CommandEnvelope {
    let mutation = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Patch {
            subject: SUBJECT.into(),
            namespace: NAMESPACE.into(),
            record_id: RECORD.into(),
            schema_fingerprint: current_fingerprint(store),
            expected_revision,
            edits_json: json!({"title":{"op":"set","value":title}}).to_string(),
            evidence_json: evidence.to_string(),
        },
    };
    let command = envelope(
        store,
        id,
        json!({"operation":"metadata","mutation":mutation}),
    );
    let source = grant(MetadataAccess::Source {
        namespace: NAMESPACE.into(),
        record_id: RECORD.into(),
        kind: SourceKind::Provider,
    });
    assert!(matches!(
        commands::execute_with_authority(
            store,
            &command,
            CatalogAuthority::new(authority(true)).with_metadata(&source),
            policy()
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    command
}
fn replace(
    store: &Store,
    id: &str,
    namespace: &str,
    revision: i64,
    fields: Value,
) -> CommandEnvelope {
    let request = ContributionReplace {
        contract_version: 2,
        subject: SUBJECT.into(),
        namespace: namespace.into(),
        record_id: RECORD.into(),
        expected_schema_fingerprint: current_schema_fingerprint(&store.db, SUBJECT).unwrap(),
        expected_revision: revision,
        fields_json: fields.to_string(),
        evidence_json: format!("{{ \"revision_evidence\": \"{id}\" }}"),
        tags: vec![],
        excluded_tags: vec![],
        origin_creation_intent: None,
    };
    let command = envelope(
        store,
        id,
        json!({"operation":"metadata_contribution_replace","request":request}),
    );
    let source = grant(MetadataAccess::Source {
        namespace: namespace.into(),
        record_id: RECORD.into(),
        kind: SourceKind::Provider,
    });
    let initialize = grant(MetadataAccess::InitializeV2 {
        system_fields: Default::default(),
    });
    let schema = grant(MetadataAccess::ExtendSchema);
    let tags = TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [TagSubject::Item(SUBJECT.into())].into(),
        allow_global_merge: false,
    };
    assert!(matches!(
        commands::execute_with_authority(
            store,
            &command,
            CatalogAuthority::new(authority(true))
                .with_metadata(&source)
                .with_metadata_initialization(&initialize)
                .with_metadata_schema(&schema)
                .with_tags(&tags),
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
    baseline: VerifiedNfoBaseline,
}
impl Fixture {
    fn new(v1: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("nfo.db");
        migration::initialize_native_at(&path, 40).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let seed = envelope(
            &store,
            "seed",
            json!({"operation":"item_put","item":{"id":SUBJECT,"kind":"video","metadata":{}}}),
        );
        commands::execute(&store, &seed, authority(true), policy()).unwrap();
        let command = if v1 {
            let mutation = MetadataMutation {
                contract_version: 1,
                change: MetadataChange::Initialize {
                    subject: SUBJECT.into(),
                    policies: vec![PolicyField {
                        field: "title".into(),
                        value_type: PolicyValueType::String,
                        nullable: false,
                        normalization: PolicyNormalization::TrimAsciiLowercase,
                    }],
                },
            };
            let initialize = envelope(
                &store,
                "initialize-v1",
                json!({"operation":"metadata","mutation":mutation}),
            );
            commands::execute_with_authority(
                &store,
                &initialize,
                CatalogAuthority::new(authority(true))
                    .with_metadata(&grant(MetadataAccess::Initialize)),
                policy(),
            )
            .unwrap();
            run_patch(&store, "baseline", 0, "  Baseline  ")
        } else {
            replace(
                &store,
                "baseline",
                NAMESPACE,
                0,
                json!({"title":"Baseline"}),
            )
        };
        // Test-only baseline minting from an exact officially-created source.
        // Production baseline provenance is a separate independently verified
        // import/native-creation owner, never this fixture's row-copy shortcut.
        store
            .transaction_detailed::<_, Error>(|_| {
                verify_committed_command_in(&store, &command, authority(false), generation(&store))
            })
            .unwrap();
        let (fields_json,evidence_json,raw_mutation_json,schema_fingerprint): (String,String,String,String) = store.db.query_row("SELECT fields_json,evidence_json,raw_mutation_json,schema_fingerprint FROM catalog_metadata_history WHERE item_id=? AND namespace=? AND record_id=? AND revision=1",params![SUBJECT,NAMESPACE,RECORD],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        let baseline = VerifiedNfoBaseline {
            subject: SUBJECT.into(),
            revision: 1,
            fields_json,
            evidence_json,
            raw_mutation_json,
            schema_fingerprint,
            source_kind: SourceKind::Provider,
            command,
            automatic_identity: Some("imdb:tt-baseline".into()),
            attribution_audit_digest: "d".repeat(64),
            origin_proof_digest: None,
            origin_physical: None,
            imported_requested_names: None,
            imported_tag_mapping: None,
            origin: Some(FileOrigin {
                file: FileKey {
                    profile: "profile".into(),
                    file_id: "file".into(),
                },
                provenance: OriginProvenance::Observed {
                    exact_revision: "a".repeat(64),
                    binding_fingerprint: "b".repeat(64),
                    ownership_fingerprint: "c".repeat(64),
                },
            }),
        };
        Self {
            _root: root,
            store,
            baseline,
        }
    }
    fn verify(&self, commands: &[CommandEnvelope]) -> Result<VerifiedNfoSource> {
        self.store.transaction_detailed(|_| {
            verify_source_history_in(
                &self.store,
                &self.baseline,
                commands,
                authority(false),
                generation(&self.store),
            )
        })
    }
    fn compact(&self) {
        assert!(
            commands::compact_receipts(
                &self.store,
                TrustedReceiptMaintenance {
                    host_namespace: HOST,
                    holder_namespace: "maintenance"
                },
                100
            )
            .unwrap()
                > 0
        );
    }
}

#[test]
fn revision_one_v1_and_v2_baselines_remain_exact_without_rewriting_history() {
    for v1 in [false, true] {
        let f = Fixture::new(v1);
        let before = generation(&f.store);
        let verified = f.verify(&[]).unwrap();
        assert_eq!(verified.subject(), SUBJECT);
        assert_eq!(verified.catalog_id(), f.store.database_id);
        assert_eq!(
            Some(verified.incarnation()),
            f.store.catalog_incarnation.as_deref()
        );
        assert_eq!(verified.current_source().revision(), 1);
        assert_eq!(
            verified.current_source().schema_fingerprint(),
            f.baseline.schema_fingerprint
        );
        assert_eq!(
            serde_json::to_string(verified.current_source().fields()).unwrap(),
            f.baseline.fields_json
        );
        assert_eq!(verified.observed_generation(), before);
        assert_eq!(verified.automatic_identity(), Some("imdb:tt-baseline"));
        assert_eq!(verified.origin(), f.baseline.origin.as_ref());
        assert_eq!(verified.proof_digest().len(), 64);
        assert_eq!(generation(&f.store), before);
    }
}

#[test]
fn complete_history_reconstructs_across_other_source_schema_advances_and_compaction() {
    let f = Fixture::new(true);
    let baseline_fingerprint = f.baseline.schema_fingerprint.clone();
    let _other = replace(
        &f.store,
        "other-first",
        "motion.provider",
        0,
        json!({"genre":"Drama"}),
    );
    let expected = current_fingerprint(&f.store);
    assert_ne!(expected, baseline_fingerprint);
    let second = replace(
        &f.store,
        "nfo-second",
        NAMESPACE,
        1,
        json!({"title":" New title ","year":2020}),
    );
    assert_eq!(
        second.payload()["request"]["expected_schema_fingerprint"],
        expected
    );
    let source_schema = current_fingerprint(&f.store);
    let _other_again = replace(
        &f.store,
        "other-second",
        "motion.provider",
        1,
        json!({"actor":"Someone"}),
    );
    let current_schema = current_fingerprint(&f.store);
    assert_ne!(source_schema, current_schema);
    let before = generation(&f.store);
    let verified = f.verify(std::slice::from_ref(&second)).unwrap();
    assert_eq!(verified.current_source().fields()["title"], "new title");
    assert_eq!(
        verified.current_source().schema_fingerprint(),
        source_schema
    );
    assert_eq!(verified.current_schema().fingerprint(), current_schema);
    assert_eq!(verified.current_source().revision(), 2);
    assert_eq!(
        verified.automatic_identity(),
        f.baseline.automatic_identity.as_deref()
    );
    assert_eq!(verified.origin(), f.baseline.origin.as_ref());
    let digest = verified.proof_digest().to_owned();
    f.compact();
    let expired = f.verify(std::slice::from_ref(&second)).unwrap();
    assert_eq!(expired.proof_digest(), digest);
    assert_eq!(generation(&f.store), before);
    assert!(matches!(
        commands::reconcile_receipt(&f.store, &second, authority(false)).unwrap(),
        Some(CommandOutcome::ReceiptExpired)
    ));
}

#[test]
fn same_schema_empty_replacement_keeps_revision_and_baseline_provenance() {
    let f = Fixture::new(false);
    let second = replace(&f.store, "empty", NAMESPACE, 1, json!({}));
    let third = replace(&f.store, "again", NAMESPACE, 2, json!({"title":"Again"}));
    let verified = f.verify(&[second, third]).unwrap();
    assert_eq!(verified.current_source().revision(), 3);
    assert_eq!(verified.current_source().fields()["title"], "Again");
    assert_eq!(
        verified.current_source().evidence(),
        &json!({"revision_evidence":"again"})
    );
    assert_eq!(verified.origin(), f.baseline.origin.as_ref());
    assert_eq!(verified.automatic_identity(), Some("imdb:tt-baseline"));
}

#[test]
fn missing_duplicate_reordered_and_extraneous_real_envelopes_refuse() {
    let f = Fixture::new(false);
    let second = replace(&f.store, "second", NAMESPACE, 1, json!({"title":"Two"}));
    let third = replace(&f.store, "third", NAMESPACE, 2, json!({"title":"Three"}));
    let other = replace(
        &f.store,
        "other",
        "motion.provider",
        0,
        json!({"genre":"Comedy"}),
    );
    assert!(f.verify(&[]).is_err());
    assert!(f.verify(std::slice::from_ref(&second)).is_err());
    assert!(f.verify(&[second.clone(), second.clone()]).is_err());
    assert!(f.verify(&[third.clone(), second.clone()]).is_err());
    assert!(
        f.verify(&[second.clone(), third.clone(), other.clone()])
            .is_err()
    );
    assert!(f.verify(&[second.clone(), other]).is_err());
    assert!(f.verify(&[second, third]).is_ok());
}

#[test]
fn unknown_but_really_committed_and_compacted_patch_history_refuses() {
    let f = Fixture::new(true);
    let patch = run_patch(&f.store, "unknown-later-patch", 1, "Changed");
    f.compact();
    assert!(matches!(
        commands::reconcile_receipt(&f.store, &patch, authority(false)).unwrap(),
        Some(CommandOutcome::ReceiptExpired)
    ));
    let error = f.verify(&[patch]).unwrap_err();
    assert!(error.0.contains("unrecognized source history operation"));
}

#[test]
fn forged_replacement_payload_cannot_borrow_a_real_expired_receipt_key() {
    let f = Fixture::new(false);
    let second = replace(&f.store, "second", NAMESPACE, 1, json!({"title":"Real"}));
    f.compact();
    for field in [
        "fields_json",
        "evidence_json",
        "expected_revision",
        "subject",
        "namespace",
        "record_id",
        "expected_schema_fingerprint",
    ] {
        let mut payload = second.payload().clone();
        payload["request"][field] = match field {
            "expected_revision" => json!(0),
            "fields_json" => json!(r#"{"title":"Forged"}"#),
            "evidence_json" => json!(r#"{"forged":true}"#),
            "expected_schema_fingerprint" => json!("f".repeat(64)),
            _ => json!("different"),
        };
        let forged = CommandEnvelope::new(
            second.contract_version(),
            second.key().clone(),
            second.admission_incarnation(),
            second.expected_generation(),
            &payload.to_string(),
        )
        .unwrap();
        assert!(f.verify(&[forged]).is_err(), "{field}");
    }
    assert!(f.verify(&[second]).is_ok());
}

#[test]
fn baseline_bytes_revision_kind_schema_and_command_are_not_caller_corrections() {
    for field in [
        "fields", "evidence", "raw", "revision", "kind", "schema", "command",
    ] {
        let mut f = Fixture::new(false);
        match field {
            "fields" => f.baseline.fields_json = json!({"title":"Forged"}).to_string(),
            "evidence" => f.baseline.evidence_json = "{}".into(),
            "raw" => f.baseline.raw_mutation_json = "{}".into(),
            "revision" => f.baseline.revision = 2,
            "kind" => f.baseline.source_kind = SourceKind::Scan,
            "schema" => f.baseline.schema_fingerprint = "f".repeat(64),
            _ => {
                f.baseline.command = envelope(
                    &f.store,
                    "never-committed",
                    json!({"operation":"item_put","item":{"id":SUBJECT,"kind":"video","metadata":{}}}),
                )
            }
        }
        assert!(f.verify(&[]).is_err(), "{field}");
    }
}

#[test]
fn retained_writer_current_authority_generation_and_restore_are_independent() {
    let f = Fixture::new(false);
    assert!(
        verify_source_history_in(
            &f.store,
            &f.baseline,
            &[],
            authority(false),
            generation(&f.store)
        )
        .is_err()
    );
    for denied in [
        authority(true),
        TrustedCommandAuthority {
            host_namespace: "different",
            ..authority(false)
        },
        TrustedCommandAuthority {
            receipt_visibility: ReceiptVisibility::CommittedStatusOnly,
            ..authority(false)
        },
    ] {
        assert!(
            f.store
                .transaction_detailed::<_, Error>(|_| verify_source_history_in(
                    &f.store,
                    &f.baseline,
                    &[],
                    denied,
                    generation(&f.store)
                ))
                .is_err()
        );
    }
    assert!(
        f.store
            .transaction_detailed::<_, Error>(|_| verify_source_history_in(
                &f.store,
                &f.baseline,
                &[],
                authority(false),
                generation(&f.store) + 1
            ))
            .is_err()
    );
    let path = f.store.path.clone();
    let Fixture {
        _root,
        store,
        baseline,
    } = f;
    drop(store);
    let db = crate::connect(&path, true).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) SELECT 'restore',catalog_id,incarnation FROM catalog_state; UPDATE catalog_state SET incarnation=lower(hex(randomblob(16))); DELETE FROM catalog_mutation_permits; COMMIT;").unwrap();
    drop(db);
    let store = Store::open(&path, true, false).unwrap();
    assert!(
        store
            .transaction_detailed::<_, Error>(|_| verify_source_history_in(
                &store,
                &baseline,
                &[],
                authority(false),
                generation(&store)
            ))
            .is_err()
    );
}

#[test]
fn cumulative_row_byte_node_and_work_admission_never_returns_a_prefix() {
    let f = Fixture::new(false);
    let repeated = vec![f.baseline.command.clone(); MAX_METADATA_JSON_NODES];
    assert!(f.verify(&repeated).is_err());
    f.store
        .transaction_detailed::<_, Error>(|_| {
            for (bytes, values, work) in [
                (MAX_METADATA_SNAPSHOT_BYTES + 1, 0, 0),
                (0, MAX_METADATA_JSON_NODES + 1, 0),
                (0, 0, MAX_METADATA_JSON_NODES + 1),
            ] {
                let mut budget = Budget {
                    shared: f.store.catalog_operation_budget()?,
                    bytes: 0,
                    values: 0,
                    work: 0,
                };
                assert!(budget.charge(bytes, values, work).is_err());
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(f.verify(&[]).unwrap().current_source().revision(), 1);
}

#[test]
fn contradictory_reconstructed_rows_fail_even_with_a_real_compacted_commit() {
    let f = Fixture::new(false);
    let second = replace(&f.store, "second", NAMESPACE, 1, json!({"title":"Actual"}));
    f.compact();
    f.store.transaction_detailed::<_, Error>(|_| {
        verify_committed_command_in(&f.store, &second, authority(false), generation(&f.store))?;
        let mut budget = Budget { shared: f.store.catalog_operation_budget()?, bytes: 0, values: 0, work: 0 };
        let registry = read_registry(&f.store, SUBJECT, &mut budget)?;
        let chain = lineage(&registry, &current_fingerprint(&f.store), &mut budget)?;
        let prior = ContributionDocument::from_json_with_system_fields(
            &registry[&f.baseline.schema_fingerprint].schema,
            ContributionKey::new(SUBJECT, NAMESPACE, RECORD).unwrap(), SourceKind::Provider, 1,
            &f.baseline.fields_json, &f.baseline.evidence_json,
        ).unwrap();
        let mut row: HistoryRow = f.store.db.query_row(
            "SELECT source_kind,schema_fingerprint,fields_json,evidence_json,raw_mutation_json FROM catalog_metadata_history WHERE item_id=? AND namespace=? AND record_id=? AND revision=2",
            params![SUBJECT,NAMESPACE,RECORD], |r|Ok(HistoryRow { revision:2, kind:r.get(0)?, fingerprint:r.get(1)?, fields:r.get(2)?, evidence:r.get(3)?, raw:r.get(4)?, command:second.key().clone() }),
        )?;
        let schema = &registry[&row.fingerprint].schema;
        let actual = row.document(SUBJECT, SourceKind::Provider, schema)?;
        verify_replacement(&row, &actual, &prior, &second, &registry, &chain, &mut budget)?;
        // Corrupt only in-memory proof material. Retained SQL history and its
        // immutable triggers remain untouched, including in negative fixtures.
        let original_raw = row.raw.clone();
        row.raw = "{}".into();
        assert!(verify_replacement(&row, &actual, &prior, &second, &registry, &chain, &mut budget).is_err());
        row.raw = original_raw;
        for (fields, evidence) in [
            (json!({"title":"Contradictory"}).to_string(), row.evidence.clone()),
            (row.fields.clone(), json!({"not":"the committed evidence"}).to_string()),
        ] {
            let contradictory = ContributionDocument::from_json_with_system_fields(schema, actual.key().clone(), actual.kind(), 2, &fields, &evidence).unwrap();
            assert!(verify_replacement(&row, &contradictory, &prior, &second, &registry, &chain, &mut budget).is_err());
        }
        Ok(())
    }).unwrap();
    assert!(f.verify(&[second]).is_ok());
}

#[test]
fn independent_attribution_audit_is_retained_in_source_proof() {
    let mut f = Fixture::new(false);
    let first = f.verify(&[]).unwrap();
    assert_eq!(first.attribution_audit_digest(), "d".repeat(64));
    f.baseline.attribution_audit_digest = "e".repeat(64);
    let second = f.verify(&[]).unwrap();
    assert_ne!(first.proof_digest(), second.proof_digest());
    assert_eq!(first.current_source(), second.current_source());
    f.baseline.attribution_audit_digest.clear();
    assert!(f.verify(&[]).is_err());
}

#[test]
fn exact_motion_v1_patch_history_survives_compaction_and_v2_extension() {
    let f = Fixture::new(true);
    let patch = run_patch_with_evidence(
        &f.store,
        "motion-v1-later",
        1,
        "Changed",
        json!({"owner":"motion.metadata.replace","request_digest":"a".repeat(64),"expected_aggregate_revision":9}),
    );
    f.compact();
    let verified = f.verify(&[patch.clone()]).unwrap();
    assert_eq!(verified.current_source().revision(), 2);
    assert_eq!(verified.automatic_identity(), Some("imdb:tt-baseline"));
    assert_eq!(verified.origin(), f.baseline.origin.as_ref());
    let replacement = replace(
        &f.store,
        "motion-v2-later",
        NAMESPACE,
        2,
        json!({"title":"After extension", "newfield":"new"}),
    );
    let verified = f.verify(&[patch, replacement]).unwrap();
    assert_eq!(verified.current_source().revision(), 3);
    assert_eq!(verified.current_source().fields()["newfield"], "new");
    assert_eq!(verified.origin(), f.baseline.origin.as_ref());
}
