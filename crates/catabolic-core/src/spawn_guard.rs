// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Process-wide coordination between child-process creation and descriptors
//! that cannot be created close-on-exec atomically (macOS has no `pipe2`).
//!
//! Every in-workspace spawner forks while holding the shared side, through
//! [`spawn`]. Code that must set `FD_CLOEXEC` after creating a descriptor holds
//! the exclusive side, so no in-workspace fork can inherit the descriptor in
//! between. Spawners outside this workspace (for example an embedding host's own
//! subprocesses) are not coordinated.
use std::{
    process::{Child, Command},
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
};

static LOCK: RwLock<()> = RwLock::new(());

/// Hold while forking a child process. Never hold it while waiting for one.
pub fn spawning() -> RwLockReadGuard<'static, ()> {
    LOCK.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Hold while creating descriptors that become close-on-exec in a second step.
pub fn descriptors() -> RwLockWriteGuard<'static, ()> {
    LOCK.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `Command::spawn` under the shared side; the guard is released on return.
pub fn spawn(command: &mut Command) -> std::io::Result<Child> {
    let _spawning = spawning();
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spawn_releases_the_shared_side_before_returning() {
        let mut child = spawn(Command::new("/bin/sh").args(["-c", "exit 0"])).unwrap();
        // The exclusive side is immediately available again.
        drop(descriptors());
        assert!(child.wait().unwrap().success());
    }
}
