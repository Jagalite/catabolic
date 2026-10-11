// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Native filesystem hints feeding the existing durable watcher/observation
//! reducers. Event receipt never creates inventory, proves absence or publishes.
use catabolic_store::{
    Error, Result, Store,
    source_events::{Snapshot, invalidate, snapshot},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform;

/// Bounded full-source coalescing. A stale acknowledgement cannot clear a hint
/// received after that batch. Counter exhaustion rejects instead of wrapping.
#[derive(Default)]
struct Dirty {
    sequence: u64,
    pending: BTreeMap<String, u64>,
}
impl Dirty {
    fn mark(&mut self, name: &str) -> Result<()> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error("native event generation exhausted".into()))?;
        self.pending.insert(name.into(), self.sequence);
        Ok(())
    }
    fn acknowledge(&mut self, batch: &BTreeMap<String, u64>) {
        self.pending
            .retain(|name, generation| batch.get(name) != Some(generation));
    }
}
fn cancelled(cancel: &AtomicBool, poll: &mut dyn FnMut()) -> Result<()> {
    poll();
    if cancel.load(Ordering::Acquire) {
        Err(Error("native event monitoring interrupted".into()))
    } else {
        Ok(())
    }
}
fn busy(error: &Error) -> bool {
    error.0.contains("another Catabolic writer") || error.0.contains("database is locked")
}

pub struct MonitorSession {
    path: PathBuf,
    snapshot: Snapshot,
    dirty: Dirty,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    backend: Option<platform::Backend>,
    rebuild: bool,
    retry_at: Instant,
    last_error: Option<String>,
}
impl MonitorSession {
    /// No writer session is held during subscription setup or kernel waits.
    pub fn new(path: &Path, profile: &str) -> Result<Self> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(Error(
            "native filesystem events are unsupported on this platform".into(),
        ));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let store = Store::open(path, false, false)?;
            let state = snapshot(&store, profile)?;
            drop(store);
            let mut result = Self {
                path: path.into(),
                snapshot: state,
                dirty: Dirty::default(),
                backend: None,
                rebuild: true,
                retry_at: Instant::now(),
                last_error: None,
            };
            result.reconnect()?;
            Ok(result)
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn reconnect(&mut self) -> Result<()> {
        // Install before invalidating: any gap during old/new registrations is
        // conservatively covered by the fresh full-source generation.
        self.backend = Some(platform::Backend::new(&self.snapshot.sources)?);
        self.rebuild = false;
        self.retry_at = Instant::now() + Duration::from_secs(1);
        self.last_error = None;
        for source in &self.snapshot.sources {
            self.dirty.mark(&source.name)?;
        }
        Ok(())
    }
    /// Drain kernel events, refresh changed bindings/policies, and atomically
    /// invalidate current sources. Busy catalogs retain this bounded batch.
    pub fn poll(
        &mut self,
        wait: Duration,
        cancel: &AtomicBool,
        poll: &mut dyn FnMut(),
    ) -> Result<Value> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (wait, cancel, poll);
            Err(Error(
                "native filesystem events are unsupported on this platform".into(),
            ))
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            cancelled(cancel, poll)?;
            let current = {
                let store = Store::open(&self.path, false, false)?;
                snapshot(&store, &self.snapshot.profile)?
            };
            if current.database_id != self.snapshot.database_id {
                return Err(Error("catalog replaced during native monitoring".into()));
            }
            if current != self.snapshot {
                self.snapshot = current;
                self.dirty
                    .pending
                    .retain(|name, _| self.snapshot.sources.iter().any(|s| s.name == *name));
                self.rebuild = true;
                self.retry_at = Instant::now();
            }
            if self.rebuild && Instant::now() >= self.retry_at {
                self.reconnect()?;
            }
            let deadline = Instant::now() + wait.min(Duration::from_secs(30));
            let mut lost = false;
            loop {
                cancelled(cancel, poll)?;
                let duration = deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(50));
                let outcome = self
                    .backend
                    .as_mut()
                    .ok_or_else(|| Error("native event monitor disconnected".into()))?
                    .wait(duration);
                match outcome {
                    Ok(batch) => {
                        let any = !batch.sources.is_empty();
                        for index in batch.sources {
                            self.dirty.mark(&self.snapshot.sources[index].name)?;
                        }
                        let newly_rebuilding = !self.rebuild && batch.rebuild;
                        self.rebuild |= batch.rebuild;
                        lost |= batch.lost;
                        if newly_rebuilding {
                            self.retry_at = Instant::now()
                                + if batch.lost {
                                    Duration::from_secs(1)
                                } else {
                                    Duration::ZERO
                                };
                        }
                        if any || Instant::now() >= deadline {
                            break;
                        }
                    }
                    Err(error) => {
                        self.last_error = Some(error.0);
                        self.rebuild = true;
                        lost = true;
                        self.retry_at = Instant::now() + Duration::from_secs(1);
                        for source in &self.snapshot.sources {
                            self.dirty.mark(&source.name)?;
                        }
                        break;
                    }
                }
            }
            cancelled(cancel, poll)?;
            let batch = self.dirty.pending.clone();
            let names = batch.keys().cloned().collect::<Vec<_>>();
            let mut invalidated = vec![];
            let mut catalog_busy = false;
            if !names.is_empty() {
                match Store::open(&self.path, true, false)
                    .and_then(|store| invalidate(&store, &self.snapshot, &names))
                {
                    Ok(result) => {
                        invalidated = result;
                        self.dirty.acknowledge(&batch);
                    }
                    Err(error) if busy(&error) => catalog_busy = true,
                    Err(error) if error.0 == "stale_native_event_snapshot" => {
                        self.rebuild = true;
                        self.retry_at = Instant::now();
                    }
                    Err(error) => return Err(error),
                }
            }
            let backend = self.backend.as_ref().unwrap();
            let unavailable: Vec<_> = backend
                .unavailable
                .iter()
                .filter_map(|i| self.snapshot.sources.get(*i))
                .map(|s| s.name.clone())
                .collect();
            if !unavailable.is_empty() {
                self.rebuild = true;
            }
            let complete =
                !self.rebuild && !lost && unavailable.is_empty() && self.dirty.pending.is_empty();
            Ok(
                json!({"complete":complete,"coverage":"hints_only","invalidated":invalidated,"pending_sources":self.dirty.pending.keys().collect::<Vec<_>>(),"unavailable_sources":unavailable,"lost_events":lost,"reconnecting":self.rebuild,"catalog_busy":catalog_busy,"registrations":backend.registrations(),"error":self.last_error}),
            )
        }
    }
}

