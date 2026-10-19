//! Real Store34/dispatcher metadata acceptance fixtures. Only disposable catalogs;
//! no production restore, external effects, alternate dispatcher or raw query API.
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility},
    catalog_metadata::{FieldResolution, SourceKind},
};
use catabolic_store::{
    Store,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, CommandStoreErrorCode, ReceiptPolicy,
        TrustedCommandAuthority, TrustedReceiptMaintenance,
    },
    catalog_metadata_store::{
        self as metadata, MetadataAccess, MetadataAuthority, MetadataChange, MetadataMutation,
        MetadataReadAuthority, MetadataStoreErrorCode, PolicyField, PolicyNormalization,
        PolicyValueType,
    },
    migration,
};
use rusqlite::{params, types::Value as SqlValue};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

const HOST: &str = "metadata-test-host";
const ITEM: &str = "metadata-item";
struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        Self::at_version(34)
    }
    fn at_version(version: u32) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        migration::initialize_native_at(&path, version).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        assert_eq!(store.schema_version, version);
        let fixture = Self {
            _root: root,
            path,
            store,
        };
        let seed = envelope(
            &fixture.store,
            "seed",
            json!({"operation":"item_put","item":{"id":ITEM,"kind":"video","metadata":{}}}),
        );
        assert!(matches!(
            commands::execute(&fixture.store, &seed, base(), policy()).unwrap(),
            CommandOutcome::Committed { .. }
        ));
        fixture
    }
    fn initialize(&self) -> CommandEnvelope {
        let command = envelope(&self.store, "initialize", payload(init()));
        assert!(matches!(
            run(&self.store, &command, &grant(MetadataAccess::Initialize)),
            CommandOutcome::Committed { .. }
        ));
        command
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
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
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
fn policies() -> Vec<PolicyField> {
    vec![
        PolicyField {
            field: "title".into(),
            value_type: PolicyValueType::String,
            nullable: true,
            normalization: PolicyNormalization::TrimAsciiLowercase,
        },
        PolicyField {
            field: "year".into(),
            value_type: PolicyValueType::Integer,
            nullable: false,
            normalization: PolicyNormalization::Exact,
        },
    ]
}
fn init() -> MetadataMutation {
    MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Initialize {
            subject: ITEM.into(),
            policies: policies(),
        },
    }
}
fn fingerprint(store: &Store) -> String {
    store
        .db
        .query_row(
            "SELECT schema_fingerprint FROM catalog_metadata_subjects WHERE item_id=?1",
            [ITEM],
            |r| r.get(0),
        )
        .unwrap()
}
fn patch(store: &Store, namespace: &str, expected: i64, edits: Value) -> MetadataMutation {
    MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Patch {
            subject: ITEM.into(),
            namespace: namespace.into(),
            record_id: "record".into(),
            schema_fingerprint: fingerprint(store),
            expected_revision: expected,
            edits_json: edits.to_string(),
            evidence_json: r#"{"observed":"fixture"}"#.into(),
        },
    }
}
fn payload(mutation: MetadataMutation) -> Value {
    json!({"operation":"metadata","mutation":mutation})
}
fn grant(access: MetadataAccess) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: HOST.into(),
        subject: ITEM.into(),
        access,
    }
}
fn provider(namespace: &str) -> MetadataAuthority {
    grant(MetadataAccess::Source {
        namespace: namespace.into(),
        record_id: "record".into(),
        kind: SourceKind::Provider,
    })
}
fn local(namespace: &str) -> MetadataAuthority {
    grant(MetadataAccess::Source {
        namespace: namespace.into(),
        record_id: "record".into(),
        kind: SourceKind::Local,
    })
}
fn run(store: &Store, command: &CommandEnvelope, grant: &MetadataAuthority) -> CommandOutcome {
    commands::execute_with_authority(
        store,
        command,
        CatalogAuthority::new(base()).with_metadata(grant),
        policy(),
    )
    .unwrap()
}
fn body(outcome: &CommandOutcome) -> &Value {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        _ => panic!("no result body: {outcome:?}"),
    }
}
fn raw_metadata(store: &Store) -> String {
    store
        .db
        .query_row("SELECT metadata FROM items WHERE id=?1", [ITEM], |r| {
            r.get(0)
        })
        .unwrap()
}
fn read_authority(store: &Store) -> MetadataReadAuthority {
    MetadataReadAuthority {
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone().unwrap(),
        subject: ITEM.into(),
        expected_generation: generation(store),
    }
}
fn snapshot(store: &Store) -> BTreeMap<&'static str, Vec<Vec<SqlValue>>> {
    [
        "items",
        "catalog_state",
        "catalog_metadata_subjects",
        "catalog_metadata_history",
        "catalog_metadata_sources",
        "catalog_metadata_projection",
        "catalog_entity_revisions",
        "catalog_command_receipts",
        "catalog_metadata_permits",
        "catalog_mutation_permits",
        "catalog_logical_edit_permits",
    ]
    .into_iter()
    .map(|table| {
        let mut statement = store
            .db
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
            .unwrap();
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get(column))
                    .collect::<rusqlite::Result<Vec<SqlValue>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        (table, rows)
    })
    .collect()
}
fn assert_no_permits(store: &Store) {
    for table in [
        "catalog_metadata_permits",
        "catalog_mutation_permits",
        "catalog_logical_edit_permits",
    ] {
        assert!(
            !store
                .db
                .prepare(&format!("SELECT 1 FROM {table} LIMIT 1"))
                .unwrap()
                .exists([])
                .unwrap()
        );
    }
}
fn fixed_receipt_bytes(command: &CommandEnvelope) -> u64 {
    (256 + command.key().catalog_id().len()
        + command.key().host_namespace().len()
        + command.key().operation_id().len()
        + command.digest().as_str().len()
        + command.admission_incarnation().len()) as u64
}

