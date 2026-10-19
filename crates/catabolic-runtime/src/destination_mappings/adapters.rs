// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Production v1 metadata and explicitly adopted collection wire adapters.
//! The owner detaches the catalog and joins each call, including on cancellation.
use crate::consumers::{Adapter, json_wire, quote};
use catabolic_store::{Error, Result, consumers, destination_mappings::equal, plex_metadata};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};

const JELLYFIN_FIELDS: &[(&str, &str)] = &[
    ("title", "Name"),
    ("sort_title", "ForcedSortName"),
    ("original_title", "OriginalTitle"),
    ("summary", "Overview"),
    ("year", "ProductionYear"),
    ("content_rating", "OfficialRating"),
    ("genres", "Genres"),
    ("tags", "Tags"),
];
const EDITABLE: &str = "Name ForcedSortName OriginalTitle CriticRating CommunityRating IndexNumber ParentIndexNumber Overview Genres AirsAfterSeasonNumber AirsBeforeEpisodeNumber AirsBeforeSeasonNumber Height Taglines Studios DateCreated EndDate PremiereDate ProductionYear OfficialRating CustomRating Tags ProductionLocations PreferredMetadataCountryCode PreferredMetadataLanguage DisplayOrder AspectRatio LockData LockedFields RunTimeTicks ProviderIds Video3DFormat AlbumArtists ArtistItems Album AirDays AirTime Status";

fn fail(code: &str) -> Error {
    Error(format!("mapping_{code}"))
}
fn network_error(error: consumers::Failure) -> Error {
    Error(error.code)
}
fn checkpoint(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error("destination mapping interrupted".into()))
    } else {
        Ok(())
    }
}
fn text(raw: Option<&str>, maximum: usize) -> Result<&str> {
    raw.filter(|s| !s.is_empty() && s.chars().count() <= maximum && !s.chars().any(|c| c < ' '))
        .ok_or_else(|| Error("invalid_configuration".into()))
}
fn path(raw: &Value) -> Result<String> {
    consumers::path(consumers::text(raw, 4096)?, false)
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
fn fields(value: &Value) -> Result<Vec<&str>> {
    if let Some(object) = value.as_object() {
        return Ok(object.keys().map(String::as_str).collect());
    }
    value
        .as_array()
        .ok_or_else(|| fail("invalid_fields"))?
        .iter()
        .map(|v| v.as_str().ok_or_else(|| fail("invalid_fields")))
        .collect()
}
fn form(params: Vec<(String, String)>) -> String {
    params
        .into_iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                quote(&k).replace("%20", "+"),
                quote(&v).replace("%20", "+")
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}
fn string_set(value: &Value) -> Result<BTreeSet<String>> {
    value
        .as_array()
        .ok_or_else(|| fail("invalid_remote_set"))?
        .iter()
        .map(|v| text(v.as_str(), 255).map(str::to_owned))
        .collect()
}
fn plex_id(id: &str) -> bool {
    (1..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}
fn jellyfin_id(id: &str) -> bool {
    (32..=36).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}
fn allowed(kind: &str, field: &str) -> bool {
    match kind {
        "movie" => plex_wire(field).is_ok(),
        "episode" => matches!(
            field,
            "title"
                | "sort_title"
                | "summary"
                | "release_date"
                | "content_rating"
                | "genres"
                | "labels"
        ),
        "track" => field == "title",
        "photo" => matches!(field, "title" | "sort_title" | "summary"),
        _ => false,
    }
}
fn plex_wire(field: &str) -> Result<&'static str> {
    match field {
        "genres" => Ok("genre"),
        "labels" => Ok("label"),
        _ => plex_metadata::FIELDS
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, w)| *w)
            .ok_or_else(|| fail("unsupported_fields_for_kind")),
    }
}
fn jellyfin_wire(field: &str) -> Result<&'static str> {
    JELLYFIN_FIELDS
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, w)| *w)
        .ok_or_else(|| fail("invalid_fields"))
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
fn integer(value: &Value) -> bool {
    matches!(value, Value::Number(n) if !n.to_string().contains(['.', 'e', 'E']))
}

