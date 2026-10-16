use super::*;
use crate::{
    catalog_metadata_store::{PolicyField, PolicyNormalization, PolicyValueType},
    curation, definitions, migration,
};
use catabolic_core::{catalog_host_lease::LeaseError, encode};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
const HOST: &str = "write-host";
use crate::catalog_schema37_test_support as schema37_fixture;
thread_local! { static FIXTURE_SCHEMA: Cell<u32> = const { Cell::new(35) }; }
struct FixtureSchema(u32);
impl Drop for FixtureSchema {
    fn drop(&mut self) {
        FIXTURE_SCHEMA.with(|value| value.set(self.0));
    }
}
fn with_schema(schema: u32, test: impl FnOnce()) {
    let _scope = FixtureSchema(FIXTURE_SCHEMA.with(|value| value.replace(schema)));
    test();
}
struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    query: SavedQueryRef,
    fingerprint: String,
    catalog: String,
    incarnation: String,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        let target = FIXTURE_SCHEMA.with(Cell::get);
        let source = if target == 37 { 36 } else { target };
        migration::initialize_native_at(&path, source).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        for id in ["a", "hidden"] {
            curation::item_put(
                &store,
                &json!({"id":id,"kind":"video","metadata":{"title":"Old","secret":"retained"}}),
            )
            .unwrap();
        }
        let mutation = MetadataMutation {
            contract_version: 1,
            change: MetadataChange::Initialize {
                subject: "a".into(),
                policies: ["title", "secret"]
                    .into_iter()
                    .map(|field| PolicyField {
                        field: field.into(),
                        value_type: PolicyValueType::String,
                        nullable: true,
                        normalization: PolicyNormalization::Exact,
                    })
                    .collect(),
            },
        };
        let generation: i64 = store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(&store.database_id, HOST, "init").unwrap(),
            store.catalog_incarnation.as_deref().unwrap(),
            generation,
            &encode(&json!({"operation":"metadata","mutation":mutation})),
        )
        .unwrap();
        let source = MetadataAuthority {
            host_namespace: HOST.into(),
            subject: "a".into(),
            access: MetadataAccess::Initialize,
        };
        catalog_commands::execute_with_authority(
            &store,
            &command,
            CatalogAuthority::new(TrustedCommandAuthority {
                host_namespace: HOST,
                receipt_visibility: ReceiptVisibility::FullResult,
                allow_new_commands: true,
                identity_authority: None,
            })
            .with_metadata(&source),
            policy(),
        )
        .unwrap();
        let row=definitions::query_put(&store,"default","editable",&json!({"version":1,"mode":"selection","selection":{"language":"sql","query":"SELECT item_id FROM catalog_items WHERE item_id='a'"}})).unwrap();
        let query = SavedQueryRef::new(
            row["id"].as_str().unwrap().into(),
            row["revision"].as_i64().unwrap(),
            row["digest"].as_str().unwrap().into(),
        )
        .unwrap();
        let fingerprint = store
            .db
            .query_row(
                "SELECT schema_fingerprint FROM catalog_metadata_subjects WHERE item_id='a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let catalog = store.database_id.clone();
        let incarnation = store.catalog_incarnation.clone().unwrap();
        drop(store);
        if target == 37 {
            schema37_fixture::upgrade(&path);
        }
        Self {
            _root: root,
            path,
            query,
            fingerprint,
            catalog,
            incarnation,
        }
    }
    fn writer(&self) -> PrivateCatalogWriter {
        PrivateCatalogWriter::open(&self.path).unwrap()
    }
    fn request(&self, id: &str) -> MetadataPatchRequest {
        MetadataPatchRequest {
            operation_id: id.into(),
            schema_fingerprint: self.fingerprint.clone(),
            expected_revision: 0,
            edits_json: r#"{"title":{"op":"set","value":"New"}}"#.into(),
            evidence_json: "{}".into(),
        }
    }
    fn grant(
        &self,
        host: &HostAuthorization,
        subject: &str,
        fields: Vec<String>,
    ) -> TrustedMetadataWriteGrant {
        TrustedMetadataWriteGrant::new(
            host.clone(),
            "default".into(),
            vec![self.query.clone()],
            MetadataAuthority {
                host_namespace: HOST.into(),
                subject: subject.into(),
                access: MetadataAccess::Source {
                    namespace: "editorial".into(),
                    record_id: "one".into(),
                    kind: SourceKind::Local,
                },
            },
            fields,
            ReceiptVisibility::FullResult,
        )
        .unwrap()
    }
    fn admit(
        &self,
        writer: &mut PrivateCatalogWriter,
        host: &HostAuthorization,
    ) -> MetadataWriteHandle {
        writer
            .admit_metadata_write(
                self.grant(host, "a", vec!["title".into()]),
                &lease(host),
                read_budget(),
            )
            .unwrap()
    }
}
fn host() -> HostAuthorization {
    HostAuthorization::new(HOST.into(), "write-revision".into()).unwrap()
}
fn lease(host: &HostAuthorization) -> AuthorizationLease {
    host.acquire(host.revision()).unwrap()
}
fn read_budget() -> ReadBudget {
    ReadBudget::new(Duration::from_secs(5), 1000).unwrap()
}
fn budget() -> WriteBudget {
    WriteBudget::new(
        Duration::from_secs(5),
        10000,
        100000,
        16 * 1024 * 1024,
        1000000,
    )
    .unwrap()
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 1000,
        max_logical_bytes: 64 * 1024 * 1024,
        max_hold_records: 100,
    }
}
fn code<T>(result: Result<T>) -> WriteErrorCode {
    match result {
        Ok(_) => panic!("unexpected success"),
        Err(error) => error.code,
    }
}
fn value(result: LeasedRead) -> Value {
    result.deliver(Clone::clone)
}
fn state(writer: &PrivateCatalogWriter) -> (String, i64, i64, i64) {
    let db = &writer.store().unwrap().db;
    (
        db.query_row("SELECT metadata FROM items WHERE id='a'", [], |r| r.get(0))
            .unwrap(),
        db.query_row("SELECT count(*) FROM catalog_metadata_history", [], |r| {
            r.get(0)
        })
        .unwrap(),
        db.query_row("SELECT count(*) FROM catalog_command_receipts", [], |r| {
            r.get(0)
        })
        .unwrap(),
        db.query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap(),
    )
}
fn envelope(handle: &MetadataWriteHandle, request: MetadataPatchRequest) -> CommandEnvelope {
    let (catalog, incarnation, generation) = handle.scope.command_stamp();
    let MetadataAccess::Source {
        namespace,
        record_id,
        ..
    } = &handle.source.access
    else {
        panic!()
    };
    let mutation = MetadataMutation {
        contract_version: 1,
        change: MetadataChange::Patch {
            subject: handle.source.subject.clone(),
            namespace: namespace.clone(),
            record_id: record_id.clone(),
            schema_fingerprint: request.schema_fingerprint,
            expected_revision: request.expected_revision,
            edits_json: request.edits_json,
            evidence_json: request.evidence_json,
        },
    };
    CommandEnvelope::new(
        1,
        CommandKey::new(catalog, HOST, request.operation_id).unwrap(),
        incarnation,
        generation,
        &encode(&json!({"operation":"metadata","mutation":mutation})),
    )
    .unwrap()
}
#[test]
fn typed_patch_commits_source_projection_index_and_receipt_atomically() {
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = f.admit(&mut writer, &host);
    let result = value(
        writer
            .execute_metadata_patch(&handle, lease(&host), f.request("edit"), policy(), budget())
            .unwrap(),
    );
    assert_eq!(result["status"], "committed");
    let db = &writer.store().unwrap().db;
    let title: String = db
        .query_row(
            "SELECT title FROM catalog_search_documents WHERE item_id='a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(title, "New");
    let metadata: Value = catabolic_core::decode_json(&state(&writer).0).unwrap();
    assert_eq!(metadata["title"], "New");
    assert_eq!(metadata["secret"], "retained");
    assert_eq!(
        db.query_row("SELECT count(*) FROM catalog_metadata_permits", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM catalog_search_dirty", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(db.is_autocommit());
    let epoch = writer.epoch;
    let before = state(&writer);
    let replay = value(
        writer
            .execute_metadata_patch(&handle, lease(&host), f.request("edit"), policy(), budget())
            .unwrap(),
    );
    assert_eq!(replay["status"], "replay");
    assert_eq!(state(&writer), before);
    assert_eq!(writer.epoch, epoch);
}
#[test]
fn write_grant_membership_and_editable_fields_are_independent() {
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let before = state(&writer);
    let hidden = writer
        .admit_metadata_write(
            f.grant(&host, "hidden", vec!["title".into()]),
            &lease(&host),
            read_budget(),
        )
        .unwrap();
    assert_eq!(
        code(writer.execute_metadata_patch(
            &hidden,
            lease(&host),
            f.request("hidden"),
            policy(),
            budget()
        )),
        WriteErrorCode::Command(CommandStoreErrorCode::AuthorityDenied)
    );
    let handle = f.admit(&mut writer, &host);
    let mut request = f.request("secret");
    request.edits_json = r#"{"secret":{"op":"set","value":"blocked"}}"#.into();
    assert_eq!(
        code(writer.execute_metadata_patch(&handle, lease(&host), request, policy(), budget())),
        WriteErrorCode::Command(CommandStoreErrorCode::AuthorityDenied)
    );
    assert_eq!(state(&writer), before);
    let denied = TrustedMetadataWriteGrant::new(
        host.clone(),
        "default".into(),
        vec![f.query.clone()],
        MetadataAuthority {
            host_namespace: HOST.into(),
            subject: "a".into(),
            access: MetadataAccess::Source {
                namespace: "editorial".into(),
                record_id: "one".into(),
                kind: SourceKind::Provider,
            },
        },
        vec!["title".into()],
        ReceiptVisibility::FullResult,
    );
    assert_eq!(code(denied), WriteErrorCode::AuthorityDenied);
}
#[test]
fn stale_source_cas_rolls_back_every_durable_projection() {
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = f.admit(&mut writer, &host);
    writer
        .execute_metadata_patch(&handle, lease(&host), f.request("one"), policy(), budget())
        .unwrap();
    let next = f.admit(&mut writer, &host);
    let before = state(&writer);
    assert_eq!(
        code(writer.execute_metadata_patch(
            &next,
            lease(&host),
            f.request("bad-cas"),
            policy(),
            budget()
        )),
        WriteErrorCode::Command(CommandStoreErrorCode::OperationConflict)
    );
    assert_eq!(state(&writer), before);
}
#[test]
fn source_trigger_vm_exhaustion_rolls_back_and_releases_all_permits() {
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = f.admit(&mut writer, &host);
    let before = state(&writer);
    writer.store().unwrap().db.execute_batch("CREATE TEMP TRIGGER slow_metadata BEFORE UPDATE ON main.items BEGIN SELECT sum(x) FROM (WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<1000000) SELECT x FROM n); END;").unwrap();
    let low = WriteBudget::new(
        Duration::from_secs(5),
        10000,
        100000,
        16 * 1024 * 1024,
        10000,
    )
    .unwrap();
    assert_eq!(
        code(writer.execute_metadata_patch(
            &handle,
            lease(&host),
            f.request("interrupted"),
            policy(),
            low
        )),
        WriteErrorCode::Budget(EvaluationFailure::WorkBudget)
    );
    assert_eq!(state(&writer), before);
    assert_eq!(writer.epoch, 0);
    let db = &writer.store().unwrap().db;
    assert!(db.is_autocommit());
    assert_eq!(
        db.query_row("SELECT count(*) FROM catalog_metadata_permits", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    assert!(crate::native_writer_vm::current_budget(db).is_none());
    db.execute_batch("DROP TRIGGER slow_metadata").unwrap();
    assert_eq!(
        value(
            writer
                .execute_metadata_patch(
                    &handle,
                    lease(&host),
                    f.request("retry"),
                    policy(),
                    budget()
                )
                .unwrap()
        )["status"],
        "committed"
    );
}
#[test]
fn maintenance_rebuild_is_privileged_and_invalidates_old_membership_epoch() {
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let write = f.admit(&mut writer, &host);
    let generation = state(&writer).3;
    let maintenance = writer
        .admit_index_maintenance(
            TrustedIndexMaintenance::new(host.clone(), f.catalog.clone(), f.incarnation.clone()),
            &lease(&host),
        )
        .unwrap();
    let result = value(
        writer
            .rebuild_index(&maintenance, lease(&host), budget())
            .unwrap(),
    );
    assert_eq!(result["ready"], true);
    assert_eq!(state(&writer).3, generation);
    assert_eq!(
        code(writer.execute_metadata_patch(
            &write,
            lease(&host),
            f.request("old-proof"),
            policy(),
            budget()
        )),
        WriteErrorCode::Command(CommandStoreErrorCode::AuthorityDenied)
    );
    let fresh = f.admit(&mut writer, &host);
    assert!(
        writer
            .execute_metadata_patch(
                &fresh,
                lease(&host),
                f.request("fresh-proof"),
                policy(),
                budget()
            )
            .is_ok()
    );
}
#[test]
fn external_nongeneration_commit_invalidates_identical_connection_data_version() {
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let write = f.admit(&mut writer, &host);
    let generation = state(&writer).3;
    let other = rusqlite::Connection::open(&f.path).unwrap();
    other
        .execute(
            "UPDATE catalog_search_state SET rebuilt_generation=123 WHERE id=1",
            [],
        )
        .unwrap();
    drop(other);
    assert_eq!(state(&writer).3, generation);
    assert_eq!(
        code(writer.execute_metadata_patch(
            &write,
            lease(&host),
            f.request("external-change"),
            policy(),
            budget()
        )),
        WriteErrorCode::Command(CommandStoreErrorCode::AuthorityDenied)
    );
}
#[test]
fn lease_stays_owned_through_commit_and_delivery_after_gate_closes() {
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = f.admit(&mut writer, &host);
    let revoked = Arc::new(Mutex::new(None));
    let saved = revoked.clone();
    let gate = host.clone();
    writer
        .store()
        .unwrap()
        .db
        .commit_hook(Some(move || {
            let revocation = gate.begin_revocation().unwrap();
            assert_eq!(
                revocation.wait_drained(Duration::ZERO),
                Err(LeaseError::Timeout)
            );
            *saved.lock().unwrap() = Some(revocation);
            false
        }))
        .unwrap();
    let result = writer
        .execute_metadata_patch(
            &handle,
            lease(&host),
            f.request("leased"),
            policy(),
            budget(),
        )
        .unwrap();
    assert!(host.acquire(host.revision()).is_err());
    let revocation = revoked.lock().unwrap().take().unwrap();
    assert_eq!(
        revocation.wait_drained(Duration::ZERO),
        Err(LeaseError::Timeout)
    );
    result.deliver(|value| {
        assert_eq!(value["status"], "committed");
        assert_eq!(
            revocation.wait_drained(Duration::ZERO),
            Err(LeaseError::Timeout)
        );
    });
    assert_eq!(revocation.wait_drained(Duration::ZERO), Ok(()));
}
#[test]
fn independent_receipt_authority_replays_after_write_revocation_and_generation_change() {
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = f.admit(&mut writer, &host);
    let command = envelope(&handle, f.request("receipt"));
    writer
        .execute_metadata_patch(
            &handle,
            lease(&host),
            f.request("receipt"),
            policy(),
            budget(),
        )
        .unwrap();
    host.begin_revocation()
        .unwrap()
        .wait_drained(Duration::ZERO)
        .unwrap();
    let receipt_host = HostAuthorization::new(HOST.into(), "receipt-read-only".into()).unwrap();
    let grant = TrustedReceiptReadGrant::new(
        receipt_host.clone(),
        f.catalog.clone(),
        "receipt".into(),
        ReceiptVisibility::FullResult,
    );
    let result = value(
        writer
            .reconcile_receipt(grant, lease(&receipt_host), &command, policy(), budget())
            .unwrap(),
    );
    assert_eq!(result["status"], "replay");
    let unknown = envelope(&handle, f.request("unknown"));
    let grant = TrustedReceiptReadGrant::new(
        receipt_host.clone(),
        f.catalog.clone(),
        "unknown".into(),
        ReceiptVisibility::FullResult,
    );
    let result = value(
        writer
            .reconcile_receipt(grant, lease(&receipt_host), &unknown, policy(), budget())
            .unwrap(),
    );
    assert_eq!(result["status"], "stale_generation");
}
#[test]
fn maintenance_budget_failure_keeps_previous_index_and_releases_vm() {
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = writer
        .admit_index_maintenance(
            TrustedIndexMaintenance::new(host.clone(), f.catalog.clone(), f.incarnation.clone()),
            &lease(&host),
        )
        .unwrap();
    let before = state(&writer);
    let low = WriteBudget::new(Duration::from_secs(5), 0, 1000, 1024 * 1024, 100000).unwrap();
    assert_eq!(
        code(writer.rebuild_index(&handle, lease(&host), low)),
        WriteErrorCode::Budget(EvaluationFailure::IdBudget)
    );
    assert_eq!(state(&writer), before);
    assert!(writer.store().unwrap().db.is_autocommit());
    assert!(crate::native_writer_vm::current_budget(&writer.store().unwrap().db).is_none());
    assert!(
        writer
            .rebuild_index(&handle, lease(&host), budget())
            .is_ok()
    );
}
#[test]
fn private_writer_and_opaque_handles_remain_send() {
    fn assert_send<T: Send>() {}
    assert_send::<PrivateCatalogWriter>();
    assert_send::<MetadataWriteHandle>();
    assert_send::<IndexMaintenanceHandle>();
}

fn durable_state(writer: &PrivateCatalogWriter) -> Vec<Vec<String>> {
    let db = &writer.store().unwrap().db;
    [
        "SELECT * FROM items ORDER BY id",
        "SELECT * FROM catalog_metadata_sources ORDER BY item_id, namespace, record_id",
        "SELECT * FROM catalog_metadata_history ORDER BY rowid",
        "SELECT * FROM catalog_metadata_projection ORDER BY item_id",
        "SELECT * FROM catalog_metadata_permits ORDER BY item_id",
        "SELECT * FROM catalog_mutation_permits ORDER BY scope_kind, scope_id",
        "SELECT * FROM catalog_search_documents ORDER BY item_id",
        "SELECT * FROM catalog_search_values ORDER BY rowid",
        "SELECT * FROM catalog_search_fts_data ORDER BY id",
        "SELECT * FROM catalog_search_fts_idx ORDER BY segid, term",
        "SELECT * FROM catalog_search_fts_docsize ORDER BY id",
        "SELECT * FROM catalog_search_fts_config ORDER BY k",
        "SELECT * FROM catalog_search_dirty ORDER BY item_id",
        "SELECT * FROM catalog_search_state ORDER BY id",
        "SELECT * FROM catalog_command_receipts ORDER BY rowid",
        "SELECT * FROM catalog_state ORDER BY id",
    ]
    .into_iter()
    .map(|sql| {
        let mut statement = db.prepare(sql).unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                Ok((0..columns)
                    .map(|column| format!("{:?}", row.get_ref(column).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    })
    .collect()
}

#[test]
fn actual_vm_budget_reaches_derived_flush_and_receipt_insertion() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for (stage, table) in [
        ("derived-flush", "catalog_search_documents"),
        ("receipt-insert", "catalog_command_receipts"),
    ] {
        let f = Fixture::new();
        let host = host();
        let mut writer = f.writer();
        let handle = f.admit(&mut writer, &host);
        let before = durable_state(&writer);
        let reached = Arc::new(AtomicBool::new(false));
        let mark = reached.clone();
        writer
            .store()
            .unwrap()
            .db
            .create_scalar_function(
                "test_mark_writer_stage",
                0,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                move |_| {
                    mark.store(true, Ordering::Release);
                    Ok(1i64)
                },
            )
            .unwrap();
        writer
            .store()
            .unwrap()
            .db
            .execute_batch(&format!(
                "CREATE TEMP TRIGGER slow_stage BEFORE INSERT ON main.{table} BEGIN
             SELECT test_mark_writer_stage();
             SELECT sum(x) FROM (WITH RECURSIVE n(x) AS
             (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<1000000) SELECT x FROM n);
             END;"
            ))
            .unwrap();
        let low = WriteBudget::new(
            Duration::from_secs(5),
            10000,
            100000,
            16 * 1024 * 1024,
            100000,
        )
        .unwrap();
        assert_eq!(
            code(writer.execute_metadata_patch(
                &handle,
                lease(&host),
                f.request(stage),
                policy(),
                low
            )),
            WriteErrorCode::Budget(EvaluationFailure::WorkBudget),
            "{stage}"
        );
        assert!(reached.load(Ordering::Acquire), "did not reach {stage}");
        assert_eq!(durable_state(&writer), before, "{stage}");
        assert!(writer.store().unwrap().db.is_autocommit());
        assert!(crate::native_writer_vm::current_budget(&writer.store().unwrap().db).is_none());
        writer
            .store()
            .unwrap()
            .db
            .execute_batch("DROP TRIGGER slow_stage")
            .unwrap();
        assert_eq!(
            value(
                writer
                    .execute_metadata_patch(
                        &handle,
                        lease(&host),
                        f.request(stage),
                        policy(),
                        budget()
                    )
                    .unwrap()
            )["status"],
            "committed"
        );
    }
}

#[test]
fn cancellation_at_receipt_stage_rolls_back_source_fts_generation_and_receipt() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = f.admit(&mut writer, &host);
    let before = durable_state(&writer);
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = cancelled.clone();
    writer
        .store()
        .unwrap()
        .db
        .create_scalar_function(
            "test_cancel_writer",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| {
                signal.store(true, Ordering::Release);
                Ok(1i64)
            },
        )
        .unwrap();
    writer.store().unwrap().db.execute_batch(
        "CREATE TEMP TRIGGER cancel_receipt BEFORE INSERT ON main.catalog_command_receipts BEGIN
         SELECT test_cancel_writer();
         SELECT sum(x) FROM (WITH RECURSIVE n(x) AS
         (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<1000000) SELECT x FROM n);
         END;"
    ).unwrap();
    let poll = cancelled.clone();
    let outcome = crate::embedding::with_interrupt_check(
        move || poll.load(Ordering::Acquire),
        || {
            Ok(writer.execute_metadata_patch(
                &handle,
                lease(&host),
                f.request("cancelled"),
                policy(),
                budget(),
            ))
        },
    )
    .unwrap();
    assert!(cancelled.load(Ordering::Acquire));
    assert_eq!(
        code(outcome),
        WriteErrorCode::Budget(EvaluationFailure::Cancelled)
    );
    assert_eq!(durable_state(&writer), before);
    assert!(writer.store().unwrap().db.is_autocommit());
    writer
        .store()
        .unwrap()
        .db
        .execute_batch("DROP TRIGGER cancel_receipt")
        .unwrap();
    assert!(
        writer
            .execute_metadata_patch(
                &handle,
                lease(&host),
                f.request("retry-cancelled"),
                policy(),
                budget()
            )
            .is_ok()
    );
}

#[test]
fn private_vm_ownership_survives_commit_and_drops_before_delivery() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = f.admit(&mut writer, &host);
    let observed = Arc::new(AtomicBool::new(false));
    let record = observed.clone();
    let after_commit = observed.clone();
    writer
        .store()
        .unwrap()
        .db
        .authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
            if after_commit.load(Ordering::Acquire)
                && matches!(context.action, rusqlite::hooks::AuthAction::Read { .. })
            {
                rusqlite::hooks::Authorization::Deny
            } else {
                rusqlite::hooks::Authorization::Allow
            }
        }))
        .unwrap();
    writer
        .store()
        .unwrap()
        .db
        .commit_hook(Some(move || {
            assert!(crate::native_writer_vm::owns_current_thread_for_test());
            assert!(OperationBudget::current().is_some());
            record.store(true, Ordering::Release);
            false
        }))
        .unwrap();
    let result = writer
        .execute_metadata_patch(
            &handle,
            lease(&host),
            f.request("commit-owned"),
            policy(),
            budget(),
        )
        .unwrap();
    assert!(observed.load(Ordering::Acquire));
    assert!(!crate::native_writer_vm::owns_current_thread_for_test());
    assert!(OperationBudget::current().is_none());
    result.deliver(|value| assert_eq!(value["status"], "committed"));
}

#[test]
fn cleanup_bypasses_sticky_cancellation_without_replacing_the_vm_hook() {
    let f = Fixture::new();
    let writer = f.writer();
    let work = budget().operation().unwrap();
    let _budget = work.enter().unwrap();
    let db = &writer.store().unwrap().db;
    let vm = NativeWriterVmAdmission::install(db, &work, &writer.poisoned, &writer.seal).unwrap();
    work.record_failure(EvaluationFailure::Cancelled);
    let sql = "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10000) SELECT sum(x) FROM n";
    {
        let _cleanup = crate::native_writer_vm::cleanup(db).unwrap();
        assert_eq!(
            db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap(),
            50005000
        );
    }
    let error = db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::OperationInterrupted)
    );
    assert_eq!(work.failure(), Some(EvaluationFailure::Cancelled));
    vm.finish().unwrap();
    assert_eq!(
        db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap(),
        50005000
    );
}

