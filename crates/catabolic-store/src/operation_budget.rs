//! One cumulative native-operation budget, borrowed by database-only owners.
//! This module installs no SQLite hook and accepts no cancellation callback.
//! SQL/GraphQL retain their existing VM-hook ownership and borrow this budget.
use crate::selection::EvaluationFailure;
use std::{
    cell::RefCell,
    marker::PhantomData,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BudgetCharge {
    pub items: usize,
    pub values: usize,
    pub bytes: usize,
    pub work: usize,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct BudgetLimits {
    pub items: usize,
    pub values: usize,
    pub bytes: usize,
    pub work: usize,
}
struct State {
    used: BudgetCharge,
    failure: Option<EvaluationFailure>,
}
struct Inner {
    deadline: Instant,
    limits: BudgetLimits,
    state: Mutex<State>,
}
/// Clone retains the same absolute deadline, counters, and sticky refusal.
#[derive(Clone)]
pub(crate) struct OperationBudget(Arc<Inner>);
thread_local! { static ACTIVE: RefCell<Option<OperationBudget>> = const { RefCell::new(None) }; }
/// Thread-affine VM-owner bridge. It cannot be transferred to another thread or
/// used to substitute a fresh budget during an already admitted operation.
pub(crate) struct OperationBudgetGuard {
    previous: Option<OperationBudget>,
    _thread: PhantomData<Rc<()>>,
}
impl Drop for OperationBudgetGuard {
    fn drop(&mut self) {
        ACTIVE.with(|slot| *slot.borrow_mut() = self.previous.take());
    }
}
impl OperationBudget {
    pub(crate) fn new(timeout: Duration, limits: BudgetLimits) -> Result<Self, EvaluationFailure> {
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(EvaluationFailure::Timeout);
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(EvaluationFailure::Timeout)?;
        Ok(Self(Arc::new(Inner {
            deadline,
            limits,
            state: Mutex::new(State {
                used: BudgetCharge::default(),
                failure: None,
            }),
        })))
    }
    #[cfg(test)]
    pub(crate) fn usage(&self) -> BudgetCharge {
        self.0.state.lock().expect("test budget lock").used
    }
    pub(crate) fn current() -> Option<Self> {
        ACTIVE.with(|slot| slot.borrow().clone())
    }
    pub(crate) fn enter(&self) -> Result<OperationBudgetGuard, EvaluationFailure> {
        self.check()?;
        ACTIVE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|current| !Arc::ptr_eq(&current.0, &self.0))
            {
                return Err(self.record_failure(EvaluationFailure::WorkBudget));
            }
            Ok(OperationBudgetGuard {
                previous: slot.replace(self.clone()),
                _thread: PhantomData,
            })
        })
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.0.deadline
    }
    pub(crate) fn failure(&self) -> Option<EvaluationFailure> {
        match self.0.state.lock() {
            Ok(state) => state.failure,
            Err(_) => Some(EvaluationFailure::WorkBudget),
        }
    }
    pub(crate) fn record_failure(&self, failure: EvaluationFailure) -> EvaluationFailure {
        match self.0.state.lock() {
            Ok(mut state) => *state.failure.get_or_insert(failure),
            Err(_) => EvaluationFailure::WorkBudget,
        }
    }
    pub(crate) fn check(&self) -> Result<(), EvaluationFailure> {
        if let Some(failure) = self.failure() {
            return Err(failure);
        }
        // Poll outside the mutex: the embedding interrupt source may reenter.
        if crate::embedding::poll_interrupt() {
            return Err(self.record_failure(EvaluationFailure::Cancelled));
        }
        if Instant::now() >= self.0.deadline {
            return Err(self.record_failure(EvaluationFailure::Timeout));
        }
        Ok(())
    }
    pub(crate) fn remaining_ms(&self) -> Result<u64, EvaluationFailure> {
        self.check()?;
        let left = self
            .0
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if left == 0 {
            Err(self.record_failure(EvaluationFailure::Timeout))
        } else {
            Ok(left)
        }
    }
    /// Charge before allocating/decoding source buffers or adding output values.
    /// Refusal is sticky; failed charges never partially increment counters.
    pub(crate) fn charge(&self, charge: BudgetCharge) -> Result<(), EvaluationFailure> {
        self.check()?;
        self.charge_without_poll(charge)
    }
    /// For GraphQL resolver accounting while its context mutex is held. The
    /// existing executor/VM owner polls interruption outside this accounting
    /// method; never call a host signal callback under a new resolver lock.
    pub(crate) fn charge_without_poll(
        &self,
        charge: BudgetCharge,
    ) -> Result<(), EvaluationFailure> {
        if Instant::now() >= self.0.deadline {
            return Err(self.record_failure(EvaluationFailure::Timeout));
        }
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| EvaluationFailure::WorkBudget)?;
        if let Some(failure) = state.failure {
            return Err(failure);
        }
        let next = BudgetCharge {
            items: state.used.items.saturating_add(charge.items),
            values: state.used.values.saturating_add(charge.values),
            bytes: state.used.bytes.saturating_add(charge.bytes),
            work: state.used.work.saturating_add(charge.work),
        };
        let failure = if state.used.items.checked_add(charge.items).is_none()
            || next.items > self.0.limits.items
        {
            Some(EvaluationFailure::IdBudget)
        } else if state.used.bytes.checked_add(charge.bytes).is_none()
            || next.bytes > self.0.limits.bytes
        {
            Some(EvaluationFailure::ByteBudget)
        } else if state.used.values.checked_add(charge.values).is_none()
            || next.values > self.0.limits.values
            || state.used.work.checked_add(charge.work).is_none()
            || next.work > self.0.limits.work
        {
            Some(EvaluationFailure::WorkBudget)
        } else {
            None
        };
        if let Some(failure) = failure {
            state.failure = Some(failure);
            return Err(failure);
        }
        state.used = next;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn budget() -> OperationBudget {
        OperationBudget::new(
            Duration::from_secs(1),
            BudgetLimits {
                items: 3,
                values: 4,
                bytes: 10,
                work: 100,
            },
        )
        .unwrap()
    }
    #[test]
    fn clones_share_all_counters_and_sticky_origin() {
        let a = budget();
        let b = a.clone();
        a.charge(BudgetCharge {
            items: 2,
            bytes: 6,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            b.charge(BudgetCharge {
                bytes: 5,
                ..Default::default()
            }),
            Err(EvaluationFailure::ByteBudget)
        );
        assert_eq!(
            a.charge(BudgetCharge {
                items: 100,
                ..Default::default()
            }),
            Err(EvaluationFailure::ByteBudget)
        );
        assert_eq!(a.0.state.lock().unwrap().used.items, 2);
    }
    #[test]
    fn each_dimension_has_typed_refusal_and_overflow_fails_closed() {
        for (charge, expected) in [
            (
                BudgetCharge {
                    items: 4,
                    ..Default::default()
                },
                EvaluationFailure::IdBudget,
            ),
            (
                BudgetCharge {
                    bytes: 11,
                    ..Default::default()
                },
                EvaluationFailure::ByteBudget,
            ),
            (
                BudgetCharge {
                    values: 5,
                    ..Default::default()
                },
                EvaluationFailure::WorkBudget,
            ),
            (
                BudgetCharge {
                    work: 101,
                    ..Default::default()
                },
                EvaluationFailure::WorkBudget,
            ),
        ] {
            assert_eq!(budget().charge(charge), Err(expected));
        }
        let b = OperationBudget::new(
            Duration::from_secs(1),
            BudgetLimits {
                items: usize::MAX,
                values: usize::MAX,
                bytes: usize::MAX,
                work: usize::MAX,
            },
        )
        .unwrap();
        b.charge(BudgetCharge {
            bytes: usize::MAX,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            b.charge(BudgetCharge {
                bytes: 1,
                ..Default::default()
            }),
            Err(EvaluationFailure::ByteBudget)
        );
    }
    #[test]
    fn bridge_restores_and_cannot_replace_outer_budget() {
        let a = budget();
        let b = budget();
        {
            let _guard = a.enter().unwrap();
            assert!(OperationBudget::current().is_some());
            assert!(b.enter().is_err());
            {
                let _same = a.enter().unwrap();
            }
            assert!(OperationBudget::current().is_some());
        }
        assert!(OperationBudget::current().is_none());
    }
    #[test]
    fn timeout_and_cancellation_keep_their_origin() {
        let a = budget();
        crate::embedding::with_interrupt_check(
            || true,
            || {
                assert_eq!(a.check(), Err(EvaluationFailure::Cancelled));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(a.check(), Err(EvaluationFailure::Cancelled));
        let b = OperationBudget(Arc::new(Inner {
            deadline: Instant::now() - Duration::from_millis(1),
            limits: BudgetLimits {
                items: 1,
                values: 1,
                bytes: 1,
                work: 1,
            },
            state: Mutex::new(State {
                used: BudgetCharge::default(),
                failure: None,
            }),
        }));
        assert_eq!(b.remaining_ms(), Err(EvaluationFailure::Timeout));
    }
}