pub(super) struct MappingAdapter {
    transport: Adapter,
    connection: Value,
    plex: bool,
    collections: Option<Vec<String>>,
}
impl MappingAdapter {
    pub(super) fn new(connection: &Value, collections: &Value) -> Result<Self> {
        let plex = match connection["application"].as_str() {
            Some("plex") => true,
            Some("jellyfin") => false,
            _ => return Err(fail("unsupported_destination")),
        };
        let adopted = if collections.is_null() {
            None
        } else {
            let ids = collections
                .as_array()
                .filter(|a| (1..=20).contains(&a.len()))
                .ok_or_else(|| fail("invalid_collection_scope"))?;
            let ids = ids
                .iter()
                .map(|id| {
                    id.as_str()
                        .filter(|id| {
                            if plex {
                                plex_id(id)
                            } else {
                                id.len() == 32
                                    && id
                                        .bytes()
                                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                            }
                        })
                        .map(str::to_owned)
                        .ok_or_else(|| fail("invalid_collection_id"))
                })
                .collect::<Result<BTreeSet<_>>>()?;
            if ids.len() != collections.as_array().unwrap().len() {
                return Err(fail("invalid_collection_id"));
            }
            Some(ids.into_iter().collect())
        };
        Ok(Self {
            transport: Adapter::new(connection).map_err(network_error)?,
            connection: connection.clone(),
            plex,
            collections: adopted,
        })
    }
    /// true means a set-valued field; false means scalar.
    pub(super) fn capabilities(&self) -> BTreeMap<String, bool> {
        let mut result: BTreeMap<String, bool> = if self.plex {
            plex_metadata::FIELDS
                .iter()
                .map(|(f, _)| ((*f).into(), false))
                .chain([("genres".into(), true), ("labels".into(), true)])
                .collect()
        } else {
            JELLYFIN_FIELDS
                .iter()
                .map(|(f, _)| ((*f).into(), matches!(*f, "genres" | "tags")))
                .collect()
        };
        if self.collections.is_some() {
            result.insert("collection_ids".into(), true);
        }
        result
    }
    pub(super) fn validate_value(&self, field: &str, value: &Value) -> Result<Value> {
        plex_metadata::value(field, value).map_err(|error| {
            let code = error
                .0
                .strip_prefix("consumer ")
                .and_then(|v| v.split(';').next())
                .unwrap_or(&error.0);
            Error(code.to_owned())
        })
    }
    pub(super) fn validate_remote(&self, library: Option<&Value>) -> Result<()> {
        self.transport
            .validate(library)
            .map(|_| ())
            .map_err(network_error)
    }
    pub(super) fn libraries(&self) -> Result<Vec<Value>> {
        self.transport.libraries().map_err(network_error)
    }
    fn raw(&self, route: &str, cancel: &AtomicBool) -> Result<Vec<u8>> {
        checkpoint(cancel)?;
        let raw = self.transport.call(route, false).map_err(network_error)?;
        checkpoint(cancel)?;
        Ok(raw)
    }
    fn data(&self, route: &str, cancel: &AtomicBool) -> Result<Value> {
        let raw = self.raw(route, cancel)?;
        json_wire::decode(&raw).map_err(|_| Error("invalid_response".into()))
    }
    pub(super) fn read(
        &self,
        library: &Value,
        target: &Value,
        selected: &Value,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        checkpoint(cancel)?;
        let selected = fields(selected)?;
        if self.collections.is_some() {
            if selected != ["collection_ids"] {
                return Err(fail("collection_mapping_requires_separate_definition"));
            }
            self.read_collections(library, target, cancel)
        } else {
            self.read_base(library, target, &selected, cancel)
        }
    }
    fn read_base(
        &self,
        library: &Value,
        target: &Value,
        selected: &[&str],
        cancel: &AtomicBool,
    ) -> Result<Value> {
        if self.plex {
            self.read_plex(library, target, selected, cancel)
        } else {
            self.read_jellyfin(library, target, selected, cancel)
        }
    }
    fn read_plex(
        &self,
        library: &Value,
        target: &Value,
        selected: &[&str],
        cancel: &AtomicBool,
    ) -> Result<Value> {
        let key = target["id"]
            .as_str()
            .filter(|s| plex_id(s))
            .ok_or_else(|| fail("invalid_plex_id"))?;
        let raw = self.raw(&format!("/library/metadata/{key}?includeGuids=1"), cancel)?;
        self.parse_plex(&raw, library, target, selected)
    }
    fn parse_plex(
        &self,
        raw: &[u8],
        library: &Value,
        target: &Value,
        selected: &[&str],
    ) -> Result<Value> {
        let xml = self.transport.xml(raw).map_err(network_error)?;
        let document = xml.parse().map_err(network_error)?;
        let root = document.root_element();
        let children = root
            .children()
            .filter(|n| n.is_element())
            .collect::<Vec<_>>();
        if !named(root, "MediaContainer") || children.len() != 1 {
            return Err(fail("invalid_item_response"));
        }
        let node = children[0];
        let kind = attr(node, "type").unwrap_or("");
        if attr(node, "ratingKey") != target["id"].as_str()
            || attr(node, "librarySectionID").or_else(|| attr(root, "librarySectionID"))
                != library["id"].as_str()
            || plex_metadata::kind(kind).ok().map(|k| k.0) != library["type"].as_str()
            || plex_metadata::kind(kind).is_err()
        {
            return Err(fail("remote_identity_mismatch"));
        }
        if selected.iter().any(|f| !allowed(kind, f)) {
            return Err(fail("unsupported_fields_for_kind"));
        }
        let mut paths = BTreeSet::new();
        for media in node.children().filter(|n| named(*n, "Media")) {
            for part in media.children().filter(|n| named(*n, "Part")) {
                paths.insert(consumers::path(text(attr(part, "file"), 4096)?, false)?);
            }
        }
        if !paths.contains(&path(&target["path"])?) || paths.len() > 100 {
            return Err(fail("remote_path_mismatch"));
        }
        let mut locks = BTreeMap::new();
        for field in node.children().filter(|n| named(*n, "Field")) {
            if !matches!(attr(field, "locked"), Some("0" | "1"))
                || locks.contains_key(&attr(field, "name"))
            {
                return Err(fail("invalid_locks"));
            }
            locks.insert(attr(field, "name"), attr(field, "locked") == Some("1"));
        }
        let mut values = Map::new();
        let mut selected_locks = Map::new();
        for field in selected {
            let wire = plex_wire(field)?;
            selected_locks.insert(
                (*field).into(),
                json!(locks.get(&Some(wire)).copied().unwrap_or(false)),
            );
            let value = if matches!(*field, "genres" | "labels") {
                let tag = if *field == "genres" { "Genre" } else { "Label" };
                let tags = node
                    .children()
                    .filter(|n| named(*n, tag))
                    .map(|n| text(attr(n, "tag"), 255).map(str::to_owned))
                    .collect::<Result<BTreeSet<_>>>()?;
                json!(tags)
            } else if *field == "year" {
                let raw = attr(node, wire).unwrap_or("");
                if raw.is_empty() {
                    Value::Null
                } else {
                    if raw.len() > 4 || !raw.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(fail("invalid_remote_year"));
                    }
                    json!(raw.parse::<u16>().unwrap())
                }
            } else {
                json!(attr(node, wire).unwrap_or(""))
            };
            values.insert((*field).into(), value);
        }
        Ok(
            json!({"identity":{"guid":text(attr(node, "guid"), 4096)?,"paths":paths,"kind":kind},"values":values,"locks":selected_locks}),
        )
    }
    fn read_jellyfin(
        &self,
        library: &Value,
        target: &Value,
        selected: &[&str],
        cancel: &AtomicBool,
    ) -> Result<Value> {
        let key = target["id"]
            .as_str()
            .filter(|s| jellyfin_id(s))
            .ok_or_else(|| fail("invalid_jellyfin_id"))?;
        let query = form(vec![("Ids".into(), key.into()), ("Limit".into(), "2".into()), ("Recursive".into(), "true".into()), ("EnableUserData".into(), "false".into()), ("Fields".into(), "Path,Genres,Tags,ProviderIds,Settings,Overview,OriginalTitle,SortName,CustomRating,DateCreated,ProductionLocations,Studios,Taglines,SpecialEpisodeNumbers,MediaSources".into())]);
        let response = self.data(&format!("/Items?{query}"), cancel)?;
        if !response.is_object()
            || !equal(&response["TotalRecordCount"], &json!(1))
            || response["Items"].as_array().is_none_or(|v| v.len() != 1)
        {
            return Err(fail("invalid_item_response"));
        }
        let dto = &response["Items"][0];
        if !dto.is_object()
            || dto["Id"] != key
            || !matches!(dto["Type"].as_str(), Some("Movie" | "Episode"))
        {
            return Err(fail("remote_identity_mismatch"));
        }
        if path(&dto["Path"])? != path(&target["path"])? {
            return Err(fail("remote_path_mismatch"));
        }
        let ancestors = self.data(&format!("/Items/{key}/Ancestors"), cancel)?;
        if ancestors
            .as_array()
            .is_none_or(|a| !a.iter().any(|a| a.is_object() && a["Id"] == library["id"]))
        {
            return Err(fail("wrong_library"));
        }
        self.snapshot_jellyfin(dto, selected)
    }
    fn snapshot_jellyfin(&self, dto: &Value, selected: &[&str]) -> Result<Value> {
        if [
            "Name",
            "Genres",
            "Tags",
            "ProviderIds",
            "LockData",
            "LockedFields",
        ]
        .iter()
        .any(|f| dto.get(*f).is_none())
        {
            return Err(fail("incomplete_metadata_dto"));
        }
        if !dto["ProviderIds"].is_object()
            || !dto["LockData"].is_boolean()
            || !dto["LockedFields"].is_array()
        {
            return Err(fail("invalid_metadata_dto"));
        }
        let payload = EDITABLE
            .split_whitespace()
            .filter_map(|f| dto.get(f).map(|v| (f.to_owned(), v.clone())))
            .collect::<Map<_, _>>();
        let mut values = Map::new();
        for field in selected {
            let wire = jellyfin_wire(field)?;
            let v = &dto[wire];
            let v = if matches!(*field, "genres" | "tags") {
                if v.as_array().is_none_or(|v| v.len() > 1000) {
                    return Err(fail("invalid_remote_set"));
                }
                json!(string_set(v)?)
            } else if *field != "year" {
                if !truthy(v) {
                    json!("")
                } else if v.is_string() {
                    v.clone()
                } else {
                    return Err(fail("invalid_remote_scalar"));
                }
            } else {
                if !v.is_null() && !integer(v) {
                    return Err(fail("invalid_remote_year"));
                }
                v.clone()
            };
            values.insert((*field).into(), v);
        }
        Ok(
            json!({"identity":{"path":dto["Path"],"kind":dto["Type"],"providers":dto["ProviderIds"]},"values":values,"payload":payload}),
        )
    }
    fn read_collections(
        &self,
        library: &Value,
        target: &Value,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        let mut snapshot = self.read_base(library, target, &[], cancel)?;
        let mut identities = Map::new();
        let mut memberships = Vec::new();
        for key in self.collections.as_ref().unwrap() {
            let (identity, members) = if self.plex {
                self.plex_collection(key, library, &snapshot["identity"]["kind"], cancel)?
            } else {
                self.jellyfin_collection(key, cancel)?
            };
            identities.insert(key.clone(), identity);
            if members.iter().any(|m| m == &target["id"]) {
                memberships.push(key.clone());
            }
        }
        snapshot["identity"]["collections"] = json!(identities);
        snapshot["values"] = json!({"collection_ids":memberships});
        Ok(snapshot)
    }
    fn plex_collection(
        &self,
        key: &str,
        library: &Value,
        kind: &Value,
        cancel: &AtomicBool,
    ) -> Result<(Value, Vec<Value>)> {
        let raw = self.raw(&format!("/library/metadata/{key}"), cancel)?;
        let xml = self.transport.xml(&raw).map_err(network_error)?;
        let document = xml.parse().map_err(network_error)?;
        let nodes = document
            .root_element()
            .children()
            .filter(|n| n.is_element())
            .collect::<Vec<_>>();
        if nodes.len() != 1 {
            return Err(fail("invalid_collection"));
        }
        let node = nodes[0];
        if attr(node, "ratingKey") != Some(key)
            || attr(node, "type") != Some("collection")
            || attr(node, "smart").unwrap_or("0") != "0"
            || attr(node, "librarySectionID") != library["id"].as_str()
            || attr(node, "subtype") != kind.as_str()
        {
            return Err(fail("unsupported_collection"));
        }
        let identity = json!(text(attr(node, "guid"), 4096)?);
        let raw = self.raw(&format!("/library/metadata/{key}/children?X-Plex-Container-Start=0&X-Plex-Container-Size=1000"), cancel)?;
        let xml = self.transport.xml(&raw).map_err(network_error)?;
        let document = xml.parse().map_err(network_error)?;
        let root = document.root_element();
        let nodes = root
            .children()
            .filter(|n| n.is_element())
            .collect::<Vec<_>>();
        if attr(root, "offset").unwrap_or("0") != "0"
            || attr(root, "totalSize").or_else(|| attr(root, "size"))
                != Some(nodes.len().to_string().as_str())
            || nodes.len() >= 1000
            || nodes
                .iter()
                .any(|n| attr(*n, "ratingKey").is_none_or(str::is_empty))
        {
            return Err(fail("incomplete_collection"));
        }
        Ok((
            identity,
            nodes
                .into_iter()
                .map(|n| json!(attr(n, "ratingKey")))
                .collect(),
        ))
    }
    fn jellyfin_collection(&self, key: &str, cancel: &AtomicBool) -> Result<(Value, Vec<Value>)> {
        let query = form(vec![
            ("Ids".into(), key.into()),
            ("Limit".into(), "2".into()),
            ("Fields".into(), "DateCreated,Path".into()),
        ]);
        let response = self.data(&format!("/Items?{query}"), cancel)?;
        if !response.is_object()
            || !equal(&response["TotalRecordCount"], &json!(1))
            || response["Items"].as_array().is_none_or(|v| v.len() != 1)
        {
            return Err(fail("invalid_collection"));
        }
        let node = &response["Items"][0];
        if node["Id"] != key || node["Type"] != "BoxSet" {
            return Err(fail("unsupported_collection"));
        }
        let identity = json!({"created":text(node["DateCreated"].as_str(), 4096)?,"path":path(&node["Path"])?});
        let children = self.data(
            &format!("/Items?ParentId={key}&Recursive=false&Limit=1000"),
            cancel,
        )?;
        let nodes = children["Items"]
            .as_array()
            .ok_or_else(|| fail("incomplete_collection"))?;
        if !children.is_object()
            || !equal(&children["TotalRecordCount"], &json!(nodes.len()))
            || nodes.len() >= 1000
            || nodes.iter().any(|n| !n.is_object() || !truthy(&n["Id"]))
        {
            return Err(fail("incomplete_collection"));
        }
        Ok((identity, nodes.iter().map(|n| n["Id"].clone()).collect()))
    }
    fn parameters(&self, row: &Value) -> Result<String> {
        let kind = row["before"]["identity"]["kind"]
            .as_str()
            .ok_or_else(|| fail("remote_identity_mismatch"))?;
        let mut params = vec![
            ("id".into(), consumers::string(&row["target"], "id")?.into()),
            (
                "type".into(),
                plex_metadata::kind(kind)
                    .map_err(|_| fail("remote_identity_mismatch"))?
                    .1
                    .to_string(),
            ),
        ];
        for (field, value) in row["after"]
            .as_object()
            .ok_or_else(|| fail("invalid_fields"))?
        {
            if equal(value, &row["before"]["values"][field]) {
                continue;
            }
            let wire = plex_wire(field)?;
            params.push((
                format!("{wire}.locked"),
                if row["before"]["locks"][field] == true {
                    "1"
                } else {
                    "0"
                }
                .into(),
            ));
            if matches!(field.as_str(), "genres" | "labels") {
                let final_tags = value.as_array().ok_or_else(|| fail("invalid_remote_set"))?;
                for (index, tag) in final_tags.iter().enumerate() {
                    params.push((
                        format!("{wire}[{index}].tag.tag"),
                        text(tag.as_str(), 255)?.into(),
                    ));
                }
                let before = string_set(&row["before"]["values"][field])?;
                let after = string_set(value)?;
                let removed = before
                    .difference(&after)
                    .map(|s| quote(s))
                    .collect::<Vec<_>>();
                if !removed.is_empty() {
                    params.push((format!("{wire}[].tag.tag-"), removed.join(",")));
                }
            } else {
                params.push((
                    format!("{wire}.value"),
                    if value.is_null() {
                        String::new()
                    } else if let Some(s) = value.as_str() {
                        s.into()
                    } else {
                        value.to_string()
                    },
                ));
            }
        }
        Ok(form(params))
    }
    fn payload(&self, row: &Value) -> Result<Value> {
        let mut payload = row["before"]["payload"]
            .as_object()
            .cloned()
            .ok_or_else(|| fail("invalid_metadata_dto"))?;
        for (field, value) in row["after"]
            .as_object()
            .ok_or_else(|| fail("invalid_fields"))?
        {
            payload.insert(jellyfin_wire(field)?.into(), value.clone());
        }
        Ok(json!(payload))
    }
    pub(super) fn validate(&self, _library: &Value, row: &Value) -> Result<()> {
        if let Some(collections) = &self.collections {
            if string_set(&row["after"]["collection_ids"])?
                .iter()
                .any(|id| !collections.contains(id))
            {
                return Err(fail("collection_outside_adopted_scope"));
            }
        } else if self.plex {
            if self.parameters(row)?.len() > 16384 {
                return Err(fail("request_too_large"));
            }
        } else if catabolic_core::encode(&self.payload(row)?).len() > 1024 * 1024 {
            return Err(fail("request_too_large"));
        }
        Ok(())
    }
    pub(super) fn write(&self, library: &Value, row: &Value, cancel: &AtomicBool) -> Result<()> {
        checkpoint(cancel)?;
        if self.collections.is_some() {
            let mut before = string_set(&row["before"]["values"]["collection_ids"])?;
            let after = string_set(&row["after"]["collection_ids"])?;
            let changes = before
                .symmetric_difference(&after)
                .cloned()
                .collect::<Vec<_>>();
            for key in changes {
                checkpoint(cancel)?;
                let observed = self.read(library, &row["target"], &row["after"], cancel)?;
                if !equal(&observed["identity"], &row["before"]["identity"])
                    || string_set(&observed["values"]["collection_ids"])? != before
                {
                    return Err(fail("collection_changed"));
                }
                let adding = after.contains(&key);
                let target = consumers::string(&row["target"], "id")?;
                checkpoint(cancel)?;
                if self.plex {
                    let route = format!("/library/collections/{key}/items");
                    if adding {
                        let server = consumers::string(&self.connection, "server_id")?;
                        let uri = format!(
                            "server://{server}/com.plexapp.plugins.library/library/metadata/{target}"
                        );
                        self.transport
                            .put(&format!("{route}?{}", form(vec![("uri".into(), uri)])))
                            .map_err(network_error)?;
                    } else {
                        self.transport
                            .delete(&format!("{route}/{target}"))
                            .map_err(network_error)?;
                    }
                } else {
                    let route = format!(
                        "/Collections/{key}/Items?{}",
                        form(vec![("ids".into(), target.into())])
                    );
                    if adding {
                        self.transport.call(&route, true).map_err(network_error)?;
                    } else {
                        self.transport.delete(&route).map_err(network_error)?;
                    }
                }
                if adding {
                    before.insert(key);
                } else {
                    before.remove(&key);
                }
            }
        } else if self.plex {
            let route = format!(
                "/library/sections/{}/all?{}",
                quote(consumers::string(library, "id")?),
                self.parameters(row)?
            );
            checkpoint(cancel)?;
            self.transport.put(&route).map_err(network_error)?;
        } else {
            let route = format!("/Items/{}", consumers::string(&row["target"], "id")?);
            let body = catabolic_core::encode(&self.payload(row)?);
            checkpoint(cancel)?;
            self.transport
                .post_json(&route, body.as_bytes())
                .map_err(network_error)?;
        }
        Ok(())
    }
    pub(super) fn matches(&self, observed: &Value, before: &Value, after: &Value) -> bool {
        if self.plex || self.collections.is_some() {
            let mut expected = before.clone();
            expected["values"] = after.clone();
            equal(observed, &expected)
        } else {
            let Ok(selected) = fields(after) else {
                return false;
            };
            let Ok(changed) = selected
                .iter()
                .map(|f| jellyfin_wire(f))
                .collect::<Result<BTreeSet<_>>>()
            else {
                return false;
            };
            let untouched = |value: &Value| {
                value["payload"].as_object().map(|p| {
                    p.iter()
                        .filter(|(k, _)| !changed.contains(k.as_str()))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<Map<_, _>>()
                })
            };
            equal(&observed["identity"], &before["identity"])
                && equal(&observed["values"], after)
                && match (untouched(observed), untouched(before)) {
                    (Some(a), Some(b)) => equal(&Value::Object(a), &Value::Object(b)),
                    _ => false,
                }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
        time::{Duration, Instant},
    };

    fn connection(application: &str, endpoint: &str) -> Value {
        // PATH is a non-secret fixture credential. Never mutate process-wide
        // environment from concurrently running Rust tests or load real tokens.
        json!({"application":application,"endpoint":endpoint,"credential_env":"PATH","server_id":"fixture-server"})
    }
    fn adapter(application: &str, collections: Value) -> MappingAdapter {
        MappingAdapter::new(&connection(application, "http://127.0.0.1:1"), &collections).unwrap()
    }
    fn reply(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }
    struct Fixture {
        endpoint: String,
        requests: Arc<Mutex<Vec<String>>>,
        join: Option<thread::JoinHandle<()>>,
    }
    impl Fixture {
        fn new(responses: Vec<String>) -> Self {
            Self::with_cancel(responses, None)
        }
        fn with_cancel(responses: Vec<String>, cancel: Option<(usize, Arc<AtomicBool>)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = requests.clone();
            let join = thread::spawn(move || {
                for (index, response) in responses.into_iter().enumerate() {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let mut socket = loop {
                        match listener.accept() {
                            Ok((socket, _)) => break socket,
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(
                                    Instant::now() < deadline,
                                    "fixture request {index} was not received"
                                );
                                thread::sleep(Duration::from_millis(2));
                            }
                            Err(e) => panic!("fixture accept: {e}"),
                        }
                    };
                    // Accepted sockets inherit nonblocking mode on macOS.
                    // Requests and replies use bounded blocking I/O here.
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut buffer = [0u8; 4096];
                    loop {
                        let n = socket.read(&mut buffer).unwrap();
                        assert_ne!(n, 0, "request ended before its body");
                        bytes.extend_from_slice(&buffer[..n]);
                        if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                            let size = headers
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|n| n.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            if bytes.len() >= end + 4 + size {
                                break;
                            }
                        }
                    }
                    captured
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(bytes).unwrap());
                    if let Some((at, flag)) = &cancel
                        && *at == index
                    {
                        flag.store(true, Ordering::Release);
                    }
                    socket.write_all(response.as_bytes()).unwrap();
                }
            });
            Self {
                endpoint,
                requests,
                join: Some(join),
            }
        }
        fn adapter(&self, application: &str, collections: Value) -> MappingAdapter {
            MappingAdapter::new(&connection(application, &self.endpoint), &collections).unwrap()
        }
        fn finish(mut self) -> Vec<String> {
            self.join.take().unwrap().join().unwrap();
            self.requests.lock().unwrap().clone()
        }
    }
    fn plex_item() -> &'static str {
        r#"<MediaContainer librarySectionID="1"><Video ratingKey="7" type="movie" guid="plex://movie/fixture" title="Old" year="2020"><Media><Part file="/movies/a.mkv"/><Part file="/movies/a.mkv"/></Media><Field name="title" locked="1"/><Field name="genre" locked="0"/><Genre tag="Old, Tag"/><Genre tag="Keep"/><Genre tag="Keep"/><Label tag="Sample"/></Video></MediaContainer>"#
    }
    fn library() -> Value {
        json!({"id":"1","type":"movie"})
    }
    fn target() -> Value {
        json!({"id":"7","path":"/movies/a.mkv"})
    }
    fn plex_collection(key: &str) -> String {
        format!(
            r#"<MediaContainer><Directory ratingKey="{key}" type="collection" guid="plex://collection/{key}" librarySectionID="1" subtype="movie" smart="0"/></MediaContainer>"#
        )
    }
    fn children(member: bool) -> String {
        if member {
            r#"<MediaContainer offset="0" totalSize="1"><Video ratingKey="7"/></MediaContainer>"#
                .into()
        } else {
            r#"<MediaContainer size="0"/>"#.into()
        }
    }
    fn collection_row() -> Value {
        json!({"target":target(),"before":{"identity":{"guid":"plex://movie/fixture","paths":["/movies/a.mkv"],"kind":"movie","collections":{"10":"plex://collection/10","20":"plex://collection/20"}},"values":{"collection_ids":["10"]},"locks":{}},"after":{"collection_ids":["20"]}})
    }
    fn jellyfin_dto() -> Value {
        json!({"Id":"0123456789abcdef0123456789abcdef","Type":"Movie","Path":"/movies/a.mkv","Name":"Old","Genres":["Drama","Drama","Action"],"Tags":["Keep"],"ProviderIds":{"imdb":"tt123"},"LockData":true,"LockedFields":["Name"],"Studios":[{"Name":"Studio"}],"CommunityRating":7.2,"ProductionYear":2020,"ExtraReadOnly":"ignored"})
    }

    #[test]
    fn capabilities_scope_and_metadata_values_are_frozen() {
        let plex = adapter("plex", Value::Null);
        assert_eq!(plex.capabilities().len(), 12);
        assert!(plex.capabilities()["genres"]);
        assert!(!plex.capabilities()["title"]);
        assert!(!plex.capabilities().contains_key("collection_ids"));
        assert_eq!(
            adapter("jellyfin", json!(["0123456789abcdef0123456789abcdef"]))
                .capabilities()
                .len(),
            9
        );
        for scope in [json!([]), json!({}), json!("7")] {
            assert_eq!(
                MappingAdapter::new(&connection("plex", "http://127.0.0.1:1"), &scope)
                    .err()
                    .unwrap()
                    .0,
                "mapping_invalid_collection_scope"
            );
        }
        for scope in [json!(["7", "7"]), json!(["x"]), json!([7])] {
            assert_eq!(
                MappingAdapter::new(&connection("plex", "http://127.0.0.1:1"), &scope)
                    .err()
                    .unwrap()
                    .0,
                "mapping_invalid_collection_id"
            );
        }
        assert!(
            MappingAdapter::new(
                &connection("jellyfin", "http://127.0.0.1:1"),
                &json!(["ABCDEF0123456789abcdef0123456789"])
            )
            .is_err()
        );
        assert_eq!(
            plex.validate_value("year", &json!(true)).unwrap_err().0,
            "metadata_invalid_year"
        );
        assert_eq!(
            plex.validate_value("title", &json!(" \t")).unwrap_err().0,
            "metadata_invalid_title"
        );
        assert!(
            plex.validate_value("release_date", &json!("2024-02-29"))
                .is_ok()
        );
        assert!(
            plex.validate_value("release_date", &json!("2023-02-29"))
                .is_err()
        );
    }

    #[test]
    fn plex_snapshot_put_sets_locks_and_double_encoded_removals() {
        let fixture = Fixture::new(vec![reply(plex_item()), reply("")]);
        let adapter = fixture.adapter("plex", Value::Null);
        let before = adapter
            .read(
                &library(),
                &target(),
                &json!({"title":{},"genres":{},"labels":{},"year":{}}),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(before["identity"]["paths"], json!(["/movies/a.mkv"]));
        assert_eq!(before["values"]["genres"], json!(["Keep", "Old, Tag"]));
        assert_eq!(before["locks"]["title"], true);
        let after = json!({"title":"New title","genres":["Keep","Sci-Fi"],"labels":[],"year":null});
        let row = json!({"target":target(),"before":before,"after":after});
        adapter.validate(&library(), &row).unwrap();
        adapter
            .write(&library(), &row, &AtomicBool::new(false))
            .unwrap();
        let requests = fixture.finish();
        assert!(requests[0].starts_with("GET /library/metadata/7?includeGuids=1 "));
        assert!(requests[1].starts_with("PUT /library/sections/1/all?id=7&type=1&"));
        assert!(requests[1].contains("genre%5B%5D.tag.tag-=Old%252C%2520Tag"));
        assert!(requests[1].contains("label%5B%5D.tag.tag-=Sample"));
        assert!(requests[1].contains("title.locked=1&title.value=New+title"));
        assert!(requests[1].contains("year.locked=0&year.value="));
        let mut observed = row["before"].clone();
        observed["values"] = row["after"].clone();
        assert!(adapter.matches(&observed, &row["before"], &row["after"]));
        observed["locks"]["title"] = json!(false);
        assert!(!adapter.matches(&observed, &row["before"], &row["after"]));
    }

    #[test]
    fn plex_rejects_identity_path_kind_locks_and_incomplete_collections() {
        let adapter = adapter("plex", Value::Null);
        for (wire, code) in [
            (
                plex_item().replace("ratingKey=\"7\"", "ratingKey=\"8\""),
                "mapping_remote_identity_mismatch",
            ),
            (
                plex_item().replace("/movies/a.mkv", "/movies/b.mkv"),
                "mapping_remote_path_mismatch",
            ),
            (
                plex_item().replace("locked=\"1\"", "locked=\"true\""),
                "mapping_invalid_locks",
            ),
            (
                plex_item().replace("year=\"2020\"", "year=\"2e3\""),
                "mapping_invalid_remote_year",
            ),
        ] {
            assert_eq!(
                adapter
                    .parse_plex(wire.as_bytes(), &library(), &target(), &["year"])
                    .unwrap_err()
                    .0,
                code
            );
        }
        let episode = plex_item().replace("type=\"movie\"", "type=\"episode\"");
        assert_eq!(
            adapter
                .parse_plex(
                    episode.as_bytes(),
                    &json!({"id":"1","type":"show"}),
                    &target(),
                    &["year"]
                )
                .unwrap_err()
                .0,
            "mapping_unsupported_fields_for_kind"
        );
        let fixture = Fixture::new(vec![
            reply(plex_item()),
            reply(&plex_collection("10")),
            reply(r#"<MediaContainer totalSize="2"><Video ratingKey="7"/></MediaContainer>"#),
        ]);
        let adapter = fixture.adapter("plex", json!(["10"]));
        assert_eq!(
            adapter
                .read(
                    &library(),
                    &target(),
                    &json!({"collection_ids":{}}),
                    &AtomicBool::new(false)
                )
                .unwrap_err()
                .0,
            "mapping_incomplete_collection"
        );
        assert_eq!(fixture.finish().len(), 3);
    }

    #[test]
    fn jellyfin_roundtrips_editable_payload_and_untouched_properties() {
        let dto = jellyfin_dto();
        let fixture = Fixture::new(vec![
            reply(&json!({"TotalRecordCount":1,"Items":[dto]}).to_string()),
            reply(r#"[{"Id":"1"}]"#),
            reply(""),
        ]);
        let adapter = fixture.adapter("jellyfin", Value::Null);
        let target = json!({"id":dto["Id"],"path":dto["Path"]});
        let before = adapter
            .read(
                &library(),
                &target,
                &json!({"title":{},"genres":{}}),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(before["values"]["genres"], json!(["Action", "Drama"]));
        assert!(before["payload"].get("ExtraReadOnly").is_none());
        let row =
            json!({"target":target,"before":before,"after":{"title":"New","genres":["Comedy"]}});
        adapter.validate(&library(), &row).unwrap();
        adapter
            .write(&library(), &row, &AtomicBool::new(false))
            .unwrap();
        let requests = fixture.finish();
        assert!(requests[0].starts_with("GET /Items?Ids=0123456789abcdef0123456789abcdef&Limit=2&Recursive=true&EnableUserData=false&Fields="));
        assert!(requests[1].starts_with("GET /Items/0123456789abcdef0123456789abcdef/Ancestors "));
        assert!(requests[2].starts_with("POST /Items/0123456789abcdef0123456789abcdef "));
        assert!(
            requests[2]
                .to_lowercase()
                .contains("content-type: application/json")
        );
        assert!(requests[2].to_lowercase().contains("x-emby-token:"));
        let payload =
            catabolic_core::decode_json(requests[2].split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(payload["Name"], "New");
        assert_eq!(payload["LockedFields"], dto["LockedFields"]);
        assert_eq!(payload["ProviderIds"], dto["ProviderIds"]);
        assert_eq!(payload["Studios"], dto["Studios"]);
        let mut observed = row["before"].clone();
        observed["values"] = row["after"].clone();
        observed["payload"] = payload;
        assert!(adapter.matches(&observed, &row["before"], &row["after"]));
        observed["payload"]["Studios"] = json!([]);
        assert!(!adapter.matches(&observed, &row["before"], &row["after"]));
    }

    #[test]
    fn jellyfin_dto_requires_complete_locks_and_typed_values() {
        let adapter = adapter("jellyfin", Value::Null);
        let mut dto = jellyfin_dto();
        dto.as_object_mut().unwrap().remove("LockData");
        assert_eq!(
            adapter.snapshot_jellyfin(&dto, &["title"]).unwrap_err().0,
            "mapping_incomplete_metadata_dto"
        );
        dto["LockData"] = json!("true");
        assert_eq!(
            adapter.snapshot_jellyfin(&dto, &["title"]).unwrap_err().0,
            "mapping_invalid_metadata_dto"
        );
        dto["LockData"] = json!(true);
        dto["ProductionYear"] = json!(2020.0);
        assert_eq!(
            adapter.snapshot_jellyfin(&dto, &["year"]).unwrap_err().0,
            "mapping_invalid_remote_year"
        );
        dto["Genres"] = json!(["good", "bad\n"]);
        assert_eq!(
            adapter.snapshot_jellyfin(&dto, &["genres"]).unwrap_err().0,
            "invalid_configuration"
        );
        dto["Name"] = json!(false);
        assert_eq!(
            adapter.snapshot_jellyfin(&dto, &["title"]).unwrap()["values"]["title"],
            ""
        );
    }

    #[test]
    fn collection_write_rechecks_each_operation_and_uses_explicit_ids() {
        let mut responses = Vec::new();
        for member in [true, false] {
            responses.extend([
                reply(plex_item()),
                reply(&plex_collection("10")),
                reply(&children(member)),
                reply(&plex_collection("20")),
                reply(&children(false)),
                reply(""),
            ]);
        }
        let fixture = Fixture::new(responses);
        let adapter = fixture.adapter("plex", json!(["20", "10"]));
        let row = collection_row();
        adapter.validate(&library(), &row).unwrap();
        adapter
            .write(&library(), &row, &AtomicBool::new(false))
            .unwrap();
        let requests = fixture.finish();
        assert_eq!(requests.len(), 12);
        assert!(requests[5].starts_with("DELETE /library/collections/10/items/7 "));
        assert!(requests[11].starts_with("PUT /library/collections/20/items?uri=server%3A%2F%2Ffixture-server%2Fcom.plexapp.plugins.library%2Flibrary%2Fmetadata%2F7 "));
        let mut bad = row.clone();
        bad["after"]["collection_ids"] = json!(["30"]);
        assert_eq!(
            adapter.validate(&library(), &bad).unwrap_err().0,
            "mapping_collection_outside_adopted_scope"
        );
    }

    #[test]
    fn collection_changes_and_cancellation_stop_before_next_effect() {
        let fixture = Fixture::new(vec![
            reply(plex_item()),
            reply(&plex_collection("10")),
            reply(&children(false)),
            reply(&plex_collection("20")),
            reply(&children(false)),
        ]);
        let adapter = fixture.adapter("plex", json!(["10", "20"]));
        assert_eq!(
            adapter
                .write(&library(), &collection_row(), &AtomicBool::new(false))
                .unwrap_err()
                .0,
            "mapping_collection_changed"
        );
        assert_eq!(fixture.finish().len(), 5);

        let cancel = Arc::new(AtomicBool::new(false));
        let fixture = Fixture::with_cancel(
            vec![
                reply(plex_item()),
                reply(&plex_collection("10")),
                reply(&children(true)),
                reply(&plex_collection("20")),
                reply(&children(false)),
                reply(""),
            ],
            Some((5, cancel.clone())),
        );
        let adapter = fixture.adapter("plex", json!(["10", "20"]));
        assert_eq!(
            adapter
                .write(&library(), &collection_row(), &cancel)
                .unwrap_err()
                .0,
            "destination mapping interrupted"
        );
        let requests = fixture.finish();
        assert_eq!(requests.len(), 6);
        assert!(requests[5].starts_with("DELETE "));

        let cancel = Arc::new(AtomicBool::new(false));
        let fixture = Fixture::with_cancel(
            vec![reply(plex_item()), reply(&plex_collection("10"))],
            Some((1, cancel.clone())),
        );
        let adapter = fixture.adapter("plex", json!(["10", "20"]));
        assert_eq!(
            adapter
                .read(
                    &library(),
                    &target(),
                    &json!({"collection_ids":{}}),
                    &cancel
                )
                .unwrap_err()
                .0,
            "destination mapping interrupted"
        );
        assert_eq!(fixture.finish().len(), 2);
    }

    #[test]
    fn transport_preserves_error_codes_bounds_and_no_redirect_replay() {
        for (response, expected) in [
            ("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_owned(), "unauthorized"),
            ("HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\r\n".to_owned(), "rate_limited"),
            ("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/forbidden\r\nContent-Length: 0\r\n\r\n".to_owned(), "temporarily_failed"),
            ("HTTP/1.1 200 OK\r\nContent-Length: 8388609\r\n\r\n".to_owned(), "response_limit"),
            (String::new(), "temporarily_failed"),
        ] {
            let fixture = Fixture::new(vec![response]);
            let adapter = fixture.adapter("jellyfin", Value::Null);
            let row = json!({"target":{"id":"0123456789abcdef0123456789abcdef"},"before":{"payload":{"Name":"Old"}},"after":{"title":"New"}});
            assert_eq!(adapter.write(&library(), &row, &AtomicBool::new(false)).unwrap_err().0, expected);
            assert_eq!(fixture.finish().len(), 1);
        }
    }

    #[test]
    fn jellyfin_explicit_collections_delete_and_post_memberships() {
        let first = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let second = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let dto = jellyfin_dto();
        let item = dto["Id"].as_str().unwrap();
        let mut responses = Vec::new();
        for member in [true, false] {
            responses.push(reply(
                &json!({"TotalRecordCount":1.0,"Items":[dto]}).to_string(),
            ));
            responses.push(reply(r#"[{"Id":"1"}]"#));
            for (key, present) in [(first, member), (second, false)] {
                responses.push(reply(&json!({"TotalRecordCount":true,"Items":[{"Id":key,"Type":"BoxSet","DateCreated":"2026-01-01","Path":format!("/collections/{key}")}]}).to_string()));
                responses.push(reply(&json!({"TotalRecordCount":if present {1} else {0},"Items":if present {json!([{"Id":item}])} else {json!([])}}).to_string()));
            }
            responses.push(reply(""));
        }
        let fixture = Fixture::new(responses);
        let adapter = fixture.adapter("jellyfin", json!([second, first]));
        let mut before = adapter.snapshot_jellyfin(&dto, &[]).unwrap();
        before["identity"]["collections"] = json!({first:{"created":"2026-01-01","path":format!("/collections/{first}")},second:{"created":"2026-01-01","path":format!("/collections/{second}")}});
        before["values"] = json!({"collection_ids":[first]});
        let row = json!({"target":{"id":item,"path":"/movies/a.mkv"},"before":before,"after":{"collection_ids":[second]}});
        adapter
            .write(&library(), &row, &AtomicBool::new(false))
            .unwrap();
        let requests = fixture.finish();
        assert_eq!(requests.len(), 14);
        assert!(requests[6].starts_with(&format!("DELETE /Collections/{first}/Items?ids={item} ")));
        assert!(requests[13].starts_with(&format!("POST /Collections/{second}/Items?ids={item} ")));
    }

    #[test]
    fn frozen_json_equality_preserves_numeric_dto_semantics() {
        assert!(equal(&json!({"n":[1,true,7.0]}), &json!({"n":[1.0,1,7]})));
        assert!(equal(&json!(0), &json!(-0.0)));
        assert!(!equal(
            &json!(9_007_199_254_740_993u64),
            &json!(9_007_199_254_740_992.0)
        ));
        assert!(!equal(&json!(false), &json!(0.25)));
        assert!(!equal(&json!(1), &json!("1")));
    }

    #[test]
    fn request_budgets_and_pre_cancel_fail_without_network() {
        let plex = adapter("plex", Value::Null);
        let mut row = json!({"target":target(),"before":{"identity":{"kind":"movie"},"values":{"summary":""},"locks":{"summary":false}},"after":{"summary":""}});
        row["after"]["summary"] = json!("é".repeat(8192));
        assert_eq!(
            plex.validate(&library(), &row).unwrap_err().0,
            "mapping_request_too_large"
        );
        assert_eq!(
            plex.write(&library(), &row, &AtomicBool::new(true))
                .unwrap_err()
                .0,
            "destination mapping interrupted"
        );
        assert_eq!(
            plex.read(
                &library(),
                &target(),
                &json!({"title":{}}),
                &AtomicBool::new(true)
            )
            .unwrap_err()
            .0,
            "destination mapping interrupted"
        );
        let jellyfin = adapter("jellyfin", Value::Null);
        let row =
            json!({"before":{"payload":{"Overview":"a".repeat(1024*1024)}},"after":{"title":"x"}});
        assert_eq!(
            jellyfin.validate(&library(), &row).unwrap_err().0,
            "mapping_request_too_large"
        );
    }
}
