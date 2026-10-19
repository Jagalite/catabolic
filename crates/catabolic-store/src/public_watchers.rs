// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Persistent public watcher adapters; existing owners retain mutations.
use crate::{Error, Result, Store, embedding::poll_interrupt, watcher};
use serde_json::Value;

/// A borrowed query installs its own progress handler and removes it on return.
/// Resume the caller's cancellation bridge before watcher completion mutates
/// that same connection, including when the query returned an error.
pub fn resume_interrupts(store: &Store) -> Result<()> {
    store.db.progress_handler(1000, Some(poll_interrupt))?;
    if poll_interrupt() {
        return Err(Error("evaluation_cancelled".into()));
    }
    Ok(())
}

pub fn call(store: &Store, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    store.transaction_available()?;
    store.db.progress_handler(1000, Some(poll_interrupt))?;
    struct Reset<'a>(&'a rusqlite::Connection);
    impl Drop for Reset<'_> {
        fn drop(&mut self) {
            let _ = self.0.progress_handler(0, None::<fn() -> bool>);
        }
    }
    let _reset = Reset(&store.db);
    match operation {
        "invalidate" => crate::observation::execute(store, profile, "dirty", args),
        "enable_validate" => {
            let definition =
                watcher::definition(store, profile, args["name"].as_str().unwrap_or(""))?;
            crate::plans::describe(store, profile, &definition["plan"])?;
            Ok(Value::Null)
        }
        "require_run" => {
            watcher::require_run(
                store,
                profile,
                args["name"].as_str().unwrap_or(""),
                args["identifier"].as_str().unwrap_or(""),
                &args["definition_id"],
            )?;
            Ok(Value::Null)
        }
        "get" | "list" | "put" | "admit" | "enable" | "disable" | "history" => {
            watcher::execute(store, profile, operation, args)
        }
        _ => Err(Error(format!(
            "unsupported native Watchers operation: {operation}"
        ))),
    }
}
