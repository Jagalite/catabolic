// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Preserved candidate intake; acceptance stays a separate proposal command.
use super::text;
use catabolic_store::{Result, Store};
use serde_json::Value;
use std::path::Path;

pub fn execute(args: &Value) -> Result<Value> {
    let args = args.clone();
    catabolic_http::interruptible(move |cancel| {
        let mut store = Store::open(Path::new(text(&args, "db")), args["apply"] == true, false)?;
        let checked = cancel.clone();
        catabolic_store::embedding::with_interrupt_check(
            move || checked.load(std::sync::atomic::Ordering::Acquire),
            || {
                catabolic_runtime::process::with_cancellation(cancel.clone(), || {
                    catabolic_runtime::identify::execute(
                        &mut store,
                        text(&args, "profile"),
                        &args,
                        &std::env::current_exe()?,
                        &cancel,
                    )
                })
            },
        )
    })
}
