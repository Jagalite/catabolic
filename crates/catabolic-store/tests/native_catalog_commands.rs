//! Durable commands use real disposable SQLite catalogs. No production restore,
//! publication, external effects or legacy default migration behavior is changed.
use catabolic_core::catalog_command::{
    CommandEnvelope, CommandError, CommandKey, ReceiptVisibility,
};
use catabolic_store::{
    OpenErrorCode, Store,
    catalog_commands::{
        self as commands, CommandOutcome, CommandStoreErrorCode, HoldOutcome, ReceiptPolicy,
        TrustedCommandAuthority, TrustedReceiptMaintenance,
    },
    migration,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};

struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native.db");
        migration::initialize_native(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        Self {
            _root: root,
            path,
            store,
        }
    }
    fn command(&self, operation: &str, payload: Value) -> CommandEnvelope {
        envelope(&self.store, operation, payload)
    }
    fn put(&self, operation: &str, id: &str) -> CommandEnvelope {
        self.command(operation, json!({"operation":"item_put","item":item(id)}))
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
fn authority() -> TrustedCommandAuthority<'static> {
    TrustedCommandAuthority {
        host_namespace: "test-host",
        receipt_visibility: ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}
fn maintenance() -> TrustedReceiptMaintenance<'static> {
    TrustedReceiptMaintenance {
        host_namespace: "test-host",
        holder_namespace: "test-holder",
    }
}
fn generation(store: &Store) -> i64 {
    store
        .db
        .query_row(
            "SELECT generation FROM catalog_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap()
}
fn count(store: &Store, table: &str) -> i64 {
    assert!(
        [
            "items",
            "catalog_command_receipts",
            "catalog_receipt_holds",
            "catalog_mutation_permits"
        ]
        .contains(&table)
    );
    store
        .db
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}
fn envelope(store: &Store, operation: &str, payload: Value) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, "test-host", operation).unwrap(),
        store.catalog_incarnation.clone().unwrap(),
        generation(store),
        &payload.to_string(),
    )
    .unwrap()
}
fn item(id: &str) -> Value {
    json!({"id":id,"kind":"video","metadata":{"title":id}})
}
fn run(store: &Store, command: &CommandEnvelope) -> CommandOutcome {
    commands::execute(store, command, authority(), policy()).unwrap()
}
fn result(outcome: &CommandOutcome) -> &Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        _ => panic!("expected result: {outcome:?}"),
    }
}

