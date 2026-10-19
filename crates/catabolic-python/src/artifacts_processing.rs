//! Synchronous Python signal bridge around borrowed native runtime owners.
//! The calling thread polls Python; work returns the same Store before any
//! captured exception escapes, including cancellation and worker unwinding.
use catabolic_store::{Result, Store, embedding::Session};
use pyo3::prelude::*;
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
fn poll(signal: &Arc<Mutex<Option<PyErr>>>, cancel: &AtomicBool) -> bool {
    if signal.lock().unwrap().is_some() {
        return true;
    }
    Python::attach(|py| {
        if let Err(error) = py.check_signals() {
            *signal.lock().unwrap() = Some(error);
            cancel.store(true, Ordering::Release);
        }
    });
    cancel.load(Ordering::Acquire)
}
fn execute(
    store: &mut Store,
    profile: &str,
    surface: &str,
    operation: &str,
    args: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    match surface {
        "artifacts" => {
            catabolic_runtime::public_artifacts::execute(store, profile, operation, args, cancel)
        }
        "processing" => {
            catabolic_runtime::public_processing::execute(store, profile, operation, args, cancel)
        }
        _ => unreachable!("checked persistent runtime surface"),
    }
}
pub fn call(
    session: &mut Session,
    profile: &str,
    surface: &str,
    operation: &str,
    args: &Value,
    signal: Arc<Mutex<Option<PyErr>>>,
) -> Result<Value> {
    let store = session.store_mut()?;
    let cancel = Arc::new(AtomicBool::new(false));
    if !store.db.is_autocommit() {
        let captured = signal.clone();
        let checked = cancel.clone();
        return catabolic_store::embedding::with_interrupt_check(
            move || poll(&captured, &checked),
            || {
                catabolic_runtime::process::with_cancellation(cancel.clone(), || {
                    execute(store, profile, surface, operation, args, cancel)
                })
            },
        );
    }
    std::thread::scope(|scope| {
        let worker_cancel = cancel.clone();
        let handle = scope.spawn(move || {
            let checked = worker_cancel.clone();
            catabolic_store::embedding::with_interrupt_check(
                move || checked.load(Ordering::Acquire),
                || {
                    catabolic_runtime::process::with_cancellation(worker_cancel.clone(), || {
                        execute(store, profile, surface, operation, args, worker_cancel)
                    })
                },
            )
        });
        while !handle.is_finished() {
            poll(&signal, &cancel);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let result = handle
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        poll(&signal, &cancel);
        result
    })
}
pub fn read(store: &Store, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    catabolic_store::mutation::profile(store, profile)?;
    let id = args["id"].as_str().unwrap_or("");
    match operation {
        "get" => catabolic_store::media_mutation::get_job(store, profile, id).map_err(|error| {
            if error.0 == "unknown processing job" {
                catabolic_store::Error("unknown job".into())
            } else {
                error
            }
        }),
        "cancel" => catabolic_store::media_mutation::cancel(store, profile, id),
        "retry" => catabolic_store::media_mutation::retry(store, profile, id),
        _ => catabolic_store::public_reads::processing(store, profile, operation, args).map_err(
            |error| {
                if error.0 == "unknown processing job" {
                    catabolic_store::Error("unknown job".into())
                } else {
                    error
                }
            },
        ),
    }
}
