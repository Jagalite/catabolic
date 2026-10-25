use super::super::tests::Fixture;
use super::*;
use crate::catalog_commands::{self as commands, TrustedReceiptMaintenance};
use crate::catalog_nfo_attribution::ProofBudget;
use catabolic_core::catalog_command::ReceiptVisibility;
const SUBJECT: &str = "motion.library:library";
fn authority() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: false,
        identity_authority: None,
    }
}
fn part(f: &Fixture) -> HostImportPart<'_> {
    HostImportPart {
        manifest: &f.manifest,
        batches: &f.batches,
        activation: &f.activation,
    }
}
fn verify(
    f: &Fixture,
    part: HostImportPart<'_>,
    authority: TrustedCommandAuthority<'_>,
) -> Result<ObservedArchivedLibrary> {
    f.store.transaction_detailed(|_| {
        let mut budget = ProofBudget::new(f.store.catalog_operation_budget()?);
        validate_archived_library_in(&f.store, SUBJECT, part, authority, &mut budget)
    })
}
// Real40 import commands followed by the existing candidate41 test installer.
// These qualify the private reader, not Completed owner admission or a bridge.
#[test]
fn archived_input_survives_actual_receipt_compaction_without_result_bodies() {
    let f = Fixture::new();
    let before = verify(&f, part(&f), authority()).unwrap();
    let compacted = commands::compact_candidate_receipts_for_test(
        &f.store,
        TrustedReceiptMaintenance {
            host_namespace: HOST,
            holder_namespace: catalog_import::IMPORT_HOLDER_NAMESPACE,
        },
        100,
    )
    .unwrap();
    assert!(compacted > 0);
    let state: (String, bool) = f.store.db.query_row(
        "SELECT result_state,result_json IS NULL FROM catalog_command_receipts WHERE operation_id=?",
        [f.activation.key().operation_id()], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(state, ("expired".into(), true));
    assert!(
        f.verify().is_err(),
        "old full-result wrapper remains strict"
    );
    let after = verify(&f, part(&f), authority()).unwrap();
    assert_eq!(after.definition(), before.definition());
    assert_eq!(
        after.current_item_revision(),
        before.current_item_revision()
    );
    assert_eq!(after.catalog_id(), f.store.database_id);
    assert_eq!(after.incarnation(), f.activation.admission_incarnation());
    assert_eq!(after.import_id(), f.manifest.import_id);
    assert_eq!(
        after.activation_operation_id(),
        f.activation.key().operation_id()
    );
    assert_eq!(after.activation_digest(), f.activation.digest().as_str());
    assert_eq!(after.observed_generation(), before.observed_generation());
}
#[test]
fn archived_reader_refuses_changed_input_command_and_receipt_authority() {
    let f = Fixture::new();
    let mut manifest = f.manifest.clone();
    manifest.source_snapshot_id.push_str("-changed");
    assert!(
        verify(
            &f,
            HostImportPart {
                manifest: &manifest,
                ..part(&f)
            },
            authority()
        )
        .is_err()
    );
    let mut batches = f.batches.clone();
    batches[0].items[0].kind = "custom:other".into();
    assert!(
        verify(
            &f,
            HostImportPart {
                batches: &batches,
                ..part(&f)
            },
            authority()
        )
        .is_err()
    );
    let mut mutation = f.activation.payload().clone();
    mutation["mutation"]["reference_ack_id"] = json!("different-audit");
    let altered = CommandEnvelope::new(
        1,
        CommandKey::new(
            &f.store.database_id,
            HOST,
            f.activation.key().operation_id(),
        )
        .unwrap(),
        f.activation.admission_incarnation(),
        f.activation.expected_generation(),
        &mutation.to_string(),
    )
    .unwrap();
    assert!(
        verify(
            &f,
            HostImportPart {
                activation: &altered,
                ..part(&f)
            },
            authority()
        )
        .is_err()
    );
    for (catalog, incarnation) in [
        ("foreign-catalog", f.activation.admission_incarnation()),
        (f.store.database_id.as_str(), "foreign-incarnation"),
    ] {
        let foreign = CommandEnvelope::new(
            1,
            CommandKey::new(catalog, HOST, f.activation.key().operation_id()).unwrap(),
            incarnation,
            f.activation.expected_generation(),
            &f.activation.payload().to_string(),
        )
        .unwrap();
        assert!(
            verify(
                &f,
                HostImportPart {
                    activation: &foreign,
                    ..part(&f)
                },
                authority()
            )
            .is_err()
        );
    }
    let mut denied = authority();
    denied.allow_new_commands = true;
    assert!(verify(&f, part(&f), denied).is_err());
    let mut foreign = authority();
    foreign.host_namespace = "foreign.host";
    assert!(verify(&f, part(&f), foreign).is_err());
}
#[test]
fn archived_observed_head_is_not_an_original_activation_baseline() {
    let f = Fixture::new();
    let before = verify(&f, part(&f), authority()).unwrap();
    f.store
        .transaction_detailed::<_, Error>(|_| {
            // Exercise actual permanent item revision triggers, not a rewritten head.
            // Restored contents deliberately do not restore the original revision.
            f.store.db.execute(
                "UPDATE items SET kind='custom:test_away' WHERE id=?",
                [SUBJECT],
            )?;
            f.store.db.execute(
                "UPDATE items SET kind='custom:motion_library' WHERE id=?",
                [SUBJECT],
            )?;
            Ok(())
        })
        .unwrap();
    let after = verify(&f, part(&f), authority()).unwrap();
    assert_eq!(after.definition(), before.definition());
    assert!(after.current_item_revision() > before.current_item_revision());
    // The independent immutable host bridge would still contain the original
    // baseline. This descriptive native API must not relabel the higher head.
    assert_ne!(
        after.current_item_revision(),
        before.current_item_revision()
    );
    assert!(
        f.verify()
            .err()
            .unwrap()
            .to_string()
            .contains("head differs from activation")
    );
}
#[test]
fn archived_reader_ignores_trimmed_result_body_but_not_current_projection() {
    let f = Fixture::new();
    verify(&f, part(&f), authority()).unwrap();
    f.store.transaction_detailed::<_, Error>(|_| {
        // Corruption fixture only: exact guard bytes restored before any read.
        let guard: String = f.store.db.query_row("SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='catalog_receipt_compaction_only'", [], |r|r.get(0))?;
        f.store.db.execute_batch("DROP TRIGGER catalog_receipt_compaction_only")?;
        f.store.db.execute("UPDATE catalog_command_receipts SET result_json='{}' WHERE operation_id=?", [f.activation.key().operation_id()])?;
        f.store.db.execute_batch(&guard)?;
        Ok(())
    }).unwrap();
    assert!(f.verify().is_err());
    verify(&f, part(&f), authority()).unwrap();
    // Use a fresh genuine owner for the independent typed-change case: the
    // preceding test-only malformed result must not become mutation input.
    let f = Fixture::new();
    // Change managed projection through its actual typed owner, not a raw
    // items UPDATE that the managed-metadata guard correctly rejects.
    use crate::catalog_metadata_store::{MetadataAccess, MetadataAuthority, MetadataSystemReplace};
    let before = verify(&f, part(&f), authority()).unwrap();
    let mut definition = before.definition().clone();
    definition["name"] = json!("Changed through typed system replacement");
    // The imported source retains its historical schema. Replacement expects
    // the current registered subject schema, not that source's fingerprint.
    let fingerprint: String = f
        .store
        .db
        .query_row(
            "SELECT schema_fingerprint FROM catalog_metadata_schema_heads WHERE item_id=?",
            [SUBJECT],
            |r| r.get(0),
        )
        .unwrap();
    let request = MetadataSystemReplace {
        contract_version: 1,
        subject: SUBJECT.into(),
        namespace: "motion.structure".into(),
        record_id: "typed_library_edit".into(),
        expected_schema_fingerprint: Some(fingerprint),
        expected_revision: 0,
        fields_json: json!({"motion_library":definition}).to_string(),
        evidence_json: json!({"fixture":"genuine post-import typed edit"}).to_string(),
        origin_creation_intent: None,
    };
    // Preserve the original mixed source, including its external title. A v2
    // external field cannot be promoted to system ownership. This new Local
    // source owns only the already-system library field and wins over Scan.
    let system_fields = std::collections::BTreeSet::from(["motion_library".into()]);
    let grant = |access| MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access,
    };
    let source = grant(MetadataAccess::SourceV2 {
        namespace: request.namespace.clone(),
        record_id: request.record_id.clone(),
        kind: catabolic_core::catalog_metadata::SourceKind::Local,
        system_fields: system_fields.clone(),
    });
    let initialization = grant(MetadataAccess::InitializeV2 { system_fields });
    let schema = grant(MetadataAccess::ExtendSchema);
    let command = CommandEnvelope::new(
        1,
        CommandKey::new(&f.store.database_id, HOST, "archived-library-typed-edit").unwrap(),
        f.activation.admission_incarnation(),
        f.store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
        &json!({"operation":"metadata_system_replace","request":request}).to_string(),
    )
    .unwrap();
    let outcome = commands::execute_candidate_dispatch_for_test(
        &f.store,
        &command,
        commands::CatalogAuthority::new(TrustedCommandAuthority {
            allow_new_commands: true,
            ..authority()
        })
        .with_metadata(&source)
        .with_metadata_initialization(&initialization)
        .with_metadata_schema(&schema),
        commands::ReceiptPolicy {
            retention: std::time::Duration::ZERO,
            max_receipts: 100,
            max_logical_bytes: 16 * 1024 * 1024,
            max_hold_records: 100,
        },
    )
    .unwrap();
    assert!(matches!(
        outcome,
        commands::CommandOutcome::Committed { .. }
    ));
    let metadata: String = f
        .store
        .db
        .query_row("SELECT metadata FROM items WHERE id=?", [SUBJECT], |r| {
            r.get(0)
        })
        .unwrap();
    let metadata: Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(metadata["motion_library"], definition);
    assert!(verify(&f, part(&f), authority()).is_err());
}

