// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Native HTTP delivery with no redirect/proxy credential forwarding. Durable
//! state is owned by catabolic-store and all I/O runs outside its writer lease.
use catabolic_store::{
    Error, Result, Store,
    notifications::{self as policy, DeliveryLease, Outcome},
};
use serde_json::Value;
use std::{io::Read, time::Duration};

pub use catabolic_store::notifications::{Origin, callback_origin};
mod providers;

pub trait Transport {
    fn deliver(&mut self, lease: &DeliveryLease) -> Outcome;
    fn deliver_webhook(&mut self, _lease: &DeliveryLease, _url: &str) -> Outcome {
        Outcome::AuthorizationUnavailable
    }
    fn deliver_mapped(&mut self, _lease: &DeliveryLease, _request: &Value) -> Outcome {
        Outcome::AuthorizationUnavailable
    }
}

/// No credentials or error details are retained in this value or in logs.
pub struct HttpTransport {
    agent: ureq::Agent,
}
impl Default for HttpTransport {
    fn default() -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .proxy(None)
                .max_redirects(0)
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(20)))
                .timeout_connect(Some(Duration::from_secs(10)))
                .build()
                .into(),
        }
    }
}
impl HttpTransport {
    pub fn send_request(&self, request: &Value, event_id: &str) -> Outcome {
        let Some(url) = request["url"].as_str() else {
            return Outcome::InvalidDestination;
        };
        if callback_origin(url).is_err() || event_id.is_empty() {
            return Outcome::InvalidDestination;
        }
        let mut outgoing = match request["method"].as_str() {
            Some("POST") => self.agent.post(url),
            Some("PUT") => self.agent.put(url),
            Some("PATCH") => self.agent.patch(url),
            _ => return Outcome::InvalidDestination,
        };
        outgoing = outgoing
            .header("Content-Type", "application/json")
            .header("Idempotency-Key", event_id)
            .header("User-Agent", "Catabolic-Webhook/1");
        let mut budget = catabolic_core::encode(
            &serde_json::json!({"url":url,"request":request,"envelope":{"id":event_id}}),
        )
        .len();
        if !request["auth"].is_null() {
            let Some(environment) = request["auth"]["environment"].as_str() else {
                return Outcome::InvalidDestination;
            };
            if policy::credential_reference(environment).is_err() {
                return Outcome::InvalidDestination;
            }
            let Ok(secret) = std::env::var(environment) else {
                return Outcome::CredentialUnavailable;
            };
            if secret.is_empty() {
                return Outcome::CredentialUnavailable;
            }
            if secret.len() > 4096 || secret.bytes().any(|c| !(32..=126).contains(&c)) {
                return Outcome::InvalidDestination;
            }
            let (Some(header), Some(prefix)) = (
                request["auth"]["header"].as_str(),
                request["auth"]["prefix"].as_str(),
            ) else {
                return Outcome::InvalidDestination;
            };
            if [
                "host",
                "content-length",
                "content-type",
                "transfer-encoding",
                "connection",
                "idempotency-key",
                "user-agent",
            ]
            .contains(&header.to_ascii_lowercase().as_str())
            {
                return Outcome::InvalidDestination;
            }
            budget += secret.len();
            outgoing = outgoing.header(header, format!("{prefix}{secret}"));
        }
        if budget > 262144 {
            return Outcome::InvalidDestination;
        }
        let response = if request["has_body"] == true {
            outgoing.send(catabolic_core::encode(&request["body"]).as_bytes())
        } else {
            outgoing.send_empty()
        };
        match response {
            Ok(mut response) if response.status().is_success() => {
                validate_response(&mut response, &request["responses"])
            }
            _ => Outcome::TemporarilyFailed,
        }
    }
    pub fn send_url(&self, url: &str, envelope: &Value, event: &str, level: &str) -> Outcome {
        let scheme = url
            .split_once(':')
            .map(|(s, _)| s.to_ascii_lowercase())
            .unwrap_or_default();
        if !matches!(scheme.as_str(), "http" | "https") {
            return if [
                "ntfy", "ntfys", "discord", "mailto", "mailtos", "tgram", "gotify", "gotifys",
            ]
            .contains(&scheme.as_str())
                && policy::EVENTS.contains(&event)
                && ["info", "warning", "error"].contains(&level)
            {
                // Provider URLs retain their frozen dialect, with native transports.
                send_provider(url, envelope, event, level)
            } else {
                Outcome::InvalidDestination
            };
        }
        if callback_origin(url).is_err() {
            return Outcome::InvalidDestination;
        }
        let Some(id) = envelope
            .get("id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        else {
            return Outcome::InvalidDestination;
        };
        let payload = catabolic_core::encode(
            &serde_json::json!({"url":url,"event":event,"severity":level,"envelope":envelope}),
        );
        if payload.len() > 16384 {
            return Outcome::InvalidDestination;
        }
        let url = url.strip_suffix('#').unwrap_or(url);
        match self
            .agent
            .post(url)
            .header("Content-Type", "application/json")
            .header("Idempotency-Key", id)
            .header("User-Agent", "Catabolic-Webhook/1")
            // Frozen send() sorts the worker payload before the HTTP worker
            // uses json.dumps(envelope), including its default ASCII escaping.
            // Mapped request bodies and the input budget retain their own codec.
            .send(catabolic_core::encode_with(envelope, true, false).as_bytes())
        {
            Ok(response) if response.status().is_success() => Outcome::Complete,
            Ok(_) | Err(_) => Outcome::TemporarilyFailed,
        }
    }
}

