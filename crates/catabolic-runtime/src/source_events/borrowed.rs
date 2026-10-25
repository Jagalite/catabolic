//! Filesystem-only event collection for an already retained native host.
//! No Store is opened here. Only the host can mint a persisted-batch ACK.
use super::*;
use std::{collections::BTreeSet, sync::Arc};

/// Opaque capture minted by the retained host. Full native snapshot is retained
/// for its existing binding/policy fence; only selected roots are subscribed.
#[derive(Clone)]
pub struct SourceEventScope {
    pub(crate) snapshot: Snapshot,
    pub(crate) selected: BTreeSet<String>,
}
pub struct SourceEventMonitor {
    inner: MonitorSession,
    scope: SourceEventScope,
    instance: Arc<()>,
}
pub struct SourceEventBatch {
    pub(crate) scope: SourceEventScope,
    pub(crate) instance: Arc<()>,
    pub(crate) sequence: BTreeMap<String, u64>,
    pub(crate) degraded: Vec<String>,
    lost: bool,
}
/// Successful native transaction result, not constructible by the embedding
/// caller. The monitor can consume this only for its own exact sequence batch.
pub struct PersistedSourceEvents {
    pub(crate) batch: SourceEventBatch,
    pub(crate) invalidated: Vec<Value>,
}
impl PersistedSourceEvents {
    pub fn invalidated(&self) -> &[Value] {
        &self.invalidated
    }
}
impl SourceEventBatch {
    /// Complete deduplicated effect set, including degraded-only reconciliation.
    /// The embedding caller must authorize every returned source under its
    /// current application writer immediately before native invalidation.
    pub fn affected_sources(&self) -> impl Iterator<Item = &str> {
        self.sequence
            .keys()
            .chain(self.degraded.iter())
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            .into_iter()
    }
    /// Alias for the complete effect set, never just the raw event keys.
    pub fn sources(&self) -> impl Iterator<Item = &str> {
        self.affected_sources()
    }
    pub fn lost_events(&self) -> bool {
        self.lost
    }
}
impl SourceEventScope {
    pub fn monitor(self) -> Result<SourceEventMonitor> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(Error(
            "native filesystem events are unsupported on this platform".into(),
        ));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let mut selected = self.snapshot.clone();
            selected
                .sources
                .retain(|source| self.selected.contains(&source.name));
            let mut inner = MonitorSession {
                // Deliberately unused: the wrapper exposes no standalone poll.
                path: PathBuf::new(),
                snapshot: selected,
                dirty: Dirty::default(),
                backend: None,
                rebuild: true,
                retry_at: Instant::now(),
                last_error: None,
                degraded: BTreeMap::new(),
                degraded_retry: DEGRADED_RETRY_INITIAL,
                reconciliation: DEGRADED_RECONCILIATION,
            };
            inner.reconnect()?;
            Ok(SourceEventMonitor {
                inner,
                scope: self,
                instance: Arc::new(()),
            })
        }
    }
}
impl SourceEventMonitor {
    /// Safe while the host session is detached for a child: collects only kernel
    /// hints. Cancellation or dropping this batch never clears pending hints.
    pub fn collect(&mut self, wait: Duration, cancel: &AtomicBool) -> Result<SourceEventBatch> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (wait, cancel);
            Err(Error(
                "native filesystem events are unsupported on this platform".into(),
            ))
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            cancelled(cancel, &mut || {})?;
            let lost = self.inner.collect_pending(wait, cancel, &mut || {})?;
            let sequence = self.inner.dirty.pending.clone();
            let now = Instant::now();
            let degraded = self
                .inner
                .degraded
                .iter()
                .filter(|(name, at)| {
                    now.duration_since(**at) >= self.inner.reconciliation
                        && !sequence.contains_key(*name)
                })
                .map(|(name, _)| name.clone())
                .collect();
            Ok(SourceEventBatch {
                scope: self.scope.clone(),
                instance: self.instance.clone(),
                sequence,
                degraded,
                lost,
            })
        }
    }
    pub fn acknowledge(&mut self, persisted: PersistedSourceEvents) -> Result<()> {
        let batch = persisted.batch;
        if !Arc::ptr_eq(&self.instance, &batch.instance)
            || self.scope.snapshot != batch.scope.snapshot
            || self.scope.selected != batch.scope.selected
        {
            return Err(Error(
                "source event acknowledgement belongs to another monitor".into(),
            ));
        }
        self.inner.dirty.acknowledge(&batch.sequence);
        let now = Instant::now();
        for name in batch.sequence.keys().chain(batch.degraded.iter()) {
            if let Some(stamp) = self.inner.degraded.get_mut(name) {
                *stamp = now;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn affected_sources_include_degraded_only_and_deduplicate() {
        // Pure enumeration test, not a host grant or persisted invalidation.
        let mut batch = SourceEventBatch {
            scope: SourceEventScope {
                snapshot: Snapshot {
                    database_id: "test".into(),
                    profile: "default".into(),
                    fence: "test".into(),
                    sources: vec![],
                },
                selected: ["a".into(), "b".into()].into_iter().collect(),
            },
            instance: Arc::new(()),
            sequence: BTreeMap::new(),
            degraded: vec!["b".into()],
            lost: false,
        };
        assert_eq!(batch.affected_sources().collect::<Vec<_>>(), vec!["b"]);
        assert_eq!(batch.sources().collect::<Vec<_>>(), vec!["b"]);
        batch.sequence.insert("a".into(), 1);
        batch.sequence.insert("b".into(), 2);
        assert_eq!(batch.affected_sources().collect::<Vec<_>>(), vec!["a", "b"]);
    }
}
