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

pub fn execute(
    path: &Path,
    profile: &str,
    surface: &str,
    operation: &str,
    request: &Value,
) -> Result<Value> {
    if !request.is_object() {
        return Err(Error("operation request must be a JSON object".into()));
    }
    catabolic_core::validate_finite(request)?;
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
        _ => true,
    };
    let mut store = Store::open(path, writable, false)?;
    catabolic_store::mutation::profile(&store, profile)?;
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
    if surface == "webhooks" || surface == "content" || surface == "playback" {
        let environment = request["token_env"]
            .as_str()
            .unwrap_or("CATABOLIC_API_TOKEN");
        catabolic_store::notifications::credential_reference(environment)?;
        let secret = std::env::var(environment).map_err(|_| Error("unauthorized".into()))?;
        let mut access = catabolic_store::access::Access::authenticate(store, &secret)?;
        if access.profile() != profile {
            return Err(Error("unauthorized".into()));
        }
        return if surface == "playback" {
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
        "playback-worker" => crate::playback::worker(&mut store, profile, operation, request),
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
        "watcher" => catabolic_store::watcher::execute(&store, profile, operation, request),
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
        "programs" if operation == "import" => {
            catabolic_store::programs::import(&store, profile, request, |name, definition| {
                match definition["kind"].as_str() {
                    Some("analysis") => crate::media::put_operation(&store, name, definition),
                    Some("render") => crate::artifacts::recipe(
                        &store,
                        name,
                        required(definition, "operation")?,
                        definition.get("options").unwrap_or(&serde_json::json!({})),
                        definition["output_definition_id"].as_str(),
                    ),
                    _ => Err(Error(
                        "native external processor import is not yet supported".into(),
                    )),
                }
            })
        }
        "fallback-policy" => {
            catabolic_store::fallback_mutation::execute(&store, profile, operation, request)
        }
        "programs" => catabolic_store::programs::dispatch(&store, profile, operation, request),
        "http-operations" => {
            catabolic_store::http_operations::execute(&mut store, profile, operation, request)
        }
        "curation" => catabolic_store::curation::dispatch(&store, profile, operation, request),
        "tag" => catabolic_store::tagging::dispatch(&store, profile, operation, request),
        "definitions" => {
            catabolic_store::definitions::dispatch(&store, profile, operation, request)
        }
        "observe" => catabolic_store::observation::execute(&store, profile, operation, request),
        "outputs" => {
            catabolic_store::filesystem_mutation::execute(&store, profile, operation, request)
        }
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
            "operation-put" => crate::media::put_operation(
                &store,
                required(request, "name")?,
                &request["definition"],
            ),
            _ => Err(Error("unknown media operation".into())),
        },
        _ => Err(Error("unknown native operation surface".into())),
    }?;
    if writable
        && matches!(
            surface,
            "outputs" | "artifact" | "receipt" | "processor" | "media"
        )
    {
        crate::consumers::after_publication(&mut store, profile, &mut result)?;
    }
    Ok(result)
}
