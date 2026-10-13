// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Variable input coercion before any resolver runs. GraphQL-core's error order,
//! paths and inspect formatting are public contract, not executor diagnostics.
use async_graphql_parser::{
    parse_schema,
    types::{BaseType, OperationDefinition, Type, TypeKind, TypeSystemDefinition},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::OnceLock};

/// GraphQL-specific transport value. Existing Rust Value callers retain their
/// supplied map order; raw JSON callers also retain original diagnostic order.
/// Canonical encoding and every unrelated serde_json map remain unchanged.
#[derive(Clone, Debug)]
pub struct InputVariables {
    pub(crate) value: Value,
    order: Order,
}
impl From<Value> for InputVariables {
    fn from(value: Value) -> Self {
        Self {
            value,
            order: Order::Scalar,
        }
    }
}
impl InputVariables {
    pub fn from_json(raw: &str) -> crate::Result<Self> {
        let value = catabolic_core::decode_json(raw)?;
        // Only record order after the hardened decoder accepted all values.
        // This scanner never parses a number or recognizes private sentinels.
        let order = Order::read(raw, &mut 0);
        Ok(Self { value, order })
    }
    /// Select a nested variables object from a bounded request envelope.
    pub fn from_request_json(raw: &str, key: &str) -> crate::Result<Self> {
        let mut request = Self::from_json(raw)?;
        let value = request
            .value
            .get_mut(key)
            .map(Value::take)
            .unwrap_or_else(|| json!({}));
        let order = request.order.child(key).cloned().unwrap_or(Order::Scalar);
        Ok(Self { value, order })
    }
}
#[derive(Clone, Debug)]
enum Order {
    Scalar,
    Object(Vec<(String, Order)>),
    List(Vec<Order>),
}
impl Order {
    fn child(&self, key: &str) -> Option<&Self> {
        if let Self::Object(fields) = self {
            fields
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, value)| value)
        } else {
            None
        }
    }
    fn index(&self, index: usize) -> Option<&Self> {
        if let Self::List(values) = self {
            values.get(index)
        } else {
            None
        }
    }
    fn keys<'a>(&'a self, value: &'a serde_json::Map<String, Value>) -> Vec<&'a String> {
        if let Self::Object(fields) = self {
            fields.iter().map(|(key, _)| key).collect()
        } else {
            value.keys().collect()
        }
    }
    fn read(raw: &str, at: &mut usize) -> Self {
        fn ws(raw: &str, at: &mut usize) {
            while raw.as_bytes().get(*at).is_some_and(u8::is_ascii_whitespace) {
                *at += 1;
            }
        }
        fn string(raw: &str, at: &mut usize) -> String {
            let start = *at;
            *at += 1;
            while raw.as_bytes()[*at] != b'"' {
                if raw.as_bytes()[*at] == b'\\' {
                    *at += 1;
                }
                *at += 1;
            }
            *at += 1;
            serde_json::from_str(&raw[start..*at]).expect("already validated JSON string")
        }
        ws(raw, at);
        match raw.as_bytes()[*at] {
            b'{' => {
                *at += 1;
                ws(raw, at);
                let mut fields: Vec<(String, Self)> = vec![];
                let mut indices = BTreeMap::<String, usize>::new();
                while raw.as_bytes()[*at] != b'}' {
                    let key = string(raw, at);
                    ws(raw, at);
                    *at += 1;
                    let value = Self::read(raw, at);
                    // Python json.loads replaces the value in place on duplicates.
                    if let Some(&index) = indices.get(&key) {
                        fields[index].1 = value;
                    } else {
                        indices.insert(key.clone(), fields.len());
                        fields.push((key, value));
                    }
                    ws(raw, at);
                    if raw.as_bytes()[*at] == b',' {
                        *at += 1;
                        ws(raw, at);
                    } else {
                        break;
                    }
                }
                *at += 1;
                Self::Object(fields)
            }
            b'[' => {
                *at += 1;
                ws(raw, at);
                let mut values = vec![];
                while raw.as_bytes()[*at] != b']' {
                    values.push(Self::read(raw, at));
                    ws(raw, at);
                    if raw.as_bytes()[*at] == b',' {
                        *at += 1;
                        ws(raw, at);
                    } else {
                        break;
                    }
                }
                *at += 1;
                Self::List(values)
            }
            b'"' => {
                string(raw, at);
                Self::Scalar
            }
            _ => {
                while raw
                    .as_bytes()
                    .get(*at)
                    .is_some_and(|c| !b",]} \r\n\t".contains(c))
                {
                    *at += 1;
                }
                Self::Scalar
            }
        }
    }
}

