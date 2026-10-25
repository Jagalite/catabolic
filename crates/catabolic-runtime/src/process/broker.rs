//! DISABLED prototype. Capability lookup never opens a child-supplied pathname.
//! This module grants neither executable identity nor native cleanup authority.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::fs::{FileExt, MetadataExt},
    },
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const RECV: libc::c_ulong = 0xc0502100;
const SEND: libc::c_ulong = 0xc0182101;
const ID_VALID: libc::c_ulong = 0x40082102;
const ADDFD: libc::c_ulong = 0x40182103;
#[repr(C)]
#[derive(Default)]
struct Data {
    nr: i32,
    arch: u32,
    ip: u64,
    args: [u64; 6],
}
#[repr(C)]
#[derive(Default)]
struct Notification {
    id: u64,
    pid: u32,
    flags: u32,
    data: Data,
}
#[repr(C)]
#[derive(Default)]
struct Response {
    id: u64,
    val: i64,
    error: i32,
    flags: u32,
}
#[repr(C)]
struct AddFd {
    id: u64,
    flags: u32,
    srcfd: u32,
    newfd: u32,
    newfd_flags: u32,
}

enum Access {
    ReadOnly,
    PrivateEmptyOutput,
}
struct Leaf {
    pinned: File,
    access: Access,
    opened: bool,
}
pub(super) struct Capabilities {
    leaves: BTreeMap<Vec<u8>, Leaf>,
}
impl Capabilities {
    pub(super) fn new() -> Self {
        Self {
            leaves: BTreeMap::new(),
        }
    }
    /// Host-selected exact aliases only. Relative aliases have virtual meaning;
    /// neither cwd nor dirfd are ever consulted. Caller owns private output dir.
    pub(super) fn insert_read_only(&mut self, alias: Vec<u8>, pinned: File) -> io::Result<()> {
        self.insert(alias, pinned, Access::ReadOnly)
    }
    /// The parent creates the leaf itself, relative to a pinned owner-only
    /// directory. No existing output file or child pathname can supply this
    /// capability. Namespace placement/publication still belongs to the native
    /// output owner, not this disabled process prototype.
    pub(super) fn create_private_output(
        &mut self,
        alias: Vec<u8>,
        directory: &File,
        name: &std::ffi::CStr,
    ) -> io::Result<()> {
        use std::os::fd::FromRawFd;
        let meta = directory.metadata()?;
        let bytes = name.to_bytes();
        if !meta.is_dir()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
            || bytes.is_empty()
            || bytes.contains(&b'/')
            || bytes == b"."
            || bytes == b".."
        {
            return Err(io::Error::other(
                "pinned private directory and single leaf name required",
            ));
        }
        if self.leaves.contains_key(&alias)
            || alias.is_empty()
            || alias.len() > 4095
            || alias.contains(&0)
        {
            return Err(io::Error::other("invalid output alias"));
        }
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        self.insert(alias, file, Access::PrivateEmptyOutput)
    }
    fn insert(&mut self, alias: Vec<u8>, pinned: File, access: Access) -> io::Result<()> {
        let meta = pinned.metadata()?;
        if alias.is_empty()
            || alias.len() > 4095
            || alias.contains(&0)
            || !meta.is_file()
            || self.leaves.contains_key(&alias)
        {
            return Err(io::Error::other(
                "invalid or duplicate regular-leaf capability",
            ));
        }
        if matches!(access, Access::PrivateEmptyOutput)
            && (meta.len() != 0
                || meta.nlink() != 1
                || meta.uid() != unsafe { libc::geteuid() }
                || meta.mode() & 0o022 != 0)
        {
            return Err(io::Error::other(
                "output must be empty, single-link, owned, and not group/world writable",
            ));
        }
        // Output insertion is private; the only non-test construction path is
        // parent exclusive creation under a pinned owner-only directory.
        for leaf in self.leaves.values() {
            let existing = leaf.pinned.metadata()?;
            if existing.dev() == meta.dev()
                && existing.ino() == meta.ino()
                && (matches!(access, Access::PrivateEmptyOutput)
                    || matches!(leaf.access, Access::PrivateEmptyOutput))
            {
                return Err(io::Error::other(
                    "output inode may have only one capability alias",
                ));
            }
        }
        self.leaves.insert(
            alias,
            Leaf {
                pinned,
                access,
                opened: false,
            },
        );
        Ok(())
    }
    fn prepare(&mut self, alias: &[u8], flags: u64) -> io::Result<(File, bool)> {
        let leaf = self
            .leaves
            .get_mut(alias)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EACCES))?;
        let allowed =
            (libc::O_ACCMODE | libc::O_CLOEXEC | libc::O_LARGEFILE | libc::O_CREAT | libc::O_TRUNC)
                as u64;
        if flags & !allowed != 0 {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        let mode = flags as i32 & libc::O_ACCMODE;
        let truncate = flags as i32 & libc::O_TRUNC != 0;
        let create = flags as i32 & libc::O_CREAT != 0;
        match leaf.access {
            Access::ReadOnly if mode != libc::O_RDONLY || truncate || create => {
                return Err(io::Error::from_raw_os_error(libc::EACCES));
            }
            Access::PrivateEmptyOutput if mode != libc::O_WRONLY && mode != libc::O_RDWR => {
                return Err(io::Error::from_raw_os_error(libc::EACCES));
            }
            Access::PrivateEmptyOutput
                if truncate && (leaf.opened || leaf.pinned.metadata()?.len() != 0) =>
            {
                return Err(io::Error::from_raw_os_error(libc::EACCES));
            }
            _ => (),
        }
        // Trusted parent descriptor path, never target bytes. Opening yields an
        // independent description; dup would incorrectly share the seek offset.
        let path = format!("/proc/self/fd/{}", leaf.pinned.as_raw_fd());
        let file = OpenOptions::new()
            .read(mode != libc::O_WRONLY)
            .write(mode != libc::O_RDONLY)
            .open(path)?;
        let a = leaf.pinned.metadata()?;
        let b = file.metadata()?;
        if a.dev() != b.dev() || a.ino() != b.ino() || !b.is_file() {
            return Err(io::Error::other("pinned leaf identity changed"));
        }
        Ok((file, flags as i32 & libc::O_CLOEXEC != 0))
    }
}

