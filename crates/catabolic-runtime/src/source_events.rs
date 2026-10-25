// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Native filesystem hints feeding the existing durable watcher/observation
//! reducers. Event receipt never creates inventory, proves absence or publishes.
use catabolic_store::{
    Error, Result, Store,
    source_events::{Snapshot, invalidate_with_reconciliation, snapshot},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
mod borrowed;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform;
pub use borrowed::{PersistedSourceEvents, SourceEventBatch, SourceEventMonitor, SourceEventScope};

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

/// Degraded-coverage reconciliation interval. It matches the frozen polling
/// `watch` default (`--interval 30`), which rescans every source each cycle.
/// A degraded source is re-invalidated at most this often, and only after the
/// previous reconciliation scan is no longer in flight.
const DEGRADED_RECONCILIATION: Duration = Duration::from_secs(30);
/// Subscription-restoration retries while coverage is degraded start at the
/// existing one-second rebuild delay and double up to the reconciliation
/// interval.
const DEGRADED_RETRY_INITIAL: Duration = Duration::from_secs(1);
const DEGRADED_RETRY_MAX: Duration = DEGRADED_RECONCILIATION;

pub struct MonitorSession {
    path: PathBuf,
    snapshot: Snapshot,
    dirty: Dirty,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    backend: Option<platform::Backend>,
    rebuild: bool,
    retry_at: Instant,
    last_error: Option<String>,
    /// Sources with incomplete subscription coverage, each with the time of
    /// its last full-source invalidation. Coverage transitions invalidate
    /// once; while degraded, only bounded reconciliation invalidates (FS7).
    degraded: BTreeMap<String, Instant>,
    degraded_retry: Duration,
    reconciliation: Duration,
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
                degraded: BTreeMap::new(),
                degraded_retry: DEGRADED_RETRY_INITIAL,
                reconciliation: DEGRADED_RECONCILIATION,
            };
            result.reconnect()?;
            Ok(result)
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn reconnect(&mut self) -> Result<()> {
        // A required rebuild (initial, lost events, directory changes, stream
        // errors, configuration changes) invalidates every current source: any
        // gap during old/new registrations is conservatively covered by the
        // fresh full-source generation. A retry made only to restore degraded
        // coverage keeps the old subscriptions installed until the new set is
        // registered, then drains them, so covered sources lose no hint in the
        // overlap and need no new generation.
        let required = self.rebuild || self.backend.is_none();
        let backend = platform::Backend::new(&self.snapshot.sources)?;
        let old = self.backend.replace(backend);
        let now = Instant::now();
        if required {
            for source in &self.snapshot.sources {
                self.dirty.mark(&source.name)?;
            }
            for stamp in self.degraded.values_mut() {
                *stamp = now;
            }
        } else if let Some(mut old) = old {
            let all = 0..self.snapshot.sources.len();
            let indices: Vec<usize> = match old.wait(Duration::ZERO) {
                Ok(batch) if !batch.lost => batch.sources.into_iter().collect(),
                _ => all.collect(),
            };
            for index in indices {
                if let Some(source) = self.snapshot.sources.get(index) {
                    self.dirty.mark(&source.name)?;
                }
            }
        }
        self.rebuild = false;
        self.last_error = None;
        self.sync_coverage(now)?;
        self.retry_at = now
            + if self.degraded.is_empty() {
                self.degraded_retry = DEGRADED_RETRY_INITIAL;
                Duration::from_secs(1)
            } else {
                let delay = self.degraded_retry;
                self.degraded_retry = (delay * 2).min(DEGRADED_RETRY_MAX);
                delay
            };
        Ok(())
    }
    /// Invalidate coverage transitions exactly once: a source newly missing
    /// subscriptions, or a degraded source whose coverage was restored.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn sync_coverage(&mut self, now: Instant) -> Result<()> {
        let backend = self
            .backend
            .as_ref()
            .ok_or_else(|| Error("native event monitor disconnected".into()))?;
        let current: std::collections::BTreeSet<String> = backend
            .unavailable
            .iter()
            .filter_map(|i| self.snapshot.sources.get(*i))
            .map(|s| s.name.clone())
            .collect();
        let restored: Vec<String> = self
            .degraded
            .keys()
            .filter(|name| !current.contains(*name))
            .cloned()
            .collect();
        for name in restored {
            self.degraded.remove(&name);
            self.dirty.mark(&name)?;
        }
        for name in current {
            if !self.degraded.contains_key(&name) {
                self.dirty.mark(&name)?;
                self.degraded.insert(name, now);
            }
        }
        Ok(())
    }
    /// Filesystem-only collection shared by standalone and retained-host adapters.
    /// Never opens the catalog or acknowledges a pending event sequence.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn collect_pending(
        &mut self,
        wait: Duration,
        cancel: &AtomicBool,
        poll: &mut dyn FnMut(),
    ) -> Result<bool> {
        if (self.rebuild || !self.degraded.is_empty()) && Instant::now() >= self.retry_at {
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
        // Root disconnections found by this wait are coverage transitions.
        self.sync_coverage(Instant::now())?;
        cancelled(cancel, poll)?;
        Ok(lost)
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
                self.degraded
                    .retain(|name, _| self.snapshot.sources.iter().any(|s| s.name == *name));
                self.rebuild = true;
                self.retry_at = Instant::now();
            }
            let lost = self.collect_pending(wait, cancel, poll)?;
            let batch = self.dirty.pending.clone();
            let names = batch.keys().cloned().collect::<Vec<_>>();
            let now = Instant::now();
            let reconcile: Vec<String> = self
                .degraded
                .iter()
                .filter(|(name, at)| {
                    now.duration_since(**at) >= self.reconciliation && !batch.contains_key(*name)
                })
                .map(|(name, _)| name.clone())
                .collect();
            let mut invalidated = vec![];
            let mut catalog_busy = false;
            if !names.is_empty() || !reconcile.is_empty() {
                match Store::open(&self.path, true, false).and_then(|store| {
                    invalidate_with_reconciliation(&store, &self.snapshot, &names, &reconcile)
                }) {
                    Ok(result) => {
                        invalidated = result;
                        self.dirty.acknowledge(&batch);
                        // Invalidated, or still served by an unfinished
                        // reconciliation: the next check is one interval away.
                        for name in names.iter().chain(&reconcile) {
                            if let Some(stamp) = self.degraded.get_mut(name) {
                                *stamp = now;
                            }
                        }
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
            // Degraded coverage stays visible and incomplete; restoration is
            // retried on a bounded backoff without re-invalidating.
            let unavailable: Vec<_> = self.degraded.keys().cloned().collect();
            let reconnecting = self.rebuild || !unavailable.is_empty();
            let complete =
                !self.rebuild && !lost && unavailable.is_empty() && self.dirty.pending.is_empty();
            Ok(
                json!({"complete":complete,"coverage":"hints_only","invalidated":invalidated,"pending_sources":self.dirty.pending.keys().collect::<Vec<_>>(),"unavailable_sources":unavailable,"degraded_coverage":!unavailable.is_empty(),"lost_events":lost,"reconnecting":reconnecting,"catalog_busy":catalog_busy,"registrations":backend.registrations(),"error":self.last_error}),
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

    /// Disposable catalog whose only source has a permission-denied subtree.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    struct Degraded {
        _root: tempfile::TempDir,
        path: PathBuf,
        locked: PathBuf,
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl Drop for Degraded {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.locked, std::fs::Permissions::from_mode(0o755));
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl Degraded {
        fn new() -> Option<Self> {
            use std::os::unix::fs::PermissionsExt;
            // SAFETY: geteuid has no arguments or side effects.
            if unsafe { libc::geteuid() } == 0 {
                eprintln!("skipped: permission-denied subtrees are readable by root");
                return None;
            }
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("catalog.db");
            let source = root.path().join("source");
            let locked = source.join("locked");
            std::fs::create_dir_all(&locked).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            catabolic_store::migration::initialize(&path).unwrap();
            let store = Store::open(&path, true, false).unwrap();
            catabolic_store::observation::execute(
                &store,
                "default",
                "bind",
                &json!({"source":"media","root":source}),
            )
            .unwrap();
            Some(Self {
                _root: root,
                path,
                locked,
            })
        }
        fn generation(&self) -> i64 {
            Store::open(&self.path, false, false)
                .unwrap()
                .db
                .query_row(
                    "SELECT dirty_generation FROM observation_sources WHERE profile='default' AND source='media'",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        }
        fn execute(&self, sql: &str, generation: i64) {
            Store::open(&self.path, true, false)
                .unwrap()
                .db
                .execute(sql, [generation])
                .unwrap();
        }
        /// Polls through more than one reconciliation interval.
        fn polls(&self, session: &mut MonitorSession) -> Vec<Value> {
            let deadline = Instant::now() + session.reconciliation * 3;
            let mut reports = vec![];
            while Instant::now() < deadline {
                let report = session
                    .poll(
                        Duration::from_millis(20),
                        &AtomicBool::new(false),
                        &mut || {},
                    )
                    .unwrap();
                assert_eq!(report["complete"], false, "{report}");
                assert_eq!(report["degraded_coverage"], true, "{report}");
                assert_eq!(report["unavailable_sources"], json!(["media"]), "{report}");
                reports.push(report);
            }
            reports
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn degraded_reconciliation_is_periodic_bounded_and_never_fences_inflight_scans() {
        let Some(f) = Degraded::new() else {
            return;
        };
        let mut session = MonitorSession::new(&f.path, "default").unwrap();
        session.reconciliation = Duration::from_millis(200);
        let reports = f.polls(&mut session);
        // The degraded transition invalidated once. No scan has claimed that
        // generation, so reconciliation does not advance it again.
        assert_eq!(reports[0]["invalidated"][0]["source"], "media");
        let transition = f.generation();
        assert_eq!(transition, 1);
        assert!(reports[1..].iter().all(|r| r["invalidated"] == json!([])));
        // A queued reconciliation scan with active demand is never fenced,
        // even beside another scan that already completed at this generation.
        f.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at) VALUES('claimed','default','media','fixture','[]',?1,10,'complete',0)", transition);
        f.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at) VALUES('first','default','media','fixture','[]',?1,1,'queued',0)", transition);
        f.execute("INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,state,created_at) SELECT 'demand','default','media','watcher:fixture','{}','first','active',0 WHERE ?1>0", transition);
        f.polls(&mut session);
        assert_eq!(f.generation(), transition);
        // So is a running scan under a live lease.
        f.execute("UPDATE observation_jobs SET state='running',lease_until=4102444800 WHERE id='first' AND dirty_generation=?1", transition);
        f.polls(&mut session);
        assert_eq!(f.generation(), transition);
        // Once it finishes, the next interval requests one more reconciliation,
        // then waits for a scan to claim the new generation.
        f.execute("UPDATE observation_jobs SET state='complete',lease_until=0 WHERE id='first' AND dirty_generation=?1", transition);
        let reports = f.polls(&mut session);
        assert_eq!(f.generation(), transition + 1);
        let reconciled: Vec<_> = reports
            .iter()
            .flat_map(|r| r["invalidated"].as_array().unwrap().clone())
            .collect();
        assert_eq!(
            reconciled,
            vec![json!({"source":"media","reason":"degraded_coverage","coverage":"full_source"})]
        );
        // An expired running lease no longer counts as in flight.
        f.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,lease_until) VALUES('second','default','media','fixture','[]',?1,2,'running',0,1)", transition + 1);
        f.polls(&mut session);
        assert_eq!(f.generation(), transition + 2);
        // A queued job whose only request was cancelled is abandoned: alone it
        // is no claim, and it never blocks once another scan completes.
        f.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at) VALUES('abandoned','default','media','fixture','[]',?1,3,'queued',0)", transition + 2);
        f.execute("INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,state,created_at) SELECT 'cancelled','default','media','watcher:abandoned','{}','abandoned','cancelled',0 WHERE ?1>0", transition + 2);
        f.polls(&mut session);
        assert_eq!(f.generation(), transition + 2);
        f.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at) VALUES('later','default','media','fixture','[]',?1,4,'complete',0)", transition + 2);
        f.polls(&mut session);
        assert_eq!(f.generation(), transition + 3);
    }
}
