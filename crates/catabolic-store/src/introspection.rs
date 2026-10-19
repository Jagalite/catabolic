//! Frozen GraphQL schema metadata projected in Rust. Framework-generated builtin
//! descriptions/order are not allowed to silently change the public contract.
use serde_json::{Value, json};
use std::sync::OnceLock;
pub(crate) fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/introspection.json"))
            .expect("frozen schema metadata")
    })
}
/// HTTP contract 1.7.0 excludes the local owner's work inbox extension.
pub(crate) fn schema_for(http: bool) -> &'static Value {
    if !http {
        return schema();
    }
    static HTTP_SCHEMA: OnceLock<Value> = OnceLock::new();
    HTTP_SCHEMA.get_or_init(|| {
        let mut value = schema().clone();
        for ty in value["types"].as_array_mut().expect("schema types") {
            if ty["name"] == "Query" {
                ty["fields"]
                    .as_array_mut()
                    .expect("query fields")
                    .retain(|field| field["name"] != "workInbox");
            }
        }
        value
    })
}

/// Resolve one native introspection field. Completion and budget accounting are
/// owned by the same executor as catalog fields, in document encounter order.
pub(crate) fn resolve(
    parent: &str,
    field: &str,
    source: &Value,
    args: &Value,
    http: bool,
) -> Value {
    if field == "__typename" {
        return json!(parent);
    }
    if parent == "Query" {
        return match field {
            "__schema" => schema_for(http).clone(),
            "__type" => schema_for(http)["types"]
                .as_array()
                .unwrap()
                .iter()
                .find(|ty| ty["name"] == args["name"])
                .cloned()
                .unwrap_or(Value::Null),
            _ => Value::Null,
        };
    }
    let source = if parent == "__Type" && source["name"].is_string() {
        schema_for(http)["types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ty| ty["name"] == source["name"])
            .unwrap_or(source)
    } else {
        source
    };
    let mut value = source.get(field).cloned().unwrap_or(Value::Null);
    if matches!(field, "fields" | "enumValues" | "inputFields" | "args")
        && args["includeDeprecated"] != true
        && let Some(values) = value.as_array_mut()
    {
        values.retain(|value| value["isDeprecated"] != true);
    }
    value
}

/// graphql-core's bounded lexical suggestions, independent of framework wording.
pub(crate) fn field_suggestions(typename: &str, input: &str) -> String {
    field_suggestions_for(typename, input, false)
}
pub(crate) fn field_suggestions_for(typename: &str, input: &str, http: bool) -> String {
    suggestions(
        schema_for(http)["types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ty| ty["name"] == typename)
            .and_then(|ty| {
                ty["fields"]
                    .as_array()
                    .or_else(|| ty["enumValues"].as_array())
            }),
        input,
    )
}
pub(crate) fn argument_suggestions(arguments: &Value, input: &str) -> String {
    suggestions(arguments.as_array(), input)
}
fn suggestions(fields: Option<&Vec<Value>>, input: &str) -> String {
    let threshold = input.chars().count() * 2 / 5 + 1;
    let lower: Vec<char> = input.to_lowercase().chars().collect();
    let mut candidates = vec![];
    if let Some(fields) = fields {
        for field in fields {
            let name = field["name"].as_str().unwrap();
            let other: Vec<char> = name.to_lowercase().chars().collect();
            if lower.len().abs_diff(other.len()) > threshold {
                continue;
            }
            let distance = if lower == other {
                usize::from(input != name)
            } else {
                let mut rows = vec![vec![0usize; lower.len() + 1]; other.len() + 1];
                for (i, row) in rows.iter_mut().enumerate() {
                    row[0] = i;
                }
                for (j, cell) in rows[0].iter_mut().enumerate() {
                    *cell = j;
                }
                for i in 1..=other.len() {
                    for j in 1..=lower.len() {
                        let cost = usize::from(other[i - 1] != lower[j - 1]);
                        rows[i][j] = (rows[i - 1][j] + 1)
                            .min(rows[i][j - 1] + 1)
                            .min(rows[i - 1][j - 1] + cost);
                        if i > 1
                            && j > 1
                            && other[i - 1] == lower[j - 2]
                            && other[i - 2] == lower[j - 1]
                        {
                            rows[i][j] = rows[i][j].min(rows[i - 2][j - 2] + cost);
                        }
                    }
                }
                rows[other.len()][lower.len()]
            };
            if distance <= threshold {
                candidates.push((distance, name));
            }
        }
    }
    candidates.sort_unstable();
    let names: Vec<String> = candidates
        .iter()
        .take(5)
        .map(|(_, name)| format!("'{name}'"))
        .collect();
    match names.len() {
        0 => String::new(),
        1 => format!(" Did you mean {}?", names[0]),
        2 => format!(" Did you mean {} or {}?", names[0], names[1]),
        _ => format!(
            " Did you mean {}, or {}?",
            names[..names.len() - 1].join(", "),
            names.last().unwrap()
        ),
    }
}
