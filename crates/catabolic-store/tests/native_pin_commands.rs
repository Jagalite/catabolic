// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Actual Store and durable-dispatcher identity-pin regressions.
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
use catabolic_store::{
    Store,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, CommandStoreErrorCode, ReceiptPolicy,
        TrustedCommandAuthority,
    },
    catalog_pins::{
        self as pins, PinAuthority, PinBatch, PinChange, PinDecision, PinErrorCode, PinMutation,
        PinPreview,
    },
    curation, migration,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::Duration,
};
const HOST: &str = "pin-integration-host";
const NS: &str = "tmdb.movie";
struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    store: Store,
    grant: PinAuthority,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native.db");
        migration::initialize_native_at(&path, 34).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        assert_eq!(store.schema_version, 34);
        seed(&store);
        Self {
            _root: root,
            path,
            store,
            grant: grant(&["one", "two", "three"]),
        }
    }
    fn command(&self, batch: PinBatch, operation: &str) -> CommandEnvelope {
        let p = pins::preview_pins(&self.store, &batch, &self.grant).unwrap();
        command(&self.store, &p, batch, operation)
    }
    fn execute(&self, c: &CommandEnvelope) -> CommandOutcome {
        commands::execute_with_authority(
            &self.store,
            c,
            CatalogAuthority::new(base()).with_pins(&self.grant),
            policy(),
        )
        .unwrap()
    }
}
fn base() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: HOST,
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn grant(items: &[&str]) -> PinAuthority {
    PinAuthority {
        host_namespace: HOST.into(),
        readable: BTreeMap::new(),
        mutable: [(
            NS.into(),
            items.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
        )]
        .into(),
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
fn seed(store: &Store) {
    for (item, value) in [("one", "438631"), ("two", "841"), ("three", "999")] {
        curation::item_put(store,&json!({"id":item,"kind":"movie","metadata":{"title":format!("Film {item}")},"identities":{NS:value}})).unwrap();
    }
}
fn set(item: &str, revision: i64, value: &str) -> PinChange {
    PinChange {
        item_id: item.into(),
        namespace: NS.into(),
        expected_revision: revision,
        decision: PinDecision::Set {
            value: value.into(),
        },
    }
}
fn remove(item: &str, revision: i64) -> PinChange {
    PinChange {
        item_id: item.into(),
        namespace: NS.into(),
        expected_revision: revision,
        decision: PinDecision::Remove,
    }
}
fn batch(changes: Vec<PinChange>) -> PinBatch {
    PinBatch {
        contract_version: 1,
        changes,
    }
}
fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
        .unwrap()
}
fn count(store: &Store, table: &str) -> i64 {
    assert!(
        [
            "catalog_identity_pins",
            "catalog_identity_pin_revisions",
            "catalog_command_receipts",
            "catalog_legacy_identity_evidence"
        ]
        .contains(&table)
    );
    store
        .db
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn command(store: &Store, p: &PinPreview, b: PinBatch, op: &str) -> CommandEnvelope {
    CommandEnvelope::new(1,CommandKey::new(&store.database_id,HOST,op).unwrap(),p.incarnation.clone(),p.expected_generation,
  &json!({"operation":"pins","mutation":PinMutation {batch:b,preview_digest:p.digest.clone()}}).to_string()).unwrap()
}
fn current_envelope(store: &Store, payload: Value, op: &str) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, HOST, op).unwrap(),
        store.catalog_incarnation.clone().unwrap(),
        generation(store),
        &payload.to_string(),
    )
    .unwrap()
}
fn result(outcome: &CommandOutcome) -> &Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        _ => panic!("expected full result: {outcome:?}"),
    }
}
fn state(store: &Store) -> String {
    store.db.query_row("SELECT json_array(id,catalog_id,incarnation,generation,contract_version) FROM catalog_state",[],|r|r.get(0)).unwrap()
}
fn tuples(store: &Store) -> Vec<String> {
    store.db.prepare("SELECT json_array(namespace,value,item_id) FROM identities ORDER BY namespace,value,item_id").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap()
}
fn revisions(store: &Store, exclude_new: bool) -> Vec<String> {
    let sql = if exclude_new {
        "SELECT json_array(entity_kind,entity_key,revision,deleted) FROM catalog_entity_revisions WHERE entity_kind NOT IN ('catalog_legacy_identity_evidence') ORDER BY entity_kind,entity_key"
    } else {
        "SELECT json_array(entity_kind,entity_key,revision,deleted) FROM catalog_entity_revisions ORDER BY entity_kind,entity_key"
    };
    store
        .db
        .prepare(sql)
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
}

