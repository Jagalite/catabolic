// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Schema-38 guards for the independent schema review (SC1-SC5 of e8f0c52).
//! Each case shows the schema-37 gap where it is observable, then the schema-38
//! refusal on the real Store connection with required native settings.
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
use catabolic_store::{
    Store,
    catalog_commands::{
        self as commands, CatalogAuthority, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority,
    },
    catalog_metadata_store::{
        MetadataAccess, MetadataAuthority, MetadataChange, MetadataMutation, PolicyField,
        PolicyNormalization, PolicyValueType,
    },
    catalog_pins::{self as pins, PinAuthority, PinBatch, PinChange, PinDecision, PinMutation},
    catalog_tag_mutation::{SuppressionChange, TagAuthority, TagMutation, TagSubject},
    connect, curation, migration, tagging,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

const HOST: &str = "guard-host";
const NS: &str = "tmdb.movie";
const FINGERPRINT: &str = "0000000000000000000000000000000000000000000000000000000000000000";

struct Fixture {
    root: tempfile::TempDir,
    path: PathBuf,
    store: Store,
}
impl Fixture {
    fn new(version: u32) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        migration::initialize_native_at(&path, version).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        assert_eq!(store.schema_version, version);
        seed(&store);
        Self { root, path, store }
    }
}
fn seed(store: &Store) {
    for id in ["item-a", "item-b"] {
        curation::item_put(
            store,
            &json!({"id":id,"kind":"movie","metadata":{"title":id},"identities":{NS:format!("{id}-tmdb")}}),
        )
        .unwrap();
    }
    store
        .transaction(|db| {
            db.execute("INSERT INTO locations VALUES('guard-source')", [])?;
            for file in ["file-a", "file-b"] {
                db.execute(
                    "INSERT INTO files(id,location,path) VALUES(?,'guard-source',?)",
                    params![file, format!("{file}.mkv")],
                )?;
            }
            Ok(())
        })
        .unwrap();
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
fn generation(db: &Connection) -> i64 {
    db.query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
        r.get(0)
    })
    .unwrap()
}
fn envelope(store: &Store, id: &str, payload: Value) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, HOST, id).unwrap(),
        store.catalog_incarnation.clone().unwrap(),
        generation(&store.db),
        &payload.to_string(),
    )
    .unwrap()
}
fn tag_grant() -> TagAuthority {
    TagAuthority {
        host_namespace: HOST.into(),
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
fn tag(store: &Store, name: &str) -> String {
    tagging::put(store, &json!(name), None).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn suppress(
    store: &Store,
    op: &str,
    subject: TagSubject,
    tag_id: &str,
    expected_revision: Option<i64>,
) -> catabolic_store::Result<CommandOutcome> {
    let mutation = TagMutation::SetSuppression {
        contract_version: 1,
        changes: vec![SuppressionChange {
            subject,
            canonical_tag_id: tag_id.into(),
            expected_revision,
            suppressed: true,
            reason: "schema review guard".into(),
        }],
    };
    let request = envelope(store, op, json!({"operation":"tags","mutation":mutation}));
    commands::execute_with_authority(
        store,
        &request,
        CatalogAuthority::new(base()).with_tags(&tag_grant()),
        policy(),
    )
    .map_err(|error| catabolic_store::Error(error.to_string()))
}
fn committed(outcome: &CommandOutcome) -> &Value {
    match outcome {
        CommandOutcome::Committed { result, .. } => result,
        _ => panic!("expected committed command: {outcome:?}"),
    }
}
fn quiescence() -> migration::NativeUpgradeQuiescence {
    migration::NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}
fn upgrade(path: &Path, parent: Option<&Path>) -> catabolic_store::Result<Value> {
    let plan = migration::plan_native(path)?;
    // Upgrades now continue through schema 39 (source ctimes), which keeps
    // every schema-38 guard.
    assert_eq!(plan["to_schema"], 39);
    migration::upgrade_native(path, &plan, quiescence(), parent, &|_, _, _| Ok(()))
}
fn error(result: rusqlite::Result<usize>) -> String {
    result.expect_err("guarded statement must fail").to_string()
}
fn count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

// SC1 ----------------------------------------------------------------------

#[test]
fn sc1_schema37_cascade_loses_the_head_and_wedges_the_next_decision() {
    let f = Fixture::new(37);
    let secret = tag(&f.store, "secret");
    committed(
        &suppress(
            &f.store,
            "s1",
            TagSubject::Item("item-a".into()),
            &secret,
            None,
        )
        .unwrap(),
    );
    f.store
        .transaction(|db| {
            db.execute("DELETE FROM identities WHERE item_id='item-a'", [])?;
            db.execute("DELETE FROM items WHERE id='item-a'", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        count(
            &f.store.db,
            "SELECT count(*) FROM catalog_item_tag_suppressions"
        ),
        0
    );
    curation::item_put(
        &f.store,
        &json!({"id":"item-a","kind":"movie","metadata":{"title":"recreated"}}),
    )
    .unwrap();
    // The finding: no head means revision 1, which collides with retained history.
    let retry = suppress(
        &f.store,
        "s2",
        TagSubject::Item("item-a".into()),
        &secret,
        None,
    );
    assert!(!matches!(retry, Ok(CommandOutcome::Committed { .. })));
}

#[test]
fn sc1_schema38_managed_suppression_subjects_cannot_be_deleted_replaced_or_rekeyed() {
    let f = Fixture::new(38);
    let secret = tag(&f.store, "secret");
    committed(
        &suppress(
            &f.store,
            "s1",
            TagSubject::Item("item-a".into()),
            &secret,
            None,
        )
        .unwrap(),
    );
    committed(
        &suppress(
            &f.store,
            "s2",
            TagSubject::File("file-a".into()),
            &secret,
            None,
        )
        .unwrap(),
    );
    let db = &f.store.db;
    let required = "catalog_tag_suppression_subject_retirement_required";
    for statement in [
        "DELETE FROM items WHERE id='item-a'",
        "INSERT OR REPLACE INTO items(id,kind,metadata) VALUES('item-a','movie','{}')",
        "REPLACE INTO items(id,kind,metadata) VALUES('item-a','movie','{}')",
        "UPDATE items SET id='item-z' WHERE id='item-a'",
        "DELETE FROM files WHERE id='file-a'",
        "INSERT OR REPLACE INTO files(id,location,path) VALUES('file-a','guard-source','file-a.mkv')",
        "UPDATE files SET id='file-z' WHERE id='file-a'",
    ] {
        let message = error(db.execute(statement, []));
        assert!(message.contains(required), "{statement}: {message}");
    }
    assert_eq!(count(db, "SELECT count(*) FROM items WHERE id='item-a'"), 1);
    assert_eq!(count(db, "SELECT count(*) FROM files WHERE id='file-a'"), 1);
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM catalog_effective_item_tags WHERE item_id='item-a'"
        ),
        0
    );
    // Unmanaged subjects keep their legacy behavior.
    f.store
        .transaction(|db| {
            db.execute("DELETE FROM files WHERE id='file-b'", [])?;
            db.execute("DELETE FROM identities WHERE item_id='item-b'", [])?;
            db.execute("DELETE FROM items WHERE id='item-b'", [])?;
            Ok(())
        })
        .unwrap();
    // The retained heads continue the revision sequence.
    committed(
        &suppress(
            &f.store,
            "s3",
            TagSubject::Item("item-a".into()),
            &secret,
            Some(1),
        )
        .unwrap(),
    );
    assert_eq!(
        count(
            db,
            "SELECT revision FROM catalog_item_tag_suppressions WHERE item_id='item-a'"
        ),
        2
    );
    migration::validate(db, true).unwrap();
}

#[test]
fn sc1_heads_cascaded_before_schema38_cannot_be_recreated_after_upgrade() {
    let f = Fixture::new(37);
    let secret = tag(&f.store, "secret");
    committed(
        &suppress(
            &f.store,
            "s1",
            TagSubject::Item("item-a".into()),
            &secret,
            None,
        )
        .unwrap(),
    );
    committed(
        &suppress(
            &f.store,
            "s2",
            TagSubject::File("file-a".into()),
            &secret,
            None,
        )
        .unwrap(),
    );
    f.store
        .transaction(|db| {
            db.execute("DELETE FROM identities WHERE item_id='item-a'", [])?;
            db.execute("DELETE FROM items WHERE id='item-a'", [])?;
            db.execute("DELETE FROM files WHERE id='file-a'", [])?;
            Ok(())
        })
        .unwrap();
    let Fixture {
        root: _root,
        path,
        store,
    } = f;
    drop(store);
    upgrade(&path, None).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    assert_eq!(store.schema_version, 39);
    let retained = "catalog_tag_suppression_subject_history_retained";
    let item = curation::item_put(
        &store,
        &json!({"id":"item-a","kind":"movie","metadata":{"title":"recreated"}}),
    )
    .unwrap_err()
    .to_string();
    assert!(item.contains(retained), "{item}");
    for statement in [
        "INSERT INTO items(id,kind,metadata) VALUES('item-a','movie','{}')",
        "UPDATE items SET id='item-a' WHERE id='item-b'",
        "INSERT INTO files(id,location,path) VALUES('file-a','guard-source','again.mkv')",
        "UPDATE files SET id='file-a' WHERE id='file-b'",
    ] {
        let message = error(store.db.execute(statement, []));
        assert!(message.contains(retained), "{statement}: {message}");
    }
    // Fresh IDs and upserts of existing subjects are unaffected.
    curation::item_put(
        &store,
        &json!({"id":"item-new","kind":"movie","metadata":{"title":"new"}}),
    )
    .unwrap();
    store
        .db
        .execute(
            "INSERT INTO items(id,kind,metadata) VALUES('item-b','movie','{}') ON CONFLICT(id) DO NOTHING",
            [],
        )
        .unwrap();
}

// SC2 ----------------------------------------------------------------------

fn raw_subject(db: &Connection, store: &Store, op: &str) -> rusqlite::Result<()> {
    let raw: String = db.query_row("SELECT metadata FROM items WHERE id='item-a'", [], |r| {
        r.get(0)
    })?;
    db.execute(
        "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES('item-a',?)",
        [store.catalog_incarnation.as_deref().unwrap()],
    )?;
    db.execute(
        "INSERT INTO catalog_metadata_subjects(item_id,contract_version,schema_fingerprint,policies_json,legacy_raw_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) VALUES('item-a',1,?,'[{}]',?,?,?,?,0)",
        params![FINGERPRINT, raw, store.database_id, HOST, op],
    )?;
    // Search admission requires every managed subject to have a projection.
    db.execute(
        "INSERT INTO catalog_metadata_projection(item_id,schema_fingerprint,resolution_json,effective_json) VALUES('item-a',?,'{}','{}')",
        [FINGERPRINT],
    )?;
    Ok(())
}
fn raw_history(db: &Connection, store: &Store, op: &str) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO catalog_metadata_history(item_id,namespace,record_id,revision,source_kind,schema_fingerprint,fields_json,evidence_json,raw_mutation_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) VALUES('item-a','local.guard','record',1,'local',?,'{}','{}','{}',?,?,?,0)",
        params![FINGERPRINT, store.database_id, HOST, op],
    )?;
    Ok(())
}
fn end_permit(db: &Connection) -> rusqlite::Result<()> {
    db.execute("DELETE FROM catalog_metadata_permits", [])?;
    Ok(())
}
fn raw_receipt(db: &Connection, store: &Store, op: &str) -> rusqlite::Result<()> {
    let generation = generation(db);
    db.execute(
        "INSERT INTO catalog_command_receipts(catalog_id,host_namespace,operation_id,contract_version,canonicalization_version,payload_digest,admission_incarnation,expected_generation,committed_generation,committed_at_us,retain_until_us,result_state,result_json) VALUES(?,?,?,1,1,?,?,?,?,1,2,'full','{}')",
        params![store.database_id, HOST, op, FINGERPRINT, store.catalog_incarnation.as_deref().unwrap(), generation, generation],
    )?;
    Ok(())
}
/// The Store owner flushes native search work before COMMIT, so a failure here
/// is the guard under test rather than an unflushed search barrier.
fn raw_transaction(
    store: &Store,
    work: impl FnOnce(&Connection) -> rusqlite::Result<()>,
) -> catabolic_store::Result<()> {
    store.transaction(|db| work(db).map_err(Into::into))
}

#[test]
fn sc2_schema37_accepts_metadata_provenance_without_a_receipt() {
    let f = Fixture::new(37);
    raw_transaction(&f.store, |db| {
        raw_subject(db, &f.store, "orphan")?;
        end_permit(db)
    })
    .unwrap();
    assert_eq!(
        count(
            &f.store.db,
            "SELECT count(*) FROM catalog_metadata_subjects"
        ),
        1
    );
    // Upgrading such a catalog backfills the obligation and fails closed.
    drop(f.store);
    let error = upgrade(&f.path, None).unwrap_err().to_string();
    assert!(error.contains("foreign-key"), "{error}");
    let store = Store::open(&f.path, false, false).unwrap();
    assert_eq!(store.schema_version, 37);
    drop(f.root);
}

#[test]
fn sc2_schema38_metadata_subjects_and_history_require_a_committed_receipt() {
    let f = Fixture::new(38);
    let db = &f.store.db;
    let orphan = raw_transaction(&f.store, |db| {
        raw_subject(db, &f.store, "orphan-subject")?;
        end_permit(db)
    })
    .unwrap_err()
    .to_string();
    assert!(orphan.contains("FOREIGN KEY constraint failed"), "{orphan}");
    let orphan = raw_transaction(&f.store, |db| {
        raw_subject(db, &f.store, "receipted")?;
        raw_history(db, &f.store, "orphan-history")?;
        end_permit(db)?;
        raw_receipt(db, &f.store, "receipted")
    })
    .unwrap_err()
    .to_string();
    assert!(orphan.contains("FOREIGN KEY constraint failed"), "{orphan}");
    assert_eq!(
        count(db, "SELECT count(*) FROM catalog_metadata_subjects"),
        0
    );
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM catalog_metadata_receipt_obligations"
        ),
        0
    );
    // Receipt-last commands satisfy the deferred obligation.
    raw_transaction(&f.store, |db| {
        raw_subject(db, &f.store, "receipted")?;
        raw_history(db, &f.store, "receipted")?;
        end_permit(db)?;
        raw_receipt(db, &f.store, "receipted")
    })
    .unwrap();
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM catalog_metadata_receipt_obligations WHERE command_operation_id='receipted'"
        ),
        1
    );
    for statement in [
        "DELETE FROM catalog_metadata_receipt_obligations",
        "UPDATE catalog_metadata_receipt_obligations SET command_operation_id='x'",
    ] {
        let message = error(db.execute(statement, []));
        assert!(
            message.contains("catalog_metadata_receipt_obligation"),
            "{statement}: {message}"
        );
    }
    migration::validate(db, true).unwrap();
}

