// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Schema35 preparation fixtures using actual Store/migrations/dispatcher.
//! Unrestricted FTS below is privileged diagnostic test access, never scoped query.
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
use catabolic_store::{
    Store,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
    },
    catalog_search_index::{self as index, SearchIndexErrorCode, SearchIndexLimits},
    curation, migration,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
const HOST: &str = "search-index-fixture";
struct Fixture {
    root: tempfile::TempDir,
    path: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        Self::at(35)
    }
    fn at(version: u32) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        migration::initialize_native_at(&path, version).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        Self { root, path, store }
    }
    fn put(&self, id: &str, metadata: Value) {
        curation::item_put(
            &self.store,
            &json!({"id":id,"kind":"movie","metadata":metadata}),
        )
        .unwrap();
    }
    fn ready(&self) {
        assert!(
            index::rebuild(&self.store, SearchIndexLimits::default(), None)
                .unwrap()
                .ready
        );
    }
}
fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
        .unwrap()
}
fn count(store: &Store, table: &str) -> i64 {
    store
        .db
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn matches(store: &Store, query: &str) -> Vec<String> {
    let mut s=store.db.prepare("SELECT d.item_id FROM catalog_search_fts f JOIN catalog_search_documents d ON d.rowid=f.rowid WHERE catalog_search_fts MATCH ? ORDER BY d.item_id").unwrap();
    s.query_map([query], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
}
fn envelope(store: &Store, key: &str, payload: Value) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, HOST, key).unwrap(),
        store.catalog_incarnation.clone().unwrap(),
        generation(store),
        &payload.to_string(),
    )
    .unwrap()
}
fn base() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 1000,
        max_logical_bytes: 16 * 1024 * 1024,
        max_hold_records: 1000,
    }
}
#[test]
fn native35_is_not_ready_until_complete_rebuild_and_derived_rows_do_not_advance_generation() {
    let f = Fixture::new();
    assert!(!index::status(&f.store).unwrap().ready);
    f.put("a", json!({"title":"needle","description":"planet"}));
    assert!(!index::status(&f.store).unwrap().ready);
    let before = generation(&f.store);
    f.ready();
    assert_eq!(generation(&f.store), before);
    assert_eq!(matches(&f.store, "needle"), ["a"]);
    assert_eq!(matches(&f.store, "planet"), ["a"]);
    assert_eq!(count(&f.store, "catalog_search_dirty"), 0);
    drop(f.store);
    let store = Store::open(&f.path, false, false).unwrap();
    assert!(index::status(&store).unwrap().ready);
}
#[test]
fn actual34_to_latest_upgrade_preserves_every_old_table_row_and_stays_not_ready() {
    let f = Fixture::at(34);
    f.put("a", json!({"title":"legacy"}));
    let before = migration::snapshot(&f.store.db).unwrap();
    let generation = generation(&f.store);
    drop(f.store);
    let plan = migration::plan_native(&f.path).unwrap();
    migration::upgrade_native(
        &f.path,
        &plan,
        migration::NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        },
        Some(&f.root.path().join("backup")),
        &|_, _, _| Ok(()),
    )
    .unwrap();
    let store = Store::open(&f.path, true, false).unwrap();
    assert_eq!(store.schema_version, migration::NATIVE_SCHEMA_VERSION);
    migration::preservation(&store.db, &before).unwrap();
    assert_eq!(generation, super_generation(&store));
    assert!(!index::status(&store).unwrap().ready);
    index::rebuild(&store, SearchIndexLimits::default(), None).unwrap();
    assert_eq!(matches(&store, "legacy"), ["a"]);
}
fn super_generation(store: &Store) -> i64 {
    generation(store)
}
#[test]
fn cancellation_and_budget_failures_leave_complete_previous_index_unchanged() {
    let f = Fixture::new();
    f.put("a", json!({"title":"alpha"}));
    f.put("b", json!({"title":"beta"}));
    let before = migration::snapshot(&f.store.db).unwrap();
    let cancel = AtomicBool::new(true);
    assert_eq!(
        index::rebuild(&f.store, SearchIndexLimits::default(), Some(&cancel))
            .unwrap_err()
            .code,
        SearchIndexErrorCode::Cancelled
    );
    for (limits, expected) in [
        (
            SearchIndexLimits {
                max_items: 1,
                ..SearchIndexLimits::default()
            },
            SearchIndexErrorCode::ItemBudget,
        ),
        (
            SearchIndexLimits {
                max_values: 0,
                ..SearchIndexLimits::default()
            },
            SearchIndexErrorCode::WorkBudget,
        ),
        (
            SearchIndexLimits {
                max_text_bytes: 1,
                ..SearchIndexLimits::default()
            },
            SearchIndexErrorCode::ByteBudget,
        ),
        (
            SearchIndexLimits {
                max_steps: 0,
                ..SearchIndexLimits::default()
            },
            SearchIndexErrorCode::WorkBudget,
        ),
        (
            SearchIndexLimits {
                timeout: Duration::ZERO,
                ..SearchIndexLimits::default()
            },
            SearchIndexErrorCode::Timeout,
        ),
    ] {
        assert_eq!(
            index::rebuild(&f.store, limits, None).unwrap_err().code,
            expected
        );
        migration::preservation(&f.store.db, &before).unwrap();
        assert!(!index::status(&f.store).unwrap().ready);
    }
    f.ready();
}
#[test]
fn legacy_item_updates_and_delete_maintain_real_fts_in_the_source_transaction() {
    let f = Fixture::new();
    f.put(
        "a",
        json!({"title":"beforeword","description":["neverflatten"]}),
    );
    f.ready();
    assert!(matches(&f.store, "neverflatten").is_empty());
    f.put("a", json!({"title":"afterword"}));
    assert!(matches(&f.store, "beforeword").is_empty());
    assert_eq!(matches(&f.store, "afterword"), ["a"]);
    assert!(index::status(&f.store).unwrap().ready);
    f.store
        .transaction(|db| {
            db.execute("DELETE FROM items WHERE id='a'", [])?;
            Ok(())
        })
        .unwrap();
    assert!(matches(&f.store, "afterword").is_empty());
    assert_eq!(count(&f.store, "catalog_search_documents"), 0);
}
#[test]
fn dirty_barrier_rejects_unfinalized_raw_commit() {
    let f = Fixture::new();
    let before = generation(&f.store);
    assert!(
        f.store
            .db
            .execute("INSERT INTO items VALUES('autocommit','movie','{}')", [])
            .is_err()
    );
    assert_eq!(count(&f.store, "items"), 0);
    assert_eq!(count(&f.store, "catalog_search_dirty"), 0);
    assert_eq!(generation(&f.store), before);

    f.store
        .db
        .execute_batch(
            "BEGIN IMMEDIATE;INSERT INTO items VALUES('raw','movie','{\"title\":\"rawtext\"}')",
        )
        .unwrap();
    assert_eq!(count(&f.store, "catalog_search_dirty"), 1);
    assert!(f.store.db.execute_batch("COMMIT").is_err());
    f.store.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(generation(&f.store), before);
    assert_eq!(count(&f.store, "items"), 0);
}
#[test]
fn late_flush_failure_rolls_back_real_document_writes_source_and_generation() {
    let f = Fixture::new();
    f.ready();
    let before = generation(&f.store);
    let observed = Arc::new(AtomicUsize::new(0));
    let copy = observed.clone();
    f.store
        .db
        .update_hook(Some(move |_, _: &str, table: &str, _| {
            if table == "catalog_search_documents" {
                copy.fetch_add(1, Ordering::SeqCst);
            }
        }))
        .unwrap();
    let result = f.store.transaction(|db| {
        db.execute(
            "INSERT INTO items VALUES('a','movie',?)",
            [json!({"title":"firstwrite"}).to_string()],
        )?;
        db.execute(
            "INSERT INTO items VALUES('z','movie',?)",
            [json!({"title":"x".repeat(65537)}).to_string()],
        )?;
        Ok(())
    });
    assert!(result.unwrap_err().0.contains("catalog_search_byte_budget"));
    assert!(observed.load(Ordering::SeqCst) > 0);
    assert_eq!(count(&f.store, "items"), 0);
    assert_eq!(count(&f.store, "catalog_search_documents"), 0);
    assert_eq!(count(&f.store, "catalog_search_dirty"), 0);
    assert_eq!(generation(&f.store), before);
    assert!(index::status(&f.store).unwrap().ready);
}
#[test]
fn dispatcher_replay_has_no_new_index_writes_and_late_receipt_failure_is_atomic() {
    let f = Fixture::new();
    f.ready();
    let command = envelope(
        &f.store,
        "typed-item",
        json!({"operation":"item_put","item":{"id":"a","kind":"movie","metadata":{"title":"durableword"}}}),
    );
    let outcome = commands::execute(&f.store, &command, base(), policy()).unwrap();
    assert!(matches!(outcome, CommandOutcome::Committed { .. }));
    assert_eq!(matches(&f.store, "durableword"), ["a"]);
    let before = migration::snapshot(&f.store.db).unwrap();
    let replay_writes = Arc::new(AtomicUsize::new(0));
    let replay_copy = replay_writes.clone();
    f.store
        .db
        .update_hook(Some(move |_, _: &str, table: &str, _| {
            if table.starts_with("catalog_search_") {
                replay_copy.fetch_add(1, Ordering::SeqCst);
            }
        }))
        .unwrap();
    assert!(matches!(
        commands::execute(&f.store, &command, base(), policy()).unwrap(),
        CommandOutcome::Replay { .. }
    ));
    migration::preservation(&f.store.db, &before).unwrap();
    assert_eq!(replay_writes.load(Ordering::SeqCst), 0);
    let c = envelope(
        &f.store,
        "late",
        json!({"operation":"item_put","item":{"id":"b","kind":"movie","metadata":{"title":"rollbackword"}}}),
    );
    let overhead = 256
        + c.key().catalog_id().len()
        + c.key().host_namespace().len()
        + c.key().operation_id().len()
        + 64
        + c.admission_incarnation().len();
    let tiny = ReceiptPolicy {
        max_logical_bytes: commands::receipt_usage(&f.store).unwrap().logical_bytes
            + overhead as u64,
        ..policy()
    };
    let observed = Arc::new(AtomicUsize::new(0));
    let copy = observed.clone();
    f.store
        .db
        .update_hook(Some(move |_, _: &str, table: &str, _| {
            if table == "catalog_search_documents" {
                copy.fetch_add(1, Ordering::SeqCst);
            }
        }))
        .unwrap();
    assert_eq!(
        commands::execute(&f.store, &c, base(), tiny).unwrap(),
        CommandOutcome::ReceiptStorageExhausted
    );
    assert!(
        observed.load(Ordering::SeqCst) > 0,
        "late receipt check follows real derived writes"
    );
    migration::preservation(&f.store.db, &before).unwrap();
    assert!(matches(&f.store, "rollbackword").is_empty());
}

