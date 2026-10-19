// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Real Store/native-schema34/durable-dispatch integration tests. Copy into
//! catabolic-store/tests only with the Store-based tag preview wrapper adopted.
//! Separate evidence from the 45 standalone preparation fixtures.
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
use catabolic_store::{
    Store,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, CommandStoreErrorCode, ReceiptPolicy,
        TrustedCommandAuthority,
    },
    catalog_tag_mutation::{
        self as tags, AssertionCollision, CollisionEvidence, SuppressionChange, TagAuthority,
        TagErrorCode, TagMerge, TagMergePreview, TagMutation, TagSubject,
    },
    curation, migration, tagging,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("tags-native.db");
        assert_eq!(
            migration::initialize_native_at(&path, 34).unwrap()["schema"],
            34,
            "integration fixture requires the combined schema34 resource"
        );
        let store = Store::open(&path, true, false).unwrap();
        for id in ["item-a", "item-b"] {
            curation::item_put(
                &store,
                &json!({"id":id,"kind":"video","metadata":{"title":id}}),
            )
            .unwrap();
        }
        store
            .transaction(|db| {
                db.execute("INSERT INTO locations VALUES('tag-fixture')", [])?;
                for file in ["file-a", "file-b"] {
                    db.execute(
                        "INSERT INTO files(id,location,path) VALUES(?,'tag-fixture',?)",
                        params![file, format!("{file}.mkv")],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        Self {
            _root: root,
            path,
            store,
        }
    }
}
fn base() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: "tag-host",
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn grant() -> TagAuthority {
    TagAuthority {
        host_namespace: "tag-host".into(),
        allowed_subjects: [
            TagSubject::Item("item-a".into()),
            TagSubject::Item("item-b".into()),
            TagSubject::File("file-a".into()),
            TagSubject::File("file-b".into()),
        ]
        .into_iter()
        .collect(),
        allow_global_merge: true,
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
fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap()
}
fn tag(store: &Store, name: &str) -> String {
    tagging::put(store, &json!(name), None).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn command(store: &Store, id: &str, mutation: &TagMutation) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, "tag-host", id).unwrap(),
        store.catalog_incarnation.clone().unwrap(),
        generation(store),
        &json!({"operation":"tags","mutation":mutation}).to_string(),
    )
    .unwrap()
}
fn run(store: &Store, request: &CommandEnvelope, scope: &TagAuthority) -> CommandOutcome {
    commands::execute_with_authority(
        store,
        request,
        CatalogAuthority::new(base()).with_tags(scope),
        policy(),
    )
    .unwrap()
}
fn result(outcome: &CommandOutcome) -> &Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        _ => panic!("expected durable result: {outcome:?}"),
    }
}
fn set(
    subject: TagSubject,
    tag_id: &str,
    expected_revision: Option<i64>,
    suppressed: bool,
) -> TagMutation {
    TagMutation::SetSuppression {
        contract_version: 1,
        changes: vec![SuppressionChange {
            subject,
            canonical_tag_id: tag_id.into(),
            expected_revision,
            suppressed,
            reason: "explicit local decision".into(),
        }],
    }
}
fn merge_from(preview: &TagMergePreview, collisions: Vec<AssertionCollision>) -> TagMutation {
    TagMutation::Merge {
        contract_version: 1,
        merge: TagMerge {
            source_tag_id: preview.body.affected.source_tag_id.clone(),
            target_tag_id: preview.body.affected.target_tag_id.clone(),
            expected_source_revision: preview.body.affected.source_revision,
            expected_target_revision: preview.body.affected.target_revision,
            preview_digest: preview.digest.clone(),
            collisions,
        },
    }
}
fn assign(
    store: &Store,
    name: &str,
    subject: TagSubject,
    source: &str,
    note: &str,
    confidence: f64,
    remove: bool,
) {
    let (items, files) = match subject {
        TagSubject::Item(id) => (vec![id], vec![]),
        TagSubject::File(id) => (vec![], vec![id]),
    };
    tagging::assign(
        store,
        tagging::Assignment {
            names: &[json!(name)],
            items: &items,
            files: &files,
            source: &json!(source),
            confidence: &json!(confidence),
            note: &json!(note),
            remove,
        },
    )
    .unwrap();
}
fn count(store: &Store, table: &str) -> i64 {
    assert!(
        [
            "items",
            "files",
            "tags",
            "item_tags",
            "file_tags",
            "catalog_effective_item_tags",
            "catalog_effective_file_tags",
            "catalog_item_tag_suppressions",
            "catalog_file_tag_suppressions",
            "catalog_tag_suppression_revisions",
            "catalog_tag_merges",
            "catalog_tag_redirects",
            "catalog_command_receipts",
            "pragma_foreign_key_check"
        ]
        .contains(&table)
    );
    store
        .db
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn snapshot(store: &Store) -> Vec<(String, Vec<String>)> {
    let tables = store
        .db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let quoted = table.replace('"', "\"\"");
            let mut q = store
                .db
                .prepare(&format!("SELECT * FROM \"{quoted}\""))
                .unwrap();
            let columns = q.column_count();
            let mut rows = q
                .query_map([], |r| {
                    Ok((0..columns)
                        .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                        .collect::<Vec<_>>()
                        .join("|"))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            rows.sort();
            (table, rows)
        })
        .collect()
}
fn overhead_budget(store: &Store, request: &CommandEnvelope) -> ReceiptPolicy {
    // Existing durable receipt accounting: enough for pre-mutation admission,
    // insufficient only once the real serialized operation result is available.
    let overhead = 256
        + request.key().catalog_id().len()
        + request.key().host_namespace().len()
        + request.key().operation_id().len()
        + 64
        + request.admission_incarnation().len();
    ReceiptPolicy {
        max_logical_bytes: commands::receipt_usage(store).unwrap().logical_bytes + overhead as u64,
        ..policy()
    }
}
fn observe_insert(store: &Store, table: &'static str) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&count);
    store
        .db
        .update_hook(Some(
            move |action: rusqlite::hooks::Action, _database: &str, changed: &str, _row: i64| {
                if action == rusqlite::hooks::Action::SQLITE_INSERT && changed == table {
                    observed.fetch_add(1, Ordering::SeqCst);
                }
            },
        ))
        .unwrap();
    count
}

#[test]
fn suppression_lost_reply_replays_original_decision_after_reopen_and_later_unsuppress() {
    let fixture = Fixture::new();
    let id = tag(&fixture.store, "Genre:Drama");
    let subject = TagSubject::Item("item-a".into());
    assign(
        &fixture.store,
        "genre:drama",
        subject.clone(),
        "provider",
        "original",
        0.8,
        false,
    );
    let request = command(
        &fixture.store,
        "suppress",
        &set(subject.clone(), &id, None, true),
    );
    let first = run(&fixture.store, &request, &grant());
    assert_eq!(first.code(), "committed");
    assert_eq!(count(&fixture.store, "catalog_effective_item_tags"), 0);
    assert_eq!(count(&fixture.store, "item_tags"), 1);
    let Fixture { _root, path, store } = fixture;
    drop(store);
    let store = Store::open(&path, true, false).unwrap();
    let replay = run(&store, &request, &grant());
    assert_eq!(replay.code(), "replay");
    assert_eq!(result(&replay), result(&first));
    tagging::rename(&store, &json!("genre:drama"), &json!("genre:theatre")).unwrap();
    assign(
        &store,
        "genre:drama",
        subject.clone(),
        "provider",
        "refresh",
        0.9,
        false,
    );
    assert_eq!(count(&store, "catalog_effective_item_tags"), 0);
    run(
        &store,
        &command(&store, "unsuppress", &set(subject, &id, Some(1), false)),
        &grant(),
    );
    assert_eq!(count(&store, "catalog_effective_item_tags"), 1);
    let before = snapshot(&store);
    let old = run(&store, &request, &grant());
    assert_eq!(old.code(), "replay");
    assert_eq!(result(&old), result(&first));
    assert_eq!(snapshot(&store), before);
    assert_eq!(count(&store, "catalog_tag_suppression_revisions"), 2);
}

#[test]
fn suppression_same_key_changed_payload_conflicts_and_stale_revision_is_typed() {
    let fixture = Fixture::new();
    let id = tag(&fixture.store, "tag");
    let subject = TagSubject::File("file-a".into());
    let mutation = set(subject.clone(), &id, None, true);
    let original = command(&fixture.store, "same-key", &mutation);
    run(&fixture.store, &original, &grant());
    let conflicting = CommandEnvelope::new(
        1,
        original.key().clone(),
        original.admission_incarnation(),
        original.expected_generation(),
        &json!({"operation":"tags","mutation":set(subject.clone(),&id,None,false)}).to_string(),
    )
    .unwrap();
    let before = snapshot(&fixture.store);
    assert_eq!(
        run(&fixture.store, &conflicting, &grant()),
        CommandOutcome::Conflict
    );
    let stale = command(
        &fixture.store,
        "stale-revision",
        &set(subject, &id, None, false),
    );
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &stale,
            CatalogAuthority::new(base()).with_tags(&grant()),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::OperationConflict
    );
    assert_eq!(snapshot(&fixture.store), before);
}

