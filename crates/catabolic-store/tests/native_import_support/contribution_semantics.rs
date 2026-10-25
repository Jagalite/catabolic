//! Import V2 representations through the existing metadata owner only. The
//! exported revisions below are snapshot evidence, not reconstructed history.
use super::*;
use catabolic_core::catalog_metadata::{
    FieldPolicy, FieldSchema, Normalization, SourceKind, ValueType,
};
use catabolic_store::{
    catalog_import_typed::{
        ImportedContribution, ImportedMetadata, ImportedSourceKind, TypedAuthority, TypedBatch,
    },
    catalog_metadata_store::{
        self as metadata, MetadataAccess, MetadataAuthority, MetadataChange, MetadataMutation,
        MetadataReadAuthority,
    },
};
use rusqlite::{params, types::Value as SqlValue};
use std::collections::BTreeMap;

const SUBJECT: &str = "contribution-item";
const FIELDS: [&str; 9] = [
    "title",
    "legacy_null",
    "local_null",
    "local_text",
    "provider_field",
    "provider_null",
    "scan_field",
    "scan_null",
    "absent",
];

fn schema_fingerprint() -> String {
    FieldSchema::new(
        FIELDS
            .iter()
            .map(|field| {
                (
                    (*field).into(),
                    FieldPolicy {
                        value_type: ValueType::String,
                        nullable: true,
                        normalization: Normalization::TrimAsciiLowercase,
                    },
                )
            })
            .collect(),
    )
    .unwrap()
    .fingerprint()
    .to_owned()
}
fn legacy_fields() -> Value {
    json!({"title":"  Legacy Title  ","legacy_null":null,
        "local_null":"Legacy Null Fallback","local_text":"Legacy Text Fallback"})
}
fn contribution_batch() -> ImportBatch {
    let mut b = batch(SUBJECT);
    b.items[0].legacy_metadata = legacy_fields();
    b.typed = Some(TypedBatch {
        relationships: Vec::new(),
        logical_aliases: Vec::new(),
        tags: Vec::new(),
        metadata: vec![ImportedMetadata {
            contract_version: 1,
            system_fields: vec![],
            item_id: SUBJECT.into(),
            policies: Value::Array(
                FIELDS
                    .iter()
                    .map(|field| {
                        json!({"field":field,
                "value_type":"string","nullable":true,"normalization":"trim_ascii_lowercase"})
                    })
                    .collect(),
            ),
            contributions: vec![
                ImportedContribution {
                    namespace: "editor-local".into(),
                    record_id: "edit-record".into(),
                    source_revision: 29,
                    kind: ImportedSourceKind::Local,
                    fields: json!({"local_null":null,"local_text":"  LOCAL WINS  "}),
                    evidence: json!({"snapshot":"local-29","note":null}),
                },
                ImportedContribution {
                    namespace: "provider-metadata".into(),
                    record_id: "provider-record".into(),
                    source_revision: 71,
                    kind: ImportedSourceKind::Provider,
                    fields: json!({"title":" Provider Title ","legacy_null":"Provider Legacy",
                        "local_null":"Provider Null","local_text":"Provider Text",
                        "provider_field":" Provider Wins ","provider_null":null}),
                    evidence: json!({"snapshot":"provider-71"}),
                },
                ImportedContribution {
                    namespace: "scan-metadata".into(),
                    record_id: "scan-record".into(),
                    source_revision: 103,
                    kind: ImportedSourceKind::Scan,
                    fields: json!({"title":"Scan Title","legacy_null":"Scan Legacy",
                        "local_null":"Scan Null","local_text":"Scan Text",
                        "provider_field":"Scan Provider","provider_null":"Scan Provider Null",
                        "scan_field":"  SCAN WINS  ","scan_null":null}),
                    evidence: json!({"snapshot":"scan-103","observations":["bounded fixture"]}),
                },
                ImportedContribution {
                    namespace: "scan-empty".into(),
                    record_id: "empty-record".into(),
                    source_revision: 4096,
                    kind: ImportedSourceKind::Scan,
                    fields: json!({}),
                    evidence: json!({"snapshot":"empty-4096"}),
                },
            ],
        }],
        ..TypedBatch::default()
    });
    b
}
fn source_grant(namespace: &str, record_id: &str, kind: SourceKind) -> MetadataAuthority {
    MetadataAuthority {
        host_namespace: "import-host".into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::Source {
            namespace: namespace.into(),
            record_id: record_id.into(),
            kind,
        },
    }
}
fn contribution_grants() -> TypedAuthority {
    // These capabilities are supplied independently; never inferred from a batch.
    TypedAuthority {
        existing_items: Default::default(),
        existing_contexts: Default::default(),
        existing_timelines: Default::default(),
        logical_aliases: None,
        relationships: None,
        tag_sources: Vec::new(),
        metadata: vec![
            MetadataAuthority {
                host_namespace: "import-host".into(),
                subject: SUBJECT.into(),
                access: MetadataAccess::Initialize,
            },
            source_grant("editor-local", "edit-record", SourceKind::Local),
            source_grant("provider-metadata", "provider-record", SourceKind::Provider),
            source_grant("scan-metadata", "scan-record", SourceKind::Scan),
            source_grant("scan-empty", "empty-record", SourceKind::Scan),
        ],
        ..TypedAuthority::default()
    }
}
fn expected_mappings() -> Value {
    Value::Array([
        ("editor-local", "edit-record", 29),
        ("provider-metadata", "provider-record", 71),
        ("scan-metadata", "scan-record", 103),
        ("scan-empty", "empty-record", 4096),
    ].into_iter().map(|(namespace, record_id, revision)| json!({
        "owner":"metadata","item_id":SUBJECT,"namespace":namespace,"record_id":record_id,
        "source_revision":revision,"catalog_revision":1,"schema_fingerprint":schema_fingerprint()
    })).collect())
}
fn source_revision(namespace: &str, record_id: &str, kind: &str, revision: i64) -> Value {
    json!({"key":{"subject":SUBJECT,"namespace":namespace,"record_id":record_id},"kind":kind,"revision":revision})
}
fn resolved(value: Value, origin: &str, source: &Value) -> Value {
    json!({"state":"resolved","value":value,"origin":origin,"sources":[source]})
}
fn expected_resolution() -> Value {
    let legacy = source_revision(
        metadata::LEGACY_NAMESPACE,
        metadata::LEGACY_RECORD_ID,
        "legacy_import",
        1,
    );
    let local = source_revision("editor-local", "edit-record", "local", 1);
    let provider = source_revision("provider-metadata", "provider-record", "provider", 1);
    let scan = source_revision("scan-metadata", "scan-record", "scan", 1);
    let empty = source_revision("scan-empty", "empty-record", "scan", 1);
    json!({"contract_version":1,"subject":SUBJECT,"schema_fingerprint":schema_fingerprint(),
        "fields":{
            "title":resolved(json!("  Legacy Title  "),"retained_legacy",&legacy),
            "legacy_null":resolved(Value::Null,"retained_legacy",&legacy),
            "local_null":resolved(Value::Null,"local",&local),
            "local_text":resolved(json!("local wins"),"local",&local),
            "provider_field":resolved(json!("provider wins"),"provider",&provider),
            "provider_null":resolved(Value::Null,"provider",&provider),
            "scan_field":resolved(json!("scan wins"),"scan",&scan),
            "scan_null":resolved(Value::Null,"scan",&scan),
            "absent":{"state":"absent"}
        },"inputs":[legacy,local,provider,empty,scan]})
}
fn expected_effective() -> Value {
    json!({"title":"  Legacy Title  ","legacy_null":null,"local_null":null,
        "local_text":"local wins","provider_field":"provider wins","provider_null":null,
        "scan_field":"scan wins","scan_null":null})
}

