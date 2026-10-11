// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Durable notification policy and delivery state. Network I/O belongs to runtime.
//!
//! Destination revisions and expiring, single-use lease tokens fence every
//! acknowledgement. Editing a policy cancels old work rather than retargeting it.
use crate::{Error, Result, Store};
use catabolic_core::{encode, sha256};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    net::Ipv6Addr,
    time::{SystemTime, UNIX_EPOCH},
};
use unicode_normalization::UnicodeNormalization;

/// Normalized approved-origin identity. Paths and queries never form part of an
/// origin; userinfo, control characters, backslashes and fragments are refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}
pub fn callback_origin(value: &str) -> std::result::Result<Origin, Outcome> {
    let invalid = || Outcome::InvalidDestination;
    if value.is_empty()
        || value.chars().count() > 4096
        || value.chars().any(|c| c <= ' ' || c == '\u{7f}')
        || value.contains('\\')
    {
        return Err(invalid());
    }
    let (scheme, rest) = value.split_once("://").ok_or_else(invalid)?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return Err(invalid());
    }
    if value
        .split_once('#')
        .is_some_and(|(_, fragment)| !fragment.is_empty())
    {
        return Err(invalid());
    }
    let authority = rest.split(['/', '?', '#']).next().ok_or_else(invalid)?;
    if authority.is_empty() || authority.contains('@') {
        return Err(invalid());
    }
    let normalized = authority
        .replace(['@', ':', '#', '?'], "")
        .nfkc()
        .collect::<String>();
    if normalized.contains(['/', '?', '#', '@', ':']) {
        return Err(invalid());
    }
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, suffix) = bracketed.split_once(']').ok_or_else(invalid)?;
        // Python also accepts scoped IPv6 addresses. The scope stays part of
        // origin identity, so an approval for one interface cannot match another.
        if let Some((_, scope)) = host.split_once('%')
            && (scope.is_empty() || scope.contains('%'))
        {
            return Err(invalid());
        }
        host.split('%')
            .next()
            .ok_or_else(invalid)?
            .parse::<Ipv6Addr>()
            .map_err(|_| invalid())?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or_else(invalid)?)
        };
        (host, port)
    } else {
        if authority.contains(['[', ']']) {
            return Err(invalid());
        }
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return Err(invalid());
    }
    let port = match port.filter(|p| !p.is_empty()) {
        Some(p) if p.bytes().all(|c| c.is_ascii_digit()) => {
            p.parse::<u16>().map_err(|_| invalid())?
        }
        Some(_) => return Err(invalid()),
        None => {
            if scheme == "https" {
                443
            } else {
                80
            }
        }
    };
    if port == 0 {
        return Err(invalid());
    }
    Ok(Origin {
        scheme,
        host: host.to_lowercase(),
        port,
    })
}

pub const EVENTS: &[&str] = &[
    "job_completed",
    "job_failed",
    "fallback_selected",
    "fallback_unresolved",
    "projection_updated",
    "scan_requested",
    "scan_failed",
    "consumer_needs_repair",
];

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn error(code: &str) -> Error {
    Error(code.into())
}
fn text(value: &Value, maximum: usize) -> Result<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.chars().count() <= maximum && !s.chars().any(|c| c < ' '))
        .ok_or_else(|| error("invalid_configuration"))
}
pub fn credential_reference(value: &str) -> Result<()> {
    let mut chars = value.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(error("invalid_configuration"));
    }
    Ok(())
}
fn severity(value: &str) -> Option<u8> {
    match value {
        "info" => Some(0),
        "warning" => Some(1),
        "error" => Some(2),
        _ => None,
    }
}

