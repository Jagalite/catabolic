//! Unreachable invocation-custody groundwork. No native cleanup witness, schema
//! transition, artifact authority, or public host entry point is issued here.
//! A process-retained supervisor owns leases even if the caller handle disappears.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
use catabolic_core::catalog_host_lease::AuthorizationLease;
use std::{
    collections::BTreeMap,
    io,
    process::{Child, ExitStatus},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
};

/// Exact durable claim identity must eventually originate from the native claim
/// owner, not JSON or an application job ID. Only fixtures can construct custody
/// until the complete external-child obligation protocol is qualified.
struct Identity {
    catalog: String,
    incarnation: String,
    job: String,
    generation: i64,
    attempt: i64,
    claim_nonce: String,
    snapshot: String,
    options: String,
}
struct Custody {
    identity: Identity,
    child: Child,
    workers: Vec<JoinHandle<io::Result<()>>>,
    cancel: Arc<AtomicBool>,
    authorization: AuthorizationLease,
    operation_permit: Box<dyn Send>,
    // Extra descriptors/resources remain owned until all workers have joined.
    resources: Vec<Box<dyn Send>>,
    reaped: Option<ExitStatus>,
    poisoned: Option<String>,
}
static SUPERVISOR: OnceLock<Mutex<BTreeMap<u64, Custody>>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);
fn supervisor() -> &'static Mutex<BTreeMap<u64, Custody>> {
    SUPERVISOR.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Caller-local handle, not the owner of authorization or capacity. Dropping it
/// requests cancellation; it cannot detach workers or release their guards.
struct Invocation {
    id: u64,
    cancel: Arc<AtomicBool>,
}
impl Drop for Invocation {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
#[derive(Debug)]
enum Progress {
    Pending,
    /// Includes ECHILD, worker panic/error, and invalid ownership. The retained
    /// supervisor keeps all remaining handles and guards for reconciliation.
    Unresolved(String),
    /// Diagnostic only. Never substitute this for native durable cleanup proof.
    Retired(ExitStatus),
}
impl Invocation {
    fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    fn poll(&self) -> io::Result<Progress> {
        poll(self.id)
    }
}
fn poll(id: u64) -> io::Result<Progress> {
    let mut all = supervisor()
        .lock()
        .map_err(|_| io::Error::other("custody supervisor poisoned; ownership retained"))?;
    let custody = all
        .get_mut(&id)
        .ok_or_else(|| io::Error::other("unknown invocation custody"))?;
    if custody.reaped.is_none() {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                custody.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                custody.poisoned = Some(format!("exact waitid failed: {error}"));
            }
        } else if unsafe { info.si_pid() } == custody.child.id() as i32 {
            match custody.child.wait() {
                Ok(status) => custody.reaped = Some(status),
                Err(error) => custody.poisoned = Some(format!("exact child reap failed: {error}")),
            }
        } else if custody.cancel.load(Ordering::Acquire) {
            // Exact Child only. No PID scan, process-group inference or guardian.
            // Failure is not completion; it retains custody for reconciliation.
            if let Err(error) = custody.child.kill() {
                custody.poisoned = Some(format!("exact child cancellation failed: {error}"));
            }
        }
    }
    let mut index = 0;
    while index < custody.workers.len() {
        if !custody.workers[index].is_finished() {
            index += 1;
            continue;
        }
        let worker = custody.workers.swap_remove(index);
        match worker.join() {
            Ok(Ok(())) => (),
            Ok(Err(error)) => custody.poisoned = Some(format!("worker cleanup failed: {error}")),
            Err(_) => custody.poisoned = Some("worker panicked; cleanup unresolved".into()),
        }
    }
    if let Some(reason) = &custody.poisoned {
        return Ok(Progress::Unresolved(reason.clone()));
    }
    if let Some(status) = custody.reaped {
        if custody.workers.is_empty() {
            // Dropping these owned guards is permitted only after exact wait
            // and every registered worker join. This does not say all external
            // effects of a job are closed; production construction is disabled.
            all.remove(&id);
            return Ok(Progress::Retired(status));
        }
    }
    Ok(Progress::Pending)
}

// No production constructor. The future qualified spawn owner must reserve its
// capacity/authorization before spawning and atomically transfer every child,
// broker, heartbeat, reader and descriptor into this retained custody.
#[cfg(test)]
fn adopt_fixture(custody: Custody) -> Invocation {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let cancel = custody.cancel.clone();
    supervisor().lock().unwrap().insert(id, custody);
    Invocation { id, cancel }
}

#[cfg(test)]
mod tests {
    use super::*;
    use catabolic_core::catalog_host_lease::{HostAuthorization, LeaseError};
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    struct Permit(Arc<AtomicBool>);
    impl Drop for Permit {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    fn fixture(
        child: Child,
        worker: JoinHandle<io::Result<()>>,
    ) -> (Invocation, HostAuthorization, Arc<AtomicBool>) {
        let gate = HostAuthorization::new("fixture".into(), "1".into()).unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let invocation = adopt_fixture(Custody {
            identity: Identity {
                catalog: "test".into(),
                incarnation: "test".into(),
                job: "test".into(),
                generation: 1,
                attempt: 1,
                claim_nonce: "test-only".into(),
                snapshot: "test".into(),
                options: "test".into(),
            },
            child,
            workers: vec![worker],
            cancel: Arc::new(AtomicBool::new(false)),
            authorization: gate.acquire("1").unwrap(),
            operation_permit: Box::new(Permit(dropped.clone())),
            resources: vec![],
            reaped: None,
            poisoned: None,
        });
        (invocation, gate, dropped)
    }
    #[test]
    fn caller_drop_keeps_actual_authorization_and_capacity_until_exact_join() {
        let child = Command::new("/bin/sleep").arg("5").spawn().unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            release_rx.recv().unwrap();
            Ok(())
        });
        let (invocation, gate, dropped) = fixture(child, worker);
        let id = invocation.id;
        let revocation = gate.begin_revocation().unwrap();
        drop(invocation); // Cancellation is not cleanup; supervisor retains all.
        assert!(!dropped.load(Ordering::Acquire));
        assert_eq!(
            revocation.wait_drained(Duration::ZERO),
            Err(LeaseError::Timeout)
        );
        for _ in 0..100 {
            let _ = poll(id).unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !dropped.load(Ordering::Acquire),
            "reader/broker still owns custody"
        );
        release_tx.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            match poll(id).unwrap() {
                Progress::Retired(_) => break,
                Progress::Unresolved(reason) => panic!("{reason}"),
                Progress::Pending => assert!(Instant::now() < until),
            };
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(dropped.load(Ordering::Acquire));
        revocation.wait_drained(Duration::ZERO).unwrap();
    }
}
