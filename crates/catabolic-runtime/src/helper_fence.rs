// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Shared native companion admission fence; schema32 envelopes stay unchanged.
use catabolic_store::{Error, Result, Store};
use serde_json::{Value, json};
use std::os::unix::fs::MetadataExt;

/// Private helper protocol. Schema32 keeps its unfenced legacy envelopes;
/// native catalogs require this versioned lifetime identity before admission.
const NATIVE_FENCE_FIELD: &str = "_native_catalog_fence";
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeFence {
    schema: u32,
    database_id: String,
    incarnation: String,
    device: u64,
    inode: u64,
}
impl NativeFence {
    pub(crate) fn capture(store: &Store) -> Result<Option<Self>> {
        if !catabolic_store::migration::is_supported_native_schema(store.schema_version) {
            return Ok(None);
        }
        store.validate_native_identity()?;
        let metadata = std::fs::symlink_metadata(&store.path)?;
        let fence = Self {
            schema: store.schema_version,
            database_id: store.database_id.clone(),
            incarnation: store
                .catalog_incarnation
                .clone()
                .ok_or_else(|| Error("native helper requires catalog incarnation".into()))?,
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        // Reject a pathname change while taking the envelope snapshot too.
        store.validate_native_identity()?;
        Ok(Some(fence))
    }
    fn value(&self) -> Value {
        json!({"version":1,"schema":self.schema,"catalog_id":self.database_id,
            "incarnation":self.incarnation,"device":self.device,"inode":self.inode})
    }
    fn parse(value: &Value) -> Result<Self> {
        let invalid = || Error("invalid native helper fence".into());
        let fields = value.as_object().ok_or_else(invalid)?;
        if fields.len() != 6 || fields.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(Error(
                "unsupported native helper fence version or fields".into(),
            ));
        }
        let schema = fields
            .get("schema")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(invalid)?;
        let database_id = fields
            .get("catalog_id")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?
            .to_owned();
        let incarnation = fields
            .get("incarnation")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?
            .to_owned();
        let device = fields
            .get("device")
            .and_then(Value::as_u64)
            .ok_or_else(invalid)?;
        let inode = fields
            .get("inode")
            .and_then(Value::as_u64)
            .ok_or_else(invalid)?;
        Ok(Self {
            schema,
            database_id,
            incarnation,
            device,
            inode,
        })
    }
}
pub(crate) fn write(store: &Store, request: &mut Value) -> Result<Option<NativeFence>> {
    let fence = NativeFence::capture(store)?;
    let object = request
        .as_object_mut()
        .ok_or_else(|| Error("helper request must be an object".into()))?;
    if let Some(fence) = &fence {
        object.insert("_native_catalog_id".into(), json!(fence.database_id));
        object.insert(NATIVE_FENCE_FIELD.into(), fence.value());
    } else {
        // A caller cannot smuggle a native fence into a legacy supervised spawn.
        object.remove(NATIVE_FENCE_FIELD);
    }
    Ok(fence)
}
pub(crate) fn check(store: &Store, request: &Value) -> Result<()> {
    let current = NativeFence::capture(store)?;
    let expected = request
        .get(NATIVE_FENCE_FIELD)
        .map(NativeFence::parse)
        .transpose()?;
    if current != expected {
        return Err(Error(
            "helper native lifetime identity mismatch or missing fence".into(),
        ));
    }
    Ok(())
}

/// Reject unsupported versioned routes before opening any catalog.
pub(crate) fn require(request: &Value) -> Result<()> {
    let value = request
        .get(NATIVE_FENCE_FIELD)
        .ok_or_else(|| Error("native helper requires a versioned fence".into()))?;
    let expected = NativeFence::parse(value)?;
    if !catabolic_store::migration::is_supported_native_schema(expected.schema) {
        return Err(Error("native helper fence has unsupported schema".into()));
    }
    Ok(())
}
