// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Preserved consumer watch transport. A private, same-executable tick owns the
//! existing consumer and notification owners; the supervisor owns cancellation.
//! No catalog handle survives a tick or an idle interval. Killing an interrupted
//! tick leaves its durable leases to the owners' ordinary restart protocol.
use super::text;
use catabolic_store::{Error, Result};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const WORKER_ARGUMENT: &str = "__legacy-consumer-watch-tick";
const MAXIMUM: usize = 4 * 1024 * 1024;

fn interrupted() -> Error {
    Error("native legacy CLI interrupted".into())
}

fn tick(args: &Value) -> Result<Value> {
    let limit = args["limit"]
        .as_u64()
        .filter(|n| (1..=100).contains(n))
        .ok_or_else(|| Error("invalid_configuration".into()))?;
    let path = Path::new(text(args, "db"));
    let profile = text(args, "profile");
    let mut report = catabolic_runtime::api::execute(
        path,
        profile,
        "consumers",
        "run",
        &json!({"limit":limit}),
    )?;
    report["notifications"] = catabolic_runtime::api::execute(
        path,
        profile,
        "notify",
        "run",
        &json!({"limit":limit.min(100)}),
    )?;
    Ok(report)
}

/// Private protocol, not a registered companion or a Python fallback. Credentials
/// remain in the inherited environment, never in the request or command line.
pub fn worker_main() {
    let result = (|| -> Result<Value> {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(MAXIMUM as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAXIMUM {
            return Err(Error(
                "consumer watch tick input exceeds byte budget".into(),
            ));
        }
        let args = catabolic_core::decode_json_bytes(&bytes)?;
        if !args.is_object() || !args["db"].is_string() || !args["profile"].is_string() {
            return Err(Error("invalid_configuration".into()));
        }
        tick(&args)
    })();
    let value = match result {
        Ok(report) => json!({"report":report}),
        Err(error) => json!({"error":error.0}),
    };
    let _ = writeln!(std::io::stdout(), "{}", catabolic_core::encode(&value));
}

fn run_tick(args: &Value, executable: &Path, cancel: &AtomicBool) -> Result<Value> {
    let input = catabolic_core::encode(&json!({
        "db":args["db"], "profile":args["profile"], "limit":args["limit"]
    }));
    if input.len() > MAXIMUM {
        return Err(Error(
            "consumer watch tick input exceeds byte budget".into(),
        ));
    }
    let result = catabolic_runtime::process::command_output(
        &[executable.as_os_str(), OsStr::new(WORKER_ARGUMENT)],
        catabolic_runtime::process::CommandOptions {
            input_bytes: Some(input.as_bytes()),
            cancel: Some(cancel),
            // Consumer and notification owners already bound each remote call.
            // This extra ceiling only bounds an unexpectedly wedged whole tick.
            timeout: Duration::from_secs(4 * 60 * 60),
            maximum: MAXIMUM,
            ..Default::default()
        },
    );
    if cancel.load(Ordering::Acquire) {
        return Err(interrupted());
    }
    let output = result.map_err(|error| Error(format!("consumer watch tick {}", error.state)))?;
    if output.returncode != 0 || !output.stderr.is_empty() {
        return Err(Error("consumer watch tick failed".into()));
    }
    let value = catabolic_core::decode_json_bytes(&output.stdout)?;
    if let Some(error) = value["error"].as_str() {
        return Err(Error(error.to_owned()));
    }
    value
        .get("report")
        .filter(|report| report.is_object())
        .cloned()
        .ok_or_else(|| Error("consumer watch tick returned an invalid report".into()))
}

pub fn execute(args: &Value) -> Result<Value> {
    let interval = args["interval"]
        .as_u64()
        .filter(|n| (1..=60).contains(n))
        .ok_or_else(|| Error("invalid_configuration".into()))?;
    // The CLI accepts 1..1000 generally, but the frozen drain owner accepts
    // only 1..100. Preserve that refusal before touching the database.
    if !args["limit"]
        .as_u64()
        .is_some_and(|n| (1..=100).contains(&n))
    {
        return Err(Error("invalid_configuration".into()));
    }
    let executable = std::env::current_exe()?;
    let args = args.clone();
    catabolic_http::interruptible(move |cancel| {
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(interrupted());
            }
            let report = run_tick(&args, &executable, &cancel)?;
            if cancel.load(Ordering::Acquire) {
                return Err(interrupted());
            }
            // Watch always emits one JSON line, regardless of --json. Machine mode
            // wraps that line and reflects consumer completeness, not notifications.
            let line = super::super::presentation::consumer_watch_line(&args, report);
            {
                super::super::transport::write("stdout", &format!("{line}\n"), true)?;
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
