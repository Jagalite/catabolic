// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Bounded, shell-free Unix subprocesses with ownership of their process group.
//!
//! A command is not complete until its output pipes close and its leader exits.
//! Every exit path kills the group and reaps the leader, including callback
//! errors and unwinding. Descendants must not deliberately escape the session.
//!
//! Parent death: every tool group also contains a small guardian, forked in the
//! child after `setsid` and detached from the tool by a double fork. It holds
//! only the read end of a lifeline pipe whose write end the spawning worker
//! owns. When the worker exits for any reason (including SIGKILL) the kernel
//! closes that end, the guardian reads EOF and SIGKILLs the whole group, so the
//! direct tool and its grandchildren die on Linux and macOS alike. Normal
//! retirement kills the group, guardian included. Linux keeps its direct-tool
//! parent-death signal as an additional guarantee.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

thread_local! {
    static CANCELLATION: std::cell::RefCell<Option<std::sync::Arc<AtomicBool>>> = const { std::cell::RefCell::new(None) };
}
/// Compose caller cancellation with explicit command tokens, including helper
/// discovery commands. Scoped restoration also runs during unwinding.
pub fn with_cancellation<T>(cancel: std::sync::Arc<AtomicBool>, action: impl FnOnce() -> T) -> T {
    struct Restore(Option<std::sync::Arc<AtomicBool>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CANCELLATION.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(CANCELLATION.with(|slot| slot.replace(Some(cancel))));
    action()
}
fn caller_cancelled() -> bool {
    CANCELLATION.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Acquire))
    })
}

type Callback<'a> = &'a mut dyn FnMut() -> Result<(), CommandFailure>;

/// Limits and explicit inputs for one command. Environment entries replace the
/// inherited environment when `env` is present. No shell is implicitly invoked.
pub struct CommandOptions<'a> {
    /// Descriptors must remain valid and open throughout this call. Only these
    /// descriptors, plus stdin/stdout/stderr, survive exec in the child.
    pub pass_fds: &'a [RawFd],
    /// Child-only working directory, pinned by an open directory descriptor.
    /// It need not be in pass_fds and remains CLOEXEC unless explicitly passed.
    pub cwd_fd: Option<RawFd>,
    pub timeout: Duration,
    /// Combined stdout + stderr budget, also enforced when capture is disabled.
    pub maximum: usize,
    pub cancel: Option<&'a AtomicBool>,
    pub env: Option<&'a BTreeMap<OsString, OsString>>,
    pub input_bytes: Option<&'a [u8]>,
    /// False discards all diagnostics, including text in failure messages.
    pub capture: bool,
    pub umask: Option<u32>,
    /// Called only after spawn succeeds. Callbacks should be prompt and must
    /// not reap the command, since the runner owns its lifetime.
    pub on_start: Option<Callback<'a>>,
    /// Called at least once per poll iteration (normally every 50 ms or less).
    pub on_poll: Option<Callback<'a>>,
}

impl Default for CommandOptions<'_> {
    fn default() -> Self {
        Self {
            pass_fds: &[],
            cwd_fd: None,
            timeout: Duration::from_secs(20),
            maximum: 2 * 1024 * 1024,
            cancel: None,
            env: None,
            input_bytes: None,
            capture: true,
            umask: None,
            on_start: None,
            on_poll: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub returncode: i32,
}

impl CommandOutput {
    /// Matches the Python adapter's stdout, optionally followed by stderr.
    pub fn into_bytes(mut self, include_stderr: bool) -> Vec<u8> {
        if include_stderr {
            self.stdout.extend_from_slice(&self.stderr);
        }
        self.stdout
    }
}

#[derive(Debug)]
pub struct CommandFailure {
    pub state: String,
    pub message: String,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Exit status, or the negative terminating signal, when available.
    pub returncode: Option<i32>,
    /// Preserves missing executable, permission and other OS failure details.
    pub io_error: Option<io::Error>,
}

impl CommandFailure {
    pub fn new(state: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            state: state.into(),
            message: message.into(),
            stdout: Vec::new(),
            stderr: Vec::new(),
            returncode: None,
            io_error: None,
        }
    }

    fn from_io(context: &str, error: io::Error) -> Self {
        let state = if error.kind() == io::ErrorKind::NotFound {
            "unavailable"
        } else {
            "failed"
        };
        let mut failure = Self::new(state, format!("{context}: {error}"));
        failure.io_error = Some(error);
        failure
    }
}

impl fmt::Display for CommandFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.io_error.as_ref().map(|error| error as _)
    }
}