fn types() -> &'static BTreeMap<String, TypeKind> {
    static TYPES: OnceLock<BTreeMap<String, TypeKind>> = OnceLock::new();
    TYPES.get_or_init(|| {
        parse_schema(include_str!("../resources/schema.graphql"))
            .expect("frozen GraphQL schema")
            .definitions
            .into_iter()
            .filter_map(|definition| {
                if let TypeSystemDefinition::Type(definition) = definition {
                    Some((definition.node.name.node.to_string(), definition.node.kind))
                } else {
                    None
                }
            })
            .collect()
    })
}
fn integer_number(value: &Value) -> bool {
    value
        .as_number()
        .is_some_and(|n| !n.to_string().contains(['.', 'e', 'E']))
}
fn integral_float(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite() && v.fract() == 0.0)
}
fn truncate(value: String) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    if chars.len() > 240 {
        chars[..118]
            .iter()
            .chain(['.', '.', '.'].iter())
            .chain(chars[chars.len() - 119..].iter())
            .collect()
    } else {
        value
    }
}
include!("graphql_printable.rs");
fn python_printable(c: char) -> bool {
    let c = c as u32;
    let i = NONPRINTABLE.partition_point(|(_, end)| *end < c);
    NONPRINTABLE.get(i).is_none_or(|(start, _)| c < *start)
}
fn string_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = quote.to_string();
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !python_printable(c) => {
                let n = c as u32;
                if n <= 255 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n <= 65535 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    truncate(out)
}
/// Bounded graphql-core inspect for JSON values. Container order is input order;
/// canonical JSON encoding is deliberately not used for objects or strings.
pub(crate) fn inspect(value: &Value) -> String {
    inspect_at(value, 0, &Order::Scalar)
}
fn inspect_at(value: &Value, depth: usize, order: &Order) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(number) if integer_number(value) => truncate(number.to_string()),
        Value::Number(_) => catabolic_core::encode(value),
        Value::String(value) => string_repr(value),
        Value::Array(values) => {
            if depth >= 2 && !values.is_empty() {
                return "[...]".into();
            }
            let mut items = vec![];
            for (i, value) in values.iter().enumerate() {
                if values.len() > 10 && (5..values.len() - 4).contains(&i) {
                    if i == 5 {
                        items.push("...".into());
                    }
                    continue;
                }
                items.push(inspect_at(
                    value,
                    depth + 1,
                    order.index(i).unwrap_or(&Order::Scalar),
                ));
            }
            format!("[{}]", items.join(", "))
        }
        Value::Object(values) => {
            if depth >= 2 && !values.is_empty() {
                return "{...}".into();
            }
            let mut items = vec![];
            for (i, key) in order.keys(values).into_iter().enumerate() {
                let value = &values[key];
                if values.len() > 10 && (5..values.len() - 4).contains(&i) {
                    if i == 5 {
                        items.push("...".into());
                    }
                    continue;
                }
                items.push(format!(
                    "{}: {}",
                    string_repr(key),
                    inspect_at(value, depth + 1, order.child(key).unwrap_or(&Order::Scalar))
                ));
            }
            format!("{{{}}}", items.join(", "))
        }
    }
}
struct Coercer<'a> {
    definitions: &'a BTreeMap<String, TypeKind>,
    errors: Vec<Value>,
    name: &'a str,
    position: async_graphql_parser::Pos,
}
impl Coercer<'_> {
    fn full(&self) -> bool {
        self.errors.len() > 50
    }
    fn error(&mut self, message: String) {
        if self.full() {
            return;
        }
        if self.errors.len() == 50 {
            self.errors.push(json!({"message":"Too many errors processing variables, error limit reached. Execution aborted."}));
        } else {
            self.errors.push(json!({"message":message,"locations":[{"line":self.position.line,"column":self.position.column}]}));
        }
    }
    fn invalid(&mut self, value: &Value, path: &str, order: &Order, reason: String) {
        let location = if path.is_empty() {
            String::new()
        } else {
            format!(" at '{}{path}'", self.name)
        };
        self.error(format!(
            "Variable '${}' got invalid value {}{location}; {reason}",
            self.name,
            inspect_at(value, 0, order)
        ));
    }
    fn coerce(&mut self, value: &Value, ty: &Type, path: &str, order: &Order) -> Value {
        if self.full() {
            return Value::Null;
        }
        if value.is_null() {
            if !ty.nullable {
                self.invalid(
                    value,
                    path,
                    order,
                    format!("Expected non-nullable type '{ty}' not to be None."),
                );
            }
            return Value::Null;
        }
        match &ty.base {
            BaseType::List(inner) => {
                if let Some(values) = value.as_array() {
                    let mut out = Vec::with_capacity(values.len().min(50));
                    for (index, value) in values.iter().enumerate() {
                        out.push(self.coerce(
                            value,
                            inner,
                            &format!("{path}[{index}]"),
                            order.index(index).unwrap_or(&Order::Scalar),
                        ));
                        if self.full() {
                            break;
                        }
                    }
                    Value::Array(out)
                } else {
                    Value::Array(vec![self.coerce(value, inner, path, order)])
                }
            }
            BaseType::Named(name) => match self.definitions.get(name.as_str()) {
                Some(TypeKind::InputObject(object)) => {
                    let Some(values) = value.as_object() else {
                        self.invalid(
                            value,
                            path,
                            order,
                            format!("Expected type '{name}' to be a mapping."),
                        );
                        return Value::Null;
                    };
                    let mut result = serde_json::Map::new();
                    for field in &object.fields {
                        let field = &field.node;
                        let key = field.name.node.as_str();
                        if let Some(value) = values.get(key) {
                            result.insert(
                                key.into(),
                                self.coerce(
                                    value,
                                    &field.ty.node,
                                    &format!("{path}.{key}"),
                                    order.child(key).unwrap_or(&Order::Scalar),
                                ),
                            );
                        } else if let Some(default) = &field.default_value {
                            let value =
                                serde_json::to_value(&default.node).expect("constant default");
                            result.insert(
                                key.into(),
                                self.coerce(
                                    &value,
                                    &field.ty.node,
                                    &format!("{path}.{key}"),
                                    &Order::Scalar,
                                ),
                            );
                        } else if !field.ty.node.nullable {
                            self.invalid(
                                value,
                                path,
                                order,
                                format!(
                                    "Field '{key}' of required type '{}' was not provided.",
                                    field.ty.node
                                ),
                            );
                        }
                        if self.full() {
                            break;
                        }
                    }
                    for key in order.keys(values) {
                        if self.full() {
                            break;
                        }
                        if !object
                            .fields
                            .iter()
                            .any(|field| field.node.name.node.as_str() == key)
                        {
                            let fields = json!(
                                object
                                    .fields
                                    .iter()
                                    .map(|field| json!({"name":field.node.name.node.as_str()}))
                                    .collect::<Vec<_>>()
                            );
                            self.invalid(
                                value,
                                path,
                                order,
                                format!(
                                    "Field '{key}' is not defined by type '{name}'.{}",
                                    crate::introspection::argument_suggestions(&fields, key)
                                ),
                            );
                        }
                    }
                    Value::Object(result)
                }
                Some(TypeKind::Enum(enumeration)) => {
                    if value
                        .as_str()
                        .is_some_and(|v| enumeration.values.iter().any(|e| e.node.value.node == v))
                    {
                        return value.clone();
                    }
                    let rendered = inspect_at(value, 0, order);
                    let unknown = value.as_str().unwrap_or(&rendered);
                    let fields = json!(
                        enumeration
                            .values
                            .iter()
                            .map(|item| json!({"name":item.node.value.node.as_str()}))
                            .collect::<Vec<_>>()
                    );
                    let suggestion = crate::introspection::argument_suggestions(&fields, unknown)
                        .replace("Did you mean", "Did you mean the enum value");
                    let reason = if let Some(value) = value.as_str() {
                        format!("Value '{value}' does not exist in '{name}' enum.{suggestion}")
                    } else {
                        format!(
                            "Enum '{name}' cannot represent non-string value: {rendered}.{suggestion}"
                        )
                    };
                    self.invalid(value, path, order, reason);
                    Value::Null
                }
                _ => {
                    let rendered = inspect_at(value, 0, order);
                    let invalid = match name.as_str() {
                        "Int" if !integer_number(value) && integral_float(value).is_none() => Some(
                            format!("Int cannot represent non-integer value: {rendered}"),
                        ),
                        "Int"
                            if if integer_number(value) {
                                value
                                    .as_number()
                                    .unwrap()
                                    .to_string()
                                    .parse::<i32>()
                                    .is_err()
                            } else {
                                integral_float(value).is_some_and(|v| {
                                    !(f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&v)
                                })
                            } =>
                        {
                            Some(format!(
                                "Int cannot represent non 32-bit signed integer value: {rendered}"
                            ))
                        }
                        "String" if !value.is_string() => Some(format!(
                            "String cannot represent a non string value: {rendered}"
                        )),
                        "Boolean" if !value.is_boolean() => Some(format!(
                            "Boolean cannot represent a non boolean value: {rendered}"
                        )),
                        "Float" if !value.is_number() => Some(format!(
                            "Float cannot represent non numeric value: {rendered}"
                        )),
                        "Float" if value.as_f64().is_none_or(|n| !n.is_finite()) => {
                            Some("Expected type 'Float'. int too large to convert to float".into())
                        }
                        "ID" if !value.is_string()
                            && !integer_number(value)
                            && integral_float(value).is_none() =>
                        {
                            Some(format!("ID cannot represent value: {rendered}"))
                        }
                        _ => None,
                    };
                    if let Some(reason) = invalid {
                        self.invalid(value, path, order, reason);
                        return Value::Null;
                    }
                    match name.as_str() {
                        "Int" => json!(if integer_number(value) {
                            value
                                .as_number()
                                .unwrap()
                                .to_string()
                                .parse::<i32>()
                                .unwrap()
                        } else {
                            integral_float(value).unwrap() as i32
                        }),
                        "ID" if integer_number(value) => {
                            json!(value.as_number().unwrap().to_string())
                        }
                        "ID" if integral_float(value).is_some() => {
                            let number = integral_float(value).unwrap();
                            json!(if number == 0.0 {
                                "0".into()
                            } else {
                                format!("{number:.0}")
                            })
                        }
                        "Float" => json!(value.as_f64().unwrap()),
                        _ => value.clone(),
                    }
                }
            },
        }
    }
}
/// The caller selects one operation only, after complete document validation.
pub(crate) fn variables(
    operation: &OperationDefinition,
    inputs: &InputVariables,
) -> (Value, Vec<Value>) {
    let values = &inputs.value;
    let mut result = serde_json::Map::new();
    let mut coercer = Coercer {
        definitions: types(),
        errors: vec![],
        name: "",
        position: async_graphql_parser::Pos::default(),
    };
    for definition in &operation.variable_definitions {
        coercer.name = definition.node.name.node.as_str();
        coercer.position = definition.pos;
        let ty = &definition.node.var_type.node;
        if let Some(value) = values.get(coercer.name) {
            if value.is_null() && !ty.nullable {
                coercer.error(format!(
                    "Variable '${}' of non-null type '{ty}' must not be null.",
                    coercer.name
                ));
            } else {
                result.insert(
                    coercer.name.into(),
                    coercer.coerce(
                        value,
                        ty,
                        "",
                        inputs.order.child(coercer.name).unwrap_or(&Order::Scalar),
                    ),
                );
            }
        } else if let Some(default) = &definition.node.default_value {
            let value = serde_json::to_value(&default.node).expect("constant variable default");
            result.insert(
                coercer.name.into(),
                coercer.coerce(&value, ty, "", &Order::Scalar),
            );
        } else if !ty.nullable {
            coercer.error(format!(
                "Variable '${}' of required type '{ty}' was not provided.",
                coercer.name
            ));
        }
        if coercer.full() {
            break;
        }
    }
    (Value::Object(result), coercer.errors)
}

