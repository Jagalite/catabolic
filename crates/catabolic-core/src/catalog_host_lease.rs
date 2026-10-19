// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Trusted host authorization lifetime, independent of catalog membership.
//!
//! The host creates a fresh authority after establishing its own permission
//! snapshot. Revocation first closes admission, then waits for every admitted
//! operation to finish. Only the drained result acknowledges effective revocation.
//! Hold a lease through mutation COMMIT or final read-result delivery. A lease
//! does not itself grant any item or field; scoped catalog admission does that.
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseError {
    InvalidIdentity,
    RevisionMismatch,
    Revoked,
    Exhausted,
    Poisoned,
    Timeout,
}
#[derive(Debug)]
struct State {
    closed: bool,
    active: usize,
}
#[derive(Debug)]
struct Inner {
    namespace: String,
    revision: String,
    state: Mutex<State>,
    drained: Condvar,
}
/// Trusted host-owned authority. Not deserializable or constructible from a
/// caller's capability claims. Clones refer to exactly the same revocation gate.
#[derive(Clone, Debug)]
pub struct HostAuthorization(Arc<Inner>);
/// Owned operation permit. It is intentionally neither Clone nor serializable.
#[derive(Debug)]
pub struct AuthorizationLease(Arc<Inner>);
/// Closed admission gate; dropping this value never reopens authorization.
#[derive(Debug)]
pub struct Revocation(Arc<Inner>);
impl HostAuthorization {
    pub fn new(namespace: String, revision: String) -> Result<Self, LeaseError> {
        if [&namespace, &revision]
            .iter()
            .any(|v| v.is_empty() || v.len() > 1024 || v.chars().any(char::is_control))
        {
            return Err(LeaseError::InvalidIdentity);
        }
        Ok(Self(Arc::new(Inner {
            namespace,
            revision,
            state: Mutex::new(State {
                closed: false,
                active: 0,
            }),
            drained: Condvar::new(),
        })))
    }
    pub fn namespace(&self) -> &str {
        &self.0.namespace
    }
    pub fn revision(&self) -> &str {
        &self.0.revision
    }
    pub fn acquire(&self, expected_revision: &str) -> Result<AuthorizationLease, LeaseError> {
        let mut state = self.0.state.lock().map_err(|_| LeaseError::Poisoned)?;
        if state.closed {
            return Err(LeaseError::Revoked);
        }
        if expected_revision != self.0.revision {
            return Err(LeaseError::RevisionMismatch);
        }
        state.active = state.active.checked_add(1).ok_or(LeaseError::Exhausted)?;
        Ok(AuthorizationLease(Arc::clone(&self.0)))
    }
    /// Nonblocking first phase: all subsequent admissions fail. Already admitted
    /// operations retain their lease until their commit/result-delivery boundary.
    pub fn begin_revocation(&self) -> Result<Revocation, LeaseError> {
        let mut state = self.0.state.lock().map_err(|_| LeaseError::Poisoned)?;
        state.closed = true;
        Ok(Revocation(Arc::clone(&self.0)))
    }
}
impl AuthorizationLease {
    pub fn namespace(&self) -> &str {
        &self.0.namespace
    }
    pub fn revision(&self) -> &str {
        &self.0.revision
    }
    /// Identity comparison prevents equal namespace/revision strings on a fresh
    /// host gate from making an old scoped capability current again.
    pub fn belongs_to(&self, authority: &HostAuthorization) -> bool {
        Arc::ptr_eq(&self.0, &authority.0)
    }
}
impl Drop for AuthorizationLease {
    fn drop(&mut self) {
        // Even poisoned state must release the ownership count so it cannot
        // strand other teardown. Admission still fails closed on poison.
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active = state.active.saturating_sub(1);
        if state.active == 0 {
            self.0.drained.notify_all();
        }
    }
}
impl Revocation {
    /// Identity proof for a drained handoff. Equal namespace/revision strings
    /// on an unrelated gate do not establish that this gate closed admission.
    pub fn belongs_to(&self, authority: &HostAuthorization) -> bool {
        Arc::ptr_eq(&self.0, &authority.0)
    }

