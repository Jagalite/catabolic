//! Contract validation before framework execution. Locations and wording belong
//! to the frozen public GraphQL contract, rather than a framework implementation.
use async_graphql_parser::{
    Pos, Positioned,
    types::{
        BaseType, Directive, ExecutableDocument, OperationDefinition, OperationType, Selection,
        SelectionSet, Type,
    },
};
use async_graphql_value::{ConstValue, Name, Value as Input};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

fn ty_for(name: &str, http: bool) -> Option<&'static Value> {
    crate::introspection::schema_for(http)["types"]
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
fn nullable(value: &Value) -> &Value {
    if value["kind"] == "NON_NULL" {
        &value["ofType"]
    } else {
        value
    }
}
fn display(value: &Value) -> String {
    match value["kind"].as_str() {
        Some("NON_NULL") => format!("{}!", display(&value["ofType"])),
        Some("LIST") => format!("[{}]", display(&value["ofType"])),
        _ => value["name"].as_str().unwrap_or("").into(),
    }
}
fn type_value(value: &Type, http: bool) -> Value {
    let base = match &value.base {
        BaseType::Named(name) => {
            json!({"name":name.as_str(),"kind":ty_for(name.as_str(), http).map(|ty| &ty["kind"])})
        }
        BaseType::List(child) => json!({"kind":"LIST","ofType":type_value(child, http)}),
    };
    if value.nullable {
        base
    } else {
        json!({"kind":"NON_NULL","ofType":base})
    }
}
fn field_definition(parent: &str, name: &str, http: bool) -> Option<Value> {
    match name {
        "__typename" if ty_for(parent, http).is_some_and(composite) => Some(
            json!({"name":name,"type":{"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"String"}},"args":[]}),
        ),
        "__schema" if parent == "Query" => Some(
            json!({"name":name,"type":{"kind":"NON_NULL","ofType":{"kind":"OBJECT","name":"__Schema"}},"args":[]}),
        ),
        "__type" if parent == "Query" => Some(
            json!({"name":name,"type":{"kind":"OBJECT","name":"__Type"},"args":[{"name":"name","type":{"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"String"}}}]}),
        ),
        _ => ty_for(parent, http)
            .and_then(|ty| ty["fields"].as_array())
            .and_then(|fields| fields.iter().find(|field| field["name"] == name))
            .cloned(),
    }
}
fn composite(ty: &Value) -> bool {
    matches!(ty["kind"].as_str(), Some("OBJECT" | "INTERFACE" | "UNION"))
}
fn directive_definition(name: &str, http: bool) -> Option<&'static Value> {
    crate::introspection::schema_for(http)["directives"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == name)
}
fn argument<'a>(args: &'a Value, name: &str) -> Option<&'a Value> {
    args.as_array()
        .and_then(|args| args.iter().find(|arg| arg["name"] == name))
}
fn has_default(def: &Value) -> bool {
    def["defaultValue"].is_string()
}

// The parser retains positions only for the outer input value. A small lexical
// position index restores child positions without reparsing or evaluating input.
// Strings and comments are single tokens, so their punctuation cannot become uses.
#[derive(Clone)]
struct Token {
    text: String,
    pos: Pos,
}
fn tokens(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let (mut i, mut line, mut column) = (0, 1, 1);
    let mut result = vec![];
    while i < chars.len() {
        let start = i;
        let pos = Pos { line, column };
        let c = chars[i];
        if c == '#' {
            while i < chars.len() && !matches!(chars[i], '\r' | '\n') {
                i += 1;
            }
        } else if c == '"' {
            if chars.get(i..i + 3) == Some(&['"', '"', '"'][..]) {
                i += 3;
                while i < chars.len() {
                    if chars[i] == '\\' && chars.get(i + 1..i + 4) == Some(&['"', '"', '"'][..]) {
                        i += 4;
                    } else if chars.get(i..i + 3) == Some(&['"', '"', '"'][..]) {
                        i += 3;
                        break;
                    } else {
                        i += 1;
                    }
                }
            } else {
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\\' {
                        i = (i + 2).min(chars.len());
                    } else if chars[i] == '"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
        } else if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '.' | '+' | '-'))
            {
                i += 1;
            }
        } else {
            i += 1;
        }
        if !c.is_whitespace() && c != '\u{feff}' && c != ',' && c != '#' {
            result.push(Token {
                text: chars[start..i].iter().collect(),
                pos,
            });
        }
        for j in start..i {
            if chars[j] == '\r' || chars[j] == '\n' && (j == 0 || chars[j - 1] != '\r') {
                line += 1;
                column = 1;
            } else if chars[j] != '\n' {
                column += 1;
            }
        }
    }
    result
}
struct Located<'a> {
    value: &'a Input,
    pos: Pos,
    scalar: Option<String>,
    children: Vec<Located<'a>>,
    fields: Vec<(String, Pos)>,
}

// Input's Display loses number spelling and block-string node kind. These are
// part of graphql-core's print_ast diagnostics, so use the accepted source token
// for lexical details and the parsed tree for list/object structure.
fn print_string(value: &str) -> String {
    let mut result = String::from("\"");
    for c in value.chars() {
        match c {
            '\"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\u{8}' => result.push_str("\\b"),
            '\u{c}' => result.push_str("\\f"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' => {
                result.push_str(&format!("\\u{:04X}", c as u32));
            }
            _ => result.push(c),
        }
    }
    result.push('"');
    result
}

fn print_block_string(value: &str) -> String {
    // The parser keeps the block-string escape in its value. graphql-core
    // removes that one backslash before its printer escapes triple quotes.
    let value = value.replace("\\\"\"\"", "\"\"\"");
    let escaped = value.replace("\"\"\"", "\\\"\"\"");

    // Python str.splitlines includes these additional separators. They affect
    // only printing layout here, not GraphQL's source-location line counting.
    let mut lines = vec![];
    let mut chars = escaped.char_indices().peekable();
    let mut start = 0;
    while let Some((index, c)) = chars.next() {
        if matches!(
            c,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'
                ..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            lines.push(&escaped[start..index]);
            start = index + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|(_, c)| *c == '\n') {
                let (index, _) = chars.next().unwrap();
                start = index + 1;
            }
        }
    }
    if start < escaped.len() {
        lines.push(&escaped[start..]);
    }
    if lines.is_empty() {
        lines.push("");
    }
    let single_line = lines.len() == 1;
    let force_leading = !single_line
        && lines[1..]
            .iter()
            .all(|line| line.is_empty() || line.starts_with([' ', '\t']));
    let trailing_triple_quotes = escaped.ends_with("\\\"\"\"");
    let force_trailing = value.ends_with('"') && !trailing_triple_quotes || value.ends_with('\\');
    let multiple_lines = !single_line
        || value.chars().count() > 70
        || force_trailing
        || force_leading
        || trailing_triple_quotes;
    let skip_leading = single_line && value.starts_with([' ', '\t']);
    let before = if multiple_lines && !skip_leading || force_leading {
        "\n"
    } else {
        ""
    };
    let after = if multiple_lines || force_trailing {
        "\n"
    } else {
        ""
    };
    format!("\"\"\"{before}{escaped}{after}\"\"\"")
}