#[test]
fn public_store_rebuild_does_not_replace_an_existing_progress_handler() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let f = Fixture::new();
    let store = Store::open(&f.path, true, false).unwrap();
    let polls = Arc::new(AtomicUsize::new(0));
    let observe = polls.clone();
    store
        .db
        .progress_handler(
            1,
            Some(move || {
                observe.fetch_add(1, Ordering::Relaxed);
                false
            }),
        )
        .unwrap();
    crate::catalog_search_index::rebuild(&store, Default::default(), None).unwrap();
    let before = polls.load(Ordering::Relaxed);
    assert!(before > 0);
    store
        .db
        .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
        .unwrap();
    assert!(polls.load(Ordering::Relaxed) > before);
    store.db.progress_handler(0, None::<fn() -> bool>).unwrap();
}

#[test]
fn lock_wait_obeys_the_same_deadline_and_preserves_reusable_writer() {
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = f.admit(&mut writer, &host);
    let before = durable_state(&writer);
    let other = rusqlite::Connection::open(&f.path).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    let short = WriteBudget::new(
        Duration::from_millis(50),
        10000,
        100000,
        16 * 1024 * 1024,
        1000000,
    )
    .unwrap();
    assert_eq!(
        code(writer.execute_metadata_patch(
            &handle,
            lease(&host),
            f.request("lock-timeout"),
            policy(),
            short
        )),
        WriteErrorCode::Budget(EvaluationFailure::Timeout)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    other.execute_batch("ROLLBACK").unwrap();
    assert!(writer.store().unwrap().db.is_autocommit());
    assert_eq!(durable_state(&writer), before);
    assert!(
        writer
            .execute_metadata_patch(
                &handle,
                lease(&host),
                f.request("lock-retry"),
                policy(),
                budget()
            )
            .is_ok()
    );
}

#[test]
fn commit_refusal_discards_prepared_delivery_and_all_durable_writes() {
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = f.admit(&mut writer, &host);
    let before = durable_state(&writer);
    writer
        .store()
        .unwrap()
        .db
        .commit_hook(Some(|| true))
        .unwrap();
    assert_eq!(
        code(writer.execute_metadata_patch(
            &handle,
            lease(&host),
            f.request("commit-refusal"),
            policy(),
            budget()
        )),
        WriteErrorCode::Command(CommandStoreErrorCode::StoreFailure)
    );
    assert_eq!(durable_state(&writer), before);
    assert!(writer.store().unwrap().db.is_autocommit());
    assert!(!crate::native_writer_vm::owns_current_thread_for_test());
    writer
        .store()
        .unwrap()
        .db
        .commit_hook(None::<fn() -> bool>)
        .unwrap();
    assert_eq!(
        value(
            writer
                .execute_metadata_patch(
                    &handle,
                    lease(&host),
                    f.request("commit-refusal"),
                    policy(),
                    budget()
                )
                .unwrap()
        )["status"],
        "committed"
    );
}

#[test]
fn cumulative_byte_budget_can_refuse_delivery_after_receipt_insert_before_commit() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = f.admit(&mut writer, &host);
    let before = durable_state(&writer);
    let receipt_inserted = Arc::new(AtomicBool::new(false));
    let mark = receipt_inserted.clone();
    writer
        .store()
        .unwrap()
        .db
        .create_scalar_function(
            "test_receipt_inserted",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| {
                mark.store(true, Ordering::Release);
                Ok(1i64)
            },
        )
        .unwrap();
    writer.store().unwrap().db.execute_batch(
        "CREATE TEMP TRIGGER observe_receipt AFTER INSERT ON main.catalog_command_receipts BEGIN SELECT test_receipt_inserted(); END"
    ).unwrap();
    let mut refused_delivery = false;
    // Find a budget just beyond source/result/index/receipt work. This avoids a
    // brittle fixed byte count tied to timestamps or metadata JSON formatting.
    for bytes in (256..=64 * 1024).step_by(256) {
        receipt_inserted.store(false, Ordering::Release);
        let bounded =
            WriteBudget::new(Duration::from_secs(5), 10000, 100000, bytes, 1000000).unwrap();
        let error = code(writer.execute_metadata_patch(
            &handle,
            lease(&host),
            f.request("delivery-budget"),
            policy(),
            bounded,
        ));
        assert_eq!(error, WriteErrorCode::Budget(EvaluationFailure::ByteBudget));
        assert_eq!(durable_state(&writer), before);
        if receipt_inserted.load(Ordering::Acquire) {
            refused_delivery = true;
            break;
        }
    }
    assert!(
        refused_delivery,
        "must exercise failure after the receipt was inserted"
    );
    assert_eq!(
        value(
            writer
                .execute_metadata_patch(
                    &handle,
                    lease(&host),
                    f.request("delivery-budget"),
                    policy(),
                    budget()
                )
                .unwrap()
        )["status"],
        "committed"
    );
}

