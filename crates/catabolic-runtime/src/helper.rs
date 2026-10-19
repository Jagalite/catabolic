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
static ENVIRONMENT_HELPER: OnceLock<(std::ffi::OsString, Helper)> = OnceLock::new();

/// Pin `CATABOLIC_NATIVE_BINARY` the first time it is used; afterwards the
/// value must be present and unchanged, and the file must still be the
/// identical file. `None` before first use means no override.
fn environment_override(
    cache: &OnceLock<(std::ffi::OsString, Helper)>,
    value: Option<&std::ffi::OsStr>,
) -> Result<Option<PathBuf>> {
    let Some(value) = value else {
        return match cache.get() {
            None => Ok(None),
            Some(_) => Err(Error(
                "CATABOLIC_NATIVE_BINARY removed after first use; restart the process".into(),
            )),
        };
    };
    if cache.get().is_none() {
        let helper = pin(Path::new(value))?;
        let _ = cache.set((value.to_owned(), helper));
    }
    let (first, helper) = cache.get().unwrap();
    if first.as_os_str() != value {
        return Err(Error(
            "CATABOLIC_NATIVE_BINARY changed after first use; restart the process".into(),
        ));
    }
    verified(helper).map(Some)
}
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
    install(Helper { path, identity })
}
fn install(helper: Helper) -> Result<()> {
    if let Some(old) = HELPER.get() {
        if old.path == helper.path && old.identity == helper.identity {
            return Ok(());
        }
        return Err(Error(
            "a different native helper is already registered".into(),
        ));
    }
    HELPER
        .set(helper)
        .map_err(|_| Error("native helper registration raced".into()))
}
/// The pinned executable, if it is still the identical file.
pub(crate) fn verified(helper: &Helper) -> Result<PathBuf> {
    if identify(&helper.path)? != helper.identity {
        return Err(Error(
            "native helper identity changed; restart with the installed artifact".into(),
        ));
    }
    Ok(helper.path.clone())
}

