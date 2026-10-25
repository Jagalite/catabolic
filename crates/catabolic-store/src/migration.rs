//! Exact migration resources, verified backups, rehearsal and atomic application.
use crate::{Error, Result, connect, database_path, writer_lock};
use catabolic_core::{encode, encode_with, sha256};
use rusqlite::{
    Connection,
    functions::FunctionFlags,
    hooks::{AuthAction, AuthContext, Authorization},
    params,
    types::ValueRef,
};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::OnceLock,
};
use uuid::Uuid;

pub use crate::schema_versions::{
    FIRST_NATIVE_SCHEMA_VERSION, NATIVE_SCHEMA_VERSION, SCHEMA_VERSION,
};
pub struct Migration {
    pub version: u32,
    pub filename: &'static str,
    pub sql: &'static str,
}
struct NativeSchemaReference {
    version: u32,
    json: &'static str,
    parsed: OnceLock<Value>,
}
include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

/// True only for native versions whose migrations and signatures ship in this build.
pub const fn is_supported_native_schema(version: u32) -> bool {
    version >= FIRST_NATIVE_SCHEMA_VERSION && version <= NATIVE_SCHEMA_VERSION
}
pub fn reference() -> &'static Value {
    static REFERENCE: OnceLock<Value> = OnceLock::new();
    REFERENCE.get_or_init(|| {
        catabolic_core::decode_json(include_str!("../resources/reference.json"))
            .expect("frozen reference resource")
    })
}
/// The signature for the latest opt-in target. Historical validation must use
/// `native_reference_at` with the database's actual version instead.
pub fn native_reference() -> &'static Value {
    native_reference_at(NATIVE_SCHEMA_VERSION).expect("latest native schema signature")
}
/// The immutable signature for one supported native schema; no fallback to latest.
pub fn native_reference_at(version: u32) -> Option<&'static Value> {
    NATIVE_SCHEMA_REFERENCES
        .iter()
        .find(|reference| reference.version == version)
        .map(|reference| {
            reference.parsed.get_or_init(|| {
                catabolic_core::decode_json(reference.json)
                    .expect("native schema reference resource")
            })
        })
}
fn steps(target: u32) -> Result<&'static [Migration]> {
    if (1..=SCHEMA_VERSION).contains(&target) {
        Ok(&MIGRATIONS[..target as usize])
    } else if is_supported_native_schema(target) {
        Ok(&NATIVE_MIGRATIONS[..target as usize])
    } else {
        Err(Error(format!("unsupported migration target {target}")))
    }
}
pub fn description(step: &Migration) -> Value {
    json!({"version":step.version, "filename":step.filename, "checksum":sha256(step.sql)})
}

pub fn execute(db: &Connection, step: &Migration) -> Result<()> {
    let authorizer = crate::catalog_source_succession::authorizer::Guard::for_migration(db)?;
    let query_upgrade = step.version == 15;
    let rebuild = step.version == 16;
    // This migration-only UDF belongs to the original cutover, not the latest target.
    let native_upgrade = step.version == FIRST_NATIVE_SCHEMA_VERSION;
    if native_upgrade {
        db.create_scalar_function(
            "catabolic_catalog_id_v1",
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| {
                let raw: String = ctx.get(0)?;
                Uuid::parse_str(&raw)
                    .map(|id| id.to_string())
                    .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
            },
        )?;
    }
    if query_upgrade {
        db.create_scalar_function(
            "catabolic_query_v1",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| {
                let raw: String = ctx.get(0)?;
                let profile: String = ctx.get(1)?;
                let mut value: Value = catabolic_core::decode_json(&raw)
                    .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?;
                let selection = value
                    .get_mut("selection")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| {
                        rusqlite::Error::UserFunctionError(Box::new(Error(
                            "invalid query selection".into(),
                        )))
                    })?;
                selection.entry("profile").or_insert(Value::String(profile));
                Ok(encode(&value))
            },
        )?;
        db.create_scalar_function(
            "catabolic_sha256_v1",
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| {
                let raw: String = ctx.get(0)?;
                Ok(sha256(raw))
            },
        )?;
    }
    if rebuild {
        db.pragma_update(None, "defer_foreign_keys", true)?;
    }
    authorizer.replace(move |context: AuthContext<'_>| match context.action {
        AuthAction::Pragma {
            pragma_name: "quick_check",
            pragma_value: Some("processing_recipes"),
        } if rebuild => Authorization::Allow,
        AuthAction::Transaction { .. }
        | AuthAction::Savepoint { .. }
        | AuthAction::Attach { .. }
        | AuthAction::Detach { .. }
        | AuthAction::Pragma { .. } => Authorization::Deny,
        AuthAction::Insert {
            table_name: "schema_migrations",
        }
        | AuthAction::Update {
            table_name: "schema_migrations",
            ..
        }
        | AuthAction::Delete {
            table_name: "schema_migrations",
        }
        | AuthAction::DropTable {
            table_name: "schema_migrations",
        }
        | AuthAction::AlterTable {
            table_name: "schema_migrations",
            ..
        } => Authorization::Deny,
        _ => Authorization::Allow,
    })?;
    // None of the frozen SQL owns COMMIT. execute_batch preserves the enclosing transaction.
    let result = db.execute_batch(step.sql);
    authorizer.restore()?;
    if native_upgrade {
        db.remove_function("catabolic_catalog_id_v1", 1)?;
    }
    if query_upgrade {
        db.remove_function("catabolic_query_v1", 2)?;
        db.remove_function("catabolic_sha256_v1", 1)?;
    }
    result.map_err(|e| Error(format!("migration {} failed: {e}", step.filename)))?;
    if step.version == 25 {
        crate::components::backfill(db)?;
    }
    if rebuild {
        if db.prepare("PRAGMA foreign_key_check")?.exists([])? {
            return Err(Error("operation migration violated a foreign key".into()));
        }
        db.pragma_update(None, "defer_foreign_keys", false)?;
    }
    Ok(())
}

pub fn validate(db: &Connection, full: bool) -> Result<Value> {
    validate_typed(db, full).map_err(Into::into)
}

