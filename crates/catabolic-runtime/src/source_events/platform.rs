// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded descriptor-relative kernel subscriptions. No source bytes are read.
use catabolic_store::{Error, Result, source_events::Source};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString, OsString},
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const MAX_WATCHES: usize = 16384;
const MAX_ENTRIES: usize = 200_000;
#[cfg(target_os = "linux")]
const MAX_EVENTS: usize = 4096;
#[derive(Clone)]
struct Target {
    source: usize,
    path: PathBuf,
    directory: bool,
}
pub struct Backend {
    kernel: Kernel,
    targets: BTreeMap<usize, Vec<Target>>,
    sources: Vec<Source>,
    roots: Vec<Option<(u64, u64)>>,
    pub unavailable: BTreeSet<usize>,
    registrations: usize,
}
#[derive(Default)]
pub struct Batch {
    pub sources: BTreeSet<usize>,
    pub rebuild: bool,
    pub lost: bool,
}
impl Backend {
    pub fn new(sources: &[Source]) -> Result<Self> {
        let mut this = Self {
            kernel: Kernel::new()?,
            targets: BTreeMap::new(),
            sources: sources.into(),
            roots: vec![None; sources.len()],
            unavailable: BTreeSet::new(),
            registrations: 0,
        };
        let mut entries = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        for (index, source) in sources.iter().enumerate() {
            let result = (|| {
                if entries >= MAX_ENTRIES {
                    return Err(Error("native event entry budget".into()));
                }
                if Instant::now() >= deadline {
                    return Err(Error("native event registration deadline".into()));
                }
                let root = open_root(&source.root)?;
                let metadata = root.metadata()?;
                if source.check_device && metadata.dev() != source.device
                    || source.check_inode && metadata.ino() != source.inode
                {
                    return Err(Error("native event root identity changed".into()));
                }
                this.roots[index] = Some((metadata.dev(), metadata.ino()));
                let mut pending = vec![(root, source.root.clone())];
                while let Some((directory, path)) = pending.pop() {
                    if Instant::now() >= deadline {
                        return Err(Error("native event registration deadline".into()));
                    }
                    this.register(&directory, index, path.clone(), true)?;
                    for name in names(&directory, &mut entries, deadline)? {
                        if Instant::now() >= deadline {
                            return Err(Error("native event registration deadline".into()));
                        }
                        let child = path.join(&name);
                        if excluded(source, &child) {
                            continue;
                        }
                        // Failed directory opens are ordinary files/symlinks or
                        // unavailable entries. statat identifies the type without
                        // following links before deciding whether coverage is lost.
                        let name_c = CString::new(name.as_bytes())
                            .map_err(|_| Error("invalid event entry".into()))?;
                        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                        // SAFETY: live parent/name and writable stat storage.
                        let status = unsafe {
                            libc::fstatat(
                                directory.as_raw_fd(),
                                name_c.as_ptr(),
                                stat.as_mut_ptr(),
                                libc::AT_SYMLINK_NOFOLLOW,
                            )
                        };
                        if status != 0 {
                            return Err(std::io::Error::last_os_error().into());
                        }
                        // SAFETY: successful fstatat initialized stat.
                        let stat = unsafe { stat.assume_init() };
                        if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
                            if pending.len() + this.registrations >= MAX_WATCHES {
                                return Err(Error("native event registration budget".into()));
                            }
                            let fd = open_child(&directory, &name, true)?;
                            if fd.metadata()?.dev() != metadata.dev() {
                                return Err(Error("native event directory device changed".into()));
                            }
                            pending.push((fd, child));
                        } else if cfg!(target_os = "macos")
                            && stat.st_mode & libc::S_IFMT == libc::S_IFREG
                        {
                            let fd = open_child(&directory, &name, false)?;
                            this.register(&fd, index, child, false)?;
                        }
                    }
                }
                Ok(())
            })();
            if result.is_err() {
                // Reported once as a coverage transition by the session.
                this.unavailable.insert(index);
            }
        }
        Ok(this)
    }
    fn register(
        &mut self,
        file: &File,
        source: usize,
        path: PathBuf,
        directory: bool,
    ) -> Result<()> {
        if self.registrations >= MAX_WATCHES {
            return Err(Error("native event registration budget".into()));
        }
        let id = self.kernel.add(file, directory)?;
        self.targets.entry(id).or_default().push(Target {
            source,
            path,
            directory,
        });
        self.registrations += 1;
        Ok(())
    }
    pub fn registrations(&self) -> usize {
        self.registrations
    }
    pub fn wait(&mut self, duration: Duration) -> Result<Batch> {
        let mut batch = Batch::default();
        for event in self.kernel.read(duration)? {
            if event.lost {
                batch.lost = true;
                batch.rebuild = true;
            }
            if let Some(targets) = self.targets.get(&event.id) {
                for target in targets {
                    let path = event
                        .name
                        .as_ref()
                        .map(|name| target.path.join(name))
                        .unwrap_or_else(|| target.path.clone());
                    if !excluded(&self.sources[target.source], &path) {
                        batch.sources.insert(target.source);
                        batch.rebuild |=
                            event.rebuild || cfg!(target_os = "macos") && target.directory;
                    }
                }
            } else if !event.lost {
                batch.lost = true;
                batch.rebuild = true;
            }
        }
        // A directory subscription follows its inode through a rename. The
        // pathname check detects a disconnected/replaced configured root even
        // if that kernel's queue did not emit a usable self event. Only the
        // transition is reported: the root pin is then cleared, and the source
        // stays in `unavailable` (degraded coverage) until a rebuild restores
        // it. Degraded sources are never re-reported on later waits (FS7).
        for (index, source) in self.sources.iter().enumerate() {
            let Some(pinned) = self.roots[index] else {
                continue;
            };
            let current = open_root(&source.root)
                .and_then(|f| Ok((f.metadata()?.dev(), f.metadata()?.ino())))
                .ok();
            if current != Some(pinned) {
                batch.sources.insert(index);
                batch.rebuild = true;
                batch.lost = true;
                self.roots[index] = None;
                self.unavailable.insert(index);
            }
        }
        if batch.lost {
            batch.sources.extend(0..self.sources.len());
        }
        Ok(batch)
    }
}
fn excluded(source: &Source, path: &Path) -> bool {
    source.exclusions.iter().any(|p| path.starts_with(p))
}
fn open_root(path: &Path) -> Result<File> {
    if !path.is_absolute() {
        return Err(Error("native event root must be absolute".into()));
    }
    let mut current = File::open("/")?;
    for component in path.components() {
        match component {
            std::path::Component::RootDir => (),
            std::path::Component::Normal(name) => current = open_child(&current, name, true)?,
            _ => return Err(Error("invalid native event root".into())),
        }
    }
    Ok(current)
}
fn open_child(parent: &File, name: &std::ffi::OsStr, directory: bool) -> Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| Error("invalid event name".into()))?;
    #[cfg(target_os = "macos")]
    let access = if directory {
        libc::O_RDONLY
    } else {
        libc::O_EVTONLY
    };
    #[cfg(target_os = "linux")]
    let access = libc::O_RDONLY;
    let flags = access
        | libc::O_NONBLOCK
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: parent and CString are live; openat returns a new descriptor.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: this successful openat descriptor has unique ownership.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn names(file: &File, entries: &mut usize, deadline: Instant) -> Result<Vec<OsString>> {
    let fd = file.try_clone()?.into_raw_fd();
    // SAFETY: fd is a newly duplicated directory descriptor transferred on success.
    let pointer = unsafe { libc::fdopendir(fd) };
    if pointer.is_null() {
        // SAFETY: fdopendir failed and did not take ownership.
        unsafe { libc::close(fd) };
        return Err(std::io::Error::last_os_error().into());
    }
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            // SAFETY: Directory uniquely owns the fdopendir result.
            unsafe { libc::closedir(self.0) };
        }
    }
    let directory = Directory(pointer);
    let mut result = vec![];
    loop {
        if Instant::now() >= deadline {
            return Err(Error("native event registration deadline".into()));
        }
        // SAFETY: errno is thread-local and directory is a live DIR pointer.
        unsafe { set_errno(0) };
        // SAFETY: readdir result is copied before the next call.
        let entry = unsafe { libc::readdir(directory.0) };
        if entry.is_null() {
            // SAFETY: errno is thread-local and read immediately after readdir.
            if unsafe { errno() } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            break;
        }
        // SAFETY: d_name is NUL-terminated by successful readdir.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if *entries >= MAX_ENTRIES {
            return Err(Error("native event entry budget".into()));
        }
        // Charge immediately: an error discarding this directory's partial
        // names must not restore the shared source-set enumeration allowance.
        *entries += 1;
        result.push(std::ffi::OsStr::from_bytes(bytes).to_owned());
    }
    Ok(result)
}
#[cfg(target_os = "linux")]
unsafe fn set_errno(value: i32) {
    // SAFETY: caller uses the current thread's errno location.
    unsafe { *libc::__errno_location() = value };
}
#[cfg(target_os = "linux")]
unsafe fn errno() -> i32 {
    // SAFETY: errno location is valid for the current thread.
    unsafe { *libc::__errno_location() }
}
#[cfg(target_os = "macos")]
unsafe fn set_errno(value: i32) {
    // SAFETY: caller uses the current thread's errno location.
    unsafe { *libc::__error() = value };
}
#[cfg(target_os = "macos")]
unsafe fn errno() -> i32 {
    // SAFETY: errno location is valid for the current thread.
    unsafe { *libc::__error() }
}
struct Event {
    id: usize,
    name: Option<OsString>,
    rebuild: bool,
    lost: bool,
}
#[cfg(target_os = "linux")]
struct Kernel(File);
#[cfg(target_os = "linux")]
impl Kernel {
    fn new() -> Result<Self> {
        // SAFETY: inotify_init1 has no pointer arguments and returns a new FD.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: successful inotify_init1 transfers a unique descriptor.
        Ok(Self(unsafe { File::from_raw_fd(fd) }))
    }
    fn add(&mut self, file: &File, _directory: bool) -> Result<usize> {
        let path = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
        let mask = libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MODIFY
            | libc::IN_CLOSE_WRITE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_ATTRIB
            | libc::IN_DELETE_SELF
            | libc::IN_MOVE_SELF
            | libc::IN_UNMOUNT
            | libc::IN_ONLYDIR;
        // SAFETY: path refers to the live pinned directory FD, not a user path.
        let id = unsafe { libc::inotify_add_watch(self.0.as_raw_fd(), path.as_ptr(), mask) };
        if id < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(id as usize)
    }
    fn read(&mut self, duration: Duration) -> Result<Vec<Event>> {
        let mut descriptor = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: a single live pollfd, bounded millisecond timeout.
        let result = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                duration.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                return Ok(vec![]);
            }
            return Err(error.into());
        }
        if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(Error("native event stream disconnected".into()));
        }
        let mut events = vec![];
        let mut bytes = [0u8; 65536];
        loop {
            // SAFETY: the byte buffer is writable for its complete length.
            let length =
                unsafe { libc::read(self.0.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
            if length < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    break;
                }
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if length == 0 {
                return Err(Error("native event stream disconnected".into()));
            }
            let mut offset = 0;
            while offset < length as usize {
                let header = std::mem::size_of::<libc::inotify_event>();
                if offset + header > length as usize {
                    return Err(Error("invalid native kernel event".into()));
                }
                // SAFETY: bounds checked header may be unaligned in byte buffer.
                let event = unsafe {
                    std::ptr::read_unaligned(
                        bytes.as_ptr().add(offset).cast::<libc::inotify_event>(),
                    )
                };
                let end = offset + header + event.len as usize;
                if end > length as usize {
                    return Err(Error("invalid native kernel event".into()));
                }
                let name = &bytes[offset + header..end];
                let name = &name[..name.iter().position(|b| *b == 0).unwrap_or(name.len())];
                events.push(Event {
                    id: event.wd as usize,
                    name: (!name.is_empty()).then(|| std::ffi::OsStr::from_bytes(name).to_owned()),
                    lost: event.mask & (libc::IN_Q_OVERFLOW | libc::IN_IGNORED | libc::IN_UNMOUNT)
                        != 0,
                    rebuild: event.mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0
                        || event.mask & libc::IN_ISDIR != 0
                            && event.mask
                                & (libc::IN_CREATE
                                    | libc::IN_DELETE
                                    | libc::IN_MOVED_FROM
                                    | libc::IN_MOVED_TO)
                                != 0,
                });
                offset = end;
                if events.len() >= MAX_EVENTS {
                    events.push(Event {
                        id: usize::MAX,
                        name: None,
                        rebuild: true,
                        lost: true,
                    });
                    return Ok(events);
                }
            }
        }
        Ok(events)
    }
}
#[cfg(target_os = "macos")]
struct Kernel {
    queue: File,
    files: Vec<File>,
}
#[cfg(target_os = "macos")]
impl Kernel {
    fn new() -> Result<Self> {
        // SAFETY: kqueue returns a new owned descriptor.
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: the new descriptor is uniquely owned by queue.
        let queue = unsafe { File::from_raw_fd(fd) };
        // SAFETY: configure close-on-exec for the new private queue descriptor.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self {
            queue,
            files: vec![],
        })
    }
    fn add(&mut self, file: &File, _directory: bool) -> Result<usize> {
        let file = file.try_clone()?;
        let id = file.as_raw_fd() as usize;
        let event = libc::kevent {
            ident: id,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_DELETE
                | libc::NOTE_WRITE
                | libc::NOTE_EXTEND
                | libc::NOTE_ATTRIB
                | libc::NOTE_RENAME
                | libc::NOTE_REVOKE,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: one initialized registration and no output/event wait.
        if unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                &event,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        self.files.push(file);
        Ok(id)
    }
    fn read(&mut self, duration: Duration) -> Result<Vec<Event>> {
        // SAFETY: kevent is a plain SDK struct whose all-zero representation is valid.
        let mut events: [libc::kevent; 256] = unsafe { std::mem::zeroed() };
        let timeout = libc::timespec {
            tv_sec: duration.as_secs() as _,
            tv_nsec: duration.subsec_nanos() as _,
        };
        // SAFETY: bounded output slice and initialized timeout, no changes.
        let n = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                events.len() as i32,
                &timeout,
            )
        };
        if n < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                return Ok(vec![]);
            }
            return Err(error.into());
        }
        let mut result = events[..n as usize]
            .iter()
            .map(|event| Event {
                id: event.ident,
                name: None,
                rebuild: event.fflags & (libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE)
                    != 0,
                lost: event.flags & (libc::EV_ERROR | libc::EV_EOF) != 0
                    || event.fflags & libc::NOTE_REVOKE != 0,
            })
            .collect::<Vec<_>>();
        if n as usize == events.len() {
            result.push(Event {
                id: usize::MAX,
                name: None,
                rebuild: true,
                lost: true,
            });
        }
        Ok(result)
    }
}
