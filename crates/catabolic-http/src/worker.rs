// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! CLI signal bridge: cancellation reaches the native render owner and the
//! supervisor joins its bounded notification worker before removing its lease.
use catabolic_store::{Error, Result};
use serde_json::Value;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub fn worker(path: &Path, profile: &str, once: bool, interval: Duration) -> Result<Value> {
    let (path, profile) = (path.to_owned(), profile.to_owned());
    interruptible(move |cancel| {
        catabolic_runtime::http_worker::run(&path, &profile, once, interval, cancel)
    })
}
/// The signal selected by the supervisor, after the operation has finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptSignal {
    Interrupt,
    #[cfg(unix)]
    Terminate,
}

/// Operation result and signal identity. No signal disposition is changed here.
pub struct InterruptibleOutcome {
    pub result: Result<Value>,
    pub signal: Option<InterruptSignal>,
}

/// Run an explicit native operation with cancellation and completion ownership.
pub fn interruptible(
    operation: impl FnOnce(Arc<AtomicBool>) -> Result<Value> + Send + 'static,
) -> Result<Value> {
    interruptible_with_signal(operation)?.result
}

/// Retain shutdown identity for callers that own their process exit policy.
/// Like `interruptible`, this joins the operation and drops its runtime before
/// returning. HTTP and embedded callers need not adopt CLI exit semantics.
pub fn interruptible_with_signal(
    operation: impl FnOnce(Arc<AtomicBool>) -> Result<Value> + Send + 'static,
) -> Result<InterruptibleOutcome> {
    let (result, signal) = supervise(operation)?;
    Ok(InterruptibleOutcome { result, signal })
}

/// Preserve a caller's typed result across the same joined cancellation bridge.
/// Supervisor failures remain native errors; command failures are not flattened.
pub fn interruptible_detailed<T: Send + 'static>(
    operation: impl FnOnce(Arc<AtomicBool>) -> T + Send + 'static,
) -> Result<T> {
    supervise(operation).map(|(result, _signal)| result)
}

/// `interruptible_detailed`, also retaining the selected shutdown signal for
/// callers that own their process exit policy.
pub fn interruptible_detailed_with_signal<T: Send + 'static>(
    operation: impl FnOnce(Arc<AtomicBool>) -> T + Send + 'static,
) -> Result<(T, Option<InterruptSignal>)> {
    supervise(operation)
}

fn supervise<T: Send + 'static>(
    operation: impl FnOnce(Arc<AtomicBool>) -> T + Send + 'static,
) -> Result<(T, Option<InterruptSignal>)> {
    let cancel = Arc::new(AtomicBool::new(false));
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|e| Error(e.to_string()))?
        .block_on(async move {
            #[cfg(unix)]
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .map_err(|e| Error(e.to_string()))?;
            let stopped = cancel.clone();
            let task = tokio::task::spawn_blocking(move || operation(stopped));
            let shutdown = Box::pin(async move {
                #[cfg(unix)]
                {
                    match futures::future::select(
                        Box::pin(tokio::signal::ctrl_c()),
                        Box::pin(terminate.recv()),
                    )
                    .await
                    {
                        futures::future::Either::Left(_) => InterruptSignal::Interrupt,
                        futures::future::Either::Right(_) => InterruptSignal::Terminate,
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = tokio::signal::ctrl_c().await;
                    InterruptSignal::Interrupt
                }
            });
            let (result, signal) = match futures::future::select(task, shutdown).await {
                futures::future::Either::Left((result, _)) => (result, None),
                futures::future::Either::Right((signal, task)) => {
                    cancel.store(true, Ordering::Release);
                    (task.await, Some(signal))
                }
            };
            Ok((
                result.map_err(|_| {
                    Error("native operation supervisor terminated unexpectedly".into())
                })?,
                signal,
            ))
        })
}