/// Normalize already validated literal inputs before handing the AST to the
/// framework. Its integer-width check must not reject a valid huge ID default
/// or round a numeric ID literal. Locations and the original public document
/// are retained; all error validation ran against that original document.
pub(crate) fn normalize_document_literals(
    document: &mut async_graphql_parser::types::ExecutableDocument,
    http: bool,
) {
    use async_graphql_parser::types::{DocumentOperations, Selection, SelectionSet};
    use async_graphql_value::Value as Input;
    fn normalize(value: &mut Input, ty: &Type) {
        if matches!(value, Input::Variable(_) | Input::Null) {
            return;
        }
        match &ty.base {
            BaseType::List(inner) => {
                if let Input::List(values) = value {
                    for value in values {
                        normalize(value, inner);
                    }
                } else {
                    normalize(value, inner);
                    let item = std::mem::replace(value, Input::Null);
                    *value = Input::List(vec![item]);
                }
            }
            BaseType::Named(name) if name == "ID" => {
                if let Input::Number(number) = value {
                    *value = Input::String(number.to_string());
                }
            }
            BaseType::Named(name) => {
                if let (Input::Object(values), Some(TypeKind::InputObject(definition))) =
                    (value, types().get(name.as_str()))
                {
                    for field in &definition.fields {
                        if let Some(value) = values.get_mut(&field.node.name.node) {
                            normalize(value, &field.node.ty.node);
                        }
                    }
                }
            }
        }
    }
    fn schema_type(value: &Value) -> String {
        match value["kind"].as_str() {
            Some("NON_NULL") => format!("{}!", schema_type(&value["ofType"])),
            Some("LIST") => format!("[{}]", schema_type(&value["ofType"])),
            _ => value["name"].as_str().unwrap_or("").into(),
        }
    }
    fn base_name(mut value: &Value) -> &str {
        while value["ofType"].is_object() {
            value = &value["ofType"];
        }
        value["name"].as_str().unwrap_or("")
    }
    fn walk(set: &mut SelectionSet, parent: &str, http: bool) {
        let definition = crate::introspection::schema_for(http)["types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["name"] == parent);
        for selection in &mut set.items {
            match &mut selection.node {
                Selection::Field(field) => {
                    let name = field.node.name.node.as_str();
                    let field_definition = definition
                        .and_then(|value| value["fields"].as_array())
                        .and_then(|fields| fields.iter().find(|value| value["name"] == name));
                    if let Some(definition) = field_definition {
                        for (name, value) in &mut field.node.arguments {
                            if let Some(argument) = definition["args"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .find(|argument| argument["name"] == name.node.as_str())
                            {
                                normalize(
                                    &mut value.node,
                                    &Type::new(&schema_type(&argument["type"]))
                                        .expect("frozen argument type"),
                                );
                            }
                        }
                        walk(
                            &mut field.node.selection_set.node,
                            base_name(&definition["type"]),
                            http,
                        );
                    } else if parent == "Query" && matches!(name, "__schema" | "__type") {
                        let child = if name == "__schema" {
                            "__Schema"
                        } else {
                            "__Type"
                        };
                        walk(&mut field.node.selection_set.node, child, http);
                    }
                }
                Selection::InlineFragment(fragment) => {
                    let child = fragment
                        .node
                        .type_condition
                        .as_ref()
                        .map(|value| value.node.on.node.as_str())
                        .unwrap_or(parent);
                    walk(&mut fragment.node.selection_set.node, child, http);
                }
                Selection::FragmentSpread(_) => {}
            }
        }
    }
    fn operation(operation: &mut OperationDefinition, http: bool) {
        for variable in &mut operation.variable_definitions {
            if let Some(default) = &mut variable.node.default_value {
                let mut value = default.node.clone().into_value();
                normalize(&mut value, &variable.node.var_type.node);
                default.node = value.into_const().expect("constant variable default");
            }
        }
        walk(&mut operation.selection_set.node, "Query", http);
    }
    match &mut document.operations {
        DocumentOperations::Single(op) => operation(&mut op.node, http),
        DocumentOperations::Multiple(operations) => {
            for op in operations.values_mut() {
                operation(&mut op.node, http);
            }
        }
    }
    for fragment in document.fragments.values_mut() {
        walk(
            &mut fragment.node.selection_set.node,
            fragment.node.type_condition.node.on.node.as_str(),
            http,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_duplicate_key_order_without_number_sentinel_confusion() {
        let inputs = InputVariables::from_json(r#"{"f":{"zzz":1,"aaa":2,"zzz":3,"$serde_json::private::Number":"123"},"id":1237940039285380274899124225}"#).unwrap();
        assert_eq!(
            inspect_at(&inputs.value["f"], 0, inputs.order.child("f").unwrap()),
            "{'zzz': 3, 'aaa': 2, '$serde_json::private::Number': '123'}"
        );
        assert!(inputs.value["f"].is_object());
        assert_eq!(
            inputs.value["id"].to_string(),
            "1237940039285380274899124225"
        );
        assert!(InputVariables::from_json(r#"{"x":1e400}"#).is_err());
    }
    #[test]
    fn synthetic_nested_required_default_and_singleton_coercion() {
        // Frozen public SDL currently has one flat, all-optional input object.
        // Exercise generic nested/required/default behavior without exposing a
        // test schema or changing production schema to manufacture reachability.
        let document = parse_schema(
            "input Inner { required: String! id: ID = 123 } input Outer { children: [Inner!]! }",
        )
        .unwrap();
        let definitions = document
            .definitions
            .into_iter()
            .filter_map(|d| {
                if let TypeSystemDefinition::Type(d) = d {
                    Some((d.node.name.node.to_string(), d.node.kind))
                } else {
                    None
                }
            })
            .collect();
        let mut coercer = Coercer {
            definitions: &definitions,
            errors: vec![],
            name: "v",
            position: Default::default(),
        };
        let inputs =
            InputVariables::from_json(r#"{"children":[{},null,{"required":1,"zzz":true}]}"#)
                .unwrap();
        coercer.coerce(
            &inputs.value,
            &Type::new("Outer").unwrap(),
            "",
            &inputs.order,
        );
        let messages = coercer
            .errors
            .iter()
            .map(|e| e["message"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            [
                "Variable '$v' got invalid value {} at 'v.children[0]'; Field 'required' of required type 'String!' was not provided.",
                "Variable '$v' got invalid value None at 'v.children[1]'; Expected non-nullable type 'Inner!' not to be None.",
                "Variable '$v' got invalid value 1 at 'v.children[2].required'; String cannot represent a non string value: 1",
                "Variable '$v' got invalid value {'required': 1, 'zzz': True} at 'v.children[2]'; Field 'zzz' is not defined by type 'Inner'.",
            ]
        );
        coercer.errors.clear();
        let value = coercer.coerce(
            &json!({"children":{"required":"yes"}}),
            &Type::new("Outer").unwrap(),
            "",
            &Order::Scalar,
        );
        assert_eq!(value, json!({"children":[{"required":"yes","id":"123"}]}));
        assert!(coercer.errors.is_empty());
    }
    #[test]
    fn inspect_depth_count_and_string_bounds() {
        assert_eq!(
            inspect(&json!([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10])),
            "[0, 1, 2, 3, 4, ..., 7, 8, 9, 10]"
        );
        assert_eq!(
            inspect(&json!({"a":{"b":{"c":true}}})),
            "{'a': {'b': {...}}}"
        );
        assert_eq!(inspect(&json!("雪".repeat(241))).chars().count(), 240);
        assert_eq!(
            inspect(&json!("\u{a0}\u{200b}\u{e000}")),
            "'\\xa0\\u200b\\ue000'"
        );
    }
}