#[test]
fn missing_narrowed_and_wrong_namespace_tag_grants_deny_new_commands_without_writes() {
    let fixture = Fixture::new();
    let id = tag(&fixture.store, "tag");
    let request = command(
        &fixture.store,
        "denied",
        &set(TagSubject::Item("item-a".into()), &id, None, true),
    );
    let before = snapshot(&fixture.store);
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &request,
            CatalogAuthority::new(base()),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::AuthorityDenied
    );
    let mut narrow = grant();
    narrow.allowed_subjects.clear();
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &request,
            CatalogAuthority::new(base()).with_tags(&narrow),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::AuthorityDenied
    );
    let mut wrong = grant();
    wrong.host_namespace = "different-host".into();
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &request,
            CatalogAuthority::new(base()).with_tags(&wrong),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::AuthorityDenied
    );
    assert_eq!(snapshot(&fixture.store), before);
}

#[test]
fn late_receipt_budget_refusal_rolls_back_suppression_after_owner_wrote() {
    let fixture = Fixture::new();
    let id = tag(&fixture.store, "tag");
    let request = command(
        &fixture.store,
        "late-budget",
        &set(TagSubject::File("file-a".into()), &id, None, true),
    );
    let before = snapshot(&fixture.store);
    let observed = observe_insert(&fixture.store, "catalog_tag_suppression_revisions");
    let outcome = commands::execute_with_authority(
        &fixture.store,
        &request,
        CatalogAuthority::new(base()).with_tags(&grant()),
        overhead_budget(&fixture.store, &request),
    )
    .unwrap();
    assert_eq!(outcome, CommandOutcome::ReceiptStorageExhausted);
    assert!(
        observed.load(Ordering::SeqCst) > 0,
        "must reach the actual tag history insert before result-budget refusal"
    );
    assert!(fixture.store.db.is_autocommit());
    assert_eq!(snapshot(&fixture.store), before);
    assert_eq!(run(&fixture.store, &request, &grant()).code(), "committed");
    assert_eq!(count(&fixture.store, "catalog_command_receipts"), 1);
}