fn send_provider(url: &str, envelope: &Value, event: &str, level: &str) -> Outcome {
    let payload = catabolic_core::encode(
        &serde_json::json!({"url":url,"event":event,"severity":level,"envelope":envelope}),
    );
    if payload.len() > 16384 {
        return Outcome::InvalidDestination;
    }
    let Ok(executable) = crate::helper::executable_with_fallback(PROVIDER_HELPER.get()) else {
        return Outcome::Uninstalled;
    };
    match crate::process::command_output(
        &[
            executable.as_os_str(),
            std::ffi::OsStr::new(WORKER_ARGUMENT),
        ],
        crate::process::CommandOptions {
            timeout: Duration::from_secs(30),
            maximum: 4096,
            input_bytes: Some(payload.as_bytes()),
            umask: Some(0o077),
            ..Default::default()
        },
    ) {
        Ok(output) if output.returncode == 0 && output.stderr.is_empty() => {
            match catabolic_core::decode_json_bytes(&output.stdout)
                .ok()
                .and_then(|v| v["status"].as_str().map(str::to_owned))
                .as_deref()
            {
                Some("complete") => Outcome::Complete,
                Some("invalid_destination") => Outcome::InvalidDestination,
                _ => Outcome::TemporarilyFailed,
            }
        }
        Err(failure) if failure.state == "unavailable" => Outcome::Uninstalled,
        _ => Outcome::TemporarilyFailed,
    }
}

// A standalone preserved CLI may host this one worker without taking over
// other helpers that require the structured companion. Explicitly selected
// companions keep priority, and an invalid selection never falls back.
static PROVIDER_HELPER: std::sync::OnceLock<crate::helper::Helper> = std::sync::OnceLock::new();
pub fn register_provider_helper(path: &std::path::Path) -> Result<()> {
    PROVIDER_HELPER
        .set(crate::helper::pin(path)?)
        .map_err(|_| Error("notification helper already registered".into()))
}

pub const WORKER_ARGUMENT: &str = "__notification-provider-worker";

