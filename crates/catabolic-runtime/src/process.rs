// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Bounded, shell-free Unix subprocesses with ownership of their process group.
//!
//! A command is not complete until its output pipes close and its leader exits.
//! Every exit path kills the group and reaps the leader, including callback
//! errors and unwinding. Descendants must not deliberately escape the session.
//!
//! Parent death: when a supervisor executable is available (every Catabolic
//! binary configures itself; embedders use the registered native helper), the
//! worker execs that supervisor in place of the tool. Its post-fork callback
//! contains only async-signal-safe calls (`fchdir`, `setsid`, `umask`, `fcntl`,
//! and Linux `prctl`/`getppid`), so the tool runs in a new session with no
//! controlling terminal, as frozen Python's `start_new_session=True` does. In
//! that fresh process image the supervisor forks a detached guardian into the
//! same session and group. The guardian ignores the job-control and
//! termination signals (an orphaned stopped group receives SIGHUP then
//! SIGCONT), closes every descriptor except a lifeline pipe whose write end the
//! worker owns, and reports readiness. Only then does the supervisor exec the
//! tool, keeping its PID, so the worker still observes the tool's exact exit
//! status and output. When the worker exits for any reason, including SIGKILL,
//! the guardian reads EOF on the lifeline and SIGKILLs the whole group: the
//! tool and its grandchildren die on Linux and macOS alike. Normal retirement
//! kills the group, guardian included. Linux keeps its direct-tool parent-death
//! signal as an additional guarantee. Without a supervisor executable the tool
//! is execed directly with the same session handling, and only Linux has the
//! parent-death signal; `parent_death_supervised` reports which applies.
//!
//! Lifeline and exec-error pipes are close-on-exec from creation on Linux
//! (`pipe2`). On macOS they are created under the exclusive side of
//! `catabolic_core::spawn_guard`, which every in-workspace spawner holds
//! shared while forking, and both ends are relocated above the standard
//! descriptors.

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
    let mut descriptors = DescriptorScan::new()
        .map_err(|error| CommandFailure::from_io("cannot determine descriptor limit", error))?;
    let pass_fds = options.pass_fds.to_vec();
    let umask = options.umask;
    let cwd_fd = options.cwd_fd;
    let supervisor = supervisor().map_err(|error| {
        CommandFailure::new(
            "failed",
            format!("parent-death supervisor unavailable: {error}"),
        )
    })?;
    // Lifeline: the guardian reads it; the worker keeps the write end open for
    // its own lifetime. Errors: the supervisor reports a failed tool exec.
    let pipes = match supervisor {
        Some(_) => {
            let guard = lifeline().map_err(|error| {
                CommandFailure::from_io("cannot create parent-death lifeline", error)
            })?;
            let errors = lifeline().map_err(|error| {
                CommandFailure::from_io("cannot create parent-death lifeline", error)
            })?;
            let liveness = lifeline().map_err(|error| {
                CommandFailure::from_io("cannot create parent-death lifeline", error)
            })?;
            // The tool's argv travels on this pipe, sent only after the
            // guardian's handshake: a replaced image never learns the tool.
            let command = lifeline().map_err(|error| {
                CommandFailure::from_io("cannot create parent-death lifeline", error)
            })?;
            Some((guard, errors, liveness, command))
        }
        None => None,
    };
    let inherited: Vec<RawFd> = pipes
        .as_ref()
        .map(|((read, _), (_, errors), (_, liveness), (command, _))| {
            vec![
                read.as_raw_fd(),
                errors.as_raw_fd(),
                liveness.as_raw_fd(),
                command.as_raw_fd(),
            ]
        })
        .unwrap_or_default();
    #[cfg(target_os = "linux")]
    let parent_pid = std::process::id() as libc::pid_t;
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    #[cfg(test)]
    if let Some((program, _)) = &supervisor {
        tests::before_supervisor_spawn(program);
    }
    let mut command = match (&supervisor, &pipes) {
        (Some((program, prefix)), Some(((lifeline, _), (_, errors), (_, liveness), (tool, _)))) => {
            let mut command = Command::new(program);
            command
                .args(prefix)
                .arg(SUPERVISE_ARGUMENT)
                .arg(lifeline.as_raw_fd().to_string())
                .arg(errors.as_raw_fd().to_string())
                .arg(liveness.as_raw_fd().to_string())
                .arg(tool.as_raw_fd().to_string())
                .arg(SUPERVISOR_PROTOCOL.to_string())
                .arg(&nonce);
            command
        }
        _ => {
            let mut command = Command::new(executable);
            command.args(&argv[1..]);
            command
        }
    };
    command
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
                // It survives the supervisor's exec of the tool (same PID).
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
            if let Some(mask) = umask {
                libc::umask(mask as libc::mode_t);
            }
            close_on_exec_all(&mut descriptors)?;
            for &fd in pass_fds.iter().chain(&inherited) {
                let flags = fd_flags(fd)?;
                if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let child = catabolic_core::spawn_guard::spawn(&mut command)
        .map_err(|error| CommandFailure::from_io("cannot start command", error))?;
    // Install the guard before any user callback or fallible operation.
    let mut process = ProcessGroup::new(child);
    if let Some((
        (lifeline_read, lifeline_write),
        (errors_read, errors_write),
        (liveness_read, liveness_write),
        (command_read, command_write),
    )) = pipes
    {
        // Only the supervisor and guardian keep these ends.
        drop(lifeline_read);
        drop(errors_write);
        drop(liveness_write);
        drop(command_read);
        process.lifeline = Some(lifeline_write);
        if let Err(failure) = await_guardian(
            &liveness_read,
            &errors_read,
            &guardian_handshake(&nonce),
            deadline,
        )
        .and_then(|()| send_command(command_write, argv, deadline))
        {
            let _ = process.retire();
            return Err(failure);
        }
        process.liveness = Some(liveness_read);
        if let Some(error) = supervised_exec_error(&errors_read, deadline) {
            let _ = process.retire();
            return Err(match error {
                Err(()) => CommandFailure::new("timeout", "command timed out"),
                Ok(error) => CommandFailure::from_io("cannot start command", error),
            });
        }
    }
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
            poll_descriptor(process.liveness.as_ref(), libc::POLLIN),
        ];
        // Round up to avoid busy-spinning in the last sub-millisecond interval.
        let millis = remaining.as_millis().saturating_add(1).min(50) as i32;
        // SAFETY: descriptors points to four initialized pollfd values for the
        // duration of the call, and all nonnegative FDs are owned by this pump.
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), 4, millis) };
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
        // The guardian never writes and only exits by the group kill, which
        // happens after this pump returns: any event means it died early. The
        // caller then retires the group by the tool's PGID, which cannot have
        // been reused because the tool is our unreaped child.
        if descriptors[3].revents != 0 {
            return Err(CommandFailure::new(
                "failed",
                "parent-death guardian exited unexpectedly",
            ));
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

/// Allocated before fork: the descriptor ceiling, and on macOS a buffer for
/// the kernel's exact open-descriptor list, read in the child without
/// allocation, so marking costs one call per open descriptor instead of one
/// per possible descriptor up to a (possibly 1M) soft limit.
struct DescriptorScan {
    limit: RawFd,
    #[cfg(target_os = "macos")]
    listed: Vec<libc::proc_fdinfo>,
}
impl DescriptorScan {
    fn new() -> io::Result<Self> {
        let limit = descriptor_limit()?;
        #[cfg(target_os = "macos")]
        {
            let open = std::fs::read_dir("/dev/fd")
                .map(|entries| entries.count())
                .unwrap_or(0);
            Ok(Self {
                limit,
                listed: Vec::with_capacity((open * 2).max(4096)),
            })
        }
        #[cfg(not(target_os = "macos"))]
        Ok(Self { limit })
    }
}

fn close_on_exec_all(scan: &mut DescriptorScan) -> io::Result<()> {
    let limit = scan.limit;
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
    #[cfg(target_os = "macos")]
    {
        let entry = std::mem::size_of::<libc::proc_fdinfo>();
        let size = (scan.listed.capacity() * entry).min(i32::MAX as usize) as libc::c_int;
        // SAFETY: proc_pidinfo writes at most `size` bytes of plain records into
        // the preallocated, unaliased buffer; no allocation or lock is involved.
        let bytes = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                scan.listed.as_mut_ptr().cast(),
                size,
            )
        };
        // A full buffer may be truncated: fall back to the exhaustive loop.
        if bytes > 0 && bytes < size {
            for index in 0..bytes as usize / entry {
                // SAFETY: the kernel initialized this many records above.
                let fd = unsafe { (*scan.listed.as_ptr().add(index)).proc_fd };
                if fd < 3 {
                    continue;
                }
                match fd_flags(fd) {
                    Ok(flags) => {
                        // SAFETY: sets close-on-exec on an existing descriptor.
                        if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1
                        {
                            return Err(io::Error::last_os_error());
                        }
                    }
                    Err(error) if error.raw_os_error() == Some(libc::EBADF) => {}
                    Err(error) => return Err(error),
                }
            }
            return Ok(());
        }
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
    /// Read end of a pipe whose only writer is the guardian. Readiness or
    /// hang-up while the command runs means the guardian died.
    liveness: Option<OwnedFd>,
}