#[test]
fn archived_preflight_propagates_first_error_before_owned_archive_encoding() {
    struct Refuse {
        attempts: usize,
    }
    impl StructuralReadBudget for Refuse {
        fn charge(&mut self, _: BudgetCharge) -> Result<()> {
            self.attempts += 1;
            Err(fail("test first preflight refusal"))
        }
        fn raw_json(&mut self, _: &str) -> Result<()> {
            panic!("owned archive encoding must not precede refused borrowed preflight")
        }
    }
    let f = Fixture::new();
    f.store
        .transaction_detailed::<_, Error>(|_| {
            let before = f.store.db.total_changes();
            let mut budget = Refuse { attempts: 0 };
            let error =
                validate_archived_library_in(&f.store, SUBJECT, part(&f), authority(), &mut budget)
                    .err()
                    .unwrap()
                    .to_string();
            assert!(error.contains("first preflight refusal"), "{error}");
            assert_eq!(budget.attempts, 1);
            assert_eq!(f.store.db.total_changes(), before);
            Ok(())
        })
        .unwrap();
}

#[test]
fn genuine_archive_refuses_mixed_deep_and_actual_oversized_inputs_before_hashing() {
    let f = Fixture::new();
    let mut mixed = f.batches.clone();
    mixed[0]
        .unresolved_evidence
        .push(crate::catalog_import::UnresolvedEvidence {
            evidence_id: "unrelated".into(),
            kind: crate::catalog_import::EvidenceKind::Contribution,
            data: json!({}),
        });
    let error = verify(
        &f,
        HostImportPart {
            batches: &mixed,
            ..part(&f)
        },
        authority(),
    )
    .err()
    .unwrap()
    .to_string();
    assert!(
        error.contains("mixed archive owners unsupported"),
        "{error}"
    );
    let mut deep = f.batches.clone();
    let mut value = json!("x");
    for _ in 0..17 {
        value = json!([value]);
    }
    deep[0].typed.as_mut().unwrap().metadata[0].contributions[0].fields = value;
    let error = verify(
        &f,
        HostImportPart {
            batches: &deep,
            ..part(&f)
        },
        authority(),
    )
    .err()
    .unwrap()
    .to_string();
    assert!(error.contains("library accounting"), "{error}");
    let mut oversized = f.batches.clone();
    oversized[0].typed.as_mut().unwrap().metadata[0].contributions[0].fields =
        json!({"title":"x".repeat(4*1024*1024)});
    let error = verify(
        &f,
        HostImportPart {
            batches: &oversized,
            ..part(&f)
        },
        authority(),
    )
    .err()
    .unwrap()
    .to_string();
    assert!(
        !error.contains("descriptor differs"),
        "preflight must fail before hashing: {error}"
    );
}

#[test]
fn actual_refused_activate_cannot_authenticate_after_receipt_maintenance() {
    let f = Fixture::with_refused_activation();
    commands::compact_candidate_receipts_for_test(
        &f.store,
        TrustedReceiptMaintenance {
            host_namespace: HOST,
            holder_namespace: catalog_import::IMPORT_HOLDER_NAMESPACE,
        },
        100,
    )
    .unwrap();
    let rows: i64 = f
        .store
        .db
        .query_row(
            "SELECT count(*) FROM catalog_command_receipts WHERE operation_id=?",
            [f.activation.key().operation_id()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
    assert!(
        verify(&f, part(&f), authority()).is_err(),
        "canonical input alone is not a successful permanent Activate"
    );
}