/// Catalog-free native child under the existing process-group supervisor.
/// Credentials enter on bounded stdin, never argv; only constant status escapes.
pub fn worker_main() {
    use std::io::Write;
    let outcome = (|| {
        let mut bytes = Vec::new();
        std::io::stdin().take(16385).read_to_end(&mut bytes).ok()?;
        if bytes.len() > 16384 {
            return Some(Outcome::InvalidDestination);
        }
        // Only constant statuses escape; a refused request sends nothing.
        let payload = catabolic_store::filesystem_mutation::admitted_json_bytes(&bytes).ok()?;
        let (Some(url), Some(event), Some(severity)) = (
            payload["url"].as_str(),
            payload["event"].as_str(),
            payload["severity"].as_str(),
        ) else {
            return Some(Outcome::InvalidDestination);
        };
        if !policy::EVENTS.contains(&event) || !["info", "warning", "error"].contains(&severity) {
            return Some(Outcome::InvalidDestination);
        }
        Some(providers::deliver(url, event, severity))
    })()
    .unwrap_or(Outcome::TemporarilyFailed);
    let _ = writeln!(
        std::io::stdout(),
        "{}",
        serde_json::json!({"status": outcome.code()})
    );
}
impl Transport for HttpTransport {
    fn deliver(&mut self, lease: &DeliveryLease) -> Outcome {
        if policy::credential_reference(&lease.credential_env).is_err() {
            return Outcome::InvalidDestination;
        }
        let Ok(url) = std::env::var(&lease.credential_env) else {
            return Outcome::CredentialUnavailable;
        };
        if url.is_empty() {
            return Outcome::CredentialUnavailable;
        }
        self.send_url(&url, &lease.envelope(), &lease.event, &lease.severity)
    }
    fn deliver_webhook(&mut self, lease: &DeliveryLease, url: &str) -> Outcome {
        self.send_url(url, &lease.envelope(), &lease.event, &lease.severity)
    }
    fn deliver_mapped(&mut self, lease: &DeliveryLease, request: &Value) -> Outcome {
        self.send_request(request, &lease.event_id)
    }
}

pub fn drain_with_transport(
    store: &mut Store,
    profile: &str,
    limit: usize,
    tag: Option<&str>,
    transport: &mut impl Transport,
) -> Result<Value> {
    if !(1..=100).contains(&limit) {
        return Err(Error("invalid_configuration".into()));
    }
    catabolic_store::filesystem_mutation::recover_publications(store, profile, limit)?;
    let mut deliveries = Vec::new();
    for _ in 0..limit {
        let Some(lease) = policy::claim(store, profile, tag, policy::now())? else {
            break;
        };
        let database = store.path.clone();
        let database_id = store.database_id.clone();
        let outcome = store.detached(|| {
            let reader = Store::open(&database, false, false)?;
            if reader.database_id != database_id {
                return Err(Error("catalog replaced during detached execution".into()));
            }
            if !policy::lease_current(&reader, &lease, policy::now())? {
                return Ok(None);
            }
            // Resolving consumes and drops the authorization snapshot before
            // I/O, and it never falls back to an environment on denied access.
            let target = policy::delivery_target(reader, &lease);
            Ok(Some(match target {
                Ok(policy::DeliveryTarget::Environment) => transport.deliver(&lease),
                Ok(policy::DeliveryTarget::Webhook(url)) => transport.deliver_webhook(&lease, &url),
                Ok(policy::DeliveryTarget::Mapped(request)) => {
                    transport.deliver_mapped(&lease, &request)
                }
                Err(error) if error.0 == "credential_unavailable" => Outcome::CredentialUnavailable,
                Err(error) if error.0 == "invalid_destination" => Outcome::InvalidDestination,
                Err(_) => Outcome::AuthorizationUnavailable,
            }))
        })?;
        if let Some(outcome) = outcome {
            deliveries.push(policy::acknowledge(store, &lease, outcome, policy::now())?);
        }
    }
    // An unverified projection remains visible and incomplete. This adapter
    // never claims verification just because a notification was dispatched.
    policy::drain_report(store, profile, deliveries)
}

