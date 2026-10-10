//! Descriptor-only validation for read-only rendition planning. No source bytes
//! are changed or hashed; root and file identities are fenced before and after.
use crate::{Error, Result};
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
fn open_root(root: &str) -> Result<File> {
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
fn volume_uuid(file: &File) -> Option<String> {
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
fn volume_uuid(file: &File) -> Option<String> {
    let root = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()?;
    let mut child = std::process::Command::new("findmnt")
        .args(["--json", "--target"])
        .arg(root)
        .args(["--output", "UUID"])
        .stdout(std::process::Stdio::piped())
        .spawn()
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
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    let rows = value["filesystems"].as_array()?;
    if rows.len() != 1 {
        return None;
    }
    rows[0]["uuid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
}
fn checked_root(snapshot: &Value) -> Result<File> {
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
    if settings["uuid"] != "skip"
        && !snapshot["volume_uuid"].is_null()
        && volume_uuid(&root).as_deref() != snapshot["volume_uuid"].as_str()
    {
        return Err(Error("bound volume identity changed or unavailable".into()));
    }
    Ok(root)
}
pub fn validate(snapshot: &Value) -> Result<Value> {
    if snapshot["status"] != "present" || snapshot["root"].as_str().is_none_or(str::is_empty) {
        return Err(Error(
            "source is not recorded present; scan the mounted source first".into(),
        ));
    }
    let root = checked_root(snapshot)?;
    let path = snapshot["path"]
        .as_str()
        .ok_or_else(|| Error("invalid source path".into()))?;
    crate::layout::relative_path(path)?;
    let mut parent = root.try_clone()?;
    let parts = path.split('/').collect::<Vec<_>>();
    for part in &parts[..parts.len() - 1] {
        parent = open_at(parent.as_raw_fd(), part, true)?;
    }
    let file = open_at(parent.as_raw_fd(), parts.last().unwrap(), false)?;
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
    if snapshot.get("ctime_ns").is_some() && before["ctime_ns"] != snapshot["ctime_ns"] {
        return Err(Error(
            "source revision changed; rescan and start a new operation".into(),
        ));
    }
    let named = open_at(parent.as_raw_fd(), parts.last().unwrap(), false)?;
    if revision(&file)? != before || revision(&named)? != before {
        return Err(Error(
            "source changed while reading; result discarded".into(),
        ));
    }
    let current = checked_root(snapshot)?;
    if root.metadata()?.dev() != current.metadata()?.dev()
        || root.metadata()?.ino() != current.metadata()?.ino()
    {
        return Err(Error(
            "source root changed while reading; result discarded".into(),
        ));
    }
    Ok(before)
}
