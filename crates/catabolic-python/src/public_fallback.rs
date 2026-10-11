// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Python signals and native fallback owner lending; no Python decision engine.
use catabolic_store::{Error, Result, embedding::Session};
use pyo3::prelude::*;
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

fn poll(
    signal: &Arc<Mutex<Option<PyErr>>>,
    cancel: &AtomicBool,
    callback: Option<&Py<PyAny>>,
) -> bool {
    if signal.lock().unwrap().is_some() {
        cancel.store(true, Ordering::Release);
        return true;
    }
    Python::attach(|py| {
        let result = py
            .check_signals()
            .and_then(|()| callback.map_or(Ok(false), |f| f.bind(py).call0()?.is_truthy()));
        match result {
            Ok(true) => cancel.store(true, Ordering::Release),
            Err(error) => {
                *signal.lock().unwrap() = Some(error);
                cancel.store(true, Ordering::Release);
            }
            _ => {}
        }
    });
    cancel.load(Ordering::Acquire)
}
fn execute<T: Send>(
    inline: bool,
    signal: Arc<Mutex<Option<PyErr>>>,
    callback: Option<Py<PyAny>>,
    operation: impl FnOnce(Arc<AtomicBool>) -> Result<T> + Send,
) -> Result<T> {
    let cancel = Arc::new(AtomicBool::new(false));
    if poll(&signal, &cancel, None) {
        return Err(Error("evaluation_cancelled".into()));
    }
    if inline {
        let captured = signal.clone();
        let checked = cancel.clone();
        return catabolic_store::embedding::with_interrupt_check(
            move || poll(&captured, &checked, callback.as_ref()),
            || catabolic_runtime::process::with_cancellation(cancel.clone(), || operation(cancel)),
        );
    }
    std::thread::scope(|scope| {
        let checked = cancel.clone();
        let worker = scope.spawn(move || {
            let interrupted = checked.clone();
            catabolic_store::embedding::with_interrupt_check(
                move || interrupted.load(Ordering::Acquire),
                || {
                    catabolic_runtime::process::with_cancellation(checked.clone(), || {
                        operation(checked)
                    })
                },
            )
        });
        while !worker.is_finished() {
            poll(&signal, &cancel, callback.as_ref());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let result = worker
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        if result.is_ok() && poll(&signal, &cancel, callback.as_ref()) {
            return Err(Error("evaluation_cancelled".into()));
        }
        result
    })
}
pub fn call(
    session: &mut Session,
    profile: &str,
    operation: &str,
    args: &Value,
    signal: Arc<Mutex<Option<PyErr>>>,
    cancelled: Option<Py<PyAny>>,
) -> Result<Value> {
    let inline = !session.store()?.db.is_autocommit();
    execute(inline, signal, cancelled, |_| {
        catabolic_runtime::public_fallback::call(session, profile, operation, args)
    })
}
#[pyfunction]
fn fallback_present(raw: &str) -> PyResult<String> {
    let value = catabolic_core::decode_json(raw)
        .map_err(|e| crate::embedding::CatabolicError::new_err(e.to_string()))?;
    if !value.is_object() {
        return Err(crate::embedding::CatabolicError::new_err(
            "projection plan must be an object",
        ));
    }
    Ok(catabolic_store::fallback_projection::present(&value).to_string())
}
#[pyfunction]
fn fallback_tick(py: Python<'_>, path: &str, profile: &str, raw: &str) -> PyResult<String> {
    let args = catabolic_core::decode_json(raw)
        .map_err(|e| crate::embedding::CatabolicError::new_err(e.to_string()))?;
    let signal = Arc::new(Mutex::new(None));
    let captured = signal.clone();
    let result = py.detach(|| {
        execute(false, captured, None, |cancel| {
            catabolic_runtime::public_fallback::tick(
                std::path::Path::new(path),
                profile,
                &args,
                &cancel,
            )
        })
    });
    if let Some(error) = signal.lock().unwrap().take() {
        return Err(error);
    }
    result
        .map(|v| v.to_string())
        .map_err(|e| crate::embedding::CatabolicError::new_err(e.to_string()))
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(fallback_present, module)?)?;
    module.add_function(wrap_pyfunction!(fallback_tick, module)?)?;
    Ok(())
}
