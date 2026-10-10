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

pub const SCHEMA_VERSION: u32 = 32;
pub struct Migration {
    pub version: u32,
    pub filename: &'static str,
    pub sql: &'static str,
}
include!(concat!(env!("OUT_DIR"), "/migrations.rs"));
pub fn reference() -> &'static Value {
    static REFERENCE: OnceLock<Value> = OnceLock::new();
    REFERENCE.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/reference.json"))
            .expect("frozen reference resource")
    })
}
pub fn description(step: &Migration) -> Value {
    json!({"version":step.version, "filename":step.filename, "checksum":sha256(step.sql)})
}

pub fn execute(db: &Connection, step: &Migration) -> Result<()> {
    let query_upgrade = step.version == 15;
    let rebuild = step.version == 16;
    if query_upgrade {
        db.create_scalar_function(
            "catabolic_query_v1",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| {
                let raw: String = ctx.get(0)?;
                let profile: String = ctx.get(1)?;
                let mut value: Value = serde_json::from_str(&raw)
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
    db.authorizer(Some(move |context: AuthContext<'_>| match context.action {
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
    }))?;
    // None of the frozen SQL owns COMMIT. execute_batch preserves the enclosing transaction.
    let result = db.execute_batch(step.sql);
    db.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
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
    let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if !(1..=SCHEMA_VERSION).contains(&version) {
        return Err(Error(format!(
            "unsupported database schema {version}; this application supports upgrades from 1 through {SCHEMA_VERSION}"
        )));
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
    if Value::Array(actual) != reference()["schemas"][version.to_string()] {
        return Err(Error(format!(
            "database structure does not match known schema {version}; refusing to modify it"
        )));
    }
    let identity: Option<String> = db
        .query_row("SELECT value FROM meta WHERE key='database_id'", [], |r| {
            r.get(0)
        })
        .ok();
    let database_id = identity
        .and_then(|s| Uuid::parse_str(&s).ok())
        .ok_or_else(|| Error("database identity is missing or invalid".into()))?
        .to_string();
    let history = if version >= 2 {
        let mut s = db.prepare("SELECT version,filename,checksum,origin,applied_at FROM schema_migrations ORDER BY version")?;
        let rows = s.query_map([], |r| Ok(json!({"version": r.get::<_,u32>(0)?, "filename":r.get::<_,String>(1)?, "checksum":r.get::<_,String>(2)?, "origin":r.get::<_,String>(3)?, "applied_at":r.get::<_,String>(4)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
        if rows.len() != version as usize
            || rows.iter().enumerate().any(|(i, r)| r["version"] != i + 1)
        {
            return Err(Error(
                "migration history is incomplete or inconsistent".into(),
            ));
        }
        for (r, step) in rows.iter().zip(MIGRATIONS) {
            if r["filename"] != step.filename || r["checksum"] != sha256(step.sql) {
                return Err(Error(format!(
                    "released migration was changed or history is damaged: {}",
                    step.filename
                )));
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
            return Err(Error(format!("database integrity check failed: {rows:?}")));
        }
        if db.prepare("PRAGMA foreign_key_check")?.exists([])? {
            return Err(Error("database foreign-key check failed".into()));
        }
        if version >= 3 && db.prepare("SELECT 1 FROM mappings m WHERE m.active=1 AND NOT EXISTS (SELECT 1 FROM item_files a WHERE a.file_id=m.file_id AND a.item_id=m.item_id AND a.active=1) LIMIT 1")?.exists([])? { return Err(Error("an active catalog mapping has no active file identification".into())); }
    }
    let pending: i64 = db.query_row("SELECT count(*) FROM journal", [], |r| r.get(0))?;
    Ok(
        json!({"database_id":database_id, "schema":version, "latest_schema":SCHEMA_VERSION, "history":history, "pending_operations":pending, "pending_migrations":MIGRATIONS.iter().skip(version as usize).map(description).collect::<Vec<_>>()}),
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
fn digest_table(db: &Connection, table: &str, columns: &[String]) -> Result<Value> {
    use sha2::{Digest, Sha256};
    let selected = columns
        .iter()
        .map(|c| identifier(c))
        .collect::<Vec<_>>()
        .join(",");
    let mut s = db.prepare(&format!(
        "SELECT {selected} FROM {} NOT INDEXED ORDER BY {selected}",
        identifier(table)
    ))?;
    let mut cursor = s.query([])?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    while let Some(row) = cursor.next()? {
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
    let tables = rows(
        db,
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        &[],
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
        result.insert(name.into(), digest_table(db, name, &columns)?);
    }
    Ok(Value::Object(result))
}
pub fn preservation(db: &Connection, before: &Value) -> Result<()> {
    for (table, expected) in before.as_object().expect("snapshot object") {
        let columns = expected["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        if digest_table(db, table, &columns)? != *expected {
            return Err(Error(format!(
                "migration changed existing data in {table}; preservation validation failed"
            )));
        }
    }
    Ok(())
}
fn record_history(db: &Connection, version: u32, initializing: bool) -> Result<()> {
    if version < 2 {
        return Ok(());
    }
    for step in MIGRATIONS.iter().take(version as usize) {
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
    role: &str,
    checkpoint: &Checkpoint<'_>,
) -> Result<Vec<String>> {
    if db.is_autocommit() {
        return Err(Error(
            "migration runner requires an explicit transaction".into(),
        ));
    }
    let result = (|| {
        for step in MIGRATIONS.iter().skip(current as usize) {
            let before = snapshot(db)?;
            checkpoint(&format!("{role}:before_migration"), db, step.version)?;
            execute(db, step)?;
            record_history(db, step.version, false)?;
            db.pragma_update(None, "user_version", step.version)?;
            preservation(db, &before)?;
            validate(db, true)?;
            checkpoint(&format!("{role}:after_migration"), db, step.version)?;
        }
        checkpoint(&format!("{role}:before_commit"), db, SCHEMA_VERSION)?;
        db.execute_batch("COMMIT")?;
        // COMMIT is durable: a later observer error cannot be called rollback.
        Ok(
            checkpoint(&format!("{role}:after_commit"), db, SCHEMA_VERSION)
                .err()
                .map(|error| format!("{role} committed, but post-commit observer failed: {error}"))
                .into_iter()
                .collect(),
        )
    })();
    if result.is_err() && !db.is_autocommit() {
        db.execute_batch("ROLLBACK")?;
    }
    result
}
pub fn initialize(path: &Path) -> Result<Value> {
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
    for step in MIGRATIONS {
        execute(&db, step)?;
        if step.version == 1 {
            db.execute("INSERT INTO meta VALUES ('database_id',?)", [&id])?;
            db.execute_batch(
                "INSERT INTO profiles VALUES ('default'); INSERT INTO catalogs VALUES ('global')",
            )?;
        }
        record_history(&db, step.version, true)?;
        db.pragma_update(None, "user_version", step.version)?;
    }
    validate(&db, true)?;
    db.execute_batch("COMMIT")?;
    Ok(json!({"database":path,"database_id":id,"schema":SCHEMA_VERSION}))
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
fn fsync(path: &Path) -> Result<()> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?
        .sync_all()?;
    Ok(())
}
fn manifest(directory: &Path, value: &Value) -> Result<()> {
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
fn copy(db: &Connection, path: &Path) -> Result<()> {
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
    for part in suffix.into_iter().rev() {
        parent.push(part);
    }
    let directory = parent.join(format!(
        "v{}-to-v{}-{}",
        info["schema"],
        SCHEMA_VERSION,
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
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(&parent)?;
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
    let receipt = json!({"status":"backup_verified","database":path,"database_id":info["database_id"],"from_schema":info["schema"],"to_schema":SCHEMA_VERSION,"backup":saved,"sha256":sha256(fs::read(&saved)?),"tables_before":before,"migrations":info["pending_migrations"]});
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
            apply(&rehearsal, current, "rehearsal", checkpoint)?;
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
        let mut warnings = apply(&db, current, "upgrade", checkpoint)?;
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
