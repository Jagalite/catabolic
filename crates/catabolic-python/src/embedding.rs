// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! PyO3 ownership boundary. Every method returns the checked-out session even
//! on native errors/unwinds. No mutex is held while Python callbacks run.
use catabolic_store::{Error, Result, embedding::Session};
use pyo3::{create_exception, exceptions::PyRuntimeError, prelude::*};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};
create_exception!(_native, CatabolicError, PyRuntimeError);
fn error(value: Error) -> PyErr {
    CatabolicError::new_err(value.to_string())
}
fn lock<T>(value: &Mutex<T>) -> PyResult<MutexGuard<'_, T>> {
    value
        .try_lock()
        .map_err(|_| CatabolicError::new_err("native store is already in use"))
}
#[pyclass(module = "catabolic_native._native")]
pub struct NativeStore {
    session: Mutex<Option<Session>>,
}
struct Checkout<'a> {
    owner: &'a Mutex<Option<Session>>,
    session: Option<Session>,
}
impl<'a> Checkout<'a> {
    fn take(owner: &'a Mutex<Option<Session>>) -> PyResult<Self> {
        let session = lock(owner)?.take().ok_or_else(|| {
            CatabolicError::new_err(
                "native store is already in use (concurrent or reentrant access)",
            )
        })?;
        Ok(Self {
            owner,
            session: Some(session),
        })
    }
}
impl Drop for Checkout<'_> {
    fn drop(&mut self) {
        if let Some(session) = self.session.as_mut() {
            if std::thread::panicking() {
                session.unwind();
            } else {
                session.synchronize();
            }
        }
        if let Ok(mut owner) = self.owner.lock() {
            *owner = self.session.take();
        }
    }
}
impl NativeStore {
    fn run<T: Send>(
        &self,
        py: Python<'_>,
        action: impl FnOnce(&mut Session) -> Result<T> + Send,
    ) -> PyResult<T> {
        py.check_signals()?;
        self.cleanup(py, action)
    }
    // Cleanup must not be preempted by a pending signal before rollback/close.
    // SQLite operations use the explicit signal bridge below. PyO3 propagates
    // other pending signals after the native owner is returned to the session.
    fn cleanup<T: Send>(
        &self,
        py: Python<'_>,
        action: impl FnOnce(&mut Session) -> Result<T> + Send,
    ) -> PyResult<T> {
        let mut checked = Checkout::take(&self.session)?;
        let result = py
            .detach(|| action(checked.session.as_mut().unwrap()))
            .map_err(error);
        drop(checked);
        result
    }
    fn read(
        &self,
        py: Python<'_>,
        action: impl FnOnce(&catabolic_store::Store) -> Result<Value> + Send,
    ) -> PyResult<String> {
        let signal = Arc::new(Mutex::new(None));
        let captured = signal.clone();
        let value = self.run(py, move |session| {
            let store = session.store()?;
            catabolic_store::embedding::interruptible(
                store,
                move || {
                    Python::attach(|py| match py.check_signals() {
                        Ok(()) => false,
                        Err(error) => {
                            *captured.lock().unwrap() = Some(error);
                            true
                        }
                    })
                },
                || action(store),
            )
        });
        if let Some(error) = signal.lock().unwrap().take() {
            return Err(error);
        }
        value.map(|v| v.to_string())
    }
}
#[pymethods]
impl NativeStore {
    #[new]
    #[pyo3(signature=(path, writable=false, for_recovery=false))]
    fn new(py: Python<'_>, path: &str, writable: bool, for_recovery: bool) -> PyResult<Self> {
        Ok(Self {
            session: Mutex::new(Some(
                py.detach(|| Session::open(Path::new(path), writable, for_recovery))
                    .map_err(error)?,
            )),
        })
    }
    #[staticmethod]
    fn validate_query(surface: &str, request: &str, profile: &str) -> PyResult<String> {
        let args: Value = catabolic_core::decode_json(request)
            .map_err(|error| CatabolicError::new_err(error.to_string()))?;
        let result = if surface == "graphql" {
            catabolic_store::graphql::validate_before_context(
                args["document"].as_str().unwrap_or(""),
                &args["variables"],
                args["timeout_ms"].as_u64().unwrap_or(0),
                false,
            )
        } else {
            catabolic_store::sql::validate_request(
                args["sql"].as_str(),
                &catabolic_store::sql::Options {
                    profile,
                    params: args["params"].as_str(),
                    describe: args["describe"].as_bool().unwrap_or(false),
                    max_rows: args["max_rows"].as_u64().unwrap_or(0) as usize,
                    timeout_ms: args["timeout_ms"].as_u64().unwrap_or(0),
                    stable: args["_stable"].as_bool().unwrap_or(false),
                    http: args["_http"].as_bool().unwrap_or(false),
                },
            )
            .map(|_| None)
        };
        result
            .map(|value| value.unwrap_or(Value::Null).to_string())
            .map_err(error)
    }
    fn info(&self, py: Python<'_>) -> PyResult<String> {
        self.run(py, |s| Ok(s.info())).map(|v| v.to_string())
    }
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        self.cleanup(py, Session::close)
    }
    fn detach(&self, py: Python<'_>) -> PyResult<()> {
        self.run(py, Session::detach)
    }
    fn reconnect(&self, py: Python<'_>) -> PyResult<()> {
        self.cleanup(py, Session::reconnect)
    }
    fn begin(&self, py: Python<'_>) -> PyResult<u64> {
        self.run(py, Session::begin)
    }
    fn finish(&self, py: Python<'_>, token: u64, commit: bool) -> PyResult<()> {
        self.cleanup(py, |s| s.finish(token, commit))
    }
    fn in_transaction(&self, py: Python<'_>) -> PyResult<bool> {
        self.run(py, |s| s.in_transaction())
    }
    #[pyo3(signature=(token=None))]
    fn check_scope(&self, py: Python<'_>, token: Option<u64>) -> PyResult<()> {
        self.run(py, |s| s.check_scope(token))
    }
    #[pyo3(signature=(sql,args,token=None))]
    fn command(
        &self,
        py: Python<'_>,
        sql: &str,
        args: &str,
        token: Option<u64>,
    ) -> PyResult<String> {
        let args: Value = catabolic_core::decode_json(args)
            .map_err(|e| CatabolicError::new_err(e.to_string()))?;
        let signal = Arc::new(Mutex::new(None));
        let captured = signal.clone();
        let value = self.run(py, |session| {
            session.check_scope(token)?;
            let store = session.store()?;
            catabolic_store::embedding::interruptible(
                store,
                move || {
                    Python::attach(|py| match py.check_signals() {
                        Ok(()) => false,
                        Err(e) => {
                            *captured.lock().unwrap() = Some(e);
                            true
                        }
                    })
                },
                || catabolic_store::embedding::command(store, sql, &args),
            )
        });
        if let Some(error) = signal.lock().unwrap().take() {
            return Err(error);
        }
        value.map(|v| v.to_string())
    }
    fn call(
        &self,
        py: Python<'_>,
        surface: &str,
        operation: &str,
        request: &str,
        profile: &str,
    ) -> PyResult<String> {
        let raw_request = request;
        let request: Value = catabolic_core::decode_json(request)
            .map_err(|e| CatabolicError::new_err(e.to_string()))?;
        if !request.is_object() {
            return Err(CatabolicError::new_err(
                "native request must be a JSON object",
            ));
        }
        if surface == "query_execution" {
            let signal = Arc::new(Mutex::new(None));
            let captured = signal.clone();
            let value = self.run(py, |session| {
                catabolic_store::embedding::with_interrupt_check(
                    move || {
                        // A consumed Python exception remains cancellation for
                        // every later resolver/SQL poll in this native call.
                        if captured.lock().unwrap().is_some() {
                            return true;
                        }
                        Python::attach(|py| match py.check_signals() {
                            Ok(()) => false,
                            Err(error) => {
                                *captured.lock().unwrap() = Some(error);
                                true
                            }
                        })
                    },
                    || session.query_call_json(profile, operation, raw_request),
                )
            });
            if let Some(error) = signal.lock().unwrap().take() {
                return Err(error);
            }
            return value.map(|v| v.to_string());
        }
        if (surface == "artifacts" && ["enqueue", "run", "recover"].contains(&operation))
            || (surface == "processing" && ["enqueue", "run"].contains(&operation))
        {
            let signal = Arc::new(Mutex::new(None));
            let captured = signal.clone();
            let value = self.run(py, |session| {
                crate::artifacts_processing::call(
                    session, profile, surface, operation, &request, captured,
                )
            });
            if let Some(error) = signal.lock().unwrap().take() {
                return Err(error);
            }
            return value.map(|value| value.to_string());
        }
        if surface == "application" && operation == "scan" {
            return self
                .run(py, |session| {
                    catabolic_runtime::public_scan::execute(session.store_mut()?, profile, &request)
                })
                .map(|v| v.to_string());
        }
        self.read(py, |store| match surface {
            "artifacts" => {
                catabolic_runtime::public_artifacts::call(store, profile, operation, &request)
            }
            "processing" => crate::artifacts_processing::read(store, profile, operation, &request),
            "application" => {
                catabolic_store::embedding::application(store, profile, operation, &request)
            }
            "saved_queries" => {
                catabolic_store::definitions::dispatch(store, profile, operation, &request)
            }
            "query" => catabolic_store::query::CatalogQuery::new(store, profile)?
                .execute(operation, &request),
            "tagging" => catabolic_store::tagging::dispatch(store, profile, operation, &request),
            "item_workflow" => {
                catabolic_store::item_workflow::dispatch(store, profile, operation, &request)
            }
            "curation" => match operation {
                "snapshot" => catabolic_store::curation::proposal_snapshot(
                    store,
                    profile,
                    request["file_id"].as_str().unwrap_or(""),
                    &request["payload"],
                ),
                "occurrence" => catabolic_store::curation::occurrence(
                    store,
                    profile,
                    request["file_id"].as_str().unwrap_or(""),
                ),
                _ => catabolic_store::curation::dispatch(store, profile, operation, &request),
            },
            "media" => {
                catabolic_store::mutation::profile(store, profile)?;
                if operation == "associate_in_transaction" {
                    if store.db.is_autocommit() {
                        return Err(Error(
                            "associate_in_transaction requires an active native transaction".into(),
                        ));
                    }
                    Ok(json!(catabolic_store::curation::associate_in_transaction(
                        store,
                        profile,
                        &store.db,
                        &request,
                        request["origin"].as_str().unwrap_or("explicit")
                    )?))
                } else {
                    catabolic_store::curation::dispatch(store, profile, operation, &request)
                }
            }
            _ => Err(Error(format!(
                "unsupported native embedding service: {surface}"
            ))),
        })
    }
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<NativeStore>()?;
    module.add("CatabolicError", module.py().get_type::<CatabolicError>())?;
    Ok(())
}
