// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Public fallback orchestration over a persistent native embedding owner.
use catabolic_store::{
    Error, Result, embedding::Session, fallback_projection as projection, public_fallback as public,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub fn call(session: &mut Session, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    session.store()?;
    match operation {
        "binding" => public::binding(session.store()?, profile, public::text(args, "catalog")?),
        "resolver-options" => public::resolver_options(session.store()?, profile, args),
        "resolver-health" => public::health(session.store()?, profile, args),
        "resolver-resolve" => {
            let limits = public::resolver_options(session.store()?, profile, args)?;
            session.with_evaluation(
                profile,
                args,
                limits["query_timeout_ms"].as_u64().unwrap(),
                limits["max_candidates"].as_u64().unwrap() as usize,
                |evaluation| public::resolve(evaluation, args, &mut crate::fallback::probe),
            )
        }
        "resolver-capture" => session.with_evaluation(profile, args, 60000, 100000, |evaluation| {
            public::capture(evaluation, args, &mut crate::fallback::probe)
        }),
        "projection-bind" => {
            let catalog = public::text(args, "catalog")?;
            let _guard = public::serialization_lock(session.store()?)?;
            catabolic_store::definitions::require_recovered(session.store()?)?;
            catabolic_store::fallback_mutation::get(
                session.store()?,
                profile,
                public::text(args, "policy_id")?,
            )?;
            catabolic_store::definitions::layout_get(
                session.store()?,
                public::text(args, "layout")?,
            )?;
            let query = catabolic_store::definitions::query_get(
                session.store()?,
                profile,
                public::text(args, "query_id")?,
            )?["definition"]
                .clone();
            session.with_evaluation(
                profile,
                args,
                query["timeout_ms"].as_u64().unwrap_or(5000),
                query["max_ids"].as_u64().unwrap_or(10000) as usize,
                |evaluation| {
                    projection::bind(
                        evaluation,
                        catalog,
                        public::text(args, "policy_id")?,
                        public::text(args, "query_id")?,
                        public::text(args, "layout")?,
                    )
                },
            )
        }
        "projection-plan" => {
            let plan = prepare(session, profile, args)?;
            session.fallback_plans().remember(profile, plan)
        }
        "projection-run" => {
            // Public callers never supply the private fixture admission bit.
            // Refuse before planning, claiming probes or touching managed output.
            if args["apply"] == true {
                catabolic_store::filesystem_mutation::require_fallback_fixture(&json!({}))?;
            }
            let _guard = public::serialization_lock(session.store()?)?;
            Ok(prepare(session, profile, args)?.present())
        }
        "projection-apply" => {
            let prepared = session.fallback_plans().checked(profile, args)?;
            let guard = catabolic_store::filesystem_mutation::publication_lock(session.store()?)?;
            let options = json!({"expected_plan":prepared.digest(),"max_removals":args["max_removals"],"max_changes":args["max_changes"]});
            crate::fallback::apply_prepared(
                session.store_mut()?,
                profile,
                prepared,
                &options,
                &guard,
            )
        }
        "projection-drop" => {
            if let Some(token) = args["plan_token"].as_u64() {
                session.fallback_plans().forget(token)?;
            }
            Ok(Value::Null)
        }
        "worker-configure-validate" => {
            public::configure_validate(session.store()?, profile, args)?;
            Ok(Value::Null)
        }
        "worker-configure" => public::configure(session.store()?, profile, args),
        _ => Err(Error(format!(
            "unsupported native fallback operation: {operation}"
        ))),
    }
}

/// Preserved path-based worker lifecycle. The singleton spans every tick, but
/// each tick releases its catalog writer before the idle interval. Publication
/// and recovery still use the unchanged public tick's fail-closed M6 gates.
pub fn run(
    database: &Path,
    profile: &str,
    once: bool,
    interval: Duration,
    cancelled: &AtomicBool,
) -> Result<Value> {
    let path = catabolic_store::database_path(database)?;
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".fallback-worker");
    // Contention here is terminal. Only tick-local catalog contention is a
    // retryable report, matching the frozen worker's admission order.
    let _worker = catabolic_store::writer_lock(Path::new(&lock_path))?;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error("fallback worker cancelled".into()));
        }
        let result = match tick(&path, profile, &json!({}), cancelled) {
            // Store errors currently retain text rather than SQLite codes.
            // Match only exact known contention diagnostics, not substrings.
            Err(error)
                if matches!(
                    error.0.as_str(),
                    "another Catabolic writer or upgrade is using this database"
                        | "database is locked"
                        | "database is busy"
                        | "database table is locked"
                ) =>
            {
                json!({"processed":0,"complete":false,"blockers":["catalog_busy"]})
            }
            other => other?,
        };
        if cancelled.load(Ordering::Acquire) {
            return Err(Error("fallback worker cancelled".into()));
        }
        if once {
            return Ok(result);
        }
        // The frozen perpetual worker emits no per-tick stdout, including
        // incomplete ticks. Its only public interval is the one-second default.
        let start = Instant::now();
        while start.elapsed() < interval {
            if cancelled.load(Ordering::Acquire) {
                return Err(Error("fallback worker cancelled".into()));
            }
            std::thread::sleep(
                Duration::from_millis(20).min(interval.saturating_sub(start.elapsed())),
            );
        }
    }
}
fn prepare(
    session: &mut Session,
    profile: &str,
    args: &Value,
) -> Result<projection::PreparedProjection> {
    let catalog = public::text(args, "catalog")?;
    let config = projection::binding(session.store()?, profile, catalog)?
        .ok_or_else(|| Error("projection has no fallback binding".into()))?;
    let policy = catabolic_store::fallback_mutation::get(
        session.store()?,
        profile,
        config["policy_id"].as_str().unwrap(),
    )?["definition"]
        .clone();
    session.with_evaluation(
        profile,
        args,
        policy["budgets"]["query_timeout_ms"].as_u64().unwrap(),
        policy["budgets"]["max_candidates"].as_u64().unwrap() as usize,
        |evaluation| {
            projection::plan(
                evaluation,
                catalog,
                args["manual_failback"] == true,
                &mut crate::fallback::probe,
            )
        },
    )
}

