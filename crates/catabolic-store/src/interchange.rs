// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Frozen Open Catalog manifest v1–v3 validation and lossless JSON round trips.
//! This boundary never resolves or executes embedded paths, queries, or extensions.
use crate::{Error, Result};
use catabolic_core::{encode, sha256};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const FORMAT: &str = "catabolic.catalog-manifest";
const MAX_BYTES: usize = 128 * 1024 * 1024;
const MAX_RECORDS: usize = 100000;
const BASE: &[&str] = &[
    "entries",
    "items",
    "files",
    "associations",
    "relationships",
    "recorded_links",
];
fn fail<T>(message: &str) -> Result<T> {
    Err(Error(message.into()))
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn structural<T>(path: &str, message: &str) -> Result<T> {
    Err(Error(format!(
        "invalid catalog document: {path}: {message}"
    )))
}
fn path(base: &str, key: &str) -> String {
    if base.is_empty() {
        key.into()
    } else {
        format!("{base}.{key}")
    }
}
fn integer(value: &Value) -> bool {
    value
        .as_number()
        .is_some_and(|n| !n.to_string().contains(['.', 'e', 'E']))
}
fn bounded_number(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| {
        if integer(value) {
            Some(if value.to_string().starts_with('-') {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            })
        } else {
            None
        }
    })
}
fn schema(version: u64) -> Value {
    serde_json::from_str(match version {
        1 => include_str!("../resources/interchange/manifest-v1.schema.json"),
        2 => include_str!("../resources/interchange/manifest-v2.schema.json"),
        _ => include_str!("../resources/interchange/manifest-v3.schema.json"),
    })
    .expect("frozen interchange schema")
}
fn decimal(text: &str, signed: bool) -> bool {
    let text = if signed {
        text.strip_prefix('-').unwrap_or(text)
    } else {
        text
    };
    text == "0"
        || !text.is_empty() && !text.starts_with('0') && text.bytes().all(|c| c.is_ascii_digit())
}
fn finite_json(value: &Value) -> Result<()> {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(n) if !integer(value) && n.as_f64().is_none_or(|v| !v.is_finite()) => {
                return fail("invalid catalog document: document must contain finite JSON values");
            }
            Value::Array(v) => pending.extend(v),
            Value::Object(v) => pending.extend(v.values()),
            _ => (),
        }
    }
    Ok(())
}
fn check(
    value: &Value,
    definition: &Value,
    root: &Value,
    location: &str,
    model: Option<&str>,
) -> Result<()> {
    if let Some(reference) = definition["$ref"].as_str() {
        let name = reference.rsplit('/').next().unwrap();
        if name == "JsonValue" {
            return Ok(());
        }
        return check(value, &root["$defs"][name], root, location, Some(name));
    }
    if let Some(options) = definition["anyOf"].as_array() {
        if value.is_null() && options.iter().any(|v| v["type"] == "null") {
            return Ok(());
        }
        let choices = options
            .iter()
            .filter(|v| v["type"] != "null")
            .collect::<Vec<_>>();
        if choices.len() == 1 {
            return check(value, choices[0], root, location, None);
        }
        for option in &choices {
            if check(value, option, root, location, None).is_ok() {
                if let Some(number) = bounded_number(value) {
                    if let Some(bound) = definition
                        .get("minimum")
                        .or_else(|| definition.get("ge"))
                        .and_then(Value::as_f64)
                        && number < bound
                    {
                        return structural(
                            location,
                            &format!("Input should be greater than or equal to {bound}"),
                        );
                    }
                    if let Some(bound) = definition
                        .get("maximum")
                        .or_else(|| definition.get("le"))
                        .and_then(Value::as_f64)
                        && number > bound
                    {
                        return structural(
                            location,
                            &format!("Input should be less than or equal to {bound}"),
                        );
                    }
                }
                return Ok(());
            }
        }
        let label = match choices[0]["type"].as_str() {
            Some("number") => "float",
            Some("integer") => "int",
            Some("string") => "str",
            _ => "value",
        };
        return check(value, choices[0], root, &path(location, label), None);
    }
    if let Some(expected) = definition.get("const")
        && value != expected
    {
        let expected = if let Some(s) = expected.as_str() {
            format!("'{s}'")
        } else {
            expected.to_string()
        };
        return structural(location, &format!("Input should be {expected}"));
    }
    if let Some(choices) = definition["enum"].as_array()
        && !choices.contains(value)
    {
        let values = choices
            .iter()
            .map(|v| format!("'{}'", v.as_str().unwrap()))
            .collect::<Vec<_>>();
        let message = if values.len() == 2 {
            values.join(" or ")
        } else {
            format!(
                "{} or {}",
                values[..values.len() - 1].join(", "),
                values.last().unwrap()
            )
        };
        return structural(location, &format!("Input should be {message}"));
    }
    match definition["type"].as_str() {
        Some("object") => {
            let Some(object) = value.as_object() else {
                return structural(
                    location,
                    &model
                        .map(|m| format!("Input should be a valid dictionary or instance of {m}"))
                        .unwrap_or("Input should be a valid dictionary".into()),
                );
            };
            // The frozen required list preserves Pydantic declaration order.
            for key in array(&definition["required"]) {
                let key = key.as_str().unwrap();
                let at = path(location, key);
                let Some(value) = object.get(key) else {
                    return structural(&at, "Field required");
                };
                check(value, &definition["properties"][key], root, &at, None)?;
            }
        }
        Some("array") => {
            let values = value.as_array().ok_or_else(|| {
                Error(format!(
                    "invalid catalog document: {location}: Input should be a valid list"
                ))
            })?;
            for (index, child) in values.iter().enumerate() {
                check(
                    child,
                    &definition["items"],
                    root,
                    &path(location, &index.to_string()),
                    None,
                )?;
            }
        }
        Some("string") => {
            let Some(text) = value.as_str() else {
                return structural(location, "Input should be a valid string");
            };
            if definition["minLength"]
                .as_u64()
                .is_some_and(|n| text.chars().count() < (n as usize))
            {
                return structural(location, "String should have at least 1 character");
            }
            if let Some(pattern) = definition["pattern"].as_str() {
                let valid = match pattern {
                    "^[0-9a-f]{64}$" => {
                        text.len() == 64
                            && text
                                .bytes()
                                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                    }
                    "^(0|[1-9][0-9]*)$" => decimal(text, false),
                    "^-?(0|[1-9][0-9]*)$" => decimal(text, true),
                    _ => {
                        return Err(Error(format!(
                            "unsupported frozen schema pattern: {pattern}"
                        )));
                    }
                };
                if !valid {
                    return structural(
                        location,
                        &format!("String should match pattern '{pattern}'"),
                    );
                }
            }
        }
        Some("integer") => {
            if !integer(value) {
                return structural(location, "Input should be a valid integer");
            }
            if let Some(bound) = definition["exclusiveMinimum"].as_f64()
                && bounded_number(value).is_none_or(|n| n <= bound)
            {
                return structural(location, &format!("Input should be greater than {bound}"));
            }
            if let Some(bound) = definition
                .get("minimum")
                .or_else(|| definition.get("ge"))
                .and_then(Value::as_f64)
                && bounded_number(value).is_none_or(|n| n < bound)
            {
                return structural(
                    location,
                    &format!("Input should be greater than or equal to {bound}"),
                );
            }
            if let Some(bound) = definition
                .get("maximum")
                .or_else(|| definition.get("le"))
                .and_then(Value::as_f64)
                && bounded_number(value).is_none_or(|n| n > bound)
            {
                return structural(
                    location,
                    &format!("Input should be less than or equal to {bound}"),
                );
            }
        }
        Some("number") => {
            let Some(number) = value.as_f64() else {
                return structural(location, "Input should be a valid number");
            };
            if let Some(bound) = definition
                .get("minimum")
                .or_else(|| definition.get("ge"))
                .and_then(Value::as_f64)
                && number < bound
            {
                return structural(
                    location,
                    &format!("Input should be greater than or equal to {bound}"),
                );
            }
            if let Some(bound) = definition
                .get("maximum")
                .or_else(|| definition.get("le"))
                .and_then(Value::as_f64)
                && number > bound
            {
                return structural(
                    location,
                    &format!("Input should be less than or equal to {bound}"),
                );
            }
        }
        Some("boolean") if !value.is_boolean() => {
            return structural(location, "Input should be a valid boolean");
        }
        _ => (),
    }
    Ok(())
}
fn index<'a>(records: &'a Value, key: &str, label: &str) -> Result<BTreeMap<&'a str, &'a Value>> {
    let mut result = BTreeMap::new();
    for record in array(records) {
        if result
            .insert(record[key].as_str().unwrap(), record)
            .is_some()
        {
            return Err(Error(format!("duplicate {label} identifiers")));
        }
    }
    Ok(result)
}
fn text_value(value: &str, label: &str, max: usize, empty: bool) -> Result<()> {
    static OTHER: std::sync::OnceLock<Vec<[u32; 2]>> = std::sync::OnceLock::new();
    let ranges = OTHER.get_or_init(|| {
        let value: Value = serde_json::from_str(include_str!("../resources/unicode-other.json"))
            .expect("frozen Unicode categories");
        serde_json::from_value(value["ranges"].clone()).expect("Unicode ranges")
    });
    let other = |c: char| {
        let c = u32::from(c);
        let i = ranges.partition_point(|r| r[1] < c);
        ranges.get(i).is_some_and(|r| r[0] <= c)
    };
    if !empty && value.trim().is_empty() || value.chars().count() > max || value.chars().any(other)
    {
        return Err(Error(format!(
            "{label} must be text without control characters, at most {max} characters"
        )));
    }
    Ok(())
}
fn tag_name(value: &str) -> Result<String> {
    use unicode_normalization::UnicodeNormalization;
    text_value(value, "tag name", 255, false)?;
    let mut value = crate::sql::casefold(value.trim()).nfc().collect::<String>();
    if let Some((namespace, label)) = value.split_once(':') {
        let (namespace, label) = (namespace.trim(), label.trim());
        if namespace.is_empty()
            || label.is_empty()
            || !namespace.bytes().enumerate().all(|(i, c)| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || i > 0 && b"_.-".contains(&c)
            })
        {
            return fail(
                "namespaced tags require namespace:value; namespace uses letters, digits, dots, underscores or hyphens",
            );
        }
        value = format!("{namespace}:{label}");
    }
    text_value(&value, "tag name", 255, false)?;
    Ok(value)
}
fn tags(
    content: &Value,
    items: &BTreeMap<&str, &Value>,
    files: &BTreeMap<&str, &Value>,
) -> Result<()> {
    let tags = index(&content["tags"], "id", "tag")?;
    index(&content["tags"], "name", "canonical tag name")?;
    let names = index(&content["tag_names"], "name", "tag name")?;
    index(&content["taggings"], "id", "tagging")?;
    for row in array(&content["tag_names"]) {
        let name = row["name"].as_str().unwrap();
        if !tags.contains_key(row["tag_id"].as_str().unwrap()) || tag_name(name)? != name {
            return fail("tag name must be normalized and reference an included tag");
        }
    }
    for row in array(&content["tags"]) {
        let name = row["name"].as_str().unwrap();
        if names.get(name).is_none_or(|v| v["tag_id"] != row["id"]) {
            return fail("canonical tag name must resolve to its included tag");
        }
        text_value(
            row["description"].as_str().unwrap(),
            "tag description",
            4000,
            true,
        )?;
    }
    let mut edges = BTreeSet::new();
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut pending: BTreeMap<&str, usize> = tags.keys().map(|k| (*k, 0)).collect();
    for row in array(&content["tag_parents"]) {
        let pair = (
            row["child_id"].as_str().unwrap(),
            row["parent_id"].as_str().unwrap(),
        );
        if !tags.contains_key(pair.0) || !tags.contains_key(pair.1) || !edges.insert(pair) {
            return fail("invalid or duplicate tag parent edge");
        }
        *pending.get_mut(pair.0).unwrap() += 1;
        children.entry(pair.1).or_default().push(pair.0);
    }
    let mut ready = pending
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(k, _)| *k)
        .collect::<Vec<_>>();
    let mut visited = 0;
    while let Some(id) = ready.pop() {
        visited += 1;
        for child in children.get(id).into_iter().flatten() {
            let count = pending.get_mut(child).unwrap();
            *count -= 1;
            if *count == 0 {
                ready.push(child);
            }
        }
    }
    if visited != tags.len() {
        return fail("tag hierarchy contains a cycle");
    }
    let mut assignments = BTreeSet::new();
    for row in array(&content["taggings"]) {
        let kind = row["subject_type"].as_str().unwrap();
        let id = row["subject_id"].as_str().unwrap();
        let tag = row["tag_id"].as_str().unwrap();
        let source = row["source"].as_str().unwrap();
        let subjects = if kind == "item" { items } else { files };
        if !subjects.contains_key(id)
            || !tags.contains_key(tag)
            || !assignments.insert((kind, id, tag, source))
        {
            return fail("invalid or duplicate tag assignment");
        }
        text_value(source, "tag source", 200, false)?;
        text_value(row["note"].as_str().unwrap(), "tag note", 4000, true)?;
    }
    Ok(())
}
pub fn validate(value: &Value) -> Result<Value> {
    let version = value["format_version"]
        .as_u64()
        .filter(|n| (1..=3).contains(n));
    if !value.is_object() || value["format"] != FORMAT || version.is_none() {
        return fail("unsupported catalog format or version; no conversion attempted");
    }
    let version = version.unwrap();
    let mut collections = BASE.to_vec();
    if version >= 2 {
        collections.extend(["tags", "tag_names", "tag_parents", "taggings"]);
    }
    if version == 3 {
        collections.extend(["hardlinks", "retained_hardlinks"]);
    }
    let content = &value["content"];
    for key in &collections {
        if content[*key]
            .as_array()
            .is_some_and(|a| a.len() > MAX_RECORDS)
        {
            return Err(Error(format!(
                "catalog exceeds {MAX_RECORDS} {key}; partition the export"
            )));
        }
    }
    finite_json(value)?;
    let schema = schema(version);
    check(value, &schema, &schema, "", Some("Document"))?;
    if sha256(encode(content)) != value["content_sha256"].as_str().unwrap() {
        return fail("catalog content checksum mismatch");
    }
    if content["filesystem_verified"] == true {
        return fail("this profile cannot claim live filesystem verification");
    }
    for key in &collections {
        if content["counts"][*key].as_u64() != Some(array(&content[*key]).len() as u64) {
            return Err(Error(format!("catalog count mismatch: {key}")));
        }
    }
    let items = index(&content["items"], "id", "item")?;
    let files = index(&content["files"], "id", "file")?;
    let associations = index(&content["associations"], "id", "association")?;
    index(&content["entries"], "mapping_id", "mapping")?;
    index(&content["entries"], "path", "destination")?;
    index(&content["relationships"], "id", "relationship")?;
    index(&content["recorded_links"], "path", "recorded link")?;
    let mut identities = BTreeSet::new();
    for item in array(&content["items"]) {
        for identity in array(&item["identities"]) {
            if !identities.insert((
                identity["namespace"].as_str().unwrap(),
                identity["value"].as_str().unwrap(),
            )) {
                return fail("duplicate provider identity");
            }
        }
    }
    let mut pairs: BTreeMap<(&str, &str), BTreeSet<&str>> = BTreeMap::new();
    for row in array(&content["associations"]) {
        let pair = (
            row["item_id"].as_str().unwrap(),
            row["file_id"].as_str().unwrap(),
        );
        if row["active"] != true || !items.contains_key(pair.0) || !files.contains_key(pair.1) {
            return fail("association must be active and reference included items/files");
        }
        pairs
            .entry(pair)
            .or_default()
            .insert(row["id"].as_str().unwrap());
    }
    let mut edges = BTreeSet::new();
    for row in array(&content["relationships"]) {
        let (source, target) = (
            row["source_id"].as_str().unwrap(),
            row["target_id"].as_str().unwrap(),
        );
        if row["active"] != true
            || !items.contains_key(source)
            || !items.contains_key(target)
            || source == target
        {
            return fail("relationship must be active and reference distinct included items");
        }
        if !edges.insert((source, target, row["kind"].as_str().unwrap())) {
            return fail("duplicate directed relationship");
        }
    }
    for entry in array(&content["entries"]) {
        let pair = (
            entry["item_id"].as_str().unwrap(),
            entry["file_id"].as_str().unwrap(),
        );
        if !items.contains_key(pair.0) || !files.contains_key(pair.1) {
            return fail("entry references an absent item/file");
        }
        let ids = array(&entry["association_ids"])
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<BTreeSet<_>>();
        if ids.len() != array(&entry["association_ids"]).len()
            || ids.is_empty()
            || pairs.get(&pair) != Some(&ids)
            || ids.iter().any(|id| !associations.contains_key(id))
        {
            return fail("entry association IDs must match its included item/file identifications");
        }
        if !entry["managed_by_layout"].is_null()
            && (content["layout"].is_null()
                || entry["managed_by_layout"] != content["layout"]["name"])
        {
            return fail("entry references an absent managing layout");
        }
        let matches = if entry["expected_link_target"].is_null() {
            Value::Null
        } else {
            json!(entry["recorded_link_target"] == entry["expected_link_target"])
        };
        if entry["recorded_link_matches_desired"] != matches {
            return fail("entry link comparison does not match recorded targets");
        }
    }
    let layout = &content["layout"];
    if !layout.is_null() {
        if layout["current_definition_sha256"] != sha256(encode(&layout["current_definition"])) {
            return fail("layout definition checksum mismatch");
        }
        if layout["definition_matches_last_apply"]
            != json!(
                layout["current_definition_sha256"] == layout["last_applied_definition_sha256"]
            )
        {
            return fail("layout definition comparison mismatch");
        }
    }
    if version >= 2 {
        tags(content, &items, &files)?;
    }
    if version == 3 {
        index(&content["hardlinks"], "path", "hardlink path")?;
        index(&content["retained_hardlinks"], "id", "retirement")?;
        index(&content["retained_hardlinks"], "path", "retained path")?;
        if content["catalog"]["link_mode"] == "symlink"
            && (!array(&content["hardlinks"]).is_empty()
                || !array(&content["retained_hardlinks"]).is_empty())
        {
            return fail("symlink catalog cannot claim hardlink ownership");
        }
        if content["catalog"]["link_mode"] == "hardlink"
            && (!array(&content["recorded_links"]).is_empty()
                || array(&content["entries"]).iter().any(|v| {
                    [
                        "expected_link_target",
                        "recorded_link_target",
                        "recorded_link_matches_desired",
                    ]
                    .iter()
                    .any(|k| !v[*k].is_null())
                }))
        {
            return fail("hardlink catalog cannot claim symlink targets");
        }
    }
    Ok(value.clone())
}

