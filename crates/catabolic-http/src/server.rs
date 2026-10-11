// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
use crate::{
    limits::Limits,
    models,
    problem::Problem,
    routes::{self, Input},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, Response},
};
use catabolic_store::{Error, Result, Store};
use std::{
    collections::BTreeMap,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub read_only: bool,
    pub allowed_hosts: Vec<String>,
    pub origins: Vec<String>,
    pub streams: usize,
    pub per_principal: usize,
    pub rate: usize,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8421,
            read_only: true,
            allowed_hosts: vec![
                "localhost".into(),
                "127.0.0.1".into(),
                "[::1]".into(),
                "testserver".into(),
            ],
            origins: vec![],
            streams: 8,
            per_principal: 2,
            rate: 120,
        }
    }
}
pub(crate) struct App {
    pub database: PathBuf,
    pub config: ServerConfig,
    pub limits: Arc<Limits>,
    pub requests: Arc<tokio::sync::Semaphore>,
}
pub fn serve(path: &Path, config: ServerConfig) -> Result<()> {
    let listener = TcpListener::bind((config.host.as_str(), config.port))?;
    serve_listener(path, config, listener)
}
pub fn serve_listener(path: &Path, config: ServerConfig, listener: TcpListener) -> Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(Error(
            "native HTTP remains loopback-only pending transport and content release qualification"
                .into(),
        ));
    }
    if config
        .allowed_hosts
        .iter()
        .chain(&config.origins)
        .any(|v| v.contains('*'))
    {
        return Err(Error("HTTP hosts and CORS origins must be explicit".into()));
    }
    if config.streams == 0 || config.per_principal == 0 || config.rate == 0 {
        return Err(Error("invalid HTTP limits".into()));
    }
    drop(Store::open(path, false, false)?);
    listener.set_nonblocking(true)?;
    let state = Arc::new(App {
        database: path.to_owned(),
        limits: Limits::new(config.streams, config.per_principal, config.rate),
        config,
        requests: Arc::new(tokio::sync::Semaphore::new(64)),
    });
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(32)
        .enable_all()
        .build()
        .map_err(|e| Error(e.to_string()))?
        .block_on(async move {
            let listener =
                tokio::net::TcpListener::from_std(listener).map_err(|e| Error(e.to_string()))?;
            axum::serve(listener, Router::new().fallback(handle).with_state(state))
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await
                .map_err(|e| Error(e.to_string()))
        })
}
fn header<'a>(h: &'a HeaderMap, key: &str) -> Option<&'a str> {
    h.get(key).and_then(|v| v.to_str().ok())
}
fn redirect_url(url: &str) -> String {
    // Starlette RedirectResponse URL quoting, including UTF-8 and control
    // characters. Never pass a decoded path directly to an HTTP header.
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut result = String::new();
    for byte in url.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~:/%#?=@[]!$&'()*+,;".contains(&byte) {
            result.push(char::from(byte));
        } else {
            result.push('%');
            result.push(char::from(HEX[(byte >> 4) as usize]));
            result.push(char::from(HEX[(byte & 15) as usize]));
        }
    }
    result
}
async fn handle(State(app): State<Arc<App>>, request: Request) -> Response<Body> {
    let method = request.method().as_str().to_owned();
    let headers = request.headers().clone();
    let mut response = handle_inner(app.clone(), request)
        .await
        .unwrap_or_else(Problem::response);
    if method == "HEAD" {
        *response.body_mut() = Body::empty();
    }
    if let Some(origin) = header(&headers, "origin")
        && app.config.origins.iter().any(|v| v == origin)
        && let Ok(value) = origin.parse()
    {
        response
            .headers_mut()
            .insert("access-control-allow-origin", value);
        response
            .headers_mut()
            .insert("vary", "Origin".parse().unwrap());
    }
    response
}
async fn handle_inner(app: Arc<App>, request: Request) -> crate::problem::Result<Response<Body>> {
    let _permit = app
        .requests
        .clone()
        .try_acquire_owned()
        .map_err(|_| Problem::new("rate_limit", 429))?;
    let (method, uri, headers) = (
        request.method().as_str().to_owned(),
        request.uri().clone(),
        request.headers().clone(),
    );
    let host = header(&headers, "host").unwrap_or("");
    if !app.config.origins.is_empty()
        && method == "OPTIONS"
        && header(&headers, "origin").is_some()
        && header(&headers, "access-control-request-method").is_some()
    {
        let origin = header(&headers, "origin").unwrap_or("");
        let requested = header(&headers, "access-control-request-method").unwrap_or("");
        let allowed_headers = [
            "accept",
            "accept-language",
            "authorization",
            "content-language",
            "content-type",
            "idempotency-key",
            "last-event-id",
        ];
        let mut failures = vec![];
        if !app.config.origins.iter().any(|v| v == origin) {
            failures.push("origin");
        }
        if !["GET", "HEAD", "POST"].contains(&requested) {
            failures.push("method");
        }
        if header(&headers, "access-control-request-headers").is_some_and(|v| {
            v.split(',')
                .any(|v| !allowed_headers.contains(&v.trim().to_ascii_lowercase().as_str()))
        }) {
            failures.push("headers");
        }
        let okay = failures.is_empty();
        return Ok(Response::builder().status(if okay {200} else {400})
            .header("Content-Type", "text/plain; charset=utf-8")
            .header("Vary", "Origin")
            .header("Access-Control-Allow-Methods","GET, HEAD, POST")
            .header("Access-Control-Max-Age","600")
            .header("Access-Control-Allow-Headers","Accept, Accept-Language, Authorization, Content-Language, Content-Type, Idempotency-Key, Last-Event-ID")
            .body(Body::from(if okay {"OK".into()} else {format!("Disallowed CORS {}", failures.join(", "))})).unwrap());
    }
    let hostname = if host.starts_with('[') {
        host.find(']').map(|n| &host[..=n]).unwrap_or(host)
    } else {
        host.split(':').next().unwrap_or("")
    };
    if !app.config.allowed_hosts.iter().any(|v| v == hostname) {
        return Ok(Response::builder()
            .status(400)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(Body::from("Invalid host header"))
            .unwrap());
    }

    let raw = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        to_bytes(request.into_body(), 1048576),
    )
    .await
    .map_err(|_| Problem::new("http_error", 408))?
    .map_err(|_| Problem::new("body_too_large", 413))?;
    let path = models::decode_path(uri.path()).ok_or_else(|| Problem::new("http_error", 404))?;
    let path_exists = |path: &str| {
        ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"]
            .iter()
            .any(|m| models::route(m, path).is_some())
    };
    let route = models::route(&method, &path);
    if route.is_none() && !path_exists(&path) {
        let alternate = if path.ends_with('/') {
            path.trim_end_matches('/').to_owned()
        } else {
            format!("{path}/")
        };
        if path_exists(&alternate) {
            let location = format!(
                "http://{host}{alternate}{}",
                uri.query().map(|q| format!("?{q}")).unwrap_or_default()
            );
            return Ok(Response::builder()
                .status(307)
                .header("Location", redirect_url(&location))
                .body(Body::empty())
                .unwrap());
        }
    }
    let (template, operation, params) = route
        .ok_or_else(|| Problem::new("http_error", if path_exists(&path) { 405 } else { 404 }))?;
    if models::contract()["paths"][&template][method.to_ascii_lowercase()]
        .get("requestBody")
        .is_some()
    {
        let content_type = header(&headers, "content-type")
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if content_type != "application/json"
            && !(content_type.starts_with("application/") && content_type.ends_with("+json"))
        {
            return Err(Problem::new("invalid_request", 422));
        }
    }
    let body = models::body(&method, &template, &raw)?;
    let query = form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .into_owned()
        .collect::<BTreeMap<_, _>>();
    if models::contract()["paths"][&template][method.to_ascii_lowercase()]["parameters"]
        .as_array()
        .is_some_and(|v| v.iter().any(|p| p["in"] == "query" && p["name"] == "limit"))
    {
        models::page(&query)?;
    }
    if let Some(parameters) =
        models::contract()["paths"][&template][method.to_ascii_lowercase()]["parameters"].as_array()
    {
        for parameter in parameters.iter().filter(|v| v["required"] == true) {
            let name = parameter["name"].as_str().unwrap_or("");
            let missing = match parameter["in"].as_str() {
                Some("header") => header(&headers, name).is_none(),
                Some("query") => !query.contains_key(name),
                _ => false,
            };
            if missing {
                return Err(Problem::new("invalid_request", 422));
            }
        }
    }
    let input = Input {
        method,
        operation,
        params: params.into_iter().collect(),
        query,
        headers,
        body,
    };
    if input.operation.ends_with("file_content")
        || input.operation.ends_with("rendition_content")
        || input.operation.ends_with("playback_segment")
    {
        return crate::content::response(app, input).await;
    }
    if input.operation == "list_events" {
        let follow = match input
            .query
            .get("follow")
            .map(|v| v.to_ascii_lowercase())
            .as_deref()
        {
            None | Some("false" | "0" | "off" | "no") => false,
            Some("true" | "1" | "on" | "yes") => true,
            _ => return Err(Problem::new("invalid_request", 422)),
        };
        if follow {
            return crate::events::response(app, input).await;
        }
    }
    tokio::task::spawn_blocking(move || routes::dispatch(&app, input))
        .await
        .map_err(|_| Problem::new("catalog_error", 500))?
}
