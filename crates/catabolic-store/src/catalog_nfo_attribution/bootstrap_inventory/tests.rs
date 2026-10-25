//! Real native40 owners create every retained row. Candidate41 admission uses
//! the controlled test-only installer, never schema spoofing or trigger bypass.
use super::*;
use crate::{
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, TrustedReceiptMaintenance,
    },
    catalog_import::{self, ImportStep},
    catalog_metadata_store::{
        self as metadata, MetadataAccess, MetadataAuthority, MetadataChange, MetadataMutation,
    },
    catalog_nfo_attribution::{bootstrap, candidate_tests as fixture},
    catalog_tag_mutation::{TagAuthority, TagSubject},
    migration,
};
use catabolic_core::{catalog_command::CommandEnvelope, catalog_metadata::SourceKind};
use serde_json::json;
use std::{collections::BTreeSet, time::Duration};
const SUBJECT: &str = "nfo-subject";
const HOST: &str = "nfo-candidate-tests";
fn capture(store: &Store) -> BootstrapInventory {
    store.transaction_detailed(|_| capture_in(store)).unwrap()
}
fn replace(store: &Store, id: &str, expected: i64) -> CommandEnvelope {
    let request = fixture::put_request(store, expected, json!({"title":id}), &[]);
    fixture::replace_40(store, id, &request, SourceKind::Provider)
}
#[test]
fn actual_created_ledger_orders_exact_command_and_effect_commitments() {
    let f = fixture::Fixture::created();
    let second = replace(&f.store, "z-revision-two", 1);
    let third = replace(&f.store, "a-revision-three", 2);
    let before = migration::snapshot(&f.store.db).unwrap();
    let inventory = capture(&f.store);
    inventory.require_complete().unwrap();
    assert_eq!(
        inventory.subjects.len(),
        1,
        "local sources are scanned, not misclassified as NFO"
    );
    let subject = &inventory.subjects[SUBJECT];
    assert_eq!(subject.source().unwrap().revision(), 3);
    assert_eq!(subject.first_revision().unwrap().revision(), 1);
    assert!(!subject.retired());
    let steps: Vec<_> = subject.history().collect();
    assert_eq!(
        steps.iter().map(|(_, s)| s.revision()).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    for (index, command) in [(1, &second), (2, &third)] {
        let step = steps[index].1;
        assert_eq!(step.command(), command.key());
        match step.receipt() {
            ReceiptCommitment::Retained { payload_digest, .. } => {
                assert_eq!(payload_digest, command.digest())
            }
            ReceiptCommitment::Missing => panic!("real committed owner receipt absent"),
        }
        let raw:String=f.store.db.query_row("SELECT raw_mutation_json FROM catalog_metadata_history WHERE item_id=? AND namespace='motion.nfo' AND record_id='document' AND revision=?",params![SUBJECT,step.revision()],|r|r.get(0)).unwrap();
        assert_eq!(step.raw_effect_digest(), raw_digest(&raw));
    }
    assert_eq!(inventory.digest(), capture(&f.store).digest());
    assert_eq!(inventory.catalog_id(), f.store.database_id);
    assert_eq!(
        Some(inventory.incarnation()),
        f.store.catalog_incarnation.as_deref()
    );
    assert!(inventory.generation() >= third.expected_generation());
    assert_eq!(before, migration::snapshot(&f.store.db).unwrap());
    assert!(
        capture_in(&f.store).is_err(),
        "an unlocked description is not admitted"
    );
}
#[test]
fn actual_import_and_retirement_preserve_activation_trace_and_complete_subject() {
    let f = fixture::Fixture::imported(false);
    f.retire_40();
    let f = f.install();
    let inventory = capture(&f.store);
    inventory.require_complete().unwrap();
    assert_eq!(inventory.subjects.len(), 1);
    let subject = &inventory.subjects[SUBJECT];
    assert!(subject.retired());
    assert_eq!(
        subject.source().unwrap().revision(),
        1,
        "archived public9 is not native source9"
    );
    assert_eq!(
        subject.first_revision().unwrap().command().operation_id(),
        catalog_import::operation_id("metadata-part", ImportStep::Activate).unwrap()
    );
    assert!(matches!(
        subject.first_revision().unwrap().receipt(),
        ReceiptCommitment::Retained { .. }
    ));
    assert!(subject.retirement_head.as_ref().unwrap().revision > 0);
    f.bootstrap();
    let active: i64 = f
        .store
        .db
        .query_row(
            "SELECT count(*) FROM catalog_nfo_identity_claims",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        active, 0,
        "descriptive retired presence never resurrects a claim"
    );
}
#[test]
fn permanent_receipt_digest_survives_actual_compaction_without_recovered_payload() {
    let f = fixture::Fixture::created();
    let request = fixture::put_request(&f.store, 1, json!({"title":"last"}), &[]);
    let command = fixture::envelope(
        &f.store,
        "expires",
        json!({"operation":"metadata_contribution_replace","request":request}),
    );
    let source = MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::Source {
            namespace: "motion.nfo".into(),
            record_id: "document".into(),
            kind: SourceKind::Provider,
        },
    };
    let initialization = MetadataAuthority {
        access: MetadataAccess::InitializeV2 {
            system_fields: BTreeSet::new(),
        },
        ..source.clone()
    };
    let extension = MetadataAuthority {
        access: MetadataAccess::ExtendSchema,
        ..source.clone()
    };
    let tags = TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [TagSubject::Item(SUBJECT.into())].into(),
        allow_global_merge: false,
    };
    let mut policy = fixture::policy();
    policy.retention = Duration::ZERO;
    assert!(matches!(
        commands::execute_with_authority(
            &f.store,
            &command,
            CatalogAuthority::new(fixture::authority(true))
                .with_metadata(&source)
                .with_metadata_initialization(&initialization)
                .with_metadata_schema(&extension)
                .with_tags(&tags),
            policy
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let before = capture(&f.store);
    assert_eq!(
        commands::compact_receipts(
            &f.store,
            TrustedReceiptMaintenance {
                host_namespace: HOST,
                holder_namespace: "inventory-test"
            },
            100
        )
        .unwrap(),
        1
    );
    assert!(matches!(
        commands::reconcile_receipt(&f.store, &command, fixture::authority(false)).unwrap(),
        Some(CommandOutcome::ReceiptExpired)
    ));
    let after = capture(&f.store);
    assert_eq!(
        before.digest(),
        after.digest(),
        "body expiry cannot erase or revive a permanent commitment"
    );
    let step = &after.subjects[SUBJECT].history[&("document".into(), 2)];
    match step.receipt() {
        ReceiptCommitment::Retained { payload_digest, .. } => {
            assert_eq!(payload_digest, command.digest())
        }
        _ => panic!("compaction is not missing proof"),
    };
}
#[test]
fn unsupported_real_owner_record_remains_an_explicit_bootstrap_obligation() {
    let f = fixture::Fixture::created();
    let mutation = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Patch {
            subject: SUBJECT.into(),
            namespace: "motion.nfo".into(),
            record_id: "other-record".into(),
            schema_fingerprint: metadata::current_schema_fingerprint(&f.store.db, SUBJECT)
                .unwrap()
                .unwrap(),
            expected_revision: 0,
            edits_json: json!({"title":{"op":"set","value":"other"}}).to_string(),
            evidence_json: "{}".into(),
        },
    };
    let command = fixture::envelope(
        &f.store,
        "other-record",
        json!({"operation":"metadata","mutation":mutation}),
    );
    let source = MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::Source {
            namespace: "motion.nfo".into(),
            record_id: "other-record".into(),
            kind: SourceKind::Provider,
        },
    };
    assert!(matches!(
        commands::execute_with_authority(
            &f.store,
            &command,
            CatalogAuthority::new(fixture::authority(true)).with_metadata(&source),
            fixture::policy()
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let f = f.install();
    let before = migration::snapshot(&f.store.db).unwrap();
    let inventory = capture(&f.store);
    assert_eq!(inventory.subjects.len(), 1);
    assert!(
        inventory.subjects[SUBJECT]
            .history
            .contains_key(&("other-record".into(), 1))
    );
    assert!(
        inventory.subjects[SUBJECT]
            .obligations()
            .contains(&Obligation::UnsupportedRecord)
    );
    assert!(inventory.require_complete().is_err());
    let error = f
        .store
        .transaction_detailed(|_| bootstrap::prepare_in(&f.store, &[]))
        .unwrap_err();
    assert!(error.to_string().contains("UnsupportedRecord"));
    assert_eq!(before, migration::snapshot(&f.store.db).unwrap());
}
#[test]
fn descriptive_history_only_gap_and_missing_receipt_are_not_dropped() {
    let f = fixture::Fixture::created();
    replace(&f.store, "second", 1);
    let mut inventory = capture(&f.store);
    // Deliberately modify descriptions, not native rows/tokens used for writes.
    // Real owner forbids deleting heads or immutable history. Classifier coverage
    // must not require bypassing those guards to synthesize an impossible owner.
    let description = inventory.subjects.get_mut(SUBJECT).unwrap();
    description.sources.clear();
    description.history.remove(&("document".into(), 1));
    description
        .history
        .get_mut(&("document".into(), 2))
        .unwrap()
        .receipt = ReceiptCommitment::Missing;
    description.obligations.clear();
    description.classify(&inventory.catalog_id);
    assert!(description.obligations.contains(&Obligation::MissingSource));
    assert!(description.obligations.contains(&Obligation::HistoryGap {
        expected: 1,
        actual: 2
    }));
    assert!(
        description
            .obligations
            .contains(&Obligation::MissingReceipt)
    );
    assert_eq!(description.first_revision().unwrap().revision(), 2);
    assert_eq!(inventory.subjects.len(), 1);
    assert!(inventory.require_complete().is_err());
}
#[test]
fn bootstrap_consumes_complete_capture_and_rejects_stale_generation_without_effects() {
    let f = fixture::Fixture::created().install();
    let plan = f
        .store
        .transaction_detailed(|_| {
            let proof = f.proof_in()?;
            bootstrap::prepare_in(&f.store, &[proof])
        })
        .unwrap();
    let stale = fixture::envelope(&f.store, "stale-bootstrap", plan.payload());
    // An actual independent Pins owner advances the native generation.
    fixture::pin(
        &f.store,
        "unrelated-pin",
        "other-subject",
        0,
        crate::catalog_pins::PinDecision::Set {
            value: "tmdb:unrelated".into(),
        },
    )
    .unwrap();
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(
        f.store
            .transaction_detailed(|_| bootstrap::apply_in(&f.store, &stale, plan))
            .is_err()
    );
    assert_eq!(before, migration::snapshot(&f.store.db).unwrap());
}
#[test]
fn inherited_budget_and_borrowed_row_bounds_refuse_without_partial_inventory() {
    let f = fixture::Fixture::created();
    f.store
        .transaction_detailed(|_| {
            let shared = f.store.catalog_operation_budget()?;
            let before = shared.usage();
            let first = capture_in(&f.store)?;
            let middle = shared.usage();
            let second = capture_in(&f.store)?;
            let after = shared.usage();
            assert_eq!(first.digest(), second.digest());
            assert!(middle.bytes > before.bytes && after.bytes > middle.bytes);
            assert!(middle.work > before.work && after.work > middle.work);
            assert!(
                shared
                    .charge(BudgetCharge {
                        bytes: usize::MAX,
                        ..Default::default()
                    })
                    .is_err()
            );
            assert!(capture_in(&f.store).is_err());
            Err::<(), _>(refused("intentional budget rollback"))
        })
        .unwrap_err();
    f.store
        .transaction_detailed(|_| {
            let mut budget = ProofBudget::new(f.store.catalog_operation_budget()?);
            let oversized =
                "x".repeat(catabolic_core::catalog_metadata::MAX_METADATA_SNAPSHOT_BYTES);
            let mut query = f.store.db.prepare("SELECT ?1,'unrelated','record'")?;
            let mut rows = query.query([&oversized])?;
            assert!(charge_row(rows.next()?.unwrap(), &[0, 1, 2], 0, &mut budget).is_err());
            Err::<(), _>(refused("intentional borrowed-row rollback"))
        })
        .unwrap_err();
}

fn assign(store: &Store, name: &str, item: Option<&str>, file: Option<&str>, remove: bool) {
    let names = [json!(name)];
    let items = item.into_iter().map(str::to_owned).collect::<Vec<_>>();
    let files = file.into_iter().map(str::to_owned).collect::<Vec<_>>();
    crate::tagging::assign(
        store,
        crate::tagging::Assignment {
            names: &names,
            items: &items,
            files: &files,
            source: &json!("motion.nfo"),
            confidence: &serde_json::Value::Null,
            note: &json!(""),
            remove,
        },
    )
    .unwrap();
}
#[test]
fn actual_generic_item_tag_only_active_and_inactive_are_bootstrap_obligations() {
    for inactive in [false, true] {
        let f = fixture::Fixture::created();
        crate::tagging::put(&f.store, &json!("tag-only"), None).unwrap();
        assign(&f.store, "tag-only", Some("other-subject"), None, false);
        if inactive {
            assign(&f.store, "tag-only", Some("other-subject"), None, true);
        }
        let f = f.install();
        let before = migration::snapshot(&f.store.db).unwrap();
        let inventory = capture(&f.store);
        assert_eq!(inventory.subjects.len(), 2);
        let subject = &inventory.subjects["other-subject"];
        assert_eq!(subject.assertions.len(), 1);
        assert!(subject.obligations().contains(&Obligation::MissingSource));
        assert!(subject.obligations().contains(&Obligation::MissingHistory));
        assert!(inventory.require_complete().is_err());
        assert!(
            f.store
                .transaction_detailed(|_| bootstrap::prepare_in(&f.store, &[]))
                .unwrap_err()
                .to_string()
                .contains("MissingSource")
        );
        assert_eq!(before, migration::snapshot(&f.store.db).unwrap());
    }
}
#[test]
fn actual_file_assertions_stay_in_separate_unsupported_inventory() {
    // This real physical-import fixture requires the explicitly qualified closed
    // HostSession helper, just like the existing physical provenance tests.
    let f = fixture::Fixture::imported(true);
    let file: String = f
        .store
        .db
        .query_row("SELECT id FROM files WHERE path='movie.mkv'", [], |r| {
            r.get(0)
        })
        .unwrap();
    crate::tagging::put(&f.store, &json!("file-only"), None).unwrap();
    assign(&f.store, "file-only", None, Some(&file), false);
    let active = capture(&f.store);
    assert!(active.files.contains_key(&file));
    assert!(!active.subjects.contains_key(&file));
    assert!(
        active
            .require_complete()
            .unwrap_err()
            .to_string()
            .contains("UnsupportedFileTags")
    );
    assign(&f.store, "file-only", None, Some(&file), true);
    let f = f.install();
    let before = migration::snapshot(&f.store.db).unwrap();
    let inactive = capture(&f.store);
    assert!(inactive.files.contains_key(&file));
    assert!(
        f.store
            .transaction_detailed(|_| bootstrap::prepare_in(&f.store, &[]))
            .unwrap_err()
            .to_string()
            .contains("UnsupportedFileTags")
    );
    assert_eq!(before, migration::snapshot(&f.store.db).unwrap());
    // Schema41's closed source guard now refuses this low-level deletion. It
    // cannot erase the unsupported historical file ownership obligation.
    assert!(
        f.store
            .transaction(|db| {
                db.execute(
                    "DELETE FROM file_tags WHERE file_id=? AND source='motion.nfo'",
                    [&file],
                )?;
                Ok(())
            })
            .is_err()
    );
    assert_eq!(before, migration::snapshot(&f.store.db).unwrap());
    let retained = capture(&f.store);
    assert!(retained.files().contains_key(&file));
    assert!(
        retained
            .require_complete()
            .unwrap_err()
            .to_string()
            .contains("UnsupportedFileTags")
    );
}
#[test]
fn actual_generic_merge_is_retained_as_unsupported_history_not_id_proof() {
    use crate::catalog_tag_mutation::{self as tags, TagMerge, TagMutation};
    let f = fixture::Fixture::created();
    let source = crate::tagging::resolve_tag(&f.store.db, &json!("old")).unwrap();
    crate::tagging::put(&f.store, &json!("destination"), None).unwrap();
    let target = crate::tagging::resolve_tag(&f.store.db, &json!("destination")).unwrap();
    let grant = TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [TagSubject::Item(SUBJECT.into())].into(),
        allow_global_merge: true,
    };
    let preview = tags::preview_merge(&f.store, &grant, HOST, &source, &target).unwrap();
    let mutation = TagMutation::Merge {
        contract_version: 1,
        merge: TagMerge {
            source_tag_id: source.clone(),
            target_tag_id: target,
            expected_source_revision: preview.body.affected.source_revision,
            expected_target_revision: preview.body.affected.target_revision,
            preview_digest: preview.digest,
            collisions: vec![],
        },
    };
    let command = fixture::envelope(
        &f.store,
        "merge-nfo-vocabulary",
        json!({"operation":"tags","mutation":mutation}),
    );
    assert!(matches!(
        commands::execute_with_authority(
            &f.store,
            &command,
            CatalogAuthority::new(fixture::authority(true)).with_tags(&grant),
            fixture::policy()
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let f = f.install();
    let before = migration::snapshot(&f.store.db).unwrap();
    let inventory = capture(&f.store);
    assert!(inventory.tag_merges.contains_key(&source));
    assert!(
        inventory.subjects[SUBJECT]
            .obligations()
            .contains(&Obligation::UnsupportedTagMerge)
    );
    assert!(
        f.store
            .transaction_detailed(|_| bootstrap::prepare_in(&f.store, &[]))
            .unwrap_err()
            .to_string()
            .contains("UnsupportedTagMerge")
    );
    assert_eq!(before, migration::snapshot(&f.store.db).unwrap());
}

#[test]
fn descriptive_public_capture_reuses_exact_inventory_and_rejects_fence_or_admission_changes() {
    use crate::catalog_nfo_attribution::inventory;
    let f = fixture::Fixture::created();
    let internal = capture(&f.store);
    let mut expected = crate::catalog_projection::TrustedProjectionAuthority {
        catalog_id: internal.catalog_id().into(),
        incarnation: internal.incarnation().into(),
        expected_generation: internal.generation(),
    };
    let before = migration::snapshot(&f.store.db).unwrap();
    let public = inventory::capture(&f.store, &expected).unwrap();
    assert_eq!(public.digest(), internal.digest());
    assert_eq!(
        serde_json::to_value(&public).unwrap(),
        serde_json::to_value(&internal).unwrap()
    );
    assert_eq!(
        public.subjects()[SUBJECT]
            .first_revision()
            .unwrap()
            .command(),
        internal.subjects()[SUBJECT]
            .first_revision()
            .unwrap()
            .command()
    );
    expected.expected_generation -= 1;
    assert!(inventory::capture(&f.store, &expected).is_err());
    expected.expected_generation += 1;
    expected.catalog_id.push('x');
    assert!(inventory::capture(&f.store, &expected).is_err());
    expected.catalog_id.pop();
    expected.incarnation.push('x');
    assert!(inventory::capture(&f.store, &expected).is_err());
    expected.incarnation.pop();
    f.store
        .transaction(|_| {
            assert!(inventory::capture(&f.store, &expected).is_err());
            Ok(())
        })
        .unwrap();
    assert_eq!(before, migration::snapshot(&f.store.db).unwrap());
    let f = f.install();
    assert!(
        inventory::capture(&f.store, &expected).is_err(),
        "test candidate41 does not broaden normal public read admission"
    );
}

#[test]
fn descriptive_public_capture_preserves_tag_only_obligations_and_inactive_assertions() {
    use crate::catalog_nfo_attribution::inventory;
    let f = fixture::Fixture::created();
    crate::tagging::put(&f.store, &json!("descriptive-tag-only"), None).unwrap();
    assign(
        &f.store,
        "descriptive-tag-only",
        Some("other-subject"),
        None,
        false,
    );
    assign(
        &f.store,
        "descriptive-tag-only",
        Some("other-subject"),
        None,
        true,
    );
    let captured = capture(&f.store);
    let before = migration::snapshot(&f.store.db).unwrap();
    let public = inventory::capture(
        &f.store,
        &crate::catalog_projection::TrustedProjectionAuthority {
            catalog_id: captured.catalog_id().into(),
            incarnation: captured.incarnation().into(),
            expected_generation: captured.generation(),
        },
    )
    .unwrap();
    let subject = &public.subjects()["other-subject"];
    assert!(subject.source().is_none());
    assert!(
        subject
            .obligations()
            .contains(&inventory::Obligation::MissingSource)
    );
    assert!(
        subject
            .obligations()
            .contains(&inventory::Obligation::MissingHistory)
    );
    assert_eq!(subject.assertions().len(), 1);
    assert!(!subject.assertions().values().next().unwrap().active());
    assert_eq!(public.digest(), captured.digest());
    assert_eq!(before, migration::snapshot(&f.store.db).unwrap());
}

#[test]
fn low_level_deleted_tag_only_assertion_is_a_global_obligation_not_absence() {
    let f = fixture::Fixture::created();
    crate::tagging::put(&f.store, &json!("erased-tag-only"), None).unwrap();
    assign(
        &f.store,
        "erased-tag-only",
        Some("other-subject"),
        None,
        false,
    );
    let id: String = f
        .store
        .db
        .query_row(
            "SELECT id FROM item_tags WHERE item_id='other-subject' AND source='motion.nfo'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // An adversarial low-level writer is not a typed NFO authority. Leave every
    // permanent-head trigger installed: the inventory must expose its trace.
    f.store
        .transaction(|db| {
            db.execute("DELETE FROM item_tags WHERE id=?", [&id])?;
            Ok(())
        })
        .unwrap();
    let current = capture(&f.store);
    assert!(!current.subjects().contains_key("other-subject"));
    assert!(
        matches!(current.global_obligations(),[Obligation::UnresolvedAssertionTombstone {entity_kind,entity_key,revision:2,..}] if entity_kind=="item_tags" && entity_key==&json!([id]).to_string())
    );
    let public = crate::catalog_nfo_attribution::inventory::capture(
        &f.store,
        &crate::catalog_projection::TrustedProjectionAuthority {
            catalog_id: current.catalog_id().into(),
            incarnation: current.incarnation().into(),
            expected_generation: current.generation(),
        },
    )
    .unwrap();
    assert_eq!(public.global_obligations(), current.global_obligations());
    assert_eq!(public.digest(), current.digest());
    let f = f.install();
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(
        f.store
            .transaction_detailed(|_| bootstrap::prepare_in(&f.store, &[]))
            .unwrap_err()
            .to_string()
            .contains("UnresolvedAssertionTombstone")
    );
    assert_eq!(before, migration::snapshot(&f.store.db).unwrap());
}

fn non_nfo_collision(store: &Store) -> (String, String, CommandEnvelope) {
    use crate::catalog_tag_mutation::{
        self as tags, AssertionCollision, CollisionEvidence, TagMerge, TagMutation,
    };
    for name in ["collision-source", "collision-target"] {
        crate::tagging::put(store, &json!(name), None).unwrap();
        crate::tagging::assign(
            store,
            crate::tagging::Assignment {
                names: &[json!(name)],
                items: &["other-subject".into()],
                files: &[],
                source: &json!("manual"),
                confidence: &serde_json::Value::Null,
                note: &json!(""),
                remove: false,
            },
        )
        .unwrap();
    }
    let source = crate::tagging::resolve_tag(&store.db, &json!("collision-source")).unwrap();
    let target = crate::tagging::resolve_tag(&store.db, &json!("collision-target")).unwrap();
    let assertion: String = store
        .db
        .query_row(
            "SELECT id FROM item_tags WHERE tag_id=? AND source='manual'",
            [&source],
            |r| r.get(0),
        )
        .unwrap();
    let grant = TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [TagSubject::Item("other-subject".into())].into(),
        allow_global_merge: true,
    };
    let preview = tags::preview_merge(store, &grant, HOST, &source, &target).unwrap();
    // This exercises the existing typed wrapper against the extracted Value
    // path, including null confidence, empty notes and complete nested mappings.
    let body = serde_json::to_value(&preview.body).unwrap();
    assert!(body["affected"]["source_assertions"][0]["confidence"].is_null());
    assert_eq!(
        tags::canonical_preview_value_digest(&body).unwrap(),
        preview.digest
    );
    let reordered = serde_json::Value::Object(
        body.as_object()
            .unwrap()
            .iter()
            .rev()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    assert_eq!(
        tags::canonical_preview_value_digest(&reordered).unwrap(),
        preview.digest
    );
    let mutation = TagMutation::Merge {
        contract_version: 1,
        merge: TagMerge {
            source_tag_id: source.clone(),
            target_tag_id: target.clone(),
            expected_source_revision: preview.body.affected.source_revision,
            expected_target_revision: preview.body.affected.target_revision,
            preview_digest: preview.digest,
            collisions: vec![AssertionCollision {
                subject: TagSubject::Item("other-subject".into()),
                source_namespace: "manual".into(),
                evidence: CollisionEvidence::Target,
            }],
        },
    };
    let command = fixture::envelope(
        store,
        "known-non-nfo-collision",
        json!({"operation":"tags","mutation":mutation}),
    );
    assert!(matches!(
        commands::execute_with_authority(
            store,
            &command,
            CatalogAuthority::new(fixture::authority(true)).with_tags(&grant),
            fixture::policy()
        )
        .unwrap(),
        CommandOutcome::Committed { .. }
    ));
    (assertion, target, command)
}
#[test]
fn exact_non_nfo_merge_collision_accounts_revision_two_without_later_generation_restriction() {
    let f = fixture::Fixture::created();
    let (id, _, _) = non_nfo_collision(&f.store);
    let head:(i64,bool)=f.store.db.query_row("SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='item_tags' AND entity_key=json_array(?)",[&id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(head, (2, true));
    crate::tagging::put(&f.store, &json!("unrelated-later-vocabulary"), None).unwrap();
    let current = capture(&f.store);
    assert!(current.global_obligations().is_empty());
    assert!(
        current.tag_merges.is_empty(),
        "non-NFO merge is not an NFO lineage grant or obligation"
    );
    current.require_complete().unwrap();
    let f = f.install();
    f.bootstrap();
}
#[test]
fn deleted_reinserted_deleted_assertion_cannot_reuse_old_merge_and_counter_cannot_reset() {
    let f = fixture::Fixture::created();
    let (id, target, _) = non_nfo_collision(&f.store);
    f.store.transaction(|db| {
        db.execute("INSERT INTO item_tags(id,item_id,tag_id,source,confidence,note,active,created_at,updated_at) VALUES(?1,'other-subject',?2,'motion.nfo',NULL,'adversarial',1,'2026-01-01T00:00:00+00:00','2026-01-01T00:00:00+00:00')",params![id,target])?;
        db.execute("DELETE FROM item_tags WHERE id=?",[&id])?;
        Ok(())
    }).unwrap();
    for sql in [
        "DELETE FROM catalog_entity_revisions WHERE entity_kind='item_tags' AND entity_key=json_array(?1)",
        "UPDATE catalog_entity_revisions SET revision=2 WHERE entity_kind='item_tags' AND entity_key=json_array(?1)",
        "INSERT OR REPLACE INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('item_tags',json_array(?1),2,1)",
    ] {
        assert!(
            f.store
                .transaction(|db| {
                    db.execute(sql, [&id])?;
                    Ok(())
                })
                .is_err(),
            "{sql}"
        );
    }
    let current = capture(&f.store);
    assert!(
        matches!(current.global_obligations(),[Obligation::UnresolvedAssertionTombstone {revision:4,reason,..}] if reason.contains("counter"))
    );
    let f = f.install();
    let before = migration::snapshot(&f.store.db).unwrap();
    assert!(
        f.store
            .transaction_detailed(|_| bootstrap::prepare_in(&f.store, &[]))
            .unwrap_err()
            .to_string()
            .contains("UnresolvedAssertionTombstone")
    );
    assert_eq!(before, migration::snapshot(&f.store.db).unwrap());
}
#[test]
fn exact_merge_authentication_rejects_changed_complete_snapshot_and_request() {
    let f = fixture::Fixture::created();
    let (_, _, command) = non_nfo_collision(&f.store);
    let before = migration::snapshot(&f.store.db).unwrap();
    let raw: String = f
        .store
        .db
        .query_row(
            "SELECT snapshot_json FROM catalog_tag_merges WHERE command_operation_id=?",
            [command.key().operation_id()],
            |r| r.get(0),
        )
        .unwrap();
    for change_request in [false, true] {
        let mut changed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        if change_request {
            changed["request"]["collisions"][0]["evidence"] = json!("source");
        } else {
            changed["preview"]["body"]["affected"]["source_assertions"][0]["note"] =
                json!("not the immutable snapshot");
        }
        let candidate = serde_json::to_string(&changed).unwrap();
        // Supply bounded candidate data to the same authenticator. Do not edit
        // immutable history or receipt rows to manufacture a proof fixture.
        f.store.transaction(|_| {
            let mut budget=ProofBudget::new(f.store.catalog_operation_budget()?);
            budget.raw_json(&candidate,64)?;
            let mut query=f.store.db.prepare("SELECT source_tag_id,original_target_tag_id,?1,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_tag_merges WHERE command_operation_id=?2")?;
            let mut rows=query.query(params![candidate,command.key().operation_id()])?;
            assert!(!tags::authenticate_merge(&f.store,rows.next()?.unwrap(),&changed,&mut budget)?);
            Ok(())
        }).unwrap();
    }
    assert_eq!(before, migration::snapshot(&f.store.db).unwrap());
}
