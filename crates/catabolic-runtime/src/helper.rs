// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Explicit native subprocess helper identity; never reinterpret a Python host
//! executable as a Catabolic worker and never fall back to Python business logic.
use catabolic_store::{Error, Result};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::OnceLock,
};
#[derive(Clone)]
pub(crate) struct Helper {
    path: PathBuf,
    identity: (u64, u64, u64, i64, i64, i64, i64),
}
static HELPER: OnceLock<Helper> = OnceLock::new();
fn identify(path: &Path) -> Result<(u64, u64, u64, i64, i64, i64, i64)> {
    let m = path.metadata()?;
    if !m.is_file() || m.mode() & 0o111 == 0 {
        return Err(Error(
            "native helper must be an executable regular file".into(),
        ));
    }
    Ok((
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    ))
}
pub fn register(path: &Path, expected_sha256: Option<&str>) -> Result<()> {
    let path = path.canonicalize()?;
    let identity = identify(&path)?;
    if let Some(expected) = expected_sha256 {
        if identity.2 > 128 * 1024 * 1024
            || catabolic_core::sha256(std::fs::read(&path)?) != expected
        {
            return Err(Error(
                "installed native helper does not match its packaged digest".into(),
            ));
        }
        if identify(&path)? != identity {
            return Err(Error("native helper changed while validating".into()));
        }
    }
    if let Some(old) = HELPER.get() {
        if old.path == path && old.identity == identity {
            return Ok(());
        }
        return Err(Error(
            "a different native helper is already registered".into(),
        ));
    }
    HELPER
        .set(Helper { path, identity })
        .map_err(|_| Error("native helper registration raced".into()))
}
pub(crate) fn pin(path: &Path) -> Result<Helper> {
    let path = path.canonicalize()?;
    let identity = identify(&path)?;
    Ok(Helper { path, identity })
}

pub fn executable() -> Result<PathBuf> {
    executable_with_fallback(None)
}

pub(crate) fn executable_with_fallback(fallback: Option<&Helper>) -> Result<PathBuf> {
    if let Some(helper) = HELPER.get() {
        if identify(&helper.path)? != helper.identity {
            return Err(Error(
                "native helper identity changed; restart with the installed artifact".into(),
            ));
        }
        return Ok(helper.path.clone());
    }
    // Explicit test/embedding override only, never an ambient PATH lookup.
    if let Some(path) = std::env::var_os("CATABOLIC_NATIVE_BINARY") {
        let path = PathBuf::from(path).canonicalize()?;
        identify(&path)?;
        return Ok(path);
    }
    if let Some(helper) = fallback {
        if identify(&helper.path)? != helper.identity {
            return Err(Error(
                "native helper identity changed; restart with the installed artifact".into(),
            ));
        }
        return Ok(helper.path.clone());
    }
    Err(Error("native helper unavailable; install the qualified companion wheel or provide CATABOLIC_NATIVE_BINARY explicitly".into()))
}
