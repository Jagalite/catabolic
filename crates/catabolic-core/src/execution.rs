// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Production durable-job lifecycle decisions. The store supplies an atomic
//! observation, process-liveness evidence and explicit time; it interprets the
//! returned effect in that same transaction. No tokens or source paths occur in
//! this state: identity comparisons use opaque SHA-256 fingerprints.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Queued,
    Running,
    Complete,
    Partial,
    Unsupported,
    Timeout,
    Changed,
    Failed,
    Mismatch,
    Cancelled,
}
impl Phase {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "complete" => Self::Complete,
            "partial" => Self::Partial,
            "unsupported" => Self::Unsupported,
            "timeout" => Self::Timeout,
            "changed" => Self::Changed,
            "failed" => Self::Failed,
            "mismatch" => Self::Mismatch,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unsupported => "unsupported",
            Self::Timeout => "timeout",
            Self::Changed => "changed",
            Self::Failed => "failed",
            Self::Mismatch => "mismatch",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    pub generation: i64,
    pub attempt: i64,
    pub token_digest: String,
    pub snapshot_digest: String,
    pub options_digest: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    pub proof: Proof,
    pub until: f64,
    pub worker_alive: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub phase: Phase,
    pub attempts: i64,
    pub snapshot_digest: String,
    pub options_digest: String,
    pub claim: Option<Lease>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Input {
    Start {
        now: f64,
        duration: f64,
    },
    /// Reclaim a durable publication intent without inventing another attempt.
    Recover {
        now: f64,
        duration: f64,
        attempt: i64,
    },
    Fence {
        now: f64,
        proof: Proof,
    },
    Renew {
        now: f64,
        duration: f64,
        proof: Proof,
    },
    Cancel {
        publication_intent: bool,
    },
    Retry {
        pending_artifact: bool,
    },
    Finish {
        now: f64,
        proof: Proof,
        outcome: Phase,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    Acquire {
        generation: i64,
        attempt: i64,
        until: f64,
    },
    Fenced,
    Renew {
        until: f64,
    },
    Cancel {
        changed: bool,
    },
    Requeue,
    Finish {
        outcome: Phase,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rejection {
    InvalidTime,
    LiveOwner,
    NotRunnable,
    StaleClaim,
    PublicationPending,
    NotRetryable,
    InvalidOutcome,
    Overflow,
}

fn time(now: f64) -> Result<(), Rejection> {
    if now.is_finite() && now >= 0.0 {
        Ok(())
    } else {
        Err(Rejection::InvalidTime)
    }
}
fn until(now: f64, duration: f64) -> Result<f64, Rejection> {
    time(now)?;
    if !duration.is_finite() || duration <= 0.0 || !(now + duration).is_finite() {
        return Err(Rejection::InvalidTime);
    }
    Ok(now + duration)
}
fn live(claim: &Lease, now: f64) -> bool {
    claim.worker_alive && claim.until.is_finite() && claim.until > now
}
fn fence(state: &State, now: f64, proof: &Proof) -> Result<(), Rejection> {
    time(now)?;
    let valid = state.phase == Phase::Running
        && state.attempts == proof.attempt
        && proof.generation > 0
        && proof.attempt > 0
        && state.snapshot_digest == proof.snapshot_digest
        && state.options_digest == proof.options_digest
        && state
            .claim
            .as_ref()
            .is_some_and(|c| live(c, now) && c.proof == *proof);
    if valid {
        Ok(())
    } else {
        Err(Rejection::StaleClaim)
    }
}
/// Every successful return is the only effect permitted for this observation.
/// A caller must not write job state or renew/publish after a rejected decision.
pub fn reduce(state: &State, input: &Input) -> Result<Effect, Rejection> {
    match input {
        Input::Start { now, duration } | Input::Recover { now, duration, .. } => {
            let until = until(*now, *duration)?;
            if state.claim.as_ref().is_some_and(|c| live(c, *now)) {
                return Err(Rejection::LiveOwner);
            }
            if !matches!(state.phase, Phase::Queued | Phase::Running) {
                return Err(Rejection::NotRunnable);
            }
            let generation = state
                .claim
                .as_ref()
                .map_or(Some(1), |c| c.proof.generation.checked_add(1))
                .filter(|n| *n > 0)
                .ok_or(Rejection::Overflow)?;
            let attempt = match input {
                Input::Recover { attempt, .. }
                    if state.phase == Phase::Running
                        && *attempt == state.attempts
                        && *attempt > 0 =>
                {
                    *attempt
                }
                Input::Recover { .. } => return Err(Rejection::NotRunnable),
                _ => state
                    .attempts
                    .checked_add(1)
                    .filter(|n| *n > 0)
                    .ok_or(Rejection::Overflow)?,
            };
            Ok(Effect::Acquire {
                generation,
                attempt,
                until,
            })
        }
        Input::Fence { now, proof } => {
            fence(state, *now, proof)?;
            Ok(Effect::Fenced)
        }
        Input::Renew {
            now,
            duration,
            proof,
        } => {
            fence(state, *now, proof)?;
            Ok(Effect::Renew {
                until: until(*now, *duration)?,
            })
        }
        Input::Cancel { publication_intent } => {
            if *publication_intent {
                return Err(Rejection::PublicationPending);
            }
            Ok(Effect::Cancel {
                changed: matches!(state.phase, Phase::Queued | Phase::Running),
            })
        }
        Input::Retry { pending_artifact } => {
            if *pending_artifact {
                return Err(Rejection::PublicationPending);
            }
            if !matches!(
                state.phase,
                Phase::Failed | Phase::Timeout | Phase::Cancelled
            ) {
                return Err(Rejection::NotRetryable);
            }
            Ok(Effect::Requeue)
        }
        Input::Finish {
            now,
            proof,
            outcome,
        } => {
            fence(state, *now, proof)?;
            if !outcome.terminal() {
                return Err(Rejection::InvalidOutcome);
            }
            Ok(Effect::Finish { outcome: *outcome })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> State {
        State {
            phase: Phase::Running,
            attempts: 2,
            snapshot_digest: "s".into(),
            options_digest: "o".into(),
            claim: Some(Lease {
                proof: Proof {
                    generation: 3,
                    attempt: 2,
                    token_digest: "t".into(),
                    snapshot_digest: "s".into(),
                    options_digest: "o".into(),
                },
                until: 10.0,
                worker_alive: true,
            }),
        }
    }
    #[test]
    fn every_identity_dimension_is_required() {
        let state = state();
        let proof = state.claim.as_ref().unwrap().proof.clone();
        assert_eq!(
            reduce(
                &state,
                &Input::Fence {
                    now: 1.0,
                    proof: proof.clone()
                }
            ),
            Ok(Effect::Fenced)
        );
        for field in 0..5 {
            let mut wrong = proof.clone();
            match field {
                0 => wrong.generation -= 1,
                1 => wrong.attempt -= 1,
                2 => wrong.token_digest.push('x'),
                3 => wrong.snapshot_digest.push('x'),
                _ => wrong.options_digest.push('x'),
            }
            assert_eq!(
                reduce(
                    &state,
                    &Input::Fence {
                        now: 1.0,
                        proof: wrong
                    }
                ),
                Err(Rejection::StaleClaim)
            );
        }
        assert_eq!(
            reduce(&state, &Input::Fence { now: 10.0, proof }),
            Err(Rejection::StaleClaim)
        );
    }
    #[test]
    fn publication_recovery_preserves_attempt_and_increments_generation() {
        let mut state = state();
        state.claim.as_mut().unwrap().worker_alive = false;
        assert_eq!(
            reduce(
                &state,
                &Input::Recover {
                    now: 2.0,
                    duration: 60.0,
                    attempt: 2
                }
            ),
            Ok(Effect::Acquire {
                generation: 4,
                attempt: 2,
                until: 62.0
            })
        );
        assert_eq!(
            reduce(
                &state,
                &Input::Cancel {
                    publication_intent: true
                }
            ),
            Err(Rejection::PublicationPending)
        );
    }
}