fn render_located(value: &Located<'_>) -> String {
    if let Some(scalar) = &value.scalar {
        return scalar.clone();
    }
    match value.value {
        Input::List(_) => format!(
            "[{}]",
            value
                .children
                .iter()
                .map(render_located)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Input::Object(_) => format!(
            "{{{}}}",
            value
                .fields
                .iter()
                .zip(&value.children)
                .map(|((name, _), child)| format!("{name}: {}", render_located(child)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => value.value.to_string(),
    }
}

pub(crate) fn render_input(document: &str, value: &Positioned<Input>) -> String {
    let tokens = tokens(document);
    let mut cursor =
        tokens.partition_point(|t| (t.pos.line, t.pos.column) < (value.pos.line, value.pos.column));
    render_located(&locate(&value.node, &tokens, &mut cursor, value.pos))
}

fn locate<'a>(
    value: &'a Input,
    tokens: &[Token],
    cursor: &mut usize,
    fallback: Pos,
) -> Located<'a> {
    let token = tokens.get(*cursor);
    let pos = token.map_or(fallback, |t| t.pos);
    let scalar = match value {
        Input::Number(_) => token.map(|t| t.text.clone()),
        Input::String(value) => Some(if token.is_some_and(|t| t.text.starts_with("\"\"\"")) {
            print_block_string(value)
        } else {
            print_string(value)
        }),
        _ => None,
    };
    *cursor += 1;
    let mut result = Located {
        value,
        pos,
        scalar,
        children: vec![],
        fields: vec![],
    };
    match value {
        Input::Variable(_) => *cursor += 1,
        Input::List(values) => {
            for value in values {
                result.children.push(locate(value, tokens, cursor, pos));
            }
            *cursor += 1;
        }
        Input::Object(values) => {
            for (name, value) in values {
                let field_pos = tokens.get(*cursor).map_or(pos, |t| t.pos);
                result.fields.push((name.to_string(), field_pos));
                *cursor += 2;
                result.children.push(locate(value, tokens, cursor, pos));
            }
            *cursor += 1;
        }
        _ => {}
    }
    result
}
#[derive(Clone)]
struct Usage {
    name: String,
    pos: Pos,
    expected: Option<Value>,
    has_default: bool,
}
struct Validator<'a> {
    doc: &'a ExecutableDocument,
    tokens: Vec<Token>,
    http: bool,
    out: Vec<Value>,
}
impl Validator<'_> {
    fn error(&mut self, message: String, positions: &[Pos]) {
        self.out.push(json!({"message":message,"locations":positions.iter().map(|p|json!({"line":p.line,"column":p.column})).collect::<Vec<_>>()}));
    }
    fn located<'a>(&self, value: &'a Positioned<Input>) -> Located<'a> {
        let mut cursor = self
            .tokens
            .partition_point(|t| (t.pos.line, t.pos.column) < (value.pos.line, value.pos.column));
        locate(&value.node, &self.tokens, &mut cursor, value.pos)
    }
    fn named_type_pos(&self, pos: Pos) -> Pos {
        self.tokens
            .iter()
            .skip_while(|t| (t.pos.line, t.pos.column) < (pos.line, pos.column))
            .find(|t| t.text != "[")
            .map_or(pos, |t| t.pos)
    }
    fn known_type(&mut self, name: &str, pos: Pos) {
        if ty_for(name, self.http).is_none() {
            self.error(
                format!(
                    "Unknown type '{name}'.{}",
                    crate::introspection::argument_suggestions(
                        &crate::introspection::schema_for(self.http)["types"],
                        name
                    )
                ),
                &[pos],
            );
        }
    }
    fn literal(&mut self, value: &Located<'_>, expected: &Value) {
        if matches!(value.value, Input::Variable(_)) {
            return;
        }
        let list_type = nullable(expected);
        if list_type["kind"] == "LIST" && !matches!(value.value, Input::Null) {
            if matches!(value.value, Input::List(_)) {
                for child in &value.children {
                    self.literal(child, &list_type["ofType"]);
                }
            } else {
                self.literal(value, &list_type["ofType"]);
            }
            return;
        }
        let kind = named(expected);
        let rendered = render_located(value);
        let definition = ty_for(kind, self.http);
        if matches!(value.value, Input::Object(_))
            && definition.is_some_and(|ty| ty["kind"] == "INPUT_OBJECT")
        {
            let fields = &definition.unwrap()["inputFields"];
            for field in fields.as_array().unwrap() {
                if field["type"]["kind"] == "NON_NULL"
                    && !has_default(field)
                    && !value
                        .fields
                        .iter()
                        .any(|(name, _)| field["name"] == name.as_str())
                {
                    self.error(
                        format!(
                            "Field '{kind}.{}' of required type '{}' was not provided.",
                            field["name"].as_str().unwrap(),
                            display(&field["type"])
                        ),
                        &[value.pos],
                    );
                }
            }
            for ((name, pos), child) in value.fields.iter().zip(&value.children) {
                if let Some(field) = argument(fields, name) {
                    self.literal(child, &field["type"]);
                } else {
                    self.error(
                        format!(
                            "Field '{name}' is not defined by type '{kind}'.{}",
                            crate::introspection::argument_suggestions(fields, name)
                        ),
                        &[*pos],
                    );
                }
            }
            return;
        }
        let reason = match (value.value, kind) {
            (Input::Null, _) if expected["kind"] == "NON_NULL" => Some(format!(
                "Expected value of type '{}', found null.",
                display(expected)
            )),
            (Input::Null, _) => None,
            (Input::Number(n), "Int") if n.as_i64().is_some_and(|n| i32::try_from(n).is_ok()) => {
                None
            }
            (Input::Number(n), "Int") if !n.to_string().contains(['.', 'e', 'E']) => Some(format!(
                "Int cannot represent non 32-bit signed integer value: {rendered}"
            )),
            (_, "Int") => Some(format!(
                "Int cannot represent non-integer value: {rendered}"
            )),
            (Input::String(_), "String")
            | (Input::Boolean(_), "Boolean")
            | (Input::Number(_), "Float")
            | (Input::String(_), "ID") => None,
            (Input::Number(n), "ID") if !n.to_string().contains(['.', 'e', 'E']) => None,
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
            (Input::Enum(name), _) if definition.is_some_and(|ty| ty["kind"] == "ENUM") => {
                if definition.unwrap()["enumValues"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v["name"] == name.as_str())
                {
                    None
                } else {
                    Some(format!(
                        "Value '{name}' does not exist in '{kind}' enum.{}",
                        crate::introspection::field_suggestions(kind, name.as_str())
                            .replace("Did you mean", "Did you mean the enum value")
                    ))
                }
            }
            (_, _) if definition.is_some_and(|ty| ty["kind"] == "ENUM") => Some(format!(
                "Enum '{kind}' cannot represent non-enum value: {rendered}.{}",
                crate::introspection::field_suggestions(kind, &rendered)
                    .replace("Did you mean", "Did you mean the enum value")
            )),
            (_, _) if definition.is_some_and(|ty| ty["kind"] == "INPUT_OBJECT") => Some(format!(
                "Expected value of type '{}', found {rendered}.",
                display(expected)
            )),
            _ => None,
        };
        if let Some(reason) = reason {
            self.error(reason, &[value.pos]);
        }
    }
    fn unique_arguments(&mut self, args: &[(Positioned<Name>, Positioned<Input>)]) {
        let mut seen = HashSet::new();
        for (name, _) in args {
            if !seen.insert(name.node.as_str()) {
                continue;
            }
            let positions: Vec<_> = args
                .iter()
                .filter(|(n, _)| n.node == name.node)
                .map(|(n, _)| n.pos)
                .collect();
            if positions.len() > 1 {
                self.error(
                    format!("There can be only one argument named '{}'.", name.node),
                    &positions,
                );
            }
        }
    }
    fn unique_directives(&mut self, dirs: &[Positioned<Directive>]) {
        let mut seen = HashMap::new();
        for directive in dirs {
            let name = directive.node.name.node.as_str();
            if directive_definition(name, self.http).is_some_and(|d| d["isRepeatable"] != true) {
                if let Some(pos) = seen.get(name) {
                    self.error(
                        format!("The directive '@{name}' can only be used once at this location."),
                        &[*pos, directive.pos],
                    );
                } else {
                    seen.insert(name, directive.pos);
                }
            }
        }
    }
    fn directives(&mut self, dirs: &[Positioned<Directive>], location: &str) {
        for directive in dirs {
            let name = directive.node.name.node.as_str();
            let definition = directive_definition(name, self.http);
            if let Some(definition) = definition {
                if !definition["locations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == location)
                {
                    self.error(
                        format!(
                            "Directive '@{name}' may not be used on {}.",
                            location.to_lowercase().replace('_', " ")
                        ),
                        &[directive.pos],
                    );
                }
            } else {
                self.error(format!("Unknown directive '@{name}'."), &[directive.pos]);
            }
            // graphql-core checks directive argument names at directive entry.
            if let Some(definition) = definition {
                for (arg, _) in &directive.node.arguments {
                    if argument(&definition["args"], arg.node.as_str()).is_none() {
                        self.error(
                            format!(
                                "Unknown argument '{}' on directive '@{name}'.{}",
                                arg.node,
                                crate::introspection::argument_suggestions(
                                    &definition["args"],
                                    arg.node.as_str()
                                )
                            ),
                            &[arg.pos],
                        );
                    }
                }
            }
            self.unique_arguments(&directive.node.arguments);
            if let Some(definition) = definition {
                for (arg, value) in &directive.node.arguments {
                    if let Some(expected) = argument(&definition["args"], arg.node.as_str()) {
                        self.literal(&self.located(value), &expected["type"]);
                    }
                }
                self.required_arguments(
                    &definition["args"],
                    &directive.node.arguments,
                    &format!("Directive '@{name}'"),
                    directive.pos,
                );
            }
        }
    }
    fn required_arguments(
        &mut self,
        defs: &Value,
        args: &[(Positioned<Name>, Positioned<Input>)],
        label: &str,
        pos: Pos,
    ) {
        for def in defs.as_array().unwrap() {
            let name = def["name"].as_str().unwrap();
            if def["type"]["kind"] == "NON_NULL"
                && !has_default(def)
                && !args.iter().any(|(arg, _)| arg.node == name)
            {
                self.error(format!("{label} argument '{name}' of type '{}' is required, but it was not provided.",display(&def["type"])),&[pos]);
            }
        }
    }
    fn fragment_type(&mut self, name: Option<&str>, condition: &Positioned<Name>) {
        let type_name = condition.node.as_str();
        if ty_for(type_name, self.http).is_some_and(|t| !composite(t)) {
            let label = name.map_or("Fragment".into(), |name| format!("Fragment '{name}'"));
            self.error(
                format!("{label} cannot condition on non composite type '{type_name}'."),
                &[condition.pos],
            );
        }
    }
    fn possible_spread(&mut self, fragment: Option<&str>, parent: &str, target: &str, pos: Pos) {
        let (Some(parent_type), Some(target_type)) =
            (ty_for(parent, self.http), ty_for(target, self.http))
        else {
            return;
        };
        if !composite(parent_type) || !composite(target_type) {
            return;
        }
        let concrete = |ty: &Value| -> Vec<String> {
            if ty["kind"] == "OBJECT" {
                vec![ty["name"].as_str().unwrap().to_string()]
            } else {
                ty["possibleTypes"]
                    .as_array()
                    .map_or_else(Vec::new, |types| {
                        types
                            .iter()
                            .filter_map(|t| t["name"].as_str().map(str::to_string))
                            .collect()
                    })
            }
        };
        let targets = concrete(target_type);
        if !concrete(parent_type).iter().any(|t| targets.contains(t)) {
            let label = fragment.map_or("Fragment".into(), |name| format!("Fragment '{name}'"));
            self.error(format!("{label} cannot be spread here as objects of type '{parent}' can never be of type '{target}'."),&[pos]);
        }
    }
    fn walk(&mut self, set: &SelectionSet, parent: &str) {
        for selection in &set.items {
            match &selection.node {
                Selection::Field(field) => {
                    let name = field.node.name.node.as_str();
                    let definition = field_definition(parent, name, self.http);
                    let has_children = !field.node.selection_set.node.items.is_empty();
                    if let Some(definition) = &definition {
                        let result_type = &definition["type"];
                        let is_leaf = ty_for(named(result_type), self.http).is_some_and(|ty| {
                            matches!(ty["kind"].as_str(), Some("SCALAR" | "ENUM"))
                        });
                        if is_leaf && has_children {
                            self.error(format!("Field '{name}' must not have a selection since type '{}' has no subfields.",display(result_type)),&[field.node.selection_set.pos]);
                        } else if !is_leaf && !has_children {
                            self.error(format!("Field '{name}' of type '{}' must have a selection of subfields. Did you mean '{name} {{ ... }}'?",display(result_type)),&[field.pos]);
                        }
                    } else if ty_for(parent, self.http).is_some_and(composite) {
                        self.error(
                            format!(
                                "Cannot query field '{name}' on type '{parent}'.{}",
                                crate::introspection::field_suggestions_for(
                                    parent, name, self.http
                                )
                            ),
                            &[field.pos],
                        );
                    }
                    self.unique_directives(&field.node.directives);
                    self.unique_arguments(&field.node.arguments);
                    for (arg, value) in &field.node.arguments {
                        if let Some(definition) = &definition {
                            if let Some(expected) = argument(&definition["args"], arg.node.as_str())
                            {
                                self.literal(&self.located(value), &expected["type"]);
                            } else {
                                self.error(
                                    format!(
                                        "Unknown argument '{}' on field '{parent}.{name}'.{}",
                                        arg.node,
                                        crate::introspection::argument_suggestions(
                                            &definition["args"],
                                            arg.node.as_str()
                                        )
                                    ),
                                    &[arg.pos],
                                );
                            }
                        }
                    }
                    self.directives(&field.node.directives, "FIELD");
                    // Unknown fields do not supply a child type, but their subtree
                    // still participates in directive and variable validation.
                    self.walk(
                        &field.node.selection_set.node,
                        definition.as_ref().map_or("", |d| named(&d["type"])),
                    );
                    if let Some(definition) = definition {
                        self.required_arguments(
                            &definition["args"],
                            &field.node.arguments,
                            &format!("Field '{name}'"),
                            field.pos,
                        );
                    }
                }
                Selection::InlineFragment(fragment) => {
                    if let Some(condition) = &fragment.node.type_condition {
                        self.fragment_type(None, &condition.node.on);
                        self.possible_spread(
                            None,
                            parent,
                            condition.node.on.node.as_str(),
                            fragment.pos,
                        );
                    }
                    self.unique_directives(&fragment.node.directives);
                    if let Some(condition) = &fragment.node.type_condition {
                        self.known_type(condition.node.on.node.as_str(), condition.node.on.pos);
                    }
                    self.directives(&fragment.node.directives, "INLINE_FRAGMENT");
                    self.walk(
                        &fragment.node.selection_set.node,
                        fragment
                            .node
                            .type_condition
                            .as_ref()
                            .map_or(parent, |t| t.node.on.node.as_str()),
                    );
                }
                Selection::FragmentSpread(spread) => {
                    if !self
                        .doc
                        .fragments
                        .contains_key(spread.node.fragment_name.node.as_str())
                    {
                        self.error(
                            format!("Unknown fragment '{}'.", spread.node.fragment_name.node),
                            &[spread.node.fragment_name.pos],
                        );
                    }
                    if let Some(fragment) = self
                        .doc
                        .fragments
                        .get(spread.node.fragment_name.node.as_str())
                    {
                        self.possible_spread(
                            Some(spread.node.fragment_name.node.as_str()),
                            parent,
                            fragment.node.type_condition.node.on.node.as_str(),
                            spread.pos,
                        );
                    }
                    self.unique_directives(&spread.node.directives);
                    self.directives(&spread.node.directives, "FRAGMENT_SPREAD");
                }
            }
        }
    }
    fn value_usages(
        &self,
        value: &Located<'_>,
        expected: Option<&Value>,
        default: bool,
        out: &mut Vec<Usage>,
    ) {
        if let Input::Variable(name) = value.value {
            out.push(Usage {
                name: name.to_string(),
                pos: value.pos,
                expected: expected.cloned(),
                has_default: default,
            });
            return;
        }
        let expected = expected.map(nullable);
        match value.value {
            Input::List(_) => {
                let item = expected.map(|t| if t["kind"] == "LIST" { &t["ofType"] } else { t });
                for child in &value.children {
                    self.value_usages(child, item, false, out);
                }
            }
            Input::Object(_) => {
                let fields = expected
                    .and_then(|t| ty_for(named(t), self.http))
                    .map(|t| &t["inputFields"]);
                for ((name, _), child) in value.fields.iter().zip(&value.children) {
                    let def = fields.and_then(|fields| argument(fields, name));
                    self.value_usages(
                        child,
                        def.map(|d| &d["type"]),
                        def.is_some_and(has_default),
                        out,
                    );
                }
            }
            _ => {}
        }
    }
    fn argument_usages(
        &self,
        args: &[(Positioned<Name>, Positioned<Input>)],
        defs: Option<&Value>,
        out: &mut Vec<Usage>,
    ) {
        for (name, value) in args {
            let def = defs.and_then(|defs| argument(defs, name.node.as_str()));
            self.value_usages(
                &self.located(value),
                def.map(|d| &d["type"]),
                def.is_some_and(has_default),
                out,
            );
        }
    }
    fn directive_usages(&self, dirs: &[Positioned<Directive>], out: &mut Vec<Usage>) {
        for dir in dirs {
            self.argument_usages(
                &dir.node.arguments,
                directive_definition(dir.node.name.node.as_str(), self.http).map(|d| &d["args"]),
                out,
            );
        }
    }
    fn usages(&self, set: &SelectionSet, parent: &str, out: &mut Vec<Usage>) {
        for selection in &set.items {
            match &selection.node {
                Selection::Field(field) => {
                    let definition =
                        field_definition(parent, field.node.name.node.as_str(), self.http);
                    self.argument_usages(
                        &field.node.arguments,
                        definition.as_ref().map(|d| &d["args"]),
                        out,
                    );
                    self.directive_usages(&field.node.directives, out);
                    self.usages(
                        &field.node.selection_set.node,
                        definition.as_ref().map_or("", |d| named(&d["type"])),
                        out,
                    );
                }
                Selection::InlineFragment(fragment) => {
                    self.directive_usages(&fragment.node.directives, out);
                    self.usages(
                        &fragment.node.selection_set.node,
                        fragment
                            .node
                            .type_condition
                            .as_ref()
                            .map_or(parent, |t| t.node.on.node.as_str()),
                        out,
                    );
                }
                Selection::FragmentSpread(spread) => {
                    self.directive_usages(&spread.node.directives, out)
                }
            }
        }
    }
    fn operation(&mut self, name: Option<&Name>, op: &Positioned<OperationDefinition>) {
        let mut seen = HashSet::new();
        for def in &op.node.variable_definitions {
            let name = def.node.name.node.as_str();
            if !seen.insert(name) {
                continue;
            }
            let positions: Vec<_> = op
                .node
                .variable_definitions
                .iter()
                .filter(|d| d.node.name.node == name)
                .map(|d| d.node.name.pos)
                .collect();
            if positions.len() > 1 {
                self.error(
                    format!("There can be only one variable named '${name}'."),
                    &positions,
                );
            }
        }
        self.unique_directives(&op.node.directives);
        for def in &op.node.variable_definitions {
            let declared = type_value(&def.node.var_type.node, self.http);
            let kind = named(&declared);
            if ty_for(kind, self.http).is_some_and(|ty| {
                !matches!(
                    ty["kind"].as_str(),
                    Some("SCALAR" | "ENUM" | "INPUT_OBJECT")
                )
            }) {
                self.error(
                    format!(
                        "Variable '${}' cannot be non-input type '{}'.",
                        def.node.name.node, def.node.var_type.node
                    ),
                    &[def.node.var_type.pos],
                );
            }
            self.unique_directives(&def.node.directives);
            self.known_type(kind, self.named_type_pos(def.node.var_type.pos));
            if ty_for(kind, self.http).is_some_and(|ty| {
                matches!(
                    ty["kind"].as_str(),
                    Some("SCALAR" | "ENUM" | "INPUT_OBJECT")
                )
            }) && let Some(default) = &def.node.default_value
            {
                let default = Positioned::new(default.node.clone().into_value(), default.pos);
                self.literal(&self.located(&default), &declared);
            }
            self.directives(&def.node.directives, "VARIABLE_DEFINITION");
        }
        self.directives(&op.node.directives, &op.node.ty.to_string().to_uppercase());
        let root = if op.node.ty == OperationType::Query {
            "Query"
        } else {
            ""
        };
        self.walk(&op.node.selection_set.node, root);
        let mut usages = vec![];
        self.directive_usages(&op.node.directives, &mut usages);
        self.usages(&op.node.selection_set.node, root, &mut usages);
        for fragment in self.referenced_fragments(&op.node.selection_set.node) {
            self.directive_usages(&fragment.node.directives, &mut usages);
            self.usages(
                &fragment.node.selection_set.node,
                fragment.node.type_condition.node.on.node.as_str(),
                &mut usages,
            );
        }
        for usage in &usages {
            if !op
                .node
                .variable_definitions
                .iter()
                .any(|d| d.node.name.node == usage.name.as_str())
            {
                let suffix = name.map_or(String::new(), |n| format!(" by operation '{n}'"));
                self.error(
                    format!("Variable '${}' is not defined{suffix}.", usage.name),
                    &[usage.pos, op.pos],
                );
            }
        }
        for def in &op.node.variable_definitions {
            if !usages.iter().any(|u| def.node.name.node == u.name.as_str()) {
                let suffix = name.map_or(String::new(), |n| format!(" in operation '{n}'"));
                self.error(
                    format!("Variable '${}' is never used{suffix}.", def.node.name.node),
                    &[def.pos],
                );
            }
        }
        for usage in &usages {
            let def = op
                .node
                .variable_definitions
                .iter()
                .rev()
                .find(|d| d.node.name.node == usage.name.as_str());
            if let (Some(def), Some(expected)) = (def, &usage.expected) {
                let declared = type_value(&def.node.var_type.node, self.http);
                if ty_for(named(&declared), self.http).is_none() {
                    continue;
                }
                let default = def
                    .node
                    .default_value
                    .as_ref()
                    .is_some_and(|v| v.node != ConstValue::Null);
                let target = if expected["kind"] == "NON_NULL"
                    && declared["kind"] != "NON_NULL"
                    && (default || usage.has_default)
                {
                    &expected["ofType"]
                } else {
                    expected
                };
                if !subtype(&declared, target) {
                    self.error(
                        format!(
                            "Variable '${}' of type '{}' used in position expecting type '{}'.",
                            usage.name,
                            display(&declared),
                            display(expected)
                        ),
                        &[def.pos, usage.pos],
                    );
                }
            }
        }
    }
    fn referenced_fragments<'a>(
        &'a self,
        set: &'a SelectionSet,
    ) -> Vec<&'a Positioned<async_graphql_parser::types::FragmentDefinition>> {
        let mut sets = vec![set];
        let mut seen = HashSet::new();
        let mut result = vec![];
        while let Some(set) = sets.pop() {
            for spread in fragment_spreads(set) {
                let name = spread.node.fragment_name.node.as_str();
                if seen.insert(name)
                    && let Some(fragment) = self.doc.fragments.get(name)
                {
                    result.push(fragment);
                    sets.push(&fragment.node.selection_set.node);
                }
            }
        }
        result
    }
}
fn subtype(actual: &Value, expected: &Value) -> bool {
    if expected["kind"] == "NON_NULL" {
        return actual["kind"] == "NON_NULL" && subtype(&actual["ofType"], &expected["ofType"]);
    }
    if actual["kind"] == "NON_NULL" {
        return subtype(&actual["ofType"], expected);
    }
    if expected["kind"] == "LIST" {
        return actual["kind"] == "LIST" && subtype(&actual["ofType"], &expected["ofType"]);
    }
    actual["kind"] != "LIST" && actual["name"] == expected["name"]
}
fn fragment_spreads(
    set: &SelectionSet,
) -> Vec<&Positioned<async_graphql_parser::types::FragmentSpread>> {
    let mut sets = vec![set];
    let mut result = vec![];
    while let Some(set) = sets.pop() {
        for selection in &set.items {
            match &selection.node {
                Selection::FragmentSpread(spread) => result.push(spread),
                Selection::Field(field) => sets.push(&field.node.selection_set.node),
                Selection::InlineFragment(fragment) => sets.push(&fragment.node.selection_set.node),
            }
        }
    }
    result
}
pub(crate) fn validate_for(doc: &ExecutableDocument, document: &str, http: bool) -> Option<Value> {
    let mut validator = Validator {
        doc,
        tokens: tokens(document),
        http,
        out: vec![],
    };
    // graphql-core visits definition source order, and emits variable-use checks
    // on leaving each operation, before visiting later fragment definitions.
    let mut definitions: Vec<_> = doc
        .operations
        .iter()
        .map(|(name, op)| (op.pos, Some((name, op)), None))
        .collect();
    definitions.extend(
        doc.fragments
            .iter()
            .map(|(name, fragment)| (fragment.pos, None, Some((name, fragment)))),
    );
    definitions.sort_by_key(|(pos, _, _)| (pos.line, pos.column));
    for (_, op, fragment) in &definitions {
        if let Some((name, op)) = op {
            validator.operation(*name, op);
        }
        if let Some((name, fragment)) = fragment {
            validator.fragment_type(Some(name.as_str()), &fragment.node.type_condition.node.on);
            validator.unique_directives(&fragment.node.directives);
            validator.known_type(
                fragment.node.type_condition.node.on.node.as_str(),
                fragment.node.type_condition.node.on.pos,
            );
            validator.directives(&fragment.node.directives, "FRAGMENT_DEFINITION");
            validator.walk(
                &fragment.node.selection_set.node,
                fragment.node.type_condition.node.on.node.as_str(),
            );
        }
    }
    let used: HashSet<_> = doc
        .operations
        .iter()
        .flat_map(|(_, op)| validator.referenced_fragments(&op.node.selection_set.node))
        .map(|fragment| (fragment.pos.line, fragment.pos.column))
        .collect();
    for (_, _, fragment) in &definitions {
        if let Some((name, fragment)) = fragment
            && !used.contains(&(fragment.pos.line, fragment.pos.column))
        {
            validator.error(format!("Fragment '{name}' is never used."), &[fragment.pos]);
        }
    }
    if validator.out.is_empty() {
        None
    } else {
        // The frozen façade supplies graphql-core's max_errors=20.
        if validator.out.len() > 20 {
            validator.out.truncate(20);
            validator.out.push(json!({"message":"Too many validation errors, error limit reached. Validation aborted."}));
        }
        Some(json!({"data":null,"errors":validator.out}))
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

#[cfg(test)]
mod literal_diagnostic_render_tests {
    use super::*;

    #[test]
    fn runtime_renderer_retains_variables_and_literal_node_spelling() {
        let document = r#"query($v:String){items(first: [{zz:$v, aa:-0, bb:1E+0, cc:"\b\f\u008a", dd:"""text"""}]){nodes{id}}}"#;
        let parsed = async_graphql_parser::parse_query(document).unwrap();
        let (_, operation) = parsed.operations.iter().next().unwrap();
        let Selection::Field(field) = &operation.node.selection_set.node.items[0].node else {
            panic!("expected test field");
        };
        assert_eq!(
            render_input(document, &field.node.arguments[0].1),
            r#"[{zz: $v, aa: -0, bb: 1E+0, cc: "\b\f\u008A", dd: """text"""}]"#,
        );
    }
}
