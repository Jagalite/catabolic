// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Original-source grammar boundaries needed by the executable parser adapter.
//!
//! This is a lazy recognizer, not a second AST or execution engine. Walking the
//! surrounding grammar prevents a later constant-value/Unicode error from hiding
//! an earlier syntax error. Only variable-definition grammar and Unicode escape
//! diagnostics are owned here; unrelated diagnostics stay with the framework.
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

struct Failure {
    error: Error,
    owned: bool,
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
}
impl<'a> Lexer<'a> {
    fn fail<T>(&self, offset: usize, message: String, unicode: bool) -> Result<T> {
        Err(Failure {
            error: syntax(self.source, offset, message),
            owned: unicode,
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
                        return self.fail(i, "Unterminated string.".into(), false);
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
                                false,
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
                let message = if bytes[i] == b'\'' {
                    "Unexpected single quote character ('), did you mean to use a double quote (\")?".into()
                } else {
                    format!("Unexpected character: {}.", point(self.source, i))
                };
                return self.fail(i, message, false);
            }
        };
        self.cursor = i;
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
        Err(Failure {
            error: syntax(self.lexer.source, self.current.start, message),
            owned: self.variable,
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
    fn expect(&mut self, text: &str) -> Result<()> {
        if self.eat(text)? {
            Ok(())
        } else {
            self.failure(format!("Expected '{text}', found {}.", self.found()))
        }
    }
    fn name(&mut self) -> Result<()> {
        if self.current.kind == Kind::Name {
            self.advance()
        } else {
            self.failure(format!("Expected Name, found {}.", self.found()))
        }
    }
    fn unexpected<T>(&self) -> Result<T> {
        self.failure(format!("Unexpected {}.", self.found()))
    }
    fn value(&mut self, constant: bool) -> Result<()> {
        // Input nesting does not consume Rust call frames. Deferring a deep
        // constant value to the framework would be unsafe for grammar admission:
        // its variable-definition directives accept ordinary variable values.
        // The existing original-source token/byte limits bound this task stack.
        enum Task {
            Value,
            List,
            Object,
        }
        let mut tasks = vec![Task::Value];
        while let Some(task) = tasks.pop() {
            match task {
                Task::List => {
                    if !self.eat("]")? {
                        tasks.push(Task::List);
                        tasks.push(Task::Value);
                    }
                }
                Task::Object => {
                    if !self.eat("}")? {
                        self.name()?;
                        self.expect(":")?;
                        tasks.push(Task::Object);
                        tasks.push(Task::Value);
                    }
                }
                Task::Value => match self.current.text {
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
                        self.name()?;
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
                    _ => return self.unexpected(),
                },
            }
        }
        Ok(())
    }
    fn arguments(&mut self, constant: bool) -> Result<()> {
        if self.eat("(")? {
            loop {
                self.name()?;
                self.expect(":")?;
                self.value(constant)?;
                if self.eat(")")? {
                    break;
                }
            }
        }
        Ok(())
    }
    fn directives(&mut self, constant: bool) -> Result<()> {
        while self.eat("@")? {
            self.name()?;
            self.arguments(constant)?;
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
                self.value(true)?;
            }
            let directive_start = self.current.start;
            let directive = self.current.text == "@";
            self.directives(true)?;
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
                    self.directives(false)?;
                    true
                } else if self.current.kind == Kind::Name {
                    self.name()?;
                    self.directives(false)?;
                    false
                } else {
                    self.directives(false)?;
                    true
                }
            } else {
                self.name()?;
                if self.eat(":")? {
                    self.name()?;
                }
                self.arguments(false)?;
                self.directives(false)?;
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
                    self.directives(false)?;
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
                    self.directives(false)?;
                    self.selection_set()?;
                }
                _ => return self.unexpected(),
            }
        }
        Ok(())
    }
}

/// Return grammar-admitted variable rearrangements, or a scoped syntax error.
/// An unrelated earlier error leaves its diagnostics and grammar acceptance to
/// the ordinary parser. Both input and selection walks are iterative.
pub(super) fn inspect(source: &str, edits: &mut Vec<Edit>) -> std::result::Result<(), Error> {
    let mut lexer = Lexer { source, cursor: 0 };
    let current = match lexer.next() {
        Ok(current) => current,
        Err(failure) => {
            return if failure.owned {
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
        Err(failure) if failure.owned => return Err(failure.error),
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