#[test]
fn receipt_insert_failure_rolls_back_tag_head_history_and_generation_then_retry_commits() {
    let fixture = Fixture::new();
    let id = tag(&fixture.store, "tag");
    let request = command(
        &fixture.store,
        "receipt-failure",
        &set(TagSubject::Item("item-a".into()), &id, None, true),
    );
    let before = snapshot(&fixture.store);
    fixture.store.db.execute_batch("CREATE TEMP TRIGGER reject_tag_receipt BEFORE INSERT ON catalog_command_receipts BEGIN SELECT RAISE(ABORT,'fixture receipt failure'); END;").unwrap();
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &request,
            CatalogAuthority::new(base()).with_tags(&grant()),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::StoreFailure
    );
    assert!(fixture.store.db.is_autocommit());
    assert_eq!(snapshot(&fixture.store), before);
    fixture
        .store
        .db
        .execute_batch("DROP TRIGGER reject_tag_receipt")
        .unwrap();
    assert_eq!(run(&fixture.store, &request, &grant()).code(), "committed");
}

#[test]
fn merge_preview_collision_choices_and_durable_replay_preserve_ids_and_evidence() {
    let fixture = Fixture::new();
    let a = tag(&fixture.store, "source");
    let b = tag(&fixture.store, "target");
    let item = TagSubject::Item("item-a".into());
    let file = TagSubject::File("file-a".into());
    assign(
        &fixture.store,
        "source",
        item.clone(),
        "provider",
        "source evidence",
        0.2,
        false,
    );
    assign(
        &fixture.store,
        "target",
        item.clone(),
        "provider",
        "target evidence",
        0.9,
        false,
    );
    assign(
        &fixture.store,
        "target",
        item.clone(),
        "provider",
        "",
        0.9,
        true,
    );
    assign(
        &fixture.store,
        "source",
        file.clone(),
        "file-provider",
        "file evidence",
        0.7,
        false,
    );
    run(
        &fixture.store,
        &command(
            &fixture.store,
            "suppress-source",
            &set(item.clone(), &a, None, true),
        ),
        &grant(),
    );
    let source_id: String = fixture
        .store
        .db
        .query_row("SELECT id FROM item_tags WHERE tag_id=?", [&a], |r| {
            r.get(0)
        })
        .unwrap();
    let target_id: String = fixture
        .store
        .db
        .query_row("SELECT id FROM item_tags WHERE tag_id=?", [&b], |r| {
            r.get(0)
        })
        .unwrap();
    let file_id: String = fixture
        .store
        .db
        .query_row("SELECT id FROM file_tags WHERE tag_id=?", [&a], |r| {
            r.get(0)
        })
        .unwrap();
    let before = snapshot(&fixture.store);
    let preview = tags::preview_merge(&fixture.store, &grant(), "tag-host", &a, &b).unwrap();
    assert_eq!(snapshot(&fixture.store), before);
    assert_eq!(preview.body.collision_options.len(), 1);
    let incomplete = command(
        &fixture.store,
        "missing-collision-choice",
        &merge_from(&preview, vec![]),
    );
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &incomplete,
            CatalogAuthority::new(base()).with_tags(&grant()),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::OperationConflict
    );
    assert_eq!(snapshot(&fixture.store), before);
    let request = command(
        &fixture.store,
        "merge",
        &merge_from(
            &preview,
            vec![AssertionCollision {
                subject: item.clone(),
                source_namespace: "provider".into(),
                evidence: CollisionEvidence::Source,
            }],
        ),
    );
    let committed = run(&fixture.store, &request, &grant());
    assert_eq!(committed.code(), "committed");
    assert_eq!(
        result(&committed)["result"]["mappings"],
        serde_json::to_value(&preview.body.mappings).unwrap()
    );
    assert_eq!(
        fixture
            .store
            .db
            .query_row("SELECT id,note,active FROM item_tags", [], |r| Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?
            )))
            .unwrap(),
        (target_id.clone(), "source evidence".into(), true)
    );
    assert_eq!(
        fixture
            .store
            .db
            .query_row("SELECT id,tag_id FROM file_tags", [], |r| Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?
            )))
            .unwrap(),
        (file_id, b.clone())
    );
    assert!(
        preview
            .body
            .mappings
            .assertions
            .iter()
            .any(|m| m.previous_assertion_id == source_id && m.surviving_assertion_id == target_id)
    );
    assert_eq!(
        tagging::resolve_tag(&fixture.store.db, &json!("source")).unwrap(),
        b
    );
    assert_eq!(count(&fixture.store, "catalog_effective_item_tags"), 0);
    assert_eq!(count(&fixture.store, "catalog_effective_file_tags"), 1);
    assert_eq!(count(&fixture.store, "items"), 2);
    assert_eq!(count(&fixture.store, "files"), 2);
    let retained: String = fixture
        .store
        .db
        .query_row(
            "SELECT snapshot_json FROM catalog_tag_merges WHERE source_tag_id=?",
            [&a],
            |r| r.get(0),
        )
        .unwrap();
    let retained: Value = serde_json::from_str(&retained).unwrap();
    assert_eq!(retained["preview"]["digest"], preview.digest);
    assert_eq!(
        retained["preview"]["body"]["affected"]["target_assertions"][0]["note"],
        "target evidence"
    );
    let after = snapshot(&fixture.store);
    let replay = run(&fixture.store, &request, &grant());
    assert_eq!(replay.code(), "replay");
    assert_eq!(result(&replay), result(&committed));
    assert_eq!(snapshot(&fixture.store), after);
    assert_eq!(count(&fixture.store, "pragma_foreign_key_check"), 0);
}

