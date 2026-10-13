// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Transport only: one writer owner, shared cancellation, frozen progress lines.
use super::text;
use catabolic_store::{Error, Result, Store};
use serde_json::Value;
use std::{path::Path, sync::atomic::Ordering};

pub fn execute(args: &Value) -> Result<Value> {
    let args = args.clone();
    catabolic_http::interruptible(move |cancel| {
        let mut store = Store::open(Path::new(text(&args, "db")), true, false)?;
        catabolic_store::mutation::profile(&store, text(&args, "profile"))?;
        let check = cancel.clone();
        store
            .db
            .progress_handler(1000, Some(move || check.load(Ordering::Acquire)))?;
        let check = cancel.clone();
        let result = catabolic_store::embedding::with_interrupt_check(
            move || check.load(Ordering::Acquire),
            || {
                catabolic_runtime::process::with_cancellation(cancel.clone(), || {
                    catabolic_runtime::maintenance::run_with_progress(
                        &mut store,
                        text(&args, "profile"),
                        &args,
                        cancel.clone(),
                        &mut |value| {
                            // Matches json.dumps with insertion order stage, complete.
                            let line = format!(
                                "{{\"stage\": {}, \"complete\": {}}}",
                                value["stage"], value["complete"]
                            );
                            super::super::transport::progress("stderr", &format!("{line}\n"), true)
                        },
                    )
                })
            },
        );
        if cancel.load(Ordering::Acquire) {
            Err(Error(catabolic_runtime::maintenance::INTERRUPTED.into()))
        } else {
            result
        }
    })
}
