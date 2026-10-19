// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! New non-overwriting export snapshots; never adopt or replace a destination.
use crate::{
    Error, Result, Store,
    filesystem_mutation::{identity, open_at, open_root, parent},
    migration::rows,
};
use catabolic_core::encode;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    fs::File,
    io::Write,
    os::fd::{AsRawFd, FromRawFd},
    path::{Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization;
fn fail<T>(message: &str) -> Result<T> {
    Err(Error(message.into()))
}
fn text(value: &Value) -> Result<&str> {
    value
        .as_str()
        .ok_or_else(|| Error("invalid export artifact".into()))
}
fn array(value: &Value) -> Result<&[Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| Error("invalid export document".into()))
}
fn cname(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| Error("invalid filesystem path".into()))
}
fn key(value: &str) -> String {
    crate::sql::casefold(&value.nfc().collect::<String>())
}

/// Shared with official application import preflight. Paths and all bound-root
/// ancestor identities are checked before the caller creates any destination.
pub(crate) fn destination(
    store: &Store,
    profile: &str,
    owner: &str,
    path: &Path,
    existing: bool,
) -> Result<(PathBuf, File)> {
    let path = std::path::absolute(path)?;
    if store.path.starts_with(&path) {
        return fail("a source or output cannot contain the database");
    }
    let bindings = rows(&store.db, "SELECT * FROM bindings", &[])?;
    for binding in &bindings {
        if binding["profile"] == profile && binding["kind"] == "output" && binding["owner"] == owner
        {
            continue;
        }
        let other = Path::new(text(&binding["root"])?);
        if path.starts_with(other) || other.starts_with(&path) {
            return Err(Error(format!(
                "root overlaps registered {}: {}",
                text(&binding["kind"])?,
                other.display()
            )));
        }
    }
    let checked = if existing && path.exists() {
        path.as_path()
    } else {
        path.parent()
            .ok_or_else(|| Error("output parent is missing".into()))?
    };
    let fd = open_root(
        checked
            .to_str()
            .ok_or_else(|| Error("paths must be valid UTF-8".into()))?,
    )?;
    let mut current = fd.try_clone()?;
    loop {
        let id = identity(&current.metadata()?);
        for binding in &bindings {
            if binding["device"].as_u64() == Some(id.0) && binding["inode"].as_u64() == Some(id.1) {
                return Err(Error(format!(
                    "default output overlaps registered {}: {}",
                    text(&binding["kind"])?,
                    text(&binding["root"])?
                )));
            }
        }
        let next = open_at(&current, "..", true)?;
        if identity(&next.metadata()?) == id {
            break;
        }
        current = next;
    }
    Ok((path, fd))
}
fn relative(source: &Path, output: &Path) -> Result<String> {
    let source: Vec<_> = source.components().collect();
    let output: Vec<_> = output.components().collect();
    let common = source
        .iter()
        .zip(&output)
        .take_while(|(a, b)| a == b)
        .count();
    let mut path = PathBuf::new();
    for _ in &output[common..] {
        path.push("..");
    }
    for component in &source[common..] {
        path.push(component.as_os_str());
    }
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error("paths must be valid UTF-8".into()))
}

/// Read-only output/source preflight shared by mapping previews and publication.
/// The returned descriptors do not create or adopt any destination.
pub(crate) struct Preflight {
    destination: PathBuf,
    directory: File,
    artifacts: Vec<Value>,
    sources: BTreeMap<String, PathBuf>,
}
pub(crate) fn preflight(
    store: &Store,
    profile: &str,
    document: &Value,
    export: &Value,
    output: &Path,
) -> Result<Preflight> {
    if !store.writable || !store.owns_writer_lock() {
        return fail("bundle publication requires the catalog writer lock");
    }
    let (destination, directory) = destination(store, profile, "export", output, false)?;
    let content = &document["content"];
    let entries = array(&content["entries"])?;
    let mut artifacts = array(&export["files"])?.to_vec();
    artifacts.push(json!({"path":"catalog-manifest.json","content":encode(document)+"\n"}));
    let mut paths = BTreeSet::new();
    for entry in entries.iter().chain(artifacts.iter()) {
        let path = text(&entry["path"])?;
        crate::layout::relative_path(path)?;
        if path.len() > 4096 || path.split('/').any(|part| part.len() > 255) {
            return fail("export path exceeds portable filesystem length limits");
        }
        if !paths.insert(key(path)) {
            return Err(Error(format!("export path collision: {path}")));
        }
    }
    for path in &paths {
        let parts: Vec<_> = path.split('/').collect();
        if (1..parts.len()).any(|i| paths.contains(&parts[..i].join("/"))) {
            return fail("export file conflicts with a parent directory");
        }
    }
    let mut sources = BTreeMap::new();
    for file in array(&content["files"])? {
        let binding =
            crate::observation::binding(store, profile, "source", text(&file["location"])?)?;
        let (root, _) = crate::observation::checked_root(store, profile, &binding)?;
        let path = text(&file["path"])?;
        let (parent, name) = parent(&root, path, false)?;
        let fd = open_at(&parent, &name, false)?;
        if !fd.metadata()?.is_file() {
            return fail("source is not a regular file");
        }
        sources.insert(
            text(&file["id"])?.to_owned(),
            Path::new(text(&binding["root"])?).join(path),
        );
    }
    // Finish all shape/source checks before creating the fresh output directory.
    for artifact in &artifacts {
        text(&artifact["content"])?;
    }
    for entry in entries {
        if !sources.contains_key(text(&entry["file_id"])?) {
            return fail("unknown export source");
        }
    }
    Ok(Preflight {
        destination,
        directory,
        artifacts,
        sources,
    })
}

