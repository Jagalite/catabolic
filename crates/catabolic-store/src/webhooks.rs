// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Principal-bound webhook management using the shared native access engine.
use crate::{Error, Result, access::Access, notifications};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

const TERMINAL: &[&str] = &["ready", "failed", "cancelled", "stale", "blocked"];
fn invalid() -> Error {
    Error("invalid_configuration".into())
}
fn required<'a>(request: &'a Value, key: &str) -> Result<&'a str> {
    request[key].as_str().ok_or_else(invalid)
}
fn limit(request: &Value) -> Result<usize> {
    let value = request
        .get("limit")
        .map(|v| v.as_u64().ok_or_else(invalid))
        .transpose()?
        .unwrap_or(100);
    if !(1..=1000).contains(&value) {
        return Err(invalid());
    }
    Ok(value as usize)
}
fn decode(raw: &str) -> Result<Value> {
    catabolic_core::decode_json(raw).map_err(|e| Error(e.to_string()))
}

pub fn register(
    access: &mut Access,
    url: &str,
    events: &Value,
    request: Option<&str>,
) -> Result<Value> {
    notifications::callback_origin(url).map_err(|_| Error("invalid_callback_url".into()))?;
    if let Some(request) = request {
        access.request_visible(request)?;
        access.allow_callback(url)?;
    } else {
        access.require_operator("webhooks:manage")?;
        let events = events
            .as_array()
            .ok_or_else(|| Error("invalid_webhook_events".into()))?;
        if events.is_empty()
            || events.iter().any(|v| {
                !v.as_str()
                    .is_some_and(|v| notifications::EVENTS.contains(&v))
            })
        {
            return Err(Error("invalid_webhook_events".into()));
        }
    }
    let count:i64=access.store().db.query_row("SELECT count(*) FROM api_webhooks w JOIN notification_destinations d ON d.profile=w.profile AND d.id=w.destination_id WHERE w.principal=? AND d.enabled=1 AND (w.request_id IS NULL OR EXISTS(SELECT 1 FROM api_requests r WHERE r.id=w.request_id AND r.state IN ('queued','running','validating')) OR EXISTS(SELECT 1 FROM notification_deliveries n WHERE n.profile=w.profile AND n.destination_id=w.destination_id AND n.state NOT IN ('complete','cancelled')))",[&access.principal()],|r|r.get(0))?;
    if count >= 1000 {
        return Err(Error("webhook_limit".into()));
    }
    let id = format!("http:{}", uuid::Uuid::new_v4());
    access.store().transaction(|db|{
        if request.is_none(){
            notifications::configure(access.store(),access.profile(),&json!({"id":id,"credential_env":"CATABOLIC_HTTP_WEBHOOK","subscriptions":events,"tags":[],"min_severity":"info","apply":true}))?;
        } else {
            db.execute("INSERT INTO notification_destinations(profile,id,credential_env,subscriptions,tags,min_severity) VALUES (?,?,'CATABOLIC_HTTP_WEBHOOK','[]','[]','info')",params![access.profile(),id])?;
        }
        db.execute("INSERT INTO api_webhooks(profile,destination_id,principal,token_id,url,request_id) VALUES (?,?,?,?,?,?)",params![access.profile(),id,access.principal(),access.token_id(),url,request])?;
        if let Some(request)=request {
            let state:String=db.query_row("SELECT state FROM api_requests WHERE id=?",[request],|r|r.get(0))?;
            if TERMINAL.contains(&state.as_str()) {
                let event=format!("request:{}",uuid::Uuid::new_v4());
                db.execute("INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,?,?,?)",params![event,access.profile(),format!("request_{state}"),if state=="failed"{"error"}else{"info"},request])?;
                db.execute("INSERT INTO notification_deliveries(event_id,profile,destination_id) VALUES (?,?,?)",params![event,access.profile(),id])?;
            }
        }
        Ok(())
    })?;
    inspect(access, &id, request.is_some())
}