/// Execute a bounded command, retaining separate stdout/stderr diagnostics.
///
/// `partial`, `timeout`, `cancelled` and `failed` match Python command failures.
/// A missing executable is `unavailable` with its original `io_error` attached.
/// As with process creation itself, a blocking caller callback cannot be
/// preempted by the elapsed-time limit.
pub fn command_output<S: AsRef<OsStr>>(
    argv: &[S],
    mut options: CommandOptions<'_>,
) -> Result<CommandOutput, CommandFailure> {
    if crate::execution_cleanup::no_child_scope_active() {
        return Err(CommandFailure::new(
            "failed",
            "native builtin execution cannot spawn tools",
        ));
    }
    if caller_cancelled()
        || options
            .cancel
            .is_some_and(|cancel| cancel.load(Ordering::Acquire))
    {
        return Err(CommandFailure::new("cancelled", "command cancelled"));
    }
    let deadline = Instant::now().checked_add(options.timeout).ok_or_else(|| {
        CommandFailure::new("failed", "command timeout is outside the supported range")
    })?;
    let executable = argv
        .first()
        .filter(|value| !value.as_ref().is_empty())
        .ok_or_else(|| CommandFailure::new("failed", "command requires an executable"))?;
    if options.umask.is_some_and(|mask| mask & !0o777 != 0) {
        return Err(CommandFailure::new("failed", "invalid command umask"));
    }
    // Validate before spawn so an invalid descriptor cannot accidentally refer
    // to one of the pipes allocated by Command during process creation.
    for fd in options.pass_fds.iter().copied().chain(options.cwd_fd) {
        if fd < 0 {
            return Err(CommandFailure::from_io(
                "invalid inherited descriptor",
                io::Error::from_raw_os_error(libc::EBADF),
            ));
        }
        fd_flags(fd)
            .map_err(|error| CommandFailure::from_io("invalid inherited descriptor", error))?;
    }
    let fd_limit = descriptor_limit()
        .map_err(|error| CommandFailure::from_io("cannot determine descriptor limit", error))?;
    let pass_fds = options.pass_fds.to_vec();
    let umask = options.umask;
    let cwd_fd = options.cwd_fd;
    let (lifeline_read, lifeline_write) = lifeline()
        .map_err(|error| CommandFailure::from_io("cannot create parent-death lifeline", error))?;
    let guardian_fd = lifeline_read.as_raw_fd();
    #[cfg(target_os = "linux")]
    let parent_pid = std::process::id() as libc::pid_t;
    let mut command = Command::new(executable);
    command
        .args(&argv[1..])
        .stdin(if options.input_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(env) = options.env {
        command.env_clear().envs(env);
    }
    // SAFETY: the post-fork callback uses only async-signal-safe OS operations
    // and preallocated data. It does not lock, allocate, or access parent state.
    unsafe {
        command.pre_exec(move || {
            #[cfg(target_os = "linux")]
            {
                // Linux clears this across fork, so each actual tool installs
                // its own death signal. Verify the parent on both sides of the
                // syscall: a worker death before installation must fail closed.
                if libc::getppid() != parent_pid {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent_pid {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
            }
            if let Some(fd) = cwd_fd
                && libc::fchdir(fd) == -1
            {
                return Err(io::Error::last_os_error());
            }
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            spawn_guardian(guardian_fd, fd_limit)?;
            if let Some(mask) = umask {
                libc::umask(mask as libc::mode_t);
            }
            close_on_exec_all(fd_limit)?;
            for &fd in &pass_fds {
                let flags = fd_flags(fd)?;
                if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let child = command.spawn();
    // Only the guardian keeps the read end. On spawn failure the write end is
    // dropped too, so a guardian that was created reaps the failed group.
    drop(lifeline_read);
    let child = child.map_err(|error| CommandFailure::from_io("cannot start command", error))?;
    // Install the guard before any user callback or fallible operation.
    let mut process = ProcessGroup::new(child);
    process.lifeline = Some(lifeline_write);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = (|| {
        if let Some(callback) = options.on_start.as_mut() {
            callback()?;
        }
        pump(
            &mut process,
            &mut options,
            deadline,
            &mut stdout,
            &mut stderr,
        )
    })();
    let status = process.retire();
    if let Err(mut failure) = result {
        failure.stdout = stdout;
        failure.stderr = stderr;
        failure.returncode = status.ok().map(returncode);
        return Err(failure);
    }
    let status = status.map_err(|error| {
        let mut failure = CommandFailure::from_io("cannot reap command", error);
        failure.stdout = std::mem::take(&mut stdout);
        failure.stderr = std::mem::take(&mut stderr);
        failure
    })?;
    let code = returncode(status);
    if !status.success() {
        let detail: String = String::from_utf8_lossy(&stderr)
            .chars()
            .take(1000)
            .collect();
        let mut failure = CommandFailure::new(
            "failed",
            if detail.is_empty() {
                format!("command exit {code}")
            } else {
                format!("command exit {code}: {detail}")
            },
        );
        failure.stdout = stdout;
        failure.stderr = stderr;
        failure.returncode = Some(code);
        return Err(failure);
    }
    Ok(CommandOutput {
        stdout,
        stderr,
        returncode: code,
    })
}

fn pump(
    process: &mut ProcessGroup,
    options: &mut CommandOptions<'_>,
    deadline: Instant,
    stdout_buffer: &mut Vec<u8>,
    stderr_buffer: &mut Vec<u8>,
) -> Result<(), CommandFailure> {
    let mut stdin = process.child.stdin.take();
    let mut stdout = process.child.stdout.take();
    let mut stderr = process.child.stderr.take();
    for fd in [
        stdin.as_ref().map(AsRawFd::as_raw_fd),
        stdout.as_ref().map(AsRawFd::as_raw_fd),
        stderr.as_ref().map(AsRawFd::as_raw_fd),
    ]
    .into_iter()
    .flatten()
    {
        nonblocking(fd)
            .map_err(|error| CommandFailure::from_io("cannot configure command pipe", error))?;
    }
    let mut pending_input = options.input_bytes.unwrap_or_default();
    if pending_input.is_empty() {
        stdin = None;
    }
    let mut total = 0;
    loop {
        // Observe without reaping: the leader reserves the PID/PGID until the
        // group is killed, even if its descendants already closed their pipes.
        if process
            .exited()
            .map_err(|error| CommandFailure::from_io("cannot inspect command status", error))?
            && stdin.is_none()
            && stdout.is_none()
            && stderr.is_none()
        {
            return Ok(());
        }
        if let Some(callback) = options.on_poll.as_mut() {
            callback()?;
        }
        if caller_cancelled()
            || options
                .cancel
                .is_some_and(|cancel| cancel.load(Ordering::Acquire))
        {
            return Err(CommandFailure::new("cancelled", "command cancelled"));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(CommandFailure::new(
                "timeout",
                "command exceeded the elapsed-time limit",
            ));
        }
        let mut descriptors = [
            poll_descriptor(stdout.as_ref(), libc::POLLIN),
            poll_descriptor(stderr.as_ref(), libc::POLLIN),
            poll_descriptor(stdin.as_ref(), libc::POLLOUT),
        ];
        // Round up to avoid busy-spinning in the last sub-millisecond interval.
        let millis = remaining.as_millis().saturating_add(1).min(50) as i32;
        // SAFETY: descriptors points to three initialized pollfd values for the
        // duration of the call, and all nonnegative FDs are owned by this pump.
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), 3, millis) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(CommandFailure::from_io("cannot poll command pipes", error));
        }
        for descriptor in &descriptors {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(CommandFailure::from_io(
                    "invalid command pipe",
                    io::Error::from_raw_os_error(libc::EBADF),
                ));
            }
        }
        // Read one chunk from each ready pipe per iteration. An unbounded drain
        // could starve cancellation, stdin, stderr, and the elapsed-time check.
        if descriptors[0].revents != 0 {
            read_output(&mut stdout, stdout_buffer, &mut total, options)?;
        }
        if descriptors[1].revents != 0 {
            read_output(&mut stderr, stderr_buffer, &mut total, options)?;
        }
        if descriptors[2].revents != 0
            && let Some(pipe) = stdin.as_mut()
        {
            match pipe.write(&pending_input[..pending_input.len().min(4096)]) {
                Ok(count) => {
                    pending_input = &pending_input[count..];
                    if count == 0 || pending_input.is_empty() {
                        stdin = None;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => stdin = None,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => {
                    return Err(CommandFailure::from_io("cannot write command input", error));
                }
            }
        }
    }
}

fn read_output<T: Read>(
    pipe: &mut Option<T>,
    buffer: &mut Vec<u8>,
    total: &mut usize,
    options: &CommandOptions<'_>,
) -> Result<(), CommandFailure> {
    let Some(reader) = pipe.as_mut() else {
        return Ok(());
    };
    let mut chunk = [0u8; 65536];
    match reader.read(&mut chunk) {
        Ok(0) => *pipe = None,
        Ok(count) => {
            let accepted = count.min(options.maximum.saturating_sub(*total));
            if options.capture {
                buffer.extend_from_slice(&chunk[..accepted]);
            }
            *total += accepted;
            if accepted < count {
                return Err(CommandFailure::new(
                    "partial",
                    "command output exceeded the byte limit",
                ));
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) => {}
        Err(error) => return Err(CommandFailure::from_io("cannot read command output", error)),
    }
    Ok(())
}

fn poll_descriptor<T: AsRawFd>(pipe: Option<&T>, events: i16) -> libc::pollfd {
    libc::pollfd {
        fd: pipe.map_or(-1, AsRawFd::as_raw_fd),
        events,
        revents: 0,
    }
}

fn fd_flags(fd: RawFd) -> io::Result<i32> {
    // SAFETY: F_GETFD only reads kernel metadata for the supplied descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(flags)
    }
}

fn nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: the descriptor is an owned pipe; these calls change its OS flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags == -1 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

fn descriptor_limit() -> io::Result<RawFd> {
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: getrlimit initializes the writable rlimit on success.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: getrlimit succeeded immediately above.
    let limit = unsafe { limit.assume_init() };
    // macOS commonly has an infinite hard limit, so iterating rlim_max can
    // stall process creation for minutes. The soft limit bounds new FDs; the
    // snapshot additionally includes FDs above a previously lowered limit.
    // All allocation/directory iteration happens before fork. Applications
    // must not concurrently raise their process-wide descriptor limit.
    let directory = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    match std::fs::read_dir(directory) {
        Ok(entries) => {
            let mut ceiling = limit.rlim_cur.min(i32::MAX as libc::rlim_t) as RawFd;
            for entry in entries {
                if let Some(fd) = entry?
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<RawFd>().ok())
                {
                    ceiling = ceiling.max(fd.saturating_add(1));
                }
            }
            Ok(ceiling)
        }
        // A finite hard limit remains a safe fallback when /proc or /dev/fd
        // is not mounted. Never silently allow unrelated FD inheritance.
        Err(_) if limit.rlim_max != libc::RLIM_INFINITY => {
            Ok(limit.rlim_max.min(i32::MAX as libc::rlim_t) as RawFd)
        }
        Err(error) => Err(error),
    }
}

fn close_on_exec_all(limit: RawFd) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // Mark rather than close: Rust's private exec-error pipe must remain
        // open until exec. CLOEXEC also covers FDs above a lowered rlimit.
        // SAFETY: close_range with CLOEXEC only updates this forked child's FDs.
        if unsafe {
            libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        } == 0
        {
            return Ok(());
        }
        // Older kernels and restricted syscall profiles use the portable path.
    }
    for fd in 3..limit {
        match fd_flags(fd) {
            Ok(flags) => {
                // SAFETY: sets close-on-exec on an existing child descriptor.
                if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            Err(error) if error.raw_os_error() == Some(libc::EBADF) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn returncode(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| -status.signal().unwrap_or(1))
}

struct ProcessGroup {
    child: Child,
    retired: bool,
    /// Write end of the guardian lifeline. Field drop follows `Drop::drop`, so
    /// it closes only after the group has been killed and its leader reaped.
    lifeline: Option<OwnedFd>,
}

/// A close-on-exec pipe: (read end for the guardian, write end for the worker).
fn lifeline() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds: [libc::c_int; 2] = [-1; 2];
    // SAFETY: fds is a writable two-element descriptor array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe returned two new descriptors owned only by this function.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&read, &write] {
        let flags = fd_flags(fd.as_raw_fd())?;
        // SAFETY: sets close-on-exec on a descriptor owned above.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((read, write))
}

/// Runs in the forked tool child after `setsid`, so the guardian joins the new
/// group. A double fork reparents it away from the tool, which therefore never
/// sees an unexpected child. Only async-signal-safe system calls are used.
///
/// # Safety
/// Must be called only between fork and exec of a group leader.
unsafe fn spawn_guardian(lifeline: RawFd, fd_limit: RawFd) -> io::Result<()> {
    // SAFETY: fork, read, kill, close, waitpid and _exit are async-signal-safe;
    // no memory is allocated and no lock is taken in either new process.
    unsafe {
        let first = libc::fork();
        if first == -1 {
            return Err(io::Error::last_os_error());
        }
        if first == 0 {
            let second = libc::fork();
            if second != 0 {
                libc::_exit(if second == -1 { 1 } else { 0 });
            }
            // Guardian: keep only the lifeline, so no output pipe, exec-error
            // pipe or the worker's write end stays open through this process.
            for fd in 0..fd_limit {
                if fd != lifeline {
                    libc::close(fd);
                }
            }
            let mut byte = 0u8;
            loop {
                let read = libc::read(lifeline, (&raw mut byte).cast(), 1);
                if read == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                break;
            }
            // EOF: the worker is gone or retired this group. Kill every member,
            // this guardian included.
            libc::kill(0, libc::SIGKILL);
            libc::_exit(0);
        }
        let mut status = 0;
        loop {
            if libc::waitpid(first, &mut status, 0) == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(error);
            }
            break;
        }
        if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
            return Err(io::Error::from_raw_os_error(libc::EAGAIN));
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", test))]
fn only_leader_in_snapshot(
    bytes: libc::c_int,
    members: &[libc::pid_t; 2],
    leader: libc::pid_t,
) -> bool {
    bytes == std::mem::size_of::<libc::pid_t>() as libc::c_int && members[0] == leader
}

impl ProcessGroup {
    fn new(child: Child) -> Self {
        Self {
            child,
            retired: false,
            lifeline: None,
        }
    }

    fn exited(&self) -> io::Result<bool> {
        // Zeroed siginfo lets a WNOHANG call indicate no waitable child.
        // SAFETY: an all-zero siginfo_t is a valid kernel output buffer.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: this child belongs to the runner, info is writable, and
        // WNOWAIT deliberately leaves its status reserved for Child::wait.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(error);
        }
        // SAFETY: waitid initialized the signal information above.
        Ok(unsafe { info.si_pid() } != 0)
    }

    fn retire(&mut self) -> io::Result<ExitStatus> {
        self.retire_with(|pgid| {
            // SAFETY: setsid made the unreaped leader PID our private PGID. It
            // cannot be reused until the following wait reaps that leader.
            if unsafe { libc::killpg(pgid, libc::SIGKILL) } == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })
    }

    fn retire_with(
        &mut self,
        kill_group: impl FnOnce(libc::pid_t) -> io::Result<()>,
    ) -> io::Result<ExitStatus> {
        if !self.retired
            && let Err(error) = kill_group(self.child.id() as libc::pid_t)
            && !self.finished_group_error(&error)
        {
            // Preserve a real signalling failure, but do not leave an
            // already-exited leader as a zombie. Never block waiting
            // for a live process whose termination was denied.
            if self.exited().unwrap_or(false) {
                let _ = self.reap();
            }
            return Err(error);
        }
        self.reap()
    }

    fn finished_group_error(&self, error: &io::Error) -> bool {
        if error.raw_os_error() == Some(libc::ESRCH) {
            return true;
        }
        #[cfg(target_os = "macos")]
        if error.raw_os_error() == Some(libc::EPERM) && self.exited().unwrap_or(false) {
            // XNU killpg filters zombies and reports EPERM when none of the
            // remaining group members can be signalled. Leader exit alone
            // cannot distinguish that from an unsignalable live descendant.
            // The locked kernel snapshot must contain only our pinned leader;
            // extra members, truncation and query failures all fail closed.
            const PROC_PGRP_ONLY: u32 = 2;
            let mut members: [libc::pid_t; 2] = [0; 2];
            // SAFETY: members is a writable PID array and the unreaped child
            // reserves the queried group identity throughout this snapshot.
            let bytes = unsafe {
                libc::proc_listpids(
                    PROC_PGRP_ONLY,
                    self.child.id(),
                    members.as_mut_ptr().cast(),
                    std::mem::size_of_val(&members) as libc::c_int,
                )
            };
            return only_leader_in_snapshot(bytes, &members, self.child.id() as libc::pid_t);
        }
        false
    }

    fn reap(&mut self) -> io::Result<ExitStatus> {
        loop {
            match self.child.wait() {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => {
                    self.retired = true;
                    return result;
                }
            }
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if !self.retired {
            let _ = self.retire();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Seek, SeekFrom};
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    fn shell(script: &str, options: CommandOptions<'_>) -> Result<CommandOutput, CommandFailure> {
        command_output(&["/bin/sh", "-c", script], options)
    }

    fn isolated_group(script: &str) -> ProcessGroup {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid is async-signal-safe and only changes this fixture
        // child's group. No parent group can be targeted by its cleanup.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        ProcessGroup::new(command.spawn().unwrap())
    }

    fn await_leader_exit(group: &ProcessGroup) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !group.exited().unwrap() {
            assert!(Instant::now() < deadline, "fixture leader failed to exit");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn successful_command_reaps_its_exited_group_leader() {
        let output = shell("printf '%s' \"$$\"; exit 0", CommandOptions::default()).unwrap();
        assert_eq!(output.returncode, 0);
        let pid = std::str::from_utf8(&output.stdout)
            .unwrap()
            .parse()
            .unwrap();
        assert_reaped(pid);
    }

    #[test]
    fn group_snapshot_requires_exactly_the_owned_leader() {
        let width = std::mem::size_of::<libc::pid_t>() as libc::c_int;
        assert!(only_leader_in_snapshot(width, &[42, 0], 42));
        assert!(!only_leader_in_snapshot(0, &[42, 0], 42));
        assert!(!only_leader_in_snapshot(-1, &[42, 0], 42));
        assert!(!only_leader_in_snapshot(width - 1, &[42, 0], 42));
        assert!(!only_leader_in_snapshot(width, &[43, 0], 42));
        assert!(!only_leader_in_snapshot(width * 2, &[42, 43], 42));
    }

    #[test]
    fn permission_denial_for_a_live_group_is_preserved() {
        let mut group = isolated_group("exec /bin/sleep 30");
        let pid = group.child.id() as libc::pid_t;
        let error = group
            .retire_with(|_| Err(io::Error::from_raw_os_error(libc::EPERM)))
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
        assert!(!group.retired);
        assert!(!group.exited().unwrap());
        group.retire().unwrap();
        assert_reaped(pid);
    }

    #[test]
    fn signalling_failure_still_reaps_an_exited_leader() {
        let mut group = isolated_group("exit 0");
        let pid = group.child.id() as libc::pid_t;
        await_leader_exit(&group);
        let error = group
            .retire_with(|_| Err(io::Error::from_raw_os_error(libc::EACCES)))
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
        assert_reaped(pid);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_permission_denial_is_not_excused_with_a_surviving_descendant() {
        let mut group = isolated_group("/bin/sleep 30 >/dev/null 2>&1 & exit 0");
        await_leader_exit(&group);
        assert!(!group.finished_group_error(&io::Error::from_raw_os_error(libc::EPERM)));
        group.retire().unwrap();
    }

    #[test]
    fn separate_output_and_combined_output() {
        let output = shell("printf out; printf err >&2", CommandOptions::default()).unwrap();
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
        assert_eq!(output.returncode, 0);
        assert_eq!(output.into_bytes(true), b"outerr");
    }

    #[test]
    fn literal_arguments_never_use_an_implicit_shell() {
        let output = command_output(
            &["/bin/echo", "$(printf injected); *; 'quoted'"],
            CommandOptions::default(),
        )
        .unwrap();
        assert_eq!(output.stdout, b"$(printf injected); *; 'quoted'\n");
    }

    #[test]
    fn large_binary_input_uses_a_pipe_without_deadlock() {
        let input: Vec<u8> = (0..1024 * 1024).map(|value| value as u8).collect();
        let output = shell(
            "cat",
            CommandOptions {
                input_bytes: Some(&input),
                ..CommandOptions::default()
            },
        )
        .unwrap();
        assert_eq!(output.stdout, input);
    }

    #[test]
    fn simultaneous_input_and_both_outputs_are_drained() {
        let input = vec![b'i'; 256 * 1024];
        let output = shell(
            "dd if=/dev/zero bs=65536 count=4 2>/dev/null; dd if=/dev/zero bs=65536 count=4 >&2 2>/dev/null; cat",
            CommandOptions { input_bytes: Some(&input), ..CommandOptions::default() },
        ).unwrap();
        assert_eq!(&output.stdout[..256 * 1024], vec![0; 256 * 1024]);
        assert_eq!(&output.stdout[256 * 1024..], input);
        assert_eq!(output.stderr, vec![0; 256 * 1024]);
    }

    #[test]
    fn empty_and_absent_stdin_both_reach_eof() {
        for input in [None, Some(&[][..])] {
            let output = shell(
                "cat; printf eof",
                CommandOptions {
                    input_bytes: input,
                    ..CommandOptions::default()
                },
            )
            .unwrap();
            assert_eq!(output.stdout, b"eof");
        }
    }

    #[test]
    fn child_closing_input_early_is_not_an_error() {
        let input = vec![0; 1024 * 1024];
        let output = shell(
            "exec 0<&-; printf done",
            CommandOptions {
                input_bytes: Some(&input),
                ..CommandOptions::default()
            },
        )
        .unwrap();
        assert_eq!(output.stdout, b"done");
    }

    #[test]
    fn combined_cap_includes_stderr_and_retains_bounded_diagnostics() {
        let error = shell(
            "printf 1234; printf 5678 >&2",
            CommandOptions {
                maximum: 7,
                ..CommandOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.state, "partial");
        assert_eq!(error.stdout.len() + error.stderr.len(), 7);
    }

    #[test]
    fn exact_and_zero_output_budgets_are_allowed() {
        let output = shell(
            "printf 1234; printf 5678 >&2",
            CommandOptions {
                maximum: 8,
                ..CommandOptions::default()
            },
        )
        .unwrap();
        assert_eq!(output.stdout.len() + output.stderr.len(), 8);
        shell(
            "exit 0",
            CommandOptions {
                maximum: 0,
                ..CommandOptions::default()
            },
        )
        .unwrap();
        let error = shell(
            "printf x",
            CommandOptions {
                maximum: 0,
                ..CommandOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.state, "partial");
        assert!(error.stdout.is_empty() && error.stderr.is_empty());
    }

    #[test]
    fn discarded_output_is_still_bounded_and_never_leaks() {
        let error = shell(
            "while :; do printf secret >&2; done",
            CommandOptions {
                maximum: 100,
                capture: false,
                ..CommandOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.state, "partial");
        assert!(error.stdout.is_empty() && error.stderr.is_empty());
        let error = shell(
            "printf secret >&2; exit 7",
            CommandOptions {
                capture: false,
                ..CommandOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.returncode, Some(7));
        assert!(!error.to_string().contains("secret"));
    }

    #[test]
    fn timeout_reaps_the_leader() {
        let start = Instant::now();
        let error = shell(
            "printf '%s\\n' \"$$\"; sleep 30",
            CommandOptions {
                // The inherited descriptor limit can require substantial
                // pre-exec work; allow startup before testing timeout/reaping.
                timeout: Duration::from_secs(1),
                ..CommandOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.state, "timeout");
        assert!(start.elapsed() < Duration::from_secs(3));
        let pid: libc::pid_t = std::str::from_utf8(&error.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_reaped(pid);
    }

    #[test]
    fn cancellation_retires_a_running_command() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("pid");
        let cancel = AtomicBool::new(false);
        let mut pid: Option<libc::pid_t> = None;
        let mut on_poll = || {
            let raw = match fs::read_to_string(&pid_file) {
                Ok(raw) => raw,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => {
                    return Err(CommandFailure::from_io("cannot read fixture PID", error));
                }
            };
            // A terminating newline proves the shell published its full PID.
            // The command's deadline bounds readiness; never cancel on a timer
            // that may expire during inherited-descriptor setup before exec.
            if let Some(raw) = raw.strip_suffix('\n') {
                pid = Some(raw.parse().map_err(|error| {
                    CommandFailure::new("failed", format!("invalid fixture PID: {error}"))
                })?);
                cancel.store(true, Ordering::Release);
            }
            Ok(())
        };
        let argv = [
            OsStr::new("/bin/sh"),
            OsStr::new("-c"),
            OsStr::new("printf '%s\\n' \"$$\" > \"$1\"; sleep 30"),
            OsStr::new("fixture"),
            pid_file.as_os_str(),
        ];
        let start = Instant::now();
        let error = command_output(
            &argv,
            CommandOptions {
                cancel: Some(&cancel),
                on_poll: Some(&mut on_poll),
                timeout: Duration::from_secs(3),
                ..CommandOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.state, "cancelled");
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_reaped(pid.expect("running fixture must publish its PID before cancellation"));
    }

    #[test]
    fn closed_output_does_not_bypass_deadline() {
        let error = shell(
            "exec 1>&- 2>&-; sleep 30",
            CommandOptions {
                timeout: Duration::from_millis(100),
                ..CommandOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.state, "timeout");
    }

    #[test]
    fn stalled_input_and_continuous_output_cannot_starve_deadline() {
        let input = vec![b'x'; 1024 * 1024];
        for script in [
            "sleep 30",
            "while :; do printf xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done",
        ] {
            let start = Instant::now();
            let error = shell(
                script,
                CommandOptions {
                    input_bytes: Some(&input),
                    timeout: Duration::from_millis(100),
                    maximum: usize::MAX,
                    capture: false,
                    ..CommandOptions::default()
                },
            )
            .unwrap_err();
            assert_eq!(error.state, "timeout");
            assert!(start.elapsed() < Duration::from_secs(2));
        }
    }

    #[test]
    fn failure_exposes_exit_and_bounded_utf8_diagnostics() {
        let error = shell(
            "printf out; printf 'error\\377' >&2; exit 7",
            CommandOptions::default(),
        )
        .unwrap_err();
        assert_eq!(error.state, "failed");
        assert_eq!(error.returncode, Some(7));
        assert_eq!(error.stdout, b"out");
        assert_eq!(error.stderr, b"error\xff");
        assert!(error.message.contains("command exit 7: error"));
        assert!(error.message.contains('\u{fffd}'));
        let error = shell("kill -TERM $$", CommandOptions::default()).unwrap_err();
        assert_eq!(error.returncode, Some(-libc::SIGTERM));
    }

    #[test]
    fn missing_executable_is_an_io_error_and_never_calls_start() {
        let starts = AtomicUsize::new(0);
        let mut on_start = || {
            starts.fetch_add(1, Ordering::Relaxed);
            Ok(())
        };
        let directory = tempfile::tempdir().unwrap();
        let error = command_output(
            &[directory.path().join("absent-command")],
            CommandOptions {
                on_start: Some(&mut on_start),
                ..CommandOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.state, "unavailable");
        assert_eq!(error.io_error.unwrap().kind(), io::ErrorKind::NotFound);
        assert_eq!(starts.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn callbacks_can_abort_and_always_retire_the_command() {
        let starts = AtomicUsize::new(0);
        let polls = AtomicUsize::new(0);
        let mut on_start = || {
            starts.fetch_add(1, Ordering::Relaxed);
            Ok(())
        };
        let mut on_poll = || {
            if polls.fetch_add(1, Ordering::Relaxed) >= 2 {
                Err(CommandFailure::new("cancelled", "lease expired"))
            } else {
                Ok(())
            }
        };
        let error = shell(
            "sleep 30",
            CommandOptions {
                on_start: Some(&mut on_start),
                on_poll: Some(&mut on_poll),
                ..CommandOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.message, "lease expired");
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        assert_eq!(polls.load(Ordering::Relaxed), 3);
        assert_eq!(error.returncode, Some(-libc::SIGKILL));
    }

    #[test]
    fn callback_panic_also_kills_and_reaps() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("pid");
        let mut on_poll = || -> Result<(), CommandFailure> {
            if pid_file.exists() {
                panic!("test callback panic");
            }
            Ok(())
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let argv = [
                OsStr::new("/bin/sh"),
                OsStr::new("-c"),
                OsStr::new("printf '%s' \"$$\" > \"$1\"; sleep 30"),
                OsStr::new("fixture"),
                pid_file.as_os_str(),
            ];
            command_output(
                &argv,
                CommandOptions {
                    on_poll: Some(&mut on_poll),
                    ..CommandOptions::default()
                },
            )
        }));
        assert!(result.is_err());
        let pid = fs::read_to_string(pid_file).unwrap().parse().unwrap();
        assert_reaped(pid);
    }

    #[test]
    fn descendants_are_retired_after_leader_exit_with_or_without_pipes() {
        for retain_pipes in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let heartbeat = directory.path().join("heartbeat");
            let redirect = if retain_pipes { "" } else { ">/dev/null 2>&1" };
            let script = format!(
                "(while :; do printf x >> \"$1\"; sleep 0.02; done) {redirect} & \
                 while [ ! -s \"$1\" ]; do :; done; exit 0"
            );
            let argv = [
                OsStr::new("/bin/sh"),
                OsStr::new("-c"),
                script.as_ref(),
                OsStr::new("fixture"),
                heartbeat.as_os_str(),
            ];
            let result = command_output(
                &argv,
                CommandOptions {
                    // Parallel platform runs can spend more than 250 ms just
                    // starting the shell. Exercise a running descendant, not
                    // a timeout before the first heartbeat exists.
                    timeout: Duration::from_secs(5),
                    ..CommandOptions::default()
                },
            );
            if retain_pipes {
                assert_eq!(result.unwrap_err().state, "timeout");
            } else {
                assert_eq!(result.unwrap().returncode, 0);
            }
            // A heartbeat distinguishes a running descendant from a briefly
            // unreaped orphan. Only its actual parent/OS reaper can reap it.
            thread::sleep(Duration::from_millis(50));
            let stopped = fs::read(&heartbeat).unwrap();
            thread::sleep(Duration::from_millis(100));
            assert_eq!(fs::read(&heartbeat).unwrap(), stopped);
        }
    }

    #[test]
    fn explicit_fd_inheritance_keeps_parent_flags_unchanged() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"source-descriptor").unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let fd = file.as_raw_fd();
        let before = fd_flags(fd).unwrap();
        assert_ne!(before & libc::FD_CLOEXEC, 0);
        let output = shell(
            &format!("cat /dev/fd/{fd}"),
            CommandOptions {
                pass_fds: &[fd],
                ..CommandOptions::default()
            },
        )
        .unwrap();
        assert_eq!(output.stdout, b"source-descriptor");
        assert_eq!(fd_flags(fd).unwrap(), before);
    }

    #[test]
    fn invalid_pass_fd_is_rejected_before_pipe_allocation() {
        let error = shell(
            "exit 0",
            CommandOptions {
                pass_fds: &[-1],
                ..CommandOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.io_error.unwrap().raw_os_error(), Some(libc::EBADF));
    }

    #[test]
    fn unrequested_inheritable_descriptor_is_closed() {
        let file = tempfile::tempfile().unwrap();
        let fd = file.as_raw_fd();
        // SAFETY: this exclusively owned fixture descriptor is made inheritable
        // only to verify that the runner closes unrelated FDs during exec.
        assert_ne!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, -1);
        let output = shell(
            &format!("if [ -e /dev/fd/{fd} ]; then exit 9; fi; printf closed"),
            CommandOptions::default(),
        )
        .unwrap();
        assert_eq!(output.stdout, b"closed");
    }

    #[test]
    fn private_environment_and_umask_are_child_only() {
        let env = BTreeMap::from([(OsString::from("CATABOLIC_FIXTURE"), OsString::from("value"))]);
        let output = shell(
            "printf '%s:' \"$CATABOLIC_FIXTURE\"; umask",
            CommandOptions {
                env: Some(&env),
                umask: Some(0o077),
                ..CommandOptions::default()
            },
        )
        .unwrap();
        let text = std::str::from_utf8(&output.stdout).unwrap();
        assert!(text == "value:0077\n" || text == "value:077\n");
    }

    #[test]
    fn descriptor_working_directory_is_child_only_and_survives_rename() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        fs::create_dir(&original).unwrap();
        fs::write(original.join("marker"), b"pinned").unwrap();
        let root = fs::File::open(&original).unwrap();
        fs::rename(&original, dir.path().join("renamed")).unwrap();
        fs::create_dir(&original).unwrap();
        fs::write(original.join("marker"), b"replaced").unwrap();
        let before = std::env::current_dir().unwrap();
        let output = command_output(
            &["/bin/cat", "marker"],
            CommandOptions {
                cwd_fd: Some(root.as_raw_fd()),
                ..CommandOptions::default()
            },
        )
        .unwrap();
        assert_eq!(output.stdout, b"pinned");
        assert_eq!(std::env::current_dir().unwrap(), before);
        assert!(
            command_output(
                &["/usr/bin/true"],
                CommandOptions {
                    cwd_fd: Some(-1),
                    ..CommandOptions::default()
                }
            )
            .is_err()
        );
        let file = tempfile::tempfile().unwrap();
        assert_eq!(
            command_output(
                &["/usr/bin/true"],
                CommandOptions {
                    cwd_fd: Some(file.as_raw_fd()),
                    ..CommandOptions::default()
                }
            )
            .unwrap_err()
            .io_error
            .unwrap()
            .raw_os_error(),
            Some(libc::ENOTDIR)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn direct_tool_is_killed_when_worker_process_dies() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("tool.pid");
        // Re-exec the running test image without creating a writable executable.
        // A concurrent fork can retain a copy's write descriptor until exec and
        // make the fixture's spawn fail with ETXTBSY, even with FD_CLOEXEC set.
        let mut worker = Command::new("/proc/self/exe")
            .args([
                "--exact",
                "process::tests::parent_death_fixture_worker",
                "--nocapture",
            ])
            .env("CATABOLIC_PARENT_DEATH_FIXTURE", &pid_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            if let Ok(raw) = fs::read_to_string(&pid_path)
                && let Ok(pid) = raw.trim().parse::<i32>()
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "tool failed to start");
            thread::sleep(Duration::from_millis(10));
        };
        fn start_time(raw: &str) -> Option<&str> {
            raw.rsplit_once(") ")?.1.split_whitespace().nth(19)
        }
        let original_stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let identity = start_time(&original_stat).unwrap();
        worker.kill().unwrap();
        worker.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let stat = fs::read_to_string(format!("/proc/{pid}/stat"));
            if stat
                .as_ref()
                .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
                || stat.as_ref().is_ok_and(|s| {
                    start_time(s) != Some(identity)
                        || s.rsplit_once(") ")
                            .is_some_and(|(_, rest)| rest.starts_with('Z'))
                })
            {
                break;
            }
            if Instant::now() >= deadline {
                // SAFETY: pid came from our disposable live fixture; this is
                // failure-path cleanup after proving that it did not exit.
                if stat.as_ref().is_ok_and(|s| start_time(s) == Some(identity)) {
                    // SAFETY: the current process still has the exact start
                    // identity captured for our fixture, preventing PID reuse.
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
                panic!("orphan direct tool survived worker death");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parent_death_fixture_worker() {
        let Some(path) = std::env::var_os("CATABOLIC_PARENT_DEATH_FIXTURE") else {
            return;
        };
        // The shell execs sleep, preserving the direct child's PID and installed
        // parent-death signal. No grandchildren or shell supervisor are needed.
        command_output(
            &[
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from("echo $$ > \"$1\"; exec /bin/sleep 60"),
                OsString::from("fixture"),
                path,
            ],
            CommandOptions {
                timeout: Duration::from_secs(70),
                ..CommandOptions::default()
            },
        )
        .unwrap();
        panic!("worker fixture unexpectedly completed");
    }

    /// Portable: the worker is SIGKILLed while its tool and a grandchild in
    /// the tool's group are alive. The guardian must kill both.
    #[test]
    fn tool_and_grandchild_are_killed_when_worker_process_dies() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("pids");
        let mut worker = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process::tests::guardian_fixture_worker",
                "--nocapture",
            ])
            .env("CATABOLIC_GUARDIAN_FIXTURE", &pid_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let pids: Vec<libc::pid_t> = loop {
            if let Ok(raw) = fs::read_to_string(&pid_path)
                && raw.ends_with('\n')
                && let Ok(pids) = raw
                    .split_whitespace()
                    .map(str::parse)
                    .collect::<Result<Vec<_>, _>>()
                && pids.len() == 2
            {
                break pids;
            }
            assert!(Instant::now() < deadline, "tool failed to start");
            thread::sleep(Duration::from_millis(10));
        };
        let group = pids[0];
        // Both processes are alive members of the tool's private group.
        for &pid in &pids {
            // SAFETY: getpgid only queries the named fixture process.
            assert_eq!(unsafe { libc::getpgid(pid) }, group);
        }
        worker.kill().unwrap();
        worker.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            // A killed member may linger briefly as a zombie of the system
            // reaper; it is then no longer a live member of the group.
            // SAFETY: signal 0 only probes the fixture group for live members.
            let alive = unsafe { libc::kill(-group, 0) } == 0;
            if !alive {
                break;
            }
            if Instant::now() >= deadline {
                // SAFETY: failure-path cleanup of our own fixture group.
                unsafe {
                    libc::kill(-group, libc::SIGKILL);
                }
                panic!("orphan tool group survived worker death");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn guardian_fixture_worker() {
        let Some(path) = std::env::var_os("CATABOLIC_GUARDIAN_FIXTURE") else {
            return;
        };
        // The tool starts a grandchild in its own group, records both PIDs
        // atomically (one write ending in a newline), then waits.
        command_output(
            &[
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from(
                    "/bin/sleep 60 & child=$!; printf '%s %s\\n' $$ $child > \"$1.tmp\"; mv \"$1.tmp\" \"$1\"; wait",
                ),
                OsString::from("fixture"),
                path,
            ],
            CommandOptions {
                timeout: Duration::from_secs(70),
                ..CommandOptions::default()
            },
        )
        .unwrap();
        panic!("worker fixture unexpectedly completed");
    }

    #[test]
    fn guardian_leaves_no_process_after_normal_and_failed_commands() {
        // Normal retirement kills the group, guardian included.
        let output =
            command_output(&["/bin/sh", "-c", "echo $$"], CommandOptions::default()).unwrap();
        let group: libc::pid_t = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        // SAFETY: signal 0 only probes the retired fixture group.
        while unsafe { libc::kill(-group, 0) } == 0 {
            assert!(Instant::now() < deadline, "guardian outlived retirement");
            thread::sleep(Duration::from_millis(10));
        }
        // A missing executable still reports the original spawn failure.
        let failure = command_output(&["/nonexistent/catabolic-tool"], CommandOptions::default())
            .unwrap_err();
        assert_eq!(failure.state, "unavailable");
    }

    fn assert_reaped(pid: libc::pid_t) {
        let mut status = 0;
        // SAFETY: WNOHANG queries only the known fixture child and cannot block.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        assert_eq!(result, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
