// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Receipt-owned render admission only. This never starts an encoder or adopts
//! output bytes; execution/publication remain with the existing artifact owner.
use crate::{Error, Result, Store, artifact_mutation, components, observation, source};
use catabolic_core::catalog_command::CommandEnvelope;
use catabolic_core::catalog_identity::ExactFileRevision;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnqueueRequest {
    pub profile: String,
    pub source: ExactFileRevision,
    pub source_ctime_ns: i64,
    pub item_id: String,
    pub recipe_id: String,
    pub recipe_digest: String,
    pub output_location: String,
    pub output_binding: Value,
    pub capabilities: Value,
}
/// Independently authorized exact preparation, not decoded from an HTTP request.
/// Capabilities must come from trusted tool discovery before recording intent.
#[derive(Clone, Debug)]
pub struct ProcessingEnqueueAuthority {
    pub host_namespace: String,
    pub request: EnqueueRequest,
}

pub(crate) fn enqueue(
    store: &Store,
    command: &CommandEnvelope,
    request: &EnqueueRequest,
    authority: &ProcessingEnqueueAuthority,
) -> Result<Value> {
    if store.db.is_autocommit()
        || authority.host_namespace != command.key().host_namespace()
        || authority.request != *request
        || !request.output_binding.is_object()
        || !request.capabilities.is_object()
    {
        return Err(Error(
            "processing enqueue authority or transaction mismatch".into(),
        ));
    }
    request
        .source
        .validate()
        .map_err(|e| Error(e.to_string()))?;
    let snapshot = components::occurrence(&store.db, &request.profile, &request.source.file_id)?;
    if components::revision(&snapshot) != request.source.revision
        || source::validate(&snapshot)?["ctime_ns"].as_i64() != Some(request.source_ctime_ns)
    {
        return Err(Error("processing enqueue exact source changed".into()));
    }
    let recipe = artifact_mutation::get_recipe(store, &request.recipe_id)?;
    if recipe["digest"].as_str() != Some(request.recipe_digest.as_str()) {
        return Err(Error("processing enqueue recipe identity changed".into()));
    }
    let binding =
        observation::binding(store, &request.profile, "source", &request.output_location)?;
    if binding != request.output_binding {
        return Err(Error("processing enqueue output binding changed".into()));
    }
    // This API has no tool probing or detached work. Its nested transaction is
    // a savepoint: failure to write the outer command receipt rolls back the job.
    let admitted = artifact_mutation::enqueue_request(
        store,
        &request.profile,
        &request.source.file_id,
        &request.recipe_id,
        &request.output_location,
        &request.item_id,
        &request.capabilities,
        None,
    )?;
    Ok(json!({"operation":"processing_enqueue","admission":admitted,"request":request}))
}
