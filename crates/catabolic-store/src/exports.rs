// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen metadata export formats; no filesystem effects during construction.
use crate::{Error, Result};
use catabolic_core::encode;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const MAX_BYTES: usize = 128 * 1024 * 1024;
fn fail<T>(message: &str) -> Result<T> {
    Err(Error(message.into()))
}
fn array(value: &Value) -> Result<&[Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| Error("invalid catalog export document".into()))
}
fn text(value: &Value) -> Result<&str> {
    value
        .as_str()
        .ok_or_else(|| Error("invalid catalog export document".into()))
}
pub fn truth(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
fn bounded_push(output: &mut String, value: &str) -> Result<()> {
    if output
        .len()
        .checked_add(value.len())
        .is_none_or(|n| n > MAX_BYTES)
    {
        return fail("metadata export exceeds 128 MiB");
    }
    output.push_str(value);
    Ok(())
}
fn repr(value: &Value) -> String {
    match value {
        Value::String(s) => {
            let quote = if s.contains('\'') && !s.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut out = quote.to_string();
            for c in s.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c if c.is_control() && (c as u32) < 256 => {
                        out.push_str(&format!("\\x{:02x}", c as u32))
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        }
        Value::Array(v) => format!("[{}]", v.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(v) => format!(
            "{{{}}}",
            v.iter()
                .map(|(k, v)| format!("{}: {}", repr(&json!(k)), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        Value::Number(_) => encode(value),
    }
}
pub fn python_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| repr(value))
}
fn xml_text(value: &Value) -> Result<String> {
    let value = python_text(value);
    if value
        .chars()
        .any(|c| matches!(c as u32, 0..=8 | 11..=12 | 14..=31 | 0xfffe | 0xffff))
    {
        return fail("metadata contains characters forbidden by XML 1.0");
    }
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;"))
}
fn element(name: &str, value: &Value) -> Result<String> {
    let value = xml_text(value)?;
    Ok(if value.is_empty() {
        format!("<{name} />")
    } else {
        format!("<{name}>{value}</{name}>")
    })
}
fn title(item: &Value) -> &Value {
    if truth(&item["metadata"]["title"]) {
        &item["metadata"]["title"]
    } else {
        &item["id"]
    }
}
pub fn quote(value: &str, slash: bool) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"_.-~".contains(&byte) || slash && byte == b'/' {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}
pub fn http_base(value: Option<&str>) -> Result<String> {
    let value = value.unwrap_or("");
    // urllib.urlsplit removes leading C0/space and embedded tab/newline for
    // parsing, but the frozen helper returns the original spelling.
    let parse = value
        .trim_start_matches(|c: char| c as u32 <= 32)
        .replace(['\t', '\r', '\n'], "");
    let Some((scheme, rest)) = parse.split_once("://") else {
        return fail(
            "base URL must be an HTTP(S) directory URL without credentials, query or fragment",
        );
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
        || authority.is_empty()
        || authority
            .rsplit_once('@')
            .is_some_and(|(userinfo, _)| !userinfo.trim_matches(':').is_empty())
        || parse
            .split_once('?')
            .is_some_and(|(_, query)| !query.split('#').next().unwrap_or("").is_empty())
        || parse
            .split_once('#')
            .is_some_and(|(_, fragment)| !fragment.is_empty())
    {
        return fail(
            "base URL must be an HTTP(S) directory URL without credentials, query or fragment",
        );
    }
    if authority.contains('[') != authority.contains(']') {
        return fail("Invalid IPv6 URL");
    }
    Ok(value.trim_end_matches('/').to_owned() + "/")
}
fn metadata_text(item: &Value, key: &str, fallback: Option<&str>) -> Result<Option<String>> {
    let value = &item["metadata"][key];
    if value.is_null() || value == "" {
        return Ok(fallback.map(str::to_owned));
    }
    value.as_str().map(|v| Some(v.to_owned())).ok_or_else(|| {
        Error(format!(
            "{key} must be text for metadata export: {}",
            item["id"].as_str().unwrap_or("")
        ))
    })
}
// Preserve json.dumps(indent=2)'s insertion order for the documented OPDS bytes.
#[derive(Clone)]
enum Json {
    Object(Vec<(&'static str, Json)>),
    Array(Vec<Json>),
    Scalar(Value),
}
impl Json {
    fn text(value: impl Into<String>) -> Self {
        Self::Scalar(json!(value.into()))
    }
    fn render(&self, depth: usize, output: &mut String) -> Result<()> {
        let (open, close, len) = match self {
            Self::Scalar(value) => return bounded_push(output, &encode(value)),
            Self::Object(values) => ("{", "}", values.len()),
            Self::Array(values) => ("[", "]", values.len()),
        };
        bounded_push(output, open)?;
        for index in 0..len {
            bounded_push(output, if index == 0 { "\n" } else { ",\n" })?;
            bounded_push(output, &"  ".repeat(depth + 1))?;
            match self {
                Self::Object(values) => {
                    bounded_push(output, &encode(&json!(values[index].0)))?;
                    bounded_push(output, ": ")?;
                    values[index].1.render(depth + 1, output)?;
                }
                Self::Array(values) => values[index].render(depth + 1, output)?,
                Self::Scalar(_) => unreachable!(),
            }
        }
        if len > 0 {
            bounded_push(output, "\n")?;
            bounded_push(output, &"  ".repeat(depth))?;
        }
        bounded_push(output, close)
    }
}
fn suffix(path: &str) -> &str {
    let name = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_start_matches('.');
    name.rfind('.')
        .filter(|i| *i > 0)
        .map(|i| &name[i..])
        .unwrap_or("")
}
fn mime(path: &str) -> String {
    match suffix(path).to_ascii_lowercase().as_str() {
        ".epub" => "application/epub+zip",
        ".pdf" => "application/pdf",
        ".cbz" => "application/vnd.comicbook+zip",
        ".cbr" => "application/vnd.comicbook-rar",
        _ => return crate::mime::guess(path).unwrap_or_else(|| "application/octet-stream".into()),
    }
    .into()
}

pub fn build(document: &Value, format: &str, base_url: Option<&str>) -> Result<Value> {
    catabolic_core::validate_finite(document)?;
    let content = &document["content"];
    let mut items = BTreeMap::new();
    for item in array(&content["items"])? {
        items.insert(text(&item["id"])?, item);
    }
    let mut associations = BTreeMap::new();
    for association in array(&content["associations"])? {
        associations.insert(text(&association["id"])?, association);
    }
    let mut entries = vec![];
    for entry in array(&content["entries"])? {
        let mut primary = false;
        for id in array(&entry["association_ids"])? {
            let association = associations
                .get(text(id)?)
                .ok_or_else(|| Error("unknown export association".into()))?;
            primary |= association["role"] == "primary";
        }
        if primary {
            entries.push(entry);
        }
    }
    let item_for = |entry: &Value| -> Result<&Value> {
        items
            .get(text(&entry["item_id"])?)
            .copied()
            .ok_or_else(|| Error("unknown export item".into()))
    };
    let mut artifacts: Vec<(String, String)> = vec![];
    let mut generated_bytes = 0usize;
    match format {
        "xspf" => {
            let base = base_url
                .filter(|s| !s.is_empty())
                .map(|s| http_base(Some(s)))
                .transpose()?
                .unwrap_or_default();
            let mut tracks = String::new();
            for entry in &entries {
                let item = item_for(entry)?;
                if ![
                    "movie",
                    "episode",
                    "video",
                    "music_video",
                    "track",
                    "recording",
                    "audiobook",
                    "audiobook_chapter",
                    "podcast_episode",
                ]
                .contains(&text(&item["kind"])?)
                {
                    continue;
                }
                bounded_push(&mut tracks, "<track>")?;
                bounded_push(
                    &mut tracks,
                    &element(
                        "location",
                        &json!(base.clone() + &quote(text(&entry["path"])?, true)),
                    )?,
                )?;
                bounded_push(&mut tracks, &element("title", title(item))?)?;
                bounded_push(&mut tracks, "</track>")?;
            }
            let tracks = if tracks.is_empty() {
                "<trackList />".into()
            } else {
                format!("<trackList>{tracks}</trackList>")
            };
            artifacts.push(("catalog.xspf".into(),format!("<?xml version='1.0' encoding='utf-8'?>\n<playlist version=\"1\" xmlns=\"http://xspf.org/ns/0/\">{}{tracks}</playlist>\n",element("title",&content["catalog"]["id"])?)));
        }
        "opds" => {
            let base = http_base(base_url)?;
            let mut publications: Vec<(&str, Json, Vec<Json>)> = vec![];
            for entry in &entries {
                let item = item_for(entry)?;
                if !["book", "book_edition", "comic_issue", "comic_volume"]
                    .contains(&text(&item["kind"])?)
                {
                    continue;
                }
                let id = text(&item["id"])?;
                let position = if let Some(i) = publications.iter().position(|v| v.0 == id) {
                    i
                } else {
                    let mut metadata = vec![
                        ("@type", Json::text("http://schema.org/Book")),
                        (
                            "title",
                            Json::text(metadata_text(item, "title", Some(id))?.unwrap()),
                        ),
                        (
                            "identifier",
                            Json::text(format!(
                                "urn:catabolic:{}:{}",
                                text(&content["database_id"])?,
                                quote(id, false)
                            )),
                        ),
                    ];
                    if let Some(author) =
                        metadata_text(item, "author", None)?.filter(|s| !s.is_empty())
                    {
                        metadata.push((
                            "author",
                            Json::Array(vec![Json::Object(vec![("name", Json::text(author))])]),
                        ));
                    }
                    publications.push((id, Json::Object(metadata), vec![]));
                    publications.len() - 1
                };
                let path = text(&entry["path"])?;
                // Bound the materialized JSON tree before repeated URL strings
                // are allocated; rendering has a separate exact byte bound.
                generated_bytes = generated_bytes
                    .checked_add(base.len())
                    .and_then(|n| n.checked_add(path.len().saturating_mul(3)))
                    .ok_or_else(|| Error("metadata export exceeds 128 MiB".into()))?;
                if generated_bytes > MAX_BYTES {
                    return fail("metadata export exceeds 128 MiB");
                }
                publications[position].2.push(Json::Object(vec![
                    ("rel", Json::text("http://opds-spec.org/acquisition")),
                    ("href", Json::text(base.clone() + &quote(path, true))),
                    ("type", Json::text(mime(path))),
                ]));
            }
            let count = publications.len();
            let publications = publications
                .into_iter()
                .map(|(_, metadata, links)| {
                    Json::Object(vec![("metadata", metadata), ("links", Json::Array(links))])
                })
                .collect();
            let value = Json::Object(vec![
                (
                    "metadata",
                    Json::Object(vec![
                        ("title", Json::Scalar(content["catalog"]["id"].clone())),
                        ("numberOfItems", Json::Scalar(json!(count))),
                    ]),
                ),
                (
                    "links",
                    Json::Array(vec![Json::Object(vec![
                        ("rel", Json::text("self")),
                        ("href", Json::text(base + "catalog.opds.json")),
                        ("type", Json::text("application/opds+json")),
                    ])]),
                ),
                ("publications", Json::Array(publications)),
            ]);
            let mut output = String::new();
            value.render(0, &mut output)?;
            bounded_push(&mut output, "\n")?;
            artifacts.push(("catalog.opds.json".into(), output));
        }
        "nfo" => {
            let relationships = array(&content["relationships"])?;
            for entry in &entries {
                let item = item_for(entry)?;
                let kind = text(&item["kind"])?;
                if !["movie", "episode"].contains(&kind) {
                    continue;
                }
                let tag = if kind == "movie" {
                    "movie"
                } else {
                    "episodedetails"
                };
                let mut body = element("title", title(item))?;
                for field in ["year", "plot", "outline", "tagline", "premiered"] {
                    if !item["metadata"][field].is_null() {
                        body.push_str(&element(field, &item["metadata"][field])?);
                    }
                }
                for identity in array(&item["identities"])? {
                    let provider = text(&identity["namespace"])?.split('.').next().unwrap();
                    if ["tmdb", "imdb", "tvdb"].contains(&provider) {
                        let value = xml_text(&identity["value"])?;
                        body.push_str(&if value.is_empty() {
                            format!("<uniqueid type=\"{provider}\" />")
                        } else {
                            format!("<uniqueid type=\"{provider}\">{value}</uniqueid>")
                        });
                    }
                }
                if kind == "episode" {
                    let parents = |id: &Value, kind: &str| -> Vec<&Value> {
                        relationships
                            .iter()
                            .filter(|e| {
                                e["source_id"] == *id
                                    && e["kind"] == "part_of"
                                    && items
                                        .get(e["target_id"].as_str().unwrap_or(""))
                                        .is_some_and(|i| i["kind"] == kind)
                            })
                            .collect()
                    };
                    let season = parents(&item["id"], "season");
                    if season.len() != 1 || season[0]["position"].is_null() {
                        return fail("episode NFO requires one numbered season association");
                    }
                    let series = parents(&season[0]["target_id"], "series");
                    if series.len() != 1 || series[0]["position"].is_null() {
                        return fail("episode NFO requires one numbered series association");
                    }
                    body.push_str(&element("season", &series[0]["position"])?);
                    body.push_str(&element("episode", &season[0]["position"])?);
                }
                let path = text(&entry["path"])?;
                let path = format!("{}.nfo", &path[..path.len() - suffix(path).len()]);
                let payload =
                    format!("<?xml version='1.0' encoding='utf-8'?>\n<{tag}>{body}</{tag}>\n");
                if let Some((_, prior)) = artifacts.iter().find(|(p, _)| p == &path) {
                    if *prior != payload {
                        return Err(Error(format!("conflicting NFO metadata at {path}")));
                    }
                } else {
                    generated_bytes = generated_bytes
                        .checked_add(payload.len())
                        .ok_or_else(|| Error("metadata export exceeds 128 MiB".into()))?;
                    if generated_bytes > MAX_BYTES {
                        return fail("metadata export exceeds 128 MiB");
                    }
                    artifacts.push((path, payload));
                }
            }
        }
        _ => return fail("unknown export format"),
    }
    if artifacts.iter().map(|(_, v)| v.len()).sum::<usize>() > MAX_BYTES {
        return fail("metadata export exceeds 128 MiB");
    }
    Ok(
        json!({"format":format,"format_version":1,"catalog":content["catalog"]["id"],"files":artifacts.into_iter().map(|(path,content)|json!({"path":path,"content":content})).collect::<Vec<_>>()}),
    )
}