#[test]
fn many_short_statements_share_exact_private_vm_accounting() {
    let f = Fixture::new();
    let writer = f.writer();
    let work = WriteBudget::new(Duration::from_secs(5), 100, 100, 10000, 1000)
        .unwrap()
        .operation()
        .unwrap();
    let _budget = work.enter().unwrap();
    let db = &writer.store().unwrap().db;
    let vm = NativeWriterVmAdmission::install(db, &work, &writer.poisoned, &writer.seal).unwrap();
    let mut interrupted = false;
    for _ in 0..2000 {
        if let Err(error) = db.query_row("SELECT 1", [], |r| r.get::<_, i64>(0)) {
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::OperationInterrupted)
            );
            interrupted = true;
            break;
        }
    }
    assert!(
        interrupted,
        "short statements must not each get a free 999-op interval"
    );
    assert_eq!(work.failure(), Some(EvaluationFailure::WorkBudget));
    vm.finish().unwrap();
}

#[test]
fn cancellation_after_fixed_commit_admission_preserves_durable_success() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = f.admit(&mut writer, &host);
    let cancel = Arc::new(AtomicBool::new(false));
    let committed = cancel.clone();
    writer
        .store()
        .unwrap()
        .db
        .commit_hook(Some(move || {
            committed.store(true, Ordering::Release);
            false
        }))
        .unwrap();
    let poll = cancel.clone();
    let outcome = crate::embedding::with_interrupt_check(
        move || poll.load(Ordering::Acquire),
        || {
            Ok(writer.execute_metadata_patch(
                &handle,
                lease(&host),
                f.request("commit-cancel"),
                policy(),
                budget(),
            ))
        },
    )
    .unwrap()
    .unwrap();
    assert!(cancel.load(Ordering::Acquire));
    assert_eq!(value(outcome)["status"], "committed");
    assert_eq!(
        writer
            .store()
            .unwrap()
            .db
            .query_row(
                "SELECT count(*) FROM catalog_command_receipts WHERE operation_id='commit-cancel'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    // Also proves the fixed-commit allowance did not poison this writer.
    writer.store().unwrap().transaction(|_| Ok(())).unwrap();
}

#[test]
fn flushed_fts_rebuild_fits_the_pinned_fixed_commit_allowance() {
    let f = Fixture::new();
    {
        let store = Store::open(&f.path, true, false).unwrap();
        store.transaction(|_| {
            for index in 0..128 {
                let title = (0..64).map(|word| format!("term{index}z{word}")).collect::<Vec<_>>().join(" ");
                curation::item_put(&store, &json!({"id":format!("bulk-{index}"), "kind":"video", "metadata":{"title":title}}))?;
            }
            Ok(())
        }).unwrap();
    }
    let writer = f.writer();
    let work = WriteBudget::new(
        Duration::from_secs(20),
        10000,
        100000,
        16 * 1024 * 1024,
        10000000,
    )
    .unwrap()
    .operation()
    .unwrap();
    let _budget = work.enter().unwrap();
    let store = writer.store().unwrap();
    let vm =
        NativeWriterVmAdmission::install(&store.db, &work, &writer.poisoned, &writer.seal).unwrap();
    store.begin_transaction().unwrap();
    crate::catalog_search_index::rebuild_connection(&store.db, &work).unwrap();
    let commit = crate::native_writer_vm::prepare_commit(&store.db)
        .unwrap()
        .unwrap();
    store.db.execute_batch("COMMIT").unwrap();
    assert!(
        commit.callbacks() > 0,
        "the actual VM callback remains installed during COMMIT"
    );
    assert!(
        commit.within_prepaid_bound(),
        "actual COMMIT callbacks: {}",
        commit.callbacks()
    );
    drop(commit);
    vm.finish().unwrap();
    assert!(crate::catalog_search_index::status(store).unwrap().ready);
}

#[test]
fn fixed_commit_admission_refuses_insufficient_budget_and_resets_on_unwind() {
    let f = Fixture::new();
    let writer = f.writer();
    let work = WriteBudget::new(Duration::from_secs(5), 100, 100, 10000, 64)
        .unwrap()
        .operation()
        .unwrap();
    {
        let _budget = work.enter().unwrap();
        let store = writer.store().unwrap();
        let vm = NativeWriterVmAdmission::install(&store.db, &work, &writer.poisoned, &writer.seal)
            .unwrap();
        store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(crate::native_writer_vm::prepare_commit(&store.db).is_err());
        assert_eq!(work.failure(), Some(EvaluationFailure::WorkBudget));
        store.rollback_transaction(None).unwrap();
        assert!(store.db.is_autocommit());
        vm.finish().unwrap();
    }
    let work = budget().operation().unwrap();
    let _budget = work.enter().unwrap();
    let db = &writer.store().unwrap().db;
    let vm = NativeWriterVmAdmission::install(db, &work, &writer.poisoned, &writer.seal).unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _commit = crate::native_writer_vm::prepare_commit(db)
            .unwrap()
            .unwrap();
        panic!("unwind fixed commit admission");
    }));
    assert!(panic.is_err());
    work.record_failure(EvaluationFailure::Cancelled);
    assert_eq!(
        db.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
            .unwrap_err()
            .sqlite_error_code(),
        Some(rusqlite::ErrorCode::OperationInterrupted)
    );
    vm.finish().unwrap();
}

