// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Preserved ordinary projection entrypoint. Publication remains unavailable.
use catabolic_store::{Error, Result, Store};
use serde_json::Value;
use std::path::Path;

pub const EXECUTION_GATE: &str = "native projection execute is blocked: filesystem publication and recovery are not yet qualified";

pub fn run(database: &Path, profile: &str, apply: bool, args: &Value) -> Result<Value> {
    // Intentionally earlier than frozen execution admission: no dependent
    // metadata staging or output planning can precede this publication refusal.
    // No debug/private request option can relax it.
    if apply {
        return Err(Error(EXECUTION_GATE.into()));
    }
    let store = Store::open(database, true, false)?;
    // Frozen CLI constructs Application before calling Projections.run, so
    // profile admission precedes run's option validation.
    catabolic_store::query::CatalogQuery::new(&store, profile)?;
    catabolic_store::projection_run::preview(&store, profile, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn execution_refuses_before_database_admission_even_with_private_options() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("absent.db");
        for args in [
            json!({}),
            json!({"catalog":"missing","limit":0,"disposable_fixture":true}),
        ] {
            assert_eq!(
                run(&database, "default", true, &args).unwrap_err().0,
                EXECUTION_GATE
            );
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
