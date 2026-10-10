//! Contract validation before framework execution. Locations and wording belong
//! to the frozen public GraphQL contract, rather than a framework implementation.
use async_graphql_parser::{
    Pos, Positioned,
    types::{ExecutableDocument, Selection, SelectionSet},
};
use async_graphql_value::Value as Input;
use serde_json::{Value, json};
fn ty(name: &str) -> Option<&'static Value> {
    crate::introspection::schema()["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|ty| ty["name"] == name)
}
fn named(mut value: &Value) -> &str {
    while value["ofType"].is_object() {
        value = &value["ofType"];
    }
    value["name"].as_str().unwrap_or("")
}
fn display(value: &Value) -> String {
    match value["kind"].as_str() {
        Some("NON_NULL") => format!("{}!", display(&value["ofType"])),
        Some("LIST") => format!("[{}]", display(&value["ofType"])),
        _ => value["name"].as_str().unwrap_or("").into(),
    }
}
fn error(out: &mut Vec<Value>, message: String, pos: Pos) {
    out.push(json!({"message":message,"locations":[{"line":pos.line,"column":pos.column}]}));
}
fn literal(value: &Positioned<Input>, expected: &Value, out: &mut Vec<Value>) {
    if matches!(value.node, Input::Variable(_)) {
        return;
    }
    let list_type = if expected["kind"] == "NON_NULL" {
        &expected["ofType"]
    } else {
        expected
    };
    if list_type["kind"] == "LIST" && !matches!(value.node, Input::Null) {
        if let Input::List(values) = &value.node {
            for child in values {
                literal(
                    &Positioned::new(child.clone(), value.pos),
                    &list_type["ofType"],
                    out,
                );
            }
        } else {
            literal(value, &list_type["ofType"], out);
        }
        return;
    }
    let kind = named(expected);
    let rendered = value.node.to_string();
    let reason = match (&value.node, kind) {
        (Input::Null, _) if expected["kind"] == "NON_NULL" => Some(format!(
            "Expected value of type '{}', found null.",
            display(expected)
        )),
        (Input::Null, _) => None,
        (Input::Number(n), "Int") if n.as_i64().is_some_and(|n| i32::try_from(n).is_ok()) => None,
        (Input::Number(n), "Int") if n.as_i64().is_some() => Some(format!(
            "Int cannot represent non 32-bit signed integer value: {rendered}"
        )),
        (_, "Int") => Some(format!(
            "Int cannot represent non-integer value: {rendered}"
        )),
        (Input::String(_), "String")
        | (Input::Boolean(_), "Boolean")
        | (Input::Number(_), "Float")
        | (Input::String(_), "ID") => None,
        (Input::Number(n), "ID") if n.as_i64().is_some() || n.as_u64().is_some() => None,
        (_, "String") => Some(format!(
            "String cannot represent a non string value: {rendered}"
        )),
        (_, "Boolean") => Some(format!(
            "Boolean cannot represent a non boolean value: {rendered}"
        )),
        (_, "Float") => Some(format!(
            "Float cannot represent non numeric value: {rendered}"
        )),
        (_, "ID") => Some(format!(
            "ID cannot represent a non-string and non-integer value: {rendered}"
        )),
        (Input::Enum(name), _) if ty(kind).is_some_and(|ty| ty["kind"] == "ENUM") => {
            let exists = ty(kind).unwrap()["enumValues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["name"] == name.as_str());
            if exists {
                None
            } else {
                let suggestions = crate::introspection::field_suggestions(kind, name.as_str())
                    .replace("Did you mean", "Did you mean the enum value");
                Some(format!(
                    "Value '{name}' does not exist in '{kind}' enum.{suggestions}"
                ))
            }
        }
        _ => None,
    };
    if let Some(reason) = reason {
        error(out, reason, value.pos);
    }
}
fn walk(set: &SelectionSet, parent: &str, doc: &ExecutableDocument, out: &mut Vec<Value>) {
    for selection in &set.items {
        match &selection.node {
            Selection::Field(field) => {
                let name = field.node.name.node.as_str();
                let synthetic = match name {
                    "__typename" => Some(
                        json!({"name":name,"type":{"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"String"}},"args":[]}),
                    ),
                    "__schema" if parent == "Query" => Some(
                        json!({"name":name,"type":{"kind":"OBJECT","name":"__Schema"},"args":[]}),
                    ),
                    "__type" if parent == "Query" => Some(
                        json!({"name":name,"type":{"kind":"OBJECT","name":"__Type"},"args":[{"name":"name","type":{"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"String"}}}]}),
                    ),
                    _ => None,
                };
                let definition = synthetic.as_ref().or_else(|| {
                    ty(parent)
                        .and_then(|ty| ty["fields"].as_array())
                        .and_then(|fields| fields.iter().find(|field| field["name"] == name))
                });
                let Some(definition) = definition else {
                    error(
                        out,
                        format!(
                            "Cannot query field '{name}' on type '{parent}'.{}",
                            crate::introspection::field_suggestions(parent, name)
                        ),
                        field.node.name.pos,
                    );
                    continue;
                };
                let result_type = &definition["type"];
                let child = named(result_type);
                let is_leaf =
                    ty(child).is_some_and(|ty| ty["kind"] == "SCALAR" || ty["kind"] == "ENUM");
                let has_children = !field.node.selection_set.node.items.is_empty();
                if is_leaf && has_children {
                    error(
                        out,
                        format!(
                            "Field '{name}' must not have a selection since type '{}' has no subfields.",
                            display(result_type)
                        ),
                        field.node.selection_set.pos,
                    );
                } else if !is_leaf && !has_children {
                    error(
                        out,
                        format!(
                            "Field '{name}' of type '{}' must have a selection of subfields. Did you mean '{name} {{ ... }}'?",
                            display(result_type)
                        ),
                        field.pos,
                    );
                }
                for (argument, value) in &field.node.arguments {
                    let expected = definition["args"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|a| a["name"] == argument.node.as_str());
                    if let Some(expected) = expected {
                        literal(value, &expected["type"], out);
                    } else {
                        error(
                            out,
                            format!(
                                "Unknown argument '{}' on field '{parent}.{name}'.{}",
                                argument.node,
                                crate::introspection::argument_suggestions(
                                    &definition["args"],
                                    argument.node.as_str()
                                )
                            ),
                            argument.pos,
                        );
                    }
                }
                if !is_leaf && has_children {
                    walk(&field.node.selection_set.node, child, doc, out);
                }
            }
            Selection::InlineFragment(fragment) => walk(
                &fragment.node.selection_set.node,
                fragment
                    .node
                    .type_condition
                    .as_ref()
                    .map(|v| v.node.on.node.as_str())
                    .unwrap_or(parent),
                doc,
                out,
            ),
            Selection::FragmentSpread(spread) => {
                if !doc
                    .fragments
                    .contains_key(spread.node.fragment_name.node.as_str())
                {
                    error(
                        out,
                        format!("Unknown fragment '{}'.", spread.node.fragment_name.node),
                        spread.node.fragment_name.pos,
                    );
                }
            }
        }
    }
}
pub(crate) fn validate(doc: &ExecutableDocument) -> Option<Value> {
    let mut out = vec![];
    for (_, op) in doc.operations.iter() {
        walk(&op.node.selection_set.node, "Query", doc, &mut out);
    }
    for fragment in doc.fragments.values() {
        walk(
            &fragment.node.selection_set.node,
            fragment.node.type_condition.node.on.node.as_str(),
            doc,
            &mut out,
        );
    }
    if out.is_empty() {
        None
    } else {
        Some(json!({"data":null,"errors":out}))
    }
}
pub(crate) fn parse_error(document: &str, error: &async_graphql_parser::Error) -> Value {
    let positions: Vec<Value> = error
        .positions()
        .map(|p| json!({"line":p.line,"column":p.column}))
        .collect();
    let message = match error {
        async_graphql_parser::Error::Syntax { message, start, .. }
            if message.contains("expected selection") =>
        {
            let suffix = document
                .lines()
                .nth(start.line - 1)
                .unwrap_or("")
                .chars()
                .skip(start.column - 1)
                .collect::<String>();
            let found = if suffix.is_empty() {
                "<EOF>".into()
            } else {
                format!("'{}'", suffix.chars().next().unwrap())
            };
            format!("Syntax Error: Expected Name, found {found}.")
        }
        async_graphql_parser::Error::OperationDuplicated { operation, .. } => {
            format!("There can be only one operation named '{operation}'.")
        }
        async_graphql_parser::Error::FragmentDuplicated { fragment, .. } => {
            format!("There can be only one fragment named '{fragment}'.")
        }
        _ => error.to_string(),
    };
    let mut error = json!({"message":message});
    if !positions.is_empty() {
        error["locations"] = json!(positions);
    }
    json!({"data":null,"errors":[error]})
}
