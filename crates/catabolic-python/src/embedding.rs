// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! PyO3 ownership boundary. Every method returns the checked-out session even
//! on native errors/unwinds. No mutex is held while Python callbacks run.
use catabolic_store::{Error, ErrorClass, Result, embedding::Session};
use pyo3::{
    create_exception,
    exceptions::{PyRuntimeError, PyTypeError},
    prelude::*,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};
create_exception!(_native, CatabolicError, PyRuntimeError);
fn error(value: Error) -> PyErr {
    CatabolicError::new_err(value.to_string())
}
fn detailed_error(value: catabolic_store::DetailedError) -> PyErr {
    match value.class() {
        ErrorClass::CatabolicError => error(value.into_error()),
        ErrorClass::TypeError => PyTypeError::new_err(value.to_string()),
    }
}
fn lock<T>(value: &Mutex<T>) -> PyResult<MutexGuard<'_, T>> {
    value
        .try_lock()
        .map_err(|_| CatabolicError::new_err("native store is already in use"))
}
#[pyclass(module = "catabolic_native._native")]
pub struct NativeStore {
    session: Mutex<Option<Session>>,
    fallback_releases: catabolic_store::public_fallback::PlanReleases,
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
        self.read_value(py, action).map(|value| value.to_string())
    }
    fn read_value<T: Send>(
        &self,
        py: Python<'_>,
        action: impl FnOnce(&catabolic_store::Store) -> Result<T> + Send,
    ) -> PyResult<T> {
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
        value
    }
}
#[pymethods]
impl NativeStore {
    #[new]
    #[pyo3(signature=(path, writable=false, for_recovery=false))]
    fn new(py: Python<'_>, path: &str, writable: bool, for_recovery: bool) -> PyResult<Self> {
        let mut session = py
            .detach(|| Session::open(Path::new(path), writable, for_recovery))
            .map_err(error)?;
        let fallback_releases = session.fallback_plans().releases();
        Ok(Self {
            session: Mutex::new(Some(session)),
            fallback_releases,
        })
    }
    fn publisher(
        &self,
        py: Python<'_>,
        profile: &str,
        job: &str,
    ) -> PyResult<crate::public_publisher::NativePublisher> {
        let job = crate::admitted_json(job).map_err(|e| CatabolicError::new_err(e.to_string()))?;
        self.run(py, |session| {
            catabolic_store::observation::Publisher::new(session.store()?, profile, &job)
        })
        .map(|owner| crate::public_publisher::NativePublisher { owner })
    }
    fn scan_with_publisher(
        &self,
        py: Python<'_>,
        profile: &str,
        request: &str,
        app: Py<PyAny>,
    ) -> PyResult<String> {
        let request =
            crate::admitted_json(request).map_err(|e| CatabolicError::new_err(e.to_string()))?;
        let captured = Arc::new(Mutex::new(None));
        let mut factory = crate::public_publisher::Factory {
            app,
            error: captured.clone(),
        };
        let result = self.run(py, |session| {
            catabolic_runtime::public_scan::execute_with(
                session.store_mut()?,
                profile,
                &request,
                |store, profile, args| {
                    catabolic_store::observation::execute_detached_with_publisher(
                        store,
                        profile,
                        "scan",
                        args,
                        Some(&mut factory),
                    )
                },
            )
        });
        if let Some(error) = captured.lock().unwrap().take() {
            return Err(error);
        }
        result.map(|v| v.to_string())
    }
    // A Python plan destructor may run while this Store is checked out, closed
    // or detached. Release only its process-local handle, without a new checkout
    // or signal/callback poll that could discard cleanup.
    fn drop_fallback_plan(&self, token: u64) -> PyResult<()> {
        self.fallback_releases.forget(token).map_err(error)
    }
    #[pyo3(signature=(request, profile, cancelled=None))]
    fn prepare_fallback(
        &self,
        py: Python<'_>,
        request: &str,
        profile: &str,
        cancelled: Option<Py<PyAny>>,
    ) -> PyResult<crate::public_fallback::PreparedReply> {
        let request: Value =
            crate::admitted_json(request).map_err(|e| CatabolicError::new_err(e.to_string()))?;
        if !request.is_object() {
            return Err(CatabolicError::new_err(
                "native request must be a JSON object",
            ));
        }
        if cancelled.is_some() && request.get("session").is_none() {
            return Err(CatabolicError::new_err(
                "live cancellation is supported only for component or fallback session lookups",
            ));
        }
        let signal = Arc::new(Mutex::new(None));
        let captured = signal.clone();
        let value = self.run(py, |session| {
            crate::public_fallback::call(
                session,
                profile,
                "projection-plan",
                &request,
                captured,
                cancelled,
            )
        });
        if let Some(error) = signal.lock().unwrap().take() {
            return Err(error);
        }
        value.map(crate::public_fallback::PreparedReply::new)
    }
    #[staticmethod]
    fn validate_query(surface: &str, request: &str, profile: &str) -> PyResult<String> {
        let args: Value = crate::admitted_json(request)
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
    /// `Reconciler.apply`/`recover`. The optional `after_filesystem` callable
    /// receives each operation as a dict after its filesystem effect and
    /// before acknowledgement. Its exception propagates unchanged, leaving
    /// the operation journaled. The Store stays checked out meanwhile, so the
    /// callback cannot reenter it.
    #[pyo3(signature=(operation, request, profile, after_filesystem=None))]
    fn reconcile_publish(
        &self,
        py: Python<'_>,
        operation: &str,
        request: &str,
        profile: &str,
        after_filesystem: Option<Py<PyAny>>,
    ) -> PyResult<String> {
        let request: Value =
            crate::admitted_json(request).map_err(|e| CatabolicError::new_err(e.to_string()))?;
        if !request.is_object() {
            return Err(CatabolicError::new_err(
                "native request must be a JSON object",
            ));
        }
        let raised: Arc<Mutex<Option<PyErr>>> = Arc::new(Mutex::new(None));
        let captured = raised.clone();
        let value = self.run(py, |session| {
            let store = session.store()?;
            let observe = |operation: &Value| -> Result<()> {
                let Some(callback) = after_filesystem.as_ref() else {
                    return Ok(());
                };
                Python::attach(|py| -> PyResult<()> {
                    // Frozen Python tests `if after_filesystem:` per operation,
                    // so a falsey callable is skipped and __bool__ errors raise.
                    if !callback.bind(py).is_truthy()? {
                        return Ok(());
                    }
                    let operation = py
                        .import("json")?
                        .call_method1("loads", (operation.to_string(),))?;
                    callback.bind(py).call1((operation,))?;
                    Ok(())
                })
                .map_err(|error| {
                    *captured.lock().unwrap() = Some(error);
                    Error("after_filesystem callback raised".into())
                })
            };
            catabolic_store::public_reconcile::publish(
                store, profile, operation, &request, &observe,
            )
        });
        if let Some(error) = raised.lock().unwrap().take() {
            return Err(error);
        }
        value.map(|v| v.to_string())
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
        let args: Value =
            crate::admitted_json(args).map_err(|e| CatabolicError::new_err(e.to_string()))?;
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
    #[pyo3(signature=(profile, observation_job=None, token=None))]
    fn catalog_refresh_enqueue(
        &self,
        py: Python<'_>,
        profile: &str,
        observation_job: Option<&str>,
        token: Option<u64>,
    ) -> PyResult<()> {
        let signal = Arc::new(Mutex::new(None));
        let captured = signal.clone();
        let value = self.run(py, |session| {
            crate::catalog_refresh::enqueue(session, profile, observation_job, token, captured)
        });
        if let Some(error) = signal.lock().unwrap().take() {
            return Err(error);
        }
        value
    }
    fn access_admin(
        &self,
        py: Python<'_>,
        operation: &str,
        request: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<String> {
        let request = crate::public_access::arguments(operation, request)?;
        catabolic_store::filesystem_mutation::admit_failpoints(&request)
            .map_err(|e| CatabolicError::new_err(e.to_string()))?;
        let signal = Arc::new(Mutex::new(None));
        let captured = signal.clone();
        let value = self.run(py, |session| {
            crate::public_access::call(session, operation, &request, captured)
        });
        if let Some(error) = signal.lock().unwrap().take() {
            return Err(error);
        }
        value.map(|value| value.to_string())
    }
    fn save_secret(
        &self,
        py: Python<'_>,
        filename: &Bound<'_, PyAny>,
        result: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let path = py
            .import("pathlib")?
            .getattr("Path")?
            .call1((filename,))?
            .call_method0("expanduser")?
            .call_method0("absolute")?;
        let native_path: std::path::PathBuf = path.extract()?;
        // Test-only failpoint fields are refused before the file or its
        // registration exists, as every other embedding request is.
        if let Ok(fields) = result.cast::<pyo3::types::PyDict>() {
            for key in ["failpoint", "crash"] {
                if fields.contains(key)? {
                    catabolic_store::filesystem_mutation::admit_failpoints(
                        &serde_json::json!({ key: null }),
                    )
                    .map_err(|e| CatabolicError::new_err(e.to_string()))?;
                }
            }
        }
        let serialized = result.clone().unbind();
        let raised = Arc::new(Mutex::new(None));
        let captured = raised.clone();
        let outcome = self.run(py, |session| {
            crate::public_secret::save(session, &native_path, serialized, captured)
        });
        if let Some(error) = raised.lock().unwrap().take() {
            return Err(error);
        }
        outcome?;
        // Frozen ordering: missing summary fields fail after registration.
        // Return original Python values, without JSON round-trip coercion.
        let summary = pyo3::types::PyDict::new(py);
        summary.set_item("token_id", result.get_item("token_id")?)?;
        summary.set_item("expires", result.get_item("expires")?)?;
        summary.set_item("credential_file", path.str()?)?;
        Ok(summary.into_any().unbind())
    }
    #[allow(clippy::too_many_arguments)]
    fn layout_run(
        &self,
        py: Python<'_>,
        request: &str,
        profile: &str,
        apply: Py<PyAny>,
        replace_layout: Py<PyAny>,
        allow_empty: Py<PyAny>,
        limit: &Bound<'_, PyAny>,
    ) -> PyResult<String> {
        // Preserve exact-int admission without calling __int__/__index__ or
        // serializing arbitrary-size invalid Python integers through JSON.
        let limit = if limit.is_exact_instance_of::<pyo3::types::PyInt>() {
            limit.extract::<u64>().ok()
        } else {
            None
        };
        let mut request: Value =
            crate::admitted_json(request).map_err(|e| CatabolicError::new_err(e.to_string()))?;
        if !request.is_object() {
            return Err(CatabolicError::new_err(
                "native request must be a JSON object",
            ));
        }
        request["limit"] = limit.map_or(Value::Null, Value::from);
        let signal = Arc::new(Mutex::new(None));
        let captured = signal.clone();
        let result = self.run(py, |session| {
            crate::public_layouts::run(
                session,
                profile,
                &request,
                captured,
                [apply, replace_layout, allow_empty],
            )
        });
        if let Some(error) = signal.lock().unwrap().take() {
            return Err(error);
        }
        result.map(|value| value.to_string())
    }
    #[pyo3(signature=(surface, operation, request, profile, cancelled=None))]
    fn call(
        &self,
        py: Python<'_>,
        surface: &str,
        operation: &str,
        request: &str,
        profile: &str,
        cancelled: Option<Py<PyAny>>,
    ) -> PyResult<String> {
        let raw_request = request;
        let request: Value =
            crate::admitted_json(request).map_err(|e| CatabolicError::new_err(e.to_string()))?;
        if !request.is_object() {
            return Err(CatabolicError::new_err(
                "native request must be a JSON object",
            ));
        }
        if cancelled.is_some()
            && !((surface == "components" && operation == "get" || surface == "fallback")
                && request.get("session").is_some())
        {
            return Err(CatabolicError::new_err(
                "live cancellation is supported only for component or fallback session lookups",
            ));
        }
        if surface == "fallback" {
            let signal = Arc::new(Mutex::new(None));
            let captured = signal.clone();
            let value = self.run(py, |session| {
                crate::public_fallback::call(
                    session, profile, operation, &request, captured, cancelled,
                )
            });
            if let Some(error) = signal.lock().unwrap().take() {
                return Err(error);
            }
            return value.map(crate::public_fallback::Reply::into_json);
        }
        if surface == "watchers" {
            let signal = Arc::new(Mutex::new(None));
            let captured = signal.clone();
            let value = self.run(py, |session| {
                crate::public_watchers::call(session, profile, operation, &request, captured)
            });
            if let Some(error) = signal.lock().unwrap().take() {
                return Err(error);
            }
            return value.map(|value| value.to_string());
        }
        if surface == "reconcile" && operation == "owner" {
            return self.run(py, |session| {
                let info = session.info();
                Ok(catabolic_store::public_reconcile::owner(
                    info["database_id"].as_str().unwrap_or(""),
                    profile,
                    request["catalog"]
                        .as_str()
                        .ok_or_else(|| Error("catalog must be text".into()))?,
                )
                .to_string())
            });
        }
        if surface == "catalog_refresh" {
            let signal = Arc::new(Mutex::new(None));
            let captured = signal.clone();
            let value = self.run(py, |session| {
                crate::catalog_refresh::call(session, profile, operation, &request, captured)
            });
            if let Some(error) = signal.lock().unwrap().take() {
                return Err(error);
            }
            return value.map(|value| value.to_string());
        }
        if surface == "query_execution" || surface == "components" || surface == "layouts" {
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
                        Python::attach(|py| {
                            let checked = py.check_signals().and_then(|()| {
                                cancelled
                                    .as_ref()
                                    .map_or(Ok(false), |check| check.bind(py).call0()?.is_truthy())
                            });
                            match checked {
                                Ok(cancelled) => cancelled,
                                Err(error) => {
                                    *captured.lock().unwrap() = Some(error);
                                    true
                                }
                            }
                        })
                    },
                    || {
                        if surface == "layouts" {
                            catabolic_store::public_layouts::call(
                                session, profile, operation, &request,
                            )
                        } else if surface == "components" {
                            session.component_call(profile, operation, &request)
                        } else {
                            session.query_call_json(profile, operation, raw_request)
                        }
                    },
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
        if surface == "query" {
            // Carry the detailed failure through the ordinary checkout/signal
            // owner; convert it only after that owner has restored the session.
            let detailed = self.read_value(py, |store| {
                Ok((|| -> catabolic_store::DetailedResult<Value> {
                    catabolic_store::query::CatalogQuery::new(store, profile)?
                        .execute_detailed(operation, &request)
                })())
            })?;
            return detailed
                .map(|value| value.to_string())
                .map_err(detailed_error);
        }
        self.read(py, |store| match surface {
            "content_access" if operation == "revision_of" => {
                catabolic_store::content_access::revision_of(
                    store,
                    profile,
                    request["file_id"]
                        .as_str()
                        .ok_or_else(|| catabolic_store::Error("file_id must be text".into()))?,
                )
                .map(Value::String)
            }
            "artifacts" => {
                catabolic_runtime::public_artifacts::call(store, profile, operation, &request)
            }
            "processing" => crate::artifacts_processing::read(store, profile, operation, &request),
            "reconcile" => {
                catabolic_store::public_reconcile::call(store, profile, operation, &request)
            }
            "application" => {
                catabolic_store::embedding::application(store, profile, operation, &request)
            }
            "saved_queries" => {
                catabolic_store::definitions::dispatch(store, profile, operation, &request)
            }
            "fallback_policies" => {
                catabolic_store::fallback_mutation::execute(store, profile, operation, &request)
            }
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
    module.add_class::<crate::public_publisher::NativePublisher>()?;
    module.add("CatabolicError", module.py().get_type::<CatabolicError>())?;
    Ok(())
}