/// Configuration is a dry run unless `apply` is true. Only credential *names*
/// are persisted; this function never reads process credentials.
pub fn configure(store: &Store, profile: &str, request: &Value) -> Result<Value> {
    let id = text(&request["id"], 255)?;
    let environment = text(&request["credential_env"], usize::MAX)?;
    credential_reference(environment)?;
    let subscriptions = request["subscriptions"]
        .as_array()
        .ok_or_else(|| error("invalid_notification_policy"))?;
    let empty_tags = json!([]);
    let tags_value = request.get("tags").unwrap_or(&empty_tags);
    let tags = tags_value
        .as_array()
        .ok_or_else(|| error("invalid_notification_policy"))?;
    let minimum = request
        .get("min_severity")
        .map(|v| {
            v.as_str()
                .ok_or_else(|| error("invalid_notification_policy"))
        })
        .transpose()?
        .unwrap_or("info");
    if subscriptions.is_empty()
        || subscriptions
            .iter()
            .any(|e| !e.as_str().is_some_and(|e| EVENTS.contains(&e)))
        || severity(minimum).is_none()
        || tags.len() > 20
    {
        return Err(error("invalid_notification_policy"));
    }
    for tag in tags {
        text(tag, 64)?;
    }
    let apply = request
        .get("apply")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let subscriptions_json = encode(&request["subscriptions"]);
    let tags_json = encode(tags_value);
    let unchanged = store.db.query_row(
        "SELECT credential_env=? AND subscriptions=? AND tags=? AND min_severity=? AND enabled=1 FROM notification_destinations WHERE profile=? AND id=?",
        params![environment, subscriptions_json, tags_json, minimum, profile, id], |r| r.get::<_, bool>(0),
    ).optional()?.unwrap_or(false);
    if apply && !unchanged {
        store.transaction(|db| {
            db.execute("INSERT INTO notification_destinations(profile,id,credential_env,subscriptions,tags,min_severity,enabled) VALUES (?,?,?,?,?,?,1) ON CONFLICT(profile,id) DO UPDATE SET credential_env=excluded.credential_env,subscriptions=excluded.subscriptions,tags=excluded.tags,min_severity=excluded.min_severity,enabled=1,revision=notification_destinations.revision+1",
                params![profile, id, environment, subscriptions_json, tags_json, minimum])?;
            cancel(db, profile, id)?;
            Ok(())
        })?;
    }
    Ok(
        json!({"id":id,"credential_env":environment,"subscriptions":subscriptions,"tags":tags,"min_severity":minimum,"applied":apply,"complete":true}),
    )
}

/// Must run in the caller's transaction, so producing an event and its matching
/// destination deliveries is atomic with the underlying domain change.
pub fn emit(
    db: &Connection,
    profile: &str,
    event: &str,
    level: &str,
    subject: &str,
    identity: &Value,
) -> Result<String> {
    if db.is_autocommit() {
        return Err(error("notification_emit_requires_transaction"));
    }
    let rank = severity(level).ok_or_else(|| error("invalid_notification_policy"))?;
    let id = sha256(encode(&json!([profile, event, identity])));
    if db.execute("INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,?,?,?) ON CONFLICT DO NOTHING", params![id,profile,event,level,subject])? == 0 {
        return Ok(id);
    }
    let mut statement = db.prepare("SELECT id,subscriptions,min_severity FROM notification_destinations WHERE profile=? AND enabled=1")?;
    let rows = statement.query_map([profile], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (destination, subscriptions, minimum) = row?;
        let subscriptions: Vec<String> = serde_json::from_str(&subscriptions)?;
        if subscriptions.iter().any(|e| e == event)
            && rank >= severity(&minimum).ok_or_else(|| error("invalid_notification_policy"))?
        {
            db.execute("INSERT INTO notification_deliveries(event_id,profile,destination_id) VALUES (?,?,?) ON CONFLICT DO NOTHING", params![id,profile,destination])?;
        }
    }
    Ok(id)
}

fn cancel(db: &Connection, profile: &str, id: &str) -> Result<()> {
    db.execute("UPDATE notification_deliveries SET state='cancelled',lease_token=NULL,lease_until=NULL WHERE profile=? AND destination_id=? AND state!='complete'", params![profile,id])?;
    Ok(())
}

