// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
use crate::{
    models,
    problem::{Problem, Result},
    server::App,
};
use axum::{
    body::Body,
    http::{HeaderMap, Response},
};
use catabolic_store::{Store, access::Access, api_catalog as catalog, migration::rows};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub struct Input {
    pub method: String,
    pub operation: String,
    pub params: BTreeMap<String, String>,
    pub query: BTreeMap<String, String>,
    pub headers: HeaderMap,
    pub body: Value,
    pub graphql_variables: Option<catabolic_store::graphql::InputVariables>,
}
impl Input {
    pub fn id(&self) -> &str {
        self.params
            .get("identifier")
            .map(String::as_str)
            .unwrap_or("")
    }
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers.get(key).and_then(|v| v.to_str().ok())
    }
    pub fn bearer(&self) -> Result<&str> {
        self.header("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| Problem::new("unauthorized", 401))
    }
    pub fn cursor(&self) -> Option<&str> {
        self.query.get("cursor").map(String::as_str)
    }
    pub fn limit(&self) -> Result<usize> {
        models::page(&self.query)
    }
}
pub fn json(value: Value, status: u16) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&value).unwrap()))
        .unwrap()
}
pub fn session(app: &App, input: &Input, write: bool) -> Result<Access> {
    let access = Access::authenticate(Store::open(&app.database, write, false)?, input.bearer()?)?;
    app.limits.request(access.principal())?;
    if write {
        catabolic_store::api_events::prune(access.store())?;
    }
    Ok(access)
}
fn mutation(app: &App) -> Result<()> {
    if app.config.read_only {
        Err(Problem::new("read_only", 403))
    } else {
        Ok(())
    }
}
pub fn dispatch(app: &App, input: Input) -> Result<Response<Body>> {
    let op = input.operation.as_str();
    let changing = matches!(
        op,
        "create_rendition_request"
            | "create_logical_rendition_request"
            | "cancel_rendition_request"
            | "retry_rendition_request"
            | "create_playback_session"
            | "cancel_playback_session"
            | "manage_definition"
            | "put_watcher"
            | "set_watcher_enabled"
            | "admit_watcher_run"
            | "create_webhook"
            | "disable_webhook"
            | "retry_webhook"
            | "retry_request_callback"
            | "disable_request_callback"
            | "approve_http_operation"
            | "disable_http_operation"
            | "retry_http_operation"
            | "apply_http_mapping"
    );
    if changing {
        mutation(app)?
    }
    if op == "apply_http_mapping" && input.body["expected_plan"].is_null() {
        return Err(Problem::new("expected_plan_required", 400));
    }
    if op == "resolve_item"
        && input.body["fallback_policy_id"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
        && ["file_id", "definition_id"]
            .iter()
            .any(|key| input.body[*key].as_str().is_some_and(|v| !v.is_empty()))
    {
        return Err(Problem::new("exact_and_logical_selection_conflict", 422));
    }
    let write = changing
        || matches!(
            op,
            "create_item_snapshot"
                | "create_content_ticket"
                | "revoke_content_ticket"
                | "list_events"
        )
        || (op == "resolve_item"
            && input.body["fallback_policy_id"]
                .as_str()
                .is_some_and(|v| !v.is_empty()));
    let mut access = session(app, &input, write)?;
    let mut status = 200;
    let result = match op {
        "get_identity" => catalog::identity(&access)?,
        "get_capabilities" => catalog::capabilities(
            &access,
            app.config.read_only,
            app.config.streams,
            app.config.per_principal,
            app.config.rate,
        )?,
        "get_openapi" => models::contract().clone(),
        "execute_sql" => catabolic_store::api_queries::sql(access, &input.body)?,
        "execute_graphql" => {
            access.require("metadata:read")?;
            let authority = catalog::install(&mut access)?;
            catabolic_store::graphql::execute_authorized_store(
                access.into_store(),
                authority,
                input.body["query"].as_str().unwrap_or(""),
                input.graphql_variables.clone().unwrap_or_else(|| {
                    input
                        .body
                        .get("variables")
                        .cloned()
                        .unwrap_or_else(|| json!({}))
                        .into()
                }),
                input.body["operationName"].as_str(),
                5000,
            )?
            .1
        }
        "run_saved_query" => catabolic_store::api_queries::saved(
            access,
            input
                .params
                .get("revision_id")
                .map(String::as_str)
                .unwrap_or(""),
            input.body["limit"].as_u64().unwrap_or(100) as usize,
            input.body["cursor"].as_str(),
        )?,
        "list_items" | "list_files" | "list_renditions" => {
            let authority = catalog::install(&mut access)?;
            catalog::listing(
                &access,
                &authority,
                &op[5..],
                input.limit()?,
                input.cursor(),
            )?
        }
        "get_item" | "get_file" | "get_rendition" => {
            let authority = catalog::install(&mut access)?;
            catalog::get(&access, &authority, &format!("{}s", &op[4..]), input.id())?
        }
        "create_item_snapshot" => {
            let authority = catalog::install(&mut access)?;
            catalog::snapshot(&access, &authority)?
        }
        "get_snapshot_page" => {
            let authority = catalog::install(&mut access)?;
            catalog::snapshot_page(
                &access,
                &authority,
                input.id(),
                input.limit()?,
                input.cursor(),
            )?
        }
        "list_projection_entries" | "list_projection_resolutions" => {
            let authority = catalog::install(&mut access)?;
            catalog::projection(
                &access,
                &authority,
                input.id(),
                input.limit()?,
                input.cursor(),
                op == "list_projection_resolutions",
            )?
        }
        "get_fallback_policy" => {
            let authority = catalog::install(&mut access)?;
            if !authority.permits_fallback_policy(input.id()) {
                return Err(Problem::new("not_found", 404));
            }
            let row = catabolic_store::fallback_mutation::get(
                access.store(),
                access.profile(),
                input.id(),
            )
            .map_err(|_| Problem::new("not_found", 404))?;
            json!({"id":row["id"],"name":row["name"],"revision":row["revision"],"tiers":row["definition"]["fallbacks"].as_array().map(|a|a.iter().map(|v|v["name"].clone()).collect::<Vec<_>>()).unwrap_or_default()})
        }
        "resolve_item" => {
            if input.body["fallback_policy_id"]
                .as_str()
                .is_some_and(|v| !v.is_empty())
            {
                catabolic_runtime::renditions::logical_resolve(
                    &mut access,
                    input.id(),
                    &input.body,
                )?
            } else {
                if !access.item(input.id(), "metadata:read")? {
                    return Err(Problem::new("not_found", 404));
                }
                let mut values = rows(
                    &access.store().db,
                    "SELECT file_id FROM item_files WHERE item_id=? AND active=1 ORDER BY id LIMIT 1001",
                    &[input.id().to_owned().into()],
                )?;
                values.extend(rows(&access.store().db,"SELECT file_id FROM media_outputs WHERE source_item_id=? AND profile=? ORDER BY id LIMIT 1001",&[input.id().to_owned().into(),access.profile().to_owned().into()])?);
                use catabolic_core::content_selection::{Effect, Input, State, advance};
                let mut selection = State::Candidate;
                let mut chosen = None;
                for row in values {
                    let id = row["file_id"].as_str().unwrap();
                    let (next, effect) = advance(
                        selection,
                        Input::Candidate {
                            requested_file_matches: input.body["file_id"]
                                .as_str()
                                .is_none_or(|v| v == id),
                        },
                    )
                    .expect("candidate follows an unsuccessful selection");
                    selection = next;
                    if effect == Effect::Continue {
                        continue;
                    }
                    let matches = if let Some(def) = input.body["definition_id"].as_str() {
                        access.store().db.prepare("SELECT 1 FROM media_outputs WHERE file_id=? AND definition_id=? AND profile=?")?.exists(rusqlite_params(id,def,access.profile()))?
                    } else {
                        true
                    };
                    let (next, effect) = advance(selection, Input::Definition { matches })
                        .expect("definition follows a candidate");
                    selection = next;
                    if effect == Effect::Continue {
                        continue;
                    }
                    let revision = catabolic_store::content_access::revision_of(
                        access.store(),
                        access.profile(),
                        id,
                    )?;
                    let usable = catabolic_store::content_access::with_opened(
                        &mut access,
                        id,
                        &revision,
                        |_, _| Ok(()),
                    )
                    .is_ok();
                    let (next, effect) = advance(selection, Input::Opened { usable })
                        .expect("open result follows a matching definition");
                    selection = next;
                    if effect == Effect::Select {
                        chosen = Some(
                            json!({"file_id":id,"revision":revision,"content_path":format!("/v1/files/{id}/content?revision={revision}")}),
                        );
                        break;
                    }
                }
                chosen.ok_or_else(|| Problem::new("no_eligible_content", 409))?
            }
        }
        "create_content_ticket" => catabolic_store::content_access::ticket(
            &mut access,
            input.body["file_id"].as_str().unwrap(),
            input.body["revision"].as_str().unwrap(),
            input.body["ttl"].as_u64().unwrap_or(300),
            input.body["max_bytes"].as_u64().unwrap_or(1073741824),
        )?,
        "revoke_content_ticket" => {
            catabolic_store::content_access::revoke_ticket(&access, input.id())?
        }
        "list_events" => catabolic_store::api_events::page(
            &mut access,
            input
                .cursor()
                .filter(|v| !v.is_empty())
                .or(input.header("last-event-id")),
        )?,
        "create_rendition_request" => {
            let capabilities = catabolic_store::rendition_mutation::capabilities(&access)?;
            let result = catabolic_store::rendition_mutation::admit(
                &mut access,
                &input.body,
                input.header("idempotency-key").unwrap_or(""),
                &capabilities,
                Default::default(),
                None,
            )?;
            status = if result["state"] == "ready" { 200 } else { 202 };
            result
        }
        "get_rendition_request" => {
            catabolic_store::rendition_mutation::status(&mut access, input.id())?
        }
        "cancel_rendition_request" => {
            catabolic_store::rendition_mutation::cancel(&mut access, input.id())?
        }
        "retry_rendition_request" => {
            catabolic_store::rendition_mutation::retry(&mut access, input.id())?
        }
        "create_logical_rendition_request" => {
            let capabilities = match catabolic_store::rendition_mutation::capabilities(&access) {
                Ok(value) => value,
                Err(error) if error.0 == "worker_unavailable" => Value::Null,
                Err(error) => return Err(error.into()),
            };
            let result = catabolic_runtime::renditions::logical_admit(
                &mut access,
                &input.body,
                input.header("idempotency-key").unwrap_or(""),
                &capabilities,
            )?;
            status = if result["state"] == "ready" { 200 } else { 202 };
            result
        }
        "create_playback_session" | "get_playback_session" | "cancel_playback_session" => {
            let action = match op {
                "create_playback_session" => "admit",
                "get_playback_session" => "status",
                _ => "cancel",
            };
            let mut body = input.body.clone();
            if let Some(o) = body.as_object_mut() {
                o.remove("ttl");
            }
            let result = catabolic_runtime::playback::execute(
                &mut access,
                action,
                &json!({"session_id":input.id(),"body":body,"ttl":input.body["ttl"].as_u64().unwrap_or(3600),"idempotency_key":input.header("idempotency-key").unwrap_or("")}),
            )?;
            if op == "create_playback_session" {
                status = if ["ready", "playing"].contains(&result["state"].as_str().unwrap_or("")) {
                    200
                } else {
                    202
                }
            }
            result
        }
        "get_playback_playlist" | "head_playback_playlist" => {
            let result = catabolic_runtime::playback::execute(
                &mut access,
                "playlist",
                &json!({"session_id":input.id()}),
            )?;
            let data = result["playlist"]
                .as_str()
                .ok_or_else(|| Problem::new("catalog_error", 500))?;
            return Ok(Response::builder()
                .header("Content-Type", "application/vnd.apple.mpegurl")
                .header("Cache-Control", "no-store")
                .header("X-Content-Type-Options", "nosniff")
                .header("Content-Length", data.len())
                .body(Body::from(data.to_owned()))
                .unwrap());
        }
        "create_webhook" => {
            access.require_operator("webhooks:manage")?;
            status = 201;
            catabolic_store::webhooks::register(
                &mut access,
                input.body["url"].as_str().unwrap(),
                &input.body["events"],
                None,
            )?
        }
        "list_webhooks" => catabolic_store::webhooks::listing(
            &mut access,
            input.limit()?,
            input.query.get("after").map(String::as_str).unwrap_or(""),
        )?,
        "get_webhook"
        | "disable_webhook"
        | "retry_webhook"
        | "list_webhook_deliveries"
        | "get_request_callback"
        | "retry_request_callback"
        | "disable_request_callback"
        | "list_request_callback_deliveries" => {
            let callback = op.contains("callback");
            let id = if callback {
                access.request_visible(input.id())?;
                rows(&access.store().db,"SELECT destination_id FROM api_webhooks WHERE profile=? AND principal=? AND request_id=?",&[access.profile().to_owned().into(),access.principal().to_owned().into(),input.id().to_owned().into()])?.first().and_then(|r|r["destination_id"].as_str()).ok_or_else(||Problem::new("not_found",404))?.to_owned()
            } else {
                input.id().to_owned()
            };
            if op.starts_with("get_") {
                catabolic_store::webhooks::inspect(&mut access, &id, callback)?
            } else if op.starts_with("list_") {
                catabolic_store::webhooks::deliveries(
                    &mut access,
                    &id,
                    input.limit()?,
                    input.query.get("after").map(String::as_str).unwrap_or(""),
                    callback,
                )?
            } else {
                catabolic_store::webhooks::control(
                    &mut access,
                    &id,
                    if op.starts_with("retry_") {
                        "retry"
                    } else {
                        "disable"
                    },
                    callback,
                )?
            }
        }
        "approve_http_operation"
        | "list_http_operations"
        | "get_http_operation"
        | "disable_http_operation"
        | "retry_http_operation"
        | "list_http_mapping_deliveries" => {
            let action = match op {
                "approve_http_operation" => {
                    status = 201;
                    "put"
                }
                "list_http_operations" => "list",
                "get_http_operation" => "get",
                "disable_http_operation" => "disable",
                "retry_http_operation" => "retry",
                _ => "deliveries",
            };
            let mut request = input.body.clone();
            request["id"] = json!(input.id());
            request["limit"] = json!(input.limit()?);
            request["after"] = json!(input.query.get("after").map(String::as_str).unwrap_or(""));
            catabolic_store::http_operations::ingress(&access, action, &request)?
        }
        "preview_http_mapping" | "apply_http_mapping" => catabolic_store::http_mappings::ingress(
            access,
            &input.body,
            op == "apply_http_mapping",
        )?,
        "list_watchers"
        | "get_watcher"
        | "put_watcher"
        | "set_watcher_enabled"
        | "get_watcher_history"
        | "admit_watcher_run" => watcher(&access, &input, &mut status)?,
        "manage_definition" => operator(&access, &input)?,
        _ => return Err(Problem::new("http_error", 404)),
    };
    let mut response = json(result.clone(), status);
    if matches!(
        op,
        "create_rendition_request" | "create_playback_session" | "create_logical_rendition_request"
    ) && let Some(location) = result["status_url"].as_str()
    {
        response.headers_mut().insert(
            "Location",
            location
                .parse()
                .map_err(|_| Problem::new("catalog_error", 500))?,
        );
    }
    if matches!(op, "create_playback_session" | "get_playback_session") {
        response
            .headers_mut()
            .insert("Cache-Control", "no-store".parse().unwrap());
    }
    Ok(response)
}
fn rusqlite_params<'a>(a: &'a str, b: &'a str, c: &'a str) -> [&'a str; 3] {
    [a, b, c]
}
fn watcher(access: &Access, input: &Input, status: &mut u16) -> Result<Value> {
    access.require_operator("operator:write")?;
    let name = input.params.get("name").map(String::as_str).unwrap_or("");
    let action = match input.operation.as_str() {
        "list_watchers" => "list",
        "get_watcher" => "get",
        "put_watcher" => "put",
        "set_watcher_enabled" => {
            if input.body["enabled"] == true {
                "enable"
            } else {
                "disable"
            }
        }
        "get_watcher_history" => "history",
        _ => "admit",
    };
    let result = catabolic_store::watcher::execute(
        access.store(),
        access.profile(),
        action,
        &json!({"name":name,"definition":input.body,"transfer":input.body["transfer"]==true}),
    )?;
    if action == "admit" {
        *status = 202;
        return Ok(
            json!({"name":name,"pending_generation":result["pending_generation"],"admitted":true}),
        );
    }
    let fields = if action == "list" {
        vec![
            "name",
            "definition_id",
            "enabled",
            "next_due",
            "pending_generation",
            "completed_generation",
            "active_run",
        ]
    } else if action == "history" {
        vec![
            "id",
            "definition_id",
            "generation",
            "trigger",
            "scheduled_at",
            "started_at",
            "completed_at",
            "state",
            "result",
            "error",
        ]
    } else {
        vec![
            "name",
            "definition_id",
            "enabled",
            "next_due",
            "pending_generation",
            "completed_generation",
            "active_run",
            "definition",
            "revision",
            "digest",
            "retry_at",
            "pending_reaction",
            "last_run",
        ]
    };
    let present = |v: &Value| -> Value {
        let mut out = serde_json::Map::new();
        for k in &fields {
            let value = if *k == "enabled" {
                json!(v[*k] == true || v[*k] == 1)
            } else if *k == "result" && v[*k].is_string() {
                catabolic_core::decode_json(v[*k].as_str().unwrap()).unwrap_or(Value::Null)
            } else {
                v[*k].clone()
            };
            out.insert((*k).into(), value);
        }
        Value::Object(out)
    };
    Ok(if action == "list" {
        json!({"watchers":result.as_array().unwrap().iter().map(present).collect::<Vec<_>>()})
    } else if action == "history" {
        json!({"runs":result.as_array().unwrap().iter().map(present).collect::<Vec<_>>()})
    } else {
        present(&result)
    })
}
fn operator(access: &Access, input: &Input) -> Result<Value> {
    access.require_operator("operator:write")?;
    let resource = input
        .params
        .get("resource")
        .map(String::as_str)
        .unwrap_or("");
    if !["queries", "rules", "projections"].contains(&resource) {
        return Err(Problem::new("invalid_request", 422));
    }
    let name = input.params.get("name").map(String::as_str).unwrap_or("");
    let mut state = vec![];
    for table in [
        "saved_queries",
        "processing_rules",
        "projection_bindings",
        "processing_recipes",
        "copy_policies",
        "rendition_policies",
        "catalog_refresh_settings",
        "bindings",
        "output_definitions",
    ] {
        let values = rows(
            &access.store().db,
            &format!("SELECT * FROM {table} ORDER BY rowid LIMIT 10001"),
            &[],
        )?;
        if values.len() > 10000 {
            return Err(Problem::new("operator_plan_limit", 422));
        }
        state.push(values)
    }
    let digest = catabolic_core::sha256(catabolic_core::encode(&json!([
        access.profile(),
        resource,
        name,
        input.body["definition"],
        state
    ])));
    let apply = input.body["apply"] == true;
    if apply && input.body["expected_plan"] != digest {
        return Err(Problem::new("stale_plan", 409));
    }
    if resource != "queries" {
        let definition = input.body["definition"]
            .as_object()
            .ok_or_else(|| Problem::new("invalid_definition", 422))?;
        let (required, allowed): (&[&str], &[&str]) = if resource == "rules" {
            (
                &["recipe_id", "location", "selection"],
                &[
                    "recipe_id",
                    "location",
                    "selection",
                    "estimates",
                    "required",
                    "allow_derived",
                ],
            )
        } else {
            (
                &["query_id", "layout"],
                &["query_id", "layout", "copies", "renditions"],
            )
        };
        if required.iter().any(|k| !definition.contains_key(*k))
            || definition.keys().any(|k| !allowed.contains(&k.as_str()))
        {
            return Err(Problem::new("invalid_definition", 422));
        }
    }
    let result = access.store().transaction(|db| {
        let result = if resource == "queries" {
            catabolic_store::definitions::query_put(
                access.store(),
                access.profile(),
                name,
                &input.body["definition"],
            )?
        } else {
            let mut arguments = input.body["definition"].clone();
            arguments[if resource == "rules" {
                "name"
            } else {
                "catalog"
            }] = json!(name);
            catabolic_store::definitions::dispatch(
                access.store(),
                access.profile(),
                if resource == "rules" {
                    "rule-put"
                } else {
                    "projection-put"
                },
                &arguments,
            )?
        };
        if !apply {
            return Err(catabolic_store::Error("http_preview_complete".into()));
        }
        db.execute(
            "INSERT INTO api_audit(principal,action,resource) VALUES (?,?,?)",
            [access.principal(), &format!("operator.{resource}"), name],
        )?;
        Ok(result)
    });
    match result {
        Err(e) if e.0 == "http_preview_complete" => {
            Ok(json!({"applied":false,"plan_id":digest,"definition":input.body["definition"]}))
        }
        Ok(result) => Ok(json!({"applied":true,"result":result})),
        Err(e) => Err(e.into()),
    }
}
