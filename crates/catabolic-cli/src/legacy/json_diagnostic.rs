// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Syntax-only diagnostics for the frozen legacy CLI's JSON input boundaries.
//! This does not construct values, normalize numeric tokens, or replace the
//! companion decoder. Accepted values still belong to the existing decoder.
#[derive(Debug, PartialEq, Eq)]
pub struct JsonError {
    message: &'static str,
    position: usize,
    line: usize,
    column: usize,
}
impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: line {} column {} (char {})",
            self.message, self.line, self.column, self.position
        )
    }
}
impl std::error::Error for JsonError {}

struct Parser<'a> {
    raw: &'a str,
    at: usize,
}
impl Parser<'_> {
    fn error(&self, message: &'static str, at: usize) -> JsonError {
        let prefix = &self.raw[..at];
        JsonError {
            message,
            position: prefix.chars().count(),
            line: prefix.bytes().filter(|c| *c == b'\n').count() + 1,
            column: prefix.rsplit('\n').next().unwrap().chars().count() + 1,
        }
    }
    fn byte(&self) -> Option<u8> {
        self.raw.as_bytes().get(self.at).copied()
    }
    fn ws(&mut self) {
        while self.byte().is_some_and(|c| b" \t\r\n".contains(&c)) {
            self.at += 1;
        }
    }
    fn string(&mut self) -> Result<(), JsonError> {
        let start = self.at;
        self.at += 1;
        while let Some(byte) = self.byte() {
            match byte {
                b'"' => {
                    self.at += 1;
                    return Ok(());
                }
                0..=31 => return Err(self.error("Invalid control character at", self.at)),
                b'\\' => {
                    let slash = self.at;
                    self.at += 1;
                    match self.byte() {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                            self.at += 1
                        }
                        Some(b'u') => {
                            let unicode = self.at;
                            self.at += 1;
                            for _ in 0..4 {
                                if !self.byte().is_some_and(|c| c.is_ascii_hexdigit()) {
                                    return Err(self.error("Invalid \\uXXXX escape", unicode));
                                }
                                self.at += 1;
                            }
                        }
                        Some(_) => return Err(self.error("Invalid \\escape", slash)),
                        None => break,
                    }
                }
                _ => self.at += self.raw[self.at..].chars().next().unwrap().len_utf8(),
            }
        }
        Err(self.error("Unterminated string starting at", start))
    }
    fn number(&mut self) -> bool {
        let start = self.at;
        if self.byte() == Some(b'-') {
            self.at += 1;
        }
        match self.byte() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                self.at += 1;
                while self.byte().is_some_and(|c| c.is_ascii_digit()) {
                    self.at += 1;
                }
            }
            _ => {
                self.at = start;
                return false;
            }
        }
        if self.byte() == Some(b'.') {
            let point = self.at;
            self.at += 1;
            if self.byte().is_some_and(|c| c.is_ascii_digit()) {
                while self.byte().is_some_and(|c| c.is_ascii_digit()) {
                    self.at += 1;
                }
            } else {
                self.at = point;
            }
        }
        if matches!(self.byte(), Some(b'e' | b'E')) {
            let exponent = self.at;
            self.at += 1;
            if matches!(self.byte(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.byte().is_some_and(|c| c.is_ascii_digit()) {
                while self.byte().is_some_and(|c| c.is_ascii_digit()) {
                    self.at += 1;
                }
            } else {
                self.at = exponent;
            }
        }
        true
    }
    // The companion's depth limit is a distinct admission constraint, not a
    // JSON syntax error. Stop classification there and retain its original error.
    fn value(&mut self, depth: usize) -> Result<bool, JsonError> {
        if depth > 512 {
            return Ok(false);
        }
        self.ws();
        match self.byte() {
            Some(b'"') => self.string()?,
            Some(b'{' | b'[') => {
                let object = self.byte() == Some(b'{');
                let close = if object { b'}' } else { b']' };
                self.at += 1;
                self.ws();
                if self.byte() == Some(close) {
                    self.at += 1;
                    return Ok(true);
                }
                loop {
                    if object {
                        if self.byte() != Some(b'"') {
                            return Err(self.error(
                                "Expecting property name enclosed in double quotes",
                                self.at,
                            ));
                        }
                        self.string()?;
                        self.ws();
                        if self.byte() != Some(b':') {
                            return Err(self.error("Expecting ':' delimiter", self.at));
                        }
                        self.at += 1;
                    }
                    if !self.value(depth + 1)? {
                        return Ok(false);
                    }
                    self.ws();
                    if self.byte() == Some(close) {
                        self.at += 1;
                        break;
                    }
                    if self.byte() != Some(b',') {
                        return Err(self.error("Expecting ',' delimiter", self.at));
                    }
                    let comma = self.at;
                    self.at += 1;
                    self.ws();
                    if self.byte() == Some(close) {
                        return Err(self.error(
                            if object {
                                "Illegal trailing comma before end of object"
                            } else {
                                "Illegal trailing comma before end of array"
                            },
                            comma,
                        ));
                    }
                }
            }
            _ => {
                if let Some(literal) = ["null", "true", "false", "NaN", "Infinity", "-Infinity"]
                    .into_iter()
                    .find(|literal| self.raw[self.at..].starts_with(literal))
                {
                    self.at += literal.len();
                } else if !self.number() {
                    return Err(self.error("Expecting value", self.at));
                }
            }
        }
        Ok(true)
    }
}

/// Return a typed syntax error, or leave valid JSON and separate decoder
/// constraints (nonfinite values, surrogates, depth) with their existing owner.
pub fn validate(raw: &str) -> Result<(), JsonError> {
    let mut parser = Parser { raw, at: 0 };
    if raw.starts_with('\u{feff}') {
        return Err(parser.error("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0));
    }
    if !parser.value(0)? {
        return Ok(());
    }
    parser.ws();
    if parser.at != raw.len() {
        return Err(parser.error("Extra data", parser.at));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn syntax_positions_use_characters_and_python_314_messages() {
        for (raw, expected) in [
            (
                "{",
                "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
            ),
            (
                "[1, ]",
                "Illegal trailing comma before end of array: line 1 column 3 (char 2)",
            ),
            (
                "{\"x\":1,}",
                "Illegal trailing comma before end of object: line 1 column 7 (char 6)",
            ),
            (
                "{\"é😀\":1,\n \"x\": [true false]}",
                "Expecting ',' delimiter: line 2 column 13 (char 21)",
            ),
            ("\"é\\q\"", "Invalid \\escape: line 1 column 3 (char 2)"),
            (
                "\"é\\u12\"",
                "Invalid \\uXXXX escape: line 1 column 4 (char 3)",
            ),
            ("1e+", "Extra data: line 1 column 2 (char 1)"),
            (
                "{\"x\":\"é",
                "Unterminated string starting at: line 1 column 6 (char 5)",
            ),
        ] {
            assert_eq!(validate(raw).unwrap_err().to_string(), expected, "{raw:?}");
        }
    }
    #[test]
    fn syntax_validation_does_not_replace_value_admission() {
        for raw in [
            "null",
            "[true, false]",
            "1.00e+20",
            "{\"x\":1,\"x\":2}",
            "NaN",
            "1e400",
            "\"\\ud800\"",
            "{\"$serde_json::private::Number\":\"123\"}",
        ] {
            assert!(validate(raw).is_ok(), "{raw}");
        }
    }
}
