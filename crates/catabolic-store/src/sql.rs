//! Frozen catalog views and bounded, authorizer-enforced read-only SQL.
use crate::{
    Error, Result, Store,
    migration::{identifier, reference, rows, value},
};
use catabolic_core::encode_with;
use rusqlite::{
    Connection,
    functions::FunctionFlags,
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
    types::Value as SqlValue,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub const MAX_SQL_BYTES: usize = 1024 * 1024;
pub const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;

pub fn casefold(text: &str) -> String {
    let map = &reference()["casefold"];
    text.chars()
        .map(|c| {
            map.get(c.to_string())
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| c.to_string())
        })
        .collect()
}
fn field(raw: String, key: &str) -> rusqlite::Result<Value> {
    let value: Value =
        serde_json::from_str(&raw).map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?;
    Ok(value[key].clone())
}
pub fn install_functions(db: &Connection) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    for function in ["catalog_title", "title_key"] {
        db.create_scalar_function(function, 1, flags, move |ctx| {
            let v = field(ctx.get(0)?, "title")?;
            Ok(if function == "title_key" {
                Some(v.as_str().map(casefold).unwrap_or_default())
            } else {
                v.as_str().map(str::to_owned)
            })
        })?;
    }
    for function in ["catalog_year", "year_key"] {
        db.create_scalar_function(function, 1, flags, move |ctx| {
            let v = field(ctx.get(0)?, "year")?;
            let year = v.as_i64().filter(|n| (1..=9999).contains(n));
            Ok(if function == "year_key" {
                Some(year.unwrap_or(-1))
            } else {
                year
            })
        })?;
    }
    db.create_scalar_function("casefold", 1, flags, |ctx| {
        let v: Option<String> = ctx.get(0)?;
        Ok(v.map(|v| casefold(&v)))
    })?;
    db.create_scalar_function("contains_text", 2, flags, |ctx| {
        let v: Option<String> = ctx.get(0)?;
        let needle: String = ctx.get(1)?;
        Ok(casefold(v.as_deref().unwrap_or("")).contains(&casefold(&needle)))
    })?;
    db.create_scalar_function("metadata_matches", 3, flags, |ctx| {
        let raw: String = ctx.get(0)?;
        let key: String = ctx.get(1)?;
        let expected: String = ctx.get(2)?;
        let value: Value = serde_json::from_str(&raw)
            .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?;
        Ok(value
            .get(key)
            .is_some_and(|v| catabolic_core::encode(v) == expected))
    })?;
    Ok(())
}
pub fn setup(db: &Connection) -> Result<()> {
    install_functions(db)?;
    let temp: i32 = db.pragma_query_value(None, "temp_store", |r| r.get(0))?;
    if temp != 2 {
        db.pragma_update(None, "temp_store", "MEMORY")?;
    }
    for view in reference()["views"].as_array().unwrap() {
        db.execute_batch(&format!(
            "CREATE TEMP VIEW IF NOT EXISTS {} AS {}",
            view["name"].as_str().unwrap(),
            view["sql"].as_str().unwrap()
        ))?;
    }
    db.pragma_update(None, "query_only", true)?;
    Ok(())
}
pub fn scalar(value: &Value) -> Result<SqlValue> {
    Ok(match value {
        Value::Null=>SqlValue::Null, Value::Bool(v)=>SqlValue::Integer(i64::from(*v)), Value::String(v)=>SqlValue::Text(v.clone()),
        Value::Number(v) if v.is_i64()=>SqlValue::Integer(v.as_i64().unwrap()),
        Value::Number(v) if v.is_f64()=>SqlValue::Real(v.as_f64().unwrap()),
        _=>return Err(Error("--params must be a JSON object with named scalar values; profile is reserved, numbers must be finite and integers fit signed 64 bits".into()))
    })
}
fn parameters(raw: Option<&str>, profile: &str) -> Result<Vec<(String, SqlValue)>> {
    let invalid = || {
        Error("--params must be a JSON object with named scalar values; profile is reserved, numbers must be finite and integers fit signed 64 bits".into())
    };
    let value: Value = serde_json::from_str(raw.unwrap_or("{}")).map_err(|_| invalid())?;
    let mut bound = vec![];
    for (key, value) in value.as_object().ok_or_else(invalid)? {
        if key == "profile"
            || key.is_empty()
            || !key
                .chars()
                .enumerate()
                .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || i > 0 && c.is_ascii_digit())
        {
            return Err(invalid());
        }
        bound.push((key.clone(), scalar(value)?));
    }
    bound.push(("profile".into(), SqlValue::Text(profile.into())));
    Ok(bound)
}
fn columns(db: &Connection, database: &str, name: &str) -> Result<Value> {
    let mut s = db.prepare(&format!(
        "PRAGMA {database}.table_info({})",
        identifier(name)
    ))?;
    Ok(Value::Array(s.query_map([],|r| {let declared: String=r.get(2)?; Ok(json!({"name":r.get::<_,String>(1)?,"declared_type":if declared.is_empty() {None} else {Some(declared)}}))})?.collect::<std::result::Result<Vec<_>,_>>()?))
}
pub fn describe(db: &Connection, profile: &str) -> Result<Value> {
    let mut views = vec![];
    for v in reference()["views"].as_array().unwrap() {
        let name = v["name"].as_str().unwrap();
        views.push(
            json!({"name":name,"description":v["description"],"columns":columns(db,"temp",name)?}),
        );
    }
    let mut tables = vec![];
    for row in rows(
        db,
        "SELECT name FROM main.sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        &[],
    )? {
        let name = row["name"].as_str().unwrap();
        tables.push(json!({"name":name,"columns":columns(db,"main",name)?}));
    }
    let allowed = reference()["functions"].as_array().unwrap();
    let functions = rows(db, "PRAGMA function_list", &[])?
        .into_iter()
        .filter_map(|v| v["name"].as_str().map(str::to_owned))
        .filter(|n| allowed.contains(&json!(n)))
        .collect::<std::collections::BTreeSet<_>>();
    let functions = functions.into_iter().collect::<Vec<_>>();
    Ok(
        json!({"interface_version":1,"profile":profile,"views":views,"tables":tables,"parameters":{"profile":"Automatically bound to the selected CLI profile. Views contain every profile; use WHERE profile=:profile."},"functions":functions,"sqlite_version":rusqlite::version(),"limits":{"default_max_rows":1000,"maximum_max_rows":10000,"default_timeout_ms":5000,"maximum_timeout_ms":60000,"max_sql_bytes":MAX_SQL_BYTES,"max_result_bytes":MAX_RESULT_BYTES}}),
    )
}
#[derive(Clone)]
pub struct Options<'a> {
    pub profile: &'a str,
    pub params: Option<&'a str>,
    pub describe: bool,
    pub max_rows: usize,
    pub timeout_ms: u64,
    pub stable: bool,
    pub http: bool,
}
impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            profile: "default",
            params: None,
            describe: false,
            max_rows: 1000,
            timeout_ms: 5000,
            stable: false,
            http: false,
        }
    }
}
pub fn execute(path: &Path, sql: Option<&str>, options: &Options<'_>) -> Result<Value> {
    let store = Store::open(path, false, false)?;
    execute_store(&store, sql, options)
}
pub fn execute_store(store: &Store, sql: Option<&str>, options: &Options<'_>) -> Result<Value> {
    if !(1..=10000).contains(&options.max_rows) {
        return Err(Error("max-rows must be between 1 and 10000".into()));
    }
    if !(1..=60000).contains(&options.timeout_ms) {
        return Err(Error("timeout-ms must be between 1 and 60000".into()));
    }
    let bound = parameters(options.params, options.profile)?;
    if options.describe {
        if sql.is_some() || options.params.is_some() {
            return Err(Error(
                "--schema cannot be combined with SQL or --params".into(),
            ));
        }
    } else if sql.is_none_or(|s| s.trim().is_empty()) {
        return Err(Error(
            "provide one SQL statement, --file PATH, --file -, or --schema".into(),
        ));
    } else if sql.unwrap().len() > MAX_SQL_BYTES {
        return Err(Error("SQL exceeds the 1 MiB input limit".into()));
    }
    let db = &store.db;
    if !db
        .prepare("SELECT 1 FROM profiles WHERE id=?")?
        .exists([options.profile])?
    {
        return Err(Error(format!("unknown profile: {}", options.profile)));
    }
    let query_only: i32 = db.pragma_query_value(None, "query_only", |r| r.get(0))?;
    let busy: i32 = db.pragma_query_value(None, "busy_timeout", |r| r.get(0))?;
    let mut old_limits = vec![];
    let denied = Arc::new(Mutex::new(None::<String>));
    let deadline = Instant::now() + Duration::from_millis(options.timeout_ms);
    let result = (|| {
        setup(db)?;
        if options.describe {
            return describe(db, options.profile);
        }
        for (category, limit) in [
            (Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES as i32),
            (Limit::SQLITE_LIMIT_LENGTH, MAX_SQL_BYTES as i32),
            (Limit::SQLITE_LIMIT_COLUMN, 256),
            (Limit::SQLITE_LIMIT_EXPR_DEPTH, 100),
            (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 50),
            (Limit::SQLITE_LIMIT_ATTACHED, 0),
            (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 1000),
        ] {
            old_limits.push((category, db.set_limit(category, limit)?));
        }
        db.pragma_update(None, "busy_timeout", options.timeout_ms.min(5000) as i64)?;
        let mut private: std::collections::BTreeSet<String> = [
            "http_operations",
            "http_mapping_definitions",
            "http_mapping_ledger",
            "http_mapping_deliveries",
            "execution_claims",
            "sqlite_master",
            "sqlite_schema",
            "fallback_probe_slots",
            "watchers",
            "watcher_definitions",
            "watcher_runs",
            "watcher_owners",
            "observation_jobs",
            "observation_sources",
            "observation_requests",
            "source_observation_policies",
            "source_observation_policy_history",
            "observation_scopes",
            "observation_batches",
            "observation_seen",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let mut http_tables = std::collections::BTreeSet::new();
        if options.http {
            for row in rows(
                db,
                "SELECT name FROM main.sqlite_schema WHERE type='table'",
                &[],
            )? {
                let table = row["name"].as_str().unwrap();
                http_tables.insert(table.to_string());
                let cols = columns(db, "main", table)?;
                if table.starts_with("api_")
                    || cols.as_array().unwrap().iter().any(|v| {
                        [
                            "token",
                            "lease_token",
                            "credential_env",
                            "secret",
                            "password",
                        ]
                        .contains(&v["name"].as_str().unwrap())
                    })
                {
                    private.insert(table.to_string());
                }
            }
        }
        let captured = denied.clone();
        let stable = options.stable;
        let http = options.http;
        db.authorizer(Some(move |context: AuthContext<'_>| {
            let (allow, name) = match context.action {
                AuthAction::Select | AuthAction::Recursive => (true, None),
                AuthAction::Read {
                    table_name,
                    column_name,
                } => {
                    let safe =
                        !(http && (table_name.starts_with("api_") || private.contains(table_name)));
                    let valid_database = matches!(context.database_name, Some("main" | "temp"))
                        || context.database_name.is_none()
                            && column_name.is_empty()
                            && (reference()["views"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|v| v["name"] == table_name)
                                || http && http_tables.contains(table_name) && safe);
                    (safe && valid_database, Some(table_name.to_string()))
                }
                AuthAction::Function { function_name } => {
                    let name = function_name.to_lowercase();
                    let allowed = reference()["functions"]
                        .as_array()
                        .unwrap()
                        .contains(&json!(name));
                    let nondeterministic = [
                        "random",
                        "randomblob",
                        "date",
                        "time",
                        "datetime",
                        "julianday",
                        "unixepoch",
                        "strftime",
                        "timediff",
                        "changes",
                        "total_changes",
                        "last_insert_rowid",
                    ]
                    .contains(&name.as_str());
                    (
                        allowed && !(stable && nondeterministic),
                        Some(function_name.into()),
                    )
                }
                AuthAction::Pragma { pragma_name, .. } => (false, Some(pragma_name.into())),
                AuthAction::Insert { table_name }
                | AuthAction::Delete { table_name }
                | AuthAction::Update { table_name, .. }
                | AuthAction::DropTable { table_name }
                | AuthAction::CreateTable { table_name } => (false, Some(table_name.into())),
                action => (false, Some(format!("{action:?}"))),
            };
            if allow {
                Authorization::Allow
            } else {
                let mut d = captured.lock().expect("denial lock");
                if d.is_none() {
                    *d = name;
                }
                Authorization::Deny
            }
        }))?;
        db.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        let sql = sql.unwrap();
        if sql.contains('\0') {
            return Err(Error(
                "SQL query failed: the query contains a null character".into(),
            ));
        }
        let mut statement = db.prepare(sql).map_err(|error| match error {
            rusqlite::Error::SqlInputError { msg, .. } => Error(msg),
            rusqlite::Error::MultipleStatement => {
                Error("You can only execute one statement at a time.".into())
            }
            other => Error::from(other),
        })?;
        if statement.column_count() == 0 {
            return Err(Error("SQL must return a result set".into()));
        }
        for i in 1..=statement.parameter_count() {
            let name=statement.parameter_name(i).and_then(|n| n.strip_prefix(':').or_else(|| n.strip_prefix('$')).or_else(|| n.strip_prefix('@'))).ok_or_else(|| Error(format!("SQL query failed: Binding {i} has no name, but you supplied a dictionary (which has only names).")))?;
            let value = bound.iter().find(|(k, _)| k == name).ok_or_else(|| {
                Error(format!(
                    "SQL query failed: You did not supply a value for binding parameter :{name}."
                ))
            })?;
            statement.raw_bind_parameter(i, &value.1)?;
        }
        let columns = statement
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let mut used = encode_with(&json!(columns), false, false).len();
        if used > MAX_RESULT_BYTES {
            return Err(Error(
                "result column names exceed the output byte limit".into(),
            ));
        }
        let mut cursor = statement.raw_query();
        let mut output = vec![];
        let mut reason = None;
        while let Some(row) = cursor.next()? {
            if Instant::now() >= deadline {
                return Err(Error(
                    "SQL execution timed out; simplify the query or increase --timeout-ms".into(),
                ));
            }
            if output.len() == options.max_rows {
                reason = Some("max_rows");
                break;
            }
            let mut converted = vec![];
            for i in 0..columns.len() {
                let raw = row.get_ref(i)?;
                let cell = if options.http {
                    if let rusqlite::types::ValueRef::Integer(n) = raw {
                        if n.unsigned_abs() > 9007199254740991 {
                            json!({"$integer":n.to_string()})
                        } else {
                            value(raw)
                        }
                    } else {
                        value(raw)
                    }
                } else {
                    value(raw)
                };
                converted.push(cell);
            }
            let converted = json!(converted);
            used += encode_with(&converted, false, false).len();
            if used > MAX_RESULT_BYTES {
                reason = Some("max_result_bytes");
                break;
            }
            output.push(converted);
        }
        Ok(
            json!({"interface_version":1,"profile":options.profile,"columns":columns,"row_count":output.len(),"rows":output,"max_rows":options.max_rows,"truncated":reason.is_some(),"truncation_reason":reason,"complete":reason.is_none()}),
        )
    })();
    db.progress_handler(0, None::<fn() -> bool>)?;
    db.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
    for (limit, old) in old_limits {
        db.set_limit(limit, old)?;
    }
    db.pragma_update(None, "query_only", query_only)?;
    db.pragma_update(None, "busy_timeout", busy)?;
    result.map_err(|error| {
        if let Some(name) = denied.lock().expect("denial lock").as_ref() {
            Error(format!(
                "read-only SQL rejected an operation or function: {name}"
            ))
        } else if Instant::now() >= deadline {
            Error("SQL execution timed out; simplify the query or increase --timeout-ms".into())
        } else if error.0.starts_with("SQL query failed:")
            || error.0.starts_with("SQL must")
            || error.0.starts_with("result column")
            || error.0.starts_with("--")
        {
            error
        } else {
            Error(format!("SQL query failed: {error}"))
        }
    })
}