pub fn decode(raw: &str) -> Result<Value> {
    if raw.len() > MAX_BYTES {
        return fail("catalog input exceeds 128 MiB");
    }
    let value = catabolic_core::json::parse(raw, true).map_err(|error| {
        Error(if error.0 == "invalid JSON" {
            "invalid catalog JSON".into()
        } else {
            error.0
        })
    })?;
    validate(&value)
}
pub fn encode_document(value: &Value) -> Result<String> {
    validate(value)?;
    let raw = encode(value) + "\n";
    if raw.len() > MAX_BYTES {
        return fail("catalog output exceeds 128 MiB");
    }
    Ok(raw)
}
pub fn dispatch(action: &str, args: &Value) -> Result<Value> {
    match action {
        "validate" => validate(&args["value"]),
        "decode" => decode(
            args["raw"]
                .as_str()
                .ok_or_else(|| Error("raw must be text".into()))?,
        ),
        "encode" => Ok(json!({"encoded":encode_document(&args["value"])?})),
        "schema" => {
            let version = args
                .get("version")
                .unwrap_or(&json!(3))
                .as_u64()
                .filter(|n| (1..=3).contains(n))
                .ok_or_else(|| Error("unsupported catalog format version".into()))?;
            Ok(schema(version))
        }
        _ => Err(Error(format!("unknown interchange action: {action}"))),
    }
}