/// Loop completion only, never a retirement or capacity-release witness.
#[derive(Debug)]
pub(super) enum BrokerStop {
    Cancelled,
    TargetsGone,
}

pub(super) struct Broker {
    listener: OwnedFd,
    memory: File,
    pid: u32,
    capabilities: Capabilities,
    #[cfg(test)]
    before_delivery: Option<Box<dyn FnOnce() + Send>>,
    #[cfg(test)]
    before_receive: Option<Box<dyn FnOnce() + Send>>,
    #[cfg(test)]
    stale_deliveries: usize,
    #[cfg(test)]
    restricted_thread_origins: bool,
}
impl Broker {
    /// `memory` is opened by the exact owning parent from its unreaped child,
    /// before execution; caller must not replace it after PID reuse.
    pub(super) fn new(
        listener: OwnedFd,
        memory: File,
        pid: u32,
        capabilities: Capabilities,
    ) -> Self {
        Self {
            listener,
            memory,
            pid,
            capabilities,
            #[cfg(test)]
            before_delivery: None,
            #[cfg(test)]
            before_receive: None,
            #[cfg(test)]
            stale_deliveries: 0,
            #[cfg(test)]
            restricted_thread_origins: false,
        }
    }
    fn valid(&self, id: u64) -> io::Result<bool> {
        if unsafe { libc::ioctl(self.listener.as_raw_fd(), ID_VALID, &id) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(false)
        } else {
            Err(error)
        }
    }
    fn checkpoint(cancel: &AtomicBool, deadline: Instant) -> io::Result<()> {
        if cancel.load(Ordering::Acquire) {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        Ok(())
    }
    fn deny(&self, id: u64, errno: i32) -> io::Result<()> {
        let response = Response {
            id,
            error: -errno,
            ..Default::default()
        };
        if unsafe { libc::ioctl(self.listener.as_raw_fd(), SEND, &response) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ENOENT) {
                return Err(error);
            }
        }
        Ok(())
    }
    fn path(&self, address: u64, cancel: &AtomicBool, deadline: Instant) -> io::Result<Vec<u8>> {
        // Bytewise bounded copy avoids requiring readability past the NUL at a
        // page boundary. Pointer races can only select another declared alias.
        let mut bytes = Vec::with_capacity(128);
        for i in 0..4096u64 {
            if i % 64 == 0 {
                Self::checkpoint(cancel, deadline)?;
            }
            let mut byte = [0];
            if self.memory.read_at(
                &mut byte,
                address
                    .checked_add(i)
                    .ok_or_else(|| io::Error::other("pointer overflow"))?,
            )? != 1
            {
                return Err(io::Error::other("short target memory read"));
            }
            if byte[0] == 0 {
                return Ok(bytes);
            }
            bytes.push(byte[0]);
        }
        Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG))
    }
    fn handle(
        &mut self,
        n: Notification,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> io::Result<()> {
        Self::checkpoint(cancel, deadline)?;
        if !self.valid(n.id)? {
            return Ok(());
        }
        // Production remains leader-only until a private listener-origin token
        // is issued by the final spawn owner. The test harness installs this
        // exact listener in a fresh child before any code, permitting only
        // CLONE_THREAD|CLONE_VM descendants and scrubbing bootstrap handles.
        let mut origin_allowed = n.pid == self.pid;
        #[cfg(test)]
        {
            origin_allowed |= self.restricted_thread_origins;
        }
        if !origin_allowed || n.data.arch != 0xc000003e || n.flags != 0 {
            return self.deny(n.id, libc::EACCES);
        }
        let (pointer, flags) = match n.data.nr as i64 {
            libc::SYS_open => (n.data.args[0], n.data.args[1]),
            libc::SYS_openat => (n.data.args[1], n.data.args[2]),
            _ => return self.deny(n.id, libc::ENOSYS),
        };
        let alias = match self.path(pointer, cancel, deadline) {
            Ok(v) => v,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(e);
            }
            Err(_) => return self.deny(n.id, libc::EFAULT),
        };
        if !self.valid(n.id)? {
            return Ok(());
        }
        Self::checkpoint(cancel, deadline)?;
        let (file, cloexec) = match self.capabilities.prepare(&alias, flags) {
            Ok(v) => v,
            Err(_) => return self.deny(n.id, libc::EACCES),
        };
        if !self.valid(n.id)? {
            return Ok(());
        }
        Self::checkpoint(cancel, deadline)?;
        #[cfg(test)]
        if let Some(hook) = self.before_delivery.take() {
            hook();
        }
        let add = AddFd {
            id: n.id,
            flags: 2, /* ADDFD_FLAG_SEND */
            srcfd: file.as_raw_fd() as u32,
            newfd: 0,
            newfd_flags: if cloexec { libc::O_CLOEXEC as u32 } else { 0 },
        };
        if unsafe { libc::ioctl(self.listener.as_raw_fd(), ADDFD, &add) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ENOENT) {
                return Err(error);
            }
            #[cfg(test)]
            {
                self.stale_deliveries += 1;
            }
        } else {
            self.capabilities.leaves.get_mut(&alias).unwrap().opened = true;
        }
        // Staged independent FD closes on success, stale ID and every error.
        Ok(())
    }
    /// Bounded servicing only, NOT a cleanup witness. Caller retains exact Child
    /// and joins this broker before considering any future retirement protocol.
    /// Checkpoints bound userspace work, not kernel I/O latency. A stuck memory
    /// read/file reopen/ioctl keeps this owner unresolved; a timeout elsewhere
    /// must never release its capacity, file capabilities, or claim.
    /// The pinned memory FD is never reopened after later exec: a changed mm
    /// fails closed rather than following a potentially reused PID.
    pub(super) fn serve(
        &mut self,
        cancel: &AtomicBool,
        budget: Duration,
    ) -> io::Result<BrokerStop> {
        let started = Instant::now();
        let deadline = started
            .checked_add(budget)
            .ok_or_else(|| io::Error::other("deadline overflow"))?;
        let mut requests = 0usize;
        while !cancel.load(Ordering::Acquire) {
            if started.elapsed() >= budget || requests >= 8192 {
                return Err(io::Error::other("broker budget exhausted"));
            }
            let mut poll = libc::pollfd {
                fd: self.listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let rc = unsafe { libc::poll(&mut poll, 1, 20) };
            if rc < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(e);
            }
            if rc == 0 {
                continue;
            }
            if poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err(io::Error::other("listener poll error"));
            }
            if poll.revents & libc::POLLHUP != 0 {
                return Ok(BrokerStop::TargetsGone);
            }
            if poll.revents & libc::POLLIN == 0 {
                continue;
            }
            // Sole receiver: one poll readiness observation consumes at most
            // one RECV. No drain loop or concurrent listener reader is allowed.
            #[cfg(test)]
            if let Some(hook) = self.before_receive.take() {
                hook();
            }
            let mut n = Notification::default();
            if unsafe { libc::ioctl(self.listener.as_raw_fd(), RECV, &mut n) } < 0 {
                let e = io::Error::last_os_error();
                if matches!(e.raw_os_error(), Some(libc::EINTR) | Some(libc::ENOENT)) {
                    continue;
                }
                return Err(e);
            }
            requests += 1;
            match self.handle(n, cancel, deadline) {
                Err(e)
                    if e.kind() == io::ErrorKind::Interrupted && cancel.load(Ordering::Acquire) =>
                {
                    return Ok(BrokerStop::Cancelled);
                }
                other => other?,
            }
        }
        Ok(BrokerStop::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    #[test]
    fn invalid_listener_errors_and_output_provenance_fail_closed() {
        let dir = tempfile::Builder::new()
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir()
            .unwrap();
        let file = tempfile::tempfile().unwrap();
        let broker = Broker::new(
            file.into(),
            tempfile::tempfile().unwrap(),
            1,
            Capabilities::new(),
        );
        assert!(
            broker.valid(1).is_err(),
            "ENOTTY is not a stale notification"
        );
        let directory = File::open(dir.path()).unwrap();
        let mut caps = Capabilities::new();
        assert!(
            caps.create_private_output(b"bad".to_vec(), &directory, c"../outside")
                .is_err()
        );
        caps.create_private_output(b"out".to_vec(), &directory, c"out")
            .unwrap();
        assert!(
            caps.create_private_output(b"other".to_vec(), &directory, c"out")
                .is_err()
        );
        let meta = std::fs::metadata(dir.path().join("out")).unwrap();
        assert_eq!(meta.mode() & 0o077, 0);
        assert_eq!(meta.nlink(), 1);
    }
    #[test]
    fn capability_descriptions_have_independent_offsets_and_exact_rights() {
        let dir = tempfile::Builder::new()
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir()
            .unwrap();
        let path = dir.path().join("source");
        std::fs::write(&path, b"abc").unwrap();
        let mut caps = Capabilities::new();
        caps.insert(
            b"/virtual/source".to_vec(),
            File::open(path).unwrap(),
            Access::ReadOnly,
        )
        .unwrap();
        let (mut a, _) = caps
            .prepare(b"/virtual/source", libc::O_RDONLY as u64)
            .unwrap();
        let (mut b, _) = caps
            .prepare(b"/virtual/source", libc::O_RDONLY as u64)
            .unwrap();
        let mut byte = [0];
        a.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"a");
        b.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"a");
        for flags in [
            libc::O_WRONLY,
            libc::O_CREAT,
            libc::O_TRUNC,
            libc::O_PATH,
            libc::O_ASYNC,
        ] {
            assert!(caps.prepare(b"/virtual/source", flags as u64).is_err());
        }
        assert!(caps.prepare(b"/proc/self/mem", 0).is_err());
        assert!(caps.prepare(b"source", 0).is_err());
    }
    #[test]
    fn output_truncation_is_virtual_first_open_only_and_never_parent_side_effect() {
        let dir = tempfile::Builder::new()
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir()
            .unwrap();
        let path = dir.path().join("output");
        let mut caps = Capabilities::new();
        caps.create_private_output(b"out".to_vec(), &File::open(dir.path()).unwrap(), c"output")
            .unwrap();
        let flags = (libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC) as u64;
        let (mut file, _) = caps.prepare(b"out", flags).unwrap();
        // Atomic ADDFD delivery is what commits this state in production core.
        caps.leaves.get_mut(b"out".as_slice()).unwrap().opened = true;
        file.write_all(b"preserve").unwrap();
        drop(file);
        assert!(caps.prepare(b"out", flags).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"preserve");
        assert!(caps.prepare(b"out", libc::O_RDONLY as u64).is_err());
    }
}

