// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Whole-batch reviewed Plex metadata publication. Every HTTP request releases
//! the catalog; a separate publication lock remains held through verification.
//! An unknown PUT outcome is never replayed and is never reported as verified.
use crate::consumers::{Adapter, network, quote};
use catabolic_store::{Error, Result, Store, consumers, plex_metadata as policy};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
pub const INTERRUPTED: &str = "Plex metadata interrupted";
fn checkpoint(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error(INTERRUPTED.into()))
    } else {
        Ok(())
    }
}
/// Join each bounded request even on interruption; do not leave an effect worker
/// running beyond the owner. Check cancellation before and after every request.
fn remote<T: Send>(
    store: &mut Store,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
    action: impl FnOnce() -> Result<T> + Send,
) -> Result<T> {
    poll();
    checkpoint(cancel)?;
    network(store, || {
        std::thread::scope(|scope| {
            let (send, receive) = mpsc::sync_channel(1);
            scope.spawn(move || {
                let _ = send.send(checkpoint(cancel).and_then(|()| action()));
            });
            loop {
                poll();
                match receive.recv_timeout(Duration::from_millis(25)) {
                    Ok(result) => {
                        checkpoint(cancel)?;
                        return result;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(Error("Plex metadata worker stopped".into()));
                    }
                }
            }
        })
    })
}
fn library(
    store: &mut Store,
    adapter: &Adapter,
    id: &str,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
) -> Result<Value> {
    remote(store, cancel, poll, || {
        adapter.validate(None).map_err(|e| e.error())
    })?;
    let libraries = remote(store, cancel, poll, || {
        adapter.libraries().map_err(|e| e.error())
    })?;
    let found = libraries
        .into_iter()
        .filter(|r| r["id"] == id)
        .collect::<Vec<_>>();
    if found.len() != 1 {
        return Err(policy::error("metadata_unknown_library"));
    }
    Ok(
        json!({"id":found[0]["id"],"uuid":found[0]["uuid"],"type":found[0]["type"],"roots":found[0]["roots"],"created_at":found[0]["created_at"]}),
    )
}
fn validate(
    store: &mut Store,
    adapter: &Adapter,
    expected: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
) -> Result<()> {
    remote(store, cancel, poll, || {
        adapter.validate(None).map_err(|e| e.error())
    })?;
    let found = remote(store, cancel, poll, || {
        adapter.libraries().map_err(|e| e.error())
    })?
    .into_iter()
    .filter(|r| r["id"] == expected["id"])
    .collect::<Vec<_>>();
    if found.len() != 1 || !consumers::same_library(&found[0], expected) {
        return Err(policy::error("library_identity_changed"));
    }
    Ok(())
}
fn named(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.is_element()
        && node.tag_name().namespace().is_none_or(str::is_empty)
        && node.tag_name().name() == name
}
fn attr<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.namespace().is_none() && a.name() == name)
        .map(|a| a.value())
}
fn text(value: Option<&str>) -> Result<&str> {
    value
        .filter(|s| !s.is_empty() && s.chars().count() <= 4096 && !s.chars().any(|c| c < ' '))
        .ok_or_else(|| policy::error("invalid_configuration"))
}
fn snapshot(adapter: &Adapter, library: &Value, item: &Value, fields: &[String]) -> Result<Value> {
    if !policy::valid_key(&item["rating_key"]) {
        return Err(policy::error("metadata_invalid_rating_key"));
    }
    let raw = adapter
        .call(
            &format!(
                "/library/metadata/{}?includeGuids=1",
                quote(item["rating_key"].as_str().unwrap())
            ),
            false,
        )
        .map_err(|e| e.error())?;
    parse_snapshot(adapter, &raw, library, item, fields)
}
fn parse_snapshot(
    adapter: &Adapter,
    raw: &[u8],
    library: &Value,
    item: &Value,
    fields: &[String],
) -> Result<Value> {
    let xml = adapter.xml(raw).map_err(|e| e.error())?;
    let document = xml.parse().map_err(|e| e.error())?;
    let root = document.root_element();
    let children = root
        .children()
        .filter(|n| n.is_element())
        .collect::<Vec<_>>();
    if !named(root, "MediaContainer") || children.len() != 1 {
        return Err(policy::error("metadata_invalid_item_response"));
    }
    let node = children[0];
    if attr(node, "ratingKey") != item["rating_key"].as_str()
        || attr(node, "librarySectionID").or_else(|| attr(root, "librarySectionID"))
            != library["id"].as_str()
        || attr(node, "type") != item["kind"].as_str()
    {
        return Err(policy::error("metadata_remote_item_mismatch"));
    }
    let guid = text(attr(node, "guid"))?;
    let mut paths = BTreeSet::new();
    for media in node.children().filter(|n| named(*n, "Media")) {
        for part in media.children().filter(|n| named(*n, "Part")) {
            paths.insert(
                consumers::path(text(attr(part, "file"))?, false)
                    .map_err(|e| policy::error(&e.0))?,
            );
        }
    }
    if paths.is_empty() || paths.len() > 100 {
        return Err(policy::error("metadata_invalid_item_paths"));
    }
    let paths = paths.into_iter().collect::<Vec<_>>();
    for proof in item["evidence"].as_array().unwrap() {
        let e = catabolic_core::decode_json(proof["evidence"].as_str().unwrap())?;
        let mut remote_paths = e.get("remote_paths").and_then(Value::as_array).cloned();
        if let Some(ref mut p) = remote_paths {
            p.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        }
        if e["guid"] != guid
            || !paths.iter().any(|p| *p == e["remote_path"])
            || e.get("remote_paths").is_some()
                && remote_paths != Some(paths.iter().map(|p| json!(p)).collect())
        {
            return Err(policy::error("metadata_import_identity_changed"));
        }
    }
    let mut locks = BTreeMap::new();
    for f in node.children().filter(|n| named(*n, "Field")) {
        let name = attr(f, "name");
        if locks.contains_key(&name) || !matches!(attr(f, "locked"), Some("0" | "1")) {
            return Err(policy::error("metadata_invalid_field_locks"));
        }
        locks.insert(name, attr(f, "locked") == Some("1"));
    }
    let mut values = serde_json::Map::new();
    for field in fields {
        let wire = policy::wire(field)?;
        let raw = attr(node, wire).unwrap_or("");
        if raw.chars().count() > 65536 {
            return Err(policy::error("metadata_remote_field_too_large"));
        }
        let value = if field == "year" {
            if raw.is_empty() {
                Value::Null
            } else {
                if raw.len() > 4 || !raw.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(policy::error("metadata_invalid_remote_year"));
                }
                json!(raw.parse::<u16>().unwrap())
            }
        } else {
            json!(raw)
        };
        values.insert(
            field.clone(),
            json!({"value":value,"locked":locks.get(&Some(wire)).copied().unwrap_or(false)}),
        );
    }
    Ok(json!({"guid":guid,"paths":paths,"fields":values}))
}
/// Frozen application/x-www-form-urlencoded scalar parameters. Credentials are
/// carried by the shared Adapter only, never by this route or review evidence.
fn parameters(row: &Value) -> Result<String> {
    let mut params = vec![
        ("id".into(), row["rating_key"].as_str().unwrap().to_owned()),
        (
            "type".into(),
            policy::kind(policy::field(row, "kind")?)?.1.to_string(),
        ),
    ];
    for (field, change) in row["changes"].as_object().unwrap() {
        let wire = policy::wire(field)?;
        let value = &change["after"]["value"];
        let raw = if value.is_null() {
            String::new()
        } else if let Some(s) = value.as_str() {
            s.to_owned()
        } else {
            value.to_string()
        };
        params.push((format!("{wire}.value"), raw));
        params.push((
            format!("{wire}.locked"),
            if change["after"]["locked"] == true {
                "1"
            } else {
                "0"
            }
            .into(),
        ));
    }
    let encoded = params
        .into_iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                quote(&k).replace("%20", "+"),
                quote(&v).replace("%20", "+")
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    if encoded.len() > 16384 {
        return Err(policy::error(
            "metadata_request_too_large_select_fewer_fields",
        ));
    }
    Ok(encoded)
}
fn error_report(error: &Error) -> Option<Value> {
    let code = error.0.strip_prefix("consumer ")?.split(';').next()?;
    Some(
        json!({"code":code,"safe_to_retry":false,"recovery":"preview_again","message":format!("Plex metadata {code}; inspect the item and preview again")}),
    )
}
pub fn execute(store: &mut Store, profile: &str, request: &Value) -> Result<Value> {
    execute_with_poll(store, profile, request, &AtomicBool::new(false), &mut || {})
}
pub fn execute_with_poll(
    store: &mut Store,
    profile: &str,
    request: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
) -> Result<Value> {
    // The lock name is interoperable with frozen metadata_publication.py and is
    // deliberately independent of Store's writer lock during detached HTTP.
    let _publication = if request["apply"] == true {
        let mut path = store.path.as_os_str().to_os_string();
        path.push(".plex-metadata");
        Some(
            catabolic_store::writer_lock(std::path::Path::new(&path))
                .map_err(|_| policy::error("metadata_writer_busy_preview_again"))?,
        )
    } else {
        None
    };
    let options = policy::options(request)?;
    poll();
    checkpoint(cancel)?;
    let connection = consumers::connection(store, profile, policy::field(request, "connection")?)?;
    if connection["application"] != "plex" {
        return Err(policy::error("metadata_requires_plex_connection"));
    }
    let adapter = Adapter::new(&connection).map_err(|e| e.error())?;
    let library = library(
        store,
        &adapter,
        policy::field(request, "library_id")?,
        cancel,
        poll,
    )?;
    let local = policy::snapshot(store, profile, &connection, &library, &options)?;
    let mut planned = Vec::new();
    for item in local["items"].as_array().unwrap() {
        let observed = remote(store, cancel, poll, || {
            snapshot(&adapter, &library, item, &options.fields)
        })?;
        let row = policy::planned(item, observed, &options);
        parameters(&row)?;
        planned.push(row);
    }
    validate(store, &adapter, &library, cancel, poll)?;
    if policy::snapshot(store, profile, &connection, &library, &options)? != local {
        return Err(policy::error("metadata_local_changed_preview_again"));
    }
    let mut result = policy::review(
        profile,
        request,
        &connection,
        &library,
        &local,
        &planned,
        &options,
    )?;
    if !options.apply {
        return Ok(result);
    }
    let mut results = Vec::new();
    for (item, row) in local["items"].as_array().unwrap().iter().zip(&planned) {
        poll();
        checkpoint(cancel)?;
        let mut target = serde_json::Map::new();
        for key in ["item_id", "rating_key", "match_id", "association_id"] {
            if let Some(v) = row.get(key) {
                target.insert(key.into(), v.clone());
            }
        }
        if row["changes"].as_object().unwrap().is_empty() {
            target.insert("state".into(), json!("unchanged"));
            results.push(Value::Object(target));
            continue;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let mut attempt = row.clone();
        for (key, value) in [
            ("plan_id", result["plan_id"].clone()),
            ("connection", request["connection"].clone()),
            ("server_id", connection["server_id"].clone()),
            ("library", library.clone()),
            ("state", json!("started")),
        ] {
            attempt[key] = value;
        }
        let mut started = false;
        let outcome = (|| {
            validate(store, &adapter, &library, cancel, poll)?;
            if policy::snapshot(store, profile, &connection, &library, &options)? != local {
                return Err(policy::error("metadata_local_changed_preview_again"));
            }
            if remote(store, cancel, poll, || {
                snapshot(&adapter, &library, item, &options.fields)
            })? != row["remote"]
            {
                return Err(policy::error("metadata_remote_changed_preview_again"));
            }
            // Recheck local evidence after the final detached detail read, too.
            if policy::snapshot(store, profile, &connection, &library, &options)? != local {
                return Err(policy::error("metadata_local_changed_preview_again"));
            }
            poll();
            checkpoint(cancel)?;
            policy::record_attempt(store, profile, &id, &attempt, true)?;
            started = true;
            let route = format!(
                "/library/sections/{}/all?{}",
                quote(policy::field(request, "library_id")?),
                parameters(row)?
            );
            remote(store, cancel, poll, || {
                adapter.put(&route).map_err(|e| e.error())
            })?;
            validate(store, &adapter, &library, cancel, poll)?;
            let observed = remote(store, cancel, poll, || {
                snapshot(&adapter, &library, item, &options.fields)
            })?;
            for field in &options.fields {
                let expected = row["changes"]
                    .get(field)
                    .map(|c| &c["after"])
                    .unwrap_or(&row["remote"]["fields"][field]);
                if observed["fields"][field] != *expected {
                    return Err(policy::error("metadata_verification_failed"));
                }
            }
            if policy::snapshot(store, profile, &connection, &library, &options)? != local {
                return Err(policy::error("metadata_local_changed_during_publish"));
            }
            // A signal observed by the last local fence must prevent verified ACK.
            poll();
            checkpoint(cancel)?;
            attempt["state"] = json!("verified");
            attempt["observed"] = observed;
            policy::record_attempt(store, profile, &id, &attempt, false)?;
            Ok(())
        })();
        match outcome {
            Ok(()) => {
                target.insert("event_id".into(), json!(id));
                target.insert("state".into(), json!("verified"));
                results.push(Value::Object(target));
            }
            Err(error) => {
                let Some(report) = error_report(&error) else {
                    return Err(error);
                };
                let state = if started { "uncertain" } else { "blocked" };
                attempt["state"] = json!(state);
                attempt["error"] = report.clone();
                if started {
                    policy::record_attempt(store, profile, &id, &attempt, false)?;
                }
                target.insert(
                    "event_id".into(),
                    if started { json!(id) } else { Value::Null },
                );
                target.insert("state".into(), json!(state));
                target.insert("error".into(), report);
                results.push(Value::Object(target));
                result["complete"] = json!(false);
                break;
            }
        }
    }
    result["remaining_item_ids"] = json!(
        planned[results.len()..]
            .iter()
            .map(|r| r["item_id"].clone())
            .collect::<Vec<_>>()
    );
    result["remaining_match_ids"] = json!(
        planned[results.len()..]
            .iter()
            .filter_map(|r| r.get("match_id").cloned())
            .collect::<Vec<_>>()
    );
    result["results"] = json!(results);
    Ok(result)
}

/// CLI-only synchronous signal bridge; embedded users retain their own handlers.
pub fn execute_cli(path: &std::path::Path, profile: &str, request: &Value) -> Result<Value> {
    crate::plex_login::with_cli_signals(INTERRUPTED, |cancel| {
        crate::api::execute_plex_metadata_with_poll(path, profile, request, cancel, &mut || {})
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_parameters_are_bounded_and_escape_field_like_text() {
        let row = json!({"rating_key":"42","kind":"movie","changes":{"title":{"after":{"value":"Été & title.locked=0 / ?","locked":true}},"year":{"after":{"value":null,"locked":false}}}});
        let params = parameters(&row).unwrap();
        assert!(params.contains("title.value=%C3%89t%C3%A9+%26+title.locked%3D0+%2F+%3F"));
        assert!(params.contains("year.value=&year.locked=0"));
        let mut huge = row;
        huge["changes"]["title"]["after"]["value"] = json!("冬".repeat(2000));
        assert!(
            parameters(&huge)
                .unwrap_err()
                .0
                .contains("request_too_large")
        );
    }
    #[test]
    fn selected_unchanged_fields_and_full_identity_are_returned() {
        let adapter = Adapter::new(&json!({"application":"plex","endpoint":"http://127.0.0.1:9"}))
            .unwrap_or_else(|_| panic!("fixture adapter"));
        let library = json!({"id":"7"});
        let item = json!({"rating_key":"1","kind":"movie","evidence":[{"evidence":catabolic_core::encode(&json!({"guid":"g","remote_path":"/a","remote_paths":["/b","/a"]}))}]});
        let raw=br#"<MediaContainer><Video ratingKey="1" librarySectionID="7" type="movie" guid="g" year="0000"><Media><Part file="/a"/><Part file="/b"/></Media><Field name="title" locked="1"/></Video></MediaContainer>"#;
        let observed = parse_snapshot(
            &adapter,
            raw,
            &library,
            &item,
            &["title".into(), "year".into()],
        )
        .unwrap();
        assert_eq!(
            observed,
            json!({"guid":"g","paths":["/a","/b"],"fields":{"title":{"value":"","locked":true},"year":{"value":0,"locked":false}}})
        );
    }
}
