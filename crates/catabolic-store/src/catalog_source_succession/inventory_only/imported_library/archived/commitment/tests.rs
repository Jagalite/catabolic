use super::*;
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
#[derive(Default)]
struct Budget;
impl StructuralReadBudget for Budget {
    fn charge(&mut self, _: BudgetCharge) -> Result<()> {
        Ok(())
    }
    fn raw_json(&mut self, _: &str) -> Result<()> {
        Ok(())
    }
}
fn batch() -> ImportBatch {
    serde_json::from_value(json!({
        "items":[{"source_id":"item","item_id":"item","kind":"custom:unrelated","legacy_metadata":{"title":"opaque legacy"}}],
        "unresolved_evidence":[],
        "typed":{
            "metadata":[{"item_id":"item","policies":[],"contributions":[
                {"namespace":"n","record_id":"local","source_revision":1,"kind":"local","fields":{},"evidence":{}},
                {"namespace":"n","record_id":"provider","source_revision":1,"kind":"provider","fields":{},"evidence":{}},
                {"namespace":"n","record_id":"scan","source_revision":1,"kind":"scan","fields":{},"evidence":{}}
            ]}],
            "pins":[{"item_id":"item","namespace":"n","history":[{"source_revision":1,"decision":{"decision":"set","value":"selected"}},{"source_revision":2,"decision":{"decision":"remove"}}]}],
            "tags":[{"item_id":"item","namespace":"n","source_revision":1,"tags":["One","Two"],"excluded_tags":["Three"]}],
            "relationships":[{"source_relationship_id":"edge","source_revision":1,"source_item_id":"item","target_item_id":"other","kind":"custom:related","position":null,"metadata":{}}],
            "logical_aliases":[{"alias_item_id":"alias","survivor_item_id":"item","source_receipt_id":"receipt","source_receipt_json":"{\"opaque\":true}"}],
            "native":{
                "occurrences":[{"source_occurrence_id":"occurrence","profile":"default","location":"media","path":"file","exact":{"file_id":"file","revision":"revision"},"observed_status":null}],
                "associations":[{"source_association_id":"association","source_occurrence_id":"occurrence","item_id":"item","part":1}],
                "mutations":[{"contract_version":1,"profile":"default","context":{"id":"context","authority_namespace":"test","context_key":"context","expected_revision":null},"timelines":[{"timeline_id":"timeline","edition_item_id":"item","expected_revision":null,"identity_state":"provisional","coverage":null}],"versions":[],"evidence":[]}]
            }
        }
    })).unwrap()
}
fn preflight(batch: &ImportBatch) -> Result<()> {
    let descriptor = BatchDescriptor {
        sha256: "0".repeat(64),
        canonical_bytes: 1,
        items: batch.items.len(),
        unresolved_evidence: batch.unresolved_evidence.len(),
        typed_records: typed_count(batch, &mut Budget)?,
    };
    let manifest = SnapshotManifest {
        contract_version: 2,
        import_id: "part".into(),
        source_namespace: "motion.schema68.export.v4".into(),
        source_snapshot_id: "snapshot".into(),
        batches: vec![descriptor],
    };
    let command = CommandEnvelope::new(
        1,
        CommandKey::new("catalog", HOST, "activate").unwrap(),
        "incarnation",
        0,
        "{\"operation\":\"test\"}",
    )
    .unwrap();
    super::super::accounting::preflight_mixed(
        HostImportPart {
            manifest: &manifest,
            batches: std::slice::from_ref(batch),
            activation: &command,
        },
        "not-selected",
        &mut Budget,
    )
}
#[test]
fn descriptor_matches_original_encoder_for_every_typed_branch_and_omission() {
    let all = batch();
    preflight(&all).unwrap();
    assert_eq!(
        describe(&all, &mut Budget).unwrap(),
        catalog_import::describe_batch(&all).unwrap()
    );
    assert_eq!(
        typed_count(&all, &mut Budget).unwrap(),
        all.typed.as_ref().unwrap().record_count()
    );
    let mut present = all.clone();
    let typed = present.typed.as_mut().unwrap();
    typed.metadata[0].contract_version = 2;
    typed.metadata[0].system_fields = vec!["special".into()];
    typed.relationships[0].position = Some(1);
    typed.native.as_mut().unwrap().occurrences[0].observed_status = Some("ok".into());
    preflight(&present).unwrap();
    assert_eq!(
        describe(&present, &mut Budget).unwrap(),
        catalog_import::describe_batch(&present).unwrap()
    );
    let raw = serde_json::to_value(&all).unwrap();
    assert!(
        raw["typed"]["metadata"][0]
            .get("contract_version")
            .is_none()
    );
    assert!(raw["typed"]["metadata"][0].get("system_fields").is_none());
    assert_eq!(
        raw["typed"]["pins"][0]["history"][1]["decision"],
        json!({"decision":"remove"})
    );
    let mut absent = all.clone();
    absent.typed = None;
    assert!(
        serde_json::to_value(&absent)
            .unwrap()
            .get("typed")
            .is_none()
    );
    preflight(&absent).unwrap();
    assert_eq!(
        describe(&absent, &mut Budget).unwrap(),
        catalog_import::describe_batch(&absent).unwrap()
    );
    for kind in [
        catalog_import::EvidenceKind::Contribution,
        catalog_import::EvidenceKind::AcceptedDecision,
        catalog_import::EvidenceKind::PhysicalOccurrence,
        catalog_import::EvidenceKind::TimelineVersion,
        catalog_import::EvidenceKind::ApplicationReference,
    ] {
        let mut with = absent.clone();
        with.unresolved_evidence
            .push(catalog_import::UnresolvedEvidence {
                evidence_id: "evidence".into(),
                kind,
                data: json!({"raw":"not interpreted"}),
            });
        preflight(&with).unwrap();
        assert_eq!(
            describe(&with, &mut Budget).unwrap(),
            catalog_import::describe_batch(&with).unwrap()
        );
    }
}
#[test]
fn unrelated_alias_strings_are_opaque_but_floats_large_maps_and_depth_refuse() {
    let mut value = batch();
    value.typed.as_mut().unwrap().logical_aliases[0].source_receipt_json =
        "not JSON, retained as exact text only".into();
    preflight(&value).unwrap();
    assert!(
        catalog_import::describe_batch(&value).is_err(),
        "canonical-only is not semantic re-admission"
    );
    value.items[0].legacy_metadata = json!({"float":1.5});
    assert!(preflight(&value).is_err());
    value.items[0].legacy_metadata =
        Value::Object((0..17).map(|n| (n.to_string(), json!(n))).collect());
    assert!(preflight(&value).is_err());
    let mut deep = json!(0);
    for _ in 0..17 {
        deep = json!([deep]);
    }
    value.items[0].legacy_metadata = deep;
    assert!(preflight(&value).is_err());
}