#[cfg(test)]
mod kernel_tests {
    use super::super::restricted::linux;
    use super::*;
    use std::{
        ffi::CString,
        os::{fd::FromRawFd, unix::process::CommandExt},
        process::Command,
        sync::Arc,
    };
    struct FixtureChild(std::process::Child);
    impl std::ops::Deref for FixtureChild {
        type Target = std::process::Child;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
    impl std::ops::DerefMut for FixtureChild {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }
    impl Drop for FixtureChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    unsafe fn transfer(socket: i32, fd: i32) -> io::Result<()> {
        let mut byte = [1u8];
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let mut control = [0usize; 4];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = unsafe { libc::CMSG_SPACE(4) } as usize;
        let c = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        unsafe {
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(4) as usize;
            std::ptr::write_unaligned(libc::CMSG_DATA(c).cast::<i32>(), fd);
        }
        if unsafe { libc::sendmsg(socket, &msg, libc::MSG_NOSIGNAL) } != 1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    fn receive(socket: i32) -> io::Result<OwnedFd> {
        let mut byte = [0u8];
        let mut control = [0usize; 4];
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&control);
        if unsafe { libc::recvmsg(socket, &mut msg, libc::MSG_CMSG_CLOEXEC) } != 1 {
            return Err(io::Error::last_os_error());
        }
        let c = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        if c.is_null()
            || msg.msg_flags & libc::MSG_CTRUNC != 0
            || unsafe {
                (*c).cmsg_level != libc::SOL_SOCKET
                    || (*c).cmsg_type != libc::SCM_RIGHTS
                    || (*c).cmsg_len != libc::CMSG_LEN(4) as usize
            }
        {
            return Err(io::Error::other("invalid listener transfer"));
        }
        Ok(unsafe {
            OwnedFd::from_raw_fd(std::ptr::read_unaligned(libc::CMSG_DATA(c).cast::<i32>()))
        })
    }
    fn tool_caps(tool: &str) -> Capabilities {
        let output = Command::new("/usr/bin/ldd")
            .arg(tool)
            .env_clear()
            .output()
            .unwrap();
        assert!(output.status.success());
        let mut caps = Capabilities::new();
        for path in std::iter::once(tool)
            .chain(std::iter::once("/etc/ld.so.cache"))
            .chain(
                std::str::from_utf8(&output.stdout)
                    .unwrap()
                    .split_whitespace()
                    .filter(|s| s.starts_with('/')),
            )
        {
            if !caps.leaves.contains_key(path.as_bytes()) {
                caps.insert(
                    path.as_bytes().to_vec(),
                    File::open(path).unwrap(),
                    Access::ReadOnly,
                )
                .unwrap();
            }
        }
        caps
    }
    fn run(root: &std::path::Path, tool: &str, args: &[&str], caps: Capabilities, cancel: bool) {
        run_mode(
            root,
            tool,
            args,
            caps,
            if cancel { Mode::Cancel } else { Mode::Normal },
        );
    }
    #[derive(Clone, Copy)]
    enum Mode {
        Normal,
        Cancel,
        StaleDelivery,
        ExitBeforeReceive,
    }
    fn run_mode(root: &std::path::Path, tool: &str, args: &[&str], caps: Capabilities, mode: Mode) {
        let image = File::open(tool).unwrap();
        let execfd = image.as_raw_fd();
        let mut sockets = [-1; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
                    0,
                    sockets.as_mut_ptr(),
                )
            },
            0
        );
        let parent = unsafe { OwnedFd::from_raw_fd(sockets[0]) };
        let child_socket = unsafe { OwnedFd::from_raw_fd(sockets[1]) };
        let socket = child_socket.as_raw_fd();
        let mut policy = linux::broker_bootstrap_filter(execfd, socket);
        let strings: Vec<_> = std::iter::once(tool)
            .chain(args.iter().copied())
            .map(|s| CString::new(s).unwrap())
            .collect();
        let mut pointers: Vec<usize> = strings.iter().map(|s| s.as_ptr() as usize).collect();
        pointers.push(0);
        let expected_parent = unsafe { libc::getpid() };
        let stdin = root.join("stdin");
        std::fs::write(&stdin, b"").unwrap();
        let stdout = root.join("stdout");
        let stderr = root.join("stderr");
        let mut cmd = Command::new("/never-used");
        cmd.current_dir(root)
            .env_clear()
            .stdin(File::open(stdin).unwrap())
            .stdout(File::create(&stdout).unwrap())
            .stderr(File::create(&stderr).unwrap());
        unsafe {
            cmd.pre_exec(move || {
                // Keep preallocated C strings alive without any child allocation.
                let _keep = &strings;
                if libc::getppid() != expected_parent
                    || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                    || libc::getppid() != expected_parent
                {
                    return Err(io::Error::other("parent changed"));
                }
                if libc::syscall(
                    libc::SYS_close_range,
                    3u32,
                    u32::MAX,
                    4u32, /* CLOEXEC */
                ) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                let listener = linux::install_listener(&mut policy)?;
                transfer(socket, listener)?;
                if libc::close(listener) != 0 || libc::close(socket) != 0 {
                    return Err(io::Error::last_os_error());
                }
                linux::finish_broker_bootstrap()?;
                let env = [std::ptr::null::<libc::c_char>()];
                libc::syscall(
                    libc::SYS_execveat,
                    execfd,
                    c"".as_ptr(),
                    pointers.as_ptr().cast::<*const libc::c_char>(),
                    env.as_ptr(),
                    libc::AT_EMPTY_PATH,
                );
                Err(io::Error::last_os_error())
            });
        }
        let mut child = FixtureChild(cmd.spawn().unwrap());
        drop(child_socket);
        let listener = receive(parent.as_raw_fd()).unwrap();
        drop(parent);
        // spawn() observes CLOEXEC error-pipe EOF: this memory fd is for the
        // executed image, not the old pre-exec address space.
        let memory = File::open(format!("/proc/{}/mem", child.id())).unwrap();
        let mut broker = Broker::new(listener, memory, child.id(), caps);
        broker.restricted_thread_origins = true;
        let (paused_tx, paused_rx) = std::sync::mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
        if matches!(mode, Mode::StaleDelivery | Mode::ExitBeforeReceive) {
            let hook: Box<dyn FnOnce() + Send> = Box::new(move || {
                paused_tx.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            });
            if matches!(mode, Mode::StaleDelivery) {
                broker.before_delivery = Some(hook);
            } else {
                broker.before_receive = Some(hook);
            }
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let result = broker.serve(&signal, Duration::from_secs(20));
            finished_tx
                .send((
                    result,
                    broker.stale_deliveries,
                    broker.capabilities.leaves.values().any(|l| l.opened),
                ))
                .unwrap();
        });
        let start = Instant::now();
        let mut killed = false;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if matches!(mode, Mode::StaleDelivery | Mode::ExitBeforeReceive)
                && paused_rx.try_recv().is_ok()
            {
                child.kill().unwrap();
                killed = true;
                let status = child.wait().unwrap();
                resume_tx.send(()).unwrap();
                break status;
            }
            if (matches!(mode, Mode::Cancel) && start.elapsed() > Duration::from_millis(300))
                || start.elapsed() > Duration::from_secs(15)
            {
                child.kill().unwrap();
                killed = true;
                break child.wait().unwrap();
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        cancelled.store(true, Ordering::Release);
        let (result, stale, any_opened) = finished_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("broker must return after exact child exit/cancellation");
        worker.join().unwrap();
        result.unwrap();
        if matches!(mode, Mode::StaleDelivery) {
            assert_eq!(stale, 1);
            assert!(
                !any_opened,
                "stale atomic delivery must not commit open state"
            );
        }
        let errors = std::fs::read_to_string(stderr).unwrap();
        if !matches!(mode, Mode::Normal) {
            assert!(
                killed && !status.success(),
                "cancellation not exercised: {errors}"
            );
        } else {
            assert!(!killed && status.success(), "{tool}: {status}: {errors}");
        }
    }
    #[test]
    #[ignore = "disabled deterministic notification lifetime races, no cleanup authority"]
    fn stale_delivery_and_exit_between_poll_and_receive_are_bounded() {
        for mode in [Mode::StaleDelivery, Mode::ExitBeforeReceive] {
            let root = tempfile::Builder::new()
                .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
                .tempdir()
                .unwrap();
            let source = root.path().join("race.c");
            let binary = root.path().join("race");
            std::fs::write(&source, b"#include <fcntl.h>\nint main(void){return open(\"out\",O_WRONLY|O_CREAT|O_TRUNC,0600)<0;}\n").unwrap();
            assert!(
                Command::new("/usr/bin/cc")
                    .args(["-static", "-O2"])
                    .arg(source)
                    .arg("-o")
                    .arg(&binary)
                    .status()
                    .unwrap()
                    .success()
            );
            let output = root.path().join("out");
            let mut caps = Capabilities::new();
            caps.create_private_output(b"out".to_vec(), &File::open(root.path()).unwrap(), c"out")
                .unwrap();
            run_mode(root.path(), binary.to_str().unwrap(), &[], caps, mode);
            assert_eq!(std::fs::metadata(output).unwrap().len(), 0);
        }
    }
    #[test]
    #[ignore = "disabled listener lineage/mutable pointer adversary, no cleanup authority"]
    fn sibling_threads_and_mutable_path_only_select_declared_capabilities() {
        let root = tempfile::Builder::new()
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir()
            .unwrap();
        let source = root.path().join("threads.c");
        let binary = root.path().join("threads");
        std::fs::write(&source,r#"
#define _GNU_SOURCE
#include <stdatomic.h>
#include <pthread.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
static _Atomic int stop=0;
static _Atomic char path[]={'/','v','i','r','t','u','a','l','/','s','o','u','r','c','e',0};
static void *thread(void *unused) {
 for(int i=0;i<32;i++){int f=open("/virtual/source",O_RDONLY);char c=0;if(f<0||read(f,&c,1)!=1||c!='a')return (void*)1;close(f);}
 while(!atomic_load(&stop)){atomic_store(&path[0],'!');atomic_store(&path[0],'/');}
 return 0;
}
int main(void){pthread_t t;if(pthread_create(&t,0,thread,0)!=0)return 10;
 for(int i=0;i<128;i++){int f=open((char*)path,O_RDONLY);if(f>=0){char c=0;if(read(f,&c,1)!=1||c!='a')return 11;close(f);}else if(errno!=EACCES)return 12;}
 atomic_store(&stop,1);void *result=0;if(pthread_join(t,&result)!=0||result)return 13;return 0;}
"#).unwrap();
        assert!(
            Command::new("/usr/bin/cc")
                .args(["-static", "-pthread", "-O2"])
                .arg(source)
                .arg("-o")
                .arg(&binary)
                .status()
                .unwrap()
                .success()
        );
        let input = root.path().join("source");
        std::fs::write(&input, b"abc").unwrap();
        let mut caps = Capabilities::new();
        caps.insert(
            b"/virtual/source".to_vec(),
            File::open(input).unwrap(),
            Access::ReadOnly,
        )
        .unwrap();
        run(root.path(), binary.to_str().unwrap(), &[], caps, false);
    }
    #[test]
    #[ignore = "disabled USER_NOTIF adversarial fixture, no cleanup authority"]
    fn broker_rejects_unknown_paths_rights_and_repeated_truncation() {
        let root = tempfile::Builder::new()
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir()
            .unwrap();
        let source = root.path().join("adversary.c");
        let binary = root.path().join("adversary");
        std::fs::write(&source, r#"
#define _GNU_SOURCE
#include <fcntl.h>
#include <errno.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <string.h>
int main(void) {
 for(int i=3;i<256;i++){errno=0;if(fcntl(i,F_GETFD)!=-1 || errno!=EBADF)return 10;}
 int a=open("/virtual/source",O_RDONLY),b=open("/virtual/source",O_RDONLY);char c=0;
 if(a<0||b<0||read(a,&c,1)!=1||c!='a'||read(b,&c,1)!=1||c!='a')return 11;
 close(a);close(b);
 if(open("/virtual/source",O_WRONLY)>=0 || open("/proc/self/mem",O_RDWR)>=0 || open("../source",O_RDONLY)>=0)return 12;
 if(openat(12345,"not-declared",O_RDONLY)>=0 || syscall(SYS_openat2,-100,"/virtual/source",0,0)!=-1 || errno!=ENOSYS)return 13;
 if(syscall(SYS_openat,-100,(void*)1,O_RDONLY,0)!=-1 || errno!=EFAULT)return 14;
 int out=open("out",O_WRONLY|O_CREAT|O_TRUNC,0600);if(out<0||write(out,"keep",4)!=4)return 15;close(out);
 if(open("out",O_WRONLY|O_TRUNC)>=0)return 16;
 for(int i=0;i<128;i++){int f=open("/virtual/source",O_RDONLY);if(f<0||read(f,&c,1)!=1||c!='a')return 17;close(f);}
 if(syscall(SYS_unlinkat,-100,"out",0)!=-1 || errno!=EPERM)return 18;
 return 0;
}
"#).unwrap();
        assert!(
            Command::new("/usr/bin/cc")
                .args(["-static", "-O2"])
                .arg(&source)
                .arg("-o")
                .arg(&binary)
                .status()
                .unwrap()
                .success()
        );
        let input = root.path().join("source");
        std::fs::write(&input, b"abc").unwrap();
        let output = root.path().join("out");
        let mut caps = Capabilities::new();
        caps.insert(
            b"/virtual/source".to_vec(),
            File::open(input).unwrap(),
            Access::ReadOnly,
        )
        .unwrap();
        caps.create_private_output(b"out".to_vec(), &File::open(root.path()).unwrap(), c"out")
            .unwrap();
        run(root.path(), binary.to_str().unwrap(), &[], caps, false);
        assert_eq!(std::fs::read(output).unwrap(), b"keep");
    }
    #[test]
    #[ignore = "disabled USER_NOTIF installed-tool compatibility, no cleanup authority"]
    fn actual_dynamic_loader_audio_video_probe_and_cancel_with_leaf_broker() {
        let root = tempfile::Builder::new()
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir()
            .unwrap();
        let mut wav = b"RIFF".to_vec();
        wav.extend(16036u32.to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(16000u32.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(16000u32.to_le_bytes());
        wav.resize(16044, 0);
        std::fs::write(root.path().join("input.wav"), wav).unwrap();
        let mut ppm = b"P6\n32 32\n255\n".to_vec();
        ppm.extend(vec![128; 3072]);
        std::fs::write(root.path().join("input.ppm"), ppm).unwrap();
        for (input, output, codec) in [
            ("input.wav", "audio.flac", "flac"),
            ("input.ppm", "video.mp4", "mpeg4"),
        ] {
            let mut caps = tool_caps("/usr/bin/ffmpeg");
            caps.insert(
                input.as_bytes().to_vec(),
                File::open(root.path().join(input)).unwrap(),
                Access::ReadOnly,
            )
            .unwrap();
            caps.create_private_output(
                output.as_bytes().to_vec(),
                &File::open(root.path()).unwrap(),
                &CString::new(output).unwrap(),
            )
            .unwrap();
            run(
                root.path(),
                "/usr/bin/ffmpeg",
                &[
                    "-nostdin",
                    "-v",
                    "error",
                    "-y",
                    "-i",
                    input,
                    "-threads",
                    "2",
                    "-filter_threads",
                    "2",
                    "-c",
                    codec,
                    output,
                ],
                caps,
                false,
            );
            let mut caps = tool_caps("/usr/bin/ffprobe");
            caps.insert(
                output.as_bytes().to_vec(),
                File::open(root.path().join(output)).unwrap(),
                Access::ReadOnly,
            )
            .unwrap();
            run(
                root.path(),
                "/usr/bin/ffprobe",
                &["-v", "error", "-show_streams", output],
                caps,
                false,
            );
        }
        let mut caps = tool_caps("/usr/bin/ffmpeg");
        caps.insert(
            b"input.wav".to_vec(),
            File::open(root.path().join("input.wav")).unwrap(),
            Access::ReadOnly,
        )
        .unwrap();
        caps.create_private_output(
            b"cancel.flac".to_vec(),
            &File::open(root.path()).unwrap(),
            c"cancel.flac",
        )
        .unwrap();
        run(
            root.path(),
            "/usr/bin/ffmpeg",
            &[
                "-nostdin",
                "-v",
                "error",
                "-y",
                "-re",
                "-stream_loop",
                "-1",
                "-i",
                "input.wav",
                "-c:a",
                "flac",
                "cancel.flac",
            ],
            caps,
            true,
        );
    }
}