#[test]
fn sc2_schema38_real_metadata_command_records_its_receipt_obligation() {
    let f = Fixture::new(38);
    let mutation = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Initialize {
            subject: "item-a".into(),
            policies: vec![PolicyField {
                field: "title".into(),
                value_type: PolicyValueType::String,
                nullable: true,
                normalization: PolicyNormalization::Exact,
            }],
        },
    };
    let request = envelope(
        &f.store,
        "metadata-init",
        json!({"operation":"metadata","mutation":mutation}),
    );
    let grant = MetadataAuthority {
        host_namespace: HOST.into(),
        subject: "item-a".into(),
        access: MetadataAccess::Initialize,
    };
    let outcome = commands::execute_with_authority(
        &f.store,
        &request,
        CatalogAuthority::new(base()).with_metadata(&grant),
        policy(),
    )
    .unwrap();
    committed(&outcome);
    assert_eq!(
        count(
            &f.store.db,
            "SELECT count(*) FROM catalog_metadata_receipt_obligations o JOIN catalog_command_receipts r ON r.catalog_id=o.command_catalog_id AND r.host_namespace=o.command_host_namespace AND r.operation_id=o.command_operation_id WHERE o.command_operation_id='metadata-init'"
        ),
        1
    );
    migration::validate(&f.store.db, true).unwrap();
}

