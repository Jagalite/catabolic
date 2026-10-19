// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Native implementation of the frozen argparse tree. The JSON is immutable
//! development-time interface data, not executable Python or a runtime oracle.
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
#[path = "help.rs"]
mod help;

#[derive(Debug)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}
/// The package version the frozen reference reported in `--version`.
const FROZEN_REFERENCE_VERSION: &str = "0.2.0";
pub fn resource() -> &'static Value {
    static DATA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(include_str!("../../resources/legacy-cli.json"))
            .expect("frozen CLI data")
    })
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn label(action: &Value) -> String {
    let names = array(&action["option_strings"]);
    if names.is_empty() {
        text(action, "dest").into()
    } else {
        names
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("/")
    }
}
fn failure(node: &Value, message: String) -> Output {
    Output {
        code: 2,
        stdout: String::new(),
        stderr: format!(
            "{}{}: error: {message}\n",
            help::usage(node),
            text(node, "prog")
        ),
    }
}
fn repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut body = String::new();
    for c in value.chars() {
        match c {
            '\\' => body.push_str("\\\\"),
            '\n' => body.push_str("\\n"),
            '\r' => body.push_str("\\r"),
            '\t' => body.push_str("\\t"),
            _ if c == quote => {
                body.push('\\');
                body.push(c);
            }
            _ if !printable(c) => body.push_str(&if (c as u32) <= 255 {
                format!("\\x{:02x}", c as u32)
            } else if (c as u32) <= 65535 {
                format!("\\u{:04x}", c as u32)
            } else {
                format!("\\U{:08x}", c as u32)
            }),
            _ => body.push(c),
        }
    }
    format!("{quote}{body}{quote}")
}
fn printable(value: char) -> bool {
    let codepoint = value as u64;
    let ranges = array(&resource()["nonprintable_ranges"]);
    let index = ranges.partition_point(|range| range[1].as_u64().unwrap() < codepoint);
    ranges
        .get(index)
        .is_none_or(|range| range[0].as_u64().unwrap() > codepoint)
}
fn convert(node: &Value, action: &Value, raw: &str) -> Result<Value, Output> {
    let value = match text(action, "type") {
        "builtins.int" => {
            let raw_trimmed = raw.trim();
            let body = raw_trimmed.strip_prefix(['+', '-']).unwrap_or(raw_trimmed);
            let digits = body
                .chars()
                .map(|c| {
                    if c == '_' {
                        Some("_".to_owned())
                    } else {
                        resource()["decimal_digits"][c.to_string()]
                            .as_str()
                            .map(str::to_owned)
                    }
                })
                .collect::<Option<Vec<_>>>();
            let valid = digits.as_ref().is_some_and(|d| {
                !d.is_empty()
                    && d.first().unwrap() != "_"
                    && d.last().unwrap() != "_"
                    && !d.windows(2).any(|w| w[0] == "_" && w[1] == "_")
            });
            if !valid
                || digits
                    .as_ref()
                    .is_some_and(|v| v.iter().filter(|c| *c != "_").count() > 4300)
            {
                return Err(failure(
                    node,
                    format!(
                        "argument {}: invalid int value: {}",
                        label(action),
                        repr(raw)
                    ),
                ));
            }
            let cleaned = format!(
                "{}{}",
                if raw_trimmed.starts_with('-') {
                    "-"
                } else {
                    ""
                },
                digits.unwrap().join("").replace('_', "")
            );
            let negative = cleaned.starts_with('-');
            let digits = cleaned
                .trim_start_matches(['+', '-'])
                .trim_start_matches('0');
            let canonical = format!(
                "{}{}",
                if negative && !digits.is_empty() {
                    "-"
                } else {
                    ""
                },
                if digits.is_empty() { "0" } else { digits }
            );
            serde_json::from_str(&canonical).map_err(|_| {
                failure(
                    node,
                    format!(
                        "argument {}: invalid int value: {}",
                        label(action),
                        repr(raw)
                    ),
                )
            })?
        }
        "builtins.float" => {
            let number = python_float(raw).ok_or_else(|| {
                failure(
                    node,
                    format!(
                        "argument {}: invalid float value: {}",
                        label(action),
                        repr(raw)
                    ),
                )
            })?;
            // argparse accepts these values; production owners still validate
            // finite typed budgets before any mutation or dispatch.
            Value::Number(serde_json::Number::from_f64(number).unwrap_or_else(|| {
                serde_json::Number::from_string_unchecked(if number.is_nan() {
                    "NaN".into()
                } else if number.is_sign_negative() {
                    "-Infinity".into()
                } else {
                    "Infinity".into()
                })
            }))
        }
        _ => json!(raw),
    };
    if !action["choices"].is_null() && !array(&action["choices"]).contains(&value) {
        let show = |v: &Value| {
            if let Some(s) = v.as_str() {
                repr(s)
            } else {
                repr(&v.to_string())
            }
        };
        return Err(failure(
            node,
            format!(
                "argument {}: invalid choice: {} (choose from {})",
                label(action),
                show(&value),
                array(&action["choices"])
                    .iter()
                    .map(show)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    Ok(value)
}
fn looks_option(value: &str) -> bool {
    if !value.starts_with('-') || value == "-" {
        return false;
    }
    let rest = value[1..].strip_prefix('.').unwrap_or(&value[1..]);
    let negative = rest
        .chars()
        .next()
        .is_some_and(|c| resource()["decimal_digits"].get(c.to_string()).is_some());
    !negative && !value.contains(' ')
}
fn option_at(node: &Value, value: &str) -> bool {
    let key = value.split_once('=').map(|(k, _)| k).unwrap_or(value);
    array(&node["arguments"]).iter().any(|a| {
        array(&a["option_strings"])
            .iter()
            .filter_map(Value::as_str)
            .any(|name| name == key || key.starts_with("--") && name.starts_with(key))
    }) || looks_option(value)
}
fn python_float(value: &str) -> Option<f64> {
    let source = value
        .trim()
        .chars()
        .map(|c| {
            resource()["decimal_digits"][c.to_string()]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| c.to_string())
        })
        .collect::<String>();
    let chars = source.as_bytes();
    for (i, c) in chars.iter().enumerate() {
        if *c == b'_'
            && (i == 0
                || i + 1 == chars.len()
                || !chars[i - 1].is_ascii_digit()
                || !chars[i + 1].is_ascii_digit())
        {
            return None;
        }
    }
    source.replace('_', "").parse().ok()
}
fn record(
    node: &Value,
    action: &Value,
    value: Value,
    values: &mut Map<String, Value>,
    seen: &mut BTreeSet<String>,
) -> Result<(), Output> {
    let dest = text(action, "dest");
    for group in array(&node["mutually_exclusive_groups"]) {
        if array(&group["members"]).iter().any(|v| v == dest) {
            for member in array(&group["members"]) {
                let name = member.as_str().unwrap();
                if name != dest && seen.contains(name) {
                    let other = array(&node["arguments"])
                        .iter()
                        .find(|a| a["dest"] == name)
                        .unwrap();
                    return Err(failure(
                        node,
                        format!(
                            "argument {}: not allowed with argument {}",
                            label(action),
                            label(other)
                        ),
                    ));
                }
            }
        }
    }
    seen.insert(dest.into());
    if action["action"] == "_AppendAction" {
        let entry = values.entry(dest).or_insert_with(|| json!([]));
        if entry.is_null() {
            *entry = json!([]);
        }
        entry.as_array_mut().expect("append default").push(value);
    } else {
        values.insert(dest.into(), value);
    }
    Ok(())
}
fn parse_node(
    node: &Value,
    args: &[String],
    values: &mut Map<String, Value>,
    unknown: &mut Vec<String>,
) -> Result<(), Output> {
    let actions = array(&node["arguments"]);
    for action in actions {
        if action["action"] != "_SubParsersAction" && action["default"] != "==SUPPRESS==" {
            let mut default = action["default"].clone();
            if action["default_encoding"] == "home-relative-path" {
                default = json!(default.as_str().unwrap().replacen(
                    "${HOME}",
                    &std::env::var("HOME").unwrap_or_else(|_| "~".into()),
                    1
                ));
            }
            if node["id"] == "cli:<root>" {
                if action["dest"] == "db" {
                    default = std::env::var("CATABOLIC_DB")
                        .ok()
                        .map(Value::String)
                        .unwrap_or(Value::Null);
                }
                if action["dest"] == "profile" {
                    default = json!(
                        std::env::var("CATABOLIC_PROFILE").unwrap_or_else(|_| "default".into())
                    );
                }
            }
            values.insert(text(action, "dest").into(), default);
        }
    }
    if let Some(defaults) = node["parser_defaults"].as_object() {
        values.extend(defaults.clone());
    }
    let positionals = actions
        .iter()
        .filter(|a| array(&a["option_strings"]).is_empty() && a["action"] != "_SubParsersAction")
        .collect::<Vec<_>>();
    let sub = actions.iter().find(|a| a["action"] == "_SubParsersAction");
    let mut seen = BTreeSet::new();
    let mut pos = 0;
    let mut i = 0;
    let mut after_dash = false;
    let mut sub_seen = false;
    while i < args.len() {
        let token = &args[i];
        if !after_dash && token == "--" {
            if pos >= positionals.len() && sub.is_none() {
                unknown.push(token.clone());
            }
            after_dash = true;
            i += 1;
            continue;
        }
        if !after_dash && token != "--" && option_at(node, token) {
            if token.starts_with("-h") && !token.starts_with("--") && !token.starts_with("-h=") {
                return Err(Output {
                    code: 0,
                    stdout: help::help(node),
                    stderr: String::new(),
                });
            }
            let (option, attached) = token
                .split_once('=')
                .map(|(a, b)| (a, Some(b)))
                .unwrap_or((token, None));
            let exact = actions
                .iter()
                .find(|a| array(&a["option_strings"]).iter().any(|v| v == option));
            let matches = if exact.is_none() && option.starts_with("--") {
                actions
                    .iter()
                    .filter(|a| {
                        array(&a["option_strings"])
                            .iter()
                            .any(|v| v.as_str().unwrap().starts_with(option))
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };
            if matches.len() > 1 {
                let names = matches
                    .iter()
                    .flat_map(|a| array(&a["option_strings"]).iter())
                    .filter_map(Value::as_str)
                    .filter(|s| s.starts_with(option))
                    .collect::<Vec<_>>();
                return Err(failure(
                    node,
                    format!(
                        "ambiguous option: {option} could match {}",
                        names.join(", ")
                    ),
                ));
            }
            let Some(action) = exact.or_else(|| matches.first().copied()) else {
                unknown.push(token.clone());
                i += 1;
                continue;
            };
            if action["nargs"] == 0 {
                if let Some(value) = attached {
                    return Err(failure(
                        node,
                        format!(
                            "argument {}: ignored explicit argument {}",
                            label(action),
                            repr(value)
                        ),
                    ));
                }
                match text(action, "action") {
                    "_HelpAction" => {
                        return Err(Output {
                            code: 0,
                            stdout: help::help(node),
                            stderr: String::new(),
                        });
                    }
                    "_VersionAction" => {
                        let shown = text(action, "version").replace("%(prog)s", text(node, "prog"));
                        // The frozen data carries the reference package's own
                        // version; this release reports its own.
                        let shown = match shown.strip_suffix(FROZEN_REFERENCE_VERSION) {
                            Some(prefix) => format!("{prefix}{}", env!("CARGO_PKG_VERSION")),
                            None => shown,
                        };
                        return Err(Output {
                            code: 0,
                            stdout: format!("{shown}\n"),
                            stderr: String::new(),
                        });
                    }
                    _ => record(node, action, action["const"].clone(), values, &mut seen)?,
                }
                i += 1;
                continue;
            }
            let raw = if let Some(value) = attached {
                value
            } else {
                i += 1;
                if i >= args.len() || option_at(node, &args[i]) {
                    return Err(failure(
                        node,
                        format!("argument {}: expected one argument", label(action)),
                    ));
                }
                &args[i]
            };
            record(node, action, convert(node, action, raw)?, values, &mut seen)?;
            i += 1;
            continue;
        }
        if pos < positionals.len() {
            let action = positionals[pos];
            if action["nargs"] == "+" {
                let mut entries = vec![];
                while i < args.len() {
                    if !after_dash && args[i] == "--" {
                        after_dash = true;
                        i += 1;
                        continue;
                    }
                    if !after_dash && option_at(node, &args[i]) {
                        break;
                    }
                    entries.push(convert(node, action, &args[i])?);
                    i += 1;
                }
                record(node, action, Value::Array(entries), values, &mut seen)?;
            } else {
                record(
                    node,
                    action,
                    convert(node, action, token)?,
                    values,
                    &mut seen,
                )?;
                i += 1;
            }
            pos += 1;
            continue;
        }
        if let Some(sub) = sub.filter(|_| !sub_seen) {
            if !array(&sub["choices"]).iter().any(|v| v == token) {
                return Err(failure(
                    node,
                    format!(
                        "argument {}: invalid choice: {} (choose from {})",
                        text(sub, "dest"),
                        repr(token),
                        array(&sub["choices"])
                            .iter()
                            .map(|v| repr(v.as_str().unwrap()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
            values.insert(text(sub, "dest").into(), json!(token));
            let child_id = format!(
                "cli:{}{}",
                text(node, "prog")
                    .strip_prefix("catabolic")
                    .unwrap()
                    .trim_start(),
                if node["id"] == "cli:<root>" {
                    token.to_string()
                } else {
                    format!(" {token}")
                }
            );
            let child = array(&resource()["nodes"])
                .iter()
                .find(|n| n["id"] == child_id)
                .expect("frozen child parser");
            parse_node(child, &args[i + 1..], values, unknown)?;
            sub_seen = true;
            break;
        }
        unknown.push(token.clone());
        i += 1;
    }
    let mut missing = vec![];
    for action in actions {
        if action["action"] == "_SubParsersAction" {
            if action["required"] == true && !sub_seen {
                missing.push(label(action));
            }
        } else if action["required"] == true && !seen.contains(text(action, "dest")) {
            missing.push(label(action));
        }
    }
    if !missing.is_empty() {
        return Err(failure(
            node,
            format!(
                "the following arguments are required: {}",
                missing.join(", ")
            ),
        ));
    }
    for group in array(&node["mutually_exclusive_groups"]) {
        if group["required"] == true
            && !array(&group["members"])
                .iter()
                .any(|v| seen.contains(v.as_str().unwrap()))
        {
            let labels = array(&group["members"])
                .iter()
                .map(|v| label(actions.iter().find(|a| a["dest"] == *v).unwrap()))
                .collect::<Vec<_>>();
            return Err(failure(
                node,
                format!("one of the arguments {} is required", labels.join(" ")),
            ));
        }
    }
    Ok(())
}
pub fn parse(args: &[String]) -> Result<Value, Output> {
    let root = &resource()["nodes"][0];
    let mut values = Map::new();
    let mut unknown = vec![];
    parse_node(root, args, &mut values, &mut unknown)?;
    if !unknown.is_empty() {
        return Err(failure(
            root,
            format!("unrecognized arguments: {}", unknown.join(" ")),
        ));
    }
    Ok(Value::Object(values))
}
