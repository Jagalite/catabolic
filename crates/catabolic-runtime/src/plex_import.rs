// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded native Plex read orchestration. The managed consumer adapter owns
//! HTTP/XML/credentials; the Store curation owner owns all import decisions.
use crate::consumers::{Adapter, network, quote};
use catabolic_store::{Error, Result, Store, consumers, plex_import as policy};
use num_bigint::BigUint;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

pub const INTERRUPTED: &str = "Plex import interrupted";
fn checkpoint(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error(INTERRUPTED.into()))
    } else {
        Ok(())
    }
}
fn text(value: Option<&str>, maximum: usize) -> Result<&str> {
    value
        .filter(|s| !s.is_empty() && s.chars().count() <= maximum && !s.chars().any(|c| c < ' '))
        .ok_or_else(|| policy::error("invalid_configuration"))
}
fn path(value: Option<&str>) -> Result<String> {
    consumers::path(text(value, 4096)?, false).map_err(|e| policy::error(&e.0))
}
/// Python accepts ASCII decimal strings only; preserve arbitrary integers and
/// leading-zero normalization instead of truncating through a machine integer.
pub fn integer(value: Option<&str>, label: &str) -> Result<Value> {
    let bad = || policy::error(&format!("invalid_import_{label}"));
    let raw = value
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.len() <= 4300)
        .ok_or_else(bad)?;
    let raw = raw.trim_start_matches('0');
    catabolic_core::decode_json(if raw.is_empty() { "0" } else { raw }).map_err(|_| bad())
}
fn integer_big(value: Option<&str>, label: &str) -> Result<(Value, BigUint)> {
    let value = integer(value, label)?;
    let number = BigUint::parse_bytes(value.to_string().as_bytes(), 10).unwrap();
    Ok((value, number))
}
fn kind(library: &Value) -> Result<(u8, &'static str)> {
    match library["type"].as_str() {
        Some("movie") => Ok((1, "movie")),
        Some("show") => Ok((4, "episode")),
        Some("artist") => Ok((10, "track")),
        Some("photo") => Ok((13, "photo")),
        _ => Err(policy::error("unsupported_import_library_type")),
    }
}
/// Reusable exact page parser for later matching/metadata owners. The adapter
/// retains its XML node/byte/DTD limits and this owner adds intake page bounds.
pub(crate) fn page(adapter: &Adapter, library: &Value, offset: u64, limit: usize) -> Result<Value> {
    let (number, kind) = kind(library)?;
    let route = format!(
        "/library/sections/{}/all?type={number}&includeGuids=1&sort=id%3Aasc&X-Plex-Container-Start={offset}&X-Plex-Container-Size={limit}",
        quote(text(library["id"].as_str(), 128)?)
    );
    let raw = adapter.call(&route, false).map_err(|e| e.error())?;
    parse_page(adapter, &raw, kind, offset, limit)
}
fn named(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.is_element()
        && node.tag_name().namespace().is_none_or(str::is_empty)
        && node.tag_name().name() == name
}
// ElementTree's get("name") selects only an unqualified attribute. roxmltree's
// string attribute lookup also matches a qualified attribute's local name.
fn attribute<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.namespace().is_none() && attribute.name() == name)
        .map(|attribute| attribute.value())
}
fn parse_page(
    adapter: &Adapter,
    raw: &[u8],
    kind: &str,
    offset: u64,
    limit: usize,
) -> Result<Value> {
    let xml = adapter.xml(raw).map_err(|e| e.error())?;
    let document = xml.parse().map_err(|e| e.error())?;
    let root = document.root_element();
    if !named(root, "MediaContainer") {
        return Err(policy::error("invalid_response"));
    }
    let items: Vec<_> = root.children().filter(|n| n.is_element()).collect();
    let (total, total_number) = integer_big(attribute(root, "totalSize"), "total_size")?;
    let returned_offset = integer(attribute(root, "offset").or(Some("0")), "offset")?;
    let size = integer(attribute(root, "size"), "size")?;
    let end = offset + items.len() as u64;
    if returned_offset.as_u64() != Some(offset)
        || size.as_u64() != Some(items.len() as u64)
        || items.len() > limit
        || BigUint::from(end) > total_number
        || (items.is_empty() && BigUint::from(offset) < total_number)
    {
        return Err(policy::error("inconsistent_import_pagination"));
    }
    let mut seen = BTreeSet::new();
    let mut records = Vec::new();
    let mut part_count = 0;
    for item in items {
        let key = text(attribute(item, "ratingKey"), 128)?;
        if !seen.insert(key) {
            return Err(policy::error("duplicate_import_rating_key"));
        }
        let mut metadata = Map::new();
        for (remote, local) in [
            ("title", "title"),
            ("year", "year"),
            ("summary", "summary"),
            ("originalTitle", "original_title"),
            ("editionTitle", "edition"),
            ("originallyAvailableAt", "release_date"),
            ("duration", "duration_ms"),
        ] {
            if let Some(value) = attribute(item, remote).filter(|s| !s.is_empty()) {
                metadata.insert(
                    local.into(),
                    if matches!(remote, "year" | "duration") {
                        integer(Some(value), remote)?
                    } else {
                        json!(text(Some(value), 65536)?)
                    },
                );
            }
        }
        let extra: &[(&str, &str)] = match kind {
            "episode" => &[
                ("grandparentTitle", "series"),
                ("parentIndex", "season"),
                ("index", "episode"),
            ],
            "track" => &[
                ("grandparentTitle", "artist"),
                ("parentTitle", "album"),
                ("parentIndex", "disc"),
                ("index", "track"),
            ],
            "photo" => &[("parentTitle", "album")],
            _ => &[],
        };
        for &(remote, local) in extra {
            if let Some(value) = attribute(item, remote) {
                metadata.insert(
                    local.into(),
                    if remote.ends_with("Index") || remote == "index" {
                        integer(Some(value), remote)?
                    } else {
                        json!(text(Some(value), 4096)?)
                    },
                );
            }
        }
        let mut parts = Vec::new();
        for media in item.children().filter(|n| named(*n, "Media")) {
            let children: Vec<_> = media.children().filter(|n| named(*n, "Part")).collect();
            for (index, part) in children.iter().enumerate() {
                parts.push(json!({"path":path(attribute(*part, "file"))?,"part":if children.len()>1 {Some(index+1)} else {None}}));
            }
        }
        if parts.len() > 100 {
            return Err(policy::error("import_part_limit"));
        }
        part_count += parts.len();
        let guids = item
            .children()
            .filter(|n| named(*n, "Guid"))
            .map(|n| text(attribute(n, "id"), 4096))
            .collect::<Result<BTreeSet<_>>>()?;
        let record = json!({"rating_key":key,"kind":kind,"metadata":metadata,"parts":parts,"guids":guids,"guid":attribute(item, "guid"),"reported_type":attribute(item, "type")});
        if catabolic_core::encode(&record).len() > 256 * 1024 {
            return Err(Error(
                "payload must be a JSON object of at most 256 KiB".into(),
            ));
        }
        records.push(record);
    }
    if part_count > 1000 {
        return Err(policy::error("import_page_part_limit"));
    }
    Ok(
        json!({"records":records,"total":total,"next_offset":if BigUint::from(end)<total_number { Some(end) } else { None }}),
    )
}

