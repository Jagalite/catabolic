// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Selected standalone legacy commands preserve TERM only after native cleanup.
use catabolic_store::Result;
use serde_json::Value;
#[cfg(unix)]
use std::sync::atomic::Ordering;
use std::sync::{Arc, atomic::AtomicBool};

#[cfg(unix)]
static TERMINATED: AtomicBool = AtomicBool::new(false);

pub fn interruptible(
    operation: impl FnOnce(Arc<AtomicBool>) -> Result<Value> + Send + 'static,
) -> Result<Value> {
    let outcome = catabolic_http::interruptible_with_signal(operation)?;
    #[cfg(unix)]
    if outcome.signal == Some(catabolic_http::InterruptSignal::Terminate)
        && !crate::transport::active()
    {
        TERMINATED.store(true, Ordering::Release);
    }
    outcome.result
}

pub fn finish() {
    #[cfg(unix)]
    if TERMINATED.load(Ordering::Acquire) {
        // All selected command owners have returned before this process-only
        // boundary. Keep the signal handler installed throughout their cleanup,
        // including repeated signals. The private Python transport never enters
        // this branch: its parent owns termination and acknowledged result IO.
        // SAFETY: SIGTERM is valid, SIG_DFL is a valid disposition, and the local
        // signal set is initialized before use. Only this terminating CLI thread
        // changes its mask; no borrowed owner or helper survives this boundary.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            let mut signals: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut signals);
            libc::sigaddset(&mut signals, libc::SIGTERM);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &signals, std::ptr::null_mut());
            libc::raise(libc::SIGTERM);
        }
        // Bounded fallback if the platform cannot restore/deliver the signal.
        std::process::exit(128 + libc::SIGTERM);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        path::Path,
        process::{Child, Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn cleanup_fixture() {
        let Ok(directory) = std::env::var("CATABOLIC_SIGNAL_TEST_DIRECTORY") else {
            return;
        };
        let mode = std::env::var("CATABOLIC_SIGNAL_TEST_MODE").unwrap();
        let operation = move |cancel: Arc<AtomicBool>| {
            let path = Path::new(&directory);
            fs::write(path.join("ready"), b"ready").unwrap();
            while !cancel.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
            }
            fs::write(path.join("cleaning"), b"cleaning").unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !path.join("release").exists() {
                assert!(Instant::now() < deadline, "cleanup was never released");
                thread::sleep(Duration::from_millis(5));
            }
            fs::write(path.join("cleaned"), b"cleanup completed").unwrap();
            Err(catabolic_store::Error("fixture owner result".into()))
        };
        let result = if mode == "legacy" {
            interruptible(operation)
        } else if mode == "typed" {
            catabolic_http::interruptible_detailed(operation).unwrap()
        } else {
            catabolic_http::interruptible(operation)
        };
        assert_eq!(result.unwrap_err().0, "fixture owner result");
        finish();
    }

    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn wait_for(path: &Path, child: &mut Child) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(child.try_wait().unwrap().is_none(), "fixture exited early");
            assert!(Instant::now() < deadline, "fixture never reached {path:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn send(child: &Child, signal: i32) {
        // SAFETY: This is the still-owned subprocess ID and a valid test signal.
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
    }

    #[test]
    fn repeated_signals_wait_for_cleanup_and_shared_api_keeps_its_result() {
        use std::os::unix::process::ExitStatusExt;
        for mode in ["legacy", "shared", "typed"] {
            let directory = tempfile::tempdir().unwrap();
            let mut child = OwnedChild(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "signals::tests::cleanup_fixture"])
                    .env("CATABOLIC_SIGNAL_TEST_DIRECTORY", directory.path())
                    .env("CATABOLIC_SIGNAL_TEST_MODE", mode)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            wait_for(&directory.path().join("ready"), &mut child.0);
            send(&child.0, libc::SIGTERM);
            wait_for(&directory.path().join("cleaning"), &mut child.0);
            send(&child.0, libc::SIGTERM);
            send(&child.0, libc::SIGINT);
            thread::sleep(Duration::from_millis(30));
            assert!(child.0.try_wait().unwrap().is_none());
            assert!(!directory.path().join("cleaned").exists());
            fs::write(directory.path().join("release"), b"release").unwrap();
            wait_for(&directory.path().join("cleaned"), &mut child.0);
            let deadline = Instant::now() + Duration::from_secs(10);
            let status = loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    break status;
                }
                assert!(Instant::now() < deadline, "fixture did not finish");
                thread::sleep(Duration::from_millis(5));
            };
            if mode == "legacy" {
                assert_eq!(status.signal(), Some(libc::SIGTERM));
            } else {
                assert!(status.success());
            }
        }
    }
}
