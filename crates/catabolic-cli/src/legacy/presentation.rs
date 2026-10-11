// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Legacy stdout/stderr envelopes and human-readable presentation.
use super::dispatch::text;
use super::parser::Output;
use serde_json::{Value, json};
fn py(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        Value::Number(_) => catabolic_core::encode(value),
        _ => value.to_string(),
    }
}
fn order(value: &Value) -> Vec<&str> {
    let object = value.as_object().unwrap();
    if object.len() == 2 {
        for cursor in ["next_after", "next_cursor"] {
            if object.contains_key(cursor) {
                return object
                    .keys()
                    .map(String::as_str)
                    .filter(|k| *k != cursor)
                    .chain(std::iter::once(cursor))
                    .collect();
            }
        }
    }

    let preferred: &[&str] = if object.contains_key("interface_version")
        && object.contains_key("exit_code")
    {
        &[
            "interface_version",
            "command",
            "operation",
            "profile",
            "outcome",
            "exit_code",
            "data",
        ]
    } else if object.contains_key("result_omitted") && object.contains_key("job_id") {
        &[
            "job_id",
            "attempt",
            "state",
            "error",
            "retryable",
            "retry_after",
            "created_at",
            "started_at",
            "finished_at",
            "result",
            "result_omitted",
        ]
    } else if object.contains_key("result_omitted") && object.contains_key("snapshot") {
        &[
            "id",
            "profile",
            "file_id",
            "operation",
            "state",
            "attempts",
            "error",
            "created_at",
            "finished_at",
            "recipe_id",
            "snapshot",
            "options",
            "result",
            "result_omitted",
        ]
    } else if object.contains_key("preset") && object.contains_key("output_definition_id") {
        &[
            "id",
            "name",
            "revision",
            "preset",
            "definition",
            "digest",
            "created_at",
            "output_definition_id",
            "operation_kind",
        ]
    } else if object.contains_key("revision")
        && object.contains_key("digest")
        && object.contains_key("definition")
        && !object.contains_key("profile")
    {
        &[
            "id",
            "name",
            "revision",
            "definition",
            "digest",
            "created_at",
        ]
    } else if object.contains_key("revision")
        && object.contains_key("digest")
        && object.contains_key("definition")
        && object.contains_key("profile")
    {
        if object.contains_key("definition_omitted") {
            &[
                "id",
                "profile",
                "name",
                "revision",
                "digest",
                "created_at",
                "definition",
                "definition_omitted",
            ]
        } else {
            &[
                "id",
                "profile",
                "name",
                "revision",
                "definition",
                "digest",
                "created_at",
            ]
        }
    } else if object.contains_key("association_ids")
        && object.contains_key("configured")
        && object.contains_key("catalog")
    {
        &[
            "catalog",
            "configured",
            "definition",
            "decisions",
            "blockers",
            "safe",
            "association_ids",
        ]
    } else if object.contains_key("interval_seconds") && object.contains_key("policy_id") {
        &[
            "profile",
            "catalog",
            "policy_id",
            "query_id",
            "layout",
            "generation",
            "enabled",
            "interval_seconds",
            "max_changes",
            "max_removals",
            "next_attempt",
        ]
    } else if object.contains_key("decision") && object.contains_key("entry_id") {
        &[
            "id",
            "profile",
            "catalog",
            "entry_id",
            "generation",
            "decision",
            "created_at",
        ]
    } else if object.contains_key("plan_id")
        && object.contains_key("library_id")
        && object.contains_key("items")
        && object.contains_key("results")
    {
        &[
            "plan_id",
            "applied",
            "connection",
            "library_id",
            "items",
            "results",
            "complete",
            "remaining_item_ids",
            "remaining_match_ids",
        ]
    } else if object.contains_key("rating_key") && object.contains_key("state") {
        &[
            "item_id",
            "rating_key",
            "match_id",
            "association_id",
            "event_id",
            "state",
            "error",
        ]
    } else if object.contains_key("code")
        && object.contains_key("safe_to_retry")
        && object.contains_key("recovery")
    {
        &["code", "safe_to_retry", "message", "recovery"]
    } else if object.contains_key("changes") && object.contains_key("rating_key") {
        &[
            "item_id",
            "match_id",
            "association_id",
            "rating_key",
            "kind",
            "remote",
            "changes",
        ]
    } else if object.contains_key("message") && object.contains_key("locations") {
        &["message", "locations", "path", "extensions"]
    } else if object.len() == 2 && object.contains_key("line") && object.contains_key("column") {
        &["line", "column"]
    } else if object.contains_key("file_associations") {
        &[
            "item",
            "profile",
            "occurrences",
            "next_cursor",
            "file_associations",
            "relationships",
        ]
    } else if object.contains_key("requested_status") {
        &[
            "item_id",
            "profile",
            "requested_status",
            "status",
            "revision",
            "updated_at",
            "updated_by",
        ]
    } else if object.contains_key("entries")
        && object.contains_key("items")
        && object.contains_key("files")
    {
        &[
            "entries",
            "items",
            "files",
            "associations",
            "relationships",
            "recorded_links",
            "tags",
            "tag_names",
            "tag_parents",
            "taggings",
            "hardlinks",
            "retained_hardlinks",
        ]
    } else if object.contains_key("documentation_version") {
        &[
            "documentation_version",
            "topic",
            "title",
            "summary",
            "source",
            "markdown",
            "query",
            "topics",
        ]
    } else if object.contains_key("database_id") && object.contains_key("counts") {
        &[
            "database",
            "database_id",
            "profile",
            "pending_operations",
            "counts",
            "bindings",
        ]
    } else if object.contains_key("columns") && object.contains_key("rows") {
        &[
            "interface_version",
            "profile",
            "columns",
            "rows",
            "row_count",
            "max_rows",
            "truncated",
            "truncation_reason",
            "complete",
        ]
    } else if object.contains_key("message") && object.contains_key("type") {
        &["message", "type", "code", "safe_to_retry"]
    } else if object.contains_key("locations") && object.contains_key("journal") {
        &[
            "locations",
            "catalogs",
            "files",
            "items",
            "mappings",
            "journal",
        ]
    } else if object.contains_key("kind") && object.contains_key("identities") {
        &["id", "kind", "metadata", "identities"]
    } else if object.contains_key("topic") {
        &["topic", "title", "summary", "source", "matches"]
    } else {
        &[]
    };
    let registry_key = object
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\x1f");
    if preferred.is_empty()
        && let Some(keys) = super::parser::resource()["object_orders"][&registry_key].as_array()
    {
        return keys.iter().filter_map(Value::as_str).collect();
    }
    let mut keys = preferred
        .iter()
        .copied()
        .filter(|k| object.contains_key(*k))
        .collect::<Vec<_>>();
    keys.extend(
        object
            .keys()
            .map(String::as_str)
            .filter(|k| !preferred.contains(k)),
    );
    keys
}
pub fn json_text(v: &Value, indent: Option<usize>, ascii: bool, depth: usize) -> String {
    json_text_ordered(v, indent, ascii, depth, false)
}
fn persisted_definition(v: &Value, key: &str) -> bool {
    key == "definition"
        && !v["revision"].is_null()
        && !v["digest"].is_null()
        && !v["profile"].is_null()
}
fn json_text_ordered(
    v: &Value,
    indent: Option<usize>,
    ascii: bool,
    depth: usize,
    canonical: bool,
) -> String {
    fn quoted(s: &str, ascii: bool) -> String {
        let s = serde_json::to_string(s).unwrap();
        if !ascii {
            return s;
        }
        let mut out = String::new();
        for c in s.chars() {
            if c.is_ascii() {
                out.push(c);
            } else {
                let mut units = [0; 2];
                for n in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{n:04x}"));
                }
            }
        }
        out
    }
    let sep = if let Some(indent) = indent {
        format!(",\n{}", " ".repeat((depth + 1) * indent))
    } else {
        ", ".into()
    };
    let wrap = |start: char, end: char, parts: Vec<String>| {
        if parts.is_empty() {
            format!("{start}{end}")
        } else if let Some(n) = indent {
            format!(
                "{start}\n{}{}\n{}{end}",
                " ".repeat((depth + 1) * n),
                parts.join(&sep),
                " ".repeat(depth * n)
            )
        } else {
            format!("{start}{}{end}", parts.join(&sep))
        }
    };
    match v {
        Value::String(s) => quoted(s, ascii),
        Value::Array(a) => wrap(
            '[',
            ']',
            a.iter()
                .map(|v| json_text_ordered(v, indent, ascii, depth + 1, canonical))
                .collect(),
        ),
        Value::Object(_) => wrap(
            '{',
            '}',
            (if canonical {
                v.as_object().unwrap().keys().map(String::as_str).collect()
            } else {
                order(v)
            })
            .iter()
            .map(|k| {
                format!(
                    "{}: {}",
                    quoted(k, ascii),
                    json_text_ordered(
                        &v[k],
                        indent,
                        ascii,
                        depth + 1,
                        canonical || persisted_definition(v, k)
                    )
                )
            })
            .collect(),
        ),
        Value::Number(_) => catabolic_core::encode(v),
        _ => v.to_string(),
    }
}
fn render(value: &Value, indent: usize) -> String {
    let prefix = " ".repeat(indent);
    match value {
        Value::Object(_) => order(value)
            .iter()
            .map(|key| {
                let child = &value[key];
                if child.is_object() || child.is_array() {
                    format!("{prefix}{key}:\n{}", render(child, indent + 2))
                } else {
                    format!("{prefix}{key}: {}", py(child))
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Array(values) => {
            if values.is_empty() {
                format!("{prefix}(none)")
            } else {
                values
                    .iter()
                    .map(|child| {
                        if child.is_object() || child.is_array() {
                            format!("{prefix}-\n{}", render(child, indent + 2))
                        } else {
                            format!("{prefix}- {}", py(child))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        _ => format!("{prefix}{}", py(value)),
    }
}
fn docs(value: &Value) -> String {
    if let Some(markdown) = value["markdown"].as_str() {
        return markdown.trim_end().into();
    }
    let mut lines = vec![
        if value.get("query").is_some() {
            format!("Documentation matches for: {}", text(value, "query"))
        } else {
            "Catabolic documentation".into()
        },
        String::new(),
    ];
    for entry in value["topics"].as_array().unwrap() {
        lines.push(format!(
            "{}: {} — {}",
            text(entry, "topic"),
            text(entry, "title"),
            text(entry, "summary")
        ));
        if let Some(matches) = entry["matches"].as_array() {
            for m in matches {
                lines.push(format!(
                    "  {}:{}: {}",
                    text(entry, "source"),
                    m["line"],
                    text(m, "text")
                ));
            }
        }
    }
    if value["topics"].as_array().unwrap().is_empty() {
        lines.push("No matching documentation topics.".into());
    }
    lines.extend(
        [
            "",
            "Read: catabolic docs TOPIC",
            "Search: catabolic docs --search TEXT",
            "Agent output: catabolic --json docs [TOPIC]",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    lines.join("\n")
}
fn table(v: &Value) -> String {
    let mut abbreviated = false;
    let mut display = |v: &Value| {
        if v.is_null() {
            return "NULL".to_string();
        }
        let result = json_text(v, None, true, 0);
        if result.chars().count() > 160 {
            abbreviated = true;
            format!("{}...", result.chars().take(157).collect::<String>())
        } else {
            result
        }
    };
    let columns = v["columns"].as_array().unwrap();
    let mut lines = vec![
        columns
            .iter()
            .map(&mut display)
            .collect::<Vec<_>>()
            .join(" | "),
        vec!["---"; columns.len()].join(" | "),
    ];
    for row in v["rows"].as_array().unwrap() {
        lines.push(
            row.as_array()
                .unwrap()
                .iter()
                .map(&mut display)
                .collect::<Vec<_>>()
                .join(" | "),
        );
    }
    lines.push(format!(
        "{} row(s){}",
        v["row_count"],
        if v["truncated"] == true {
            format!("; truncated ({})", text(v, "truncation_reason"))
        } else {
            String::new()
        }
    ));
    if abbreviated {
        lines.push("Long cells abbreviated; use --format json for full values.".into());
    }
    lines.join("\n")
}
pub fn exit_code(a: &Value, v: &Value) -> i32 {
    let command = text(a, "command");
    if command == "spec" {
        return if a["operation"] == "diff" && v["complete"] == false {
            3
        } else {
            0
        };
    }
    if command == "target" && a["operation"] == "import" && v["interrupted"] == true {
        return 130;
    }
    if matches!(command, "query" | "graphql")
        && v["errors"].as_array().is_some_and(|v| !v.is_empty())
    {
        return 2;
    }
    if ["safe", "healthy", "complete"]
        .iter()
        .any(|k| v[k] == false)
    {
        3
    } else {
        0
    }
}
fn envelope(a: &Value, v: Value, code: i32) -> Value {
    let operation = if !a["operation"].is_null() {
        a["operation"].clone()
    } else if a["command"] == "query"
        && ["save", "show", "list", "run", "contract"].contains(&text(a, "sql"))
    {
        a["sql"].clone()
    } else {
        Value::Null
    };
    json!({"interface_version":1,"command":a["command"],"operation":operation,"profile":a["profile"],"outcome":if code==0{"success"}else if code==3{"incomplete"}else{"error"},"exit_code":code,"data":v})
}
fn json_output(a: &Value) -> bool {
    a["json"] == true
        || [
            "graphql", "manifest", "spec", "target", "export", "consumer", "notify",
        ]
        .contains(&text(a, "command"))
        || a["format"] == "json"
}
pub fn success(a: &Value, value: Value) -> Output {
    let static_key = format!(
        "{} {}",
        text(a, "command"),
        if a["command"] == "query" {
            text(a, "sql")
        } else {
            text(a, "operation")
        }
    );
    if let Some(static_output) = super::parser::resource()["static_output"].get(&static_key) {
        if a["machine"] == true {
            let template = json_text(&envelope(a, Value::Null, 0), None, false, 0);
            return Output {
                code: 0,
                stdout: format!(
                    "{}{}{}\n",
                    template.strip_suffix("null}").unwrap(),
                    text(static_output, "compact"),
                    "}"
                ),
                stderr: String::new(),
            };
        }
        if json_output(a) {
            return Output {
                code: 0,
                stdout: format!("{}\n", text(static_output, "pretty")),
                stderr: String::new(),
            };
        }
    }

    let code = exit_code(a, &value);
    let stdout = if a["machine"] == true {
        format!("{}\n", json_text(&envelope(a, value, code), None, false, 0))
    } else if a["command"] == "rule"
        && ["preview", "apply", "run"].contains(&text(a, "operation"))
        && !json_output(a)
    {
        format!("{}\n", rule_report(&value))
    } else if a["command"] == "export" && a["raw"] == true {
        text(&value, "raw").into()
    } else if a["command"] == "docs" && a["json"] != true {
        format!("{}\n", docs(&value))
    } else if a["command"] == "spec" && a["operation"] == "docs" && a["json"] != true {
        text(&value, "markdown").into()
    } else if json_output(a) {
        format!(
            "{}\n",
            if a["command"] == "spec" && a["operation"] == "schema" {
                serde_json::to_string_pretty(&value).unwrap()
            } else {
                json_text(&value, Some(2), false, 0)
            }
        )
    } else if a["command"] == "query" && value.get("rows").is_some() {
        format!("{}\n", table(&value))
    } else {
        format!("{}\n", render(&value, 0))
    };
    Output {
        code,
        stdout,
        stderr: String::new(),
    }
}
pub fn error(a: &Value, error: catabolic_store::Error) -> Output {
    let interrupted = error.0 == "native legacy CLI interrupted"
        || error.0 == catabolic_runtime::plex_import::INTERRUPTED
        || error.0 == catabolic_runtime::plex_matching::INTERRUPTED
        || error.0 == catabolic_runtime::plex_login::INTERRUPTED
        || error.0 == catabolic_runtime::plex_metadata::INTERRUPTED;
    let code = if interrupted { 130 } else { 2 };
    let mut value = if interrupted {
        json!({"error":{"type":"KeyboardInterrupt","message":"interrupted; inspect jobs and recover pending filesystem operations"}})
    } else {
        json!({"error":{"message":error.0,"type":"CatabolicError"}})
    };
    if !interrupted
        && (matches!(text(a, "command"), "consumer" | "notify")
            || error.0.starts_with("http_operation_"))
    {
        let message = &error.0;
        let raw = message
            .strip_prefix("consumer ")
            .and_then(|s| s.split(';').next())
            .unwrap_or(message);
        if raw.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            let retry = [
                "temporarily_failed",
                "rate_limited",
                "unavailable",
                "busy",
                "publication_unhealthy",
                "publication_changed",
            ]
            .contains(&raw);
            value = json!({"error":{"message":format!("consumer {raw}; {}",if retry{"retry delivery"}else{"inspect and repair configuration"}),"type":"ConsumerError","code":raw,"safe_to_retry":retry}});
        }
    }
    if a["command"] == "projection"
        && text(a, "operation").starts_with("http-operation-")
        && error.0 == "not_found"
    {
        value["error"]["type"] = json!("AccessError");
    }
    if a["machine"] == true {
        Output {
            code,
            stdout: format!("{}\n", json_text(&envelope(a, value, code), None, true, 0)),
            stderr: String::new(),
        }
    } else {
        Output {
            code,
            stdout: String::new(),
            stderr: if interrupted {
                "interrupted; use recover if an operation is pending\n".into()
            } else if json_output(a) {
                format!("{}\n", json_text(&value, None, true, 0))
            } else {
                format!("error: {}\n", text(&value["error"], "message"))
            },
        }
    }
}

fn human_bytes(value: &Value) -> String {
    if value.is_null() {
        return "unknown".into();
    }
    let raw = value.to_string();
    let mut grouped = String::new();
    for (i, c) in raw.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let grouped = grouped.chars().rev().collect::<String>();
    format!(
        "{:.2} GiB ({grouped} bytes)",
        value.as_f64().unwrap_or(0.0) / 1073741824.0
    )
}
fn rule_report(original: &Value) -> String {
    let execution_errors = original["execution"]["errors"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let refresh = &original["execution"]["catalog_refresh"];
    let result = original.get("after").unwrap_or(original);
    let rule = &result["rule"];
    let space = &result["space"];
    let counts_order = [
        "missing",
        "queued",
        "running",
        "satisfied",
        "stale",
        "failed",
        "excluded",
        "deferred",
    ];
    let counts = result["counts"].as_object();
    let counts = counts
        .map(|c| {
            let mut keys = counts_order
                .iter()
                .copied()
                .filter(|k| c.contains_key(*k))
                .collect::<Vec<_>>();
            keys.extend(
                c.keys()
                    .map(String::as_str)
                    .filter(|k| !counts_order.contains(k)),
            );
            keys.iter()
                .map(|k| format!("{k}: {}", py(&c[*k])))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    let mut lines = vec![
        format!(
            "Rule: {} revision {} ({})",
            text(rule, "name"),
            py(&rule["revision"]),
            text(rule, "id")
        ),
        format!("Matched inputs: {}", py(&result["matched_inputs"])),
        counts,
        format!(
            "Additional output space: {}",
            human_bytes(&space["expected_bytes"])
        ),
        format!(
            "Estimated range: {} to {}",
            human_bytes(&space["low_bytes"]),
            human_bytes(&space["high_bytes"])
        ),
        format!(
            "Unknown output sizes: {}; known subtotal: {}",
            py(&space["unknown_count"]),
            human_bytes(&space["known_expected_bytes"])
        ),
        format!(
            "Existing satisfied outputs: {}",
            human_bytes(&space["already_satisfied_bytes"])
        ),
        format!(
            "Available destination space: {}",
            human_bytes(&space["available_bytes"])
        ),
        format!(
            "Planning allowance including unknown recipe limits: {}",
            human_bytes(&space["planning_bytes"])
        ),
        text(space, "note").into(),
    ];
    if let Some(batch) = result.get("batch_space") {
        lines.push(format!(
            "Selected batch allowance: {}",
            human_bytes(&batch["planning_bytes"])
        ));
        lines.push(format!(
            "Jobs queued/reused: {}; still to enqueue: {}",
            result["queued"].as_array().map_or(0, Vec::len),
            py(&result["remaining_to_enqueue"])
        ));
        for blocker in result["blockers"].as_array().into_iter().flatten() {
            lines.push(py(blocker));
        }
        for error in result["errors"].as_array().into_iter().flatten() {
            lines.push(text(error, "message").into());
        }
    }
    for row in result["matches"].as_array().into_iter().flatten() {
        lines.push(format!(
            "  {} / {}: {}; {}{}",
            text(row, "file_id"),
            text(row, "item_id"),
            text(row, "state"),
            human_bytes(&row["estimate"]["expected_bytes"]),
            if text(row, "reason").is_empty() {
                String::new()
            } else {
                format!("; {}", text(row, "reason"))
            }
        ));
        for assumption in row["estimate"]["assumptions"]
            .as_array()
            .into_iter()
            .flatten()
        {
            lines.push(format!("    {}", py(assumption)));
        }
    }
    for error in execution_errors {
        lines.push(format!("Rendering error: {}", text(error, "error")));
    }
    if refresh.is_object() {
        lines.push(format!(
            "Catalog link refresh: {}",
            if refresh["complete"] == true {
                "complete"
            } else {
                "pending retry"
            }
        ));
        for error in refresh["errors"].as_array().into_iter().flatten() {
            lines.push(format!("Link refresh: {}", text(error, "error")));
        }
    }
    lines.join("\n")
}
