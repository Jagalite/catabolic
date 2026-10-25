//! Disabled Linux x86_64 kernel-policy candidate for the fixed software-tool
//! cohort. This is not a packaged-tool grant or a native cleanup witness.
//! No production caller may enable it until packaging and adversarial tests
//! qualify the complete execution path, including exact wait/descriptor joins.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(super) mod linux {
    use std::io;
    const LD: u16 = 0x20;
    const JEQ: u16 = 0x15;
    const JSET: u16 = 0x45;
    const RET: u16 = 0x06;
    const ALLOW: u32 = 0x7fff0000;
    const KILL: u32 = 0x80000000;
    const DENY: u32 = 0x00050000 | libc::EPERM as u32;
    const ENOSYS: u32 = 0x00050000 | libc::ENOSYS as u32;
    const ARCH_X86_64: u32 = 0xc000003e;
    const THREAD_FLAGS: u32 = (libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM
        | libc::CLONE_SETTLS
        | libc::CLONE_PARENT_SETTID
        | libc::CLONE_CHILD_CLEARTID) as u32;
    fn stmt(code: u16, k: u32) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k,
        }
    }
    fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
        libc::sock_filter { code, jt, jf, k }
    }
    /// A sealed static image is only a loader-closure mechanism, NOT proof that
    /// a particular FFmpeg build/configuration is approved. No production tool
    /// authority is constructed here; packaging qualification remains separate.
    pub(crate) struct StaticImage(std::fs::File);
    impl StaticImage {
        pub(crate) fn seal(bytes: &[u8]) -> io::Result<Self> {
            use std::io::Write;
            use std::os::fd::{AsRawFd, FromRawFd};
            if bytes.len() < 64
                || bytes.len() > 128 * 1024 * 1024
                || &bytes[..4] != b"\x7fELF"
                || bytes[4] != 2
                || bytes[5] != 1
                || u16::from_le_bytes([bytes[18], bytes[19]]) != 62
            {
                return Err(io::Error::other("static native x86_64 ELF required"));
            }
            let offset = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
            let size = u16::from_le_bytes(bytes[54..56].try_into().unwrap()) as usize;
            let count = u16::from_le_bytes(bytes[56..58].try_into().unwrap()) as usize;
            if size != 56 || count == 0 || count > 256 {
                return Err(io::Error::other("unsupported ELF program headers"));
            }
            let offset: usize = offset
                .try_into()
                .map_err(|_| io::Error::other("ELF offset overflow"))?;
            let end = offset
                .checked_add(size * count)
                .ok_or_else(|| io::Error::other("ELF header overflow"))?;
            if end > bytes.len() {
                return Err(io::Error::other("truncated ELF headers"));
            }
            for header in bytes[offset..end].chunks_exact(size) {
                let kind = u32::from_le_bytes(header[..4].try_into().unwrap());
                if kind == 2 || kind == 3 {
                    return Err(io::Error::other(
                        "dynamic loader/dependency closure is not qualified",
                    ));
                }
            }
            let fd = unsafe {
                libc::memfd_create(
                    c"catabolic-static-tool".as_ptr(),
                    libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            file.write_all(bytes)?;
            if unsafe {
                libc::fcntl(
                    file.as_raw_fd(),
                    libc::F_ADD_SEALS,
                    libc::F_SEAL_WRITE
                        | libc::F_SEAL_GROW
                        | libc::F_SEAL_SHRINK
                        | libc::F_SEAL_SEAL,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(file))
        }
        pub(crate) fn fd(&self) -> i32 {
            use std::os::fd::AsRawFd;
            self.0.as_raw_fd()
        }
    }

    /// Default deny. Native ABI only; x32/compat cannot select another syscall
    /// namespace. No fork, vfork, clone3, session/group escape, namespaces,
    /// ptrace/process_vm, pidfd broker, networking, io_uring, BPF or keyrings.
    pub(crate) fn filter(exec_fd: i32) -> Vec<libc::sock_filter> {
        filter_mode(exec_fd, false)
    }
    #[cfg(test)]
    pub(super) fn installed_tool_fixture_filter(exec_fd: i32) -> Vec<libc::sock_filter> {
        filter_mode(exec_fd, true)
    }
    fn filter_mode(exec_fd: i32, dynamic_fixture: bool) -> Vec<libc::sock_filter> {
        let mut p = vec![
            stmt(LD, 4),
            jump(JEQ, ARCH_X86_64, 1, 0),
            stmt(RET, KILL),
            stmt(LD, 0),
            jump(JSET, 0x40000000, 0, 1),
            stmt(RET, KILL),
        ];
        // clone3 carries a pointer, so never inspect/race its pointed arguments.
        p.extend([jump(JEQ, libc::SYS_clone3 as u32, 0, 1), stmt(RET, ENOSYS)]);
        // Exactly glibc's approved pthread flags, high word zero. No process
        // clone and no alternate exit signal can slip through a flag mask.
        p.extend([
            jump(JEQ, libc::SYS_clone as u32, 0, 7),
            stmt(LD, 20),
            jump(JEQ, 0, 1, 0),
            stmt(RET, DENY),
            stmt(LD, 16),
            jump(JEQ, THREAD_FLAGS, 0, 1),
            stmt(RET, ALLOW),
            stmt(RET, DENY),
        ]);
        // This checks only fd and flags, NOT the pointed pathname. An absolute
        // pathname can ignore dirfd; reexec retains confinement but this is NOT
        // executable identity authority. A trusted startup deny-exec stage is
        // still required for any fixed-package identity claim.
        p.extend([
            jump(JEQ, libc::SYS_execveat as u32, 0, 10),
            stmt(LD, 20),
            jump(JEQ, 0, 1, 0),
            stmt(RET, DENY),
            stmt(LD, 16),
            jump(JEQ, exec_fd as u32, 1, 0),
            stmt(RET, DENY),
            stmt(LD, 48),
            jump(JEQ, libc::AT_EMPTY_PATH as u32, 0, 1),
            stmt(RET, ALLOW),
            stmt(RET, DENY),
        ]);
        // mmap/mprotect cannot introduce executable pages after initial exec.
        for syscall in [libc::SYS_mmap, libc::SYS_mprotect] {
            if dynamic_fixture {
                p.extend([jump(JEQ, syscall as u32, 0, 1), stmt(RET, ALLOW)]);
                continue;
            }
            p.extend([
                jump(JEQ, syscall as u32, 0, 4),
                stmt(LD, 32),
                jump(JSET, libc::PROT_EXEC as u32, 0, 1),
                stmt(RET, DENY),
                stmt(RET, ALLOW),
            ]);
        }
        if dynamic_fixture {
            // Test-only dynamic glibc startup: query limits only. new_limit
            // pointer must be NULL in both words; changing limits stays denied.
            p.extend([
                jump(JEQ, libc::SYS_prlimit64 as u32, 0, 7),
                stmt(LD, 32),
                jump(JEQ, 0, 1, 0),
                stmt(RET, DENY),
                stmt(LD, 36),
                jump(JEQ, 0, 0, 1),
                stmt(RET, ALLOW),
                stmt(RET, DENY),
            ]);
        }
        // No prctl can reset PDEATHSIG or alter seccomp. The initial fresh
        // single-thread child sets both before installing this inherited filter.
        for syscall in [
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_readv,
            libc::SYS_writev,
            libc::SYS_pread64,
            libc::SYS_pwrite64,
            libc::SYS_close,
            libc::SYS_fstat,
            libc::SYS_newfstatat,
            libc::SYS_statx,
            libc::SYS_lseek,
            libc::SYS_openat,
            libc::SYS_access,
            libc::SYS_faccessat,
            libc::SYS_readlink,
            libc::SYS_readlinkat,
            libc::SYS_getdents64,
            libc::SYS_munmap,
            libc::SYS_madvise,
            libc::SYS_brk,
            libc::SYS_mremap,
            libc::SYS_futex,
            libc::SYS_set_tid_address,
            libc::SYS_set_robust_list,
            libc::SYS_rseq,
            libc::SYS_arch_prctl,
            libc::SYS_rt_sigaction,
            libc::SYS_rt_sigprocmask,
            libc::SYS_rt_sigreturn,
            libc::SYS_sigaltstack,
            libc::SYS_clock_gettime,
            libc::SYS_clock_nanosleep,
            libc::SYS_nanosleep,
            libc::SYS_gettimeofday,
            libc::SYS_times,
            libc::SYS_getrusage,
            libc::SYS_getpid,
            libc::SYS_gettid,
            libc::SYS_getppid,
            libc::SYS_getuid,
            libc::SYS_geteuid,
            libc::SYS_getgid,
            libc::SYS_getegid,
            libc::SYS_uname,
            libc::SYS_sysinfo,
            libc::SYS_sched_getaffinity,
            libc::SYS_sched_yield,
            libc::SYS_getrandom,
            libc::SYS_poll,
            libc::SYS_ppoll,
            libc::SYS_select,
            libc::SYS_pselect6,
            libc::SYS_fcntl,
            libc::SYS_dup,
            libc::SYS_dup2,
            libc::SYS_dup3,
            libc::SYS_fsync,
            libc::SYS_fdatasync,
            libc::SYS_ftruncate,
            libc::SYS_exit,
            libc::SYS_exit_group,
        ] {
            p.extend([jump(JEQ, syscall as u32, 0, 1), stmt(RET, ALLOW)]);
        }
        p.push(stmt(RET, DENY));
        p
    }
    /// Disabled capability broker policy. No direct path opens, asynchronous
    /// fcntl signaling, or descriptor acquisition via brokers. Initial exec
    /// retains the documented pathname/code-origin limitation.
    pub(super) fn broker_filter(exec_fd: i32) -> Vec<libc::sock_filter> {
        let mut p = filter_mode(exec_fd, true);
        // Insert after architecture/x32 checks, before ordinary syscall rules.
        let mut gates = Vec::new();
        for nr in [libc::SYS_open, libc::SYS_openat] {
            gates.extend([jump(JEQ, nr as u32, 0, 1), stmt(RET, 0x7fc00000)]);
        }
        gates.extend([jump(JEQ, libc::SYS_openat2 as u32, 0, 1), stmt(RET, ENOSYS)]);
        // Only descriptor flag queries and setting FD_CLOEXEC are supported.
        // F_SETOWN/F_SETSIG/F_NOTIFY/F_SETLEASE/F_SETFL stay denied.
        gates.extend([
            jump(JEQ, libc::SYS_fcntl as u32, 0, 13),
            stmt(LD, 28),
            jump(JEQ, 0, 1, 0),
            stmt(RET, DENY),
            stmt(LD, 24),
            jump(JEQ, libc::F_GETFD as u32, 6, 0),
            jump(JEQ, libc::F_GETFL as u32, 5, 0),
            jump(JEQ, libc::F_SETFD as u32, 1, 0),
            stmt(RET, DENY),
            stmt(LD, 32),
            jump(JEQ, libc::FD_CLOEXEC as u32, 1, 0),
            stmt(RET, DENY),
            stmt(RET, ALLOW),
            stmt(RET, DENY),
        ]);
        p.splice(6..6, gates);
        p
    }
    #[cfg(test)]
    pub(in crate::process) fn broker_bootstrap_filter(exec_fd: i32, socket_fd: i32) -> Vec<libc::sock_filter> {
        let mut p = broker_filter(exec_fd);
        let gates = vec![
            jump(JEQ, libc::SYS_sendmsg as u32, 0, 4),
            stmt(LD, 16),
            jump(JEQ, socket_fd as u32, 0, 1),
            stmt(RET, ALLOW),
            stmt(RET, DENY),
            jump(JEQ, libc::SYS_seccomp as u32, 0, 7),
            stmt(LD, 16),
            jump(JEQ, 1, 1, 0),
            stmt(RET, DENY),
            stmt(LD, 24),
            jump(JEQ, 0, 0, 1),
            stmt(RET, ALLOW),
            stmt(RET, DENY),
        ];
        p.splice(6..6, gates);
        p
    }
    #[cfg(test)]
    pub(in crate::process) unsafe fn install_listener(program: &mut [libc::sock_filter]) -> io::Result<i32> {
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let p = libc::sock_fprog {
            len: program.len() as u16,
            filter: program.as_mut_ptr(),
        };
        let fd = unsafe {
            libc::syscall(libc::SYS_seccomp, 1, 8 /* NEW_LISTENER */, &p)
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(fd as i32)
        }
    }
    #[cfg(test)]
    pub(in crate::process) unsafe fn finish_broker_bootstrap() -> io::Result<()> {
        let mut p = [
            stmt(LD, 0),
            jump(JEQ, libc::SYS_sendmsg as u32, 2, 0),
            jump(JEQ, libc::SYS_seccomp as u32, 1, 0),
            stmt(RET, ALLOW),
            stmt(RET, DENY),
        ];
        let prog = libc::sock_fprog {
            len: p.len() as u16,
            filter: p.as_mut_ptr(),
        };
        if unsafe { libc::syscall(libc::SYS_seccomp, 1, 0, &prog) } < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    /// Called only by trusted preexec after all other child setup, with no
    /// allocation between this filter installation and exact descriptor exec.
    pub(crate) unsafe fn install(program: &mut [libc::sock_filter]) -> io::Result<()> {
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let p = libc::sock_fprog {
            len: program
                .len()
                .try_into()
                .map_err(|_| io::Error::other("seccomp policy too large"))?,
            filter: program.as_mut_ptr(),
        };
        if unsafe { libc::prctl(libc::PR_SET_SECCOMP, 2, &p) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        // Tiny interpreter verifies branches without applying a host filter.
        fn action(nr: u32, arch: u32, args: [u64; 6]) -> u32 {
            evaluate(filter(42), nr, arch, args)
        }
        fn evaluate(p: Vec<libc::sock_filter>, nr: u32, arch: u32, args: [u64; 6]) -> u32 {
            let mut data = [0u8; 64];
            data[0..4].copy_from_slice(&nr.to_ne_bytes());
            data[4..8].copy_from_slice(&arch.to_ne_bytes());
            for (i, a) in args.iter().enumerate() {
                data[16 + 8 * i..24 + 8 * i].copy_from_slice(&a.to_ne_bytes());
            }
            let (mut pc, mut acc) = (0, 0);
            loop {
                let i = &p[pc];
                match i.code {
                    LD => {
                        acc = u32::from_ne_bytes(
                            data[i.k as usize..i.k as usize + 4].try_into().unwrap(),
                        );
                        pc += 1
                    }
                    JEQ => {
                        pc += 1 + if acc == i.k {
                            i.jt as usize
                        } else {
                            i.jf as usize
                        }
                    }
                    JSET => {
                        pc += 1 + if acc & i.k != 0 {
                            i.jt as usize
                        } else {
                            i.jf as usize
                        }
                    }
                    RET => return i.k,
                    _ => panic!("instruction"),
                }
            }
        }
        #[test]
        fn broker_policy_notifies_opens_and_denies_brokers_and_fcntl_signals() {
            for nr in [libc::SYS_open, libc::SYS_openat] {
                assert_eq!(
                    evaluate(broker_filter(42), nr as u32, ARCH_X86_64, [0; 6]),
                    0x7fc00000
                );
            }
            assert_eq!(
                evaluate(
                    broker_filter(42),
                    libc::SYS_openat2 as u32,
                    ARCH_X86_64,
                    [0; 6]
                ),
                ENOSYS
            );
            for command in [
                libc::F_SETOWN,
                10 /* Linux UAPI F_SETSIG; not exported by this libc crate */,
                libc::F_NOTIFY,
                libc::F_SETLEASE,
                libc::F_SETFL,
                libc::F_DUPFD,
            ] {
                let mut args = [0; 6];
                args[1] = command as u64;
                assert_eq!(
                    evaluate(broker_filter(42), libc::SYS_fcntl as u32, ARCH_X86_64, args),
                    DENY
                );
            }
            for nr in [
                libc::SYS_sendmsg,
                libc::SYS_recvmsg,
                libc::SYS_ioctl,
                libc::SYS_io_uring_setup,
                libc::SYS_kill,
                libc::SYS_ptrace,
                libc::SYS_socket,
                libc::SYS_fork,
                libc::SYS_renameat,
                libc::SYS_unlinkat,
            ] {
                assert_eq!(
                    evaluate(broker_filter(42), nr as u32, ARCH_X86_64, [0; 6]),
                    DENY
                );
            }
        }
        #[test]
        fn policy_denies_process_escape_and_allows_only_exact_threads() {
            for syscall in [
                libc::SYS_fork,
                libc::SYS_vfork,
                libc::SYS_setsid,
                libc::SYS_setpgid,
                libc::SYS_setns,
                libc::SYS_unshare,
                libc::SYS_ptrace,
                libc::SYS_process_vm_writev,
                libc::SYS_socket,
                libc::SYS_prctl,
                libc::SYS_execve,
            ] {
                assert_eq!(
                    action(syscall as u32, ARCH_X86_64, [0; 6]),
                    DENY,
                    "{syscall}"
                );
            }
            assert_eq!(action(libc::SYS_clone3 as u32, ARCH_X86_64, [0; 6]), ENOSYS);
            let mut args = [0; 6];
            args[0] = THREAD_FLAGS as u64;
            assert_eq!(action(libc::SYS_clone as u32, ARCH_X86_64, args), ALLOW);
            for flags in [
                0,
                libc::SIGCHLD as u64,
                THREAD_FLAGS as u64 | (1 << 32),
                THREAD_FLAGS as u64 & !(libc::CLONE_THREAD as u64),
            ] {
                args[0] = flags;
                assert_eq!(action(libc::SYS_clone as u32, ARCH_X86_64, args), DENY);
            }
            assert_eq!(action(libc::SYS_read as u32, 0x40000003, [0; 6]), KILL);
            assert_eq!(action(0x40000000, ARCH_X86_64, [0; 6]), KILL);
        }
        #[test]
        fn policy_pins_initial_exec_and_disallows_new_executable_pages() {
            let mut args = [0; 6];
            args[0] = 42;
            args[4] = libc::AT_EMPTY_PATH as u64;
            assert_eq!(action(libc::SYS_execveat as u32, ARCH_X86_64, args), ALLOW);
            args[0] = 43;
            assert_eq!(action(libc::SYS_execveat as u32, ARCH_X86_64, args), DENY);
            args = [0; 6];
            args[2] = libc::PROT_EXEC as u64;
            assert_eq!(action(libc::SYS_mmap as u32, ARCH_X86_64, args), DENY);
            assert_eq!(action(libc::SYS_mprotect as u32, ARCH_X86_64, args), DENY);
        }
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod kernel_tests {
    use super::linux;
    use std::{
        fs,
        os::unix::process::CommandExt,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    pub(super) fn fixture(root: &std::path::Path, dynamic: bool) -> std::path::PathBuf {
        let source = root.join("cohort.c");
        let binary = root.join("cohort");
        fs::write(&source,r#"
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <unistd.h>
static void *thread(void *p) { *(int*)p=7; return 0; }
#define DENIED(x) do {errno=0;if((x)!=-1 || errno!=EPERM)return __LINE__;}while(0)
int main(void){
 DENIED(syscall(SYS_fork)); DENIED(syscall(SYS_vfork));
 errno=0;if(syscall(SYS_clone3,0,0)!=-1 || errno!=ENOSYS)return 20;
 DENIED(setsid());DENIED(setpgid(0,0));DENIED(unshare(CLONE_NEWUSER));DENIED(setns(-1,0));
 DENIED(prctl(PR_SET_PDEATHSIG,0));DENIED(socket(AF_UNIX,SOCK_STREAM,0));
#ifndef DYNAMIC_FIXTURE
 errno=0;if(mmap(0,4096,PROT_READ|PROT_EXEC,MAP_PRIVATE|MAP_ANONYMOUS,-1,0)!=MAP_FAILED || errno!=EPERM)return 30;
#endif
 pthread_t t;int value=0;if(pthread_create(&t,0,thread,&value)!=0)return 31;
 if(pthread_join(t,0)!=0 || value!=7)return 32;
 write(1,"restricted-ok\n",14);return 0;
}
"#).unwrap();
        let status = Command::new("cc")
            .args(["-static", "-pthread", "-O2"])
            .arg(if dynamic {
                "-DDYNAMIC_FIXTURE"
            } else {
                "-USTATIC_UNUSED"
            })
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .status()
            .expect("static C compiler is required for explicit kernel qualification");
        assert!(
            status.success(),
            "static C fixture must build; no dynamic fallback"
        );
        binary
    }
    #[test]
    #[ignore = "explicit Linux seccomp/static compiler qualification; parent runs"]
    fn inherited_policy_blocks_process_escape_but_allows_real_pthread() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fixture(dir.path(), false);
        let bytes = fs::read(binary).unwrap();
        let executable = linux::StaticImage::seal(&bytes).unwrap();
        let fd = executable.fd();
        let mut policy = linux::filter(fd);
        let name = std::ffi::CString::new("qualified-fixture").unwrap();
        let mut command = Command::new("/never-used");
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear();
        // This fixture owns the exact Child, with no detached guardian. The
        // initial fd exec occurs only after inherited no-new-privs+seccomp.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                linux::install(&mut policy)?;
                let args = [name.as_ptr(), std::ptr::null()];
                let env = [std::ptr::null::<libc::c_char>()];
                libc::syscall(
                    libc::SYS_execveat,
                    fd,
                    c"".as_ptr(),
                    args.as_ptr(),
                    env.as_ptr(),
                    libc::AT_EMPTY_PATH,
                );
                Err(std::io::Error::last_os_error())
            });
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let status = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            assert_eq!(status, 0);
            if unsafe { info.si_pid() } != 0 {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("restricted child timeout");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "status={:?} stderr={:?}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"restricted-ok\n");
    }
}

/// Compatibility experiment ONLY. Installed dynamic tools and their dependencies
/// are mutable and not a packaged identity/cleanup grant. Nothing here enters
/// native processing, acquires claims, publishes artifacts, or retires lifetimes.
#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod installed_tool_tests {
    use super::linux;
    use std::{
        ffi::CString,
        fs,
        os::{fd::AsRawFd, unix::process::CommandExt},
        path::Path,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    fn run(root: &Path, label: &str, tool: &Path, args: &[&str], cancel: bool) -> Vec<u8> {
        let executable = fs::File::open(tool).expect("explicit installed tool required");
        let fd = executable.as_raw_fd();
        let mut policy = linux::installed_tool_fixture_filter(fd);
        let argv: Vec<CString> = std::iter::once(tool.to_str().unwrap())
            .chain(args.iter().copied())
            .map(|s| CString::new(s).unwrap())
            .collect();
        let output = root.join(format!("{label}.stdout"));
        let error = root.join(format!("{label}.stderr"));
        let mut command = Command::new("/never-used");
        command
            .current_dir(root)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(fs::File::create(&output).unwrap())
            .stderr(fs::File::create(&error).unwrap());
        unsafe {
            command.pre_exec(move || {
                // Prebuilt strings, fixed empty environment, no guardian. The
                // vector allocation happens before installing the filter.
                let mut pointers: Vec<_> = argv.iter().map(|s| s.as_ptr()).collect();
                pointers.push(std::ptr::null());
                let env = [std::ptr::null::<libc::c_char>()];
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                linux::install(&mut policy)?;
                libc::syscall(
                    libc::SYS_execveat,
                    fd,
                    c"".as_ptr(),
                    pointers.as_ptr(),
                    env.as_ptr(),
                    libc::AT_EMPTY_PATH,
                );
                Err(std::io::Error::last_os_error())
            });
        }
        let mut child = command.spawn().expect("restricted installed-tool spawn");
        let start = Instant::now();
        let mut killed = false;
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            assert_eq!(
                rc,
                0,
                "exact wait failed: {}",
                std::io::Error::last_os_error()
            );
            if unsafe { info.si_pid() } != 0 {
                break;
            }
            if cancel && !killed && start.elapsed() > Duration::from_millis(300) {
                child.kill().expect("exact owned child cancellation");
                killed = true;
            }
            if start.elapsed() > Duration::from_secs(30) {
                // Test cleanup only, never returns a native retirement witness.
                let _ = child.kill();
                let _ = child.wait();
                panic!("{label}: bounded compatibility experiment timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = child.wait().expect("exact owned child reap");
        let stderr = fs::read_to_string(error).unwrap();
        if cancel {
            assert!(
                killed && !status.success(),
                "cancel did not exercise live child: {stderr}"
            );
        } else {
            assert!(status.success(), "{label}: {status}: {stderr}");
        }
        fs::read(output).unwrap()
    }
    fn provenance(tool: &Path) {
        // Observational provenance, not verification or an immutable closure.
        for (program, args) in [
            ("sha256sum", vec![tool.to_str().unwrap()]),
            ("ldd", vec![tool.to_str().unwrap()]),
        ] {
            let output = Command::new(program)
                .args(args)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .output()
                .unwrap();
            assert!(output.status.success());
            eprintln!(
                "UNQUALIFIED installed provenance {program}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            if program == "ldd" {
                for path in String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .filter(|s| s.starts_with('/'))
                {
                    let hash = Command::new("/usr/bin/sha256sum")
                        .arg(path)
                        .output()
                        .unwrap();
                    assert!(hash.status.success());
                    eprintln!(
                        "UNQUALIFIED dependency: {}",
                        String::from_utf8_lossy(&hash.stdout)
                    );
                }
            }
        }
    }
    #[test]
    #[ignore = "explicit installed dynamic FFmpeg compatibility experiment, NOT authority; parent runs"]
    fn installed_ffmpeg_encode_probe_cancel_under_inherited_filter() {
        let root = tempfile::tempdir().unwrap();
        let ffmpeg = Path::new("/usr/bin/ffmpeg");
        let ffprobe = Path::new("/usr/bin/ffprobe");
        let adversary = super::kernel_tests::fixture(root.path(), true);
        assert_eq!(
            run(root.path(), "adversary", &adversary, &[], false),
            b"restricted-ok\n"
        );
        provenance(ffmpeg);
        provenance(ffprobe);
        let samples = 8000u32;
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend((36 + samples * 2).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(16000u32.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend((samples * 2).to_le_bytes());
        wav.resize(44 + samples as usize * 2, 0);
        fs::write(root.path().join("input.wav"), wav).unwrap();
        let mut ppm = b"P6\n32 32\n255\n".to_vec();
        ppm.extend(vec![128u8; 32 * 32 * 3]);
        fs::write(root.path().join("input.ppm"), ppm).unwrap();
        run(
            root.path(),
            "audio",
            ffmpeg,
            &[
                "-nostdin",
                "-v",
                "error",
                "-i",
                "input.wav",
                "-threads",
                "2",
                "-c:a",
                "flac",
                "audio.flac",
            ],
            false,
        );
        run(
            root.path(),
            "video",
            ffmpeg,
            &[
                "-nostdin",
                "-v",
                "error",
                "-loop",
                "1",
                "-i",
                "input.ppm",
                "-t",
                "1",
                "-threads",
                "2",
                "-filter_threads",
                "2",
                "-c:v",
                "mpeg4",
                "-pix_fmt",
                "yuv420p",
                "video.mp4",
            ],
            false,
        );
        for file in ["audio.flac", "video.mp4"] {
            let output = run(
                root.path(),
                file,
                ffprobe,
                &[
                    "-v",
                    "error",
                    "-show_streams",
                    "-show_format",
                    "-of",
                    "json",
                    file,
                ],
                false,
            );
            let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert!(!value["streams"].as_array().unwrap().is_empty());
        }
        run(
            root.path(),
            "cancel",
            ffmpeg,
            &[
                "-nostdin",
                "-v",
                "error",
                "-re",
                "-stream_loop",
                "-1",
                "-i",
                "input.wav",
                "-threads",
                "2",
                "-c:a",
                "flac",
                "cancel.flac",
            ],
            true,
        );
    }
}