fn remote(
    connection: &Value,
    library_id: &str,
    offset: u64,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<(Value, Value)> {
    checkpoint(cancel)?;
    let adapter = Adapter::new(connection).map_err(|e| e.error())?;
    adapter.validate(None).map_err(|e| e.error())?;
    checkpoint(cancel)?;
    let libraries: Vec<_> = adapter
        .libraries()
        .map_err(|e| e.error())?
        .into_iter()
        .filter(|l| l["id"] == library_id)
        .collect();
    if libraries.len() != 1 {
        return Err(policy::error("unknown_import_library"));
    }
    let library: Value = Value::Object(
        ["id", "uuid", "type", "roots", "created_at"]
            .into_iter()
            .map(|k| (k.to_owned(), libraries[0][k].clone()))
            .collect(),
    );
    checkpoint(cancel)?;
    let data = page(&adapter, &library, offset, limit)?;
    checkpoint(cancel)?;
    // Keep a cancellation boundary between identity and library reads. The
    // shared validate(Some(...)) combines both requests, which would otherwise
    // issue another request after cancellation during the identity read.
    adapter.validate(None).map_err(|e| e.error())?;
    checkpoint(cancel)?;
    let found: Vec<_> = adapter
        .libraries()
        .map_err(|e| e.error())?
        .into_iter()
        .filter(|row| row["id"] == library["id"])
        .collect();
    checkpoint(cancel)?;
    if found.len() != 1 || !consumers::same_library(&found[0], &library) {
        return Err(policy::error("library_identity_changed"));
    }
    Ok((library, data))
}

pub fn execute(store: &mut Store, profile: &str, request: &Value) -> Result<Value> {
    execute_with_poll(store, profile, request, &AtomicBool::new(false), &mut || {})
}
/// Signal polling continues while the bounded read runs. We always join it,
/// retaining ownership until the current managed HTTP deadline completes; no
/// detached background request can survive this operation or publish locally.
pub fn execute_with_poll(
    store: &mut Store,
    profile: &str,
    request: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
) -> Result<Value> {
    let (offset, limit) = policy::page_bounds(request)?;
    if request["apply"] == true && request["expected_plan"].as_str().is_none_or(str::is_empty) {
        return Err(policy::error("import_apply_requires_expected_plan"));
    }
    let mappings = policy::mappings(&request["mappings"])?;
    let connection =
        consumers::connection(store, profile, text(request["connection"].as_str(), 255)?)?;
    if connection["application"] != "plex" {
        return Err(policy::error("plex_import_requires_plex_connection"));
    }
    let library_id = text(request["library_id"].as_str(), 128)?;
    poll();
    checkpoint(cancel)?;
    let (library, data) = network(store, || {
        std::thread::scope(|scope| {
            let (send, receive) = mpsc::sync_channel(1);
            let connection = &connection;
            scope.spawn(move || {
                let _ = send.send(remote(connection, library_id, offset, limit, cancel));
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
                        return Err(Error("Plex import read worker stopped".into()));
                    }
                }
            }
        })
    })?;
    policy::run(
        store,
        profile,
        request,
        policy::RemotePage {
            connection: &connection,
            library: &library,
            mappings: &mappings,
            data: &data,
        },
        &mut || {
            poll();
            checkpoint(cancel)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qualified_attributes_do_not_supply_or_shadow_unqualified_values() {
        let adapter = Adapter::new(&json!({"application":"plex","endpoint":"http://127.0.0.1:9","credential_env":"UNUSED"})).ok().unwrap();
        let raw = br#"<MediaContainer xmlns:x="urn:other" x:totalSize="900" totalSize="1" x:size="99" size="1" x:offset="9"><Video x:ratingKey="wrong" ratingKey="1" x:type="clip" type="movie" x:title="Wrong" title="Right" x:year="9999" year="2024" x:guid="bad" guid="good"><Media><Part x:file="/wrong" file="/right"/></Media><Guid x:id="wrong" id="right"/></Video></MediaContainer>"#;
        let page = parse_page(&adapter, raw, "movie", 0, 100).unwrap();
        assert_eq!(page["total"], 1);
        assert_eq!(page["records"][0]["rating_key"], "1");
        assert_eq!(page["records"][0]["reported_type"], "movie");
        assert_eq!(
            page["records"][0]["metadata"],
            json!({"title":"Right","year":2024})
        );
        assert_eq!(page["records"][0]["parts"][0]["path"], "/right");
        assert_eq!(page["records"][0]["guids"], json!(["right"]));
        assert_eq!(page["records"][0]["guid"], "good");
        let raw = br#"<MediaContainer xmlns:x="urn:other" totalSize="1" size="1"><Video ratingKey="1" x:type="movie" x:title="Wrong" x:grandparentTitle="Series" x:parentIndex="3" x:index="4" x:guid="wrong"><Media><Part file="/right"/></Media></Video></MediaContainer>"#;
        let page = parse_page(&adapter, raw, "episode", 0, 100).unwrap();
        assert_eq!(page["records"][0]["metadata"], json!({}));
        assert_eq!(page["records"][0]["reported_type"], Value::Null);
        assert_eq!(page["records"][0]["guid"], Value::Null);
    }
    #[test]
    fn decimal_and_page_validation_preserve_the_wire_contract() {
        assert_eq!(integer(Some("000"), "year").unwrap(), json!(0));
        assert_eq!(
            integer(Some("18446744073709551616000"), "year")
                .unwrap()
                .to_string(),
            "18446744073709551616000"
        );
        for raw in ["", "-1", "+1", "1.0", " 1", "١"] {
            assert!(
                integer(Some(raw), "year")
                    .unwrap_err()
                    .0
                    .contains("invalid_import_year")
            );
        }
        let adapter = Adapter::new(&json!({"application":"plex","endpoint":"http://127.0.0.1:9","credential_env":"UNUSED"})).ok().unwrap();
        let page = parse_page(&adapter,b"<MediaContainer totalSize='1' size='1'><Video ratingKey='1' type='movie' title='One'><Media><Part file='/m/one'/><Part file='/m/two'/></Media><Guid id='a'/><Guid id='a'/></Video></MediaContainer>","movie",0,100).unwrap();
        assert_eq!(page["records"][0]["parts"][1]["part"], 2);
        assert_eq!(page["records"][0]["guids"], json!(["a"]));
        for xml in [
            "<X totalSize='0' size='0'/>",
            "<MediaContainer xmlns='urn:other' totalSize='0' size='0'/>",
            "<MediaContainer totalSize='1' size='0'/>",
            "<MediaContainer totalSize='0' size='0' offset='1'/>",
        ] {
            assert!(parse_page(&adapter, xml.as_bytes(), "movie", 0, 100).is_err());
        }
    }
}