#[test]
fn typed_metadata_conflict_never_indexes_a_winner_or_retained_legacy_title() {
    use catabolic_core::catalog_metadata::SourceKind;
    use catabolic_store::catalog_metadata_store::{
        MetadataAccess, MetadataAuthority, MetadataChange, MetadataMutation, PolicyField,
        PolicyNormalization, PolicyValueType,
    };
    let f = Fixture::new();
    f.put("a", json!({"title":"legacyword"}));
    f.ready();
    let initialize = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Initialize {
            subject: "a".into(),
            policies: vec![PolicyField {
                field: "title".into(),
                value_type: PolicyValueType::String,
                nullable: true,
                normalization: PolicyNormalization::Exact,
            }],
        },
    };
    let init_grant = MetadataAuthority {
        host_namespace: HOST.into(),
        subject: "a".into(),
        access: MetadataAccess::Initialize,
    };
    commands::execute_with_authority(
        &f.store,
        &envelope(
            &f.store,
            "metadata-init",
            json!({"operation":"metadata","mutation":initialize}),
        ),
        CatalogAuthority::new(base()).with_metadata(&init_grant),
        policy(),
    )
    .unwrap();
    let fingerprint: String = f
        .store
        .db
        .query_row(
            "SELECT schema_fingerprint FROM catalog_metadata_subjects WHERE item_id='a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    for (namespace, title) in [("first", "firstcandidate"), ("second", "secondcandidate")] {
        let grant = MetadataAuthority {
            host_namespace: HOST.into(),
            subject: "a".into(),
            access: MetadataAccess::Source {
                namespace: namespace.into(),
                record_id: "record".into(),
                kind: SourceKind::Provider,
            },
        };
        let mutation = MetadataMutation {
            contract_version: 1,
            change: MetadataChange::Patch {
                subject: "a".into(),
                namespace: namespace.into(),
                record_id: "record".into(),
                schema_fingerprint: fingerprint.clone(),
                expected_revision: 0,
                edits_json: json!({"title":{"op":"set","value":title}}).to_string(),
                evidence_json: "{}".into(),
            },
        };
        commands::execute_with_authority(
            &f.store,
            &envelope(
                &f.store,
                namespace,
                json!({"operation":"metadata","mutation":mutation}),
            ),
            CatalogAuthority::new(base()).with_metadata(&grant),
            policy(),
        )
        .unwrap();
    }
    // R3 retains legacy precedence until an explicit typed reconciliation.
    assert_eq!(matches(&f.store, "legacyword"), ["a"]);
    let reconcile_grant = MetadataAuthority {
        host_namespace: HOST.into(),
        subject: "a".into(),
        access: MetadataAccess::ReconcileLegacy,
    };
    let reconcile = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::ReconcileLegacy {
            subject: "a".into(),
            schema_fingerprint: fingerprint.clone(),
            expected_revision: 1,
            fields: vec!["title".into()],
        },
    };
    commands::execute_with_authority(
        &f.store,
        &envelope(
            &f.store,
            "reconcile",
            json!({"operation":"metadata","mutation":reconcile}),
        ),
        CatalogAuthority::new(base()).with_metadata(&reconcile_grant),
        policy(),
    )
    .unwrap();
    for text in ["legacyword", "firstcandidate", "secondcandidate"] {
        assert!(matches(&f.store, text).is_empty());
    }
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT title FROM catalog_search_documents WHERE item_id='a'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        ""
    );
    assert!(
        f.store
            .db
            .query_row(
                "SELECT resolution_json FROM catalog_metadata_projection WHERE item_id='a'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap()
            .contains("conflict")
    );
    let grant = MetadataAuthority {
        host_namespace: HOST.into(),
        subject: "a".into(),
        access: MetadataAccess::Source {
            namespace: "local".into(),
            record_id: "record".into(),
            kind: SourceKind::Local,
        },
    };
    let mutation = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Patch {
            subject: "a".into(),
            namespace: "local".into(),
            record_id: "record".into(),
            schema_fingerprint: fingerprint,
            expected_revision: 0,
            edits_json: json!({"title":{"op":"set","value":"resolvedword"}}).to_string(),
            evidence_json: "{}".into(),
        },
    };
    commands::execute_with_authority(
        &f.store,
        &envelope(
            &f.store,
            "resolve",
            json!({"operation":"metadata","mutation":mutation}),
        ),
        CatalogAuthority::new(base()).with_metadata(&grant),
        policy(),
    )
    .unwrap();
    assert_eq!(matches(&f.store, "resolvedword"), ["a"]);
    assert!(index::status(&f.store).unwrap().ready);
}
#[test]
fn accepted_pin_set_and_removal_update_search_without_promoting_legacy_evidence() {
    use catabolic_store::catalog_pins::{
        self as pins, PinAuthority, PinBatch, PinChange, PinDecision, PinMutation,
    };
    let f = Fixture::new();
    curation::item_put(
        &f.store,
        &json!({"id":"a","kind":"movie","metadata":{},"identities":{"provider":"legacyidentity"}}),
    )
    .unwrap();
    f.ready();
    assert!(matches(&f.store, "legacyidentity").is_empty());
    let grant = PinAuthority {
        host_namespace: HOST.into(),
        readable: Default::default(),
        mutable: [("provider".into(), ["a".into()].into())].into(),
    };
    for (revision, decision, operation) in [
        (
            0,
            PinDecision::Set {
                value: "acceptedidentity".into(),
            },
            "set",
        ),
        (1, PinDecision::Remove, "remove"),
    ] {
        let batch = PinBatch {
            contract_version: 1,
            changes: vec![PinChange {
                item_id: "a".into(),
                namespace: "provider".into(),
                expected_revision: revision,
                decision,
            }],
        };
        let preview = pins::preview_pins(&f.store, &batch, &grant).unwrap();
        let command = envelope(
            &f.store,
            operation,
            json!({"operation":"pins","mutation":PinMutation{batch,preview_digest:preview.digest}}),
        );
        commands::execute_with_authority(
            &f.store,
            &command,
            CatalogAuthority::new(base()).with_pins(&grant),
            policy(),
        )
        .unwrap();
        assert_eq!(
            matches(&f.store, "acceptedidentity").len(),
            if revision == 0 { 1 } else { 0 }
        );
    }
    assert_eq!(count(&f.store, "catalog_identity_pin_revisions"), 2);
    assert!(index::status(&f.store).unwrap().ready);
}
fn assign(store: &Store, name: &str, item: &str) {
    catabolic_store::tagging::assign(
        store,
        catabolic_store::tagging::Assignment {
            names: &[json!(name)],
            items: &[item.into()],
            files: &[],
            source: &json!("provider"),
            confidence: &json!(0.5),
            note: &json!("retained source"),
            remove: false,
        },
    )
    .unwrap();
}
fn tag_grant() -> catabolic_store::catalog_tag_mutation::TagAuthority {
    use catabolic_store::catalog_tag_mutation::{TagAuthority, TagSubject};
    TagAuthority {
        host_namespace: HOST.into(),
        allowed_subjects: [TagSubject::Item("a".into())].into(),
        allow_global_merge: true,
    }
}
#[test]
fn tag_rename_alias_suppression_and_provider_refresh_update_effective_index() {
    use catabolic_store::{
        catalog_tag_mutation::{SuppressionChange, TagMutation, TagSubject},
        tagging,
    };
    let f = Fixture::new();
    f.put("a", json!({}));
    let tag = tagging::put(&f.store, &json!("oldtag"), None).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assign(&f.store, "oldtag", "a");
    f.ready();
    tagging::rename(&f.store, &json!("oldtag"), &json!("newtag")).unwrap();
    assert_eq!(matches(&f.store, "newtag"), ["a"]);
    assert_eq!(matches(&f.store, "oldtag"), ["a"]);
    tagging::alias(&f.store, &json!("newtag"), &json!("oldtag"), true).unwrap();
    assert!(matches(&f.store, "oldtag").is_empty());
    for (revision, suppressed, key) in [(None, true, "suppress"), (Some(1), false, "unsuppress")] {
        let mutation = TagMutation::SetSuppression {
            contract_version: 1,
            changes: vec![SuppressionChange {
                subject: TagSubject::Item("a".into()),
                canonical_tag_id: tag.clone(),
                expected_revision: revision,
                suppressed,
                reason: "index fixture".into(),
            }],
        };
        commands::execute_with_authority(
            &f.store,
            &envelope(
                &f.store,
                key,
                json!({"operation":"tags","mutation":mutation}),
            ),
            CatalogAuthority::new(base()).with_tags(&tag_grant()),
            policy(),
        )
        .unwrap();
        assign(&f.store, "newtag", "a");
        assert_eq!(matches(&f.store, "newtag").len(), usize::from(!suppressed));
    }
    assert_eq!(count(&f.store, "item_tags"), 1);
    assert_eq!(count(&f.store, "catalog_tag_suppression_revisions"), 2);
}
#[test]
fn tag_merge_reconciles_collisions_and_indexes_canonical_and_retained_names_atomically() {
    use catabolic_store::{
        catalog_tag_mutation::{
            self as tags, AssertionCollision, CollisionEvidence, TagMerge, TagMutation, TagSubject,
        },
        tagging,
    };
    let f = Fixture::new();
    f.put("a", json!({}));
    let a = tagging::put(&f.store, &json!("sourceword"), None).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let b = tagging::put(&f.store, &json!("targetword"), None).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assign(&f.store, "sourceword", "a");
    assign(&f.store, "targetword", "a");
    f.ready();
    let preview = tags::preview_merge(&f.store, &tag_grant(), HOST, &a, &b).unwrap();
    let mutation = TagMutation::Merge {
        contract_version: 1,
        merge: TagMerge {
            source_tag_id: a,
            target_tag_id: b,
            expected_source_revision: preview.body.affected.source_revision,
            expected_target_revision: preview.body.affected.target_revision,
            preview_digest: preview.digest,
            collisions: vec![AssertionCollision {
                subject: TagSubject::Item("a".into()),
                source_namespace: "provider".into(),
                evidence: CollisionEvidence::Source,
            }],
        },
    };
    commands::execute_with_authority(
        &f.store,
        &envelope(
            &f.store,
            "merge-tags",
            json!({"operation":"tags","mutation":mutation}),
        ),
        CatalogAuthority::new(base()).with_tags(&tag_grant()),
        policy(),
    )
    .unwrap();
    assert_eq!(count(&f.store, "item_tags"), 1);
    assert_eq!(count(&f.store, "catalog_tag_merges"), 1);
    assert_eq!(matches(&f.store, "sourceword"), ["a"]);
    assert_eq!(matches(&f.store, "targetword"), ["a"]);
    assert!(index::status(&f.store).unwrap().ready);
}
fn logical(
    store: &Store,
    key: &str,
    change: catabolic_store::catalog_logical_edits::LogicalChange,
    new_items: Vec<catabolic_store::catalog_logical_edits::NewItem>,
) {
    use catabolic_store::catalog_logical_edits::{
        self as logical, LogicalEditAuthority, LogicalEditRequest,
    };
    let grant = LogicalEditAuthority {
        host_namespace: HOST.into(),
        item_ids: ["a", "b", "c", "left", "right"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        new_item_ids: ["left".into(), "right".into()].into(),
    };
    let request = LogicalEditRequest {
        contract_version: 1,
        change,
        new_items,
        association_clones: vec![],
        contexts: vec![],
    };
    let preview = logical::preview_logical_edit(store, request, &grant, None).unwrap();
    commands::execute_with_authority(
        store,
        &envelope(
            store,
            key,
            json!({"operation":"logical_edit","reviewed":preview.reviewed}),
        ),
        CatalogAuthority::new(base()).with_logical_edits(&grant),
        policy(),
    )
    .unwrap();
}
#[test]
fn logical_alias_chain_keeps_original_path_dependencies_and_never_moves_retired_text() {
    use catabolic_store::catalog_logical_edits::LogicalChange;
    let f = Fixture::new();
    f.put("a", json!({"title":"historicaltitle"}));
    f.put("b", json!({"title":"middletitle"}));
    f.put("c", json!({"title":"currenttitle"}));
    f.ready();
    logical(
        &f.store,
        "merge-a",
        LogicalChange::Merge {
            sources: vec!["a".into()],
            survivor: "b".into(),
        },
        vec![],
    );
    assert!(matches(&f.store, "historicaltitle").is_empty());
    assert_eq!(count(&f.store, "catalog_search_documents"), 2);
    logical(
        &f.store,
        "merge-b",
        LogicalChange::Merge {
            sources: vec!["b".into()],
            survivor: "c".into(),
        },
        vec![],
    );
    assert!(matches(&f.store, "middletitle").is_empty());
    assert_eq!(count(&f.store, "catalog_search_documents"), 1);
    let path:String=f.store.db.query_row("SELECT required_subjects_json FROM catalog_search_values WHERE item_id='c' AND rank_class='aliases' AND value_text='a'",[],|r|r.get(0)).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&path).unwrap(),
        json!(["a", "b", "c"])
    );
    assert_eq!(count(&f.store, "items"), 3);
    assert_eq!(matches(&f.store, "currenttitle"), ["c"]);
}
#[test]
fn split_retirement_removes_ancestor_aliases_without_selecting_a_child() {
    use catabolic_store::catalog_logical_edits::{LogicalChange, NewItem};
    let f = Fixture::new();
    f.put("a", json!({"title":"historicaltitle"}));
    f.put("b", json!({"title":"splittitle"}));
    f.ready();
    logical(
        &f.store,
        "merge",
        LogicalChange::Merge {
            sources: vec!["a".into()],
            survivor: "b".into(),
        },
        vec![],
    );
    logical(
        &f.store,
        "split",
        LogicalChange::Split {
            source: "b".into(),
            children: vec!["left".into(), "right".into()],
        },
        vec![
            NewItem {
                id: "left".into(),
                kind: "movie".into(),
            },
            NewItem {
                id: "right".into(),
                kind: "movie".into(),
            },
        ],
    );
    assert_eq!(count(&f.store, "catalog_search_documents"), 2);
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT count(*) FROM catalog_search_values WHERE rank_class='aliases'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert!(matches(&f.store, "historicaltitle OR splittitle").is_empty());
    assert_eq!(count(&f.store, "items"), 4);
}

