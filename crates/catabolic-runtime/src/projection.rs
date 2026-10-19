// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Ordinary projection preview and execute. Execute publishes through the
//! shared journal executor; fallback bindings remain gated.
use catabolic_store::{Result, Store};
use serde_json::Value;
use std::path::Path;

pub fn run(database: &Path, profile: &str, apply: bool, args: &Value) -> Result<Value> {
    catabolic_store::filesystem_mutation::admit_failpoints(args)?;
    let store = Store::open(database, true, false)?;
    // Frozen CLI constructs Application before calling Projections.run, so
    // profile admission precedes run's option validation.
    catabolic_store::query::CatalogQuery::new(&store, profile)?;
    if apply {
        catabolic_store::projection_run::execute(&store, profile, args)
    } else {
        catabolic_store::projection_run::preview(&store, profile, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn execution_of_an_absent_catalog_creates_nothing() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("absent.db");
        for args in [
            json!({}),
            json!({"catalog":"missing","limit":0,"disposable_fixture":true}),
        ] {
            assert!(run(&database, "default", true, &args).is_err());
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