fn verify_mixed(
    f: &super::super::super::tests::Fixture,
    part: HostImportPart<'_>,
) -> Result<ObservedArchivedLibrary> {
    f.store.transaction_detailed(|_| {
        let mut budget =
            crate::catalog_nfo_attribution::ProofBudget::new(f.store.catalog_operation_budget()?);
        super::super::validate_mixed_archived_library_in(
            &f.store,
            "motion.library:library",
            part,
            TrustedCommandAuthority {
                host_namespace: HOST,
                receipt_visibility: catabolic_core::catalog_command::ReceiptVisibility::FullResult,
                allow_new_commands: false,
                identity_authority: None,
            },
            &mut budget,
        )
    })
}
#[test]
fn genuine_mixed_activation_compaction_preserves_exact_selected_lineage() {
    let f = super::super::super::tests::Fixture::with_mixed_archive(false);
    let part = HostImportPart {
        manifest: &f.manifest,
        batches: &f.batches,
        activation: &f.activation,
    };
    // The library-only wrapper must not silently broaden to mixed producers.
    f.store
        .transaction_detailed::<_, Error>(|_| {
            let mut budget = crate::catalog_nfo_attribution::ProofBudget::new(
                f.store.catalog_operation_budget()?,
            );
            assert!(
                super::super::validate_archived_library_in(
                    &f.store,
                    "motion.library:library",
                    part,
                    TrustedCommandAuthority {
                        host_namespace: HOST,
                        receipt_visibility:
                            catabolic_core::catalog_command::ReceiptVisibility::FullResult,
                        allow_new_commands: false,
                        identity_authority: None,
                    },
                    &mut budget,
                )
                .is_err()
            );
            Ok(())
        })
        .unwrap();
    let before = verify_mixed(&f, part).unwrap();
    let compacted = crate::catalog_commands::compact_candidate_receipts_for_test(
        &f.store,
        crate::catalog_commands::TrustedReceiptMaintenance {
            host_namespace: HOST,
            holder_namespace: catalog_import::IMPORT_HOLDER_NAMESPACE,
        },
        100,
    )
    .unwrap();
    assert_eq!(compacted, f.batches.len() + 3);
    let after = verify_mixed(&f, part).unwrap();
    assert_eq!(before.definition(), after.definition());
    assert_eq!(
        before.current_item_revision(),
        after.current_item_revision()
    );
    let state:(String,bool)=f.store.db.query_row("SELECT result_state,result_json IS NULL FROM catalog_command_receipts WHERE operation_id=?",[f.activation.key().operation_id()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(state, ("expired".into(), true));
    assert!(f.verify().is_err(), "full-result wrapper stays strict");
}
#[test]
fn genuine_mixed_archive_rejects_reordered_changed_counts_and_foreign_identity() {
    let f = super::super::super::tests::Fixture::with_mixed_archive(false);
    let part = HostImportPart {
        manifest: &f.manifest,
        batches: &f.batches,
        activation: &f.activation,
    };
    let mut reordered = f.batches.clone();
    reordered.swap(0, 1);
    assert!(
        verify_mixed(
            &f,
            HostImportPart {
                batches: &reordered,
                ..part
            }
        )
        .is_err()
    );
    let mut manifest = f.manifest.clone();
    manifest.batches[0].typed_records += 1;
    assert!(
        verify_mixed(
            &f,
            HostImportPart {
                manifest: &manifest,
                ..part
            }
        )
        .is_err()
    );
    for field in 0..4 {
        let mut changed = f.manifest.clone();
        match field {
            0 => changed.batches[0].items += 1,
            1 => changed.batches[0].unresolved_evidence += 1,
            2 => changed.batches[0].canonical_bytes += 1,
            _ => changed.batches[0].sha256 = "0".repeat(64),
        }
        assert!(
            verify_mixed(
                &f,
                HostImportPart {
                    manifest: &changed,
                    ..part
                }
            )
            .is_err()
        );
    }
    manifest = f.manifest.clone();
    manifest.contract_version = 1;
    assert!(
        verify_mixed(
            &f,
            HostImportPart {
                manifest: &manifest,
                ..part
            }
        )
        .is_err()
    );
    manifest = f.manifest.clone();
    manifest.source_namespace = "foreign".into();
    assert!(
        verify_mixed(
            &f,
            HostImportPart {
                manifest: &manifest,
                ..part
            }
        )
        .is_err()
    );
    let foreign = CommandEnvelope::new(
        1,
        CommandKey::new("foreign", HOST, f.activation.key().operation_id()).unwrap(),
        f.activation.admission_incarnation(),
        f.activation.expected_generation(),
        &f.activation.payload().to_string(),
    )
    .unwrap();
    assert!(
        verify_mixed(
            &f,
            HostImportPart {
                activation: &foreign,
                ..part
            }
        )
        .is_err()
    );
}
#[test]
fn genuine_refused_mixed_activate_does_not_gain_commitment_from_maintenance() {
    let f = super::super::super::tests::Fixture::with_mixed_archive(true);
    crate::catalog_commands::compact_candidate_receipts_for_test(
        &f.store,
        crate::catalog_commands::TrustedReceiptMaintenance {
            host_namespace: HOST,
            holder_namespace: catalog_import::IMPORT_HOLDER_NAMESPACE,
        },
        100,
    )
    .unwrap();
    assert!(
        verify_mixed(
            &f,
            HostImportPart {
                manifest: &f.manifest,
                batches: &f.batches,
                activation: &f.activation
            }
        )
        .is_err()
    );
}
