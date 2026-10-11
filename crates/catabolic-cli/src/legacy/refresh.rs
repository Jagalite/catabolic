// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen Jellyfin refresh CLI leaves; no catalog-refresh or identify routing.
use super::text;
use catabolic_store::{Error, Result, Store, legacy_refresh};
use serde_json::Value;
use std::path::Path;

pub fn execute(args: &Value) -> Result<Value> {
    if text(args, "operation") == "run" {
        let args = args.clone();
        return catabolic_http::interruptible(move |cancel| {
            let mut store = Store::open(Path::new(text(&args, "db")), true, false)?;
            let checked = cancel.clone();
            catabolic_store::embedding::with_interrupt_check(
                move || checked.load(std::sync::atomic::Ordering::Acquire),
                || {
                    catabolic_runtime::process::with_cancellation(cancel.clone(), || {
                        catabolic_runtime::legacy_refresh::run(
                            &mut store,
                            text(&args, "profile"),
                            &args,
                            &std::env::current_exe()?,
                            &cancel,
                        )
                    })
                },
            )
        });
    }
    let operation = text(args, "operation");
    let store = Store::open(Path::new(text(args, "db")), operation != "list", false)?;
    let profile = text(args, "profile");
    catabolic_store::mutation::profile(&store, profile)?;
    match operation {
        "configure" => legacy_refresh::configure(&store, profile, args),
        "list" => legacy_refresh::list(&store, profile, args),
        "retry" => legacy_refresh::retry(&store, profile, args),
        _ => Err(Error("unknown legacy refresh action".into())),
    }
}