// SC3 ----------------------------------------------------------------------

#[test]
fn sc3_schema38_curation_records_cannot_reuse_a_committed_receipt() {
    let f = Fixture::new(38);
    let secret = tag(&f.store, "secret");
    // Real receipt-last owners still commit under the sealing guards.
    committed(
        &suppress(
            &f.store,
            "sealed",
            TagSubject::Item("item-a".into()),
            &secret,
            None,
        )
        .unwrap(),
    );
    let pin_grant = PinAuthority {
        host_namespace: HOST.into(),
        readable: BTreeMap::new(),
        mutable: [(NS.into(), BTreeSet::from(["item-a".to_string()]))].into(),
    };
    let batch = PinBatch {
        contract_version: 1,
        changes: vec![PinChange {
            item_id: "item-a".into(),
            namespace: NS.into(),
            expected_revision: 0,
            decision: PinDecision::Set {
                value: "accepted".into(),
            },
        }],
    };
    let preview = pins::preview_pins(&f.store, &batch, &pin_grant).unwrap();
    let request = CommandEnvelope::new(
        1,
        CommandKey::new(&f.store.database_id, HOST, "pin").unwrap(),
        preview.incarnation.clone(),
        preview.expected_generation,
        &json!({"operation":"pins","mutation":PinMutation{batch,preview_digest:preview.digest.clone()}}).to_string(),
    )
    .unwrap();
    committed(
        &commands::execute_with_authority(
            &f.store,
            &request,
            CatalogAuthority::new(base()).with_pins(&pin_grant),
            policy(),
        )
        .unwrap(),
    );

    let sealed = "catalog_committed_history_sealed";
    let (catalog, incarnation) = (
        f.store.database_id.clone(),
        f.store.catalog_incarnation.clone().unwrap(),
    );
    type HistoryMutation<'a> = Box<dyn Fn(&Connection, &str) -> rusqlite::Result<()> + 'a>;
    let attempts: Vec<(&str, HistoryMutation<'_>)> = vec![
        (
            "catalog_tag_suppression_revisions",
            Box::new(|db, op| {
                db.execute("INSERT INTO catalog_tag_suppression_revisions VALUES('item','item-b',?,1,1,'forged',0,?,?,?)", params![secret, catalog, HOST, op])?;
                Ok(())
            }),
        ),
        (
            "catalog_tag_merges",
            Box::new(|db, op| {
                db.execute(
                    "INSERT INTO catalog_tag_merges VALUES('retired-x','target-y','{}',0,?,?,?)",
                    params![catalog, HOST, op],
                )?;
                Ok(())
            }),
        ),
        (
            "catalog_identity_pin_revisions",
            Box::new(|db, op| {
                db.execute("INSERT INTO catalog_identity_pin_revisions VALUES('item-b',?,1,'forged',0,?,?,?)", params![NS, catalog, HOST, op])?;
                Ok(())
            }),
        ),
        (
            "catalog_metadata_subjects",
            Box::new(|db, op| raw_subject(db, &f.store, op)),
        ),
        (
            "catalog_logical_retirements",
            Box::new(|db, op| {
                db.execute("INSERT INTO catalog_logical_edit_permits(operation_id,incarnation) VALUES(?,?)", params![op, incarnation])?;
                db.execute("INSERT INTO catalog_logical_retirements VALUES('item-a','merge','item-b','[]','{}',?,?,?,0)", params![catalog, HOST, op])?;
                Ok(())
            }),
        ),
    ];
    for (table, attempt) in &attempts {
        for op in ["sealed", "pin"] {
            let message = raw_transaction(&f.store, |db| attempt(db, op))
                .unwrap_err()
                .to_string();
            assert!(message.contains(sealed), "{table}/{op}: {message}");
        }
    }
    // History needs an existing subject, so seal it in the same transaction as
    // a valid subject keyed by a fresh, receipted command.
    let message = raw_transaction(&f.store, |db| {
        raw_subject(db, &f.store, "fresh")?;
        raw_history(db, &f.store, "sealed")
    })
    .unwrap_err()
    .to_string();
    assert!(
        message.contains(sealed),
        "catalog_metadata_history: {message}"
    );
    drop(attempts);

    // Version retirement needs a live version owner; isolate the new trigger on
    // a separate rolled-back connection without the schema-34 guard or FKs.
    let isolated = Connection::open(&f.path).unwrap();
    isolated.pragma_update(None, "foreign_keys", false).unwrap();
    isolated
        .execute_batch("BEGIN IMMEDIATE; DROP TRIGGER catalog_version_retirement_guard;")
        .unwrap();
    let insert = |op: &str| {
        isolated.execute(
            "INSERT INTO catalog_version_retirements(version_id,version_revision,command_catalog_id,command_host_namespace,command_operation_id,recorded_at_us) VALUES(?,1,?,?,?,0)",
            params![format!("version-{op}"), f.store.database_id, HOST, op],
        )
    };
    let message = error(insert("sealed"));
    assert!(
        message.contains(sealed),
        "catalog_version_retirements: {message}"
    );
    insert("not-yet-receipted").unwrap();
    isolated.execute_batch("ROLLBACK").unwrap();
    drop(isolated);
    migration::validate(&f.store.db, true).unwrap();
}

