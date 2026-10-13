// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Rollback-only ordinary projection orchestration over the existing owners.
//! This does not expose publication, fallback probing, or private fixture gates.
use crate::{
    Error, Result, Store, definitions,
    embedding::poll_interrupt,
    fallback_projection,
    layout_mutation::{self, PreparedLayout},
    public_reconcile,
};
use serde_json::{Value, json};

pub const FALLBACK_GATE: &str = "native projection preview is blocked for fallback bindings: mutable fallback probing and publication are not yet qualified";

enum Rollback {
    Report(Value),
    Failure(Error),
}
impl From<Error> for Rollback {
    fn from(error: Error) -> Self {
        Self::Failure(error)
    }
}
impl std::fmt::Display for Rollback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Report(_) => f.write_str("projection preview rollback"),
            Self::Failure(error) => error.fmt(f),
        }
    }
}
fn cancelled() -> Result<()> {
    if poll_interrupt() {
        Err(Error("evaluation_cancelled".into()))
    } else {
        Ok(())
    }
}
struct Interrupt<'a>(&'a rusqlite::Connection);
impl<'a> Interrupt<'a> {
    fn new(store: &'a Store) -> Result<Self> {
        store.db.progress_handler(1000, Some(poll_interrupt))?;
        Ok(Self(&store.db))
    }
}
impl Drop for Interrupt<'_> {
    fn drop(&mut self) {
        // Drop inside the transaction closure, before its rollback (also on
        // unwind). A latched cancellation must never preempt cleanup.
        let _ = self.0.progress_handler(0, None::<fn() -> bool>);
    }
}

pub fn preview(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    let limit = args
        .get("limit")
        .unwrap_or(&json!(100))
        .as_u64()
        .filter(|n| (1..=1000).contains(n))
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))? as usize;
    let catalog = args["catalog"]
        .as_str()
        .ok_or_else(|| Error("catalog must be text".into()))?;
    cancelled()?;
    let _interrupt = Interrupt::new(store)?;
    if fallback_projection::binding(store, profile, catalog)?.is_some() {
        return Err(Error(FALLBACK_GATE.into()));
    }
    if !store.owns_writer_lock() || !store.db.is_autocommit() {
        return Err(Error(
            "projection planning requires the writer lock outside a transaction".into(),
        ));
    }
    let config = definitions::projection_get(store, profile, catalog)?;
    let layout = config["layout"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error("projection has no saved layout".into()))?;
    let prepared = layout_mutation::prepare(
        store,
        profile,
        layout,
        catalog,
        args["replace_layout"] == true,
        args["allow_empty"] == true,
    )?;
    cancelled()?;
    // Evaluation owns a separate bounded snapshot. Apply checks its sealed
    // identity and epoch again before writes on this writer-owned connection.
    preview_prepared(store, profile, catalog, args, limit, &prepared)
}

