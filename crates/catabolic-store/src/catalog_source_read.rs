//! Descriptor authority minted from a current native occurrence, never path JSON.
//! This is a privileged retained-host read. Application ACLs remain the host's
//! responsibility, and every new use must revalidate under its current fence.
use crate::{Error, Result, Store, catalog_projection::TrustedProjectionAuthority};
use catabolic_core::catalog_identity::ExactFileRevision;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::{fs::File, os::unix::fs::MetadataExt};

pub struct ExactSource {
    file: File,
    catalog: String,
    incarnation: String,
    profile: String,
    source: String,
    exact: ExactFileRevision,
    snapshot: Value,
    policy: Value,
}

fn identity(m: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
fn facts(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    profile: &str,
    source: &str,
    exact: &ExactFileRevision,
) -> Result<(Value, Value)> {
    crate::query::name(profile)?;
    crate::query::name(source)?;
    if exact.file_id.is_empty() || exact.file_id.len() > 256 || exact.revision.len() != 64 {
        return Err(Error("invalid exact source selection".into()));
    }
    let (catalog, incarnation, generation): (String, String, i64) = store.db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if catalog != authority.catalog_id
        || incarnation != authority.incarnation
        || generation != authority.expected_generation
    {
        return Err(Error("exact source identity or generation changed".into()));
    }
    let mut snapshot = crate::components::occurrence(&store.db, profile, &exact.file_id)?;
    if snapshot["location"] != source
        || snapshot["status"] != "present"
        || crate::components::revision(&snapshot) != exact.revision
    {
        return Err(Error("exact source occurrence changed".into()));
    }
    let ctime: Option<i64> = store.db.query_row(
        "SELECT c.ctime_ns FROM native_source_ctimes c JOIN observations o ON o.profile=c.profile AND o.file_id=c.file_id AND o.size=c.size AND o.mtime_ns=c.mtime_ns AND o.device=c.device AND o.inode=c.inode WHERE c.profile=? AND c.file_id=? AND o.status='present'",
        [profile, exact.file_id.as_str()], |r| r.get(0),
    ).optional()?;
    snapshot["ctime_ns"] =
        json!(ctime.ok_or_else(|| Error("exact source requires native ctime evidence".into()))?);
    let policy =
        crate::observation::execute(store, profile, "scan_policy", &json!({"source":source}))?;
    Ok((snapshot, policy))
}

impl ExactSource {
    pub fn open(
        store: &Store,
        authority: &TrustedProjectionAuthority,
        profile: &str,
        source: &str,
        exact: &ExactFileRevision,
    ) -> Result<Self> {
        if store.schema_version < 40 || !store.db.is_autocommit() {
            return Err(Error(
                "exact source requires a native top-level snapshot".into(),
            ));
        }
        store.transaction(|_| {
            let (snapshot, policy) = facts(store, authority, profile, source, exact)?;
            // Ordinary UUID validation may launch a platform helper. This
            // capability has no child custodian, so never allow that fallback.
            let file = crate::source::without_children(|| {
                crate::source::with_validated_source(&snapshot, |file| Ok(file.try_clone()?))
            })?;
            Ok(Self {
                file,
                catalog: authority.catalog_id.clone(),
                incarnation: authority.incarnation.clone(),
                profile: profile.into(),
                source: source.into(),
                exact: exact.clone(),
                snapshot,
                policy,
            })
        })
    }

    /// Reopen only through this capability's native anchored occurrence guard.
    /// The callback-local descriptor has a fresh open-file description. Its
    /// duplicate does NOT share an offset with this capability or another read:
    /// the callback-local original is dropped as this call returns.
    pub fn descriptor(&self) -> Result<File> {
        crate::source::without_children(|| {
            crate::source::with_validated_source(&self.snapshot, |fresh| {
                if identity(&fresh.metadata()?) != identity(&self.file.metadata()?) {
                    return Err(Error("exact source descriptor changed".into()));
                }
                Ok(fresh.try_clone()?)
            })
        })
    }

    pub fn validate(&self, store: &Store, authority: &TrustedProjectionAuthority) -> Result<()> {
        if self.catalog != authority.catalog_id
            || self.incarnation != authority.incarnation
            || !store.db.is_autocommit()
        {
            return Err(Error("exact source catalog changed".into()));
        }
        store.transaction(|_| {
            let (snapshot, policy) =
                facts(store, authority, &self.profile, &self.source, &self.exact)?;
            if snapshot != self.snapshot || policy != self.policy {
                return Err(Error(
                    "exact source representation or policy changed".into(),
                ));
            }
            crate::source::without_children(|| {
                crate::source::with_validated_source(&snapshot, |fresh| {
                    if identity(&fresh.metadata()?) != identity(&self.file.metadata()?) {
                        return Err(Error("exact source descriptor changed".into()));
                    }
                    Ok(())
                })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek};
    fn capability() -> (tempfile::TempDir, ExactSource) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.mp4");
        std::fs::write(&path, b"0123456789").unwrap();
        let stat = std::fs::metadata(&path).unwrap();
        let root = std::fs::metadata(dir.path()).unwrap();
        let snapshot = json!({"status":"present","root":dir.path(),"path":"input.mp4","size":stat.len(),
            "mtime_ns":stat.mtime()*1_000_000_000+stat.mtime_nsec(),"ctime_ns":stat.ctime()*1_000_000_000+stat.ctime_nsec(),
            "device":stat.dev(),"inode":stat.ino(),"root_device":root.dev(),"root_inode":root.ino(),"source_policy":{"settings":{}}});
        let file = crate::source::without_children(|| {
            crate::source::with_validated_source(&snapshot, |f| Ok(f.try_clone()?))
        })
        .unwrap();
        (
            dir,
            ExactSource {
                file,
                catalog: "catalog".into(),
                incarnation: "incarnation".into(),
                profile: "default".into(),
                source: "source".into(),
                exact: ExactFileRevision {
                    file_id: "file".into(),
                    revision: "a".repeat(64),
                },
                snapshot,
                policy: json!({}),
            },
        )
    }
    #[test]
    fn independent_reads_do_not_share_open_file_description_offsets() {
        let (_dir, source) = capability();
        let mut first = source.descriptor().unwrap();
        let mut second = source.descriptor().unwrap();
        let mut one = [0; 3];
        let mut two = [0; 3];
        first.read_exact(&mut one).unwrap();
        second.read_exact(&mut two).unwrap();
        assert_eq!(&one, b"012");
        assert_eq!(one, two);
        first.seek(std::io::SeekFrom::Start(8)).unwrap();
        second.read_exact(&mut two).unwrap();
        assert_eq!(&two, b"345");
        assert_eq!(
            source.file.try_clone().unwrap().stream_position().unwrap(),
            0
        );
    }
    #[test]
    fn replaced_and_in_place_mutated_sources_never_reopen_from_old_capability() {
        let (dir, source) = capability();
        std::fs::rename(dir.path().join("input.mp4"), dir.path().join("old.mp4")).unwrap();
        std::fs::write(dir.path().join("input.mp4"), b"0123456789").unwrap();
        assert!(source.descriptor().is_err());
        let (dir, source) = capability();
        std::fs::write(dir.path().join("input.mp4"), b"changed123").unwrap();
        assert!(source.descriptor().is_err());
    }
}