/// Preserve catalog-admission causes while retaining legacy validation diagnostics.
pub fn validate_typed(db: &Connection, full: bool) -> crate::OpenResult<Value> {
    use crate::{OpenError, OpenErrorCode};

    let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if !(1..=SCHEMA_VERSION).contains(&version) && !is_supported_native_schema(version) {
        return Err(OpenError::new(
            OpenErrorCode::UnsupportedSchema,
            Error(format!(
                "unsupported database schema {version}; this application supports upgrades from 1 through {SCHEMA_VERSION}"
            )),
        ));
    }
    let mut statement = db.prepare("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY type,name")?;
    let actual = statement
        .query_map([], |r| {
            let sql: String = r.get(2)?;
            Ok(json!([
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                sql.split_whitespace().collect::<Vec<_>>().join(" ")
            ]))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let expected = if version <= SCHEMA_VERSION {
        &reference()["schemas"][version.to_string()]
    } else {
        native_reference_at(version).expect("supported native schema signature")
    };
    if Value::Array(actual) != *expected {
        return Err(OpenError::invalid(Error(format!(
            "database structure does not match known schema {version}; refusing to modify it"
        ))));
    }
    let identity: String = db
        .query_row("SELECT value FROM meta WHERE key='database_id'", [], |r| {
            r.get(0)
        })
        .map_err(|cause| {
            // The legacy validator used .ok() here. Keep its diagnostic while
            // retaining an I/O, lock, or internal SQLite cause for typed callers.
            let mut error = OpenError::from(cause);
            error.error = Error("database identity is missing or invalid".into());
            error
        })?;
    let database_id = Uuid::parse_str(&identity)
        .map_err(|_| OpenError::invalid(Error("database identity is missing or invalid".into())))?
        .to_string();
    if is_supported_native_schema(version) {
        let recursive: bool =
            db.pragma_query_value(None, "recursive_triggers", |row| row.get(0))?;
        let foreign_keys: bool = db.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
        let read_uncommitted: bool =
            db.pragma_query_value(None, "read_uncommitted", |row| row.get(0))?;
        if !recursive || !foreign_keys || read_uncommitted {
            return Err(OpenError::invalid(Error("native catalog connection requires recursive_triggers=ON, foreign_keys=ON and read_uncommitted=OFF".into())));
        }
        let valid: bool = db.query_row(
            "SELECT count(*)=1 FROM catalog_state WHERE id=1 AND catalog_id=? AND typeof(generation)='integer' AND generation>=0 AND contract_version=1 AND length(CAST(incarnation AS BLOB))=32 AND incarnation NOT GLOB '*[^0-9a-f]*'",
            [&database_id],
            |row| row.get(0),
        )?;
        if !valid {
            return Err(OpenError::invalid(Error(
                "native catalog state is missing or inconsistent with database identity".into(),
            )));
        }
        // A committed permit/barrier would make transaction-scope guards unsafe.
        // These tables are empty at admission and migration boundaries.
        if db.prepare("SELECT 1 FROM catalog_commit_barrier UNION ALL SELECT 1 FROM catalog_mutation_permits LIMIT 1")?.exists([])? {
            return Err(OpenError::invalid(Error("native catalog has an unclosed mutation permit or invalid commit barrier".into())));
        }
    }
    if version >= 34 && db.prepare("SELECT 1 FROM catalog_metadata_permits UNION ALL SELECT 1 FROM catalog_logical_edit_permits LIMIT 1")?.exists([])? {
        return Err(OpenError::invalid(Error("native curation has an unclosed mutation permit".into())));
    }
    if version >= 35 {
        crate::catalog_search_index::validate_closed(db).map_err(OpenError::invalid)?;
    }
    let history = if version >= 2 {
        let mut s = db.prepare("SELECT version,filename,checksum,origin,applied_at FROM schema_migrations ORDER BY version")?;
        let rows = s.query_map([], |r| Ok(json!({"version": r.get::<_,u32>(0)?, "filename":r.get::<_,String>(1)?, "checksum":r.get::<_,String>(2)?, "origin":r.get::<_,String>(3)?, "applied_at":r.get::<_,String>(4)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
        if rows.len() != version as usize
            || rows.iter().enumerate().any(|(i, r)| r["version"] != i + 1)
        {
            return Err(OpenError::invalid(Error(
                "migration history is incomplete or inconsistent".into(),
            )));
        }
        for (r, step) in rows.iter().zip(steps(version).map_err(OpenError::invalid)?) {
            if r["filename"] != step.filename || r["checksum"] != sha256(step.sql) {
                return Err(OpenError::invalid(Error(format!(
                    "released migration was changed or history is damaged: {}",
                    step.filename
                ))));
            }
        }
        rows
    } else {
        vec![]
    };
    if full {
        let mut s = db.prepare("PRAGMA integrity_check")?;
        let rows = s
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows != ["ok"] {
            return Err(OpenError::invalid(Error(format!(
                "database integrity check failed: {rows:?}"
            ))));
        }
        if db.prepare("PRAGMA foreign_key_check")?.exists([])? {
            return Err(OpenError::invalid(Error(
                "database foreign-key check failed".into(),
            )));
        }
        if version >= 3 && db.prepare("SELECT 1 FROM mappings m WHERE m.active=1 AND NOT EXISTS (SELECT 1 FROM item_files a WHERE a.file_id=m.file_id AND a.item_id=m.item_id AND a.active=1) LIMIT 1")?.exists([])? { return Err(OpenError::invalid(Error("an active catalog mapping has no active file identification".into()))); }
        // Schema36's CHECK accepts a NULL cleanup_kind on a terminal row. Every
        // persisted non-owned row must carry the complete NULL-safe witness.
        if version >= 36 && db.prepare("SELECT 1 FROM native_execution_lifetimes WHERE state IS NOT 'owned' AND NOT (state IS 'quiescent' AND cleanup_kind IS 'builtin_no_child_v1' AND child_obligations IS 0 AND verified_retirements IS 0 AND capacity_released IS 1 AND sealed_at IS NOT NULL) LIMIT 1")?.exists([])? { return Err(OpenError::invalid(Error("a native execution lifetime has an incomplete terminal witness".into()))); }
    }
    let pending: i64 = db.query_row("SELECT count(*) FROM journal", [], |r| r.get(0))?;
    let latest = if version > SCHEMA_VERSION {
        NATIVE_SCHEMA_VERSION
    } else {
        SCHEMA_VERSION
    };
    Ok(
        json!({"database_id":database_id, "schema":version, "latest_schema":latest, "history":history, "pending_operations":pending, "pending_migrations":steps(latest).map_err(OpenError::invalid)?.iter().skip(version as usize).map(description).collect::<Vec<_>>()}),
    )
}
pub fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
pub fn value(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => json!(n),
        ValueRef::Real(n) => {
            if n.is_finite() {
                json!(n)
            } else {
                json!({"$float": if n>0. {"Infinity"} else {"-Infinity"}})
            }
        }
        ValueRef::Text(s) => Value::String(String::from_utf8_lossy(s).into_owned()),
        ValueRef::Blob(b) => json!({"$blob":hex(b)}),
    }
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn rows(db: &Connection, sql: &str, args: &[rusqlite::types::Value]) -> Result<Vec<Value>> {
    rows_with_image(db, sql, args, None)
}
// Only the pinned copied-image owner can provide the explicit paid loan. Ordinary
// callers retain the complete foreign-connection charge, even on this same db.
pub(in crate::migration) fn rows_with_image(
    db: &Connection,
    sql: &str,
    args: &[rusqlite::types::Value],
    image: Option<&native_session::allocation::PaidImageSqliteLoan<'_>>,
) -> Result<Vec<Value>> {
    let context = native_session::allocation::current();
    if context.is_some() || image.is_some() {
        // Offline-only actual borrowed preflight. Ordinary migration encoding
        // and query behavior below are unchanged.
        let mut statement = db.prepare(sql)?;
        let count = statement.column_count();
        let mut name_bytes = 0_u64;
        for i in 0..count {
            name_bytes = name_bytes
                .checked_add(statement.column_name(i)?.len() as u64)
                .and_then(|n| n.checked_add(1024))
                .ok_or_else(|| Error("offline row names overflow".into()))?;
        }
        let mut owned = match image {
            Some(image) => image.reserve(db, name_bytes)?,
            None => context.as_ref().expect("ordinary offline allocation").reserve(db, name_bytes)?,
        };
        let names = (0..count)
            .map(|i| statement.column_name(i).map(str::to_owned))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut cursor = statement.query(rusqlite::params_from_iter(args))?;
        let mut result = Vec::new();
        let mut admitted = name_bytes;
        while let Some(row) = cursor.next()? {
            let bytes = native_session::allocation::row_bytes(row, count)?
                .checked_add(name_bytes)
                .ok_or_else(|| Error("offline row storage overflow".into()))?;
            admitted = admitted
                .checked_add(bytes)
                .filter(|n| *n <= 32 * 1024 * 1024)
                .ok_or_else(|| {
                    Error("offline retained metadata rows exceed bounded migration scratch".into())
                })?;
            match image {
                Some(image) => image.grow(&mut owned, db, bytes)?,
                None => owned.grow(db, bytes)?,
            }
            let mut object = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                object.insert(name.clone(), value(row.get_ref(i)?));
            }
            result.push(Value::Object(object));
        }
        return Ok(result);
    }
    let mut s = db.prepare(sql)?;
    let names = s
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    Ok(s.query_map(rusqlite::params_from_iter(args), |r| {
        let mut object = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            object.insert(name.clone(), value(r.get_ref(i)?));
        }
        Ok(Value::Object(object))
    })?
    .collect::<std::result::Result<Vec<_>, _>>()?)
}
pub(crate) fn digest_table(db: &Connection, table: &str, columns: &[String]) -> Result<Value> {
    digest_table_where(db, table, columns, "")
}
// Predicate is an internal static SQL constant, never caller input.
fn digest_table_where(
    db: &Connection,
    table: &str,
    columns: &[String],
    predicate: &str,
) -> Result<Value> {
    digest_table_where_params(db, table, columns, predicate, &[])
}
// Both SQL predicate and table/column identities are fixed by native owners;
// only exact retained command-key values enter bound parameters.
fn digest_table_where_params(
    db: &Connection,
    table: &str,
    columns: &[String],
    predicate: &str,
    args: &[rusqlite::types::Value],
) -> Result<Value> {
    digest_table_with_image(db, table, columns, predicate, args, None)
}
fn digest_table_with_image(
    db: &Connection,
    table: &str,
    columns: &[String],
    predicate: &str,
    args: &[rusqlite::types::Value],
    image: Option<&native_session::allocation::PaidImageSqliteLoan<'_>>,
) -> Result<Value> {
    use sha2::{Digest, Sha256};
    let selected = columns
        .iter()
        .map(|c| identifier(c))
        .collect::<Vec<_>>()
        .join(",");
    let mut s = db.prepare(&format!(
        "SELECT {selected} FROM {} NOT INDEXED {predicate} ORDER BY {selected}",
        identifier(table)
    ))?;
    let mut cursor = s.query(rusqlite::params_from_iter(args))?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    while let Some(row) = cursor.next()? {
        let _row_allocation = match image {
            Some(image) => Some(image.reserve(
                db, native_session::allocation::row_bytes(row, columns.len())?,
            )?),
            None => native_session::allocation::row_reservation(db, row, columns.len())?,
        };
        let mut typed = vec![];
        for i in 0..columns.len() {
            typed.push(match row.get_ref(i)? {
                ValueRef::Null => json!(["NoneType", null]),
                ValueRef::Integer(v) => json!(["int", v]),
                ValueRef::Real(v) => {
                    if !v.is_finite() {
                        return Err(Error(
                            "Out of range float values are not JSON compliant".into(),
                        ));
                    }
                    json!(["float", v])
                }
                ValueRef::Text(v) => json!([
                    "str",
                    std::str::from_utf8(v).map_err(|e| Error(e.to_string()))?
                ]),
                ValueRef::Blob(v) => json!(["bytes", hex(v)]),
            });
        }
        digest.update(encode_with(&Value::Array(typed), true, true).as_bytes());
        digest.update(b"\n");
        count += 1;
    }
    Ok(json!({"columns":columns,"rows":count,"sha256":format!("{:x}",digest.finalize())}))
}
pub fn snapshot(db: &Connection) -> Result<Value> {
    snapshot_with_image(db, None)
}
pub(in crate::migration) fn snapshot_with_image(
    db: &Connection,
    image: Option<&native_session::allocation::PaidImageSqliteLoan<'_>>,
) -> Result<Value> {
    let tables = rows_with_image(
        db,
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        &[],
        image,
    )?;
    let mut result = serde_json::Map::new();
    for table in tables {
        let name = table["name"].as_str().unwrap();
        if name == "schema_migrations" {
            continue;
        }
        let mut s = db.prepare(&format!("PRAGMA table_info({})", identifier(name)))?;
        let columns = s
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        result.insert(name.into(), digest_table_with_image(db, name, &columns, "", &[], image)?);
    }
    Ok(Value::Object(result))
}
pub fn preservation(db: &Connection, before: &Value) -> Result<()> {
    preservation_with_image(db, before, None)
}
fn preservation_with_image(
    db: &Connection,
    before: &Value,
    image: Option<&native_session::allocation::PaidImageSqliteLoan<'_>>,
) -> Result<()> {
    for (table, expected) in before.as_object().expect("snapshot object") {
        let columns = expected["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        if digest_table_with_image(db, table, &columns, "", &[], image)? != *expected {
            return Err(Error(format!(
                "migration changed existing data in {table}; preservation validation failed"
            )));
        }
    }
    Ok(())
}
/// Schema34 adds exactly one baseline kind to the existing invalidation index.
/// Preserve all prior rows byte-for-byte and require the appended baseline to
/// equal the complete immutable legacy-identity snapshot, with revision 1.
/// This exception is deliberately unavailable to other migrations.
fn preservation_schema34(db: &Connection, before: &Value) -> Result<()> {
    let mut unchanged = before.clone();
    let expected = unchanged
        .as_object_mut()
        .expect("snapshot object")
        .remove("catalog_entity_revisions")
        .ok_or_else(|| Error("schema34 requires the native entity index".into()))?;
    if before.get("catalog_legacy_identity_evidence").is_some() {
        return Err(Error(
            "schema34 legacy evidence must be newly introduced".into(),
        ));
    }
    preservation(db, &unchanged)?;
    let columns = expected["columns"]
        .as_array()
        .expect("snapshot columns")
        .iter()
        .map(|v| v.as_str().expect("column name").to_owned())
        .collect::<Vec<_>>();
    if digest_table_where(
        db,
        "catalog_entity_revisions",
        &columns,
        "WHERE entity_kind <> 'catalog_legacy_identity_evidence'",
    )? != expected
    {
        return Err(Error("schema34 changed existing entity index rows".into()));
    }
    let invalid: bool = db.query_row(
        "SELECT EXISTS(
          SELECT entity_key,revision,deleted FROM catalog_entity_revisions WHERE entity_kind='catalog_legacy_identity_evidence'
          EXCEPT SELECT json_array(namespace,value,item_id),1,0 FROM catalog_legacy_identity_evidence
        ) OR EXISTS(
          SELECT json_array(namespace,value,item_id),1,0 FROM catalog_legacy_identity_evidence
          EXCEPT SELECT entity_key,revision,deleted FROM catalog_entity_revisions WHERE entity_kind='catalog_legacy_identity_evidence'
        ) OR EXISTS(
          SELECT namespace,value,item_id FROM catalog_legacy_identity_evidence
          EXCEPT SELECT namespace,value,item_id FROM identities
        ) OR EXISTS(
          SELECT namespace,value,item_id FROM identities
          EXCEPT SELECT namespace,value,item_id FROM catalog_legacy_identity_evidence
        )", [], |r| r.get(0))?;
    if invalid {
        return Err(Error(
            "schema34 legacy evidence baseline is not exact".into(),
        ));
    }
    Ok(())
}

fn record_history(db: &Connection, version: u32, initializing: bool) -> Result<()> {
    if version < 2 {
        return Ok(());
    }
    for step in steps(version)? {
        let origin = if initializing {
            "initialized"
        } else if step.version == 1 {
            "baseline"
        } else {
            "migrated"
        };
        db.execute("INSERT OR IGNORE INTO schema_migrations(version,filename,checksum,origin) VALUES (?,?,?,?)", params![step.version,step.filename,sha256(step.sql),origin])?;
    }
    Ok(())
}
pub type Checkpoint<'a> = dyn Fn(&str, &Connection, u32) -> Result<()> + 'a;
fn apply(
    db: &Connection,
    current: u32,
    target: u32,
    role: &str,
    checkpoint: &Checkpoint<'_>,
) -> Result<Vec<String>> {
    apply_target(db, current, target, role, checkpoint, None)
}
fn apply_target(
    db: &Connection,
    current: u32,
    target: u32,
    role: &str,
    checkpoint: &Checkpoint<'_>,
    closed: Option<&native_session::closed41::Closed41Target>,
) -> Result<Vec<String>> {
    apply_target_with_image(db, current, target, role, checkpoint, closed, None)
}
#[allow(clippy::too_many_arguments)]
fn apply_target_with_image(
    db: &Connection,
    current: u32,
    target: u32,
    role: &str,
    checkpoint: &Checkpoint<'_>,
    closed: Option<&native_session::closed41::Closed41Target>,
    image: Option<&native_session::allocation::PaidImageSqliteLoan<'_>>,
) -> Result<Vec<String>> {
    if image.is_some() && (closed.is_none() || current != 40 || target != 41) {
        return Err(Error("paid image migration requires exact closed40-to41 target".into()));
    }
    if db.is_autocommit() {
        return Err(Error(
            "migration runner requires an explicit transaction".into(),
        ));
    }
    let result = (|| {
        let exact_step;
        let pending = if let Some(owner) = closed {
            if current != 40 || target != 41 {
                return Err(Error("invalid exact offline41 target".into()));
            }
            exact_step = owner.step();
            std::slice::from_ref(&exact_step)
        } else {
            steps(target)?.get(current as usize..).unwrap_or(&[])
        };
        for step in pending {
            let before = snapshot_with_image(db, image)?;
            checkpoint(&format!("{role}:before_migration"), db, step.version)?;
            execute(db, step)?;
            if let Some(owner) = closed {
                owner.record(db)?;
            } else {
                record_history(db, step.version, false)?;
            }
            db.pragma_update(None, "user_version", step.version)?;
            if is_supported_native_schema(step.version) {
                crate::configure_native_connection(db, step.version)?;
            }
            if step.version == 34 {
                preservation_schema34(db, &before)?;
            } else if step.version == 40 {
                schema40_preservation::preservation_schema40(db, &before)?;
            } else {
                preservation_with_image(db, &before, image)?;
            }
            if let Some(owner) = closed {
                owner.validate_with_image(db, image)?;
            } else {
                validate(db, true)?;
            }
            checkpoint(&format!("{role}:after_migration"), db, step.version)?;
        }
        checkpoint(&format!("{role}:before_commit"), db, target)?;
        db.execute_batch("COMMIT")?;
        // COMMIT is durable: a later observer error cannot be called rollback.
        Ok(checkpoint(&format!("{role}:after_commit"), db, target)
            .err()
            .map(|error| format!("{role} committed, but post-commit observer failed: {error}"))
            .into_iter()
            .collect())
    })();
    if result.is_err() && !db.is_autocommit() {
        db.execute_batch("ROLLBACK")?;
    }
    result
}
pub fn initialize(path: &Path) -> Result<Value> {
    initialize_target(path, SCHEMA_VERSION)
}
/// Create a new native catalog explicitly, using the same owner as legacy initialization.
pub fn initialize_native(path: &Path) -> Result<Value> {
    initialize_native_at(path, NATIVE_SCHEMA_VERSION)
}
/// Create a native catalog at a specific supported historical version. Reject
/// unsupported targets before resolving or creating paths, locks, or databases.
/// This is initialization only; it neither downgrades nor replaces an existing file.
pub fn initialize_native_at(path: &Path, target: u32) -> Result<Value> {
    if !is_supported_native_schema(target) {
        return Err(Error(format!(
            "unsupported native schema target {target}; supported targets are {FIRST_NATIVE_SCHEMA_VERSION} through {NATIVE_SCHEMA_VERSION}"
        )));
    }
    initialize_target(path, target)
}
fn initialize_target(path: &Path, target: u32) -> Result<Value> {
    let migrations = steps(target)?;
    let absolute = std::path::absolute(path)?;
    let path = absolute.parent().unwrap().canonicalize()?.join(
        absolute
            .file_name()
            .ok_or_else(|| Error("database name missing".into()))?,
    );
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let _lock = writer_lock(&path)?;
    let db = connect(&path, true)?;
    let id = Uuid::new_v4().to_string();
    db.execute_batch("BEGIN IMMEDIATE")?;
    initialize_connection(&db, migrations, &id)?;
    db.execute_batch("COMMIT")?;
    Ok(json!({"database":path,"database_id":id,"schema":target}))
}
// One initialization algorithm for real create-only files and independent
// closed-resource reference construction. Caller owns the transaction/custody.
fn initialize_connection(db: &Connection, migrations: &[Migration], id: &str) -> Result<()> {
    for step in migrations {
        execute(db, step)?;
        if step.version == 1 {
            db.execute("INSERT INTO meta VALUES ('database_id',?)", [id])?;
            db.execute_batch(
                "INSERT INTO profiles VALUES ('default'); INSERT INTO catalogs VALUES ('global')",
            )?;
        }
        record_history(db, step.version, true)?;
        db.pragma_update(None, "user_version", step.version)?;
        if is_supported_native_schema(step.version) {
            crate::configure_native_connection(db, step.version)?;
        }
    }
    validate(db, true)?;
    Ok(())
}

/// Independent resource-only predecessor, never a target catalog or an admission
/// capability. No filesystem, temporary path, lifecycle lease or candidate flag.
pub(crate) fn closed_native40_reference_connection() -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.pragma_update(None, "temp_store", "MEMORY")?;
    db.pragma_update(None, "foreign_keys", true)?;
    crate::components::install(&db)?;
    crate::catalog_source_succession::install(&db)?;
    db.execute_batch("BEGIN IMMEDIATE")?;
    initialize_connection(&db, steps(40)?, "00000000-0000-0000-0000-000000000001")?;
    db.execute_batch("COMMIT")?;
    Ok(db)
}

pub fn inspect(path: &Path, full: bool) -> Result<Value> {
    let path = database_path(path)?;
    let db = connect(&path, false)?;
    db.execute_batch("BEGIN")?;
    let mut info = validate(&db, full)?;
    info["database"] = json!(path);
    info["safe"] = json!(
        info["pending_operations"] == 0
            || info["pending_migrations"].as_array().unwrap().is_empty()
    );
    Ok(info)
}
pub(crate) fn fsync(path: &Path) -> Result<()> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?
        .sync_all()?;
    Ok(())
}
pub(crate) fn manifest(directory: &Path, value: &Value) -> Result<()> {
    let temporary = directory.join(format!("manifest-{}.tmp", Uuid::new_v4().simple()));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    f.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    fs::rename(temporary, directory.join("manifest.json"))?;
    fsync(directory)
}
pub(crate) fn copy(db: &Connection, path: &Path) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let mut target = connect(path, true)?;
    {
        let backup = rusqlite::backup::Backup::new(db, &mut target)?;
        backup.run_to_completion(256, std::time::Duration::from_millis(10), None)?;
    }
    target.pragma_update(None, "journal_mode", "DELETE")?;
    Ok(())
}
fn backup(
    db: &Connection,
    path: &Path,
    parent: &Path,
    info: &Value,
    before: &Value,
    target: u32,
    approved_plan: Option<&Value>,
) -> Result<(PathBuf, Value)> {
    let parent = std::path::absolute(parent)?;
    // Resolve even non-existent backup parents without treating symlink aliases as safe roots.
    let mut ancestor = parent.as_path();
    let mut suffix = vec![];
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .ok_or_else(|| Error("invalid backup parent".into()))?
                .to_os_string(),
        );
        ancestor = ancestor.parent().unwrap();
    }
    let mut parent = ancestor.canonicalize()?;
    let mut missing = vec![];
    for part in suffix.into_iter().rev() {
        parent.push(part);
        missing.push(parent.clone());
    }
    let directory = parent.join(format!(
        "v{}-to-v{}-{}",
        info["schema"],
        target,
        Uuid::new_v4().simple()
    ));
    for row in rows(db, "SELECT root FROM bindings", &[])? {
        let bound = Path::new(row["root"].as_str().unwrap());
        if directory.starts_with(bound) || bound.starts_with(&directory) {
            return Err(Error(
                "backup directory overlaps a registered source or catalog output".into(),
            ));
        }
    }
    if path.starts_with(&directory) {
        return Err(Error(
            "backup directory cannot contain the active database".into(),
        ));
    }
    // Create each missing ancestor individually and fsync the directory that
    // received its entry, so a verified backup never depends on an unsynced name.
    for created in &missing {
        match std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
            .create(created)
        {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && created.is_dir() => {
            }
            result => result?,
        }
        fsync(
            created
                .parent()
                .expect("created backup ancestor has a parent"),
        )?;
    }
    std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
        .create(&directory)?;
    fsync(&parent)?;
    let partial = directory.join("snapshot.sqlite3.partial");
    copy(db, &partial)?;
    let verify = connect(&partial, false)?;
    let valid = validate(&verify, true)?;
    if valid["database_id"] != info["database_id"] || valid["schema"] != info["schema"] {
        return Err(Error("backup identity or schema mismatch".into()));
    }
    if snapshot(&verify)? != *before {
        return Err(Error(
            "backup contents differ from the source snapshot".into(),
        ));
    }
    drop(verify);
    fsync(&partial)?;
    let saved = directory.join("snapshot.sqlite3");
    fs::rename(partial, &saved)?;
    fsync(&directory)?;
    let mut receipt = json!({"status":"backup_verified","database":path,"database_id":info["database_id"],"from_schema":info["schema"],"to_schema":target,"backup":saved,"sha256":sha256(fs::read(&saved)?),"tables_before":before,"migrations":info["pending_migrations"]});
    if let Some(plan) = approved_plan {
        receipt["plan"] = plan.clone();
        receipt["quiescence"] =
            json!({"operator_asserted":true,"legacy_shutdown_automatically_verified":false});
    }
    manifest(&directory, &receipt)?;
    Ok((directory, receipt))
}
pub fn upgrade(
    path: &Path,
    dry_run: bool,
    parent: Option<&Path>,
    checkpoint: &Checkpoint<'_>,
) -> Result<Value> {
    if dry_run {
        let mut value = inspect(path, true)?;
        value["dry_run"] = json!(true);
        return Ok(value);
    }
    let path = database_path(path)?;
    let _lock = writer_lock(&path)?;
    let db = connect(&path, true)?;
    let mut saved: Option<(PathBuf, Value)> = None;
    let result = (|| {
        db.execute_batch("BEGIN")?;
        let info = validate(&db, true)?;
        let current = info["schema"].as_u64().unwrap() as u32;
        if current > SCHEMA_VERSION {
            return Err(Error(
                "native catalogs cannot be downgraded by the legacy upgrade path".into(),
            ));
        }
        if current == SCHEMA_VERSION {
            return Ok(json!({"database":path,"schema":current,"upgraded":false,"backup":null}));
        }
        if info["pending_operations"] != 0 {
            return Err(Error(
                "recover pending link operations in every profile before upgrading".into(),
            ));
        }
        let before = snapshot(&db)?;
        let default_parent = PathBuf::from(format!("{}.backups", path.display()));
        saved = Some(backup(
            &db,
            &path,
            parent.unwrap_or(&default_parent),
            &info,
            &before,
            SCHEMA_VERSION,
            None,
        )?);
        db.execute_batch("ROLLBACK")?;
        checkpoint("upgrade:after_backup", &db, current)?;
        let (directory, receipt) = saved.as_mut().unwrap();
        {
            let temporary = tempfile::Builder::new()
                .prefix("rehearsal-")
                .tempdir_in(&*directory)?;
            let source = connect(Path::new(receipt["backup"].as_str().unwrap()), false)?;
            let rehearsal_path = temporary.path().join("rehearsal.sqlite3");
            copy(&source, &rehearsal_path)?;
            let rehearsal = connect(&rehearsal_path, true)?;
            rehearsal.execute_batch("BEGIN IMMEDIATE")?;
            apply(&rehearsal, current, SCHEMA_VERSION, "rehearsal", checkpoint)?;
        }
        receipt["status"] = json!("rehearsed");
        manifest(directory, receipt)?;
        checkpoint("upgrade:after_rehearsal", &db, current)?;
        db.execute_batch("BEGIN IMMEDIATE")?;
        let live = validate(&db, true)?;
        if live["schema"] != info["schema"] || snapshot(&db)? != before {
            return Err(Error(
                "database changed after backup; upgrade cancelled, rerun db upgrade".into(),
            ));
        }
        let mut warnings = apply(&db, current, SCHEMA_VERSION, "upgrade", checkpoint)?;
        receipt["status"] = json!("committed");
        if let Err(e) = manifest(directory, receipt) {
            warnings.push(format!(
                "upgrade committed, but backup manifest could not be updated: {e}"
            ));
        }
        Ok(
            json!({"database":path,"database_id":info["database_id"],"from_schema":current,"schema":SCHEMA_VERSION,"upgraded":true,"backup":receipt["backup"],"applied":info["pending_migrations"],"warnings":warnings}),
        )
    })();
    if let Err(e) = &result {
        if !db.is_autocommit() {
            let _ = db.execute_batch("ROLLBACK");
        }
        if let Some((directory, receipt)) = &mut saved {
            receipt["status"] = json!("failed");
            receipt["error"] = json!(e.to_string());
            let _ = manifest(directory, receipt);
        }
    }
    result
}