fn preview_prepared(
    store: &Store,
    profile: &str,
    catalog: &str,
    args: &Value,
    limit: usize,
    prepared: &PreparedLayout,
) -> Result<Value> {
    let outcome: std::result::Result<(), Rollback> = store.transaction_detailed(|_| {
        let _interrupt = Interrupt::new(store)?;
        cancelled()?;
        let mut layout = layout_mutation::apply(store, profile, prepared, limit)?;
        cancelled()?;
        // A report never leaves this owner until the entire staging scope has
        // rolled back. Store converts cleanup failure into Failure, not Report.
        layout["applied"] = json!(false);
        let mut report = json!({"projection":catalog,"layout":layout,
            "complete":false,"safe":false,"applied":false});
        if report["layout"]["safe"] == true {
            let mut plan = public_reconcile::call(
                store,
                profile,
                "preview",
                &json!({
                    "catalog":catalog,"max_removals":args.get("max_removals").unwrap_or(&json!(0))
                }),
            )?;
            cancelled()?;
            let actions = plan["actions"]
                .as_array()
                .ok_or_else(|| Error("projection reconciliation returned no action list".into()))?;
            let blocked: Vec<Value> = actions
                .iter()
                .filter(|a| a["kind"] == "blocked_source")
                .cloned()
                .collect();
            let count = actions.len();
            let bounded: Vec<Value> = actions.iter().take(limit).cloned().collect();
            report["safe"] = json!(plan["safe"] == true && blocked.is_empty());
            report["complete"] = report["safe"].clone();
            plan["actions"] = json!(bounded);
            plan["action_count"] = json!(count);
            plan["actions_truncated"] = json!(count > limit);
            plan["blocked_sources_count"] = json!(blocked.len());
            plan["blocked_sources"] = json!(blocked.into_iter().take(limit).collect::<Vec<_>>());
            // Frozen Python emits an integer zero for the empty denominator.
            if plan["removal_budget"]["owned_entries"] == 0 {
                plan["removal_budget"]["percent"] = json!(0);
            }
            report["reconciliation"] = plan;
        }
        cancelled()?;
        Err(Rollback::Report(report))
    });
    match outcome {
        Err(Rollback::Report(report)) => Ok(report),
        Err(Rollback::Failure(error)) => Err(error),
        Ok(()) => Err(Error("projection preview unexpectedly committed".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store.transaction(|db| {
            db.execute_batch("INSERT INTO locations(id) VALUES ('media'); INSERT INTO files(id,location,path) VALUES ('file','media','book.txt'); INSERT INTO items(id,kind,metadata) VALUES ('book','book','{}'); INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('association','file','book','primary','explicit'); INSERT INTO meta(key,value) VALUES ('layout-catalog:global','{\"layout\":\"flat\",\"mapping_ids\":[]}');")?;
            Ok(())
        }).unwrap();
        definitions::dispatch(&store, "default", "layout-put", &json!({"name":"flat","value":{"version":1,"rules":[{"name":"all","path":"{item.id}/{file.name}"}]}})).unwrap();
        (root, store)
    }
    fn state(store: &Store) -> Value {
        let names: Vec<String> = store.db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap().query_map([], |row| row.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
        let mut state = serde_json::Map::new();
        for name in names {
            state.insert(
                name.clone(),
                json!(
                    crate::migration::rows(
                        &store.db,
                        &format!(
                            "SELECT * FROM \"{}\" ORDER BY rowid",
                            name.replace('"', "\"\"")
                        ),
                        &[]
                    )
                    .unwrap_or_else(|_| crate::migration::rows(
                        &store.db,
                        &format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")),
                        &[]
                    )
                    .unwrap())
                ),
            );
        }
        Value::Object(state)
    }
    fn staged_callback(store: &Store) -> Arc<AtomicBool> {
        let staged = Arc::new(AtomicBool::new(false));
        let signal = staged.clone();
        store
            .db
            .create_scalar_function(
                "projection_staged",
                0,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                move |_| {
                    signal.store(true, Ordering::Release);
                    Ok(0)
                },
            )
            .unwrap();
        store.db.execute_batch("CREATE TEMP TRIGGER projection_stage AFTER UPDATE ON meta WHEN NEW.key='layout-catalog:global' BEGIN SELECT projection_staged(); END;").unwrap();
        staged
    }
    #[test]
    fn preview_rolls_back_all_tables_and_reuses_writer() {
        let (root, store) = fixture();
        let before = state(&store);
        for limit in [1, 100] {
            let report = preview(
                &store,
                "default",
                &json!({"catalog":"global","limit":limit}),
            )
            .unwrap();
            assert_eq!(report["layout"]["desired_count"], 1);
            assert_eq!(report["layout"]["applied"], false);
            assert_eq!(report["applied"], false);
            assert_eq!(report["safe"], false); // no output binding in this fixture
            assert_eq!(before, state(&store));
            assert!(store.db.is_autocommit());
        }
        assert!(!root.path().join("catalog.db.publication.lock").exists());
    }
    #[test]
    fn stale_and_tampered_preparation_never_stages() {
        let (_root, store) = fixture();
        let prepared =
            layout_mutation::prepare(&store, "default", "flat", "global", false, false).unwrap();
        store
            .transaction(|db| {
                db.execute(
                    "UPDATE items SET metadata='{\"title\":\"new\"}' WHERE id='book'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let before = state(&store);
        let error =
            preview_prepared(&store, "default", "global", &json!({}), 100, &prepared).unwrap_err();
        assert!(error.0.contains("stale layout plan"));
        assert_eq!(before, state(&store));
        let mut prepared =
            layout_mutation::prepare(&store, "default", "flat", "global", false, false).unwrap();
        prepared.report["safe"] = json!(false);
        assert!(
            preview_prepared(&store, "default", "global", &json!({}), 100, &prepared)
                .unwrap_err()
                .0
                .contains("stale layout plan")
        );
        assert_eq!(before, state(&store));
    }
    #[test]
    fn late_cancellation_and_callback_unwind_rollback_and_clear_handler() {
        for panic in [false, true] {
            let (_root, store) = fixture();
            let staged = staged_callback(&store);
            let before = state(&store);
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::embedding::with_interrupt_check(
                    move || {
                        if staged.load(Ordering::Acquire) {
                            assert!(!panic, "projection callback unwind");
                            return true;
                        }
                        false
                    },
                    || preview(&store, "default", &json!({"catalog":"global"})),
                )
            }));
            if panic {
                assert!(outcome.is_err());
            } else {
                assert!(
                    outcome
                        .unwrap()
                        .unwrap_err()
                        .0
                        .contains("evaluation_cancelled")
                );
            }
            assert!(store.db.is_autocommit());
            assert_eq!(before, state(&store));
            // A new callback would interrupt this long raw query if the old
            // progress handler survived the rollback-only scope.
            crate::embedding::with_interrupt_check(|| true, || {
                let total:i64=store.db.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<3000) SELECT sum(x) FROM n",[],|row|row.get(0))?;
                assert_eq!(total,4501500); Ok(())
            }).unwrap();
            assert!(preview(&store, "default", &json!({"catalog":"global"})).is_ok());
        }
    }
    #[test]
    fn sqlite_interrupt_after_mapping_insert_rolls_back_before_reuse() {
        let (_root, store) = fixture();
        let staged = staged_callback(&store);
        let observed = staged.clone();
        store.db.execute_batch("CREATE TEMP TRIGGER projection_mid_insert AFTER INSERT ON mappings BEGIN SELECT projection_staged(); SELECT (WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<3000) SELECT sum(x) FROM n); END;").unwrap();
        let before = state(&store);
        let error = crate::embedding::with_interrupt_check(
            move || staged.load(Ordering::Acquire),
            || preview(&store, "default", &json!({"catalog":"global"})),
        )
        .unwrap_err();
        assert!(observed.load(Ordering::Acquire));
        assert!(error.0.contains("interrupted"), "{error}");
        assert!(store.db.is_autocommit());
        assert_eq!(before, state(&store));
        assert!(preview(&store, "default", &json!({"catalog":"global"})).is_ok());
        assert_eq!(before, state(&store));
    }

    #[test]
    fn write_error_and_budget_error_restore_every_staged_table() {
        let (_root, store) = fixture();
        let before = state(&store);
        assert_eq!(
            preview(
                &store,
                "default",
                &json!({"catalog":"global","max_removals":-1})
            )
            .unwrap_err()
            .0,
            "max removals must be a nonnegative integer"
        );
        assert_eq!(before, state(&store));
        store.db.execute_batch("CREATE TEMP TRIGGER projection_failure BEFORE INSERT ON meta WHEN NEW.key LIKE 'statistics:%' BEGIN SELECT RAISE(ABORT,'injected staging failure'); END;").unwrap();
        assert!(
            preview(&store, "default", &json!({"catalog":"global"}))
                .unwrap_err()
                .0
                .contains("injected staging failure")
        );
        assert_eq!(before, state(&store));
        assert!(store.db.is_autocommit());
    }
    #[test]
    fn admission_limit_writer_and_active_transaction() {
        let (_root, store) = fixture();
        assert_eq!(
            preview(&store, "default", &json!({"catalog":"missing","limit":0}))
                .unwrap_err()
                .0,
            "limit must be between 1 and 1000"
        );
        store.db.execute_batch("BEGIN").unwrap();
        let before = state(&store);
        assert!(
            preview(&store, "default", &json!({"catalog":"missing"}))
                .unwrap_err()
                .0
                .contains("writer lock outside a transaction")
        );
        assert_eq!(before, state(&store));
        assert!(!store.db.is_autocommit());
        store.db.execute_batch("ROLLBACK").unwrap();
        let reader = Store::open(&store.path, false, false).unwrap();
        assert!(
            preview(&reader, "default", &json!({"catalog":"global"}))
                .unwrap_err()
                .0
                .contains("writer lock outside a transaction")
        );
    }
}