pub fn control(
    store: &Store,
    profile: &str,
    id: &str,
    operation: &str,
    timestamp: f64,
) -> Result<Value> {
    if !timestamp.is_finite() {
        return Err(error("invalid_configuration"));
    }
    store.transaction(|db| {
        let enabled = db.query_row("SELECT enabled FROM notification_destinations WHERE profile=? AND id=?", params![profile,id], |r| r.get::<_, bool>(0)).optional()?.ok_or_else(|| error("unknown_destination"))?;
        match operation {
            "disable" => {
                db.execute("UPDATE notification_destinations SET enabled=0,revision=revision+1 WHERE profile=? AND id=?", params![profile,id])?;
                cancel(db, profile, id)?;
            }
            "retry" => {
                if !enabled { return Err(error("webhook_disabled")); }
                if db.query_row("SELECT EXISTS(SELECT 1 FROM notification_deliveries WHERE profile=? AND destination_id=? AND state='leased' AND lease_until>?)", params![profile,id,timestamp], |r| r.get::<_, bool>(0))? {
                    return Err(error("delivery_in_progress"));
                }
                db.execute("UPDATE notification_deliveries SET state='pending',attempts=0,due_at=0,error=NULL,lease_token=NULL,lease_until=NULL WHERE profile=? AND destination_id=? AND state NOT IN ('complete','cancelled')", params![profile,id])?;
            }
            _ => return Err(error("invalid_configuration")),
        }
        Ok(json!({"id":id,"operation":operation,"complete":true}))
    })
}

#[derive(Clone, Debug)]
pub struct DeliveryLease {
    pub profile: String,
    pub event_id: String,
    pub destination_id: String,
    pub credential_env: String,
    pub destination_revision: i64,
    /// Attempts before this claim (matching the frozen backoff calculation).
    pub previous_attempts: u32,
    pub token: String,
    pub until: f64,
    pub event: String,
    pub severity: String,
    pub subject: String,
    pub created_at: String,
}
impl DeliveryLease {
    /// Human summaries never disclose catalog paths or media names. Job and
    /// request events carry only the documented durable identifiers.
    pub fn envelope(&self) -> Value {
        let mut value = json!({"version":1,"id":self.event_id,"event":self.event,"profile":self.profile,"severity":self.severity,"created_at":self.created_at});
        if matches!(self.event.as_str(), "job_completed" | "job_failed") {
            value["job_id"] = json!(self.subject);
        } else if let Some(state) = self.event.strip_prefix("request_") {
            value["request_id"] = json!(self.subject);
            value["state"] = json!(state);
            value["status_url"] = json!(format!("/v1/rendition-requests/{}", self.subject));
        }
        value
    }
}

pub fn claim(
    store: &Store,
    profile: &str,
    tag: Option<&str>,
    timestamp: f64,
) -> Result<Option<DeliveryLease>> {
    if !timestamp.is_finite() {
        return Err(error("invalid_configuration"));
    }
    store.transaction(|db| {
        db.execute("UPDATE notification_deliveries SET state='exhausted',lease_token=NULL,lease_until=NULL WHERE profile=? AND state IN ('pending','retry','leased') AND attempts>=5 AND (lease_until IS NULL OR lease_until<=?)", params![profile,timestamp])?;
        let lease = db.query_row(
            "SELECT n.event_id,n.destination_id,n.attempts,d.credential_env,d.revision,e.event,e.severity,e.subject,e.created_at FROM notification_deliveries n JOIN notification_destinations d ON d.profile=n.profile AND d.id=n.destination_id JOIN consumer_events e ON e.id=n.event_id AND e.profile=n.profile WHERE n.profile=? AND d.enabled=1 AND n.state IN ('pending','retry','leased') AND n.due_at<=? AND (n.lease_until IS NULL OR n.lease_until<=?) AND (? IS NULL OR EXISTS(SELECT 1 FROM json_each(d.tags) WHERE value=?)) ORDER BY e.created_at,n.event_id LIMIT 1",
            params![profile,timestamp,timestamp,tag,tag], |r| Ok(DeliveryLease {
                profile:profile.into(),event_id:r.get(0)?,destination_id:r.get(1)?,previous_attempts:r.get(2)?,credential_env:r.get(3)?,destination_revision:r.get(4)?,event:r.get(5)?,severity:r.get(6)?,subject:r.get(7)?,created_at:r.get(8)?,token:uuid::Uuid::new_v4().to_string(),until:timestamp+60.0,
            }),
        ).optional()?;
        if let Some(lease) = &lease {
            db.execute("UPDATE notification_deliveries SET state='leased',attempts=attempts+1,lease_token=?,lease_until=? WHERE profile=? AND event_id=? AND destination_id=?", params![lease.token,lease.until,profile,lease.event_id,lease.destination_id])?;
        }
        Ok(lease)
    })
}