/// The frozen worker's entry point is genuinely path based. It owns one writer
/// for the whole bounded tick; each ordinary facade invocation lends that owner.
pub fn tick(database: &Path, profile: &str, args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let limit = args
        .get("limit")
        .map_or(Some(10), Value::as_u64)
        .filter(|n| *n <= 1000)
        .ok_or_else(|| Error("native fallback worker limit must be between 0 and 1000".into()))?;
    let mut session = Session::open(database, true, false)?;
    catabolic_store::mutation::profile(session.store()?, profile)?;
    let rows = catabolic_store::migration::rows(
        &session.store()?.db,
        "SELECT * FROM fallback_bindings WHERE profile=? AND enabled=1 AND next_attempt<=? ORDER BY next_attempt,catalog LIMIT ?",
        &[
            profile.to_owned().into(),
            catabolic_store::notifications::now().into(),
            (limit as i64).into(),
        ],
    )?;
    let mut results = Vec::new();
    for config in rows {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error("fallback worker cancelled".into()));
        }
        let catalog = public::text(&config, "catalog")?;
        if session.store()?.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM watcher_owners WHERE profile=? AND catalog=?)",
            [profile, catalog],
            |row| row.get::<_, bool>(0),
        )? {
            continue;
        }
        let options = json!({"catalog":catalog,"apply":true,"automatic":true,"max_changes":config["max_changes"],"max_removals":config["max_removals"]});
        let result = (|| -> Result<Value> {
            if session.store()?.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM journal WHERE profile=? AND catalog=?)",
                [profile, catalog],
                |row| row.get::<_, bool>(0),
            )? {
                catabolic_store::filesystem_mutation::execute_mut(
                    session.store_mut()?,
                    profile,
                    "recover",
                    &options,
                    &mut crate::fallback::probe,
                )?;
            }
            call(&mut session, profile, "projection-run", &options)
        })();
        results.push(match result {
            Ok(result)=>json!({"catalog":catalog,"complete":result["complete"]==true,"states":result["desired"].as_array().unwrap_or(&Vec::new()).iter().map(|d|d["state"].clone()).collect::<Vec<_>>()}),
            Err(error)=>json!({"catalog":catalog,"complete":false,"error":error.0}),
        });
        let next = catabolic_store::notifications::now()
            + config["interval_seconds"]
                .as_f64()
                .ok_or_else(|| Error("invalid fallback worker interval".into()))?;
        session.store()?.transaction(|db| {
            db.execute(
                "UPDATE fallback_bindings SET next_attempt=? WHERE profile=? AND catalog=?",
                [
                    catabolic_store::sql::scalar(&json!(next))?,
                    catabolic_store::sql::scalar(&json!(profile))?,
                    catabolic_store::sql::scalar(&json!(catalog))?,
                ],
            )?;
            Ok(())
        })?;
    }
    Ok(
        json!({"processed":results.len(),"complete":results.iter().all(|r|r["complete"]==true),"projections":results}),
    )
}
