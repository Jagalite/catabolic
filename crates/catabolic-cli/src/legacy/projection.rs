// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Preserved HTTP-v3 queue, ordinary v1 metadata and rollback-only v2 preview.
//! Filesystem-v2 apply/recovery remain gated; no engine fallback exists.
use super::{read_text, text};
use catabolic_store::{Error, Result, Store};
use serde_json::{Value, json};
use std::{
    io,
    os::fd::{AsFd, AsRawFd, BorrowedFd},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// Borrow stdin exclusively for this admission and restore its shared descriptor
/// flags before returning. No detached reader can outlive the signal supervisor.
struct Nonblocking<'a> {
    fd: BorrowedFd<'a>,
    flags: libc::c_int,
}
impl<'a> Nonblocking<'a> {
    fn new(fd: BorrowedFd<'a>) -> io::Result<Self> {
        // SAFETY: fd remains borrowed/live throughout this owning guard.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: preserve all existing status flags, changing only O_NONBLOCK.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, flags })
    }
}
impl Drop for Nonblocking<'_> {
    fn drop(&mut self) {
        // SAFETY: the borrowed descriptor still lives; restore the captured flags.
        unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_SETFL, self.flags) };
    }
}
fn definition_text(path: &str, cancel: &AtomicBool) -> Result<String> {
    const MAXIMUM: usize = 1024 * 1024;
    if path != "-" || super::super::transport::active() {
        // Callable input belongs to Python's text stream, never protocol stdin.
        // The parent interrupts/reaps this process and closes the wire on failure.
        // Ordinary file admission and its diagnostics remain the existing path.
        return read_text(path, MAXIMUM);
    }
    let stdin = io::stdin();
    let stdin = stdin.lock();
    let descriptor = Nonblocking::new(stdin.as_fd())?;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(Error("native legacy CLI interrupted".into()));
        }
        let mut ready = libc::pollfd {
            fd: descriptor.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd lives through this bounded wait.
        let polled = unsafe { libc::poll(&mut ready, 1, 50) };
        if polled == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if polled == 0 {
            continue;
        }
        let length = buffer.len().min(MAXIMUM + 1 - bytes.len());
        // SAFETY: buffer owns length writable bytes and the descriptor is live.
        // O_NONBLOCK also covers readiness races without stranding the worker.
        let read = unsafe { libc::read(ready.fd, buffer.as_mut_ptr().cast(), length) };
        if read == -1 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(error.into());
        }
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read as usize]);
        if bytes.len() > MAXIMUM {
            return Err(Error(format!("input exceeds {MAXIMUM} bytes")));
        }
    }
    String::from_utf8(bytes).map_err(|error| Error(error.to_string()))
}

fn serialization_path(database: &Path) -> Result<PathBuf> {
    // Resolve the same existing database path before taking the metadata lock.
    // A missing final component still reaches Store's ordinary admission below.
    let path = database.canonicalize().or_else(|_| {
        let absolute = std::path::absolute(database)?;
        Ok::<_, std::io::Error>(
            absolute
                .parent()
                .unwrap()
                .canonicalize()?
                .join(absolute.file_name().unwrap()),
        )
    })?;
    let mut path = path.into_os_string();
    path.push(".plex-metadata");
    Ok(path.into())
}

pub fn mapping(a: &Value, cancel: &AtomicBool) -> Result<Value> {
    let raw = definition_text(text(a, "definition"), cancel)?;
    let definition = catabolic_core::decode_json(&raw)?;
    if definition["version"].as_f64() == Some(2.0) {
        return catabolic_store::publication_mappings::run_with_account(
            Path::new(text(a, "db")),
            text(a, "profile"),
            &definition,
            &json!({"apply":a["operation"] == "mapping-apply", "expected_plan":a["expected_plan"], "max_removals":a.get("max_removals").unwrap_or(&json!(0))}),
            &mut |destination| {
                catabolic_runtime::imports::mapping_identity(
                    destination,
                    &std::env::current_exe()?,
                    cancel,
                )
            },
        );
    }
    if definition["version"].as_f64() != Some(3.0) {
        return catabolic_runtime::destination_mappings::run(
            Path::new(text(a, "db")),
            text(a, "profile"),
            &json!({"definition":definition,"apply":a["operation"]=="mapping-apply",
                "expected_plan":a["expected_plan"],"max_removals":a.get("max_removals").cloned().unwrap_or(json!(0))}),
            cancel,
        );
    }
    let apply = a["operation"] == "mapping-apply";
    let database = Path::new(text(a, "db"));
    let _serialization = if apply {
        Some(
            catabolic_store::writer_lock(&serialization_path(database)?)
                .map_err(|_| Error("metadata_writer_busy_preview_again".into()))?,
        )
    } else {
        None
    };
    if apply && text(a, "expected_plan").is_empty() {
        return Err(Error("mapping_apply_requires_expected_plan".into()));
    }
    let mut request = json!({"definition":definition,"apply":apply});
    if apply {
        request["expected_plan"] = a["expected_plan"].clone();
    }
    // Frozen version-three dispatch intentionally ignores --max-removals. The
    // HTTP owner supplies its existing default max_changes=100 and transaction.
    let store = Store::open(database, apply, false)?;
    catabolic_store::http_mappings::execute(store, text(a, "profile"), "run", &request)
}

/// The local CLI has the same catalog-owner authority as frozen Store reads.
/// Do not route this through the scoped, operation-filtered HTTP endpoint.
pub fn history(a: &Value, cancel: &AtomicBool) -> Result<Value> {
    let database = Path::new(text(a, "db"));
    let recover = a["operation"] == "mapping-recover";
    if recover {
        catabolic_runtime::destination_mappings::recover(database, text(a, "profile"), a, cancel)?
            .map_or_else(
                || {
                    Err(Error(
                        catabolic_store::publication_mappings::PUBLICATION_GATE.into(),
                    ))
                },
                Ok,
            )
    } else {
        catabolic_store::mapping_history::events(
            Store::open(database, false, false)?,
            text(a, "profile"),
            a["event"].as_str(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdin_status_guard_restores_original_flags() {
        let file = tempfile::tempfile().unwrap();
        // SAFETY: file owns a live descriptor through this test.
        let before = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        assert_ne!(before, -1);
        {
            let guard = Nonblocking::new(file.as_fd()).unwrap();
            // SAFETY: guard borrows the live file descriptor.
            let during = unsafe { libc::fcntl(guard.fd.as_raw_fd(), libc::F_GETFL) };
            assert_eq!(during, before | libc::O_NONBLOCK);
        }
        // SAFETY: file remains open after the non-owning status guard drops.
        let after = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(after, before);
    }
}