/// Whether a native helper was registered or explicitly provided, so that a
/// later resolution failure must be treated as an error.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn configured() -> bool {
    // A pinned override stays configured even if the variable is removed, so
    // its removal fails closed rather than allowing unsupervised spawns.
    HELPER.get().is_some()
        || ENVIRONMENT_HELPER.get().is_some()
        || std::env::var_os("CATABOLIC_NATIVE_BINARY").is_some()
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
    // Its identity is pinned at first use and verified on every later use.
    if let Some(path) = environment_override(
        &ENVIRONMENT_HELPER,
        std::env::var_os("CATABOLIC_NATIVE_BINARY").as_deref(),
    )? {
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

/// Explicit qualified closed-companion capabilities. These are not negotiated
/// from helper output or a caller-supplied string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClosedObservationProtocol {
    NoChildV1,
    NoChildWithExactJoinV2,
}
/// Explicit trusted packaging grant. The grant is deliberately separate from a
/// digest or generic helper registration; it represents the packaging owner's
/// verification of the versioned built-in no-descendant implementation.
#[derive(Clone)]
pub struct TrustedClosedObservationArtifact {
    helper: Helper,
    digest: String,
    protocol: ClosedObservationProtocol,
}
impl TrustedClosedObservationArtifact {
    /// Trusted packaging boundary, never an application/user-input constructor.
    /// The caller must have verified this exact artifact's closed implementation.
    /// Hashing an unknown program is insufficient authorization for this call.
    pub fn after_packaging_verification(
        path: &Path,
        expected_sha256: &str,
        protocol: ClosedObservationProtocol,
    ) -> Result<Self> {
        if expected_sha256.len() != 64
            || !expected_sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(Error("closed helper requires a lowercase SHA-256".into()));
        }
        let helper = pin(path)?;
        if helper.identity.2 > 128 * 1024 * 1024
            || catabolic_core::sha256(std::fs::read(&helper.path)?) != expected_sha256
            || identify(&helper.path)? != helper.identity
        {
            return Err(Error(
                "closed helper differs from verified packaging artifact".into(),
            ));
        }
        Ok(Self {
            helper,
            digest: expected_sha256.into(),
            protocol,
        })
    }
}
/// Enroll only an explicit trusted packaging grant. Generic registration and
/// environment overrides never acquire the closed-companion capability.
pub fn register_closed_observation(grant: &TrustedClosedObservationArtifact) -> Result<()> {
    let TrustedClosedObservationArtifact {
        helper,
        digest,
        protocol,
    } = grant;
    // The typed grant already verified these bytes before host enrollment. Do
    // not hash the full artifact again while holding a catalog writer: retain
    // its exact metadata/inode fence and install only that verified identity.
    if identify(&helper.path)? != helper.identity {
        return Err(Error(
            "verified closed helper changed before enrollment".into(),
        ));
    }
    install(helper.clone())?;
    if let Some(old) = CLOSED_OBSERVATION.get() {
        if old.helper.path == helper.path
            && old.helper.identity == helper.identity
            && old.digest == *digest
            && old.protocol == *protocol
        {
            return Ok(());
        }
        return Err(Error(
            "a different closed observation helper is already registered".into(),
        ));
    }
    CLOSED_OBSERVATION
        .set(ClosedArtifact {
            helper: helper.clone(),
            digest: digest.clone(),
            protocol: *protocol,
        })
        .map_err(|_| Error("closed helper registration raced".into()))
}
struct ClosedArtifact {
    helper: Helper,
    digest: String,
    protocol: ClosedObservationProtocol,
}
static CLOSED_OBSERVATION: OnceLock<ClosedArtifact> = OnceLock::new();
pub(crate) fn closed_observation() -> Result<(PathBuf, String)> {
    let artifact = CLOSED_OBSERVATION.get().ok_or_else(|| {
        Error("schema37 scan requires an explicitly qualified closed observation helper".into())
    })?;
    if identify(&artifact.helper.path)? != artifact.helper.identity {
        return Err(Error("closed observation helper identity changed".into()));
    }
    Ok((artifact.helper.path.clone(), artifact.digest.clone()))
}
pub(crate) fn closed_shared_observation() -> Result<(PathBuf, String)> {
    if CLOSED_OBSERVATION
        .get()
        .is_none_or(|a| a.protocol != ClosedObservationProtocol::NoChildWithExactJoinV2)
    {
        return Err(Error(
            "shared join requires the explicit exact-join packaging capability".into(),
        ));
    }
    closed_observation()
}
/// Digest the executable selected by the actual helper process. No PATH lookup.
pub(crate) fn current_digest() -> Result<String> {
    let path = std::env::current_exe()?;
    let before = identify(&path)?;
    if before.2 > 128 * 1024 * 1024 {
        return Err(Error("closed helper exceeds digest bound".into()));
    }
    let digest = catabolic_core::sha256(std::fs::read(&path)?);
    if identify(&path)? != before {
        return Err(Error("closed helper changed during digest".into()));
    }
    Ok(digest)
}

#[cfg(test)]
mod closed_grant_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn changed_artifact_cannot_reuse_a_preverified_packaging_grant() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let digest = catabolic_core::sha256(std::fs::read(&path).unwrap());
        let grant = TrustedClosedObservationArtifact::after_packaging_verification(
            &path,
            &digest,
            ClosedObservationProtocol::NoChildV1,
        )
        .unwrap();
        std::fs::write(&path, "#!/bin/sh\n# changed bytes\nexit 0\n").unwrap();
        assert!(
            register_closed_observation(&grant)
                .unwrap_err()
                .0
                .contains("changed before enrollment")
        );
    }
}

#[cfg(test)]
mod environment_tests {
    use super::*;
    #[test]
    fn environment_override_is_pinned_at_first_use() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("helper");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cache = OnceLock::new();
        assert!(environment_override(&cache, None).unwrap().is_none());
        let first = environment_override(&cache, Some(path.as_os_str())).unwrap();
        assert_eq!(
            environment_override(&cache, Some(path.as_os_str())).unwrap(),
            first
        );
        // Removed after first use: refused, never an unsupervised fallback.
        assert!(environment_override(&cache, None).is_err());
        // Replaced between commands: refused.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"#!/bin/sh\nexec \"$@\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(environment_override(&cache, Some(path.as_os_str())).is_err());
        // A different value after first use: refused.
        let other = dir.path().join("other");
        std::fs::copy(&path, &other).unwrap();
        assert!(environment_override(&cache, Some(other.as_os_str())).is_err());
    }
}