/// Privileged operator assertions, not facts inferred from a lock or a quiet queue.
/// Old Python processes do not participate in native admission locking. The caller
/// must stop admission, stop/join every reader and worker, and retain exclusive
/// operational ownership of this catalog and its managed outputs through cutover.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeUpgradeQuiescence {
    pub admission_stopped: bool,
    pub readers_and_workers_stopped: bool,
    pub exclusive_operational_ownership: bool,
}
impl NativeUpgradeQuiescence {
    pub(crate) fn require(self) -> Result<()> {
        if !(self.admission_stopped
            && self.readers_and_workers_stopped
            && self.exclusive_operational_ownership)
        {
            return Err(Error("native upgrade requires explicit operator confirmation that admission, all legacy/native readers and workers are stopped and exclusive operational ownership is retained; advisory locks cannot prove this".into()));
        }
        Ok(())
    }
}

/// Conservative cutover inventory across every profile. Expired leases and failed
/// retryable work remain blockers: elapsed time does not prove an external worker
/// stopped, nor that an uncertain filesystem/network effect was recovered.
pub(crate) fn native_pending_work(db: &Connection, version: u32) -> Result<Value> {
    let mut pending = serde_json::Map::new();
    for (introduced, name, predicate) in [
        (1, "journal", "1"),
        (
            6,
            "processing_jobs",
            "state NOT IN ('complete','failed','timeout','changed','cancelled')",
        ),
        (6, "refresh_events", "state NOT IN ('complete','cancelled')"),
        (6, "refresh_dirty", "1"),
        (7, "processing_retry_queue", "1"),
        (
            8,
            "processing_artifacts",
            "state NOT IN ('ready','failed','interrupted')",
        ),
        (
            13,
            "processor_jobs",
            "state NOT IN ('complete','failed','cancelled') OR lease_token IS NOT NULL OR lease_until IS NOT NULL",
        ),
        (13, "processor_attempts", "finished_at IS NULL"),
        (14, "catalog_refresh_queue", "1"),
        (
            17,
            "consumer_deliveries",
            "state NOT IN ('accepted','complete','cancelled') OR lease_token IS NOT NULL OR lease_until IS NOT NULL",
        ),
        (17, "consumer_bindings", "generation>acknowledged"),
        (17, "consumer_attempts", "finished_at IS NULL"),
        (17, "consumer_creations", "state!='complete'"),
        (
            17,
            "notification_deliveries",
            "state NOT IN ('complete','cancelled') OR lease_token IS NOT NULL OR lease_until IS NOT NULL",
        ),
        (18, "remount_guard", "1"),
        (21, "execution_claims", "1"),
        (36, "native_execution_lifetimes", "state='owned'"),
        (37, "native_observation_companions", "state!='quiescent'"),
        (
            22,
            "api_requests",
            "state NOT IN ('ready','failed','cancelled','stale','blocked') OR reserved_bytes!=0",
        ),
        (22, "api_reservations", "1"),
        (
            23,
            "fallback_probe_slots",
            "owner IS NOT NULL OR pid IS NOT NULL",
        ),
        (
            24,
            "observation_jobs",
            "state NOT IN ('complete','unavailable','failed','stale') OR lease_until!=0",
        ),
        (
            24,
            "watchers",
            "active_run IS NOT NULL OR lease_until!=0 OR pending_reaction IS NOT NULL OR pending_generation>completed_generation",
        ),
        (
            24,
            "watcher_runs",
            "completed_at IS NULL OR state NOT IN ('complete','failed','interrupted','waiting')",
        ),
        (
            27,
            "observation_scopes",
            "state NOT IN ('complete','excluded','registry_complete')",
        ),
        (
            28,
            "api_playback_jobs",
            "state NOT IN ('ready','failed','cancelled','stale') OR reserved_bytes!=0",
        ),
    ] {
        if version >= introduced {
            let count: i64 = db.query_row(
                &format!(
                    "SELECT count(*) FROM {} WHERE {predicate}",
                    identifier(name)
                ),
                [],
                |row| row.get(0),
            )?;
            pending.insert(name.into(), json!(count));
        }
    }
    // Claims added to an existing table are inventoried separately so historical
    // schemas are still inspected without querying columns that did not exist.
    if version >= 17 {
        let count: i64 = db.query_row("SELECT count(*) FROM refresh_events WHERE lease_token IS NOT NULL OR lease_until IS NOT NULL", [], |row| row.get(0))?;
        pending.insert("refresh_event_claims".into(), json!(count));
    }
    Ok(Value::Object(pending))
}
pub(crate) fn work_is_settled(pending: &Value) -> bool {
    pending
        .as_object()
        .is_some_and(|entries| entries.values().all(|count| count == 0))
}
fn native_inspection(db: &Connection, path: &Path, full: bool) -> Result<Value> {
    native_inspection_to(db, path, full, NATIVE_SCHEMA_VERSION)
}
fn native_inspection_to(db: &Connection, path: &Path, full: bool, target: u32) -> Result<Value> {
    if !is_supported_native_schema(target) {
        return Err(Error("unsupported exact native target".into()));
    }
    let mut info = validate(db, full)?;
    let version = info["schema"].as_u64().expect("validated schema") as u32;
    info["database"] = json!(path);
    if version > target {
        return Err(Error(
            "native target cannot downgrade current schema".into(),
        ));
    }
    info["latest_schema"] = json!(target);
    info["pending_migrations"] = json!(
        steps(target)?
            .iter()
            .skip(version as usize)
            .map(description)
            .collect::<Vec<_>>()
    );
    info["pending_work"] = native_pending_work(db, version)?;
    info["journal_mode"] =
        json!(db.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))?);
    info["work_settled"] = json!(work_is_settled(&info["pending_work"]));
    info["requires_quiescence"] = json!(version < target);
    info["safe"] = json!(version == target);
    // A read-only inspection never declares cross-process quiescence established.
    info["quiescence_verified"] = json!(false);
    Ok(info)
}
/// Inspect without creating, upgrading, taking ownership or mutating a catalog.
pub fn inspect_native(path: &Path, full: bool) -> Result<Value> {
    let path = database_path(path)?;
    let db = connect(&path, false)?;
    db.execute_batch("BEGIN")?;
    native_inspection(&db, &path, full)
}
fn native_plan(db: &Connection, path: &Path, target: u32) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    let info = native_inspection_to(db, path, true, target)?;
    let metadata = fs::metadata(path)?;
    let sequences = if db
        .prepare("SELECT 1 FROM sqlite_schema WHERE type='table' AND name='sqlite_sequence'")?
        .exists([])?
    {
        rows(
            db,
            "SELECT name,seq FROM sqlite_sequence ORDER BY name",
            &[],
        )?
    } else {
        vec![]
    };
    let mut input = json!({
        "database":path, "file_identity":{"device":metadata.dev(),"inode":metadata.ino()},
        "database_id":info["database_id"], "schema":info["schema"],
        "history":info["history"], "tables":snapshot(db)?, "sequences":sequences,
        "pending_work":info["pending_work"], "journal_mode":info["journal_mode"]
    });
    if matches!(info["schema"].as_u64(), Some(39 | 40)) {
        let (catalog, incarnation): (String, String) = db.query_row(
            "SELECT catalog_id,incarnation FROM catalog_state WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        input["catalog_identity"] = json!({"catalog_id":catalog,"incarnation":incarnation});
        input["qualified_pending_work"] = crate::native_upgrade_inventory::qualified_pending_work(
            db,
            info["schema"].as_u64().unwrap() as u32,
        )?;
    }
    let mut plan = json!({
        "contract":"catabolic-native-upgrade-v1", "from_schema":info["schema"],
        "to_schema":target, "input":input,
        "input_fingerprint":sha256(encode(&input)),
        "migrations":info["pending_migrations"],
        "migration_resources":steps(target)?.iter().map(description).collect::<Vec<_>>(),
        "native_schema_fingerprint":sha256(encode(native_reference_at(target).ok_or_else(|| Error("exact native target reference missing".into()))?)),
        "work_settled":work_is_settled(input.get("qualified_pending_work").unwrap_or(&input["pending_work"])),
        "quiescence_required":info["requires_quiescence"],
        "execution_journal_modes":["delete","truncate","persist"],
        "quiescence_preconditions":["stop admission", "stop and join all legacy/native readers and workers", "retain exclusive operational ownership of the catalog and managed outputs"],
        "rollback":"native forward recovery only; database backup does not reverse filesystem or network effects"
    });
    plan["plan_fingerprint"] = json!(sha256(encode(&plan)));
    Ok(plan)
}
/// A plan binds exact logical inputs, file identity, full history and migration
/// resource checksums. It is read-only and does not authorize or prove quiescence.
pub fn plan_native(path: &Path) -> Result<Value> {
    plan_native_to(path, NATIVE_SCHEMA_VERSION)
}
/// Preserve the immutable Motion89 historical corridor after a later native
/// target ships. This neither plans41 nor bypasses any original plan checks.
pub fn plan_native_39_to_40(path: &Path) -> Result<Value> {
    let plan = plan_native_to(path, 40)?;
    require_historical_40_target(&plan)?;
    Ok(plan)
}
fn plan_native_to(path: &Path, target: u32) -> Result<Value> {
    let path = database_path(path)?;
    let db = connect(&path, false)?;
    db.execute_batch("BEGIN")?;
    native_plan(&db, &path, target)
}
fn require_historical_40_target(plan: &Value) -> Result<()> {
    if !matches!(plan["from_schema"].as_u64(), Some(39 | 40)) || plan["to_schema"] != 40 {
        return Err(Error(
            "historical native owner requires exact39→40 or completed40 target".into(),
        ));
    }
    Ok(())
}
fn require_native_plan(actual: &Value, expected: &Value) -> Result<()> {
    if actual != expected {
        return Err(Error(
            "native upgrade plan or inputs changed; inspect and approve a fresh native plan".into(),
        ));
    }
    if !work_is_settled(
        actual["input"]
            .get("qualified_pending_work")
            .unwrap_or(&actual["input"]["pending_work"]),
    ) {
        return Err(Error(format!(
            "settle or recover all pending catalog work before native upgrade: {}",
            encode(&actual["input"]["pending_work"])
        )));
    }
    Ok(())
}
pub(crate) fn require_native_journal(db: &Connection) -> Result<()> {
    let mode: String = db.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if !matches!(mode.as_str(), "delete" | "truncate" | "persist") {
        return Err(Error(format!(
            "native upgrade requires rollback-journal mode for a real SQLite exclusive lock; found {mode}. Stop all users and perform a separately controlled offline journal-mode conversion, then create a fresh plan"
        )));
    }
    Ok(())
}
/// Explicit native cutover. A caller must present the unchanged plan and assert
/// operational shutdown. The native admission lease and SQLite exclusive write
/// transaction enforce participating-session and active-reader exclusion; neither
/// proves that idle old-engine descriptors or external workers have stopped.
pub fn upgrade_native(
    path: &Path,
    expected_plan: &Value,
    quiescence: NativeUpgradeQuiescence,
    parent: Option<&Path>,
    checkpoint: &Checkpoint<'_>,
) -> Result<Value> {
    upgrade_native_to(
        path,
        expected_plan,
        quiescence,
        parent,
        checkpoint,
        NATIVE_SCHEMA_VERSION,
    )
}
/// Apply only the exact historical target through the same backup, rehearsal,
/// exclusive-lock, full-plan comparison and preservation owner as latest upgrade.
pub fn upgrade_native_39_to_40(
    path: &Path,
    expected_plan: &Value,
    quiescence: NativeUpgradeQuiescence,
    parent: Option<&Path>,
    checkpoint: &Checkpoint<'_>,
) -> Result<Value> {
    require_historical_40_target(expected_plan)?;
    upgrade_native_to(path, expected_plan, quiescence, parent, checkpoint, 40)
}
#[path = "migration_native_session.rs"]
mod native_session;
pub(crate) use native_session::{BootstrapWorkspace, CurrentImageInventory, OfflineAllocationSink, OwnedImageAllocation};
#[path = "migration_offline_bootstrap.rs"]
pub mod offline_bootstrap;
#[cfg(test)]
pub(crate) use native_session::assert_live_pending41_for_test;
pub(crate) use native_session::{
    AdmittedPending41, CompletedCurrentImageAdmission, CompletedSourceAdmission,
    CompletedSourcePreparationAdmission, CompletedSourceWithdrawalAdmission, CurrentImageProof,
    CurrentImageRotationAdmission, OfflineBootstrapComplete, OfflineBootstrapExecution,
    OfflineBootstrapIdentity, OfflineBootstrapSession, OfflineImageAllocation,
};

