// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Legacy stdout/stderr envelopes and human-readable presentation.
use super::dispatch::text;
use super::parser::Output;
use serde_json::{Value, json};
#[path = "scan_presentation.rs"]
mod scan_presentation;
use scan_presentation::Context as ScanContext;
pub(super) fn py(value: &Value) -> String {
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
    if object.contains_key("processed") && object.contains_key("notifications") {
        // The supervisor appends notification results to the owner's report.
        // Resolve the owner's existing shape before adding that final field.
        let mut base = object.clone();
        base.remove("notifications");
        return order(&Value::Object(base))
            .into_iter()
            .filter_map(|key| object.get_key_value(key).map(|(key, _)| key.as_str()))
            .chain(std::iter::once("notifications"))
            .collect();
    }
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

    if object
        .get("stage")
        .and_then(Value::as_str)
        .is_some_and(|stage| {
            matches!(
                stage,
                "preflight" | "scan" | "processing" | "rules" | "planning" | "remount"
            )
        })
    {
        let mut base = object.clone();
        base.remove("stage");
        return std::iter::once("stage")
            .chain(
                order(&Value::Object(base))
                    .into_iter()
                    .filter_map(|key| object.get_key_value(key).map(|(key, _)| key.as_str())),
            )
            .collect();
    }
    let preferred: &[&str] = if object.contains_key("plan_id")
        && object.contains_key("mapping")
        && object.contains_key("removals")
        && object.contains_key("results")
    {
        &[
            "plan_id",
            "mapping",
            "removals",
            "rows",
            "applied",
            "complete",
            "results",
            "event_id",
            "state",
            "safe_to_retry",
            "error",
        ]
    } else if object.contains_key("key")
        && object.contains_key("target")
        && object.contains_key("before")
        && object.contains_key("after")
    {
        &["key", "target", "before", "after"]
    } else if object.contains_key("identity")
        && object.contains_key("values")
        && (object.contains_key("locks") || object.contains_key("payload"))
    {
        &["identity", "values", "locks", "payload"]
    } else if object.contains_key("guid")
        && object.contains_key("paths")
        && object.contains_key("kind")
    {
        &["guid", "paths", "kind", "collections"]
    } else if object.contains_key("path")
        && object.contains_key("providers")
        && object.contains_key("kind")
    {
        &["path", "kind", "providers", "collections"]
    } else if object.contains_key("event_id")
        && object.contains_key("recorded")
        && object.contains_key("safe_to_retry")
    {
        &["event_id", "state", "recorded", "complete", "safe_to_retry"]
    } else if object.contains_key("event_id")
        && object.contains_key("state")
        && object.contains_key("key")
    {
        &["key", "event_id", "state"]
    } else if object.contains_key("event_id")
        && object.contains_key("state")
        && object.len() == 3
        && object.contains_key("complete")
    {
        &["event_id", "state", "complete"]
    } else if object.contains_key("event_id") && object.contains_key("delivery_acknowledged") {
        &["event_id", "state", "complete", "delivery_acknowledged"]
    } else if object.contains_key("id")
        && object.contains_key("owner")
        && object.contains_key("key")
        && object.contains_key("plan_id")
        && object.contains_key("state")
    {
        if object.len() == 5 {
            &["id", "owner", "key", "state", "plan_id"]
        } else if object.len() == 8
            && object.contains_key("attempts")
            && object.contains_key("error")
            && object.get("version").and_then(Value::as_f64) == Some(3.0)
        {
            &[
                "id", "owner", "key", "plan_id", "state", "attempts", "error", "version",
            ]
        } else {
            // Publication subjects are persisted by the owner with sorted keys.
            &["id"]
        }
    } else if object.len() == 2 && object.contains_key("events") && object.contains_key("truncated")
    {
        &["events", "truncated"]
    } else if object.contains_key("projection")
        && object.contains_key("layout")
        && object.contains_key("applied")
    {
        &[
            "projection",
            "layout",
            "complete",
            "safe",
            "applied",
            "reconciliation",
        ]
    } else if object.contains_key("definition_sha256")
        && object.contains_key("selection_statistics")
    {
        &[
            "layout",
            "catalog",
            "definition_sha256",
            "safe",
            "blockers",
            "changes",
            "desired_count",
            "unchanged_count",
            "skipped_associations",
            "selection_statistics",
            "publication",
            "selection",
            "applied",
            "change_count",
            "blocker_count",
            "details_truncated",
        ]
    } else if object.contains_key("action_count") && object.contains_key("blocked_sources_count") {
        &[
            "safe",
            "removal_budget",
            "actions",
            "blockers",
            "warnings",
            "hardlink_catalogs",
            "action_count",
            "actions_truncated",
            "blocked_sources_count",
            "blocked_sources",
        ]
    } else if object.contains_key("owned_entries") && object.contains_key("max_removal_percent") {
        &[
            "removals",
            "owned_entries",
            "percent",
            "max_removals",
            "max_removal_percent",
        ]
    } else if object.contains_key("action")
        && object.contains_key("file_id")
        && object.contains_key("catalog")
    {
        &[
            "action", "id", "catalog", "file_id", "item_id", "path", "active",
        ]
    } else if object.contains_key("byte_scope") && object.contains_key("known_referenced_bytes") {
        &[
            "scope",
            "query",
            "items",
            "associations",
            "files",
            "known_referenced_bytes",
            "unknown_size_files",
            "availability",
            "oldest_observation_at",
            "newest_observation_at",
            "unobserved_files",
            "byte_scope",
        ]
    } else if object.contains_key("selected_ids") && object.contains_key("language") {
        &[
            "language",
            "profile",
            "entity",
            "selected_ids",
            "returned_rows",
            "pages",
            "complete",
            "query_id",
            "selected_associations",
        ]
    } else if object.contains_key("configured")
        && object.contains_key("admitted")
        && object.contains_key("excluded")
    {
        &[
            "configured",
            "definition",
            "admitted",
            "excluded",
            "excluded_count",
            "details_truncated",
        ]
    } else if object.contains_key("kind")
        && object.contains_key("previous")
        && object.contains_key("target")
        && object.contains_key("catalog")
    {
        &["kind", "catalog", "path", "target", "previous", "reason"]
    } else if object.contains_key("report_version") && object.contains_key("inventory_updated") {
        &[
            "report_version",
            "database",
            "profile",
            "catalogs",
            "complete",
            "inventory_updated",
            "stages",
            "errors",
            "stopped_at",
            "new_files",
            "summary",
            "warnings",
        ]
    } else if object.contains_key("scope")
        && object.contains_key("inspect")
        && object.contains_key("outputs")
    {
        &[
            "scope",
            "items",
            "files",
            "jobs",
            "proposals",
            "inspect",
            "rules",
            "outputs",
        ]
    } else if object.len() == 6
        && object.contains_key("needs_attention")
        && object.contains_key("in_progress")
    {
        &catabolic_store::maintenance::STATUSES
    } else if object.len() == 3
        && object.contains_key("present")
        && object.contains_key("missing")
        && object.contains_key("unknown")
    {
        &["present", "missing", "unknown"]
    } else if object.contains_key("waiting_for_stability") {
        &[
            "operation",
            "queued",
            "cached",
            "waiting_for_stability",
            "pending_beyond_batch",
            "cached_unsuccessful_jobs",
            "error_count",
            "errors",
            "processing",
            "complete",
            "safe_to_continue",
        ]
    } else if object.contains_key("candidates") && object.contains_key("provider_page") {
        &["candidates", "complete", "provider_page", "attribution"]
    } else if object.contains_key("payload")
        && object.contains_key("evidence")
        && object.contains_key("file_id")
        && !object.contains_key("snapshot")
    {
        &["file_id", "payload", "evidence", "proposal"]
    } else if object.contains_key("ranking_method") && object.contains_key("title_word_overlap") {
        &[
            "provider",
            "url",
            "retrieved_at",
            "query",
            "title_word_overlap",
            "year_matches",
            "ranking_method",
        ]
    } else if object.contains_key("kind")
        && object.contains_key("identities")
        && object.contains_key("metadata")
        && !object.contains_key("id")
    {
        &["kind", "identities", "metadata"]
    } else if object.len() == 3
        && object.contains_key("processed")
        && object.contains_key("complete")
        && object.contains_key("projections")
    {
        &["processed", "complete", "projections"]
    } else if object.len() == 3
        && object.contains_key("catalog")
        && object.contains_key("complete")
        && (object.contains_key("states") || object.contains_key("error"))
    {
        &["catalog", "complete", "states", "error"]
    } else if object.len() == 3
        && object.contains_key("processed")
        && object.contains_key("complete")
        && object.contains_key("blockers")
    {
        &["processed", "complete", "blockers"]
    } else if object.len() == 3
        && object.contains_key("token_id")
        && object.contains_key("expires")
        && object.contains_key("credential_file")
    {
        &["token_id", "expires", "credential_file"]
    } else if object.len() == 4
        && object.contains_key("id")
        && object.contains_key("principal")
        && object.contains_key("expires")
        && object.contains_key("revoked")
    {
        &["id", "principal", "expires", "revoked"]
    } else if object.contains_key("interface_version") && object.contains_key("exit_code") {
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
    } else if object.contains_key("target")
        && object.contains_key("lease_token")
        && object.contains_key("catalog")
    {
        &[
            "id",
            "profile",
            "catalog",
            "target",
            "state",
            "attempts",
            "error",
            "created_at",
            "finished_at",
            "lease_token",
            "lease_until",
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
        && object.contains_key("operation_digest")
        && object.contains_key("delivery_acknowledged")
    {
        &[
            "database_id",
            "profile",
            "definition",
            "operation_digest",
            "rows",
            "plan_id",
            "mapping",
            "complete",
            "applied",
            "queued",
            "delivery_acknowledged",
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
    } else if object.contains_key("group_id")
        && object.contains_key("state")
        && object.contains_key("complete")
    {
        &[
            "group_id",
            "state",
            "complete",
            "error",
            "scan_request_accepted",
            "indexing_verified",
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
    } else if object.get("type").and_then(Value::as_str) == Some("KeyboardInterrupt")
        && object.contains_key("message")
    {
        &["type", "message"]
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
    json_text_ordered(v, indent, ascii, depth, false, false, ScanContext::Other)
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
    id_first: bool,
    scan: ScanContext,
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
                .map(|v| {
                    json_text_ordered(
                        v,
                        indent,
                        ascii,
                        depth + 1,
                        canonical,
                        false,
                        scan.element(v),
                    )
                })
                .collect(),
        ),
        Value::Object(_) => wrap(
            '{',
            '}',
            scan.keys(v)
                .unwrap_or_else(|| ordered_keys(v, canonical, id_first))
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
                            canonical || persisted_definition(v, k),
                            false,
                            scan.field(v, k),
                        )
                    )
                })
                .collect(),
        ),
        Value::Number(_) => scan
            .number(v)
            .map(str::to_owned)
            .unwrap_or_else(|| catabolic_core::encode(v)),
        _ => v.to_string(),
    }
}
fn ordered_keys(value: &Value, canonical: bool, id_first: bool) -> Vec<&str> {
    if canonical {
        let keys = value.as_object().unwrap().keys().map(String::as_str);
        if id_first {
            std::iter::once("id")
                .chain(keys.filter(|key| *key != "id"))
                .collect()
        } else {
            keys.collect()
        }
    } else {
        order(value)
    }
}
fn render(value: &Value, indent: usize) -> String {
    render_ordered(value, indent, false, false, ScanContext::Other)
}
fn render_ordered(
    value: &Value,
    indent: usize,
    canonical: bool,
    id_first: bool,
    scan: ScanContext,
) -> String {
    let prefix = " ".repeat(indent);
    match value {
        Value::Object(_) => scan
            .keys(value)
            .unwrap_or_else(|| ordered_keys(value, canonical, id_first))
            .iter()
            .map(|key| {
                let child = &value[key];
                if child.is_object() || child.is_array() {
                    format!(
                        "{prefix}{key}:\n{}",
                        render_ordered(child, indent + 2, canonical, false, scan.field(value, key))
                    )
                } else {
                    format!(
                        "{prefix}{key}: {}",
                        scan.field(value, key)
                            .number(child)
                            .map(str::to_owned)
                            .unwrap_or_else(|| py(child))
                    )
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
                            format!(
                                "{prefix}-\n{}",
                                render_ordered(
                                    child,
                                    indent + 2,
                                    canonical,
                                    false,
                                    scan.element(child)
                                )
                            )
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
/// A watch cycle is a known command report, including its stderr progress copy.
pub fn scan_watch_cycle_text(report: &Value) -> String {
    json_text_ordered(report, None, true, 0, false, false, ScanContext::Cycle)
}
pub fn consumer_watch_line(a: &Value, report: Value) -> String {
    let code = if report["complete"] == true { 0 } else { 3 };
    let value = if a["machine"] == true {
        envelope(a, report, code)
    } else {
        report
    };
    json_text(&value, None, true, 0)
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
    let scan = ScanContext::command(a);
    let stdout = if a["machine"] == true {
        let body = json_text_ordered(&value, None, false, 0, false, false, scan);
        let template = json_text(&envelope(a, Value::Null, code), None, false, 0);
        format!("{}{body}}}\n", template.strip_suffix("null}").unwrap())
    } else if a["command"] == "maintenance" && !json_output(a) {
        format!("{}\n", maintenance_report(&value))
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
                json_text_ordered(&value, Some(2), false, 0, false, false, scan)
            }
        )
    } else if a["command"] == "query" && value.get("rows").is_some() {
        format!("{}\n", table(&value))
    } else {
        format!(
            "{}\n",
            if scan == ScanContext::Other {
                render(&value, 0)
            } else {
                render_ordered(&value, 0, false, false, scan)
            }
        )
    };
    Output {
        code,
        stdout,
        stderr: String::new(),
    }
}
/// Complete mapping event body ordered from the owner's same-snapshot subject.
/// Envelope fields and exit status remain controlled by the ordinary presenter.
pub fn mapping_event_success(a: &Value, value: &Value, body: String) -> Output {
    let code = exit_code(a, value);
    let stdout = if a["machine"] == true {
        let template = json_text(&envelope(a, Value::Null, code), None, false, 0);
        format!("{}{body}}}\n", template.strip_suffix("null}").unwrap())
    } else {
        format!("{body}\n")
    };
    Output {
        code,
        stdout,
        stderr: String::new(),
    }
}

/// A validated spec roundtrip has a command-local ordered JSON body. Keep the
/// ordinary envelope here, rather than letting document keys influence it.
pub fn roundtrip_success(a: &Value, body: String) -> Output {
    let stdout = if a["machine"] == true {
        let template = json_text(&envelope(a, Value::Null, 0), None, false, 0);
        format!("{}{body}}}\n", template.strip_suffix("null}").unwrap())
    } else {
        format!("{body}\n")
    };
    Output {
        code: 0,
        stdout,
        stderr: String::new(),
    }
}
/// Preserve failure identity from command admission, never from message text.
pub fn legacy_error(a: &Value, error: super::failure::Failure) -> Output {
    match error {
        super::failure::Failure::Detailed(error) => admin_error(a, error),
        super::failure::Failure::Boundary(error) => boundary_error(a, error),
        super::failure::Failure::Variable(error) => python_error(
            a,
            json!({"type": error.kind(), "message": error.to_string()}),
        ),
        super::failure::Failure::Json(error) => python_error(
            a,
            json!({
                "type": "JSONDecodeError", "message": error.to_string()
            }),
        ),
    }
}
/// Detailed catalog failures retain source-selected class without altering
/// ordinary administrative, interruption or consumer presentation.
pub fn boundary_error(a: &Value, error: catabolic_store::DetailedError) -> Output {
    if let Some((class, message)) = error.mapping_runtime_diagnostic() {
        return python_error_with_code(a, json!({"type": class, "message": message}), 1);
    }
    match error.class() {
        catabolic_store::ErrorClass::CatabolicError => self::error(a, error.into_error()),
        catabolic_store::ErrorClass::TypeError => python_error(
            a,
            json!({"type": error.class().as_str(), "message": error.to_string()}),
        ),
    }
}
pub fn admin_error(a: &Value, error: catabolic_store::access_admin::Failure) -> Output {
    match error {
        catabolic_store::access_admin::Failure::Access(error) => {
            error_with_access(a, catabolic_store::Error(error.code().into()), true)
        }
        catabolic_store::access_admin::Failure::Native(error) => self::error(a, error),
        catabolic_store::access_admin::Failure::Boundary(error) => boundary_error(a, error),
    }
}
/// Render a Python text-stream failure using the preserved CLI envelope owner.
pub fn python_error(a: &Value, error: Value) -> Output {
    python_error_with_code(a, error, 2)
}

fn python_error_with_code(a: &Value, error: Value, code: i32) -> Output {
    let value = json!({"error":error});
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
            stderr: if json_output(a) {
                format!("{}\n", json_text(&value, None, true, 0))
            } else {
                format!("error: {}\n", text(&value["error"], "message"))
            },
        }
    }
}
pub fn error(a: &Value, error: catabolic_store::Error) -> Output {
    error_with_access(a, error, false)
}
fn error_with_access(a: &Value, error: catabolic_store::Error, access: bool) -> Output {
    let interrupted = error.0 == "native legacy CLI interrupted"
        || error.0 == catabolic_runtime::maintenance::INTERRUPTED
        || error.0 == catabolic_runtime::plex_import::INTERRUPTED
        || error.0 == catabolic_runtime::plex_matching::INTERRUPTED
        || error.0 == catabolic_runtime::plex_login::INTERRUPTED
        || error.0 == catabolic_runtime::plex_metadata::INTERRUPTED
        || error.0 == catabolic_runtime::destination_mappings::INTERRUPTED;
    let code = if interrupted { 130 } else { 2 };
    let mut value = if interrupted {
        json!({"error":{"type":"KeyboardInterrupt","message":"interrupted; inspect jobs and recover pending filesystem operations"}})
    } else {
        json!({"error":{"message":error.0,"type":"CatabolicError"}})
    };
    // These adapters preserve terminal writer-contention provenance.
    // Catalog contention handled by the API worker remains a normal report.
    if (a["command"] == "identify"
        || a["command"] == "watch"
        || (a["command"] == "api" && a["operation"] == "worker")
        || (a["command"] == "fallback" && a["operation"] == "worker")
        || (a["command"] == "consumer" && a["operation"] == "watch")
        || (a["command"] == "projection"
            && matches!(
                text(a, "operation"),
                "mapping-apply" | "mapping-recover" | "preview"
            )))
        && error.0 == "another Catabolic writer or upgrade is using this database"
    {
        value["error"]["type"] = json!("CatalogBusy");
    }
    if !interrupted
        && (matches!(text(a, "command"), "consumer" | "notify")
            || error.0.starts_with("http_operation_")
            || (a["command"] == "projection"
                && matches!(
                    text(a, "operation"),
                    "mapping-preview" | "mapping-apply" | "mapping-recover"
                )
                && (error.0.starts_with("mapping_")
                    || error.0.starts_with("consumer ")
                    || error.0.starts_with("metadata_invalid_")
                    || matches!(
                        error.0.as_str(),
                        "invalid_configuration"
                            | "metadata_writer_busy_preview_again"
                            | "unknown_connection"
                            | "invalid_path"
                            | "unauthorized"
                            | "temporarily_failed"
                            | "rate_limited"
                            | "unavailable"
                            | "unsupported"
                            | "response_limit"
                            | "invalid_response"
                            | "server_identity_changed"
                            | "library_identity_changed"
                    ))))
    {
        let message = &error.0;
        let raw = message
            .strip_prefix("consumer ")
            .and_then(|s| s.split(';').next())
            .unwrap_or(message);
        if raw.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            || (raw.starts_with("mapping_unresolved_write_recover_")
                && raw
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')))
        {
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
    if access {
        value["error"]["type"] = json!("AccessError");
    }
    if a["command"] == "projection"
        && (text(a, "operation").starts_with("http-operation-")
            || matches!(text(a, "operation"), "mapping-preview" | "mapping-apply"))
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

fn maintenance_report(report: &Value) -> String {
    let summary = &report["summary"];
    let items = &summary["items"];
    let files = &summary["files"];
    let pairs = |value: &Value, keys: &[&str]| {
        keys.iter()
            .map(|key| format!("{key}: {}", py(&value[key])))
            .collect::<Vec<_>>()
            .join("; ")
    };
    let mut lines = vec![
        format!(
            "Maintenance cycle: {}",
            if report["complete"] == true {
                "complete"
            } else {
                "incomplete"
            }
        ),
        format!(
            "Database: {} (profile: {})",
            text(report, "database"),
            text(report, "profile")
        ),
        format!("New inventory files: {}", report["new_files"]),
        format!(
            "Items: {} total; {} incomplete",
            items["total"], items["incomplete"]
        ),
        format!(
            "  {}",
            pairs(&items["by_status"], &catabolic_store::maintenance::STATUSES)
        ),
        format!(
            "Files: {} total; {} uncataloged ({} present)",
            files["total"], files["uncataloged"], files["uncataloged_present"]
        ),
        format!(
            "  {}",
            pairs(
                &files["by_availability"],
                &["present", "missing", "unknown"]
            )
        ),
    ];
    for (key, label) in [("jobs", "Jobs"), ("proposals", "Proposals")] {
        let keys = summary[key]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        lines.push(format!(
            "{label}: {}",
            if keys.is_empty() {
                "none".into()
            } else {
                pairs(&summary[key], &keys)
            }
        ));
    }
    for (catalog, count) in files["unmapped_present_by_catalog"].as_object().unwrap() {
        lines.push(format!("Unmapped present files in {catalog}: {count}"));
    }
    let outputs = &summary["outputs"];
    lines.push(format!(
        "Output verification: {}",
        if outputs["checked"] != true {
            "not run"
        } else if outputs["healthy"] == true {
            "healthy"
        } else {
            "unhealthy"
        }
    ));
    for entry in outputs["catalogs"].as_array().unwrap() {
        lines.push(format!(
            "  {}: {} verified links; {} issues; {} retained",
            text(entry, "catalog"),
            entry["verified_links"],
            entry["issue_count"],
            entry["retained_count"]
        ));
    }
    for warning in report["warnings"].as_array().unwrap() {
        lines.push(format!("Warning: {}", warning.as_str().unwrap_or("")));
    }
    for stage in report["stages"].as_array().unwrap() {
        match text(stage, "stage") {
            "rules" => {
                lines.push(format!(
                    "Rules: {}",
                    pairs(&stage["backlog"], &catabolic_store::rules::STATES)
                ));
                for volume in stage["space_by_device"].as_array().unwrap() {
                    lines.push(format!(
                        "  Device {}: additional {}; range {} to {}; {} unknown; free {}",
                        volume["device"],
                        human_bytes(&volume["expected_bytes"]),
                        human_bytes(&volume["low_bytes"]),
                        human_bytes(&volume["high_bytes"]),
                        volume["unknown_count"],
                        human_bytes(&volume["available_bytes"])
                    ));
                }
            }
            "scan" => {
                for scan in stage["scans"].as_array().unwrap() {
                    let excluded = &scan["excluded"];
                    lines.push(format!(
                        "Scan exclusions for {} (source-relative): {}",
                        text(scan, "location"),
                        if excluded.is_null() {
                            "unknown".into()
                        } else if excluded.as_array().is_some_and(|a| a.is_empty()) {
                            "none".into()
                        } else {
                            catabolic_core::encode(excluded)
                        }
                    ));
                    for error in scan["errors"].as_array().unwrap() {
                        lines.push(format!(
                            "  {}: {}",
                            text(scan, "location"),
                            error.as_str().unwrap_or("")
                        ));
                    }
                }
                if !stage["scans"].as_array().unwrap().is_empty() {
                    lines.extend([
                    "Exclusions combine persistent source policy with this run's --exclude paths; each directory excludes its subtree.",
                    "For one run, use --exclude PATH. For persistent changes, preview: catabolic location scan-policy SOURCE --file POLICY.json; add --apply to save.",
                    "The policy file's exclusions list replaces the saved list; retain existing exclusions. Rerun maintenance after changes to create a new scan request.",
                    "Exclusions do not delete files. Use --json for the effective exclusions and coverage recorded in each scan request.",
                ].into_iter().map(str::to_owned));
                }
            }
            "planning" => {
                for layout in stage["layouts"].as_array().unwrap() {
                    for blocker in layout["blockers"].as_array().unwrap() {
                        lines.push(format!("  {}", text(blocker, "reason")));
                    }
                }
            }
            "processing" => {
                lines.push(format!("Analysis: {} processed; {} settling; {} beyond batch; {} cached unsuccessful jobs",stage["processing"]["processed"],stage["waiting_for_stability"],stage["pending_beyond_batch"],stage["cached_unsuccessful_jobs"]));
                for entry in stage["errors"].as_array().unwrap() {
                    lines.push(format!("  {}", text(entry, "error")));
                }
            }
            _ => {}
        }
    }
    for entry in report["errors"].as_array().unwrap() {
        lines.push(format!(
            "Stopped at {}: {}",
            text(entry, "stage"),
            text(entry, "message")
        ));
    }
    lines.push(
        "Cycle success and curation completion are separate. Inspect recorded backlog with:".into(),
    );
    for command in summary["inspect"].as_array().unwrap() {
        lines.push(format!("  catabolic {}", command.as_str().unwrap()));
    }
    lines.push(
        "Use --json for stage details; keep the same database and profile when inspecting.".into(),
    );
    lines.join("\n")
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

#[cfg(test)]
mod adapter_order_tests {
    use super::*;

    #[test]
    fn consumer_watch_lines_preserve_order_ascii_and_outer_completeness() {
        let args =
            json!({"command":"consumer","operation":"watch","profile":"café","machine":true});
        let report = json!({"deliveries":[],"processed":0,"unresolved_bindings":0,"complete":true,
            "notifications":{"deliveries":[],"unresolved":1,"unverified_publications":0,"complete":false}});
        assert_eq!(
            consumer_watch_line(&args, report),
            concat!(
                "{\"interface_version\": 1, \"command\": \"consumer\", \"operation\": \"watch\", ",
                "\"profile\": \"caf\\u00e9\", \"outcome\": \"success\", \"exit_code\": 0, ",
                "\"data\": {\"deliveries\": [], \"processed\": 0, \"unresolved_bindings\": 0, \"complete\": true, ",
                "\"notifications\": {\"deliveries\": [], \"unresolved\": 1, \"unverified_publications\": 0, \"complete\": false}}}"
            )
        );
        let report = json!({"deliveries":[{"group_id":"x","state":"stale_acknowledgement","complete":false}],"processed":1,"unresolved_bindings":1,"complete":false});
        let line = consumer_watch_line(&args, report);
        assert!(line.contains("\"outcome\": \"incomplete\", \"exit_code\": 3"));
        assert!(line.contains(
            "{\"group_id\": \"x\", \"state\": \"stale_acknowledgement\", \"complete\": false}"
        ));
    }

    #[test]
    fn worker_notifications_follow_the_existing_owner_shape() {
        let base = json!({"processed":0,"complete":true,"jobs":[]});
        let mut expected = order(&base)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        expected.push("notifications".into());
        let mut report = base.clone();
        report["notifications"] = json!({"complete":true});
        assert_eq!(order(&report), expected);
        assert_eq!(
            order(&json!({"alpha":0,"notifications":{},"zulu":1})),
            ["alpha", "notifications", "zulu"]
        );
    }

    #[test]
    fn legacy_refresh_event_columns_keep_frozen_table_order() {
        let row = json!({"id":1,"profile":"default","catalog":"global","target":{},"state":"pending","attempts":0,"error":null,"created_at":"x","finished_at":null,"lease_token":null,"lease_until":null});
        assert_eq!(
            order(&row),
            [
                "id",
                "profile",
                "catalog",
                "target",
                "state",
                "attempts",
                "error",
                "created_at",
                "finished_at",
                "lease_token",
                "lease_until"
            ]
        );
    }

    #[test]
    fn dictionary_type_provenance_reaches_cli_without_message_spoofing() {
        use catabolic_store::{Store, migration, query::CatalogQuery};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sort-type.db");
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, false, false).unwrap();
        let query = CatalogQuery::new(&store, "default").unwrap();
        let message = "cannot use 'dict' as a dict key (unhashable type: 'dict')";
        for (machine, json_output) in [(false, false), (false, true), (true, false)] {
            let args = json!({"command":"items","operation":"list","profile":"default","machine":machine,"json":json_output});
            for (failure, expected_type) in [
                (
                    query
                        .execute_detailed("items", &json!({"sort":{}}))
                        .unwrap_err(),
                    "TypeError",
                ),
                (
                    catabolic_store::Error(message.into()).into(),
                    "CatabolicError",
                ),
            ] {
                let output = legacy_error(&args, failure.into());
                assert_eq!(output.code, 2);
                if machine || json_output {
                    let data = json!({"error":{"message":message,"type":expected_type}});
                    if machine {
                        assert_eq!(
                            output.stdout,
                            format!("{}\n", json_text(&envelope(&args, data, 2), None, true, 0))
                        );
                        assert_eq!(output.stderr, "");
                    } else {
                        assert_eq!(output.stdout, "");
                        assert_eq!(
                            output.stderr,
                            format!("{}\n", json_text(&data, None, true, 0))
                        );
                    }
                } else {
                    assert_eq!(output.stdout, "");
                    assert_eq!(output.stderr, format!("error: {message}\n"));
                }
            }
        }
        assert!(query.execute("items", &json!({"sort":"id"})).is_ok());
    }

    #[test]
    fn list_type_provenance_reaches_cli_without_message_spoofing() {
        use catabolic_store::{Store, migration, query::CatalogQuery};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sort-type.db");
        migration::initialize(&path).unwrap();
        let store = Store::open(&path, false, false).unwrap();
        let query = CatalogQuery::new(&store, "default").unwrap();
        let message = "cannot use 'list' as a dict key (unhashable type: 'list')";
        for (machine, json_output) in [(false, false), (false, true), (true, false)] {
            let args = json!({"command":"items","operation":"list","profile":"default","machine":machine,"json":json_output});
            for (failure, expected_type) in [
                (
                    query
                        .execute_detailed("items", &json!({"sort":[{"nested":[]}]}))
                        .unwrap_err(),
                    "TypeError",
                ),
                (
                    catabolic_store::Error(message.into()).into(),
                    "CatabolicError",
                ),
            ] {
                let output = legacy_error(&args, failure.into());
                assert_eq!(output.code, 2);
                if machine || json_output {
                    let data = json!({"error":{"message":message,"type":expected_type}});
                    if machine {
                        assert_eq!(
                            output.stdout,
                            format!("{}\n", json_text(&envelope(&args, data, 2), None, true, 0))
                        );
                        assert_eq!(output.stderr, "");
                    } else {
                        assert_eq!(output.stdout, "");
                        assert_eq!(
                            output.stderr,
                            format!("{}\n", json_text(&data, None, true, 0))
                        );
                    }
                } else {
                    assert_eq!(output.stdout, "");
                    assert_eq!(output.stderr, format!("error: {message}\n"));
                }
            }
        }
        assert!(query.execute("items", &json!({"sort":"id"})).is_ok());
    }

    #[test]
    fn worker_and_consumer_watch_refusals_preserve_catalog_busy() {
        let message = "another Catabolic writer or upgrade is using this database";
        for (command, operation, message, expected) in [
            ("api", "worker", message, "CatalogBusy"),
            ("fallback", "worker", message, "CatalogBusy"),
            ("fallback", "enable", message, "CatabolicError"),
            ("projection", "mapping-apply", message, "CatalogBusy"),
            ("projection", "mapping-recover", message, "CatalogBusy"),
            ("projection", "preview", message, "CatalogBusy"),
            ("projection", "execute", message, "CatabolicError"),
            ("projection", "mapping-preview", message, "CatabolicError"),
            ("api", "serve", message, "CatabolicError"),
            ("consumer", "worker", message, "CatabolicError"),
            ("consumer", "watch", message, "CatalogBusy"),
            ("consumer", "run", message, "CatabolicError"),
            (
                "api",
                "worker",
                "unknown profile: missing",
                "CatabolicError",
            ),
            ("api", "worker", "database is locked", "CatabolicError"),
            (
                "api",
                "worker",
                "another Catabolic writer or upgrade is using this database: detail",
                "CatabolicError",
            ),
        ] {
            let args =
                json!({"command":command,"operation":operation,"profile":"default","machine":true});
            let output = error(&args, catabolic_store::Error(message.into()));
            assert_eq!(output.code, 2);
            assert!(output.stderr.is_empty());
            let envelope: Value = serde_json::from_str(&output.stdout).unwrap();
            assert_eq!(envelope["data"]["error"]["type"], expected);
            assert_eq!(envelope["data"]["error"]["message"], message);
        }
    }

    #[test]
    fn only_keyboard_interrupt_uses_type_before_message() {
        assert_eq!(
            order(&json!({"message":"","type":"KeyboardInterrupt"})),
            ["type", "message"]
        );
        assert_eq!(
            order(&json!({"message":"failure","type":"CatabolicError"})),
            ["message", "type"]
        );
    }
    #[test]
    fn access_provenance_survives_owner_and_cli_envelopes() {
        use catabolic_store::{access_admin, admin_cli, embedding::Session, migration};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("provenance.db");
        migration::initialize(&path).unwrap();
        let mut session = Session::open(&path, true, false).unwrap();
        admin_cli::execute_detailed(
            &mut session,
            "default",
            "principal-put",
            &json!({"id":"fixture"}),
        )
        .unwrap();
        session.store().unwrap().db.execute("INSERT INTO api_tokens(id,principal,digest,grants,expires) VALUES ('fixture-token','fixture','fake-digest','[]',4102444800)", []).unwrap();
        for (operation, table, request) in [
            ("principal-put", "api_principals", json!({"id":"collision"})),
            (
                "grant-put",
                "api_audit",
                json!({"principal":"fixture","definition":{"actions":["metadata:read"]},"id":"collision"}),
            ),
            ("token-revoke", "api_audit", json!({"id":"fixture-token"})),
        ] {
            for message in [
                "invalid_grant",
                "unknown_principal",
                "principal_profile_conflict",
                "fixture failure",
            ] {
                session.store().unwrap().db.execute_batch(&format!("CREATE TRIGGER fail_provenance BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'{message}'); END")).unwrap();
                for (machine, json) in [(false, false), (false, true), (true, false)] {
                    let failure =
                        admin_cli::execute_detailed(&mut session, "default", operation, &request)
                            .unwrap_err();
                    assert!(matches!(failure, access_admin::Failure::Native(_)));
                    let args = json!({"command":"api","operation":operation,"machine":machine,"json":json});
                    let output = admin_error(&args, failure);
                    assert_eq!(output.code, 2);
                    if machine || json {
                        let payload: Value = serde_json::from_str(if machine {
                            &output.stdout
                        } else {
                            &output.stderr
                        })
                        .unwrap();
                        assert_eq!(
                            (if machine { &payload["data"] } else { &payload })["error"],
                            json!({"message":message,"type":"CatabolicError"})
                        );
                    } else {
                        assert_eq!(output.stderr, format!("error: {message}\n"));
                    }
                    let store = session.store().unwrap();
                    assert!(store.db.is_autocommit());
                    assert_eq!(
                        store
                            .db
                            .query_row(
                                "SELECT count(*) FROM api_principals WHERE id='collision'",
                                [],
                                |r| r.get::<_, i64>(0)
                            )
                            .unwrap(),
                        0
                    );
                    assert_eq!(
                        store
                            .db
                            .query_row("SELECT count(*) FROM api_grants", [], |r| r
                                .get::<_, i64>(0))
                            .unwrap(),
                        0
                    );
                    assert_eq!(
                        store
                            .db
                            .query_row(
                                "SELECT revoked FROM api_tokens WHERE id='fixture-token'",
                                [],
                                |r| r.get::<_, i64>(0)
                            )
                            .unwrap(),
                        0
                    );
                    assert_eq!(
                        store
                            .db
                            .query_row("SELECT count(*) FROM api_audit", [], |r| r.get::<_, i64>(0))
                            .unwrap(),
                        0
                    );
                }
                session
                    .store()
                    .unwrap()
                    .db
                    .execute_batch("DROP TRIGGER fail_provenance")
                    .unwrap();
            }
        }
        for (operation, request, code) in [
            ("principal-put", json!({"id":""}), "invalid_principal"),
            (
                "principal-put",
                json!({"id":"fixture"}),
                "principal_profile_conflict",
            ),
            (
                "grant-put",
                json!({"principal":"missing","definition":{"actions":["metadata:read"]}}),
                "unknown_principal",
            ),
        ] {
            let failure =
                admin_cli::execute_detailed(&mut session, "alternate", operation, &request)
                    .unwrap_err();
            assert!(matches!(failure, access_admin::Failure::Access(_)));
            let output = admin_error(
                &json!({"command":"api","operation":operation,"machine":true}),
                failure,
            );
            let payload: Value = serde_json::from_str(&output.stdout).unwrap();
            assert_eq!(
                payload["data"]["error"],
                json!({"message":code,"type":"AccessError"})
            );
        }
    }
}
