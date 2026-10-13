// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Output-only ordering for the preserved `spec roundtrip` command. The native
//! owner runs first and remains responsible for every validation and refusal.
//! This sidecar records container order, never replaces or converts JSON values.
use super::{dispatch, parser, presentation};
use catabolic_store::{Error, Result};
use serde_json::Value;

#[derive(Debug)]
enum Order {
    Object(Vec<(String, Order)>),
    Array(Vec<Order>),
    Scalar,
}

struct Input<'a> {
    raw: &'a [u8],
    at: usize,
}

impl Input<'_> {
    fn whitespace(&mut self) {
        while self.raw.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn take(&mut self, byte: u8) -> Result<()> {
        self.whitespace();
        if self.raw.get(self.at) != Some(&byte) {
            return Err(order_error());
        }
        self.at += 1;
        Ok(())
    }

    fn string(&mut self) -> Result<&[u8]> {
        self.whitespace();
        let start = self.at;
        self.take(b'"')?;
        let mut escaped = false;
        while let Some(&byte) = self.raw.get(self.at) {
            self.at += 1;
            if byte == b'"' && !escaped {
                return Ok(&self.raw[start..self.at]);
            }
            escaped = byte == b'\\' && !escaped;
        }
        Err(order_error())
    }

    fn order(&mut self) -> Result<Order> {
        self.whitespace();
        match self.raw.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                self.whitespace();
                let mut fields = Vec::new();
                if self.raw.get(self.at) != Some(&b'}') {
                    loop {
                        let key =
                            serde_json::from_slice(self.string()?).map_err(|_| order_error())?;
                        self.take(b':')?;
                        fields.push((key, self.order()?));
                        self.whitespace();
                        if self.raw.get(self.at) == Some(&b'}') {
                            break;
                        }
                        self.take(b',')?;
                    }
                }
                self.take(b'}')?;
                Ok(Order::Object(fields))
            }
            Some(b'[') => {
                self.at += 1;
                self.whitespace();
                let mut items = Vec::new();
                if self.raw.get(self.at) != Some(&b']') {
                    loop {
                        items.push(self.order()?);
                        self.whitespace();
                        if self.raw.get(self.at) == Some(&b']') {
                            break;
                        }
                        self.take(b',')?;
                    }
                }
                self.take(b']')?;
                Ok(Order::Array(items))
            }
            Some(b'"') => {
                self.string()?;
                Ok(Order::Scalar)
            }
            Some(_) => {
                let start = self.at;
                while self
                    .raw
                    .get(self.at)
                    .is_some_and(|c| !b" \r\n\t,]}".contains(c))
                {
                    self.at += 1;
                }
                if self.at == start {
                    return Err(order_error());
                }
                Ok(Order::Scalar)
            }
            None => Err(order_error()),
        }
    }
}

fn order_error() -> Error {
    Error("validated catalog output order could not be recovered".into())
}

// Frozen model fields are all required, and these arrays retain their declaration
// order. JSON Schema object-property maps do not. The focused oracle test checks
// this invariant against the pinned models for every v1-v3 record type.
fn model_schema<'a>(mut schema: &'a Value, root: &'a Value) -> &'a Value {
    loop {
        if let Some(reference) = schema["$ref"].as_str() {
            schema = root.pointer(reference.strip_prefix('#').unwrap()).unwrap();
        } else if let Some(record) = schema["anyOf"]
            .as_array()
            .and_then(|choices| choices.iter().find(|choice| choice.get("$ref").is_some()))
        {
            schema = record;
        } else {
            return schema;
        }
    }
}

