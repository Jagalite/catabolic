// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Original-source grammar boundaries needed by the executable parser adapter.
//!
//! This is a lazy recognizer, not a second AST or execution engine. Walking the
//! surrounding grammar prevents a later constant-value/Unicode error from hiding
//! an earlier syntax error. Variable-definition grammar, Unicode escapes and a
//! closing argument list immediately after its value colon, invalid strings,
//! unsupported scalars, malformed numbers, single quotes, empty field arguments,
//! and required field-value closing delimiters/bang are owned here. Required
//! field-value EOF is ordinary-owned only. Three exact field-directive
//! required-value position/closer pairs are owned in both policies; newly captured
//! directive value tuples are ordinary-only. Exact captured caller-site
//! name/colon tuples have independent ordinary/admission ownership; a root field
//! variable-reference name before `)` is ordinary-only. Generic sites remain
//! unowned apart from legacy variable/string exceptions. Directive lexical failures
//! share the same typed rules. Executable-document punctuation/spread is ordinary-owned;
//! only bang/spread is admission-owned via a private token discriminator.
//! Versioned admission
//! and ordinary diagnostic policies use failure provenance independently; unrelated failures
//! stop this walk and retain their existing owner.
use super::{Edit, Error, Pos};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Name,
    Number,
    String,
    Punct,
    Eof,
}
#[derive(Clone, Copy)]
struct Lexeme<'a> {
    text: &'a str,
    start: usize,
    kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrammarSite {
    Other,
    SelectionName(SelectionName),
    ArgumentName {
        context: ArgumentContext,
        first: bool,
    },
    DirectiveArgumentColon(ArgumentContext),
    ObjectMemberName(ValueContext),
    ObjectMemberColon(ValueContext),
    VariableReferenceName {
        context: ValueContext,
        position: ValuePosition,
    },
    // New argument-site promotions cover fields, not directive arguments.
    ArgumentStart,
    ArgumentColon,
    ArgumentValue,
    RequiredValue {
        context: ValueContext,
        position: ValuePosition,
    },
    ExecutableDocument,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionName {
    Field,
    Alias,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArgumentContext {
    Field,
    FieldDirective,
    VariableDefinitionDirective,
    Directive,
}
// Context is passed into each value walk; it is never a mutable owner flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValueContext {
    Argument(ArgumentContext),
    VariableDefault,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValuePosition {
    Root,
    ListElement,
    ObjectMember,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expected {
    Name,
    Literal(&'static str),
    Value,
    Unexpected,
    Other,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Found {
    Name,
    Number,
    String,
    Punctuation(char),
    Spread,
    Eof,
}
impl From<Lexeme<'_>> for Found {
    fn from(lexeme: Lexeme<'_>) -> Self {
        match lexeme.kind {
            Kind::Name => Self::Name,
            Kind::Number => Self::Number,
            Kind::String => Self::String,
            Kind::Punct if lexeme.text == "..." => Self::Spread,
            Kind::Punct => Self::Punctuation(lexeme.text.chars().next().unwrap()),
            Kind::Eof => Self::Eof,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumberReason {
    LeadingZero,
    MissingDigit,
    InvalidContinuation,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StringReason {
    InvalidEscape,
    InvalidUnicodeEscape,
    Unterminated,
    DanglingBackslash,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharacterReason {
    // Assigned only after token-start dispatch rejects whitespace, comments,
    // strings, recognized punctuation/spread, numbers, and ASCII names.
    UnsupportedScalar,
    SingleQuote,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind {
    Grammar {
        site: GrammarSite,
        expected: Expected,
        found: Found,
    },
    InvalidNumber(NumberReason),
    UnexpectedCharacter {
        reason: CharacterReason,
        code_point: char,
        initial: bool,
    },
    String(StringReason),
    TokenLimit,
}
struct Failure {
    error: Error,
    // Existing variable/string ownership is retained separately from both
    // versioned promotions. No message or whole-document matching is used.
    owned: bool,
    kind: FailureKind,
}
impl Failure {
    // The fixed residual capture witnesses these name/colon boundaries in both
    // ordinary parsing and before a later token overrun. Keep this typed set
    // separate from ordinary-only value/punctuation promotions below.
    fn captured_name_boundary(&self) -> bool {
        matches!(
            self.kind,
            FailureKind::Grammar {
                site: GrammarSite::SelectionName(SelectionName::Alias),
                expected: Expected::Name,
                found: Found::Punctuation('!'),
            } | FailureKind::Grammar {
                site: GrammarSite::ArgumentName {
                    context: ArgumentContext::Field,
                    first: false,
                },
                expected: Expected::Name,
                found: Found::Punctuation(':' | '!'),
            } | FailureKind::Grammar {
                site: GrammarSite::ArgumentName {
                    context: ArgumentContext::Directive,
                    first: true,
                },
                expected: Expected::Name,
                found: Found::Punctuation(')'),
            } | FailureKind::Grammar {
                site: GrammarSite::DirectiveArgumentColon(ArgumentContext::Directive),
                expected: Expected::Literal(":"),
                found: Found::Name,
            } | FailureKind::Grammar {
                site: GrammarSite::DirectiveArgumentColon(ArgumentContext::FieldDirective),
                expected: Expected::Literal(":"),
                found: Found::Number,
            } | FailureKind::Grammar {
                site: GrammarSite::ArgumentName {
                    context: ArgumentContext::FieldDirective,
                    first: true,
                },
                expected: Expected::Name,
                found: Found::Punctuation(':'),
            } | FailureKind::Grammar {
                site: GrammarSite::ObjectMemberName(ValueContext::Argument(
                    ArgumentContext::FieldDirective,
                )),
                expected: Expected::Name,
                found: Found::Punctuation(':'),
            } | FailureKind::Grammar {
                site: GrammarSite::ObjectMemberColon(ValueContext::Argument(
                    ArgumentContext::FieldDirective,
                )),
                expected: Expected::Literal(":"),
                found: Found::Number,
            } | FailureKind::Grammar {
                site: GrammarSite::ObjectMemberColon(ValueContext::Argument(
                    ArgumentContext::Field
                )),
                expected: Expected::Literal(":"),
                found: Found::Name,
            } | FailureKind::Grammar {
                site: GrammarSite::VariableReferenceName {
                    context: ValueContext::Argument(ArgumentContext::FieldDirective),
                    position: ValuePosition::Root,
                },
                expected: Expected::Name,
                found: Found::Punctuation(')'),
            } | FailureKind::Grammar {
                site: GrammarSite::VariableReferenceName {
                    context: ValueContext::Argument(ArgumentContext::Field),
                    position: ValuePosition::ListElement,
                },
                expected: Expected::Name,
                found: Found::Punctuation(']'),
            } | FailureKind::Grammar {
                site: GrammarSite::VariableReferenceName {
                    context: ValueContext::Argument(ArgumentContext::Field),
                    position: ValuePosition::ObjectMember,
                },
                expected: Expected::Name,
                found: Found::Punctuation(')'),
            }
        )
    }

    fn captured_ordinary_boundary(&self) -> bool {
        matches!(
            self.kind,
            FailureKind::Grammar {
                site: GrammarSite::RequiredValue {
                    context: ValueContext::Argument(ArgumentContext::Field),
                    position: ValuePosition::ObjectMember,
                },
                expected: Expected::Value,
                found: Found::Eof,
            } | FailureKind::Grammar {
                site: GrammarSite::RequiredValue {
                    context: ValueContext::Argument(
                        ArgumentContext::FieldDirective | ArgumentContext::Directive,
                    ),
                    ..
                },
                expected: Expected::Value,
                found: Found::Punctuation('!') | Found::Eof,
            } | FailureKind::Grammar {
                site: GrammarSite::RequiredValue {
                    context: ValueContext::Argument(ArgumentContext::FieldDirective),
                    position: ValuePosition::Root | ValuePosition::ListElement,
                },
                expected: Expected::Value,
                found: Found::Punctuation('}'),
            } | FailureKind::Grammar {
                site: GrammarSite::RequiredValue {
                    context: ValueContext::Argument(ArgumentContext::FieldDirective),
                    position: ValuePosition::ObjectMember,
                },
                expected: Expected::Value,
                found: Found::Punctuation(')' | ']'),
            } | FailureKind::Grammar {
                site: GrammarSite::RequiredValue {
                    context: ValueContext::Argument(ArgumentContext::Directive),
                    position: ValuePosition::Root,
                },
                expected: Expected::Value,
                found: Found::Punctuation(']'),
            } | FailureKind::Grammar {
                site: GrammarSite::RequiredValue {
                    context: ValueContext::Argument(ArgumentContext::Directive),
                    position: ValuePosition::ListElement,
                },
                expected: Expected::Value,
                found: Found::Punctuation(')'),
            } | FailureKind::Grammar {
                site: GrammarSite::RequiredValue {
                    context: ValueContext::Argument(ArgumentContext::Directive),
                    position: ValuePosition::ObjectMember,
                },
                expected: Expected::Value,
                found: Found::Punctuation('}'),
            } | FailureKind::Grammar {
                site: GrammarSite::RequiredValue {
                    context: ValueContext::Argument(
                        ArgumentContext::Field | ArgumentContext::FieldDirective,
                    ),
                    position: ValuePosition::Root,
                },
                expected: Expected::Value,
                found: Found::Spread,
            } | FailureKind::Grammar {
                site: GrammarSite::ArgumentName {
                    context: ArgumentContext::FieldDirective,
                    first: false,
                },
                expected: Expected::Name,
                found: Found::Eof,
            } | FailureKind::Grammar {
                site: GrammarSite::ExecutableDocument,
                expected: Expected::Unexpected,
                found: Found::Spread
                    | Found::Punctuation(
                        '$' | '&' | '(' | ')' | ':' | '=' | '@' | '[' | ']' | '}' | '|'
                    ),
            }
        )
    }

    fn before_limit(&self) -> bool {
        if self.captured_name_boundary() {
            return true;
        }
        matches!(
            self.kind,
            FailureKind::Grammar {
                site: GrammarSite::SelectionName(SelectionName::Field),
                expected: Expected::Name,
                found: Found::Punctuation('}' | '!'),
            } | FailureKind::Grammar {
                site: GrammarSite::ArgumentName {
                    context: ArgumentContext::Field,
                    first: true
                },
                expected: Expected::Name,
                found: Found::Punctuation(':'),
            } | FailureKind::Grammar {
                site: GrammarSite::ArgumentName {
                    context: ArgumentContext::FieldDirective
                        | ArgumentContext::VariableDefinitionDirective,
                    first: true,
                },
                expected: Expected::Name,
                found: Found::Punctuation(')'),
            } | FailureKind::Grammar {
                site: GrammarSite::DirectiveArgumentColon(
                    ArgumentContext::FieldDirective | ArgumentContext::VariableDefinitionDirective
                ),
                expected: Expected::Literal(":"),
                found: Found::Name,
            } | FailureKind::Grammar {
                site: GrammarSite::ObjectMemberName(ValueContext::Argument(ArgumentContext::Field)),
                expected: Expected::Name,
                found: Found::Punctuation(':'),
            } | FailureKind::Grammar {
                site: GrammarSite::ObjectMemberColon(ValueContext::Argument(
                    ArgumentContext::Field
                )),
                expected: Expected::Literal(":"),
                found: Found::Number,
            } | FailureKind::Grammar {
                site: GrammarSite::ArgumentValue,
                expected: Expected::Value,
                found: Found::Punctuation(')'),
            } | FailureKind::Grammar {
                site: GrammarSite::RequiredValue {
                    context: ValueContext::Argument(ArgumentContext::Field),
                    ..
                },
                expected: Expected::Value,
                found: Found::Punctuation(')' | ']' | '}' | '!'),
            } | FailureKind::Grammar {
                site: GrammarSite::RequiredValue {
                    context: ValueContext::Argument(ArgumentContext::FieldDirective),
                    position: ValuePosition::Root,
                },
                expected: Expected::Value,
                found: Found::Punctuation(']'),
            } | FailureKind::Grammar {
                site: GrammarSite::RequiredValue {
                    context: ValueContext::Argument(ArgumentContext::FieldDirective),
                    position: ValuePosition::ListElement,
                },
                expected: Expected::Value,
                found: Found::Punctuation(')'),
            } | FailureKind::Grammar {
                site: GrammarSite::RequiredValue {
                    context: ValueContext::Argument(ArgumentContext::FieldDirective),
                    position: ValuePosition::ObjectMember,
                },
                expected: Expected::Value,
                found: Found::Punctuation('}'),
            } | FailureKind::Grammar {
                site: GrammarSite::ArgumentStart,
                expected: Expected::Name,
                found: Found::Punctuation(')'),
            } | FailureKind::Grammar {
                site: GrammarSite::ArgumentColon,
                expected: Expected::Literal(":"),
                ..
            } | FailureKind::Grammar {
                site: GrammarSite::ExecutableDocument,
                expected: Expected::Unexpected,
                found: Found::Punctuation('!') | Found::Spread,
            } | FailureKind::InvalidNumber(
                NumberReason::LeadingZero
                    | NumberReason::MissingDigit
                    | NumberReason::InvalidContinuation,
            ) | FailureKind::UnexpectedCharacter {
                reason: CharacterReason::UnsupportedScalar | CharacterReason::SingleQuote,
                ..
            } | FailureKind::String(
                StringReason::InvalidEscape | StringReason::InvalidUnicodeEscape,
            )
        )
    }

    fn owned(&self) -> bool {
        self.owned
            || self.captured_name_boundary()
            || self.captured_ordinary_boundary()
            || matches!(
                self.kind,
                FailureKind::Grammar {
                    site: GrammarSite::SelectionName(SelectionName::Field),
                    expected: Expected::Name,
                    found: Found::Punctuation('}' | '!'),
                } | FailureKind::Grammar {
                    site: GrammarSite::ArgumentName {
                        context: ArgumentContext::Field,
                        first: true
                    },
                    expected: Expected::Name,
                    found: Found::Punctuation(':'),
                } | FailureKind::Grammar {
                    site: GrammarSite::ArgumentName {
                        context: ArgumentContext::FieldDirective
                            | ArgumentContext::VariableDefinitionDirective,
                        first: true,
                    },
                    expected: Expected::Name,
                    found: Found::Punctuation(')'),
                } | FailureKind::Grammar {
                    site: GrammarSite::DirectiveArgumentColon(
                        ArgumentContext::FieldDirective
                            | ArgumentContext::VariableDefinitionDirective
                    ),
                    expected: Expected::Literal(":"),
                    found: Found::Name,
                } | FailureKind::Grammar {
                    site: GrammarSite::ObjectMemberName(ValueContext::Argument(
                        ArgumentContext::Field
                    )),
                    expected: Expected::Name,
                    found: Found::Punctuation(':'),
                } | FailureKind::Grammar {
                    site: GrammarSite::ObjectMemberColon(ValueContext::Argument(
                        ArgumentContext::Field
                    )),
                    expected: Expected::Literal(":"),
                    found: Found::Number,
                } | FailureKind::Grammar {
                    site: GrammarSite::VariableReferenceName {
                        context: ValueContext::Argument(ArgumentContext::Field),
                        position: ValuePosition::Root,
                    },
                    expected: Expected::Name,
                    found: Found::Punctuation(')'),
                } | FailureKind::UnexpectedCharacter {
                    reason: CharacterReason::UnsupportedScalar | CharacterReason::SingleQuote,
                    ..
                } | FailureKind::Grammar {
                    site: GrammarSite::RequiredValue {
                        context: ValueContext::Argument(ArgumentContext::FieldDirective),
                        position: ValuePosition::Root,
                    },
                    expected: Expected::Value,
                    found: Found::Punctuation(']'),
                } | FailureKind::Grammar {
                    site: GrammarSite::RequiredValue {
                        context: ValueContext::Argument(ArgumentContext::FieldDirective),
                        position: ValuePosition::ListElement,
                    },
                    expected: Expected::Value,
                    found: Found::Punctuation(')'),
                } | FailureKind::Grammar {
                    site: GrammarSite::RequiredValue {
                        context: ValueContext::Argument(ArgumentContext::FieldDirective),
                        position: ValuePosition::ObjectMember,
                    },
                    expected: Expected::Value,
                    found: Found::Punctuation('}'),
                } | FailureKind::Grammar {
                    site: GrammarSite::ArgumentStart,
                    expected: Expected::Name,
                    found: Found::Punctuation(')'),
                } | FailureKind::Grammar {
                    site: GrammarSite::RequiredValue {
                        context: ValueContext::Argument(ArgumentContext::Field),
                        ..
                    },
                    expected: Expected::Value,
                    found: Found::Punctuation(')' | ']' | '}' | '!'),
                } | FailureKind::Grammar {
                    site: GrammarSite::RequiredValue {
                        context: ValueContext::Argument(ArgumentContext::Field),
                        position: ValuePosition::Root | ValuePosition::ListElement,
                    },
                    expected: Expected::Value,
                    found: Found::Eof,
                } | FailureKind::Grammar {
                    site: GrammarSite::ArgumentColon,
                    expected: Expected::Literal(":"),
                    ..
                } | FailureKind::Grammar {
                    site: GrammarSite::ExecutableDocument,
                    expected: Expected::Unexpected,
                    found: Found::Punctuation('!'),
                } | FailureKind::InvalidNumber(
                    NumberReason::LeadingZero
                        | NumberReason::MissingDigit
                        | NumberReason::InvalidContinuation,
                )
            )
    }
}
type Result<T> = std::result::Result<T, Failure>;

fn location(source: &str, offset: usize) -> Pos {
    // graphql-core computes locations with Python's splitlines on the prefix.
    let (mut line, mut column, mut previous_column) = (1, 1, 1);
    let mut previous = None;
    for c in source[..offset].chars() {
        if c == '\n' && previous == Some('\r') {
            previous = Some(c);
            continue;
        }
        if matches!(
            c,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'
                ..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            previous_column = column;
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
        previous = Some(c);
    }
    if line > 1 && column == 1 {
        Pos {
            line: line - 1,
            column: previous_column,
        }
    } else {
        Pos { line, column }
    }
}
fn syntax(source: &str, offset: usize, message: String) -> Error {
    Error::Syntax {
        message: format!("Syntax Error: {message}"),
        start: location(source, offset),
        end: None,
    }
}
fn point(source: &str, offset: usize) -> String {
    match source[offset..].chars().next() {
        None => "<EOF>".into(),
        Some(c) if (' '..='~').contains(&c) => format!("'{c}'"),
        Some(c) => format!("U+{:04X}", c as u32),
    }
}
fn name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}
fn name_continue(c: u8) -> bool {
    name_start(c) || c.is_ascii_digit()
}

struct Lexer<'a> {
    source: &'a str,
    cursor: usize,
    maximum: Option<usize>,
    count: usize,
    started: bool,
}
impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            cursor: 0,
            maximum: None,
            count: 0,
            started: false,
        }
    }
    fn fail<T>(&self, offset: usize, message: String, kind: FailureKind, owned: bool) -> Result<T> {
        Err(Failure {
            error: syntax(self.source, offset, message),
            owned,
            kind,
        })
    }
    fn digit(&self, offset: usize) -> bool {
        self.source
            .as_bytes()
            .get(offset)
            .is_some_and(u8::is_ascii_digit)
    }
    fn digits(&self, mut i: usize) -> Result<usize> {
        if !self.digit(i) {
            return self.fail(
                i,
                format!(
                    "Invalid number, expected digit but got: {}.",
                    point(self.source, i)
                ),
                FailureKind::InvalidNumber(NumberReason::MissingDigit),
                false,
            );
        }
        while self.digit(i) {
            i += 1;
        }
        Ok(i)
    }
    fn unicode_escape(&self, i: usize) -> Result<usize> {
        let bytes = self.source.as_bytes();
        let hex = |at: usize| -> Option<u32> {
            let raw = bytes.get(at..at + 4)?;
            if !raw.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            u32::from_str_radix(std::str::from_utf8(raw).ok()?, 16).ok()
        };
        if bytes.get(i + 2) == Some(&b'{') {
            let mut size = 3;
            let mut value = Some(0u32);
            for c in self.source[i + 3..].chars().take(9) {
                size += c.len_utf8();
                if c == '}' {
                    if size >= 5 && value.and_then(char::from_u32).is_some() {
                        return Ok(i + size);
                    }
                    break;
                }
                value = value.and_then(|v| {
                    c.to_digit(16)
                        .and_then(|c| v.checked_mul(16)?.checked_add(c))
                });
                if value.is_none() {
                    break;
                }
            }
            let text = &self.source[i..i + size];
            return self.fail(
                i,
                format!("Invalid Unicode escape sequence: '{text}'."),
                FailureKind::String(StringReason::InvalidUnicodeEscape),
                true,
            );
        }
        if let Some(value) = hex(i + 2) {
            if char::from_u32(value).is_some() {
                return Ok(i + 6);
            }
            if (0xd800..=0xdbff).contains(&value)
                && bytes.get(i + 6..i + 8) == Some(b"\\u")
                && hex(i + 8).is_some_and(|v| (0xdc00..=0xdfff).contains(&v))
            {
                return Ok(i + 12);
            }
        }
        let text: String = self.source[i..].chars().take(6).collect();
        self.fail(
            i,
            format!("Invalid Unicode escape sequence: '{text}'."),
            FailureKind::String(StringReason::InvalidUnicodeEscape),
            true,
        )
    }
    fn next(&mut self) -> Result<Lexeme<'a>> {
        let bytes = self.source.as_bytes();
        let mut i = self.cursor;
        while i < bytes.len() {
            match bytes[i] {
                b' ' | b'\t' | b'\r' | b'\n' | b',' => i += 1,
                b'#' => {
                    self.count += 1;
                    while i < bytes.len() && !matches!(bytes[i], b'\r' | b'\n') {
                        i += 1;
                    }
                }
                _ if self.source[i..].starts_with('\u{feff}') => i += 3,
                _ => break,
            }
        }
        let start = i;
        let kind = match bytes.get(i).copied() {
            None => Kind::Eof,
            Some(b'"') => {
                let block = self.source[i..].starts_with("\"\"\"");
                i += if block { 3 } else { 1 };
                loop {
                    if i == bytes.len() || !block && matches!(bytes[i], b'\r' | b'\n') {
                        return self.fail(
                            i,
                            "Unterminated string.".into(),
                            FailureKind::String(StringReason::Unterminated),
                            true,
                        );
                    }
                    if block && self.source[i..].starts_with("\\\"\"\"") {
                        i += 4;
                    } else if block && self.source[i..].starts_with("\"\"\"") {
                        i += 3;
                        break;
                    } else if !block && bytes[i] == b'"' {
                        i += 1;
                        break;
                    } else if !block && bytes[i] == b'\\' {
                        if bytes.get(i + 1) == Some(&b'u') {
                            i = self.unicode_escape(i)?;
                        } else if bytes.get(i + 1).is_some_and(|c| b"\"\\/bfnrt".contains(c)) {
                            i += 2;
                        } else {
                            let text: String = self.source[i..].chars().take(2).collect();
                            return self.fail(
                                i,
                                format!("Invalid character escape sequence: '{text}'."),
                                FailureKind::String(if bytes.get(i + 1).is_some() {
                                    StringReason::InvalidEscape
                                } else {
                                    StringReason::DanglingBackslash
                                }),
                                // The frozen oracle raises IndexError for a dangling
                                // backslash. Keep that non-parity framework-owned.
                                bytes.get(i + 1).is_some(),
                            );
                        }
                    } else {
                        i += self.source[i..].chars().next().unwrap().len_utf8();
                    }
                }
                Kind::String
            }
            Some(c) if name_start(c) => {
                i += 1;
                while bytes.get(i).copied().is_some_and(name_continue) {
                    i += 1;
                }
                Kind::Name
            }
            Some(c) if c.is_ascii_digit() || c == b'-' => {
                if c == b'-' {
                    i += 1;
                }
                if bytes.get(i) == Some(&b'0') {
                    i += 1;
                    if self.digit(i) {
                        return self.fail(
                            i,
                            format!(
                                "Invalid number, unexpected digit after 0: {}.",
                                point(self.source, i)
                            ),
                            FailureKind::InvalidNumber(NumberReason::LeadingZero),
                            false,
                        );
                    }
                } else {
                    i = self.digits(i)?;
                }
                if bytes.get(i) == Some(&b'.') {
                    i = self.digits(i + 1)?;
                }
                if bytes.get(i).is_some_and(|c| b"eE".contains(c)) {
                    i += 1;
                    if bytes.get(i).is_some_and(|c| b"+-".contains(c)) {
                        i += 1;
                    }
                    i = self.digits(i)?;
                }
                if bytes.get(i).is_some_and(|c| *c == b'.' || name_start(*c)) {
                    return self.fail(
                        i,
                        format!(
                            "Invalid number, expected digit but got: {}.",
                            point(self.source, i)
                        ),
                        FailureKind::InvalidNumber(NumberReason::InvalidContinuation),
                        false,
                    );
                }
                Kind::Number
            }
            Some(c) if b"!$&():=@[]{|}".contains(&c) => {
                i += 1;
                Kind::Punct
            }
            Some(b'.') if self.source[i..].starts_with("...") => {
                i += 3;
                Kind::Punct
            }
            Some(_) => {
                let code_point = self.source[i..].chars().next().unwrap();
                let reason = if code_point == '\'' {
                    CharacterReason::SingleQuote
                } else {
                    CharacterReason::UnsupportedScalar
                };
                let message = if reason == CharacterReason::SingleQuote {
                    "Unexpected single quote character ('), did you mean to use a double quote (\")?".into()
                } else {
                    format!("Unexpected character: {}.", point(self.source, i))
                };
                return Err(Failure {
                    error: syntax(self.source, i, message),
                    owned: false,
                    kind: FailureKind::UnexpectedCharacter {
                        reason,
                        code_point,
                        initial: !self.started,
                    },
                });
            }
        };
        self.cursor = i;
        self.count += usize::from(kind != Kind::Eof);
        // Match the frozen parser's one-token advance: the next lexeme is read
        // before checking its combined count, including skipped comments. A
        // lexical failure at that advance can therefore precede the limit.
        // No token list or recursive parser is allocated here.
        if let Some(maximum) = self.maximum
            && self.count > maximum
        {
            return self.fail(
                start,
                format!("Document contains more than {maximum} tokens. Parsing aborted."),
                FailureKind::TokenLimit,
                false,
            );
        }
        self.started |= kind != Kind::Eof;
        Ok(Lexeme {
            text: &self.source[start..i],
            start,
            kind,
        })
    }
}

// Lexing has already validated every escape before this is used in an error's
// token description. Decoding here does not admit a value into the executable AST.
fn string_value(text: &str) -> String {
    if text.starts_with("\"\"\"") {
        let content = text[3..text.len() - 3]
            .replace("\\\"\"\"", "\"\"\"")
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        let mut lines: Vec<_> = content.split('\n').map(str::to_owned).collect();
        let indentation = lines
            .iter()
            .skip(1)
            .filter_map(|line| {
                let leading = line.chars().take_while(|c| matches!(c, ' ' | '\t')).count();
                (leading < line.chars().count()).then_some(leading)
            })
            .min()
            .unwrap_or(0);
        for line in lines.iter_mut().skip(1) {
            *line = line.chars().skip(indentation).collect();
        }
        let blank = |line: &String| line.chars().all(|c| matches!(c, ' ' | '\t'));
        let start = lines
            .iter()
            .position(|line| !blank(line))
            .unwrap_or(lines.len());
        let end = lines
            .iter()
            .rposition(|line| !blank(line))
            .map_or(start, |i| i + 1);
        return lines[start..end].join("\n");
    }
    let mut result = String::new();
    let mut chars = text[1..text.len() - 1].chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        let escaped = match chars.next().expect("validated escape") {
            'u' => {
                let mut value = 0;
                if chars.peek() == Some(&'{') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                        value = value * 16 + c.to_digit(16).expect("validated hex");
                    }
                } else {
                    for c in chars.by_ref().take(4) {
                        value = value * 16 + c.to_digit(16).expect("validated hex");
                    }
                    if (0xd800..=0xdbff).contains(&value) {
                        chars.next();
                        chars.next();
                        let mut low = 0;
                        for c in chars.by_ref().take(4) {
                            low = low * 16 + c.to_digit(16).expect("validated pair");
                        }
                        value = 0x10000 + ((value - 0xd800) << 10) + low - 0xdc00;
                    }
                }
                char::from_u32(value).expect("validated Unicode scalar")
            }
            'b' => '\u{8}',
            'f' => '\u{c}',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            c => c,
        };
        result.push(escaped);
    }
    result
}

struct Recognizer<'a> {
    lexer: Lexer<'a>,
    current: Lexeme<'a>,
    variable: bool,
    // A variable's default must precede directives in the source, but the
    // framework needs them in the opposite order in its private parser view.
    moves: Vec<(usize, usize, usize)>,
}
impl<'a> Recognizer<'a> {
    fn failure<T>(&self, message: String) -> Result<T> {
        self.failure_at(GrammarSite::Other, Expected::Other, message)
    }
    fn failure_at<T>(&self, site: GrammarSite, expected: Expected, message: String) -> Result<T> {
        Err(Failure {
            error: syntax(self.lexer.source, self.current.start, message),
            owned: self.variable,
            kind: FailureKind::Grammar {
                site,
                expected,
                found: self.current.into(),
            },
        })
    }
    fn found(&self) -> String {
        match self.current.kind {
            Kind::Eof => "<EOF>".into(),
            Kind::Name => format!("Name '{}'", self.current.text),
            Kind::Number => format!(
                "{} '{}'",
                if self.current.text.contains(['.', 'e', 'E']) {
                    "Float"
                } else {
                    "Int"
                },
                self.current.text
            ),
            Kind::String => format!(
                "{} '{}'",
                if self.current.text.starts_with("\"\"\"") {
                    "BlockString"
                } else {
                    "String"
                },
                string_value(self.current.text)
            ),
            Kind::Punct => format!("'{}'", self.current.text),
        }
    }
    fn advance(&mut self) -> Result<()> {
        self.current = self.lexer.next().map_err(|mut failure| {
            failure.owned |= self.variable;
            failure
        })?;
        Ok(())
    }
    fn eat(&mut self, text: &str) -> Result<bool> {
        if self.current.text != text || self.current.kind == Kind::Eof {
            return Ok(false);
        }
        self.advance()?;
        Ok(true)
    }
    fn expect(&mut self, text: &'static str) -> Result<()> {
        self.expect_at(text, GrammarSite::Other)
    }
    fn expect_at(&mut self, text: &'static str, site: GrammarSite) -> Result<()> {
        if self.eat(text)? {
            Ok(())
        } else {
            self.failure_at(
                site,
                Expected::Literal(text),
                format!("Expected '{text}', found {}.", self.found()),
            )
        }
    }
    fn name(&mut self) -> Result<()> {
        self.name_at(GrammarSite::Other)
    }
    fn name_at(&mut self, site: GrammarSite) -> Result<()> {
        if self.current.kind == Kind::Name {
            self.advance()
        } else {
            self.failure_at(
                site,
                Expected::Name,
                format!("Expected Name, found {}.", self.found()),
            )
        }
    }
    fn unexpected<T>(&self) -> Result<T> {
        self.unexpected_at(GrammarSite::Other)
    }
    fn unexpected_at<T>(&self, site: GrammarSite) -> Result<T> {
        self.failure_at(
            site,
            Expected::Unexpected,
            format!("Unexpected {}.", self.found()),
        )
    }
    fn value(&mut self, constant: bool, context: ValueContext) -> Result<()> {
        // Input nesting does not consume Rust call frames. Deferring a deep
        // constant value to the framework would be unsafe for grammar admission:
        // its variable-definition directives accept ordinary variable values.
        // The existing original-source token/byte limits bound this task stack.
        enum Task {
            Value(ValuePosition),
            List,
            Object,
        }
        let mut tasks = vec![Task::Value(ValuePosition::Root)];
        while let Some(task) = tasks.pop() {
            match task {
                Task::List => {
                    if !self.eat("]")? {
                        tasks.push(Task::List);
                        tasks.push(Task::Value(ValuePosition::ListElement));
                    }
                }
                Task::Object => {
                    if !self.eat("}")? {
                        self.name_at(GrammarSite::ObjectMemberName(context))?;
                        self.expect_at(":", GrammarSite::ObjectMemberColon(context))?;
                        tasks.push(Task::Object);
                        tasks.push(Task::Value(ValuePosition::ObjectMember));
                    }
                }
                Task::Value(position) => match self.current.text {
                    "$" if constant => {
                        let next = self.lexer.next().map_err(|mut failure| {
                            failure.owned |= self.variable;
                            failure
                        })?;
                        return if next.kind == Kind::Name {
                            self.failure(format!(
                                "Unexpected variable '${}' in constant value.",
                                next.text
                            ))
                        } else {
                            self.unexpected()
                        };
                    }
                    "$" => {
                        self.advance()?;
                        self.name_at(GrammarSite::VariableReferenceName { context, position })?;
                    }
                    "[" => {
                        self.advance()?;
                        tasks.push(Task::List);
                    }
                    "{" => {
                        self.advance()?;
                        tasks.push(Task::Object);
                    }
                    _ if matches!(self.current.kind, Kind::Name | Kind::Number | Kind::String) => {
                        self.advance()?
                    }
                    _ => {
                        return self.failure_at(
                            GrammarSite::RequiredValue { context, position },
                            Expected::Value,
                            format!("Unexpected {}.", self.found()),
                        );
                    }
                },
            }
        }
        Ok(())
    }
    fn arguments(&mut self, constant: bool, context: ArgumentContext) -> Result<()> {
        if self.eat("(")? {
            if context == ArgumentContext::Field
                && self.current.kind == Kind::Punct
                && self.current.text == ")"
            {
                return self.failure_at(
                    GrammarSite::ArgumentStart,
                    Expected::Name,
                    format!("Expected Name, found {}.", self.found()),
                );
            }
            let mut first = true;
            loop {
                self.name_at(GrammarSite::ArgumentName { context, first })?;
                self.expect_at(
                    ":",
                    if context == ArgumentContext::Field {
                        GrammarSite::ArgumentColon
                    } else {
                        GrammarSite::DirectiveArgumentColon(context)
                    },
                )?;
                // Preserve the older immediate-colon/`)` rule for fields and
                // directives. The separately versioned required-value category
                // below is field-scoped and must not withdraw this exception.
                if self.current.kind == Kind::Punct && self.current.text == ")" {
                    return Err(Failure {
                        error: syntax(
                            self.lexer.source,
                            self.current.start,
                            "Unexpected ')'.".into(),
                        ),
                        owned: true,
                        kind: FailureKind::Grammar {
                            site: GrammarSite::ArgumentValue,
                            expected: Expected::Value,
                            found: self.current.into(),
                        },
                    });
                }
                self.value(constant, ValueContext::Argument(context))?;
                if self.eat(")")? {
                    break;
                }
                first = false;
            }
        }
        Ok(())
    }
    fn directives(&mut self, constant: bool, context: ArgumentContext) -> Result<()> {
        while self.eat("@")? {
            self.name()?;
            self.arguments(constant, context)?;
        }
        Ok(())
    }
    fn variable_type(&mut self) -> Result<()> {
        let mut depth = 0;
        while self.eat("[")? {
            depth += 1;
        }
        self.name()?;
        self.eat("!")?;
        for _ in 0..depth {
            self.expect("]")?;
            self.eat("!")?;
        }
        Ok(())
    }
    fn variables(&mut self) -> Result<()> {
        if self.current.text != "(" {
            return Ok(());
        }
        self.variable = true;
        self.advance()?;
        loop {
            self.expect("$")?;
            self.name()?;
            self.expect(":")?;
            self.variable_type()?;
            let default_start = self.current.start;
            let default = self.eat("=")?;
            if default {
                self.value(true, ValueContext::VariableDefault)?;
            }
            let directive_start = self.current.start;
            let directive = self.current.text == "@";
            self.directives(true, ArgumentContext::VariableDefinitionDirective)?;
            if default && directive {
                self.moves
                    .push((default_start, directive_start, self.current.start));
            }
            if self.current.text == ")" {
                self.variable = false;
                self.advance()?;
                break;
            }
            // In particular, a default after any directive is not admitted.
        }
        Ok(())
    }
    fn selection_set(&mut self) -> Result<()> {
        // Stay iterative here too: an early recognizer-only recursion cutoff
        // could miss invalid constants in later definitions. The ordinary AST
        // parser still enforces its own recursion limit on admitted grammar.
        self.expect("{")?;
        let mut open_sets = 1;
        loop {
            let child = if self.eat("...")? {
                if self.eat("on")? {
                    self.name()?;
                    self.directives(false, ArgumentContext::Directive)?;
                    true
                } else if self.current.kind == Kind::Name {
                    self.name()?;
                    self.directives(false, ArgumentContext::Directive)?;
                    false
                } else {
                    self.directives(false, ArgumentContext::Directive)?;
                    true
                }
            } else {
                self.name_at(GrammarSite::SelectionName(SelectionName::Field))?;
                if self.eat(":")? {
                    self.name_at(GrammarSite::SelectionName(SelectionName::Alias))?;
                }
                self.arguments(false, ArgumentContext::Field)?;
                self.directives(false, ArgumentContext::FieldDirective)?;
                self.current.text == "{"
            };
            if child {
                self.expect("{")?;
                open_sets += 1;
                continue;
            }
            while self.eat("}")? {
                open_sets -= 1;
                if open_sets == 0 {
                    return Ok(());
                }
            }
        }
    }
    fn document(&mut self) -> Result<()> {
        while self.current.kind != Kind::Eof {
            match self.current.text {
                "{" => self.selection_set()?,
                "query" | "mutation" | "subscription" => {
                    self.advance()?;
                    if self.current.kind == Kind::Name {
                        self.name()?;
                    }
                    self.variables()?;
                    self.directives(false, ArgumentContext::Directive)?;
                    self.selection_set()?;
                }
                "fragment" => {
                    self.advance()?;
                    if self.current.text == "on" {
                        return self.unexpected();
                    }
                    self.name()?;
                    self.expect("on")?;
                    self.name()?;
                    self.directives(false, ArgumentContext::Directive)?;
                    self.selection_set()?;
                }
                _ => return self.unexpected_at(GrammarSite::ExecutableDocument),
            }
        }
        Ok(())
    }
}

/// Resolve only versioned failure categories before a known overrun.
/// The caller has already enforced byte/variable/timeout admission. At most
/// `maximum + 1` non-comment lexemes are read, with comments counted without
/// allocation. An unrelated grammar/lexer error stops the walk and retains the
/// existing budget result, rather than broadening generic syntax ownership.
pub(crate) fn before_token_limit(source: &str, maximum: usize) -> Option<Error> {
    let mut lexer = Lexer::new(source);
    lexer.maximum = Some(maximum);
    let result = lexer.next().and_then(|current| {
        Recognizer {
            lexer,
            current,
            variable: false,
            moves: vec![],
        }
        .document()
    });
    match result {
        Err(failure) if failure.before_limit() => Some(failure.error),
        _ => None,
    }
}

/// Return grammar-admitted variable rearrangements, or a scoped syntax error.
/// An unrelated earlier error leaves its diagnostics and grammar acceptance to
/// the ordinary parser. Both input and selection walks are iterative.
pub(super) fn inspect(source: &str, edits: &mut Vec<Edit>) -> std::result::Result<(), Error> {
    let mut lexer = Lexer::new(source);
    let current = match lexer.next() {
        Ok(current) => current,
        Err(failure) => {
            return if failure.owned() {
                Err(failure.error)
            } else {
                Ok(())
            };
        }
    };
    let mut recognizer = Recognizer {
        lexer,
        current,
        variable: false,
        moves: vec![],
    };
    match recognizer.document() {
        Err(failure) if failure.owned() => return Err(failure.error),
        // Earlier valid variable rewrites may still be needed to avoid masking
        // an unrelated later syntax error with the framework's grammar mismatch.
        _ => (),
    }
    for (start, middle, end) in recognizer.moves {
        let mut nested = vec![];
        edits.retain(|edit| {
            if edit.start >= start && edit.end <= end {
                nested.push(Edit {
                    start: edit.start,
                    end: edit.end,
                    chars: edit.chars.clone(),
                });
                false
            } else {
                true
            }
        });
        let mut chars = super::edited_chars(source, middle, end, &nested);
        chars.push((' ', middle));
        chars.extend(super::edited_chars(source, start, middle, &nested));
        edits.push(Edit { start, end, chars });
    }
    edits.sort_by_key(|edit| edit.start);
    Ok(())
}

#[cfg(test)]
mod string_diagnostic_tests {
    use super::*;

    #[test]
    fn string_termination_errors_are_owned_at_the_original_boundary() {
        for (source, offset, message) in [
            ("\"snow 雪😀", "\"snow 雪😀".len(), "Unterminated string."),
            ("\"a\nmore", 2, "Unterminated string."),
            ("\"a\r\nmore", 2, "Unterminated string."),
            ("\"\"\"雪\r\n", "\"\"\"雪\r\n".len(), "Unterminated string."),
            ("\"\\q", 1, "Invalid character escape sequence: '\\q'."),
        ] {
            let mut lexer = Lexer::new(source);
            let Err(failure) = lexer.next() else {
                panic!("expected error")
            };
            assert!(failure.owned, "{source:?}");
            assert_eq!(failure.error, syntax(source, offset, message.into()));
        }
    }

    #[test]
    fn dangling_backslash_retains_the_frozen_oracle_non_parity() {
        let mut lexer = Lexer::new("\"a\\");
        let Err(failure) = lexer.next() else {
            panic!("expected error")
        };
        assert!(!failure.owned);
    }

    #[test]
    fn escaped_quotes_and_block_delimiters_are_not_false_terminators() {
        for source in [r#""a\"b""#, r#""a\\""#, "\"\"\"a\\\"\"\"b\"\"\""] {
            let mut lexer = Lexer::new(source);
            let Ok(lexeme) = lexer.next() else {
                panic!("valid string {source:?}")
            };
            assert_eq!(lexeme.text, source);
            assert_eq!(lexer.cursor, source.len());
        }
    }
}

#[cfg(test)]
mod budget_precedence_tests {
    use super::*;

    fn first_failure(source: &str, maximum: Option<usize>) -> Failure {
        let mut lexer = Lexer::new(source);
        lexer.maximum = maximum;
        let result = lexer.next().and_then(|current| {
            Recognizer {
                lexer,
                current,
                variable: false,
                moves: vec![],
            }
            .document()
        });
        match result {
            Err(failure) => failure,
            Ok(()) => panic!("expected failure: {source:?}"),
        }
    }

    #[test]
    fn versioned_precedence_categories_keep_their_original_locations() {
        for (source, offset, message) in [
            ("{items(first:)}", 13, "Unexpected ')'."),
            ("?", 0, "Unexpected character: '?'."),
            ("{items()}", 7, "Expected Name, found ')'."),
            ("{items(first 1)}", 13, "Expected ':', found Int '1'."),
            ("!", 0, "Unexpected '!'."),
            ("😀", 0, "Unexpected character: U+1F600."),
            (
                "{items(first:01) items(first:)}",
                14,
                "Invalid number, unexpected digit after 0: '1'.",
            ),
            (
                r#"{items(search:"\q") items(first:)}"#,
                15,
                "Invalid character escape sequence: '\\q'.",
            ),
        ] {
            let source = format!("{source} {}", "profile ".repeat(4001));
            assert_eq!(
                before_token_limit(&source, 4000),
                Some(syntax(&source, offset, message.into()))
            );
        }
        // Retain the exact old inputs as positive ownership controls. Additional
        // branch text/process expectations await capture; the 13 historical
        // complete envelopes below provide the existing exact-text witnesses.
        for source in [
            "' {items(first:)}",
            "{items(first:1e+)} {items(first:)}",
            "{items(first:1x)} {items(first:)}",
            r#"{items(search:"\uD800")} {items(first:)}"#,
            "{} {items(first:)}",
            "{items(first:{name 1})} {items(first:)}",
            "{!} {items(first:)}",
        ] {
            assert!(before_token_limit(source, 4000).is_some(), "{source}");
        }
        for source in ["query($x:Int=$y){profile}", "\"a\\"] {
            assert!(before_token_limit(source, 4000).is_none(), "{source}");
        }
    }

    #[test]
    fn ordinary_ownership_is_versioned_independently_of_precedence() {
        for (source, offset, message) in [
            ("! {items(search:\"", 0, "Unexpected '!'."),
            ("? {items(search:\"", 0, "Unexpected character: '?'."),
            ("{items()} {items(search:\"", 7, "Expected Name, found ')'."),
            ("😀 {items(search:\"", 0, "Unexpected character: U+1F600."),
            (
                "{profile} ? {items(search:\"",
                10,
                "Unexpected character: '?'.",
            ),
            (
                "{items(first 1)} {items(search:\"",
                13,
                "Expected ':', found Int '1'.",
            ),
            (
                "{items(first:01)} {items(search:\"",
                14,
                "Invalid number, unexpected digit after 0: '1'.",
            ),
        ] {
            assert_eq!(
                inspect(source, &mut vec![]),
                Err(syntax(source, offset, message.into()))
            );
        }
        // The fixed residual capture now witnesses ordinary executable spread;
        // an earlier grammar failure keeps its independently witnessed ownership.
        let source = "... {items(search:\"";
        assert!(inspect(source, &mut vec![]).is_err(), "{source:?}");
        let source = "{items(first:{name 1})} {items(search:\"";
        assert!(inspect(source, &mut vec![]).is_err(), "{source:?}");
    }

    #[test]
    fn typed_lexical_failure_provenance_is_not_inferred_from_messages() {
        for (source, expected, eligible, owned) in [
            (
                "01",
                FailureKind::InvalidNumber(NumberReason::LeadingZero),
                true,
                true,
            ),
            (
                "1e+",
                FailureKind::InvalidNumber(NumberReason::MissingDigit),
                true,
                true,
            ),
            (
                "1x",
                FailureKind::InvalidNumber(NumberReason::InvalidContinuation),
                true,
                true,
            ),
            (
                r#""\q""#,
                FailureKind::String(StringReason::InvalidEscape),
                true,
                true,
            ),
            (
                r#""\uD800""#,
                FailureKind::String(StringReason::InvalidUnicodeEscape),
                true,
                true,
            ),
            (
                "\"a",
                FailureKind::String(StringReason::Unterminated),
                false,
                true,
            ),
            (
                "\"a\\",
                FailureKind::String(StringReason::DanglingBackslash),
                false,
                false,
            ),
        ] {
            let Err(failure) = Lexer::new(source).next() else {
                panic!("expected lexical failure: {source:?}");
            };
            assert_eq!(failure.kind, expected);
            assert_eq!(failure.before_limit(), eligible);
            assert_eq!(failure.owned(), owned);
        }
        let mut lexer = Lexer::new("!");
        lexer.maximum = Some(0);
        let Err(failure) = lexer.next() else {
            panic!("a successfully lexed token must stop at the limit");
        };
        assert_eq!(failure.kind, FailureKind::TokenLimit);
        assert!(!failure.before_limit());
        assert!(!failure.owned());
    }

    #[test]
    fn unsupported_scalar_provenance_owns_the_whole_lexical_category() {
        // All 25 exact inputs were captured from the pinned frozen oracle.
        for (code_point, message) in [
            ('\u{3f}', "Unexpected character: '?'."),
            ('\u{2f}', "Unexpected character: '/'."),
            ('\u{25}', "Unexpected character: '%'."),
            ('\u{2a}', "Unexpected character: '*'."),
            ('\u{2b}', "Unexpected character: '+'."),
            ('\u{2e}', "Unexpected character: '.'."),
            ('\u{3b}', "Unexpected character: ';'."),
            ('\u{3c}', "Unexpected character: '<'."),
            ('\u{3e}', "Unexpected character: '>'."),
            ('\u{5c}', "Unexpected character: '\\'."),
            ('\u{5e}', "Unexpected character: '^'."),
            ('\u{60}', "Unexpected character: '`'."),
            ('\u{7e}', "Unexpected character: '~'."),
            ('\u{0}', "Unexpected character: U+0000."),
            ('\u{b}', "Unexpected character: U+000B."),
            ('\u{7f}', "Unexpected character: U+007F."),
            ('\u{80}', "Unexpected character: U+0080."),
            ('\u{a0}', "Unexpected character: U+00A0."),
            ('\u{e9}', "Unexpected character: U+00E9."),
            ('\u{2028}', "Unexpected character: U+2028."),
            ('\u{d7ff}', "Unexpected character: U+D7FF."),
            ('\u{e000}', "Unexpected character: U+E000."),
            ('\u{10000}', "Unexpected character: U+10000."),
            ('\u{1f600}', "Unexpected character: U+1F600."),
            ('\u{10ffff}', "Unexpected character: U+10FFFF."),
        ] {
            let source = format!("{code_point} profile");
            let Err(failure) = Lexer::new(&source).next() else {
                panic!("expected unsupported scalar: {code_point:?}");
            };
            assert_eq!(
                failure.kind,
                FailureKind::UnexpectedCharacter {
                    reason: CharacterReason::UnsupportedScalar,
                    code_point,
                    initial: true,
                }
            );
            assert!(failure.before_limit());
            assert!(failure.owned());
            assert_eq!(failure.error, syntax(&source, 0, message.into()));
        }
    }

    #[test]
    fn scalar_extension_preserves_single_quote_tokens_numbers_and_strings() {
        let Err(quote) = Lexer::new("'").next() else {
            panic!("expected special single-quote failure");
        };
        assert_eq!(
            quote.kind,
            FailureKind::UnexpectedCharacter {
                reason: CharacterReason::SingleQuote,
                code_point: '\'',
                initial: true,
            }
        );
        assert!(quote.before_limit());
        assert!(quote.owned());
        assert_eq!(
            quote.error,
            syntax(
                "'",
                0,
                "Unexpected single quote character ('), did you mean to use a double quote (\")?"
                    .into()
            )
        );
        for source in [
            "!", "$", "&", "(", ")", ":", "=", "@", "[", "]", "{", "}", "|", "...",
        ] {
            let Ok(token) = Lexer::new(source).next() else {
                panic!("recognized token must not become a fallback: {source:?}");
            };
            assert!(token.kind == Kind::Punct);
        }
        for source in ["1e+é", "-é"] {
            let Err(failure) = Lexer::new(source).next() else {
                panic!("expected required-digit error");
            };
            assert_eq!(
                failure.kind,
                FailureKind::InvalidNumber(NumberReason::MissingDigit)
            );
            assert!(failure.before_limit());
        }
        for source in ["\"é\0😀\"", "\"\"\"é\0😀\"\"\"", "# é\0😀\nprofile"] {
            assert!(Lexer::new(source).next().is_ok(), "{source:?}");
        }
    }

    #[test]
    fn nul_document_is_supported_in_memory_without_an_argv_transport() {
        let source = format!("\0 {}profile", "profile ".repeat(4000));
        assert_eq!(
            before_token_limit(&source, 4000),
            Some(syntax(&source, 0, "Unexpected character: U+0000.".into()))
        );
        // The independent ordinary lexical-category contract owns NUL too.
        assert_eq!(
            inspect("\0", &mut vec![]),
            Err(syntax("\0", 0, "Unexpected character: U+0000.".into()))
        );
    }

    #[test]
    fn grammar_provenance_distinguishes_field_directive_and_document_sites() {
        for (source, site, expected, found, eligible, owned) in [
            (
                "{items()}",
                GrammarSite::ArgumentStart,
                Expected::Name,
                Found::Punctuation(')'),
                true,
                true,
            ),
            (
                "{items(first 1)}",
                GrammarSite::ArgumentColon,
                Expected::Literal(":"),
                Found::Number,
                true,
                true,
            ),
            (
                "{items(first:)}",
                GrammarSite::ArgumentValue,
                Expected::Value,
                Found::Punctuation(')'),
                true,
                true,
            ),
            (
                "!",
                GrammarSite::ExecutableDocument,
                Expected::Unexpected,
                Found::Punctuation('!'),
                true,
                true,
            ),
            (
                "{!}",
                GrammarSite::SelectionName(SelectionName::Field),
                Expected::Name,
                Found::Punctuation('!'),
                true,
                true,
            ),
            (
                "{profile @skip()}",
                GrammarSite::ArgumentName {
                    context: ArgumentContext::FieldDirective,
                    first: true,
                },
                Expected::Name,
                Found::Punctuation(')'),
                true,
                true,
            ),
            (
                "{profile @skip(if 1)}",
                GrammarSite::DirectiveArgumentColon(ArgumentContext::FieldDirective),
                Expected::Literal(":"),
                Found::Number,
                true,
                true,
            ),
        ] {
            let mut lexer = Lexer::new(source);
            let Ok(current) = lexer.next() else {
                panic!("expected initial lexeme: {source:?}");
            };
            let Err(failure) = (Recognizer {
                lexer,
                current,
                variable: false,
                moves: vec![],
            })
            .document() else {
                panic!("expected grammar failure: {source:?}");
            };
            assert_eq!(
                failure.kind,
                FailureKind::Grammar {
                    site,
                    expected,
                    found
                }
            );
            assert_eq!(failure.before_limit(), eligible);
            assert_eq!(failure.owned(), owned);
        }
    }

    #[test]
    fn required_field_value_closers_preserve_existing_ownership() {
        for (source, position, closer, offset) in [
            ("{items(first:])}", ValuePosition::Root, ']', 13),
            ("{items(first:})}", ValuePosition::Root, '}', 13),
            ("{items(first:[)])}", ValuePosition::ListElement, ')', 14),
            ("{items(first:[}])}", ValuePosition::ListElement, '}', 14),
            (
                "{items(hasRendition:{kind:)})}",
                ValuePosition::ObjectMember,
                ')',
                26,
            ),
            (
                "{items(hasRendition:{kind:]})}",
                ValuePosition::ObjectMember,
                ']',
                26,
            ),
            (
                "{items(hasRendition:{kind:})}",
                ValuePosition::ObjectMember,
                '}',
                26,
            ),
        ] {
            let mut lexer = Lexer::new(source);
            let Ok(current) = lexer.next() else {
                panic!("initial token")
            };
            let Err(failure) = (Recognizer {
                lexer,
                current,
                variable: false,
                moves: vec![],
            })
            .document() else {
                panic!("expected required value: {source}");
            };
            assert_eq!(
                failure.kind,
                FailureKind::Grammar {
                    site: GrammarSite::RequiredValue {
                        context: ValueContext::Argument(ArgumentContext::Field),
                        position,
                    },
                    expected: Expected::Value,
                    found: Found::Punctuation(closer),
                }
            );
            assert!(failure.owned());
            assert!(failure.before_limit());
            let expected = syntax(source, offset, format!("Unexpected '{closer}'."));
            assert_eq!(failure.error, expected);
            assert_eq!(inspect(source, &mut vec![]), Err(expected.clone()));
            let tail = format!("{source} {}", "profile ".repeat(4001));
            assert_eq!(before_token_limit(&tail, 4000), Some(expected));
        }
        for source in [
            "{items(hasRendition:{:1})}",
            "{items(hasRendition:{kind 1})}",
            "{items(first:$)}",
        ] {
            assert!(inspect(source, &mut vec![]).is_err(), "{source}");
            assert_eq!(
                before_token_limit(source, 4000).is_some(),
                source != "{items(first:$)}",
                "{source}"
            );
        }
        for source in [
            "{items(first:[]){id}}",
            "{items(hasRendition:{}){id}}",
            "{items(hasRendition:{kind:[{k:1}]}){id}}",
        ] {
            assert!(inspect(source, &mut vec![]).is_ok(), "{source}");
            assert!(before_token_limit(source, 4000).is_none(), "{source}");
        }
    }

    #[test]
    fn value_context_does_not_leak_and_directive_exceptions_are_preserved() {
        for (source, position) in [
            ("{profile @skip(if:])}", ValuePosition::Root),
            ("{profile @skip(if:[)])}", ValuePosition::ListElement),
            ("{profile @skip(if:{kind:})}", ValuePosition::ObjectMember),
            (
                "{items(first:[]) @skip(if:[)])}",
                ValuePosition::ListElement,
            ),
        ] {
            let mut lexer = Lexer::new(source);
            let Ok(current) = lexer.next() else {
                panic!("initial token")
            };
            let Err(failure) = (Recognizer {
                lexer,
                current,
                variable: false,
                moves: vec![],
            })
            .document() else {
                panic!("expected directive value failure: {source}");
            };
            assert!(matches!(failure.kind, FailureKind::Grammar {
                site: GrammarSite::RequiredValue { context: ValueContext::Argument(ArgumentContext::FieldDirective), position: actual },
                expected: Expected::Value, ..
            } if actual == position));
            assert!(failure.owned());
            assert!(failure.before_limit());
        }
        for (source, offset, message, eligible) in [
            ("{profile @skip(if:)}", 18, "Unexpected ')'.", true),
            (
                "query Q($x:Int @skip(if:[)])){profile}",
                25,
                "Unexpected ')'.",
                false,
            ),
            ("query Q($x:Int=]){profile}", 15, "Unexpected ']'.", false),
            (
                "{profile @skip(if:[]) items(first:[)])}",
                35,
                "Unexpected ')'.",
                true,
            ),
            (
                "{profile @skip(if:%)}",
                18,
                "Unexpected character: '%'.",
                true,
            ),
        ] {
            let expected = syntax(source, offset, message.into());
            assert_eq!(
                inspect(source, &mut vec![]),
                Err(expected.clone()),
                "{source}"
            );
            assert_eq!(
                before_token_limit(source, 4000),
                eligible.then_some(expected),
                "{source}"
            );
        }
    }

    #[test]
    fn earlier_unclaimed_failure_stops_before_every_new_owned_category() {
        for earlier in [
            "{}",
            "{!}",
            "{items(:1)}",
            "{items(first:{name 1})}",
            "{profile @skip()}",
            "{profile @skip(if true)}",
        ] {
            for later in [
                "%",
                "{items()}",
                "{items(first:])}",
                "{items(first:[)])}",
                "{items(hasRendition:{kind:})}",
                "{items(first:!)}",
                "{items(first:[!])}",
                "{items(hasRendition:{kind:!})}",
                "{items(first:1e+)}",
                "{items(first:1x)}",
                "'",
                r#"{items(search:"\uD800")}"#,
            ] {
                let source = format!("{earlier} {later}");
                let first = first_failure(earlier, None);
                let next = first_failure(&source, None);
                assert_eq!(next.kind, first.kind);
                assert_eq!(next.error, first.error);
                assert_eq!(
                    inspect(&source, &mut vec![]).is_err(),
                    first.owned(),
                    "{source}"
                );
                let source = format!("{source} {}", "profile ".repeat(4001));
                assert_eq!(
                    before_token_limit(&source, 4000),
                    first.before_limit().then_some(first.error),
                    "{source}"
                );
            }
        }
        // Executable bang has separately witnessed ordinary ownership.
        let source = "! {items(first:])}";
        assert_eq!(
            inspect(source, &mut vec![]),
            Err(syntax(source, 0, "Unexpected '!'.".into()))
        );
        assert_eq!(
            before_token_limit(source, 4000),
            Some(syntax(source, 0, "Unexpected '!'.".into()))
        );
    }

    #[test]
    fn required_value_closers_respect_comment_and_token_4001_boundaries() {
        // All 27 exact combinations have independent frozen parse captures.
        for nl in ["\n", "\r", "\r\n"] {
            for total in [3999, 4000, 4001] {
                for (source, tokens, offset, message) in [
                    ("{items(first:])}", 6, 13, "Unexpected ']'."),
                    ("{items(first:[)])}", 7, 14, "Unexpected ')'."),
                    ("{items(hasRendition:{kind:})}", 9, 26, "Unexpected '}'."),
                ] {
                    let prefix = format!("#{nl}").repeat(total - tokens);
                    let source = format!("{prefix}{source}");
                    assert_eq!(
                        before_token_limit(&source, 4000),
                        (total <= 4000).then(|| syntax(
                            &source,
                            prefix.len() + offset,
                            message.into()
                        )),
                        "total={total}, newline={nl:?}"
                    );
                }
            }
        }
        let source = format!("{{{}items(first:])}}", "profile ".repeat(4000));
        assert!(before_token_limit(&source, 4000).is_none());
    }

    #[test]
    fn categorical_diagnostics_use_original_unicode_and_newline_coordinates() {
        for nl in ["\n", "\r", "\r\n"] {
            for (source, column, message) in [
                ("{profile %}", 10, "Unexpected character: '%'."),
                ("{items(hasRendition:{kind:})}", 27, "Unexpected '}'."),
            ] {
                let source = format!("# 雪😀{nl}{source}");
                assert_eq!(
                    inspect(&source, &mut vec![]),
                    Err(Error::Syntax {
                        message: format!("Syntax Error: {message}"),
                        start: Pos { line: 2, column },
                        end: None,
                    })
                );
            }
        }
        let source = "{item(id:\"雪😀\"){id} items(first:])}";
        assert_eq!(
            inspect(source, &mut vec![]),
            Err(Error::Syntax {
                message: "Syntax Error: Unexpected ']'.".into(),
                start: Pos {
                    line: 1,
                    column: 32
                },
                end: None,
            })
        );
    }

    #[test]
    fn grammar_categories_stop_on_token_4001_but_lexing_still_precedes_counting() {
        for nl in ["\n", "\r", "\r\n"] {
            for total in [3999, 4000, 4001] {
                for (source, tokens) in [("{items()}", 4), ("{items(first 1)}", 5), ("!", 1)] {
                    let source = format!("{}{source}", format!("#{nl}").repeat(total - tokens));
                    assert_eq!(before_token_limit(&source, 4000).is_some(), total <= 4000);
                }
                for (prefix, failure) in [
                    ("", "😀"),
                    ("{items(first:", "01)}"),
                    ("{items(search:", r#""\q")}"#),
                ] {
                    let source = format!("{prefix}{}{failure}", format!("#{nl}").repeat(total));
                    assert!(before_token_limit(&source, 4000).is_some(), "{source:?}");
                }
            }
        }
    }

    #[test]
    fn bounded_cursor_counts_exact_tokens_comments_and_single_lookahead() {
        for nl in ["\n", "\r", "\r\n"] {
            let source = format!("# comment{nl}{{item(id:\"# ? )\"){{id}}}} ?");
            let mut lexer = Lexer::new(&source);
            lexer.maximum = Some(11);
            while lexer.next().is_ok() {}
            assert_eq!(lexer.count, 12);
            assert!(source[lexer.cursor..].starts_with(" ?"));
            // Frozen advance lexes before counting: '?' after skipped comments
            // is a lexical failure even when those comments would exceed 4000.
            let source = format!("{}?", format!("#{nl}").repeat(4001));
            assert!(before_token_limit(&source, 4000).is_some());
            let source = format!("{}{{?}}", format!("#{nl}").repeat(4001));
            assert!(before_token_limit(&source, 4000).is_none());
            for total in [3999, 4000, 4001] {
                let source = format!("{}{{items(first:)}}", format!("#{nl}").repeat(total - 6));
                assert_eq!(before_token_limit(&source, 4000).is_some(), total <= 4000);
            }
        }
    }

    #[test]
    fn admitted_tokens_bound_iterative_stacks_before_recursive_parsing() {
        for source in [
            format!("{{items(first:{}?", "[".repeat(4001)),
            format!("{{{}?", "field{".repeat(2001)),
            format!("query($x:{}Int", "[".repeat(4001)),
        ] {
            assert!(before_token_limit(&source, 4000).is_none());
        }
        // A large iterative value walk reaches the scoped boundary without
        // recursive Rust frames; frozen recursion parity is not claimed here.
        let source = format!(
            "{{items(first:{}0{}) items(first:)}}",
            "[".repeat(1500),
            "]".repeat(1500)
        );
        assert!(before_token_limit(&source, 4000).is_some());
    }
}

#[cfg(test)]
mod lexical_ownership_tests {
    use super::*;

    fn first_failure(source: &str, maximum: Option<usize>) -> Failure {
        let mut lexer = Lexer::new(source);
        lexer.maximum = maximum;
        let result = lexer.next().and_then(|current| {
            Recognizer {
                lexer,
                current,
                variable: false,
                moves: vec![],
            }
            .document()
        });
        match result {
            Err(failure) => failure,
            Ok(()) => panic!("expected failure: {source:?}"),
        }
    }

    #[test]
    fn historical_field_tokens_keep_exact_captured_diagnostics() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/parity/graphql-parser-field-value-tokens-v1.json"
        ))
        .unwrap();
        let rows = contract["replacements"].as_array().unwrap();
        assert_eq!(rows.len(), 6);
        for row in rows {
            let case = &row["preimage"];
            let source = case["document"].as_str().unwrap();
            let captured = &case["reference"]["stdout"]["data"]["errors"][0];
            let expected = Error::Syntax {
                message: captured["message"].as_str().unwrap().into(),
                start: Pos {
                    line: captured["locations"][0]["line"].as_u64().unwrap() as usize,
                    column: captured["locations"][0]["column"].as_u64().unwrap() as usize,
                },
                end: None,
            };
            let position = match row["position"].as_str().unwrap() {
                "Root" => ValuePosition::Root,
                "ListElement" => ValuePosition::ListElement,
                "ObjectMember" => ValuePosition::ObjectMember,
                _ => panic!("unknown captured position"),
            };
            let found = match row["token"].as_str().unwrap() {
                "Bang" => Found::Punctuation('!'),
                "Eof" => Found::Eof,
                _ => panic!("unknown captured token"),
            };
            let failure = first_failure(source, None);
            assert_eq!(
                failure.kind,
                FailureKind::Grammar {
                    site: GrammarSite::RequiredValue {
                        context: ValueContext::Argument(ArgumentContext::Field),
                        position,
                    },
                    expected: Expected::Value,
                    found,
                }
            );
            assert!(!failure.owned);
            assert_eq!(failure.error, expected);
            assert!(failure.owned());
            assert_eq!(
                failure.before_limit(),
                row["before_limit_policy"].as_bool().unwrap()
            );
            assert_eq!(inspect(source, &mut vec![]), Err(expected.clone()));
            assert_eq!(
                before_token_limit(source, 4000),
                row["before_limit_policy"]
                    .as_bool()
                    .unwrap()
                    .then_some(expected)
            );
        }
    }

    // These structural controls assert narrow policy/provenance invariants;
    // they are not frozen process captures or newly invented diagnostic text.
    #[test]
    fn field_token_policy_is_typed_and_independent_of_legacy_variable_ownership() {
        for context in [
            ValueContext::Argument(ArgumentContext::Field),
            ValueContext::Argument(ArgumentContext::FieldDirective),
            ValueContext::Argument(ArgumentContext::VariableDefinitionDirective),
            ValueContext::Argument(ArgumentContext::Directive),
            ValueContext::VariableDefault,
        ] {
            for position in [
                ValuePosition::Root,
                ValuePosition::ListElement,
                ValuePosition::ObjectMember,
            ] {
                for found in [Found::Punctuation('!'), Found::Eof, Found::Punctuation('@')] {
                    let field = context == ValueContext::Argument(ArgumentContext::Field);
                    let before = field && found == Found::Punctuation('!');
                    let directive = matches!(
                        context,
                        ValueContext::Argument(
                            ArgumentContext::FieldDirective | ArgumentContext::Directive
                        )
                    );
                    let ordinary = before
                        || field && found == Found::Eof
                        || directive && matches!(found, Found::Punctuation('!') | Found::Eof);
                    for owned in [false, true] {
                        let failure = Failure {
                            error: syntax("", 0, "same unrelated message for all kinds".into()),
                            owned,
                            kind: FailureKind::Grammar {
                                site: GrammarSite::RequiredValue { context, position },
                                expected: Expected::Value,
                                found,
                            },
                        };
                        assert_eq!(failure.owned(), owned || ordinary);
                        assert_eq!(failure.before_limit(), before);
                    }
                }
            }
        }
        for found in [Found::Punctuation('!'), Found::Eof] {
            for site in [GrammarSite::Other, GrammarSite::ExecutableDocument] {
                let failure = Failure {
                    error: syntax("", 0, "same unrelated message for all kinds".into()),
                    owned: false,
                    kind: FailureKind::Grammar {
                        site,
                        expected: Expected::Value,
                        found,
                    },
                };
                assert!(!failure.owned());
                assert!(!failure.before_limit());
            }
        }
    }

    #[test]
    fn field_tokens_preserve_directive_and_variable_default_caller_provenance() {
        for (prefix, context, owned) in [
            (
                "{profile @skip(if:",
                ValueContext::Argument(ArgumentContext::FieldDirective),
                false,
            ),
            (
                "{items(first:[]) @skip(if:",
                ValueContext::Argument(ArgumentContext::FieldDirective),
                false,
            ),
            (
                "query Q @skip(if:",
                ValueContext::Argument(ArgumentContext::Directive),
                false,
            ),
            (
                "fragment F on Query @skip(if:",
                ValueContext::Argument(ArgumentContext::Directive),
                false,
            ),
            (
                "{...F @skip(if:",
                ValueContext::Argument(ArgumentContext::Directive),
                false,
            ),
            (
                "{... on Query @skip(if:",
                ValueContext::Argument(ArgumentContext::Directive),
                false,
            ),
            (
                "query Q($x:Int @skip(if:",
                ValueContext::Argument(ArgumentContext::VariableDefinitionDirective),
                true,
            ),
            ("query Q($x:Int=", ValueContext::VariableDefault, true),
        ] {
            for (nested, position) in [
                ("", ValuePosition::Root),
                ("[", ValuePosition::ListElement),
                ("{kind:", ValuePosition::ObjectMember),
            ] {
                for token in ["!", ""] {
                    let source = format!("{prefix}{nested}{token}");
                    let failure = first_failure(&source, None);
                    assert_eq!(
                        failure.kind,
                        FailureKind::Grammar {
                            site: GrammarSite::RequiredValue { context, position },
                            expected: Expected::Value,
                            found: if token.is_empty() {
                                Found::Eof
                            } else {
                                Found::Punctuation('!')
                            },
                        },
                        "{source}"
                    );
                    assert_eq!(failure.owned, owned);
                    assert!(failure.owned());
                    assert!(!failure.before_limit());
                }
            }
        }
        // A successful variable rewrite must not leave the variable-owner flag
        // set for a later field, nor may it change original-source diagnostics.
        let source = "query Q($x:Int=1 @tag){items(first:!)}";
        let failure = first_failure(source, None);
        assert!(!failure.owned);
        assert!(failure.owned());
        assert!(failure.before_limit());
        assert_eq!(inspect(source, &mut vec![]), Err(failure.error.clone()));
        assert_eq!(before_token_limit(source, 4000), Some(failure.error));
        for source in [
            "{items(first:[]){id}}",
            "{items(hasRendition:{}){id}}",
            "query Q($x:Int!){items(first:$x){id}}",
        ] {
            assert!(inspect(source, &mut vec![]).is_ok());
            assert!(before_token_limit(source, 4000).is_none());
        }
    }

    #[test]
    fn field_tokens_stop_at_first_failure_and_preserve_comment_token_limits() {
        for earlier in [
            "{}",
            "{!}",
            "{items(:1)}",
            "{items(first:{name 1})}",
            "{profile @skip()}",
            "{profile @skip(if:!)}",
        ] {
            for later in ["{items(first:!)}", "{items(first:", "{items(first:["] {
                let source = format!("{earlier} {later}");
                let first = first_failure(earlier, None);
                let next = first_failure(&source, None);
                assert_eq!(next.kind, first.kind);
                assert_eq!(next.error, first.error);
                assert_eq!(
                    inspect(&source, &mut vec![]).is_err(),
                    first.owned(),
                    "{source}"
                );
                assert_eq!(
                    before_token_limit(&source, 4000),
                    first.before_limit().then_some(first.error),
                    "{source}"
                );
            }
        }
        for source in [
            "{items(first:!)}",
            "{items(first:[!])}",
            "{items(hasRendition:{kind:!})}",
        ] {
            let first = first_failure(source, None);
            for later in [
                " %".to_string(),
                " {items(search:\"".into(),
                format!(" {}", "profile ".repeat(4001)),
            ] {
                let source = format!("{source}{later}");
                let next = first_failure(&source, Some(4000));
                assert_eq!(next.kind, first.kind);
                assert_eq!(next.error, first.error);
                assert_eq!(before_token_limit(&source, 4000), Some(first.error.clone()));
            }
        }
        for (prefix, token, tokens, position) in [
            ("{items(first:", "!", 6, ValuePosition::Root),
            ("{items(first:[", "!", 7, ValuePosition::ListElement),
            (
                "{items(hasRendition:{kind:",
                "!",
                9,
                ValuePosition::ObjectMember,
            ),
            ("{items(first:", "", 5, ValuePosition::Root),
            ("{items(first:[", "", 6, ValuePosition::ListElement),
        ] {
            for nl in ["\n", "\r", "\r\n"] {
                for total in [3999, 4000, 4001] {
                    let source = format!(
                        "{prefix}{}{token}",
                        format!("# 雪😀{nl}").repeat(total - tokens)
                    );
                    let failure = first_failure(&source, Some(4000));
                    if total > 4000 {
                        assert_eq!(failure.kind, FailureKind::TokenLimit);
                    } else {
                        assert_eq!(
                            failure.kind,
                            FailureKind::Grammar {
                                site: GrammarSite::RequiredValue {
                                    context: ValueContext::Argument(ArgumentContext::Field),
                                    position,
                                },
                                expected: Expected::Value,
                                found: if token.is_empty() {
                                    Found::Eof
                                } else {
                                    Found::Punctuation('!')
                                },
                            }
                        );
                        assert!(failure.owned());
                    }
                    assert_eq!(failure.before_limit(), total <= 4000 && !token.is_empty());
                }
            }
            let source = format!("{{{}{prefix}{token}", "profile ".repeat(4000));
            assert_eq!(
                first_failure(&source, Some(4000)).kind,
                FailureKind::TokenLimit
            );
            assert!(before_token_limit(&source, 4000).is_none());
        }
        let captured_object_eof = first_failure("{items(hasRendition:{kind:", None);
        assert!(captured_object_eof.owned());
        assert!(!captured_object_eof.before_limit());
    }

    // These are typed source controls, not newly captured frozen diagnostics.
    // Full-process/text qualification for the added branches remains pending.
    #[test]
    fn lexical_reason_branch_controls_do_not_classify_by_text() {
        for (sources, reason) in [
            (
                &[
                    "-", "-)", "-é", "1.", "1.)", "1.é", "1e", "1E", "1e+", "1e-", "1E+", "1E-",
                    "1e)", "1E+)", "1eé", "1e+é",
                ][..],
                NumberReason::MissingDigit,
            ),
            (
                &["1x", "1_", "1.2.3"][..],
                NumberReason::InvalidContinuation,
            ),
        ] {
            for source in sources {
                let Err(mut failure) = Lexer::new(source).next() else {
                    panic!("expected number failure: {source:?}");
                };
                assert_eq!(failure.kind, FailureKind::InvalidNumber(reason));
                assert!(!failure.owned);
                failure.error = syntax(source, 0, "unrelated diagnostic text".into());
                assert!(failure.before_limit());
                assert!(failure.owned());
            }
        }
        for source in [
            r#""\uZZZZ""#,
            r#""\u12""#,
            r#""\uD800""#,
            r#""\uDC00""#,
            r#""\uD800\u0041""#,
            r#""\uD800\uD800""#,
            r#""\u{}""#,
            r#""\u{Q}""#,
            r#""\u{110000}""#,
            r#""\u{D800}""#,
            r#""\u{123""#,
        ] {
            let Err(mut failure) = Lexer::new(source).next() else {
                panic!("expected Unicode escape failure: {source:?}");
            };
            assert_eq!(
                failure.kind,
                FailureKind::String(StringReason::InvalidUnicodeEscape)
            );
            assert!(failure.owned);
            failure.error = syntax(source, 0, "unrelated diagnostic text".into());
            assert!(failure.owned());
            failure.owned = false;
            assert!(failure.before_limit());
            // Ordinary Unicode ownership remains the existing lexer flag.
            assert!(!failure.owned());
        }
        for prefix in [
            "",
            "\u{feff}# comment\n",
            "profile ",
            "profile # comment\r\n",
        ] {
            let source = format!("{prefix}'");
            let mut lexer = Lexer::new(&source);
            let initial = !prefix.starts_with("profile");
            if !initial {
                assert!(lexer.next().is_ok());
            }
            let Err(failure) = lexer.next() else {
                panic!("expected single quote: {source:?}");
            };
            assert_eq!(
                failure.kind,
                FailureKind::UnexpectedCharacter {
                    reason: CharacterReason::SingleQuote,
                    code_point: '\'',
                    initial,
                }
            );
            assert!(failure.before_limit());
            assert!(failure.owned());
        }
    }

    #[test]
    fn lexical_reason_neighbors_keep_their_existing_discriminants() {
        for source in ["0", "-0", "1.2", "-0.1", "1e2", "1E+2", "1e-2"] {
            let Ok(token) = Lexer::new(source).next() else {
                panic!("valid number")
            };
            assert!(token.kind == Kind::Number);
            assert_eq!(token.text, source);
        }
        let mut lexer = Lexer::new("1é");
        assert!(lexer.next().is_ok());
        let Err(failure) = lexer.next() else {
            panic!("unsupported scalar")
        };
        assert_eq!(
            failure.kind,
            FailureKind::UnexpectedCharacter {
                reason: CharacterReason::UnsupportedScalar,
                code_point: 'é',
                initial: false,
            }
        );
        for source in [
            r#""\uD83D\uDE00""#,
            r#""\u{1F600}""#,
            r#""\\uD800'""#,
            r#""apostrophe '""#,
            r#""""\uD800 '""""#,
            "# \\uD800 '雪😀\nprofile",
        ] {
            assert!(Lexer::new(source).next().is_ok(), "{source:?}");
        }
        for (kind, ordinary, before) in [
            (
                FailureKind::InvalidNumber(NumberReason::LeadingZero),
                true,
                true,
            ),
            (FailureKind::String(StringReason::InvalidEscape), true, true),
            (FailureKind::String(StringReason::Unterminated), true, false),
            (
                FailureKind::String(StringReason::DanglingBackslash),
                false,
                false,
            ),
            (FailureKind::TokenLimit, false, false),
        ] {
            let failure = Failure {
                error: syntax("", 0, "identical message for unrelated kinds".into()),
                owned: matches!(
                    kind,
                    FailureKind::String(StringReason::InvalidEscape | StringReason::Unterminated)
                ),
                kind,
            };
            assert_eq!(failure.owned(), ordinary);
            assert_eq!(failure.before_limit(), before);
        }
    }

    #[test]
    fn lexical_first_failure_and_single_advance_token_boundaries_are_preserved() {
        for (prefix, token, kind) in [
            (
                "{items(first:",
                "1e+)",
                FailureKind::InvalidNumber(NumberReason::MissingDigit),
            ),
            (
                "{items(first:",
                "1x)",
                FailureKind::InvalidNumber(NumberReason::InvalidContinuation),
            ),
            (
                "{items(search:",
                "')",
                FailureKind::UnexpectedCharacter {
                    reason: CharacterReason::SingleQuote,
                    code_point: '\'',
                    initial: false,
                },
            ),
            (
                "{items(search:",
                r#""\uD800")"#,
                FailureKind::String(StringReason::InvalidUnicodeEscape),
            ),
        ] {
            let short = format!("{prefix}{token}}}");
            let first = first_failure(&short, None);
            assert_eq!(first.kind, kind);
            for tail in [
                " {items(search:\"".to_string(),
                " %".into(),
                format!(" {}", "profile ".repeat(4001)),
            ] {
                let source = format!("{short}{tail}");
                let failure = first_failure(&source, Some(4000));
                assert_eq!(failure.kind, kind);
                assert_eq!(failure.error, first.error);
                assert_eq!(inspect(&source, &mut vec![]), Err(first.error.clone()));
                assert_eq!(before_token_limit(&source, 4000), Some(first.error.clone()));
            }
            for nl in ["\n", "\r", "\r\n"] {
                for total in [3999, 4000, 4001] {
                    // Five successful prefix tokens; comments and failing token
                    // are consumed in the same advance. Lexing precedes count enforcement.
                    let source = format!(
                        "{prefix}{}{token}}}",
                        format!("# 雪😀{nl}").repeat(total - 6)
                    );
                    let failure = first_failure(&source, Some(4000));
                    assert_eq!(failure.kind, kind, "total={total} nl={nl:?}");
                    assert!(failure.before_limit());
                }
            }
            let source = format!("{{{}{prefix}{token}}}", "profile ".repeat(4000));
            assert_eq!(
                first_failure(&source, Some(4000)).kind,
                FailureKind::TokenLimit
            );
            assert!(before_token_limit(&source, 4000).is_none());
        }
    }

    #[test]
    fn historical_lexical_preimages_keep_captured_messages_and_original_locations() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/parity/graphql-parser-lexical-ownership-v1.json"
        ))
        .unwrap();
        let replacements = contract["replacements"].as_array().unwrap();
        assert_eq!(replacements.len(), 13);
        for replacement in replacements {
            let case = &replacement["preimage"];
            let source = case["document"].as_str().unwrap();
            let expected = &case["reference"]["stdout"]["data"]["errors"][0];
            let error = Error::Syntax {
                message: expected["message"].as_str().unwrap().into(),
                start: Pos {
                    line: expected["locations"][0]["line"].as_u64().unwrap() as usize,
                    column: expected["locations"][0]["column"].as_u64().unwrap() as usize,
                },
                end: None,
            };
            assert_eq!(
                inspect(source, &mut vec![]),
                Err(error.clone()),
                "{}",
                case["name"]
            );
            assert_eq!(
                before_token_limit(source, 4000),
                Some(error),
                "{}",
                case["name"]
            );
        }
    }
}

