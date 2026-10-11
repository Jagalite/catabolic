// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Detached native observation owner; requester timeouts never cancel shared work.
use catabolic_store::{Error, Result, Store};
use serde_json::{Value, json};
use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub fn worker(path: &Path, profile: &str, request: &Value) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Store::open(path, true, false) {
            Ok(mut store) => {
                return catabolic_store::observation::execute_detached(
                    &mut store, profile, "scan", request,
                );
            }
            Err(error)
                if error.0.contains("another Catabolic writer") && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(error) => return Err(error),
        }
    }
}
pub fn execute(store: &mut Store, profile: &str, action: &str, request: &Value) -> Result<Value> {
    if action != "scan" || request["isolate"] != true {
        return catabolic_store::observation::execute_detached(store, profile, action, request);
    }
    let binary = crate::helper::executable()?;

    let timeout = request
        .get("wait_seconds")
        .map(|v| {
            v.as_f64()
                .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 30.0)
                .ok_or_else(|| Error("invalid observation wait timeout".into()))
        })
        .transpose()?
        .unwrap_or(30.0);
    let mut child_request = request.clone();
    child_request["isolate"] = json!(false);
    if !child_request["requester"].is_string() {
        child_request["requester"] = json!(format!("native:{}", uuid::Uuid::new_v4()));
    }
    let path = store.path.clone();
    let output = tempfile::tempfile()?;
    let mut reader = output.try_clone()?;
    let (mut child, status) = store.detached(|| {
        let mut child = Command::new(binary)
            .arg("--db")
            .arg(&path)
            .args([
                "--profile",
                profile,
                "observation-worker",
                &catabolic_core::encode(&child_request),
            ])
            .stdout(Stdio::from(output.try_clone()?))
            .stderr(Stdio::from(output))
            .spawn()?;
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok((child, Some(status)));
            }
            if started.elapsed() >= Duration::from_secs_f64(timeout) {
                return Ok((child, None));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    })?;
    if let Some(status) = status {
        reader.seek(SeekFrom::Start(0))?;
        let mut data = String::new();
        (&mut reader)
            .take(8 * 1024 * 1024)
            .read_to_string(&mut data)?;
        if !status.success() {
            return Err(Error(
                data.trim()
                    .strip_prefix("catabolic: ")
                    .unwrap_or(data.trim())
                    .to_string(),
            ));
        }
        return catabolic_core::decode_json(&data).map_err(Into::into);
    }
    // Reap opportunistically while this host remains alive; dropping a waiter
    // does not signal the independent worker, whose PID and guard are durable.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let existing = catabolic_store::observation::request_id(
        store,
        profile,
        child_request["source"].as_str().unwrap_or("media"),
        child_request["requester"].as_str().unwrap(),
    )?;
    let Some(existing) = existing else {
        return Err(Error("observation_pending".into()));
    };
    catabolic_store::observation::execute_detached(
        store,
        profile,
        "scan",
        &json!({"request_id":existing}),
    )
}
