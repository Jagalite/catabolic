//! Values and deterministic codecs shared by every Catabolic interface.
pub mod coordination;
pub mod execution;
pub mod fallback;
pub mod json;
pub mod lifecycle;
pub mod media;
pub mod playback;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error(pub String);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self(value.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self(value.to_string())
    }
}
pub fn sha256(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

/// Parse a companion-boundary JSON value without accepting non-finite floats.
/// Arbitrarily large integers remain lossless for frozen interchange extensions.
pub fn decode_json(raw: &str) -> Result<Value> {
    let value = json::parse(raw, false)?;
    validate_finite(&value)?;
    Ok(value)
}

pub fn decode_json_bytes(raw: &[u8]) -> Result<Value> {
    decode_json(std::str::from_utf8(raw).map_err(|e| Error(e.to_string()))?)
}

pub fn validate_finite(value: &Value) -> Result<()> {
    match value {
        Value::Number(number)
            if number.to_string().contains(['.', 'e', 'E']) && number.as_f64().is_none() =>
        {
            Err(Error(
                "Out of range float values are not JSON compliant".into(),
            ))
        }
        Value::Array(values) => values.iter().try_for_each(validate_finite),
        Value::Object(values) => values.values().try_for_each(validate_finite),
        _ => Ok(()),
    }
}

/// Python's frozen json.dumps(sort_keys=True, ensure_ascii=False, allow_nan=False).
pub fn encode(value: &Value) -> String {
    encode_with(value, false, false)
}
pub fn encode_with(value: &Value, ascii: bool, compact: bool) -> String {
    let comma = if compact { "," } else { ", " };
    let colon = if compact { ":" } else { ": " };
    match value {
        Value::String(s) => {
            let quoted = serde_json::to_string(s).expect("JSON string");
            let mut result = String::new();
            for c in quoted.chars() {
                if ascii && c as u32 > 126 {
                    let mut units = [0; 2];
                    for unit in c.encode_utf16(&mut units) {
                        result.push_str(&format!("\\u{unit:04x}"));
                    }
                } else {
                    result.push(c);
                }
            }
            result
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(|v| encode_with(v, ascii, compact))
                .collect::<Vec<_>>()
                .join(comma)
        ),
        Value::Object(o) => {
            let mut keys = o.keys().collect::<Vec<_>>();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|k| format!(
                        "{}{colon}{}",
                        encode_with(&Value::String(k.clone()), ascii, compact),
                        encode_with(&o[k], ascii, compact)
                    ))
                    .collect::<Vec<_>>()
                    .join(comma)
            )
        }
        Value::Number(n) if n.is_f64() => {
            let number = n.as_f64().expect("JSON float");
            // With arbitrary_precision, parsed numbers retain their lexical
            // spelling (for example 1e2). Canonical identities follow Python's
            // decoded f64 value, rather than the original JSON token.
            let s = serde_json::Number::from_f64(number)
                .expect("finite JSON float")
                .to_string();
            if number != 0.0 && (number.abs() < 1e-4 || number.abs() >= 1e16) && !s.contains('e') {
                let negative = s.starts_with('-');
                let unsigned = s.trim_start_matches('-');
                let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
                let (digits, exponent) = if integer == "0" {
                    let zeros = fraction.chars().take_while(|c| *c == '0').count();
                    (
                        fraction[zeros..].trim_end_matches('0').to_string(),
                        -(zeros as i32) - 1,
                    )
                } else {
                    let digits = format!("{integer}{fraction}");
                    (
                        digits.trim_end_matches('0').to_string(),
                        integer.len() as i32 - 1,
                    )
                };
                let mantissa = if digits.len() == 1 {
                    digits
                } else {
                    format!("{}.{}", &digits[..1], &digits[1..])
                };
                return format!(
                    "{}{mantissa}e{}{abs:02}",
                    if negative { "-" } else { "" },
                    if exponent < 0 { "-" } else { "+" },
                    abs = exponent.abs()
                );
            }
            if let Some((mantissa, exponent)) = s.split_once('e') {
                let exponent: i32 = exponent.parse().expect("JSON exponent");
                let mantissa = mantissa.strip_suffix(".0").unwrap_or(mantissa);
                format!(
                    "{mantissa}e{}{abs:02}",
                    if exponent < 0 { "-" } else { "+" },
                    abs = exponent.abs()
                )
            } else {
                s
            }
        }
        Value::Number(n) if n.is_i64() => n.as_i64().expect("JSON integer").to_string(),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn frozen_encoding() {
        let v = json!({"z": "Café😀", "a": [true, null, 9223372036854775807_i64, 1e-7]});
        assert_eq!(
            encode(&v),
            "{\"a\": [true, null, 9223372036854775807, 1e-07], \"z\": \"Café😀\"}"
        );
        assert!(encode_with(&v, true, true).contains("Caf\\u00e9\\ud83d\\ude00"));
    }
    #[test]
    fn decoded_number_tokens_have_canonical_identities() {
        for (raw, expected) in [
            ("1e2", "100.0"),
            ("-0", "0"),
            ("1.000", "1.0"),
            ("1E-7", "1e-07"),
            ("-0.00", "-0.0"),
            ("1208925819614629174706176", "1208925819614629174706176"),
        ] {
            assert_eq!(
                encode(&serde_json::from_str::<Value>(raw).unwrap()),
                expected,
                "{raw}"
            );
        }
    }
    #[test]
    fn overflowing_floats_reject_without_truncating_large_integers() {
        assert!(decode_json("{\"nested\":[1e400]}").is_err());
        assert!(decode_json("-1e400").is_err());
        assert!(decode_json("1208925819614629174706176").is_ok());
        assert!(
            decode_json("{\"$serde_json::private::Number\":\"1e400\"}")
                .unwrap()
                .is_object()
        );
    }
}
