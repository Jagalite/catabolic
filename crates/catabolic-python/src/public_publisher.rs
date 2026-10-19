// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Owned, short-checkout publication bridge. Python hooks run without a borrowed
//! Publisher or a held mutex; their original exceptions survive native cleanup.
use catabolic_store::{
    Error, Result,
    observation::{Publisher, PublisherCallbacks, PublisherFactory},
};
use pyo3::{create_exception, prelude::*};
create_exception!(
    _native,
    NativePublicationFailure,
    crate::embedding::CatabolicError
);
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[pyclass(module = "catabolic_native._native")]
pub struct NativePublisher {
    pub owner: Publisher,
}

#[pymethods]
impl NativePublisher {
    fn call(&self, py: Python<'_>, operation: &str, request: &str) -> PyResult<String> {
        let request: Value = crate::admitted_json(request)
            .map_err(|e| crate::embedding::CatabolicError::new_err(e.to_string()))?;
        py.check_signals()?;
        py.detach(|| match operation {
            "state" => self.owner.state(),
            "initial_scopes" => self.owner.initial_scopes(request.as_i64().unwrap_or(0)),
            "entry" => self.owner.append(request).map(|v| json!(v)),
            "directory" => self
                .owner
                .directory(
                    request["path"]
                        .as_str()
                        .ok_or_else(|| Error("directory path must be text".into()))?,
                    request["state"]
                        .as_str()
                        .ok_or_else(|| Error("directory state must be text".into()))?,
                    &request["detail"],
                )
                .map(|v| json!(v)),
            "checkpoint" => self.owner.checkpoint().map(|v| json!(v)),
            "stop" => self
                .owner
                .stop(
                    request
                        .as_str()
                        .ok_or_else(|| Error("stop reason must be text".into()))?,
                )
                .map(|_| Value::Null),
            "flush" => self.owner.flush(None).map(|_| Value::Null),
            "root_identity" => {
                let pair = if request.is_null() {
                    None
                } else {
                    Some((
                        request[0]
                            .as_u64()
                            .ok_or_else(|| Error("invalid root identity".into()))?,
                        request[1]
                            .as_u64()
                            .ok_or_else(|| Error("invalid root identity".into()))?,
                    ))
                };
                self.owner.set_root_identity(pair).map(|_| Value::Null)
            }
            _ => Err(Error(format!("unknown publisher operation: {operation}"))),
        })
        .map(|v| v.to_string())
        .map_err(|e| {
            if operation == "flush" {
                NativePublicationFailure::new_err(e.to_string())
            } else {
                crate::embedding::CatabolicError::new_err(e.to_string())
            }
        })
    }
}

pub struct Factory {
    pub app: Py<PyAny>,
    pub error: Arc<Mutex<Option<PyErr>>>,
}
struct Callbacks {
    halted: bool,
    publisher: Py<PyAny>,
    error: Arc<Mutex<Option<PyErr>>>,
}
fn remember<T>(error: &Arc<Mutex<Option<PyErr>>>, value: PyResult<T>) -> Result<T> {
    value.map_err(|failure| {
        let message = Python::attach(|py| {
            failure
                .value(py)
                .str()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "publisher callback failed".into())
        });
        *error.lock().unwrap() = Some(failure);
        Error(message)
    })
}
impl PublisherFactory for Factory {
    fn create(
        &mut self,
        owner: Publisher,
        job: Value,
        binding: Value,
    ) -> Result<Box<dyn PublisherCallbacks>> {
        let publisher = remember(
            &self.error,
            Python::attach(|py| {
                let native = Py::new(py, NativePublisher { owner })?;
                let class = py
                    .import("catabolic_native.scan_execution")?
                    .getattr("Publisher")?;
                class
                    .call_method1(
                        "_from_native",
                        (
                            self.app.bind(py),
                            native,
                            job.to_string(),
                            binding.to_string(),
                        ),
                    )
                    .map(Bound::unbind)
            }),
        )?;
        Ok(Box::new(Callbacks {
            publisher,
            halted: false,
            error: self.error.clone(),
        }))
    }
}
impl Callbacks {
    fn invoke(&mut self, method: &str, value: Value) -> Result<()> {
        if self.failed() {
            return Err(Error("publisher callback failed".into()));
        }
        let result: PyResult<()> = Python::attach(|py| {
            py.check_signals()?;
            self.publisher
                .bind(py)
                .call_method1(method, (value.to_string(),))?;
            Ok(())
        });
        if let Err(failure) = result {
            self.halted = true;
            let stopped = Python::attach(|py| -> PyResult<bool> {
                let class = py
                    .import("catabolic_native.scan_execution")?
                    .getattr("PublicationStopped")?;
                failure.value(py).is_instance(&class)
            })
            .unwrap_or(false);
            let cause = if stopped {
                Python::attach(|py| failure.cause(py))
            } else {
                None
            };
            let failure = cause.unwrap_or(failure);
            if Python::attach(|py| failure.is_instance_of::<NativePublicationFailure>(py)) {
                let message = Python::attach(|py| {
                    failure
                        .value(py)
                        .str()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| "publication stopped".into())
                });
                Err(Error(message))
            } else {
                remember(&self.error, Err(failure))
            }
        } else {
            Ok(())
        }
    }
}
impl PublisherCallbacks for Callbacks {
    fn directory(&mut self, path: &str, state: &str, detail: &Value) -> Result<()> {
        self.invoke("_directory_json", json!([path, state, detail]))
    }
    fn entry(&mut self, entry: Value) -> Result<()> {
        self.invoke("_entry_json", entry)
    }
    fn failed(&self) -> bool {
        self.halted
    }
    fn exceptional(&self) -> bool {
        self.error.lock().unwrap().is_some()
    }
}
