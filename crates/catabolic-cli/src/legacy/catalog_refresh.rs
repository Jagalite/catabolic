// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen catalog-refresh adapter. Each watch tick opens and closes one writer;
//! the interval is idle, cancellation-aware, and never holds a catalog lock.
use super::text;
use catabolic_store::{Error, Result, Store};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn once(args: &Value, operation: &str, cancel: Arc<AtomicBool>) -> Result<Value> {
    let mut store = Store::open(Path::new(text(args, "db")), operation != "pending", false)?;
    catabolic_store::mutation::profile(&store, text(args, "profile"))?;
    if matches!(operation, "pending" | "run" | "watch")
        && !args["limit"]
            .as_u64()
            .is_some_and(|v| (1..=1000).contains(&v))
    {
        return Err(Error("limit must be between 1 and 1000".into()));
    }
    let checked = cancel.clone();
    store
        .db
        .progress_handler(1000, Some(move || checked.load(Ordering::Acquire)))?;
    let mut request = json!({});
    for key in ["catalog", "limit", "force", "max_removals"] {
        if !args[key].is_null() {
            request[key] = args[key].clone();
        }
    }
    let action = if matches!(operation, "enable" | "disable") {
        request["enabled"] = json!(operation == "enable");
        "configure"
    } else if operation == "watch" {
        "run"
    } else {
        operation
    };
    let checked = cancel.clone();
    catabolic_store::embedding::with_interrupt_check(
        move || checked.load(Ordering::Acquire),
        || {
            catabolic_runtime::process::with_cancellation(cancel, || {
                catabolic_runtime::catalog_refresh::execute(
                    &mut store,
                    text(args, "profile"),
                    action,
                    &request,
                )
            })
        },
    )
}

fn interrupted() -> Error {
    Error("native legacy CLI interrupted".into())
}

pub fn execute(args: &Value) -> Result<Value> {
    let operation = text(args, "operation").to_owned();
    let interval = if operation == "watch" {
        if !args["limit"]
            .as_u64()
            .is_some_and(|v| (1..=1000).contains(&v))
        {
            return Err(Error("limit must be between 1 and 1000".into()));
        }
        Some(
            args["interval"]
                .as_u64()
                .filter(|v| (1..=60).contains(v))
                .ok_or_else(|| Error("watch interval must be 1..60 seconds".into()))?,
        )
    } else {
        None
    };
    let args = args.clone();
    catabolic_http::interruptible(move |cancel| {
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(interrupted());
            }
            let report = once(&args, &operation, cancel.clone());
            if cancel.load(Ordering::Acquire) {
                return Err(interrupted());
            }
            let Some(interval) = interval else {
                return report;
            };
            let report = report.unwrap_or_else(|error| json!({"complete":false,"errors":[{"error":error.0.chars().take(4000).collect::<String>()}]}));
            if report["processed"].as_u64().unwrap_or(0) > 0
                || report["errors"].as_array().is_some_and(|v| !v.is_empty())
            {
                // Python json.dumps defaults and the frozen report insertion order.
                let keys = if report.get("processed").is_some() {
                    &["processed", "refreshed", "errors", "pending", "complete"][..]
                } else {
                    &["complete", "errors"][..]
                };
                let fields = keys
                    .iter()
                    .map(|key| {
                        format!(
                            "\"{key}\": {}",
                            super::super::presentation::json_text(&report[key], None, true, 0)
                        )
                    })
                    .collect::<Vec<_>>();
                super::super::transport::write(
                    "stdout",
                    &format!("{{{}}}\n", fields.join(", ")),
                    true,
                )?;
            }
            let deadline = Instant::now() + Duration::from_secs(interval);
            while Instant::now() < deadline {
                if cancel.load(Ordering::Acquire) {
                    return Err(interrupted());
                }
                std::thread::sleep(
                    Duration::from_millis(20)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
        }
    })
}
