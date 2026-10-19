//! VM ownership for a freshly opened, unescaped private native writer only.
//! No wrapper around an arbitrary public Store/Connection can mint this proof.
use crate::{
    Error,
    operation_budget::{BudgetCharge, OperationBudget},
};
use rusqlite::Connection;
use std::{
    cell::{Cell, RefCell},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
#[derive(Clone)]
struct Active {
    connection: usize,
    budget: OperationBudget,
    cleanup: Arc<AtomicBool>,
    commit: Arc<CommitState>,
}
#[derive(Default)]
struct CommitState {
    active: AtomicBool,
    callbacks: AtomicUsize,
}
thread_local! { static ACTIVE:RefCell<Option<Active>>=const{RefCell::new(None)}; }
// Pinned SQLite's fixed COMMIT program is Init/AutoCommit/Halt/Goto;
// schema35 explicitly flushes FTS pending terms before entering this phase.
// Include parser callbacks and conservative spare room, not just VM opcodes.
const COMMIT_ALLOWANCE: usize = 64;
const BUSY_POLL: Duration = Duration::from_millis(5);

// SQLite does not invoke the VM progress hook while waiting on another writer.
// This callback belongs to the same private connection and absolute deadline.
fn busy_retry(attempts: i32) -> bool {
    let active = ACTIVE.with(|slot| slot.borrow().clone());
    let Some(active) = active else {
        return false;
    };
    if active.cleanup.load(Ordering::Acquire) {
        // Cleanup ignores cancellation but retains the legacy finite busy wait.
        if attempts >= 1000 {
            return false;
        }
        std::thread::sleep(BUSY_POLL);
        return true;
    }
    let Ok(left) = active.budget.remaining_ms() else {
        return false;
    };
    std::thread::sleep(BUSY_POLL.min(Duration::from_millis(left)));
    active.budget.check().is_ok()
}
pub(crate) struct NativeWriterVmAdmission<'a> {
    db: &'a Connection,
    poisoned: &'a Cell<bool>,
    armed: bool,
}
impl<'a> NativeWriterVmAdmission<'a> {
    pub(crate) fn install(
        db: &'a Connection,
        budget: &OperationBudget,
        poisoned: &'a Cell<bool>,
        _seal: &crate::catalog_write::WriterSeal,
    ) -> crate::Result<Self> {
        if poisoned.get() || !db.is_autocommit() {
            return Err(Error(
                "private native writer is unavailable or has an ambient transaction".into(),
            ));
        }
        if ACTIVE.with(|slot| slot.borrow().is_some()) {
            return Err(Error("native writer VM already owned".into()));
        }
        budget
            .check()
            .map_err(|cause| Error(format!("operation budget: {cause:?}")))?;
        let cleanup = Arc::new(AtomicBool::new(false));
        let cleaning = cleanup.clone();
        let work = budget.clone();
        let commit = Arc::new(CommitState::default());
        let committing = commit.clone();
        db.progress_handler(
            1,
            Some(move || {
                if cleaning.load(Ordering::Acquire) {
                    return false;
                }
                if committing.active.load(Ordering::Acquire) {
                    // SQLite also invokes this callback after AutoCommit has
                    // made the transaction durable. Consume the prepaid fixed
                    // phase without introducing a new failure after durability.
                    committing.callbacks.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
                work.charge(BudgetCharge {
                    work: 1,
                    ..Default::default()
                })
                .is_err()
            }),
        )?;
        ACTIVE.with(|slot| {
            *slot.borrow_mut() = Some(Active {
                connection: std::ptr::from_ref(db) as usize,
                budget: budget.clone(),
                cleanup,
                commit,
            })
        });
        let owner = Self {
            db,
            poisoned,
            armed: true,
        };
        if let Err(error) = db.busy_handler(Some(busy_retry)) {
            poisoned.set(true);
            return Err(error.into());
        }
        Ok(owner)
    }
    pub(crate) fn finish(mut self) -> crate::Result<()> {
        if !self.db.is_autocommit() {
            // A successful operation cannot surrender VM ownership while a
            // transaction survives. Drop performs uninterruptible cleanup.
            self.poisoned.set(true);
            return Err(Error(
                "private writer retained an unfinished transaction".into(),
            ));
        }
        self.db
            .progress_handler(0, None::<fn() -> bool>)
            .inspect_err(|_| self.poisoned.set(true))?;
        self.db
            .busy_timeout(Duration::from_secs(5))
            .inspect_err(|_| self.poisoned.set(true))?;
        ACTIVE.with(|slot| *slot.borrow_mut() = None);
        self.armed = false;
        Ok(())
    }
}
impl Drop for NativeWriterVmAdmission<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _cleanup = cleanup(self.db);
            if !self.db.is_autocommit() {
                // Normally Store's unwind guard has already rolled this back.
                // Retire a writer that leaked its transaction even if rollback
                // succeeds, rather than letting a later operation inherit it.
                self.poisoned.set(true);
                let _ = self.db.execute_batch("ROLLBACK");
            }
            if self.db.progress_handler(0, None::<fn() -> bool>).is_err() {
                self.poisoned.set(true);
            }
            if self.db.busy_timeout(Duration::from_secs(5)).is_err() {
                self.poisoned.set(true);
            }
            ACTIVE.with(|slot| *slot.borrow_mut() = None);
        }
    }
}
/// Exact-connection witness for fixed finalizers. A current generic operation
/// budget alone does not prove that a writer VM callback is installed.
pub(crate) fn current_budget(db: &Connection) -> Option<OperationBudget> {
    ACTIVE.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|a| a.connection == std::ptr::from_ref(db) as usize)
            .map(|a| a.budget.clone())
    })
}
pub(crate) struct Cleanup {
    flag: Arc<AtomicBool>,
    previous: bool,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.flag.store(self.previous, Ordering::Release);
    }
}
/// Rollback must finish even after cancellation/timeout exhausted the operation.
/// This suspends the owned callback only during Store's typed rollback boundary;
/// it never changes or replaces a handler on an arbitrary legacy Connection.
pub(crate) fn cleanup(db: &Connection) -> Option<Cleanup> {
    ACTIVE.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|a| a.connection == std::ptr::from_ref(db) as usize)
            .map(|a| Cleanup {
                flag: a.cleanup.clone(),
                previous: a.cleanup.swap(true, Ordering::AcqRel),
            })
    })
}

