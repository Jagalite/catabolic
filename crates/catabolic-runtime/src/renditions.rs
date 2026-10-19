// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! One API rendition worker step. The outer supervisor owns singleton locking,
//! playback and notifications. SQL selection belongs solely to the store owner.
use catabolic_store::{
    Result, Store, artifact_mutation,
    rendition_mutation::{self, WorkerSelection},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

pub fn tick(path: &Path, profile: &str, capabilities: &Value) -> Result<Value> {
    tick_with_cancel(
        path,
        profile,
        capabilities,
        Arc::new(AtomicBool::new(false)),
    )
}
pub fn tick_with_cancel(
    path: &Path,
    profile: &str,
    capabilities: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    let mut store = Store::open(path, true, false)?;
    tick_store(&mut store, profile, capabilities, cancel)
}
pub fn tick_store(
    store: &mut Store,
    profile: &str,
    capabilities: &Value,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    let WorkerSelection::Selected(id) =
        rendition_mutation::select_work(store, profile, capabilities)?
    else {
        return Ok(json!({"processed":0}));
    };
    artifact_mutation::recover(store, profile, 100)?;
    rendition_mutation::resume_work(store, &id)?;
    crate::artifacts::run(store, profile, 1, Some(&[id]), cancel)
}

pub fn logical_admit(
    access: &mut catabolic_store::access::Access,
    body: &Value,
    key: &str,
    capabilities: &Value,
) -> Result<Value> {
    let mut probe = crate::fallback::probe;
    rendition_mutation::logical_admit(access, body, key, capabilities, &mut probe)
}
pub fn logical_resolve(
    access: &mut catabolic_store::access::Access,
    item: &str,
    body: &Value,
) -> Result<Value> {
    let mut probe = crate::fallback::probe;
    catabolic_store::fallback_resolution::resolve_access(access, item, body, None, &mut probe)
}
