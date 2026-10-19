// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Signal propagation for persistent public watcher and invalidation adapters.
use catabolic_store::{Result, embedding::Session};
use pyo3::prelude::*;
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

fn poll(signal: &Arc<Mutex<Option<PyErr>>>, cancel: &AtomicBool) -> bool {
    if signal.lock().unwrap().is_some() {
        cancel.store(true, Ordering::Release);
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

pub fn call(
    session: &mut Session,
    profile: &str,
    operation: &str,
    args: &Value,
    signal: Arc<Mutex<Option<PyErr>>>,
) -> Result<Value> {
    // Validate the current owner/thread before moving an idle owner or polling.
    let inline = !session.store()?.db.is_autocommit();
    let cancel = Arc::new(AtomicBool::new(false));
    if inline {
        let checked = cancel.clone();
        return catabolic_store::embedding::with_interrupt_check(
            move || poll(&signal, &checked),
            || {
                catabolic_runtime::process::with_cancellation(cancel, || {
                    catabolic_runtime::public_watchers::call(session, profile, operation, args)
                })
            },
        );
    }
    std::thread::scope(|scope| {
        let worker_cancel = cancel.clone();
        let worker = scope.spawn(move || {
            let checked = worker_cancel.clone();
            catabolic_store::embedding::with_interrupt_check(
                move || checked.load(Ordering::Acquire),
                || {
                    catabolic_runtime::process::with_cancellation(worker_cancel, || {
                        catabolic_runtime::public_watchers::call(session, profile, operation, args)
                    })
                },
            )
        });
        while !worker.is_finished() {
            poll(&signal, &cancel);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let result = worker
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        poll(&signal, &cancel);
        result
    })
}
