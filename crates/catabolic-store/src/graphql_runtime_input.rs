// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Runtime argument-null checks, distinct from variable coercion and static
//! validation. Errors are emitted only at an executed field/selection boundary.
use async_graphql_parser::{
    Pos, Positioned,
    types::{Directive, ExecutableDocument, Field, Selection, SelectionSet, Type},
};
use async_graphql_value::Value as Input;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub message: String,
    pub pos: Pos,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl Failure {
    pub fn server_error(
        &self,
        path: Vec<async_graphql::PathSegment>,
    ) -> async_graphql::ServerError {
        let mut error = async_graphql::ServerError::new(&self.message, Some(self.pos));
        error.path = path;
        error
    }
}
#[derive(Default)]
struct FieldPlan {
    argument: Option<Failure>,
}
pub(crate) struct RuntimeInputs {
    document: ExecutableDocument,
    variables: Value,
    source: String,
    http: bool,
    fields: BTreeMap<(usize, usize), FieldPlan>,
    deferred: BTreeSet<(usize, usize)>,
}
fn key(pos: Pos) -> (usize, usize) {
    (pos.line, pos.column)
}
pub(crate) fn named(mut ty: &Value) -> &str {
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
pub(crate) fn field_def(parent: &str, name: &str, http: bool) -> Option<Value> {
    if name == "__typename" {
        return Some(
            serde_json::json!({"name":name,"type":{"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"String"}},"args":[]}),
        );
    }
    if parent == "Query" && name == "__schema" {
        return Some(
            serde_json::json!({"name":name,"type":{"kind":"NON_NULL","ofType":{"kind":"OBJECT","name":"__Schema"}},"args":[]}),
        );
    }
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
    fn fields(
        &self,
        set: &SelectionSet,
        parent: &str,
        seen: &mut BTreeSet<String>,
        out: &mut Vec<CollectedField>,
        indices: &mut BTreeMap<String, usize>,
    ) -> Result<(), Failure> {
        for selection in &set.items {
            // A visited named fragment is ignored before evaluating directives,
            // matching graphql-core CollectFields, including merged selections.
            if let Selection::FragmentSpread(spread) = &selection.node
                && seen.contains(spread.node.fragment_name.node.as_str())
            {
                continue;
            }
            if !self.condition(selection.node.directives())? {
                continue;
            }
            match &selection.node {
                Selection::Field(field) => {
                    let response_key = field.node.response_key().node.to_string();
                    if let Some(index) = indices.get(&response_key) {
                        let existing = &mut out[*index];
                        existing.locations.push(field.pos);
                        existing
                            .field
                            .node
                            .selection_set
                            .node
                            .items
                            .extend(field.node.selection_set.node.items.clone());
                    } else {
                        indices.insert(response_key, out.len());
                        out.push(CollectedField {
                            field: field.clone(),
                            locations: vec![field.pos],
                        });
                    }
                }
                Selection::InlineFragment(fragment) => {
                    if fragment
                        .node
                        .type_condition
                        .as_ref()
                        .is_none_or(|c| self.matches(parent, c.node.on.node.as_str()))
                    {
                        self.fields(
                            &fragment.node.selection_set.node,
                            parent,
                            seen,
                            out,
                            indices,
                        )?;
                    }
                }
                Selection::FragmentSpread(spread) => {
                    let name = spread.node.fragment_name.node.as_str();
                    seen.insert(name.into());
                    if let Some(fragment) = self.ast.fragments.get(name)
                        && self.matches(parent, fragment.node.type_condition.node.on.node.as_str())
                    {
                        self.fields(
                            &fragment.node.selection_set.node,
                            parent,
                            seen,
                            out,
                            indices,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
    fn collect(&self, set: &SelectionSet, parent: &str, out: &mut RuntimeInputs) {
        for selection in &set.items {
            for directive in selection.node.directives() {
                if matches!(directive.node.name.node.as_str(), "skip" | "include") {
                    let ty = serde_json::json!({"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"Boolean"}});
                    for (name, value) in &directive.node.arguments {
                        if name.node == "if"
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
                                out.deferred.insert(key(name.pos));
                                if plan.argument.is_none() {
                                    plan.argument = Some(error);
                                }
                            }
                        }
                        let child = named(&definition["type"]);
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
        _operation: Option<&str>,
        http: bool,
        source: &str,
    ) -> Self {
        let planner = Planner {
            ast,
            variables,
            http,
            source,
        };
        let mut out = Self {
            document: ast.clone(),
            variables: variables.clone(),
            source: source.into(),
            http,
            fields: BTreeMap::new(),
            deferred: BTreeSet::new(),
        };
        for (_, op) in ast.operations.iter() {
            planner.collect(&op.node.selection_set.node, "Query", &mut out);
        }
        for fragment in ast.fragments.values() {
            planner.collect(
                &fragment.node.selection_set.node,
                fragment.node.type_condition.node.on.node.as_str(),
                &mut out,
            );
        }
        crate::graphql_input::normalize_document_literals(&mut out.document, http);
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
    pub fn operation(&self, operation: Option<&str>) -> &SelectionSet {
        &self
            .document
            .operations
            .iter()
            .find(|(name, _)| {
                operation.is_none_or(|selected| name.is_some_and(|name| name.as_str() == selected))
            })
            .expect("validated selected operation")
            .1
            .node
            .selection_set
            .node
    }
    pub fn fields(&self, set: &SelectionSet, parent: &str) -> Result<Vec<CollectedField>, Failure> {
        let planner = Planner {
            ast: &self.document,
            variables: &self.variables,
            http: self.http,
            source: &self.source,
        };
        let mut fields = vec![];
        planner.fields(
            set,
            parent,
            &mut BTreeSet::new(),
            &mut fields,
            &mut BTreeMap::new(),
        )?;
        Ok(fields)
    }
    pub fn arguments(&self, field: &Field, definition: &Value) -> Value {
        fn input(value: &Input, variables: &Value) -> Option<Value> {
            match value {
                Input::Variable(name) => variables.get(name.as_str()).cloned(),
                Input::Object(fields) => Some(Value::Object(
                    fields
                        .iter()
                        .filter_map(|(name, value)| {
                            input(value, variables).map(|value| (name.to_string(), value))
                        })
                        .collect(),
                )),
                Input::List(values) => Some(Value::Array(
                    values
                        .iter()
                        .map(|value| input(value, variables).unwrap_or(Value::Null))
                        .collect(),
                )),
                value => Some(
                    serde_json::to_value(value.clone().into_const().expect("constant input"))
                        .expect("input JSON"),
                ),
            }
        }
        let mut args = serde_json::Map::new();
        for argument in definition["args"].as_array().expect("field arguments") {
            let name = argument["name"].as_str().unwrap();
            let value = field
                .get_argument(name)
                .and_then(|value| input(&value.node, &self.variables));
            if let Some(value) = value {
                args.insert(name.into(), value);
            } else if let Some(default) = argument["defaultValue"].as_str() {
                args.insert(name.into(), default_value(default));
            }
        }
        Value::Object(args)
    }
}

pub(crate) struct CollectedField {
    pub field: Positioned<Field>,
    pub locations: Vec<Pos>,
}

pub(crate) fn field_type(definition: &Value) -> Type {
    Type::new(&display(&definition["type"])).expect("frozen field type")
}

fn default_value(text: &str) -> Value {
    // Defaults belong to the immutable native schema, not caller input.
    static DEFAULTS: std::sync::OnceLock<BTreeMap<String, Value>> = std::sync::OnceLock::new();
    DEFAULTS.get_or_init(|| {
        let mut values = BTreeMap::new();
        for ty in crate::introspection::schema()["types"].as_array().unwrap() {
            for field in ty["fields"].as_array().into_iter().flatten() {
                for argument in field["args"].as_array().unwrap() {
                    if let Some(text) = argument["defaultValue"].as_str() {
                        values.entry(text.into()).or_insert_with(|| {
                            let ast = async_graphql_parser::parse_query(format!("{{f(a:{text})}}"))
                                .expect("schema default");
                            let operation = ast.operations.iter().next().unwrap().1;
                            let Selection::Field(field) =
                                &operation.node.selection_set.node.items[0].node
                            else {
                                unreachable!()
                            };
                            serde_json::to_value(
                                field.node.arguments[0].1.node.clone().into_const().unwrap(),
                            )
                            .unwrap()
                        });
                    }
                }
            }
        }
        values
    })[text]
        .clone()
}

/// Classify a recorded string only for GraphQL Int output completion. The frozen
/// Python serializer uses int(str), including Nd digits and single underscores
/// between digits, with its default 4300-digit cap. Keep only the magnitude needed
/// for i32 classification, and validate the whole spelling even after overflow.
/// This is not an arbitrary-precision parser or GraphQL input coercion.
fn recorded_int_string(raw: &str) -> Option<i128> {
    let trimmed = raw.trim();
    let negative = trimmed.starts_with('-');
    let body = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
    let mut digits = 0_u32;
    let mut previous_digit = false;
    let mut magnitude = 0_i128;
    for character in body.chars() {
        if character == '_' && previous_digit {
            previous_digit = false;
            continue;
        }
        let digit = catabolic_core::python_decimal::decimal_digit(character)?;
        digits += 1;
        if digits > 4300 {
            return None;
        }
        // i32::MIN has magnitude i32::MAX + 1, so retain one more sentinel.
        magnitude = (magnitude * 10 + i128::from(digit)).min(i128::from(i32::MAX) + 2);
        previous_digit = true;
    }
    if !previous_digit {
        return None;
    }
    Some(if negative { -magnitude } else { magnitude })
}

/// Complete native scalar/enum leaves without relaxing the schema's output
/// contract. JSON remains opaque; BigInt intentionally uses a decimal string.
pub(crate) fn serialize(name: &str, value: Value, http: bool) -> Result<Value, String> {
    let fail = |prefix: &str| format!("{prefix}{}", crate::graphql_input::inspect(&value));
    match name {
        "JSON" => Ok(value),
        "BigInt" => Ok(Value::String(
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string()),
        )),
        "String" => match &value {
            Value::String(_) => Ok(value),
            Value::Bool(_) | Value::Number(_) => Ok(Value::String(value.to_string())),
            _ => Err(fail("String cannot represent value: ")),
        },
        "ID" => match &value {
            Value::String(_) => Ok(value),
            Value::Number(number) if number.is_i64() || number.is_u64() => {
                Ok(Value::String(number.to_string()))
            }
            Value::Number(number)
                if number
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n.fract() == 0.0) =>
            {
                Ok(Value::String(format!("{:.0}", number.as_f64().unwrap())))
            }
            _ => Err(fail("ID cannot represent value: ")),
        },
        "Boolean" => match &value {
            Value::Bool(_) => Ok(value),
            Value::Number(n) if n.as_f64().is_some_and(f64::is_finite) => {
                Ok(Value::Bool(n.as_f64() != Some(0.0)))
            }
            _ => Err(fail("Boolean cannot represent a non boolean value: ")),
        },
        "Int" => {
            let number = match &value {
                Value::Bool(b) => Some(i128::from(*b)),
                Value::Number(n) => n
                    .as_i64()
                    .map(i128::from)
                    .or_else(|| n.as_u64().map(i128::from))
                    .or_else(|| {
                        n.as_f64()
                            .filter(|n| n.is_finite() && n.fract() == 0.0)
                            .map(|n| n as i128)
                    }),
                Value::String(s) => recorded_int_string(s),
                _ => None,
            }
            .ok_or_else(|| fail("Int cannot represent non-integer value: "))?;
            i32::try_from(number)
                .map(Value::from)
                .map_err(|_| fail("Int cannot represent non 32-bit signed integer value: "))
        }
        "Float" => {
            let number = match &value {
                Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.trim().parse::<f64>().ok(),
                _ => None,
            }
            .filter(|n| n.is_finite())
            .ok_or_else(|| fail("Float cannot represent non numeric value: "))?;
            Ok(serde_json::json!(number))
        }
        _ => {
            if type_def(name, http)
                .and_then(|ty| ty["enumValues"].as_array())
                .is_some_and(|values| values.iter().any(|entry| entry["name"] == value))
            {
                Ok(value)
            } else {
                Err(fail(&format!("Enum '{name}' cannot represent value: ")))
            }
        }
    }
}

/// The framework misses some cross-fragment argument and merged-child conflicts.
/// Refuse incompatible groups before constructing a scoped context or invoking
/// a resolver. Static directives cannot make conflicting groups valid. This is
/// a compatibility guard, not a replacement for full overlap-rule diagnostics.
pub(crate) fn merged_selection_conflict(ast: &ExecutableDocument, http: bool) -> Option<Value> {
    fn collect<'a>(
        ast: &'a ExecutableDocument,
        set: &'a SelectionSet,
        parent: &str,
        seen: &mut BTreeSet<String>,
        groups: &mut BTreeMap<String, Vec<&'a Positioned<Field>>>,
    ) {
        for selection in &set.items {
            // Static compatibility is independent of skip/include conditions.
            match &selection.node {
                Selection::Field(field) => groups
                    .entry(field.node.response_key().node.to_string())
                    .or_default()
                    .push(field),
                Selection::InlineFragment(fragment)
                    if fragment
                        .node
                        .type_condition
                        .as_ref()
                        .is_none_or(|c| c.node.on.node == parent) =>
                {
                    collect(ast, &fragment.node.selection_set.node, parent, seen, groups)
                }
                Selection::FragmentSpread(spread) => {
                    let name = spread.node.fragment_name.node.as_str();
                    if seen.insert(name.into())
                        && let Some(fragment) = ast.fragments.get(name)
                        && fragment.node.type_condition.node.on.node == parent
                    {
                        collect(ast, &fragment.node.selection_set.node, parent, seen, groups);
                    }
                }
                _ => (),
            }
        }
    }
    fn walk(
        ast: &ExecutableDocument,
        set: &SelectionSet,
        parent: &str,
        http: bool,
    ) -> Option<Value> {
        let mut groups = BTreeMap::new();
        collect(ast, set, parent, &mut BTreeSet::new(), &mut groups);
        for (key, fields) in groups {
            let first = &fields[0].node;
            let arguments = |field: &Field| {
                field
                    .arguments
                    .iter()
                    .map(|(name, value)| (name.node.to_string(), value.node.clone()))
                    .collect::<BTreeMap<_, _>>()
            };
            let incompatible = fields.iter().skip(1).find(|field| {
                field.node.name.node != first.name.node
                    || arguments(&field.node) != arguments(first)
            });
            if let Some(other) = incompatible {
                return Some(
                    serde_json::json!({"data":null,"errors":[{"message":format!("Fields '{key}' conflict within merged selections. Use different aliases on the fields to fetch both if this was intentional."),"locations":[{"line":fields[0].pos.line,"column":fields[0].pos.column},{"line":other.pos.line,"column":other.pos.column}]}]}),
                );
            }
            let child = field_def(parent, first.name.node.as_str(), http)
                .map(|field| named(&field["type"]).to_owned())
                .unwrap_or_default();
            let combined_set = SelectionSet {
                items: fields
                    .iter()
                    .flat_map(|field| field.node.selection_set.node.items.clone())
                    .collect(),
            };
            if let Some(error) = walk(ast, &combined_set, &child, http) {
                return Some(error);
            }
        }
        None
    }
    for (_, operation) in ast.operations.iter() {
        if let Some(error) = walk(ast, &operation.node.selection_set.node, "Query", http) {
            return Some(error);
        }
    }
    None
}

#[cfg(test)]
mod recorded_int_tests {
    use super::{recorded_int_string, serialize};
    use serde_json::{Value, json};

    #[test]
    fn recorded_integer_output_is_bounded_without_changing_other_scalar_branches() {
        for http in [false, true] {
            for (raw, expected) in [
                ("0", 0),
                ("+1", 1),
                ("-1", -1),
                ("  12\t", 12),
                ("2147483647", i32::MAX),
                ("-2147483648", i32::MIN),
                ("1_000", 1000),
                ("١٢٣", 123),
                ("00000000000000000000000000000000000000000001", 1),
            ] {
                assert_eq!(serialize("Int", json!(raw), http), Ok(json!(expected)));
            }
            for raw in ["2147483648", "-2147483649", "9".repeat(100).as_str()] {
                assert!(
                    serialize("Int", json!(raw), http)
                        .unwrap_err()
                        .starts_with("Int cannot represent non 32-bit signed integer value: ")
                );
            }
            for value in [json!(true), json!(false), json!(1), json!(1.0)] {
                assert!(serialize("Int", value, http).is_ok());
            }
            for value in [Value::Null, json!(1.5), json!([]), json!({})] {
                assert!(
                    serialize("Int", value, http)
                        .unwrap_err()
                        .starts_with("Int cannot represent non-integer value: ")
                );
            }
            assert_eq!(serialize("JSON", json!("١٢٣"), http), Ok(json!("١٢٣")));
            assert_eq!(
                serialize("BigInt", json!("1_000"), http),
                Ok(json!("1_000"))
            );
            assert_eq!(
                serialize("String", json!("1_000"), http),
                Ok(json!("1_000"))
            );
            assert!(serialize("Float", json!("1_000"), http).is_err());
        }
        assert_eq!(recorded_int_string(&"0".repeat(4300)), Some(0));
        assert_eq!(recorded_int_string(&"0".repeat(4301)), None);
        assert_eq!(recorded_int_string(&format!("{}x", "9".repeat(4300))), None);
        assert_eq!(recorded_int_string(&format!("{}_", "9".repeat(4300))), None);
    }
}