/// A pipe whose ends are both close-on-exec and above the standard
/// descriptors: (read end for the guardian, write end for the worker).
fn lifeline() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds: [libc::c_int; 2] = [-1; 2];
    #[cfg(target_os = "linux")]
    {
        // SAFETY: fds is a writable two-element descriptor array.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    // Without pipe2, no in-workspace spawner may fork until both ends are
    // close-on-exec: they all hold the shared side of this lock while forking.
    #[cfg(not(target_os = "linux"))]
    let _exclusive = catabolic_core::spawn_guard::descriptors();
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: fds is a writable two-element descriptor array.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: pipe returned two new descriptors owned only by this function.
    let original = unsafe { [OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])] };
    // Relocate to >= 3 with close-on-exec, so a closed caller stdio slot can
    // never be replaced by a child's stdio setup.
    let relocate = |fd: &OwnedFd| -> io::Result<OwnedFd> {
        // SAFETY: duplicates a descriptor owned above into a new owned one.
        let new = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if new == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fcntl returned a new descriptor owned only by this function.
        Ok(unsafe { OwnedFd::from_raw_fd(new) })
    };
    let read = relocate(&original[0])?;
    let write = relocate(&original[1])?;
    drop(original);
    Ok((read, write))
}

/// Hidden argument selecting supervisor mode in every Catabolic executable.
pub const SUPERVISE_ARGUMENT: &str = "__catabolic-supervise";

struct Supervisor {
    /// Pinned identity: a removed or replaced executable fails closed.
    helper: crate::helper::Helper,
    prefix: Vec<OsString>,
}
impl Supervisor {
    fn resolve(&self) -> Result<(std::path::PathBuf, Vec<OsString>), String> {
        crate::helper::verified(&self.helper)
            .map(|program| (program, self.prefix.clone()))
            .map_err(|error| error.0)
    }
}
static SUPERVISOR: std::sync::OnceLock<Supervisor> = std::sync::OnceLock::new();

/// Called first by Catabolic executables: run supervisor mode when requested
/// (never returns), otherwise register this executable as the supervisor.
pub fn supervise_or_configure() {
    let args: Vec<OsString> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == SUPERVISE_ARGUMENT) {
        supervise(&args[2..]);
    }
    if let Ok(helper) = std::env::current_exe()
        .map_err(catabolic_store::Error::from)
        .and_then(|program| crate::helper::pin(&program))
    {
        let _ = SUPERVISOR.set(Supervisor {
            helper,
            prefix: Vec::new(),
        });
    }
}

/// The supervisor to exec, `Ok(None)` for direct execution, or an error that
/// must fail the spawn closed. Once a supervisor or native helper has been
/// configured, any later resolution failure (removed or replaced executable)
/// is an error, never a silent fallback to unsupervised execution. Only a
/// process that never configured one runs tools directly; on non-Linux that
/// provides no parent-death protection, and workers that require it refuse
/// to start (`parent_death_supervised`).
fn supervisor() -> Result<Option<(std::path::PathBuf, Vec<OsString>)>, String> {
    #[cfg(test)]
    if let Some(result) = tests::supervisor_override() {
        return result;
    }
    if let Some(supervisor) = SUPERVISOR.get() {
        return supervisor.resolve().map(Some);
    }
    #[cfg(test)]
    {
        // Unit tests: this test binary enters supervisor mode from a load-time
        // hook (see `tests::SUPERVISOR_HOOK`) before the test harness starts.
        std::env::current_exe()
            .map(|program| Some((program, Vec::new())))
            .map_err(|error| error.to_string())
    }
    #[cfg(not(test))]
    {
        if !crate::helper::configured() {
            return Ok(None);
        }
        // Embedders: the registered native helper is a Catabolic executable.
        crate::helper::executable()
            .map(|program| Some((program, Vec::new())))
            .map_err(|error| error.0)
    }
}

/// Whether tools started by `command_output` are killed, with their whole
/// group, when this process dies on this platform.
pub fn parent_death_supervised() -> bool {
    matches!(supervisor(), Ok(Some(_)))
}