/// Recheck immediately before I/O. An edit after dispatch cannot unsend bytes;
/// acknowledge nevertheless rechecks the revision and token independently.
pub fn lease_current(store: &Store, lease: &DeliveryLease, timestamp: f64) -> Result<bool> {
    Ok(store.db.query_row("SELECT EXISTS(SELECT 1 FROM notification_deliveries n JOIN notification_destinations d ON d.profile=n.profile AND d.id=n.destination_id WHERE n.profile=? AND n.event_id=? AND n.destination_id=? AND n.state='leased' AND n.lease_token=? AND n.lease_until>? AND d.enabled=1 AND d.revision=?)", params![lease.profile,lease.event_id,lease.destination_id,lease.token,timestamp,lease.destination_revision], |r| r.get(0))?)
}

/// HTTP-owned callbacks and mapped operations require the shared access engine.
/// A local environment destination must never become a fallback for those rows.
pub fn requires_authorization(store: &Store, lease: &DeliveryLease) -> Result<bool> {
    Ok(store.db.query_row("SELECT EXISTS(SELECT 1 FROM api_webhooks WHERE profile=? AND destination_id=?) OR EXISTS(SELECT 1 FROM http_mapping_deliveries WHERE event_id=?)", params![lease.profile,lease.destination_id,lease.event_id], |r| r.get(0))?)
}