#[test]
fn metadata_receipt_replay_precedes_stale_generation_write_grants_and_storage_admission() {
    let fixture = Fixture::new();
    let command = fixture.initialize();
    let first = commands::execute(&fixture.store, &command, base(), policy()).unwrap();
    assert!(matches!(first, CommandOutcome::Replay { .. }));
    let update = envelope(
        &fixture.store,
        "provider",
        payload(patch(
            &fixture.store,
            "p",
            0,
            json!({"title":{"op":"set","value":"new"}}),
        )),
    );
    run(&fixture.store, &update, &provider("p"));
    let before = snapshot(&fixture.store);
    let exhausted = ReceiptPolicy {
        retention: Duration::MAX,
        max_receipts: 0,
        max_logical_bytes: 0,
        ..policy()
    };
    let wrong = provider("wrong");
    let replay = commands::execute_with_authority(
        &fixture.store,
        &command,
        CatalogAuthority::new(base()).with_metadata(&wrong),
        exhausted,
    )
    .unwrap();
    assert_eq!(body(&replay), body(&first));
    assert_eq!(snapshot(&fixture.store), before);
    // FullResult is independently trusted current receipt-read authority. A
    // revoked/absent write grant must not remove that separately granted access.
    let read_only = TrustedCommandAuthority {
        allow_new_commands: false,
        ..base()
    };
    assert!(matches!(
        commands::execute_with_authority(
            &fixture.store,
            &command,
            CatalogAuthority::new(read_only),
            exhausted
        )
        .unwrap(),
        CommandOutcome::Replay { .. }
    ));
    let redacted = TrustedCommandAuthority {
        receipt_visibility: ReceiptVisibility::CommittedStatusOnly,
        ..read_only
    };
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &command,
            CatalogAuthority::new(redacted),
            exhausted
        )
        .unwrap(),
        CommandOutcome::CommittedRedacted
    );
}
#[test]
fn receipt_conflict_precedes_metadata_payload_parsing_and_current_grant_lookup() {
    let fixture = Fixture::new();
    let committed = fixture.initialize();
    let malformed = CommandEnvelope::new(
        1,
        committed.key().clone(),
        committed.admission_incarnation(),
        committed.expected_generation(),
        r#"{"operation":"metadata","mutation":{"unrecognized":"invalid"}}"#,
    )
    .unwrap();
    let before = snapshot(&fixture.store);
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &malformed,
            CatalogAuthority::new(base()),
            policy()
        )
        .unwrap(),
        CommandOutcome::Conflict
    );
    assert_eq!(snapshot(&fixture.store), before);
    let fresh = envelope(&fixture.store, "new-malformed", malformed.payload().clone());
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &fresh,
            CatalogAuthority::new(base()),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::InvalidOperation
    );
}
#[test]
fn expired_metadata_receipt_never_reexecutes_with_missing_write_grant() {
    let fixture = Fixture::new();
    let command = envelope(&fixture.store, "zero-retention", payload(init()));
    let authority = grant(MetadataAccess::Initialize);
    let immediate = ReceiptPolicy {
        retention: Duration::ZERO,
        ..policy()
    };
    run_with_policy(&fixture.store, &command, &authority, immediate);
    let before_generation = generation(&fixture.store);
    assert_eq!(
        commands::compact_receipts(
            &fixture.store,
            TrustedReceiptMaintenance {
                host_namespace: HOST,
                holder_namespace: "holder"
            },
            10
        )
        .unwrap(),
        1
    );
    let before = snapshot(&fixture.store);
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &command,
            CatalogAuthority::new(base()),
            policy()
        )
        .unwrap(),
        CommandOutcome::ReceiptExpired
    );
    assert_eq!(generation(&fixture.store), before_generation);
    assert_eq!(snapshot(&fixture.store), before);
}
fn run_with_policy(
    store: &Store,
    command: &CommandEnvelope,
    authority: &MetadataAuthority,
    policy: ReceiptPolicy,
) -> CommandOutcome {
    commands::execute_with_authority(
        store,
        command,
        CatalogAuthority::new(base()).with_metadata(authority),
        policy,
    )
    .unwrap()
}
#[test]
fn new_metadata_commands_require_current_exact_scope_and_cannot_claim_source_kind() {
    let fixture = Fixture::new();
    let initialize = envelope(&fixture.store, "initialize", payload(init()));
    let before = snapshot(&fixture.store);
    assert_eq!(
        commands::execute(&fixture.store, &initialize, base(), policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::AuthorityDenied
    );
    assert_eq!(snapshot(&fixture.store), before);
    run(
        &fixture.store,
        &initialize,
        &grant(MetadataAccess::Initialize),
    );
    let command = envelope(
        &fixture.store,
        "scoped",
        payload(patch(&fixture.store, "p", 0, json!({}))),
    );
    let mut wrong_subject = provider("p");
    wrong_subject.subject = "another-item".into();
    let mut wrong_host = provider("p");
    wrong_host.host_namespace = "another-host".into();
    let wrong_record = grant(MetadataAccess::Source {
        namespace: "p".into(),
        record_id: "other-record".into(),
        kind: SourceKind::Provider,
    });
    let before = snapshot(&fixture.store);
    for authority in [
        provider("other"),
        wrong_subject,
        wrong_host,
        wrong_record,
        grant(MetadataAccess::Initialize),
    ] {
        assert_eq!(
            commands::execute_with_authority(
                &fixture.store,
                &command,
                CatalogAuthority::new(base()).with_metadata(&authority),
                policy()
            )
            .unwrap_err()
            .code,
            CommandStoreErrorCode::AuthorityDenied
        );
        assert_eq!(snapshot(&fixture.store), before);
    }
    let mut claimed = command.payload().clone();
    claimed["mutation"]["change"]["kind"] = json!("local");
    let claimed = envelope(&fixture.store, "claim-local", claimed);
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &claimed,
            CatalogAuthority::new(base()).with_metadata(&provider("p")),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::InvalidOperation
    );
    run(&fixture.store, &command, &provider("p"));
    assert_no_permits(&fixture.store);
}
#[test]
fn actual_receipt_result_bytes_roll_back_initialization_then_same_key_can_retry() {
    let fixture = Fixture::new();
    let command = envelope(&fixture.store, "limited-init", payload(init()));
    let grant = grant(MetadataAccess::Initialize);
    let before = snapshot(&fixture.store);
    let usage = commands::receipt_usage(&fixture.store).unwrap();
    let tiny = ReceiptPolicy {
        max_logical_bytes: usage.logical_bytes + fixed_receipt_bytes(&command),
        ..policy()
    };
    assert_eq!(
        run_with_policy(&fixture.store, &command, &grant, tiny),
        CommandOutcome::ReceiptStorageExhausted
    );
    assert_eq!(snapshot(&fixture.store), before);
    assert_no_permits(&fixture.store);
    assert!(matches!(
        run(&fixture.store, &command, &grant),
        CommandOutcome::Committed { .. }
    ));
}
#[test]
fn actual_receipt_result_bytes_roll_back_source_history_projection_and_invalidations() {
    let fixture = Fixture::new();
    fixture.initialize();
    let command = envelope(
        &fixture.store,
        "limited-patch",
        payload(patch(
            &fixture.store,
            "p",
            0,
            json!({"title":{"op":"set","value":"not committed"}}),
        )),
    );
    let before = snapshot(&fixture.store);
    let usage = commands::receipt_usage(&fixture.store).unwrap();
    let tiny = ReceiptPolicy {
        max_logical_bytes: usage.logical_bytes + fixed_receipt_bytes(&command),
        ..policy()
    };
    assert_eq!(
        run_with_policy(&fixture.store, &command, &provider("p"), tiny),
        CommandOutcome::ReceiptStorageExhausted
    );
    assert_eq!(snapshot(&fixture.store), before);
    assert_no_permits(&fixture.store);
    assert!(fixture.store.db.is_autocommit());
    let Fixture { _root, path, store } = fixture;
    drop(store);
    let reopened = Store::open(&path, true, false).unwrap();
    assert_eq!(snapshot(&reopened), before);
}
#[test]
fn stale_source_cas_is_typed_conflict_with_fresh_outer_generation() {
    let fixture = Fixture::new();
    fixture.initialize();
    let first = envelope(
        &fixture.store,
        "first",
        payload(patch(
            &fixture.store,
            "p",
            0,
            json!({"title":{"op":"set","value":"first"}}),
        )),
    );
    run(&fixture.store, &first, &provider("p"));
    let stale = envelope(
        &fixture.store,
        "stale-source",
        payload(patch(
            &fixture.store,
            "p",
            0,
            json!({"title":{"op":"set","value":"stale"}}),
        )),
    );
    let before = snapshot(&fixture.store);
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &stale,
            CatalogAuthority::new(base()).with_metadata(&provider("p")),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::OperationConflict
    );
    assert_eq!(snapshot(&fixture.store), before);
    let remove = envelope(
        &fixture.store,
        "remove",
        payload(patch(
            &fixture.store,
            "p",
            1,
            json!({"title":{"op":"remove"}}),
        )),
    );
    run(&fixture.store, &remove, &provider("p"));
    let recreate = envelope(
        &fixture.store,
        "recreate",
        payload(patch(&fixture.store, "p", 0, json!({}))),
    );
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &recreate,
            CatalogAuthority::new(base()).with_metadata(&provider("p")),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::OperationConflict
    );
    let revision: i64 = fixture
        .store
        .db
        .query_row(
            "SELECT revision FROM catalog_metadata_sources WHERE namespace='p'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(revision, 2);
}
#[test]
fn outer_generation_rejects_phantom_source_before_new_command_parsing() {
    let fixture = Fixture::new();
    fixture.initialize();
    let stale = envelope(
        &fixture.store,
        "stale-phantom",
        payload(patch(&fixture.store, "p", 0, json!({}))),
    );
    let Fixture { _root, path, store } = fixture;
    drop(store); // Native Store intentionally has one writer lease at a time.
    let other = Store::open(&path, true, false).unwrap();
    let phantom = envelope(
        &other,
        "phantom",
        payload(patch(&other, "other", 0, json!({}))),
    );
    run(&other, &phantom, &provider("other"));
    let before = snapshot(&other);
    assert_eq!(
        commands::execute_with_authority(&other, &stale, CatalogAuthority::new(base()), policy())
            .unwrap(),
        CommandOutcome::StaleGeneration
    );
    assert_eq!(snapshot(&other), before);
}
#[test]
fn projection_conflict_local_null_and_removal_commit_with_receipt_generation() {
    let fixture = Fixture::new();
    fixture.initialize();
    for (op, ns, title) in [("p1", "p1", "a"), ("p2", "p2", "b")] {
        let command = envelope(
            &fixture.store,
            op,
            payload(patch(
                &fixture.store,
                ns,
                0,
                json!({"title":{"op":"set","value":title}}),
            )),
        );
        run(&fixture.store, &command, &provider(ns));
    }
    let view = metadata::read_metadata(&fixture.store, &read_authority(&fixture.store)).unwrap();
    assert_eq!(view.metadata, json!({}));
    assert!(matches!(
        view.resolution.fields["title"],
        FieldResolution::Conflict { .. }
    ));
    let override_command = envelope(
        &fixture.store,
        "local-null",
        payload(patch(
            &fixture.store,
            "local",
            0,
            json!({"title":{"op":"set","value":null}}),
        )),
    );
    let outcome = run(&fixture.store, &override_command, &local("local"));
    assert_eq!(
        serde_json::from_str::<Value>(&raw_metadata(&fixture.store)).unwrap(),
        json!({"title":null})
    );
    let receipt_generation:i64=fixture.store.db.query_row("SELECT committed_generation FROM catalog_command_receipts WHERE operation_id='local-null'",[],|r|r.get(0)).unwrap();
    assert_eq!(receipt_generation, generation(&fixture.store));
    assert_eq!(
        body(&outcome)["resolution"]["fields"]["title"]["state"],
        "resolved"
    );
    let remove = envelope(
        &fixture.store,
        "local-remove",
        payload(patch(
            &fixture.store,
            "local",
            1,
            json!({"title":{"op":"remove"}}),
        )),
    );
    run(&fixture.store, &remove, &local("local"));
    let view = metadata::read_metadata(&fixture.store, &read_authority(&fixture.store)).unwrap();
    assert_eq!(view.metadata, json!({}));
    assert_eq!(view.resolution.inputs.len(), 4);
    assert!(
        view.resolution
            .inputs
            .iter()
            .any(|input| input.kind == SourceKind::Local && input.revision == 2)
    );
    let projection: String = fixture
        .store
        .db
        .query_row(
            "SELECT resolution_json FROM catalog_metadata_projection WHERE item_id=?1",
            [ITEM],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&projection).unwrap(),
        serde_json::to_value(&view.resolution).unwrap()
    );
    assert_no_permits(&fixture.store);
}
#[test]
fn managed_legacy_item_put_blocks_changes_but_preserves_unchanged_raw_bytes() {
    let fixture = Fixture::new();
    let raw = "{ \"title\" : \" Original Case \" }";
    fixture
        .store
        .db
        .execute(
            "UPDATE items SET metadata=?1 WHERE id=?2",
            params![raw, ITEM],
        )
        .unwrap();
    fixture.initialize();
    let before = snapshot(&fixture.store);
    let changed = envelope(
        &fixture.store,
        "legacy-changed",
        json!({"operation":"item_put","item":{"id":ITEM,"kind":"video","metadata":{"title":"overwrite"}}}),
    );
    assert_eq!(
        commands::execute(&fixture.store, &changed, base(), policy())
            .unwrap_err()
            .code,
        CommandStoreErrorCode::OperationConflict
    );
    assert_eq!(snapshot(&fixture.store), before);
    let unchanged = envelope(
        &fixture.store,
        "legacy-unchanged",
        json!({"operation":"item_put","item":{"id":ITEM,"kind":"video","metadata":{"title":" Original Case "}}}),
    );
    assert!(matches!(
        commands::execute(&fixture.store, &unchanged, base(), policy()).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    assert_eq!(raw_metadata(&fixture.store), raw);
    let history: i64 = fixture
        .store
        .db
        .query_row("SELECT count(*) FROM catalog_metadata_history", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(history, 1);
}
#[test]
fn receipt_insert_and_deferred_commit_failures_roll_back_complete_metadata_mutation() {
    let fixture = Fixture::new();
    fixture.initialize();
    let command = envelope(
        &fixture.store,
        "late-failure",
        payload(patch(
            &fixture.store,
            "p",
            0,
            json!({"title":{"op":"set","value":"rollback"}}),
        )),
    );
    let before = snapshot(&fixture.store);
    fixture.store.db.execute_batch("CREATE TEMP TRIGGER metadata_reject_receipt BEFORE INSERT ON catalog_command_receipts BEGIN SELECT RAISE(ABORT,'fixture failure'); END").unwrap();
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &command,
            CatalogAuthority::new(base()).with_metadata(&provider("p")),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::StoreFailure
    );
    assert_eq!(snapshot(&fixture.store), before);
    fixture.store.db.execute_batch("DROP TRIGGER metadata_reject_receipt; CREATE TEMP TRIGGER metadata_fail_commit AFTER INSERT ON catalog_command_receipts BEGIN INSERT INTO catalog_metadata_permits(item_id,incarnation) SELECT 'metadata-item',incarnation FROM catalog_state; END").unwrap();
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &command,
            CatalogAuthority::new(base()).with_metadata(&provider("p")),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::StoreFailure
    );
    assert_eq!(snapshot(&fixture.store), before);
    assert!(fixture.store.db.is_autocommit());
    assert_no_permits(&fixture.store);
    fixture
        .store
        .db
        .execute_batch("DROP TRIGGER metadata_fail_commit")
        .unwrap();
    assert!(matches!(
        run(&fixture.store, &command, &provider("p")),
        CommandOutcome::Committed { .. }
    ));
}
#[test]
fn public_read_requires_current_scope_stamps_and_rejects_ambient_transaction() {
    let fixture = Fixture::new();
    fixture.initialize();
    let before = snapshot(&fixture.store);
    let authority = read_authority(&fixture.store);
    let view = metadata::read_metadata(&fixture.store, &authority).unwrap();
    assert_eq!(view.subject, ITEM);
    assert_eq!(snapshot(&fixture.store), before);
    let mut stale = authority.clone();
    stale.expected_generation -= 1;
    assert_eq!(
        metadata::read_metadata(&fixture.store, &stale)
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::StaleGeneration
    );
    let mut wrong = authority.clone();
    wrong.incarnation = "different-incarnation".into();
    assert_eq!(
        metadata::read_metadata(&fixture.store, &wrong)
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::IncarnationMismatch
    );
    let mut wrong = authority.clone();
    wrong.catalog_id = "different-catalog".into();
    assert_eq!(
        metadata::read_metadata(&fixture.store, &wrong)
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::CatalogMismatch
    );
    let mut wrong = authority.clone();
    wrong.subject = "different-item".into();
    assert_eq!(
        metadata::read_metadata(&fixture.store, &wrong)
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::NotInitialized
    );
    fixture.store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(
        metadata::read_metadata(&fixture.store, &authority)
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::AmbientReadTransaction
    );
    assert!(!fixture.store.db.is_autocommit());
    fixture.store.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(snapshot(&fixture.store), before);
}
#[test]
fn public_read_fences_replaced_native_path_and_old_schema_admission() {
    let fixture = Fixture::new();
    fixture.initialize();
    let authority = read_authority(&fixture.store);
    let old_path = fixture.path.with_extension("moved");
    std::fs::rename(&fixture.path, &old_path).unwrap();
    std::fs::copy(&old_path, &fixture.path).unwrap();
    assert_eq!(
        metadata::read_metadata(&fixture.store, &authority)
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::StoreFailure
    );
    let old = Fixture::at_version(33);
    let authority = read_authority(&old.store);
    assert_eq!(
        metadata::read_metadata(&old.store, &authority)
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::UnsupportedContract
    );
    let command = envelope(&old.store, "metadata-on33", payload(init()));
    assert_eq!(
        commands::execute_with_authority(
            &old.store,
            &command,
            CatalogAuthority::new(base()).with_metadata(&grant(MetadataAccess::Initialize)),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::UnsupportedSchema
    );
}

#[test]
fn lost_metadata_reply_replays_after_reopen_without_write_authority() {
    let fixture = Fixture::new();
    let command = envelope(&fixture.store, "lost-reply", payload(init()));
    let first = run(&fixture.store, &command, &grant(MetadataAccess::Initialize));
    let next = envelope(
        &fixture.store,
        "newer-metadata",
        payload(patch(
            &fixture.store,
            "p",
            0,
            json!({"title":{"op":"set","value":"newer"}}),
        )),
    );
    run(&fixture.store, &next, &provider("p"));
    let before = snapshot(&fixture.store);
    let Fixture { _root, path, store } = fixture;
    drop(store);
    let reopened = Store::open(&path, true, false).unwrap();
    let read_only = TrustedCommandAuthority {
        allow_new_commands: false,
        ..base()
    };
    let replay = commands::execute_with_authority(
        &reopened,
        &command,
        CatalogAuthority::new(read_only),
        policy(),
    )
    .unwrap();
    assert_eq!(body(&replay), body(&first));
    assert!(matches!(replay, CommandOutcome::Replay { .. }));
    assert_eq!(snapshot(&reopened), before);
    assert_no_permits(&reopened);
}
#[test]
fn source_kind_schema_and_single_local_ownership_are_typed_conflicts() {
    let fixture = Fixture::new();
    fixture.initialize();
    let first = envelope(
        &fixture.store,
        "first-provider",
        payload(patch(&fixture.store, "p", 0, json!({}))),
    );
    run(&fixture.store, &first, &provider("p"));
    let first_local = envelope(
        &fixture.store,
        "first-local",
        payload(patch(&fixture.store, "local", 0, json!({}))),
    );
    run(&fixture.store, &first_local, &local("local"));
    let mut wrong_schema = patch(&fixture.store, "p", 1, json!({}));
    if let MetadataChange::Patch {
        schema_fingerprint, ..
    } = &mut wrong_schema.change
    {
        *schema_fingerprint = "f".repeat(64);
    }
    let before = snapshot(&fixture.store);
    for (name, mutation, authority) in [
        (
            "wrong-kind",
            patch(&fixture.store, "p", 1, json!({})),
            local("p"),
        ),
        ("wrong-schema", wrong_schema, provider("p")),
        (
            "second-local",
            patch(&fixture.store, "local2", 0, json!({})),
            local("local2"),
        ),
    ] {
        let command = envelope(&fixture.store, name, payload(mutation));
        assert_eq!(
            commands::execute_with_authority(
                &fixture.store,
                &command,
                CatalogAuthority::new(base()).with_metadata(&authority),
                policy()
            )
            .unwrap_err()
            .code,
            CommandStoreErrorCode::OperationConflict
        );
        assert_eq!(snapshot(&fixture.store), before);
    }
}

#[test]
fn retained_receipt_from_obsolete_metadata_payload_replays_without_current_parser() {
    let fixture = Fixture::new();
    let initialized = fixture.initialize();
    // Model a valid retained historical receipt whose operation-specific shape
    // is no longer recognized by this implementation. The durable envelope and
    // result contracts remain supported. This is fixture data, not an alternate
    // runtime dispatcher, and the new malformed-key case below cannot execute.
    let historic = envelope(
        &fixture.store,
        "historical-shape",
        json!({"operation":"metadata","mutation":{"contract_version":0,"change":{"action":"obsolete_metadata_v0"}}}),
    );
    fixture.store.db.execute("INSERT INTO catalog_command_receipts(catalog_id,host_namespace,operation_id,contract_version,canonicalization_version,payload_digest,admission_incarnation,expected_generation,committed_generation,committed_at_us,retain_until_us,result_state,result_json) SELECT catalog_id,host_namespace,?1,contract_version,?2,?3,admission_incarnation,?4,committed_generation,committed_at_us,retain_until_us,result_state,result_json FROM catalog_command_receipts WHERE operation_id=?5",params![historic.key().operation_id(),historic.digest().canonicalization_version(),historic.digest().as_str(),historic.expected_generation(),initialized.key().operation_id()]).unwrap();
    let expected = commands::execute(&fixture.store, &initialized, base(), policy()).unwrap();
    let before = snapshot(&fixture.store);
    let current_receipt_only = TrustedCommandAuthority {
        allow_new_commands: false,
        ..base()
    };
    let exhausted = ReceiptPolicy {
        retention: Duration::MAX,
        max_receipts: 0,
        max_logical_bytes: 0,
        ..policy()
    };
    let replay = commands::execute_with_authority(
        &fixture.store,
        &historic,
        CatalogAuthority::new(current_receipt_only),
        exhausted,
    )
    .unwrap();
    assert!(matches!(replay, CommandOutcome::Replay { .. }));
    assert_eq!(body(&replay), body(&expected));
    assert_eq!(snapshot(&fixture.store), before);
    let unknown = envelope(
        &fixture.store,
        "unknown-old-shape",
        historic.payload().clone(),
    );
    assert_eq!(
        commands::execute_with_authority(
            &fixture.store,
            &unknown,
            CatalogAuthority::new(base()),
            policy()
        )
        .unwrap_err()
        .code,
        CommandStoreErrorCode::InvalidOperation
    );
}
