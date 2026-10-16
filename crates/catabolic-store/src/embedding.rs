// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Persistent native embedding session. Never borrows a Python sqlite connection.
use crate::{Error, Result, Store, migration::rows};
use rusqlite::{
    OptionalExtension,
    types::{Value as SqlValue, ValueRef},
};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    path::{Path, PathBuf},
    thread::ThreadId,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct EvaluationError {
    pub error: Error,
    pub failure: Option<crate::selection::EvaluationFailure>,
}
impl From<Error> for EvaluationError {
    fn from(error: Error) -> Self {
        Self {
            error,
            failure: None,
        }
    }
}

pub struct Session {
    store: Option<Store>,
    path: PathBuf,
    writable: bool,
    recovery: bool,
    database_id: String,
    catalog_incarnation: Option<String>,
    native_path_identity: Option<(u64, u64)>,
    schema_version: u32,
    detached: bool,
    scopes: Vec<(u64, Option<String>)>,
    next_scope: u64,
    evaluations: std::collections::BTreeMap<u64, QueryEvaluation>,
    next_evaluation: u64,
    fallback_plans: crate::public_fallback::Plans,
    transaction_thread: Option<ThreadId>,
    poisoned_scopes: Cell<bool>,
}
struct QueryEvaluation {
    profile: String,
    epoch: i64,
    evaluation: crate::selection::Evaluation,
}
impl Session {
    pub fn open(path: &Path, writable: bool, recovery: bool) -> Result<Self> {
        Self::open_typed(path, writable, recovery).map_err(Into::into)
    }
    /// Retain the native Store's admission cause without changing session ownership.
    pub fn open_typed(path: &Path, writable: bool, recovery: bool) -> crate::OpenResult<Self> {
        let store = Store::open_typed(path, writable, recovery)?;
        Ok(Self {
            path: store.path.clone(),
            database_id: store.database_id.clone(),
            catalog_incarnation: store.catalog_incarnation.clone(),
            native_path_identity: store.native_path_identity,
            schema_version: store.schema_version,
            store: Some(store),
            writable,
            recovery,
            detached: false,
            scopes: vec![],
            next_scope: 0,
            evaluations: Default::default(),
            next_evaluation: 0,
            fallback_plans: Default::default(),
            transaction_thread: None,
            poisoned_scopes: Cell::new(false),
        })
    }
    /// Validate a fallback evaluation before an outer callback can run. This
    /// performs no cancellation polling and does not lend the Store.
    pub fn check_evaluation(&self, profile: &str, args: &Value) -> Result<()> {
        self.store()?;
        if args["session_owner_matches"] == false {
            return Err(Error("evaluation_session_identity_mismatch".into()));
        }
        let epoch = crate::fallback_resolution::epoch(self.store()?)?;
        if let Some(token) = args["session"].as_u64() {
            let entry = self
                .evaluations
                .get(&token)
                .ok_or_else(|| Error("evaluation_session_identity_mismatch".into()))?;
            if entry.profile != profile {
                return Err(Error("evaluation_session_identity_mismatch".into()));
            }
            if entry.epoch != epoch {
                return Err(Error("stale_evaluation_snapshot".into()));
            }
        } else if !args["session"].is_null() {
            return Err(Error("evaluation_session_identity_mismatch".into()));
        }
        Ok(())
    }
    /// Lend the same Store to a bounded evaluator without reopening a catalog.
    /// Native session handles retain their consumed budget, cache and epoch.
    pub fn with_evaluation<T>(
        &mut self,
        profile: &str,
        args: &Value,
        timeout_ms: u64,
        max_ids: usize,
        operation: impl FnOnce(&mut crate::selection::Evaluation) -> Result<T>,
    ) -> Result<T> {
        self.with_evaluation_typed(profile, args, timeout_ms, max_ids, operation)
            .map_err(|error| error.error)
    }
    /// Retain typed refusal evidence even before the caller's closure runs.
    pub fn with_evaluation_typed<T>(
        &mut self,
        profile: &str,
        args: &Value,
        timeout_ms: u64,
        max_ids: usize,
        operation: impl FnOnce(&mut crate::selection::Evaluation) -> Result<T>,
    ) -> std::result::Result<T, EvaluationError> {
        self.check_evaluation(profile, args)?;
        let mut temporary;
        let evaluation = if let Some(token) = args["session"].as_u64() {
            &mut self.evaluations.get_mut(&token).unwrap().evaluation
        } else {
            temporary = crate::selection::Evaluation::unbound(profile, timeout_ms, max_ids, false);
            &mut temporary
        };
        if args["cancelled"] == true || poll_interrupt() {
            return Err(EvaluationError {
                error: Error("evaluation_cancelled".into()),
                failure: Some(crate::selection::EvaluationFailure::Cancelled),
            });
        }
        evaluation
            .remaining("query composition timed out")
            .map_err(|error| EvaluationError {
                error,
                failure: evaluation.failure(),
            })?;
        self.store
            .as_ref()
            .unwrap()
            .db
            .progress_handler(1000, Some(poll_interrupt))
            .map_err(Error::from)?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            evaluation.with_store(&mut self.store, operation)
        }));
        if let Some(store) = &self.store {
            let _ = store.db.progress_handler(0, None::<fn() -> bool>);
        }
        match result {
            Ok(result) => result.map_err(|error| EvaluationError {
                error,
                failure: evaluation.failure(),
            }),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
    /// Borrow the exact catalog owner for a sealed administrative grant check.
    /// This evaluator is temporary and cannot enter the public session table.
    pub(crate) fn select_access_grant(
        &mut self,
        profile: &str,
        id: &str,
        admission: crate::access_admin::GrantPreflight,
    ) -> Result<crate::selection::Selected> {
        let definition =
            crate::definitions::query_get(self.store()?, profile, id)?["definition"].clone();
        let mut evaluation = crate::selection::Evaluation::for_access_grant(
            profile,
            definition["timeout_ms"].as_u64().unwrap_or(5000),
            definition["max_ids"].as_u64().unwrap_or(10000) as usize,
            admission,
        );
        let result = evaluation.with_store(&mut self.store, |evaluation| {
            evaluation.select(&json!({"query_id":id,"profile":profile}))
        });
        // Bounded SQL/GraphQL evaluation removes its own deadline handler.
        // Restore an enclosing embedding bridge before the owner's mutation.
        if INTERRUPT.with(|slot| slot.borrow().is_some())
            && let Some(store) = &self.store
        {
            store.db.progress_handler(1000, Some(poll_interrupt))?;
        }
        result
    }
    /// Native plans are tied to this Session and never serialized into SQLite.
    pub fn fallback_plans(&mut self) -> &mut crate::public_fallback::Plans {
        &mut self.fallback_plans
    }
    /// Public watcher metadata and invalidation retain the caller's exact owner.
    pub fn watcher_call(&mut self, profile: &str, operation: &str, args: &Value) -> Result<Value> {
        crate::public_watchers::call(self.store()?, profile, operation, args)
    }
    /// Dispatch persistent query engines using the caller's current transaction.
    pub fn query_call(&mut self, profile: &str, operation: &str, args: &Value) -> Result<Value> {
        self.query_call_inputs(profile, operation, args, None)
    }
    /// Preserve exact native command scope during adoption, and lend the same
    /// owner to an existing bounded native evaluation for component lookups.
    pub fn component_call(
        &mut self,
        profile: &str,
        operation: &str,
        args: &Value,
    ) -> Result<Value> {
        if operation == "get" && args.get("session").is_some() {
            return self.query_call(profile, "component-get", args);
        }
        if operation == "backfill" {
            let token = match args.get("token") {
                Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_u64()
                        .ok_or_else(|| Error("invalid native transaction handle".into()))?,
                ),
                None => {
                    return Err(Error(
                        "backfill requires a native Store command handle".into(),
                    ));
                }
            };
            self.check_scope(token)?;
        }
        let store = self.store()?;
        store.db.progress_handler(1000, Some(poll_interrupt))?;
        struct Reset<'a>(&'a rusqlite::Connection);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                let _ = self.0.progress_handler(0, None::<fn() -> bool>);
            }
        }
        let _reset = Reset(&store.db);
        crate::public_components::call(store, profile, operation, args)
    }
    /// Preserve GraphQL input order from the existing bounded Python request.
    pub fn query_call_json(&mut self, profile: &str, operation: &str, raw: &str) -> Result<Value> {
        let args = catabolic_core::decode_json(raw)?;
        let variables = if operation == "graphql" {
            Some(crate::graphql::InputVariables::from_request_json(
                raw,
                "variables",
            )?)
        } else {
            None
        };
        self.query_call_inputs(profile, operation, &args, variables)
    }
    fn query_call_inputs(
        &mut self,
        profile: &str,
        operation: &str,
        args: &Value,
        variables: Option<crate::graphql::InputVariables>,
    ) -> Result<Value> {
        self.store()?;
        match operation {
            "graphql" => crate::graphql::execute_session(
                &mut self.store,
                None,
                args["document"]
                    .as_str()
                    .ok_or_else(|| Error("query document must be at most 131072 bytes".into()))?,
                profile,
                variables.unwrap_or_else(|| {
                    args.get("variables")
                        .cloned()
                        .unwrap_or_else(|| json!({}))
                        .into()
                }),
                args["operation_name"].as_str(),
                query_bound(
                    args,
                    "timeout_ms",
                    5000,
                    60000,
                    "timeout-ms must be between 1 and 60000",
                )? as u64,
            ),
            "sql" => crate::sql::execute_store(
                self.store()?,
                args["sql"].as_str(),
                &crate::sql::Options {
                    profile,
                    params: args["params"].as_str(),
                    describe: args["describe"].as_bool().unwrap_or(false),
                    max_rows: query_bound(
                        args,
                        "max_rows",
                        1000,
                        10000,
                        "max-rows must be between 1 and 10000",
                    )?,
                    timeout_ms: query_bound(
                        args,
                        "timeout_ms",
                        5000,
                        60000,
                        "timeout-ms must be between 1 and 60000",
                    )? as u64,
                    stable: args["_stable"].as_bool().unwrap_or(false),
                    http: args["_http"].as_bool().unwrap_or(false),
                },
            ),
            "evaluation-create" => {
                let timeout = query_bound(
                    args,
                    "timeout_ms",
                    5000,
                    60000,
                    "timeout-ms must be between 1 and 60000",
                )?;
                let maximum = query_bound(args, "max_ids", 10000, 100000, "invalid query max_ids")?;
                let max_bytes = args["max_bytes"]
                    .as_u64()
                    .ok_or_else(|| Error("invalid evaluation max_bytes".into()))?
                    as usize;
                let epoch = self.store()?.db.query_row(
                    "SELECT generation FROM fallback_epoch WHERE id=1",
                    [],
                    |row| row.get(0),
                )?;
                let mut evaluation = crate::selection::Evaluation::unbound(
                    profile,
                    timeout as u64,
                    maximum,
                    args["http"].as_bool().unwrap_or(false),
                );
                evaluation.set_max_bytes(max_bytes);
                self.next_evaluation += 1;
                self.evaluations.insert(
                    self.next_evaluation,
                    QueryEvaluation {
                        profile: profile.into(),
                        epoch,
                        evaluation,
                    },
                );
                Ok(json!({"token":self.next_evaluation,"epoch":epoch}))
            }
            "evaluation-drop" => {
                self.evaluations
                    .remove(&args["session"].as_u64().unwrap_or(0));
                Ok(Value::Null)
            }
            "evaluation-check" | "evaluation-account" | "evaluation-pause" | "select" | "run"
            | "component-get" => {
                let session = args["session"].as_u64();
                let epoch: i64 = self.store()?.db.query_row(
                    "SELECT generation FROM fallback_epoch WHERE id=1",
                    [],
                    |row| row.get(0),
                )?;
                let identifier = args["id"].as_str().unwrap_or("");
                // Frozen get precedes session.check for select/run, and run's
                // page limit precedes get. Keep this error precedence.
                let limit = if operation == "run" {
                    query_bound(
                        args,
                        "limit",
                        1000,
                        1000,
                        "limit must be between 1 and 1000",
                    )?
                } else {
                    1000
                };
                let definition = if matches!(operation, "select" | "run") {
                    crate::definitions::query_get(self.store()?, profile, identifier)?["definition"]
                        .clone()
                } else {
                    Value::Null
                };
                // Token values are local to their Store. Defer the ownership
                // check until after frozen limit/lookup validation, but never
                // resolve a different Store's token against this token table.
                if args["session_owner_matches"] == false {
                    return Err(Error("evaluation_session_identity_mismatch".into()));
                }
                let mut temporary;
                let evaluation = if let Some(token) = session {
                    let entry = self
                        .evaluations
                        .get_mut(&token)
                        .ok_or_else(|| Error("evaluation_session_identity_mismatch".into()))?;
                    if entry.profile != profile {
                        return Err(Error("evaluation_session_identity_mismatch".into()));
                    }
                    if entry.epoch != epoch {
                        return Err(Error("stale_evaluation_snapshot".into()));
                    }
                    if args["cancelled"] == true {
                        return Err(Error("evaluation_cancelled".into()));
                    }
                    if operation != "evaluation-pause" {
                        entry.evaluation.remaining("query composition timed out")?;
                    }
                    &mut entry.evaluation
                } else {
                    temporary = crate::selection::Evaluation::unbound(
                        profile,
                        definition["timeout_ms"].as_u64().unwrap_or(5000),
                        definition["max_ids"].as_u64().unwrap_or(10000) as usize,
                        args["_http"].as_bool().unwrap_or(false),
                    );
                    &mut temporary
                };
                evaluation.with_store(&mut self.store, |evaluation| match operation {
                    "evaluation-check" => Ok(Value::Null),
                    "evaluation-account" => {
                        evaluation.account_value(&args["value"])?;
                        Ok(Value::Null)
                    }
                    "evaluation-pause" => {
                        evaluation.pause(
                            args["seconds"]
                                .as_f64()
                                .ok_or_else(|| Error("invalid evaluation pause duration".into()))?,
                        )?;
                        Ok(Value::Null)
                    }
                    "select" => {
                        let selected =
                            evaluation.select(&json!({"query_id":identifier,"profile":profile}))?;
                        Ok(json!([selected.entity, selected.ids, selected.report]))
                    }
                    "component-get" => crate::public_components::get_evaluated(
                        evaluation,
                        profile,
                        identifier,
                        args["require_current"].as_bool().unwrap_or(false),
                        epoch,
                    ),
                    _ => {
                        let result = evaluation.run(identifier, limit)?;
                        if session.is_some() && definition["mode"] != "selection" {
                            evaluation.account_value(&result)?;
                        }
                        Ok(result)
                    }
                })
            }
            _ => Err(Error(format!(
                "unsupported native query operation: {operation}"
            ))),
        }
    }
    pub fn info(&self) -> Value {
        json!({"path":self.path,"writable":self.writable,"database_id":self.database_id,"schema_version":self.schema_version,"closed":self.store.is_none(),"detached":self.detached})
    }
    fn raw_store(&self) -> Result<&Store> {
        if self
            .transaction_thread
            .is_some_and(|id| id != std::thread::current().id())
        {
            return Err(Error("native transaction belongs to another thread".into()));
        }
        self.store.as_ref().ok_or_else(|| {
            Error(
                if self.detached {
                    "native store is detached"
                } else {
                    "native store is closed"
                }
                .into(),
            )
        })
    }
    /// Detect SQLite's implicit rollback before any later handle/service use.
    /// Keep lexical tokens until their contexts unwind; they are all poisoned.
    pub fn synchronize(&self) {
        if !self.scopes.is_empty()
            && self
                .store
                .as_ref()
                .is_none_or(|s| s.db.is_autocommit() || s.transaction_available().is_err())
        {
            self.poisoned_scopes.set(true);
        }
    }
    pub fn store(&self) -> Result<&Store> {
        self.synchronize();
        let store = self.raw_store()?;
        if self.poisoned_scopes.get() {
            return Err(Error(
                "native transaction was aborted; exit all active transaction scopes before reuse"
                    .into(),
            ));
        }
        store.transaction_available()?;
        Ok(store)
    }
    /// Cleanup only: remove a bridge callback even after SQLite aborted an
    /// active lexical scope. This does not admit further operations or revive it.
    pub fn clear_interrupt_handler(&self) {
        if let Some(store) = &self.store {
            let _ = store.db.progress_handler(0, None::<fn() -> bool>);
        }
    }
    /// Cancel all live lexical scopes, retaining tokens only for clean unwind.
    pub fn abort_scopes(&mut self) {
        if !self.scopes.is_empty() {
            self.poisoned_scopes.set(true);
        }
        if let Some(store) = &self.store
            && store.writable
            && !store.db.is_autocommit()
            && store.rollback_transaction(None).is_err()
        {
            self.store.take();
        }
    }
    /// A panic must never restore an unchecked, live transaction to Python.
    pub fn unwind(&mut self) {
        self.abort_scopes();
    }
    pub fn store_mut(&mut self) -> Result<&mut Store> {
        self.store()?;
        Ok(self.store.as_mut().unwrap())
    }
    pub fn close(&mut self) -> Result<()> {
        if self.store.is_some() {
            self.raw_store()?;
        }
        self.store.take();
        if self.scopes.is_empty() {
            self.transaction_thread = None;
            self.poisoned_scopes.set(false);
        } else {
            self.poisoned_scopes.set(true);
        }
        Ok(())
    }
    pub fn begin(&mut self) -> Result<u64> {
        let scope = self.store()?.begin_transaction()?;
        self.next_scope += 1;
        self.scopes.push((self.next_scope, scope));
        self.transaction_thread = Some(std::thread::current().id());
        Ok(self.next_scope)
    }
    pub fn check_scope(&self, token: Option<u64>) -> Result<()> {
        self.store()?;
        if token.is_some_and(|id| !self.scopes.iter().any(|(current, _)| *current == id)) {
            return Err(Error(
                "native transaction handle is no longer active".into(),
            ));
        }
        Ok(())
    }
    pub fn finish(&mut self, token: u64, commit: bool) -> Result<()> {
        if self
            .transaction_thread
            .is_some_and(|id| id != std::thread::current().id())
        {
            return Err(Error("native transaction belongs to another thread".into()));
        }
        self.synchronize();
        if self.scopes.last().map(|s| s.0) != Some(token) {
            return Err(Error(
                "native transactions must exit in nesting order".into(),
            ));
        }
        let (_, scope) = self.scopes.pop().unwrap();
        let poisoned = self.poisoned_scopes.get();
        let result = if poisoned {
            // The underlying transaction already vanished or was cancelled.
            // Cleanup does not mask the exception that initiated scope unwind.
            self.abort_scopes();
            if commit {
                Err(Error(
                    "native transaction was aborted; commit refused".into(),
                ))
            } else {
                Ok(())
            }
        } else {
            match self.raw_store() {
                Ok(store) if commit => store.finish_transaction(scope, Ok(())),
                Ok(store) => store.rollback_transaction(scope.as_deref()),
                Err(error) => Err(error),
            }
        };
        // Commit/rollback failure may itself abort or invalidate parent scopes.
        self.synchronize();
        if result.is_err()
            && self
                .store
                .as_ref()
                .is_some_and(|s| s.transaction_available().is_err())
        {
            self.store.take();
            self.poisoned_scopes.set(true);
        }
        if self.scopes.is_empty() {
            self.transaction_thread = None;
            self.poisoned_scopes.set(false);
        }
        result
    }
    pub fn detach(&mut self) -> Result<()> {
        if !self.store()?.db.is_autocommit() {
            return Err(Error("cannot detach an active transaction".into()));
        }
        self.store.take();
        self.detached = true;
        Ok(())
    }
    pub fn reconnect(&mut self) -> Result<()> {
        if !self.detached {
            return Err(Error("native store is not detached".into()));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let reopened = loop {
            match Store::open(&self.path, self.writable, self.recovery) {
                Err(error)
                    if error.0.contains("another Catabolic writer")
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(20))
                }
                result => break result,
            }
        };
        self.detached = false;
        let store = reopened?;
        if store.database_id != self.database_id
            || store.schema_version != self.schema_version
            || store.catalog_incarnation != self.catalog_incarnation
            || store.native_path_identity != self.native_path_identity
        {
            return Err(Error("catalog replaced during detached execution".into()));
        }
        // A reconnect is a fresh admission boundary, never validation of old
        // evaluation/fallback handles simply because the catalog UUID matches.
        if self.catalog_incarnation.is_some() {
            self.evaluations.clear();
            self.fallback_plans.invalidate()?;
        }
        self.store = Some(store);
        Ok(())
    }
    pub fn in_transaction(&self) -> Result<bool> {
        Ok(!self.store()?.db.is_autocommit())
    }
}

