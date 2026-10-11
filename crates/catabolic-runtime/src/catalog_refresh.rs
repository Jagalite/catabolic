// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Durable opt-in refresh draining. Producer completion and publication health
//! are separate outcomes; late acknowledgment cannot clear newer queue work.
use catabolic_store::{Error, Result, Store};
use serde_json::{Value, json};

fn limit(request: &Value) -> Result<usize> {
    let limit = request
        .get("limit")
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| Error("limit must be 1..1000".into()))
        })
        .transpose()?
        .unwrap_or(100);
    if !(1..=1000).contains(&limit) {
        return Err(Error("limit must be 1..1000".into()));
    }
    Ok(limit as usize)
}
pub fn execute(store: &mut Store, profile: &str, action: &str, request: &Value) -> Result<Value> {
    let limit = limit(request)?;
    match action {
        "pending" => catabolic_store::catalog_refresh::pending(store, profile, limit),
        "run" => run_with_options(
            store,
            profile,
            request["catalog"].as_str(),
            limit,
            request["force"] == true,
            request,
        ),
        "configure" => {
            let mut result = catabolic_store::catalog_refresh::configure(store, profile, request)?;
            if result["enabled"] == true {
                result["catalog_refresh"] = run_with_options(
                    store,
                    profile,
                    request["catalog"].as_str(),
                    100,
                    false,
                    request,
                )?;
            }
            Ok(result)
        }
        _ => Err(Error("unknown catalog refresh action".into())),
    }
}
/// Compatibility adapter on an already-owned Store. The companion API above
/// remains strict; only the Python boundary supplies int() coercion results.
pub fn execute_public(
    store: &mut Store,
    profile: &str,
    action: &str,
    request: &Value,
) -> Result<Value> {
    match action {
        "configure_prevalidate" => {
            catabolic_store::catalog_refresh::validate_configuration_preconditions(store, request)?;
            Ok(Value::Null)
        }
        "configure_validate" => {
            catabolic_store::catalog_refresh::validate_configuration(store, profile, request)?;
            Ok(Value::Null)
        }
        "configure" => {
            let stored_enabled = request["_enabled_storage"]
                .as_i64()
                .ok_or_else(|| Error("Python int too large to convert to SQLite INTEGER".into()))?;
            let mut result = catabolic_store::catalog_refresh::configure_integer(
                store,
                profile,
                request,
                stored_enabled,
            )?;
            if request["enabled"] == true {
                result["catalog_refresh"] =
                    run(store, profile, request["catalog"].as_str(), 100, false)?;
            }
            Ok(result)
        }
        "run_validate" => {
            public_limit(request)?;
            require_writer(store)?;
            Ok(Value::Null)
        }
        "run" => {
            let force = request["force"]
                .as_i64()
                .ok_or_else(|| Error("Python int too large to convert to SQLite INTEGER".into()))?
                != 0;
            run(
                store,
                profile,
                request["catalog"].as_str(),
                public_limit(request)?,
                force,
            )
        }
        "pending" => {
            catabolic_store::catalog_refresh::pending(store, profile, public_limit(request)?)
        }
        _ => Err(Error("unknown public catalog refresh action".into())),
    }
}
fn public_limit(request: &Value) -> Result<usize> {
    limit(request).map_err(|_| Error("limit must be between 1 and 1000".into()))
}
fn require_writer(store: &Store) -> Result<()> {
    if !store.owns_writer_lock() || !store.db.is_autocommit() {
        return Err(Error(
            "catalog refresh requires a writer outside an active transaction".into(),
        ));
    }
    Ok(())
}
pub fn finish(store: &mut Store, profile: &str, mut result: Value) -> Result<Value> {
    match run(store, profile, None, 100, false) {
        Ok(report) if report["processed"] != 0 || report["pending"] != 0 => {
            result["catalog_refresh"] = report
        }
        Ok(_) => {}
        Err(error) => {
            result["catalog_refresh"] = json!({"complete":false,"errors":[{"error":error.0.chars().take(4000).collect::<String>()}]})
        }
    }
    Ok(result)
}
pub fn run(
    store: &mut Store,
    profile: &str,
    catalog: Option<&str>,
    limit: usize,
    force: bool,
) -> Result<Value> {
    run_with_options(store, profile, catalog, limit, force, &json!({}))
}
fn run_with_options(
    store: &mut Store,
    profile: &str,
    catalog: Option<&str>,
    limit: usize,
    force: bool,
    request: &Value,
) -> Result<Value> {
    if !(1..=1000).contains(&limit) {
        return Err(Error("limit must be 1..1000".into()));
    }
    require_writer(store)?;
    let selected =
        catabolic_store::catalog_refresh::selected(store, profile, catalog, limit, force)?;
    let mut refreshed = vec![];
    let mut errors = vec![];
    let mut publications = json!([]);
    for row in &selected {
        let catalog = row["catalog"]
            .as_str()
            .ok_or_else(|| Error("invalid refresh catalog".into()))?;
        let attempt = (|| -> Result<bool> {
            // Target decoding belongs to this row's retry boundary. One bad
            // layout must not abort later rows or lose the failed row's backoff.
            let target = catabolic_store::catalog_refresh::target(store, profile, catalog)?;
            if target.watcher_owned {
                return Ok(false);
            }
            let mut options = json!({"catalog":catalog,"max_removals":row["max_removals"],"notify_consumers":false});
            if cfg!(debug_assertions) && request["disposable_fixture"] == true {
                options["disposable_fixture"] = json!(true);
            }
            if target.pending_journal {
                catabolic_store::filesystem_mutation::execute_mut(
                    store,
                    profile,
                    "recover",
                    &options,
                    &mut crate::fallback::probe,
                )?;
            }
            catabolic_store::mutation::require_recovered(&store.db)?;
            let synced = if let Some(fallback) = &target.fallback {
                options["apply"] = json!(true);
                options["automatic"] = json!(true);
                // Frozen FallbackProjection constructs its own default
                // reconciler; only layout refresh/recovery suppress legacy
                // notifications through CatalogRefresh's reconciler.
                options["notify_consumers"] = json!(true);
                options["max_changes"] = fallback["max_changes"].clone();
                let result = crate::fallback::projection_store(store, profile, "run", &options)?;
                if !result["publication"].is_null() {
                    publications
                        .as_array_mut()
                        .unwrap()
                        .push(result["publication"].clone());
                }
                json!({"safe":result["safe"],"healthy":result["complete"]})
            } else {
                let layout = target
                    .layout
                    .as_deref()
                    .ok_or_else(|| Error("catalog no longer has a saved layout".into()))?;
                let prepared = catabolic_store::layout_mutation::prepare(
                    store, profile, layout, catalog, false, false,
                )?;
                store.transaction(|_| {
                    let report =
                        catabolic_store::layout_mutation::apply(store, profile, &prepared, limit)?;
                    if report["safe"] != true {
                        return Err(Error(
                            "saved layout has blockers; inspect layout preview".into(),
                        ));
                    }
                    let plan = catabolic_store::filesystem_mutation::execute(
                        store, profile, "preview", &options,
                    )?;
                    if plan["safe"] != true
                        || plan["actions"]
                            .as_array()
                            .is_some_and(|rows| rows.iter().any(|a| a["kind"] == "blocked_source"))
                    {
                        return Err(Error(
                        "link plan has unavailable sources, collisions or removal-budget blockers"
                            .into(),
                    ));
                    }
                    Ok(())
                })?;
                catabolic_store::filesystem_mutation::execute(store, profile, "apply", &options)?
            };
            if synced["safe"] != true || synced["healthy"] != true {
                return Err(Error(
                    "link synchronization or verification is incomplete".into(),
                ));
            }
            if !synced["consumers"].is_null() {
                publications
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"consumers":synced["consumers"]}));
            }
            catabolic_store::catalog_refresh::acknowledge(store, profile, row)?;
            Ok(true)
        })();
        match attempt {
            Ok(true) => refreshed.push(catalog.to_string()),
            Ok(false) => {}
            Err(error) => {
                let message = error.0.chars().take(4000).collect::<String>();
                catabolic_store::catalog_refresh::retry(store, profile, row, &message)?;
                errors.push(json!({"catalog":catalog,"error":message}));
            }
        }
    }
    // Match one command-close automatic consumer drain after all publication
    // stages, preserving the frozen refresh report instead of adding markers.
    crate::consumers::after_publication(store, profile, &mut publications)?;
    let pending = catabolic_store::catalog_refresh::pending_count(store, profile, catalog)?;
    Ok(
        json!({"processed":selected.len(),"refreshed":refreshed,"errors":errors,"pending":pending,"complete":pending==0}),
    )
}
