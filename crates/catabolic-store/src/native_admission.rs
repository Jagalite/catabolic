// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Cooperative native lifecycle quiescence, separate from single-writer admission.
//! This cannot prove that an unsupported legacy process has stopped. Operators
//! must establish that separately before native upgrade or controlled restore.
use crate::{Error, OpenError, OpenErrorCode, OpenResult, Result};
use std::{
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

pub struct NativeAdmissionLock {
    file: File,
    path: PathBuf,
    device: u64,
    inode: u64,
    creator_pid: u32,
}

impl NativeAdmissionLock {
    /// Refuse a replaced lock pathname instead of silently claiming quiescence.
    pub(crate) fn validate_path(&self) -> Result<()> {
        if std::process::id() != self.creator_pid {
            return Err(Error(
                "native catalog inherited across fork; fresh admission required".into(),
            ));
        }
        let metadata = std::fs::symlink_metadata(&self.path)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
        {
            return Err(Error(
                "native catalog admission lock was replaced; reopen required".into(),
            ));
        }
        Ok(())
    }
}

impl Drop for NativeAdmissionLock {
    fn drop(&mut self) {
        // SAFETY: this guard owns the live descriptor and its flock. Explicit
        // unlock avoids inheriting the open-file-description lock across fork.
        if std::process::id() == self.creator_pid {
            // SAFETY: only the creating process explicitly releases its owned flock.
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

/// Private single-attempt acquisition slots. A successfully returned File is
/// installed before metadata/flock, and the held lease before path validation.
pub(crate) enum NativeAdmissionAcquisition {
    Unopened(PathBuf),
    Opened { file: File, path: PathBuf },
    Locked(NativeAdmissionLock),
}
impl NativeAdmissionAcquisition {
    pub(crate) const SUFFIX: &'static str = ".native-admission.lock";
    pub(crate) fn new(database: &Path) -> Self {
        let mut name =
            std::ffi::OsString::with_capacity(database.as_os_str().len() + Self::SUFFIX.len());
        name.push(database);
        name.push(Self::SUFFIX);
        Self::Unopened(PathBuf::from(name))
    }
    pub(crate) fn path_capacity(&self) -> usize {
        match self {
            Self::Unopened(path) | Self::Opened { path, .. } => path.capacity(),
            Self::Locked(lock) => lock.path.capacity(),
        }
    }
    pub(crate) fn open(&mut self) -> Result<()> {
        let Self::Unopened(path) = self else {
            return Err(Error(
                "native preparation lifecycle open already attempted".into(),
            ));
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&*path)?;
        // Only moves between successful open and stable ownership.
        *self = Self::Opened {
            file,
            path: std::mem::take(path),
        };
        Ok(())
    }
    pub(crate) fn lock(&mut self) -> Result<()> {
        let Self::Opened { file, .. } = self else {
            return Err(Error("native preparation lifecycle file missing".into()));
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(Error(
                "native catalog admission lock must be a regular file with one link".into(),
            ));
        }
        let creator_pid = std::process::id();
        // SAFETY: the caller-owned acquisition slot retains this live File.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // No fallible call, allocation or poll intervenes after successful flock.
        match std::mem::replace(self, Self::Unopened(PathBuf::new())) {
            Self::Opened { file, path } => {
                *self = Self::Locked(NativeAdmissionLock {
                    file,
                    path,
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    creator_pid,
                });
                Ok(())
            }
            original => {
                *self = original;
                Err(Error("native preparation lifecycle state changed".into()))
            }
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        match self {
            Self::Locked(lock) => lock.validate_path(),
            _ => Err(Error("native preparation lifecycle lease missing".into())),
        }
    }
    #[cfg(test)]
    pub(crate) fn descriptor_for_test(&self) -> Option<std::os::fd::RawFd> {
        match self {
            Self::Unopened(_) => None,
            Self::Opened { file, .. } => Some(file.as_raw_fd()),
            Self::Locked(lock) => Some(lock.file.as_raw_fd()),
        }
    }
}

fn lock(path: &Path, exclusive: bool) -> OpenResult<NativeAdmissionLock> {
    let mut name = path.as_os_str().to_os_string();
    name.push(".native-admission.lock");
    let path = PathBuf::from(name);
    let create = || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
    };
    let file = if exclusive {
        create()?
    } else {
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => create()?,
            Err(error) => return Err(error.into()),
        }
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(OpenError::invalid(Error(
            "native catalog admission lock must be a regular file with one link".into(),
        )));
    }
    let operation = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    // SAFETY: file owns a valid descriptor for the duration of this call.
    if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Err(OpenError::new(
                OpenErrorCode::BusyWriter,
                Error("native catalog sessions or exclusive lifecycle operation are active".into()),
            ));
        }
        return Err(error.into());
    }
    let guard = NativeAdmissionLock {
        file,
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
        creator_pid: std::process::id(),
    };
    guard.validate_path().map_err(OpenError::invalid)?;
    Ok(guard)
}

pub(crate) fn native_admission_shared(path: &Path) -> OpenResult<NativeAdmissionLock> {
    lock(path, false)
}

/// Acquire a nonblocking exclusive lifecycle lease. Callers must also acquire
/// the existing writer lock, actual SQLite exclusion, and reconcile durable work.
/// This function neither restores bytes nor expands recovery capabilities.
pub fn native_admission_exclusive(path: &Path) -> Result<NativeAdmissionLock> {
    lock(path, true).map_err(Into::into)
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn existing_shared_sidecar_uses_read_only_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("catalog.sqlite3");
        let creator = native_admission_exclusive(&database).unwrap();
        let sidecar = creator.path.clone();
        drop(creator);
        std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o400)).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o500)).unwrap();

        // Restore directory permissions before asserting so cleanup also works
        // when this test runs under an unprivileged account.
        let opened = native_admission_shared(&database);
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let shared = opened.unwrap();
        shared.validate_path().unwrap();
        // Checking descriptor mode catches O_RDWR regressions even as root.
        // SAFETY: shared owns the live file descriptor being queried.
        let flags = unsafe { libc::fcntl(shared.file.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
        assert!(native_admission_exclusive(&database).is_err());
    }

    #[test]
    fn inherited_drop_preserves_parent_lease_and_creator_drop_releases_it() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "native_admission::lifecycle_tests::fork_drop_helper",
                "--test-threads=1",
            ])
            .env("CATABOLIC_NATIVE_ADMISSION_FORK_HELPER", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(stdout.contains("1 passed"), "{stdout}");
    }

    #[test]
    #[ignore = "run only through the isolated subprocess wrapper"]
    fn fork_drop_helper() {
        if std::env::var_os("CATABOLIC_NATIVE_ADMISSION_FORK_HELPER").is_none() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("catalog.sqlite3");
        let guard = native_admission_shared(&database).unwrap();
        let passive = guard.file.try_clone().unwrap();
        let mut ready = [-1; 2];
        let mut finish = [-1; 2];
        // SAFETY: each array provides storage for two newly allocated pipe fds.
        assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
        // SAFETY: finish provides storage for two newly allocated pipe fds.
        assert_eq!(unsafe { libc::pipe(finish.as_mut_ptr()) }, 0);
        // SAFETY: the helper runs alone in its subprocess; no SQLite connection
        // or concurrent application work exists, and the child uses _exit.
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            // SAFETY: these are the unused ends of the child's inherited pipes.
            unsafe {
                libc::close(ready[0]);
                libc::close(finish[1]);
            }
            let rejected = u8::from(guard.validate_path().is_err());
            drop(guard); // Must not LOCK_UN the live parent's description.
            // SAFETY: the pipe ends are live, buffers are one byte, and _exit
            // terminates the child without inherited Rust destructor effects.
            unsafe {
                if libc::write(ready[1], (&rejected as *const u8).cast(), 1) != 1 {
                    libc::_exit(2);
                }
                let mut waiting = libc::pollfd {
                    fd: finish[0],
                    events: libc::POLLIN,
                    revents: 0,
                };
                if libc::poll(&mut waiting, 1, 10000) != 1 {
                    libc::_exit(3);
                }
                let mut release = 0_u8;
                if libc::read(finish[0], (&mut release as *mut u8).cast(), 1) != 1 {
                    libc::_exit(4);
                }
                // Retain this passive inherited duplicate until the creator
                // has proved that its own drop explicitly releases the flock.
                let _ = passive.as_raw_fd();
                libc::_exit(0);
            }
        }
        // SAFETY: these are the parent's unused pipe ends.
        unsafe {
            libc::close(ready[1]);
            libc::close(finish[0]);
        }
        drop(passive);
        let mut rejected = 0_u8;
        let mut waiting = libc::pollfd {
            fd: ready[0],
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: waiting points to one initialized pollfd with a live pipe fd.
        let signaled = unsafe { libc::poll(&mut waiting, 1, 10000) };
        // SAFETY: only read after poll signaled, into a writable one-byte buffer.
        let read = if signaled == 1 {
            // SAFETY: ready[0] is readable and rejected is a one-byte buffer.
            unsafe { libc::read(ready[0], (&mut rejected as *mut u8).cast(), 1) }
        } else {
            -1
        };
        let child_did_not_unlock = native_admission_exclusive(&database).is_err();
        drop(guard);
        let parent_released = native_admission_exclusive(&database).is_ok();
        let release = 1_u8;
        // SAFETY: notify the live child through its pipe before waiting; on
        // timeout, terminate that exact child instead of leaving the test hung.
        unsafe {
            if signaled == 1 {
                libc::write(finish[1], (&release as *const u8).cast(), 1);
            } else {
                libc::kill(child, libc::SIGKILL);
            }
            libc::close(ready[0]);
            libc::close(finish[1]);
        }
        let mut status = 0;
        // SAFETY: child is the pid created above; status is valid output storage.
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert_eq!(read, 1);
        assert_eq!(rejected, 1, "inherited admission was not refused");
        assert!(
            child_did_not_unlock,
            "child drop released the parent's lease"
        );
        assert!(
            parent_released,
            "passive fork duplicate retained the creator's lease"
        );
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
    }
}
