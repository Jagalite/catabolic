// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Local administration forwarding, owner lending, and Python signal transport.
//! Validation, SQL, entropy, preflight authority, and transactions stay in Store.
use catabolic_store::{Error, Result, access_admin, embedding::Session};
use pyo3::{
    exceptions::{PyTypeError, PyValueError},
    prelude::*,
    types::{PyDict, PyInt, PyList, PyString},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn poll(signal: &Arc<Mutex<Option<PyErr>>>) -> bool {
    if signal.lock().unwrap().is_some() {
        return true;
    }
    Python::attach(|py| match py.check_signals() {
        Ok(()) => false,
        Err(error) => {
            *signal.lock().unwrap() = Some(error);
            true
        }
    })
}

pub fn call(
    session: &mut Session,
    operation: &str,
    args: &Value,
    signal: Arc<Mutex<Option<PyErr>>>,
) -> Result<Value> {
    // Do not move an active lexical transaction to another thread. The enclosing
    // PyO3 checkout lends this exact Session and restores it on every exit.
    let checked = signal.clone();
    session
        .store()?
        .db
        .progress_handler(1000, Some(move || poll(&checked)))?;
    struct Reset<'a>(&'a mut Session);
    impl Drop for Reset<'_> {
        fn drop(&mut self) {
            self.0.clear_interrupt_handler();
        }
    }
    let reset = Reset(session);
    let result = catabolic_store::embedding::with_interrupt_check(
        move || poll(&signal),
        || {
            Ok(match operation {
                "principal_put" => access_admin::principal_put_detailed(
                    reset.0.store()?,
                    &args["profile"],
                    &args["identifier"],
                ),
                "grant_put" => access_admin::grant_put_detailed(
                    reset.0,
                    &args["principal"],
                    &args["definition"],
                    args.get("identifier"),
                ),
                "issue" => access_admin::issue_detailed(
                    reset.0.store()?,
                    &args["principal"],
                    &args["grants"],
                    &args["ttl"],
                ),
                _ => Err(Error(format!(
                    "unsupported native access administration operation: {operation}"
                ))
                .into()),
            })
        },
    )?;
    // Only the owner's explicit AccessError codes become the preserved public
    // exception. Owner-state, SQLite, and query errors retain their native type.
    match result {
        Ok(value) => Ok(json!({"value": value})),
        Err(access_admin::Failure::Access(error)) => {
            Ok(json!({"error": {"code": error.code(), "status": error.status()}}))
        }
        Err(access_admin::Failure::Native(error)) => Err(error),
        Err(access_admin::Failure::Boundary(error)) => Err(error.into_error()),
    }
}

// Preserve Python container/exact-int distinctions at the representation
// boundary. These are type translations, not a second validation/SQL owner.
// Unsupported scalar objects never undergo lossy repr/string conversion.
fn json_value(value: &Bound<'_, PyAny>) -> PyResult<Value> {
    let py = value.py();
    let json = py.import("json")?;
    let options = PyDict::new(py);
    options.set_item("allow_nan", false)?;
    let raw: String = json
        .getattr("dumps")?
        .call((value,), Some(&options))?
        .extract()?;
    catabolic_core::decode_json(&raw).map_err(|error| PyTypeError::new_err(error.to_string()))
}
fn strict_value(value: &Bound<'_, PyAny>) -> PyResult<Value> {
    strict_value_at(value, 0)
}
fn strict_value_at(value: &Bound<'_, PyAny>, depth: usize) -> PyResult<Value> {
    // Cyclic/deep Python containers must not overflow the native stack. Their
    // exact CPython diagnostics are outside this selected representation gate.
    if depth >= 128 {
        return Err(PyValueError::new_err(
            "native access argument nesting exceeds 128",
        ));
    }
    use pyo3::types::{PyBool, PyFloat};
    if value.is_none()
        || value.is_instance_of::<PyString>()
        || value.is_instance_of::<PyBool>()
        || value.is_instance_of::<PyInt>()
        || value.is_instance_of::<PyFloat>()
    {
        return json_value(value);
    }
    if let Ok(values) = value.cast::<PyList>() {
        return values
            .iter()
            .map(|v| strict_value_at(&v, depth + 1))
            .collect::<PyResult<Vec<_>>>()
            .map(Value::Array);
    }
    if let Ok(values) = value.cast::<PyDict>() {
        let mut result = serde_json::Map::new();
        for (key, value) in values.iter() {
            if !key.is_instance_of::<PyString>() {
                return Err(PyTypeError::new_err(
                    "native access transport requires string mapping keys",
                ));
            }
            result.insert(key.extract()?, strict_value_at(&value, depth + 1)?);
        }
        return Ok(Value::Object(result));
    }
    Err(PyTypeError::new_err(format!(
        "unsupported native access argument type: {}",
        value.get_type().name()?
    )))
}
fn definition(value: &Bound<'_, PyAny>) -> PyResult<Value> {
    let Ok(fields) = value.cast::<PyDict>() else {
        return Ok(Value::Null);
    };
    let mut result = serde_json::Map::new();
    for (key, value) in fields.iter() {
        if !key.is_instance_of::<PyString>() {
            return Ok(Value::Null);
        }
        let key: String = key.extract()?;
        let translated = match key.as_str() {
            "actions"
            | "item_ids"
            | "file_ids"
            | "occurrence_ids"
            | "metadata_fields"
            | "operation_ids"
            | "projection_ids"
            | "report_ids"
            | "fallback_policy_ids"
            | "callback_origins" => match value.cast::<PyList>() {
                Ok(values) => Value::Array(
                    values
                        .iter()
                        .map(|value| {
                            if value.is_instance_of::<PyString>() {
                                value.extract().map(Value::String)
                            } else {
                                Ok(Value::Null)
                            }
                        })
                        .collect::<PyResult<Vec<_>>>()?,
                ),
                Err(_) => Value::Null,
            },
            "revisions" => match value.cast::<PyDict>() {
                Ok(values)
                    if values
                        .keys()
                        .iter()
                        .all(|key| key.is_instance_of::<PyString>()) =>
                {
                    let mut revisions = serde_json::Map::new();
                    for (key, value) in values.iter() {
                        revisions.insert(
                            key.extract()?,
                            if value.is_instance_of::<PyString>() {
                                Value::String(value.extract()?)
                            } else {
                                Value::Null
                            },
                        );
                    }
                    Value::Object(revisions)
                }
                _ => Value::Null,
            },
            "operator" | "derivatives" => {
                if value.is_exact_instance_of::<pyo3::types::PyBool>() {
                    json_value(&value)?
                } else {
                    Value::Null
                }
            }
            "query_id" => strict_value(&value)?,
            // Unknown fields are rejected by the owner before their values.
            _ => Value::Null,
        };
        result.insert(key, translated);
    }
    Ok(Value::Object(result))
}
pub fn arguments(operation: &str, args: &Bound<'_, PyDict>) -> PyResult<Value> {
    let mut result = serde_json::Map::new();
    for (key, value) in args.iter() {
        let key: String = key.extract()?;
        let translated = match (operation, key.as_str()) {
            ("principal_put", "identifier") if !value.is_instance_of::<PyString>() => Value::Null,
            ("grant_put", "definition") => definition(&value)?,
            ("issue", "grants") if !value.is_instance_of::<PyList>() => Value::Null,
            ("issue", "ttl") if !value.is_exact_instance_of::<PyInt>() => Value::Null,
            _ => strict_value(&value)?,
        };
        result.insert(key, translated);
    }
    Ok(Value::Object(result))
}