#[test]
fn private_writer_rejects_unqualified_future_schema_on_every_store_access() {
    let f = Fixture::new();
    {
        let mut store = Store::open(&f.path, true, false).unwrap();
        // Schema38 is qualified; 39 and later are the unqualified future.
        for schema in [33, 39, 40] {
            store.schema_version = schema;
            assert_eq!(code(qualified_store(&store)), WriteErrorCode::Unsupported);
        }
    }
    let writer = f.writer();
    let other = rusqlite::Connection::open(&f.path).unwrap();
    let actual: u32 = other
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    other.pragma_update(None, "user_version", 39).unwrap();
    assert_eq!(code(writer.store()), WriteErrorCode::Unsupported);
    other.pragma_update(None, "user_version", actual).unwrap();
    assert!(writer.store().is_ok());
}

#[test]
fn prepaid_commit_still_rejects_unclosed_native_deferred_fk_barrier() {
    let f = Fixture::new();
    let writer = f.writer();
    let store = writer.store().unwrap();
    let work = budget().operation().unwrap();
    let _budget = work.enter().unwrap();
    let vm =
        NativeWriterVmAdmission::install(&store.db, &work, &writer.poisoned, &writer.seal).unwrap();
    store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    store.db.execute(
        "INSERT INTO catalog_metadata_permits(item_id, incarnation) SELECT 'a',incarnation FROM catalog_state WHERE id=1", [],
    ).unwrap();
    let commit = crate::native_writer_vm::prepare_commit(&store.db)
        .unwrap()
        .unwrap();
    let error = store.db.execute_batch("COMMIT").unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    );
    assert!(commit.within_prepaid_bound());
    drop(commit);
    store.rollback_transaction(None).unwrap();
    assert!(store.db.is_autocommit());
    vm.finish().unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM catalog_metadata_permits", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
}

