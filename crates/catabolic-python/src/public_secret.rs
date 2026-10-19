// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Python path/JSON/exception representation; the native owner performs effects.
use catabolic_store::{Error, Result, embedding::Session, private_secret};
use pyo3::{exceptions::PyOSError, prelude::*};
use std::{
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};

fn io_error(error: std::io::Error, path: &Path) -> PyErr {
    Python::attach(|py| {
        if let Some(errno) = error.raw_os_error()
            && let Ok(message) = py
                .import("os")
                .and_then(|os| os.call_method1("strerror", (errno,)))
            && let Ok(filename) = py
                .import("os")
                .and_then(|os| os.call_method1("fsdecode", (path.to_path_buf(),)))
        {
            return PyOSError::new_err((
                errno,
                message.extract::<String>().unwrap_or_default(),
                filename.unbind(),
            ));
        }
        error.into()
    })
}

pub fn save(
    session: &mut Session,
    path: &Path,
    value: Py<PyAny>,
    raised: Arc<Mutex<Option<PyErr>>>,
) -> Result<()> {
    let signal = raised.clone();
    let store = session.store()?;
    catabolic_store::embedding::interruptible(
        store,
        move || {
            Python::attach(|py| match py.check_signals() {
                Ok(()) => false,
                Err(error) => {
                    *signal.lock().unwrap() = Some(error);
                    true
                }
            })
        },
        || {
            let saved = private_secret::save(store, path, |stream| -> PyResult<()> {
                Python::attach(|py| {
                    // json.dump's default encoder preserves insertion order,
                    // ASCII escapes, nonfinite values and partial-write errors.
                    let chunks = py
                        .import("json")?
                        .getattr("_default_encoder")?
                        .call_method1("iterencode", (value.bind(py),))?;
                    for chunk in chunks.try_iter()? {
                        py.check_signals()?;
                        let chunk: String = chunk?.extract()?;
                        stream.write_all(chunk.as_bytes()).map_err(PyErr::from)?;
                    }
                    Ok(())
                })
            });
            match saved {
                Ok(()) => Ok(()),
                Err(private_secret::Failure::Native(error)) => Err(error),
                Err(private_secret::Failure::Io(error)) => {
                    *raised.lock().unwrap() = Some(io_error(error, path));
                    Err(Error("private credential file I/O failed".into()))
                }
                Err(private_secret::Failure::Serialization(error)) => {
                    *raised.lock().unwrap() = Some(error);
                    Err(Error("private credential JSON serialization failed".into()))
                }
            }
        },
    )
}
