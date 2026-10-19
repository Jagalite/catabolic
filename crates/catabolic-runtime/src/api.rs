// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Opt-in local operation boundary shared by the native executable and wheel.
//! There is no fallback to another engine after admission or a failed effect.
use catabolic_store::{Error, Result, Store};
use serde_json::Value;
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

fn required<'a>(request: &'a Value, key: &str) -> Result<&'a str> {
    request[key]
        .as_str()
        .ok_or_else(|| Error(format!("{key} must be text")))
}
fn identifiers(request: &Value, key: &str) -> Result<Vec<String>> {
    request[key]
        .as_array()
        .ok_or_else(|| Error(format!("{key} must be an array of IDs")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error(format!("{key} must be an array of IDs")))
        })
        .collect()
}

fn count(request: &Value, key: &str, default: usize) -> Result<usize> {
    request
        .get(key)
        .map(|v| {
            v.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| Error(format!("{key} must be a nonnegative integer")))
        })
        .transpose()
        .map(|v| v.unwrap_or(default))
}
/// Import-specific cancellable boundary shared by the CLI signal bridge and
/// Python signal polling. No implicit engine fallback follows admission.
pub fn execute_import_with_poll(
    path: &Path,
    profile: &str,
    request: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
) -> Result<Value> {
    if !request.is_object() {
        return Err(Error("operation request must be a JSON object".into()));
    }
    catabolic_core::validate_finite(request)?;
    catabolic_store::filesystem_mutation::admit_failpoints(request)?;
    let mut store = Store::open(path, request["apply"] == true, false)?;
    catabolic_store::mutation::profile(&store, profile)?;
    crate::imports::execute_with_poll(&mut store, profile, request, cancel, poll)
}

/// Bounded Plex intake with signal polling; no remote writes are issued.
pub fn execute_plex_import_with_poll(
    path: &Path,
    profile: &str,
    request: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
) -> Result<Value> {
    if !request.is_object() {
        return Err(Error("operation request must be a JSON object".into()));
    }
    catabolic_core::validate_finite(request)?;
    catabolic_store::filesystem_mutation::admit_failpoints(request)?;
    let mut store = Store::open(path, request["apply"] == true, false)?;
    catabolic_store::mutation::profile(&store, profile)?;
    crate::plex_import::execute_with_poll(&mut store, profile, request, cancel, poll)
}

/// Catalog-independent Plex login with supervisor/Python signal polling.
pub fn execute_plex_login_with_poll(
    operation: &str,
    request: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
) -> Result<Value> {
    if !request.is_object() {
        return Err(Error("operation request must be a JSON object".into()));
    }
    catabolic_core::validate_finite(request)?;
    catabolic_store::filesystem_mutation::admit_failpoints(request)?;
    crate::plex_login::execute_with_poll(operation, request, cancel, poll)
}

/// Reviewed Plex matching with cancellable, detached, read-only remote intake.
pub fn execute_plex_match_with_poll(
    path: &Path,
    profile: &str,
    request: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
) -> Result<Value> {
    if !request.is_object() {
        return Err(Error("operation request must be a JSON object".into()));
    }
    catabolic_core::validate_finite(request)?;
    catabolic_store::filesystem_mutation::admit_failpoints(request)?;
    let mut store = Store::open(path, request["apply"] == true, false)?;
    catabolic_store::mutation::profile(&store, profile)?;
    crate::plex_matching::execute_with_poll(&mut store, profile, request, cancel, poll)
}

/// Whole-batch reviewed Plex metadata publication with cooperative cancellation.
pub fn execute_plex_metadata_with_poll(
    path: &Path,
    profile: &str,
    request: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
) -> Result<Value> {
    if !request.is_object() {
        return Err(Error("operation request must be a JSON object".into()));
    }
    catabolic_core::validate_finite(request)?;
    catabolic_store::filesystem_mutation::admit_failpoints(request)?;
    let mut store = Store::open(path, request["apply"] == true, false)?;
    catabolic_store::mutation::profile(&store, profile)?;
    crate::plex_metadata::execute_with_poll(&mut store, profile, request, cancel, poll)
}

pub fn execute(
    path: &Path,
    profile: &str,
    surface: &str,
    operation: &str,
    request: &Value,
) -> Result<Value> {
    execute_ordered(path, profile, surface, operation, request, None)
}

