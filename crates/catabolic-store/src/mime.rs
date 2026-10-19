// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen Python 3.14.6/Linux MIME lookup. OS-registry differences remain a
//! platform qualification item, not an implicit runtime policy change.
use serde_json::Value;
use std::sync::OnceLock;
fn registry() -> &'static Value {
    static MAP: OnceLock<Value> = OnceLock::new();
    MAP.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/http-mime.json"))
            .expect("frozen MIME registry")
    })
}
fn split(path: &str) -> (&str, &str) {
    let start = path.rfind('/').map_or(0, |v| v + 1);
    if let Some(dot) = path.rfind('.')
        && dot >= start
        && path[start..dot].chars().any(|c| c != '.')
    {
        (&path[..dot], &path[dot..])
    } else {
        (path, "")
    }
}
pub fn guess(path: &str) -> Option<String> {
    let map = registry();
    let mut path = path.to_owned();
    if let Some((scheme, rest)) = path.split_once(':')
        && scheme.len() > 1
        && scheme.bytes().enumerate().all(|(i, b)| {
            b.is_ascii_alphabetic() || i > 0 && (b.is_ascii_digit() || b"+.-".contains(&b))
        })
    {
        if scheme.eq_ignore_ascii_case("data") {
            let comma = rest.find(',')?;
            let kind = rest[..comma].split(';').next()?;
            return Some(if kind.contains('/') && !kind.contains('=') {
                kind.to_owned()
            } else {
                "text/plain".into()
            });
        }
        let rest = if let Some(authority) = rest.strip_prefix("//") {
            authority.find('/').map_or("", |at| &authority[at..])
        } else {
            rest
        };
        path = rest.split(['?', '#']).next().unwrap_or("").to_owned();
    }
    for _ in 0..16 {
        let (base, ext) = split(&path);
        if let Some(suffix) = map["suffixes"][ext.to_lowercase()].as_str() {
            path = format!("{base}{suffix}")
        } else {
            break;
        }
    }
    let (base, ext) = split(&path);
    let ext = if map["encodings"].get(ext).is_some() {
        split(base).1
    } else {
        ext
    };
    map["types"][ext.to_lowercase()].as_str().map(str::to_owned)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_python_extensions() {
        for (path, want) in [
            ("x.svg", Some("image/svg+xml")),
            ("x.SVG", Some("image/svg+xml")),
            ("x.svgz", Some("image/svg+xml")),
            ("x.tar.gz", Some("application/x-tar")),
            (".html", None),
            ("x.bin", Some("application/octet-stream")),
            ("data:text/html,hello", Some("text/html")),
        ] {
            assert_eq!(guess(path).as_deref(), want, "{path}");
        }
    }
}