#[cfg(test)]
mod directive_value_closer_tests {
    use super::*;

    fn first_failure(source: &str, maximum: Option<usize>) -> Failure {
        let mut lexer = Lexer::new(source);
        lexer.maximum = maximum;
        let result = lexer.next().and_then(|current| {
            Recognizer {
                lexer,
                current,
                variable: false,
                moves: vec![],
            }
            .document()
        });
        match result {
            Err(failure) => failure,
            Ok(()) => panic!("expected failure: {source:?}"),
        }
    }

    #[test]
    fn seven_historical_field_directive_closers_keep_captured_diagnostics() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/parity/graphql-parser-directive-value-closers-v1.json"
        ))
        .unwrap();
        let rows = contract["replacements"].as_array().unwrap();
        assert_eq!(rows.len(), 7);
        for row in rows {
            let case = &row["preimage"];
            let source = case["document"].as_str().unwrap();
            let captured = &case["reference"]["stdout"]["data"]["errors"][0];
            let expected = Error::Syntax {
                message: captured["message"].as_str().unwrap().into(),
                start: Pos {
                    line: captured["locations"][0]["line"].as_u64().unwrap() as usize,
                    column: captured["locations"][0]["column"].as_u64().unwrap() as usize,
                },
                end: None,
            };
            let position = match row["position"].as_str().unwrap() {
                "Root" => ValuePosition::Root,
                "ListElement" => ValuePosition::ListElement,
                "ObjectMember" => ValuePosition::ObjectMember,
                _ => panic!("unknown captured position"),
            };
            let closer = row["closer"].as_str().unwrap().chars().next().unwrap();
            let failure = first_failure(source, None);
            assert_eq!(
                failure.kind,
                FailureKind::Grammar {
                    site: GrammarSite::RequiredValue {
                        context: ValueContext::Argument(ArgumentContext::FieldDirective),
                        position,
                    },
                    expected: Expected::Value,
                    found: Found::Punctuation(closer),
                }
            );
            assert!(!failure.owned);
            assert!(failure.owned());
            assert!(failure.before_limit());
            assert_eq!(failure.error, expected);
            assert_eq!(inspect(source, &mut vec![]), Err(expected.clone()));
            assert_eq!(before_token_limit(source, 4000), Some(expected));
        }
    }

    // Structural source controls below are not frozen captures or process results.
    #[test]
    fn directive_closer_policy_requires_exact_context_position_token_and_expectation() {
        for context in [
            ValueContext::Argument(ArgumentContext::Field),
            ValueContext::Argument(ArgumentContext::FieldDirective),
            ValueContext::Argument(ArgumentContext::VariableDefinitionDirective),
            ValueContext::Argument(ArgumentContext::Directive),
            ValueContext::VariableDefault,
        ] {
            for position in [
                ValuePosition::Root,
                ValuePosition::ListElement,
                ValuePosition::ObjectMember,
            ] {
                for found in [
                    Found::Punctuation(')'),
                    Found::Punctuation(']'),
                    Found::Punctuation('}'),
                    Found::Punctuation('!'),
                    Found::Punctuation('@'),
                    Found::Eof,
                    Found::Spread,
                ] {
                    for expected in [
                        Expected::Value,
                        Expected::Name,
                        Expected::Literal(":"),
                        Expected::Unexpected,
                    ] {
                        let field = context == ValueContext::Argument(ArgumentContext::Field);
                        let exact_directive = context
                            == ValueContext::Argument(ArgumentContext::FieldDirective)
                            && matches!(
                                (position, found),
                                (ValuePosition::Root, Found::Punctuation(']'))
                                    | (ValuePosition::ListElement, Found::Punctuation(')'))
                                    | (ValuePosition::ObjectMember, Found::Punctuation('}'))
                            );
                        let before = expected == Expected::Value
                            && (exact_directive
                                || field
                                    && matches!(found, Found::Punctuation(')' | ']' | '}' | '!')));
                        let directive = matches!(
                            context,
                            ValueContext::Argument(
                                ArgumentContext::FieldDirective | ArgumentContext::Directive
                            )
                        );
                        let captured_directive = directive
                            && matches!(
                                (position, found),
                                (_, Found::Punctuation('!') | Found::Eof)
                                    | (ValuePosition::Root, Found::Punctuation(']'))
                                    | (ValuePosition::ListElement, Found::Punctuation(')'))
                                    | (ValuePosition::ObjectMember, Found::Punctuation('}'))
                            )
                            || context == ValueContext::Argument(ArgumentContext::FieldDirective)
                                && matches!(
                                    (position, found),
                                    (
                                        ValuePosition::Root | ValuePosition::ListElement,
                                        Found::Punctuation('}')
                                    ) | (
                                        ValuePosition::ObjectMember,
                                        Found::Punctuation(')' | ']')
                                    )
                                );
                        let ordinary = before
                            || expected == Expected::Value
                                && (field && found == Found::Eof
                                    || captured_directive
                                    || matches!(
                                        context,
                                        ValueContext::Argument(
                                            ArgumentContext::Field
                                                | ArgumentContext::FieldDirective
                                        )
                                    ) && position == ValuePosition::Root
                                        && found == Found::Spread);
                        for owned in [false, true] {
                            let failure = Failure {
                                error: syntax("", 0, "unrelated sentinel text".into()),
                                owned,
                                kind: FailureKind::Grammar {
                                    site: GrammarSite::RequiredValue { context, position },
                                    expected,
                                    found,
                                },
                            };
                            assert_eq!(failure.owned(), owned || ordinary);
                            assert_eq!(failure.before_limit(), before);
                        }
                    }
                }
            }
        }
        for site in [GrammarSite::Other, GrammarSite::ExecutableDocument] {
            for found in [
                Found::Punctuation(')'),
                Found::Punctuation(']'),
                Found::Punctuation('}'),
            ] {
                let failure = Failure {
                    error: syntax("", 0, "unrelated sentinel text".into()),
                    owned: false,
                    kind: FailureKind::Grammar {
                        site,
                        expected: Expected::Value,
                        found,
                    },
                };
                assert!(!failure.owned());
                assert!(!failure.before_limit());
            }
        }
    }

    #[test]
    fn field_directive_provenance_does_not_spread_to_other_directive_parents() {
        for (prefix, context, legacy_owned) in [
            (
                "{profile @skip(if:",
                ValueContext::Argument(ArgumentContext::FieldDirective),
                false,
            ),
            (
                "{p:profile @skip(if:",
                ValueContext::Argument(ArgumentContext::FieldDirective),
                false,
            ),
            (
                "{items(first:[]) @skip(if:",
                ValueContext::Argument(ArgumentContext::FieldDirective),
                false,
            ),
            (
                "query Q($x:Int=1 @tag){profile @skip(if:",
                ValueContext::Argument(ArgumentContext::FieldDirective),
                false,
            ),
            (
                "query Q @skip(if:",
                ValueContext::Argument(ArgumentContext::Directive),
                false,
            ),
            (
                "fragment F on Query @skip(if:",
                ValueContext::Argument(ArgumentContext::Directive),
                false,
            ),
            (
                "{...F @skip(if:",
                ValueContext::Argument(ArgumentContext::Directive),
                false,
            ),
            (
                "{... on Query @skip(if:",
                ValueContext::Argument(ArgumentContext::Directive),
                false,
            ),
            (
                "{... @skip(if:",
                ValueContext::Argument(ArgumentContext::Directive),
                false,
            ),
            (
                "query Q($x:Int @skip(if:",
                ValueContext::Argument(ArgumentContext::VariableDefinitionDirective),
                true,
            ),
            ("query Q($x:Int=", ValueContext::VariableDefault, true),
        ] {
            for (nested, position, closer) in [
                ("", ValuePosition::Root, ']'),
                ("[", ValuePosition::ListElement, ')'),
                ("{kind:", ValuePosition::ObjectMember, '}'),
            ] {
                let source = format!("{prefix}{nested}{closer}");
                let failure = first_failure(&source, None);
                assert_eq!(
                    failure.kind,
                    FailureKind::Grammar {
                        site: GrammarSite::RequiredValue { context, position },
                        expected: Expected::Value,
                        found: Found::Punctuation(closer),
                    },
                    "{source}"
                );
                let promoted = context == ValueContext::Argument(ArgumentContext::FieldDirective);
                assert_eq!(failure.owned, legacy_owned);
                let ordinary =
                    promoted || context == ValueContext::Argument(ArgumentContext::Directive);
                assert_eq!(failure.owned(), legacy_owned || ordinary);
                assert_eq!(failure.before_limit(), promoted);
                assert_eq!(
                    before_token_limit(&source, 4000),
                    promoted.then_some(failure.error)
                );
            }
        }
        // The immediate-colon/) exception stays separate for every directive parent.
        for prefix in [
            "{profile @skip(if:",
            "query Q @skip(if:",
            "fragment F on Query @skip(if:",
            "{...F @skip(if:",
            "{... on Query @skip(if:",
            "{... @skip(if:",
            "query Q($x:Int @skip(if:",
        ] {
            let source = format!("{prefix})");
            let failure = first_failure(&source, None);
            assert_eq!(
                failure.kind,
                FailureKind::Grammar {
                    site: GrammarSite::ArgumentValue,
                    expected: Expected::Value,
                    found: Found::Punctuation(')')
                }
            );
            assert!(failure.owned);
            assert!(failure.owned());
            assert!(failure.before_limit());
        }
        for source in [
            "{profile @skip(if:[])}",
            "{profile @skip(if:{})}",
            "query Q($x:Boolean){profile @skip(if:$x)}",
            "query Q($x:Boolean @tag(if:true)){profile}",
            "{profile @tag(text:\"] ) }\")}",
            "{profile @tag(text:\"\"\"] ) }\"\"\")}",
        ] {
            assert!(inspect(source, &mut vec![]).is_ok(), "{source}");
            assert!(before_token_limit(source, 4000).is_none(), "{source}");
        }
    }

    #[test]
    fn directive_closers_stop_at_first_failure_without_widening_other_pairs() {
        for earlier in [
            "{}",
            "{!}",
            "{items(:1)}",
            "{profile @skip()}",
            "{profile @skip(if true)}",
            "{profile @skip(if:})}",
            "{profile @skip(if:[}])}",
            "{profile @skip(if:{kind:]})}",
            "query Q @skip(if:]){profile}",
            "{profile @skip(if:!)}",
        ] {
            for later in [
                "{profile @skip(if:])}",
                "{profile @skip(if:[)])}",
                "{profile @skip(if:{kind:})}",
            ] {
                let source = format!("{earlier} {later}");
                let first = first_failure(earlier, None);
                let next = first_failure(&source, None);
                assert_eq!(next.kind, first.kind);
                assert_eq!(next.error, first.error);
                assert_eq!(
                    inspect(&source, &mut vec![]).is_err(),
                    first.owned(),
                    "{source}"
                );
                assert_eq!(
                    before_token_limit(&source, 4000),
                    first.before_limit().then_some(first.error),
                    "{source}"
                );
            }
        }
        for source in [
            "{profile @skip(if:])}",
            "{profile @skip(if:[)])}",
            "{profile @skip(if:{kind:})}",
        ] {
            let first = first_failure(source, None);
            for tail in [
                " %".to_string(),
                " {items(search:\"".into(),
                format!(" {}", "profile ".repeat(4001)),
            ] {
                let longer = format!("{source}{tail}");
                let failure = first_failure(&longer, Some(4000));
                assert_eq!(failure.kind, first.kind);
                assert_eq!(failure.error, first.error);
                assert_eq!(inspect(&longer, &mut vec![]), Err(first.error.clone()));
                assert_eq!(before_token_limit(&longer, 4000), Some(first.error.clone()));
            }
        }
    }

    #[test]
    fn directive_closers_keep_original_positions_and_comment_token_boundaries() {
        for (prefix, closer, tokens, position) in [
            ("{profile @skip(if:", ']', 8, ValuePosition::Root),
            ("{profile @skip(if:[", ')', 9, ValuePosition::ListElement),
            (
                "{profile @skip(if:{kind:",
                '}',
                11,
                ValuePosition::ObjectMember,
            ),
        ] {
            for nl in ["\n", "\r", "\r\n"] {
                for total in [3999, 4000, 4001] {
                    let source = format!(
                        "{prefix}{}{closer}",
                        format!("# 雪😀{nl}").repeat(total - tokens)
                    );
                    let failure = first_failure(&source, Some(4000));
                    if total > 4000 {
                        assert_eq!(failure.kind, FailureKind::TokenLimit);
                        assert!(!failure.owned());
                    } else {
                        assert_eq!(
                            failure.kind,
                            FailureKind::Grammar {
                                site: GrammarSite::RequiredValue {
                                    context: ValueContext::Argument(
                                        ArgumentContext::FieldDirective
                                    ),
                                    position
                                },
                                expected: Expected::Value,
                                found: Found::Punctuation(closer)
                            }
                        );
                        assert!(failure.owned());
                        assert_eq!(inspect(&source, &mut vec![]), Err(failure.error.clone()));
                    }
                    assert_eq!(failure.before_limit(), total <= 4000);
                }
            }
            let source = format!("{{{}{prefix}{closer}", "profile ".repeat(4000));
            assert_eq!(
                first_failure(&source, Some(4000)).kind,
                FailureKind::TokenLimit
            );
            assert!(before_token_limit(&source, 4000).is_none());
        }
        for nl in ["\n", "\r", "\r\n"] {
            let source =
                format!("query Q($x:Int=1 @tag){{profile @tag(text:\"雪😀\"){nl} @skip(if:])}}");
            let failure = first_failure(&source, None);
            assert!(!failure.owned);
            assert_eq!(inspect(&source, &mut vec![]), Err(failure.error.clone()));
            assert_eq!(before_token_limit(&source, 4000), Some(failure.error));
        }
    }
}