#[cfg(test)]
pub(crate) fn owns_current_thread_for_test() -> bool {
    ACTIVE.with(|slot| slot.borrow().is_some())
}

/// Prepay the fixed top-level COMMIT after all source, FTS and output work.
/// This is the final cancellation-admission point. Once accepted, late
/// cancellation cannot turn a durable success into an apparent rollback.
/// Busy waits remain interruptible while SQLite has not completed the commit.
pub(crate) struct CommitAdmission {
    state: Arc<CommitState>,
}
impl CommitAdmission {
    pub(crate) fn within_prepaid_bound(&self) -> bool {
        self.state.callbacks.load(Ordering::Relaxed) <= COMMIT_ALLOWANCE
    }
    #[cfg(test)]
    pub(crate) fn callbacks(&self) -> usize {
        self.state.callbacks.load(Ordering::Relaxed)
    }
}
impl Drop for CommitAdmission {
    fn drop(&mut self) {
        self.state.active.store(false, Ordering::Release);
    }
}
pub(crate) fn prepare_commit(db: &Connection) -> crate::Result<Option<CommitAdmission>> {
    let active = ACTIVE.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|active| active.connection == std::ptr::from_ref(db) as usize)
            .cloned()
    });
    let Some(active) = active else {
        return Ok(None);
    };
    if active.commit.active.load(Ordering::Acquire) {
        return Err(Error(
            "private writer commit admission already active".into(),
        ));
    }
    active
        .budget
        .charge(BudgetCharge {
            work: COMMIT_ALLOWANCE,
            ..Default::default()
        })
        .map_err(|cause| Error(format!("operation budget: {cause:?}")))?;
    active.commit.callbacks.store(0, Ordering::Relaxed);
    active.commit.active.store(true, Ordering::Release);
    Ok(Some(CommitAdmission {
        state: active.commit,
    }))
}
