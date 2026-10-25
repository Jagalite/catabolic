use super::*;
use catabolic_core::catalog_command::CommandKey;
use std::path::Path;

const CATALOG: &str = "catalog";
const INCARNATION: &str = "0123456789abcdef0123456789abcdef";
const SUBJECT: &str = "item-1";
const HOST: &str = "test-host";

// Deliberately independent fixture, not schema033 and not a production migration
// runner: only the established items/catalog-state/barrier interfaces are modeled.
fn fixture() -> Connection {
    fixture_at(None)
}
fn fixture_at(path: Option<&Path>) -> Connection {
    fixture_with_raw(path, "{}")
}
fn fixture_with_raw(path: Option<&Path>, raw: &str) -> Connection {
    let db = path.map_or_else(
        || Connection::open_in_memory().unwrap(),
        |p| Connection::open(p).unwrap(),
    );
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON;
        CREATE TABLE items(id TEXT PRIMARY KEY,kind TEXT NOT NULL,metadata TEXT NOT NULL);
        CREATE TABLE catalog_state(id INTEGER PRIMARY KEY CHECK(id=1),catalog_id TEXT NOT NULL,incarnation TEXT NOT NULL,generation INTEGER NOT NULL CHECK(typeof(generation)='integer' AND generation>=0));
        CREATE TABLE catalog_commit_barrier(id INTEGER PRIMARY KEY CHECK(id=1));
        CREATE TABLE catalog_entity_revisions(entity_kind TEXT NOT NULL,entity_key TEXT NOT NULL,revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=1),deleted INTEGER NOT NULL CHECK(deleted IN (0,1)),PRIMARY KEY(entity_kind,entity_key));
        CREATE TRIGGER fixture_barrier_insert BEFORE INSERT ON catalog_commit_barrier BEGIN SELECT RAISE(ABORT,'permanently empty'); END;
        CREATE TRIGGER fixture_item_generation AFTER UPDATE ON items BEGIN UPDATE catalog_state SET generation=generation+1 WHERE id=1; END;
        CREATE TABLE fixture_receipts(operation_id TEXT PRIMARY KEY,result_json TEXT NOT NULL);") .unwrap();
    db.execute(
        "INSERT INTO catalog_state VALUES(1,?1,?2,0)",
        params![CATALOG, INCARNATION],
    )
    .unwrap();
    db.execute(
        "INSERT INTO items VALUES(?1,'movie',?2)",
        params![SUBJECT, raw],
    )
    .unwrap();
    db.execute_batch(include_str!("../resources/native-catalog/034_metadata.sql"))
        .unwrap();
    db
}
fn generation(db: &Connection) -> i64 {
    db.query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
        r.get(0)
    })
    .unwrap()
}
fn count(db: &Connection, table: &str) -> i64 {
    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn current(db: &Connection) -> String {
    db.query_row("SELECT metadata FROM items WHERE id=?1", [SUBJECT], |r| {
        r.get(0)
    })
    .unwrap()
}
fn effective(db: &Connection) -> Value {
    decode_json(&current(db)).unwrap()
}
fn policy(
    field: &str,
    value_type: PolicyValueType,
    nullable: bool,
    normalization: PolicyNormalization,
) -> PolicyField {
    PolicyField {
        field: field.into(),
        value_type,
        nullable,
        normalization,
    }
}
fn policies() -> Vec<PolicyField> {
    vec![
        policy(
            "title",
            PolicyValueType::String,
            true,
            PolicyNormalization::TrimAsciiLowercase,
        ),
        policy(
            "extra",
            PolicyValueType::Json,
            true,
            PolicyNormalization::Exact,
        ),
        policy(
            "year",
            PolicyValueType::Integer,
            false,
            PolicyNormalization::Exact,
        ),
    ]
}
fn fingerprint() -> String {
    field_schema(&policies()).unwrap().fingerprint().into()
}
fn grant(access: MetadataAccess) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: HOST.into(),
        subject: SUBJECT.into(),
        access,
    }
}
fn source(namespace: &str, record_id: &str, kind: SourceKind) -> MetadataAuthority {
    grant(MetadataAccess::Source {
        namespace: namespace.into(),
        record_id: record_id.into(),
        kind,
    })
}
fn init_mutation() -> MetadataMutation {
    MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Initialize {
            subject: SUBJECT.into(),
            policies: policies(),
        },
    }
}
fn patch(namespace: &str, record_id: &str, expected: i64, edits: &str) -> MetadataMutation {
    MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Patch {
            subject: SUBJECT.into(),
            namespace: namespace.into(),
            record_id: record_id.into(),
            schema_fingerprint: fingerprint(),
            expected_revision: expected,
            edits_json: edits.into(),
            evidence_json: "{}".into(),
        },
    }
}
fn reconcile(expected: i64, fields: &[&str]) -> MetadataMutation {
    MetadataMutation {
        contract_version: 1,
        change: MetadataChange::ReconcileLegacy {
            subject: SUBJECT.into(),
            schema_fingerprint: fingerprint(),
            expected_revision: expected,
            fields: fields.iter().map(|s| s.to_string()).collect(),
        },
    }
}
fn envelope(db: &Connection, mutation: &MetadataMutation, op: &str) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(CATALOG, HOST, op).unwrap(),
        INCARNATION,
        generation(db),
        &serde_json::to_string(&json!({"operation":"metadata","mutation":mutation})).unwrap(),
    )
    .unwrap()
}
fn execute_at(
    db: &Connection,
    mutation: &MetadataMutation,
    authority: &MetadataAuthority,
    command: &CommandEnvelope,
) -> Result<Value> {
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let result = apply_metadata(db, mutation, command, authority, 100).and_then(|v| {
        validate_metadata_permits_empty(db)?;
        Ok(v)
    });
    match result {
        Ok(v) => {
            db.execute_batch("COMMIT").unwrap();
            Ok(v)
        }
        Err(e) => {
            db.execute_batch("ROLLBACK").unwrap();
            Err(e)
        }
    }
}
fn execute(
    db: &Connection,
    mutation: &MetadataMutation,
    authority: &MetadataAuthority,
) -> Result<Value> {
    let command = envelope(db, mutation, &format!("command-{}", generation(db)));
    execute_at(db, mutation, authority, &command)
}
fn initialize(db: &Connection) {
    execute(db, &init_mutation(), &grant(MetadataAccess::Initialize)).unwrap();
}
fn apply_patch(
    db: &Connection,
    namespace: &str,
    kind: SourceKind,
    revision: i64,
    edits: &str,
) -> Value {
    execute(
        db,
        &patch(namespace, "record", revision, edits),
        &source(namespace, "record", kind),
    )
    .unwrap()
}
fn head(db: &Connection, namespace: &str) -> (i64, String, String) {
    db.query_row("SELECT revision,source_kind,fields_json FROM catalog_metadata_sources WHERE item_id=?1 AND namespace=?2",params![SUBJECT,namespace],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap()
}
fn unchanged_on_error(
    db: &Connection,
    mutation: &MetadataMutation,
    authority: &MetadataAuthority,
    expected: MetadataStoreErrorCode,
) {
    let before = (
        current(db),
        generation(db),
        count(db, "catalog_metadata_history"),
        count(db, "catalog_metadata_sources"),
    );
    assert_eq!(execute(db, mutation, authority).unwrap_err().code, expected);
    assert_eq!(
        before,
        (
            current(db),
            generation(db),
            count(db, "catalog_metadata_history"),
            count(db, "catalog_metadata_sources")
        )
    );
    validate_metadata_permits_empty(db).unwrap();
}

#[test]
fn migration_is_additive_and_lazy() {
    let raw = "{ \"unrecognized\": [1, null, {\"untouched\":true}] }";
    let db = fixture_with_raw(None, raw);
    assert_eq!(current(&db), raw);
    for table in [
        "catalog_metadata_subjects",
        "catalog_metadata_sources",
        "catalog_metadata_history",
        "catalog_metadata_projection",
        "catalog_metadata_permits",
    ] {
        assert_eq!(count(&db, table), 0);
    }
    assert_eq!(generation(&db), 0);
}
#[test]
fn initialization_retains_exact_legacy_bytes_and_provenance() {
    let db = fixture();
    let raw = "{ \"year\": 123456789012345678901234567890, \"title\": \"  Original NAME  \", \"extra\": {\"z\":[null,2,1]} }";
    db.execute("UPDATE items SET metadata=?1", [raw]).unwrap();
    initialize(&db);
    assert_eq!(current(&db), raw);
    let saved: String = db
        .query_row(
            "SELECT legacy_raw_json FROM catalog_metadata_subjects",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(saved, raw);
    assert_eq!(head(&db, LEGACY_NAMESPACE).1, "legacy_import");
    let result = read_documents(&db, &field_schema(&policies()).unwrap(), SUBJECT).unwrap();
    assert_eq!(result[0].fields()["title"], "  Original NAME  ");
    assert_eq!(count(&db, "catalog_metadata_history"), 1);
    assert_eq!(count(&db, "catalog_metadata_permits"), 0);
}
#[test]
fn initialization_requires_policy_coverage_and_exact_valid_shapes() {
    let db = fixture();
    db.execute("UPDATE items SET metadata=?1", [r#"{"unknown":5}"#])
        .unwrap();
    unchanged_on_error(
        &db,
        &init_mutation(),
        &grant(MetadataAccess::Initialize),
        MetadataStoreErrorCode::Core(MetadataError::UnknownField),
    );
    db.execute(
        "UPDATE items SET metadata=?1",
        [r#"{"year":"not an integer"}"#],
    )
    .unwrap();
    unchanged_on_error(
        &db,
        &init_mutation(),
        &grant(MetadataAccess::Initialize),
        MetadataStoreErrorCode::Core(MetadataError::InvalidFieldValue),
    );
    assert_eq!(count(&db, "catalog_metadata_subjects"), 0);
}
#[test]
fn initialization_rejects_ambiguous_duplicate_legacy_json() {
    let db = fixture();
    db.execute(
        "UPDATE items SET metadata=?1",
        [r#"{"title":"a","title":"b"}"#],
    )
    .unwrap();
    unchanged_on_error(
        &db,
        &init_mutation(),
        &grant(MetadataAccess::Initialize),
        MetadataStoreErrorCode::Core(MetadataError::InvalidJson),
    );
}
#[test]
fn explicit_schema_is_immutable() {
    let db = fixture();
    initialize(&db);
    unchanged_on_error(
        &db,
        &init_mutation(),
        &grant(MetadataAccess::Initialize),
        MetadataStoreErrorCode::AlreadyInitialized,
    );
    assert!(
        db.execute(
            "UPDATE catalog_metadata_subjects SET policies_json='[]'",
            []
        )
        .is_err()
    );
    let mut mutation = patch("p", "record", 0, "{}");
    if let MetadataChange::Patch {
        schema_fingerprint, ..
    } = &mut mutation.change
    {
        *schema_fingerprint = "f".repeat(64);
    }
    unchanged_on_error(
        &db,
        &mutation,
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::Core(MetadataError::SchemaMismatch),
    );
}
#[test]
fn provider_agreement_attributes_every_source() {
    let db = fixture();
    initialize(&db);
    apply_patch(
        &db,
        "p1",
        SourceKind::Provider,
        0,
        r#"{"title":{"op":"set","value":" TITLE "}}"#,
    );
    let result = apply_patch(
        &db,
        "p2",
        SourceKind::Provider,
        0,
        r#"{"title":{"op":"set","value":"Title"}}"#,
    );
    assert_eq!(effective(&db), json!({"title":"title"}));
    let resolved = &result["resolution"]["fields"]["title"];
    assert_eq!(resolved["state"], "resolved");
    assert_eq!(resolved["sources"].as_array().unwrap().len(), 2);
    assert_eq!(result["resolution"]["inputs"].as_array().unwrap().len(), 3);
}
#[test]
fn provider_conflict_is_explicit_and_not_materialized_or_hidden_by_scan() {
    let db = fixture();
    initialize(&db);
    apply_patch(
        &db,
        "scan",
        SourceKind::Scan,
        0,
        r#"{"title":{"op":"set","value":"fallback"}}"#,
    );
    apply_patch(
        &db,
        "p1",
        SourceKind::Provider,
        0,
        r#"{"title":{"op":"set","value":"a"}}"#,
    );
    let result = apply_patch(
        &db,
        "p2",
        SourceKind::Provider,
        0,
        r#"{"title":{"op":"set","value":"b"}}"#,
    );
    assert_eq!(effective(&db), json!({}));
    assert_eq!(result["resolution"]["fields"]["title"]["state"], "conflict");
    assert_eq!(
        result["resolution"]["fields"]["title"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn scan_is_fallback_and_scan_conflict_stays_unresolved() {
    let db = fixture();
    initialize(&db);
    apply_patch(
        &db,
        "scan1",
        SourceKind::Scan,
        0,
        r#"{"title":{"op":"set","value":"one"}}"#,
    );
    assert_eq!(effective(&db)["title"], "one");
    apply_patch(
        &db,
        "scan2",
        SourceKind::Scan,
        0,
        r#"{"title":{"op":"set","value":"two"}}"#,
    );
    assert!(effective(&db).get("title").is_none());
    apply_patch(
        &db,
        "p",
        SourceKind::Provider,
        0,
        r#"{"title":{"op":"set","value":"provider"}}"#,
    );
    assert_eq!(effective(&db)["title"], "provider");
}
#[test]
fn local_override_omission_null_and_remove_are_distinct() {
    let db = fixture();
    initialize(&db);
    apply_patch(
        &db,
        "p",
        SourceKind::Provider,
        0,
        r#"{"title":{"op":"set","value":"provider"}}"#,
    );
    apply_patch(
        &db,
        "local",
        SourceKind::Local,
        0,
        r#"{"title":{"op":"set","value":"local"}}"#,
    );
    assert_eq!(effective(&db)["title"], "local");
    apply_patch(&db, "local", SourceKind::Local, 1, "{}");
    assert_eq!(effective(&db)["title"], "local");
    apply_patch(
        &db,
        "local",
        SourceKind::Local,
        2,
        r#"{"title":{"op":"set","value":null}}"#,
    );
    assert_eq!(effective(&db), json!({"title":null}));
    apply_patch(
        &db,
        "local",
        SourceKind::Local,
        3,
        r#"{"title":{"op":"remove"}}"#,
    );
    assert_eq!(effective(&db)["title"], "provider");
    assert_eq!(head(&db, "local"), (4, "local".into(), "{}".into()));
}
#[test]
fn local_removal_reveals_unreconciled_legacy_without_relabeling() {
    let db = fixture();
    db.execute(
        "UPDATE items SET metadata=?1",
        [r#"{"title":" Legacy ","extra":{"retained":true}}"#],
    )
    .unwrap();
    initialize(&db);
    apply_patch(
        &db,
        "p",
        SourceKind::Provider,
        0,
        r#"{"title":{"op":"set","value":"provider"}}"#,
    );
    apply_patch(
        &db,
        "local",
        SourceKind::Local,
        0,
        r#"{"title":{"op":"set","value":"local"}}"#,
    );
    apply_patch(
        &db,
        "local",
        SourceKind::Local,
        1,
        r#"{"title":{"op":"remove"}}"#,
    );
    assert_eq!(effective(&db)["title"], " Legacy ");
    assert_eq!(head(&db, LEGACY_NAMESPACE).0, 1);
    execute(
        &db,
        &reconcile(1, &["title"]),
        &grant(MetadataAccess::ReconcileLegacy),
    )
    .unwrap();
    assert_eq!(
        effective(&db),
        json!({"title":"provider","extra":{"retained":true}})
    );
    assert_eq!(head(&db, LEGACY_NAMESPACE).1, "legacy_import");
    assert_eq!(head(&db, LEGACY_NAMESPACE).0, 2);
    execute(
        &db,
        &reconcile(2, &["extra"]),
        &grant(MetadataAccess::ReconcileLegacy),
    )
    .unwrap();
    assert_eq!(
        head(&db, LEGACY_NAMESPACE),
        (3, "legacy_import".into(), "{}".into())
    );
}
#[test]
fn stale_source_refresh_and_empty_recreation_are_rejected() {
    let db = fixture();
    initialize(&db);
    apply_patch(
        &db,
        "p",
        SourceKind::Provider,
        0,
        r#"{"title":{"op":"set","value":"new"}}"#,
    );
    unchanged_on_error(
        &db,
        &patch("p", "record", 0, "{}"),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::Core(MetadataError::StaleSourceRevision {
            expected: 0,
            actual: 1,
        }),
    );
    apply_patch(
        &db,
        "p",
        SourceKind::Provider,
        1,
        r#"{"title":{"op":"remove"}}"#,
    );
    unchanged_on_error(
        &db,
        &patch("p", "record", 1, "{}"),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::Core(MetadataError::StaleSourceRevision {
            expected: 1,
            actual: 2,
        }),
    );
    unchanged_on_error(
        &db,
        &patch("p", "record", 0, "{}"),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::Core(MetadataError::StaleSourceRevision {
            expected: 0,
            actual: 2,
        }),
    );
}
#[test]
fn phantom_source_membership_rejected_across_connections() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("catalog.db");
    let db = fixture_at(Some(&path));
    initialize(&db);
    let mutation = patch("p1", "record", 0, "{}");
    let stale = envelope(&db, &mutation, "before-phantom");
    let other = Connection::open(&path).unwrap();
    other
        .execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON")
        .unwrap();
    apply_patch(&other, "phantom", SourceKind::Provider, 0, "{}");
    assert_eq!(
        execute_at(
            &db,
            &mutation,
            &source("p1", "record", SourceKind::Provider),
            &stale
        )
        .unwrap_err()
        .code,
        MetadataStoreErrorCode::StaleGeneration
    );
    assert_eq!(count(&db, "catalog_metadata_sources"), 2);
}
#[test]
fn authority_binds_subject_namespace_record_and_host() {
    let db = fixture();
    initialize(&db);
    let mutation = patch("p", "record", 0, "{}");
    let mut wrong_subject = source("p", "record", SourceKind::Provider);
    wrong_subject.subject = "another-item".into();
    for authority in [
        source("other", "record", SourceKind::Provider),
        source("p", "other", SourceKind::Provider),
        wrong_subject,
    ] {
        unchanged_on_error(
            &db,
            &mutation,
            &authority,
            MetadataStoreErrorCode::AuthorityDenied,
        );
    }
    let mut authority = source("p", "record", SourceKind::Provider);
    authority.host_namespace = "other-host".into();
    unchanged_on_error(
        &db,
        &mutation,
        &authority,
        MetadataStoreErrorCode::AuthorityDenied,
    );
}
#[test]
fn request_cannot_claim_kind_or_editorial_privilege() {
    let db = fixture();
    unchanged_on_error(
        &db,
        &init_mutation(),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::AuthorityDenied,
    );
    initialize(&db);
    unchanged_on_error(
        &db,
        &reconcile(1, &["title"]),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::AuthorityDenied,
    );
    let mut raw = serde_json::to_value(patch("p", "record", 0, "{}")).unwrap();
    raw["change"]["kind"] = json!("local");
    assert!(serde_json::from_value::<MetadataMutation>(raw).is_err());
    apply_patch(&db, "p", SourceKind::Provider, 0, "{}");
    unchanged_on_error(
        &db,
        &patch("p", "record", 1, "{}"),
        &source("p", "record", SourceKind::Local),
        MetadataStoreErrorCode::Core(MetadataError::SourceKindMismatch),
    );
    unchanged_on_error(
        &db,
        &patch(LEGACY_NAMESPACE, LEGACY_RECORD_ID, 1, "{}"),
        &source(LEGACY_NAMESPACE, LEGACY_RECORD_ID, SourceKind::Provider),
        MetadataStoreErrorCode::AuthorityDenied,
    );
    unchanged_on_error(
        &db,
        &patch("p", "record", 1, "{}"),
        &source("p", "record", SourceKind::LegacyImport),
        MetadataStoreErrorCode::AuthorityDenied,
    );
}
#[test]
fn one_local_document_including_empty_head() {
    let db = fixture();
    initialize(&db);
    apply_patch(&db, "local1", SourceKind::Local, 0, "{}");
    unchanged_on_error(
        &db,
        &patch("local2", "record", 0, "{}"),
        &source("local2", "record", SourceKind::Local),
        MetadataStoreErrorCode::Core(MetadataError::MultipleLocalSources),
    );
}
#[test]
fn raw_inputs_and_all_normalized_revisions_survive() {
    let db = fixture();
    initialize(&db);
    let raw = "{ \"title\" : {\"op\":\"set\",\"value\":\"  MiXeD  \"} }";
    let evidence = "{ \"observed\": [1, null, 3] }";
    let mut mutation = patch("p", "record", 0, raw);
    if let MetadataChange::Patch { evidence_json, .. } = &mut mutation.change {
        *evidence_json = evidence.into();
    }
    execute(&db, &mutation, &source("p", "record", SourceKind::Provider)).unwrap();
    apply_patch(
        &db,
        "p",
        SourceKind::Provider,
        1,
        r#"{"title":{"op":"remove"}}"#,
    );
    let stored:String=db.query_row("SELECT raw_mutation_json FROM catalog_metadata_history WHERE namespace='p' AND revision=1",[],|r|r.get(0)).unwrap();
    let stored: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["change"]["edits_json"], raw);
    assert_eq!(stored["change"]["evidence_json"], evidence);
    let fields: String = db
        .query_row(
            "SELECT fields_json FROM catalog_metadata_history WHERE namespace='p' AND revision=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(decode_json(&fields).unwrap(), json!({"title":"mixed"}));
    assert_eq!(count(&db, "catalog_metadata_history"), 3);
}
#[test]
fn old_item_put_is_typed_fail_closed_and_unchanged_is_lossless() {
    let db = fixture();
    let raw = "{ \"title\" : \"ORIGINAL\" }";
    db.execute("UPDATE items SET metadata=?1", [raw]).unwrap();
    initialize(&db);
    assert_eq!(
        legacy_item_put_metadata(&db, SUBJECT, &json!({"title":"ORIGINAL"})).unwrap(),
        raw
    );
    assert_eq!(
        legacy_item_put_metadata(&db, SUBJECT, &json!({"title":"changed"}))
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::ManagedWriteBlocked
    );
    assert!(
        db.execute("UPDATE items SET metadata='{}' WHERE id=?1", [SUBJECT])
            .is_err()
    );
    assert_eq!(current(&db), raw);
    assert!(db.execute("INSERT INTO items VALUES(?1,'movie','{}') ON CONFLICT(id) DO UPDATE SET metadata=excluded.metadata",[SUBJECT]).is_err());
}
#[test]
fn unmanaged_items_and_legacy_schemas_keep_old_write_behavior() {
    let db = fixture();
    assert_eq!(
        legacy_item_put_metadata(&db, SUBJECT, &json!({"free":1})).unwrap(),
        encode(&json!({"free":1}))
    );
    db.execute("UPDATE items SET metadata='{}'", []).unwrap();
    let legacy = Connection::open_in_memory().unwrap();
    legacy
        .execute_batch("CREATE TABLE items(id TEXT PRIMARY KEY,kind TEXT,metadata TEXT)")
        .unwrap();
    assert_eq!(
        legacy_item_put_metadata(&legacy, "new", &json!({"unknown":[1,2,3]})).unwrap(),
        encode(&json!({"unknown":[1,2,3]}))
    );
}
#[test]
fn late_projection_failure_rolls_back_history_head_generation_and_permit() {
    let db = fixture();
    initialize(&db);
    let before = generation(&db);
    db.execute_batch("CREATE TRIGGER fixture_late_projection BEFORE UPDATE ON catalog_metadata_projection BEGIN SELECT RAISE(ABORT,'injected projection failure'); END").unwrap();
    unchanged_on_error(
        &db,
        &patch(
            "p",
            "record",
            0,
            r#"{"title":{"op":"set","value":"not committed"}}"#,
        ),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::StoreFailure,
    );
    assert_eq!(generation(&db), before);
    assert_eq!(count(&db, "catalog_metadata_history"), 1);
}
#[test]
fn failure_after_owner_success_rolls_back_all_data_on_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("catalog.db");
    let db = fixture_at(Some(&path));
    initialize(&db);
    let before = generation(&db);
    let mutation = patch("p", "record", 0, r#"{"title":{"op":"set","value":"lost"}}"#);
    let command = envelope(&db, &mutation, "late-failure");
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    apply_metadata(
        &db,
        &mutation,
        &command,
        &source("p", "record", SourceKind::Provider),
        100,
    )
    .unwrap();
    validate_metadata_permits_empty(&db).unwrap();
    assert!(
        db.execute(
            "INSERT INTO fixture_receipts VALUES('late-failure',NULL)",
            []
        )
        .is_err()
    );
    db.execute_batch("ROLLBACK").unwrap();
    drop(db);
    let reopened = Connection::open(path).unwrap();
    assert_eq!(effective(&reopened), json!({}));
    assert_eq!(generation(&reopened), before);
    assert_eq!(count(&reopened, "catalog_metadata_history"), 1);
    assert_eq!(count(&reopened, "fixture_receipts"), 0);
    validate_metadata_permits_empty(&reopened).unwrap();
}
#[test]
fn deferred_barrier_prevents_leaked_permit_commit() {
    let db = fixture();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    db.execute(
        "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES(?1,?2)",
        params![SUBJECT, INCARNATION],
    )
    .unwrap();
    assert_eq!(
        validate_metadata_permits_empty(&db).unwrap_err().code,
        MetadataStoreErrorCode::CorruptState
    );
    assert!(db.execute_batch("COMMIT").is_err());
    assert!(!db.is_autocommit());
    db.execute_batch("ROLLBACK").unwrap();
    validate_metadata_permits_empty(&db).unwrap();
    assert!(
        db.execute("INSERT INTO catalog_commit_barrier VALUES(1)", [])
            .is_err()
    );
}
#[test]
fn permits_are_incarnation_bound_immutable_and_subject_scoped() {
    let db = fixture();
    initialize(&db);
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(
        db.execute(
            "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES(?1,'wrong')",
            [SUBJECT]
        )
        .is_err()
    );
    db.execute("INSERT INTO items VALUES('another','movie','{}')", [])
        .unwrap();
    db.execute(
        "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES('another',?1)",
        [INCARNATION],
    )
    .unwrap();
    assert!(
        db.execute("UPDATE catalog_metadata_permits SET item_id=?1", [SUBJECT])
            .is_err()
    );
    assert!(
        db.execute(
            "UPDATE items SET metadata=?1 WHERE id=?2",
            params!["{\"title\":\"changed\"}", SUBJECT]
        )
        .is_err()
    );
    db.execute_batch("ROLLBACK").unwrap();
}
#[test]
fn history_heads_subjects_and_item_identity_cannot_be_deleted_or_rewritten() {
    let db = fixture();
    initialize(&db);
    apply_patch(&db, "p", SourceKind::Provider, 0, "{}");
    for sql in [
        "DELETE FROM catalog_metadata_history",
        "UPDATE catalog_metadata_history SET fields_json='{}'",
        "DELETE FROM catalog_metadata_sources",
        "DELETE FROM catalog_metadata_subjects",
        "DELETE FROM catalog_metadata_projection",
        "DELETE FROM items",
        "UPDATE items SET id='changed'",
        "UPDATE catalog_metadata_sources SET source_kind='local'",
        "UPDATE catalog_metadata_projection SET effective_json='{}'",
    ] {
        assert!(db.execute(sql, []).is_err(), "{sql}");
    }
    assert_eq!(count(&db, "catalog_metadata_history"), 2);
}
#[test]
fn generations_cover_empty_new_sources_history_and_projection_rebuilds() {
    let db = fixture();
    let before = generation(&db);
    initialize(&db);
    assert_eq!(generation(&db), before + 4);
    let before = generation(&db);
    apply_patch(&db, "p", SourceKind::Provider, 0, "{}");
    assert_eq!(generation(&db), before + 3);
    let before = generation(&db);
    apply_patch(&db, "p", SourceKind::Provider, 1, "{}");
    assert_eq!(generation(&db), before + 3);
}
#[test]
fn all_source_document_limits_fail_before_mutation() {
    let db = fixture();
    initialize(&db);
    for n in 0..127 {
        apply_patch(&db, &format!("p{n:03}"), SourceKind::Provider, 0, "{}");
    }
    assert_eq!(count(&db, "catalog_metadata_sources"), 128);
    unchanged_on_error(
        &db,
        &patch("over", "record", 0, "{}"),
        &source("over", "record", SourceKind::Provider),
        MetadataStoreErrorCode::Core(MetadataError::SourceBudget),
    );
    // Refresh is still allowed at the source-count boundary.
    apply_patch(&db, "p000", SourceKind::Provider, 1, "{}");
}
#[test]
fn invalid_patch_shapes_duplicate_fields_and_null_rules_are_typed() {
    let db = fixture();
    initialize(&db);
    for (raw, error) in [
        (
            r#"{"title":{"op":"set","value":"a"},"title":{"op":"remove"}}"#,
            MetadataError::InvalidJson,
        ),
        (
            r#"{"title":{"op":"remove","value":null}}"#,
            MetadataError::InvalidOperation,
        ),
        (
            r#"{"unknown":{"op":"set","value":3}}"#,
            MetadataError::UnknownField,
        ),
        (
            r#"{"year":{"op":"set","value":null}}"#,
            MetadataError::NullNotAllowed,
        ),
    ] {
        unchanged_on_error(
            &db,
            &patch("p", "record", 0, raw),
            &source("p", "record", SourceKind::Provider),
            MetadataStoreErrorCode::Core(error),
        );
    }
}
#[test]
fn evidence_document_and_depth_limits_are_typed() {
    let db = fixture();
    initialize(&db);
    let mut m = patch("p", "record", 0, "{}");
    if let MetadataChange::Patch { evidence_json, .. } = &mut m.change {
        *evidence_json = format!("{{\"s\":\"{}\"}}", "x".repeat(8192));
    }
    unchanged_on_error(
        &db,
        &m,
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::Core(MetadataError::EvidenceBudget),
    );
    let raw = format!(
        "{{\"title\":{{\"op\":\"set\",\"value\":\"{}\"}}}}",
        "x".repeat(65536)
    );
    unchanged_on_error(
        &db,
        &patch("p", "record", 0, &raw),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::Core(MetadataError::DocumentBudget),
    );
    let raw = format!(
        "{{\"extra\":{{\"op\":\"set\",\"value\":{}0{}}}}}",
        "[".repeat(33),
        "]".repeat(33)
    );
    unchanged_on_error(
        &db,
        &patch("p", "record", 0, &raw),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::Core(MetadataError::JsonDepthBudget),
    );
}
#[test]
fn source_revision_exhaustion_is_typed_before_write() {
    let db = fixture();
    initialize(&db);
    apply_patch(&db, "p", SourceKind::Provider, 0, "{}");
    // Construct an exhausted persisted fixture, without iterating i64::MAX
    // commands. Disable only this fixture's head checks; production SQL stays.
    db.execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER catalog_metadata_source_update; UPDATE catalog_metadata_sources SET revision=9223372036854775807 WHERE namespace='p'; PRAGMA foreign_keys=ON").unwrap();
    unchanged_on_error(
        &db,
        &patch("p", "record", i64::MAX, "{}"),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::Core(MetadataError::RevisionExhausted),
    );
}
#[test]
fn generation_exhaustion_rolls_back_cleanly() {
    let db = fixture();
    initialize(&db);
    db.execute("UPDATE catalog_state SET generation=?1", [i64::MAX])
        .unwrap();
    unchanged_on_error(
        &db,
        &patch("p", "record", 0, "{}"),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::StoreFailure,
    );
}
#[test]
fn owner_requires_outer_transaction_and_initialized_item() {
    let db = fixture();
    let m = init_mutation();
    let c = envelope(&db, &m, "tx");
    assert_eq!(
        apply_metadata(&db, &m, &c, &grant(MetadataAccess::Initialize), 100)
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::TransactionRequired
    );
    unchanged_on_error(
        &db,
        &patch("p", "record", 0, "{}"),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::NotInitialized,
    );
    db.execute("DELETE FROM items", []).unwrap();
    assert_eq!(
        execute(&db, &m, &grant(MetadataAccess::Initialize))
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::SubjectMissing
    );
}
#[test]
fn rehydration_rebuild_matches_projection_and_complete_input_set() {
    let db = fixture();
    initialize(&db);
    apply_patch(
        &db,
        "scan",
        SourceKind::Scan,
        0,
        r#"{"title":{"op":"set","value":"scan"}}"#,
    );
    apply_patch(&db, "p", SourceKind::Provider, 0, "{}");
    let schema = read_schema(&db, SUBJECT).unwrap();
    let documents = read_documents(&db, &schema, SUBJECT).unwrap();
    let resolved = resolve(&schema, SUBJECT, &documents).unwrap();
    let raw: String = db
        .query_row(
            "SELECT resolution_json FROM catalog_metadata_projection",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&raw).unwrap(),
        serde_json::to_value(resolved).unwrap()
    );
    assert_eq!(documents.len(), 3);
}

#[test]
fn snapshot_and_node_budgets_apply_to_persisted_complete_set() {
    let db = fixture();
    initialize(&db);
    let raw =
        serde_json::to_string(&json!({"title":{"op":"set","value":"x".repeat(60_000)}})).unwrap();
    for n in 0..34 {
        apply_patch(&db, &format!("p{n:03}"), SourceKind::Provider, 0, &raw);
    }
    unchanged_on_error(
        &db,
        &patch("over", "record", 0, &raw),
        &source("over", "record", SourceKind::Provider),
        MetadataStoreErrorCode::Core(MetadataError::SnapshotBudget),
    );
    let raw = serde_json::to_string(&json!({"extra":{"op":"set","value":vec![0;8192]}})).unwrap();
    unchanged_on_error(
        &db,
        &patch("over", "record", 0, &raw),
        &source("over", "record", SourceKind::Provider),
        MetadataStoreErrorCode::Core(MetadataError::JsonNodeBudget),
    );
}
#[test]
fn command_payload_catalog_and_incarnation_are_bound() {
    let db = fixture();
    let mutation = init_mutation();
    for (catalog, incarnation, payload, error) in [
        (
            "another-catalog",
            INCARNATION,
            json!({"operation":"metadata","mutation":mutation}),
            MetadataStoreErrorCode::CatalogMismatch,
        ),
        (
            CATALOG,
            "another-incarnation",
            json!({"operation":"metadata","mutation":mutation}),
            MetadataStoreErrorCode::IncarnationMismatch,
        ),
        (
            CATALOG,
            INCARNATION,
            json!({"operation":"item_put","item":{}}),
            MetadataStoreErrorCode::InvalidInput,
        ),
    ] {
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(catalog, HOST, "bound").unwrap(),
            incarnation,
            generation(&db),
            &serde_json::to_string(&payload).unwrap(),
        )
        .unwrap();
        assert_eq!(
            execute_at(&db, &mutation, &grant(MetadataAccess::Initialize), &command)
                .unwrap_err()
                .code,
            error
        );
        assert_eq!(count(&db, "catalog_metadata_subjects"), 0);
        validate_metadata_permits_empty(&db).unwrap();
    }
}

fn read_authority(db: &Connection) -> MetadataReadAuthority {
    MetadataReadAuthority {
        catalog_id: CATALOG.into(),
        incarnation: INCARNATION.into(),
        subject: SUBJECT.into(),
        expected_generation: generation(db),
    }
}
#[test]
fn privileged_read_exposes_conflicts_attribution_empty_inputs_and_removal() {
    let db = fixture();
    initialize(&db);
    apply_patch(
        &db,
        "p1",
        SourceKind::Provider,
        0,
        r#"{"title":{"op":"set","value":"a"}}"#,
    );
    apply_patch(
        &db,
        "p2",
        SourceKind::Provider,
        0,
        r#"{"title":{"op":"set","value":"b"}}"#,
    );
    let before = generation(&db);
    let view = read_metadata_connection(&db, &read_authority(&db)).unwrap();
    assert_eq!(view.generation, before);
    assert_eq!(view.incarnation, INCARNATION);
    assert_eq!(view.metadata, json!({}));
    assert_eq!(view.resolution.inputs.len(), 3);
    assert!(
        matches!(&view.resolution.fields["title"],FieldResolution::Conflict {candidates,..} if candidates.len()==2)
    );
    assert_eq!(generation(&db), before);
    apply_patch(
        &db,
        "local",
        SourceKind::Local,
        0,
        r#"{"title":{"op":"set","value":null}}"#,
    );
    assert_eq!(
        read_metadata_connection(&db, &read_authority(&db))
            .unwrap()
            .metadata,
        json!({"title":null})
    );
    apply_patch(
        &db,
        "local",
        SourceKind::Local,
        1,
        r#"{"title":{"op":"remove"}}"#,
    );
    let view = read_metadata_connection(&db, &read_authority(&db)).unwrap();
    assert_eq!(view.metadata, json!({}));
    assert_eq!(view.resolution.inputs.len(), 4);
    assert!(
        view.resolution
            .inputs
            .iter()
            .any(|source| source.kind == SourceKind::Local && source.revision == 2)
    );
}
#[test]
fn read_authority_scope_and_admission_fail_closed_without_changes() {
    let db = fixture();
    initialize(&db);
    let before = generation(&db);
    let mut authority = read_authority(&db);
    authority.subject = "different-item".into();
    assert_eq!(
        read_metadata_connection(&db, &authority).unwrap_err().code,
        MetadataStoreErrorCode::NotInitialized
    );
    let mut authority = read_authority(&db);
    authority.catalog_id = "different-catalog".into();
    assert_eq!(
        read_metadata_connection(&db, &authority).unwrap_err().code,
        MetadataStoreErrorCode::CatalogMismatch
    );
    let mut authority = read_authority(&db);
    authority.incarnation = "old-incarnation".into();
    assert_eq!(
        read_metadata_connection(&db, &authority).unwrap_err().code,
        MetadataStoreErrorCode::IncarnationMismatch
    );
    let mut authority = read_authority(&db);
    authority.expected_generation -= 1;
    assert_eq!(
        read_metadata_connection(&db, &authority).unwrap_err().code,
        MetadataStoreErrorCode::StaleGeneration
    );
    let authority = read_authority(&db);
    db.execute_batch("BEGIN").unwrap();
    assert_eq!(
        read_metadata_connection(&db, &authority).unwrap_err().code,
        MetadataStoreErrorCode::AmbientReadTransaction
    );
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(generation(&db), before);
}
#[test]
fn read_rechecks_generation_and_incarnation_after_source_queries() {
    for change_incarnation in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("catalog.db");
        let db = fixture_at(Some(&path));
        initialize(&db);
        db.execute_batch("PRAGMA journal_mode=WAL").unwrap();
        let authority = read_authority(&db);
        let writer = Connection::open(&path).unwrap();
        let fired = std::sync::atomic::AtomicBool::new(false);
        // Test-only deterministic interleaving: a second WAL writer commits
        // after admission, while the bounded source query is in progress.
        db.create_scalar_function("fixture_race",1,rusqlite::functions::FunctionFlags::SQLITE_UTF8,move |ctx| {
            if !fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                if change_incarnation { writer.execute("UPDATE catalog_state SET incarnation='restored-incarnation' WHERE id=1",[])?; }
                else { writer.execute("UPDATE catalog_state SET generation=generation+1 WHERE id=1",[])?; }
            }
            ctx.get::<String>(0)
        }).unwrap();
        db.execute_batch("CREATE TEMP VIEW catalog_metadata_sources AS SELECT item_id,namespace,record_id,source_kind,revision,schema_fingerprint,fields_json,fixture_race(evidence_json) AS evidence_json FROM main.catalog_metadata_sources").unwrap();
        let expected = if change_incarnation {
            MetadataStoreErrorCode::IncarnationMismatch
        } else {
            MetadataStoreErrorCode::StaleGeneration
        };
        assert_eq!(
            read_metadata_connection(&db, &authority).unwrap_err().code,
            expected
        );
    }
}
#[test]
fn read_preflights_source_count_and_bytes_before_document_decode() {
    let db = fixture();
    initialize(&db);
    // A deliberately malformed fixture makes decoding fail if budget preflight
    // is not performed first. Production constraints prevent this persisted state.
    db.execute_batch("PRAGMA ignore_check_constraints=ON; PRAGMA foreign_keys=OFF; DROP TRIGGER catalog_metadata_source_update").unwrap();
    db.execute(
        "UPDATE catalog_metadata_sources SET fields_json=?1",
        ["x".repeat(2 * 1024 * 1024 + 1)],
    )
    .unwrap();
    assert_eq!(
        read_metadata_connection(&db, &read_authority(&db))
            .unwrap_err()
            .code,
        MetadataStoreErrorCode::Core(MetadataError::SnapshotBudget)
    );
}

#[test]
fn retired_subject_rejects_mutation_but_historical_read_does_not_redirect() {
    let db = fixture();
    initialize(&db);
    apply_patch(
        &db,
        "p",
        SourceKind::Provider,
        0,
        r#"{"title":{"op":"set","value":"historical"}}"#,
    );
    let old_read = read_authority(&db);
    db.execute_batch("CREATE TABLE catalog_logical_retirements(item_id TEXT PRIMARY KEY, survivor_item_id TEXT); CREATE TRIGGER fixture_retirement_generation AFTER INSERT ON catalog_logical_retirements BEGIN UPDATE catalog_state SET generation=generation+1 WHERE id=1; END; INSERT INTO items VALUES('survivor','movie','{}')").unwrap();
    db.execute(
        "INSERT INTO catalog_logical_retirements VALUES(?1,'survivor')",
        [SUBJECT],
    )
    .unwrap();
    unchanged_on_error(
        &db,
        &patch(
            "p",
            "record",
            1,
            r#"{"title":{"op":"set","value":"changed"}}"#,
        ),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::RetiredSubject,
    );
    unchanged_on_error(
        &db,
        &reconcile(1, &["title"]),
        &grant(MetadataAccess::ReconcileLegacy),
        MetadataStoreErrorCode::RetiredSubject,
    );
    unchanged_on_error(
        &db,
        &init_mutation(),
        &grant(MetadataAccess::Initialize),
        MetadataStoreErrorCode::RetiredSubject,
    );
    assert_eq!(
        read_metadata_connection(&db, &old_read).unwrap_err().code,
        MetadataStoreErrorCode::StaleGeneration
    );
    let view = read_metadata_connection(&db, &read_authority(&db)).unwrap();
    assert_eq!(view.subject, SUBJECT);
    assert_eq!(view.metadata, json!({"title":"historical"}));
    let survivor: String = db
        .query_row("SELECT metadata FROM items WHERE id='survivor'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(survivor, "{}");
    assert!(
        view.resolution
            .inputs
            .iter()
            .all(|input| input.key.subject() == SUBJECT)
    );
}

fn invalidation_revision(db: &Connection, kind: &str, key: Value) -> i64 {
    db.query_row(
        "SELECT revision FROM catalog_entity_revisions WHERE entity_kind=?1 AND entity_key=?2",
        params![kind, serde_json::to_string(&key).unwrap()],
        |r| r.get(0),
    )
    .unwrap()
}
#[test]
fn every_durable_metadata_table_updates_entity_invalidations() {
    let db = fixture();
    initialize(&db);
    assert_eq!(
        invalidation_revision(&db, "catalog_metadata_subjects", json!([SUBJECT])),
        1
    );
    assert_eq!(
        invalidation_revision(
            &db,
            "catalog_metadata_sources",
            json!([SUBJECT, LEGACY_NAMESPACE, LEGACY_RECORD_ID])
        ),
        1
    );
    assert_eq!(
        invalidation_revision(
            &db,
            "catalog_metadata_history",
            json!([SUBJECT, LEGACY_NAMESPACE, LEGACY_RECORD_ID, 1])
        ),
        1
    );
    assert_eq!(
        invalidation_revision(&db, "catalog_metadata_projection", json!([SUBJECT])),
        1
    );
    apply_patch(&db, "p", SourceKind::Provider, 0, "{}");
    let key = serde_json::to_string(&json!([SUBJECT, "p", "record"])).unwrap();
    db.execute("UPDATE catalog_entity_revisions SET revision=100 WHERE entity_kind='catalog_metadata_sources' AND entity_key=?1",[key]).unwrap();
    apply_patch(&db, "p", SourceKind::Provider, 1, "{}");
    assert_eq!(head(&db, "p").0, 2);
    assert_eq!(
        invalidation_revision(
            &db,
            "catalog_metadata_sources",
            json!([SUBJECT, "p", "record"])
        ),
        101
    );
    assert_eq!(
        invalidation_revision(
            &db,
            "catalog_metadata_history",
            json!([SUBJECT, "p", "record", 1])
        ),
        1
    );
    assert_eq!(
        invalidation_revision(
            &db,
            "catalog_metadata_history",
            json!([SUBJECT, "p", "record", 2])
        ),
        1
    );
    assert_eq!(
        invalidation_revision(&db, "catalog_metadata_projection", json!([SUBJECT])),
        3
    );
    let permit_rows:i64=db.query_row("SELECT count(*) FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_permits'",[],|r|r.get(0)).unwrap();
    assert_eq!(permit_rows, 0);
    let deleted: i64 = db
        .query_row(
            "SELECT count(*) FROM catalog_entity_revisions WHERE deleted!=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(deleted, 0);
}
#[test]
fn entity_invalidation_overflow_aborts_and_rolls_back_whole_owner() {
    let db = fixture();
    initialize(&db);
    db.execute("UPDATE catalog_entity_revisions SET revision=?1 WHERE entity_kind='catalog_metadata_projection'",[i64::MAX]).unwrap();
    unchanged_on_error(
        &db,
        &patch(
            "p",
            "record",
            0,
            r#"{"title":{"op":"set","value":"rollback"}}"#,
        ),
        &source("p", "record", SourceKind::Provider),
        MetadataStoreErrorCode::StoreFailure,
    );
    assert_eq!(count(&db, "catalog_metadata_history"), 1);
    assert_eq!(count(&db, "catalog_metadata_sources"), 1);
    assert_eq!(count(&db, "catalog_entity_revisions"), 4);
    assert_eq!(
        invalidation_revision(&db, "catalog_metadata_projection", json!([SUBJECT])),
        i64::MAX
    );
}

#[test]
fn motion_imported_per_field_policy_cannot_accept_a_new_document_field() {
    // Motion export installs policies for the union of PRESENT doc.values keys,
    // not an extensible `document` JSON field. This regression deliberately
    // tests the real owner instead of treating absence as schema permission.
    let db = fixture();
    let initialize = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Initialize {
            subject: SUBJECT.into(),
            policies: vec![policy(
                "title",
                PolicyValueType::Json,
                false,
                PolicyNormalization::Exact,
            )],
        },
    };
    execute(&db, &initialize, &grant(MetadataAccess::Initialize)).unwrap();
    let schema = read_schema(&db, SUBJECT).unwrap();
    let mutation = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Patch {
            subject: SUBJECT.into(),
            namespace: "motion.local".into(),
            record_id: "document".into(),
            schema_fingerprint: schema.fingerprint().into(),
            expected_revision: 0,
            edits_json: r#"{"description":{"op":"set","value":"new field"}}"#.into(),
            evidence_json: "{}".into(),
        },
    };
    unchanged_on_error(
        &db,
        &mutation,
        &source("motion.local", "document", SourceKind::Local),
        MetadataStoreErrorCode::Core(MetadataError::UnknownField),
    );
    assert_eq!(count(&db, "catalog_metadata_sources"), 1);
}