#[test]
fn late_receipt_budget_refusal_rolls_back_complete_merge_and_can_retry() {
    let fixture = Fixture::new();
    let a = tag(&fixture.store, "source");
    let b = tag(&fixture.store, "target");
    let subject = TagSubject::Item("item-a".into());
    assign(
        &fixture.store,
        "source",
        subject.clone(),
        "provider",
        "kept",
        0.5,
        false,
    );
    run(
        &fixture.store,
        &command(&fixture.store, "suppress", &set(subject, &a, None, true)),
        &grant(),
    );
    let preview = tags::preview_merge(&fixture.store, &grant(), "tag-host", &a, &b).unwrap();
    let request = command(
        &fixture.store,
        "merge-budget",
        &merge_from(&preview, vec![]),
    );
    let before = snapshot(&fixture.store);
    let observed = observe_insert(&fixture.store, "catalog_tag_merges");
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &request,
            CatalogAuthority::new(base()).with_tags(&grant()),
            overhead_budget(&fixture.store, &request)
        )
        .unwrap(),
        CommandOutcome::ReceiptStorageExhausted
    );
    assert!(observed.load(Ordering::SeqCst) > 0);
    assert_eq!(snapshot(&fixture.store), before);
    assert!(fixture.store.db.is_autocommit());
    assert_eq!(run(&fixture.store, &request, &grant()).code(), "committed");
    assert_eq!(count(&fixture.store, "catalog_tag_merges"), 1);
}

