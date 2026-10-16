// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Command-local container order for complete mapping publication events.
//! The Store aggregate owns validation/selection; the sidecar is never public
//! data and never replaces a value or causes a second catalog read.
use super::{dispatch::text, parser, presentation, signals};
use catabolic_store::{DetailedError, DetailedResult, Error, Result, Store};
use serde_json::Value;
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

#[derive(Debug)]
enum Order {
    Object(Vec<(String, Order)>),
    Array(Vec<Order>),
    Scalar,
}

struct Input<'a> {
    raw: &'a [u8],
    at: usize,
    cancel: &'a AtomicBool,
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
        interrupted(self.cancel)?;
        self.whitespace();
        match self.raw.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                self.whitespace();
                let mut fields: Vec<(String, Order)> = Vec::new();
                let mut positions: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                if self.raw.get(self.at) != Some(&b'}') {
                    loop {
                        let key: String =
                            serde_json::from_slice(self.string()?).map_err(|_| order_error())?;
                        self.take(b':')?;
                        let order = self.order()?;
                        // Python json.loads keeps the first position and the
                        // last value for repeated (including escaped) keys.
                        if let Some(&index) = positions.get(&key) {
                            fields[index].1 = order;
                        } else {
                            positions.insert(key.clone(), fields.len());
                            fields.push((key, order));
                        }
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

fn interrupted(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error("interrupted".into()))
    } else {
        Ok(())
    }
}

fn order_error() -> Error {
    Error("validated mapping event output order could not be recovered".into())
}

