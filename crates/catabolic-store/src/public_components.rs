// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Persistent public Components adapters. Existing component owners retain all
//! indexing, identity and decoding decisions; no source files are opened here.
use crate::{Error, Result, Store, component_mutation, components, migration::rows};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub fn revision(snapshot: &Value) -> Result<String> {
    if !snapshot.is_object() {
        return Err(Error("component snapshot must be an object".into()));
    }
    Ok(components::revision(snapshot))
}

pub fn call(store: &Store, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    store.transaction_available()?;
    match operation {
        "index_file" => {
            if store.schema_version >= 25 {
                components::index_file(
                    &store.db,
                    profile,
                    args["file_id"].as_str().unwrap_or(""),
                    args["historical"].as_bool().unwrap_or(false),
                )?;
            }
            Ok(Value::Null)
        }
        "backfill" => {
            components::backfill(&store.db)?;
            Ok(Value::Null)
        }
        "get" => component_mutation::get(
            store,
            profile,
            args["id"].as_str().unwrap_or(""),
            args["require_current"].as_bool().unwrap_or(false),
        ),
        _ => Err(Error(format!(
            "unsupported native Components operation: {operation}"
        ))),
    }
}

/// The native public EvaluationSession has no foreign Access context. Charge
/// exactly the raw lookup rows, as the frozen session.rows contract does, before
/// the existing component decoder adds the live recorded revision check.
pub(crate) fn get_evaluated(
    evaluation: &mut crate::selection::Evaluation,
    profile: &str,
    identifier: &str,
    require_current: bool,
    epoch: i64,
) -> Result<Value> {
    if crate::embedding::poll_interrupt() {
        return Err(Error("evaluation_cancelled".into()));
    }
    let deadline = Instant::now()
        + Duration::from_millis(evaluation.remaining("query composition timed out")?);
    let records = {
        let store = evaluation.store();
        store.db.progress_handler(
            1000,
            Some(move || crate::embedding::poll_interrupt() || Instant::now() >= deadline),
        )?;
        struct Reset<'a>(&'a rusqlite::Connection);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                let _ = self.0.progress_handler(0, None::<fn() -> bool>);
            }
        }
        let _reset = Reset(&store.db);
        // occurrence_id is the primary-key identity. At most one row can be
        // expanded, within every supported native session's max_ids >= 1.
        rows(
            &store.db,
            &format!(
                "SELECT * FROM ({}) WHERE profile=? AND occurrence_id=? LIMIT 1",
                crate::query::view_sql("catalog_component_occurrences")
            ),
            &[profile.to_owned().into(), identifier.to_owned().into()],
        )
        .map_err(|error| {
            if error.0.contains("interrupted") {
                Error("evaluation_expansion_cancelled_or_timed_out".into())
            } else {
                error
            }
        })?
    };
    // The frozen session revalidates after fetching and before accounting. A
    // cancellation must not turn into a successful, budget-consuming result.
    let current_epoch: i64 = evaluation.store().db.query_row(
        "SELECT generation FROM fallback_epoch WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    if current_epoch != epoch {
        return Err(Error("stale_evaluation_snapshot".into()));
    }
    if crate::embedding::poll_interrupt() {
        return Err(Error("evaluation_cancelled".into()));
    }
    evaluation.remaining("query composition timed out")?;
    evaluation.account_value(&json!(records))?;
    let row = records
        .into_iter()
        .next()
        .ok_or_else(|| Error("unknown component occurrence in this profile".into()))?;
    component_mutation::decode_occurrence(evaluation.store(), profile, row, require_current)
}
