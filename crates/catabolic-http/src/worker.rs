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
/// Run an explicit native operation with cancellation and completion ownership.
pub fn interruptible(
    operation: impl FnOnce(Arc<AtomicBool>) -> Result<Value> + Send + 'static,
) -> Result<Value> {
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
                    let _ = futures::future::select(
                        Box::pin(tokio::signal::ctrl_c()),
                        Box::pin(terminate.recv()),
                    )
                    .await;
                }
                #[cfg(not(unix))]
                {
                    let _ = tokio::signal::ctrl_c().await;
                }
            });
            let result = match futures::future::select(task, shutdown).await {
                futures::future::Either::Left((result, _)) => result,
                futures::future::Either::Right((_, task)) => {
                    cancel.store(true, Ordering::Release);
                    task.await
                }
            };
            result
                .map_err(|_| Error("native operation supervisor terminated unexpectedly".into()))?
        })
}
