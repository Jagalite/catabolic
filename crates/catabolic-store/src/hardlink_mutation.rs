// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Regular output files are never unlinked or overwritten. Retirement is an
//! exclusive atomic rename into a separately owned, permanent retention tree.
use crate::{Error, Result, Store, filesystem_mutation as fs, observation};
use catabolic_core::{decode_json, encode};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs::File,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
};

pub const RETAINED: &str = ".catabolic-retained";
pub const WARNING: &str = "Hardlinks share source data and permissions. Retired links are retained, never automatically purged.";
pub const RETIREMENT_RACE_GATE: &str = "hardlink retirement/replacement/removal is blocked: concurrent foreign-leaf relocation safety is not yet qualified";
pub(crate) fn retirement_allowed(kind: &str, args: &Value) -> bool {
    !matches!(kind, "hard_replace" | "hard_remove")
        || (cfg!(debug_assertions) && args["disposable_fixture"] == true)
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| Error(format!("invalid hardlink journal {key}")))
}
fn cstring(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| Error("invalid filesystem path".into()))
}
fn absent(error: &Error) -> bool {
    error.0.contains("No such file or directory")
}
// libc mode_t is u16 on macOS and u32 on Linux.
#[allow(clippy::useless_conversion)]
fn stat_at(fd: &File, name: &str) -> Result<Value> {
    let name = cstring(name)?;
    // SAFETY: zero initialization is valid for the stat output buffer.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: descriptor, NUL-terminated name and output storage remain live.
    if unsafe {
        libc::fstatat(
            fd.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(
        json!({"mode":u32::from(st.st_mode)&u32::from(libc::S_IFMT),"device":st.st_dev,"inode":st.st_ino,"size":st.st_size,"mtime_ns":st.st_mtime*1_000_000_000+st.st_mtime_nsec,"nlink":st.st_nlink}),
    )
}
fn state(root: &File, path: &str) -> Result<Option<Value>> {
    match fs::parent(root, path, false).and_then(|(parent, leaf)| stat_at(&parent, &leaf)) {
        Ok(v) => Ok(Some(v)),
        Err(e) if absent(&e) => Ok(None),
        Err(e) => Err(e),
    }
}
fn matches(actual: Option<&Value>, raw: &str) -> Result<bool> {
    let expected = decode_json(raw)?;
    if expected["device"].as_u64().is_none() || expected["inode"].as_u64().is_none() {
        return Err(Error("invalid hardlink target identity".into()));
    }
    Ok(actual.is_some_and(|a| {
        a["mode"] == json!(libc::S_IFREG)
            && a["device"] == expected["device"]
            && a["inode"] == expected["inode"]
    }))
}
fn check_count(actual: &Value, path: &str) -> Result<()> {
    if actual["nlink"].as_u64().unwrap_or(0) <= 1 {
        return Err(Error(format!(
            "last hardlink protected: {path} is the final filesystem reference; keep this file or restore another hardlink to the same inode before retrying"
        )));
    }
    Ok(())
}
fn checked_parents(root: &File, path: &str) -> Result<()> {
    crate::layout::relative_path(path)?;
    let expected = root.metadata()?.dev();
    let mut fd = root.try_clone()?;
    let parts: Vec<_> = path.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        fd = match fs::open_at(&fd, part, true) {
            Ok(v) => v,
            Err(e) if absent(&e) => break,
            Err(e) => return Err(e),
        };
        if fd.metadata()?.dev() != expected {
            return Err(Error(format!(
                "cross-filesystem output parent refused: {path}"
            )));
        }
    }
    Ok(())
}
pub fn target(
    store: &Store,
    profile: &str,
    mapping: &Value,
    output: &Value,
) -> Result<(Option<String>, Option<String>)> {
    let (_, reason) = fs::source_target(store, profile, mapping, output)?;
    if reason.is_some() {
        return Ok((None, reason));
    }
    if mapping["device"] != output["device"] {
        return Err(Error(format!(
            "cross-filesystem hardlink refused: source {}:{} and output {} are on different filesystems; choose a same-filesystem output or a symlink catalog (no automatic fallback)",
            field(mapping, "location")?,
            field(mapping, "source_path")?,
            field(output, "root")?
        )));
    }
    let mut value = serde_json::Map::new();
    for key in [
        "file_id",
        "location",
        "source_path",
        "device",
        "inode",
        "size",
        "mtime_ns",
    ] {
        value.insert(key.into(), mapping[key].clone());
    }
    Ok((Some(encode(&Value::Object(value))), None))
}
fn expected_retention(store: &Store, profile: &str, catalog: &str) -> Value {
    let mut expected = fs::owner(store, profile, catalog);
    expected["purpose"] = json!("retained-hardlinks");
    expected
}
fn check_retention(store: &Store, profile: &str, catalog: &str, root: &File) -> Result<()> {
    let fd = match fs::open_at(root, RETAINED, true) {
        Ok(v) => v,
        Err(e) if absent(&e) => return Ok(()),
        Err(e) => return Err(e),
    };
    if fd.metadata()?.dev() != root.metadata()?.dev() {
        return Err(Error(
            "retention directory crossed a filesystem boundary".into(),
        ));
    }
    if !["owned", "empty"].contains(&fs::owner_state(
        &fd,
        &expected_retention(store, profile, catalog),
    )?) {
        return Err(Error("retention directory contains unowned files".into()));
    }
    Ok(())
}
pub fn delta(store: &Store, profile: &str, catalog: &str) -> Result<Vec<Value>> {
    let output = observation::binding(store, profile, "output", catalog)?;
    let owned: BTreeMap<String, String> = store
        .db
        .prepare("SELECT path,target FROM owned_hardlinks WHERE profile=? AND catalog=?")?
        .query_map([profile, catalog], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    if store
        .db
        .prepare("SELECT 1 FROM owned_links WHERE catalog=? LIMIT 1")?
        .exists([catalog])?
    {
        return Err(Error(
            "hardlink catalog has incompatible symlink ownership records".into(),
        ));
    }
    let (root, _) = observation::checked_root(store, profile, &output)?;
    let ownership = fs::owner_state(&root, &fs::owner(store, profile, catalog))?;
    if ownership == "unowned" || ownership == "empty" && !owned.is_empty() {
        return Err(Error(
            "hardlink output ownership is missing or changed".into(),
        ));
    }
    let mut actions = vec![];
    let mut desired = BTreeMap::new();
    if ownership == "empty" {
        actions.push(json!({"kind":"prepare","catalog":catalog,"path":"","target":null,"previous":null,"reason":null}));
    }
    for mapping in fs::mappings(store, profile, catalog)? {
        let path = field(&mapping, "path")?;
        checked_parents(&root, path)?;
        let (target, reason) = target(store, profile, &mapping, &output)?;
        if let Some(reason) = reason {
            actions.push(json!({"kind":"blocked_source","catalog":catalog,"path":path,"target":null,"previous":null,"reason":reason}));
        } else {
            desired.insert(
                path.to_owned(),
                target.ok_or_else(|| Error("missing hardlink target".into()))?,
            );
        }
    }
    for (path, target) in &desired {
        let actual = state(&root, path)?;
        let previous = owned.get(path);
        let (kind, reason) = if actual.is_none() {
            ("hard_create", None)
        } else if previous.is_none() || !matches(actual.as_ref(), previous.unwrap())? {
            return Err(Error(format!(
                "unowned or externally changed regular-file destination: {path}"
            )));
        } else if matches(actual.as_ref(), target)? {
            ("unchanged", None)
        } else {
            check_count(
                actual
                    .as_ref()
                    .ok_or_else(|| Error("hardlink disappeared during planning".into()))?,
                path,
            )?;
            ("hard_replace", Some("previous hardlink will be retained"))
        };
        actions.push(json!({"kind":kind,"catalog":catalog,"path":path,"target":target,"previous":previous,"reason":reason}));
    }
    for (path, previous) in &owned {
        if desired.contains_key(path) {
            continue;
        }
        checked_parents(&root, path)?;
        let actual = state(&root, path)?;
        let (kind, reason) = if actual.is_none() {
            ("hard_forget", None)
        } else if matches(actual.as_ref(), previous)? {
            check_count(
                actual
                    .as_ref()
                    .ok_or_else(|| Error("hardlink disappeared during planning".into()))?,
                path,
            )?;
            (
                "hard_remove",
                Some("hardlink will move into protected retention"),
            )
        } else {
            return Err(Error(format!("owned hardlink changed externally: {path}")));
        };
        actions.push(json!({"kind":kind,"catalog":catalog,"path":path,"target":null,"previous":previous,"reason":reason}));
    }
    if actions
        .iter()
        .any(|a| matches!(a["kind"].as_str(), Some("hard_replace" | "hard_remove")))
    {
        check_retention(store, profile, catalog, &root)?;
    }
    Ok(actions)
}
fn mkdir(parent: &File, name: &str) -> Result<()> {
    let name = cstring(name)?;
    // SAFETY: descriptor and name are valid; mkdir never replaces an entry.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
        parent.sync_all()?;
    } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn retention(
    store: &Store,
    profile: &str,
    root: &File,
    op: &Value,
    create: bool,
    args: &Value,
) -> Result<Option<File>> {
    let id = uuid::Uuid::parse_str(field(op, "id")?)
        .map_err(|_| Error("invalid journal operation identity".into()))?
        .to_string();
    if create {
        mkdir(root, RETAINED)?;
    }
    let fd = match fs::open_at(root, RETAINED, true) {
        Ok(v) => v,
        Err(e) if absent(&e) && !create => return Ok(None),
        Err(e) => return Err(e),
    };
    if fd.metadata()?.dev() != root.metadata()?.dev() {
        return Err(Error(
            "retention directory must be on the output filesystem".into(),
        ));
    }
    let expected = expected_retention(store, profile, field(op, "catalog")?);
    if create {
        fs::claim_output_at(&fd, &expected, &id, args, true)?;
    } else if fs::owner_state(&fd, &expected)? != "owned" {
        let mut pending = true;
        fs::entries(&fd, |name| {
            pending &= name == format!(".catabolic-claim-{id}");
            Ok(pending)
        })?;
        if pending {
            return Ok(None);
        }
        return Err(Error("retention ownership is missing".into()));
    }
    if create {
        mkdir(&fd, &id)?;
    }
    let child = match fs::open_at(&fd, &id, true) {
        Ok(v) => v,
        Err(e) if absent(&e) && !create => return Ok(None),
        Err(e) => return Err(e),
    };
    if child.metadata()?.dev() != root.metadata()?.dev() {
        return Err(Error(
            "retention operation directory crossed a filesystem boundary".into(),
        ));
    }
    // Retained entries may have been renamed in before a crash skipped syncs.
    fd.sync_all()?;
    child.sync_all()?;
    Ok(Some(child))
}
fn retained(store: &Store, profile: &str, root: &File, op: &Value) -> Result<bool> {
    let Some(fd) = retention(store, profile, root, op, false, &Value::Null)? else {
        return Ok(false);
    };
    let actual = match stat_at(&fd, "data") {
        Ok(v) => v,
        Err(e) if absent(&e) => return Ok(false),
        Err(e) => return Err(e),
    };
    if !matches(Some(&actual), field(op, "previous")?)? {
        return Err(Error(
            "retained hardlink changed externally; no data removed".into(),
        ));
    }
    Ok(true)
}
fn rename_noreplace(source: &File, name: &str, destination: &File, leaf: &str) -> Result<()> {
    let name = cstring(name)?;
    let leaf = cstring(leaf)?;
    #[cfg(target_os = "linux")]
    // SAFETY: live descriptors and names; RENAME_NOREPLACE prevents overwrite.
    let result = unsafe {
        libc::renameat2(
            source.as_raw_fd(),
            name.as_ptr(),
            destination.as_raw_fd(),
            leaf.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: live descriptors and names; RENAME_EXCL prevents overwrite.
    let result = unsafe {
        libc::renameatx_np(
            source.as_raw_fd(),
            name.as_ptr(),
            destination.as_raw_fd(),
            leaf.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result < 0 {
        return Err(Error(format!(
            "exclusive hardlink retirement refused: {}; destination was not overwritten",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}
fn retain(
    store: &Store,
    profile: &str,
    root: &File,
    op: &Value,
    args: &Value,
    observer: &dyn Fn(&str, &Value) -> Result<()>,
) -> Result<()> {
    let saved = retention(store, profile, root, op, true, args)?
        .ok_or_else(|| Error("missing retention directory".into()))?;
    let actual = match stat_at(&saved, "data") {
        Ok(v) => Some(v),
        Err(e) if absent(&e) => None,
        Err(e) => return Err(e),
    };
    if actual.is_some() {
        if !matches(actual.as_ref(), field(op, "previous")?)? {
            return Err(Error(
                "retention destination collision; no data overwritten".into(),
            ));
        }
        return Ok(());
    }
    let path = field(op, "path")?;
    let (parent, leaf) = fs::parent(root, path, false)?;
    let actual = stat_at(&parent, &leaf)?;
    if !matches(Some(&actual), field(op, "previous")?)? {
        return Err(Error("hardlink changed before retirement".into()));
    }
    check_count(&actual, path)?;
    observer("before_retention_rename", op)?;
    fs::failpoint(args, "before_retention_rename")?;
    rename_noreplace(&parent, &leaf, &saved, "data")?;
    saved.sync_all()?;
    parent.sync_all()?;
    if !matches(Some(&stat_at(&saved, "data")?), field(op, "previous")?)? {
        // A foreign entry was raced into the leaf and retired. Return it to
        // its name without overwriting anything; if the name has been reused
        // meanwhile, keep it in retention.
        observer("before_retention_restore", op)?;
        if rename_noreplace(&saved, "data", &parent, &leaf).is_ok() {
            saved.sync_all()?;
            parent.sync_all()?;
            return Err(Error(format!(
                "hardlink changed before retirement: {path}; the foreign entry was restored and nothing was overwritten"
            )));
        }
        return Err(Error(
            "destination changed concurrently; unexpected file preserved in retention".into(),
        ));
    }
    observer("after_retention", op)?;
    fs::failpoint(args, "after_retention")
}
fn record_retention(db: &Connection, profile: &str, op: &Value) -> Result<()> {
    let id = field(op, "id")?;
    db.execute("INSERT INTO retained_hardlinks(id,profile,catalog,path,original_path,target) VALUES (?,?,?,?,?,?) ON CONFLICT(id) DO NOTHING",params![id,profile,field(op,"catalog")?,format!("{RETAINED}/{id}/data"),field(op,"path")?,field(op,"previous")?])?;
    Ok(())
}
fn active_mapping(
    store: &Store,
    profile: &str,
    catalog: &str,
    path: &str,
) -> Result<Option<Value>> {
    Ok(fs::mappings(store, profile, catalog)?
        .into_iter()
        .find(|m| m["path"] == path))
}
fn desired(store: &Store, profile: &str, op: &Value) -> Result<Option<String>> {
    let catalog = field(op, "catalog")?;
    active_mapping(store, profile, catalog, field(op, "path")?)?
        .map(|mapping| {
            target(
                store,
                profile,
                &mapping,
                &observation::binding(store, profile, "output", catalog)?,
            )
            .map(|p| p.0)
        })
        .transpose()
        .map(Option::flatten)
}
/// Whether a reserved internal leaf exists, without following symlinks.
/// Reserved `.catabolic` names bypass catalog path validation by design.
fn leaf_exists(dir: &File, leaf: &str) -> Result<bool> {
    match stat_at(dir, leaf) {
        Ok(_) => Ok(true),
        Err(e) if absent(&e) => Ok(false),
        Err(e) => Err(e),
    }
}
/// Create a hard link to the source pinned by `opened`. Linux links the exact
/// inode through its descriptor, so the source name is never resolved again.
/// Other platforms re-resolve the name without following symlinks; callers
/// verify the new entry before publishing it.
fn link_pinned(opened: &File, src: &File, src_name: &str, dst: &File, leaf: &str) -> Result<()> {
    let leaf_c = cstring(leaf)?;
    let link_named = || -> std::result::Result<(), std::io::Error> {
        let src_name = cstring(src_name).map_err(|e| std::io::Error::other(e.0))?;
        // SAFETY: verified source/destination descriptors and names are live.
        // Without AT_SYMLINK_FOLLOW a raced symlink is never followed.
        let rc = unsafe {
            libc::linkat(
                src.as_raw_fd(),
                src_name.as_ptr(),
                dst.as_raw_fd(),
                leaf_c.as_ptr(),
                0,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    #[cfg(target_os = "linux")]
    let result = {
        let path = cstring(&format!("/proc/self/fd/{}", opened.as_raw_fd()))?;
        // SAFETY: live descriptor path and destination; AT_SYMLINK_FOLLOW
        // resolves the descriptor's magic link to the pinned inode itself.
        let rc = unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                path.as_ptr(),
                dst.as_raw_fd(),
                leaf_c.as_ptr(),
                libc::AT_SYMLINK_FOLLOW,
            )
        };
        let error = (rc != 0).then(std::io::Error::last_os_error);
        match error {
            None => Ok(()),
            // Without a mounted /proc, fall back to the verified named link.
            Some(e)
                if e.raw_os_error() == Some(libc::ENOENT)
                    && !std::path::Path::new("/proc/self/fd").is_dir() =>
            {
                link_named()
            }
            Some(e) => Err(e),
        }
    };
    #[cfg(not(target_os = "linux"))]
    let result = {
        let _ = opened;
        link_named()
    };
    let Err(error) = result else {
        return Ok(());
    };
    match error.raw_os_error() {
        Some(libc::EXDEV) => Err(Error("cross-filesystem hardlink rejected by filesystem (EXDEV); choose a symlink catalog; no fallback".into())),
        // The pinned or named source disappeared after validation.
        Some(libc::ENOENT) => Err(Error(
            "source changed during hardlink creation; scan and recover".into(),
        )),
        Some(code) if [libc::EPERM, libc::EACCES, libc::ENOTSUP, libc::EROFS].contains(&code) => {
            Err(Error(format!(
                "filesystem or permissions rejected hardlink creation: {error}; no fallback"
            )))
        }
        _ => Err(error.into()),
    }
}
fn create(
    store: &Store,
    profile: &str,
    root: &File,
    op: &Value,
    args: &Value,
    observer: &dyn Fn(&str, &Value) -> Result<()>,
) -> Result<()> {
    let raw = field(op, "target")?;
    if desired(store, profile, op)?.as_deref() != Some(raw) {
        return Err(Error(
            "pending hardlink source is no longer desired; scan and recover".into(),
        ));
    }
    let target = decode_json(raw)?;
    let binding = observation::binding(store, profile, "source", field(&target, "location")?)?;
    let (source, _) = observation::checked_root(store, profile, &binding)?;
    let (src, src_name) = fs::parent(&source, field(&target, "source_path")?, false)?;
    let (dst, dst_name) = fs::parent(root, field(op, "path")?, true)?;
    let opened = fs::open_at(&src, &src_name, false)?;
    let observed = opened.metadata()?;
    let actual = json!({"mode":if observed.is_file(){libc::S_IFREG}else{0},"device":observed.dev(),"inode":observed.ino()});
    if !matches(Some(&actual), raw)?
        || json!(observed.size()) != target["size"]
        || json!(observed.mtime() * 1_000_000_000 + observed.mtime_nsec()) != target["mtime_ns"]
    {
        return Err(Error(
            "source changed before hardlink creation; scan and recover".into(),
        ));
    }
    if observed.dev() != dst.metadata()?.dev() {
        return Err(Error(
            "cross-filesystem hardlink refused at destination; no fallback".into(),
        ));
    }
    observer("before_link", op)?;
    fs::failpoint(args, "before_link")?;
    // Link into a staging name, verify it, then publish by exclusive rename.
    // A source raced after validation can only reach the staging name, where
    // it is preserved for inspection; it never becomes the selected output.
    // The name is derived from the journal operation so recovery finds it.
    let staged = format!(".catabolic-hardlink-{}", field(op, "id")?);
    if !leaf_exists(&dst, &staged)? {
        link_pinned(&opened, &src, &src_name, &dst, &staged)?;
        dst.sync_all()?;
    }
    let linked = stat_at(&dst, &staged)?;
    if !matches(Some(&linked), raw)?
        || linked["size"] != target["size"]
        || linked["mtime_ns"] != target["mtime_ns"]
    {
        return Err(Error(format!(
            "source changed during hardlink creation; staged entry {staged} preserved for inspection"
        )));
    }
    fs::failpoint(args, "after_staged_link")?;
    // Walk from a freshly validated output root so a relocated root or
    // ancestor is detected, not just a moved destination directory.
    let fresh = || -> Result<File> {
        let output = observation::binding(store, profile, "output", field(op, "catalog")?)?;
        Ok(observation::checked_root(store, profile, &output)?.0)
    };
    fs::verify_containment(&fresh()?, field(op, "path")?, &dst)?;
    if let Err(error) = rename_noreplace(&dst, &staged, &dst, &dst_name) {
        return Err(if state(&dst, &dst_name)?.is_some() {
            Error(format!(
                "hardlink destination appeared during creation; staged entry {staged} preserved"
            ))
        } else {
            Error(format!("{}; staged entry {staged} preserved", error.0))
        });
    }
    dst.sync_all()?;
    fs::verify_containment(&fresh()?, field(op, "path")?, &dst)?;
    let current = stat_at(&dst, &dst_name)?;
    if !matches(Some(&current), raw)?
        || current["size"] != target["size"]
        || current["mtime_ns"] != target["mtime_ns"]
    {
        return Err(Error(
            "source changed during hardlink creation; destination preserved for inspection".into(),
        ));
    }
    observer("after_link", op)?;
    fs::failpoint(args, "after_link")
}
pub fn execute(store: &Store, profile: &str, op: &Value, args: &Value) -> Result<()> {
    fs::admit_failpoints(args)?;
    execute_with(store, profile, op, args, &|_, _| Ok(()))
}
fn execute_with(
    store: &Store,
    profile: &str,
    op: &Value,
    args: &Value,
    observer: &dyn Fn(&str, &Value) -> Result<()>,
) -> Result<()> {
    let catalog = field(op, "catalog")?;
    let path = field(op, "path")?;
    let kind = field(op, "kind")?;
    if !retirement_allowed(kind, args) {
        return Err(Error(RETIREMENT_RACE_GATE.into()));
    }
    let output = observation::binding(store, profile, "output", catalog)?;
    let (root, _) = observation::checked_root(store, profile, &output)?;
    if fs::owner_state(&root, &fs::owner(store, profile, catalog))? != "owned" {
        return Err(Error(
            "hardlink output ownership missing during recovery".into(),
        ));
    }
    checked_parents(&root, path)?;
    let mut actual = state(&root, path)?;
    let mut was_retained = false;
    if ["hard_replace", "hard_remove"].contains(&kind) {
        was_retained = retained(store, profile, &root, op)?;
        if !was_retained && !(actual.is_none() && kind == "hard_remove") {
            if !matches(actual.as_ref(), field(op, "previous")?)? {
                return Err(Error("owned hardlink changed before retirement".into()));
            }
            let wanted = desired(store, profile, op)?;
            if kind == "hard_remove" && wanted.is_some() {
                return Err(Error(
                    "pending removal is no longer desired; inspect catalog state".into(),
                ));
            }
            if kind == "hard_replace" && wanted.as_deref() != op["target"].as_str() {
                return Err(Error(
                    "pending hardlink replacement is no longer desired".into(),
                ));
            }
            retain(store, profile, &root, op, args, observer)?;
            was_retained = true;
        }
        actual = state(&root, path)?;
    }
    match kind {
        "hard_create" | "hard_replace" => {
            if actual.is_none() {
                create(store, profile, &root, op, args, observer)?;
            } else if !matches(actual.as_ref(), field(op, "target")?)? {
                return Err(Error(
                    "hardlink creation destination changed externally".into(),
                ));
            }
        }
        "hard_remove" | "hard_forget" => {
            if actual.is_some() {
                return Err(Error(
                    "hardlink removal destination reappeared; no data overwritten".into(),
                ));
            }
        }
        _ => return Err(Error(format!("unknown hardlink operation: {kind}"))),
    }
    // Complete directory syncs an interrupted predecessor may have skipped
    // before acknowledging, even when the postcondition was already observed.
    fs::sync_ancestry(&root, path)?;
    fs::failpoint(args, "after_filesystem")?;
    store.transaction(|db|{
        if was_retained{record_retention(db,profile,op)?;}
        if ["hard_create","hard_replace"].contains(&kind){db.execute("INSERT INTO owned_hardlinks VALUES (?,?,?,?) ON CONFLICT(profile,catalog,path) DO UPDATE SET target=excluded.target",params![profile,catalog,path,field(op,"target")?])?;}
        else{db.execute("DELETE FROM owned_hardlinks WHERE profile=? AND catalog=? AND path=?",[profile,catalog,path])?;}
        if ["hard_create","hard_replace","hard_remove"].contains(&kind){fs::record_change(db,profile,catalog,path,&output,fs::notify_consumers(args)?)?;}
        db.execute("DELETE FROM journal WHERE id=?",[field(op,"id")?])?;fs::failpoint(args,"before_commit")?;Ok(())
    })?;
    fs::failpoint(args, "after_commit")
}
pub fn cancel_obsolete(store: &Store, profile: &str, op: &Value, force: bool) -> Result<bool> {
    let kind = field(op, "kind")?;
    if kind == "hard_forget" {
        return Ok(false);
    }
    let catalog = field(op, "catalog")?;
    let output = observation::binding(store, profile, "output", catalog)?;
    let (root, _) = observation::checked_root(store, profile, &output)?;
    if fs::owner_state(&root, &fs::owner(store, profile, catalog))? != "owned" {
        return Err(Error("hardlink output ownership missing".into()));
    }
    let actual = state(&root, field(op, "path")?)?;
    let was_retained =
        ["hard_replace", "hard_remove"].contains(&kind) && retained(store, profile, &root, op)?;
    let untouched = if kind == "hard_create" {
        actual.is_none()
    } else {
        matches(actual.as_ref(), field(op, "previous")?)? && !was_retained
    };
    if !untouched && !(actual.is_none() && was_retained) {
        return Ok(false);
    }
    if !force {
        let wanted = desired(store, profile, op)?;
        let obsolete = if kind == "hard_remove" {
            wanted.is_some()
        } else {
            wanted.as_deref() != op["target"].as_str()
        };
        if !obsolete {
            return Ok(false);
        }
    }
    // An interrupted creation may have staged a link that is now the source
    // inode's only reference; never orphan it by clearing the intent.
    if let Ok((parent, _)) = fs::parent(&root, field(op, "path")?, false) {
        let staged = format!(".catabolic-hardlink-{}", field(op, "id")?);
        if leaf_exists(&parent, &staged)? {
            return Err(Error(format!(
                "staged hardlink {staged} exists; inspect it before cancelling"
            )));
        }
    }
    // A retention rename may have preceded a crash that skipped the source
    // parent's sync; complete it before ownership and journal are cleared.
    fs::sync_ancestry(&root, field(op, "path")?)?;
    store.transaction(|db| {
        if was_retained {
            record_retention(db, profile, op)?;
            db.execute(
                "DELETE FROM owned_hardlinks WHERE profile=? AND catalog=? AND path=?",
                [profile, catalog, field(op, "path")?],
            )?;
        }
        db.execute("DELETE FROM journal WHERE id=?", [field(op, "id")?])?;
        Ok(())
    })?;
    Ok(true)
}
pub fn verify(store: &Store, profile: &str, catalog: &str) -> Result<Value> {
    let mut issues = vec![];
    let mut verified = 0;
    let check = (|| -> Result<()> {
        let (root, _) = observation::checked_root(
            store,
            profile,
            &observation::binding(store, profile, "output", catalog)?,
        )?;
        if fs::owner_state(&root, &fs::owner(store, profile, catalog))? != "owned" {
            return Err(Error("hardlink output ownership is missing".into()));
        }
        if store
            .db
            .prepare("SELECT 1 FROM journal WHERE profile=? AND catalog=?")?
            .exists([profile, catalog])?
        {
            return Err(Error("pending hardlink operations require recovery".into()));
        }
        for action in delta(store, profile, catalog)? {
            if action["kind"] == "unchanged" {
                verified += 1;
            } else {
                issues.push(json!({"path":action["path"],"reason":action["reason"].as_str().map(str::to_owned).unwrap_or_else(||format!("{} needs synchronization",action["kind"].as_str().unwrap_or("unknown")))}));
            }
        }
        Ok(())
    })();
    if let Err(error) = check {
        issues.push(json!({"reason":error.0}));
    }
    let count: i64 = store.db.query_row(
        "SELECT count(*) FROM retained_hardlinks WHERE profile=? AND catalog=?",
        [profile, catalog],
        |r| r.get(0),
    )?;
    let mut warnings = vec![WARNING.to_owned()];
    if count > 0 {
        warnings.push(format!(
            "{count} retired hardlinks remain in {RETAINED}; review with catalog retained"
        ));
    }
    Ok(
        json!({"catalog":catalog,"link_mode":"hardlink","healthy":issues.is_empty(),"verified_links":verified,"issues":issues,"retained_count":count,"warnings":warnings}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        tempfile::TempDir,
        Store,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let source = temporary.path().join("source");
        let output = temporary.path().join("output");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&output).unwrap();
        std::fs::write(source.join("one.txt"), "original source").unwrap();
        observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"media","root":source}),
        )
        .unwrap();
        observation::execute(
            &store,
            "default",
            "bind",
            &json!({"kind":"output","owner":"global","root":output}),
        )
        .unwrap();
        observation::execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        store.transaction(|db| {
            db.execute("INSERT INTO items(id,kind,metadata) VALUES('item','movie','{}')",[])?;
            db.execute("INSERT INTO mappings(id,catalog,file_id,item_id,path) SELECT 'mapping','global',id,'item','folder/one.txt' FROM files WHERE path='one.txt'",[])?;
            db.execute("INSERT INTO catalog_link_modes VALUES('global','hardlink')",[])?;
            Ok(())
        }).unwrap();
        let root = fs::open_root(output.to_str().unwrap()).unwrap();
        fs::claim_output(
            &root,
            &fs::owner(&store, "default", "global"),
            &uuid::Uuid::new_v4().to_string(),
        )
        .unwrap();
        (temporary, store, source, output)
    }
    fn intent(store: &Store, kind: &str) -> Value {
        let mut op = delta(store, "default", "global")
            .unwrap()
            .into_iter()
            .find(|a| a["kind"] == kind)
            .unwrap();
        op["id"] = json!(uuid::Uuid::new_v4().to_string());
        op["profile"] = json!("default");
        store.transaction(|db|{db.execute("INSERT INTO journal(id,profile,catalog,path,kind,target,previous) VALUES(?,?,?,?,?,?,?)",params![op["id"].as_str(),"default","global",op["path"].as_str(),kind,op["target"].as_str(),op["previous"].as_str()])?;Ok(())}).unwrap();
        op
    }
    #[test]
    fn removing_source_after_final_count_check_preserves_retained_bytes() {
        let (_temporary, store, source, output) = fixture();
        execute(
            &store,
            "default",
            &intent(&store, "hard_create"),
            &json!({"disposable_fixture":true}),
        )
        .unwrap();
        store
            .transaction(|db| {
                db.execute("UPDATE mappings SET active=0", [])?;
                Ok(())
            })
            .unwrap();
        let op = intent(&store, "hard_remove");
        execute_with(
            &store,
            "default",
            &op,
            &json!({"disposable_fixture":true}),
            &|stage, _| {
                if stage == "before_retention_rename" {
                    std::fs::remove_file(source.join("one.txt"))?;
                }
                Ok(())
            },
        )
        .unwrap();
        let retained = output.join(format!("{RETAINED}/{}/data", op["id"].as_str().unwrap()));
        assert_eq!(std::fs::read(&retained).unwrap(), b"original source");
        assert_eq!(retained.metadata().unwrap().nlink(), 1);
        assert!(!output.join("folder/one.txt").exists());
    }
    #[test]
    fn exclusive_retention_never_overwrites_a_concurrent_final_reference() {
        let (_temporary, store, _source, output) = fixture();
        execute(
            &store,
            "default",
            &intent(&store, "hard_create"),
            &json!({"disposable_fixture":true}),
        )
        .unwrap();
        store
            .transaction(|db| {
                db.execute("UPDATE mappings SET active=0", [])?;
                Ok(())
            })
            .unwrap();
        let op = intent(&store, "hard_remove");
        let retained = output.join(format!("{RETAINED}/{}/data", op["id"].as_str().unwrap()));
        let error = execute_with(
            &store,
            "default",
            &op,
            &json!({"disposable_fixture":true}),
            &|stage, _| {
                if stage == "before_retention_rename" {
                    std::fs::write(&retained, "foreign final reference")?;
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.0.contains("not overwritten"));
        assert_eq!(
            std::fs::read(&retained).unwrap(),
            b"foreign final reference"
        );
        assert_eq!(
            std::fs::read(output.join("folder/one.txt")).unwrap(),
            b"original source"
        );
        assert!(
            store
                .db
                .prepare("SELECT 1 FROM journal")
                .unwrap()
                .exists([])
                .unwrap()
        );
    }
    #[test]
    fn interrupted_staging_publishes_only_a_verified_staged_link() {
        let (_temporary, store, source, output) = fixture();
        let op = intent(&store, "hard_create");
        std::fs::create_dir_all(output.join("folder")).unwrap();
        let staged = output.join(format!(
            "folder/.catabolic-hardlink-{}",
            op["id"].as_str().unwrap()
        ));
        // A foreign staged entry is refused and preserved, never published.
        std::fs::write(&staged, "foreign staged bytes").unwrap();
        let error =
            execute(&store, "default", &op, &json!({"disposable_fixture":true})).unwrap_err();
        assert!(error.0.contains("preserved for inspection"), "{error:?}");
        assert!(!output.join("folder/one.txt").exists());
        assert_eq!(std::fs::read(&staged).unwrap(), b"foreign staged bytes");
        // Cancelling the intent would orphan the staged entry; it is refused.
        let error = cancel_obsolete(&store, "default", &op, true).unwrap_err();
        assert!(
            error.0.contains("inspect it before cancelling"),
            "{error:?}"
        );
        assert_eq!(std::fs::read(&staged).unwrap(), b"foreign staged bytes");
        // A staged link to the recorded source, left by an interrupted
        // predecessor, is verified and published on recovery.
        std::fs::remove_file(&staged).unwrap();
        std::fs::hard_link(source.join("one.txt"), &staged).unwrap();
        // Public recovery consults cancellation first; a still-desired staged
        // creation is not cancelled but verified and published.
        crate::filesystem_mutation::execute(
            &store,
            "default",
            "recover",
            &json!({"catalog":"global","disposable_fixture":true}),
        )
        .unwrap();
        assert!(!staged.exists());
        assert_eq!(
            std::fs::read(output.join("folder/one.txt")).unwrap(),
            b"original source"
        );
    }
    #[test]
    fn swapped_source_is_preserved_for_inspection_without_false_ownership() {
        let (_temporary, store, source, output) = fixture();
        let op = intent(&store, "hard_create");
        let error = execute_with(
            &store,
            "default",
            &op,
            &json!({"disposable_fixture":true}),
            &|stage, _| {
                if stage == "before_link" {
                    std::fs::remove_file(source.join("one.txt"))?;
                    std::fs::write(source.join("one.txt"), "new raced inode")?;
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.0.contains("source changed during hardlink creation"));
        // The raced source never becomes the selected output.
        assert!(!output.join("folder/one.txt").exists());
        let staged = output.join(format!(
            "folder/.catabolic-hardlink-{}",
            op["id"].as_str().unwrap()
        ));
        if cfg!(target_os = "linux") {
            // The pinned (since unlinked) inode cannot be linked at all.
            assert!(!staged.exists());
        } else {
            // A name-based link reached only the staging name, preserved there.
            assert_eq!(std::fs::read(&staged).unwrap(), b"new raced inode");
        }
        assert!(
            !store
                .db
                .prepare("SELECT 1 FROM owned_hardlinks")
                .unwrap()
                .exists([])
                .unwrap()
        );
        assert!(
            store
                .db
                .prepare("SELECT 1 FROM journal")
                .unwrap()
                .exists([])
                .unwrap()
        );
    }
    #[test]
    fn raced_foreign_retirement_is_restored_without_acknowledging_ownership() {
        let (_temporary, store, source, output) = fixture();
        execute(
            &store,
            "default",
            &intent(&store, "hard_create"),
            &json!({"disposable_fixture":true}),
        )
        .unwrap();
        store
            .transaction(|db| {
                db.execute("UPDATE mappings SET active=0", [])?;
                Ok(())
            })
            .unwrap();
        let op = intent(&store, "hard_remove");
        let error = execute_with(
            &store,
            "default",
            &op,
            &json!({"disposable_fixture":true}),
            &|stage, _| {
                if stage == "before_retention_rename" {
                    std::fs::remove_file(output.join("folder/one.txt"))?;
                    std::fs::write(output.join("folder/one.txt"), "foreign final reference")?;
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(
            error.0.contains("the foreign entry was restored"),
            "{error:?}"
        );
        // The foreign file is back at its own name, not relocated.
        assert_eq!(
            std::fs::read(output.join("folder/one.txt")).unwrap(),
            b"foreign final reference"
        );
        let retained = output.join(format!("{RETAINED}/{}/data", op["id"].as_str().unwrap()));
        assert!(std::fs::symlink_metadata(&retained).is_err());
        assert_eq!(
            std::fs::read(source.join("one.txt")).unwrap(),
            b"original source"
        );
        assert!(
            !store
                .db
                .prepare("SELECT 1 FROM retained_hardlinks")
                .unwrap()
                .exists([])
                .unwrap()
        );
        assert!(
            store
                .db
                .prepare("SELECT 1 FROM journal")
                .unwrap()
                .exists([])
                .unwrap()
        );
        assert!(
            execute(&store, "default", &op, &json!({"disposable_fixture":true}))
                .unwrap_err()
                .0
                .contains("owned hardlink changed before retirement")
        );
        assert_eq!(
            std::fs::read(output.join("folder/one.txt")).unwrap(),
            b"foreign final reference"
        );
    }
    #[test]
    fn foreign_retirement_stays_retained_when_its_name_is_reused() {
        let (_temporary, store, source, output) = fixture();
        execute(
            &store,
            "default",
            &intent(&store, "hard_create"),
            &json!({"disposable_fixture":true}),
        )
        .unwrap();
        store
            .transaction(|db| {
                db.execute("UPDATE mappings SET active=0", [])?;
                Ok(())
            })
            .unwrap();
        let op = intent(&store, "hard_remove");
        let error = execute_with(
            &store,
            "default",
            &op,
            &json!({"disposable_fixture":true}),
            &|stage, _| {
                let leaf = output.join("folder/one.txt");
                if stage == "before_retention_rename" {
                    std::fs::remove_file(&leaf)?;
                    std::fs::write(&leaf, "first foreign")?;
                } else if stage == "before_retention_restore" {
                    std::fs::write(&leaf, "second foreign")?;
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.0.contains("unexpected file preserved in retention"));
        let retained = output.join(format!("{RETAINED}/{}/data", op["id"].as_str().unwrap()));
        assert_eq!(std::fs::read(retained).unwrap(), b"first foreign");
        assert_eq!(
            std::fs::read(output.join("folder/one.txt")).unwrap(),
            b"second foreign"
        );
        assert_eq!(
            std::fs::read(source.join("one.txt")).unwrap(),
            b"original source"
        );
        assert!(
            execute(&store, "default", &op, &json!({"disposable_fixture":true}))
                .unwrap_err()
                .0
                .contains("retained hardlink changed externally")
        );
    }
}
