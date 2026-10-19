// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Deferred Python option truth and signal transport for ordinary Layouts.run.
use catabolic_store::{Error, Result, embedding::Session, layout::RunFlag};
use pyo3::prelude::*;
use serde_json::Value;
use std::sync::{Arc, Mutex};

pub fn run(
    session: &mut Session,
    profile: &str,
    args: &Value,
    signal: Arc<Mutex<Option<PyErr>>>,
    flags: [Py<PyAny>; 3],
) -> Result<Value> {
    let checked = signal.clone();
    catabolic_store::embedding::with_interrupt_check(
        move || {
            if checked.lock().unwrap().is_some() {
                return true;
            }
            Python::attach(|py| match py.check_signals() {
                Ok(()) => false,
                Err(error) => {
                    *checked.lock().unwrap() = Some(error);
                    true
                }
            })
        },
        || {
            catabolic_store::public_layouts::run(session, profile, args, &mut |flag| {
                let index = match flag {
                    RunFlag::Apply => 0,
                    RunFlag::ReplaceLayout => 1,
                    RunFlag::AllowEmpty => 2,
                };
                Python::attach(|py| flags[index].bind(py).is_truthy()).map_err(|error| {
                    *signal.lock().unwrap() = Some(error);
                    Error("evaluation_cancelled".into())
                })
            })
        },
    )
}
