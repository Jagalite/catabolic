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
    let callback = callback.map(Arc::new);
    if poll(&signal, &cancel, None) {
        return Err(Error("evaluation_cancelled".into()));
    }
    let result = if inline {
        let captured = signal.clone();
        let checked = cancel.clone();
        let observed = callback.clone();
        catabolic_store::embedding::with_interrupt_check(
            move || poll(&captured, &checked, observed.as_deref()),
            || {
                catabolic_runtime::process::with_cancellation(cancel.clone(), || {
                    operation(cancel.clone())
                })
            },
        )
    } else {
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
                poll(&signal, &cancel, callback.as_deref());
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        })
    };
    if result.is_ok() && poll(&signal, &cancel, callback.as_deref()) {
        return Err(Error("evaluation_cancelled".into()));
    }
    result
}

/// Keep an allocated native plan owned through native JSON serialization.
/// Native cancellation, a latched PyErr, or unwind drops this guard instead.
/// Python JSON decoding and facade allocation happen after this handoff.
pub struct Reply {
    value: Value,
    prepared: Option<(catabolic_store::public_fallback::PlanReleases, u64)>,
}
impl Reply {
    pub fn into_json(mut self) -> String {
        let result = self.value.to_string();
        self.prepared = None;
        result
    }
    fn release(&mut self) -> Result<()> {
        if let Some((releases, token)) = self.prepared.take() {
            releases.forget(token)?;
        }
        Ok(())
    }
}
impl Drop for Reply {
    fn drop(&mut self) {
        // Poisoned registries already fail closed for all plan operations.
        let _ = self.release();
    }
}

/// Python owns this lease before decoding the reply or allocating its mapping.
/// Returning/serializing the payload never disarms it. The facade explicitly
/// releases failed deliveries, including failures retained by a traceback.
#[pyclass(module = "catabolic_native._native")]
pub struct PreparedReply {
    reply: Reply,
}
impl PreparedReply {
    pub fn new(reply: Reply) -> Self {
        Self { reply }
    }
}
#[pymethods]
impl PreparedReply {
    fn json(&self) -> String {
        self.reply.value.to_string()
    }
    fn release(&mut self) -> PyResult<()> {
        self.reply
            .release()
            .map_err(|error| crate::embedding::CatabolicError::new_err(error.to_string()))
    }
}
pub fn call(
    session: &mut Session,
    profile: &str,
    operation: &str,
    args: &Value,
    signal: Arc<Mutex<Option<PyErr>>>,
    cancelled: Option<Py<PyAny>>,
) -> Result<Reply> {
    let inline = !session.store()?.db.is_autocommit();
    if cancelled.is_some()
        && matches!(
            operation,
            "resolver-resolve"
                | "resolver-capture"
                | "projection-bind"
                | "projection-plan"
                | "projection-run"
        )
    {
        session.check_evaluation(profile, args)?;
    }
    execute(inline, signal, cancelled, |_| {
        let value = catabolic_runtime::public_fallback::call(session, profile, operation, args)?;
        let prepared = if operation == "projection-plan" {
            value["token"]
                .as_u64()
                .map(|token| (session.fallback_plans().releases(), token))
        } else {
            None
        };
        Ok(Reply { value, prepared })
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
    let args = crate::admitted_json(raw)
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
    module.add_class::<PreparedReply>()?;
    module.add_function(wrap_pyfunction!(fallback_present, module)?)?;
    module.add_function(wrap_pyfunction!(fallback_tick, module)?)?;
    Ok(())
}
