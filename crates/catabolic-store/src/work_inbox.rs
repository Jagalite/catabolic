// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Read-only work discovery through the production bounded query owners.
//! Evidence tokens describe a snapshot; they are never mutation authorization.
use crate::{Error, Result, Store, graphql, sql};
use catabolic_core::{encode, sha256};
use serde_json::{Map, Value, json};

fn records(result: &Value) -> Result<Vec<Value>> {
    let columns = result["columns"]
        .as_array()
        .ok_or_else(|| Error("invalid query result columns".into()))?;
    result["rows"]
        .as_array()
        .ok_or_else(|| Error("invalid query result rows".into()))?
        .iter()
        .map(|row| {
            let row = row
                .as_array()
                .filter(|r| r.len() == columns.len())
                .ok_or_else(|| Error("invalid query result width".into()))?;
            let values = columns
                .iter()
                .zip(row)
                .map(|(key, value)| {
                    Ok((
                        key.as_str()
                            .ok_or_else(|| Error("invalid query column".into()))?
                            .to_owned(),
                        value.clone(),
                    ))
                })
                .collect::<Result<Map<String, Value>>>()?;
            Ok(Value::Object(values))
        })
        .collect()
}

// graphql-core's formatted error dictionaries have stable insertion order.
// Preserve it inside the legacy Python-repr error string without changing the
// canonical JSON map ordering used by content and revision digests.
fn graphql_errors_text(value: &Value) -> String {
    match value {
        Value::Array(rows) => format!(
            "[{}]",
            rows.iter()
                .map(graphql_errors_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(fields) => {
            let order = if fields.contains_key("message") {
                &["message", "locations", "path", "extensions"][..]
            } else if fields.contains_key("line") && fields.contains_key("column") {
                &["line", "column"][..]
            } else {
                &[][..]
            };
            let keys = order
                .iter()
                .copied()
                .filter(|key| fields.contains_key(*key))
                .chain(
                    fields
                        .keys()
                        .map(String::as_str)
                        .filter(|key| !order.contains(key)),
                );
            format!(
                "{{{}}}",
                keys.map(|key| format!(
                    "{}: {}",
                    python_repr_string(key),
                    graphql_errors_text(&fields[key])
                ))
                .collect::<Vec<_>>()
                .join(", ")
            )
        }
        Value::String(text) => python_repr_string(text),
        _ => crate::exports::python_text(value),
    }
}

fn python_repr_string(text: &str) -> String {
    // Reuse the existing compatibility renderer with a one-element array.
    let list = crate::exports::python_text(&json!([text]));
    list[1..list.len() - 1].to_owned()
}

pub fn evidence_token(row: &Value) -> String {
    sha256(encode(row))
}

pub fn validate_request(request: &Value) -> Result<u64> {
    request
        .get("timeout_ms")
        .map(Value::as_u64)
        .unwrap_or(Some(5000))
        .filter(|v| (1..=60000).contains(v))
        .ok_or_else(|| Error("timeout-ms must be between 1 and 60000".into()))
}

/// Lends the original Store to GraphQL and returns it, preserving its identity.
/// SQL and GraphQL install only their existing connection-local read contexts.
pub fn execute(
    store: Store,
    profile: &str,
    operation: &str,
    request: &Value,
) -> Result<(Store, Value)> {
    let timeout_ms = validate_request(request)?;
    if operation == "list" {
        let (store, result) = graphql::execute_store(
            store,
            "query($first:Int!,$after:String,$inactive:Boolean!){workInbox(first:$first,after:$after,includeInactive:$inactive){nodes pageInfo{endCursor hasNextPage}}}",
            profile,
            json!({"first":request.get("limit").cloned().unwrap_or(json!(100)),"after":request["after"],"inactive":request.get("include_inactive").cloned().unwrap_or(json!(false))}),
            None,
            timeout_ms,
        )?;
        if result["errors"].as_array().is_some_and(|v| !v.is_empty()) {
            return Err(Error(format!(
                "inbox evaluation incomplete: {}",
                graphql_errors_text(&result["errors"])
            )));
        }
        let page = &result["data"]["workInbox"];
        let report = json!({"profile":profile,"entries":page["nodes"],"next_cursor":page["pageInfo"]["endCursor"],"complete":true,"result_complete":page["pageInfo"]["hasNextPage"]!=true,"evaluation_complete":true,"evidence":"recorded only; inventory coverage and physical health may be unknown"});
        return Ok((store, report));
    }
    let report = match operation {
        "show" => {
            let parameters = encode(&json!({"key":request["work_key"]}));
            let result = sql::execute_store(
                &store,
                Some("SELECT * FROM catalog_work_inbox WHERE profile=:profile AND work_key=:key"),
                &sql::Options {
                    profile,
                    params: Some(&parameters),
                    max_rows: 1,
                    timeout_ms,
                    ..Default::default()
                },
            )?;
            let mut row = records(&result)?.into_iter().next().ok_or_else(|| Error("unknown work key in this profile; resolved derived work may only remain in its owner's history".into()))?;
            crate::query::decode(
                &mut row,
                &[
                    "related_work",
                    "preconditions",
                    "suggested_actions",
                    "evidence",
                ],
            )?;
            let token = evidence_token(&row);
            if let Some(expected) = request["expected_evidence"]
                .as_str()
                .filter(|s| !s.is_empty())
                && expected != token
            {
                return Err(Error(
                    "inbox evidence changed; reload the owner record and replan".into(),
                ));
            }
            json!({"entry":row,"evidence_token":token,"evaluation_complete":true,"action_contract":"Descriptors only. Reload the owner and use its authorization, revision checks and transaction. This token is not an atomic claim."})
        }
        "summary" => {
            let result = sql::execute_store(
                &store,
                Some(
                    "SELECT category,actionability,count(*) AS entries,sum(countable) AS work_count FROM catalog_work_inbox WHERE profile=:profile GROUP BY category,actionability ORDER BY category,actionability",
                ),
                &sql::Options {
                    profile,
                    max_rows: 1000,
                    timeout_ms,
                    ..Default::default()
                },
            )?;
            if result["truncated"] == true {
                return Err(Error("inbox summary evaluation incomplete".into()));
            }
            let groups = records(&result)?;
            let count = groups
                .iter()
                .filter(|r| {
                    matches!(
                        r["actionability"].as_str(),
                        Some("ready" | "blocked" | "needs_decision")
                    )
                })
                .try_fold(0i64, |n, row| {
                    n.checked_add(row["work_count"].as_i64().unwrap_or(0))
                        .ok_or_else(|| Error("inbox count exceeds integer range".into()))
                })?;
            json!({"profile":profile,"groups":groups,"actionable_count":count,"evaluation_complete":true,"curation_finished":null,"evidence":"Counts describe recorded work, not complete inventory or verified projection health."})
        }
        _ => return Err(Error("unknown inbox operation".into())),
    };
    Ok((store, report))
}

/// Recorded retained links only. This does not inspect, modify or purge files.
pub fn retained(store: &Store, profile: &str, catalog: &str, request: &Value) -> Result<Value> {
    crate::mutation::profile(store, profile)?;
    let limit = request
        .get("limit")
        .map(Value::as_u64)
        .unwrap_or(Some(100))
        .filter(|n| (1..=1000).contains(n))
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))?;
    if !store
        .db
        .prepare("SELECT id FROM catalogs WHERE id=?")?
        .exists([catalog])?
    {
        return Err(Error(format!("unknown catalog: {catalog}")));
    }
    let after = request["after"].as_str().unwrap_or("");
    let mut rows = crate::migration::rows(
        &store.db,
        "SELECT * FROM retained_hardlinks WHERE profile=? AND catalog=? AND id>? ORDER BY id LIMIT ?",
        &[
            profile.to_owned().into(),
            catalog.to_owned().into(),
            after.to_owned().into(),
            ((limit + 1) as i64).into(),
        ],
    )?;
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next = if more {
        rows.last().unwrap()["id"].clone()
    } else {
        Value::Null
    };
    Ok(
        json!({"retained":rows,"next_after":next,"warning":"Recorded retained data is never automatically purged; inspect before any manual deletion."}),
    )
}