/// Native supervisor foreground loop. The same session remains subscribed
/// during watcher child execution; each worker-wait pulse drains durable hints.
/// Cancellation ends this waiter, never revokes shared observation requests.
pub fn supervise_with_poll(
    path: &Path,
    profile: &str,
    request: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
) -> Result<Value> {
    let path = catabolic_store::database_path(path)?;
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".supervisor");
    let _owner = catabolic_store::writer_lock(Path::new(&lock_path))?;
    let native = request["native"] == true;
    let once = request["once"] == true;
    let mut monitor = if native {
        Some(MonitorSession::new(&path, profile)?)
    } else {
        None
    };
    let mut initialized = false;
    loop {
        cancelled(cancel, poll)?;
        let mut monitoring = Value::Null;
        if let Some(session) = monitor.as_mut() {
            monitoring = session.poll(Duration::ZERO, cancel, poll)?;
        }
        let result = (|| {
            let mut store = Store::open(&path, true, false)?;
            if !initialized {
                catabolic_store::watcher::execute(&store, profile, "recover", &json!({}))?;
                if !once || native {
                    crate::watchers::execute(&mut store, profile, "startup", &json!({}))?;
                }
                initialized = true;
            }
            let mut options = request.clone();
            options["native"] = json!(false);
            crate::watchers::tick_with_poll(&mut store, profile, &options, &mut || {
                cancelled(cancel, poll)?;
                if let Some(session) = monitor.as_mut() {
                    monitoring = session.poll(Duration::ZERO, cancel, poll)?;
                }
                Ok(())
            })
        })();
        let mut report = match result {
            Ok(value) => value,
            Err(error) if busy(&error) => {
                json!({"runs":[],"pending":null,"complete":false,"error":"catalog_busy"})
            }
            Err(error) => return Err(error),
        };
        if let Some(session) = monitor.as_mut() {
            monitoring = session.poll(Duration::ZERO, cancel, poll)?;
            if monitoring["complete"] != true
                || monitoring["invalidated"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
            {
                report["complete"] = json!(false);
            }
            if request["include_monitoring"] == true {
                report["monitoring"] = monitoring;
            }
        }
        if once {
            return Ok(report);
        }
        // Foreground supervisor emits only its final/once report. Sleep outside
        // every catalog session, with bounded signal polling.
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            cancelled(cancel, poll)?;
            if let Some(session) = monitor.as_mut() {
                session.poll(Duration::from_millis(50), cancel, poll)?;
            } else {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_ack_cannot_clear_new_hint_and_overflow_never_wraps() {
        let mut dirty = Dirty::default();
        dirty.mark("media").unwrap();
        let old = dirty.pending.clone();
        dirty.mark("media").unwrap();
        dirty.acknowledge(&old);
        assert_eq!(dirty.pending.len(), 1);
        let current = dirty.pending.clone();
        dirty.acknowledge(&current);
        assert!(dirty.pending.is_empty());
        dirty.sequence = u64::MAX;
        assert!(dirty.mark("media").is_err());
    }
}