fn wire_value(value: &Value) -> Result<SqlValue> {
    let kind = value[0]
        .as_str()
        .ok_or_else(|| Error("invalid native SQL parameter".into()))?;
    Ok(match kind {
        "null" => SqlValue::Null,
        "int" => SqlValue::Integer(
            value[1]
                .as_str()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| Error("Python int too large to convert to SQLite INTEGER".into()))?,
        ),
        "float" => SqlValue::Real(
            value[1]
                .as_str()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| Error("invalid SQL float".into()))?,
        ),
        "str" => SqlValue::Text(
            value[1]
                .as_str()
                .ok_or_else(|| Error("invalid SQL text".into()))?
                .into(),
        ),
        "bytes" => SqlValue::Blob(
            value[1]
                .as_array()
                .ok_or_else(|| Error("invalid SQL bytes".into()))?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .filter(|n| *n <= 255)
                        .map(|n| n as u8)
                        .ok_or_else(|| Error("invalid SQL byte".into()))
                })
                .collect::<Result<_>>()?,
        ),
        _ => return Err(Error("unsupported native SQL parameter".into())),
    })
}
fn wire_result(value: ValueRef<'_>) -> Result<Value> {
    Ok(match value {
        ValueRef::Null => json!(["null"]),
        ValueRef::Integer(v) => json!(["int", v.to_string()]),
        ValueRef::Real(v) => json!(["float", v.to_string()]),
        ValueRef::Text(v) => json!([
            "str",
            std::str::from_utf8(v).map_err(|_| Error("SQLite text is not valid UTF-8".into()))?
        ]),
        ValueRef::Blob(v) => json!(["bytes", v]),
    })
}
/// Explicit native command result, not the Python sqlite3 API or connection.
pub fn command(store: &Store, sql: &str, args: &Value) -> Result<Value> {
    store.transaction_available()?;
    // Session contexts own transaction state. SQL must not escape their scopes.
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    store
        .db
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Transaction { .. }
            | AuthAction::Savepoint { .. }
            | AuthAction::Attach { .. }
            | AuthAction::Detach { .. } => Authorization::Deny,
            AuthAction::Pragma {
                pragma_name,
                pragma_value: Some(_),
            } if ["writable_schema", "query_only"]
                .iter()
                .any(|v| pragma_name.eq_ignore_ascii_case(v)) =>
            {
                Authorization::Deny
            }
            _ => Authorization::Allow,
        }))?;
    struct Reset<'a>(&'a rusqlite::Connection);
    impl Drop for Reset<'_> {
        fn drop(&mut self) {
            let _ = self
                .0
                .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        }
    }
    let _reset = Reset(&store.db);
    let mut statement = store.db.prepare(sql)?;
    if let Some(values) = args.as_array() {
        let values = values.iter().map(wire_value).collect::<Result<Vec<_>>>()?;
        for (index, value) in values.iter().enumerate() {
            statement.raw_bind_parameter(index + 1, value)?;
        }
        if values.len() != statement.parameter_count() {
            return Err(Error(format!(
                "SQL statement uses {} parameters, {} supplied",
                statement.parameter_count(),
                values.len()
            )));
        }
    } else if let Some(values) = args.as_object() {
        for index in 1..=statement.parameter_count() {
            let name = statement
                .parameter_name(index)
                .ok_or_else(|| Error("unnamed parameter requires a sequence".into()))?;
            let value = values
                .get(name.trim_start_matches([':', '@', '$']))
                .ok_or_else(|| Error(format!("missing SQL parameter: {name}")))?;
            statement.raw_bind_parameter(index, wire_value(value)?)?;
        }
    } else {
        return Err(Error("SQL parameters must be a sequence or mapping".into()));
    }
    let columns = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let readonly = statement.readonly();
    let mut result = Vec::new();
    {
        let mut cursor = statement.raw_query();
        while let Some(row) = cursor.next()? {
            result.push(
                (0..columns.len())
                    .map(|i| wire_result(row.get_ref(i)?))
                    .collect::<Result<Vec<_>>>()?,
            );
        }
    }
    Ok(
        json!({"columns":columns,"rows":result,"rowcount":if readonly {-1_i64}else{store.db.changes() as i64},"lastrowid":store.db.last_insert_rowid()}),
    )
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| Error(format!("{key} must be text")))
}
pub fn application(store: &Store, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    crate::mutation::profile(store, profile)?;
    match operation {
        "initialize" => {
            crate::query::CatalogQuery::new(store, profile)?;
            Ok(Value::Null)
        }
        "status" => {
            let mut counts = serde_json::Map::new();
            for table in [
                "locations",
                "catalogs",
                "files",
                "items",
                "mappings",
                "journal",
            ] {
                counts.insert(
                    table.into(),
                    json!(store.db.query_row(
                        &format!("SELECT count(*) FROM {table}"),
                        [],
                        |r| r.get::<_, i64>(0)
                    )?),
                );
            }
            Ok(
                json!({"database":store.path,"database_id":store.database_id,"profile":profile,"pending_operations":rows(&store.db,"SELECT * FROM journal WHERE profile=? ORDER BY rowid",&[profile.to_owned().into()])?,"counts":counts,"bindings":rows(&store.db,"SELECT * FROM bindings WHERE profile=? ORDER BY kind,owner",&[profile.to_owned().into()])?}),
            )
        }
        "require_recovered" => {
            crate::mutation::require_recovered(&store.db)?;
            Ok(Value::Null)
        }
        "add_profile" => {
            let name = text(args, "profile")?;
            crate::query::name(name)?;
            store.transaction(|db| {
                db.execute(
                    "INSERT INTO profiles(id) VALUES (?) ON CONFLICT DO NOTHING",
                    [name],
                )?;
                Ok(())
            })?;
            Ok(json!({"profile":name}))
        }
        "link_mode" => Ok(json!(link_mode(store, text(args, "catalog")?)?)),
        "binding" => {
            let kind = text(args, "kind")?;
            let owner = text(args, "owner")?;
            let binding = crate::observation::binding(store, profile, kind, owner)?;
            let volume: Option<String> = if store.schema_version >= 18 {
                store.db.query_row("SELECT volume_uuid FROM binding_volumes WHERE profile=? AND kind=? AND owner=?",[profile,kind,owner],|r|r.get(0)).optional()?
            } else {
                None
            };
            let policy = if kind == "source" {
                crate::observation::trust_policy(store, profile, owner)?
            } else {
                json!({"revision":0,"settings":{"uuid":"check","device":"check","inode":"check"}})
            };
            Ok(json!({"value":binding,"volume_uuid":volume,"policy":policy}))
        }
        "bind" => bind(store, profile, args),
        "put_item" => crate::curation::item_put(store, args),
        "mapping_id" => Ok(json!(mapping_id(store, args)?)),
        "put_mapping" => put_mapping(store, profile, args),
        "disable_mapping" => {
            crate::mutation::require_recovered(&store.db)?;
            let id = text(args, "id")?;
            store.transaction(|db| {
                if !db
                    .prepare("SELECT id FROM mappings WHERE id=?")?
                    .exists([id])?
                {
                    return Err(Error(format!("unknown mapping: {id}")));
                }
                db.execute("UPDATE mappings SET active=0 WHERE id=?", [id])?;
                Ok(())
            })?;
            Ok(json!({"id":id,"active":false}))
        }
        _ => Err(Error(format!(
            "unsupported native Application operation: {operation}"
        ))),
    }
}
fn link_mode(store: &Store, catalog: &str) -> Result<String> {
    if store.schema_version < 5 {
        return Ok("symlink".into());
    }
    Ok(store
        .db
        .query_row(
            "SELECT mode FROM catalog_link_modes WHERE catalog=?",
            [catalog],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| "symlink".into()))
}
fn bind(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    crate::mutation::require_recovered(&store.db)?;
    let kind = text(args, "kind")?;
    let owner = text(args, "owner")?;
    if !matches!(kind, "source" | "output") {
        return Err(Error("binding kind must be source or output".into()));
    }
    crate::query::name(owner)?;
    let mode = args["link_mode"].as_str();
    if let Some(mode) = mode {
        if kind != "output" {
            return Err(Error("link mode applies only to output catalogs".into()));
        }
        if !matches!(mode, "symlink" | "hardlink") {
            return Err(Error("link mode must be symlink or hardlink".into()));
        }
        if mode != link_mode(store, owner)? {
            for table in [
                "owned_links",
                "owned_hardlinks",
                "retained_hardlinks",
                "journal",
            ] {
                if store
                    .db
                    .prepare(&format!("SELECT 1 FROM {table} WHERE catalog=? LIMIT 1"))?
                    .exists([owner])?
                {
                    return Err(Error("cannot change link mode while this catalog has owned links, retained data or pending operations; create a separate catalog".into()));
                }
            }
        }
    } else if !args["link_mode"].is_null() {
        return Err(Error("link mode must be symlink or hardlink".into()));
    }
    let value = if args["root"].is_null() {
        if kind != "output" {
            return Err(Error("source bindings require an explicit root".into()));
        }
        let existing=crate::observation::binding(store,profile,kind,owner).map_err(|_|Error("native embedding does not yet support creating an omitted output root; supply root explicitly".into()))?;
        crate::observation::checked_root(store, profile, &existing)?;
        existing
    } else {
        crate::observation::execute(store, profile, "bind", args)?
    };
    if let Some(mode) = mode {
        store.transaction(|db|{db.execute("INSERT INTO catalog_link_modes VALUES (?,?) ON CONFLICT(catalog) DO UPDATE SET mode=excluded.mode",[owner,mode])?;Ok(())})?;
    }
    Ok(value)
}
fn mapping_id(store: &Store, args: &Value) -> Result<String> {
    let namespace = uuid::Uuid::parse_str(&store.database_id).map_err(|e| Error(e.to_string()))?;
    Ok(uuid::Uuid::new_v5(
        &namespace,
        catabolic_core::encode(&json!([
            args["catalog"],
            args["file_id"],
            args["item_id"],
            args["path"]
        ]))
        .as_bytes(),
    )
    .to_string())
}
fn put_mapping(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    crate::mutation::require_recovered(&store.db)?;
    let catalog = text(args, "catalog")?;
    let file = text(args, "file_id")?;
    let item = text(args, "item_id")?;
    let path = text(args, "path")?;
    crate::layout::relative_path(path)?;
    let id = mapping_id(store, args)?;
    store.transaction(|db| {
        for (table, key) in [("catalogs", catalog), ("files", file), ("items", item)] {
            if !db
                .prepare(&format!("SELECT id FROM {table} WHERE id=?"))?
                .exists([key])?
            {
                return Err(Error(format!("unknown {table} ID: {key}")));
            }
        }
        for row in rows(
            db,
            "SELECT id,path FROM mappings WHERE catalog=? AND active=1",
            &[catalog.to_owned().into()],
        )? {
            let other = row["path"].as_str().unwrap();
            if row["id"] != id
                && (other == path
                    || other.starts_with(&format!("{path}/"))
                    || path.starts_with(&format!("{other}/")))
            {
                return Err(Error(format!(
                    "destination conflicts with active mapping: {other}"
                )));
            }
        }
        db.execute(
            "INSERT INTO mappings VALUES (?,?,?,?,?,1) ON CONFLICT(id) DO UPDATE SET active=1",
            rusqlite::params![id, catalog, file, item, path],
        )?;
        if !db
            .prepare("SELECT 1 FROM item_files WHERE file_id=? AND item_id=? AND active=1")?
            .exists([file, item])?
        {
            crate::curation::associate_in_transaction(
                store,
                profile,
                db,
                &json!({"file_id":file,"item_id":item}),
                "mapping",
            )?;
        }
        Ok(())
    })?;
    Ok(rows(&store.db, "SELECT * FROM mappings WHERE id=?", &[id.into()])?.remove(0))
}