fn event_order(raw: &str, cancel: &AtomicBool) -> Result<Order> {
    // The owner's bounded-depth JSON parser has already validated all values.
    let mut input = Input {
        raw: raw.as_bytes(),
        at: 0,
        cancel,
    };
    let order = input.order()?;
    input.whitespace();
    if input.at != raw.len() {
        return Err(order_error());
    }
    match order {
        Order::Object(mut fields) if !fields.iter().any(|(key, _)| key == "id") => {
            fields.insert(0, ("id".into(), Order::Scalar));
            Ok(Order::Object(fields))
        }
        _ => Err(order_error()),
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Text,
    Json(Option<usize>),
}

fn render(
    value: &Value,
    order: &Order,
    mode: Mode,
    depth: usize,
    cancel: &AtomicBool,
) -> Result<String> {
    interrupted(cancel)?;
    let wrap = |start, end, parts: Vec<String>, indent: Option<usize>| {
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
    let prefix = " ".repeat(depth * 2);
    match (value, order) {
        (Value::Object(object), Order::Object(fields)) if object.len() == fields.len() => {
            let parts = fields
                .iter()
                .map(|(key, order)| {
                    interrupted(cancel)?;
                    let value = object.get(key).ok_or_else(order_error)?;
                    match mode {
                        Mode::Json(_) => Ok(format!(
                            "{}: {}",
                            catabolic_core::encode(&Value::String(key.clone())),
                            render(value, order, mode, depth + 1, cancel)?
                        )),
                        Mode::Text if value.is_object() || value.is_array() => Ok(format!(
                            "{prefix}{key}:\n{}",
                            render(value, order, mode, depth + 1, cancel)?
                        )),
                        Mode::Text => Ok(format!("{prefix}{key}: {}", scalar(value, order)?)),
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(match mode {
                Mode::Json(indent) => wrap('{', '}', parts, indent),
                Mode::Text => parts.join("\n"),
            })
        }
        (Value::Array(values), Order::Array(orders)) if values.len() == orders.len() => {
            let parts = values
                .iter()
                .zip(orders)
                .map(|(value, order)| match mode {
                    Mode::Json(_) => render(value, order, mode, depth + 1, cancel),
                    Mode::Text if value.is_object() || value.is_array() => Ok(format!(
                        "{prefix}-\n{}",
                        render(value, order, mode, depth + 1, cancel)?
                    )),
                    Mode::Text => Ok(format!("{prefix}- {}", scalar(value, order)?)),
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(match mode {
                Mode::Json(indent) => wrap('[', ']', parts, indent),
                Mode::Text if parts.is_empty() => format!("{prefix}(none)"),
                Mode::Text => parts.join("\n"),
            })
        }
        (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_), Order::Scalar) => {
            Ok(match mode {
                Mode::Json(_) => catabolic_core::encode(value),
                Mode::Text => format!("{prefix}{}", presentation::py(value)),
            })
        }
        _ => Err(order_error()),
    }
}

fn scalar(value: &Value, order: &Order) -> Result<String> {
    match (value, order) {
        (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_), Order::Scalar) => {
            Ok(presentation::py(value))
        }
        _ => Err(order_error()),
    }
}

fn present(
    args: &Value,
    value: &Value,
    subject: Option<String>,
    cancel: &AtomicBool,
) -> Result<parser::Output> {
    let Some(subject) = subject else {
        // Listings and real HTTP rows retain their ordinary presenter.
        return Ok(presentation::success(args, value.clone()));
    };
    let order = event_order(&subject, cancel)?;
    let mode = if args["machine"] == true {
        Mode::Json(None)
    } else if args["json"] == true {
        Mode::Json(Some(2))
    } else {
        Mode::Text
    };
    let body = render(value, &order, mode, 0, cancel)?;
    Ok(presentation::mapping_event_success(args, value, body))
}

// Never wait on a sidecar after an error/cancellation or channel disconnect.
// The joined supervisor has already completed before this is called.
fn completed(
    result: DetailedResult<Value>,
    sidecar: mpsc::Receiver<parser::Output>,
) -> super::failure::Result<parser::Output> {
    result.map_err(mapping_failure)?;
    sidecar.try_recv().map_err(|_| order_error().into())
}

fn mapping_failure(error: DetailedError) -> super::failure::Failure {
    // Only the owner's actual decoder failure carries this same-snapshot raw
    // subject. Matching message text or error-looking values cannot forge it.
    if let Some(raw) = error.mapping_subject_json()
        && let Err(diagnostic) = super::json_diagnostic::validate(raw)
    {
        return diagnostic.into();
    }
    // Syntax-valid decoder refusals (e.g. depth, nonfinite, surrogates) keep
    // their original native error. No accepted value is replaced or bypassed.
    error.into()
}

pub fn execute(args: &Value) -> super::failure::Result<parser::Output> {
    let (sender, sidecar) = mpsc::sync_channel(1);
    let owned = args.clone();
    let result = catabolic_http::interruptible_detailed(move |cancel| {
        let check = cancel.clone();
        let result = catabolic_store::embedding::with_interrupt_check(
            move || check.load(Ordering::Acquire),
            || {
                Ok((|| -> DetailedResult<Value> {
                    let (value, subject) =
                        catabolic_store::mapping_history::events_with_subject_detailed(
                            Store::open(Path::new(text(&owned, "db")), false, false)?,
                            text(&owned, "profile"),
                            owned["event"].as_str(),
                        )?;
                    // Render only after complete aggregate validation/selection.
                    // The same interrupt owner publishes one completed sidecar.
                    let output = present(&owned, &value, subject, &cancel)?;
                    sender.try_send(output).map_err(|_| order_error())?;
                    Ok(value)
                })())
            },
        );
        if cancel.load(Ordering::Acquire) {
            Err(Error("native legacy CLI interrupted".into()).into())
        } else {
            // The interrupt-check reset scope has finished before conversion.
            result
                .map_err(DetailedError::from)
                .and_then(|result| result)
        }
    });
    signals::finish();
    completed(
        result
            .map_err(DetailedError::from)
            .and_then(|result| result),
        sidecar,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event_order(raw: &str) -> Result<Order> {
        super::event_order(raw, &AtomicBool::new(false))
    }
    fn render(value: &Value, order: &Order, mode: Mode, depth: usize) -> Result<String> {
        super::render(value, order, mode, depth, &AtomicBool::new(false))
    }

    #[test]
    fn mapping_json_syntax_requires_actual_parse_provenance() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mapping.db");
        catabolic_store::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store.db.execute(
            "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES ('event','default','mapping_publish','info','{')", [],
        ).unwrap();
        drop(store);
        let error = catabolic_store::mapping_history::events_with_subject_detailed(
            Store::open(&path, false, false).unwrap(),
            "default",
            Some("event"),
        )
        .unwrap_err();
        let failure = mapping_failure(error);
        assert!(matches!(failure, super::super::failure::Failure::Json(_)));
        let message = failure.to_string();
        let args = json!({"command":"projection", "operation":"mapping-events", "machine":true, "profile":"default"});
        let output = presentation::legacy_error(&args, failure);
        let payload: Value = serde_json::from_str(&output.stdout).unwrap();
        assert_eq!(output.code, 2);
        assert_eq!(output.stderr, "");
        assert_eq!(payload["data"]["error"]["type"], "JSONDecodeError");
        for spoof in ["invalid JSON".to_owned(), message] {
            let output = presentation::legacy_error(&args, mapping_failure(Error(spoof).into()));
            let payload: Value = serde_json::from_str(&output.stdout).unwrap();
            assert_eq!(payload["data"]["error"]["type"], "CatabolicError");
        }
    }

    #[test]
    fn duplicate_keys_keep_first_position_and_last_container_order() {
        let raw = r#"{"z":{"gone":0},"a":true,"\u007a":{"q":1,"b":[null,{}]}}"#;
        let mut value = catabolic_core::decode_json(raw).unwrap();
        value["id"] = json!("event");
        assert_eq!(
            render(&value, &event_order(raw).unwrap(), Mode::Json(None), 0).unwrap(),
            r#"{"id": "event", "z": {"q": 1, "b": [null, {}]}, "a": true}"#
        );
    }

    #[test]
    fn sidecar_never_replaces_owner_values_or_accepts_missing_shapes() {
        let raw = r#"{"z":100,"a":[]}"#;
        let order = event_order(raw).unwrap();
        assert_eq!(
            render(
                &json!({"id":"event","z":5,"a":[]}),
                &order,
                Mode::Json(None),
                0
            )
            .unwrap(),
            r#"{"id": "event", "z": 5, "a": []}"#
        );
        assert!(
            render(
                &json!({"id":"event","z":5,"a":{}}),
                &order,
                Mode::Json(None),
                0
            )
            .is_err()
        );
        assert!(render(&json!({"id":"event","z":5}), &order, Mode::Text, 0).is_err());
    }

    #[test]
    fn container_order_scan_and_render_remain_interruptible() {
        let cancel = AtomicBool::new(true);
        let error = super::event_order(r#"{"z":[1,2],"a":3}"#, &cancel).unwrap_err();
        assert_eq!(error.0, "interrupted");
        let order = event_order(r#"{"z":[1,2],"a":3}"#).unwrap();
        let error = super::render(
            &json!({"id":"event","z":[1,2],"a":3}),
            &order,
            Mode::Json(None),
            0,
            &cancel,
        )
        .unwrap_err();
        assert_eq!(error.0, "interrupted");
    }

    #[test]
    fn channel_disconnect_and_owner_error_never_wait_or_emit_partial_data() {
        let (sender, sidecar) = mpsc::sync_channel(1);
        drop(sender);
        assert!(completed(Ok(json!({})), sidecar).is_err());
        let (sender, sidecar) = mpsc::sync_channel(1);
        sender
            .try_send(parser::Output {
                code: 0,
                stdout: "must not publish".into(),
                stderr: String::new(),
            })
            .ok()
            .unwrap();
        let error = completed(Err(Error("owner error".into()).into()), sidecar)
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "owner error");
        // A sender still alive without a message must not block either.
        let (_sender, sidecar) = mpsc::sync_channel(1);
        assert!(completed(Ok(json!({})), sidecar).is_err());
    }
}