/// Read exactly the guardian's handshake from the liveness pipe before the
/// tool is accepted. The supervisor execs the tool only after the guardian has
/// written it, so the exec-error pipe closing (or reporting) first, wrong
/// bytes, or no handshake within thirty seconds all refuse the spawn. The
/// command's own deadline still reports an ordinary timeout.
fn await_guardian(
    liveness: &OwnedFd,
    errors: &OwnedFd,
    expected: &[u8],
    deadline: Instant,
) -> Result<(), CommandFailure> {
    let mismatch = || CommandFailure::new("failed", "parent-death supervisor protocol mismatch");
    let cap = Instant::now() + Duration::from_secs(30);
    let limit = deadline.min(cap);
    let mut received = Vec::with_capacity(expected.len());
    while received.len() < expected.len() {
        let remaining = limit.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(if Instant::now() >= deadline {
                CommandFailure::new("timeout", "command exceeded the elapsed-time limit")
            } else {
                mismatch()
            });
        }
        let mut polls = [
            libc::pollfd {
                fd: liveness.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: errors.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
        // SAFETY: poll reads two initialized pollfd values during the call.
        let ready = unsafe { libc::poll(polls.as_mut_ptr(), 2, millis) };
        if ready == -1 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(CommandFailure::from_io(
                "cannot await parent-death guardian",
                io::Error::last_os_error(),
            ));
        }
        if polls[0].revents != 0 {
            let mut chunk = [0u8; 128];
            let want = (expected.len() - received.len()).min(chunk.len());
            // SAFETY: reads at most `want` bytes into a local buffer.
            let read = unsafe { libc::read(liveness.as_raw_fd(), chunk.as_mut_ptr().cast(), want) };
            match read {
                -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => continue,
                count if count <= 0 => return Err(mismatch()),
                count => {
                    received.extend_from_slice(&chunk[..count as usize]);
                    if !expected.starts_with(&received) {
                        return Err(mismatch());
                    }
                }
            }
            continue;
        }
        if polls[1].revents != 0 {
            // The supervisor failed (errno) or something else execed first.
            return Err(match supervised_exec_error(errors, deadline) {
                Some(Ok(error)) => CommandFailure::from_io("cannot start command", error),
                _ => mismatch(),
            });
        }
    }
    Ok(())
}

/// Wait for the supervisor's exec-error pipe to close (the tool was execed) or
/// to report an errno. `Some(Err(()))` is a deadline expiry.
fn supervised_exec_error(errors: &OwnedFd, deadline: Instant) -> Option<Result<io::Error, ()>> {
    let mut code = [0u8; 4];
    let mut filled = 0;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Some(Err(()));
        }
        let mut poll = libc::pollfd {
            fd: errors.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
        // SAFETY: poll reads one initialized pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut poll, 1, millis) };
        if ready == -1 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Some(Ok(io::Error::last_os_error()));
        }
        if ready == 0 {
            continue;
        }
        // SAFETY: reads into the unfilled tail of a local buffer.
        let read = unsafe {
            libc::read(
                errors.as_raw_fd(),
                code[filled..].as_mut_ptr().cast(),
                code.len() - filled,
            )
        };
        match read {
            0 if filled == 0 => return None,
            0 => return Some(Ok(io::Error::from_raw_os_error(libc::EIO))),
            -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
            -1 => return Some(Ok(io::Error::last_os_error())),
            count => {
                filled += count as usize;
                if filled == code.len() {
                    return Some(Ok(io::Error::from_raw_os_error(i32::from_ne_bytes(code))));
                }
            }
        }
    }
}

/// Signals the guardian ignores: job control and termination requests. An
/// orphaned stopped group receives SIGHUP then SIGCONT; the guardian must
/// survive them to kill the group. Only SIGKILL (its own group kill) ends it.
const GUARDIAN_IGNORED: [libc::c_int; 10] = [
    libc::SIGHUP,
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGTERM,
    libc::SIGTSTP,
    libc::SIGTTIN,
    libc::SIGTTOU,
    libc::SIGPIPE,
    libc::SIGUSR1,
    libc::SIGUSR2,
];

/// Whether this process may act as a supervisor: it leads its own session
/// and process group, and every protocol descriptor is an open pipe/FIFO.
fn supervisor_admitted(descriptors: &[RawFd]) -> bool {
    // SAFETY: getpid, getpgrp and getsid(0) only query this process.
    let leader = unsafe {
        let pid = libc::getpid();
        libc::getpgrp() == pid && libc::getsid(0) == pid
    };
    leader
        && descriptors.iter().all(|&fd| {
            // SAFETY: fstat writes into a local, zero-initialized stat buffer.
            let mut status: libc::stat = unsafe { std::mem::zeroed() };
            (unsafe { libc::fstat(fd, &mut status) }) == 0
                && status.st_mode & libc::S_IFMT == libc::S_IFIFO
        })
}

/// Version of the worker/supervisor/guardian protocol. A supervisor speaking
/// any other version refuses, and the worker refuses its handshake.
pub const SUPERVISOR_PROTOCOL: u32 = 3;
/// Largest encoded tool argv the supervisor accepts on its command pipe.
const MAX_COMMAND_BYTES: usize = 4 * 1024 * 1024;

/// `<count>\0` then each argument followed by `\0` (arguments cannot contain
/// NUL), so a truncated payload is detectable.
fn encode_command<S: AsRef<OsStr>>(argv: &[S]) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut payload = format!("{}\0", argv.len()).into_bytes();
    for arg in argv {
        payload.extend_from_slice(arg.as_ref().as_bytes());
        payload.push(0);
    }
    payload
}

fn decode_command(payload: &[u8]) -> Option<Vec<OsString>> {
    use std::os::unix::ffi::OsStringExt as _;
    let mut parts = payload.split(|byte| *byte == 0);
    let count: usize = std::str::from_utf8(parts.next()?).ok()?.parse().ok()?;
    let mut argv: Vec<OsString> = parts
        .map(|part| OsString::from_vec(part.to_vec()))
        .collect();
    // The payload ends with NUL, which leaves one empty trailing part.
    if count == 0 || argv.pop()? != OsString::new() || argv.len() != count {
        return None;
    }
    if argv[0].is_empty() {
        return None;
    }
    Some(argv)
}

/// Send the tool's argv to the verified supervisor, then close the pipe. The
/// write end is non-blocking and polled against the command deadline.
fn send_command<S: AsRef<OsStr>>(
    pipe: OwnedFd,
    argv: &[S],
    deadline: Instant,
) -> Result<(), CommandFailure> {
    let payload = encode_command(argv);
    if payload.len() > MAX_COMMAND_BYTES {
        return Err(CommandFailure::new(
            "failed",
            "command arguments are too large",
        ));
    }
    let fd = pipe.as_raw_fd();
    // SAFETY: reads and updates the status flags of a descriptor we own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags == -1 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == -1 {
            return Err(CommandFailure::from_io(
                "cannot start command",
                io::Error::last_os_error(),
            ));
        }
    }
    let expired = || CommandFailure::new("timeout", "command exceeded the elapsed-time limit");
    let mut written = 0;
    while written < payload.len() {
        // Checked before every write: nothing more is delivered after expiry,
        // whatever the supervisor drained meanwhile.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(expired());
        }
        // SAFETY: writes from a live local buffer to a descriptor we own.
        let count = unsafe {
            libc::write(
                fd,
                payload[written..].as_ptr().cast(),
                payload.len() - written,
            )
        };
        if count > 0 {
            written += count as usize;
            continue;
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => {
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
                // SAFETY: poll reads one initialized pollfd during the call.
                match unsafe { libc::poll(&mut poll, 1, millis) } {
                    0 => return Err(expired()),
                    -1 if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted => {
                        return Err(CommandFailure::from_io(
                            "cannot start command",
                            io::Error::last_os_error(),
                        ));
                    }
                    _ => {}
                }
            }
            _ => return Err(CommandFailure::from_io("cannot start command", error)),
        }
    }
    Ok(())
}

