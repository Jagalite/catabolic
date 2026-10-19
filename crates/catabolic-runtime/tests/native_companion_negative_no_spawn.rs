//! Actual spawn failure after reservation. This invalid executable is never
//! eligible for positive artifact/cleanup qualification.
#![cfg(unix)]
mod native_companion_negative_support;
use catabolic_runtime::host::{CONTRACT_VERSION, HostSession};
use native_companion_negative_support::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
#[test]
fn schema37_definite_no_spawn_keeps_the_committed_reservation_unresolved() {
    let (root, path) = fixture();
    let missing = root.path().join("missing-interpreter");
    assert!(!missing.exists());
    let grant = control_grant(root.path(), &format!("#!{}\n", missing.display()));
    let authorization = gate();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = HostSession::open_authorized(
        &path,
        "default",
        true,
        CONTRACT_VERSION,
        authorization.clone(),
    )
    .unwrap();
    host.register_closed_observation_helper(&grant).unwrap();
    assert!(host.start_scan("media", Permit(drops.clone())).is_err());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(host.active_scan().is_none());
    authorization
        .begin_revocation()
        .unwrap()
        .wait_drained(Duration::ZERO)
        .unwrap();
    host.close().unwrap();
    assert_reserved_and_reopen_refused(&path);
}