fn validate_response(
    response: &mut ureq::http::Response<ureq::Body>,
    contracts: &Value,
) -> Outcome {
    let code = response.status().as_u16().to_string();
    let Some(contract) = contracts
        .get(&code)
        .or_else(|| contracts.get("2XX"))
        .or_else(|| contracts.get("default"))
    else {
        return Outcome::UnexpectedResponse;
    };
    let has_body = contract["has_body"] == true;
    if let Some(length) = response.headers().get("Content-Length") {
        let Some(length) = length.to_str().ok().and_then(|v| v.parse::<i64>().ok()) else {
            return Outcome::InvalidResponse;
        };
        if !(0..=65536).contains(&length) || (!has_body && length != 0) {
            return Outcome::InvalidResponse;
        }
    }
    if has_body {
        let content = response
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let encoding = response
            .headers()
            .get("Content-Encoding")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("identity");
        if !content
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/json")
            || !encoding.eq_ignore_ascii_case("identity")
        {
            return Outcome::InvalidResponse;
        }
    }
    let mut raw = Vec::new();
    if response
        .body_mut()
        .as_reader()
        .take(65537)
        .read_to_end(&mut raw)
        .is_err()
    {
        return Outcome::TemporarilyFailed;
    }
    if raw.len() > 65536 {
        return Outcome::InvalidResponse;
    }
    if !has_body {
        return if raw.is_empty() {
            Outcome::Complete
        } else {
            Outcome::InvalidResponse
        };
    }
    let Ok(raw) = std::str::from_utf8(&raw) else {
        return Outcome::InvalidResponse;
    };
    let Ok(body) = catabolic_core::json::parse(raw, true) else {
        return Outcome::InvalidResponse;
    };
    if catabolic_core::validate_finite(&body).is_err() {
        return Outcome::InvalidResponse;
    }
    match catabolic_store::http_operations::validator(&contract["schema"]) {
        Ok(schema) if schema.is_valid(&body) => Outcome::Complete,
        _ => Outcome::InvalidResponse,
    }
}
pub fn drain(store: &mut Store, profile: &str, limit: usize, tag: Option<&str>) -> Result<Value> {
    drain_with_transport(store, profile, limit, tag, &mut HttpTransport::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn approved_origin_identity_and_unsafe_urls() {
        assert_eq!(
            callback_origin("HTTPS://LOCALHOST/a?key=private").unwrap(),
            callback_origin("https://localhost:443/other").unwrap()
        );
        assert_ne!(
            callback_origin("http://localhost/").unwrap(),
            callback_origin("https://localhost/").unwrap()
        );
        assert_ne!(
            callback_origin("http://localhost:81/").unwrap(),
            callback_origin("http://localhost:80/").unwrap()
        );
        assert_eq!(
            callback_origin("http://[::1]:8080/hook").unwrap().host,
            "::1"
        );
        for url in [
            "http://user:secret@localhost/hook",
            "https://localhost/hook#secret",
            "http:///hook",
            "http://@localhost/hook",
            "http://localhost:0/hook",
            "http://localhost/\nhook",
            "http://[invalid/hook",
            "file:///tmp/hook",
            "https://localhost:65536/hook",
            "http://localhost\\@evil/path",
        ] {
            assert_eq!(
                callback_origin(url),
                Err(Outcome::InvalidDestination),
                "{url}"
            );
        }
    }

    struct EditingTransport {
        database: std::path::PathBuf,
        calls: usize,
    }
    impl Transport for EditingTransport {
        fn deliver(&mut self, lease: &DeliveryLease) -> Outcome {
            self.calls += 1;
            // Opening a second writer proves both the transaction and shared
            // flock have been released around the actual effect boundary.
            let writer = Store::open(&self.database, true, false).unwrap();
            policy::control(
                &writer,
                &lease.profile,
                &lease.destination_id,
                "disable",
                policy::now(),
            )
            .unwrap();
            Outcome::Complete
        }
    }
    #[test]
    fn external_effect_releases_lock_and_superseded_ack_is_fenced() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        catabolic_store::migration::initialize(&path).unwrap();
        let mut store = Store::open(&path, true, false).unwrap();
        policy::configure(&store,"default",&json!({"id":"one","credential_env":"PRIVATE_URL","subscriptions":["scan_failed"],"apply":true})).unwrap();
        store
            .transaction(|db| {
                policy::emit(
                    db,
                    "default",
                    "scan_failed",
                    "error",
                    "private-title",
                    &json!("one"),
                )
            })
            .unwrap();
        let mut transport = EditingTransport {
            database: path,
            calls: 0,
        };
        let report = drain_with_transport(&mut store, "default", 10, None, &mut transport).unwrap();
        assert_eq!(transport.calls, 1);
        assert_eq!(report["deliveries"][0]["state"], "stale_acknowledgement");
        assert_eq!(report["complete"], true);
        assert_eq!(
            policy::status(&store, "default", 100).unwrap()["deliveries"][0]["state"],
            "cancelled"
        );
    }
}