/// A resolved secret URL exists only for the lifetime of one detached dispatch.
/// Deliberately does not implement Debug or Serialize.
pub enum DeliveryTarget {
    Environment,
    Webhook(String),
    Mapped(Value),
}
pub fn delivery_target(store: Store, lease: &DeliveryLease) -> Result<DeliveryTarget> {
    let mapped = store
        .db
        .query_row(
            "SELECT operation_id,request,token_id FROM http_mapping_deliveries WHERE event_id=?",
            [&lease.event_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((operation_id, request, token)) = mapped {
        if lease.destination_id != format!("operation:{operation_id}") {
            return Err(error("authorization_unavailable"));
        }
        let operation = crate::http_operations::get(&store, &lease.profile, &operation_id)?;
        let _authorized =
            crate::http_operations::authorize(store, &lease.profile, &operation, token.as_deref())?;
        let mut request =
            catabolic_core::decode_json(&request).map_err(|_| error("invalid_destination"))?;
        let definition = &operation["definition"];
        let target = callback_origin(
            request["url"]
                .as_str()
                .ok_or_else(|| error("invalid_destination"))?,
        )
        .map_err(|_| error("invalid_destination"))?;
        let approved = callback_origin(
            definition["base_url"]
                .as_str()
                .ok_or_else(|| error("invalid_destination"))?,
        )
        .map_err(|_| error("invalid_destination"))?;
        if target != approved
            || request["method"] != definition["method"]
            || !definition["responses"].is_object()
        {
            return Err(error("invalid_destination"));
        }
        request["auth"] = definition["auth"].clone();
        request["responses"] = definition["responses"].clone();
        return Ok(DeliveryTarget::Mapped(request));
    }
    let row=store.db.query_row("SELECT principal,token_id,url,request_id FROM api_webhooks WHERE profile=? AND destination_id=?",params![lease.profile,lease.destination_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?))).optional()?;
    let Some((principal, token, url, request)) = row else {
        return Ok(DeliveryTarget::Environment);
    };
    let mut access = crate::access::Access::from_stored_token(store, &token)?;
    if access.profile() != lease.profile || access.principal() != principal {
        return Err(error("authorization_unavailable"));
    }
    if let Some(request) = request {
        access.request_visible(&request)?;
        access.allow_callback(&url)?;
    } else {
        access.require_operator("webhooks:manage")?;
    }
    callback_origin(&url).map_err(|_| error("invalid_destination"))?;
    Ok(DeliveryTarget::Webhook(url))
}

/// Only stable outcome codes escape the transport, never URLs, response bodies
/// or underlying TLS/HTTP errors, which may embed credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Complete,
    TemporarilyFailed,
    Uninstalled,
    CredentialUnavailable,
    InvalidDestination,
    AuthorizationUnavailable,
    InvalidResponse,
    UnexpectedResponse,
}
impl Outcome {
    pub fn code(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::TemporarilyFailed => "temporarily_failed",
            Self::Uninstalled => "uninstalled",
            Self::CredentialUnavailable => "credential_unavailable",
            Self::InvalidDestination => "invalid_destination",
            Self::AuthorizationUnavailable => "authorization_unavailable",
            Self::InvalidResponse => "invalid_response",
            Self::UnexpectedResponse => "unexpected_response",
        }
    }
}
pub fn acknowledge(
    store: &Store,
    lease: &DeliveryLease,
    outcome: Outcome,
    timestamp: f64,
) -> Result<Value> {
    if !timestamp.is_finite() {
        return Err(error("invalid_configuration"));
    }
    let state = match outcome {
        Outcome::Complete => "complete",
        Outcome::TemporarilyFailed if lease.previous_attempts + 1 >= 5 => "exhausted",
        Outcome::TemporarilyFailed => "retry",
        _ => "repair",
    };
    let backoff = 5.0 * (2_u32.pow(lease.previous_attempts.min(10)) as f64);
    let updated = store.transaction(|db| Ok(db.execute(
        "UPDATE notification_deliveries SET state=?,error=?,lease_token=NULL,lease_until=NULL,due_at=? WHERE profile=? AND event_id=? AND destination_id=? AND lease_token=? AND lease_until>? AND EXISTS(SELECT 1 FROM notification_destinations d WHERE d.profile=notification_deliveries.profile AND d.id=notification_deliveries.destination_id AND d.enabled=1 AND d.revision=?)",
        params![state,if outcome == Outcome::Complete {None} else {Some(outcome.code())},timestamp+backoff.min(3600.0),lease.profile,lease.event_id,lease.destination_id,lease.token,timestamp,lease.destination_revision],
    )?))?;
    Ok(
        json!({"event_id":lease.event_id,"destination_id":lease.destination_id,"state":if updated==0 {"stale_acknowledgement"} else {state}}),
    )
}

pub fn pending_publications(store: &Store, profile: &str) -> Result<i64> {
    Ok(store.db.query_row("SELECT CASE WHEN EXISTS(SELECT 1 FROM notification_destinations WHERE profile=? AND enabled=1 AND min_severity='info' AND EXISTS(SELECT 1 FROM json_each(subscriptions) WHERE value='projection_updated')) THEN (SELECT count(*) FROM publication_generations WHERE profile=? AND generation>verified) ELSE 0 END", params![profile,profile], |r| r.get(0))?)
}
pub fn drain_report(store: &Store, profile: &str, deliveries: Vec<Value>) -> Result<Value> {
    let pending: i64 = store.db.query_row("SELECT count(*) FROM notification_deliveries n JOIN notification_destinations d ON d.profile=n.profile AND d.id=n.destination_id WHERE n.profile=? AND d.enabled=1 AND n.state NOT IN ('complete','cancelled')", [profile], |r| r.get(0))?;
    let publications = pending_publications(store, profile)?;
    Ok(
        json!({"deliveries":deliveries,"unresolved":pending,"unverified_publications":publications,"complete":pending==0 && publications==0}),
    )
}