/// `execute` for a request decoded from `source`. Owners whose frozen Python
/// traversal follows dictionary insertion order (program bundles) receive the
/// source's object order; every value still comes from `request`.
pub fn execute_with_source(
    path: &Path,
    profile: &str,
    surface: &str,
    operation: &str,
    request: &Value,
    source: &str,
) -> Result<Value> {
    let order = if surface == "programs" && matches!(operation, "import" | "validate") {
        Some(catabolic_core::json::source_order(source)?)
    } else {
        None
    };
    execute_ordered(path, profile, surface, operation, request, order.as_ref())
}

/// `execute` with an already recorded request object order.
pub fn execute_ordered(
    path: &Path,
    profile: &str,
    surface: &str,
    operation: &str,
    request: &Value,
    order: Option<&catabolic_core::json::SourceOrder>,
) -> Result<Value> {
    if !request.is_object() {
        return Err(Error("operation request must be a JSON object".into()));
    }
    catabolic_core::validate_finite(request)?;
    catabolic_store::filesystem_mutation::admit_failpoints(request)?;
    if surface == "plex-login" {
        return crate::plex_login::execute(operation, request);
    }
    if surface == "target" {
        return match operation {
            "describe" | "list" => catabolic_store::targets::describe(request["target"].as_str()),
            "definitions" => Ok(catabolic_store::targets::definitions()),
            _ => Err(Error("unknown target operation".into())),
        };
    }
    if surface == "export" && operation == "build" {
        return catabolic_store::exports::build(
            &request["document"],
            required(request, "format")?,
            request["base_url"].as_str(),
        );
    }
    if surface == "rendition-worker" {
        return match operation {
            "tick" => {
                let capabilities = match request.get("capabilities") {
                    Some(value) => value.clone(),
                    None => crate::rendering::capabilities()?,
                };
                crate::renditions::tick(path, profile, &capabilities)
            }
            _ => Err(Error("unknown rendition worker operation".into())),
        };
    }
    let writable = match (surface, operation) {
        ("definitions", operation)
            if operation.ends_with("-get")
                || operation.ends_with("-list")
                || operation.starts_with("estimate")
                || operation.ends_with("-validate") =>
        {
            false
        }
        ("outputs", "preview" | "verify")
        | ("media", "get")
        | ("receipt", "source")
        | ("content", "inspect" | "inspect-ticket")
        | ("processor", "get" | "job" | "jobs")
        | ("component", "get")
        | ("playback", "status" | "playlist" | "inspect-segment")
        | ("curation", "completeness")
        | ("http-operations", "compile" | "get") => false,
        ("observe", "trust" | "scan_policy" | "remount")
        | ("notify", "put" | "configure")
        | ("http-mappings", _) => request["apply"] == true,
        ("notify", "list" | "deliveries" | "status")
        | ("webhooks", "inspect" | "list" | "deliveries") => false,
        ("watcher", "get" | "list" | "history" | "due" | "schedule")
        | ("artifact", "get" | "recipe-get" | "capabilities")
        | ("output", "get" | "definition-get") => false,
        ("programs", "export" | "validate") | ("fallback-policy", "get" | "list") => false,
        ("plans", _) => false,
        ("workflow", "get" | "checks" | "log")
        | ("layout", "preview" | "prepare")
        | ("catalog-refresh", "pending")
        | ("rules", "preview" | "statistics")
        | ("rendition-request", "status" | "capabilities") => false,
        (
            "consumers",
            "connections" | "bindings" | "status" | "attempts" | "events" | "creations"
            | "publications" | "discover",
        ) => false,
        ("consumers", "connection-put" | "bind") => request["apply"] == true,
        ("consumers", "creation" | "create") => {
            request["apply"] == true || request["reconcile"] == true
        }
        ("component", "extract") => request["apply"] == true,
        ("import" | "plex-import" | "plex-match" | "plex-metadata", _) => request["apply"] == true,
        _ => true,
    };
    let mut store = Store::open(path, writable, false)?;
    catabolic_store::mutation::profile(&store, profile)?;
    if surface == "fallback" && operation == "resolve" {
        return crate::fallback::resolve(store, profile, request);
    }
    if surface == "fallback" && matches!(operation, "bind" | "plan" | "run") {
        return crate::fallback::projection(store, profile, operation, request);
    }
    if surface == "component" && operation == "package" {
        return crate::component_packaging::package(
            store,
            profile,
            required(request, "policy_id")?,
            required(request, "item_id")?,
            required(request, "location")?,
            request["apply"] == true,
            request["expected_plan"].as_str(),
        );
    }
    if surface == "plans" {
        let mut evaluation =
            catabolic_store::selection::Evaluation::new(store, profile, 60_000, 100_000, false)?;
        return catabolic_store::plans::dispatch(&mut evaluation, operation, request);
    }
    if surface == "content" && matches!(operation, "inspect-ticket" | "debit-ticket") {
        let environment = request["ticket_env"]
            .as_str()
            .unwrap_or("CATABOLIC_CONTENT_TICKET");
        catabolic_store::notifications::credential_reference(environment)?;
        let secret = std::env::var(environment).map_err(|_| Error("unauthorized".into()))?;
        let id = required(request, "file_id")?;
        let revision = required(request, "revision")?;
        let (mut access, ticket) =
            catabolic_store::content_access::authenticate_ticket(store, &secret, id, revision)?;
        if access.profile() != profile {
            return Err(Error("unauthorized".into()));
        }
        let report = catabolic_store::content_access::execute(&mut access, "inspect", request)?;
        if operation == "debit-ticket" {
            let length = request["length"]
                .as_u64()
                .ok_or_else(|| Error("invalid_content_request".into()))?;
            catabolic_store::content_access::debit_ticket(&access, &ticket, length)?;
        }
        return Ok(report);
    }
    if matches!(
        surface,
        "webhooks" | "content" | "playback" | "rendition-request"
    ) {
        let environment = request["token_env"]
            .as_str()
            .unwrap_or("CATABOLIC_API_TOKEN");
        catabolic_store::notifications::credential_reference(environment)?;
        let secret = std::env::var(environment).map_err(|_| Error("unauthorized".into()))?;
        let mut access = catabolic_store::access::Access::authenticate(store, &secret)?;
        if access.profile() != profile {
            return Err(Error("unauthorized".into()));
        }
        return if surface == "rendition-request" {
            catabolic_store::rendition_mutation::execute(&mut access, operation, request)
        } else if surface == "playback" {
            crate::playback::execute(&mut access, operation, request)
        } else if surface == "content" {
            catabolic_store::content_access::execute(&mut access, operation, request)
        } else {
            catabolic_store::webhooks::execute(&mut access, operation, request)
        };
    }
    if surface == "http-mappings" {
        return catabolic_store::http_mappings::execute(store, profile, operation, request);
    }
    let mut result = match surface {
        "workflow" => catabolic_store::item_workflow::dispatch(&store, profile, operation, request),
        "import" if operation == "catalog" => crate::imports::execute(&mut store, profile, request),
        "rules" => crate::rules::dispatch(&mut store, profile, operation, request),
        "layout" => catabolic_store::layout_mutation::dispatch(&store, profile, operation, request),
        "catalog-refresh" => {
            crate::catalog_refresh::execute(&mut store, profile, operation, request)
        }
        "export" if operation == "publish" => catabolic_store::export_bundle::publish(
            &store,
            profile,
            &request["document"],
            &request["export"],
            Path::new(required(request, "output")?),
        ),
        "playback-worker" => crate::playback::worker(&mut store, profile, operation, request),
        "api-worker" => crate::http_worker::execute(&mut store, profile, operation, request),
        "component" => match operation {
            "get" => catabolic_store::component_mutation::get(
                &store,
                profile,
                required(request, "id")?,
                request["require_current"] == true,
            ),
            "extract" => crate::component_packaging::extraction(
                &mut store,
                profile,
                required(request, "id")?,
                required(request, "operation_id")?,
                required(request, "location")?,
                request["apply"] == true,
                request["expected_plan"].as_str(),
            ),
            _ => Err(Error("unknown component operation".into())),
        },
        "processor" => match operation {
            "put" => catabolic_store::processor_mutation::put(
                &store,
                profile,
                required(request, "id")?,
                &request["definition"],
            ),
            "get" => {
                catabolic_store::processor_mutation::get(&store, profile, required(request, "id")?)
            }
            "job" => {
                catabolic_store::processor_mutation::job(&store, profile, required(request, "id")?)
            }
            "jobs" => catabolic_store::processor_mutation::jobs(
                &store,
                profile,
                required(request, "processor")?,
                count(request, "limit", 100)?,
                request["after"].as_str().unwrap_or(""),
            ),
            "enqueue" => catabolic_store::processor_mutation::enqueue(
                &store,
                profile,
                required(request, "processor")?,
                required(request, "file_id")?,
                required(request, "item_id")?,
                required(request, "definition_id")?,
                request
                    .get("configuration")
                    .unwrap_or(&serde_json::json!({})),
                request["operation_id"].as_str(),
            ),
            "retry" => catabolic_store::processor_mutation::retry(
                &store,
                profile,
                required(request, "id")?,
            ),
            "cancel" => catabolic_store::processor_mutation::cancel(
                &store,
                profile,
                required(request, "id")?,
            ),
            "tick" | "claim" => {
                let ids = request
                    .get("job_ids")
                    .map(|_| identifiers(request, "job_ids"))
                    .transpose()?;
                let lease = request
                    .get("lease_seconds")
                    .map(|v| {
                        v.as_i64()
                            .ok_or_else(|| Error("lease_seconds must be an integer".into()))
                    })
                    .transpose()?
                    .unwrap_or(120);
                if operation == "tick" {
                    crate::processors::tick(
                        &mut store,
                        profile,
                        required(request, "processor")?,
                        required(request, "worker")?,
                        lease,
                        ids.as_deref(),
                    )
                } else {
                    catabolic_store::processor_mutation::claim(
                        &store,
                        profile,
                        required(request, "processor")?,
                        required(request, "worker")?,
                        lease,
                        request["resume"] == true,
                        ids.as_deref(),
                    )
                }
            }
            "heartbeat" => catabolic_store::processor_mutation::heartbeat(
                &store,
                profile,
                required(request, "id")?,
                required(request, "lease_token")?,
                request["lease_seconds"]
                    .as_i64()
                    .ok_or_else(|| Error("lease_seconds must be an integer".into()))?,
            ),
            "transition" => catabolic_store::processor_mutation::transition(
                &store,
                profile,
                required(request, "id")?,
                required(request, "lease_token")?,
                required(request, "state")?,
            ),
            "complete" => catabolic_store::processor_mutation::complete(
                &store,
                profile,
                required(request, "id")?,
                required(request, "lease_token")?,
                &request["receipt"],
            ),
            _ => Err(Error("unknown processor operation".into())),
        },
        "plex-metadata" if operation == "run" => {
            crate::plex_metadata::execute(&mut store, profile, request)
        }
        "plex-match" if operation == "run" => {
            crate::plex_matching::execute(&mut store, profile, request)
        }
        "plex-match" if operation == "list" => {
            catabolic_store::plex_matching::listing(&store, profile, request)
        }
        "plex-import" if operation == "run" => {
            crate::plex_import::execute(&mut store, profile, request)
        }
        "consumers" => crate::consumers::execute(&mut store, profile, operation, request),
        "manifest" if operation == "write" => store.transaction(|_| {
            catabolic_store::manifest::write(&store, profile, &request["document"], request)
        }),
        "receipt" => match operation {
            "source" => crate::receipts::source_receipt(
                &store,
                profile,
                required(request, "file_id")?,
                required(request, "item_id")?,
            ),
            "import" => crate::receipts::import_receipt(&store, profile, &request["receipt"]),
            "import-file" => {
                crate::receipts::import_file(&store, profile, Path::new(required(request, "path")?))
            }
            _ => Err(Error("unknown receipt operation".into())),
        },
        "watcher" => crate::watchers::execute(&mut store, profile, operation, request),
        "artifact" => match operation {
            "bind" => catabolic_store::artifact_mutation::bind(
                &store,
                profile,
                required(request, "location")?,
                required(request, "path")?,
            ),
            "recipe" => crate::artifacts::recipe(
                &store,
                required(request, "name")?,
                required(request, "preset")?,
                request.get("options").unwrap_or(&serde_json::json!({})),
                request["output_definition_id"].as_str(),
            ),
            "recipe-get" => {
                catabolic_store::artifact_mutation::get_recipe(&store, required(request, "id")?)
            }
            "get" => {
                catabolic_store::artifact_mutation::get(&store, profile, required(request, "id")?)
            }
            "enqueue" if request.get("source_lineage_mode").is_some() => {
                crate::artifacts::enqueue_lineage(
                    &mut store,
                    profile,
                    required(request, "file_id")?,
                    required(request, "recipe_id")?,
                    required(request, "location")?,
                    required(request, "item_id")?,
                    required(request, "source_lineage_mode")?,
                )
            }
            "enqueue" => crate::artifacts::enqueue(
                &mut store,
                profile,
                required(request, "file_id")?,
                required(request, "recipe_id")?,
                required(request, "location")?,
                required(request, "item_id")?,
            ),
            "run" => {
                let ids = request
                    .get("job_ids")
                    .map(|_| identifiers(request, "job_ids"))
                    .transpose()?;
                crate::artifacts::run(
                    &mut store,
                    profile,
                    count(request, "limit", 100)?,
                    ids.as_deref(),
                    Arc::new(AtomicBool::new(false)),
                )
            }
            "recover" => {
                crate::artifacts::recover(&mut store, profile, count(request, "limit", 100)?)
            }
            "capabilities" => crate::rendering::capabilities(),
            _ => Err(Error("unknown artifact operation".into())),
        },
        "output" => match operation {
            "define" => catabolic_store::output_mutation::define(
                &store,
                required(request, "name")?,
                &request["definition"],
            ),
            "definition-get" => {
                catabolic_store::output_mutation::get_definition(&store, required(request, "id")?)
            }
            "get" => {
                catabolic_store::output_mutation::get(&store, profile, required(request, "id")?)
            }
            "register" => catabolic_store::output_mutation::register(
                &store,
                profile,
                required(request, "file_id")?,
                required(request, "source_file_id")?,
                required(request, "item_id")?,
                required(request, "definition_id")?,
                request.get("metadata").unwrap_or(&serde_json::json!({})),
            ),
            _ => Err(Error("unknown output operation".into())),
        },
        "programs" if operation == "import" => catabolic_store::programs::import_ordered(
            &store,
            profile,
            request,
            order,
            // Frozen program import admits every operation kind, including
            // external processors, through the ordinary `Operations.put` owner.
            |name, definition| crate::operations::put(&store, profile, name, definition),
        ),
        "fallback-policy" => {
            catabolic_store::fallback_mutation::execute(&store, profile, operation, request)
        }
        "programs" => {
            catabolic_store::programs::dispatch_ordered(&store, profile, operation, request, order)
        }
        "http-operations" => {
            catabolic_store::http_operations::execute(&mut store, profile, operation, request)
        }
        "curation" => catabolic_store::curation::dispatch(&store, profile, operation, request),
        "tag" => catabolic_store::tagging::dispatch(&store, profile, operation, request),
        "definitions" => {
            catabolic_store::definitions::dispatch(&store, profile, operation, request)
        }
        "observe" => crate::observation::execute(&mut store, profile, operation, request),
        "outputs" => catabolic_store::filesystem_mutation::execute_mut(
            &mut store,
            profile,
            operation,
            request,
            &mut crate::fallback::probe,
        ),
        "notify" if matches!(operation, "drain" | "run") => {
            let limit = request
                .get("limit")
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or_else(|| Error("invalid_configuration".into()))
                })
                .transpose()?
                .unwrap_or(10);
            crate::notifications::drain(&mut store, profile, limit, request["tag"].as_str())
        }
        "notify" => {
            catabolic_store::notifications::execute(&mut store, profile, operation, request)
        }
        "media" => match operation {
            "enqueue" => crate::media::enqueue(
                &store,
                profile,
                required(request, "operation")?,
                &identifiers(request, "file_ids")?,
                request.get("options").unwrap_or(&serde_json::json!({})),
                request["refresh"] == true,
                request["recipe"].as_str(),
            ),
            "run" => crate::media::run(
                &mut store,
                profile,
                &identifiers(request, "job_ids")?,
                Arc::new(AtomicBool::new(false)),
            ),
            "get" => {
                catabolic_store::media_mutation::get_job(&store, profile, required(request, "id")?)
            }
            "cancel" => {
                catabolic_store::media_mutation::cancel(&store, profile, required(request, "id")?)
            }
            "retry" => {
                catabolic_store::media_mutation::retry(&store, profile, required(request, "id")?)
            }
            // Definition options are user data, not publication reports. Do
            // not let the generic post-publication visitor interpret them.
            "operation-put" => {
                return crate::operations::put(
                    &store,
                    profile,
                    required(request, "name")?,
                    &request["definition"],
                );
            }
            _ => Err(Error("unknown media operation".into())),
        },
        _ => Err(Error("unknown native operation surface".into())),
    }?;
    if writable
        && matches!(
            surface,
            "outputs"
                | "artifact"
                | "receipt"
                | "processor"
                | "media"
                | "rules"
                | "catalog-refresh"
                | "watcher"
        )
    {
        crate::consumers::after_publication(&mut store, profile, &mut result)?;
    }
    Ok(result)
}
