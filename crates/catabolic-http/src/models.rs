// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Validate against the immutable published schema, with Python strict numeric
//! types and defaults applied explicitly rather than framework coercions.
use crate::problem::{Problem, Result};
use serde_json::{Value, json};
use std::sync::OnceLock;
pub fn contract() -> &'static Value {
    static V: OnceLock<Value> = OnceLock::new();
    V.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../src/catabolic/http/releases/1.7.0/openapi.json"
        ))
        .expect("frozen HTTP1.7.0")
    })
}
type Route = (String, String, Vec<(String, String)>);
pub fn route(method: &str, path: &str) -> Option<Route> {
    for (template, methods) in contract()["paths"].as_object()? {
        let Some(op) = methods.get(method.to_ascii_lowercase()) else {
            continue;
        };
        let expected = template.split('/').collect::<Vec<_>>();
        let actual = path.split('/').collect::<Vec<_>>();
        if expected.len() != actual.len() {
            continue;
        }
        let mut params = vec![];
        let mut okay = true;
        for (e, a) in expected.iter().zip(actual) {
            if e.starts_with('{') && e.ends_with('}') {
                if a.is_empty() {
                    okay = false;
                    break;
                }
                params.push((e[1..e.len() - 1].into(), a.to_owned()));
            } else if *e != a {
                okay = false;
                break;
            }
        }
        if okay {
            return Some((template.clone(), op["operationId"].as_str()?.into(), params));
        }
    }
    None
}
pub fn decode_path(v: &str) -> Option<String> {
    let mut bytes = vec![];
    let raw = v.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            if i + 2 >= raw.len() {
                return None;
            }
            let hex = std::str::from_utf8(&raw[i + 1..i + 3]).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3
        } else {
            bytes.push(raw[i]);
            i += 1
        }
    }
    String::from_utf8(bytes).ok()
}
fn strict(value: &mut Value, schema: &Value, depth: usize) -> bool {
    if depth > 64 {
        return false;
    }
    if let Some(reference) = schema["$ref"].as_str() {
        return contract()
            .pointer(reference.strip_prefix('#').unwrap_or(""))
            .is_some_and(|s| strict(value, s, depth + 1));
    }
    if let Some(branches) = schema["oneOf"].as_array() {
        let mut selected = None;
        for schema in branches {
            let mut candidate = value.clone();
            if strict(&mut candidate, schema, depth + 1) {
                if selected.is_some() {
                    return false;
                }
                selected = Some(candidate);
            }
        }
        if let Some(candidate) = selected {
            *value = candidate;
            return true;
        }
        return false;
    }
    if let Some(branches) = schema["anyOf"].as_array() {
        for schema in branches {
            let mut candidate = value.clone();
            if strict(&mut candidate, schema, depth + 1) {
                *value = candidate;
                return true;
            }
        }
        return false;
    }
    if let Some(t) = schema["type"].as_str()
        && !match t {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        }
    {
        return false;
    }
    if let Some(expected) = schema.get("const")
        && value != expected
    {
        return false;
    }
    if let Some(allowed) = schema["enum"].as_array()
        && !allowed.contains(value)
    {
        return false;
    }
    if let Some(v) = value.as_str() {
        let len = v.chars().count() as u64;
        if schema["minLength"].as_u64().is_some_and(|n| len < n)
            || schema["maxLength"].as_u64().is_some_and(|n| len > n)
        {
            return false;
        }
    }
    if let Some(n) = value.as_f64()
        && (!n.is_finite()
            || schema["minimum"].as_f64().is_some_and(|v| n < v)
            || schema["maximum"].as_f64().is_some_and(|v| n > v))
    {
        return false;
    }
    if let Some(a) = value.as_array_mut() {
        let len = a.len() as u64;
        if schema["minItems"].as_u64().is_some_and(|n| len < n)
            || schema["maxItems"].as_u64().is_some_and(|n| len > n)
        {
            return false;
        }
        if let Some(item) = schema.get("items") {
            for v in a {
                if !strict(v, item, depth + 1) {
                    return false;
                }
            }
        }
    }
    if let Some(obj) = value.as_object_mut() {
        if let Some(required) = schema["required"].as_array()
            && required
                .iter()
                .any(|k| k.as_str().is_none_or(|k| !obj.contains_key(k)))
        {
            return false;
        }
        if let Some(props) = schema["properties"].as_object() {
            for (k, s) in props {
                if let Some(v) = obj.get_mut(k) {
                    if !strict(v, s, depth + 1) {
                        return false;
                    }
                } else if let Some(default) = s.get("default") {
                    obj.insert(k.clone(), default.clone());
                }
            }
        }
        for (k, v) in obj.iter_mut() {
            if schema["properties"].get(k).is_none() {
                if schema["additionalProperties"] == false {
                    return false;
                }
                if schema["additionalProperties"].is_object()
                    && !strict(v, &schema["additionalProperties"], depth + 1)
                {
                    return false;
                }
            }
        }
    }
    true
}
pub fn body(method: &str, template: &str, raw: &[u8]) -> Result<Value> {
    let operation = &contract()["paths"][template][method.to_ascii_lowercase()];
    let Some(schema) = operation.pointer("/requestBody/content/application~1json/schema") else {
        return Ok(json!({}));
    };
    let mut value =
        catabolic_core::decode_json_bytes(raw).map_err(|_| Problem::new("invalid_request", 422))?;
    if !strict(&mut value, schema, 0) {
        return Err(Problem::new("invalid_request", 422));
    }
    Ok(value)
}
pub fn page(query: &std::collections::BTreeMap<String, String>) -> Result<usize> {
    query
        .get("limit")
        .map(|s| {
            let s = s.trim();
            let valid = s.bytes().enumerate().all(|(i, b)| {
                b.is_ascii_digit()
                    || (i == 0 && b == b'+')
                    || b == b'.'
                    || (b == b'_'
                        && !s.contains('.')
                        && i > 0
                        && s.as_bytes()[i - 1].is_ascii_digit()
                        && s.as_bytes().get(i + 1).is_some_and(u8::is_ascii_digit))
            });
            let s = s.replace('_', "");
            let parts = s.split('.').collect::<Vec<_>>();
            let whole = parts.first().copied().unwrap_or("");
            let integer = valid
                && parts.len() <= 2
                && parts
                    .get(1)
                    .is_none_or(|v| !v.is_empty() && v.bytes().all(|b| b == b'0'));
            whole
                .parse::<usize>()
                .ok()
                .filter(|n| integer && (1..=1000).contains(n))
                .ok_or_else(|| Problem::new("invalid_request", 422))
        })
        .transpose()
        .map(|v| v.unwrap_or(100))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_decode_once_without_panicking_on_utf8_boundaries() {
        assert_eq!(decode_path("/a%2Fb"), Some("/a/b".into()));
        assert_eq!(decode_path("/a%252Fb"), Some("/a%2Fb".into()));
        for path in ["%aé", "%éa", "%", "%1", "%zz", "%ff"] {
            assert_eq!(decode_path(path), None);
        }
    }
    #[test]
    fn discriminated_watcher_plans_are_strict() {
        let mut value = json!({"kind":"query","query_id":1});
        let schema =
            &contract()["components"]["schemas"]["WatcherDefinition"]["properties"]["plan"];
        assert!(!strict(&mut value, schema, 0));
        let mut value = json!({"kind":"query","query_id":"approved"});
        assert!(strict(&mut value, schema, 0));
        let mut value = json!({"kind":"unrecognized","query_id":"approved"});
        assert!(!strict(&mut value, schema, 0));
    }
}