#[cfg(test)]
mod lexical_capture_tests;

#[cfg(test)]
mod grammar_provenance_tests;

#[cfg(test)]
mod executable_punctuation_tests {
    use super::*;

    fn first_failure(source: &str, maximum: Option<usize>) -> Failure {
        let mut lexer = Lexer::new(source);
        lexer.maximum = maximum;
        let result = lexer.next().and_then(|current| {
            Recognizer {
                lexer,
                current,
                variable: false,
                moves: vec![],
            }
            .document()
        });
        match result {
            Err(failure) => failure,
            Ok(()) => panic!("expected failure: {source:?}"),
        }
    }

    #[test]
    fn three_historical_executable_punctuation_rows_keep_complete_capture_diagnostics() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/parity/graphql-parser-executable-punctuation-v1.json"
        ))
        .unwrap();
        let rows = contract["replacements"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        for row in rows {
            let source = row["preimage"]["document"].as_str().unwrap();
            let captured = &row["preimage"]["reference"]["stdout"]["data"]["errors"][0];
            let expected = Error::Syntax {
                message: captured["message"].as_str().unwrap().into(),
                start: Pos {
                    line: captured["locations"][0]["line"].as_u64().unwrap() as usize,
                    column: captured["locations"][0]["column"].as_u64().unwrap() as usize,
                },
                end: None,
            };
            let spread = row["found"] == "Spread";
            let failure = first_failure(source, None);
            assert_eq!(
                failure.kind,
                FailureKind::Grammar {
                    site: GrammarSite::ExecutableDocument,
                    expected: Expected::Unexpected,
                    found: if spread {
                        Found::Spread
                    } else {
                        Found::Punctuation('!')
                    },
                }
            );
            assert!(!failure.owned);
            assert!(failure.owned());
            assert!(failure.before_limit());
            assert_eq!(failure.error, expected);
            assert_eq!(inspect(source, &mut vec![]), Err(expected.clone()));
            assert_eq!(before_token_limit(source, 4000), Some(expected));
        }
        let historical: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/parity/graphql-parser-required-value-precedence-v1.json"
        ))
        .unwrap();
        let case = &historical["cases"][39];
        assert_eq!(case["name"], "overbudget-earlier-bang-before-bracket");
        assert_eq!(case["expectation"], "baseline");
        assert_eq!(case["reference"], case["baseline"]["legacy"]);
        let source = case["document"].as_str().unwrap();
        let failure = first_failure(source, Some(4000));
        assert!(failure.owned());
        assert!(failure.before_limit());
        assert_eq!(before_token_limit(source, 4000), Some(failure.error));
    }

    // These are typed source controls, not frozen captures or runtime results.
    #[test]
    fn executable_policy_requires_exact_site_expectation_token_and_independent_lane() {
        for site in [GrammarSite::Other, GrammarSite::ExecutableDocument] {
            for expected in [
                Expected::Unexpected,
                Expected::Value,
                Expected::Name,
                Expected::Literal(":"),
                Expected::Other,
            ] {
                for found in [
                    Found::Spread,
                    Found::Punctuation('!'),
                    Found::Punctuation('.'),
                    Found::Punctuation('@'),
                    Found::Punctuation('$'),
                    Found::Punctuation(')'),
                    Found::Punctuation(']'),
                    Found::Punctuation('}'),
                    Found::Name,
                    Found::Number,
                    Found::String,
                    Found::Eof,
                ] {
                    let exact =
                        site == GrammarSite::ExecutableDocument && expected == Expected::Unexpected;
                    for owned in [false, true] {
                        let failure = Failure {
                            error: syntax("", 0, "same unrelated sentinel text".into()),
                            owned,
                            kind: FailureKind::Grammar {
                                site,
                                expected,
                                found,
                            },
                        };
                        assert_eq!(
                            failure.owned(),
                            owned
                                || exact
                                    && matches!(
                                        found,
                                        Found::Spread
                                            | Found::Punctuation(
                                                '!' | '$'
                                                    | '&'
                                                    | '('
                                                    | ')'
                                                    | ':'
                                                    | '='
                                                    | '@'
                                                    | '['
                                                    | ']'
                                                    | '}'
                                                    | '|'
                                            )
                                    )
                        );
                        assert_eq!(
                            failure.before_limit(),
                            exact && matches!(found, Found::Punctuation('!') | Found::Spread)
                        );
                    }
                }
            }
        }
        for site in [
            GrammarSite::ArgumentStart,
            GrammarSite::ArgumentColon,
            GrammarSite::ArgumentValue,
            GrammarSite::RequiredValue {
                context: ValueContext::Argument(ArgumentContext::Field),
                position: ValuePosition::Root,
            },
            GrammarSite::RequiredValue {
                context: ValueContext::Argument(ArgumentContext::FieldDirective),
                position: ValuePosition::ListElement,
            },
            GrammarSite::RequiredValue {
                context: ValueContext::Argument(ArgumentContext::Directive),
                position: ValuePosition::ObjectMember,
            },
            GrammarSite::RequiredValue {
                context: ValueContext::VariableDefault,
                position: ValuePosition::Root,
            },
        ] {
            let failure = Failure {
                error: syntax("", 0, "same sentinel".into()),
                owned: false,
                kind: FailureKind::Grammar {
                    site,
                    expected: Expected::Unexpected,
                    found: Found::Spread,
                },
            };
            assert!(!failure.owned());
            assert!(!failure.before_limit());
        }
    }

    #[test]
    fn spread_discriminator_preserves_all_other_punctuation_and_one_two_dot_lexing() {
        for text in [
            "!", "$", "&", "(", ")", ":", "=", "@", "[", "]", "{", "}", "|",
        ] {
            let lexeme = Lexer::new(text)
                .next()
                .unwrap_or_else(|_| panic!("punctuation {text}"));
            assert_eq!(
                Found::from(lexeme),
                Found::Punctuation(text.chars().next().unwrap())
            );
        }
        let spread = Lexer::new("...")
            .next()
            .unwrap_or_else(|_| panic!("spread"));
        assert_eq!(spread.text, "...");
        assert_eq!(Found::from(spread), Found::Spread);
        for text in [".", ".."] {
            // A discriminator conversion cannot make a one/two-dot lexeme spread.
            assert_eq!(
                Found::from(Lexeme {
                    text,
                    start: 0,
                    kind: Kind::Punct
                }),
                Found::Punctuation('.')
            );
            for prefix in ["", "{profile} ", "\u{feff}# 雪😀\r\n"] {
                let source = format!("{prefix}{text}");
                let failure = first_failure(&source, None);
                assert!(matches!(
                    failure.kind,
                    FailureKind::UnexpectedCharacter {
                        reason: CharacterReason::UnsupportedScalar,
                        code_point: '.',
                        ..
                    }
                ));
                assert!(failure.owned());
                assert!(failure.before_limit());
            }
        }
        for source in [
            "{...F}",
            "{... on Query {profile}}",
            "{... @tag {profile}}",
            "{profile @tag(text:\"... !\")}",
            "{profile @tag(text:\"\"\"... !\"\"\")}",
            "# ... !\n{profile}",
        ] {
            assert!(inspect(source, &mut vec![]).is_ok(), "{source}");
            assert!(before_token_limit(source, 4000).is_none(), "{source}");
        }
    }

    #[test]
    fn executable_punctuation_stops_at_first_failure_without_broadening_other_sites() {
        for (earlier, site, expected, found) in [
            (
                "{}",
                GrammarSite::SelectionName(SelectionName::Field),
                Expected::Name,
                Found::Punctuation('}'),
            ),
            (
                "{!}",
                GrammarSite::SelectionName(SelectionName::Field),
                Expected::Name,
                Found::Punctuation('!'),
            ),
            (
                "{items(:1)}",
                GrammarSite::ArgumentName {
                    context: ArgumentContext::Field,
                    first: true,
                },
                Expected::Name,
                Found::Punctuation(':'),
            ),
            (
                "{items(first:{name 1})}",
                GrammarSite::ObjectMemberColon(ValueContext::Argument(ArgumentContext::Field)),
                Expected::Literal(":"),
                Found::Number,
            ),
            (
                "{profile @skip()}",
                GrammarSite::ArgumentName {
                    context: ArgumentContext::FieldDirective,
                    first: true,
                },
                Expected::Name,
                Found::Punctuation(')'),
            ),
            (
                "{profile @skip(if true)}",
                GrammarSite::DirectiveArgumentColon(ArgumentContext::FieldDirective),
                Expected::Literal(":"),
                Found::Name,
            ),
        ] {
            let first = first_failure(earlier, None);
            assert_eq!(
                first.kind,
                FailureKind::Grammar {
                    site,
                    expected,
                    found
                }
            );
            let combined: serde_json::Value = serde_json::from_str(include_str!(
                "../../../../tests/parity/graphql-parser-final-composition-v1.json"
            ))
            .unwrap();
            let witness = combined["test_reconciliations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["prefix"] == earlier)
                .unwrap();
            let diagnostic = &witness["diagnostic"];
            assert_eq!(
                first.error,
                Error::Syntax {
                    message: diagnostic["message"].as_str().unwrap().into(),
                    start: Pos {
                        line: diagnostic["locations"][0]["line"].as_u64().unwrap() as usize,
                        column: diagnostic["locations"][0]["column"].as_u64().unwrap() as usize,
                    },
                    end: None,
                }
            );
            assert!(first.owned());
            assert!(first.before_limit());
            for later in ["!", "...", "! {items(search:\"", "... {items(first:])}"] {
                let source = format!("{earlier} {later} {}", "profile ".repeat(4001));
                let actual = first_failure(&source, None);
                assert_eq!(actual.kind, first.kind);
                assert_eq!(actual.error, first.error);
                assert_eq!(inspect(&source, &mut vec![]), Err(first.error.clone()));
                assert_eq!(before_token_limit(&source, 4000), Some(first.error.clone()));
            }
        }
        for token in ["!", "..."] {
            let first = first_failure(token, None);
            for tail in [
                " {items(search:\"",
                " {items(first:])}",
                " {items(first:1e+)}",
                " '",
                " %",
            ] {
                let source = format!("{token}{tail}");
                let actual = first_failure(&source, None);
                assert_eq!(actual.kind, first.kind);
                assert_eq!(actual.error, first.error);
                assert!(inspect(&source, &mut vec![]).is_err());
                assert_eq!(before_token_limit(&source, 4000), Some(first.error.clone()));
            }
        }
        for source in [
            "{!}",
            "{items(first:...)}",
            "{profile @skip(if:!)}",
            "{profile @skip(if:...)}",
        ] {
            let failure = first_failure(source, None);
            if source == "{!}" {
                assert_eq!(
                    failure.kind,
                    FailureKind::Grammar {
                        site: GrammarSite::SelectionName(SelectionName::Field),
                        expected: Expected::Name,
                        found: Found::Punctuation('!'),
                    }
                );
            }
            assert!(failure.owned());
            assert_eq!(failure.before_limit(), source == "{!}");
        }
    }

    #[test]
    fn executable_punctuation_preserves_original_locations_and_comment_token_boundaries() {
        for token in ["!", "..."] {
            for nl in ["\n", "\r", "\r\n"] {
                for total in [3999, 4000, 4001] {
                    let source = format!("{} {token}", format!("# 雪😀{nl}").repeat(total - 1));
                    let ordinary = first_failure(&source, None);
                    assert!(
                        matches!(ordinary.error, Error::Syntax { start: Pos { line, column: 2 }, .. } if line == total)
                    );
                    let budgeted = first_failure(&source, Some(4000));
                    if total <= 4000 {
                        assert_eq!(budgeted.kind, ordinary.kind);
                        assert_eq!(budgeted.error, ordinary.error);
                        assert_eq!(before_token_limit(&source, 4000), Some(ordinary.error));
                    } else {
                        assert!(matches!(budgeted.kind, FailureKind::TokenLimit));
                        assert!(before_token_limit(&source, 4000).is_none());
                    }
                }
                let prefix = format!("query Q($x:Int=1 @tag){{profile @tag(text:\"雪😀\")}}{nl}  ");
                let source = format!("{prefix}{token}");
                let failure = first_failure(&source, None);
                assert!(!failure.owned);
                assert!(matches!(
                    failure.error,
                    Error::Syntax {
                        start: Pos { line: 2, column: 3 },
                        ..
                    }
                ));
                assert!(failure.owned());
                assert_eq!(before_token_limit(&source, 4000), Some(failure.error));
            }
            let source = format!("{{{}}} {token}", "profile ".repeat(4000));
            assert!(matches!(
                first_failure(&source, Some(4000)).kind,
                FailureKind::TokenLimit
            ));
            assert!(before_token_limit(&source, 4000).is_none());
        }
    }
}