#[test]
fn hidden_affected_member_denies_public_preview_and_dispatcher_merge_without_details() {
    let fixture = Fixture::new();
    let a = tag(&fixture.store, "source");
    let b = tag(&fixture.store, "target");
    assign(
        &fixture.store,
        "source",
        TagSubject::Item("item-b".into()),
        "private-provider",
        "private evidence",
        0.4,
        false,
    );
    let full = tags::preview_merge(&fixture.store, &grant(), "tag-host", &a, &b).unwrap();
    let request = command(&fixture.store, "hidden", &merge_from(&full, vec![]));
    let mut narrow = grant();
    narrow
        .allowed_subjects
        .remove(&TagSubject::Item("item-b".into()));
    let before = snapshot(&fixture.store);
    let error = tags::preview_merge(&fixture.store, &narrow, "tag-host", &a, &b).unwrap_err();
    assert_eq!(error.code, TagErrorCode::AuthorityDenied);
    assert_eq!(
        error.to_string(),
        "tag operation is outside the trusted host scope"
    );
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &request,
            CatalogAuthority::new(base()).with_tags(&narrow),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::AuthorityDenied
    );
    assert_eq!(snapshot(&fixture.store), before);
}

#[test]
fn phantom_rejects_old_generation_and_old_digest_even_with_fresh_envelope() {
    let fixture = Fixture::new();
    let a = tag(&fixture.store, "source");
    let b = tag(&fixture.store, "target");
    let preview = tags::preview_merge(&fixture.store, &grant(), "tag-host", &a, &b).unwrap();
    let mutation = merge_from(&preview, vec![]);
    let old = command(&fixture.store, "old-generation", &mutation);
    assign(
        &fixture.store,
        "source",
        TagSubject::File("file-b".into()),
        "new-provider",
        "phantom",
        0.3,
        false,
    );
    let before = snapshot(&fixture.store);
    assert_eq!(
        run(&fixture.store, &old, &grant()),
        CommandOutcome::StaleGeneration
    );
    let fresh = command(&fixture.store, "old-preview-new-envelope", &mutation);
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &fresh,
            CatalogAuthority::new(base()).with_tags(&grant()),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::OperationConflict
    );
    assert_eq!(snapshot(&fixture.store), before);
}

