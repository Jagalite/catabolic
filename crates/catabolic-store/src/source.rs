//! Descriptor-only validation for read-only rendition planning. No source bytes
//! are changed or hashed; root and file identities are fenced before and after.
use crate::{Error, Result};
pub(crate) mod relocation_uuid;
#[derive(Clone, Copy)]
enum RootProbe<'a> {
    Legacy,
    Relocation(
        &'a crate::operation_budget::OperationBudget,
        &'a std::sync::atomic::AtomicBool,
    ),
}
use serde_json::{Value, json};
use std::{
    ffi::CString,
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::Path,
};
// This private scope is part of the closed observation protocol, not a general
// operating-system spawn interceptor. A blocked attempt poisons the completion
// acknowledgement even if a higher layer converts validation failure to a result.
thread_local! {
    static NO_CHILD: std::cell::Cell<(bool, bool)> = const { std::cell::Cell::new((false, false)) };
}
pub(crate) fn children_forbidden() -> bool {
    NO_CHILD.with(|state| state.get().0)
}
pub(crate) fn without_children<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    struct Restore((bool, bool));
    impl Drop for Restore {
        fn drop(&mut self) {
            NO_CHILD.with(|state| {
                let attempted = state.get().1;
                state.set((self.0.0, self.0.1 || (self.0.0 && attempted)));
            });
        }
    }
    let previous = NO_CHILD.with(|state| state.replace((true, false)));
    let restore = Restore(previous);
    let result = operation();
    let attempted = NO_CHILD.with(|state| state.get().1);
    drop(restore);
    if attempted {
        Err(Error("closed_observation_child_spawn_refused".into()))
    } else {
        result
    }
}
#[cfg(target_os = "linux")]
fn child_spawn_refused() -> bool {
    NO_CHILD.with(|state| {
        let (forbidden, attempted) = state.get();
        state.set((forbidden, attempted || forbidden));
        forbidden
    })
}
fn open_at(parent: i32, name: &str, directory: bool) -> Result<File> {
    let name = CString::new(name).map_err(|_| Error("invalid source path".into()))?;
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | libc::O_CLOEXEC
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: CString is live; openat returns a new owned descriptor or -1.
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful openat transfers a fresh descriptor to this File.
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub(crate) fn open_root(root: &str) -> Result<File> {
    if !Path::new(root).is_absolute() || root.split('/').any(|p| p == "..") {
        return Err(Error(
            "root must be absolute and contain no parent traversal".into(),
        ));
    }
    let mut directory = File::open("/")?;
    for part in root.split('/').filter(|p| !p.is_empty()) {
        directory = open_at(directory.as_raw_fd(), part, true)?;
    }
    Ok(directory)
}
fn revision(file: &File) -> Result<Value> {
    let stat = file.metadata()?;
    Ok(
        json!({"size":stat.size(),"mtime_ns":i128::from(stat.mtime())*1_000_000_000+i128::from(stat.mtime_nsec()),"ctime_ns":i128::from(stat.ctime())*1_000_000_000+i128::from(stat.ctime_nsec()),"device":stat.dev(),"inode":stat.ino()}),
    )
}
#[cfg(target_os = "macos")]
pub(crate) fn volume_uuid(file: &File) -> Option<String> {
    let mut attributes = libc::attrlist {
        bitmapcount: 5,
        reserved: 0,
        commonattr: 0,
        volattr: 0x80040000,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut result = [0u8; 20];
    // SAFETY: SDK attrlist and fixed result buffer have the requested sizes.
    let rc = unsafe {
        libc::fgetattrlist(
            file.as_raw_fd(),
            (&mut attributes as *mut libc::attrlist).cast(),
            result.as_mut_ptr().cast(),
            result.len(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let id = uuid::Uuid::from_slice(&result[4..]).ok()?;
    (!id.is_nil()).then(|| id.to_string())
}
#[cfg(target_os = "linux")]
pub(crate) fn volume_uuid(file: &File) -> Option<String> {
    if child_spawn_refused() {
        return None;
    }
    let root = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()?;
    let mut child = catabolic_core::spawn_guard::spawn(
        std::process::Command::new("findmnt")
            .args(["--json", "--target"])
            .arg(root)
            .args(["--output", "UUID"])
            .stdout(std::process::Stdio::piped()),
    )
    .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if child.try_wait().ok()?.is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().ok()?;
    if output.stdout.len() > 65536 {
        return None;
    }
    let value: Value = catabolic_core::decode_json_bytes(&output.stdout).ok()?;
    let rows = value["filesystems"].as_array()?;
    if rows.len() != 1 {
        return None;
    }
    rows[0]["uuid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
}
fn checked_root(snapshot: &Value, probe: RootProbe<'_>) -> Result<File> {
    let path = snapshot["root"].as_str().ok_or_else(|| {
        Error("source is not recorded present; scan the mounted source first".into())
    })?;
    let root = open_root(path)
        .map_err(|error| Error(format!("unavailable source root: {path}: {error}")))?;
    let metadata = root.metadata()?;
    let settings = &snapshot["source_policy"]["settings"];
    for (key, actual, expected) in [
        ("device", metadata.dev(), &snapshot["root_device"]),
        ("inode", metadata.ino(), &snapshot["root_inode"]),
    ] {
        if settings[key] != "skip" && expected.as_u64() != Some(actual) {
            return Err(Error(format!(
                "root identity changed: {path}; inspect remount repair"
            )));
        }
    }
    if settings["uuid"] != "skip" && !snapshot["volume_uuid"].is_null() {
        match probe {
            RootProbe::Legacy => {
                if volume_uuid(&root).as_deref() != snapshot["volume_uuid"].as_str() {
                    return Err(Error("bound volume identity changed or unavailable".into()));
                }
            }
            RootProbe::Relocation(budget, cancel) => {
                budget
                    .charge(crate::operation_budget::BudgetCharge {
                        work: 1,
                        values: 1,
                        bytes: 36,
                        ..Default::default()
                    })
                    .map_err(|e| Error(format!("relocation UUID parsing budget: {e:?}")))?;
                let expected = relocation_uuid::canonical_enrollment(
                    snapshot["volume_uuid"]
                        .as_str()
                        .ok_or_else(|| Error("malformed source UUID enrollment".into()))?,
                )?;
                relocation_uuid::validate(&root, expected, budget, cancel)?;
            }
        }
    }
    Ok(root)
}
pub fn validate(snapshot: &Value) -> Result<Value> {
    with_validated_source(snapshot, |file| revision(file))
}
pub(crate) fn with_validated_source<T>(
    snapshot: &Value,
    operation: impl FnOnce(&mut File) -> Result<T>,
) -> Result<T> {
    with_validated_source_ctime(snapshot, None, operation)
}

/// Validate the same occurrence with an independently retained expected ctime,
/// without cloning its JSON or allocating a fourth, returned revision object.
/// The override REPLACES any snapshot ctime, exactly like assigning ctime_ns on
/// a cloned snapshot before validate(). It is descriptive, never source authority.
///
/// Callers must precharge the fields actually inspected: status, root, path,
/// size, mtime_ns, device, inode, root_device, root_inode, volume_uuid and
/// source_policy.settings.{device,inode,uuid}, plus expected_ctime. Unused
/// occurrence fields are neither visited nor cloned here.
///
/// For R nonempty root components and P relative path components, the successful
/// non-UUID path allocates three five-number revision JSON objects, one Vec of
/// P borrowed component references and at most 2*(R+P) temporary CStrings. It
/// performs at most 2*(R+1)+2*P opens, one descriptor duplication and ten metadata
/// reads. checked_root runs twice; an enabled volume UUID check retains the
/// existing platform-specific probe and its existing bounds (including Linux
/// closed-observation child refusal). Errors retain the same cleanup behavior.
pub(crate) fn validate_expected_ctime(snapshot: &Value, expected_ctime: i64) -> Result<()> {
    with_validated_source_ctime(snapshot, Some(expected_ctime), |_| Ok(()))
}

fn with_validated_source_ctime<T>(
    snapshot: &Value,
    expected_ctime: Option<i64>,
    operation: impl FnOnce(&mut File) -> Result<T>,
) -> Result<T> {
    with_validated_source_mode(snapshot, expected_ctime, RootProbe::Legacy, operation)
}

/// Fixed relocation caller retains the blocking task and descriptors until all
/// checks settle. Ordinary consumers never select this no-child probe mode.
pub(crate) fn with_validated_source_relocation<T>(
    snapshot: &Value,
    budget: &crate::operation_budget::OperationBudget,
    cancel: &std::sync::atomic::AtomicBool,
    operation: impl FnOnce(&mut File) -> Result<T>,
) -> Result<T> {
    with_validated_source_mode(
        snapshot,
        None,
        RootProbe::Relocation(budget, cancel),
        operation,
    )
}
fn with_validated_source_mode<T>(
    snapshot: &Value,
    expected_ctime: Option<i64>,
    probe: RootProbe<'_>,
    operation: impl FnOnce(&mut File) -> Result<T>,
) -> Result<T> {
    if snapshot["status"] != "present" || snapshot["root"].as_str().is_none_or(str::is_empty) {
        return Err(Error(
            "source is not recorded present; scan the mounted source first".into(),
        ));
    }
    let root = checked_root(snapshot, probe)?;
    let path = snapshot["path"]
        .as_str()
        .ok_or_else(|| Error("invalid source path".into()))?;
    crate::layout::relative_path(path)?;
    let mut parent = root.try_clone()?;
    let mut parts = Vec::with_capacity(path.split('/').count());
    parts.extend(path.split('/'));
    for part in &parts[..parts.len() - 1] {
        parent = open_at(parent.as_raw_fd(), part, true)?;
    }
    let mut file = open_at(parent.as_raw_fd(), parts.last().unwrap(), false)?;
    let before = revision(&file)?;
    if !file.metadata()?.is_file()
        || ["size", "mtime_ns", "device", "inode"]
            .iter()
            .any(|k| before[k] != snapshot[k])
    {
        return Err(Error(
            "source changed since scan; rescan before reading".into(),
        ));
    }
    let ctime_changed = match expected_ctime {
        Some(expected) => before["ctime_ns"].as_i64() != Some(expected),
        None => snapshot.get("ctime_ns").is_some() && before["ctime_ns"] != snapshot["ctime_ns"],
    };
    if ctime_changed {
        return Err(Error(
            "source revision changed; rescan and start a new operation".into(),
        ));
    }
    let result = operation(&mut file)?;
    let current = checked_root(snapshot, probe)?;
    if root.metadata()?.dev() != current.metadata()?.dev()
        || root.metadata()?.ino() != current.metadata()?.ino()
    {
        return Err(Error(
            "source root changed while reading; result discarded".into(),
        ));
    }
    // Re-walk the whole chain from the current root: a pinned parent moved
    // outside the root would still resolve the old leaf by name.
    let mut walked = current;
    for part in &parts[..parts.len() - 1] {
        walked = open_at(walked.as_raw_fd(), part, true)
            .map_err(|_| Error("source changed while reading; result discarded".into()))?;
    }
    let named = open_at(walked.as_raw_fd(), parts.last().unwrap(), false)
        .map_err(|_| Error("source changed while reading; result discarded".into()))?;
    if revision(&file)? != before || revision(&named)? != before {
        return Err(Error(
            "source changed while reading; result discarded".into(),
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod no_child_tests {
    use super::*;
    #[test]
    fn closed_scope_restores_on_error_nested_completion_and_panic() {
        assert!(!children_forbidden());
        without_children(|| {
            assert!(children_forbidden());
            without_children(|| {
                assert!(children_forbidden());
                Ok(())
            })?;
            assert!(children_forbidden());
            Ok(())
        })
        .unwrap();
        assert!(!children_forbidden());
        let failed: Result<()> = without_children(|| Err(Error("injected".into())));
        assert!(failed.is_err());
        assert!(!children_forbidden());
        let panic = std::panic::catch_unwind(|| without_children::<()>(|| panic!("injected")));
        assert!(panic.is_err());
        assert!(!children_forbidden());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn blocked_uuid_subprocess_poisons_even_a_successful_outer_result() {
        let root = tempfile::tempdir().unwrap();
        let file = File::open(root.path()).unwrap();
        let error = without_children(|| {
            assert!(volume_uuid(&file).is_none());
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.0, "closed_observation_child_spawn_refused");
        assert!(!children_forbidden());
        assert!(
            without_children(|| {
                let _ = without_children(|| {
                    assert!(volume_uuid(&file).is_none());
                    Ok(())
                });
                Ok(())
            })
            .is_err()
        );
    }
}

#[cfg(test)]
mod containment_tests {
    use super::*;
    #[test]
    fn a_parent_moved_outside_the_root_discards_the_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/file.bin"), b"bytes").unwrap();
        let stat = std::fs::metadata(root.join("sub/file.bin")).unwrap();
        let root_stat = std::fs::metadata(&root).unwrap();
        let snapshot = json!({
            "status": "present",
            "root": root.to_str().unwrap(),
            "path": "sub/file.bin",
            "size": stat.size(),
            "mtime_ns": i128::from(stat.mtime()) * 1_000_000_000 + i128::from(stat.mtime_nsec()),
            "device": stat.dev(),
            "inode": stat.ino(),
            "root_device": root_stat.dev(),
            "root_inode": root_stat.ino(),
            "source_policy": {"settings": {}},
        });
        with_validated_source(&snapshot, |_| Ok(())).unwrap();
        let error = with_validated_source(&snapshot, |_| {
            std::fs::rename(root.join("sub"), dir.path().join("moved"))?;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.0, "source changed while reading; result discarded");
    }
}

#[cfg(test)]
mod expected_ctime_tests;
