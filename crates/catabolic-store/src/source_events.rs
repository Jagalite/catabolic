// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Catalog snapshots and fenced admission for native filesystem hints.
//! Hints only invalidate full-source evidence. They never publish inventory.
use crate::{Error, Result, Store, migration::rows};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf};

pub const MAX_SOURCES: usize = 1000;

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub name: String,
    pub root: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub check_device: bool,
    pub check_inode: bool,
    pub exclusions: Vec<PathBuf>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub database_id: String,
    pub profile: String,
    pub fence: String,
    pub sources: Vec<Source>,
}

/// Read catalog configuration only; subscription and filesystem waits happen
/// after the runtime releases the catalog session.
pub fn snapshot(store: &Store, profile: &str) -> Result<Snapshot> {
    crate::mutation::profile(store, profile)?;
    let bindings = rows(
        &store.db,
        "SELECT * FROM bindings WHERE profile=? ORDER BY kind,owner LIMIT 2001",
        &[profile.to_string().into()],
    )?;
    if bindings.len() > 2000 {
        return Err(Error("native event binding budget exceeded".into()));
    }
    let generated = rows(
        &store.db,
        "SELECT location FROM generated_locations WHERE profile=? ORDER BY location LIMIT 1001",
        &[profile.to_string().into()],
    )?;
    if generated.len() > MAX_SOURCES {
        return Err(Error("native event source budget exceeded".into()));
    }
    let generated: BTreeSet<_> = generated
        .iter()
        .filter_map(|v| v["location"].as_str())
        .collect();
    let excluded: Vec<_> = bindings
        .iter()
        .filter(|v| v["kind"] != "source" || generated.contains(v["owner"].as_str().unwrap_or("")))
        .filter_map(|v| v["root"].as_str())
        .map(PathBuf::from)
        .collect();
    let mut sources = vec![];
    let mut pins = vec![];
    for binding in &bindings {
        let name = binding["owner"].as_str().unwrap_or("");
        if binding["kind"] != "source" || generated.contains(name) {
            continue;
        }
        if sources.len() == MAX_SOURCES {
            return Err(Error("native event source budget exceeded".into()));
        }
        let policy =
            crate::observation::execute(store, profile, "scan_policy", &json!({"source":name}))?;
        let trust = crate::observation::trust_policy(store, profile, name)?;
        let root = PathBuf::from(
            binding["root"]
                .as_str()
                .ok_or_else(|| Error("invalid native event source root".into()))?,
        );
        let mut exclusions = excluded.clone();
        for relative in policy["exclusions"].as_array().into_iter().flatten() {
            if let Some(relative) = relative.as_str() {
                exclusions.push(root.join(relative));
            }
        }
        exclusions.sort();
        exclusions.dedup();
        sources.push(Source {
            name: name.into(),
            root,
            device: binding["device"].as_i64().unwrap_or(0) as u64,
            inode: binding["inode"].as_i64().unwrap_or(0) as u64,
            check_device: trust["settings"]["device"] != "skip",
            check_inode: trust["settings"]["inode"] != "skip",
            exclusions,
        });
        pins.push(json!({"binding":binding,"policy":policy,"trust":trust}));
    }
    let fence = catabolic_core::sha256(catabolic_core::encode(
        &json!({"bindings":bindings,"generated":generated,"pins":pins}),
    ));
    Ok(Snapshot {
        database_id: store.database_id.clone(),
        profile: profile.into(),
        fence,
        sources,
    })
}

/// Acknowledgment is allowed only after this transaction returns. Changed
/// bindings/policies/catalog identity reject the entire old batch; the runtime
/// rebuilds subscriptions and invalidates the newly captured source set.
pub fn invalidate(store: &Store, expected: &Snapshot, names: &[String]) -> Result<Vec<Value>> {
    store.transaction(|_| {
        if store.database_id != expected.database_id
            || snapshot(store, &expected.profile)? != *expected
        {
            return Err(Error("stale_native_event_snapshot".into()));
        }
        let allowed: BTreeSet<_> = expected.sources.iter().map(|s| s.name.as_str()).collect();
        let mut result = vec![];
        for name in names.iter().collect::<BTreeSet<_>>() {
            if !allowed.contains(name.as_str()) {
                return Err(Error("unknown_native_event_source".into()));
            }
            result.push(crate::observation::execute(
                store,
                &expected.profile,
                "dirty",
                &json!({"source":name}),
            )?);
        }
        Ok(result)
    })
}
