// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! JSON value parsing that preserves arbitrary integers and literal objects.
//! Objects never enter serde_json's private-number deserialization channel.
use crate::{Error, Result};
use serde_json::{Map, Value, json};
fn fail<T>(message: &str) -> Result<T> {
    Err(Error(message.into()))
}

/// Parse containers ourselves so duplicate object keys are never silently lost.
/// Strings and number tokens use serde_json, but objects can never be mistaken
/// for serde_json's private arbitrary-precision number sentinel.
struct Parser<'a> {
    raw: &'a str,
    at: usize,
    reject_duplicates: bool,
}
impl Parser<'_> {
    fn ws(&mut self) {
        while self
            .raw
            .as_bytes()
            .get(self.at)
            .is_some_and(|c| b" \r\n\t".contains(c))
        {
            self.at += 1;
        }
    }
    fn take(&mut self, byte: u8) -> bool {
        self.ws();
        if self.raw.as_bytes().get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn string(&mut self) -> Result<String> {
        self.ws();
        let start = self.at;
        if !self.take(b'"') {
            return fail("invalid JSON");
        }
        let mut escaped = false;
        while let Some(byte) = self.raw.as_bytes().get(self.at) {
            self.at += 1;
            if *byte == b'"' && !escaped {
                return serde_json::from_str(&self.raw[start..self.at])
                    .map_err(|_| Error("invalid JSON".into()));
            }
            escaped = *byte == b'\\' && !escaped;
        }
        fail("invalid JSON")
    }
    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > 512 {
            return fail("invalid JSON");
        }
        self.ws();
        match self.raw.as_bytes().get(self.at).copied() {
            Some(b'{') => {
                self.at += 1;
                let mut object = Map::new();
                let mut duplicate = false;
                if !self.take(b'}') {
                    loop {
                        let key = self.string()?;
                        if !self.take(b':') {
                            return fail("invalid JSON");
                        }
                        let value = self.value(depth + 1)?;
                        duplicate |= object.insert(key, value).is_some();
                        if self.take(b'}') {
                            break;
                        }
                        if !self.take(b',') {
                            return fail("invalid JSON");
                        }
                    }
                }
                if duplicate && self.reject_duplicates {
                    return fail("duplicate JSON object key; refusing lossy parsing");
                }
                Ok(Value::Object(object))
            }
            Some(b'[') => {
                self.at += 1;
                let mut values = vec![];
                if !self.take(b']') {
                    loop {
                        values.push(self.value(depth + 1)?);
                        if self.take(b']') {
                            break;
                        }
                        if !self.take(b',') {
                            return fail("invalid JSON");
                        }
                    }
                }
                Ok(Value::Array(values))
            }
            Some(b'"') => Ok(json!(self.string()?)),
            Some(_) => {
                let start = self.at;
                while self
                    .raw
                    .as_bytes()
                    .get(self.at)
                    .is_some_and(|c| !b" \r\n\t,]}".contains(c))
                {
                    self.at += 1;
                }
                let token = &self.raw[start..self.at];
                if ["NaN", "Infinity", "-Infinity"].contains(&token) {
                    return Err(Error(format!(
                        "nonfinite JSON number is not permitted: {token}"
                    )));
                }
                let value: Value =
                    serde_json::from_str(token).map_err(|_| Error("invalid JSON".into()))?;
                if value.is_array() || value.is_object() || value.is_string() {
                    return fail("invalid JSON");
                }
                Ok(value)
            }
            None => fail("invalid JSON"),
        }
    }
}
pub fn parse(raw: &str, reject_duplicates: bool) -> Result<Value> {
    let mut parser = Parser {
        raw,
        at: 0,
        reject_duplicates,
    };
    let value = parser.value(0)?;
    parser.ws();
    if parser.at != raw.len() {
        return fail("invalid JSON");
    }
    Ok(value)
}

