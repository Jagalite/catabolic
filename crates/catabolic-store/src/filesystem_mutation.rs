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
    /// A fixed Store whose caller observes each operation after its
    /// filesystem effect and before its acknowledgement, like the frozen
    /// `after_filesystem` callback.
    Observed(&'s Store, &'s dyn Fn(&Value) -> Result<()>),
}
impl Reconciliation<'_, '_> {
    fn store(&self) -> &Store {
        match self {
            Self::Fixed(s) | Self::Observed(s, _) => s,
            Self::Mutable(s, _, _) => s,
        }
    }
    fn observe(&self, operation: &Value) -> Result<()> {
        match self {
            Self::Observed(_, observer) => observer(operation),
            _ => Ok(()),
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
    admit_failpoints(args)?;
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
    admit_failpoints(args)?;
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
/// Durably sync the root and every existing directory on a root-relative
/// path's chain. Recovery that finds an intended postcondition must still
/// complete directory syncs an interrupted predecessor may have skipped.
pub(crate) fn sync_ancestry(root: &File, path: &str) -> Result<()> {
    crate::layout::relative_path(path)?;
    root.sync_all()?;
    let parts: Vec<_> = path.split('/').collect();
    let mut fd = root.try_clone()?;
    for component in &parts[..parts.len() - 1] {
        fd = match open_at(&fd, component, true) {
            Ok(fd) => fd,
            Err(e) if e.0.contains("No such file or directory") => return Ok(()),
            Err(e) => return Err(e),
        };
        fd.sync_all()?;
    }
    Ok(())
}
/// Confirm that `parent`, pinned while operating on `path`, is still the
/// directory reached by walking `path` from `root`. Callers pass a freshly
/// opened and validated output root, so a pinned directory, the root itself or
/// one of its ancestors moved elsewhere is detected rather than acknowledged.
pub(crate) fn verify_containment(root: &File, path: &str, parent: &File) -> Result<()> {
    let moved = || {
        Error(format!(
            "output directory moved during operation; inspect {path}"
        ))
    };
    let (walked, _) = self::parent(root, path, false).map_err(|_| moved())?;
    if identity(&walked.metadata()?) != identity(&parent.metadata()?) {
        return Err(moved());
    }
    Ok(())
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
/// Claim an empty output by creating its ownership marker exclusively.
///
/// Native claims never create a temporary name, so there is nothing to clean
/// up by pathname: a pathname check cannot pin an inode before unlink, and a
/// substituted foreign file would be deleted. A legacy `.catabolic-claim-ID`
/// temporary left by an interrupted Python or earlier native claim is never
/// deleted; it is preserved for inspection. A crash while writing leaves an
/// incomplete marker, which ownership checks refuse rather than repair.
pub(crate) fn claim_output(root: &File, expected: &Value, id: &str) -> Result<()> {
    claim_output_at(root, expected, id, &Value::Null, false)
}
/// Crash points inside a claim. An output claim and a hardlink retention
/// claim are distinct durable boundaries, so each has its own stage names.
fn claim_failpoint(args: &Value, retention: bool, written: bool) -> Result<()> {
    match (retention, written) {
        (false, false) => failpoint(args, "after_marker_create"),
        (false, true) => failpoint(args, "after_marker_write"),
        (true, false) => failpoint(args, "after_retention_marker_create"),
        (true, true) => failpoint(args, "after_retention_marker_write"),
    }
}
/// [`claim_output`] with failpoints for the publication crash matrix.
pub(crate) fn claim_output_at(
    root: &File,
    expected: &Value,
    id: &str,
    args: &Value,
    retention: bool,
) -> Result<()> {
    let temporary = format!(".catabolic-claim-{id}");
    if state_at(root, MARKER)?.0 != "absent" {
        owner_state(root, expected)?;
        // An interrupted predecessor may have written or published the marker
        // without syncing; make both durable before acknowledging the claim.
        open_at(root, MARKER, false)?.sync_all()?;
        root.sync_all()?;
        return Ok(());
    }
    if state_at(root, &temporary)?.0 != "absent" {
        return Err(Error(format!(
            "interrupted ownership claim left {temporary}; inspect and remove it before retrying"
        )));
    }
    let mut empty = true;
    entries(root, |_| {
        empty = false;
        Ok(false)
    })?;
    if !empty {
        return Err(Error("only an empty output can be claimed".into()));
    }
    let name = cstring(MARKER)?;
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
    claim_failpoint(args, retention, false)?;
    writeln!(file, "{}", encode(expected))?;
    claim_failpoint(args, retention, true)?;
    file.sync_all()?;
    root.sync_all()?;
    Ok(())
}

/// Preview or execute native symlink reconciliation. Unsupported modes are
/// refused before journaling or modifying any output.
pub fn execute(store: &Store, profile: &str, action: &str, args: &Value) -> Result<Value> {
    admit_failpoints(args)?;
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
    let statistics = begin_execution(context.store(), profile, &catalogs, &actions)?;
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
    finish_execution(context.store(), profile, statistics, &applied, &verified)?;
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
    admit_failpoints(args)?;
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
/// Apply or recover with a caller callback after each operation's
/// filesystem effect and before its acknowledgement, matching the frozen
/// `Reconciler.apply`/`recover` `after_filesystem` order. A callback error
/// propagates and leaves the operation journaled for recovery. Gates,
/// locking and journal semantics are those of [`execute`].
pub fn execute_observed(
    store: &Store,
    profile: &str,
    action: &str,
    args: &Value,
    observer: &dyn Fn(&Value) -> Result<()>,
) -> Result<Value> {
    admit_failpoints(args)?;
    notify_consumers(args)?;
    if !matches!(action, "apply" | "recover") {
        return Err(Error(format!(
            "unknown observed filesystem action: {action}"
        )));
    }
    let mut context = Reconciliation::Observed(store, observer);
    if args.get("catalog").is_some_and(Value::is_null) {
        return all_catalogs_context(&mut context, profile, action, args);
    }
    let catalog = args
        .get("catalog")
        .map(|v| {
            v.as_str()
                .ok_or_else(|| Error("catalog must be text or null".into()))
        })
        .transpose()?
        .unwrap_or("global");
    require_writer(store)?;
    let _lock = publication_lock(store)?;
    if action == "apply" {
        apply_locked_context(&mut context, profile, catalog, args)
    } else {
        recover_locked_context(&mut context, profile, catalog, args)
    }
}
/// Durable publication snapshots, as frozen `projection_stats` records them
/// around every apply: begun before the first journal row and finished
/// after verification.
struct Execution {
    started: std::time::Instant,
    snapshots: BTreeMap<String, Value>,
}
fn statistics_key(profile: &str, catalog: &str) -> String {
    format!(
        "statistics:{}",
        encode(&json!([profile, "execution", catalog]))
    )
}
fn statistics_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}
fn kinds(actions: &[Value], catalog: &str) -> Value {
    let mut counts = serde_json::Map::new();
    for action in actions.iter().filter(|a| a["catalog"] == catalog) {
        let kind = action["kind"].as_str().unwrap_or_default().to_owned();
        let next = counts.get(&kind).and_then(Value::as_u64).unwrap_or(0) + 1;
        counts.insert(kind, json!(next));
    }
    Value::Object(counts)
}
fn save_execution(
    db: &rusqlite::Connection,
    profile: &str,
    catalog: &str,
    snapshot: &Value,
) -> Result<()> {
    db.execute(
        "INSERT INTO meta(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![statistics_key(profile, catalog), encode(snapshot)],
    )?;
    Ok(())
}
fn begin_execution(
    store: &Store,
    profile: &str,
    catalogs: &[String],
    actions: &[Value],
) -> Result<Execution> {
    let mut snapshots = BTreeMap::new();
    for catalog in catalogs {
        let inputs =
            crate::manifest::statistics(store, profile, catalog)?["current_recorded"].take();
        snapshots.insert(
            catalog.clone(),
            json!({"started_at":statistics_now(),"finished_at":null,"elapsed_seconds":null,"status":"completion_unknown","planned_actions":kinds(actions,catalog),"applied_actions":null,"verification":null,"inputs":inputs}),
        );
    }
    store.transaction(|db| {
        for (catalog, snapshot) in &snapshots {
            save_execution(db, profile, catalog, snapshot)?;
        }
        Ok(())
    })?;
    Ok(Execution {
        started: std::time::Instant::now(),
        snapshots,
    })
}
fn finish_execution(
    store: &Store,
    profile: &str,
    mut execution: Execution,
    applied: &[Value],
    verification: &Value,
) -> Result<()> {
    let elapsed = execution.started.elapsed().as_secs_f64();
    store.transaction(|db| {
        for report in verification["catalogs"].as_array().into_iter().flatten() {
            let catalog = report["catalog"]
                .as_str()
                .ok_or_else(|| Error("verification report has no catalog".into()))?;
            let snapshot = execution
                .snapshots
                .get_mut(catalog)
                .ok_or_else(|| Error(format!("no execution snapshot for {catalog}")))?;
            snapshot["finished_at"] = json!(statistics_now());
            snapshot["elapsed_seconds"] = json!(elapsed);
            snapshot["status"] = json!(if report["healthy"] == true {
                "complete"
            } else {
                "verification_failed"
            });
            snapshot["applied_actions"] = kinds(applied, catalog);
            snapshot["verification"] = json!({"verified_at":statistics_now(),"healthy":report["healthy"],"verified_links":report.get("verified_links").cloned().unwrap_or(Value::Null),"issue_count":report["issues"].as_array().map_or(0,Vec::len)});
            save_execution(db, profile, catalog, snapshot)?;
        }
        Ok(())
    })
}
/// Every crash point of symlink and hardlink publication, retirement and
/// recovery. `tests/publication_failpoints.rs` crashes a real process at each
/// one and checks recovery. It also fails if a literal stage passed to `failpoint`
/// in either mutation source is missing from this list.
pub const PUBLICATION_FAILPOINTS: &[&str] = &[
    "after_journal",
    "after_marker_create",
    "after_marker_write",
    "after_temporary",
    "after_exchange",
    "after_quarantine",
    "after_publish",
    "after_private_move",
    "after_retention_marker_create",
    "after_retention_marker_write",
    "before_retention_rename",
    "after_retention",
    "before_link",
    "after_staged_link",
    "after_link",
    "after_filesystem",
    "before_commit",
    "after_commit",
];
/// Whether this build accepts the request fields `failpoint` and `crash`.
/// Only the workspace test build enables the `failpoints` feature (through
/// `catabolic-cli`'s dev-dependency). Shipped binaries and wheels are built
/// without dev-dependencies, so the feature is off there.
pub const FAILPOINTS_ENABLED: bool = cfg!(feature = "failpoints");
/// Structured refusal of a failpoint request in a build without the feature.
/// Frozen Python has no failpoint fields, so no compatibility contract keeps
/// them; refusing is preferred to silently ignoring them.
pub const FAILPOINT_REFUSAL: &str = "failpoint_injection_unavailable: the request fields failpoint and crash are accepted only by test builds with the failpoints feature";
#[cfg(test)]
thread_local! {
    // Lets unit tests in a feature-enabled test build exercise the default
    // (feature-disabled) admission path end to end.
    pub(crate) static SIMULATE_DEFAULT_FEATURES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
fn failpoints_enabled() -> bool {
    #[cfg(test)]
    if SIMULATE_DEFAULT_FEATURES.with(std::cell::Cell::get) {
        return false;
    }
    FAILPOINTS_ENABLED
}
/// Refuse, before any effect, a request carrying `failpoint` or `crash` when
/// this build does not accept failpoints.
pub fn admit_failpoints(args: &Value) -> Result<()> {
    let carries = args
        .as_object()
        .is_some_and(|o| o.contains_key("failpoint") || o.contains_key("crash"));
    if carries && !failpoints_enabled() {
        return Err(Error(FAILPOINT_REFUSAL.into()));
    }
    Ok(())
}
pub(crate) fn failpoint(args: &Value, stage: &str) -> Result<()> {
    // Entry points admit first; this keeps the refusal if one was missed.
    admit_failpoints(args)?;
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
    let statistics = begin_execution(context.store(), profile, &[catalog.to_owned()], &actions)?;
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
    finish_execution(context.store(), profile, statistics, &applied, &verified)?;
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
    // Test-only deterministic schedulers for a concurrent foreign leaf writer:
    // just before the leaf is displaced, and just after it is displaced.
    static BEFORE_DESTRUCTIVE_LEAF: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static AFTER_DESTRUCTIVE_LEAF: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    // Between a caller's verification of a private entry and its move into
    // the exclusive directory (the window Sol round 1 found), and between
    // verification inside that directory and the unlink (the residual).
    static BEFORE_PRIVATE_MOVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static BEFORE_PRIVATE_UNLINK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}
fn before_private_move() {
    #[cfg(test)]
    BEFORE_PRIVATE_MOVE.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}
fn before_private_unlink() {
    #[cfg(test)]
    BEFORE_PRIVATE_UNLINK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}
fn before_destructive_leaf() {
    #[cfg(test)]
    BEFORE_DESTRUCTIVE_LEAF.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}
fn after_destructive_leaf() {
    #[cfg(test)]
    AFTER_DESTRUCTIVE_LEAF.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}

// Safe replace/remove protocol (track C2). A published leaf is never renamed
// over or unlinked in place. It is first displaced to a private name derived
// from the journal operation, by atomic exchange or exclusive rename. Only an
// entry verified there as the expected owned link is deleted. A foreign entry
// is moved back without overwriting anything, or left at the private name.

/// Private name holding the new link before publication. After an atomic
/// exchange it holds the displaced entry. Frozen Python uses the same name.
fn temporary_name(id: &str) -> String {
    format!(".catabolic-link-{id}")
}
/// Private name holding a leaf displaced by exclusive rename.
fn quarantine_name(id: &str) -> String {
    format!(".catabolic-unlink-{id}")
}
#[derive(Clone, Copy)]
enum Rename {
    Exclusive,
    Exchange,
}
/// Rename within one directory. Neither mode can replace an existing entry:
/// an exclusive rename refuses an occupied destination, and an exchange swaps
/// the two entries atomically.
fn rename_in(parent: &File, from: &str, to: &str, mode: Rename) -> std::io::Result<()> {
    rename_between(parent, from, parent, to, mode)
}
/// [`rename_in`] between two directories on one filesystem.
fn rename_between(
    source: &File,
    from: &str,
    destination: &File,
    to: &str,
    mode: Rename,
) -> std::io::Result<()> {
    let invalid = |_| std::io::Error::from(std::io::ErrorKind::InvalidInput);
    let from = CString::new(from).map_err(invalid)?;
    let to = CString::new(to).map_err(invalid)?;
    #[cfg(target_os = "linux")]
    let flags = match mode {
        Rename::Exclusive => libc::RENAME_NOREPLACE,
        Rename::Exchange => libc::RENAME_EXCHANGE,
    };
    #[cfg(target_os = "linux")]
    // SAFETY: live descriptor and NUL-terminated names; the flags never replace an entry.
    let result = unsafe {
        libc::renameat2(
            source.as_raw_fd(),
            from.as_ptr(),
            destination.as_raw_fd(),
            to.as_ptr(),
            flags,
        )
    };
    #[cfg(target_os = "macos")]
    let flags = match mode {
        Rename::Exclusive => libc::RENAME_EXCL,
        Rename::Exchange => libc::RENAME_SWAP,
    };
    #[cfg(target_os = "macos")]
    // SAFETY: live descriptor and NUL-terminated names; the flags never replace an entry.
    let result = unsafe {
        libc::renameatx_np(
            source.as_raw_fd(),
            from.as_ptr(),
            destination.as_raw_fd(),
            to.as_ptr(),
            flags,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
/// Whether the filesystem lacks atomic exchange (for example HFS+).
fn exchange_unsupported(error: &std::io::Error) -> bool {
    // ENOTSUP and EOPNOTSUPP are equal on Linux and distinct on macOS.
    error.raw_os_error().is_some_and(|code| {
        [libc::ENOTSUP, libc::EOPNOTSUPP, libc::EINVAL, libc::ENOSYS].contains(&code)
    })
}
/// The only name a private directory may hold.
const PRIVATE_ENTRY: &str = "entry";
/// Exclusive directory used to delete the private entry `name`. Its name is
/// derived from the journal operation, like `name`, so recovery finds an
/// interrupted predecessor's directory without scanning the parent.
fn private_directory(name: &str) -> String {
    format!("{name}.private")
}
fn retained(name: &str, directory: &str) -> Error {
    Error(format!(
        "private publication entry {name} changed externally; retained in {directory}; inspect it"
    ))
}
/// Check that `held`, opened from `parent` by `directory`, is the directory
/// at that name: same filesystem, owned by this user and mode 0700.
fn verify_private_directory(parent: &File, directory: &str, held: &File) -> Result<()> {
    let metadata = held.metadata()?;
    let (_, named) = stat_at(parent, directory)?;
    // SAFETY: geteuid has no preconditions.
    let user = unsafe { libc::geteuid() };
    if !metadata.is_dir()
        || metadata.dev() != parent.metadata()?.dev()
        || named["device"] != json!(metadata.dev())
        || named["inode"] != json!(metadata.ino())
        || metadata.uid() != user
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(Error(format!(
            "private publication directory {directory} failed verification; nothing was removed"
        )));
    }
    Ok(())
}
/// Delete the verified entry inside a held private directory, then the
/// directory itself. Unexpected content is retained and reported.
fn finish_private(
    parent: &File,
    name: &str,
    directory: &str,
    held: &File,
    allowed: &[Option<&str>],
) -> Result<()> {
    let mut names = Vec::new();
    entries(held, |entry| {
        names.push(entry.to_owned());
        Ok(names.len() <= 1)
    })?;
    match names.as_slice() {
        [] => {}
        [only] if only == PRIVATE_ENTRY => {
            match state_at(held, PRIVATE_ENTRY)? {
                (s, t) if s == "link" && allowed.contains(&t.as_deref()) => {}
                _ => return Err(retained(name, directory)),
            }
            before_private_unlink();
            unlink(held, PRIVATE_ENTRY)?;
            held.sync_all()?;
        }
        _ => return Err(retained(name, directory)),
    }
    let path = cstring(directory)?;
    // SAFETY: live descriptor and NUL-terminated name. AT_REMOVEDIR removes
    // only an empty directory, so nothing added meanwhile can be lost.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), path.as_ptr(), libc::AT_REMOVEDIR) } < 0 {
        return Err(Error(format!(
            "private publication directory {directory} could not be removed ({}); inspect it",
            std::io::Error::last_os_error()
        )));
    }
    parent.sync_all()?;
    Ok(())
}
/// Delete the private entry `name` only after moving it into a freshly and
/// exclusively created 0700 directory and verifying it there as a symlink to
/// one of `allowed`. A writer who swaps `name` after the caller's check moves
/// its own entry into that directory, where verification retains it.
/// Anything unexpected, including a leftover directory with other content,
/// is retained and reported; nothing unverified is deleted.
fn discard_private(
    parent: &File,
    name: &str,
    allowed: &[Option<&str>],
    args: &Value,
) -> Result<()> {
    let directory = private_directory(name);
    // An interrupted predecessor may have left its directory behind.
    match open_at(parent, &directory, true) {
        Ok(held) => {
            verify_private_directory(parent, &directory, &held)?;
            finish_private(parent, name, &directory, &held, allowed)?;
        }
        Err(e) if e.0.contains("No such file or directory") => {}
        Err(_) => return Err(retained(name, &directory)),
    }
    if state_at(parent, name)?.0 == "absent" {
        return Ok(());
    }
    let path = cstring(&directory)?;
    // SAFETY: live descriptor and NUL-terminated name; mkdirat fails rather
    // than reuse an existing entry.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), path.as_ptr(), 0o700) } < 0 {
        return Err(Error(format!(
            "cannot create private publication directory {directory} ({}); nothing was removed",
            std::io::Error::last_os_error()
        )));
    }
    let held = open_at(parent, &directory, true)?;
    verify_private_directory(parent, &directory, &held)?;
    before_private_move();
    if let Err(e) = rename_between(parent, name, &held, PRIVATE_ENTRY, Rename::Exclusive) {
        let missing = e.kind() == std::io::ErrorKind::NotFound;
        // The directory is still empty and ours; remove it either way.
        finish_private(parent, name, &directory, &held, allowed)?;
        return if missing { Ok(()) } else { Err(e.into()) };
    }
    held.sync_all()?;
    parent.sync_all()?;
    failpoint(args, "after_private_move")?;
    finish_private(parent, name, &directory, &held, allowed)
}
/// Return a displaced foreign entry to its leaf, never overwriting.
fn restore_displaced(parent: &File, private: &str, leaf: &str, path: &str, what: &str) -> Error {
    match rename_in(parent, private, leaf, Rename::Exclusive) {
        Ok(()) => match parent.sync_all() {
            Ok(()) => Error(format!(
                "owned link changed before {what}: {path}; the foreign entry was restored and nothing was overwritten or removed"
            )),
            Err(e) => e.into(),
        },
        Err(e) => Error(format!(
            "owned link changed during {what}: {path}; the foreign entry is preserved at {private} ({e})"
        )),
    }
}
/// Publish the staged new link after the previous link was quarantined, then
/// delete the verified previous link.
fn publish_quarantined(
    parent: &File,
    leaf: &str,
    id: &str,
    previous: Option<&str>,
    path: &str,
    args: &Value,
) -> Result<()> {
    let (temporary, quarantine) = (temporary_name(id), quarantine_name(id));
    if let Err(e) = rename_in(parent, &temporary, leaf, Rename::Exclusive) {
        return Err(Error(format!(
            "replacement destination appeared during replacement: {path}; the previous link is preserved at {quarantine} and the new link at {temporary} ({e})"
        )));
    }
    parent.sync_all()?;
    failpoint(args, "after_publish")?;
    discard_private(parent, &quarantine, &[previous], args)
}
/// Replace the verified owned link at `leaf` with the staged temporary.
fn replace_leaf(
    parent: &File,
    leaf: &str,
    id: &str,
    target: &str,
    previous: Option<&str>,
    path: &str,
    args: &Value,
) -> Result<()> {
    let (temporary, quarantine) = (temporary_name(id), quarantine_name(id));
    before_destructive_leaf();
    match rename_in(parent, &temporary, leaf, Rename::Exchange) {
        Ok(()) => {
            parent.sync_all()?;
            failpoint(args, "after_exchange")?;
            after_destructive_leaf();
            // The displaced entry is now at the temporary name.
            if state_at(parent, &temporary)? == ("link".into(), previous.map(str::to_owned)) {
                return discard_private(parent, &temporary, &[previous], args);
            }
            // A foreign entry was displaced: swap it back atomically.
            if let Err(e) = rename_in(parent, &temporary, leaf, Rename::Exchange) {
                return Err(Error(format!(
                    "owned link changed during replacement: {path}; the foreign entry is preserved at {temporary} ({e})"
                )));
            }
            parent.sync_all()?;
            if state_at(parent, &temporary)? == ("link".into(), Some(target.to_owned())) {
                Err(Error(format!(
                    "owned link changed before replacement: {path}; the foreign entry was restored and nothing was overwritten or removed"
                )))
            } else {
                Err(Error(format!(
                    "owned link changed during replacement: {path}; displaced entries are preserved at {leaf} and {temporary}"
                )))
            }
        }
        Err(e) if exchange_unsupported(&e) => {
            // No atomic exchange: quarantine the previous link, then publish
            // exclusively. The leaf is briefly absent between the two renames.
            if state_at(parent, &quarantine)?.0 != "absent" {
                return Err(Error(format!(
                    "private publication entry {quarantine} changed externally; inspect it"
                )));
            }
            rename_in(parent, leaf, &quarantine, Rename::Exclusive)?;
            parent.sync_all()?;
            failpoint(args, "after_quarantine")?;
            after_destructive_leaf();
            if state_at(parent, &quarantine)? != ("link".into(), previous.map(str::to_owned)) {
                return Err(restore_displaced(
                    parent,
                    &quarantine,
                    leaf,
                    path,
                    "replacement",
                ));
            }
            publish_quarantined(parent, leaf, id, previous, path, args)
        }
        Err(e) => Err(e.into()),
    }
}
/// Remove the verified owned link at `leaf` by quarantining it first.
fn remove_leaf(
    parent: &File,
    leaf: &str,
    id: &str,
    previous: Option<&str>,
    path: &str,
    args: &Value,
) -> Result<()> {
    let quarantine = quarantine_name(id);
    if state_at(parent, &quarantine)?.0 != "absent" {
        return Err(Error(format!(
            "private publication entry {quarantine} changed externally; inspect it"
        )));
    }
    before_destructive_leaf();
    rename_in(parent, leaf, &quarantine, Rename::Exclusive)?;
    parent.sync_all()?;
    failpoint(args, "after_quarantine")?;
    after_destructive_leaf();
    if state_at(parent, &quarantine)? != ("link".into(), previous.map(str::to_owned)) {
        return Err(restore_displaced(
            parent,
            &quarantine,
            leaf,
            path,
            "removal",
        ));
    }
    discard_private(parent, &quarantine, &[previous], args)
}
/// The leaf's parent, if it exists.
fn existing_parent(root: &File, path: &str) -> Result<Option<(File, String)>> {
    match parent(root, path, false) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.0.contains("No such file or directory") => Ok(None),
        Err(e) => Err(e),
    }
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
        return match context {
            Reconciliation::Observed(store, observer) => {
                crate::hardlink_mutation::execute_observed(store, profile, op, args, *observer)
            }
            _ => crate::hardlink_mutation::execute(context.store(), profile, op, args),
        };
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
        claim_output_at(
            &root,
            &owner(context.store(), profile, catalog),
            id,
            args,
            false,
        )?;
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
                let quarantined = if kind == "replace" && current == "absent" {
                    match existing_parent(&root, path)? {
                        Some((parent, leaf)) => {
                            let held = state_at(&parent, &quarantine_name(id))?;
                            (held == ("link".into(), previous.map(str::to_owned)))
                                .then_some((parent, leaf))
                        }
                        None => None,
                    }
                } else {
                    None
                };
                if (current.as_str(), actual.as_deref()) == ("link", Some(target)) {
                    if kind == "replace"
                        && let Some((parent, _)) = existing_parent(&root, path)?
                    {
                        // An interrupted replacement may have left the
                        // displaced previous link at a private name.
                        discard_private(
                            &parent,
                            &temporary_name(id),
                            &[previous, Some(target)],
                            args,
                        )?;
                        discard_private(&parent, &quarantine_name(id), &[previous], args)?;
                    }
                } else if let Some((parent, leaf)) = quarantined {
                    // Interrupted between quarantining the previous link and
                    // publishing the new one. Finish only a still-desired
                    // replacement; otherwise put the previous link back.
                    if let Err(error) =
                        validate_intent_context(context, profile, catalog, path, Some(target))
                    {
                        rename_in(&parent, &quarantine_name(id), &leaf, Rename::Exclusive)?;
                        parent.sync_all()?;
                        return Err(Error(format!("{}; previous link restored", error.0)));
                    }
                    match state_at(&parent, &temporary_name(id))? {
                        (s, _) if s == "absent" => {
                            symlink(&parent, &temporary_name(id), target)?;
                            parent.sync_all()?;
                        }
                        (s, t) if s == "link" && t.as_deref() == Some(target) => {}
                        _ => {
                            return Err(Error(
                                "replacement recovery link changed externally".into(),
                            ));
                        }
                    }
                    publish_quarantined(&parent, &leaf, id, previous, path, args)?;
                    verify_containment(
                        &crate::observation::checked_root(context.store(), profile, &output)?.0,
                        path,
                        &parent,
                    )?;
                } else {
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
                        let temporary = temporary_name(id);
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
                        replace_leaf(&parent, &leaf, id, target, previous, path, args)?;
                    }
                    parent.sync_all()?;
                    verify_containment(
                        &crate::observation::checked_root(context.store(), profile, &output)?.0,
                        path,
                        &parent,
                    )?;
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
                    remove_leaf(&parent, &leaf, id, previous, path, args)?;
                    verify_containment(
                        &crate::observation::checked_root(context.store(), profile, &output)?.0,
                        path,
                        &parent,
                    )?;
                } else if kind == "remove"
                    && let Some((parent, _)) = existing_parent(&root, path)?
                {
                    // Interrupted after quarantining the owned link.
                    discard_private(&parent, &quarantine_name(id), &[previous], args)?;
                }
            }
            _ => return Err(Error(format!("unknown journal operation: {kind}"))),
        }
        sync_ancestry(&root, path)?;
    }
    failpoint(args, "after_filesystem")?;
    context.observe(op)?;
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
                            // Removing the temporary link is a destructive leaf
                            // effect; refuse before it unless the gate allows it.
                            if !symlink_leaf_allowed(kind, args) {
                                return Err(Error(SYMLINK_RACE_GATE.into()));
                            }
                            if s != "link" || t.as_deref() != op["target"].as_str() {
                                return Err(Error(
                                    "replacement recovery link changed externally".into(),
                                ));
                            }
                        }
                        let leftover = state_at(&parent, &private_directory(&name))?.0;
                        if leftover != "absent" && !symlink_leaf_allowed(kind, args) {
                            return Err(Error(SYMLINK_RACE_GATE.into()));
                        }
                        discard_private(&parent, &name, &[op["target"].as_str()], args)?;
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
    fn leaves(dir: &std::path::Path) -> Vec<String> {
        let mut names = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        names
    }
    #[test]
    fn pinned_directories_moved_outside_the_root_are_detected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("root/a")).unwrap();
        let root = File::open(dir.path().join("root")).unwrap();
        let (pinned, leaf) = parent(&root, "a/link", false).unwrap();
        assert_eq!(leaf, "link");
        verify_containment(&root, "a/link", &pinned).unwrap();
        std::fs::rename(dir.path().join("root/a"), dir.path().join("outside")).unwrap();
        assert!(verify_containment(&root, "a/link", &pinned).is_err());
        // A different directory recreated at the same name is also refused.
        std::fs::create_dir(dir.path().join("root/a")).unwrap();
        let error = verify_containment(&root, "a/link", &pinned).unwrap_err();
        assert!(error.0.contains("output directory moved"), "{error:?}");
        // Relocating the root itself is caught by walking from a fresh root.
        std::fs::create_dir_all(dir.path().join("root2/b")).unwrap();
        let old_root = File::open(dir.path().join("root2")).unwrap();
        let (pinned, _) = parent(&old_root, "b/link", false).unwrap();
        std::fs::rename(dir.path().join("root2"), dir.path().join("relocated")).unwrap();
        std::fs::create_dir_all(dir.path().join("root2/b")).unwrap();
        verify_containment(&old_root, "b/link", &pinned).unwrap();
        let fresh = File::open(dir.path().join("root2")).unwrap();
        assert!(verify_containment(&fresh, "b/link", &pinned).is_err());
    }
    #[test]
    fn claims_never_delete_temporaries_and_create_only_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        let root = File::open(dir.path()).unwrap();
        let expected = json!({"profile":"default","catalog":"main"});
        claim_output(&root, &expected, "1").unwrap();
        assert_eq!(leaves(dir.path()), [MARKER]);
        assert_eq!(owner_state(&root, &expected).unwrap(), "owned");
        // Recovery of an already published claim keeps a legacy temporary.
        std::fs::write(
            dir.path().join(".catabolic-claim-1"),
            b"unrelated user data",
        )
        .unwrap();
        claim_output(&root, &expected, "1").unwrap();
        assert_eq!(leaves(dir.path()), [".catabolic-claim-1", MARKER]);
        assert_eq!(
            std::fs::read(dir.path().join(".catabolic-claim-1")).unwrap(),
            b"unrelated user data"
        );
    }
    #[test]
    fn interrupted_claims_are_preserved_for_inspection() {
        let dir = tempfile::tempdir().unwrap();
        let root = File::open(dir.path()).unwrap();
        let expected = json!({"profile":"default","catalog":"main"});
        // A legacy interrupted claim (or a substituted foreign file) is never
        // deleted, even when its bytes are an empty or partial marker.
        for bytes in [&b""[..], b"{\"pro", b"unrelated user data"] {
            std::fs::write(dir.path().join(".catabolic-claim-1"), bytes).unwrap();
            let error = claim_output(&root, &expected, "1").unwrap_err();
            assert!(error.0.contains("inspect and remove it"), "{error:?}");
            assert_eq!(leaves(dir.path()), [".catabolic-claim-1"]);
            assert_eq!(
                std::fs::read(dir.path().join(".catabolic-claim-1")).unwrap(),
                bytes
            );
        }
        // An incomplete native marker fails closed rather than being repaired.
        std::fs::remove_file(dir.path().join(".catabolic-claim-1")).unwrap();
        std::fs::write(dir.path().join(MARKER), b"{\"pro").unwrap();
        assert!(claim_output(&root, &expected, "1").is_err());
        assert_eq!(std::fs::read(dir.path().join(MARKER)).unwrap(), b"{\"pro");
    }
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
    /// A catalog after one healthy apply, whose next apply plans one symlink
    /// replacement (`replacement`) or removal of `folder/one.txt`.
    fn destructive_catalog(
        replacement: bool,
    ) -> (
        tempfile::TempDir,
        Store,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let (root, store, source, output) = catalog();
        execute(&store, "default", "apply", &json!({})).unwrap();
        if replacement {
            std::fs::write(source.join("two.txt"), "next source").unwrap();
            crate::observation::execute(&store, "default", "scan", &json!({"source":"media"}))
                .unwrap();
            store
                .transaction(|db| {
                    db.execute(
                        "UPDATE mappings SET file_id=(SELECT id FROM files WHERE path='two.txt')",
                        [],
                    )?;
                    Ok(())
                })
                .unwrap();
        } else {
            store
                .transaction(|db| {
                    db.execute("UPDATE mappings SET active=0", [])?;
                    Ok(())
                })
                .unwrap();
        }
        (root, store, source, output)
    }
    fn journal_rows(store: &Store) -> i64 {
        store
            .db
            .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
            .unwrap()
    }
    /// Regular-file contents among the given names, sorted; links are listed
    /// by their target.
    fn contents(dir: &std::path::Path, names: &[String]) -> Vec<String> {
        let mut found = Vec::new();
        for name in names {
            let path = dir.join(name);
            match std::fs::symlink_metadata(&path) {
                Ok(m) if m.file_type().is_symlink() => found.push(format!(
                    "link:{}",
                    std::fs::read_link(&path).unwrap().display()
                )),
                Ok(_) => found.push(std::fs::read_to_string(&path).unwrap()),
                Err(_) => {}
            }
        }
        found.sort();
        found
    }
    #[test]
    fn foreign_leaf_swapped_before_displacement_is_restored_not_destroyed() {
        // Frozen Python checks the leaf and then renames over or unlinks it,
        // so a foreign file swapped in between is destroyed. The native
        // protocol displaces the leaf to a private name first and verifies it
        // there, so the foreign file is returned to its name instead.
        for replacement in [true, false] {
            let (_root, store, source, output) = destructive_catalog(replacement);
            let leaf = output.join("folder/one.txt");
            let swapped = leaf.clone();
            BEFORE_DESTRUCTIVE_LEAF.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    std::fs::remove_file(&swapped).unwrap();
                    std::fs::write(&swapped, "external concurrent bytes").unwrap();
                }))
            });
            let error =
                execute(&store, "default", "apply", &json!({"max_removals":1})).unwrap_err();
            assert!(
                error.0.contains("the foreign entry was restored"),
                "{replacement}: {error:?}"
            );
            assert_eq!(
                std::fs::read_to_string(&leaf).unwrap(),
                "external concurrent bytes"
            );
            assert_eq!(
                std::fs::read_to_string(source.join("one.txt")).unwrap(),
                "original source"
            );
            // The intent stays pending, ownership is not acknowledged, and
            // recovery refuses the changed destination rather than act on it.
            assert_eq!(journal_rows(&store), 1);
            assert!(execute(&store, "default", "recover", &json!({})).is_err());
            assert_eq!(
                std::fs::read_to_string(&leaf).unwrap(),
                "external concurrent bytes"
            );
            let names: Vec<String> = leaves(&output.join("folder"));
            let private: Vec<_> = names
                .iter()
                .filter(|n| n.starts_with(".catabolic-unlink-"))
                .collect();
            assert!(private.is_empty(), "{names:?}");
        }
    }
    #[test]
    fn foreign_writer_reusing_the_leaf_name_keeps_every_entry() {
        // A second foreign writer recreates the leaf after the first foreign
        // entry was displaced, so it cannot be moved back. Nothing is
        // overwritten or deleted: both foreign entries survive.
        for replacement in [true, false] {
            let (_root, store, _source, output) = destructive_catalog(replacement);
            let leaf = output.join("folder/one.txt");
            let (first, second) = (leaf.clone(), leaf.clone());
            BEFORE_DESTRUCTIVE_LEAF.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    std::fs::remove_file(&first).unwrap();
                    std::fs::write(&first, "first foreign").unwrap();
                }))
            });
            AFTER_DESTRUCTIVE_LEAF.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    // After an exchange the leaf holds the new link; after a
                    // quarantine rename it is absent.
                    let _ = std::fs::remove_file(&second);
                    std::fs::write(&second, "second foreign").unwrap();
                }))
            });
            let error =
                execute(&store, "default", "apply", &json!({"max_removals":1})).unwrap_err();
            assert!(error.0.contains("preserved"), "{replacement}: {error:?}");
            let names = leaves(&output.join("folder"));
            let found = contents(&output.join("folder"), &names);
            assert!(found.contains(&"first foreign".to_owned()), "{found:?}");
            assert!(found.contains(&"second foreign".to_owned()), "{found:?}");
            assert_eq!(journal_rows(&store), 1);
        }
    }
    #[test]
    fn interrupted_replacement_and_removal_clean_up_verified_private_links() {
        // Each private link left by an interrupted displacement is verified
        // before deletion; a foreign entry at the private name blocks.
        for replacement in [true, false] {
            for stage in [
                "after_exchange",
                "after_quarantine",
                "after_publish",
                "after_private_move",
            ] {
                let (_root, store, _source, output) = destructive_catalog(replacement);
                let result = execute(
                    &store,
                    "default",
                    "apply",
                    &json!({"max_removals":1,"failpoint":stage}),
                );
                let Err(error) = result else {
                    // This stage belongs to the other protocol mode, or to
                    // replacement only.
                    continue;
                };
                assert!(error.0.contains(stage), "{error:?}");
                execute(&store, "default", "recover", &json!({})).unwrap();
                assert_eq!(journal_rows(&store), 0);
                let names = leaves(&output.join("folder"));
                assert!(
                    names.iter().all(|n| !n.starts_with(".catabolic-")),
                    "{stage}: {names:?}"
                );
                assert_eq!(
                    execute(&store, "default", "verify", &json!({})).unwrap()["healthy"],
                    true
                );
            }
        }
        // A foreign file planted at the quarantine name is never deleted.
        let (_root, store, _source, output) = destructive_catalog(false);
        assert!(
            execute(
                &store,
                "default",
                "apply",
                &json!({"max_removals":1,"failpoint":"after_quarantine"})
            )
            .is_err()
        );
        let folder = output.join("folder");
        let quarantine = leaves(&folder)
            .into_iter()
            .find(|n| n.starts_with(".catabolic-unlink-"))
            .unwrap();
        std::fs::remove_file(folder.join(&quarantine)).unwrap();
        std::fs::write(folder.join(&quarantine), "planted").unwrap();
        let error = execute(&store, "default", "recover", &json!({})).unwrap_err();
        assert!(
            error.0.contains("changed externally; retained in"),
            "{error:?}"
        );
        let kept = folder.join(format!("{quarantine}.private")).join("entry");
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "planted");
        assert_eq!(journal_rows(&store), 1);
        // A leftover private directory with foreign content keeps blocking.
        let error = execute(&store, "default", "recover", &json!({})).unwrap_err();
        assert!(error.0.contains("retained in"), "{error:?}");
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "planted");
    }
    /// The private entry (temporary or quarantine name) currently in `folder`.
    fn private_entry(folder: &std::path::Path) -> std::path::PathBuf {
        let name = leaves(folder)
            .into_iter()
            .find(|n| n.starts_with(".catabolic-") && !n.ends_with(".private"))
            .unwrap();
        folder.join(name)
    }
    #[test]
    fn entry_swapped_after_verification_is_retained_not_deleted() {
        // Sol round 1: a writer swaps the private name for a foreign file
        // after the caller verified it. The swapped entry is moved into the
        // exclusive directory, fails verification there and is retained.
        for replacement in [true, false] {
            let (_root, store, _source, output) = destructive_catalog(replacement);
            let folder = output.join("folder");
            let watched = folder.clone();
            BEFORE_PRIVATE_MOVE.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    let entry = private_entry(&watched);
                    std::fs::remove_file(&entry).unwrap();
                    std::fs::write(&entry, "foreign at the private name").unwrap();
                }))
            });
            let error =
                execute(&store, "default", "apply", &json!({"max_removals":1})).unwrap_err();
            assert!(error.0.contains("retained in"), "{replacement}: {error:?}");
            let directory = leaves(&folder)
                .into_iter()
                .find(|n| n.ends_with(".private"))
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(folder.join(directory).join("entry")).unwrap(),
                "foreign at the private name"
            );
            assert_eq!(journal_rows(&store), 1);
            assert!(execute(&store, "default", "recover", &json!({})).is_err());
        }
    }
    #[test]
    fn private_directory_squatter_blocks_without_deleting() {
        // A non-directory, or a directory with other content, at the private
        // directory name is never removed or reused.
        for squat in ["file", "directory"] {
            let (_root, store, _source, output) = destructive_catalog(false);
            let folder = output.join("folder");
            assert!(
                execute(
                    &store,
                    "default",
                    "apply",
                    &json!({"max_removals":1,"failpoint":"after_quarantine"})
                )
                .is_err()
            );
            let entry = private_entry(&folder);
            let directory = std::path::PathBuf::from(format!("{}.private", entry.display()));
            if squat == "file" {
                std::fs::write(&directory, "squatter").unwrap();
            } else {
                std::fs::create_dir(&directory).unwrap();
                std::fs::write(directory.join("other"), "squatter").unwrap();
            }
            let error = execute(&store, "default", "recover", &json!({})).unwrap_err();
            assert!(
                error.0.contains("retained in") || error.0.contains("failed verification"),
                "{squat}: {error:?}"
            );
            assert!(std::fs::symlink_metadata(&entry).is_ok(), "{squat}");
            assert_eq!(journal_rows(&store), 1);
        }
    }
    #[test]
    fn characterize_residual_same_user_writer_inside_the_private_directory() {
        // RESIDUAL, not a qualification: the exclusive directory is 0700 and
        // owned by this user, so only a writer running as the same user can
        // reach it. Such a writer can still replace the verified entry
        // between verification and unlink, and that entry is deleted.
        let (_root, store, _source, output) = destructive_catalog(false);
        let folder = output.join("folder");
        let watched = folder.clone();
        BEFORE_PRIVATE_UNLINK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let directory = leaves(&watched)
                    .into_iter()
                    .find(|n| n.ends_with(".private"))
                    .unwrap();
                let entry = watched.join(directory).join("entry");
                std::fs::remove_file(&entry).unwrap();
                std::fs::write(&entry, "same-user substitution").unwrap();
            }))
        });
        execute(&store, "default", "apply", &json!({"max_removals":1})).unwrap();
        assert!(
            leaves(&folder)
                .iter()
                .all(|n| !n.starts_with(".catabolic-"))
        );
        eprintln!(
            "RESIDUAL: a same-user writer inside the 0700 private directory can substitute the verified entry before unlink"
        );
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
    fn default_feature_builds_refuse_failpoint_requests_before_effects() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                SIMULATE_DEFAULT_FEATURES.with(|s| s.set(false));
            }
        }
        let reset = Reset;
        SIMULATE_DEFAULT_FEATURES.with(|s| s.set(true));
        for request in [
            json!({"failpoint":"after_journal"}),
            json!({"crash":true}),
            json!({"failpoint":"after_journal","crash":true}),
            json!({"failpoint":null}),
        ] {
            let (_root, store, source, output) = catalog();
            for action in ["apply", "recover", "preview", "verify"] {
                let error = execute(&store, "default", action, &request).unwrap_err();
                assert_eq!(error.0, FAILPOINT_REFUSAL, "{action} {request}");
            }
            let guard = publication_lock(&store).unwrap();
            let mut locked = request.clone();
            locked["catalog"] = json!("global");
            let error = execute_locked(&store, "default", "apply", &locked, &guard).unwrap_err();
            assert_eq!(error.0, FAILPOINT_REFUSAL);
            drop(guard);
            let error = crate::hardlink_mutation::execute(
                &store,
                "default",
                &json!({"kind":"hard_create","catalog":"global","path":"x"}),
                &request,
            )
            .unwrap_err();
            assert_eq!(error.0, FAILPOINT_REFUSAL);
            // Refused before any journal row, marker or link exists.
            assert_eq!(
                store
                    .db
                    .query_row("SELECT count(*) FROM journal", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                0
            );
            assert!(leaves(&output).is_empty());
            assert_eq!(
                std::fs::read(source.join("one.txt")).unwrap(),
                b"original source"
            );
            // A request without the fields is unaffected.
            assert_eq!(
                execute(&store, "default", "apply", &json!({})).unwrap()["healthy"],
                true
            );
        }
        assert!(admit_failpoints(&json!({"crash":false})).is_err());
        assert!(admit_failpoints(&json!({"failpoints":"x"})).is_ok());
        drop(reset);
        // The workspace test build itself enables the feature.
        assert!(
            failpoints_enabled(),
            "workspace test builds enable failpoints"
        );
        assert!(admit_failpoints(&json!({"failpoint":"after_journal"})).is_ok());
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