// Full rows, not just counts: catches rollback/replay changes to retained input,
// heads, evidence, invalidation revisions, projections, search, receipts and holds.
fn durable_snapshot(
    store: &Store,
    include_receipts: bool,
) -> BTreeMap<&'static str, Vec<Vec<SqlValue>>> {
    let mut tables = vec![
        "items",
        "files",
        "catalog_state",
        "catalog_metadata_subjects",
        "catalog_metadata_history",
        "catalog_metadata_sources",
        "catalog_metadata_projection",
        "catalog_entity_revisions",
        "catalog_metadata_permits",
        "catalog_mutation_permits",
        "catalog_logical_edit_permits",
    ];
    if include_receipts {
        tables.extend(["catalog_command_receipts", "catalog_receipt_holds"]);
    }
    if store.schema_version >= 35 {
        tables.extend([
            "catalog_search_state",
            "catalog_search_values",
            "catalog_search_documents",
            "catalog_search_dirty",
            "catalog_search_fts_data",
            "catalog_search_fts_idx",
            "catalog_search_fts_docsize",
            "catalog_search_fts_config",
        ]);
    }
    tables
        .into_iter()
        .map(|table| {
            let mut statement = store.db.prepare(&format!("SELECT * FROM {table}")).unwrap();
            let columns = statement.column_count();
            let order = (1..=columns)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(",");
            // All-column SQL ordering makes equality independent of physical row order.
            statement = store
                .db
                .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
                .unwrap();
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
fn assert_no_metadata_permits(store: &Store) {
    for table in [
        "catalog_metadata_permits",
        "catalog_mutation_permits",
        "catalog_logical_edit_permits",
    ] {
        let n: i64 = store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(n, 0, "leftover permit in {table}");
    }
    assert!(store.db.is_autocommit());
}
fn read_view(store: &Store) -> metadata::MetadataReadView {
    metadata::read_metadata(
        store,
        &MetadataReadAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store.catalog_incarnation.clone().unwrap(),
            subject: SUBJECT.into(),
            expected_generation: generation(store),
        },
    )
    .unwrap()
}
fn assert_imported_projection(store: &Store) {
    let view = read_view(store);
    assert_eq!(view.metadata, expected_effective());
    assert_eq!(
        serde_json::to_value(&view.resolution).unwrap(),
        expected_resolution()
    );
    assert_eq!(view.schema_fingerprint, schema_fingerprint());
    for field in ["legacy_null", "local_null", "provider_null", "scan_null"] {
        assert_eq!(
            view.metadata.as_object().unwrap().get(field),
            Some(&Value::Null)
        );
    }
    assert!(!view.metadata.as_object().unwrap().contains_key("absent"));
    let (raw, effective, resolution): (String, String, String) = store.db.query_row(
        "SELECT i.metadata,p.effective_json,p.resolution_json FROM items i JOIN catalog_metadata_projection p ON p.item_id=i.id WHERE i.id=?1",
        [SUBJECT], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&raw).unwrap(),
        expected_effective()
    );
    assert_eq!(
        serde_json::from_str::<Value>(&effective).unwrap(),
        expected_effective()
    );
    assert_eq!(
        serde_json::from_str::<Value>(&resolution).unwrap(),
        expected_resolution()
    );
}
fn assert_imported_history(store: &Store, command: &CommandEnvelope) {
    let mut statement = store.db.prepare(
        "SELECT namespace,record_id,source_kind,revision,schema_fingerprint,fields_json,evidence_json,raw_mutation_json,command_catalog_id,command_host_namespace,command_operation_id FROM catalog_metadata_history WHERE item_id=?1 ORDER BY namespace,record_id,revision").unwrap();
    type HistoryRow = (
        String,
        String,
        String,
        i64,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    );
    let rows: Vec<HistoryRow> = statement
        .query_map([SUBJECT], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    // Exactly one legacy initialization and one fresh local revision per supplied
    // source, even though the exported revisions are 29, 71, 103 and 4096.
    assert_eq!(rows.len(), 5);
    let b = contribution_batch();
    let contributions = &b.typed.as_ref().unwrap().metadata[0].contributions;
    for (
        namespace,
        record_id,
        kind,
        revision,
        fingerprint,
        fields,
        evidence,
        raw_mutation,
        catalog,
        host,
        operation,
    ) in rows
    {
        assert_eq!(revision, 1);
        assert_eq!(fingerprint, schema_fingerprint());
        assert_eq!(catalog, store.database_id);
        assert_eq!(host, "import-host");
        assert_eq!(operation, command.key().operation_id());
        let fields: Value = serde_json::from_str(&fields).unwrap();
        let evidence: Value = serde_json::from_str(&evidence).unwrap();
        let mutation: MetadataMutation = serde_json::from_str(&raw_mutation).unwrap();
        if namespace == metadata::LEGACY_NAMESPACE {
            assert_eq!(record_id, metadata::LEGACY_RECORD_ID);
            assert_eq!(kind, "legacy_import");
            assert_eq!(fields, legacy_fields());
            assert_eq!(
                evidence,
                json!({"origin":"unknown","import":"items.metadata"})
            );
            assert!(
                matches!(mutation.change, MetadataChange::Initialize { subject, .. } if subject == SUBJECT)
            );
        } else {
            let original = contributions
                .iter()
                .find(|c| c.namespace == namespace)
                .unwrap();
            assert_eq!(record_id, original.record_id);
            assert_eq!(serde_json::to_value(original.kind).unwrap(), json!(kind));
            let expected_fields: serde_json::Map<String, Value> = original
                .fields
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        match value.as_str() {
                            Some(s) => json!(s.trim().to_ascii_lowercase()),
                            None => value.clone(),
                        },
                    )
                })
                .collect();
            assert_eq!(fields, Value::Object(expected_fields));
            assert_eq!(
                evidence,
                json!({"import_operation_id":command.key().operation_id(),
                "source_revision":original.source_revision,"source_evidence":original.evidence})
            );
            let MetadataChange::Patch {
                subject,
                namespace: patched_namespace,
                record_id: patched_record,
                schema_fingerprint: patched_schema,
                expected_revision,
                edits_json,
                evidence_json,
            } = mutation.change
            else {
                panic!("import contribution was not applied through metadata patch");
            };
            assert_eq!(mutation.contract_version, 1);
            assert_eq!(subject, SUBJECT);
            assert_eq!(patched_namespace, namespace);
            assert_eq!(patched_record, record_id);
            assert_eq!(patched_schema, schema_fingerprint());
            assert_eq!(expected_revision, 0);
            let expected_edits: serde_json::Map<String, Value> = original
                .fields
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| (key.clone(), json!({"op":"set","value":value})))
                .collect();
            assert_eq!(
                serde_json::from_str::<Value>(&edits_json).unwrap(),
                Value::Object(expected_edits)
            );
            assert_eq!(
                serde_json::from_str::<Value>(&evidence_json).unwrap(),
                evidence
            );
        }
        let (head_kind,head_revision,head_fingerprint,head_fields,head_evidence):(String,i64,String,String,String) = store.db.query_row(
            "SELECT source_kind,revision,schema_fingerprint,fields_json,evidence_json FROM catalog_metadata_sources WHERE item_id=?1 AND namespace=?2 AND record_id=?3",
            params![SUBJECT,namespace,record_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
        assert_eq!(
            (head_kind, head_revision, head_fingerprint),
            (kind, 1, schema_fingerprint())
        );
        assert_eq!(serde_json::from_str::<Value>(&head_fields).unwrap(), fields);
        assert_eq!(
            serde_json::from_str::<Value>(&head_evidence).unwrap(),
            evidence
        );
    }
    let (raw_legacy, operation): (String,String) = store.db.query_row(
        "SELECT legacy_raw_json,command_operation_id FROM catalog_metadata_subjects WHERE item_id=?1",
        [SUBJECT], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(raw_legacy, catabolic_core::encode(&legacy_fields()));
    assert_eq!(operation, command.key().operation_id());
    let source_count: i64 = store
        .db
        .query_row("SELECT count(*) FROM catalog_metadata_sources", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(source_count, 5);
    assert_no_metadata_permits(store);
}

#[test]
fn v2_local_scan_null_absence_and_retained_legacy_rehearse_activate_and_replay_native34_through37()
{
    for schema in [34, 35, 36, 37] {
        let f = Fixture::at(schema);
        let baseline = durable_snapshot(&f.store, false);
        let digest = stage_typed(&f, &contribution_batch());
        assert_eq!(durable_snapshot(&f.store, false), baseline);
        let grants = contribution_grants();
        let validation_intent = f.envelope(&validate(&digest));
        let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
        assert_eq!(result(&validated)["contract_version"], 2);
        assert_eq!(result(&validated)["activation_supported"], true);
        assert_eq!(
            result(&validated)["proposed_mappings"],
            json!([{"source_id":format!("source-{SUBJECT}"),"item_id":SUBJECT}])
        );
        assert_eq!(
            result(&validated)["proposed_typed_mappings"],
            expected_mappings()
        );
        assert_eq!(durable_snapshot(&f.store, false), baseline);
        assert_eq!(count(&f.store, "catalog_command_receipts"), 3);
        assert_eq!(count(&f.store, "catalog_receipt_holds"), 3);
        let staged = durable_snapshot(&f.store, true);
        let validation_replay = commands::execute(
            &f.store,
            &validation_intent,
            TrustedCommandAuthority {
                allow_new_commands: false,
                ..base()
            },
            policy(),
        )
        .unwrap();
        assert_eq!(validation_replay.code(), "replay");
        assert_eq!(result(&validation_replay), result(&validated));
        assert_eq!(durable_snapshot(&f.store, true), staged);
        let map = result(&validated)["mapping_digest"].as_str().unwrap();
        let mutation = activate(&digest, map);
        let intent = f.envelope(&mutation);
        let active = activation_grant(&f.store, &digest, map);
        let committed = run_typed(&f, &mutation, &grants, Some(&active)).unwrap();
        assert_eq!(committed.code(), "committed");
        assert_eq!(result(&committed)["typed_results"], expected_mappings());
        assert_eq!(
            result(&committed)["mappings"],
            result(&validated)["proposed_mappings"]
        );
        assert_eq!(
            result(&committed)["mapping_digest"],
            result(&validated)["mapping_digest"]
        );
        assert_imported_projection(&f.store);
        assert_imported_history(&f.store, &intent);
        assert_eq!(count(&f.store, "catalog_command_receipts"), 4);
        assert_eq!(count(&f.store, "catalog_receipt_holds"), 4);
        let receipt_generation: i64 = f
            .store
            .db
            .query_row(
                "SELECT committed_generation FROM catalog_command_receipts WHERE operation_id=?1",
                [intent.key().operation_id()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(receipt_generation, generation(&f.store));
        assert!(receipt_generation > intent.expected_generation());
        let committed_snapshot = durable_snapshot(&f.store, true);
        let Fixture { _root, path, store } = f;
        drop(store);
        let reopened = Store::open(&path, true, false).unwrap();
        let replay = commands::execute(
            &reopened,
            &intent,
            TrustedCommandAuthority {
                allow_new_commands: false,
                ..base()
            },
            policy(),
        )
        .unwrap();
        assert_eq!(replay.code(), "replay");
        assert_eq!(result(&replay), result(&committed));
        assert_eq!(durable_snapshot(&reopened, true), committed_snapshot);
        assert_imported_projection(&reopened);
        assert_imported_history(&reopened, &intent);
        drop(reopened);
        drop(_root);
    }
}

#[test]
fn v2_local_scan_invalid_owner_values_and_exact_grants_refuse_without_visibility_native34_through37()
 {
    for schema in [34, 35, 36, 37] {
        for invalid_case in [
            "null_not_allowed",
            "scan_unknown_field",
            "second_local",
            "local_namespace",
            "local_record",
            "scan_kind",
            "scan_subject",
        ] {
            let f = Fixture::at(schema);
            let mut b = contribution_batch();
            let mut grants = contribution_grants();
            let m = &mut b.typed.as_mut().unwrap().metadata[0];
            let expected = match invalid_case {
                "null_not_allowed" => {
                    let policy = m
                        .policies
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|p| p["field"] == "local_null")
                        .unwrap();
                    policy["nullable"] = json!(false);
                    ImportErrorCode::SnapshotConflict
                }
                "scan_unknown_field" => {
                    m.contributions[2].fields["unknown"] = json!("invalid");
                    ImportErrorCode::SnapshotConflict
                }
                "second_local" => {
                    m.contributions.push(ImportedContribution {
                        namespace: "second-local".into(),
                        record_id: "record".into(),
                        source_revision: 8,
                        kind: ImportedSourceKind::Local,
                        fields: json!({}),
                        evidence: json!({}),
                    });
                    grants
                        .metadata
                        .push(source_grant("second-local", "record", SourceKind::Local));
                    ImportErrorCode::SnapshotConflict
                }
                "local_namespace" => {
                    grants.metadata[1] =
                        source_grant("wrong-local", "edit-record", SourceKind::Local);
                    ImportErrorCode::AuthorityDenied
                }
                "local_record" => {
                    grants.metadata[1] =
                        source_grant("editor-local", "wrong-record", SourceKind::Local);
                    ImportErrorCode::AuthorityDenied
                }
                "scan_kind" => {
                    grants.metadata[3] =
                        source_grant("scan-metadata", "scan-record", SourceKind::Provider);
                    ImportErrorCode::AuthorityDenied
                }
                "scan_subject" => {
                    grants.metadata[3].subject = "another-item".into();
                    ImportErrorCode::AuthorityDenied
                }
                _ => unreachable!(),
            };
            let digest = stage_typed(&f, &b);
            let before = durable_snapshot(&f.store, true);
            let error = run_typed(&f, &validate(&digest), &grants, None).unwrap_err();
            assert_eq!(
                error.code,
                CommandStoreErrorCode::Import(expected),
                "schema {schema}: {invalid_case}"
            );
            assert_eq!(
                durable_snapshot(&f.store, true),
                before,
                "schema {schema}: {invalid_case}"
            );
            assert_eq!(count(&f.store, "items"), 0);
            assert_eq!(count(&f.store, "catalog_command_receipts"), 2);
            assert_eq!(count(&f.store, "catalog_receipt_holds"), 2);
            assert_no_metadata_permits(&f.store);
        }
    }
    for kind in [
        ImportedSourceKind::Local,
        ImportedSourceKind::Provider,
        ImportedSourceKind::Scan,
    ] {
        let mut b = contribution_batch();
        let contribution = &mut b.typed.as_mut().unwrap().metadata[0].contributions[0];
        contribution.kind = kind;
        contribution.source_revision = 0;
        assert_eq!(
            import::describe_batch(&b).unwrap_err().code,
            ImportErrorCode::InvalidInput
        );
    }
}

#[test]
fn v2_metadata_import_stale_activation_refuses_even_with_fresh_envelope_native34_through37() {
    for schema in [34, 35, 36, 37] {
        let f = Fixture::at(schema);
        let digest = stage_typed(&f, &contribution_batch());
        let grants = contribution_grants();
        let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
        let map = result(&validated)["mapping_digest"].as_str().unwrap();
        let stale_intent = f.envelope(&activate(&digest, map));
        let original_gate = activation_grant(&f.store, &digest, map);
        catabolic_store::curation::item_put(
            &f.store,
            &json!({"id":"unrelated","kind":"video","metadata":{}}),
        )
        .unwrap();
        let before = durable_snapshot(&f.store, true);
        let stale = commands::execute_with_authority(
            &f.store,
            &stale_intent,
            CatalogAuthority::new(base())
                .with_imports(&grant())
                .with_import_typed(&grants)
                .with_import_activation(&original_gate),
            policy(),
        )
        .unwrap();
        assert_eq!(stale, CommandOutcome::StaleGeneration);
        let fresh_gate = activation_grant(&f.store, &digest, map);
        assert_eq!(
            run_typed(&f, &activate(&digest, map), &grants, Some(&fresh_gate))
                .unwrap_err()
                .code,
            CommandStoreErrorCode::Import(ImportErrorCode::StaleValidation)
        );
        assert_eq!(durable_snapshot(&f.store, true), before);
        assert_no_metadata_permits(&f.store);
    }
}

#[test]
fn v2_local_scan_late_receipt_and_hold_failures_roll_back_complete_import_native34_through37() {
    for schema in [34, 35, 36, 37] {
        for target in ["catalog_command_receipts", "catalog_receipt_holds"] {
            let f = Fixture::at(schema);
            let digest = stage_typed(&f, &contribution_batch());
            let grants = contribution_grants();
            let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
            let map = result(&validated)["mapping_digest"].as_str().unwrap();
            let mutation = activate(&digest, map);
            let intent = f.envelope(&mutation);
            let gate = activation_grant(&f.store, &digest, map);
            let before = durable_snapshot(&f.store, true);
            // Both hooks are after all metadata owners and the search flush. A
            // distinct diagnostic proves the test actually reached that point.
            f.store
                .db
                .execute_batch(&format!(
                    "CREATE TEMP TRIGGER fail_contribution_commit BEFORE INSERT ON {target}
                WHEN NEW.operation_id LIKE '%/activate' BEGIN
                SELECT CASE WHEN (SELECT count(*) FROM catalog_metadata_history)!=5
                    OR (SELECT count(*) FROM catalog_metadata_sources)!=5
                    OR (SELECT count(*) FROM catalog_metadata_projection)!=1
                    THEN RAISE(ABORT,'metadata owners not complete') END;
                SELECT RAISE(ABORT,'contribution late commit failure'); END;"
                ))
                .unwrap();
            let error = run_typed(&f, &mutation, &grants, Some(&gate)).unwrap_err();
            assert_eq!(error.code, CommandStoreErrorCode::StoreFailure);
            assert!(
                error
                    .to_string()
                    .contains("contribution late commit failure"),
                "{error}"
            );
            assert_eq!(
                durable_snapshot(&f.store, true),
                before,
                "schema {schema}: {target}"
            );
            assert_no_metadata_permits(&f.store);
            f.store
                .db
                .execute_batch("DROP TRIGGER fail_contribution_commit")
                .unwrap();
            let committed = run_typed(&f, &mutation, &grants, Some(&gate)).unwrap();
            assert_eq!(committed.code(), "committed");
            assert_eq!(result(&committed)["typed_results"], expected_mappings());
            assert_imported_projection(&f.store);
            assert_imported_history(&f.store, &intent);
            assert_eq!(count(&f.store, "catalog_command_receipts"), 4);
            assert_eq!(count(&f.store, "catalog_receipt_holds"), 4);
        }
    }
}

#[test]
fn v2_imported_local_revision_cas_and_removal_restore_retained_legacy_native34_through37() {
    for schema in [34, 35, 36, 37] {
        let f = Fixture::at(schema);
        let digest = stage_typed(&f, &contribution_batch());
        let grants = contribution_grants();
        let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
        let map = result(&validated)["mapping_digest"].as_str().unwrap();
        let gate = activation_grant(&f.store, &digest, map);
        run_typed(&f, &activate(&digest, map), &grants, Some(&gate)).unwrap();
        let local = source_grant("editor-local", "edit-record", SourceKind::Local);
        let patch = |expected_revision| MetadataMutation {
            contract_version: 1,
            change: MetadataChange::Patch {
                subject: SUBJECT.into(),
                namespace: "editor-local".into(),
                record_id: "edit-record".into(),
                schema_fingerprint: schema_fingerprint(),
                expected_revision,
                edits_json: json!({"local_null":{"op":"remove"},"local_text":{"op":"remove"}})
                    .to_string(),
                evidence_json: json!({"fixture":"explicit local removal after import"}).to_string(),
            },
        };
        let command = |id: &str, expected_revision| {
            CommandEnvelope::new(
                1,
                CommandKey::new(&f.store.database_id, "import-host", id).unwrap(),
                f.store.catalog_incarnation.clone().unwrap(),
                generation(&f.store),
                &json!({"operation":"metadata","mutation":patch(expected_revision)}).to_string(),
            )
            .unwrap()
        };
        let before = durable_snapshot(&f.store, true);
        // Exported revision 29 is evidence; it cannot be used as the new head's CAS.
        for expected in [0, 29] {
            let refused = command(&format!("wrong-imported-cas-{expected}"), expected);
            assert_eq!(
                commands::execute_with_authority(
                    &f.store,
                    &refused,
                    CatalogAuthority::new(base()).with_metadata(&local),
                    policy()
                )
                .unwrap_err()
                .code,
                CommandStoreErrorCode::OperationConflict
            );
            assert_eq!(durable_snapshot(&f.store, true), before);
        }
        let removed = commands::execute_with_authority(
            &f.store,
            &command("remove-imported-local", 1),
            CatalogAuthority::new(base()).with_metadata(&local),
            policy(),
        )
        .unwrap();
        assert_eq!(removed.code(), "committed");
        let view = read_view(&f.store);
        let mut effective = expected_effective();
        effective["local_null"] = json!("Legacy Null Fallback");
        effective["local_text"] = json!("Legacy Text Fallback");
        assert_eq!(view.metadata, effective);
        let mut resolution = expected_resolution();
        let legacy = source_revision(
            metadata::LEGACY_NAMESPACE,
            metadata::LEGACY_RECORD_ID,
            "legacy_import",
            1,
        );
        resolution["fields"]["local_null"] =
            resolved(json!("Legacy Null Fallback"), "retained_legacy", &legacy);
        resolution["fields"]["local_text"] =
            resolved(json!("Legacy Text Fallback"), "retained_legacy", &legacy);
        resolution["inputs"][1]["revision"] = json!(2);
        assert_eq!(serde_json::to_value(view.resolution).unwrap(), resolution);
        let (revision,fields):(i64,String) = f.store.db.query_row(
            "SELECT revision,fields_json FROM catalog_metadata_sources WHERE item_id=?1 AND namespace='editor-local'",
            [SUBJECT],|r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(revision, 2);
        assert_eq!(serde_json::from_str::<Value>(&fields).unwrap(), json!({}));
        let retained:Vec<(i64,String)> = f.store.db.prepare(
            "SELECT revision,fields_json FROM catalog_metadata_history WHERE item_id=?1 AND namespace='editor-local' ORDER BY revision").unwrap()
            .query_map([SUBJECT],|r| Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(retained.len(), 2);
        assert_eq!(retained[0].0, 1);
        assert_eq!(
            serde_json::from_str::<Value>(&retained[0].1).unwrap(),
            json!({"local_null":null,"local_text":"local wins"})
        );
        assert_eq!(retained[1], (2, fields));
        let after = durable_snapshot(&f.store, true);
        let stale = command("stale-after-local-removal", 1);
        assert_eq!(
            commands::execute_with_authority(
                &f.store,
                &stale,
                CatalogAuthority::new(base()).with_metadata(&local),
                policy()
            )
            .unwrap_err()
            .code,
            CommandStoreErrorCode::OperationConflict
        );
        assert_eq!(durable_snapshot(&f.store, true), after);
        assert_no_metadata_permits(&f.store);
    }
}

#[test]
fn native40_explicit_metadata_v2_imports100_fields_per_source_and_retains_v1_wire_shape() {
    let old = contribution_batch();
    let old_wire = serde_json::to_value(&old).unwrap();
    assert!(
        old_wire["typed"]["metadata"][0]
            .get("contract_version")
            .is_none()
    );
    assert!(
        old_wire["typed"]["metadata"][0]
            .get("system_fields")
            .is_none()
    );
    let f = Fixture::at(40);
    let mut batch = contribution_batch();
    batch.items[0].legacy_metadata = json!({});
    let metadata = &mut batch.typed.as_mut().unwrap().metadata[0];
    metadata.contract_version = 2;
    metadata.system_fields = vec!["motion_catalog".into()];
    let mut policies = Vec::new();
    let mut contributions = Vec::new();
    let mut grants = TypedAuthority::default();
    grants.metadata.push(MetadataAuthority {
        host_namespace: "import-host".into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::InitializeV2 {
            system_fields: ["motion_catalog".into()].into(),
        },
    });
    for (namespace, prefix) in [("motion.local", "a"), ("motion.provider", "b")] {
        let fields: serde_json::Map<String, Value> = (0..100)
            .map(|i| {
                (
                    if i == 0 {
                        format!("{prefix}{}", "x".repeat(99))
                    } else {
                        format!("{prefix}{i}")
                    },
                    json!(i),
                )
            })
            .collect();
        policies.extend(fields.keys().map(|field|json!({"field":field,"value_type":"json","nullable":false,"normalization":"exact"})));
        contributions.push(ImportedContribution {
            namespace: namespace.into(),
            record_id: "document".into(),
            source_revision: 73,
            kind: if namespace == "motion.local" {
                ImportedSourceKind::Local
            } else {
                ImportedSourceKind::Provider
            },
            fields: Value::Object(fields),
            evidence: json!({"fixture":"v2"}),
        });
        grants.metadata.push(MetadataAuthority {
            host_namespace: "import-host".into(),
            subject: SUBJECT.into(),
            access: MetadataAccess::SourceV2 {
                namespace: namespace.into(),
                record_id: "document".into(),
                kind: if namespace == "motion.local" {
                    SourceKind::Local
                } else {
                    SourceKind::Provider
                },
                system_fields: Default::default(),
            },
        });
    }
    policies.push(json!({"field":"motion_catalog","value_type":"json","nullable":false,"normalization":"exact"}));
    contributions.push(ImportedContribution {
        namespace: "motion_catalog".into(),
        record_id: "projection".into(),
        source_revision: 1,
        kind: ImportedSourceKind::Scan,
        fields: json!({"motion_catalog":{"contract_version":1,"item_id":SUBJECT,"kind":"movie"}}),
        evidence: json!({"owner":"fixture"}),
    });
    grants.metadata.push(MetadataAuthority {
        host_namespace: "import-host".into(),
        subject: SUBJECT.into(),
        access: MetadataAccess::SourceV2 {
            namespace: "motion_catalog".into(),
            record_id: "projection".into(),
            kind: SourceKind::Scan,
            system_fields: ["motion_catalog".into()].into(),
        },
    });
    metadata.policies = Value::Array(policies);
    metadata.contributions = contributions;
    let digest = stage_typed(&f, &batch);
    let validated = run_typed(&f, &validate(&digest), &grants, None).unwrap();
    let mapping = result(&validated)["mapping_digest"].as_str().unwrap();
    let activation = activation_grant(&f.store, &digest, mapping);
    let mutation = activate(&digest, mapping);
    let command = f.envelope(&mutation);
    run_typed(&f, &mutation, &grants, Some(&activation)).unwrap();
    let snapshot = metadata::read_contributions(
        &f.store,
        &MetadataReadAuthority {
            catalog_id: f.store.database_id.clone(),
            incarnation: f.store.catalog_incarnation.clone().unwrap(),
            subject: SUBJECT.into(),
            expected_generation: generation(&f.store),
        },
    )
    .unwrap();
    assert_eq!(
        snapshot
            .sources
            .iter()
            .find(|s| s.namespace == "motion.local")
            .unwrap()
            .fields
            .len(),
        100
    );
    assert_eq!(
        snapshot
            .sources
            .iter()
            .find(|s| s.namespace == "motion.provider")
            .unwrap()
            .fields
            .len(),
        100
    );
    assert!(matches!(
        commands::execute(&f.store, &command, base(), policy()).unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_no_metadata_permits(&f.store);
}