/// Container shape of raw JSON text with Python `json.loads` object key order:
/// a repeated key keeps its first position and the last occurrence's contents.
/// Scalars are not retained; callers take every value from the decoded `Value`.
#[derive(Clone, Debug, PartialEq)]
pub enum SourceOrder {
    Object(Vec<(String, SourceOrder)>),
    Array(Vec<SourceOrder>),
    Scalar,
}
impl SourceOrder {
    /// The order recorded for an object member, if this is an object containing it.
    pub fn get(&self, key: &str) -> Option<&SourceOrder> {
        match self {
            Self::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    /// Object keys in source insertion order; empty for arrays and scalars.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        let fields: &[(String, SourceOrder)] = match self {
            Self::Object(fields) => fields,
            _ => &[],
        };
        fields.iter().map(|(k, _)| k.as_str())
    }
}
impl Parser<'_> {
    fn order(&mut self, depth: usize) -> Result<SourceOrder> {
        if depth > 512 {
            return fail("invalid JSON");
        }
        self.ws();
        match self.raw.as_bytes().get(self.at).copied() {
            Some(b'{') => {
                self.at += 1;
                let mut fields: Vec<(String, SourceOrder)> = vec![];
                let mut positions = std::collections::HashMap::<String, usize>::new();
                if !self.take(b'}') {
                    loop {
                        let key = self.string()?;
                        if !self.take(b':') {
                            return fail("invalid JSON");
                        }
                        let child = self.order(depth + 1)?;
                        if let Some(&at) = positions.get(&key) {
                            fields[at].1 = child;
                        } else {
                            positions.insert(key.clone(), fields.len());
                            fields.push((key, child));
                        }
                        if self.take(b'}') {
                            break;
                        }
                        if !self.take(b',') {
                            return fail("invalid JSON");
                        }
                    }
                }
                Ok(SourceOrder::Object(fields))
            }
            Some(b'[') => {
                self.at += 1;
                let mut values = vec![];
                if !self.take(b']') {
                    loop {
                        values.push(self.order(depth + 1)?);
                        if self.take(b']') {
                            break;
                        }
                        if !self.take(b',') {
                            return fail("invalid JSON");
                        }
                    }
                }
                Ok(SourceOrder::Array(values))
            }
            _ => self.value(depth).map(|_| SourceOrder::Scalar),
        }
    }
}

/// Record object insertion order for owners whose frozen Python traversal
/// iterates user-supplied dictionaries. It never changes decoded values,
/// canonical encoding or digests, which remain sorted.
pub fn source_order(raw: &str) -> Result<SourceOrder> {
    let mut parser = Parser {
        raw,
        at: 0,
        reject_duplicates: false,
    };
    let order = parser.order(0)?;
    parser.ws();
    if parser.at != raw.len() {
        return fail("invalid JSON");
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_order_keeps_python_insertion_positions() {
        let raw = r#" {"z":{"b":1,"a":[{"y":0,"x":0}]},"a":2,"z":{"d":3,"c":4}} "#;
        let order = source_order(raw).unwrap();
        assert_eq!(order.keys().collect::<Vec<_>>(), ["z", "a"]);
        // A repeated key keeps its first position but the last value's shape.
        assert_eq!(
            order.get("z").unwrap().keys().collect::<Vec<_>>(),
            ["d", "c"]
        );
        assert_eq!(order.get("a"), Some(&SourceOrder::Scalar));
        assert!(order.get("missing").is_none());
        let nested = source_order(r#"{"q":[{"y":0,"x":0}]}"#).unwrap();
        let Some(SourceOrder::Array(items)) = nested.get("q") else {
            panic!("array order")
        };
        assert_eq!(items[0].keys().collect::<Vec<_>>(), ["y", "x"]);
        for raw in ["{\"a\":1,}", "{}x", "[1,]", r#"{"x":NaN}"#] {
            assert!(source_order(raw).is_err(), "{raw}");
        }
    }
    #[test]
    fn private_sentinel_is_an_object_and_big_integers_are_exact() {
        let raw = r#"{"$serde_json::private::Number":"1e400","n":1208925819614629174706176}"#;
        let value = parse(raw, false).unwrap();
        assert!(value.is_object());
        assert_eq!(value["$serde_json::private::Number"], "1e400");
        assert_eq!(value["n"].to_string(), "1208925819614629174706176");
    }
    #[test]
    fn duplicate_policy_is_explicit() {
        assert_eq!(parse(r#"{"x":1,"\u0078":2}"#, false).unwrap()["x"], 2);
        assert!(parse(r#"{"x":1,"\u0078":2}"#, true).is_err());
        for raw in ["[1,]", "{\"a\":1,}", "{}x", r#"{"x":undefined}"#] {
            assert!(parse(raw, false).is_err(), "{raw}");
        }
    }
}
