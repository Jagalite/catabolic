// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen versioned target descriptions. Application scanner acceptance is separate.
use crate::{Error, Result};
use serde_json::Value;

fn resource() -> Value {
    serde_json::from_str(include_str!("../resources/targets.json")).expect("frozen targets")
}

pub fn definitions() -> Value {
    resource()["definitions"].take()
}

pub fn describe(target: Option<&str>) -> Result<Value> {
    let mut description = resource()["description"].take();
    if let Some(target) = target {
        let entries = description["targets"].as_array_mut().unwrap();
        entries.retain(|entry| entry["id"] == target);
        if entries.is_empty() {
            return Err(Error(format!("unknown target: {target}; use target list")));
        }
    }
    Ok(description)
}
