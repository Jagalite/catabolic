//! Deterministic held-child control with a deliberately malformed acknowledgment.
//! The control never qualifies positive cleanup or a production helper artifact.
#![cfg(unix)]
mod native_companion_negative_support;
use catabolic_core::catalog_host_lease::LeaseError;
use catabolic_runtime::host::{CONTRACT_VERSION, ErrorCode, HostSession, Lifecycle};
use native_companion_negative_support::*;
use std::{
    ffi::CString,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
struct ReleaseOnDrop(PathBuf);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        // Keep a failing assertion from leaving HostSession::drop waiting on
        // the deliberately held control. Never block opening the test FIFO.
        if let Ok(mut release) = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.0)
        {
            let _ = release.write_all(b"release\n");
        }
    }
}

#[test]
fn schema37_held_child_refuses_zero_budget_close_and_bad_ack_leaves_no_witness() {
    let (root, path) = fixture();
    let fifo = path.with_extension("release");
    let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: the CString is live and NUL-terminated; the temporary test path is
    // owned by this fixture, and mode 0600 grants no access outside this user.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let grant = control_grant(
        root.path(),
        "#!/bin/sh\nprintf 'ready' > \"$2.started\"\nIFS= read -r ignored < \"$2.release\"\nprintf '{\"complete\":true}\\n'\n",
    );
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
    let handle = host.start_scan("media", Permit(drops.clone())).unwrap();
    let _release_on_panic = ReleaseOnDrop(fifo.clone());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.with_extension("started").exists() {
        assert!(
            Instant::now() < deadline,
            "control did not reach its held-child point"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let revocation = authorization.begin_revocation().unwrap();
    let before = Instant::now();
    assert!(!host.drain(Duration::ZERO).unwrap());
    assert!(
        before.elapsed() < Duration::from_secs(1),
        "zero-budget pending drain was not bounded"
    );
    assert_eq!(host.lifecycle(), Lifecycle::Draining);
    assert_eq!(host.active_scan(), Some(handle));
    assert_eq!(host.close().unwrap_err().code, ErrorCode::Busy);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(
        revocation.wait_drained(Duration::ZERO),
        Err(LeaseError::Timeout)
    );
    let mut release = OpenOptions::new().write(true).open(&fifo).unwrap();
    release.write_all(b"release\n").unwrap();
    drop(release);
    let failure = host.drain(Duration::from_secs(3)).unwrap_err();
    assert_eq!(failure.code, ErrorCode::Internal);
    assert_eq!(host.lifecycle(), Lifecycle::Closed);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    revocation.wait_drained(Duration::ZERO).unwrap();
    assert_eq!(
        fs::read_to_string(path.with_extension("started")).unwrap(),
        "ready"
    );
    assert_reserved_and_reopen_refused(&path);
}
