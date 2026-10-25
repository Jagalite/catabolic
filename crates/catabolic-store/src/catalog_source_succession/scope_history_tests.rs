//! Real candidate installation and existing curation/receipt-backed relationship
//! producers. These tests do not create succession effects or filesystem grants.
use super::{authorizer, scope_history};
use crate::{
    Error, Store, candidate41_test_support as candidate,
    catalog_commands::{self as commands, ReceiptPolicy, TrustedCommandAuthority},
    catalog_relationships::{
        self as relationships, AuthorizedRelationshipDelete, RelationshipAuthority,
        RelationshipEdge, RelationshipKindRule, RelationshipMutation,
    },
    curation, migration,
};
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};
const HOST: &str = "scoped-history-test";
const KIND: &str = "custom:motion_edition";
struct Fixture {
    store: Store,
    _root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native.sqlite3");
        migration::initialize_native_at(&path, 40).unwrap();
        candidate::install_candidate41_for_test(&path).unwrap();
        let store = candidate::open_candidate41_for_test(&path, true).unwrap();
        for id in ["a", "b", "c", "d"] {
            curation::item_put(&store, &json!({"id":id,"kind":"video","metadata":{}})).unwrap();
        }
        Self { store, _root: root }
    }
    fn head(&self, target: &str) -> i64 {
        self.store.db.query_row("SELECT coalesce((SELECT revision FROM catalog_source_scope_heads WHERE scope_kind='motion_parent_target' AND scope_key=json_array(?1,?2)),0)",[KIND,target],|r|r.get(0)).unwrap()
    }
    fn envelope(&self, id: &str, mutation: &RelationshipMutation) -> CommandEnvelope {
        let generation = self
            .store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        CommandEnvelope::new(
            1,
            CommandKey::new(&self.store.database_id, HOST, id).unwrap(),
            self.store.catalog_incarnation.as_deref().unwrap(),
            generation,
            &catabolic_core::encode(&json!({"operation":"relationships","mutation":mutation})),
        )
        .unwrap()
    }
    fn execute(
        &self,
        command: &CommandEnvelope,
        mutation: &RelationshipMutation,
        grant: &RelationshipAuthority,
    ) -> commands::CommandOutcome {
        commands::execute_candidate_for_test(
            &self.store,
            command,
            TrustedCommandAuthority {
                host_namespace: HOST,
                receipt_visibility: ReceiptVisibility::FullResult,
                allow_new_commands: true,
                identity_authority: None,
            },
            ReceiptPolicy {
                retention: Duration::from_secs(3600),
                max_receipts: 100,
                max_hold_records: 10,
                max_logical_bytes: 16 * 1024 * 1024,
            },
            |store, envelope, _| {
                relationships::apply(store, envelope, mutation, grant).map_err(|e| e.error)
            },
        )
        .unwrap()
    }
    fn create(
        &self,
        id: &str,
        source: &str,
        target: &str,
    ) -> (CommandEnvelope, RelationshipMutation, RelationshipAuthority) {
        let edge = RelationshipEdge {
            source: source.into(),
            target: target.into(),
            kind: KIND.into(),
        };
        let mutation = RelationshipMutation::Create {
            id: id.into(),
            source: source.into(),
            target: target.into(),
            kind: KIND.into(),
            metadata: json!({}),
        };
        let grant = RelationshipAuthority {
            host_namespace: HOST.into(),
            create_edges: [edge].into(),
            kind_rules: [(KIND.into(), RelationshipKindRule::Directed)].into(),
            delete_edges: BTreeMap::new(),
        };
        let command = self.envelope(&format!("create-{id}"), &mutation);
        assert!(matches!(
            self.execute(&command, &mutation, &grant),
            commands::CommandOutcome::Committed { .. }
        ));
        (command, mutation, grant)
    }
}
#[test]
fn scoped_history_real_producer_away_back_unrelated_rollback_and_replay() {
    let f = Fixture::new();
    assert_eq!(f.head("b"), 0);
    let (command, mutation, grant) = f.create("new-edge", "a", "b");
    assert_eq!(f.head("b"), 1);
    let before = migration::snapshot(&f.store.db).unwrap();
    let _ = f.execute(&command, &mutation, &grant);
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    // Genuine ignored INSERT and identical UPDATE must not manufacture history.
    f.store.transaction(|db| {db.execute("INSERT OR IGNORE INTO item_relationships SELECT * FROM item_relationships WHERE id='new-edge'",[])?;db.execute("UPDATE item_relationships SET metadata=metadata WHERE id='new-edge'",[])?;Ok(())}).unwrap();
    assert_eq!(f.head("b"), 1);
    let edge = RelationshipEdge {
        source: "a".into(),
        target: "b".into(),
        kind: KIND.into(),
    };
    let revision=f.store.db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='item_relationships' AND entity_key=json_array('new-edge')",[],|r|r.get(0)).unwrap();
    let delete = RelationshipMutation::Delete {
        id: "new-edge".into(),
        expected_revision: revision,
        expected_edge: edge.clone(),
    };
    let grant = RelationshipAuthority {
        host_namespace: HOST.into(),
        create_edges: Default::default(),
        kind_rules: [(KIND.into(), RelationshipKindRule::Directed)].into(),
        delete_edges: [(
            "new-edge".into(),
            AuthorizedRelationshipDelete {
                expected_revision: revision,
                edge,
            },
        )]
        .into(),
    };
    assert!(matches!(
        f.execute(&f.envelope("delete-edge", &delete), &delete, &grant),
        commands::CommandOutcome::Committed { .. }
    ));
    assert_eq!(f.head("b"), 2);
    assert!(
        !f.store
            .db
            .prepare("SELECT 1 FROM item_relationships WHERE id='new-edge'")
            .unwrap()
            .exists([])
            .unwrap()
    );
    let _ = f.create("unrelated-edge", "c", "d");
    assert_eq!(f.head("b"), 2);
    // Even a relationship on the same items, outside the structural owner kinds,
    // must not invalidate the retained structural parent collection.
    curation::relate(
        &f.store,
        &json!({"source":"a","target":"b","kind":"custom:library_membership"}),
    )
    .unwrap();
    assert_eq!(f.head("b"), 2);
    let before = migration::snapshot(&f.store.db).unwrap();
    let rolled: crate::Result<()> = f.store.transaction(|_| {
        curation::relate(&f.store, &json!({"source":"a","target":"b","kind":KIND}))?;
        assert!(f.head("b") > 2);
        Err(Error("intentional rollback".into()))
    });
    assert!(rolled.is_err());
    assert_eq!(migration::snapshot(&f.store.db).unwrap(), before);
    // Reopen through genuine candidate admission, retaining history and absence.
    let path = f.store.path.clone();
    drop(f.store);
    let reopened = candidate::open_candidate41_for_test(&path, true).unwrap();
    assert_eq!(reopened.db.query_row("SELECT revision FROM catalog_source_scope_heads WHERE scope_kind='motion_parent_target' AND scope_key=json_array(?1,'b')",[KIND],|r|r.get::<_,i64>(0)).unwrap(),2);
}
fn direct_denied(store: &Store) {
    assert!(
        store
            .db
            .execute(
                "INSERT INTO catalog_source_scope_heads VALUES('fake','[]',1)",
                []
            )
            .is_err()
    );
    assert!(store.db.execute("INSERT INTO catalog_source_scope_events VALUES('fake','[]',1,'files','insert',NULL,'[]')",[]).is_err());
    assert!(store.db.execute_batch("CREATE TEMP TRIGGER catalog_source_scope_files_insert AFTER INSERT ON items BEGIN INSERT INTO catalog_source_scope_heads VALUES('fake','[]',1); END;").is_err());
    assert!(
        store
            .db
            .execute_batch("DROP TRIGGER catalog_source_scope_files_insert")
            .is_err()
    );
}
#[test]
fn candidate_scope_baseline_survives_query_embedding_failure_unwind_and_poison() {
    let f = Fixture::new();
    direct_denied(&f.store);
    let options = crate::sql::Options {
        profile: "default",
        ..Default::default()
    };
    crate::sql::execute_store(&f.store, Some("SELECT 1"), &options).unwrap();
    direct_denied(&f.store);
    assert!(crate::sql::execute_store(&f.store, Some("SELECT missing_column"), &options).is_err());
    direct_denied(&f.store);
    crate::embedding::command(&f.store, "SELECT 1", &json!([])).unwrap();
    direct_denied(&f.store);
    assert!(
        crate::embedding::command(
            &f.store,
            "INSERT INTO catalog_source_scope_heads VALUES('fake','[]',1)",
            &json!([])
        )
        .is_err()
    );
    direct_denied(&f.store);
    // Controlled test-only hook replacement prepares a statement without the
    // baseline, then restores it before any execution. This is not supported
    // caller authority to disable hooks; it tests SQLite statement invalidation.
    let reset = authorizer::Guard::for_store(&f.store).unwrap();
    f.store
        .db
        .authorizer(Some(|_: rusqlite::hooks::AuthContext<'_>| {
            rusqlite::hooks::Authorization::Allow
        }))
        .unwrap();
    let mut prepared = f
        .store
        .db
        .prepare("INSERT INTO catalog_source_scope_heads VALUES('prepared-fake','[]',1)")
        .unwrap();
    reset.restore().unwrap();
    assert!(prepared.execute([]).is_err());
    drop(prepared);
    assert!(
        !f.store
            .db
            .prepare("SELECT 1 FROM catalog_source_scope_heads WHERE scope_kind='prepared-fake'")
            .unwrap()
            .exists([])
            .unwrap()
    );
    let _ = f.create("after-query-embedding", "a", "b");
    assert_eq!(f.head("b"), 1);
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let guard = authorizer::Guard::for_store(&f.store).unwrap();
        guard
            .replace(|_| rusqlite::hooks::Authorization::Allow)
            .unwrap();
        direct_denied(&f.store);
        panic!("controlled authorizer unwind");
    }));
    assert!(unwind.is_err());
    direct_denied(&f.store);
    let guard = authorizer::Guard::for_store(&f.store).unwrap();
    guard
        .replace(|_| rusqlite::hooks::Authorization::Allow)
        .unwrap();
    f.store.poison_transaction_owner();
    assert!(guard.restore().is_err());
    assert!(
        f.store
            .db
            .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
            .is_err()
    );
    assert!(authorizer::Guard::for_store(&f.store).is_err());
    assert!(authorizer::Guard::for_migration(&f.store.db).is_err());
}
#[test]
fn scope_manifest_matches_real_candidate_columns_and_exact_origins() {
    let f = Fixture::new();
    scope_history::assert_manifest_columns_for_test(&f.store.db);
    let names = scope_history::origins();
    assert_eq!(names.len(), 54);
    for name in names {
        assert!(
            f.store
                .db
                .prepare("SELECT 1 FROM sqlite_schema WHERE type='trigger' AND name=?")
                .unwrap()
                .exists([name])
                .unwrap()
        );
    }
}

