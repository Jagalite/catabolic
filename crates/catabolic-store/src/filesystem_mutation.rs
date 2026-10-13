// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Descriptor-relative filesystem publication. The schema-32 journal remains
//! the durable authority; an effect is acknowledged only with ownership updates.
use crate::{Error, Result, Store, migration::rows};
use catabolic_core::encode;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::{CStr, CString},
    fs::{File, Metadata},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path},
    time::{SystemTime, UNIX_EPOCH},
};

// One production journal executor, with explicit mutable ownership only when
// isolated fallback probes may suspend the catalog. Legacy callers retain the
// original fixed Store boundary and its fail-closed unsupported-mode check.
enum Reconciliation<'s, 'p> {
    Fixed(&'s Store),
    Mutable(
        &'s mut Store,
        &'s mut crate::fallback_resolution::Probe<'p>,
        crate::fallback_publication::Selections,
    ),
}
impl Reconciliation<'_, '_> {
    fn store(&self) -> &Store {
        match self {
            Self::Fixed(s) => s,
            Self::Mutable(s, _, _) => s,
        }
    }
    fn supported(&self, profile: &str, catalog: &str) -> Result<()> {
        if matches!(self, Self::Mutable(..))
            && crate::fallback_projection::binding(self.store(), profile, catalog)?.is_some()
        {
            if hardlink_mode(self.store(), catalog)? {
                return Err(Error("fallback publication supports symlinks only".into()));
            }
            if !self
                .store()
                .db
                .prepare("SELECT 1 FROM catalogs WHERE id=?")?
                .exists([catalog])?
            {
                return Err(Error(format!("unknown catalog: {catalog}")));
            }
            Ok(())
        } else {
            supported(self.store(), profile, catalog)
        }
    }
    fn mappings(&self, profile: &str, catalog: &str) -> Result<Vec<Value>> {
        if matches!(self, Self::Mutable(..))
            && crate::fallback_projection::binding(self.store(), profile, catalog)?.is_some()
        {
            if self.store().db.prepare("SELECT 1 FROM fallback_entries WHERE profile=? AND catalog=? AND active=1 AND state IN ('blocked','unresolved') LIMIT 1")?.exists([profile,catalog])? {
                return Err(Error("fallback membership unresolved; owned output retained".into()));
            }
            return crate::fallback_publication::effective_mappings(self.store(), profile, catalog);
        }
        mappings(self.store(), profile, catalog)
    }
    fn source_target(
        &mut self,
        profile: &str,
        mapping: &Value,
        output: &Value,
    ) -> Result<(Option<String>, Option<String>)> {
        if mapping.get("revision").is_some() {
            use crate::fallback_publication::{Outcome, capture};
            let Self::Mutable(store, probe, selections) = self else {
                return Err(Error(
                    "native fallback output reconciliation requires the mutable probe owner".into(),
                ));
            };
            let captured = match capture(store, profile, mapping, selections)? {
                Outcome::Unavailable(reason) => return Ok((None, Some(reason.into()))),
                Outcome::Stale(reason) => return Err(Error(reason.into())),
                Outcome::Ready(captured) => captured,
            };
            let (path, database) = (store.path.clone(), store.database_id.clone());
            let result = store.detached(|| {
                probe(
                    &path,
                    profile,
                    &database,
                    &captured.checks,
                    captured.timeout_ms,
                )
            })?;
            crate::fallback_resolution::require_epoch(store, captured.epoch)?;
            if result["usable"] != true {
                let reason = result["reason"].as_str().unwrap_or("probe_failed");
                if matches!(reason, "probe_timeout" | "probe_capacity" | "probe_failed") {
                    return Err(Error(format!("fallback probe incomplete: {reason}")));
                }
                return Ok((None, Some(reason.into())));
            }
            let target = Path::new(
                captured.snapshot["root"]
                    .as_str()
                    .ok_or_else(|| Error("invalid fallback source root".into()))?,
            )
            .join(
                captured.snapshot["path"]
                    .as_str()
                    .ok_or_else(|| Error("invalid fallback source path".into()))?,
            );
            let destination = Path::new(
                output["root"]
                    .as_str()
                    .ok_or_else(|| Error("invalid fallback output root".into()))?,
            )
            .join(
                mapping["path"]
                    .as_str()
                    .ok_or_else(|| Error("invalid fallback output path".into()))?,
            );
            return Ok((
                Some(relative(
                    &target,
                    destination
                        .parent()
                        .ok_or_else(|| Error("invalid fallback output path".into()))?,
                )?),
                None,
            ));
        }
        source_target(self.store(), profile, mapping, output)
    }
}
/// Mutable owner for an explicit isolated-probe callback. All ordinary journal
/// execution, leaf-race restrictions and durable transitions are shared above.
pub fn execute_mut_locked(
    store: &mut Store,
    profile: &str,
    action: &str,
    args: &Value,
    probe: &mut crate::fallback_resolution::Probe<'_>,
    guard: &PublicationGuard,
) -> Result<Value> {
    require_writer(store)?;
    if guard.path != store.path || guard.database_id != store.database_id {
        return Err(Error(
            "publication guard belongs to a different catalog".into(),
        ));
    }
    notify_consumers(args)?;
    let catalog = args["catalog"]
        .as_str()
        .ok_or_else(|| Error("locked publication requires one explicit catalog".into()))?;
    if matches!(action, "apply" | "recover")
        && crate::fallback_projection::binding(store, profile, catalog)?.is_some()
    {
        require_fallback_fixture(args)?;
    }
    let mut context = Reconciliation::Mutable(store, probe, Default::default());
    match action {
        "preview" => preview_context(&mut context, profile, catalog, args),
        "apply" => apply_locked_context(&mut context, profile, catalog, args),
        "recover" => recover_locked_context(&mut context, profile, catalog, args),
        "verify" => verify_context(&mut context, profile, catalog),
        _ => Err(Error(format!("unknown filesystem action: {action}"))),
    }
}

/// Logical publication remains restricted to explicitly disposable debug
/// fixtures while the shared filesystem qualification gate is open.
pub fn require_fallback_fixture(args: &Value) -> Result<()> {
    if cfg!(debug_assertions) && args["disposable_fixture"] == true {
        Ok(())
    } else {
        Err(Error("native fallback publication is unqualified; only disposable debug fixtures may apply or recover".into()))
    }
}
/// Named logical reconciliation with a mutable owner for every live probe.
/// Exact catalogs keep their established fixed-Store boundary unchanged.
pub fn execute_mut(
    store: &mut Store,
    profile: &str,
    action: &str,
    args: &Value,
    probe: &mut crate::fallback_resolution::Probe<'_>,
) -> Result<Value> {
    let catalog = args
        .get("catalog")
        .and_then(Value::as_str)
        .unwrap_or("global");
    let all = args.get("catalog").is_some_and(Value::is_null);
    let fallback = if all {
        store
            .db
            .prepare("SELECT 1 FROM fallback_bindings WHERE profile=? LIMIT 1")?
            .exists([profile])?
    } else {
        crate::fallback_projection::binding(store, profile, catalog)?.is_some()
    };
    if !fallback {
        return execute(store, profile, action, args);
    }
    notify_consumers(args)?;
    if matches!(action, "apply" | "recover") {
        require_fallback_fixture(args)?;
    }
    let readonly_snapshot = !store.writable && !store.db.is_autocommit();
    if readonly_snapshot {
        store.db.execute_batch("COMMIT")?;
    }
    let result = (|| {
        if all {
            let mut context = Reconciliation::Mutable(store, probe, Default::default());
            all_catalogs_context(&mut context, profile, action, args)
        } else if matches!(action, "apply" | "recover") {
            let guard = publication_lock(store)?;
            let mut request = args.clone();
            request["catalog"] = json!(catalog);
            execute_mut_locked(store, profile, action, &request, probe, &guard)
        } else {
            let mut context = Reconciliation::Mutable(store, probe, Default::default());
            match action {
                "preview" => preview_context(&mut context, profile, catalog, args),
                "verify" => verify_context(&mut context, profile, catalog),
                _ => Err(Error(format!("unknown filesystem action: {action}"))),
            }
        }
    })();
    if readonly_snapshot && store.db.is_autocommit() {
        store.db.execute_batch("BEGIN")?;
    }
    result
}

pub(crate) const MARKER: &str = ".catabolic-owner.json";
pub(crate) fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn cstring(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| Error("invalid filesystem path".into()))
}
pub(crate) fn open_at(parent: &File, name: &str, directory: bool) -> Result<File> {
    let name = cstring(name)?;
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | libc::O_CLOEXEC
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: parent is live, name is NUL terminated, and a successful call owns a new descriptor.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: openat returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub(crate) fn open_root(path: &str) -> Result<File> {
    if !Path::new(path).is_absolute()
        || Path::new(path)
            .components()
            .any(|c| c == Component::ParentDir)
    {
        return Err(Error(
            "root must be absolute and contain no parent traversal".into(),
        ));
    }
    let mut fd = File::open("/")?;
    for part in path.split('/').filter(|p| !p.is_empty()) {
        fd = open_at(&fd, part, true)?;
    }
    Ok(fd)
}
pub(crate) fn parent(root: &File, path: &str, create: bool) -> Result<(File, String)> {
    crate::layout::relative_path(path)?;
    let parts: Vec<_> = path.split('/').collect();
    let mut fd = root.try_clone()?;
    for component in &parts[..parts.len() - 1] {
        if create {
            let name = cstring(component)?;
            // SAFETY: descriptor and name remain live during mkdirat.
            if unsafe { libc::mkdirat(fd.as_raw_fd(), name.as_ptr(), 0o755) } == 0 {
                fd.sync_all()?;
            } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        fd = open_at(&fd, component, true)?;
    }
    Ok((fd, parts.last().unwrap().to_string()))
}
pub(crate) fn identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}
pub(crate) fn version(metadata: &Metadata) -> Value {
    json!({"size":metadata.size(),"mtime_ns":metadata.mtime()*1_000_000_000+metadata.mtime_nsec(),"ctime_ns":metadata.ctime()*1_000_000_000+metadata.ctime_nsec(),"device":metadata.dev(),"inode":metadata.ino()})
}
pub(crate) fn entries(root: &File, mut visit: impl FnMut(&str) -> Result<bool>) -> Result<()> {
    // Open a separate directory description; dup would share the directory cursor.
    let directory = open_at(root, ".", true)?;
    use std::os::fd::IntoRawFd;
    let fd = directory.into_raw_fd();
    // SAFETY: fd is an owned directory descriptor, transferred to DIR on success.
    let raw = unsafe { libc::fdopendir(fd) };
    if raw.is_null() {
        // SAFETY: fdopendir failed and did not take ownership.
        unsafe { libc::close(fd) };
        return Err(std::io::Error::last_os_error().into());
    }
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            // SAFETY: this wrapper uniquely owns the DIR pointer.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let directory = Directory(raw);
    loop {
        #[cfg(target_os = "linux")]
        // SAFETY: errno is thread-local and valid for this thread.
        unsafe {
            *libc::__errno_location() = 0;
        }
        #[cfg(target_os = "macos")]
        // SAFETY: errno is thread-local and valid for this thread.
        unsafe {
            *libc::__error() = 0;
        }
        // SAFETY: directory owns a valid stream; no other operation uses it.
        let row = unsafe { libc::readdir(directory.0) };
        if row.is_null() {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(0) {
                return Err(e.into());
            }
            break;
        }
        // SAFETY: d_name is NUL terminated and valid until next readdir.
        let name = unsafe { CStr::from_ptr((*row).d_name.as_ptr()) }
            .to_str()
            .map_err(|_| Error("paths must be valid UTF-8".into()))?;
        if name != "." && name != ".." && !visit(name)? {
            break;
        }
    }
    Ok(())
}
pub(crate) fn state(root: &File, path: &str) -> Result<(String, Option<String>)> {
    let (fd, leaf) = match parent(root, path, false) {
        Ok(v) => v,
        Err(e) if e.0.contains("No such file or directory") => return Ok(("absent".into(), None)),
        Err(e) => return Err(e),
    };
    state_at(&fd, &leaf)
}
pub(crate) fn stat_at(fd: &File, leaf: &str) -> Result<(u32, Value)> {
    let name = cstring(leaf)?;
    // SAFETY: zero is a valid initialization for the stat output buffer.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: descriptor, name and output buffer remain valid; no symlink is followed.
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
    #[cfg(target_os = "linux")]
    let (mtime, ctime) = (
        st.st_mtime * 1_000_000_000 + st.st_mtime_nsec,
        st.st_ctime * 1_000_000_000 + st.st_ctime_nsec,
    );
    #[cfg(target_os = "macos")]
    let (mtime, ctime) = (
        st.st_mtime * 1_000_000_000 + st.st_mtime_nsec,
        st.st_ctime * 1_000_000_000 + st.st_ctime_nsec,
    );
    Ok((
        (u64::from(st.st_mode) & u64::from(libc::S_IFMT)) as u32,
        json!({"size":st.st_size,"mtime_ns":mtime,"ctime_ns":ctime,"device":st.st_dev,"inode":st.st_ino}),
    ))
}
fn state_at(fd: &File, leaf: &str) -> Result<(String, Option<String>)> {
    let name = cstring(leaf)?;
    // SAFETY: zero is a valid initialization for the stat result buffer.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: descriptor/name/buffer are valid and symlinks are not followed.
    if unsafe {
        libc::fstatat(
            fd.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::NotFound {
            return Ok(("absent".into(), None));
        }
        return Err(e.into());
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFLNK {
        return Ok(("other".into(), None));
    }
    let mut buf = vec![0u8; 65536];
    // SAFETY: buffer capacity is passed correctly; readlinkat writes no more than len.
    let n = unsafe {
        libc::readlinkat(
            fd.as_raw_fd(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if n as usize == buf.len() {
        return Err(Error("symlink target exceeds limit".into()));
    }
    buf.truncate(n as usize);
    Ok((
        "link".into(),
        Some(String::from_utf8(buf).map_err(|_| Error("symlink target is not UTF-8".into()))?),
    ))
}
fn unlink(fd: &File, name: &str) -> Result<()> {
    let name = cstring(name)?;
    // SAFETY: descriptor and NUL-terminated name are live.
    if unsafe { libc::unlinkat(fd.as_raw_fd(), name.as_ptr(), 0) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn symlink(fd: &File, name: &str, target: &str) -> Result<()> {
    let name = cstring(name)?;
    let target = cstring(target)?;
    // SAFETY: all arguments are valid for symlinkat; it never overwrites a leaf.
    if unsafe { libc::symlinkat(target.as_ptr(), fd.as_raw_fd(), name.as_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
pub(crate) fn owner(store: &Store, profile: &str, catalog: &str) -> Value {
    crate::public_reconcile::owner(&store.database_id, profile, catalog)
}
pub(crate) fn owner_state(root: &File, expected: &Value) -> Result<&'static str> {
    match open_at(root, MARKER, false) {
        Ok(mut file) => {
            if !file.metadata()?.is_file() {
                return Err(Error(
                    "output ownership marker is not a regular file".into(),
                ));
            }
            let mut raw = Vec::new();
            Read::by_ref(&mut file).take(8193).read_to_end(&mut raw)?;
            let actual: Value = catabolic_core::decode_json_bytes(&raw)
                .map_err(|_| Error("invalid output ownership marker".into()))?;
            if &actual != expected {
                return Err(Error(
                    "output belongs to a different database, profile, or catalog".into(),
                ));
            }
            Ok("owned")
        }
        Err(e) if e.0.contains("No such file or directory") => {
            let mut empty = true;
            entries(root, |_| {
                empty = false;
                Ok(false)
            })?;
            Ok(if empty { "empty" } else { "unowned" })
        }
        Err(e) => Err(Error(format!("unsafe output ownership marker: {e}"))),
    }
}
pub(crate) fn claim_output(root: &File, expected: &Value, id: &str) -> Result<()> {
    let temporary = format!(".catabolic-claim-{id}");
    if state_at(root, MARKER)?.0 != "absent" {
        owner_state(root, expected)?;
        if state_at(root, &temporary)?.0 != "absent" {
            remove_claim(root, &temporary)?;
        }
        return Ok(());
    }
    let mut names = Vec::new();
    entries(root, |name| {
        names.push(name.to_string());
        Ok(names.len() < 3)
    })?;
    if names.iter().any(|n| n == MARKER) {
        owner_state(root, expected)?;
        if names.contains(&temporary) {
            remove_claim(root, &temporary)?;
        }
        return Ok(());
    }
    if names.iter().any(|n| n != &temporary) {
        return Err(Error("only an empty output can be claimed".into()));
    }
    if names.contains(&temporary) {
        remove_claim(root, &temporary)?;
    }
    let name = cstring(&temporary)?;
    // SAFETY: valid directory/name; O_EXCL and O_NOFOLLOW prevent overwrite and symlink following.
    let fd = unsafe {
        libc::openat(
            root.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful openat returned a fresh owned descriptor.
    let mut file = unsafe { File::from_raw_fd(fd) };
    writeln!(file, "{}", encode(expected))?;
    file.sync_all()?;
    let marker = cstring(MARKER)?;
    // SAFETY: names/descriptor remain live; linkat without follow flags publishes without overwrite.
    if unsafe {
        libc::linkat(
            root.as_raw_fd(),
            name.as_ptr(),
            root.as_raw_fd(),
            marker.as_ptr(),
            0,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    root.sync_all()?;
    unlink(root, &temporary)?;
    root.sync_all()?;
    Ok(())
}
fn remove_claim(root: &File, name: &str) -> Result<()> {
    if !open_at(root, name, false)?.metadata()?.is_file() {
        return Err(Error("claim recovery file was changed externally".into()));
    }
    unlink(root, name)
}

/// Preview or execute native symlink reconciliation. Unsupported modes are
/// refused before journaling or modifying any output.
pub fn execute(store: &Store, profile: &str, action: &str, args: &Value) -> Result<Value> {
    notify_consumers(args)?;
    if args.get("catalog").is_some_and(Value::is_null) {
        return all_catalogs(store, profile, action, args);
    }
    let catalog = args
        .get("catalog")
        .map(|v| {
            v.as_str()
                .ok_or_else(|| Error("catalog must be text or null".into()))
        })
        .transpose()?
        .unwrap_or("global");
    match action {
        "preview" => preview(store, profile, catalog, args),
        "apply" => apply(store, profile, catalog, args),
        "recover" => recover(store, profile, catalog, args),
        "verify" => verify(store, profile, catalog),
        _ => Err(Error(format!("unknown filesystem action: {action}"))),
    }
}
pub(crate) fn notify_consumers(args: &Value) -> Result<bool> {
    args.get("notify_consumers")
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| Error("notify_consumers must be boolean".into()))
        })
        .transpose()
        .map(|v| v.unwrap_or(true))
}
fn all_catalogs(store: &Store, profile: &str, action: &str, args: &Value) -> Result<Value> {
    all_catalogs_context(&mut Reconciliation::Fixed(store), profile, action, args)
}
fn all_catalogs_context(
    context: &mut Reconciliation<'_, '_>,
    profile: &str,
    action: &str,
    args: &Value,
) -> Result<Value> {
    let catalogs: Vec<String> = context
        .store()
        .db
        .prepare("SELECT id FROM catalogs ORDER BY id")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    if action == "recover" {
        let mut recovered = Vec::new();
        let mut cancelled = Vec::new();
        for catalog in &catalogs {
            let _guard = publication_lock(context.store())?;
            let result = recover_locked_context(context, profile, catalog, args)?;
            recovered.extend(result["recovered"].as_array().unwrap().iter().cloned());
            cancelled.extend(result["cancelled"].as_array().unwrap().iter().cloned());
        }
        return Ok(
            json!({"recovered":recovered,"cancelled":cancelled,"next":"run sync --dry-run to inspect current desired state"}),
        );
    }
    if action == "verify" {
        return verify_catalogs_context(context, profile, &catalogs);
    }
    if !matches!(action, "preview" | "apply") {
        return Err(Error(format!("unknown filesystem action: {action}")));
    }
    let _lock = if action == "apply" {
        require_writer(context.store())?;
        Some(publication_lock(context.store())?)
    } else {
        None
    };
    let mut local = args.clone();
    local["max_removals"] = Value::Null;
    local["max_removal_percent"] = Value::Null;
    let mut actions = Vec::new();
    let mut blockers = Vec::new();
    let mut removals = 0u64;
    let mut count = 0u64;
    for catalog in &catalogs {
        let plan = preview_context(context, profile, catalog, &local)?;
        actions.extend(plan["actions"].as_array().unwrap().iter().cloned());
        blockers.extend(plan["blockers"].as_array().unwrap().iter().cloned());
        removals += plan["removal_budget"]["removals"].as_u64().unwrap();
        count += plan["removal_budget"]["owned_entries"].as_u64().unwrap();
    }
    actions.sort_by(|a, b| {
        order(a["kind"].as_str().unwrap())
            .cmp(&order(b["kind"].as_str().unwrap()))
            .then(a["catalog"].as_str().cmp(&b["catalog"].as_str()))
            .then(a["path"].as_str().cmp(&b["path"].as_str()))
    });
    let percent = if count == 0 {
        0.0
    } else {
        100.0 * removals as f64 / count as f64
    };
    let max = args
        .get("max_removals")
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| Error("max removals must be a nonnegative integer".into()))
        })
        .transpose()?;
    let maximum_percent = args
        .get("max_removal_percent")
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_f64()
                .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
                .ok_or_else(|| {
                    Error("max removal percent must be finite and between 0 and 100".into())
                })
        })
        .transpose()?;
    if max.is_some_and(|n| removals > n) || maximum_percent.is_some_and(|n| percent > n) {
        blockers.push(json!({"catalog":null,"reason":"bulk-removal limit exceeded; review the full plan before increasing the explicit limit"}));
    }
    let mut plan = json!({"safe":blockers.is_empty(),"actions":actions,"blockers":blockers,"removal_budget":{"removals":removals,"owned_entries":count,"percent":percent,"max_removals":max,"max_removal_percent":maximum_percent}});
    let mut hard_catalogs = Vec::new();
    for catalog in &catalogs {
        if hardlink_mode(context.store(), catalog)? {
            hard_catalogs.push(catalog.clone());
        }
    }
    if !hard_catalogs.is_empty() {
        plan["warnings"] = json!([crate::hardlink_mutation::WARNING]);
        plan["hardlink_catalogs"] = json!(hard_catalogs);
    }
    if action == "preview" {
        return Ok(plan);
    }
    if plan["safe"] != true {
        plan["applied"] = json!([]);
        plan["healthy"] = json!(false);
        return Ok(plan);
    }
    let mutations = actions
        .iter()
        .filter(|a| {
            !matches!(
                a["kind"].as_str(),
                Some("prepare" | "unchanged" | "blocked_source")
            )
        })
        .count();
    if let Some(max) = args.get("max_changes").filter(|v| !v.is_null())
        && mutations as u64
            > max
                .as_u64()
                .ok_or_else(|| Error("max changes must be a nonnegative integer".into()))?
    {
        return Err(Error("fallback_change_or_removal_budget".into()));
    }
    let mut applied = Vec::new();
    for mut op in actions {
        if matches!(op["kind"].as_str(), Some("unchanged" | "blocked_source")) {
            continue;
        }
        let action = op.clone();
        op["id"] = json!(uuid::Uuid::new_v4().to_string());
        op["profile"] = json!(profile);
        context.store().transaction(|db|{db.execute("INSERT INTO journal(id,profile,catalog,path,kind,target,previous) VALUES(?,?,?,?,?,?,?)",params![op["id"].as_str(),profile,op["catalog"].as_str(),op["path"].as_str(),op["kind"].as_str(),op["target"].as_str(),op["previous"].as_str()])?;Ok(())})?;
        failpoint(args, "after_journal")?;
        execute_operation_context(context, profile, &op, args)?;
        applied.push(action);
    }
    let verified = verify_catalogs_context(context, profile, &catalogs)?;
    let mut result = json!({"safe":true,"applied":applied,"healthy":verified["healthy"],"verification":verified});
    if let Some(warnings) = plan.get("warnings") {
        result["warnings"] = warnings.clone();
    }
    for catalog in &catalogs {
        published_context(
            context,
            profile,
            catalog,
            &mut result,
            notify_consumers(args)?,
        )?;
    }
    Ok(result)
}
fn verify_catalogs_context(
    context: &mut Reconciliation<'_, '_>,
    profile: &str,
    catalogs: &[String],
) -> Result<Value> {
    let mut reports = Vec::new();
    for catalog in catalogs {
        reports.extend(
            verify_context(context, profile, catalog)?["catalogs"]
                .as_array()
                .unwrap()
                .iter()
                .cloned(),
        );
    }
    Ok(json!({"healthy":reports.iter().all(|r|r["healthy"]==true),"catalogs":reports}))
}
fn hardlink_mode(store: &Store, catalog: &str) -> Result<bool> {
    Ok(store
        .db
        .query_row(
            "SELECT mode='hardlink' FROM catalog_link_modes WHERE catalog=?",
            [catalog],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false))
}
fn supported(store: &Store, profile: &str, catalog: &str) -> Result<()> {
    if !store
        .db
        .prepare("SELECT 1 FROM catalogs WHERE id=?")?
        .exists([catalog])?
    {
        return Err(Error(format!("unknown catalog: {catalog}")));
    }
    if store
        .db
        .prepare("SELECT 1 FROM fallback_bindings WHERE profile=? AND catalog=?")?
        .exists([profile, catalog])?
    {
        return Err(Error(
            "native fallback output reconciliation is not yet qualified".into(),
        ));
    }
    Ok(())
}
fn owned(store: &Store, profile: &str, catalog: &str) -> Result<BTreeMap<String, String>> {
    Ok(store
        .db
        .prepare("SELECT path,target FROM owned_links WHERE profile=? AND catalog=?")?
        .query_map([profile, catalog], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?)
}
pub(crate) fn mappings(store: &Store, profile: &str, catalog: &str) -> Result<Vec<Value>> {
    rows(
        &store.db,
        "SELECT m.path,m.file_id,f.location,f.path AS source_path,o.size,o.mtime_ns,o.device,o.inode,o.status FROM mappings m JOIN files f ON f.id=m.file_id LEFT JOIN observations o ON o.file_id=f.id AND o.profile=? WHERE m.catalog=? AND m.active=1 ORDER BY m.path",
        &[profile.to_string().into(), catalog.to_string().into()],
    )
}
pub(crate) fn relative(target: &Path, parent: &Path) -> Result<String> {
    let a: Vec<_> = target.components().collect();
    let b: Vec<_> = parent.components().collect();
    let n = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let mut result = vec!["..".to_string(); b.len() - n];
    for p in &a[n..] {
        result.push(
            p.as_os_str()
                .to_str()
                .ok_or_else(|| Error("paths must be valid UTF-8".into()))?
                .to_string(),
        );
    }
    Ok(result.join("/"))
}
pub(crate) fn source_target(
    store: &Store,
    profile: &str,
    mapping: &Value,
    output: &Value,
) -> Result<(Option<String>, Option<String>)> {
    let binding = crate::observation::binding(
        store,
        profile,
        "source",
        mapping["location"].as_str().unwrap(),
    )?;
    let (root, _) = crate::observation::checked_root(store, profile, &binding)?;
    let path = mapping["source_path"].as_str().unwrap();
    if mapping["status"].is_null() {
        return Err(Error(format!("source needs a complete scan: {path}")));
    }
    let (parent, leaf) = match parent(&root, path, false) {
        Ok(v) => v,
        Err(e) if e.0.contains("No such file or directory") && mapping["status"] == "missing" => {
            return Ok((None, Some("confirmed_missing".into())));
        }
        Err(e) => return Err(e),
    };
    let file = match open_at(&parent, &leaf, false) {
        Ok(v) => v,
        Err(e) if e.0.contains("No such file or directory") => {
            if mapping["status"] == "missing" {
                return Ok((None, Some("confirmed_missing".into())));
            }
            return Err(Error(format!("unconfirmed missing source: {path}")));
        }
        Err(e) => return Err(e),
    };
    if mapping["status"] == "missing" {
        return Err(Error(format!(
            "source returned; scan before synchronization: {path}"
        )));
    }
    let metadata = file.metadata()?;
    let current = version(&metadata);
    if !metadata.is_file()
        || ["size", "mtime_ns", "device", "inode"]
            .iter()
            .any(|k| current[k] != mapping[k])
    {
        return Err(Error(format!(
            "source changed; scan before synchronization: {path}"
        )));
    }
    let health = crate::observation::header_check(&file, path, &current)?;
    let reason = if health["status"] == "invalid" {
        health["reason"].as_str().map(str::to_owned)
    } else {
        recorded_reason(
            store,
            profile,
            mapping["file_id"].as_str().unwrap(),
            &current,
        )?
    };
    if let Some(reason) = reason {
        return Ok((None, Some(reason)));
    }
    let named = open_at(&parent, &leaf, false)?;
    if version(&named.metadata()?) != current || version(&file.metadata()?) != current {
        return Err(Error("source changed while checking media header".into()));
    }
    let (current_root, _) = crate::observation::checked_root(store, profile, &binding)?;
    if identity(&current_root.metadata()?) != identity(&root.metadata()?) {
        return Err(Error("source root changed during reconciliation".into()));
    }
    let target = Path::new(binding["root"].as_str().unwrap()).join(path);
    let destination =
        Path::new(output["root"].as_str().unwrap()).join(mapping["path"].as_str().unwrap());
    Ok((
        Some(relative(&target, destination.parent().unwrap())?),
        None,
    ))
}
pub(crate) fn recorded_reason(
    store: &Store,
    profile: &str,
    file: &str,
    current: &Value,
) -> Result<Option<String>> {
    for operation in ["probe", "decode"] {
        let matches = rows(
            &store.db,
            "WITH evidence AS (SELECT a.rowid AS sequence,a.state,a.result,j.options FROM processing_jobs j JOIN processing_attempts a ON a.job_id=j.id WHERE j.profile=? AND j.file_id=? AND j.operation=? AND a.state IN ('failed','complete') AND json_extract(snapshot,'$.size')=? AND json_extract(snapshot,'$.mtime_ns')=? AND json_extract(snapshot,'$.device')=? AND json_extract(snapshot,'$.inode')=? AND json_extract(snapshot,'$.ctime_ns')=?) SELECT bad.result FROM evidence bad WHERE json_extract(bad.result,'$.media_health.status')='invalid' AND NOT EXISTS (SELECT 1 FROM evidence good WHERE good.sequence>bad.sequence AND good.state='complete' AND json_extract(good.result,'$.media_health.status') IN ('not_detected','content_mismatch') AND (?='decode' OR (json_extract(good.options,'$.extractor.adapter')='probe-v3' AND coalesce(json_extract(good.options,'$.analysis_bytes'),0)>=coalesce(json_extract(bad.options,'$.analysis_bytes'),9223372036854775807) AND coalesce(json_extract(good.options,'$.analysis_us'),0)>=coalesce(json_extract(bad.options,'$.analysis_us'),9223372036854775807)))) ORDER BY bad.sequence DESC LIMIT 1",
            &[
                profile.to_string().into(),
                file.to_string().into(),
                operation.to_string().into(),
                crate::sql::scalar(&current["size"])?,
                crate::sql::scalar(&current["mtime_ns"])?,
                crate::sql::scalar(&current["device"])?,
                crate::sql::scalar(&current["inode"])?,
                crate::sql::scalar(&current["ctime_ns"])?,
                operation.to_string().into(),
            ],
        )?;
        if let Some(row) = matches.first() {
            let result: Value = catabolic_core::decode_json(row["result"].as_str().unwrap())?;
            return Ok(result["media_health"]["reason"].as_str().map(str::to_owned));
        }
    }
    Ok(None)
}
fn order(kind: &str) -> usize {
    match kind {
        "prepare" => 0,
        "blocked_source" => 1,
        "unchanged" => 2,
        "create" | "hard_create" => 3,
        "replace" | "hard_replace" => 4,
        "remove" | "hard_remove" => 5,
        "forget" | "hard_forget" => 6,
        _ => 7,
    }
}
fn delta_context(
    context: &mut Reconciliation<'_, '_>,
    profile: &str,
    catalog: &str,
) -> Result<Vec<Value>> {
    context.supported(profile, catalog)?;
    if hardlink_mode(context.store(), catalog)? {
        return crate::hardlink_mutation::delta(context.store(), profile, catalog);
    }
    let output = crate::observation::binding(context.store(), profile, "output", catalog)?;
    let (root, _) = crate::observation::checked_root(context.store(), profile, &output)?;
    let owned = owned(context.store(), profile, catalog)?;
    let mut actions = Vec::new();
    let mut desired = BTreeMap::new();
    match owner_state(&root, &owner(context.store(), profile, catalog))? {
        "unowned" => {
            return Err(Error(
                "output is not empty and has no matching ownership marker".into(),
            ));
        }
        "empty" => {
            if !owned.is_empty() {
                return Err(Error(
                    "ownership marker disappeared from a previously managed output".into(),
                ));
            }
            actions.push(json!({"kind":"prepare","catalog":catalog,"path":"","target":null,"previous":null,"reason":null}));
        }
        _ => {}
    }
    for mapping in context.mappings(profile, catalog)? {
        let (target, reason) = context.source_target(profile, &mapping, &output)?;
        if let Some(target) = target {
            desired.insert(mapping["path"].as_str().unwrap().to_string(), target);
        } else {
            actions.push(json!({"kind":"blocked_source","catalog":catalog,"path":mapping["path"],"target":null,"previous":null,"reason":reason}));
        }
    }
    for (path, target) in &desired {
        let (state, actual) = state(&root, path)?;
        let previous = owned.get(path);
        let kind = if state == "absent" {
            "create"
        } else if state != "link" || previous.is_none() || actual.as_ref() != previous {
            return Err(Error(format!(
                "destination collision or externally changed owned link: {path}"
            )));
        } else if actual.as_ref() == Some(target) {
            "unchanged"
        } else {
            "replace"
        };
        actions.push(json!({"kind":kind,"catalog":catalog,"path":path,"target":target,"previous":previous,"reason":null}));
    }
    for (path, target) in &owned {
        if desired.contains_key(path) {
            continue;
        }
        let (state, actual) = state(&root, path)?;
        let kind = if state == "absent" {
            "forget"
        } else if state == "link" && actual.as_ref() == Some(target) {
            "remove"
        } else {
            return Err(Error(format!("owned path changed externally: {path}")));
        };
        actions.push(json!({"kind":kind,"catalog":catalog,"path":path,"target":null,"previous":target,"reason":null}));
    }
    actions.sort_by(|a, b| {
        order(a["kind"].as_str().unwrap())
            .cmp(&order(b["kind"].as_str().unwrap()))
            .then(a["path"].as_str().cmp(&b["path"].as_str()))
    });
    Ok(actions)
}
const SYMLINK_RACE_GATE: &str = "symlink replacement/removal is blocked: concurrent external leaf-writer safety is not yet qualified";
fn symlink_leaf_allowed(kind: &str, args: &Value) -> bool {
    if !matches!(kind, "replace" | "remove") {
        return true;
    }
    // This exception is absent from optimized release builds. It only permits
    // deterministic disposable-fixture qualification of the frozen journal.
    if cfg!(all(test, debug_assertions)) {
        return true;
    }
    #[cfg(debug_assertions)]
    if args["disposable_fixture"] == true {
        return true;
    }
    #[cfg(not(debug_assertions))]
    let _ = args;
    false
}

fn preview(store: &Store, profile: &str, catalog: &str, args: &Value) -> Result<Value> {
    preview_context(&mut Reconciliation::Fixed(store), profile, catalog, args)
}
fn preview_context(
    context: &mut Reconciliation<'_, '_>,
    profile: &str,
    catalog: &str,
    args: &Value,
) -> Result<Value> {
    let max = args
        .get("max_removals")
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| Error("max removals must be a nonnegative integer".into()))
        })
        .transpose()?;
    let percent = args
        .get("max_removal_percent")
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_f64()
                .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
                .ok_or_else(|| {
                    Error("max removal percent must be finite and between 0 and 100".into())
                })
        })
        .transpose()?;
    let mut blockers = Vec::new();
    let actions = if context
        .store()
        .db
        .prepare("SELECT 1 FROM journal WHERE profile=? AND catalog=?")?
        .exists([profile, catalog])?
    {
        blockers.push(json!({"catalog":catalog,"reason":"pending operations require recover before a fresh preview"}));
        vec![]
    } else {
        match delta_context(context, profile, catalog) {
            Ok(v) => v,
            Err(e) => {
                blockers.push(json!({"catalog":catalog,"reason":e.0}));
                vec![]
            }
        }
    };
    if actions
        .iter()
        .any(|action| !symlink_leaf_allowed(action["kind"].as_str().unwrap_or(""), args))
    {
        blockers.push(json!({"catalog":catalog,"reason":SYMLINK_RACE_GATE}));
    }
    if actions.iter().any(|action| {
        !crate::hardlink_mutation::retirement_allowed(action["kind"].as_str().unwrap_or(""), args)
    }) {
        blockers.push(
            json!({"catalog":catalog,"reason":crate::hardlink_mutation::RETIREMENT_RACE_GATE}),
        );
    }
    let removals = actions
        .iter()
        .filter(|a| matches!(a["kind"].as_str(), Some("remove" | "hard_remove")))
        .count();
    let hard = hardlink_mode(context.store(), catalog)?;
    let count: i64 = context.store().db.query_row(
        &format!(
            "SELECT count(*) FROM {} WHERE profile=? AND catalog=?",
            if hard {
                "owned_hardlinks"
            } else {
                "owned_links"
            }
        ),
        [profile, catalog],
        |r| r.get(0),
    )?;
    let actual = if count == 0 {
        0.0
    } else {
        100.0 * removals as f64 / count as f64
    };
    if max.is_some_and(|m| removals as u64 > m) || percent.is_some_and(|p| actual > p) {
        blockers.push(json!({"catalog":catalog,"reason":"bulk-removal limit exceeded; review the full plan before increasing the explicit limit"}));
    }
    let mut result = json!({"safe":blockers.is_empty(),"removal_budget":{"removals":removals,"owned_entries":count,"percent":actual,"max_removals":max,"max_removal_percent":percent},"actions":actions,"blockers":blockers});
    if hard {
        result["warnings"] = json!([crate::hardlink_mutation::WARNING]);
        result["hardlink_catalogs"] = json!([catalog]);
    }
    Ok(result)
}
fn require_writer(store: &Store) -> Result<()> {
    if !store.owns_writer_lock() || !store.db.is_autocommit() {
        return Err(Error(
            "filesystem publication requires a writer outside an active transaction".into(),
        ));
    }
    Ok(())
}
/// Opaque publication serialization guard. It may outlive a detached SQLite
/// session but only admits effects against the same catalog identity and path.
pub struct PublicationGuard {
    _file: crate::WriterLock,
    path: std::path::PathBuf,
    database_id: String,
}
pub fn publication_lock(store: &Store) -> Result<PublicationGuard> {
    require_writer(store)?;
    let mut path = store.path.as_os_str().to_os_string();
    path.push(".publication");
    Ok(PublicationGuard {
        _file: crate::writer_lock(Path::new(&path))?,
        path: store.path.clone(),
        database_id: store.database_id.clone(),
    })
}
/// Reuse an existing publication guard across projection planning/admission and
/// reconciliation. The ordinary protocol, gates and journal remain unchanged.
pub fn execute_locked(
    store: &Store,
    profile: &str,
    action: &str,
    args: &Value,
    guard: &PublicationGuard,
) -> Result<Value> {
    require_writer(store)?;
    if guard.path != store.path || guard.database_id != store.database_id {
        return Err(Error(
            "publication guard belongs to a different catalog".into(),
        ));
    }
    notify_consumers(args)?;
    let catalog = args["catalog"]
        .as_str()
        .ok_or_else(|| Error("locked publication requires one explicit catalog".into()))?;
    match action {
        "preview" => preview(store, profile, catalog, args),
        "apply" => apply_locked(store, profile, catalog, args),
        "recover" => recover_locked(store, profile, catalog, args),
        "verify" => verify(store, profile, catalog),
        _ => Err(Error(format!("unknown filesystem action: {action}"))),
    }
}
pub(crate) fn failpoint(args: &Value, stage: &str) -> Result<()> {
    if args["failpoint"] == stage {
        if args["crash"] == true {
            std::process::exit(86);
        }
        return Err(Error(format!("injected filesystem failure: {stage}")));
    }
    Ok(())
}
fn apply(store: &Store, profile: &str, catalog: &str, args: &Value) -> Result<Value> {
    require_writer(store)?;
    let _lock = publication_lock(store)?;
    apply_locked(store, profile, catalog, args)
}
fn apply_locked(store: &Store, profile: &str, catalog: &str, args: &Value) -> Result<Value> {
    apply_locked_context(&mut Reconciliation::Fixed(store), profile, catalog, args)
}
fn apply_locked_context(
    context: &mut Reconciliation<'_, '_>,
    profile: &str,
    catalog: &str,
    args: &Value,
) -> Result<Value> {
    let mut plan = preview_context(context, profile, catalog, args)?;
    if plan["safe"] != true {
        plan["applied"] = json!([]);
        plan["healthy"] = json!(false);
        return Ok(plan);
    }
    let actions = plan["actions"].as_array().unwrap().clone();
    let mutations = actions
        .iter()
        .filter(|a| {
            !matches!(
                a["kind"].as_str(),
                Some("prepare" | "unchanged" | "blocked_source")
            )
        })
        .count();
    if let Some(max) = args.get("max_changes").filter(|v| !v.is_null())
        && mutations as u64
            > max
                .as_u64()
                .ok_or_else(|| Error("max changes must be a nonnegative integer".into()))?
    {
        return Err(Error("fallback_change_or_removal_budget".into()));
    }
    let mut applied = Vec::new();
    for mut operation in actions {
        if matches!(
            operation["kind"].as_str(),
            Some("unchanged" | "blocked_source")
        ) {
            continue;
        }
        operation["id"] = json!(uuid::Uuid::new_v4().to_string());
        operation["profile"] = json!(profile);
        context.store().transaction(|db|{db.execute("INSERT INTO journal(id,profile,catalog,path,kind,target,previous) VALUES(?,?,?,?,?,?,?)",params![operation["id"].as_str(),profile,catalog,operation["path"].as_str(),operation["kind"].as_str(),operation["target"].as_str(),operation["previous"].as_str()])?;Ok(())})?;
        failpoint(args, "after_journal")?;
        execute_operation_context(context, profile, &operation, args)?;
        operation.as_object_mut().unwrap().remove("id");
        operation.as_object_mut().unwrap().remove("profile");
        applied.push(operation);
    }
    let verified = verify_context(context, profile, catalog)?;
    let mut result = json!({"safe":true,"applied":applied,"healthy":verified["healthy"],"verification":verified});
    if let Some(warnings) = plan.get("warnings") {
        result["warnings"] = warnings.clone();
    }
    published_context(
        context,
        profile,
        catalog,
        &mut result,
        notify_consumers(args)?,
    )?;
    Ok(result)
}
fn validate_intent_context(
    context: &mut Reconciliation<'_, '_>,
    profile: &str,
    catalog: &str,
    path: &str,
    target: Option<&str>,
) -> Result<()> {
    let mapping = context
        .mappings(profile, catalog)?
        .into_iter()
        .find(|mapping| mapping["path"] == path);
    let output = crate::observation::binding(context.store(), profile, "output", catalog)?;
    if let Some(mapping) = mapping {
        let (current, reason) = context.source_target(profile, &mapping, &output)?;
        if target.is_some() && current.as_deref() == target
            || target.is_none() && current.is_none() && reason.is_some()
        {
            return Ok(());
        }
    } else if target.is_none() {
        return Ok(());
    }
    Err(Error(
        if target.is_some() {
            "pending link target is no longer desired; inspect catalog state"
        } else {
            "pending removal is no longer desired; inspect catalog state"
        }
        .into(),
    ))
}
#[cfg(test)]
thread_local! {
    // Test-only deterministic scheduler for the unresolved POSIX leaf race.
    static BEFORE_DESTRUCTIVE_LEAF: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}
#[cfg(test)]
fn before_destructive_leaf() {
    BEFORE_DESTRUCTIVE_LEAF.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}

fn execute_operation_context(
    context: &mut Reconciliation<'_, '_>,
    profile: &str,
    op: &Value,
    args: &Value,
) -> Result<()> {
    let catalog = op["catalog"].as_str().unwrap();
    context.supported(profile, catalog)?;
    if op["kind"]
        .as_str()
        .is_some_and(|kind| kind.starts_with("hard_"))
    {
        return crate::hardlink_mutation::execute(context.store(), profile, op, args);
    }

    let output = crate::observation::binding(context.store(), profile, "output", catalog)?;
    let (root, _) = crate::observation::checked_root(context.store(), profile, &output)?;
    let kind = op["kind"].as_str().unwrap();
    if !symlink_leaf_allowed(kind, args) {
        return Err(Error(SYMLINK_RACE_GATE.into()));
    }
    let path = op["path"].as_str().unwrap();
    let id = op["id"].as_str().unwrap();
    uuid::Uuid::parse_str(id).map_err(|_| Error("invalid journal operation identity".into()))?;
    let target = op["target"].as_str();
    let previous = op["previous"].as_str();
    if kind == "prepare" {
        claim_output(&root, &owner(context.store(), profile, catalog), id)?;
    } else {
        if owner_state(&root, &owner(context.store(), profile, catalog))? != "owned" {
            return Err(Error(
                "output ownership missing during operation recovery".into(),
            ));
        }
        let (current, actual) = state(&root, path)?;
        match kind {
            "create" | "replace" => {
                let target = target.ok_or_else(|| Error("journal target is missing".into()))?;
                if (current.as_str(), actual.as_deref()) != ("link", Some(target)) {
                    validate_intent_context(context, profile, catalog, path, Some(target))?;
                    if kind == "create" && current != "absent"
                        || kind == "replace"
                            && (current.as_str(), actual.as_deref()) != ("link", previous)
                    {
                        return Err(Error(format!(
                            "{kind} destination changed externally: {path}"
                        )));
                    }
                    let (parent, leaf) = parent(&root, path, true)?;
                    if kind == "create" {
                        symlink(&parent, &leaf, target)?;
                    } else {
                        let temporary = format!(".catabolic-link-{id}");
                        match state_at(&parent, &temporary)? {
                            (s, _) if s == "absent" => {
                                symlink(&parent, &temporary, target)?;
                                parent.sync_all()?;
                            }
                            (s, t) if s == "link" && t.as_deref() == Some(target) => {}
                            _ => {
                                return Err(Error(
                                    "replacement recovery link changed externally".into(),
                                ));
                            }
                        }
                        failpoint(args, "after_temporary")?;
                        if state_at(&parent, &leaf)? != ("link".into(), previous.map(str::to_owned))
                        {
                            return Err(Error(format!(
                                "owned link changed before replacement: {path}"
                            )));
                        }
                        let temporary = cstring(&temporary)?;
                        let leaf = cstring(&leaf)?;
                        #[cfg(test)]
                        before_destructive_leaf();
                        // SAFETY: both entries are relative to the verified output parent; inputs are live.
                        if unsafe {
                            libc::renameat(
                                parent.as_raw_fd(),
                                temporary.as_ptr(),
                                parent.as_raw_fd(),
                                leaf.as_ptr(),
                            )
                        } < 0
                        {
                            return Err(std::io::Error::last_os_error().into());
                        }
                    }
                    parent.sync_all()?;
                }
            }
            "remove" | "forget" => {
                if current != "absent" {
                    if kind == "forget"
                        || (current.as_str(), actual.as_deref()) != ("link", previous)
                    {
                        return Err(Error(format!(
                            "removal destination changed externally: {path}"
                        )));
                    }
                    validate_intent_context(context, profile, catalog, path, None)?;
                    let (parent, leaf) = parent(&root, path, false)?;
                    if state_at(&parent, &leaf)? != ("link".into(), previous.map(str::to_owned)) {
                        return Err(Error(format!("owned link changed before removal: {path}")));
                    }
                    #[cfg(test)]
                    before_destructive_leaf();
                    unlink(&parent, &leaf)?;
                    parent.sync_all()?;
                }
            }
            _ => return Err(Error(format!("unknown journal operation: {kind}"))),
        }
    }
    failpoint(args, "after_filesystem")?;
    context.store().transaction(|db|{
        if matches!(kind,"create"|"replace"){db.execute("INSERT INTO owned_links VALUES(?,?,?,?) ON CONFLICT(profile,catalog,path) DO UPDATE SET target=excluded.target",params![profile,catalog,path,target])?;}
        else if matches!(kind,"remove"|"forget"){db.execute("DELETE FROM owned_links WHERE profile=? AND catalog=? AND path=?",[profile,catalog,path])?;}
        if matches!(kind,"create"|"replace"|"remove"){record_change(db,profile,catalog,path,&output,notify_consumers(args)?)?;}
        db.execute("DELETE FROM journal WHERE id=?",[id])?;failpoint(args,"before_commit")?;Ok(())
    })?;
    failpoint(args, "after_commit")?;
    Ok(())
}
pub(crate) fn record_change(
    db: &rusqlite::Connection,
    profile: &str,
    catalog: &str,
    path: &str,
    output: &Value,
    legacy_refresh: bool,
) -> Result<()> {
    db.execute("INSERT INTO publication_generations(profile,catalog,local_binding,generation) VALUES(?,?,?,1) ON CONFLICT(profile,catalog) DO UPDATE SET generation=generation+1,local_binding=excluded.local_binding",params![profile,catalog,encode(output)])?;
    db.execute("UPDATE consumer_bindings SET generation=generation+1,due_at=?+debounce,indexing=NULL WHERE profile=? AND catalog=? AND enabled=1 AND (subtree='' OR ?=subtree OR substr(?,1,length(subtree)+1)=subtree||'/')",params![now(),profile,catalog,path,path])?;
    if !legacy_refresh {
        return Ok(());
    }
    let targets = rows(
        db,
        "SELECT * FROM refresh_targets WHERE profile=? AND catalog=?",
        &[profile.to_string().into(), catalog.to_string().into()],
    )?;
    if let Some(target) = targets.first() {
        db.execute("INSERT INTO refresh_dirty VALUES(?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET target=excluded.target",params![profile,catalog,encode(target)])?;
    }
    Ok(())
}
fn recover(store: &Store, profile: &str, catalog: &str, args: &Value) -> Result<Value> {
    require_writer(store)?;
    let _lock = publication_lock(store)?;
    recover_locked(store, profile, catalog, args)
}
fn recover_locked(store: &Store, profile: &str, catalog: &str, args: &Value) -> Result<Value> {
    recover_locked_context(&mut Reconciliation::Fixed(store), profile, catalog, args)
}
fn recover_locked_context(
    context: &mut Reconciliation<'_, '_>,
    profile: &str,
    catalog: &str,
    args: &Value,
) -> Result<Value> {
    context.supported(profile, catalog)?;
    let operations = rows(
        &context.store().db,
        "SELECT * FROM journal WHERE profile=? AND catalog=? ORDER BY rowid",
        &[profile.to_string().into(), catalog.to_string().into()],
    )?;
    let mut recovered = Vec::new();
    let mut cancelled = Vec::new();
    for op in operations {
        uuid::Uuid::parse_str(op["id"].as_str().unwrap())
            .map_err(|_| Error("invalid journal operation identity".into()))?;
        let kind = op["kind"].as_str().unwrap();
        let path = op["path"].as_str().unwrap();
        if !crate::hardlink_mutation::retirement_allowed(kind, args) {
            return Err(Error(crate::hardlink_mutation::RETIREMENT_RACE_GATE.into()));
        }
        if kind.starts_with("hard_")
            && crate::hardlink_mutation::cancel_obsolete(
                context.store(),
                profile,
                &op,
                args["cancel_unapplied"].as_bool().unwrap_or(false),
            )?
        {
            cancelled.push(op["id"].clone());
            continue;
        }

        if matches!(kind, "create" | "replace" | "remove") {
            let output = crate::observation::binding(context.store(), profile, "output", catalog)?;
            let (root, _) = crate::observation::checked_root(context.store(), profile, &output)?;
            let initial = if kind == "create" {
                ("absent".to_string(), None)
            } else {
                ("link".into(), op["previous"].as_str().map(str::to_owned))
            };
            if state(&root, path)? == initial {
                let desired = match delta_context(context, profile, catalog) {
                    Err(error)
                        if matches!(
                            error.0.as_str(),
                            "stale_resolution_policy"
                                | "stale_resolution_membership"
                                | "stale_resolution_candidate"
                                | "stale_resolution_evidence"
                        ) =>
                    {
                        if owner_state(&root, &owner(context.store(), profile, catalog))? != "owned"
                        {
                            return Err(Error(
                                "output ownership missing during cancellation".into(),
                            ));
                        }
                        let recorded = owned(context.store(), profile, catalog)?;
                        if recorded.get(path).map(String::as_str) != op["previous"].as_str() {
                            return Err(Error(
                                "recorded ownership changed during cancellation".into(),
                            ));
                        }
                        vec![]
                    }
                    result => result?,
                };
                if !desired.iter().any(|a| {
                    ["kind", "path", "target", "previous"]
                        .iter()
                        .all(|k| a[k] == op[k])
                }) {
                    if kind == "replace" {
                        let (parent, _) = parent(&root, path, false)?;
                        let name = format!(".catabolic-link-{}", op["id"].as_str().unwrap());
                        let (s, t) = state_at(&parent, &name)?;
                        if s != "absent" {
                            if s != "link" || t.as_deref() != op["target"].as_str() {
                                return Err(Error(
                                    "replacement recovery link changed externally".into(),
                                ));
                            }
                            unlink(&parent, &name)?;
                            parent.sync_all()?;
                        }
                    }
                    context.store().transaction(|db| {
                        db.execute(
                            "DELETE FROM journal WHERE id=?",
                            [op["id"].as_str().unwrap()],
                        )?;
                        Ok(())
                    })?;
                    cancelled.push(op["id"].clone());
                    continue;
                }
            }
        }
        execute_operation_context(context, profile, &op, args)?;
        recovered.push(op["id"].clone());
    }
    let mut result = json!({"recovered":recovered,"cancelled":cancelled,"next":"run sync --dry-run to inspect current desired state"});
    published_context(context, profile, catalog, &mut result, false)?;
    Ok(result)
}
/// Revalidate a bounded set of committed publication generations. No network
/// calls occur here; pending events are emitted in the same verification commit.
pub fn recover_publications(store: &Store, profile: &str, limit: usize) -> Result<()> {
    if !(1..=1000).contains(&limit) {
        return Err(Error(
            "publication recovery limit must be between 1 and 1000".into(),
        ));
    }
    let pending = rows(
        &store.db,
        "SELECT * FROM publication_generations WHERE profile=? AND generation>verified ORDER BY last_checked,catalog LIMIT ?",
        &[profile.to_string().into(), (limit as i64).into()],
    )?;
    for row in pending {
        let catalog = row["catalog"]
            .as_str()
            .ok_or_else(|| Error("invalid publication catalog".into()))?;
        store.transaction(|db| {
            db.execute(
                "UPDATE publication_generations SET last_checked=? WHERE profile=? AND catalog=?",
                params![now(), profile, catalog],
            )?;
            Ok(())
        })?;
        let current = crate::observation::binding(store, profile, "output", catalog);
        if current.is_err()
            || catabolic_core::decode_json(row["local_binding"].as_str().unwrap())?
                != current.unwrap()
        {
            continue;
        }
        if verify(store, profile, catalog).is_ok_and(|report| report["healthy"] == true) {
            published(store, profile, catalog, &mut json!({}), false)?;
        }
    }
    Ok(())
}
pub(crate) fn published(
    store: &Store,
    profile: &str,
    catalog: &str,
    result: &mut Value,
    refresh: bool,
) -> Result<()> {
    published_context(
        &mut Reconciliation::Fixed(store),
        profile,
        catalog,
        result,
        refresh,
    )
}
/// Revalidate current logical publication using the same production verifier.
/// Callers own a writable Store at an autocommit boundary; each isolated probe
/// may release that ownership, so caller-specific leases need a fresh fence.
pub(crate) fn published_mut(
    store: &mut Store,
    profile: &str,
    catalog: &str,
    result: &mut Value,
    refresh: bool,
    probe: &mut crate::fallback_resolution::Probe<'_>,
) -> Result<()> {
    published_context(
        &mut Reconciliation::Mutable(store, probe, Default::default()),
        profile,
        catalog,
        result,
        refresh,
    )
}
fn published_context(
    context: &mut Reconciliation<'_, '_>,
    profile: &str,
    catalog: &str,
    result: &mut Value,
    refresh: bool,
) -> Result<()> {
    let verification = verify_context(context, profile, catalog)?;
    if verification["healthy"] != true {
        return Ok(());
    }
    let binding = crate::observation::binding(context.store(), profile, "output", catalog)?;
    context.store().transaction(|db|{
        let publication_rows=rows(db,"SELECT * FROM publication_generations WHERE profile=? AND catalog=? AND generation>verified",&[profile.to_string().into(),catalog.to_string().into()])?;
        if let Some(row)=publication_rows.first()&& catabolic_core::decode_json(row["local_binding"].as_str().unwrap())?==binding {
            db.execute("UPDATE publication_generations SET verified=generation,verified_at=CURRENT_TIMESTAMP WHERE profile=? AND catalog=?",[profile,catalog])?;
            crate::notifications::emit(db,profile,"projection_updated","info",catalog,&json!(format!("publication:{catalog}:{}",row["generation"])))?;
        }
        db.execute("UPDATE consumer_bindings SET verified=generation,verified_at=CURRENT_TIMESTAMP,error=NULL WHERE profile=? AND catalog=? AND enabled=1 AND generation>verified AND local_binding=?",params![profile,catalog,encode(&binding)])?;
        if refresh {
            let dirty=rows(db,"SELECT * FROM refresh_dirty WHERE profile=? AND catalog=?",&[profile.to_string().into(),catalog.to_string().into()])?;
            let mut ids=Vec::new();for row in dirty {db.execute("INSERT INTO refresh_events(profile,catalog,target) VALUES(?,?,?)",params![profile,catalog,row["target"].as_str()])?;ids.push(db.last_insert_rowid());db.execute("DELETE FROM refresh_dirty WHERE profile=? AND catalog=?",[profile,catalog])?;}
            if !ids.is_empty(){result["refresh_events"]=json!(ids);}
        }
        Ok(())
    })?;
    if context
        .store()
        .db
        .prepare("SELECT 1 FROM consumer_bindings WHERE profile=? AND enabled=1 LIMIT 1")?
        .exists([profile])?
    {
        let unresolved:i64=context.store().db.query_row("SELECT count(*) FROM consumer_bindings WHERE profile=? AND enabled=1 AND generation>acknowledged",[profile],|r|r.get(0))?;
        result["consumers"] = json!({"scope":"profile","complete":unresolved==0,"unresolved_bindings":unresolved,"processed":0,"state":if unresolved==0{"idle"}else{"pending_worker"}});
    }
    // Match the frozen historical acknowledgment rule: a healthy no-op is not
    // proof that this invocation completed any interrupted fallback intent.
    // This follows the last live verification without another detach/wait.
    if ["applied", "recovered"]
        .iter()
        .any(|key| result[key].as_array().is_some_and(|v| !v.is_empty()))
    {
        context.store().transaction(|db| {
            if !db.prepare("SELECT 1 FROM journal WHERE profile=? AND catalog=?")?.exists([profile,catalog])? {
                db.execute("UPDATE fallback_entries SET published_generation=generation WHERE profile=? AND catalog=? AND state NOT IN ('blocked','unresolved')",[profile,catalog])?;
            }
            Ok(())
        })?;
    }
    Ok(())
}
pub(crate) fn verify(store: &Store, profile: &str, catalog: &str) -> Result<Value> {
    verify_context(&mut Reconciliation::Fixed(store), profile, catalog)
}
pub(crate) fn verify_mut(
    store: &mut Store,
    profile: &str,
    catalog: &str,
    probe: &mut crate::fallback_resolution::Probe<'_>,
) -> Result<Value> {
    verify_context(
        &mut Reconciliation::Mutable(store, probe, Default::default()),
        profile,
        catalog,
    )
}
fn verify_context(
    context: &mut Reconciliation<'_, '_>,
    profile: &str,
    catalog: &str,
) -> Result<Value> {
    if hardlink_mode(context.store(), catalog)? {
        context.supported(profile, catalog)?;
        let report = crate::hardlink_mutation::verify(context.store(), profile, catalog)?;
        return Ok(json!({"healthy":report["healthy"],"catalogs":[report]}));
    }
    let mut issues = Vec::new();
    let mut verified = 0;
    let mut evidence = serde_json::Map::new();
    let check = (|| -> Result<()> {
        context.supported(profile, catalog)?;
        let output = crate::observation::binding(context.store(), profile, "output", catalog)?;
        let (root, _) = crate::observation::checked_root(context.store(), profile, &output)?;
        if owner_state(&root, &owner(context.store(), profile, catalog))? != "owned" {
            return Err(Error("output ownership is missing".into()));
        }
        if context
            .store()
            .db
            .prepare("SELECT 1 FROM journal WHERE profile=? AND catalog=?")?
            .exists([profile, catalog])?
        {
            issues.push(json!({"reason":"pending operations require recovery"}));
        }
        let owned = owned(context.store(), profile, catalog)?;
        let mappings = context.mappings(profile, catalog)?;
        for mapping in &mappings {
            let path = mapping["path"].as_str().unwrap();
            let check = (|| -> Result<()> {
                let binding = crate::observation::binding(
                    context.store(),
                    profile,
                    "source",
                    mapping["location"].as_str().unwrap(),
                )?;
                let (_, validation) =
                    crate::observation::checked_root(context.store(), profile, &binding)?;
                evidence.insert(mapping["location"].as_str().unwrap().into(), validation);
                let (target, reason) = context.source_target(profile, mapping, &output)?;
                if let Some(reason) = reason {
                    return Err(Error(reason));
                }
                if target.is_none()
                    || owned.get(path) != target.as_ref()
                    || state(&root, path)? != ("link".into(), target)
                {
                    return Err(Error(
                        "link or ownership does not match the selected source".into(),
                    ));
                }
                Ok(())
            })();
            if let Err(e) = check {
                issues.push(json!({"path":path,"reason":e.0}));
            } else {
                verified += 1;
            }
        }
        for path in owned.keys() {
            if !mappings.iter().any(|m| m["path"] == path.as_str()) {
                issues.push(
                    json!({"path":path,"reason":"obsolete owned link needs synchronization"}),
                );
            }
        }
        Ok(())
    })();
    if let Err(e) = check {
        issues.push(json!({"reason":e.0}));
    }
    let healthy = issues.is_empty();
    let identity_verified = healthy && evidence.values().all(|v| v["identity_verified"] == true);
    Ok(
        json!({"healthy":healthy,"catalogs":[{"catalog":catalog,"healthy":healthy,"source_validation":evidence,"identity_verified":identity_verified,"verified_links":verified,"issues":issues}]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn catalog() -> (
        tempfile::TempDir,
        Store,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let source = root.path().join("source");
        let output = root.path().join("output");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&output).unwrap();
        std::fs::write(source.join("one.txt"), "original source").unwrap();
        crate::observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"media","root":source}),
        )
        .unwrap();
        crate::observation::execute(
            &store,
            "default",
            "bind",
            &json!({"kind":"output","owner":"global","root":output}),
        )
        .unwrap();
        crate::observation::execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        store.transaction(|db|{db.execute("INSERT INTO items(id,kind,metadata) VALUES('item','movie','{}')",[])?;db.execute("INSERT INTO mappings(id,catalog,file_id,item_id,path) SELECT 'mapping','global',id,'item','folder/one.txt' FROM files WHERE path='one.txt'",[])?;Ok(())}).unwrap();
        (root, store, source, output)
    }
    #[test]
    fn held_publication_guard_survives_detach_and_refuses_other_catalog() {
        let (_root, mut store, source, output) = catalog();
        let guard = publication_lock(&store).unwrap();
        assert!(publication_lock(&store).is_err());
        store.detached(|| Ok(())).unwrap();
        let options = json!({"catalog":"global"});
        let report = execute_locked(&store, "default", "apply", &options, &guard).unwrap();
        assert_eq!(report["healthy"], true);
        assert_eq!(
            std::fs::read(source.join("one.txt")).unwrap(),
            b"original source"
        );
        assert_eq!(
            std::fs::read(output.join("folder/one.txt")).unwrap(),
            b"original source"
        );
        let (_other_root, other, _, _) = catalog();
        assert!(
            execute_locked(&other, "default", "apply", &options, &guard)
                .unwrap_err()
                .0
                .contains("different catalog")
        );
        assert!(
            execute_locked(&store, "default", "apply", &json!({"catalog":null}), &guard).is_err()
        );
        drop(guard);
        assert!(publication_lock(&store).is_ok());
    }
    #[test]
    fn characterize_unresolved_external_writer_leaf_race() {
        // Frozen Python-compatible replacement/removal cannot atomically compare
        // an existing symlink and mutate that same inode. These assertions
        // characterize the open safety gate; they do NOT qualify these races.
        for replacement in [true, false] {
            let (_root, store, source, output) = catalog();
            execute(&store, "default", "apply", &json!({})).unwrap();
            if replacement {
                std::fs::write(source.join("two.txt"), "next source").unwrap();
                crate::observation::execute(&store, "default", "scan", &json!({"source":"media"}))
                    .unwrap();
                store.transaction(|db| {db.execute("UPDATE mappings SET file_id=(SELECT id FROM files WHERE path='two.txt')",[])?;Ok(())}).unwrap();
            } else {
                store
                    .transaction(|db| {
                        db.execute("UPDATE mappings SET active=0", [])?;
                        Ok(())
                    })
                    .unwrap();
            }
            let leaf = output.join("folder/one.txt");
            let swapped = leaf.clone();
            BEFORE_DESTRUCTIVE_LEAF.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    std::fs::remove_file(&swapped).unwrap();
                    std::fs::write(&swapped, "external concurrent bytes").unwrap();
                }))
            });
            execute(&store, "default", "apply", &json!({"max_removals":1})).unwrap();
            if replacement {
                assert_eq!(std::fs::read_to_string(&leaf).unwrap(), "next source");
            } else {
                assert!(!leaf.exists());
            }
            assert_eq!(
                std::fs::read_to_string(source.join("one.txt")).unwrap(),
                "original source"
            );
            eprintln!(
                "OPEN SAFETY GATE: concurrent external leaf writer can lose replacement bytes during {}",
                if replacement {
                    "symlink replacement"
                } else {
                    "symlink removal"
                }
            );
        }
    }
    #[test]
    fn symlink_publication_and_unconfirmed_missing_preserve_output() {
        let (_root, store, source, output) = catalog();
        let report = execute(&store, "default", "apply", &json!({})).unwrap();
        assert_eq!(report["healthy"], true);
        assert_eq!(
            std::fs::read_to_string(output.join("folder/one.txt")).unwrap(),
            "original source"
        );
        std::fs::remove_file(source.join("one.txt")).unwrap();
        let plan = execute(&store, "default", "apply", &json!({})).unwrap();
        assert_eq!(plan["safe"], false);
        assert!(std::fs::symlink_metadata(output.join("folder/one.txt")).is_ok());
        crate::observation::execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        assert_eq!(
            execute(&store, "default", "preview", &json!({"max_removals":0})).unwrap()["safe"],
            false
        );
        execute(&store, "default", "apply", &json!({"max_removals":1})).unwrap();
        assert!(std::fs::symlink_metadata(output.join("folder/one.txt")).is_err());
    }
    #[test]
    fn crash_boundaries_are_recoverable_and_do_not_change_source_bytes() {
        for stage in [
            "after_journal",
            "after_filesystem",
            "before_commit",
            "after_commit",
        ] {
            let (_root, store, source, output) = catalog();
            assert!(execute(&store, "default", "apply", &json!({"failpoint":stage})).is_err());
            execute(&store, "default", "recover", &json!({})).unwrap();
            execute(&store, "default", "apply", &json!({})).unwrap();
            assert_eq!(
                execute(&store, "default", "verify", &json!({})).unwrap()["healthy"],
                true
            );
            assert_eq!(
                std::fs::read(source.join("one.txt")).unwrap(),
                b"original source"
            );
            assert!(
                std::fs::symlink_metadata(output.join("folder/one.txt"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                store
                    .db
                    .query_row("SELECT count(*) FROM journal", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
    #[test]
    fn ownership_collisions_and_symlink_parents_are_refused() {
        let (root, store, _source, output) = catalog();
        std::fs::write(output.join("foreign"), "do not touch").unwrap();
        assert_eq!(
            execute(&store, "default", "apply", &json!({})).unwrap()["safe"],
            false
        );
        assert_eq!(
            std::fs::read_to_string(output.join("foreign")).unwrap(),
            "do not touch"
        );
        std::fs::remove_file(output.join("foreign")).unwrap();
        execute(&store, "default", "apply", &json!({})).unwrap();
        std::fs::remove_file(output.join("folder/one.txt")).unwrap();
        std::fs::remove_dir(output.join("folder")).unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, output.join("folder")).unwrap();
        assert_eq!(
            execute(&store, "default", "apply", &json!({})).unwrap()["safe"],
            false
        );
        assert!(!outside.join("one.txt").exists());
    }
    #[test]
    fn journal_cancels_obsolete_unapplied_intent() {
        let (_root, store, _source, output) = catalog();
        execute(&store, "default", "apply", &json!({})).unwrap();
        std::fs::remove_file(output.join("folder/one.txt")).unwrap();
        assert!(
            execute(
                &store,
                "default",
                "apply",
                &json!({"failpoint":"after_journal"})
            )
            .is_err()
        );
        store
            .transaction(|db| {
                db.execute("UPDATE mappings SET active=0", [])?;
                Ok(())
            })
            .unwrap();
        let report = execute(&store, "default", "recover", &json!({})).unwrap();
        assert_eq!(report["cancelled"].as_array().unwrap().len(), 1);
        assert!(std::fs::symlink_metadata(output.join("folder/one.txt")).is_err());
    }
    #[test]
    fn completed_effect_is_acknowledged_once_after_crash() {
        let (_root, store, _source, output) = catalog();
        execute(&store, "default", "apply", &json!({})).unwrap();
        std::fs::remove_file(output.join("folder/one.txt")).unwrap();
        assert!(
            execute(
                &store,
                "default",
                "apply",
                &json!({"failpoint":"after_filesystem"})
            )
            .is_err()
        );
        assert!(std::fs::symlink_metadata(output.join("folder/one.txt")).is_ok());
        let before:i64=store.db.query_row("SELECT generation FROM publication_generations WHERE profile='default' AND catalog='global'",[],|r|r.get(0)).unwrap();
        execute(&store, "default", "recover", &json!({})).unwrap();
        let once:i64=store.db.query_row("SELECT generation FROM publication_generations WHERE profile='default' AND catalog='global'",[],|r|r.get(0)).unwrap();
        execute(&store, "default", "recover", &json!({})).unwrap();
        let twice:i64=store.db.query_row("SELECT generation FROM publication_generations WHERE profile='default' AND catalog='global'",[],|r|r.get(0)).unwrap();
        assert_eq!(once, before + 1);
        assert_eq!(twice, once);
    }
}
