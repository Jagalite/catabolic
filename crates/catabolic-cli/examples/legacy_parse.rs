// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Development-only parser probe for frozen-oracle differential tests.
#[path = "../src/legacy/parser.rs"]
mod parser;
fn main() {
    use std::io::BufRead;
    for line in std::io::stdin().lock().lines() {
        let args: Vec<String> = serde_json::from_str(&line.unwrap()).unwrap();
        let output = match parser::parse(&args) {
            Ok(value) => {
                let mut map = serde_json::Map::new();
                map.insert("args".into(), value);
                serde_json::Value::Object(map)
            }
            Err(value) => {
                serde_json::json!({"code":value.code,"stdout":value.stdout,"stderr":value.stderr})
            }
        };
        println!("{output}");
    }
}