#[test]
fn normal40_query_embedding_restoration_does_not_install_candidate_history() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("normal40.sqlite3");
    migration::initialize_native_at(&path, 40).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    let options = crate::sql::Options {
        profile: "default",
        ..Default::default()
    };
    crate::sql::execute_store(&store, Some("SELECT 1"), &options).unwrap();
    assert!(crate::sql::execute_store(&store, Some("SELECT missing_column"), &options).is_err());
    crate::embedding::command(&store, "SELECT 1", &json!([])).unwrap();
    assert!(crate::embedding::command(&store, "SELECT missing_column", &json!([])).is_err());
    curation::item_put(
        &store,
        &json!({"id":"ordinary40","kind":"video","metadata":{}}),
    )
    .unwrap();
    assert!(!store.db.prepare("SELECT 1 FROM sqlite_schema WHERE name IN ('catalog_source_scope_heads','catalog_source_scope_events')").unwrap().exists([]).unwrap());
    assert_eq!(store.schema_version, 40);
    drop(store);
    Store::open(&path, true, false).unwrap();
}

#[test]
fn genuine_candidate_rotation_preserves_scoped_history_and_reopens_baseline() {
    use std::sync::atomic::AtomicBool;
    let f = Fixture::new();
    let _ = f.create("before-rotation", "a", "b");
    let history = |store: &Store| {
        (
            migration::rows(
                &store.db,
                "SELECT * FROM catalog_source_scope_heads ORDER BY scope_kind,scope_key",
                &[],
            )
            .unwrap(),
            migration::rows(
                &store.db,
                "SELECT * FROM catalog_source_scope_events ORDER BY scope_kind,scope_key,revision",
                &[],
            )
            .unwrap(),
        )
    };
    let before = history(&f.store);
    let old_incarnation = f.store.catalog_incarnation.clone().unwrap();
    f.store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    // Explicit fixture commitments exercise genuine native rotation, not pair
    // artifact verification or a grant to publish a restored installation.
    let new_incarnation = crate::catalog_restore_lineage::rotate_candidate_for_test(
        &f.store,
        &"a".repeat(64),
        &"b".repeat(64),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_ne!(new_incarnation, old_incarnation);
    assert!(f.store.validate_native_identity().is_err());
    assert_eq!(history(&f.store), before);
    f.store.db.execute_batch("COMMIT").unwrap();
    let Fixture { store, _root: root } = f;
    drop(store);
    let store =
        candidate::open_candidate41_for_test(&root.path().join("native.sqlite3"), true).unwrap();
    assert_eq!(
        store.catalog_incarnation.as_deref(),
        Some(new_incarnation.as_str())
    );
    assert_eq!(history(&store), before);
    direct_denied(&store);
    let reopened = Fixture { store, _root: root };
    let _ = reopened.create("after-rotation", "c", "d");
    assert_eq!(reopened.head("b"), 1);
    assert_eq!(reopened.head("d"), 1);
}

#[test]
fn candidate_public_sql_owns_read_budget_and_preserves_inherited_refusal() {
    use crate::operation_budget::{BudgetLimits, OperationBudget};
    let f = Fixture::new();
    let read = candidate::open_candidate41_for_test(&f._root.path().join("native.sqlite3"), false)
        .unwrap();
    let options = crate::sql::Options {
        profile: "default",
        ..Default::default()
    };
    let run = || {
        crate::sql::execute_store(
            &read,
            Some("SELECT catalog_source_evidence_work(64,1) AS n"),
            &options,
        )
    };
    assert!(OperationBudget::current().is_none());
    assert_eq!(run().unwrap()["rows"][0][0], 64);
    assert!(
        OperationBudget::current().is_none(),
        "standalone SQL leaked its owner"
    );
    let budget = OperationBudget::new(
        Duration::from_secs(5),
        BudgetLimits {
            items: 100,
            values: 100,
            bytes: 4096,
            work: 1,
        },
    )
    .unwrap();
    {
        let _scope = budget.enter().unwrap();
        assert!(run().is_err());
        assert!(budget.failure().is_some());
        let used = budget.usage();
        assert!(
            crate::sql::execute_store(&read, Some("SELECT 1"), &options).is_err(),
            "nested query replaced sticky refusal"
        );
        assert_eq!(budget.usage(), used);
    }
    assert!(run().is_ok());
    assert!(crate::embedding::with_interrupt_check(|| true, run).is_err());
    assert!(OperationBudget::current().is_none());
    assert!(run().is_ok(), "cancelled SQL leaked its scope");
    // The readonly owner retains a real snapshot; settle it before testing a
    // separate writer commit, rather than weakening SQLite writer custody.
    drop(read);
    direct_denied(&f.store);
    f.create("after-standalone-query-budget", "a", "b");
    assert_eq!(f.head("b"), 1);
}
