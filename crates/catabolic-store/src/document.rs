//! Bounded GraphQL lexical preflight, before recursive parsing or validation.
use serde_json::{Value, json};
pub fn token_limit(text: &str, maximum: usize) -> Option<Value> {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut count = 0;
    while index < bytes.len() {
        let c = bytes[index];
        if c.is_ascii_whitespace() || c == b',' {
            index += 1;
            continue;
        }
        if c == b'#' {
            while index < bytes.len() && bytes[index] != b'\n' && bytes[index] != b'\r' {
                index += 1;
            }
            continue;
        }
        if bytes[index..].starts_with(&[0xef, 0xbb, 0xbf]) {
            index += 3;
            continue;
        }
        let start = index;
        count += 1;
        if count > maximum {
            let prefix = &text[..start];
            let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
            let column = prefix.rsplit('\n').next().unwrap().chars().count() + 1;
            return Some(
                json!({"data":null,"errors":[{"message":format!("Syntax Error: Document contains more than {maximum} tokens. Parsing aborted."),"locations":[{"line":line,"column":column}]}]}),
            );
        }
        if bytes[index..].starts_with(b"\"\"\"") {
            index += 3;
            while index < bytes.len() {
                if bytes[index..].starts_with(b"\\\"\"\"") {
                    index += 4;
                } else if bytes[index..].starts_with(b"\"\"\"") {
                    index += 3;
                    break;
                } else {
                    index += 1;
                }
            }
        } else if c == b'"' {
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                } else if bytes[index] == b'"' {
                    index += 1;
                    break;
                } else {
                    index += 1;
                }
            }
        } else if c.is_ascii_alphabetic() || c == b'_' {
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
        } else if c.is_ascii_digit() || c == b'-' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if index < bytes.len() && bytes[index] == b'.' {
                index += 1;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
            }
            if index < bytes.len() && b"eE".contains(&bytes[index]) {
                index += 1;
                if index < bytes.len() && b"+-".contains(&bytes[index]) {
                    index += 1;
                }
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
            }
        } else if bytes[index..].starts_with(b"...") {
            index += 3;
        } else {
            index += text[index..].chars().next().unwrap().len_utf8();
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lexical_limit_ignores_comments_strings_and_commas() {
        assert!(token_limit("{ profile # comment\n schemaVersion }", 4).is_none());
        assert!(token_limit("{ item(id:\"brace { and # text\") { id } }", 11).is_none());
        assert_eq!(
            token_limit("{ a b c }", 3).unwrap()["errors"][0]["locations"],
            json!([{"line":1,"column":7}])
        );
    }
}
