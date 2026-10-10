//! Frozen GraphQL schema metadata projected in Rust. Framework-generated builtin
//! descriptions/order are not allowed to silently change the public contract.
use crate::{Error, Result};
use async_graphql_parser::types::{ExecutableDocument, Field, Selection, SelectionSet};
use async_graphql_value::Value as Input;
use serde_json::{Value, json};
use std::sync::OnceLock;
fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/introspection.json"))
            .expect("frozen schema metadata")
    })
}
fn input(value: &Input, variables: &Value) -> Value {
    match value {
        Input::Variable(name) => variables[name.as_str()].clone(),
        _ => value
            .clone()
            .into_const_with(|name| {
                async_graphql_value::ConstValue::from_json(variables[name.as_str()].clone())
            })
            .ok()
            .and_then(|v| serde_json::to_value(v).ok())
            .unwrap_or(Value::Null),
    }
}
fn included(
    directives: &[async_graphql_parser::Positioned<async_graphql_parser::types::Directive>],
    variables: &Value,
) -> bool {
    directives.iter().all(|d| {
        let condition = d
            .node
            .arguments
            .iter()
            .find(|(name, _)| name.node == "if")
            .map(|(_, v)| input(&v.node, variables));
        match d.node.name.node.as_str() {
            "skip" => condition != Some(json!(true)),
            "include" => condition == Some(json!(true)),
            _ => true,
        }
    })
}
fn fields(
    set: &SelectionSet,
    document: &ExecutableDocument,
    variables: &Value,
    out: &mut Vec<Field>,
) {
    for selection in &set.items {
        match &selection.node {
            Selection::Field(field) if included(&field.node.directives, variables) => {
                let key = field
                    .node
                    .alias
                    .as_ref()
                    .unwrap_or(&field.node.name)
                    .node
                    .as_str();
                if let Some(existing) = out
                    .iter_mut()
                    .find(|f| f.alias.as_ref().unwrap_or(&f.name).node.as_str() == key)
                {
                    existing
                        .selection_set
                        .node
                        .items
                        .extend(field.node.selection_set.node.items.clone());
                } else {
                    out.push(field.node.clone());
                }
            }
            Selection::InlineFragment(fragment)
                if included(&fragment.node.directives, variables) =>
            {
                fields(&fragment.node.selection_set.node, document, variables, out)
            }
            Selection::FragmentSpread(spread) if included(&spread.node.directives, variables) => {
                if let Some(fragment) = document
                    .fragments
                    .get(spread.node.fragment_name.node.as_str())
                {
                    fields(&fragment.node.selection_set.node, document, variables, out);
                }
            }
            _ => {}
        }
    }
}
fn child_type(parent: &str, field: &str) -> &'static str {
    match (parent, field) {
        ("Query", "__schema") => "__Schema",
        ("Query", "__type") => "__Type",
        ("__Schema", "types" | "queryType" | "mutationType" | "subscriptionType")
        | ("__Type", "interfaces" | "possibleTypes" | "ofType")
        | ("__Field" | "__InputValue", "type") => "__Type",
        ("__Schema", "directives") => "__Directive",
        ("__Type", "fields") => "__Field",
        ("__Type", "enumValues") => "__EnumValue",
        ("__Type", "inputFields") | ("__Field" | "__Directive", "args") => "__InputValue",
        _ => "",
    }
}
fn typed(source: &Value, typename: &str) -> Value {
    if typename == "__Type"
        && source["name"].is_string()
        && let Some(value) = schema()["types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == source["name"])
    {
        return value.clone();
    }
    source.clone()
}
fn project(
    source: &Value,
    typename: &str,
    set: &SelectionSet,
    document: &ExecutableDocument,
    variables: &Value,
    charge: &mut impl FnMut(&Value) -> Result<()>,
) -> Result<Value> {
    if source.is_null() {
        return Ok(Value::Null);
    }
    let source = typed(source, typename);
    let mut output = serde_json::Map::new();
    let mut selections = vec![];
    fields(set, document, variables, &mut selections);
    for field in selections {
        let name = field.name.node.as_str();
        let key = field.alias.as_ref().unwrap_or(&field.name).node.as_str();
        let mut value = if name == "__typename" {
            json!(typename)
        } else {
            source.get(name).cloned().unwrap_or(Value::Null)
        };
        let include = field
            .arguments
            .iter()
            .find(|(n, _)| n.node == "includeDeprecated")
            .map(|(_, v)| input(&v.node, variables))
            == Some(json!(true));
        if ["fields", "enumValues", "inputFields", "args"].contains(&name)
            && value.is_array()
            && !include
        {
            value
                .as_array_mut()
                .unwrap()
                .retain(|v| v["isDeprecated"] != true);
        }
        // Frozen graphql-core returns dict views for these fields. Its budget
        // middleware counts the field but charges records only for list/tuple.
        if typename == "__Schema" && name == "types"
            || include && matches!(name, "enumValues" | "inputFields" | "args")
        {
            charge(&Value::Null)?;
        } else {
            charge(&value)?;
        }
        if !field.selection_set.node.items.is_empty() {
            let child = child_type(typename, name);
            value = if let Some(list) = value.as_array() {
                Value::Array(
                    list.iter()
                        .map(|v| {
                            project(
                                v,
                                child,
                                &field.selection_set.node,
                                document,
                                variables,
                                charge,
                            )
                        })
                        .collect::<Result<Vec<_>>>()?,
                )
            } else {
                project(
                    &value,
                    child,
                    &field.selection_set.node,
                    document,
                    variables,
                    charge,
                )?
            };
        }
        output.insert(key.into(), value);
    }
    Ok(Value::Object(output))
}
pub fn apply(
    document: &ExecutableDocument,
    operation: Option<&str>,
    variables: &Value,
    result: &mut Value,
    charge: &mut impl FnMut(&Value) -> Result<()>,
) -> Result<()> {
    if !result["data"].is_object() {
        return Ok(());
    }
    let op = document
        .operations
        .iter()
        .find(|(name, _)| operation.is_none_or(|wanted| name.is_some_and(|n| n.as_str() == wanted)))
        .map(|(_, op)| &op.node)
        .ok_or_else(|| Error("unknown operation".into()))?;
    let mut variables = variables.clone();
    for definition in &op.variable_definitions {
        let key = definition.node.name.node.as_str();
        if variables.get(key).is_none()
            && let Some(default) = &definition.node.default_value
        {
            variables[key] = serde_json::to_value(&default.node)?;
        }
    }
    let mut roots = vec![];
    fields(&op.selection_set.node, document, &variables, &mut roots);
    for field in roots {
        let name = field.name.node.as_str();
        let key = field.alias.as_ref().unwrap_or(&field.name).node.as_str();
        let (source, typename) = match name {
            "__schema" => (schema().clone(), "__Schema"),
            "__type" => {
                let wanted = field
                    .arguments
                    .iter()
                    .find(|(n, _)| n.node == "name")
                    .map(|(_, v)| input(&v.node, &variables))
                    .unwrap_or(Value::Null);
                (
                    schema()["types"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|v| v["name"] == wanted)
                        .cloned()
                        .unwrap_or(Value::Null),
                    "__Type",
                )
            }
            _ => continue,
        };
        charge(&source)?;
        result["data"][key] = project(
            &source,
            typename,
            &field.selection_set.node,
            document,
            &variables,
            charge,
        )?;
    }
    Ok(())
}
