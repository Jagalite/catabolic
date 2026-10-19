// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Narrow executable-parser compatibility for the frozen GraphQL grammar.
//!
//! The framework rejects UTF-16 pairs, braced escapes and ignored text inside types.
//! Admit those constructs through a lexical view, then restore every AST source
//! position before validation or execution. The original document remains the
//! budget, spelling, diagnostics, and request identity input.
use async_graphql_parser::{Error, Pos, Positioned, types::*};
use std::collections::{BTreeMap, HashSet};

use crate::validation::{Token, tokens};

mod boundary;

pub(crate) use boundary::before_token_limit;

struct Edit {
    start: usize,
    end: usize,
    // Each output character points to its original UTF-8 byte offset.
    chars: Vec<(char, usize)>,
}

fn name(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn type_end(tokens: &[Token], start: usize) -> Option<usize> {
    let mut i = start;
    let mut depth = 0;
    while tokens.get(i).is_some_and(|t| t.text == "[") {
        depth += 1;
        i += 1;
    }
    if !tokens.get(i).is_some_and(|t| name(&t.text)) {
        return None;
    }
    i += 1;
    if tokens.get(i).is_some_and(|t| t.text == "!") {
        i += 1;
    }
    for _ in 0..depth {
        if !tokens.get(i).is_some_and(|t| t.text == "]") {
            return None;
        }
        i += 1;
        if tokens.get(i).is_some_and(|t| t.text == "!") {
            i += 1;
        }
    }
    Some(i)
}

// The location lexer recognizes Python's wider whitespace for diagnostics.
// Only GraphQL's ignored characters may be removed from an accepted type.
fn ignored(text: &str) -> bool {
    let mut comment = false;
    text.chars().all(|c| {
        if comment && !matches!(c, '\r' | '\n') {
            return true;
        }
        comment = c == '#';
        comment || matches!(c, ' ' | '\t' | '\r' | '\n' | ',' | '\u{feff}')
    })
}

fn braced_escape(bytes: &[u8], start: usize) -> Option<(char, usize)> {
    if bytes.get(start..start + 3)? != b"\\u{" {
        return None;
    }
    let rest = bytes.get(start + 3..)?;
    let length = rest.iter().take(9).position(|byte| *byte == b'}')?;
    if length == 0 || !rest[..length].iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let value = u32::from_str_radix(std::str::from_utf8(&rest[..length]).ok()?, 16).ok()?;
    Some((char::from_u32(value)?, start + 4 + length))
}

fn edits(source: &str, tokens: &[Token]) -> Vec<Edit> {
    let mut edits = vec![];
    for (index, token) in tokens.iter().enumerate() {
        if token.text == "$"
            && tokens.get(index + 1).is_some_and(|t| name(&t.text))
            && tokens.get(index + 2).is_some_and(|t| t.text == ":")
            && let Some(end) = type_end(tokens, index + 3)
        {
            let ty = &tokens[index + 3..end];
            if ty.windows(2).any(|pair| pair[0].end != pair[1].start)
                && ty
                    .windows(2)
                    .all(|pair| ignored(&source[pair[0].end..pair[1].start]))
            {
                edits.push(Edit {
                    start: ty[0].start,
                    end: ty.last().unwrap().end,
                    chars: ty
                        .iter()
                        .flat_map(|t| {
                            t.text
                                .char_indices()
                                .map(|(offset, c)| (c, t.start + offset))
                        })
                        .collect(),
                });
            }
        }
        if !token.text.starts_with('"') || token.text.starts_with("\"\"\"") {
            continue;
        }
        let bytes = token.text.as_bytes();
        let mut i = 1;
        while i < bytes.len() {
            if bytes[i] != b'\\' {
                i += 1;
                continue;
            }
            let hex = |at: usize| -> Option<u32> {
                let raw = bytes.get(at..at + 6)?;
                if &raw[..2] != b"\\u" || !raw[2..].iter().all(u8::is_ascii_hexdigit) {
                    return None;
                }
                u32::from_str_radix(std::str::from_utf8(&raw[2..]).ok()?, 16).ok()
            };
            if let Some((scalar, end)) = braced_escape(bytes, i) {
                // Keep quotes, backslashes and controls escaped in the parser view.
                // Supplementary scalars use the same literal form as paired UTF-16.
                let replacement = if u32::from(scalar) <= 0xffff {
                    format!("\\u{:04X}", u32::from(scalar))
                } else {
                    scalar.to_string()
                };
                edits.push(Edit {
                    start: token.start + i,
                    end: token.start + end,
                    chars: replacement.chars().map(|c| (c, token.start + i)).collect(),
                });
                i = end;
            } else if let (Some(high), Some(low)) = (hex(i), hex(i + 6))
                && (0xd800..=0xdbff).contains(&high)
                && (0xdc00..=0xdfff).contains(&low)
            {
                let c = char::from_u32(0x10000 + ((high - 0xd800) << 10) + low - 0xdc00)
                    .expect("paired UTF-16 is a Unicode scalar");
                edits.push(Edit {
                    start: token.start + i,
                    end: token.start + i + 12,
                    chars: vec![(c, token.start + i)],
                });
                i += 12;
            } else {
                // Escaped backslashes and quotes cannot start a Unicode escape.
                i = (i + 2).min(bytes.len());
            }
        }
    }
    edits.sort_by_key(|edit| edit.start);
    edits
}

fn edited_chars(source: &str, start: usize, end: usize, edits: &[Edit]) -> Vec<(char, usize)> {
    let mut chars = vec![];
    let mut cursor = start;
    for edit in edits.iter().filter(|e| e.start >= start && e.end <= end) {
        chars.extend(
            source[cursor..edit.start]
                .char_indices()
                .map(|(i, c)| (c, cursor + i)),
        );
        chars.extend(edit.chars.iter().copied());
        cursor = edit.end;
    }
    chars.extend(
        source[cursor..end]
            .char_indices()
            .map(|(i, c)| (c, cursor + i)),
    );
    chars
}

struct View {
    text: String,
    positions: BTreeMap<Pos, Pos>,
}
impl View {
    fn new(source: &str, edits: Vec<Edit>) -> Self {
        let mut original = vec![Pos::default(); source.len() + 1];
        let mut pos = Pos { line: 1, column: 1 };
        let mut previous = None;
        for (offset, c) in source.char_indices() {
            original[offset] = pos;
            if c == '\r' || c == '\n' && previous != Some('\r') {
                pos.line += 1;
                pos.column = 1;
            } else if c != '\n' {
                pos.column += 1;
            }
            previous = Some(c);
        }
        original[source.len()] = pos;
        let mut output = Vec::with_capacity(source.len());
        let mut cursor = 0;
        for edit in edits {
            output.extend(
                source[cursor..edit.start]
                    .char_indices()
                    .map(|(i, c)| (c, cursor + i)),
            );
            output.extend(edit.chars);
            cursor = edit.end;
        }
        output.extend(
            source[cursor..]
                .char_indices()
                .map(|(i, c)| (c, cursor + i)),
        );
        let mut result = Self {
            text: String::new(),
            positions: BTreeMap::new(),
        };
        let mut pos = Pos { line: 1, column: 1 };
        let mut previous = None;
        for (c, offset) in output {
            // Bare CR produces colliding framework positions. Use canonical
            // newlines only in the parser view; original positions stay intact.
            if c == '\n' && previous == Some('\r') {
                previous = Some(c);
                continue;
            }
            let normalized = if c == '\r' { '\n' } else { c };
            result.positions.insert(pos, original[offset]);
            result.text.push(normalized);
            if normalized == '\n' {
                pos.line += 1;
                pos.column = 1;
            } else {
                pos.column += 1;
            }
            previous = Some(c);
        }
        result.positions.insert(pos, original[source.len()]);
        result
    }
    fn position(&self, pos: &mut Pos) {
        if let Some(original) = self.positions.get(pos) {
            *pos = *original;
        }
    }
    fn node<T>(&self, node: &mut Positioned<T>) {
        self.position(&mut node.pos);
    }
    fn directives(&self, directives: &mut [Positioned<Directive>]) {
        for directive in directives {
            self.node(directive);
            self.node(&mut directive.node.name);
            for (name, value) in &mut directive.node.arguments {
                self.node(name);
                self.node(value);
            }
        }
    }
    fn condition(&self, condition: &mut Positioned<TypeCondition>) {
        self.node(condition);
        self.node(&mut condition.node.on);
    }
    fn selection(&self, selection: &mut Positioned<SelectionSet>) {
        self.node(selection);
        for item in &mut selection.node.items {
            self.node(item);
            match &mut item.node {
                Selection::Field(field) => {
                    self.node(field);
                    self.node(&mut field.node.name);
                    if let Some(alias) = &mut field.node.alias {
                        self.node(alias);
                    }
                    for (name, value) in &mut field.node.arguments {
                        self.node(name);
                        self.node(value);
                    }
                    self.directives(&mut field.node.directives);
                    self.selection(&mut field.node.selection_set);
                }
                Selection::FragmentSpread(spread) => {
                    self.node(spread);
                    self.node(&mut spread.node.fragment_name);
                    self.directives(&mut spread.node.directives);
                }
                Selection::InlineFragment(fragment) => {
                    self.node(fragment);
                    if let Some(condition) = &mut fragment.node.type_condition {
                        self.condition(condition);
                    }
                    self.directives(&mut fragment.node.directives);
                    self.selection(&mut fragment.node.selection_set);
                }
            }
        }
    }
    fn operation(&self, operation: &mut Positioned<OperationDefinition>) {
        self.node(operation);
        for variable in &mut operation.node.variable_definitions {
            self.node(variable);
            self.node(&mut variable.node.name);
            self.node(&mut variable.node.var_type);
            if let Some(default) = &mut variable.node.default_value {
                self.node(default);
            }
            self.directives(&mut variable.node.directives);
        }
        self.directives(&mut operation.node.directives);
        self.selection(&mut operation.node.selection_set);
    }
    fn document(&self, document: &mut ExecutableDocument) {
        match &mut document.operations {
            DocumentOperations::Single(operation) => self.operation(operation),
            DocumentOperations::Multiple(operations) => {
                for operation in operations.values_mut() {
                    self.operation(operation);
                }
            }
        }
        for fragment in document.fragments.values_mut() {
            self.node(fragment);
            self.condition(&mut fragment.node.type_condition);
            self.directives(&mut fragment.node.directives);
            self.selection(&mut fragment.node.selection_set);
        }
    }
}

pub(crate) fn parse_query(source: &str) -> Result<ExecutableDocument, Error> {
    let tokens = tokens(source);
    let mut edits = edits(source, &tokens);
    boundary::inspect(source, &mut edits)?;
    // Canonicalize line endings even without grammar edits. Bare CR otherwise
    // gives distinct accepted fields colliding framework positions, which are
    // also keys for runtime argument plans.
    let view = (!edits.is_empty() || source.contains('\r')).then(|| View::new(source, edits));
    let result =
        async_graphql_parser::parse_query(view.as_ref().map_or(source, |v| v.text.as_str()));
    match (result, view) {
        (Ok(mut document), Some(view)) => {
            view.document(&mut document);
            Ok(document)
        }
        (Err(Error::RecursionLimitExceeded), Some(_)) => Err(Error::RecursionLimitExceeded),
        // This bounded adapter does not redefine unrelated syntax diagnostics.
        // Invalid transformed documents retain the original parser's rejection
        // and original source excerpt, never a rewritten query or location.
        (Err(_), Some(_)) => async_graphql_parser::parse_query(source),
        (result, None) => result,
    }
}

/// The framework's input maps overwrite duplicate keys. Preserve every input
/// node in a validation-only AST by assigning later occurrences private keys.
/// `validation::locate` reads the original key tokens, so these keys never enter
/// diagnostics, type lookup, variable-use lookup, execution, or stored JSON.
/// Admission and all document budgets have already used the original source.
pub(crate) fn validation_document(
    source: &str,
    document: &ExecutableDocument,
    tokens: &[Token],
) -> Result<Option<ExecutableDocument>, Error> {
    fn directives(dirs: &[Positioned<Directive>], positions: &mut Vec<Pos>) {
        for dir in dirs {
            positions.extend(dir.node.arguments.iter().map(|(_, value)| value.pos));
        }
    }
    fn selection(set: &SelectionSet, positions: &mut Vec<Pos>) {
        for item in &set.items {
            match &item.node {
                Selection::Field(field) => {
                    positions.extend(field.node.arguments.iter().map(|(_, value)| value.pos));
                    directives(&field.node.directives, positions);
                    selection(&field.node.selection_set.node, positions);
                }
                Selection::InlineFragment(fragment) => {
                    directives(&fragment.node.directives, positions);
                    selection(&fragment.node.selection_set.node, positions);
                }
                Selection::FragmentSpread(spread) => directives(&spread.node.directives, positions),
            }
        }
    }
    fn value(
        tokens: &[Token],
        cursor: &mut usize,
        edits: &mut Vec<Edit>,
        names: &mut HashSet<String>,
    ) {
        let Some(token) = tokens.get(*cursor) else {
            return;
        };
        *cursor += 1;
        match token.text.as_str() {
            "$" => *cursor += 1,
            "[" => {
                while tokens.get(*cursor).is_some_and(|t| t.text != "]") {
                    value(tokens, cursor, edits, names);
                }
                *cursor += 1;
            }
            "{" => {
                let mut seen = HashSet::new();
                while let Some(field) = tokens.get(*cursor).filter(|t| t.text != "}") {
                    if !seen.insert(field.text.as_str()) {
                        let mut replacement = format!("__catabolic_validation_{}", field.start);
                        while !names.insert(replacement.clone()) {
                            replacement.push('_');
                        }
                        edits.push(Edit {
                            start: field.start,
                            end: field.end,
                            chars: replacement.chars().map(|c| (c, field.start)).collect(),
                        });
                    }
                    *cursor += 2;
                    value(tokens, cursor, edits, names);
                }
                *cursor += 1;
            }
            _ => {}
        }
    }
    let mut positions = vec![];
    for (_, operation) in document.operations.iter() {
        for variable in &operation.node.variable_definitions {
            if let Some(default) = &variable.node.default_value {
                positions.push(default.pos);
            }
            directives(&variable.node.directives, &mut positions);
        }
        directives(&operation.node.directives, &mut positions);
        selection(&operation.node.selection_set.node, &mut positions);
    }
    for fragment in document.fragments.values() {
        directives(&fragment.node.directives, &mut positions);
        selection(&fragment.node.selection_set.node, &mut positions);
    }
    let mut edits = vec![];
    let mut names = tokens.iter().map(|t| t.text.clone()).collect();
    for pos in positions {
        if let Ok(mut cursor) = tokens.binary_search_by_key(&pos, |t| t.pos) {
            value(tokens, &mut cursor, &mut edits, &mut names);
        }
    }
    if edits.is_empty() {
        return Ok(None);
    }
    edits.sort_by_key(|edit| edit.start);
    let view = View::new(source, edits);
    let mut result = parse_query(&view.text)?;
    view.document(&mut result);
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braced_unicode_preserves_scalars_including_escaped_syntax_and_controls() {
        for raw in [
            "0", "8", "9", "A", "D", "1F", "22", "5C", "7F", "A0", "D7FF", "E000", "FFFF", "10000",
            "1F600", "10FFFF", "00000041", "0001f600",
        ] {
            let source = format!(r#"{{item(id:"\u{{{raw}}}"){{id}}}}"#);
            let document = parse_query(&source).unwrap();
            let (_, operation) = document.operations.iter().next().unwrap();
            let Selection::Field(field) = &operation.node.selection_set.node.items[0].node else {
                panic!("expected field");
            };
            let scalar = char::from_u32(u32::from_str_radix(raw, 16).unwrap()).unwrap();
            assert_eq!(
                field.node.arguments[0].1.node,
                async_graphql_value::Value::String(scalar.to_string()),
                "{source}"
            );
        }
    }

    #[test]
    fn braced_unicode_does_not_decode_literal_backslashes_blocks_or_comments() {
        for source in [
            r#"{item(id:"\\u{1F600}"){id}}"#,
            r#"{item(id:"""\u{1F600}"""){id}}"#,
            "# \\u{1F600}\n{profile}",
        ] {
            assert!(edits(source, &tokens(source)).is_empty(), "{source}");
            assert!(parse_query(source).is_ok(), "{source}");
        }
    }

    #[test]
    fn invalid_braced_unicode_remains_owned_by_the_original_boundary() {
        for raw in ["", "G", "D800", "DFFF", "110000", "FFFFFFFF", "000000041"] {
            let source = format!(r#"{{item(id:"\u{{{raw}}}"){{id}}}}"#);
            assert!(edits(&source, &tokens(&source)).is_empty(), "{source}");
            assert!(parse_query(&source).is_err(), "{source}");
        }
        assert!(braced_escape(br"\u{1F600", 0).is_none());
    }

    #[test]
    fn braced_unicode_restores_original_following_field_positions() {
        let source = "{item(id:\"\\u{0001F600}\\u{22}\\u{5c}\"){id}\r\n profile}";
        let document = parse_query(source).unwrap();
        let (_, operation) = document.operations.iter().next().unwrap();
        let Selection::Field(field) = &operation.node.selection_set.node.items[1].node else {
            panic!("expected field");
        };
        let expected = tokens(source)
            .into_iter()
            .find(|token| token.text == "profile")
            .unwrap();
        assert_eq!(field.pos, expected.pos);
        assert_eq!(field.node.name.pos, expected.pos);
    }

    #[test]
    fn braced_unicode_composes_with_reordered_variable_directives() {
        let source = r#"query Q($id: ID = "\u{1F600}" @skip(if:true)) { item(id:$id) { id } }"#;
        let document = parse_query(source).unwrap();
        let (_, operation) = document.operations.iter().next().unwrap();
        assert_eq!(
            operation.node.variable_definitions[0]
                .node
                .default_value
                .as_ref()
                .unwrap()
                .node,
            async_graphql_value::ConstValue::String("😀".into())
        );
    }

    #[test]
    fn lexical_view_only_changes_valid_pairs_and_graphql_ignored_type_text() {
        for source in [
            r#"{item(id:"\\uD83D\\uDE00"){id}}"#,
            r#"{item(id:"""\uD83D\uDE00"""){id}}"#,
            "# query($n:[ Int ! ])\n{profile}",
            "query($n:[ Int\u{b}! ]){profile}",
            "query($n:[ I nt ! ]){profile}",
            r#"{item(id:"\uD800x\uDC00"){id}}"#,
            r#"{item(id:"\uDC00\uD800"){id}}"#,
        ] {
            assert!(edits(source, &tokens(source)).is_empty(), "{source}");
        }
        let source = "query($n:[ # comment\r\n Int , ! ] !){item(id:\"\\uD83D\\uDE00\"){id}}";
        let view = View::new(source, edits(source, &tokens(source)));
        assert_eq!(view.text, "query($n:[Int!]!){item(id:\"😀\"){id}}");
        let mapped = parse_query(source).unwrap();
        let (_, operation) = mapped.operations.iter().next().unwrap();
        let Selection::Field(field) = &operation.node.selection_set.node.items[0].node else {
            panic!("expected field");
        };
        let expected = tokens(source)
            .into_iter()
            .find(|token| token.text == "item")
            .unwrap()
            .pos;
        assert_eq!(field.pos, expected);
        assert_eq!(
            field.node.arguments[0].1.node,
            async_graphql_value::Value::String("😀".into())
        );
    }

    #[test]
    fn unsupported_scalars_keep_frozen_diagnostics_and_original_coordinates() {
        // Exact frozen controls include the original surrogate-pair spelling:
        // its unsupported scalar is at column 30, not the old fallback's 10.
        for (source, column) in [
            (r#"{item(id:"\uD83D\uDE00"){id} %}"#, 30),
            ("{profile %}", 10),
        ] {
            assert_eq!(
                parse_query(source).unwrap_err(),
                Error::Syntax {
                    message: "Syntax Error: Unexpected character: '%'.".into(),
                    start: Pos { line: 1, column },
                    end: None,
                },
                "{source}"
            );
        }
    }

    #[test]
    fn closing_argument_list_after_colon_keeps_original_diagnostic_and_order() {
        for (source, column) in [
            ("{items(first:)} query($x:Int=$bad){profile}", 14),
            ("{items(first:)} query($x:ID=\"\\uD800\"){profile}", 14),
            ("query($x:Int=1 @skip(if:true)){items(first:)}", 44),
            ("{items(first:){nodes{id}}}", 14),
        ] {
            assert_eq!(
                parse_query(source).unwrap_err(),
                Error::Syntax {
                    message: "Syntax Error: Unexpected ')'.".into(),
                    start: Pos { line: 1, column },
                    end: None,
                },
                "{source}"
            );
        }
    }

    #[test]
    fn missing_field_argument_colon_retains_versioned_ownership_and_first_failure() {
        // The ordinary diagnostic-ownership contract already owns the field
        // argument colon. These exact frozen-oracle results supersede the old
        // framework-equality controls, including before later owned failures.
        for (source, column, message) in [
            (
                "{items(first 1)} query($x:Int=$bad){profile}",
                14,
                "Syntax Error: Expected ':', found Int '1'.",
            ),
            (
                "{items(first 1)} query($x:ID=\"\\uD800\"){profile}",
                14,
                "Syntax Error: Expected ':', found Int '1'.",
            ),
            (
                "{items(first)}",
                13,
                "Syntax Error: Expected ':', found ')'.",
            ),
        ] {
            let expected = Error::Syntax {
                message: message.into(),
                start: Pos { line: 1, column },
                end: None,
            };
            assert_eq!(parse_query(source).unwrap_err(), expected, "{source}");
            assert_ne!(
                async_graphql_parser::parse_query(source).unwrap_err(),
                expected,
                "the versioned field-argument-colon diagnostic is distinct: {source}"
            );
        }
    }

    #[test]
    fn categorical_field_boundaries_keep_exact_frozen_diagnostics() {
        // Versioned category ownership replaces the seven old framework-equality
        // assertions using exact frozen evidence, never the candidate's output.
        for (source, column, message) in [
            (
                "{profile % items(first:)}",
                10,
                "Unexpected character: '%'.",
            ),
            ("{items()}", 8, "Expected Name, found ')'."),
            ("{items(first:])}", 14, "Unexpected ']'."),
            ("{items(first:[)])}", 15, "Unexpected ')'."),
            ("{items(hasRendition:{kind:})}", 27, "Unexpected '}'."),
        ] {
            assert_eq!(
                parse_query(source).unwrap_err(),
                Error::Syntax {
                    message: format!("Syntax Error: {message}"),
                    start: Pos { line: 1, column },
                    end: None,
                },
                "{source}"
            );
        }
    }

    #[test]
    fn admitted_default_directive_view_restores_values_and_all_ast_positions() {
        let source = "query Q($id: ID = \"\\uD83D\\uDE00\" @skip(if:true), $xs: [ ID ! ] = [\"x\"] @include(if:false)) { item(id:$id) { id } }";
        let document = parse_query(source).unwrap();
        let (_, operation) = document.operations.iter().next().unwrap();
        let variables = &operation.node.variable_definitions;
        assert_eq!(variables.len(), 2);
        assert_eq!(
            variables[0].node.default_value.as_ref().unwrap().node,
            async_graphql_value::ConstValue::String("😀".into())
        );
        let source_tokens = tokens(source);
        for (variable, variable_name, directive_name) in [
            (&variables[0], "id", "skip"),
            (&variables[1], "xs", "include"),
        ] {
            assert_eq!(
                variable.node.name.pos,
                source_tokens
                    .iter()
                    .find(|t| t.text == variable_name)
                    .unwrap()
                    .pos
            );
            let directive = &variable.node.directives[0];
            assert_eq!(
                directive.node.name.pos,
                source_tokens
                    .iter()
                    .find(|t| t.text == directive_name)
                    .unwrap()
                    .pos
            );
            assert_eq!(
                variable.node.default_value.as_ref().unwrap().pos,
                source_tokens
                    .iter()
                    .skip_while(|t| t.text != variable_name)
                    .skip_while(|t| t.text != "=")
                    .nth(1)
                    .unwrap()
                    .pos
            );
        }
        for source in [
            "query($x:Int @skip(if:true)=1){items(first:$x){nodes{id}}}",
            "query($x:Int=1 @skip(if:true)=2){items(first:$x){nodes{id}}}",
        ] {
            assert!(
                parse_query(source)
                    .unwrap_err()
                    .to_string()
                    .contains("Expected '$', found '='."),
                "{source}"
            );
        }
    }

    #[test]
    fn deep_constants_never_fall_back_to_variable_accepting_directives() {
        for depth in [129, 190, 500] {
            for (open, close) in [("[", "]"), ("{key:", "}")] {
                let nested = format!("{}$bad{}", open.repeat(depth), close.repeat(depth));
                for source in [
                    format!("query($x:Int={nested}){{profile}}"),
                    format!("query($x:Int @skip(if:{nested})){{profile}}"),
                ] {
                    assert!(
                        parse_query(&source)
                            .unwrap_err()
                            .to_string()
                            .contains("Unexpected variable '$bad' in constant value.")
                    );
                }
            }
        }
        let value = format!("{}1{}", "[".repeat(190), "]".repeat(190));
        assert!(parse_query(&format!("query($x:Int={value} @skip(if:true)){{profile}}")).is_ok());
    }

    #[test]
    fn deep_selections_do_not_skip_later_constant_grammar() {
        for depth in [63, 64, 65, 129] {
            let selections = format!("{}profile{}", "... {".repeat(depth), "}".repeat(depth));
            let source =
                format!("query A{{{selections}}} query B($n:Int @skip(if:$bad)){{profile}}");
            assert!(
                parse_query(&source)
                    .unwrap_err()
                    .to_string()
                    .contains("Unexpected variable '$bad' in constant value.")
            );
        }
    }

    #[test]
    fn parser_view_preserves_framework_recursion_guard() {
        let selections = format!("{}profile{}", "... {".repeat(70), "}".repeat(70));
        let plain = format!("query($x:Int=1){{{selections}}}");
        let adapted = format!("query($x:Int=1 @skip(if:true)){{{selections}}}");
        assert_eq!(
            parse_query(&plain).unwrap_err(),
            Error::RecursionLimitExceeded
        );
        assert_eq!(
            parse_query(&adapted).unwrap_err(),
            Error::RecursionLimitExceeded
        );
    }

    #[test]
    fn constant_directive_error_precedes_later_syntax_but_ignores_strings() {
        for source in [
            "query($x:Int! @skip(if:{z:[$first],a:$second})){items(first:)}",
            "query($x:[ Int ! ] ! @skip(if:$first)){profile %}",
            r#"query($x:Int! @skip(if:{before:"\uD83D\uDE00",after:$first})){profile %}"#,
        ] {
            assert!(
                parse_query(source)
                    .unwrap_err()
                    .to_string()
                    .contains("Unexpected variable '$first' in constant value.")
            );
        }
        assert!(
            parse_query(r#"query($x:Int! @skip(if:"$notVariable")){items(first:$x){nodes{id}}}"#)
                .is_ok()
        );
    }
}