// SC4 ----------------------------------------------------------------------

fn owned_lifetime(store: &Store) {
    store
        .transaction(|db| {
            db.execute("INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state,attempts) VALUES('job','default','file-a','hash','guard-source','{}','{}','key','complete',1)", [])?;
            db.execute("INSERT INTO execution_claims VALUES('job',1,'token',1,1,0,'{}','{}')", [])?;
            db.execute("INSERT INTO native_execution_lifetimes(catalog_id,incarnation,job_id,generation,attempt,claim_token_digest,snapshot_digest,options_digest,runtime_owner_nonce,protocol_version) SELECT catalog_id,incarnation,'job',1,1,?1,?1,?1,'0123456789abcdef0123456789abcdef',1 FROM catalog_state WHERE id=1", [FINGERPRINT])?;
            Ok(())
        })
        .unwrap();
}
const NULL_KIND: &str = "UPDATE native_execution_lifetimes SET state='quiescent',cleanup_kind=NULL,capacity_released=1,sealed_at='2026-01-01'";

#[test]
fn sc4_schema37_null_cleanup_kind_terminal_row_is_rejected_by_full_validation() {
    let f = Fixture::new(37);
    owned_lifetime(&f.store);
    // The schema-36 CHECK evaluates NULL for this row and accepts it.
    f.store.db.execute(NULL_KIND, []).unwrap();
    let error = migration::validate(&f.store.db, true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("incomplete terminal witness"), "{error}");
    drop(f.store);
    let error = migration::plan_native(&f.path).unwrap_err().to_string();
    assert!(error.contains("incomplete terminal witness"), "{error}");
}