#[test]
fn commit_shared_lock_timeout_precedes_durability_and_rolls_back_every_projection() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new();
    let host = host();
    let mut writer = f.writer();
    let handle = f.admit(&mut writer, &host);
    let before = durable_state(&writer);
    let reached_receipt = Arc::new(AtomicBool::new(false));
    let mark = reached_receipt.clone();
    writer
        .store()
        .unwrap()
        .db
        .create_scalar_function(
            "test_commit_lock_receipt",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| {
                mark.store(true, Ordering::Release);
                Ok(1i64)
            },
        )
        .unwrap();
    writer.store().unwrap().db.execute_batch(
        "CREATE TEMP TRIGGER commit_lock_receipt AFTER INSERT ON main.catalog_command_receipts BEGIN SELECT test_commit_lock_receipt(); END"
    ).unwrap();
    let reached_commit_hook = Arc::new(AtomicBool::new(false));
    let committed = reached_commit_hook.clone();
    writer
        .store()
        .unwrap()
        .db
        .commit_hook(Some(move || {
            committed.store(true, Ordering::Release);
            false
        }))
        .unwrap();
    let reader = rusqlite::Connection::open(&f.path).unwrap();
    assert_eq!(
        reader
            .pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    reader.execute_batch("BEGIN").unwrap();
    reader
        .query_row("SELECT count(*) FROM items", [], |r| r.get::<_, i64>(0))
        .unwrap();
    let short = WriteBudget::new(
        Duration::from_millis(500),
        10000,
        100000,
        16 * 1024 * 1024,
        1000000,
    )
    .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        code(writer.execute_metadata_patch(
            &handle,
            lease(&host),
            f.request("commit-shared-lock"),
            policy(),
            short
        )),
        WriteErrorCode::Budget(EvaluationFailure::Timeout)
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        reached_receipt.load(Ordering::Acquire),
        "source, FTS and receipt work must finish before this lock wait"
    );
    assert!(
        !reached_commit_hook.load(Ordering::Acquire),
        "exclusive lock acquisition precedes commit-hook and durability"
    );
    reader.execute_batch("ROLLBACK").unwrap();
    assert_eq!(durable_state(&writer), before);
    assert!(writer.store().unwrap().db.is_autocommit());
    assert_eq!(
        value(
            writer
                .execute_metadata_patch(
                    &handle,
                    lease(&host),
                    f.request("commit-shared-lock"),
                    policy(),
                    budget()
                )
                .unwrap()
        )["status"],
        "committed"
    );
    assert!(reached_commit_hook.load(Ordering::Acquire));
}

