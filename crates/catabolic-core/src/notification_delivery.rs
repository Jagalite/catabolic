// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Delivery acknowledgement decisions from explicit attempt and time evidence.
//! The adapter must atomically revalidate the live lease and destination revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Complete,
    TemporaryFailure,
    Repair,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Complete,
    Retry,
    Exhausted,
    Repair,
}
impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Retry => "retry",
            Self::Exhausted => "exhausted",
            Self::Repair => "repair",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decision {
    pub state: State,
    pub due_at: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    InvalidTime,
}
/// `previous_attempts` is the durable count before the current claim.
pub fn acknowledge(
    outcome: Outcome,
    previous_attempts: u32,
    now: f64,
) -> Result<Decision, Rejection> {
    if !now.is_finite() {
        return Err(Rejection::InvalidTime);
    }
    let state = match outcome {
        Outcome::Complete => State::Complete,
        Outcome::TemporaryFailure if previous_attempts >= 4 => State::Exhausted,
        Outcome::TemporaryFailure => State::Retry,
        Outcome::Repair => State::Repair,
    };
    let backoff = 5.0 * f64::from(2_u32.pow(previous_attempts.min(10)));
    Ok(Decision {
        state,
        due_at: now + backoff.min(3600.0),
    })
}
/// Explicit lease and destination observations from the durable catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeaseEvidence {
    pub leased: bool,
    pub token_matches: bool,
    pub unexpired: bool,
    pub destination_enabled: bool,
    pub revision_matches: bool,
}
pub fn lease_current(e: LeaseEvidence) -> bool {
    e.leased && e.token_matches && e.unexpired && e.destination_enabled && e.revision_matches
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_exhaustion_and_backoff_are_explicit() {
        for previous in 0..5 {
            let decision = acknowledge(Outcome::TemporaryFailure, previous, 100.0).unwrap();
            assert_eq!(
                decision.state,
                if previous == 4 {
                    State::Exhausted
                } else {
                    State::Retry
                }
            );
            assert_eq!(
                decision.due_at,
                100.0 + 5.0 * f64::from(2_u32.pow(previous))
            );
        }
        assert_eq!(
            acknowledge(Outcome::TemporaryFailure, u32::MAX, 0.0).unwrap(),
            Decision {
                state: State::Exhausted,
                due_at: 3600.0
            }
        );
        assert_eq!(
            acknowledge(Outcome::Complete, 0, f64::NAN),
            Err(Rejection::InvalidTime)
        );
    }
    #[test]
    fn every_lease_fence_is_required() {
        let e = LeaseEvidence {
            leased: true,
            token_matches: true,
            unexpired: true,
            destination_enabled: true,
            revision_matches: true,
        };
        assert!(lease_current(e));
        assert!(!lease_current(LeaseEvidence { leased: false, ..e }));
        assert!(!lease_current(LeaseEvidence {
            token_matches: false,
            ..e
        }));
        assert!(!lease_current(LeaseEvidence {
            unexpired: false,
            ..e
        }));
        assert!(!lease_current(LeaseEvidence {
            destination_enabled: false,
            ..e
        }));
        assert!(!lease_current(LeaseEvidence {
            revision_matches: false,
            ..e
        }));
    }
}
