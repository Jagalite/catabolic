//! Test-only closed-DDL admission for the composed, unregistered owner candidate.
//! Production open, migration support and host admission remain unchanged.
use crate::{Error, Result, Store, migration};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{os::unix::fs::MetadataExt, path::Path, sync::OnceLock};

// Both the independent reference and installer execute this exact ordered set.
// Component SQL cannot alter the ledger; one composite commitment identifies the
// complete candidate, never a prefix that omits another owner's tables/guards.
fn sql_parts() -> &'static [(&'static str, &'static str)] {
    crate::native41_resources::parts()
}
const NAME: &str = "0041_integration_candidate_test_only.sql";
struct Reference {
    schema: Value,
    ledger: Value,
}
static REFERENCE: OnceLock<Reference> = OnceLock::new();
fn require(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error(format!("candidate41_test_admission: {message}")))
    }
}
fn schema(db: &Connection) -> Result<Value> {
    let mut s = db.prepare("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY type,name")?;
    let rows = s
        .query_map([], |r| {
            let sql: String = r.get(2)?;
            Ok(json!([
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                sql.split_whitespace().collect::<Vec<_>>().join(" ")
            ]))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(json!(rows))
}
fn ledger(db: &Connection, full: bool) -> Result<Value> {
    let sql = if full {
        "SELECT version,filename,checksum,origin,applied_at FROM schema_migrations ORDER BY version"
    } else {
        "SELECT version,filename,checksum FROM schema_migrations ORDER BY version"
    };
    let mut s = db.prepare(sql)?;
    let rows = s
        .query_map([], |r| {
            let mut v = vec![
                json!(r.get::<_, i64>(0)?),
                json!(r.get::<_, String>(1)?),
                json!(r.get::<_, String>(2)?),
            ];
            if full {
                v.push(json!(r.get::<_, String>(3)?));
                v.push(json!(r.get::<_, String>(4)?));
            }
            Ok(Value::Array(v))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(json!(rows))
}
fn apply(db: &Connection) -> Result<()> {
    // Reuse the migration owner's SQL authorizer; draft SQL cannot commit or
    // alter the ledger. Only this exact test helper appends its distinct step.
    for &(_, sql) in sql_parts() {
        migration::execute(
            db,
            &migration::Migration {
                version: 41,
                filename: NAME,
                sql,
            },
        )?;
    }
    let checksum = catabolic_core::sha256(serde_json::to_vec(sql_parts())?);
    db.execute(
        "INSERT INTO schema_migrations(version,filename,checksum,origin) VALUES(41,?,?,'migrated')",
        params![NAME, checksum],
    )?;
    db.pragma_update(None, "user_version", 41)?;
    Ok(())
}
fn reference() -> Result<&'static Reference> {
    if let Some(r) = REFERENCE.get() {
        return Ok(r);
    }
    let independent = crate::native41_resources::reference()?;
    let mut expected_ledger = independent.predecessor_ledger.clone();
    expected_ledger
        .as_array_mut()
        .expect("closed ledger array")
        .push(json!([
            41,
            NAME,
            catabolic_core::sha256(serde_json::to_vec(sql_parts())?)
        ]));
    let r = Reference {
        schema: independent.schema.clone(),
        ledger: expected_ledger,
    };
    let _ = REFERENCE.set(r);
    Ok(REFERENCE.get().expect("reference initialized"))
}
fn settings(db: &Connection) -> Result<()> {
    for (name, expected) in [
        ("foreign_keys", 1_i64),
        ("recursive_triggers", 1),
        ("read_uncommitted", 0),
    ] {
        let actual: i64 = db.pragma_query_value(None, name, |r| r.get(0))?;
        require(actual == expected, "unsafe connection settings")?;
    }
    Ok(())
}
fn closed(db: &Connection) -> Result<()> {
    require(!db.prepare("SELECT 1 FROM catalog_commit_barrier UNION ALL SELECT 1 FROM catalog_mutation_permits UNION ALL SELECT 1 FROM catalog_metadata_permits UNION ALL SELECT 1 FROM catalog_logical_edit_permits UNION ALL SELECT 1 FROM catalog_source_succession_permits UNION ALL SELECT 1 FROM catalog_nfo_source_effect_permits LIMIT 1")?.exists([])?,"unclosed owner permit")?;
    crate::catalog_search_index::validate_closed(db)?;
    require(
        !db.prepare("PRAGMA foreign_key_check")?.exists([])?,
        "foreign key violation",
    )?;
    Ok(())
}
fn validate_db(db: &Connection) -> Result<(String, String)> {
    settings(db)?;
    let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    require(version == 41, "wrong candidate version")?;
    let expected = reference()?;
    require(
        schema(db)? == expected.schema,
        "closed candidate DDL differs",
    )?;
    require(
        ledger(db, false)? == expected.ledger,
        "closed candidate ledger differs",
    )?;
    let id: String = db.query_row("SELECT value FROM meta WHERE key='database_id'", [], |r| {
        r.get(0)
    })?;
    require(uuid::Uuid::parse_str(&id).is_ok(), "invalid catalog ID")?;
    // Use the existing native identity/settings validator with supported40 only
    // for its identity invariant; candidate schema is independently checked above.
    let incarnation = crate::native_incarnation(db, 40, &id)?
        .ok_or_else(|| Error("candidate incarnation missing".into()))?;
    Ok((id, incarnation))
}

/// Closed-resource validation for an independently owned test image. This does
/// not mint filesystem, writer, pair, or restore authority. The caller retains
/// its own stable connection/custody and must establish all image commitments.
pub(crate) fn validate_closed_image(db: &Connection) -> Result<(String, String)> {
    let identity = validate_db(db)?;
    closed(db)?;
    crate::catalog_source_succession::authorizer::install_validated_candidate(db)?;
    Ok(identity)
}

pub(crate) fn install_candidate41_for_test(path: &Path) -> Result<()> {
    let _ = reference()?;
    let path = crate::database_path(path)?;
    let admission = crate::native_admission_exclusive(&path)?;
    let _writer = crate::writer_lock(&path)?;
    let metadata = std::fs::symlink_metadata(&path)?;
    require(
        metadata.is_file() && metadata.nlink() == 1,
        "installation path is not unique",
    )?;
    let pinned = (metadata.dev(), metadata.ino());
    let db = crate::connect(&path, true)?;
    db.execute_batch("BEGIN EXCLUSIVE")?;
    let result = (|| {
        let info = migration::validate(&db, true)?;
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        require(version == 40, "installation requires exact closed40")?;
        require(
            migration::work_is_settled(&migration::native_pending_work(&db, 40)?),
            "native work is not settled",
        )?;
        let before = migration::snapshot(&db)?;
        let history = ledger(&db, true)?;
        apply(&db)?;
        migration::preservation(&db, &before)?;
        let after = ledger(&db, true)?;
        require(
            after
                .as_array()
                .is_some_and(|a| a.len() == 41 && a[..40] == history.as_array().unwrap()[..]),
            "old migration history changed",
        )?;
        let (id, _) = validate_db(&db)?;
        require(info["database_id"] == id, "catalog identity changed")?;
        closed(&db)?;
        admission.validate_path()?;
        let current = std::fs::symlink_metadata(&path)?;
        require(
            current.is_file() && current.nlink() == 1 && pinned == (current.dev(), current.ino()),
            "installation pathname changed",
        )?;
        crate::catalog_source_succession::authorizer::install_validated_candidate(&db)?;
        db.execute_batch("COMMIT")?;
        Ok(())
    })();
    if result.is_err() && !db.is_autocommit() {
        db.execute_batch("ROLLBACK")?;
    }
    result
}

pub(crate) fn open_candidate41_for_test(path: &Path, writable: bool) -> Result<Store> {
    let _ = reference()?;
    let path = crate::database_path(path)?;
    let lock = if writable {
        Some(crate::writer_lock(&path)?)
    } else {
        None
    };
    let admission = crate::native_admission::native_admission_shared(&path)?;
    let metadata = std::fs::symlink_metadata(&path)?;
    require(
        metadata.is_file() && metadata.nlink() == 1,
        "candidate path is not a unique file",
    )?;
    let path_identity = (metadata.dev(), metadata.ino());
    let db = crate::connect(&path, writable)?;
    crate::configure_native_connection(&db, 40)?;
    db.execute_batch("BEGIN")?;
    let (database_id, incarnation) = validate_db(&db)?;
    closed(&db)?;
    if writable {
        db.execute_batch("ROLLBACK")?;
    }
    let store = Store {
        offline_bootstrap: None,
        candidate41: true,
        db,
        path,
        writable,
        schema_version: 41,
        database_id,
        catalog_incarnation: Some(incarnation),
        native_path_identity: Some(path_identity),
        native_admission: Some(admission),
        lock,
        transaction_poisoned: std::cell::Cell::new(false),
        catalog_operation_budget: std::cell::RefCell::new(None),
    };
    validate_identity(&store)?;
    crate::catalog_source_succession::authorizer::install_validated_candidate(&store.db)?;
    Ok(store)
}

pub(super) fn validate_identity(store: &Store) -> Result<()> {
    require(
        store.candidate41 && store.schema_version == 41,
        "not candidate admission",
    )?;
    let metadata = std::fs::symlink_metadata(&store.path)?;
    require(
        metadata.is_file()
            && metadata.nlink() == 1
            && store.native_path_identity == Some((metadata.dev(), metadata.ino())),
        "candidate pathname replaced",
    )?;
    store
        .native_admission
        .as_ref()
        .ok_or_else(|| Error("candidate lifecycle lease missing".into()))?
        .validate_path()?;
    let (id, incarnation) = validate_db(&store.db)?;
    require(
        id == store.database_id
            && store.catalog_incarnation.as_deref() == Some(incarnation.as_str()),
        "candidate lifetime identity changed",
    )
}
impl Store {
    pub(crate) fn validate_candidate41_for_test(&self) -> Result<()> {
        validate_identity(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("candidate.sqlite3");
        migration::initialize_native_at(&path, 40).unwrap();
        (dir, path)
    }
    #[test]
    fn candidate_is_test_only_and_retains_real_lifecycle_and_writer_locks() {
        let (_dir, path) = fixture();
        let ordinary = Store::open(&path, true, false).unwrap();
        assert!(install_candidate41_for_test(&path).is_err());
        drop(ordinary);
        install_candidate41_for_test(&path).unwrap();
        assert!(!migration::is_supported_native_schema(41));
        assert!(Store::open(&path, true, false).is_err());
        let store = open_candidate41_for_test(&path, true).unwrap();
        store.validate_candidate41_for_test().unwrap();
        for table in [
            "catalog_nfo_states",
            "catalog_source_successions",
            "catalog_restore_lineages",
            "catalog_source_scope_heads",
            "catalog_source_scope_events",
            "catalog_native_transition_40_41",
        ] {
            assert!(
                store
                    .db
                    .prepare("SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?")
                    .unwrap()
                    .exists([table])
                    .unwrap(),
                "candidate omitted {table}"
            );
        }
        let checksum: String = store
            .db
            .query_row(
                "SELECT checksum FROM schema_migrations WHERE version=41",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            checksum,
            catabolic_core::sha256(serde_json::to_vec(sql_parts()).unwrap())
        );
        assert_eq!(sql_parts().len(), 7);
        assert!(store.db.prepare("SELECT 1 FROM sqlite_schema WHERE type='view' AND name='catalog_source_current_context_membership'").unwrap().exists([]).unwrap());
        assert!(crate::native_admission_exclusive(&path).is_err());
        assert!(open_candidate41_for_test(&path, true).is_err());
        drop(store);
        let read = open_candidate41_for_test(&path, false).unwrap();
        read.validate_candidate41_for_test().unwrap();
        assert!(!read.writable);
    }
    #[test]
    fn candidate_rechecks_schema_ledger_and_safe_settings() {
        let (_dir, path) = fixture();
        install_candidate41_for_test(&path).unwrap();
        let store = open_candidate41_for_test(&path, true).unwrap();
        store
            .db
            .pragma_update(None, "recursive_triggers", false)
            .unwrap();
        assert!(store.validate_candidate41_for_test().is_err());
        store
            .db
            .pragma_update(None, "recursive_triggers", true)
            .unwrap();
        store.validate_candidate41_for_test().unwrap();
        store
            .db
            .execute_batch(
                "SAVEPOINT membership_guard; DROP VIEW catalog_source_current_context_membership",
            )
            .unwrap();
        assert!(store.validate_candidate41_for_test().is_err());
        store
            .db
            .execute_batch("ROLLBACK TO membership_guard; RELEASE membership_guard")
            .unwrap();
        store.validate_candidate41_for_test().unwrap();
        store.db.execute_batch("SAVEPOINT unfinished_nfo; INSERT INTO catalog_nfo_source_effect_permits(item_id,incarnation,revision,command_catalog_id,command_host_namespace,command_operation_id,outer_digest,raw_mutation_json,mode) SELECT 'unclosed',incarnation,1,catalog_id,'test','unclosed',printf('%064d',0),'{}','publication' FROM catalog_state WHERE id=1;").unwrap();
        assert!(closed(&store.db).is_err());
        store
            .db
            .execute_batch("ROLLBACK TO unfinished_nfo; RELEASE unfinished_nfo")
            .unwrap();
        closed(&store.db).unwrap();
        for trigger in [
            "catalog_source_successions_insert_owner",
            "catalog_restore_lineages_insert",
        ] {
            store.db.execute_batch("SAVEPOINT component_guard").unwrap();
            store
                .db
                .execute_batch(&format!("DROP TRIGGER {trigger}"))
                .unwrap();
            assert!(
                store.validate_candidate41_for_test().is_err(),
                "missing {trigger} accepted"
            );
            store
                .db
                .execute_batch("ROLLBACK TO component_guard; RELEASE component_guard")
                .unwrap();
            store.validate_candidate41_for_test().unwrap();
        }
        store
            .db
            .execute_batch("DROP TRIGGER catalog_nfo_pin_collision_insert")
            .unwrap();
        assert!(store.validate_candidate41_for_test().is_err());
        drop(store);
        assert!(open_candidate41_for_test(&path, true).is_err());
    }
    #[test]
    fn candidate_refuses_corrupt_predecessor_without_installing_any_ddl() {
        let (_dir, path) = fixture();
        let db = crate::connect(&path, true).unwrap();
        db.execute(
            "UPDATE schema_migrations SET checksum=? WHERE version=40",
            ["0".repeat(64)],
        )
        .unwrap();
        drop(db);
        assert!(install_candidate41_for_test(&path).is_err());
        let db = crate::connect(&path, false).unwrap();
        let version: u32 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 40);
        assert!(
            !db.prepare("SELECT 1 FROM sqlite_schema WHERE name='catalog_nfo_states'")
                .unwrap()
                .exists([])
                .unwrap()
        );
    }
}