// Historical actual35→36 evidence is frozen in slice03. Once latest advances,
// schema36 below is an explicitly fresh fixed-version control, not an upgrade
// receipt. Schema37 cases use actual populated36→37 public-owner upgrades.
macro_rules! schema_case {
    ($name:ident,$version:literal,$case:ident) => {
        #[test]
        fn $name() {
            with_schema($version, $case);
        }
    };
}
schema_case!(
    schema36_typed_patch_commits_source_projection_index_and_receipt_atomically,
    36,
    typed_patch_commits_source_projection_index_and_receipt_atomically
);
schema_case!(
    schema36_write_grant_membership_and_editable_fields_are_independent,
    36,
    write_grant_membership_and_editable_fields_are_independent
);
schema_case!(
    schema36_stale_source_cas_rolls_back_every_durable_projection,
    36,
    stale_source_cas_rolls_back_every_durable_projection
);
schema_case!(
    schema36_source_trigger_vm_exhaustion_rolls_back_and_releases_all_permits,
    36,
    source_trigger_vm_exhaustion_rolls_back_and_releases_all_permits
);
schema_case!(
    schema36_maintenance_rebuild_is_privileged_and_invalidates_old_membership_epoch,
    36,
    maintenance_rebuild_is_privileged_and_invalidates_old_membership_epoch
);
schema_case!(
    schema36_external_nongeneration_commit_invalidates_identical_connection_data_version,
    36,
    external_nongeneration_commit_invalidates_identical_connection_data_version
);
schema_case!(
    schema36_lease_stays_owned_through_commit_and_delivery_after_gate_closes,
    36,
    lease_stays_owned_through_commit_and_delivery_after_gate_closes
);
schema_case!(
    schema36_independent_receipt_authority_replays_after_write_revocation_and_generation_change,
    36,
    independent_receipt_authority_replays_after_write_revocation_and_generation_change
);
schema_case!(
    schema36_maintenance_budget_failure_keeps_previous_index_and_releases_vm,
    36,
    maintenance_budget_failure_keeps_previous_index_and_releases_vm
);
schema_case!(
    schema36_actual_vm_budget_reaches_derived_flush_and_receipt_insertion,
    36,
    actual_vm_budget_reaches_derived_flush_and_receipt_insertion
);
schema_case!(
    schema36_cancellation_at_receipt_stage_rolls_back_source_fts_generation_and_receipt,
    36,
    cancellation_at_receipt_stage_rolls_back_source_fts_generation_and_receipt
);
schema_case!(
    schema36_private_vm_ownership_survives_commit_and_drops_before_delivery,
    36,
    private_vm_ownership_survives_commit_and_drops_before_delivery
);
schema_case!(
    schema36_cleanup_bypasses_sticky_cancellation_without_replacing_the_vm_hook,
    36,
    cleanup_bypasses_sticky_cancellation_without_replacing_the_vm_hook
);
schema_case!(
    schema36_public_store_rebuild_does_not_replace_an_existing_progress_handler,
    36,
    public_store_rebuild_does_not_replace_an_existing_progress_handler
);
schema_case!(
    schema36_lock_wait_obeys_the_same_deadline_and_preserves_reusable_writer,
    36,
    lock_wait_obeys_the_same_deadline_and_preserves_reusable_writer
);
schema_case!(
    schema36_commit_refusal_discards_prepared_delivery_and_all_durable_writes,
    36,
    commit_refusal_discards_prepared_delivery_and_all_durable_writes
);
schema_case!(
    schema36_cumulative_byte_budget_can_refuse_delivery_after_receipt_insert_before_commit,
    36,
    cumulative_byte_budget_can_refuse_delivery_after_receipt_insert_before_commit
);
schema_case!(
    schema36_many_short_statements_share_exact_private_vm_accounting,
    36,
    many_short_statements_share_exact_private_vm_accounting
);
schema_case!(
    schema36_cancellation_after_fixed_commit_admission_preserves_durable_success,
    36,
    cancellation_after_fixed_commit_admission_preserves_durable_success
);
schema_case!(
    schema36_flushed_fts_rebuild_fits_the_pinned_fixed_commit_allowance,
    36,
    flushed_fts_rebuild_fits_the_pinned_fixed_commit_allowance
);
schema_case!(
    schema36_fixed_commit_admission_refuses_insufficient_budget_and_resets_on_unwind,
    36,
    fixed_commit_admission_refuses_insufficient_budget_and_resets_on_unwind
);
schema_case!(
    schema36_private_writer_rejects_unqualified_future_schema_on_every_store_access,
    36,
    private_writer_rejects_unqualified_future_schema_on_every_store_access
);
schema_case!(
    schema36_prepaid_commit_still_rejects_unclosed_native_deferred_fk_barrier,
    36,
    prepaid_commit_still_rejects_unclosed_native_deferred_fk_barrier
);
schema_case!(
    schema36_commit_shared_lock_timeout_precedes_durability_and_rolls_back_every_projection,
    36,
    commit_shared_lock_timeout_precedes_durability_and_rolls_back_every_projection
);
schema_case!(
    schema37_typed_patch_commits_source_projection_index_and_receipt_atomically,
    37,
    typed_patch_commits_source_projection_index_and_receipt_atomically
);
schema_case!(
    schema37_write_grant_membership_and_editable_fields_are_independent,
    37,
    write_grant_membership_and_editable_fields_are_independent
);
schema_case!(
    schema37_stale_source_cas_rolls_back_every_durable_projection,
    37,
    stale_source_cas_rolls_back_every_durable_projection
);
schema_case!(
    schema37_source_trigger_vm_exhaustion_rolls_back_and_releases_all_permits,
    37,
    source_trigger_vm_exhaustion_rolls_back_and_releases_all_permits
);
schema_case!(
    schema37_maintenance_rebuild_is_privileged_and_invalidates_old_membership_epoch,
    37,
    maintenance_rebuild_is_privileged_and_invalidates_old_membership_epoch
);
schema_case!(
    schema37_external_nongeneration_commit_invalidates_identical_connection_data_version,
    37,
    external_nongeneration_commit_invalidates_identical_connection_data_version
);
schema_case!(
    schema37_lease_stays_owned_through_commit_and_delivery_after_gate_closes,
    37,
    lease_stays_owned_through_commit_and_delivery_after_gate_closes
);
schema_case!(
    schema37_independent_receipt_authority_replays_after_write_revocation_and_generation_change,
    37,
    independent_receipt_authority_replays_after_write_revocation_and_generation_change
);
schema_case!(
    schema37_maintenance_budget_failure_keeps_previous_index_and_releases_vm,
    37,
    maintenance_budget_failure_keeps_previous_index_and_releases_vm
);
schema_case!(
    schema37_actual_vm_budget_reaches_derived_flush_and_receipt_insertion,
    37,
    actual_vm_budget_reaches_derived_flush_and_receipt_insertion
);
schema_case!(
    schema37_cancellation_at_receipt_stage_rolls_back_source_fts_generation_and_receipt,
    37,
    cancellation_at_receipt_stage_rolls_back_source_fts_generation_and_receipt
);
schema_case!(
    schema37_private_vm_ownership_survives_commit_and_drops_before_delivery,
    37,
    private_vm_ownership_survives_commit_and_drops_before_delivery
);
schema_case!(
    schema37_cleanup_bypasses_sticky_cancellation_without_replacing_the_vm_hook,
    37,
    cleanup_bypasses_sticky_cancellation_without_replacing_the_vm_hook
);
schema_case!(
    schema37_public_store_rebuild_does_not_replace_an_existing_progress_handler,
    37,
    public_store_rebuild_does_not_replace_an_existing_progress_handler
);
schema_case!(
    schema37_lock_wait_obeys_the_same_deadline_and_preserves_reusable_writer,
    37,
    lock_wait_obeys_the_same_deadline_and_preserves_reusable_writer
);
schema_case!(
    schema37_commit_refusal_discards_prepared_delivery_and_all_durable_writes,
    37,
    commit_refusal_discards_prepared_delivery_and_all_durable_writes
);
schema_case!(
    schema37_cumulative_byte_budget_can_refuse_delivery_after_receipt_insert_before_commit,
    37,
    cumulative_byte_budget_can_refuse_delivery_after_receipt_insert_before_commit
);
schema_case!(
    schema37_many_short_statements_share_exact_private_vm_accounting,
    37,
    many_short_statements_share_exact_private_vm_accounting
);
schema_case!(
    schema37_cancellation_after_fixed_commit_admission_preserves_durable_success,
    37,
    cancellation_after_fixed_commit_admission_preserves_durable_success
);
schema_case!(
    schema37_flushed_fts_rebuild_fits_the_pinned_fixed_commit_allowance,
    37,
    flushed_fts_rebuild_fits_the_pinned_fixed_commit_allowance
);
schema_case!(
    schema37_fixed_commit_admission_refuses_insufficient_budget_and_resets_on_unwind,
    37,
    fixed_commit_admission_refuses_insufficient_budget_and_resets_on_unwind
);
schema_case!(
    schema37_private_writer_rejects_unqualified_future_schema_on_every_store_access,
    37,
    private_writer_rejects_unqualified_future_schema_on_every_store_access
);
schema_case!(
    schema37_prepaid_commit_still_rejects_unclosed_native_deferred_fk_barrier,
    37,
    prepaid_commit_still_rejects_unclosed_native_deferred_fk_barrier
);
schema_case!(
    schema37_commit_shared_lock_timeout_precedes_durability_and_rolls_back_every_projection,
    37,
    commit_shared_lock_timeout_precedes_durability_and_rolls_back_every_projection
);