#[test]
fn public_preview_requires_schema34_and_rejects_caller_owned_transactions() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("native33.db");
    migration::initialize_native_at(&old, 33).unwrap();
    let old = Store::open(&old, true, false).unwrap();
    assert_eq!(
        tags::preview_merge(&old, &grant(), "tag-host", "unused-a", "unused-b")
            .unwrap_err()
            .code,
        TagErrorCode::UnsupportedSchema
    );
    let fixture = Fixture::new();
    let a = tag(&fixture.store, "source");
    let b = tag(&fixture.store, "target");
    let before = snapshot(&fixture.store);
    fixture.store.db.execute_batch("BEGIN").unwrap();
    assert_eq!(
        tags::preview_merge(&fixture.store, &grant(), "tag-host", &a, &b)
            .unwrap_err()
            .code,
        TagErrorCode::AmbientReadTransaction
    );
    fixture.store.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(snapshot(&fixture.store), before);
}

#[test]
fn public_preview_checks_store_path_identity_before_read() {
    let fixture = Fixture::new();
    let a = tag(&fixture.store, "source");
    let b = tag(&fixture.store, "target");
    let moved = fixture.path.with_extension("moved");
    std::fs::rename(&fixture.path, &moved).unwrap();
    let error = tags::preview_merge(&fixture.store, &grant(), "tag-host", &a, &b).unwrap_err();
    assert_eq!(error.code, TagErrorCode::StoreFailure);
    std::fs::rename(&moved, &fixture.path).unwrap();
}

#[test]
fn public_preview_rechecks_store_path_identity_after_materializing_rows() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let fixture = Fixture::new();
    let a = tag(&fixture.store, "source");
    let b = tag(&fixture.store, "target");
    let original = fixture.path.clone();
    let moved = fixture.path.with_extension("moved-during-read");
    let target = moved.clone();
    let observed = Arc::new(AtomicUsize::new(0));
    let observed_hook = Arc::clone(&observed);
    let mut reads = 0;
    // Fault injection belongs only to this disposable fixture, not the owner API.
    fixture
        .store
        .db
        .authorizer(Some(move |context: AuthContext<'_>| {
            if matches!(
                context.action,
                AuthAction::Read {
                    table_name: "tag_names",
                    column_name: "name"
                }
            ) {
                reads += 1;
                if reads == 2 {
                    std::fs::rename(&original, &target).unwrap();
                    observed_hook.fetch_add(1, Ordering::SeqCst);
                }
            }
            Authorization::Allow
        }))
        .unwrap();
    let error = tags::preview_merge(&fixture.store, &grant(), "tag-host", &a, &b).unwrap_err();
    assert_eq!(observed.load(Ordering::SeqCst), 1);
    assert_eq!(error.code, TagErrorCode::StoreFailure);
    std::fs::rename(&moved, &fixture.path).unwrap();
}

#[test]
fn known_receipt_uses_independent_read_authority_after_write_grant_is_removed() {
    let fixture = Fixture::new();
    let id = tag(&fixture.store, "tag");
    let request = command(
        &fixture.store,
        "receipt-read-authority",
        &set(TagSubject::Item("item-a".into()), &id, None, true),
    );
    let committed = run(&fixture.store, &request, &grant());
    let before = snapshot(&fixture.store);
    // The host independently refreshed read authority for the entire retained
    // result. No current tag mutation grant remains, and new writes are denied.
    let read_only = TrustedCommandAuthority {
        allow_new_commands: false,
        ..base()
    };
    let replay = commands::execute_with_authority(
        &fixture.store,
        &request,
        CatalogAuthority::new(read_only),
        policy(),
    )
    .unwrap();
    assert_eq!(replay.code(), "replay");
    assert_eq!(result(&replay), result(&committed));
    // When any retained member is no longer readable, the host grants only
    // status reconciliation. No result body, mapping or affected ID is returned.
    let status_only = TrustedCommandAuthority {
        receipt_visibility: ReceiptVisibility::CommittedStatusOnly,
        ..read_only
    };
    let redacted = commands::execute_with_authority(
        &fixture.store,
        &request,
        CatalogAuthority::new(status_only),
        policy(),
    )
    .unwrap();
    assert_eq!(redacted, CommandOutcome::CommittedRedacted);
    assert_eq!(
        serde_json::to_value(redacted).unwrap(),
        json!({"status":"committed_redacted"})
    );
    assert_eq!(snapshot(&fixture.store), before);
}
