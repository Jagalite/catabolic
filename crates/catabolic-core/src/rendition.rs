// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Production decisions for independent caller demand on shared physical jobs.
//! Only opaque identities and explicit observations enter this reducer. Existing
//! catalog triggers, rather than a second state machine, fan out physical state.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemandPhase {
    Queued,
    Running,
    Validating,
    Ready,
    Failed,
    Cancelled,
    Blocked,
    Stale,
    Other,
}
impl DemandPhase {
    pub fn parse(value: &str) -> Self {
        match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "validating" => Self::Validating,
            "ready" => Self::Ready,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "blocked" => Self::Blocked,
            "stale" => Self::Stale,
            _ => Self::Other,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Validating => "validating",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Blocked => "blocked",
            Self::Stale => "stale",
            Self::Other => "other",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capacity {
    pub principal_pending: u64,
    pub global_pending: u64,
    pub principal_limit: u64,
    pub global_limit: u64,
    pub available: u128,
    pub reserved: u128,
    pub output_budget: u64,
    pub reserve_floor: u64,
    pub job_reserved: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Input {
    Admit {
        existing_body_matches: Option<bool>,
        job_phase: String,
        capacity: Capacity,
    },
    Cancel,
    Retry {
        demand: DemandPhase,
        job_phase: String,
        capacity: Capacity,
    },
    Status {
        demand: DemandPhase,
        source_current: bool,
        output_present: bool,
    },
    Worker {
        demand: DemandPhase,
        job_phase: String,
        authorized: bool,
        operation_enabled: bool,
        source_current: bool,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    ReuseDemand,
    Attach {
        demand: DemandPhase,
        reserve: bool,
    },
    CancelDemand,
    Retry {
        demand: DemandPhase,
        requeue_job: bool,
        reserve: bool,
    },
    Status(DemandPhase),
    BlockDemand,
    ChangeQueuedJob,
    SelectJob,
    Ignore,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rejection {
    IdempotencyConflict,
    RequestLimit,
    StorageUnavailable,
    NotRetryable,
}
impl Rejection {
    pub fn code(self) -> &'static str {
        match self {
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::RequestLimit => "request_limit",
            Self::StorageUnavailable => "storage_reservation_unavailable",
            Self::NotRetryable => "request_not_retryable",
        }
    }
}
fn capacity(c: &Capacity, complete: bool) -> Result<bool, Rejection> {
    if c.principal_pending >= c.principal_limit || c.global_pending >= c.global_limit {
        return Err(Rejection::RequestLimit);
    }
    let reserve = !complete && !c.job_reserved;
    if reserve
        && c.available
            .checked_sub(c.reserved)
            .and_then(|n| n.checked_sub(u128::from(c.output_budget)))
            .is_none_or(|free| free < u128::from(c.reserve_floor))
    {
        return Err(Rejection::StorageUnavailable);
    }
    Ok(reserve)
}
pub fn reduce(input: &Input) -> Result<Effect, Rejection> {
    Ok(match input {
        Input::Admit {
            existing_body_matches: Some(matches),
            ..
        } => {
            if !matches {
                return Err(Rejection::IdempotencyConflict);
            }
            Effect::ReuseDemand
        }
        Input::Admit {
            job_phase,
            capacity: c,
            ..
        } => Effect::Attach {
            demand: if job_phase == "complete" {
                DemandPhase::Ready
            } else {
                DemandPhase::parse(job_phase)
            },
            reserve: capacity(c, job_phase == "complete")?,
        },
        Input::Cancel => Effect::CancelDemand,
        Input::Retry {
            demand,
            job_phase,
            capacity: c,
        } => {
            if !matches!(
                demand,
                DemandPhase::Failed | DemandPhase::Cancelled | DemandPhase::Blocked
            ) {
                return Err(Rejection::NotRetryable);
            }
            Effect::Retry {
                demand: if job_phase == "complete" {
                    DemandPhase::Ready
                } else {
                    DemandPhase::Queued
                },
                requeue_job: matches!(job_phase.as_str(), "failed" | "timeout" | "cancelled"),
                reserve: capacity(c, job_phase == "complete")?,
            }
        }
        Input::Status {
            demand,
            source_current,
            output_present,
        } => Effect::Status(
            if *demand != DemandPhase::Cancelled && !source_current
                || *demand == DemandPhase::Ready && !output_present
            {
                DemandPhase::Stale
            } else {
                *demand
            },
        ),
        Input::Worker {
            demand,
            job_phase,
            authorized,
            operation_enabled,
            source_current,
        } => {
            if !authorized {
                if *demand == DemandPhase::Cancelled {
                    Effect::Ignore
                } else {
                    Effect::BlockDemand
                }
            } else if *demand != DemandPhase::Cancelled && !source_current && job_phase == "queued"
            {
                Effect::ChangeQueuedJob
            } else if !operation_enabled {
                if *demand == DemandPhase::Cancelled {
                    Effect::Ignore
                } else {
                    Effect::BlockDemand
                }
            } else {
                Effect::SelectJob
            }
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_never_owns_physical_work() {
        assert_eq!(reduce(&Input::Cancel), Ok(Effect::CancelDemand));
        assert_eq!(
            reduce(&Input::Worker {
                demand: DemandPhase::Cancelled,
                job_phase: "queued".into(),
                authorized: true,
                operation_enabled: true,
                source_current: false
            }),
            Ok(Effect::SelectJob)
        );
    }
    #[test]
    fn stale_job_precedes_disabled_operation_but_never_bypasses_authorization() {
        let input = Input::Worker {
            demand: DemandPhase::Queued,
            job_phase: "queued".into(),
            authorized: true,
            operation_enabled: false,
            source_current: false,
        };
        assert_eq!(reduce(&input), Ok(Effect::ChangeQueuedJob));
        assert_eq!(
            reduce(&Input::Worker {
                demand: DemandPhase::Queued,
                job_phase: "queued".into(),
                authorized: false,
                operation_enabled: false,
                source_current: false
            }),
            Ok(Effect::BlockDemand)
        );
        assert_eq!(
            reduce(&Input::Worker {
                demand: DemandPhase::Queued,
                job_phase: "running".into(),
                authorized: true,
                operation_enabled: false,
                source_current: false
            }),
            Ok(Effect::BlockDemand)
        );
    }
    #[test]
    fn cancelled_requests_still_count_toward_pending_cap() {
        let c = Capacity {
            principal_pending: 10,
            global_pending: 10,
            principal_limit: 10,
            global_limit: 100,
            available: 1000,
            reserved: 0,
            output_budget: 1,
            reserve_floor: 1,
            job_reserved: true,
        };
        assert_eq!(
            reduce(&Input::Admit {
                existing_body_matches: None,
                job_phase: "queued".into(),
                capacity: c
            }),
            Err(Rejection::RequestLimit)
        );
        assert_eq!(
            reduce(&Input::Admit {
                existing_body_matches: Some(true),
                job_phase: "queued".into(),
                capacity: c
            }),
            Ok(Effect::ReuseDemand)
        );
    }
}