#[test]
fn cancellation_after_real_rebuild_writes_rolls_back_previous_ready_index() {
    let f = Fixture::new();
    f.put("a", json!({"title":"beforecancel"}));
    f.ready();
    let before = migration::snapshot(&f.store.db).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    f.store
        .db
        .update_hook(Some(move |_, _: &str, table: &str, _| {
            if table == "catalog_search_documents" {
                signal.store(true, Ordering::Release);
            }
        }))
        .unwrap();
    assert_eq!(
        index::rebuild(&f.store, SearchIndexLimits::default(), Some(&cancel))
            .unwrap_err()
            .code,
        SearchIndexErrorCode::Cancelled
    );
    assert!(cancel.load(Ordering::Acquire));
    migration::preservation(&f.store.db, &before).unwrap();
    assert!(index::status(&f.store).unwrap().ready);
}
#[test]
fn typed_dispatcher_late_index_failure_has_no_source_receipt_or_generation_commit() {
    let f = Fixture::new();
    f.ready();
    let before = migration::snapshot(&f.store.db).unwrap();
    let command = envelope(
        &f.store,
        "too-large",
        json!({"operation":"item_put","item":{"id":"a","kind":"movie","metadata":{"title":"x".repeat(65537)}}}),
    );
    let error = commands::execute(&f.store, &command, base(), policy()).unwrap_err();
    assert!(error.error.0.contains("catalog_search_byte_budget"));
    migration::preservation(&f.store.db, &before).unwrap();
    assert_eq!(count(&f.store, "catalog_command_receipts"), 0);
    assert_eq!(count(&f.store, "items"), 0);
}

