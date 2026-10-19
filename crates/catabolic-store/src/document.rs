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
            // graphql-core counts skipped comments, but checks the combined
            // count only after advancing to the next non-comment token or EOF.
            // Keep only the count; never allocate comment/token objects.
            count += 1;
            continue;
        }
        if bytes[index..].starts_with(&[0xef, 0xbb, 0xbf]) {
            index += 3;
            continue;
        }
        let start = index;
        count += 1;
        if count > maximum {
            return Some(token_limit_error(text, start, maximum));
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
    (count > maximum).then(|| token_limit_error(text, text.len(), maximum))
}

fn token_limit_error(text: &str, start: usize, maximum: usize) -> Value {
    let position = crate::validation::source_position(&text[..start]);
    let (line, column) = (position.line, position.column);
    json!({"data":null,"errors":[{"message":format!("Syntax Error: Document contains more than {maximum} tokens. Parsing aborted."),"locations":[{"line":line,"column":column}]}]})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lexical_limit_counts_comments_but_not_string_contents_or_commas() {
        assert!(token_limit("{ profile # comment\n schemaVersion }", 5).is_none());
        assert!(token_limit("{ profile # comment\n schemaVersion }", 4).is_some());
        assert!(token_limit("{ item(id:\"brace { and # text\") { id } }", 11).is_none());
        assert_eq!(
            token_limit("{ a b c }", 3).unwrap()["errors"][0]["locations"],
            json!([{"line":1,"column":7}])
        );
    }
}

#[cfg(test)]
mod token_location_tests {
    use super::*;

    #[test]
    fn exact_4000_token_boundary_and_eof_are_unchanged() {
        for separator in ["\n", "\r", "\r\n"] {
            for count in [3999, 4000] {
                let document = ("profile".to_owned() + separator).repeat(count);
                assert!(token_limit(&document, 4000).is_none());
            }
            let document = ("profile".to_owned() + separator).repeat(4001);
            assert_eq!(
                token_limit(&document, 4000).unwrap(),
                json!({"data":null,"errors":[{"message":"Syntax Error: Document contains more than 4000 tokens. Parsing aborted.","locations":[{"line":4000,"column":8}]}]}),
            );
        }
    }

    #[test]
    fn original_lf_cr_crlf_budget_witnesses_match_frozen_coordinates() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/parity/graphql-source-locations.json"
        ))
        .unwrap();
        let mut count = 0;
        for case in fixture["unchanged_parser_and_budget_controls"]
            .as_array()
            .unwrap()
        {
            if !case["name"].as_str().unwrap().contains("token-budget") {
                continue;
            }
            count += 1;
            let actual = token_limit(case["document"].as_str().unwrap(), 4000).unwrap();
            assert_eq!(
                actual, case["reference"]["stdout"]["data"],
                "{}",
                case["name"]
            );
            assert_ne!(actual, case["native"]["stdout"]["data"]);
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn splitline_boundaries_indentation_and_unicode_are_only_coordinates() {
        for (prefix, line, column) in [
            ("", 1, 1),
            ("\n", 1, 1),
            ("\r", 1, 1),
            ("\r\n", 1, 1),
            ("\n\n", 2, 1),
            ("\r\n\r\n", 2, 1),
            ("\n  ", 2, 3),
            ("\r  ", 2, 3),
            ("\r\n  ", 2, 3),
            ("#雪😀\n", 1, 4),
            ("#雪😀\r\n", 1, 4),
            ("#雪\u{2028}😀\n", 2, 2),
            ("\u{feff}", 1, 2),
        ] {
            assert_eq!(
                token_limit(&format!("{prefix}profile"), 0).unwrap()["errors"][0]["locations"],
                json!([{"line":line,"column":column}]),
                "{prefix:?}",
            );
        }
    }

    #[test]
    fn diagnostic_change_does_not_expand_the_lexical_budget() {
        for token in [
            "name",
            "_name1",
            "-12.5e+3",
            "...",
            "\"snow 雪😀 # {\"",
            "\"\"\"block\n# text\"\"\"",
        ] {
            assert!(
                token_limit(&format!("{token} # comment\r\n,\u{feff}"), 2).is_none(),
                "{token}"
            );
            assert!(token_limit(&format!("{token} x"), 1).is_some(), "{token}");
        }
        // Comment objects count even when the next non-comment token is EOF.
        assert!(token_limit(&format!("{{profile}}{}", "\n# comment".repeat(4001)), 4000).is_some());
        // Invalid characters and incomplete strings keep the existing behavior.
        assert!(token_limit(&format!("\"{}", "profile ".repeat(4001)), 4000).is_none());
        assert!(token_limit(&format!("? {}", "profile ".repeat(4001)), 4000).is_some());
        assert!(token_limit("{ a b c }", 5).is_none());
        assert_eq!(
            token_limit("{ a b c }", 3).unwrap()["errors"][0]["locations"],
            json!([{"line":1,"column":7}])
        );
    }

    #[test]
    fn frozen_comment_runs_count_without_expanding_strings() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/parity/graphql-comment-token-budget-v1.json"
        ))
        .unwrap();
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 80);
        for case in cases {
            let actual = token_limit(case["document"].as_str().unwrap(), 4000);
            let expected = (case["token_count"].as_u64().unwrap() > 4000)
                .then(|| case["reference"]["stdout"]["data"].clone());
            assert_eq!(actual, expected, "{}", case["name"]);
        }
    }

    #[test]
    fn comment_checks_wait_for_the_next_token_or_eof() {
        for separator in ["\n", "\r", "\r\n"] {
            assert!(token_limit("#", 1).is_none());
            assert!(token_limit(&format!("#{separator}#"), 2).is_none());
            let document = format!("#{separator}#雪😀{separator}   name");
            assert_eq!(
                token_limit(&document, 1).unwrap()["errors"][0]["locations"],
                json!([{"line":3,"column":4}]),
            );
            let document = format!("#{separator}#雪😀{separator} ,\u{feff}  ");
            assert_eq!(
                token_limit(&document, 1).unwrap()["errors"][0]["locations"],
                json!([{"line":3,"column":6}]),
            );
        }
    }
}
