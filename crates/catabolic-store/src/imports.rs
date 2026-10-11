// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Official-CLI import preflight. The runtime receives immutable snapshots and
//! copies source descriptors into private staging after detaching the catalog.
use crate::{Error, Result, Store};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs::OpenOptions, path::Path};

pub struct Decision {
    pub file: Value,
    pub item: Value,
    pub snapshot: Value,
}
pub struct Prepared {
    pub report: Value,
    pub groups: Vec<Vec<Decision>>,
    pub executable: &'static str,
    pub destination: String,
    pub database_id: String,
    pub timeout: f64,
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| Error(format!("invalid import {key}")))
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value]> {
    value[key]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| Error(format!("invalid import {key}")))
}
pub fn suffix(path: &str) -> String {
    let name = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_start_matches('.');
    name.rfind('.')
        .filter(|i| *i > 0)
        .map(|i| name[i..].to_lowercase())
        .unwrap_or_default()
}

pub fn prepare(store: &Store, profile: &str, request: &Value) -> Result<Prepared> {
    let target = text(request, "target")?;
    if !matches!(target, "calibre" | "calibre-web" | "immich") {
        return Err(Error(
            "import supports calibre, calibre-web and immich; use layout presets for other targets"
                .into(),
        ));
    }
    let limit = request
        .get("limit")
        .map(Value::as_u64)
        .unwrap_or(Some(1000));
    let timeout = request
        .get("timeout")
        .map(Value::as_f64)
        .unwrap_or(Some(300.));
    if limit.is_none_or(|v| !(1..=10000).contains(&v))
        || timeout.is_none_or(|v| !v.is_finite() || !(1.0..=3600.0).contains(&v))
    {
        return Err(Error(
            "import limit must be 1–10000 and timeout 1–3600 seconds".into(),
        ));
    }
    let destination = text(request, "destination")?;
    let destination = if target == "immich" {
        crate::exports::http_base(Some(destination))?
            .trim_end_matches('/')
            .to_owned()
    } else {
        crate::export_bundle::destination(store, profile, "import", Path::new(destination), true)?
            .0
            .to_str()
            .ok_or_else(|| Error("paths must be valid UTF-8".into()))?
            .to_owned()
    };
    let content = &request["document"]["content"];
    let items = array(content, "items")?
        .iter()
        .map(|v| Ok((text(v, "id")?, v)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let files = array(content, "files")?
        .iter()
        .map(|v| Ok((text(v, "id")?, v)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut positions: BTreeMap<&str, usize> = BTreeMap::new();
    let mut decisions: Vec<Decision> = vec![];
    for association in array(content, "associations")? {
        let item = *items
            .get(text(association, "item_id")?)
            .ok_or_else(|| Error("unknown import item".into()))?;
        let kind = text(item, "kind")?;
        if association["role"] != "primary"
            || !(if target == "immich" {
                matches!(kind, "photo" | "video")
            } else {
                matches!(kind, "book" | "book_edition")
            })
        {
            continue;
        }
        let file = *files
            .get(text(association, "file_id")?)
            .ok_or_else(|| Error("unknown import file".into()))?;
        let id = text(file, "id")?;
        let decision = Decision {
            file: file.clone(),
            item: item.clone(),
            snapshot: Value::Null,
        };
        if let Some(&position) = positions.get(id) {
            if decisions[position].item["id"] != item["id"] {
                return Err(Error(
                    "one import file has multiple media identities; select a smaller catalog"
                        .into(),
                ));
            }
            decisions[position] = decision;
        } else {
            positions.insert(id, decisions.len());
            decisions.push(decision);
        }
    }
    if decisions.len() as u64 > limit.unwrap() {
        return Err(Error(
            "import exceeds limit; select a smaller query catalog or raise --limit explicitly"
                .into(),
        ));
    }
    let mut actions = vec![];
    for decision in &mut decisions {
        let mut snapshot =
            crate::media_mutation::snapshot(store, profile, text(&decision.file, "id")?)?;
        if ["location", "path", "status", "size", "mtime_ns"]
            .iter()
            .any(|key| {
                crate::exports::python_text(&snapshot[key])
                    != crate::exports::python_text(&decision.file[key])
            })
        {
            return Err(Error(
                "import source changed or disagrees with inventory; rescan before importing".into(),
            ));
        }
        let revision = crate::source::validate(&snapshot)?;
        snapshot["ctime_ns"] = revision["ctime_ns"].clone();
        actions.push(json!({"file_id":decision.file["id"],"item_id":decision.item["id"],"source":Path::new(text(&snapshot,"root")?).join(text(&snapshot,"path")?),"size":decision.file["size"]}));
        decision.snapshot = snapshot;
    }
    let mut groups: Vec<Vec<Decision>> = vec![];
    let mut positions = BTreeMap::new();
    for decision in decisions {
        let key = text(
            if target == "immich" {
                &decision.file
            } else {
                &decision.item
            },
            "id",
        )?
        .to_owned();
        let position = *positions.entry(key).or_insert_with(|| {
            groups.push(vec![]);
            groups.len() - 1
        });
        let extension = suffix(text(&decision.file, "path")?);
        if groups[position]
            .iter()
            .any(|prior| suffix(prior.file["path"].as_str().unwrap()) == extension)
        {
            return Err(Error(
                "multiple copies of one book format; select one file per format".into(),
            ));
        }
        groups[position].push(decision);
    }
    let executable = if target == "immich" {
        "immich"
    } else {
        "calibredb"
    };
    let report = json!({"target":target,"destination":destination,"executable":executable,"actions":actions,"applied":false,"completed":[],"complete":true,"copies_media":true,"outcome":"not_started","attempts":[]});
    Ok(Prepared {
        report,
        groups,
        executable,
        destination,
        database_id: text(content, "database_id")?.into(),
        timeout: timeout.unwrap(),
    })
}
/// Copies only a pinned source descriptor. Post-read identity checks happen
/// before the official application may see the private copy.
pub fn stage(decision: &Decision, path: &Path, poll: &mut dyn FnMut() -> Result<()>) -> Result<()> {
    crate::source::with_validated_source(&decision.snapshot, |source| {
        use std::io::{Read, Write};
        let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            poll()?;
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count])?;
        }
        poll()?;
        output.sync_all()?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn suffix_matches_python_314_leading_and_trailing_dots() {
        for (path, expected) in [
            ("..book", ""),
            ("...book", ""),
            ("...", ""),
            (".book.EPUB", ".epub"),
            ("book.", "."),
            ("book.PDF", ".pdf"),
            ("folder/.book", ""),
        ] {
            assert_eq!(super::suffix(path), expected, "{path}");
        }
    }
}
