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
        let request: Value = catabolic_core::decode_json(request)
            .map_err(|e| CatabolicError::new_err(e.to_string()))?;
        if !request.is_object() {
            return Err(CatabolicError::new_err(
                "native request must be a JSON object",
            ));
        }
        if surface == "application" && operation == "scan" {
            return self
                .run(py, |session| scan(session.store_mut()?, profile, &request))
                .map(|v| v.to_string());
        }
        self.read(py, |store| match surface {
            "application" => {
                catabolic_store::embedding::application(store, profile, operation, &request)
            }
            "query" => catabolic_store::query::CatalogQuery::new(store, profile)?
                .execute(operation, &request),
            "tagging" => catabolic_store::tagging::dispatch(store, profile, operation, &request),
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
fn scan(store: &mut catabolic_store::Store, profile: &str, request: &Value) -> Result<Value> {
    catabolic_store::mutation::profile(store, profile)?;
    let mut args = request.clone();
    if request["scopes"].as_array().is_some_and(|a| !a.is_empty())
        && request["continue_id"].is_null()
    {
        return Err(Error(
            "--scope selects outstanding work; use --continue-request".into(),
        ));
    }
    let excludes = request["exclude"].as_array().cloned().unwrap_or_default();
    let continuation = !request["continue_id"].is_null();
    let resume = !request["request_id"].is_null();
    if continuation && (resume || !request["location"].is_null() || !excludes.is_empty()) {
        return Err(Error(
            "continuation uses the original source and exclusions".into(),
        ));
    }
    if resume
        && (request["extended"] == true
            || request["budgets"]
                .as_object()
                .is_some_and(|o| !o.is_empty())
            || request["scopes"].as_array().is_some_and(|a| !a.is_empty()))
    {
        return Err(Error(
            "use --continue-request to amend execution budgets".into(),
        ));
    }
    if resume && (!request["location"].is_null() || !excludes.is_empty()) {
        return Err(Error(
            "resuming a scan uses its recorded scope (source and exclusions)".into(),
        ));
    }
    args["exclude"] = json!(excludes);
    for key in ["budgets", "scopes", "continue_id", "request_id"] {
        if args[key].is_null()
            || key == "scopes" && args[key].as_array().is_some_and(|a| a.is_empty())
        {
            args.as_object_mut().unwrap().remove(key);
        }
    }
    if resume || continuation {
        return catabolic_runtime::observation::execute(store, profile, "scan", &args);
    }
    let sources = if let Some(source) = request["location"].as_str() {
        vec![source.to_owned()]
    } else {
        catabolic_store::migration::rows(&store.db, "SELECT id FROM locations ORDER BY id", &[])?
            .into_iter()
            .map(|r| r["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let mut scans = vec![];
    let mut observations = vec![];
    let mut complete = true;
    let mut started = 0;
    for source in &sources {
        args["source"] = json!(source);
        args["reuse_completed"] = json!(false);
        let value = catabolic_runtime::observation::execute(store, profile, "scan", &args)?;
        complete &= value["complete"] == true;
        started += 1;
        if let Some(v) = value["scans"].as_array() {
            scans.extend(v.iter().cloned());
        }
        if let Some(v) = value["observations"].as_array() {
            observations.extend(v.iter().cloned());
        }
        if observations.last().is_some_and(|r| {
            matches!(
                r["report"]["blocker"].as_str(),
                Some("temporary_storage_low" | "temporary_storage_budget")
            )
        }) {
            break;
        }
    }
    Ok(
        json!({"complete":complete,"scans":scans,"observations":observations,"unstarted_sources":&sources[started..]}),
    )
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<NativeStore>()?;
    module.add("CatabolicError", module.py().get_type::<CatabolicError>())?;
    Ok(())
}