fn rows(db: &Connection, sql: &str, profile: &str, limit: usize) -> Result<Vec<Value>> {
    let mut statement = db.prepare(sql)?;
    let columns: Vec<_> = statement
        .column_names()
        .into_iter()
        .map(String::from)
        .collect();
    let mapped = statement.query_map(params![profile, limit as i64], |row| {
        let mut object = serde_json::Map::new();
        for (i, name) in columns.iter().enumerate() {
            use rusqlite::types::ValueRef;
            let value = match row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(v) => json!(v),
                ValueRef::Real(v) => json!(v),
                ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
                ValueRef::Blob(_) => Value::Null,
            };
            object.insert(name.clone(), value);
        }
        Ok(Value::Object(object))
    })?;
    Ok(mapped.collect::<std::result::Result<_, _>>()?)
}
pub fn status(store: &Store, profile: &str, limit: usize) -> Result<Value> {
    if !(1..=1000).contains(&limit) {
        return Err(error("invalid_configuration"));
    }
    let mut destinations = rows(
        &store.db,
        "SELECT * FROM notification_destinations WHERE profile=? ORDER BY id LIMIT ?",
        profile,
        limit + 1,
    )?;
    let mut deliveries = rows(
        &store.db,
        "SELECT * FROM notification_deliveries WHERE profile=? ORDER BY event_id LIMIT ?",
        profile,
        limit + 1,
    )?;
    let destinations_truncated = destinations.len() > limit;
    let deliveries_truncated = deliveries.len() > limit;
    destinations.truncate(limit);
    deliveries.truncate(limit);
    let publications = pending_publications(store, profile)?;
    let pending: bool = store.db.query_row("SELECT EXISTS(SELECT 1 FROM notification_deliveries WHERE profile=? AND state NOT IN ('complete','cancelled'))",[profile],|r|r.get(0))?;
    Ok(
        json!({"destinations":destinations,"deliveries":deliveries,"destinations_truncated":destinations_truncated,"deliveries_truncated":deliveries_truncated,"unverified_publications":publications,"complete":!pending && publications==0}),
    )
}

