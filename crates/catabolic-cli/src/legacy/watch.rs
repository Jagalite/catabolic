// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Portable top-level polling transport. Each cycle forwards native scan,
//! stable-revision metadata and Processing owners; no output gate is relaxed.
use super::text;
use catabolic_store::{Error, Result, Store};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error("native legacy CLI interrupted".into()))
    } else {
        Ok(())
    }
}

fn bounded(args: &Value, key: &str, low: u64, high: u64, message: &str) -> Result<u64> {
    args[key]
        .as_u64()
        .filter(|value| (low..=high).contains(value))
        .ok_or_else(|| Error(message.into()))
}

fn with_cycle_cancel<T>(
    signal: Arc<AtomicBool>,
    operation: impl FnOnce(Arc<AtomicBool>) -> Result<T>,
) -> Result<T> {
    // Processing also sets its token when stopping siblings after an ordinary
    // failure. Keep that cleanup separate from the supervisor's signal flag so
    // an ordinary owner error cannot become a spurious KeyboardInterrupt.
    std::thread::scope(|scope| {
        let (stop, stopped) = mpsc::channel::<()>();
        let owner = Arc::new(AtomicBool::new(signal.load(Ordering::Acquire)));
        let checked = owner.clone();
        scope.spawn(move || {
            loop {
                if signal.load(Ordering::Acquire) {
                    checked.store(true, Ordering::Release);
                    break;
                }
                match stopped.recv_timeout(Duration::from_millis(20)) {
                    Err(mpsc::RecvTimeoutError::Timeout) => (),
                    _ => break,
                }
            }
        });
        let result = operation(owner);
        drop(stop);
        result
    })
}

fn cycle(args: &Value, cancel: Arc<AtomicBool>) -> Result<Value> {
    let mut store = Store::open(Path::new(text(args, "db")), true, false)?;
    let profile = text(args, "profile");
    catabolic_store::mutation::profile(&store, profile)?;
    let settle = bounded(args, "settle", 0, 86400, "settle must be 0..86400 seconds")?;
    let batch = bounded(args, "batch", 1, 1000, "batch must be 1..1000")?;
    if let Some(location) = args["location"].as_str() {
        let exists: bool = store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM locations WHERE id=?)",
            [location],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(Error(format!("unknown location: {location}")));
        }
    }
    let check = cancel.clone();
    store
        .db
        .progress_handler(1000, Some(move || check.load(Ordering::Acquire)))?;
    let check = cancel.clone();
    catabolic_store::embedding::with_interrupt_check(
        move || check.load(Ordering::Acquire),
        || {
            catabolic_runtime::process::with_cancellation(cancel.clone(), || {
                cancelled(&cancel)?;
                let scan = catabolic_runtime::public_scan::execute(
                    &mut store,
                    profile,
                    &json!({"location":args["location"]}),
                )?;
                cancelled(&cancel)?;
                let now = catabolic_runtime::media::now();
                let (mut queued, mut cached) = (0, 0);
                let mut errors = vec![];
                for scanned in scan["scans"].as_array().unwrap() {
                    if scanned["complete"] != true {
                        continue;
                    }
                    let mut after = String::new();
                    loop {
                        cancelled(&cancel)?;
                        let (ids, _, next) = catabolic_store::maintenance::stable_page_with_limit(
                            &store,
                            profile,
                            scanned,
                            &after,
                            now,
                            settle,
                            batch as usize,
                        )?;
                        let Some(next) = next else { break };
                        after = next;
                        cancelled(&cancel)?;
                        if ids.is_empty() {
                            continue;
                        }
                        let result = catabolic_runtime::public_processing::execute(
                            &mut store,
                            profile,
                            "enqueue",
                            &json!({"operation":args["kind"],"file_ids":ids}),
                            cancel.clone(),
                        )?;
                        queued += result["queued"].as_array().unwrap().len();
                        cached += result["cached"].as_array().unwrap().len();
                        errors.extend(
                            result["errors"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .take(100 - errors.len())
                                .cloned(),
                        );
                    }
                }
                cancelled(&cancel)?;
                // Frozen watch runs the whole queued backlog up to batch, even
                // jobs not enqueued by this cycle, and Processing.run then
                // refreshes pending catalogs through the shared executor.
                let processing = catabolic_runtime::public_processing::execute(
                    &mut store,
                    profile,
                    "run",
                    &json!({"workers":args["workers"],"limit":batch,
                        "retry_transient":args["retry_transient"],"retry_delay":args["retry_delay"]}),
                    cancel.clone(),
                )?;
                cancelled(&cancel)?;
                Ok(
                    json!({"complete":scan["complete"]==true && errors.is_empty() && processing["complete"]==true,
                    "scan":scan,"queued":queued,"cached":cached,"errors":errors,"processing":processing}),
                )
            })
        },
    )
}

pub fn execute(args: &Value) -> Result<Value> {
    // The reference checks loop bounds before opening the first writer.
    let interval = bounded(
        args,
        "interval",
        1,
        86400,
        "interval must be 1..86400 seconds",
    )?;
    let cycles = bounded(
        args,
        "cycles",
        0,
        1_000_000,
        "cycles must be 0 (continuous) or 1..1000000",
    )?;
    let args = args.clone();
    crate::signals::interruptible(move |cancel| {
        let mut count = 0;
        loop {
            cancelled(&cancel)?;
            let report = with_cycle_cancel(cancel.clone(), |owner| cycle(&args, owner));
            // Translate any interrupted dependent owner without returning a
            // partially completed cycle as success.
            cancelled(&cancel)?;
            let report = report?;
            count += 1;
            {
                // Cycle results already have a frozen object-order registry
                // entry. Prefix this command's progress field locally instead
                // of adding a heuristic that could reorder unrelated data.
                let body = super::super::presentation::scan_watch_cycle_text(&report);
                super::super::transport::write(
                    "stderr",
                    &format!(
                        "{{\"cycle\": {count}, {}\n",
                        body.strip_prefix('{').unwrap()
                    ),
                    true,
                )?;
            }
            if count == cycles {
                return Ok(json!({"cycles":count,"complete":report["complete"],"last":report}));
            }
            // cycle's Store is already dropped; no writer survives this wait.
            let deadline = Instant::now() + Duration::from_secs(interval);
            while Instant::now() < deadline {
                cancelled(&cancel)?;
                std::thread::sleep(
                    Duration::from_millis(20)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_cleanup_does_not_impersonate_a_signal() {
        let signal = Arc::new(AtomicBool::new(false));
        let result: Result<()> = with_cycle_cancel(signal.clone(), |owner| {
            owner.store(true, Ordering::Release);
            Err(Error("fixture ordinary owner failure".into()))
        });
        assert_eq!(result.unwrap_err().0, "fixture ordinary owner failure");
        assert!(!signal.load(Ordering::Acquire));
    }

    #[test]
    fn a_real_signal_reaches_the_owner_and_bridge_joins() {
        let signal = Arc::new(AtomicBool::new(false));
        with_cycle_cancel(signal.clone(), |owner| {
            signal.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(2);
            while !owner.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(owner.load(Ordering::Acquire));
            Ok(())
        })
        .unwrap();
    }
}