pub fn inspect(access: &mut Access, id: &str, request_scope: bool) -> Result<Value> {
    if !request_scope {
        access.require_operator("webhooks:manage")?;
    }
    let row=access.store().db.query_row("SELECT d.enabled,d.subscriptions,w.request_id FROM notification_destinations d JOIN api_webhooks w ON w.profile=d.profile AND w.destination_id=d.id WHERE d.profile=? AND d.id=? AND w.principal=?",params![access.profile(),id,access.principal()],|r|Ok((r.get::<_,bool>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?))).optional()?.ok_or_else(||Error("not_found".into()))?;
    if request_scope {
        let request = row.2.as_deref().ok_or_else(|| Error("not_found".into()))?;
        access.request_visible(request)?;
    }
    let events = if row.2.is_some() {
        let mut values = TERMINAL
            .iter()
            .map(|state| format!("request_{state}"))
            .collect::<Vec<_>>();
        values.sort();
        json!(values)
    } else {
        decode(&row.1)?
    };
    Ok(json!({"id":id,"enabled":row.0,"events":events,"request_id":row.2}))
}

pub fn listing(access: &mut Access, limit: usize, after: &str) -> Result<Value> {
    if !(1..=1000).contains(&limit) {
        return Err(invalid());
    }
    access.require_operator("webhooks:manage")?;
    let ids = {
        let mut statement=access.store().db.prepare("SELECT destination_id FROM api_webhooks WHERE profile=? AND principal=? AND destination_id>? ORDER BY destination_id LIMIT ?")?;
        statement
            .query_map(
                params![
                    access.profile(),
                    access.principal(),
                    after,
                    (limit + 1) as i64
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let next = if ids.len() > limit {
        Some(ids[limit - 1].clone())
    } else {
        None
    };
    let mut values = Vec::new();
    for id in ids.into_iter().take(limit) {
        values.push(inspect(access, &id, false)?);
    }
    Ok(json!({"webhooks":values,"next_cursor":next}))
}

pub fn deliveries(
    access: &mut Access,
    id: &str,
    limit: usize,
    after: &str,
    request_scope: bool,
) -> Result<Value> {
    if !(1..=1000).contains(&limit) {
        return Err(invalid());
    }
    inspect(access, id, request_scope)?;
    let mut statement=access.store().db.prepare("SELECT event_id,state,attempts,error FROM notification_deliveries WHERE profile=? AND destination_id=? AND event_id>? ORDER BY event_id LIMIT ?")?;
    let mut values=statement.query_map(params![access.profile(),id,after,(limit+1) as i64],|r|Ok(json!({"event_id":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?,"attempts":r.get::<_,i64>(2)?,"error":r.get::<_,Option<String>>(3)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let next = if values.len() > limit {
        values[limit - 1]["event_id"].clone()
    } else {
        Value::Null
    };
    values.truncate(limit);
    Ok(json!({"deliveries":values,"next_cursor":next}))
}

pub fn control(
    access: &mut Access,
    id: &str,
    operation: &str,
    request_scope: bool,
) -> Result<Value> {
    let destination = inspect(access, id, request_scope)?;
    if operation == "retry" {
        if destination["enabled"] != true {
            return Err(Error("webhook_disabled".into()));
        }
        if request_scope {
            let url: String = access.store().db.query_row(
                "SELECT url FROM api_webhooks WHERE profile=? AND destination_id=?",
                params![access.profile(), id],
                |r| r.get(0),
            )?;
            access.allow_callback(&url)?;
        }
    }
    access.store().transaction(|db| {
        notifications::control(
            access.store(),
            access.profile(),
            id,
            operation,
            notifications::now(),
        )?;
        if operation == "retry" {
            db.execute(
                "UPDATE api_webhooks SET token_id=? WHERE profile=? AND destination_id=?",
                params![access.token_id(), access.profile(), id],
            )?;
        }
        Ok(())
    })?;
    inspect(access, id, request_scope)
}

pub fn execute(access: &mut Access, operation: &str, request: &Value) -> Result<Value> {
    if !request.is_object() {
        return Err(invalid());
    }
    let request_scope = request["request_scope"] == true;
    match operation {
        "register" => register(
            access,
            required(request, "url")?,
            request.get("events").unwrap_or(&json!([])),
            request
                .get("request_id")
                .map(|v| v.as_str().ok_or_else(invalid))
                .transpose()?,
        ),
        "inspect" => inspect(access, required(request, "id")?, request_scope),
        "list" => listing(
            access,
            limit(request)?,
            request["after"].as_str().unwrap_or(""),
        ),
        "deliveries" => deliveries(
            access,
            required(request, "id")?,
            limit(request)?,
            request["after"].as_str().unwrap_or(""),
            request_scope,
        ),
        "retry" | "disable" => control(access, required(request, "id")?, operation, request_scope),
        _ => Err(Error("unknown_webhook_operation".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    fn fixture() -> (tempfile::TempDir, Access) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store.transaction(|db|{
            db.execute("INSERT INTO api_principals(id,profile) VALUES ('operator','default')",[])?;
            db.execute("INSERT INTO api_grants(id,principal,definition) VALUES ('g','operator',?)",[catabolic_core::encode(&json!({"actions":["webhooks:manage"],"operator":true}))])?;
            db.execute("INSERT INTO api_tokens(id,principal,digest,grants,expires) VALUES ('t','operator',?,'[\"g\"]',9999999999)",[catabolic_core::sha256("t.disposable")])?;
            Ok(())
        }).unwrap();
        (root, Access::authenticate(store, "t.disposable").unwrap())
    }
    #[test]
    fn bearer_authentication_redacted_management_and_pagination() {
        let (root, mut access) = fixture();
        let mut expected = Vec::new();
        for _ in 0..3 {
            let value = register(
                &mut access,
                "https://example.invalid/hook?key=PRIVATE",
                &json!(["job_failed"]),
                None,
            )
            .unwrap();
            assert!(!value.to_string().contains("PRIVATE"));
            expected.push(value["id"].as_str().unwrap().to_owned());
        }
        expected.sort();
        let first = listing(&mut access, 2, "").unwrap();
        let second = listing(&mut access, 2, first["next_cursor"].as_str().unwrap()).unwrap();
        let actual = first["webhooks"]
            .as_array()
            .unwrap()
            .iter()
            .chain(second["webhooks"].as_array().unwrap())
            .map(|v| v["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        assert_eq!(second["next_cursor"], Value::Null);
        assert_eq!(
            inspect(&mut access, &expected[0], true).unwrap_err().0,
            "not_found"
        );
        drop(access);
        assert!(
            Access::authenticate(
                Store::open(&root.path().join("catalog.db"), false, false).unwrap(),
                "t.wrong"
            )
            .is_err()
        );
    }
    #[test]
    fn management_is_principal_scoped_and_invalid_url_is_atomic() {
        let (_root, mut access) = fixture();
        let id = register(
            &mut access,
            "https://example.invalid/hook",
            &json!(["scan_failed"]),
            None,
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        for url in [
            "http://user:private@host/",
            "http:///path",
            "https://host/#fragment",
        ] {
            assert!(register(&mut access, url, &json!(["scan_failed"]), None).is_err());
        }
        assert_eq!(
            listing(&mut access, 100, "").unwrap()["webhooks"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        access
            .store()
            .transaction(|db| {
                db.execute(
                    "INSERT INTO api_principals(id,profile) VALUES ('another-principal','default')",
                    [],
                )?;
                db.execute(
                    "UPDATE api_webhooks SET principal='another-principal' WHERE destination_id=?",
                    [&id],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(inspect(&mut access, &id, false).unwrap_err().0, "not_found");
        assert!(
            listing(&mut access, 100, "").unwrap()["webhooks"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn retry_preserves_live_token_and_disable_fences_deliveries() {
        let (_root, mut access) = fixture();
        let id = register(
            &mut access,
            "https://example.invalid/hook",
            &json!(["scan_failed"]),
            None,
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        access
            .store()
            .transaction(|db| {
                notifications::emit(
                    db,
                    "default",
                    "scan_failed",
                    "error",
                    "private-title",
                    &json!("test"),
                )
            })
            .unwrap();
        let lease = notifications::claim(access.store(), "default", None, notifications::now())
            .unwrap()
            .unwrap();
        assert_eq!(
            control(&mut access, &id, "retry", false).unwrap_err().0,
            "delivery_in_progress"
        );
        assert!(
            notifications::lease_current(access.store(), &lease, notifications::now()).unwrap()
        );
        assert_eq!(
            control(&mut access, &id, "disable", false).unwrap()["enabled"],
            false
        );
        assert_eq!(
            deliveries(&mut access, &id, 100, "", false).unwrap()["deliveries"][0]["state"],
            "cancelled"
        );
        assert_eq!(
            control(&mut access, &id, "retry", false).unwrap_err().0,
            "webhook_disabled"
        );
    }
}
