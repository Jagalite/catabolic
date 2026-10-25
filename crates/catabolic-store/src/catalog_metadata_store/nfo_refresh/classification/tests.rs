//! Malformed-registry unit fixtures only. These connections deliberately have no
//! Store admission, receipts or immutable-history triggers and NEVER establish a
//! positive owner grant. Genuine positive ancestry is exercised by the actual
//! import→PUT→refresh tests in catalog_nfo_attribution::refresh::batch_tests.
use super::*;
use crate::operation_budget::{BudgetLimits, OperationBudget, OperationBudgetGuard};

fn policy() -> FieldPolicy {
    FieldPolicy {
        value_type: ValueType::Json,
        nullable: false,
        normalization: Normalization::Exact,
    }
}
fn registered(db: &Connection, schema: &FieldSchema, prior: Option<&str>) {
    db.execute(
        "INSERT INTO catalog_metadata_schemas VALUES('work',?,?,?,?,?)",
        params![
            schema.fingerprint(),
            i64::from(schema.contract_version()),
            serde_json::to_string(&schema_registry::policies(schema)).unwrap(),
            serde_json::to_string(schema.system_fields()).unwrap(),
            prior
        ],
    )
    .unwrap();
}
fn malformed_fixture() -> (
    Connection,
    OperationBudgetGuard,
    FieldSchema,
    FieldSchema,
    ContributionDocument,
) {
    let budget = OperationBudget::new(
        std::time::Duration::from_secs(30),
        BudgetLimits {
            items: 8192,
            values: 8192,
            bytes: MAX_METADATA_SNAPSHOT_BYTES,
            work: 8192,
        },
    )
    .unwrap();
    let guard = budget.enter().unwrap();
    let root =
        FieldSchema::new(vec![(FIELD.into(), policy()), ("title".into(), policy())]).unwrap();
    let historical = FieldSchema::new_v2(
        vec![
            (FIELD.into(), policy()),
            ("title".into(), policy()),
            ("rating".into(), policy()),
        ],
        vec![],
    )
    .unwrap();
    let current = FieldSchema::new_v2(
        vec![("title".into(), policy()), ("rating".into(), policy())],
        vec![(FIELD.into(), policy())],
    )
    .unwrap();
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("PRAGMA user_version=41; CREATE TABLE catalog_metadata_subjects(item_id TEXT PRIMARY KEY,contract_version INTEGER,schema_fingerprint TEXT,policies_json TEXT); CREATE TABLE catalog_metadata_schemas(item_id TEXT,schema_fingerprint TEXT,contract_version INTEGER,policies_json TEXT,system_fields_json TEXT,prior_fingerprint TEXT,PRIMARY KEY(item_id,schema_fingerprint)); CREATE TABLE catalog_metadata_schema_heads(item_id TEXT PRIMARY KEY,schema_fingerprint TEXT); BEGIN IMMEDIATE;").unwrap();
    db.execute(
        "INSERT INTO catalog_metadata_subjects VALUES('work',1,?,?)",
        params![
            root.fingerprint(),
            serde_json::to_string(&schema_registry::policies(&root)).unwrap()
        ],
    )
    .unwrap();
    registered(&db, &root, None);
    registered(&db, &historical, Some(root.fingerprint()));
    registered(&db, &current, Some(historical.fingerprint()));
    db.execute(
        "INSERT INTO catalog_metadata_schema_heads VALUES('work',?)",
        [current.fingerprint()],
    )
    .unwrap();
    let document = ContributionDocument::from_json(
        &historical,
        ContributionKey::new("work", "motion.local", "document").unwrap(),
        SourceKind::Local,
        1,
        r#"{"title":"retained"}"#,
        "{}",
    )
    .unwrap();
    (db, guard, historical, current, document)
}