#[test]
fn lost_reply_reopens_and_replays_original_generated_id_and_final_generation() {
    let fixture = Fixture::new();
    let command = fixture.command("lost-reply",json!({"operation":"item_put","item":{"kind":"video","metadata":{"title":"original"},"identities":{"imdb":"tt-test"}}}));
    let first = run(&fixture.store, &command);
    let id = result(&first)["item"]["id"].as_str().unwrap().to_owned();
    assert!(!id.is_empty());
    let committed_generation = generation(&fixture.store);
    assert!(committed_generation >= 2);
    let stored_generation: i64 = fixture
        .store
        .db
        .query_row(
            "SELECT committed_generation FROM catalog_command_receipts",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(committed_generation, stored_generation);
    let Fixture { _root, path, store } = fixture;
    drop(store); // The successful response is deliberately discarded by the host.
    let store = Store::open(&path, true, false).unwrap();
    let replay = run(&store, &command);
    assert_eq!(replay.code(), "replay");
    assert_eq!(result(&replay), result(&first));
    assert_eq!(result(&replay)["item"]["id"], id);
    assert_eq!(generation(&store), committed_generation);
    assert_eq!(count(&store, "items"), 1);
    assert_eq!(count(&store, "catalog_command_receipts"), 1);
    drop(_root);
}

#[test]
fn exact_replay_precedes_staleness_storage_and_current_body_visibility() {
    let fixture = Fixture::new();
    let command = fixture.put("one", "item-a");
    let first = run(&fixture.store, &command);
    run(&fixture.store, &fixture.put("two", "item-b"));
    let exhausted = ReceiptPolicy {
        max_receipts: 0,
        max_logical_bytes: 0,
        retention: Duration::MAX,
        ..policy()
    };
    let replay = commands::execute(&fixture.store, &command, authority(), exhausted).unwrap();
    assert_eq!(result(&first), result(&replay));
    let redacted = TrustedCommandAuthority {
        receipt_visibility: ReceiptVisibility::CommittedStatusOnly,
        allow_new_commands: false,
        ..authority()
    };
    assert_eq!(
        commands::execute(&fixture.store, &command, redacted, exhausted).unwrap(),
        CommandOutcome::CommittedRedacted
    );
    assert_eq!(
        serde_json::to_value(CommandOutcome::CommittedRedacted).unwrap(),
        json!({"status":"committed_redacted"})
    );
    let fresh = fixture.put("three", "item-c");
    assert_eq!(
        commands::execute(&fixture.store, &fresh, redacted, policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::AuthorityDenied
    );
    let redacted_writer = TrustedCommandAuthority {
        allow_new_commands: true,
        ..redacted
    };
    assert_eq!(
        commands::execute(&fixture.store, &fresh, redacted_writer, policy()).unwrap(),
        CommandOutcome::CommittedRedacted
    );
    assert_eq!(count(&fixture.store, "items"), 3);
}

#[test]
fn payload_or_precondition_reuse_conflicts_and_new_stale_command_does_not_write() {
    let fixture = Fixture::new();
    let original = fixture.put("operation", "original");
    let stale = fixture.put("stale", "not-written");
    run(&fixture.store, &original);
    let conflicting = CommandEnvelope::new(
        1,
        original.key().clone(),
        original.admission_incarnation(),
        original.expected_generation(),
        &json!({"operation":"item_put","item":item("different")}).to_string(),
    )
    .unwrap();
    assert_eq!(run(&fixture.store, &conflicting), CommandOutcome::Conflict);
    let changed_precondition = fixture.put("operation", "original");
    assert_eq!(
        run(&fixture.store, &changed_precondition),
        CommandOutcome::Conflict
    );
    assert_eq!(run(&fixture.store, &stale), CommandOutcome::StaleGeneration);
    assert_eq!(count(&fixture.store, "items"), 1);
    assert_eq!(count(&fixture.store, "catalog_command_receipts"), 1);
}

#[test]
fn namespace_and_catalog_keys_are_checked_before_replay() {
    let fixture = Fixture::new();
    let command = fixture.put("key", "item");
    run(&fixture.store, &command);
    let wrong = TrustedCommandAuthority {
        host_namespace: "other-host",
        ..authority()
    };
    assert_eq!(
        commands::execute(&fixture.store, &command, wrong, policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::Validation(CommandError::HostNamespaceMismatch)
    );
    let other = CommandEnvelope::new(
        1,
        CommandKey::new("other-catalog", "test-host", "key").unwrap(),
        command.admission_incarnation(),
        command.expected_generation(),
        &command.payload().to_string(),
    )
    .unwrap();
    assert_eq!(
        commands::execute(&fixture.store, &other, authority(), policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::Validation(CommandError::CatalogMismatch)
    );
}

#[test]
fn batch_failure_rolls_back_earlier_rows_generation_and_receipt() {
    let fixture = Fixture::new();
    let initial_generation = generation(&fixture.store);
    let command = fixture.command("batch-fail",json!({"operation":"item_put_batch","items":[item("same"),{"id":"same","kind":"movie","metadata":{}}]}));
    let error = commands::execute(&fixture.store, &command, authority(), policy()).unwrap_err();
    assert_eq!(error.code, CommandStoreErrorCode::OperationConflict);
    assert_eq!(generation(&fixture.store), initial_generation);
    assert_eq!(count(&fixture.store, "items"), 0);
    assert_eq!(count(&fixture.store, "catalog_command_receipts"), 0);
    assert!(fixture.store.db.is_autocommit());
    let success = fixture.command(
        "batch-success",
        json!({"operation":"item_put_batch","items":[item("first"),item("second")]}),
    );
    assert_eq!(
        result(&run(&fixture.store, &success))["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(count(&fixture.store, "catalog_command_receipts"), 1);
}

#[test]
fn receipt_insert_and_commit_failures_never_acknowledge_or_preserve_mutations() {
    let fixture = Fixture::new();
    let command = fixture.put("failure", "item");
    let original_generation = generation(&fixture.store);
    fixture.store.db.execute_batch("CREATE TEMP TRIGGER reject_receipt BEFORE INSERT ON catalog_command_receipts BEGIN SELECT RAISE(ABORT,'injected_receipt_failure'); END;").unwrap();
    assert!(commands::execute(&fixture.store, &command, authority(), policy()).is_err());
    assert_eq!(count(&fixture.store, "items"), 0);
    assert_eq!(generation(&fixture.store), original_generation);
    assert_eq!(count(&fixture.store, "catalog_command_receipts"), 0);
    fixture.store.db.execute_batch("DROP TRIGGER reject_receipt; CREATE TEMP TRIGGER fail_commit AFTER INSERT ON catalog_command_receipts BEGIN INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) SELECT 'context','injected',incarnation FROM catalog_state; END;").unwrap();
    assert!(commands::execute(&fixture.store, &command, authority(), policy()).is_err());
    assert!(fixture.store.db.is_autocommit());
    assert_eq!(count(&fixture.store, "items"), 0);
    assert_eq!(count(&fixture.store, "catalog_command_receipts"), 0);
    assert_eq!(count(&fixture.store, "catalog_mutation_permits"), 0);
    assert_eq!(generation(&fixture.store), original_generation);
    fixture
        .store
        .db
        .execute_batch("DROP TRIGGER fail_commit")
        .unwrap();
    assert_eq!(run(&fixture.store, &command).code(), "committed");
}

#[test]
fn receipt_storage_count_and_actual_result_bytes_refuse_without_eviction() {
    let fixture = Fixture::new();
    let command = fixture.put("large", "item");
    let no_count = ReceiptPolicy {
        max_receipts: 0,
        ..policy()
    };
    assert_eq!(
        commands::execute(&fixture.store, &command, authority(), no_count).unwrap(),
        CommandOutcome::ReceiptStorageExhausted
    );
    let too_small = ReceiptPolicy {
        max_logical_bytes: 1,
        ..policy()
    };
    assert_eq!(
        commands::execute(&fixture.store, &command, authority(), too_small).unwrap(),
        CommandOutcome::ReceiptStorageExhausted
    );
    // The fixed key/digest overhead fits; the actual result does not. This must
    // exercise rollback after the legacy item-put owner has already run.
    let fixed = 256
        + command.key().catalog_id().len()
        + command.key().host_namespace().len()
        + command.key().operation_id().len()
        + 64
        + command.admission_incarnation().len();
    let only_overhead = ReceiptPolicy {
        max_logical_bytes: fixed as u64,
        ..policy()
    };
    let before = generation(&fixture.store);
    assert_eq!(
        commands::execute(&fixture.store, &command, authority(), only_overhead).unwrap(),
        CommandOutcome::ReceiptStorageExhausted
    );
    assert_eq!(generation(&fixture.store), before);
    assert_eq!(count(&fixture.store, "items"), 0);
    assert_eq!(commands::receipt_usage(&fixture.store).unwrap().receipts, 0);
    run(&fixture.store, &command);
    let usage = commands::receipt_usage(&fixture.store).unwrap();
    assert!(usage.logical_bytes > fixed as u64);
    let next = fixture.put("next", "next-item");
    let full = ReceiptPolicy {
        max_receipts: 1,
        ..policy()
    };
    assert_eq!(
        commands::execute(&fixture.store, &next, authority(), full).unwrap(),
        CommandOutcome::ReceiptStorageExhausted
    );
    assert_eq!(run(&fixture.store, &command).code(), "replay");
}

#[test]
fn retention_holds_and_tombstones_are_permanent_and_do_not_advance_generation() {
    let fixture = Fixture::new();
    let command = fixture.put("retained", "item");
    let immediate = ReceiptPolicy {
        retention: Duration::ZERO,
        ..policy()
    };
    commands::execute(&fixture.store, &command, authority(), immediate).unwrap();
    let before = generation(&fixture.store);
    let usage = commands::receipt_usage(&fixture.store).unwrap();
    assert_eq!(
        commands::acquire_hold(
            &fixture.store,
            command.key(),
            maintenance(),
            "hold",
            policy()
        )
        .unwrap(),
        HoldOutcome::Acquired
    );
    assert_eq!(
        commands::acquire_hold(
            &fixture.store,
            command.key(),
            maintenance(),
            "hold",
            ReceiptPolicy {
                max_hold_records: 0,
                ..policy()
            }
        )
        .unwrap(),
        HoldOutcome::AlreadyHeld
    );
    assert_eq!(
        commands::compact_receipts(&fixture.store, maintenance(), 10).unwrap(),
        0
    );
    assert_eq!(
        commands::release_hold(&fixture.store, command.key(), maintenance(), "hold").unwrap(),
        HoldOutcome::Released
    );
    assert_eq!(
        commands::release_hold(&fixture.store, command.key(), maintenance(), "hold").unwrap(),
        HoldOutcome::AlreadyReleased
    );
    assert_eq!(
        commands::acquire_hold(
            &fixture.store,
            command.key(),
            maintenance(),
            "hold",
            policy()
        )
        .unwrap(),
        HoldOutcome::AlreadyReleased
    );
    assert_eq!(
        commands::compact_receipts(&fixture.store, maintenance(), 10).unwrap(),
        1
    );
    assert_eq!(
        commands::compact_receipts(&fixture.store, maintenance(), 10).unwrap(),
        0
    );
    assert_eq!(
        run(&fixture.store, &command),
        CommandOutcome::ReceiptExpired
    );
    assert_eq!(
        commands::acquire_hold(
            &fixture.store,
            command.key(),
            maintenance(),
            "another",
            policy()
        )
        .unwrap(),
        HoldOutcome::ReceiptExpired
    );
    assert_eq!(count(&fixture.store, "catalog_command_receipts"), 1);
    assert_eq!(count(&fixture.store, "catalog_receipt_holds"), 1);
    assert_eq!(generation(&fixture.store), before);
    let after = commands::receipt_usage(&fixture.store).unwrap();
    assert_eq!(after.receipts, usage.receipts);
    assert_eq!(after.hold_records, 1);
    assert!(
        fixture
            .store
            .db
            .execute("DELETE FROM catalog_command_receipts", [])
            .is_err()
    );
    assert!(
        fixture
            .store
            .db
            .execute("DELETE FROM catalog_receipt_holds", [])
            .is_err()
    );
    let reuse = fixture.put("retained", "replacement");
    assert_eq!(run(&fixture.store, &reuse), CommandOutcome::Conflict);
    let fresh = fixture.put("fresh", "fresh-item");
    assert_eq!(
        commands::execute(
            &fixture.store,
            &fresh,
            authority(),
            ReceiptPolicy {
                max_receipts: 1,
                ..policy()
            }
        )
        .unwrap(),
        CommandOutcome::ReceiptStorageExhausted
    );
}

#[test]
fn maintenance_limits_and_namespaces_do_not_allow_unbounded_or_early_compaction() {
    let fixture = Fixture::new();
    let command = fixture.put("future", "item");
    run(&fixture.store, &command);
    assert_eq!(
        commands::compact_receipts(&fixture.store, maintenance(), 10).unwrap(),
        0
    );
    assert_eq!(
        commands::compact_receipts(&fixture.store, maintenance(), 0)
            .unwrap_err()
            .code,
        CommandStoreErrorCode::InvalidCompactionLimit
    );
    assert_eq!(
        commands::compact_receipts(&fixture.store, maintenance(), 1025)
            .unwrap_err()
            .code,
        CommandStoreErrorCode::InvalidCompactionLimit
    );
    assert_eq!(
        commands::acquire_hold(
            &fixture.store,
            command.key(),
            maintenance(),
            "hold",
            ReceiptPolicy {
                max_hold_records: 0,
                ..policy()
            }
        )
        .unwrap(),
        HoldOutcome::ReceiptStorageExhausted
    );
    let bytes = commands::receipt_usage(&fixture.store)
        .unwrap()
        .logical_bytes;
    assert_eq!(
        commands::acquire_hold(
            &fixture.store,
            command.key(),
            maintenance(),
            "hold",
            ReceiptPolicy {
                max_logical_bytes: bytes,
                ..policy()
            }
        )
        .unwrap(),
        HoldOutcome::ReceiptStorageExhausted
    );
    assert_eq!(
        commands::acquire_hold(&fixture.store, command.key(), maintenance(), "", policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::InvalidHoldIdentifier
    );
    let wrong = TrustedReceiptMaintenance {
        host_namespace: "other",
        ..maintenance()
    };
    assert_eq!(
        commands::acquire_hold(&fixture.store, command.key(), wrong, "hold", policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::Validation(CommandError::HostNamespaceMismatch)
    );
    assert_eq!(
        commands::release_hold(&fixture.store, command.key(), maintenance(), "absent").unwrap(),
        HoldOutcome::HoldNotFound
    );
}

#[test]
fn public_entrypoints_reject_ambient_transactions_without_ending_them() {
    let fixture = Fixture::new();
    let command = fixture.put("outer", "item");
    fixture
        .store
        .db
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO items VALUES('outer-item','video','{}')")
        .unwrap();
    assert_eq!(
        commands::execute(&fixture.store, &command, authority(), policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::AmbientTransaction
    );
    assert_eq!(
        commands::acquire_hold(
            &fixture.store,
            command.key(),
            maintenance(),
            "hold",
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::AmbientTransaction
    );
    assert_eq!(
        commands::release_hold(&fixture.store, command.key(), maintenance(), "hold")
            .unwrap_err()
            .code,
        CommandStoreErrorCode::AmbientTransaction
    );
    assert_eq!(
        commands::compact_receipts(&fixture.store, maintenance(), 1)
            .unwrap_err()
            .code,
        CommandStoreErrorCode::AmbientTransaction
    );
    assert_eq!(
        commands::receipt_usage(&fixture.store).unwrap_err().code,
        CommandStoreErrorCode::AmbientTransaction
    );
    assert!(!fixture.store.db.is_autocommit());
    assert_eq!(count(&fixture.store, "items"), 1);
    fixture.store.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(count(&fixture.store, "items"), 0);
}

#[test]
fn old_incarnation_absent_receipt_is_uncertain_but_known_receipt_replays() {
    let fixture = Fixture::new();
    let committed = fixture.put("known", "known-item");
    let absent = fixture.put("absent", "never-written");
    let first = run(&fixture.store, &committed);
    let Fixture { _root, path, store } = fixture;
    drop(store);
    // Explicit fixture-only simulation under no live Store owners. This does
    // not expose or qualify production restore/publication.
    let db = catabolic_store::connect(&path, true).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) SELECT 'restore',catalog_id,incarnation FROM catalog_state; UPDATE catalog_state SET incarnation=lower(hex(randomblob(16))); DELETE FROM catalog_mutation_permits; COMMIT;").unwrap();
    drop(db);
    let store = Store::open(&path, true, false).unwrap();
    assert_ne!(
        store.catalog_incarnation.as_deref(),
        Some(committed.admission_incarnation())
    );
    assert_eq!(
        run(&store, &absent),
        CommandOutcome::OutcomeUnknownAfterRestore
    );
    assert_eq!(result(&run(&store, &committed)), result(&first));
    assert_eq!(count(&store, "items"), 1);
    drop(_root);
}

#[test]
fn concurrent_duplicate_admissions_serialize_to_one_commit_and_one_replay() {
    let fixture = Fixture::new();
    let command = fixture.put("concurrent", "one-item");
    let Fixture { _root, path, store } = fixture;
    drop(store);
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let barrier = barrier.clone();
            let path = path.clone();
            let command = command.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let deadline = Instant::now() + Duration::from_secs(10);
                let store = loop {
                    match Store::open_typed(&path, true, false) {
                        Ok(store) => break store,
                        Err(error)
                            if error.code == OpenErrorCode::BusyWriter
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("unexpected admission failure: {error:?}"),
                    }
                };
                run(&store, &command)
            })
        })
        .collect();
    barrier.wait();
    let outcomes: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.code() == "committed")
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.code() == "replay")
            .count(),
        1
    );
    assert_eq!(result(&outcomes[0]), result(&outcomes[1]));
    let store = Store::open(&path, true, false).unwrap();
    assert_eq!(count(&store, "items"), 1);
    assert_eq!(count(&store, "catalog_command_receipts"), 1);
    drop(_root);
}

#[test]
fn malformed_and_overlarge_operations_and_legacy_schema_fail_closed() {
    let fixture = Fixture::new();
    for payload in [
        json!({"operation":"raw_sql","sql":"DELETE FROM items"}),
        json!({"operation":"item_put","item":item("one"),"ignored":true}),
        json!({"operation":"item_put","item":{"id":"one","kind":"video","metadata":{},"ignored":true}}),
        json!({"operation":"item_put","item":{"id":" ","kind":"video","metadata":{}}}),
        json!({"operation":"item_put_batch","items":[]}),
        json!({"operation":"item_put_batch","items":vec![item("item");129]}),
    ] {
        assert_eq!(
            commands::execute(
                &fixture.store,
                &fixture.command("invalid", payload),
                authority(),
                policy()
            )
            .unwrap_err()
            .code,
            CommandStoreErrorCode::InvalidOperation
        );
    }
    assert_eq!(count(&fixture.store, "items"), 0);
    let command = fixture.put("invalid-retention", "item");
    assert_eq!(
        commands::execute(
            &fixture.store,
            &command,
            authority(),
            ReceiptPolicy {
                retention: Duration::MAX,
                ..policy()
            }
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::InvalidPolicy
    );
    let legacy = fixture._root.path().join("legacy.db");
    migration::initialize(&legacy).unwrap();
    let store = Store::open(&legacy, true, false).unwrap();
    assert_eq!(
        commands::execute(&store, &command, authority(), policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::UnsupportedSchema
    );
}

#[test]
fn unsafe_connection_settings_are_rejected_before_mutation() {
    let fixture = Fixture::new();
    let command = fixture.put("unsafe", "item");
    fixture
        .store
        .db
        .pragma_update(None, "recursive_triggers", false)
        .unwrap();
    assert_eq!(
        commands::execute(&fixture.store, &command, authority(), policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::StoreFailure
    );
    assert_eq!(count(&fixture.store, "items"), 0);
    assert!(fixture.store.db.is_autocommit());
}

#[test]
fn accepted_legacy_uuid_spellings_keep_raw_meta_but_use_canonical_receipt_keys() {
    let canonical = "6f6370bb-4274-409d-b757-2c9cf6b0ac43";
    for raw in [
        canonical.to_ascii_uppercase(),
        canonical.replace('-', ""),
        format!("{{{canonical}}}"),
        format!("urn:uuid:{canonical}"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("legacy-spelling.db");
        migration::initialize(&path).unwrap();
        let db = catabolic_store::connect(&path, true).unwrap();
        db.execute("UPDATE meta SET value=?1 WHERE key='database_id'", [&raw])
            .unwrap();
        drop(db);
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
        let store = Store::open(&path, true, false).unwrap();
        assert_eq!(store.database_id, canonical);
        let retained: String = store
            .db
            .query_row(
                "SELECT value FROM meta WHERE key='database_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained, raw);
        let command = envelope(
            &store,
            "canonical-key",
            json!({"operation":"item_put","item":item("canonical-item")}),
        );
        commands::execute(
            &store,
            &command,
            authority(),
            ReceiptPolicy {
                retention: Duration::ZERO,
                ..policy()
            },
        )
        .unwrap();
        assert_eq!(
            commands::acquire_hold(&store, command.key(), maintenance(), "hold", policy()).unwrap(),
            HoldOutcome::Acquired
        );
        assert_eq!(
            commands::release_hold(&store, command.key(), maintenance(), "hold").unwrap(),
            HoldOutcome::Released
        );
        assert_eq!(
            commands::compact_receipts(&store, maintenance(), 1).unwrap(),
            1
        );
        assert_eq!(run(&store, &command), CommandOutcome::ReceiptExpired);
        let key: String = store
            .db
            .query_row(
                "SELECT catalog_id FROM catalog_command_receipts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(key, canonical);
        assert_eq!(commands::receipt_usage(&store).unwrap().receipts, 1);
    }
}

#[test]
fn no_op_puts_get_distinct_receipts_without_fabricating_generation_changes() {
    let fixture = Fixture::new();
    let first = fixture.put("first", "same");
    run(&fixture.store, &first);
    let before = generation(&fixture.store);
    let second = fixture.put("second", "same");
    let second_result = run(&fixture.store, &second);
    assert_eq!(second_result.code(), "committed");
    assert_eq!(generation(&fixture.store), before);
    assert_eq!(count(&fixture.store, "items"), 1);
    assert_eq!(count(&fixture.store, "catalog_command_receipts"), 2);
    assert_eq!(
        result(&run(&fixture.store, &second)),
        result(&second_result)
    );
}

#[test]
fn compaction_reclaims_result_bytes_but_retains_key_and_hold_record_budgets() {
    let fixture = Fixture::new();
    let command = fixture.command("large-body",json!({"operation":"item_put","item":{"id":"item","kind":"video","metadata":{"title":"x".repeat(8192)}}}));
    commands::execute(
        &fixture.store,
        &command,
        authority(),
        ReceiptPolicy {
            retention: Duration::ZERO,
            ..policy()
        },
    )
    .unwrap();
    let before = commands::receipt_usage(&fixture.store).unwrap();
    let next = fixture.put("next", "new-item");
    let budget = ReceiptPolicy {
        max_logical_bytes: before.logical_bytes,
        ..policy()
    };
    assert_eq!(
        commands::execute(&fixture.store, &next, authority(), budget).unwrap(),
        CommandOutcome::ReceiptStorageExhausted
    );
    assert_eq!(
        commands::compact_receipts(&fixture.store, maintenance(), 1).unwrap(),
        1
    );
    let after = commands::receipt_usage(&fixture.store).unwrap();
    assert_eq!(after.receipts, 1);
    assert!(before.logical_bytes - after.logical_bytes > 8192);
    assert_eq!(
        commands::execute(&fixture.store, &next, authority(), budget)
            .unwrap()
            .code(),
        "committed"
    );
    commands::acquire_hold(&fixture.store, next.key(), maintenance(), "one", policy()).unwrap();
    commands::release_hold(&fixture.store, next.key(), maintenance(), "one").unwrap();
    assert_eq!(
        commands::acquire_hold(
            &fixture.store,
            next.key(),
            maintenance(),
            "two",
            ReceiptPolicy {
                max_hold_records: 1,
                ..policy()
            }
        )
        .unwrap(),
        HoldOutcome::ReceiptStorageExhausted
    );
    assert_eq!(
        commands::receipt_usage(&fixture.store)
            .unwrap()
            .hold_records,
        1
    );
}
