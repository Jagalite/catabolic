//! Borrowed native event owner. Embedders must independently retain current
//! requester/config/source/lifecycle authorization and their application writer.
//! These methods are host-operator capabilities, never a user grant.
use super::*;
use crate::source_events::PersistedSourceEvents;
use crate::source_events::{SourceEventBatch, SourceEventScope};
use std::collections::BTreeSet;

impl HostSession {
    pub fn capture_source_events(&self, selected: &[String]) -> Result<SourceEventScope> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        let selected_set: BTreeSet<String> = selected.iter().cloned().collect();
        if selected.is_empty()
            || selected.len() > catabolic_store::source_events::MAX_SOURCES
            || selected_set.len() != selected.len()
        {
            return Err(internal(catabolic_store::Error(
                "invalid selected event source set".into(),
            )));
        }
        let snapshot = catabolic_store::source_events::snapshot(
            self.session.store().map_err(internal)?,
            &self.profile,
        )
        .map_err(internal)?;
        if selected_set
            .iter()
            .any(|name| !snapshot.sources.iter().any(|source| &source.name == name))
        {
            return Err(internal(catabolic_store::Error(
                "selected event source is not registered".into(),
            )));
        }
        Ok(SourceEventScope {
            snapshot,
            selected: selected_set,
        })
    }
    /// Descriptive generation/claim facts, never a completion or reuse grant.
    pub fn source_event_generations(
        &self,
        scope: &SourceEventScope,
    ) -> Result<Vec<catabolic_store::source_events::GenerationFact>> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        if scope.snapshot.profile != self.profile {
            return Err(internal(catabolic_store::Error(
                "source event profile differs".into(),
            )));
        }
        let names: Vec<String> = scope.selected.iter().cloned().collect();
        catabolic_store::source_events::generation_facts(
            self.session.store().map_err(internal)?,
            &scope.snapshot,
            &names,
        )
        .map_err(internal)
    }
    /// Persists invalidation through the existing snapshot-fenced dirty owner.
    /// Detached/unavailable sessions refuse; no reconnect or observation spawn.
    /// The returned ACK is minted only after the native transaction commits.
    pub fn invalidate_source_events(
        &mut self,
        batch: SourceEventBatch,
    ) -> Result<PersistedSourceEvents> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        if !self.writable
            || batch.scope.snapshot.profile != self.profile
            || batch
                .sequence
                .keys()
                .chain(batch.degraded.iter())
                .any(|name| !batch.scope.selected.contains(name))
        {
            return Err(internal(catabolic_store::Error(
                "source event batch is outside retained host scope".into(),
            )));
        }
        let names: Vec<String> = batch.sequence.keys().cloned().collect();
        let invalidated = catabolic_store::source_events::invalidate_with_reconciliation(
            self.session.store().map_err(internal)?,
            &batch.scope.snapshot,
            &names,
            &batch.degraded,
        )
        .map_err(internal)?;
        Ok(PersistedSourceEvents { batch, invalidated })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::{ClosedObservationProtocol, TrustedClosedObservationArtifact};
    use std::{fs, path::PathBuf};

    fn fixture() -> (tempfile::TempDir, PathBuf, HostSession, HostAuthorization) {
        let binary = PathBuf::from(
            std::env::var_os("CATABOLIC_HOST_TEST_HELPER")
                .expect("actual packaging-verified CATABOLIC_HOST_TEST_HELPER is required"),
        );
        let artifact = TrustedClosedObservationArtifact::after_packaging_verification(
            &binary,
            &catabolic_core::sha256(fs::read(&binary).unwrap()),
            ClosedObservationProtocol::NoChildWithExactJoinV2,
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.sqlite3");
        catabolic_store::migration::initialize_native_at(&path, 40).unwrap();
        fs::create_dir(dir.path().join("source")).unwrap();
        fs::write(dir.path().join("source/one.txt"), b"first").unwrap();
        let gate =
            HostAuthorization::new("source-event-fixture".into(), "revision-1".into()).unwrap();
        let mut host =
            HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate.clone())
                .unwrap();
        host.register_source("media", &dir.path().join("source"), &[])
            .unwrap();
        host.register_closed_observation_helper(&artifact).unwrap();
        (dir, path, host, gate)
    }
    fn dirty(path: &Path) -> i64 {
        let db =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        db.query_row("SELECT dirty_generation FROM observation_sources WHERE profile='default' AND source='media'",[],|row|row.get(0)).unwrap()
    }
    fn batch(monitor: &mut crate::source_events::SourceEventMonitor) -> SourceEventBatch {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let value = monitor
                .collect(Duration::from_millis(50), &AtomicBool::new(false))
                .unwrap();
            if value.sources().any(|name| name == "media") {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "actual filesystem event was not collected"
            );
        }
    }
    #[test]
    fn during_scan_hints_wait_for_join_and_exact_ack_preserves_later_events() {
        let (dir, path, mut host, gate) = fixture();
        let mut monitor = host
            .capture_source_events(&["media".into()])
            .unwrap()
            .monitor()
            .unwrap();
        let initial = batch(&mut monitor);
        let ack = host.invalidate_source_events(initial).unwrap();
        monitor.acknowledge(ack).unwrap();
        let baseline = dirty(&path);
        let handle = host.start_scan("media", ()).unwrap();
        assert!(host.detached);
        fs::write(
            dir.path().join("source/one.txt"),
            b"changed during detached observation",
        )
        .unwrap();
        let pending = batch(&mut monitor);
        assert!(host.invalidate_source_events(pending).is_err());
        assert!(
            host.detached,
            "event adapter must never reconnect a running child session"
        );
        assert_eq!(dirty(&path), baseline);
        assert!(
            monitor
                .collect(Duration::ZERO, &AtomicBool::new(true))
                .is_err()
        );
        let mut terminal = host.cancel_scan(&handle).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while terminal.is_none() {
            terminal = host.poll_scan(&handle, Duration::from_millis(50)).unwrap();
            assert!(
                Instant::now() < deadline,
                "actual cancelled observation failed to join"
            );
        }
        assert!(!host.detached);
        assert!(host.worker_inventory().unwrap().admits_new_work());
        let pending = batch(&mut monitor);
        let old_ack = host.invalidate_source_events(pending).unwrap();
        let persisted_sequence = old_ack.batch.sequence["media"];
        assert!(dirty(&path) > baseline);
        fs::write(
            dir.path().join("source/one.txt"),
            b"later event before old ACK",
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let later = batch(&mut monitor);
            if later.sequence["media"] > persisted_sequence {
                break; // collection alone is not acknowledgement
            }
            assert!(
                Instant::now() < deadline,
                "later real event did not advance sequence"
            );
        }
        monitor.acknowledge(old_ack).unwrap();
        let still_pending = batch(&mut monitor);
        let ack = host.invalidate_source_events(still_pending).unwrap();
        monitor.acknowledge(ack).unwrap();
        drop(monitor);
        host.close().unwrap();
        drop(host);
        let mut reopened =
            HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate).unwrap();
        // Restart always owes catch-up for the registration gap; no saved
        // monitor cursor is treated as evidence that the filesystem was quiet.
        let mut restored = reopened
            .capture_source_events(&["media".into()])
            .unwrap()
            .monitor()
            .unwrap();
        let catchup = batch(&mut restored);
        let before = dirty(&path);
        let ack = reopened.invalidate_source_events(catchup).unwrap();
        restored.acknowledge(ack).unwrap();
        assert!(dirty(&path) > before);
        reopened.close().unwrap();
    }
    #[test]
    fn stale_binding_scope_wrong_monitor_ack_and_revocation_fail_closed() {
        let (dir, path, mut host, gate) = fixture();
        let scope = host.capture_source_events(&["media".into()]).unwrap();
        let mut first = scope.clone().monitor().unwrap();
        let mut second = scope.monitor().unwrap();
        let ack = host.invalidate_source_events(batch(&mut first)).unwrap();
        assert!(second.acknowledge(ack).is_err());
        assert!(batch(&mut first).sources().any(|name| name == "media"));
        let before = dirty(&path);
        fs::create_dir(dir.path().join("other")).unwrap();
        host.register_source("other", &dir.path().join("other"), &[])
            .unwrap();
        assert!(host.invalidate_source_events(batch(&mut first)).is_err());
        assert_eq!(dirty(&path), before);
        let mut current = host
            .capture_source_events(&["media".into()])
            .unwrap()
            .monitor()
            .unwrap();
        let pending = batch(&mut current);
        let revocation = gate.begin_revocation().unwrap();
        revocation.wait_drained(Duration::ZERO).unwrap();
        assert!(host.invalidate_source_events(pending).is_err());
        assert_eq!(dirty(&path), before);
        host.close().unwrap();
    }
}