#[test]
fn malformed_registered_ancestry_cannot_enable_retained_document_reader() {
    for case in 0..8 {
        let (db, _guard, historical, current, document) = malformed_fixture();
        match case {
            0 => {
                db.execute(
                    "DELETE FROM catalog_metadata_schemas WHERE contract_version=1",
                    [],
                )
                .unwrap();
            }
            1 => {
                db.execute("UPDATE catalog_metadata_schemas SET prior_fingerprint='missing' WHERE schema_fingerprint=?",[current.fingerprint()]).unwrap();
            }
            2 => {
                db.execute("UPDATE catalog_metadata_schemas SET prior_fingerprint=? WHERE schema_fingerprint=?",params![current.fingerprint(),historical.fingerprint()]).unwrap();
            }
            3 => {
                let changed = FieldSchema::new_v2(
                    vec![
                        (
                            FIELD.into(),
                            FieldPolicy {
                                nullable: true,
                                ..policy()
                            },
                        ),
                        ("title".into(), policy()),
                        ("rating".into(), policy()),
                    ],
                    vec![],
                )
                .unwrap();
                let prior:String=db.query_row("SELECT prior_fingerprint FROM catalog_metadata_schemas WHERE schema_fingerprint=?",[historical.fingerprint()],|r|r.get(0)).unwrap();
                db.execute(
                    "DELETE FROM catalog_metadata_schemas WHERE schema_fingerprint=?",
                    [historical.fingerprint()],
                )
                .unwrap();
                registered(&db, &changed, Some(&prior));
                db.execute("UPDATE catalog_metadata_schemas SET prior_fingerprint=? WHERE schema_fingerprint=?",params![changed.fingerprint(),current.fingerprint()]).unwrap();
            }
            4 => {
                db.execute(
                    "UPDATE catalog_metadata_subjects SET contract_version=2",
                    [],
                )
                .unwrap();
            }
            5 => {
                db.execute("UPDATE catalog_metadata_schemas SET policies_json='[]' WHERE schema_fingerprint=?",[historical.fingerprint()]).unwrap();
            }
            6 => {
                let extra = FieldSchema::new_v2(vec![(FIELD.into(), policy())], vec![]).unwrap();
                registered(&db, &extra, None);
            }
            _ => {
                db.execute_batch("PRAGMA user_version=40;").unwrap();
            }
        }
        let error = revalidate_retained_document(&db, "work", &current, &historical, &document)
            .unwrap_err();
        assert!(
            matches!(
                error.code,
                MetadataStoreErrorCode::AuthorityDenied
                    | MetadataStoreErrorCode::CorruptState
                    | MetadataStoreErrorCode::Core(_)
            ),
            "case {case}: {error}"
        );
    }
}

#[test]
fn borrowed_registered_parser_has_exact_owner_schema_and_fingerprint_parity() {
    let (db, _guard, _, _, _) = malformed_fixture();
    let mut query=db.prepare("SELECT schema_fingerprint,contract_version,policies_json,system_fields_json FROM catalog_metadata_schemas ORDER BY schema_fingerprint").unwrap();
    let mut rows = query.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let fingerprint = row.get_ref(0).unwrap().as_str().unwrap();
        let raw = row.get_ref(2).unwrap().as_str().unwrap();
        let system = row.get_ref(3).unwrap().as_str().unwrap();
        let borrowed =
            schema_registry::registered_from_parts(row.get(1).unwrap(), fingerprint, raw, system)
                .unwrap();
        assert_eq!(
            borrowed,
            schema_registry::read_registered(&db, "work", fingerprint).unwrap()
        );
    }
}

#[test]
fn registered_text_bounds_precede_utf8_scan() {
    let db = Connection::open_in_memory().unwrap();
    let mut query = db.prepare("SELECT CAST(x'ff' AS TEXT)").unwrap();
    let mut rows = query.query([]).unwrap();
    let row = rows.next().unwrap().unwrap();
    assert!(matches!(
        text(row, 0, 0).unwrap_err().code,
        MetadataStoreErrorCode::Core(MetadataError::SnapshotBudget)
    ));
    assert!(matches!(
        text(row, 0, 1).unwrap_err().code,
        MetadataStoreErrorCode::AuthorityDenied
    ));
}
