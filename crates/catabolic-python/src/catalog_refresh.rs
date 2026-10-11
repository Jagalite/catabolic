// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Borrow the persistent Python caller's Store, including lexical handle scopes.
//! Return the owner before propagating the exact Python cancellation exception.
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

pub fn enqueue(
    session: &mut Session,
    profile: &str,
    observation_job: Option<&str>,
    token: Option<u64>,
    signal: Arc<Mutex<Option<PyErr>>>,
) -> Result<()> {
    session.check_scope(token)?;
    let store = session.store()?;
    let cancel = AtomicBool::new(false);
    catabolic_store::embedding::interruptible(
        store,
        move || poll(&signal, &cancel),
        || catabolic_store::catalog_refresh::enqueue(&store.db, profile, observation_job),
    )
}

fn execute(
    store: &mut Store,
    profile: &str,
    operation: &str,
    args: &Value,
    cancel: Arc<AtomicBool>,
    check: impl FnMut() -> bool + Send + 'static,
) -> Result<Value> {
    store.db.progress_handler(1000, Some(check))?;
    struct Reset<'a>(&'a mut Store);
    impl Drop for Reset<'_> {
        fn drop(&mut self) {
            let _ = self.0.db.progress_handler(0, None::<fn() -> bool>);
        }
    }
    let reset = Reset(store);
    catabolic_runtime::process::with_cancellation(cancel, || {
        catabolic_runtime::catalog_refresh::execute_public(reset.0, profile, operation, args)
    })
}

pub fn call(
    session: &mut Session,
    profile: &str,
    operation: &str,
    args: &Value,
    signal: Arc<Mutex<Option<PyErr>>>,
) -> Result<Value> {
    let store = session.store_mut()?;
    let cancel = Arc::new(AtomicBool::new(false));
    // An active lexical transaction must remain on its owning thread.
    if !store.db.is_autocommit() {
        let captured = signal.clone();
        let checked = cancel.clone();
        return catabolic_store::embedding::with_interrupt_check(
            move || poll(&captured, &checked),
            || {
                let captured = signal.clone();
                let checked = cancel.clone();
                execute(store, profile, operation, args, cancel, move || {
                    poll(&captured, &checked)
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
                    let checked = worker_cancel.clone();
                    execute(store, profile, operation, args, worker_cancel, move || {
                        checked.load(Ordering::Acquire)
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