fn render(
    value: &Value,
    order: &Order,
    schema: &Value,
    root: &Value,
    indent: Option<usize>,
    depth: usize,
) -> Result<String> {
    let schema = model_schema(schema, root);
    let wrap = |start, end, parts: Vec<String>| {
        if parts.is_empty() {
            format!("{start}{end}")
        } else if let Some(n) = indent {
            let pad = " ".repeat((depth + 1) * n);
            format!(
                "{start}\n{pad}{}\n{}{end}",
                parts.join(&format!(",\n{pad}")),
                " ".repeat(depth * n)
            )
        } else {
            format!("{start}{}{end}", parts.join(", "))
        }
    };
    match (value, order) {
        (Value::Object(object), Order::Object(fields)) if object.len() == fields.len() => {
            let mut fields = fields.iter().collect::<Vec<_>>();
            if let Some(required) = schema["required"].as_array() {
                // Stable sorting leaves all extension keys in their input order.
                fields.sort_by_key(|(key, _)| {
                    required
                        .iter()
                        .position(|v| v == key)
                        .unwrap_or(required.len())
                });
            }
            let parts = fields
                .into_iter()
                .map(|(key, order)| {
                    let value = object.get(key).ok_or_else(order_error)?;
                    Ok(format!(
                        "{}: {}",
                        catabolic_core::encode(&Value::String(key.clone())),
                        render(
                            value,
                            order,
                            &schema["properties"][key],
                            root,
                            indent,
                            depth + 1
                        )?
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(wrap('{', '}', parts))
        }
        (Value::Array(values), Order::Array(orders)) if values.len() == orders.len() => {
            let parts = values
                .iter()
                .zip(orders)
                .map(|(value, order)| {
                    render(value, order, &schema["items"], root, indent, depth + 1)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(wrap('[', ']', parts))
        }
        (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_), Order::Scalar) => {
            Ok(catabolic_core::encode(value))
        }
        _ => Err(order_error()),
    }
}

pub fn execute(args: &Value) -> Result<parser::Output> {
    let raw = dispatch::read_text(dispatch::text(args, "file"), 128 * 1024 * 1024)?;
    let value = catabolic_store::interchange::decode(&raw)?;
    // Never scan or render rejected input: the original owner controls error
    // precedence, duplicate detection, number bounds and semantic validation.
    let mut input = Input {
        raw: raw.as_bytes(),
        at: 0,
    };
    let order = input.order()?;
    input.whitespace();
    if input.at != raw.len() {
        return Err(order_error());
    }
    let version = value["format_version"].to_string();
    let schema = &parser::resource()["spec"][&version]["schema"];
    let indent = if args["machine"] == true {
        None
    } else {
        Some(2)
    };
    let body = render(&value, &order, schema, schema, indent, 0)?;
    Ok(presentation::roundtrip_success(args, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn output(raw: &str, schema: &Value, indent: Option<usize>) -> String {
        let value = catabolic_core::decode_json(raw).unwrap();
        let mut input = Input {
            raw: raw.as_bytes(),
            at: 0,
        };
        let order = input.order().unwrap();
        input.whitespace();
        assert_eq!(input.at, raw.len());
        let result = render(&value, &order, schema, schema, indent, 0).unwrap();
        // The owner scalar encoder normalizes finite float token spelling.
        assert_eq!(
            catabolic_core::encode(&catabolic_core::decode_json(&result).unwrap()),
            catabolic_core::encode(&value)
        );
        result
    }

    #[test]
    fn opaque_objects_do_not_use_report_shape_heuristics() {
        let raw = r#"{"z": {"type": "KeyboardInterrupt", "message": "opaque"}, "a": [{"next_cursor": null, "first": []}], "$serde_json::private::Number": "123"}"#;
        assert_eq!(output(raw, &Value::Null, None), raw);
    }

    #[test]
    fn model_fields_precede_stably_ordered_extensions() {
        let schema = json!({
            "required": ["a", "z"],
            "properties": {"a": {"$ref": "#/$defs/Child"}, "z": {}},
            "$defs": {"Child": {"required": ["c", "b"], "properties": {"c": {}, "b": {}}}}
        });
        assert_eq!(
            output(
                r#"{"x":1,"z":2,"y":3,"a":{"x":4,"b":5,"c":6,"a":7}}"#,
                &schema,
                None
            ),
            r#"{"a": {"c": 6, "b": 5, "x": 4, "a": 7}, "z": 2, "x": 1, "y": 3}"#
        );
    }

    #[test]
    fn escaped_keys_and_large_numbers_keep_owner_values() {
        assert_eq!(
            output(
                " \n{\"z\\\"\\\\\": [184467440737095516160, -0.0, 1e-7], \"\\u96ea\": {}}\t",
                &Value::Null,
                Some(2)
            ),
            "{\n  \"z\\\"\\\\\": [\n    184467440737095516160,\n    -0.0,\n    1e-07\n  ],\n  \"雪\": {}\n}"
        );
    }
}