#[test]
fn sc4_schema38_terminal_rows_require_the_complete_witness() {
    let f = Fixture::new(38);
    owned_lifetime(&f.store);
    let db = &f.store.db;
    let message = error(db.execute(NULL_KIND, []));
    assert!(
        message.contains("invalid_native_execution_terminal_witness"),
        "{message}"
    );
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM native_execution_lifetimes WHERE state='owned'"
        ),
        1
    );
    db.execute("UPDATE native_execution_lifetimes SET state='quiescent',cleanup_kind='builtin_no_child_v1',capacity_released=1,sealed_at='2026-01-01'", []).unwrap();
    migration::validate(db, true).unwrap();
}

// SC5 ----------------------------------------------------------------------

#[test]
fn sc5_upgrade_creates_every_missing_backup_ancestor_privately() {
    let f = Fixture::new(37);
    drop(f.store);
    let parent = f.root.path().join("absent-a/absent-b/absent-c");
    let report = upgrade(&f.path, Some(&parent)).unwrap();
    assert_eq!(report["schema"], 39);
    let backup = PathBuf::from(report["backup"].as_str().unwrap());
    assert!(backup.starts_with(parent.canonicalize().unwrap()));
    for ancestor in [
        "absent-a",
        "absent-a/absent-b",
        "absent-a/absent-b/absent-c",
    ] {
        let mode = std::fs::metadata(f.root.path().join(ancestor))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "{ancestor}");
    }
    let saved = connect(&backup, false).unwrap();
    assert_eq!(migration::validate(&saved, true).unwrap()["schema"], 37);
}