#[test]
fn source_phantom_during_flush_cannot_be_cleared_as_if_indexed() {
    let f = Fixture::new();
    f.ready();
    let before = migration::snapshot(&f.store.db).unwrap();
    f.store.db.execute_batch("CREATE TEMP TRIGGER search_phantom AFTER INSERT ON catalog_search_documents WHEN NEW.item_id='a' BEGIN INSERT INTO items VALUES('phantom','movie','{\"title\":\"hiddenphantom\"}'); END;").unwrap();
    let error = curation::item_put(
        &f.store,
        &json!({"id":"a","kind":"movie","metadata":{"title":"alpha"}}),
    )
    .unwrap_err();
    assert!(
        error
            .0
            .contains("source changed during derived index finalization")
    );
    migration::preservation(&f.store.db, &before).unwrap();
    assert_eq!(count(&f.store, "catalog_search_dirty"), 0);
}
#[test]
fn unassigned_vocabulary_change_does_not_leave_an_unflushable_revision() {
    let f = Fixture::new();
    f.ready();
    f.store
        .db
        .execute("INSERT INTO tags VALUES('unused','unused','')", [])
        .unwrap();
    f.store
        .db
        .execute("INSERT INTO tag_names VALUES('unused','unused')", [])
        .unwrap();
    let status = index::status(&f.store).unwrap();
    assert!(status.ready);
    assert_eq!(status.input_revision, status.indexed_revision);
    drop(f.store);
    Store::open(&f.path, true, false).unwrap();
}
