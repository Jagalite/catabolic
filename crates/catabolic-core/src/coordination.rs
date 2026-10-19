// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Production observation/watcher lease decisions. Callers supply a durable
//! snapshot and explicit clock, then apply exactly one accepted effect under
//! the same writer transaction. Cancellation is caller-local and never revokes
//! shared inventory work. Tokens and identities are opaque digest strings.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    pub generation: i64,
    pub token: String,
    pub identity: String,
    pub until: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct State {
    pub generation: i64,
    pub lease: Option<Lease>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Generation {
    Next,
    Resume,
    AtLeast(i64),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Input {
    Acquire {
        now: f64,
        duration: f64,
        generation: Generation,
        token: String,
        identity: String,
        owner_alive: bool,
    },
    Fence {
        now: f64,
        generation: i64,
        token: String,
        identity: String,
    },
    CancelCaller {
        active: bool,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    Acquire(Lease),
    Fenced,
    CancelCaller { changed: bool },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rejection {
    InvalidTime,
    LiveOwner,
    InvalidIdentity,
    Overflow,
    StaleClaim,
}
fn clock(now: f64) -> Result<(), Rejection> {
    if now.is_finite() && now >= 0.0 {
        Ok(())
    } else {
        Err(Rejection::InvalidTime)
    }
}
pub fn reduce(state: &State, input: &Input) -> Result<Effect, Rejection> {
    match input {
        Input::Acquire {
            now,
            duration,
            generation,
            token,
            identity,
            owner_alive,
        } => {
            clock(*now)?;
            let until = now + duration;
            if !duration.is_finite() || *duration <= 0.0 || !until.is_finite() || until <= *now {
                return Err(Rejection::InvalidTime);
            }
            if token.is_empty() || identity.is_empty() {
                return Err(Rejection::InvalidIdentity);
            }
            if *owner_alive && state.lease.as_ref().is_some_and(|lease| lease.until > *now) {
                return Err(Rejection::LiveOwner);
            }
            let generation = match generation {
                Generation::Resume if state.generation > 0 => state.generation,
                Generation::Resume => return Err(Rejection::Overflow),
                Generation::Next | Generation::AtLeast(_) => {
                    let next = state
                        .generation
                        .checked_add(1)
                        .filter(|n| *n > 0)
                        .ok_or(Rejection::Overflow)?;
                    if let Generation::AtLeast(minimum) = generation {
                        next.max(*minimum)
                    } else {
                        next
                    }
                }
            };
            Ok(Effect::Acquire(Lease {
                generation,
                token: token.clone(),
                identity: identity.clone(),
                until,
            }))
        }
        Input::Fence {
            now,
            generation,
            token,
            identity,
        } => {
            clock(*now)?;
            if state.generation != *generation
                || *generation <= 0
                || !state.lease.as_ref().is_some_and(|lease| {
                    lease.generation == *generation
                        && lease.token == *token
                        && lease.identity == *identity
                        && lease.until.is_finite()
                        && lease.until > *now
                })
            {
                return Err(Rejection::StaleClaim);
            }
            Ok(Effect::Fenced)
        }
        Input::CancelCaller { active } => Ok(Effect::CancelCaller { changed: *active }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashSet, VecDeque};

    #[test]
    fn bounded_production_claim_graph_rejects_late_and_cancelled_callers() {
        // This is the same reducer called by durable scan/watcher transactions.
        // The independent ledger retains every issued token, cancelled caller
        // and generation, including callback attempts after takeover.
        #[derive(Clone, Debug, Serialize)]
        struct Model {
            state: State,
            cancelled: [bool; 2],
            issued: Vec<Lease>,
        }
        let initial = Model {
            state: State::default(),
            cancelled: [false; 2],
            issued: vec![],
        };
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([initial]);
        let mut transitions = 0;
        while let Some(before) = queue.pop_front() {
            if !seen.insert(serde_json::to_string(&before).unwrap()) {
                continue;
            }
            for caller in 0..2 {
                let mut after = before.clone();
                let effect = reduce(
                    &before.state,
                    &Input::CancelCaller {
                        active: !before.cancelled[caller],
                    },
                )
                .unwrap();
                assert_eq!(
                    effect,
                    Effect::CancelCaller {
                        changed: !before.cancelled[caller]
                    }
                );
                after.cancelled[caller] = true;
                assert_eq!(
                    after.state, before.state,
                    "caller cancellation revoked shared claim"
                );
                queue.push_back(after);
                transitions += 1;
            }
            for proof in &before.issued {
                for now in [0.0, 1.0, 2.0, 3.0] {
                    let result = reduce(
                        &before.state,
                        &Input::Fence {
                            now,
                            generation: proof.generation,
                            token: proof.token.clone(),
                            identity: proof.identity.clone(),
                        },
                    );
                    let expected = before.state.lease.as_ref() == Some(proof) && proof.until > now;
                    assert_eq!(
                        result == Ok(Effect::Fenced),
                        expected,
                        "stale or expired proof was accepted"
                    );
                    transitions += 1;
                }
            }
            if before.state.generation >= 2 {
                continue;
            }
            for now in [0.0, 1.0, 2.0] {
                for alive in [true, false] {
                    let mut after = before.clone();
                    let result = reduce(
                        &before.state,
                        &Input::Acquire {
                            now,
                            duration: 1.0,
                            generation: Generation::Next,
                            token: format!("token-{}-{now}", before.state.generation + 1),
                            identity: "definition".into(),
                            owner_alive: alive,
                        },
                    );
                    transitions += 1;
                    if let Ok(Effect::Acquire(lease)) = result {
                        assert!(
                            !alive
                                || before
                                    .state
                                    .lease
                                    .as_ref()
                                    .is_none_or(|old| old.until <= now)
                        );
                        assert_eq!(lease.generation, before.state.generation + 1);
                        after.state = State {
                            generation: lease.generation,
                            lease: Some(lease.clone()),
                        };
                        after.issued.push(lease);
                        queue.push_back(after);
                    }
                }
            }
        }
        eprintln!(
            "coordination graph: {} states, {transitions} transitions, zero skipped",
            seen.len()
        );
        assert!(seen.len() > 20 && transitions > 500);
    }
    #[test]
    fn every_fence_dimension_and_clock_is_required() {
        let state = State {
            generation: 1,
            lease: Some(Lease {
                generation: 1,
                token: "claim".into(),
                identity: "policy-and-dirty".into(),
                until: 5.0,
            }),
        };
        for input in [
            Input::Fence {
                now: 1.0,
                generation: 2,
                token: "claim".into(),
                identity: "policy-and-dirty".into(),
            },
            Input::Fence {
                now: 1.0,
                generation: 1,
                token: "old".into(),
                identity: "policy-and-dirty".into(),
            },
            Input::Fence {
                now: 1.0,
                generation: 1,
                token: "claim".into(),
                identity: "changed-dirty".into(),
            },
            Input::Fence {
                now: 5.0,
                generation: 1,
                token: "claim".into(),
                identity: "policy-and-dirty".into(),
            },
            Input::Fence {
                now: f64::NAN,
                generation: 1,
                token: "claim".into(),
                identity: "policy-and-dirty".into(),
            },
        ] {
            assert!(reduce(&state, &input).is_err());
        }
        let overflow = State {
            generation: i64::MAX,
            lease: None,
        };
        assert_eq!(
            reduce(
                &overflow,
                &Input::Acquire {
                    now: 1.0,
                    duration: 1.0,
                    generation: Generation::Next,
                    token: "new".into(),
                    identity: "policy".into(),
                    owner_alive: false
                }
            ),
            Err(Rejection::Overflow)
        );
    }
    #[test]
    fn resume_keeps_generation_but_replaces_fence() {
        let state = State {
            generation: 7,
            lease: Some(Lease {
                generation: 7,
                token: "old".into(),
                identity: "policy".into(),
                until: 2.0,
            }),
        };
        assert_eq!(
            reduce(
                &state,
                &Input::Acquire {
                    now: 3.0,
                    duration: 4.0,
                    generation: Generation::Resume,
                    token: "new".into(),
                    identity: "policy".into(),
                    owner_alive: false
                }
            ),
            Ok(Effect::Acquire(Lease {
                generation: 7,
                token: "new".into(),
                identity: "policy".into(),
                until: 7.0
            }))
        );
    }
}
