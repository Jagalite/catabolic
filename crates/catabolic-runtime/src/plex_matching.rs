// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Complete bounded Plex library scan and exact published-path matching.
//! HTTP is read-only and detached; Store/curation own accepted provenance.
use crate::consumers::{Adapter, network, quote};
use catabolic_store::{Error, Result, Store, consumers, plex_matching as policy};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
pub const INTERRUPTED: &str = "Plex matching interrupted";
fn checkpoint(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error(INTERRUPTED.into()))
    } else {
        Ok(())
    }
}
fn named(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.is_element()
        && node.tag_name().namespace().is_none_or(str::is_empty)
        && node.tag_name().name() == name
}
fn attribute<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.namespace().is_none() && a.name() == name)
        .map(|a| a.value())
}
fn text(value: Option<&str>, maximum: usize) -> Result<&str> {
    value
        .filter(|s| !s.is_empty() && s.chars().count() <= maximum && !s.chars().any(|c| c < ' '))
        .ok_or_else(|| policy::error("invalid_configuration"))
}
fn validate(adapter: &Adapter, library: &Value, cancel: &AtomicBool) -> Result<()> {
    checkpoint(cancel)?;
    adapter.validate(None).map_err(|e| e.error())?;
    checkpoint(cancel)?;
    let found: Vec<_> = adapter
        .libraries()
        .map_err(|e| e.error())?
        .into_iter()
        .filter(|r| r["id"] == library["id"])
        .collect();
    checkpoint(cancel)?;
    if found.len() != 1 || !consumers::same_library(&found[0], library) {
        return Err(policy::error("library_identity_changed"));
    }
    Ok(())
}
/// Frozen detail identity validation, including full part identity and lock
/// syntax even though matching itself neither selects nor writes metadata.
fn detail(adapter: &Adapter, library: &Value, evidence: &Value) -> Result<()> {
    let key = evidence["rating_key"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| policy::error("metadata_invalid_rating_key"))?;
    let raw = adapter
        .call(
            &format!("/library/metadata/{}?includeGuids=1", quote(key)),
            false,
        )
        .map_err(|e| e.error())?;
    parse_detail(adapter, &raw, library, evidence)
}
fn parse_detail(adapter: &Adapter, raw: &[u8], library: &Value, evidence: &Value) -> Result<()> {
    let xml = adapter.xml(raw).map_err(|e| e.error())?;
    let document = xml.parse().map_err(|e| e.error())?;
    let root = document.root_element();
    let children: Vec<_> = root.children().filter(|n| n.is_element()).collect();
    if !named(root, "MediaContainer") || children.len() != 1 {
        return Err(policy::error("metadata_invalid_item_response"));
    }
    let node = children[0];
    if attribute(node, "ratingKey") != evidence["rating_key"].as_str()
        || attribute(node, "librarySectionID").or_else(|| attribute(root, "librarySectionID"))
            != library["id"].as_str()
        || attribute(node, "type") != evidence["kind"].as_str()
    {
        return Err(policy::error("metadata_remote_item_mismatch"));
    }
    let guid = text(attribute(node, "guid"), 4096)?;
    let mut paths = BTreeSet::new();
    for media in node.children().filter(|n| named(*n, "Media")) {
        for part in media.children().filter(|n| named(*n, "Part")) {
            paths.insert(consumers::path(
                text(attribute(part, "file"), 4096)?,
                false,
            )?);
        }
    }
    if paths.is_empty() || paths.len() > 100 {
        return Err(policy::error("metadata_invalid_item_paths"));
    }
    let paths: Vec<_> = paths.into_iter().collect();
    if evidence["guid"] != guid
        || !paths.iter().any(|p| evidence["remote_path"] == *p)
        || evidence["remote_paths"] != json!(paths)
    {
        return Err(policy::error("metadata_import_identity_changed"));
    }
    let mut locks = BTreeSet::new();
    for field in node.children().filter(|n| named(*n, "Field")) {
        if !locks.insert(attribute(field, "name"))
            || !matches!(attribute(field, "locked"), Some("0" | "1"))
        {
            return Err(policy::error("metadata_invalid_field_locks"));
        }
    }
    Ok(())
}
fn remote(
    connection: &Value,
    local: &Value,
    source: &str,
    maximum: usize,
    cancel: &AtomicBool,
) -> Result<Value> {
    checkpoint(cancel)?;
    let adapter = Adapter::new(connection).map_err(|e| e.error())?;
    let library = policy::library(local)?;
    validate(&adapter, &library, cancel)?;
    let mut offset = 0;
    let mut matched = None;
    let mut ambiguous = false;
    loop {
        checkpoint(cancel)?;
        let data = crate::plex_import::page(
            &adapter,
            &library,
            offset,
            100.min(maximum - offset as usize),
        )?;
        checkpoint(cancel)?;
        if data["total"].as_u64().is_none_or(|n| n > maximum as u64) {
            return Err(policy::error("metadata_match_library_exceeds_limit"));
        }
        for record in data["records"].as_array().unwrap() {
            if record["parts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["path"] == local["remote_path"])
            {
                if matched.is_some() {
                    ambiguous = true;
                } else {
                    matched = Some(record.clone());
                }
            }
        }
        match data["next_offset"].as_u64() {
            Some(next) if next > offset && next < maximum as u64 => offset = next,
            Some(_) => return Err(policy::error("metadata_match_library_exceeds_limit")),
            None => break,
        }
    }
    if ambiguous || matched.is_none() {
        return Err(policy::error("metadata_match_path_missing_or_ambiguous"));
    }
    let evidence = policy::evidence(connection, &library, local, &matched.unwrap(), source)?;
    checkpoint(cancel)?;
    detail(&adapter, &library, &evidence)?;
    checkpoint(cancel)?;
    validate(&adapter, &library, cancel)?;
    Ok(evidence)
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
    let (source, maximum) = policy::options(request)?;
    poll();
    checkpoint(cancel)?;
    let local = policy::published_snapshot(
        store,
        profile,
        policy::field(request, "binding_id")?,
        policy::field(request, "mapping_id")?,
    )?;
    let connection = consumers::connection(
        store,
        profile,
        policy::field(&local["binding"], "connection_id")?,
    )?;
    if connection["application"] != "plex" {
        return Err(policy::error("metadata_requires_plex_connection"));
    }
    poll();
    checkpoint(cancel)?;
    let evidence = network(store, || {
        std::thread::scope(|scope| {
            let (send, receive) = mpsc::sync_channel(1);
            let connection = &connection;
            let local = &local;
            scope.spawn(move || {
                let _ = send.send(remote(connection, local, source, maximum, cancel));
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
                        return Err(Error("Plex matching read worker stopped".into()));
                    }
                }
            }
        })
    })?;
    policy::finish(
        store,
        profile,
        request,
        &connection,
        &local,
        &evidence,
        &mut || {
            poll();
            checkpoint(cancel)
        },
    )
}