fn upgrade_native_to(
    path: &Path,
    expected_plan: &Value,
    quiescence: NativeUpgradeQuiescence,
    parent: Option<&Path>,
    checkpoint: &Checkpoint<'_>,
    target: u32,
) -> Result<Value> {
    let session = native_session::RetainedNativeUpgrade::acquire(path, quiescence)?;
    let (_session, result) = session.apply(expected_plan, parent, checkpoint, target)?;
    Ok(result)
}

#[cfg(test)]
#[path = "migration_schema34_tests.rs"]
mod schema34_preservation_tests;

#[path = "migration_schema40.rs"]
mod schema40_preservation;

#[path = "migration_upgrade_reconcile.rs"]
mod migration_upgrade_reconcile;
pub use migration_upgrade_reconcile::{ReconciledNativeUpgrade, with_reconciled_native_upgrade};

/// Descriptive retained-capacity measurement using the offline allocation owner's
/// existing bounded borrowed-tree convention. This does not admit an operation.
pub(crate) fn retained_value_allocation_bytes(value: &serde_json::Value) -> Result<u64> {
    native_session::allocation::value_bytes(value)
}

pub(crate) use native_session::EndpointFreshFacts;

// Fixed source history representation, retaining private allocation custody.
#[path = "catalog_source_succession/history/fixed_rows.rs"]
pub(crate) mod source_history_rows;
