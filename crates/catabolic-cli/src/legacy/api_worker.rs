// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Preserved CLI transport for the existing bounded API-worker supervisor.
//! No administration, ingress listener or authorization policy is added here.
use catabolic_store::{Error, Result};
use serde_json::Value;
use std::{path::Path, sync::atomic::Ordering, time::Duration};

pub fn run(path: &Path, profile: &str, once: bool) -> Result<Value> {
    let (path, profile) = (path.to_owned(), profile.to_owned());
    crate::signals::interruptible(move |cancel| {
        let result = catabolic_runtime::http_worker::run(
            &path,
            &profile,
            once,
            Duration::from_secs(2),
            cancel.clone(),
        );
        // The native supervisor has joined the notifier and removed its lease
        // before the legacy transport restores the ordinary interrupt result.
        if cancel.load(Ordering::Acquire) {
            Err(Error("native legacy CLI interrupted".into()))
        } else {
            result
        }
    })
}