    /// A timeout keeps admission closed. It never claims revocation completed
    /// while an admitted operation still owns a lease. Safe to call again.
    pub fn wait_drained(&self, timeout: Duration) -> Result<(), LeaseError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(LeaseError::Timeout)?;
        let mut state = self.0.state.lock().map_err(|_| LeaseError::Poisoned)?;
        while state.active != 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(LeaseError::Timeout);
            }
            let (next, _) = self
                .0
                .drained
                .wait_timeout(state, left)
                .map_err(|_| LeaseError::Poisoned)?;
            state = next;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn authority() -> HostAuthorization {
        HostAuthorization::new("host".into(), "revision-1".into()).unwrap()
    }
    #[test]
    fn revision_and_identity_are_not_forgeable_with_equal_strings() {
        let first = authority();
        assert!(matches!(
            first.acquire("old"),
            Err(LeaseError::RevisionMismatch)
        ));
        let lease = first.acquire(first.revision()).unwrap();
        assert!(lease.belongs_to(&first.clone()));
        assert!(!lease.belongs_to(&authority()));
    }
    #[test]
    fn revocation_closes_admission_before_waiting_for_owned_commit() {
        let host = authority();
        let lease = host.acquire(host.revision()).unwrap();
        let revoked = host.begin_revocation().unwrap();
        assert!(matches!(
            host.acquire(host.revision()),
            Err(LeaseError::Revoked)
        ));
        assert_eq!(
            revoked.wait_drained(Duration::ZERO),
            Err(LeaseError::Timeout)
        );
        assert_eq!(lease.revision(), "revision-1");
        drop(lease);
        assert_eq!(revoked.wait_drained(Duration::ZERO), Ok(()));
        assert!(matches!(
            host.acquire(host.revision()),
            Err(LeaseError::Revoked)
        ));
    }
    #[test]
    fn worker_transfer_retains_permit_until_actual_exit_boundary() {
        let host = authority();
        let lease = host.acquire(host.revision()).unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            wait.recv().unwrap();
            drop(lease);
        });
        let revoked = host.begin_revocation().unwrap();
        assert_eq!(
            revoked.wait_drained(Duration::ZERO),
            Err(LeaseError::Timeout)
        );
        release.send(()).unwrap();
        revoked.wait_drained(Duration::from_secs(5)).unwrap();
        worker.join().unwrap();
    }
    #[test]
    fn dropping_revocation_does_not_reopen_gate() {
        let host = authority();
        drop(host.begin_revocation().unwrap());
        assert!(matches!(
            host.acquire(host.revision()),
            Err(LeaseError::Revoked)
        ));
    }
    #[test]
    fn dropping_one_lease_does_not_drain_another() {
        let host = authority();
        let a = host.acquire(host.revision()).unwrap();
        let b = host.acquire(host.revision()).unwrap();
        let revoked = host.begin_revocation().unwrap();
        drop(a);
        assert_eq!(
            revoked.wait_drained(Duration::ZERO),
            Err(LeaseError::Timeout)
        );
        drop(b);
        revoked.wait_drained(Duration::ZERO).unwrap();
    }
    #[test]
    fn invalid_inputs_and_counter_exhaustion_fail_closed() {
        assert!(matches!(
            HostAuthorization::new("".into(), "r".into()),
            Err(LeaseError::InvalidIdentity)
        ));
        assert!(matches!(
            HostAuthorization::new("host".into(), "r\n".into()),
            Err(LeaseError::InvalidIdentity)
        ));
        let host = authority();
        host.0.state.lock().unwrap().active = usize::MAX;
        assert!(matches!(
            host.acquire(host.revision()),
            Err(LeaseError::Exhausted)
        ));
    }
    #[test]
    fn poisoned_authority_never_admits_or_reports_drained() {
        let host = authority();
        let lease = host.acquire(host.revision()).unwrap();
        let revoked = host.begin_revocation().unwrap();
        let copy = host.clone();
        let result = std::thread::spawn(move || {
            let _locked = copy.0.state.lock().unwrap();
            panic!("injected poison");
        })
        .join();
        assert!(result.is_err());
        drop(lease);
        assert!(matches!(
            host.acquire(host.revision()),
            Err(LeaseError::Poisoned)
        ));
        assert_eq!(
            revoked.wait_drained(Duration::ZERO),
            Err(LeaseError::Poisoned)
        );
    }
}