#[test]
fn production_pin_command_preview_cas_refresh_removal_and_recreation() {
    let f = Fixture::new();
    let b = batch(vec![set("one", 0, "accepted")]);
    let g = generation(&f.store);
    let changes = f.store.db.total_changes();
    let p = pins::preview_pins(&f.store, &b, &f.grant).unwrap();
    assert_eq!(p.expected_generation, g);
    assert_eq!(f.store.db.total_changes(), changes);
    let c = command(&f.store, &p, b, "first");
    let outcome = f.execute(&c);
    assert!(matches!(outcome, CommandOutcome::Committed { .. }));
    assert_eq!(result(&outcome)["decisions"][0]["after"]["revision"], 1);
    curation::item_put(&f.store,&json!({"id":"one","kind":"movie","metadata":{"title":"Refreshed provider title"},"identities":{NS:"provider-refresh"}})).unwrap();
    assert_eq!(
        pins::lookup_pin(&f.store, "one", NS, &f.grant)
            .unwrap()
            .accepted_value
            .as_deref(),
        Some("accepted")
    );
    assert_eq!(
        pins::pin_history(&f.store, "one", NS, 0, 128, &f.grant)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert_eq!(
        pins::preview_pins(&f.store, &batch(vec![set("one", 0, "stale")]), &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::RevisionConflict
    );
    f.execute(&f.command(batch(vec![remove("one", 1)]), "remove"));
    assert_eq!(
        pins::lookup_pin(&f.store, "one", NS, &f.grant)
            .unwrap()
            .accepted_value,
        None
    );
    assert_eq!(
        pins::preview_pins(
            &f.store,
            &batch(vec![set("one", 0, "recreate-stale")]),
            &f.grant
        )
        .unwrap_err()
        .code,
        PinErrorCode::RevisionConflict
    );
    f.execute(&f.command(batch(vec![set("one", 2, "recreated")]), "recreate"));
    assert_eq!(
        pins::lookup_pin(&f.store, "one", NS, &f.grant)
            .unwrap()
            .decision
            .unwrap()
            .revision,
        3
    );
    assert_eq!(count(&f.store, "catalog_command_receipts"), 3);
}
#[test]
fn production_lost_reply_reopens_and_replays_exact_pin_result_once() {
    let f = Fixture::new();
    let c = f.command(batch(vec![set("one", 0, "accepted")]), "lost-reply");
    let initial = f.execute(&c);
    let g = generation(&f.store);
    let path = f.path.clone();
    drop(f.store);
    let reopened = Store::open(&path, true, false).unwrap();
    let replay = commands::execute_with_authority(
        &reopened,
        &c,
        CatalogAuthority::new(base()).with_pins(&f.grant),
        policy(),
    )
    .unwrap();
    assert!(matches!(replay, CommandOutcome::Replay { .. }));
    assert_eq!(result(&replay), result(&initial));
    assert_eq!(generation(&reopened), g);
    assert_eq!(count(&reopened, "catalog_identity_pin_revisions"), 1);
    assert_eq!(count(&reopened, "catalog_command_receipts"), 1);
}
#[test]
fn production_receipt_read_authority_is_independent_of_revoked_mutation_scope() {
    let f = Fixture::new();
    let c = f.command(batch(vec![set("one", 0, "accepted")]), "receipt");
    let committed = f.execute(&c);
    // FullResult is a separately refreshed, host-trusted receipt-read capability.
    // Typed pin grants authorize new execution; replay must not rerun stale writes.
    let status = TrustedCommandAuthority {
        allow_new_commands: false,
        ..base()
    };
    let replay =
        commands::execute_with_authority(&f.store, &c, CatalogAuthority::new(status), policy())
            .unwrap();
    assert!(matches!(replay, CommandOutcome::Replay { .. }));
    assert_eq!(result(&replay), result(&committed));
    let redacted = TrustedCommandAuthority {
        receipt_visibility: ReceiptVisibility::CommittedStatusOnly,
        ..status
    };
    assert_eq!(
        commands::execute_with_authority(&f.store, &c, CatalogAuthority::new(redacted), policy())
            .unwrap(),
        CommandOutcome::CommittedRedacted
    );
    let next = f.command(batch(vec![set("one", 1, "next")]), "new-without-grant");
    assert_eq!(
        commands::execute_with_authority(&f.store, &next, CatalogAuthority::new(base()), policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::AuthorityDenied
    );
}
#[test]
fn production_duplicate_key_payload_conflict_and_unique_ownership_are_nonmutating() {
    let f = Fixture::new();
    let c = f.command(batch(vec![set("one", 0, "unique")]), "key");
    f.execute(&c);
    let before = generation(&f.store);
    let other=CommandEnvelope::new(1,c.key().clone(),c.admission_incarnation(),c.expected_generation(),"{\"operation\":\"item_put\",\"item\":{\"id\":\"three\",\"kind\":\"movie\",\"metadata\":{}}}").unwrap();
    assert_eq!(f.execute(&other), CommandOutcome::Conflict);
    assert_eq!(generation(&f.store), before);
    assert_eq!(
        pins::preview_pins(&f.store, &batch(vec![set("two", 0, "unique")]), &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::ValueOwned
    );
    let collision = current_envelope(
        &f.store,
        json!({"operation":"pins","mutation":{"batch":batch(vec![set("two",0,"unique")]),"preview_digest":"a".repeat(64)}}),
        "collision",
    );
    assert_eq!(
        commands::execute_with_authority(
            &f.store,
            &collision,
            CatalogAuthority::new(base()).with_pins(&f.grant),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::OperationConflict
    );
    assert_eq!(count(&f.store, "catalog_identity_pin_revisions"), 1);
    assert_eq!(count(&f.store, "catalog_command_receipts"), 1);
    assert_eq!(
        pins::lookup_pin(&f.store, "two", NS, &f.grant)
            .unwrap()
            .accepted_value,
        None
    );
}
#[test]
fn production_full_batch_scope_and_host_namespace_denials_precede_execution() {
    let f = Fixture::new();
    let c = f.command(
        batch(vec![set("one", 0, "a"), set("two", 0, "b")]),
        "scoped",
    );
    let narrow = grant(&["one"]);
    assert_eq!(
        commands::execute_with_authority(
            &f.store,
            &c,
            CatalogAuthority::new(base()).with_pins(&narrow),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::AuthorityDenied
    );
    let mut wrong = f.grant.clone();
    wrong.host_namespace = "other-host".into();
    assert_eq!(
        commands::execute_with_authority(
            &f.store,
            &c,
            CatalogAuthority::new(base()).with_pins(&wrong),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::AuthorityDenied
    );
    assert_eq!(count(&f.store, "catalog_identity_pins"), 0);
    assert_eq!(count(&f.store, "catalog_identity_pin_revisions"), 0);
    assert_eq!(count(&f.store, "catalog_command_receipts"), 0);
    assert_eq!(
        pins::lookup_pin(&f.store, "two", NS, &narrow)
            .unwrap_err()
            .code,
        PinErrorCode::AuthorityDenied
    );
}
#[test]
fn production_receipt_result_budget_rolls_back_pins_history_and_generation_then_retry_succeeds() {
    let f = Fixture::new();
    let c = f.command(
        batch(vec![set("one", 0, "a"), set("two", 0, "b")]),
        "budget",
    );
    let before = generation(&f.store);
    let fixed = 256
        + c.key().catalog_id().len()
        + c.key().host_namespace().len()
        + c.key().operation_id().len()
        + 64
        + c.admission_incarnation().len();
    let constrained = ReceiptPolicy {
        max_logical_bytes: fixed as u64,
        ..policy()
    };
    assert_eq!(
        commands::execute_with_authority(
            &f.store,
            &c,
            CatalogAuthority::new(base()).with_pins(&f.grant),
            constrained
        )
        .unwrap(),
        CommandOutcome::ReceiptStorageExhausted
    );
    assert_eq!(generation(&f.store), before);
    assert_eq!(count(&f.store, "catalog_identity_pins"), 0);
    assert_eq!(count(&f.store, "catalog_identity_pin_revisions"), 0);
    assert_eq!(count(&f.store, "catalog_command_receipts"), 0);
    assert!(matches!(f.execute(&c), CommandOutcome::Committed { .. }));
    assert_eq!(count(&f.store, "catalog_identity_pin_revisions"), 2);
    assert_eq!(count(&f.store, "catalog_command_receipts"), 1);
}
#[test]
fn production_stale_generation_and_recomputed_preview_digest_refuse_admission() {
    let f = Fixture::new();
    let c = f.command(batch(vec![set("one", 0, "a")]), "stale");
    curation::item_put(
        &f.store,
        &json!({"id":"three","kind":"movie","metadata":{"title":"Unrelated concurrent change"}}),
    )
    .unwrap();
    assert_eq!(f.execute(&c), CommandOutcome::StaleGeneration);
    // Refreshing just the outer envelope cannot revive an old reviewed digest.
    let refreshed = current_envelope(&f.store, c.payload().clone(), "stale-preview");
    assert_eq!(
        commands::execute_with_authority(
            &f.store,
            &refreshed,
            CatalogAuthority::new(base()).with_pins(&f.grant),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::OperationConflict
    );
    assert_eq!(count(&f.store, "catalog_identity_pins"), 0);
    assert_eq!(count(&f.store, "catalog_command_receipts"), 0);
}
#[test]
fn production_populated_33_upgrade_preserves_raw_tuples_state_and_old_entity_index() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native33.db");
    migration::initialize_native_at(&path, 33).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    seed(&store);
    let old_tuples = tuples(&store);
    let old_state = state(&store);
    let old_revisions = revisions(&store, false);
    let a = grant(&["one", "two", "three"]);
    assert_eq!(
        pins::lookup_pin(&store, "one", NS, &a).unwrap_err().code,
        PinErrorCode::UnsupportedSchema
    );
    drop(store);
    let plan = migration::plan_native(&path).unwrap();
    migration::upgrade_native(
        &path,
        &plan,
        migration::NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        },
        None,
        &|_, _, _| Ok(()),
    )
    .unwrap();
    let upgraded = Store::open(&path, true, false).unwrap();
    assert_eq!(upgraded.schema_version, migration::NATIVE_SCHEMA_VERSION);
    assert_eq!(tuples(&upgraded), old_tuples);
    assert_eq!(state(&upgraded), old_state);
    assert_eq!(revisions(&upgraded, true), old_revisions);
    assert_eq!(count(&upgraded, "catalog_legacy_identity_evidence"), 3);
    assert_eq!(count(&upgraded, "catalog_identity_pins"), 0);
    let valid:i64=upgraded.db.query_row("SELECT count(*) FROM catalog_entity_revisions e JOIN catalog_legacy_identity_evidence l ON e.entity_kind='catalog_legacy_identity_evidence' AND e.entity_key=json_array(l.namespace,l.value,l.item_id) WHERE e.revision=1 AND e.deleted=0",[],|r|r.get(0)).unwrap();
    assert_eq!(valid, 3);
    let evidence = pins::legacy_identity_evidence(&upgraded, "one", NS, None, 128, &a).unwrap();
    assert_eq!(evidence.entries[0].value, "438631");
    assert_eq!(evidence.entries[0].provenance, "legacy_import");
    assert_eq!(
        pins::lookup_pin(&upgraded, "one", NS, &a)
            .unwrap()
            .accepted_value,
        None
    );
    assert!(
        !upgraded
            .db
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
}
#[test]
fn production_public_reads_reject_ambient_transactions_but_dispatch_apply_succeeds() {
    let f = Fixture::new();
    let b = batch(vec![set("one", 0, "a")]);
    let c = f.command(b.clone(), "apply");
    f.store.db.execute_batch("BEGIN").unwrap();
    assert_eq!(
        pins::preview_pins(&f.store, &b, &f.grant).unwrap_err().code,
        PinErrorCode::AmbientTransaction
    );
    assert_eq!(
        pins::lookup_pin(&f.store, "one", NS, &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::AmbientTransaction
    );
    assert_eq!(
        pins::pin_history(&f.store, "one", NS, 0, 128, &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::AmbientTransaction
    );
    assert_eq!(
        pins::legacy_identity_evidence(&f.store, "one", NS, None, 128, &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::AmbientTransaction
    );
    assert_eq!(
        pins::accepted_identity_owner(&f.store, NS, "a", &["one".into()], &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::AmbientTransaction
    );
    f.store.db.execute_batch("ROLLBACK").unwrap();
    assert!(matches!(f.execute(&c), CommandOutcome::Committed { .. }));
}
#[test]
fn production_public_reads_reject_store_path_replacement_at_entry() {
    let f = Fixture::new();
    let displaced = f.path.with_extension("displaced");
    std::fs::rename(&f.path, &displaced).unwrap();
    std::fs::copy(&displaced, &f.path).unwrap();
    assert_eq!(
        pins::preview_pins(&f.store, &batch(vec![set("one", 0, "a")]), &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::StoreFailure
    );
    assert_eq!(
        pins::lookup_pin(&f.store, "one", NS, &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::StoreFailure
    );
    assert_eq!(
        pins::pin_history(&f.store, "one", NS, 0, 128, &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::StoreFailure
    );
    assert_eq!(
        pins::legacy_identity_evidence(&f.store, "one", NS, None, 128, &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::StoreFailure
    );
    assert_eq!(
        pins::accepted_identity_owner(&f.store, NS, "a", &["one".into()], &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::StoreFailure
    );
}
#[test]
fn production_public_read_validates_lifetime_again_after_reading() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let f = Fixture::new();
    f.execute(&f.command(batch(vec![set("one", 0, "a")]), "seed"));
    let path = f.path.clone();
    let displaced = path.with_extension("during-read-old");
    let fired = Arc::new(AtomicBool::new(false));
    let observed = fired.clone();
    // Test-only fault injection into SQL preparation: change pathname identity
    // after entry admission but before the authorized pin result is returned.
    f.store
        .db
        .authorizer(Some(move |ctx: AuthContext<'_>| {
            if matches!(
                ctx.action,
                AuthAction::Read {
                    table_name: "catalog_identity_pins",
                    ..
                }
            ) && !fired.swap(true, Ordering::SeqCst)
            {
                std::fs::rename(&path, &displaced).unwrap();
                std::fs::copy(&displaced, &path).unwrap();
            }
            Authorization::Allow
        }))
        .unwrap();
    assert_eq!(
        pins::lookup_pin(&f.store, "one", NS, &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::StoreFailure
    );
    assert!(observed.load(Ordering::SeqCst));
    f.store
        .db
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .unwrap();
}
#[test]
fn production_failed_detach_leaves_pin_reads_closed() {
    let mut f = Fixture::new();
    let path = f.path.clone();
    let displaced = path.with_extension("detached-old");
    assert!(
        f.store
            .detached(|| {
                std::fs::rename(&path, &displaced)?;
                std::fs::copy(&displaced, &path)?;
                Ok(())
            })
            .is_err()
    );
    assert_eq!(
        pins::lookup_pin(&f.store, "one", NS, &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::StoreFailure
    );
    assert_eq!(
        pins::preview_pins(&f.store, &batch(vec![set("one", 0, "a")]), &f.grant)
            .unwrap_err()
            .code,
        PinErrorCode::StoreFailure
    );
}
