// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Runtime argument-null checks, distinct from variable coercion and static
//! validation. Errors are emitted only at an executed field/selection boundary.
use async_graphql_parser::{
    Pos, Positioned,
    types::{BaseType, Directive, ExecutableDocument, Field, Selection, SelectionSet, Type},
};
use async_graphql_value::Value as Input;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub message: String,
    pub pos: Pos,
    pub indices: Vec<usize>,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl Failure {
    pub fn server_error(
        &self,
        mut path: Vec<async_graphql::PathSegment>,
    ) -> async_graphql::ServerError {
        path.extend(
            self.indices
                .iter()
                .copied()
                .map(async_graphql::PathSegment::Index),
        );
        let mut error = async_graphql::ServerError::new(&self.message, Some(self.pos));
        error.path = path;
        error
    }
}
#[derive(Default)]
struct FieldPlan {
    argument: Option<Failure>,
    children: Option<Failure>,
}
#[derive(Default)]
pub(crate) struct RuntimeInputs {
    pub root_error: Option<Failure>,
    fields: BTreeMap<(usize, usize), FieldPlan>,
    deferred: BTreeSet<(usize, usize)>,
}
fn key(pos: Pos) -> (usize, usize) {
    (pos.line, pos.column)
}
fn named(mut ty: &Value) -> &str {
    while ty["ofType"].is_object() {
        ty = &ty["ofType"];
    }
    ty["name"].as_str().unwrap_or("")
}
fn nullable(ty: &Value) -> &Value {
    if ty["kind"] == "NON_NULL" {
        &ty["ofType"]
    } else {
        ty
    }
}
fn display(ty: &Value) -> String {
    match ty["kind"].as_str() {
        Some("NON_NULL") => format!("{}!", display(&ty["ofType"])),
        Some("LIST") => format!("[{}]", display(&ty["ofType"])),
        _ => named(ty).into(),
    }
}
fn type_def(name: &str, http: bool) -> Option<&'static Value> {
    crate::introspection::schema_for(http)["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
}
fn field_def(parent: &str, name: &str, http: bool) -> Option<Value> {
    if parent == "Query" && name == "__type" {
        return Some(
            serde_json::json!({"name":"__type","type":{"kind":"OBJECT","name":"__Type"},"args":[{"name":"name","type":{"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"String"}}}]}),
        );
    }
    type_def(parent, http)?
        .get("fields")?
        .as_array()?
        .iter()
        .find(|v| v["name"] == name)
        .cloned()
}
fn invalid_null(input: &Input, expected: &Value, variables: &Value, http: bool) -> bool {
    if let Input::Variable(name) = input {
        return variables.get(name.as_str()).is_none_or(Value::is_null)
            && expected["kind"] == "NON_NULL";
    }
    if matches!(input, Input::Null) {
        return expected["kind"] == "NON_NULL";
    }
    let expected = nullable(expected);
    if expected["kind"] == "LIST" {
        return if let Input::List(values) = input {
            values
                .iter()
                .any(|value| invalid_null(value, &expected["ofType"], variables, http))
        } else {
            invalid_null(input, &expected["ofType"], variables, http)
        };
    }
    if let Input::Object(values) = input {
        return type_def(named(expected), http)
            .and_then(|ty| ty["inputFields"].as_array())
            .is_some_and(|fields| {
                fields.iter().any(|field| {
                    values
                        .get(field["name"].as_str().unwrap())
                        .is_some_and(|value| invalid_null(value, &field["type"], variables, http))
                })
            });
    }
    false
}
fn argument_failure(
    name: &str,
    input: &Positioned<Input>,
    ty: &Value,
    variables: &Value,
    http: bool,
    source: &str,
) -> Option<Failure> {
    if !invalid_null(&input.node, ty, variables, http) {
        return None;
    }
    let direct_null = matches!(input.node, Input::Null)
        || matches!(&input.node, Input::Variable(name) if variables.get(name.as_str()).is_some_and(Value::is_null));
    let message = if direct_null && ty["kind"] == "NON_NULL" {
        format!(
            "Argument '{name}' of non-null type '{}' must not be null.",
            display(ty)
        )
    } else {
        format!(
            "Argument '{name}' has invalid value {}.",
            crate::validation::render_input(source, input)
        )
    };
    Some(Failure {
        message,
        pos: input.pos,
        indices: vec![],
    })
}
struct Planner<'a> {
    ast: &'a ExecutableDocument,
    variables: &'a Value,
    http: bool,
    source: &'a str,
}
impl Planner<'_> {
    fn condition(&self, directives: &[Positioned<Directive>]) -> Result<bool, Failure> {
        // graphql-core evaluates skip before include, regardless of source order.
        for name in ["skip", "include"] {
            if let Some(directive) = directives.iter().find(|d| d.node.name.node == name)
                && let Some(value) = directive.node.get_argument("if")
            {
                let ty = serde_json::json!({"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"Boolean"}});
                if let Some(error) =
                    argument_failure("if", value, &ty, self.variables, self.http, self.source)
                {
                    return Err(error);
                }
                let value = match &value.node {
                    Input::Boolean(value) => *value,
                    Input::Variable(name) => {
                        self.variables[name.as_str()].as_bool().unwrap_or(false)
                    }
                    _ => false,
                };
                if (name == "skip" && value) || (name == "include" && !value) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
    fn matches(&self, actual: &str, condition: &str) -> bool {
        actual == condition
            || type_def(condition, self.http)
                .and_then(|ty| ty["possibleTypes"].as_array())
                .is_some_and(|types| types.iter().any(|ty| ty["name"] == actual))
    }
    fn selection_error(
        &self,
        set: &SelectionSet,
        parent: &str,
        seen: &mut BTreeSet<String>,
    ) -> Option<Failure> {
        for selection in &set.items {
            if let Selection::FragmentSpread(spread) = &selection.node
                && seen.contains(spread.node.fragment_name.node.as_str())
            {
                continue;
            }
            match self.condition(selection.node.directives()) {
                Err(error) => return Some(error),
                Ok(false) => continue,
                Ok(true) => {}
            }
            match &selection.node {
                Selection::InlineFragment(fragment) => {
                    if fragment
                        .node
                        .type_condition
                        .as_ref()
                        .is_none_or(|condition| {
                            self.matches(parent, condition.node.on.node.as_str())
                        })
                        && let Some(error) =
                            self.selection_error(&fragment.node.selection_set.node, parent, seen)
                    {
                        return Some(error);
                    }
                }
                Selection::FragmentSpread(spread) => {
                    let name = spread.node.fragment_name.node.as_str();
                    seen.insert(name.into());
                    if let Some(fragment) = self.ast.fragments.get(name)
                        && self.matches(parent, fragment.node.type_condition.node.on.node.as_str())
                        && let Some(error) =
                            self.selection_error(&fragment.node.selection_set.node, parent, seen)
                    {
                        return Some(error);
                    }
                }
                Selection::Field(_) => {}
            }
        }
        None
    }
    fn merged_runtime_input(&self, set: &SelectionSet, parent: &str, out: &RuntimeInputs) -> bool {
        fn fields<'a>(
            planner: &Planner<'a>,
            set: &'a SelectionSet,
            parent: &str,
            seen: &mut BTreeSet<String>,
            result: &mut Vec<&'a Positioned<Field>>,
        ) {
            for selection in &set.items {
                if matches!(planner.condition(selection.node.directives()), Ok(false)) {
                    continue;
                }
                match &selection.node {
                    Selection::Field(field) => result.push(field),
                    Selection::InlineFragment(fragment)
                        if fragment
                            .node
                            .type_condition
                            .as_ref()
                            .is_none_or(|c| planner.matches(parent, c.node.on.node.as_str())) =>
                    {
                        fields(
                            planner,
                            &fragment.node.selection_set.node,
                            parent,
                            seen,
                            result,
                        )
                    }
                    Selection::FragmentSpread(spread) => {
                        let name = spread.node.fragment_name.node.as_str();
                        if seen.insert(name.into())
                            && let Some(fragment) = planner.ast.fragments.get(name)
                            && planner
                                .matches(parent, fragment.node.type_condition.node.on.node.as_str())
                        {
                            fields(
                                planner,
                                &fragment.node.selection_set.node,
                                parent,
                                seen,
                                result,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        fn affected(
            planner: &Planner<'_>,
            field: &Field,
            parent: &str,
            out: &RuntimeInputs,
        ) -> bool {
            if out
                .fields
                .get(&key(field.name.pos))
                .is_some_and(|p| p.argument.is_some() || p.children.is_some())
            {
                return true;
            }
            let child = field_def(parent, field.name.node.as_str(), planner.http)
                .map(|f| named(&f["type"]).to_owned())
                .unwrap_or_default();
            let mut nested = vec![];
            fields(
                planner,
                &field.selection_set.node,
                &child,
                &mut BTreeSet::new(),
                &mut nested,
            );
            nested
                .iter()
                .any(|field| affected(planner, &field.node, &child, out))
        }
        let mut collected = vec![];
        fields(self, set, parent, &mut BTreeSet::new(), &mut collected);
        let mut groups = BTreeMap::<String, Vec<&Positioned<Field>>>::new();
        for field in collected {
            groups
                .entry(field.node.response_key().node.to_string())
                .or_default()
                .push(field);
        }
        for group in groups.values() {
            let first = &group[0].node;
            let arguments = |field: &Field| {
                field
                    .arguments
                    .iter()
                    .map(|(name, value)| (name.node.to_string(), value.node.clone()))
                    .collect::<BTreeMap<_, _>>()
            };
            if group.len() > 1
                && group.iter().all(|field| {
                    field.node.name.node == first.name.node
                        && arguments(&field.node) == arguments(first)
                })
                && group
                    .iter()
                    .any(|field| affected(self, &field.node, parent, out))
            {
                return true;
            }
            for field in group {
                let child = field_def(parent, field.node.name.node.as_str(), self.http)
                    .map(|f| named(&f["type"]).to_owned())
                    .unwrap_or_default();
                if self.merged_runtime_input(&field.node.selection_set.node, &child, out) {
                    return true;
                }
            }
        }
        false
    }
    fn collect(&self, set: &SelectionSet, parent: &str, out: &mut RuntimeInputs) {
        for selection in &set.items {
            for directive in selection.node.directives() {
                if matches!(directive.node.name.node.as_str(), "skip" | "include") {
                    let ty = serde_json::json!({"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"Boolean"}});
                    for (name, value) in &directive.node.arguments {
                        if !parent.starts_with("__")
                            && name.node == "if"
                            && argument_failure(
                                "if",
                                value,
                                &ty,
                                self.variables,
                                self.http,
                                self.source,
                            )
                            .is_some()
                        {
                            out.deferred.insert(key(name.pos));
                        }
                    }
                }
            }
            match &selection.node {
                Selection::Field(field) => {
                    let definition = field_def(parent, field.node.name.node.as_str(), self.http);
                    let mut plan = FieldPlan::default();
                    if let Some(definition) = definition {
                        for arg in definition["args"].as_array().unwrap() {
                            if let Some((name, value)) = field
                                .node
                                .arguments
                                .iter()
                                .find(|(name, _)| arg["name"] == name.node.as_str())
                                // An absent variable uses the argument's schema
                                // default. Explicit null must not use that default.
                                && !(arg["defaultValue"].is_string()
                                    && matches!(&value.node, Input::Variable(variable) if self.variables.get(variable.as_str()).is_none()))
                                && let Some(error) = argument_failure(
                                    name.node.as_str(),
                                    value,
                                    &arg["type"],
                                    self.variables,
                                    self.http,
                                    self.source,
                                )
                            {
                                if !parent.starts_with("__")
                                    && !field.node.name.node.starts_with("__")
                                {
                                    out.deferred.insert(key(name.pos));
                                }
                                if plan.argument.is_none() {
                                    plan.argument = Some(error);
                                }
                            }
                        }
                        let child = named(&definition["type"]);
                        plan.children = self.selection_error(
                            &field.node.selection_set.node,
                            child,
                            &mut BTreeSet::new(),
                        );
                        self.collect(&field.node.selection_set.node, child, out);
                    } else if field.node.name.node == "__schema" {
                        self.collect(&field.node.selection_set.node, "__Schema", out);
                    }
                    out.fields.insert(key(field.node.name.pos), plan);
                }
                Selection::InlineFragment(fragment) => self.collect(
                    &fragment.node.selection_set.node,
                    fragment
                        .node
                        .type_condition
                        .as_ref()
                        .map_or(parent, |c| c.node.on.node.as_str()),
                    out,
                ),
                Selection::FragmentSpread(_) => {}
            }
        }
    }
}
impl RuntimeInputs {
    pub fn new(
        ast: &ExecutableDocument,
        variables: &Value,
        operation: Option<&str>,
        http: bool,
        source: &str,
    ) -> Self {
        let planner = Planner {
            ast,
            variables,
            http,
            source,
        };
        let mut out = Self::default();
        for (name, op) in ast.operations.iter() {
            if operation.is_none_or(|selected| name.is_some_and(|name| name.as_str() == selected)) {
                out.root_error = planner.selection_error(
                    &op.node.selection_set.node,
                    "Query",
                    &mut BTreeSet::new(),
                );
            }
            planner.collect(&op.node.selection_set.node, "Query", &mut out);
        }
        for fragment in ast.fragments.values() {
            planner.collect(
                &fragment.node.selection_set.node,
                fragment.node.type_condition.node.on.node.as_str(),
                &mut out,
            );
        }
        // The framework executes merge-compatible repeated fields separately.
        // Do not introduce new duplicate resolver work while deferring their
        // runtime input errors: keep its existing strict-validation result until
        // field merging receives its own complete implementation.
        if !out.deferred.is_empty()
            && ast.operations.iter().any(|(name, op)| {
                operation.is_none_or(|selected| name.is_some_and(|name| name.as_str() == selected))
                    && planner.merged_runtime_input(&op.node.selection_set.node, "Query", &out)
            })
        {
            return Self::default();
        }
        out
    }
    pub fn defers(&self, error: &async_graphql::ServerError) -> bool {
        error.message.starts_with("Invalid value for argument ")
            && error.locations.len() == 1
            && self.deferred.contains(&key(error.locations[0]))
    }
    pub fn argument_error(&self, field: &Field) -> Option<Failure> {
        self.fields.get(&key(field.name.pos))?.argument.clone()
    }
    pub fn completion_error(&self, field: &Field, value: &Value, ty: &Type) -> Option<Failure> {
        let mut error = self.fields.get(&key(field.name.pos))?.children.clone()?;
        fn first_object(value: &Value, ty: &Type, indices: &mut Vec<usize>) -> bool {
            if value.is_null() {
                return false;
            }
            if let BaseType::List(inner) = &ty.base {
                if let Some(values) = value.as_array() {
                    for (index, value) in values.iter().enumerate() {
                        indices.push(index);
                        if first_object(value, inner, indices) {
                            return true;
                        }
                        indices.pop();
                    }
                }
                false
            } else {
                true
            }
        }
        first_object(value, ty, &mut error.indices).then_some(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_composite_list_items_are_non_null() {
        // completion_error reports the first failing object. This is the exact
        // current schema rule: composite list items are non-null, so their
        // first failed selection aborts that list. Adding nullable object list
        // items requires per-item completion errors before changing this guard.
        fn check(ty: &Type, kinds: &BTreeMap<String, bool>) {
            if let BaseType::List(inner) = &ty.base {
                let mut base = inner.as_ref();
                while let BaseType::List(next) = &base.base {
                    base = next;
                }
                if let BaseType::Named(name) = &base.base
                    && kinds.get(name.as_str()) == Some(&true)
                {
                    assert!(!inner.nullable, "nullable composite list item: {ty}");
                }
                check(inner, kinds);
            }
        }
        let schema =
            async_graphql_parser::parse_schema(include_str!("../resources/schema.graphql"))
                .unwrap();
        let definitions: Vec<_> = schema
            .definitions
            .iter()
            .filter_map(|d| {
                if let async_graphql_parser::types::TypeSystemDefinition::Type(ty) = d {
                    Some(&ty.node)
                } else {
                    None
                }
            })
            .collect();
        let kinds = definitions
            .iter()
            .map(|ty| {
                (
                    ty.name.node.to_string(),
                    matches!(ty.kind, async_graphql_parser::types::TypeKind::Object(_)),
                )
            })
            .collect();
        for ty in definitions {
            if let async_graphql_parser::types::TypeKind::Object(object) = &ty.kind {
                for field in &object.fields {
                    check(&field.node.ty.node, &kinds);
                }
            }
        }
    }
}