pub fn execute(
    store: &mut Store,
    profile: &str,
    operation: &str,
    request: &Value,
) -> Result<Value> {
    if !request.is_object() {
        return Err(error("invalid_configuration"));
    }
    match operation {
        "put" | "configure" => configure(store, profile, request),
        "status" => status(
            store,
            profile,
            request
                .get("limit")
                .map(|v| v.as_u64().ok_or_else(|| error("invalid_configuration")))
                .transpose()?
                .unwrap_or(100) as usize,
        ),
        "disable" | "retry" => {
            control(store, profile, text(&request["id"], 255)?, operation, now())
        }
        "emit" => {
            let event = text(&request["event"], 64)?;
            let level = text(&request["severity"], 32)?;
            let subject = request["subject"]
                .as_str()
                .ok_or_else(|| error("invalid_configuration"))?;
            let identity = request
                .get("identity")
                .ok_or_else(|| error("invalid_configuration"))?;
            let id = store.transaction(|db| emit(db, profile, event, level, subject, identity))?;
            Ok(json!({"id":id,"complete":true}))
        }
        _ => Err(error("unknown_notification_operation")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        (root, store)
    }
    fn put(store: &Store, id: &str, minimum: &str, tags: Value) {
        configure(store,"default",&json!({"id":id,"credential_env":"TEST_URL","subscriptions":["projection_updated","scan_failed"],"tags":tags,"min_severity":minimum,"apply":true})).unwrap();
    }
    fn event(store: &Store, id: &str) -> String {
        store
            .transaction(|db| {
                emit(
                    db,
                    "default",
                    "projection_updated",
                    "info",
                    "private-title",
                    &json!(id),
                )
            })
            .unwrap()
    }
    fn delivery_state(store: &Store) -> Value {
        status(store, "default", 100).unwrap()["deliveries"][0].clone()
    }

    #[test]
    fn producer_events_are_not_subscription_policy() {
        let (_root, store) = fixture();
        let id = store
            .transaction(|db| {
                emit(
                    db,
                    "default",
                    "watcher_changed",
                    "info",
                    "watch",
                    &json!("one"),
                )
            })
            .unwrap();
        assert_eq!(id, sha256("[\"default\", \"watcher_changed\", \"one\"]"));
        assert!(configure(&store,"default",&json!({"id":"watch","credential_env":"TEST_URL","subscriptions":["watcher_changed"],"apply":true})).is_err());
    }

    #[test]
    fn exact_identity_routing_atomicity_and_idempotent_policy() {
        let (_root, store) = fixture();
        put(&store, "one", "info", json!(["home"]));
        put(&store, "errors", "error", json!(["ops"]));
        let id = event(&store, "café😀");
        assert_eq!(
            id,
            sha256("[\"default\", \"projection_updated\", \"café😀\"]")
        );
        assert_eq!(event(&store, "café😀"), id);
        assert_eq!(
            status(&store, "default", 100).unwrap()["deliveries"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let lease = claim(&store, "default", Some("home"), 100.0)
            .unwrap()
            .unwrap();
        put(&store, "one", "info", json!(["home"]));
        assert!(lease_current(&store, &lease, 101.0).unwrap());
        assert_eq!(lease.destination_revision, 1);
        assert!(!lease.envelope().to_string().contains("private-title"));
        let failure: Result<()> = store.transaction(|db| {
            emit(
                db,
                "default",
                "scan_failed",
                "error",
                "private-title",
                &json!("rollback"),
            )?;
            Err(error("rollback"))
        });
        assert!(failure.is_err());
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM consumer_events", [], |r| r
                    .get::<_, u32>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn edits_cancel_inflight_work_and_old_ack_cannot_complete() {
        let (_root, store) = fixture();
        put(&store, "one", "info", json!(["home"]));
        event(&store, "one");
        let lease = claim(&store, "default", None, 100.0).unwrap().unwrap();
        put(&store, "one", "info", json!(["changed"]));
        assert!(!lease_current(&store, &lease, 101.0).unwrap());
        assert_eq!(
            acknowledge(&store, &lease, Outcome::Complete, 101.0).unwrap()["state"],
            "stale_acknowledgement"
        );
        assert_eq!(delivery_state(&store)["state"], "cancelled");
        control(&store, "default", "one", "retry", 102.0).unwrap();
        assert_eq!(delivery_state(&store)["state"], "cancelled");
        assert!(
            drain_report(&store, "default", vec![]).unwrap()["complete"]
                .as_bool()
                .unwrap()
        );
    }

    #[test]
    fn lease_expiry_reclaim_and_single_use_ack() {
        let (_root, store) = fixture();
        put(&store, "one", "info", json!([]));
        event(&store, "one");
        let old = claim(&store, "default", None, 100.0).unwrap().unwrap();
        assert!(claim(&store, "default", None, 159.999).unwrap().is_none());
        assert_eq!(
            control(&store, "default", "one", "retry", 159.999)
                .unwrap_err()
                .0,
            "delivery_in_progress"
        );
        assert_eq!(delivery_state(&store)["lease_token"], old.token);
        let new = claim(&store, "default", None, 160.0).unwrap().unwrap();
        assert_ne!(new.token, old.token);
        assert_eq!(
            acknowledge(&store, &old, Outcome::Complete, 160.1).unwrap()["state"],
            "stale_acknowledgement"
        );
        assert_eq!(
            acknowledge(&store, &new, Outcome::Complete, 161.0).unwrap()["state"],
            "complete"
        );
        assert_eq!(
            acknowledge(&store, &new, Outcome::TemporarilyFailed, 161.1).unwrap()["state"],
            "stale_acknowledgement"
        );
        assert_eq!(delivery_state(&store)["state"], "complete");
    }

    #[test]
    fn five_attempts_backoff_exhaustion_and_explicit_retry() {
        let (_root, store) = fixture();
        put(&store, "one", "info", json!([]));
        event(&store, "one");
        let mut clock = 100.0;
        for attempt in 0..5 {
            let lease = claim(&store, "default", None, clock).unwrap().unwrap();
            assert_eq!(lease.previous_attempts, attempt);
            let result =
                acknowledge(&store, &lease, Outcome::TemporarilyFailed, clock + 1.0).unwrap();
            assert_eq!(
                result["state"],
                if attempt == 4 { "exhausted" } else { "retry" }
            );
            clock += 1.0 + 5.0 * (2_u32.pow(attempt) as f64);
            assert_eq!(delivery_state(&store)["due_at"], json!(clock));
            assert!(
                claim(&store, "default", None, clock - 0.001)
                    .unwrap()
                    .is_none()
            );
        }
        assert!(
            claim(&store, "default", None, clock + 1000.0)
                .unwrap()
                .is_none()
        );
        assert_eq!(delivery_state(&store)["attempts"], 5);
        control(&store, "default", "one", "retry", clock).unwrap();
        assert_eq!(delivery_state(&store)["attempts"], 0);
        assert_eq!(delivery_state(&store)["error"], Value::Null);
        let lease = claim(&store, "default", None, clock).unwrap().unwrap();
        acknowledge(&store, &lease, Outcome::CredentialUnavailable, clock + 1.0).unwrap();
        assert_eq!(delivery_state(&store)["state"], "repair");
        assert!(
            claim(&store, "default", None, clock + 1000.0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn fifth_abandoned_lease_exhausts_only_after_expiry() {
        let (_root, store) = fixture();
        put(&store, "one", "info", json!([]));
        event(&store, "one");
        let mut last = None;
        for i in 0..5 {
            last = claim(&store, "default", None, 100.0 + 60.0 * (i as f64)).unwrap();
        }
        let lease = last.unwrap();
        assert!(
            claim(&store, "default", None, lease.until - 0.1)
                .unwrap()
                .is_none()
        );
        assert_eq!(delivery_state(&store)["state"], "leased");
        assert!(
            claim(&store, "default", None, lease.until)
                .unwrap()
                .is_none()
        );
        assert_eq!(delivery_state(&store)["state"], "exhausted");
        assert_eq!(delivery_state(&store)["lease_token"], Value::Null);
    }

    #[test]
    fn disabled_and_successful_deliveries_are_not_retried() {
        let (_root, store) = fixture();
        put(&store, "one", "info", json!([]));
        event(&store, "one");
        let lease = claim(&store, "default", None, 100.0).unwrap().unwrap();
        acknowledge(&store, &lease, Outcome::Complete, 101.0).unwrap();
        control(&store, "default", "one", "retry", 102.0).unwrap();
        assert_eq!(delivery_state(&store)["attempts"], 1);
        control(&store, "default", "one", "disable", 102.0).unwrap();
        assert_eq!(delivery_state(&store)["state"], "complete");
        assert_eq!(
            control(&store, "default", "one", "retry", 103.0)
                .unwrap_err()
                .0,
            "webhook_disabled"
        );
    }

    #[test]
    fn dry_run_and_read_only_refusal_leave_policy_unchanged() {
        let (root, store) = fixture();
        let request = json!({"id":"one","credential_env":"PRIVATE_URL","subscriptions":["scan_failed"],"apply":false});
        configure(&store, "default", &request).unwrap();
        assert!(
            status(&store, "default", 100).unwrap()["destinations"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        drop(store);
        let reader = Store::open(&root.path().join("catalog.db"), false, false).unwrap();
        let mut applied = request;
        applied["apply"] = json!(true);
        assert!(
            configure(&reader, "default", &applied)
                .unwrap_err()
                .0
                .contains("read-only")
        );
        for value in [
            "https://secret.example/path",
            "file:/secret",
            "NAME=value",
            "",
            "9NAME",
        ] {
            assert!(credential_reference(value).is_err());
        }
    }
}
