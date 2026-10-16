// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Error-only frozen GraphQL CLI admission. This private tree is never handed
//! to a resolver or a transport decoder. Accepted input still passes unchanged
//! through the hardened shared decoder and InputVariables order owner.
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct AdmissionError {
    kind: &'static str,
    message: String,
}
impl AdmissionError {
    pub fn kind(&self) -> &'static str {
        self.kind
    }
    fn value(message: String) -> Self {
        Self {
            kind: "ValueError",
            message,
        }
    }
}
impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for AdmissionError {}

enum Diagnostic {
    Atom(String),
    Nonfinite(&'static str),
    String(Vec<u32>),
    List(Vec<Self>),
    Object(BTreeMap<Vec<u32>, Self>),
}
struct Reader<'a> {
    raw: &'a str,
    at: usize,
}
impl Reader<'_> {
    fn byte(&self) -> Option<u8> {
        self.raw.as_bytes().get(self.at).copied()
    }
    fn ws(&mut self) {
        while self.byte().is_some_and(|c| b" \r\n\t".contains(&c)) {
            self.at += 1;
        }
    }
    // The syntax owner has already validated escapes. Keep UTF-16 code points
    // solely to reproduce the refusal location; Rust strings stay scalar-only.
    fn string(&mut self) -> Option<Vec<u32>> {
        if self.byte()? != b'"' {
            return None;
        }
        self.at += 1;
        let mut out = vec![];
        loop {
            match self.byte()? {
                b'"' => {
                    self.at += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.at += 1;
                    let escaped = self.byte()?;
                    self.at += 1;
                    let c = match escaped {
                        b'"' | b'\\' | b'/' => u32::from(escaped),
                        b'b' => 8,
                        b'f' => 12,
                        b'n' => 10,
                        b'r' => 13,
                        b't' => 9,
                        b'u' => {
                            let mut c =
                                u32::from_str_radix(self.raw.get(self.at..self.at + 4)?, 16)
                                    .ok()?;
                            self.at += 4;
                            if (0xd800..=0xdbff).contains(&c)
                                && self.raw[self.at..].starts_with("\\u")
                            {
                                let low = u32::from_str_radix(
                                    self.raw.get(self.at + 2..self.at + 6)?,
                                    16,
                                )
                                .ok()?;
                                if (0xdc00..=0xdfff).contains(&low) {
                                    c = 0x10000 + (c - 0xd800) * 1024 + low - 0xdc00;
                                    self.at += 6;
                                }
                            }
                            c
                        }
                        _ => return None,
                    };
                    out.push(c);
                }
                _ => {
                    let c = self.raw[self.at..].chars().next()?;
                    self.at += c.len_utf8();
                    out.push(c as u32);
                }
            }
        }
    }
    fn read(&mut self, depth: usize) -> Result<Option<Diagnostic>, AdmissionError> {
        if depth > 512 {
            return Ok(None);
        }
        self.ws();
        Ok(Some(match self.byte() {
            Some(b'"') => match self.string() {
                Some(s) => Diagnostic::String(s),
                None => return Ok(None),
            },
            Some(b'{' | b'[') => {
                let object = self.byte() == Some(b'{');
                let close = if object { b'}' } else { b']' };
                self.at += 1;
                self.ws();
                let mut fields = BTreeMap::new();
                let mut values = vec![];
                if self.byte() != Some(close) {
                    loop {
                        let key = if object {
                            let Some(key) = self.string() else {
                                return Ok(None);
                            };
                            self.ws();
                            if self.byte() != Some(b':') {
                                return Ok(None);
                            }
                            self.at += 1;
                            Some(key)
                        } else {
                            None
                        };
                        let Some(value) = self.read(depth + 1)? else {
                            return Ok(None);
                        };
                        if let Some(key) = key {
                            fields.insert(key, value);
                        } else {
                            values.push(value);
                        }
                        self.ws();
                        if self.byte() == Some(close) {
                            break;
                        }
                        if self.byte() != Some(b',') {
                            return Ok(None);
                        }
                        self.at += 1;
                        self.ws();
                    }
                }
                self.at += 1;
                if object {
                    Diagnostic::Object(fields)
                } else {
                    Diagnostic::List(values)
                }
            }
            Some(_) => {
                let start = self.at;
                while self.byte().is_some_and(|c| !b",]} \r\n\t".contains(&c)) {
                    self.at += 1;
                }
                let token = &self.raw[start..self.at];
                let nonfinite = match token {
                    "NaN" => Some("nan"),
                    "Infinity" => Some("inf"),
                    "-Infinity" => Some("-inf"),
                    _ if token.contains(['.', 'e', 'E']) => token
                        .parse::<f64>()
                        .ok()
                        .filter(|v| v.is_infinite())
                        .map(|v| if v.is_sign_negative() { "-inf" } else { "inf" }),
                    _ => None,
                };
                if let Some(value) = nonfinite {
                    Diagnostic::Nonfinite(value)
                } else {
                    let digits = token.strip_prefix('-').unwrap_or(token);
                    if digits.len() > 4300 && digits.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(AdmissionError::value(format!(
                            "Exceeds the limit (4300 digits) for integer string conversion: value has {} digits; use sys.set_int_max_str_digits() to increase the limit",
                            digits.len()
                        )));
                    }
                    let Ok(value) = catabolic_core::decode_json(token) else {
                        return Ok(None);
                    };
                    Diagnostic::Atom(catabolic_core::encode(&value))
                }
            }
            None => return Ok(None),
        }))
    }
}
fn append(out: &mut Vec<u32>, text: &str) {
    out.extend(text.chars().map(u32::from));
}
fn string(out: &mut Vec<u32>, value: &[u32]) {
    append(out, "\"");
    for &c in value {
        match c {
            8 => append(out, "\\b"),
            9 => append(out, "\\t"),
            10 => append(out, "\\n"),
            12 => append(out, "\\f"),
            13 => append(out, "\\r"),
            34 => append(out, "\\\""),
            92 => append(out, "\\\\"),
            0..=31 => append(out, &format!("\\u{c:04x}")),
            _ => out.push(c),
        }
    }
    append(out, "\"");
}
impl Diagnostic {
    fn encode(&self, out: &mut Vec<u32>) -> Result<(), AdmissionError> {
        match self {
            Self::Atom(value) => append(out, value),
            Self::Nonfinite(value) => {
                return Err(AdmissionError::value(format!(
                    "Out of range float values are not JSON compliant: {value}"
                )));
            }
            Self::String(value) => string(out, value),
            Self::List(values) => {
                append(out, "[");
                for (i, value) in values.iter().enumerate() {
                    if i > 0 {
                        append(out, ", ");
                    }
                    value.encode(out)?;
                }
                append(out, "]");
            }
            Self::Object(values) => {
                append(out, "{");
                for (i, (key, value)) in values.iter().enumerate() {
                    if i > 0 {
                        append(out, ", ");
                    }
                    string(out, key);
                    append(out, ": ");
                    value.encode(out)?;
                }
                append(out, "}");
            }
        }
        Ok(())
    }
}
/// Called only after ordinary syntax validation and before shared value decoding.
/// The pinned CLI contract uses CPython's default 4,300 decimal-digit limit.
/// Malformed/deep input and overwritten invalid values keep existing admission.
pub fn validate(raw: &str) -> Result<(), AdmissionError> {
    let mut reader = Reader { raw, at: 0 };
    let Some(value) = reader.read(0)? else {
        return Ok(());
    };
    reader.ws();
    if reader.at != raw.len() {
        return Ok(());
    }
    // Frozen GraphQL checks the top-level shape before re-encoding values.
    if !matches!(value, Diagnostic::Object(_)) {
        return Ok(());
    }
    let mut encoded = vec![];
    value.encode(&mut encoded)?;
    if let Some(start) = encoded.iter().position(|c| (0xd800..=0xdfff).contains(c)) {
        let end = start
            + encoded[start..]
                .iter()
                .take_while(|c| (0xd800..=0xdfff).contains(*c))
                .count();
        let message = if end == start + 1 {
            format!(
                "'utf-8' codec can't encode character '\\u{:04x}' in position {start}: surrogates not allowed",
                encoded[start]
            )
        } else {
            format!(
                "'utf-8' codec can't encode characters in position {start}-{}: surrogates not allowed",
                end - 1
            )
        };
        return Err(AdmissionError {
            kind: "UnicodeEncodeError",
            message,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn error_only_admission_retains_scalar_and_duplicate_boundaries() {
        for raw in [
            "NaN",
            r#""\ud800""#,
            r#"{"x":NaN,"x":1}"#,
            r#"{"x":"\ud800","x":"ok"}"#,
            r#"{"x":"\ud800\udc00"}"#,
        ] {
            assert!(validate(raw).is_ok(), "{raw}");
        }
        for (raw, kind, suffix) in [
            (r#"{"x":NaN}"#, "ValueError", "nan"),
            (r#"{"x":-1e400}"#, "ValueError", "-inf"),
            (
                r#"{"x":"\ud800"}"#,
                "UnicodeEncodeError",
                "surrogates not allowed",
            ),
        ] {
            let error = validate(raw).unwrap_err();
            assert_eq!(error.kind(), kind);
            assert!(error.to_string().ends_with(suffix));
        }
        assert!(validate(&format!("{{\"x\":{}}}", "1".repeat(4300))).is_ok());
        assert_eq!(
            validate(&format!("{{\"x\":{}}}", "1".repeat(4301)))
                .unwrap_err()
                .kind(),
            "ValueError"
        );
    }
    #[test]
    fn canonical_position_and_nonfinite_precedence() {
        assert_eq!(
            validate(r#"{"z":"\ud800","a":"雪😀\n"}"#)
                .unwrap_err()
                .to_string(),
            "'utf-8' codec can't encode character '\\ud800' in position 20: surrogates not allowed"
        );
        assert_eq!(
            validate(r#"{"x":"\ud800\ud801"}"#).unwrap_err().to_string(),
            "'utf-8' codec can't encode characters in position 7-8: surrogates not allowed"
        );
        assert_eq!(
            validate(r#"{"a":"\ud800","z":NaN}"#).unwrap_err().kind(),
            "ValueError"
        );
    }
}