fn query_bound(
    args: &Value,
    key: &str,
    default: usize,
    maximum: usize,
    message: &str,
) -> Result<usize> {
    args.get(key)
        .map_or(Some(default as u64), Value::as_u64)
        .filter(|n| (1..=maximum as u64).contains(n))
        .map(|n| n as usize)
        .ok_or_else(|| Error(message.into()))
}

thread_local! {
    static INTERRUPT: std::cell::RefCell<Option<Box<dyn FnMut() -> bool + Send>>> =
        std::cell::RefCell::new(None);
}
/// Poll the outer embedding cancellation bridge even when a bounded query owner
/// installs its own SQLite deadline handler. This is thread-local, not global.
pub(crate) fn poll_interrupt() -> bool {
    // Never retain a RefCell borrow while a Python signal callback runs: that
    // callback may legitimately use a different native Store on this thread.
    struct Restore(Option<Box<dyn FnMut() -> bool + Send>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            INTERRUPT.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let mut check = Restore(INTERRUPT.with(|slot| slot.borrow_mut().take()));
    check.0.as_mut().is_some_and(|check| check())
}
pub fn with_interrupt_check<T>(
    check: impl FnMut() -> bool + Send + 'static,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    struct Reset(Option<Box<dyn FnMut() -> bool + Send>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            INTERRUPT.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(INTERRUPT.with(|slot| slot.replace(Some(Box::new(check)))));
    action()
}
/// SQLite cancellation checks are installed and removed on this same owner.
pub fn interruptible<T>(
    store: &Store,
    check: impl FnMut() -> bool + Send + 'static,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_interrupt_check(check, || {
        store.db.progress_handler(1000, Some(poll_interrupt))?;
        struct Reset<'a>(&'a rusqlite::Connection);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                let _ = self.0.progress_handler(0, None::<fn() -> bool>);
            }
        }
        let _reset = Reset(&store.db);
        action()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> (tempfile::TempDir, Session) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        crate::migration::initialize(&path).unwrap();
        let owner = Session::open(&path, true, false).unwrap();
        (root, owner)
    }
    #[test]
    fn query_evaluation_lending_restores_error_and_unwind() {
        let (_root, mut owner) = session();
        let token = owner.begin().unwrap();
        let mut evaluation = crate::selection::Evaluation::unbound("default", 5000, 10000, false);
        let result: Result<()> = evaluation.with_store(&mut owner.store, |evaluation| {
            evaluation
                .store()
                .db
                .execute("INSERT INTO profiles VALUES ('lent')", [])?;
            Err(Error("ordinary evaluation failure".into()))
        });
        assert!(result.is_err());
        assert!(owner.store().unwrap().owns_writer_lock());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<()> =
                evaluation.with_store(&mut owner.store, |_| panic!("evaluation unwind"));
        }));
        assert!(result.is_err());
        assert!(owner.store().unwrap().owns_writer_lock());
        owner.unwind();
        owner.finish(token, false).unwrap();
        assert!(
            !owner
                .store()
                .unwrap()
                .db
                .prepare("SELECT 1 FROM profiles WHERE id='lent'")
                .unwrap()
                .exists([])
                .unwrap()
        );
    }
    #[test]
    fn query_interrupt_callback_can_reenter_independent_bridge() {
        with_interrupt_check(
            || with_interrupt_check(|| true, || Ok(poll_interrupt())).unwrap(),
            || {
                assert!(poll_interrupt());
                assert!(poll_interrupt());
                Ok(())
            },
        )
        .unwrap();
        assert!(!poll_interrupt());
    }
    #[test]
    fn transaction_tokens_cannot_escape_or_exit_out_of_order() {
        let (_root, mut owner) = session();
        let first = owner.begin().unwrap();
        let second = owner.begin().unwrap();
        assert!(owner.finish(first, true).is_err());
        owner.finish(second, false).unwrap();
        assert!(owner.check_scope(Some(second)).is_err());
        owner.finish(first, true).unwrap();
        assert!(!owner.in_transaction().unwrap());
    }
    #[test]
    fn command_preserves_blob_and_blocks_transaction_escape() {
        let (_root, mut owner) = session();
        let first = owner.begin().unwrap();
        let store = owner.store().unwrap();
        let value = command(
            store,
            "SELECT ? AS b, ? AS i",
            &json!([["bytes", [0, 255]], ["int", "9223372036854775807"]]),
        )
        .unwrap();
        assert_eq!(value["rows"][0][0], json!(["bytes", [0, 255]]));
        assert_eq!(value["rows"][0][1], json!(["int", "9223372036854775807"]));
        assert!(command(store, "COMMIT", &json!([])).is_err());
        assert!(command(store, "SELECT 1; SELECT 2", &json!([])).is_err());
        owner.finish(first, false).unwrap();
    }
    #[test]
    fn failed_runtime_detach_disables_embedding_commands() {
        let (_root, mut owner) = session();
        let path = owner.path.clone();
        let result = owner.store_mut().unwrap().detached(|| {
            std::fs::remove_file(&path)?;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!owner.store.as_ref().unwrap().owns_writer_lock());
        assert!(
            command(
                owner.store.as_ref().unwrap(),
                "CREATE TABLE escaped(n)",
                &json!([])
            )
            .is_err()
        );
        // The native command adapter replaces its own temporary authorizer.
        // It must never obtain the failed-detach placeholder connection.
        assert!(
            owner.store().is_err(),
            "failed detached owner remained available to command adapter"
        );
        owner.close().unwrap();
    }
    #[test]
    fn detached_replacement_retires_owner() {
        let (root, mut owner) = session();
        let replacement = root.path().join("replacement");
        crate::migration::initialize(&replacement).unwrap();
        owner.detach().unwrap();
        std::fs::rename(replacement, &owner.path).unwrap();
        assert!(owner.reconnect().unwrap_err().0.contains("replaced"));
        assert!(owner.store().is_err());
        owner.close().unwrap();
    }
}