pub fn publish(
    store: &Store,
    profile: &str,
    document: &Value,
    export: &Value,
    output: &Path,
) -> Result<Value> {
    let Preflight {
        destination,
        directory,
        artifacts,
        sources,
    } = preflight(store, profile, document, export, output)?;
    let entries = array(&document["content"]["entries"])?;
    let leaf = destination
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| Error("invalid output directory".into()))?;
    let name = cname(leaf)?;
    // SAFETY: directory is live; name is a NUL-terminated single path component.
    if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o755) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    directory.sync_all()?;
    let root = open_at(&directory, leaf, true)?;
    for entry in entries {
        let path = text(&entry["path"])?;
        let source = &sources[text(&entry["file_id"])?];
        let relative = relative(source, destination.join(path).parent().unwrap())?;
        let (parent, name) = parent(&root, path, true)?;
        let name = cname(&name)?;
        let target = cname(&relative)?;
        // SAFETY: parent and both NUL-terminated strings remain live. symlinkat
        // creates exclusively; an existing leaf is never replaced.
        if unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), name.as_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        parent.sync_all()?;
    }
    for artifact in &artifacts {
        let (parent, name) = parent(&root, text(&artifact["path"])?, true)?;
        let name = cname(&name)?;
        // SAFETY: parent/name remain live; O_EXCL/O_NOFOLLOW cannot adopt a leaf.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o644,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: openat returned a new descriptor uniquely owned by this File.
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(text(&artifact["content"])?.as_bytes())?;
        file.sync_all()?;
        parent.sync_all()?;
    }
    if identity(&root.metadata()?) != identity(&open_at(&directory, leaf, true)?.metadata()?) {
        return fail("export output identity changed during publication");
    }
    Ok(
        json!({"output":destination.to_str(),"format":export["format"],"links":entries.len(),"metadata_files":artifacts.len(),"written":true}),
    )
}

/// Frozen immutable-bundle comparison. This cannot publish, repair, remove, or
/// adopt a path. File bytes are read through the existing no-follow owner.
pub(crate) fn verify(event: &Value) -> Result<bool> {
    use std::io::Read;
    let data = &event["data"];
    let output = Path::new(text(&data["output"])?);
    let root = match open_root(text(&data["output"])?) {
        Ok(root) => root,
        Err(_) => return Ok(false),
    };
    let identity_value = |file: &File| -> Result<Value> {
        let (device, inode) = identity(&file.metadata()?);
        Ok(json!({"device":device,"inode":inode}))
    };
    if event["output_identity"].is_null() || identity_value(&root)? != event["output_identity"] {
        return Ok(false);
    }
    let check = || -> Result<bool> {
        let directory = open_root(
            output
                .parent()
                .and_then(Path::to_str)
                .ok_or_else(|| Error("invalid output parent".into()))?,
        )?;
        if identity_value(&directory)? != data["parent"] {
            return Ok(false);
        }
        let mut expected = BTreeMap::new();
        for entry in array(&data["document"]["content"]["entries"])? {
            let path = text(&entry["path"])?;
            crate::layout::relative_path(path)?;
            let source = &data["sources"][text(&entry["file_id"])?];
            let source_path = Path::new(text(&source["root"])?).join(text(&source["path"])?);
            expected.insert(
                path.to_owned(),
                (
                    true,
                    relative(&source_path, output.join(path).parent().unwrap())?,
                ),
            );
        }
        for artifact in array(&data["export"]["files"])? {
            let path = text(&artifact["path"])?;
            crate::layout::relative_path(path)?;
            expected.insert(
                path.to_owned(),
                (false, text(&artifact["content"])?.to_owned()),
            );
        }
        expected.insert(
            "catalog-manifest.json".into(),
            (false, encode(&data["document"]) + "\n"),
        );
        let mut parents = BTreeSet::new();
        for path in expected.keys() {
            let mut path = Path::new(path).parent();
            while let Some(p) = path.filter(|p| !p.as_os_str().is_empty()) {
                parents.insert(p.to_path_buf());
                path = p.parent();
            }
        }
        let mut directories = vec![PathBuf::new()];
        let mut seen = BTreeSet::new();
        while let Some(directory) = directories.pop() {
            if crate::embedding::poll_interrupt() {
                return Err(Error("evaluation_cancelled".into()));
            }
            for entry in std::fs::read_dir(output.join(&directory))? {
                let entry = entry?;
                let path = directory.join(entry.file_name());
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    if !parents.contains(&path) {
                        return Ok(false);
                    }
                    directories.push(path);
                    continue;
                }
                let relative = path
                    .to_str()
                    .ok_or_else(|| Error("paths must be valid UTF-8".into()))?;
                let Some((link, value)) = expected.get(relative) else {
                    return Ok(false);
                };
                let (parent, name) = parent(&root, relative, false)?;
                if *link {
                    if !kind.is_symlink()
                        || crate::filesystem_mutation::state(&root, relative)?
                            != ("link".into(), Some(value.clone()))
                    {
                        return Ok(false);
                    }
                } else {
                    let file = open_at(&parent, &name, false)?;
                    if !file.metadata()?.is_file() || file.metadata()?.len() != value.len() as u64 {
                        return Ok(false);
                    }
                    let mut bytes = Vec::new();
                    file.take(value.len() as u64 + 1).read_to_end(&mut bytes)?;
                    if bytes != value.as_bytes() {
                        return Ok(false);
                    }
                }
                seen.insert(relative.to_owned());
            }
        }
        Ok(seen == expected.keys().cloned().collect()
            && identity_value(&open_root(text(&data["output"])?)?)? == event["output_identity"])
    };
    match check() {
        Err(error) if error.0 == "evaluation_cancelled" => Err(error),
        Err(_) => Ok(false),
        result => result,
    }
}