#[test]
fn schema34_metadata_commit_and_replay_remain_supported_without_fts() {
    with_schema(34, || {
        let f = Fixture::new();
        let h = host();
        let mut writer = f.writer();
        assert_eq!(writer.store().unwrap().schema_version, 34);
        let handle = f.admit(&mut writer, &h);
        let result = value(
            writer
                .execute_metadata_patch(
                    &handle,
                    lease(&h),
                    f.request("schema34"),
                    policy(),
                    budget(),
                )
                .unwrap(),
        );
        assert_eq!(result["status"], "committed");
        let raw: String = writer
            .store()
            .unwrap()
            .db
            .query_row("SELECT metadata FROM items WHERE id='a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(catabolic_core::decode_json(&raw).unwrap()["title"], "New");
        assert_eq!(
            value(
                writer
                    .execute_metadata_patch(
                        &handle,
                        lease(&h),
                        f.request("schema34"),
                        policy(),
                        budget()
                    )
                    .unwrap()
            )["status"],
            "replay"
        );
        assert!(writer.store().unwrap().db.is_autocommit());
    });
}

#[test]
fn schema37_companion_statement_triggers_finish_before_fixed_commit() {
    with_schema(37, || {
        let f = Fixture::new();
        let writer = f.writer();
        let work = WriteBudget::new(
            Duration::from_secs(20),
            10000,
            100000,
            16 * 1024 * 1024,
            1000000,
        )
        .unwrap()
        .operation()
        .unwrap();
        let _budget = work.enter().unwrap();
        let store = writer.store().unwrap();
        let vm = NativeWriterVmAdmission::install(&store.db, &work, &writer.poisoned, &writer.seal)
            .unwrap();
        let before: i64 = store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            !store
                .db
                .prepare("PRAGMA foreign_key_list(native_observation_companions)")
                .unwrap()
                .exists([])
                .unwrap()
        );
        store.begin_transaction().unwrap();
        // Privileged unit-fixture rows exercise the new statement-time guards.
        // They are reservations only: no child exit or cleanup witness is invented.
        for index in 0..128 {
            store.db.execute("INSERT INTO native_observation_companions(reservation_nonce,catalog_id,incarnation,protocol_kind,protocol_version,helper_digest,profile,source,requester,args_digest) SELECT ?,catalog_id,incarnation,'native_observation_no_child',1,?,'default','fixture','qualification',? FROM catalog_state WHERE id=1",rusqlite::params![format!("{index:032x}"),"a".repeat(64),"b".repeat(64)]).unwrap();
        }
        assert_eq!(
            store
                .db
                .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            before + 128
        );
        assert_eq!(store.db.query_row("SELECT count(*) FROM catalog_entity_revisions WHERE entity_kind='native_observation_companions'",[],|r|r.get::<_,i64>(0)).unwrap(),128);
        let commit = crate::native_writer_vm::prepare_commit(&store.db)
            .unwrap()
            .unwrap();
        store.db.execute_batch("COMMIT").unwrap();
        assert!(commit.callbacks() > 0);
        assert!(
            commit.within_prepaid_bound(),
            "actual COMMIT callbacks: {}",
            commit.callbacks()
        );
        drop(commit);
        vm.finish().unwrap();
        assert!(store.db.is_autocommit());
        assert_eq!(
            store
                .db
                .query_row(
                    "SELECT count(*) FROM native_observation_companions WHERE state='reserved'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            128
        );
        assert_eq!(
            store
                .db
                .query_row(
                    "SELECT count(*) FROM native_observation_companions WHERE state='quiescent'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    });
}
