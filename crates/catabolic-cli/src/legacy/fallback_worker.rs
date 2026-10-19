// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Preserved fallback worker transport over the existing native tick owner.
use catabolic_store::{Error, Result};
use serde_json::Value;
use std::{path::Path, sync::atomic::Ordering, time::Duration};

pub fn run(path: &Path, profile: &str, once: bool) -> Result<Value> {
    let (path, profile) = (path.to_owned(), profile.to_owned());
    crate::signals::interruptible(move |cancel| {
        let result = catabolic_runtime::public_fallback::run(
            &path,
            &profile,
            once,
            Duration::from_secs(1),
            &cancel,
        );
        // run has dropped both the tick owner and singleton before delivery.
        if cancel.load(Ordering::Acquire) {
            Err(Error("native legacy CLI interrupted".into()))
        } else {
            result
        }
    })
}
