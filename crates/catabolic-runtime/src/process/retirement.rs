//! DISABLED PROTOTYPE: private process-group observations for protocol review.
//! These are NOT descendant cleanup authority: a child can escape its group.
//! No native lifetime/schema/host admission may consume these observations until
//! a kernel-enforced closed descendant boundary has independently qualified.
use std::{
    cell::RefCell,
    io,
    rc::Rc,
    time::{Duration, Instant},
};
#[derive(Default)]
struct Ledger {
    attempts: u32,
    spawned: u32,
    retired: u32,
    not_started: u32,
    poisoned: bool,
}
thread_local! { static CURRENT: RefCell<Option<Rc<RefCell<Ledger>>>> = const { RefCell::new(None) }; }
pub(crate) struct Scope {
    ledger: Rc<RefCell<Ledger>>,
}
/// Diagnostic counts only; not a cleanup witness or authority token.
pub(crate) struct GroupObservations {
    attempts: u32,
    spawned: u32,
    retired: u32,
    not_started: u32,
}
impl GroupObservations {
    pub(crate) fn counts(&self) -> (u32, u32, u32, u32) {
        (self.attempts, self.spawned, self.retired, self.not_started)
    }
}
impl Scope {
    pub(crate) fn begin() -> io::Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(io::Error::other(
                "external retirement protocol requires Linux procfs",
            ));
        }
        // A hidden process inventory is not evidence that an owned group is empty.
        let mounts = std::fs::read_to_string("/proc/self/mountinfo")?;
        if !mounts.lines().any(|line| {
            let parts: Vec<_> = line.split_whitespace().collect();
            parts.get(4) == Some(&"/proc")
                && line.contains(" - proc ")
                && !line.contains("hidepid=")
        }) {
            return Err(io::Error::other(
                "external retirement requires visible procfs",
            ));
        }
        let ledger = Rc::new(RefCell::new(Ledger::default()));
        CURRENT.with(|current| {
            if current.borrow().is_some() {
                return Err(io::Error::other("nested external retirement scope"));
            }
            *current.borrow_mut() = Some(ledger.clone());
            Ok(())
        })?;
        Ok(Self { ledger })
    }
    pub(crate) fn finish(self) -> io::Result<GroupObservations> {
        let l = self.ledger.borrow();
        if l.poisoned || l.attempts != l.retired + l.not_started || l.spawned != l.retired {
            return Err(io::Error::other(
                "external child cleanup remains unresolved",
            ));
        }
        Ok(GroupObservations {
            attempts: l.attempts,
            spawned: l.spawned,
            retired: l.retired,
            not_started: l.not_started,
        })
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|current| {
            *current.borrow_mut() = None;
        });
    }
}
pub(super) fn active() -> bool {
    CURRENT.with(|current| current.borrow().is_some())
}
pub(super) struct Obligation {
    ledger: Rc<RefCell<Ledger>>,
    resolved: bool,
    spawned: bool,
}
impl Obligation {
    pub(super) fn register() -> io::Result<Option<Self>> {
        CURRENT.with(|current| {
            let Some(ledger) = current.borrow().clone() else {
                return Ok(None);
            };
            let attempts = ledger
                .borrow()
                .attempts
                .checked_add(1)
                .ok_or_else(|| io::Error::other("child obligation budget"))?;
            if attempts > 1024 {
                return Err(io::Error::other("child obligation budget"));
            }
            ledger.borrow_mut().attempts = attempts;
            Ok(Some(Self {
                ledger,
                resolved: false,
                spawned: false,
            }))
        })
    }
    pub(super) fn spawned(&mut self) {
        self.spawned = true;
        self.ledger.borrow_mut().spawned += 1;
    }
    pub(super) fn not_started(mut self) {
        self.ledger.borrow_mut().not_started += 1;
        self.resolved = true;
    }
    pub(super) fn retired(&mut self) {
        if !self.resolved && self.spawned {
            self.ledger.borrow_mut().retired += 1;
            self.resolved = true;
        }
    }
    pub(super) fn poison(&self) {
        self.ledger.borrow_mut().poisoned = true;
    }
}
impl Drop for Obligation {
    fn drop(&mut self) {
        if !self.resolved {
            self.poison();
        }
    }
}

/// The unreaped leader pins this PGID throughout the inventory. Successful
/// SIGKILL alone is insufficient: every visible member must be exited before
/// the leader is reaped. Bounded/read/parse failures are refusal, not absence.
#[cfg(target_os = "linux")]
pub(super) fn verify_group_exited(pgid: u32) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut live = false;
        let mut leader = false;
        let mut count = 0;
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            count += 1;
            if count > 65536 {
                return Err(io::Error::other("process inventory truncated"));
            }
            let stat = match std::fs::read_to_string(entry.path().join("stat")) {
                Ok(s) => s,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let end = stat
                .rfind(')')
                .ok_or_else(|| io::Error::other("invalid proc stat"))?;
            let fields: Vec<_> = stat[end + 1..].split_whitespace().collect();
            let group = fields
                .get(2)
                .and_then(|s| s.parse::<u32>().ok())
                .ok_or_else(|| io::Error::other("invalid proc group"))?;
            if group == pgid {
                if pid == pgid {
                    leader = true;
                }
                if !matches!(fields.first(), Some(&"Z") | Some(&"X")) {
                    live = true;
                }
            }
        }
        if !leader {
            return Err(io::Error::other(
                "owned unreaped leader missing from process inventory",
            ));
        }
        if !live {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("owned process group has not exited"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[cfg(not(target_os = "linux"))]
pub(super) fn verify_group_exited(_pgid: u32) -> io::Result<()> {
    Err(io::Error::other("external retirement unsupported"))
}