fn valid_nonce(nonce: &str) -> bool {
    nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The exact bytes the guardian writes on the liveness pipe.
fn guardian_handshake(nonce: &str) -> Vec<u8> {
    format!("catabolic-guardian {SUPERVISOR_PROTOCOL} {nonce}\n").into_bytes()
}

/// Supervisor mode: `<lifeline-fd> <exec-error-fd> <liveness-fd>
/// <command-fd> <protocol> <nonce>`. A misused invocation exits 127 without
/// signalling anything.
///
/// The tool is not on this command line. The worker sends its argv on the
/// command pipe only after reading, on the liveness pipe whose sole writer is
/// the guardian, the guardian's echo of this spawn's protocol version and
/// random nonce. An executable replaced between identity verification and
/// exec (an upgrade, another version, or a wrapper) therefore never learns
/// which tool to run, and cannot start it unguarded even if the worker dies
/// meanwhile. A deliberately malicious same-user replacement that fakes the
/// handshake is out of scope: the same user can already kill or trace the
/// guardian.
/// Runs in a fresh process image that is already the session leader, with the
/// worker's environment, directory, umask and descriptors for the tool.
pub fn supervise(args: &[OsString]) -> ! {
    fn number(value: Option<&OsString>) -> Option<RawFd> {
        value?.to_str()?.parse().ok().filter(|fd: &RawFd| *fd >= 3)
    }
    // Refuse anything but the worker's exact protocol, before touching any
    // descriptor and without ever signalling: this process must be the leader
    // of its own new session (so the guardian's group kill reaches only the
    // tool's group), and all three protocol descriptors must be pipes.
    let (Some(lifeline), Some(errors), Some(liveness), Some(command)) = (
        number(args.first()),
        number(args.get(1)),
        number(args.get(2)),
        number(args.get(3)),
    ) else {
        // SAFETY: immediate exit of a misused supervisor process.
        unsafe { libc::_exit(127) }
    };
    if !supervisor_admitted(&[lifeline, errors, liveness, command]) {
        // SAFETY: immediate exit of a misused supervisor process.
        unsafe { libc::_exit(127) }
    }
    let fail = |code: i32| -> ! {
        let bytes = code.to_ne_bytes();
        // SAFETY: writes four bytes from a local buffer, then exits.
        unsafe {
            libc::write(errors, bytes.as_ptr().cast(), bytes.len());
            libc::_exit(127)
        }
    };
    // Protocol version and the worker's per-spawn nonce, echoed back by the
    // guardian itself once it is ready (see `guardian_handshake`).
    let handshake = match (args.get(4).and_then(|v| v.to_str()), args.get(5)) {
        (Some(version), Some(nonce))
            if version == SUPERVISOR_PROTOCOL.to_string() && args.len() == 6 =>
        {
            match nonce.to_str() {
                Some(nonce) if valid_nonce(nonce) => guardian_handshake(nonce),
                _ => fail(libc::EINVAL),
            }
        }
        _ => fail(libc::EINVAL),
    };
    // All supervisor descriptors close when the tool is execed.
    for fd in [lifeline, errors, liveness, command] {
        // SAFETY: marks an inherited descriptor of this process close-on-exec.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            fail(
                io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            );
        }
    }
    // The guardian closes exactly the descriptors open now, enumerated before
    // forking (this process is single-threaded and may allocate here).
    let directory = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    let open: Vec<RawFd> = match std::fs::read_dir(directory) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
            .collect(),
        Err(error) => fail(error.raw_os_error().unwrap_or(libc::EIO)),
    };
    let mut ready: [libc::c_int; 2] = [-1; 2];
    // SAFETY: ready is a writable two-element descriptor array.
    if unsafe { libc::pipe(ready.as_mut_ptr()) } == -1 {
        fail(
            io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO),
        );
    }
    // SAFETY: fork, sigaction-style signal, close, write, read, kill, waitpid
    // and _exit are used below; no lock is taken in the forked processes.
    unsafe {
        for fd in ready {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let first = libc::fork();
        if first == -1 {
            fail(
                io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            );
        }
        if first == 0 {
            let second = libc::fork();
            if second != 0 {
                libc::_exit(if second == -1 { 1 } else { 0 });
            }
            // Guardian: reparented away from the tool, same session/group.
            for signal in GUARDIAN_IGNORED {
                libc::signal(signal, libc::SIG_IGN);
            }
            // The guardian holds the liveness write end for its whole life:
            // the worker treats its closure as the guardian's death.
            for &fd in &open {
                if fd != lifeline && fd != liveness {
                    libc::close(fd);
                }
            }
            libc::close(ready[0]);
            // Prove to the worker, on the guardian-owned liveness pipe, that
            // this guardian (signals ignored, lifeline held) exists for this
            // exact spawn. Then release the supervisor to exec the tool.
            let mut written = 0;
            while written < handshake.len() {
                let count = libc::write(
                    liveness,
                    handshake[written..].as_ptr().cast(),
                    handshake.len() - written,
                );
                if count > 0 {
                    written += count as usize;
                } else if count == -1
                    && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                {
                    continue;
                } else {
                    libc::_exit(1);
                }
            }
            let byte = 1u8;
            libc::write(ready[1], (&raw const byte).cast(), 1);
            libc::close(ready[1]);
            let mut buffer = 0u8;
            loop {
                let read = libc::read(lifeline, (&raw mut buffer).cast(), 1);
                if read == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                break;
            }
            // EOF: the worker is gone or retired this group.
            libc::kill(0, libc::SIGKILL);
            libc::_exit(0);
        }
        libc::close(ready[1]);
        let mut status = 0;
        while libc::waitpid(first, &mut status, 0) == -1 {
            if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                fail(libc::ECHILD);
            }
        }
        if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
            fail(libc::EAGAIN);
        }
        // Do not admit the tool until the guardian has finished initializing.
        let mut byte = 0u8;
        let read = loop {
            let read = libc::read(ready[0], (&raw mut byte).cast(), 1);
            if read == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break read;
        };
        if read != 1 {
            fail(libc::EAGAIN);
        }
        libc::close(ready[0]);
    }
    // The worker sends the tool only after verifying the guardian's handshake.
    let mut payload = Vec::new();
    {
        use std::io::Read as _;
        use std::os::fd::FromRawFd as _;
        // SAFETY: `command` is an inherited pipe this process owns exclusively;
        // the guardian has closed its copy.
        let pipe = unsafe { std::fs::File::from_raw_fd(command) };
        if let Err(error) = pipe
            .take(MAX_COMMAND_BYTES as u64 + 1)
            .read_to_end(&mut payload)
        {
            fail(error.raw_os_error().unwrap_or(libc::EIO));
        }
    }
    let Some(tool) = payload
        .len()
        .le(&MAX_COMMAND_BYTES)
        .then(|| decode_command(&payload))
        .flatten()
    else {
        fail(libc::EINVAL)
    };
    // Same PID: the worker observes this tool's exact status. `exec` searches
    // PATH like `Command::spawn` and restores default SIGPIPE handling.
    use std::os::unix::process::CommandExt as _;
    let error = Command::new(&tool[0]).args(&tool[1..]).exec();
    fail(error.raw_os_error().unwrap_or(libc::EIO))
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
            liveness: None,
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
            // already-exited child as a zombie. Never block waiting
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
    fn high_numbered_inheritable_descriptor_is_closed() {
        let file = tempfile::tempfile().unwrap();
        // SAFETY: duplicates the fixture file to a fresh, inheritable high FD.
        let high = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 9000) };
        assert!(high >= 9000);
        // SAFETY: the duplicate is owned only by this test.
        let _high = unsafe { OwnedFd::from_raw_fd(high) };
        let output = shell(
            &format!("if [ -e /dev/fd/{high} ]; then exit 9; fi; printf closed"),
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
        // The tool leads its own session and group; its grandchild is a member.
        let group = pids[0];
        // SAFETY: getsid only queries the named fixture process.
        assert_eq!(unsafe { libc::getsid(group) }, group);
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

    /// Re-run one fixture test of this binary in a fresh process.
    fn fixture_process(name: &str, env: &[(&str, &std::path::Path)]) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", name, "--nocapture"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }

    static ATFORK_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
    extern "C" fn atfork_child() {
        let fd = ATFORK_PIPE.load(Ordering::Relaxed);
        if fd >= 0 {
            // SAFETY: write is async-signal-safe; fd is the fixture's pipe.
            unsafe { libc::write(fd, b"c".as_ptr().cast(), 1) };
        }
    }

    /// Every child-side atfork handler run is recorded. One command forks
    /// exactly once from this process and never inside a forked child, where a
    /// host handler could deadlock.
    #[test]
    fn no_fork_happens_inside_a_forked_child() {
        let dir = tempfile::tempdir().unwrap();
        let result = dir.path().join("forks");
        let status = fixture_process(
            "process::tests::atfork_fixture",
            &[("CATABOLIC_ATFORK_FIXTURE", &result)],
        )
        .status()
        .unwrap();
        assert!(status.success());
        // The supervisor is forked once here; it execs before its own forks,
        // which run in a fresh image without this process's handlers. A fork
        // inside the forked child (before exec) would add more.
        assert_eq!(fs::read_to_string(&result).unwrap(), "1");
    }

    #[test]
    fn atfork_fixture() {
        let Some(path) = std::env::var_os("CATABOLIC_ATFORK_FIXTURE") else {
            return;
        };
        let mut fds = [-1; 2];
        // SAFETY: fds is a writable two-element descriptor array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: the read end is nonblocking so counting cannot hang.
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) };
        ATFORK_PIPE.store(fds[1], Ordering::Relaxed);
        // SAFETY: registers an async-signal-safe child handler for this fixture.
        assert_eq!(
            unsafe { libc::pthread_atfork(None, None, Some(atfork_child)) },
            0
        );
        command_output(&["/bin/sh", "-c", "exit 0"], CommandOptions::default()).unwrap();
        let mut buffer = [0u8; 64];
        // SAFETY: reads into a local buffer from the fixture's own pipe.
        let count = unsafe { libc::read(fds[0], buffer.as_mut_ptr().cast(), buffer.len()) };
        fs::write(path, count.max(0).to_string()).unwrap();
    }

    /// While another thread keeps spawning long-lived children through the
    /// workspace spawn guard, worker death must still kill the tool group.
    #[test]
    fn concurrent_spawns_cannot_retain_the_lifeline() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("pids");
        let noise_path = dir.path().join("noise");
        fs::create_dir(&noise_path).unwrap();
        let mut worker = fixture_process(
            "process::tests::concurrent_spawn_fixture",
            &[
                ("CATABOLIC_GUARDIAN_FIXTURE", &pid_path),
                ("CATABOLIC_SPAWN_NOISE", &noise_path),
            ],
        )
        .spawn()
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let group: libc::pid_t = loop {
            if let Ok(raw) = fs::read_to_string(&pid_path)
                && raw.ends_with('\n')
                && let Some(pid) = raw.split_whitespace().next()
                && let Ok(pid) = pid.parse::<libc::pid_t>()
            {
                // SAFETY: getpgid only queries the named fixture process.
                break unsafe { libc::getpgid(pid) };
            }
            assert!(Instant::now() < deadline, "tool failed to start");
            thread::sleep(Duration::from_millis(10));
        };
        assert!(group > 0);
        // Let the noise thread keep spawning around the running command.
        thread::sleep(Duration::from_millis(300));
        worker.kill().unwrap();
        worker.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        // SAFETY: signal 0 only probes the fixture group for live members.
        while unsafe { libc::kill(-group, 0) } == 0 {
            if Instant::now() >= deadline {
                // SAFETY: failure-path cleanup of our own fixture group.
                unsafe { libc::kill(-group, libc::SIGKILL) };
                panic!("tool group survived worker death during concurrent spawns");
            }
            thread::sleep(Duration::from_millis(10));
        }
        // Clean up the long-lived noise children by their recorded PIDs.
        for entry in fs::read_dir(&noise_path).unwrap() {
            if let Ok(pid) = entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .parse::<libc::pid_t>()
            {
                // SAFETY: each PID names a noise child spawned by the fixture.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    #[test]
    fn concurrent_spawn_fixture() {
        let (Some(path), Some(noise)) = (
            std::env::var_os("CATABOLIC_GUARDIAN_FIXTURE"),
            std::env::var_os("CATABOLIC_SPAWN_NOISE"),
        ) else {
            return;
        };
        let noise = std::path::PathBuf::from(noise);
        thread::spawn(move || {
            // Bounded and throttled, so the host process limit is never hit.
            for _ in 0..150 {
                thread::sleep(Duration::from_millis(5));
                // Long-lived, ordinary children that do not close descriptors.
                let child = catabolic_core::spawn_guard::spawn(
                    Command::new("/bin/sleep")
                        .arg("30")
                        .stdout(Stdio::null())
                        .stderr(Stdio::null()),
                )
                .unwrap();
                fs::write(noise.join(child.id().to_string()), b"").unwrap();
                std::mem::forget(child);
            }
        });
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

    /// With the caller's standard descriptors closed, pipe() returns 0 and 1.
    /// The lifeline must be relocated, so the guardian does not see EOF early.
    #[test]
    fn closed_standard_descriptors_do_not_break_the_lifeline() {
        let status = fixture_process("process::tests::closed_stdio_fixture", &[])
            .env("CATABOLIC_CLOSED_STDIO_FIXTURE", "1")
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{status:?}");
    }

    #[test]
    fn closed_stdio_fixture() {
        if std::env::var_os("CATABOLIC_CLOSED_STDIO_FIXTURE").is_none() {
            return;
        }
        // SAFETY: this fixture process owns its standard descriptors.
        unsafe {
            libc::close(0);
            libc::close(1);
            libc::close(2);
        }
        let output = command_output(
            &["/bin/sh", "-c", "/bin/sleep 1; printf ok"],
            CommandOptions {
                timeout: Duration::from_secs(20),
                ..CommandOptions::default()
            },
        );
        let ok = output.is_ok_and(|output| output.stdout == b"ok" && output.returncode == 0);
        std::process::exit(if ok { 0 } else { 3 });
    }

    /// Load-time hook: this test binary acts as the supervisor executable
    /// before the test harness starts (argc/argv come from the loader).
    extern "C" fn supervisor_hook(
        argc: libc::c_int,
        argv: *const *const libc::c_char,
        _env: *const *const libc::c_char,
    ) {
        use std::os::unix::ffi::OsStrExt;
        if argc < 2 || argv.is_null() {
            return;
        }
        // SAFETY: the loader passes argc valid NUL-terminated argument strings.
        let args: Vec<OsString> = (0..argc as usize)
            .map(|i| unsafe {
                OsStr::from_bytes(std::ffi::CStr::from_ptr(*argv.add(i)).to_bytes()).to_owned()
            })
            .collect();
        if args[1] == SUPERVISE_ARGUMENT {
            supervise(&args[2..]);
        }
    }
    #[used]
    #[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
    #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__mod_init_func"))]
    static SUPERVISOR_HOOK: extern "C" fn(
        libc::c_int,
        *const *const libc::c_char,
        *const *const libc::c_char,
    ) = supervisor_hook;

    thread_local! {
        static SUPERVISOR_OVERRIDE: std::cell::RefCell<Option<Supervisor>> =
            const { std::cell::RefCell::new(None) };
    }
    /// Per-thread supervisor substitute for fail-closed tests.
    pub(super) fn supervisor_override()
    -> Option<Result<Option<(std::path::PathBuf, Vec<OsString>)>, String>> {
        SUPERVISOR_OVERRIDE.with(|slot| slot.borrow().as_ref().map(|s| s.resolve().map(Some)))
    }

    thread_local! {
        static BEFORE_SPAWN: std::cell::RefCell<Option<Box<dyn Fn(&std::path::Path)>>> =
            const { std::cell::RefCell::new(None) };
    }
    /// Test hook between supervisor identity verification and its exec.
    pub(super) fn before_supervisor_spawn(program: &std::path::Path) {
        BEFORE_SPAWN.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook(program)
            }
        });
    }

    /// Replace the pinned supervisor with `wrapper(argv_file)` after
    /// verification and before exec. The spawn must be refused, the tool must
    /// never start, and the replacement must never see the tool's argv.
    fn refused_when_replaced_during_spawn(wrapper: fn(&std::path::Path) -> String) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let argv_file = dir.path().join("argv");
        let wrapper = wrapper(&argv_file);
        let copy = dir.path().join("supervisor");
        fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
        let helper = crate::helper::pin(&copy).unwrap();
        SUPERVISOR_OVERRIDE.with(|slot| {
            *slot.borrow_mut() = Some(Supervisor {
                helper,
                prefix: Vec::new(),
            })
        });
        BEFORE_SPAWN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |program: &std::path::Path| {
                // No in-workspace fork may inherit the write descriptor
                // (ETXTBSY on exec): hold the exclusive spawn guard.
                let _exclusive = catabolic_core::spawn_guard::descriptors();
                fs::remove_file(program).unwrap();
                fs::write(program, &wrapper).unwrap();
                fs::set_permissions(program, fs::Permissions::from_mode(0o755)).unwrap();
            }))
        });
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                SUPERVISOR_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
                BEFORE_SPAWN.with(|hook| *hook.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let pid_path = dir.path().join("pid");
        let script = format!(
            "printf '%s\\n' $$ > '{0}.tmp'; mv '{0}.tmp' '{0}'; exec /bin/sleep 30",
            pid_path.display()
        );
        let failure = command_output(
            &["/bin/sh", "-c", &script],
            CommandOptions {
                timeout: Duration::from_secs(60),
                ..CommandOptions::default()
            },
        )
        .unwrap_err();
        assert_eq!(failure.state, "failed");
        assert_eq!(failure.message, "parent-death supervisor protocol mismatch");
        // The tool never started, so nothing can outlive a worker death.
        assert!(!pid_path.exists(), "a replaced supervisor started the tool");
        let seen = fs::read_to_string(&argv_file).unwrap();
        assert!(
            !seen.contains("sleep 30"),
            "the replacement saw the tool: {seen}"
        );
        assert!(!seen.contains(&*pid_path.to_string_lossy()), "{seen}");
    }

    #[test]
    fn replaced_supervisor_never_receives_the_tool() {
        // Records its argv and exits without a handshake: the tool is on
        // neither its command line nor (see the next test) its command pipe.
        refused_when_replaced_during_spawn(|argv| {
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{0}'\nexit 0\n",
                argv.display()
            )
        });
    }

    #[test]
    fn older_protocol_supervisor_is_refused() {
        // Echoes the nonce with the previous protocol version on the liveness
        // pipe. argv: mode, lifeline, errors, liveness, command, protocol,
        // nonce.
        refused_when_replaced_during_spawn(|argv| {
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{0}'\nprintf 'catabolic-guardian 2 %s\\n' \"$7\" > /dev/fd/$4\ncat <&$5 >> '{0}'\n",
                argv.display()
            )
        });
    }

    /// A payload larger than the pipe buffer, with nobody reading: the send
    /// stops at its deadline after a partial write and delivers nothing more.
    #[test]
    fn command_send_stops_at_its_deadline_during_a_partial_write() {
        let (read, write) = lifeline().unwrap();
        let big = "x".repeat(1024 * 1024);
        let argv = ["/bin/sh", "-c", big.as_str()];
        let started = Instant::now();
        let failure =
            send_command(write, &argv, Instant::now() + Duration::from_millis(200)).unwrap_err();
        assert_eq!(failure.state, "timeout");
        assert!(started.elapsed() < Duration::from_secs(5));
        // Some bytes were written before expiry, never the whole payload.
        let mut pending: libc::c_int = 0;
        // SAFETY: FIONREAD writes the pending byte count to a local integer.
        unsafe { libc::ioctl(read.as_raw_fd(), libc::FIONREAD, &mut pending) };
        assert!(pending > 0 && (pending as usize) < encode_command(&argv).len());
        // An already expired deadline writes nothing at all.
        let (read, write) = lifeline().unwrap();
        let failure = send_command(write, &["/bin/true"], Instant::now()).unwrap_err();
        assert_eq!(failure.state, "timeout");
        let mut pending: libc::c_int = 0;
        // SAFETY: as above.
        unsafe { libc::ioctl(read.as_raw_fd(), libc::FIONREAD, &mut pending) };
        assert_eq!(pending, 0);
    }

    #[test]
    fn command_payload_round_trips_and_rejects_truncation() {
        let argv = ["/bin/sh", "-c", "printf 'a b'", ""];
        let payload = encode_command(&argv);
        let decoded = decode_command(&payload).unwrap();
        assert_eq!(decoded, argv.map(OsString::from).to_vec());
        for cut in 0..payload.len() {
            assert!(decode_command(&payload[..cut]).is_none(), "cut {cut}");
        }
        assert!(decode_command(b"0\0").is_none());
        assert!(decode_command(b"1\0\0").is_none());
        assert!(decode_command(b"x\0a\0").is_none());
        assert!(decode_command(b"1\0a\0b\0").is_none());
    }

    /// A supervisor that was configured and is later removed or replaced makes
    /// every tool spawn refuse; the tool never runs unsupervised.
    #[test]
    fn removed_or_replaced_supervisor_fails_closed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("supervisor");
        fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
        let helper = crate::helper::pin(&copy).unwrap();
        SUPERVISOR_OVERRIDE.with(|slot| {
            *slot.borrow_mut() = Some(Supervisor {
                helper,
                prefix: Vec::new(),
            })
        });
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                SUPERVISOR_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        // Intact: the pinned copy supervises normally.
        let output =
            command_output(&["/bin/sh", "-c", "printf ok"], CommandOptions::default()).unwrap();
        assert_eq!(output.stdout, b"ok");
        let marker = dir.path().join("ran");
        let script = format!("touch '{}'", marker.display());
        // Removed: refuse before any tool runs.
        fs::remove_file(&copy).unwrap();
        let failure =
            command_output(&["/bin/sh", "-c", &script], CommandOptions::default()).unwrap_err();
        assert_eq!(failure.state, "failed");
        assert!(
            failure
                .message
                .starts_with("parent-death supervisor unavailable"),
            "{}",
            failure.message
        );
        // Replaced by a different file at the same path: still refused.
        fs::write(&copy, b"#!/bin/sh\nexec \"$@\"\n").unwrap();
        fs::set_permissions(&copy, fs::Permissions::from_mode(0o755)).unwrap();
        let failure =
            command_output(&["/bin/sh", "-c", &script], CommandOptions::default()).unwrap_err();
        assert_eq!(failure.state, "failed");
        assert!(
            failure
                .message
                .starts_with("parent-death supervisor unavailable")
        );
        assert!(!marker.exists(), "a tool ran without a supervisor");
    }

    /// Invoked directly by an ordinary (non-leader) process, or as a leader
    /// with non-pipe descriptors, the supervisor exits non-zero and signals
    /// nothing: its caller and the caller's other children survive.
    #[test]
    fn misused_supervisor_refuses_without_killing_anything() {
        use std::os::unix::process::CommandExt as _;
        let mut sibling = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let script = format!("touch '{}'", marker.display());
        for leader in [false, true] {
            let null = fs::File::open("/dev/null").unwrap();
            let null = null.as_raw_fd();
            let mut command = Command::new(std::env::current_exe().unwrap());
            let protocol = SUPERVISOR_PROTOCOL.to_string();
            command.args([
                SUPERVISE_ARGUMENT,
                "3",
                "4",
                "5",
                "6",
                &protocol,
                "0123456789abcdef0123456789abcdef",
            ]);
            // Even a tool payload on stdin is never read by a refused process.
            let _ = &script;
            command.stdout(Stdio::null()).stderr(Stdio::null());
            // SAFETY: dup2 and setsid are async-signal-safe.
            unsafe {
                command.pre_exec(move || {
                    for fd in [3, 4, 5, 6] {
                        if libc::dup2(null, fd) == -1 {
                            return Err(io::Error::last_os_error());
                        }
                    }
                    if leader && libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let status = command.status().unwrap();
            assert_eq!(status.code(), Some(127), "leader={leader}: {status:?}");
        }
        assert!(!marker.exists());
        // The caller's other child (same process group) is still running.
        assert!(sibling.try_wait().unwrap().is_none());
        sibling.kill().unwrap();
        sibling.wait().unwrap();
    }

    /// Killing the guardian while the tool runs is detected: the command fails
    /// with a structured error and the tool's whole group is retired.
    #[test]
    fn guardian_death_retires_the_group_and_fails_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("pid");
        let script = format!(
            "/bin/sleep 60 & printf '%s\\n' $$ > '{0}.tmp'; mv '{0}.tmp' '{0}'; wait",
            pid_path.display()
        );
        let started = Instant::now();
        let runner = thread::spawn(move || {
            command_output(
                &["/bin/sh", "-c", &script],
                CommandOptions {
                    timeout: Duration::from_secs(60),
                    ..CommandOptions::default()
                },
            )
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        let tool: libc::pid_t = loop {
            if let Ok(raw) = fs::read_to_string(&pid_path)
                && raw.ends_with('\n')
                && let Ok(pid) = raw.trim().parse()
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "tool failed to start");
            thread::sleep(Duration::from_millis(10));
        };
        // The guardian is the group member that is neither the tool nor the
        // tool's sleeping child: the only member whose parent is not the tool.
        let table = Command::new("/bin/ps")
            .args(["-A", "-o", "pid=,ppid=,pgid="])
            .output()
            .unwrap();
        let guardian: libc::pid_t = String::from_utf8_lossy(&table.stdout)
            .lines()
            .filter_map(|line| {
                let fields: Vec<libc::pid_t> = line
                    .split_whitespace()
                    .filter_map(|f| f.parse().ok())
                    .collect();
                (fields.len() == 3 && fields[2] == tool && fields[0] != tool && fields[1] != tool)
                    .then_some(fields[0])
            })
            .next()
            .expect("guardian not found in the tool's group");
        // SAFETY: the PID was just read as a live member of our fixture group.
        assert_eq!(unsafe { libc::kill(guardian, libc::SIGKILL) }, 0);
        let failure = runner.join().unwrap().unwrap_err();
        assert_eq!(failure.state, "failed");
        assert_eq!(failure.message, "parent-death guardian exited unexpectedly");
        assert!(started.elapsed() < Duration::from_secs(30));
        let deadline = Instant::now() + Duration::from_secs(5);
        // SAFETY: signal 0 only probes the retired fixture group.
        while unsafe { libc::kill(-tool, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "tool group survived guardian death"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn supervised_tool_keeps_exact_status_output_and_session() {
        assert!(parent_death_supervised());
        // Exact stdout/stderr bytes and a normal exit code.
        let output = command_output(
            &["/bin/sh", "-c", "printf 'out\\0x'; printf err >&2; exit 0"],
            CommandOptions::default(),
        )
        .unwrap();
        assert_eq!(
            (output.stdout, output.stderr),
            (b"out\0x".to_vec(), b"err".to_vec())
        );
        let failure =
            command_output(&["/bin/sh", "-c", "exit 7"], CommandOptions::default()).unwrap_err();
        assert_eq!(failure.returncode, Some(7));
        // Death by signal is reported as the negative signal number.
        let failure = command_output(
            &["/bin/sh", "-c", "kill -TERM $$"],
            CommandOptions::default(),
        )
        .unwrap_err();
        assert_eq!(failure.returncode, Some(-libc::SIGTERM));
        // The tool (same PID as the supervisor) leads its group and session.
        let output = command_output(
            &["/bin/sh", "-c", "/bin/ps -o pgid= -p $$; echo $$"],
            CommandOptions::default(),
        )
        .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        let lines: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(lines[0], lines[1]);
        // A missing tool is still the ordinary unavailable failure.
        let failure = command_output(
            &["catabolic-no-such-tool-fixture"],
            CommandOptions::default(),
        )
        .unwrap_err();
        assert_eq!(failure.state, "unavailable");
        assert_eq!(failure.io_error.unwrap().kind(), io::ErrorKind::NotFound);
    }

    /// The tool stops itself and ignores SIGHUP, so killing the worker orphans
    /// a stopped group: the kernel sends SIGHUP then SIGCONT to every member.
    /// The guardian must survive that and still kill the tool and grandchild.
    #[test]
    fn stopped_hup_ignoring_group_is_killed_after_worker_death() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("pids");
        let mut worker = fixture_process(
            "process::tests::stopped_group_fixture",
            &[("CATABOLIC_GUARDIAN_FIXTURE", &pid_path)],
        )
        .spawn()
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let group: libc::pid_t = loop {
            if let Ok(raw) = fs::read_to_string(&pid_path)
                && raw.ends_with('\n')
                && let Some(pid) = raw.split_whitespace().next()
                && let Ok(pid) = pid.parse::<libc::pid_t>()
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "tool failed to start");
            thread::sleep(Duration::from_millis(10));
        };
        // Wait until the tool has actually stopped.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = Command::new("/bin/ps")
                .args(["-o", "stat=", "-p", &group.to_string()])
                .output()
                .unwrap();
            if String::from_utf8_lossy(&state.stdout)
                .trim_start()
                .starts_with('T')
            {
                break;
            }
            assert!(Instant::now() < deadline, "tool never stopped");
            thread::sleep(Duration::from_millis(10));
        }
        worker.kill().unwrap();
        worker.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        // SAFETY: signal 0 only probes the fixture group for live members.
        while unsafe { libc::kill(-group, 0) } == 0 {
            if Instant::now() >= deadline {
                // SAFETY: failure-path cleanup of our own fixture group.
                unsafe {
                    libc::kill(-group, libc::SIGKILL);
                }
                panic!("orphaned stopped group survived worker death");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn stopped_group_fixture() {
        let Some(path) = std::env::var_os("CATABOLIC_GUARDIAN_FIXTURE") else {
            return;
        };
        command_output(
            &[
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from(
                    "trap '' HUP; /bin/sleep 60 & child=$!; printf '%s %s\\n' $$ $child > \"$1.tmp\"; mv \"$1.tmp\" \"$1\"; kill -STOP $$; wait",
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

    /// From a process with a controlling terminal, opening /dev/tty fails
    /// immediately for the tool (a new session), as with frozen Python's
    /// `start_new_session=True`, instead of stopping on SIGTTIN.
    #[test]
    fn tool_has_no_controlling_terminal() {
        let mut primary = -1;
        let mut secondary = -1;
        // SAFETY: openpty writes two descriptors; null name/termios/winsize.
        let opened = unsafe {
            libc::openpty(
                &mut primary,
                &mut secondary,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0);
        // SAFETY: the descriptors were just created and are owned here.
        let (primary, secondary) = unsafe {
            (
                OwnedFd::from_raw_fd(primary),
                OwnedFd::from_raw_fd(secondary),
            )
        };
        let dir = tempfile::tempdir().unwrap();
        let result = dir.path().join("tty");
        let tty = secondary.as_raw_fd();
        let mut command = fixture_process(
            "process::tests::controlling_terminal_fixture",
            &[("CATABOLIC_TTY_FIXTURE", &result)],
        );
        // SAFETY: setsid and ioctl are async-signal-safe; the fixture gets the
        // pseudo-terminal as its controlling terminal.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 || libc::ioctl(tty, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let started = Instant::now();
        let status = command.status().unwrap();
        drop(secondary);
        drop(primary);
        assert!(status.success(), "{status:?}");
        assert!(started.elapsed() < Duration::from_secs(15));
        assert_eq!(
            fs::read_to_string(&result).unwrap(),
            "fixture-tty-ok failed"
        );
    }

    #[test]
    fn controlling_terminal_fixture() {
        let Some(path) = std::env::var_os("CATABOLIC_TTY_FIXTURE") else {
            return;
        };
        // The fixture itself can open its controlling terminal.
        let own = std::fs::OpenOptions::new()
            .read(true)
            .open("/dev/tty")
            .is_ok();
        let output = command_output(
            &[
                "/bin/sh",
                "-c",
                "if (: </dev/tty) 2>/dev/null; then printf opened; else printf failed; fi",
            ],
            CommandOptions {
                timeout: Duration::from_secs(10),
                ..CommandOptions::default()
            },
        )
        .unwrap();
        fs::write(
            path,
            format!(
                "{} {}",
                if own {
                    "fixture-tty-ok"
                } else {
                    "fixture-tty-missing"
                },
                String::from_utf8_lossy(&output.stdout)
            ),
        )
        .unwrap();
    }

    #[test]
    fn guardian_leaves_no_process_after_normal_and_failed_commands() {
        // Normal retirement kills the group, guardian included.
        let output = command_output(
            &["/bin/sh", "-c", "/bin/ps -o pgid= -p $$"],
            CommandOptions::default(),
        )
        .unwrap();
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
