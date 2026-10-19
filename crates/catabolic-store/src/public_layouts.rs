// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Ordinary Layouts adapters. Evaluation borrows the caller's live Store;
//! only the existing complete desired-mapping owner applies its sealed plan.
use crate::{
    Error, Result,
    embedding::{Session, poll_interrupt},
    layout::RunFlag,
    layout_mutation,
};
use serde_json::{Value, json};

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .ok_or_else(|| Error(format!("{key} must be text")))
}

pub fn call(session: &mut Session, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    if operation != "run" {
        let store = session.store()?;
        store.db.progress_handler(1000, Some(poll_interrupt))?;
        struct Reset<'a>(&'a rusqlite::Connection);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                let _ = self.0.progress_handler(0, None::<fn() -> bool>);
            }
        }
        let _reset = Reset(&store.db);
        return match operation {
            "get" | "list" | "put" => {
                crate::definitions::dispatch(store, profile, &format!("layout-{operation}"), args)
            }
            "effective_definition" => crate::layout::effective_definition(
                store,
                profile,
                text(args, "name")?,
                text(args, "catalog")?,
            ),
            "projection_digest" => {
                crate::plans::projection_digest(store, profile, text(args, "catalog")?)
                    .map(Value::String)
            }
            _ => Err(Error(format!(
                "unsupported native layout operation: {operation}"
            ))),
        };
    }
    run(session, profile, args, &mut |flag| {
        Ok(args[match flag {
            RunFlag::Apply => "apply",
            RunFlag::ReplaceLayout => "replace_layout",
            RunFlag::AllowEmpty => "allow_empty",
        }] == true)
    })
}

pub fn run(
    session: &mut Session,
    profile: &str,
    args: &Value,
    flag: &mut impl FnMut(RunFlag) -> Result<bool>,
) -> Result<Value> {
    // The frozen run contract rejects limit before looking up a definition,
    // checking catalog ownership, or beginning a write transaction.
    let limit = args["limit"]
        .as_u64()
        .filter(|n| (1..=1000).contains(n))
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))? as usize;
    let apply = flag(RunFlag::Apply)?;
    // Readonly Stores already hold a read snapshot. Frozen run validates and
    // plans before a safe application attempts its first write; blocked plans
    // return normally. Writable calls keep a private rollback scope.
    let scope = if apply && session.store()?.writable {
        Some(session.begin()?)
    } else {
        None
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let prepared = session.with_evaluation(profile, &json!({}), 60000, 100000, |evaluation| {
            layout_mutation::prepare_evaluated(
                evaluation,
                text(args, "name")?,
                text(args, "catalog")?,
                flag,
            )
        });
        // Evaluation removes its SQLite handler, including on ordinary errors.
        // Restore the embedding interrupt bridge before mutation or commit.
        let store = session.store()?;
        store.db.progress_handler(1000, Some(poll_interrupt))?;
        let prepared = prepared?;
        if poll_interrupt() {
            return Err(Error("evaluation_cancelled".into()));
        }
        let report = if apply && prepared.report["safe"] == true {
            layout_mutation::apply(store, profile, &prepared, limit)?
        } else {
            prepared.preview(limit)?
        };
        if poll_interrupt() {
            return Err(Error("evaluation_cancelled".into()));
        }
        Ok(report)
    }));
    // Never allow repeated interruption to preempt rollback/owner restoration.
    session.clear_interrupt_handler();
    match result {
        Ok(result) => {
            if let Some(scope) = scope {
                let cleanup = session.finish(scope, result.is_ok());
                match (result, cleanup) {
                    (Ok(value), Ok(())) => Ok(value),
                    (Err(error), _) | (_, Err(error)) => Err(error),
                }
            } else {
                result
            }
        }
        Err(panic) => {
            if let Some(scope) = scope {
                let _ = session.finish(scope, false);
            }
            std::panic::resume_unwind(panic)
        }
    }
}
