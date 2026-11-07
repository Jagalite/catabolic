// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded delivery claims, retry deadlines, replacement and acknowledgement.
//!
//! Two request revisions and two destination revisions, five attempts per request,
//! and unique claim tokens.
//! Time is relative: acknowledgement is evaluated at time zero; deadline events
//! visit immediately-before and exactly-at expiry/due boundaries. No wall-clock
//! fairness or network exactly-once guarantee is claimed. Claim, exhaustion and
//! mapped replacement guards below abstract the production SQL; this model is
//! not a committed-database comparison or a multi-row mapped-batch model.
use catabolic_core::{
    encode,
    notification_delivery::{self, LeaseEvidence, Outcome, State},
    sha256,
};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::execution::{ReplayOptions, ReplayOutcome, record, replay};
use stateless::explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink};
use stateless::trace::{ReadLimits, RunConfig, Termination, Trace};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use std::{error::Error, fs::File, path::Path};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Phase {
    #[default]
    Pending,
    Leased,
    Retry,
    Complete,
    Repair,
    Exhausted,
    Cancelled,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    token: u8,
    revision: u8,
    destination_revision: u8,
    lease_revision: u8,
    attempts: u8,
    deadline: u16,
    enabled: bool,
    phase: Phase,
    violation: bool,
}
impl Default for Audited {
    fn default() -> Self {
        Self {
            token: 0,
            revision: 0,
            destination_revision: 0,
            lease_revision: 0,
            attempts: 0,
            deadline: 0,
            enabled: true,
            phase: Phase::Pending,
            violation: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Claim,
    BeforeDeadline,
    Deadline,
    Supersede,
    Revoke,
    EditDestination,
    Acknowledge { old: bool, outcome: u8 },
}
struct Adapter {
    fault: bool,
}
// These are required effects of the abstract SQL adapter, not observations of
// production database writes. Keeping an independent pre/post oracle catches
// omissions in this model instead of treating every no-op as safe progress.
fn required_effects(before: &Audited, event: &Event, after: &Audited) -> bool {
    let mut expected = before.clone();
    match event {
        Event::Claim => {
            let unfinished = before.phase == Phase::Pending
                || before.phase == Phase::Retry
                || before.phase == Phase::Leased;
            if unfinished && before.deadline == 0 {
                if before.attempts >= 5 {
                    expected.phase = Phase::Exhausted;
                } else if before.enabled {
                    expected.phase = Phase::Leased;
                    expected.attempts = before.attempts + 1;
                    expected.token = before.token + 1;
                    expected.lease_revision = before.destination_revision;
                    expected.deadline = 60;
                }
            }
        }
        Event::Supersede => {
            let blocked = before.phase == Phase::Leased && before.deadline != 0;
            if before.revision == 0 && !blocked {
                expected.revision = 1;
                expected.attempts = 0;
                expected.deadline = 0;
                expected.phase = Phase::Pending;
            }
        }
        _ => return true,
    }
    *after == expected
}
fn apply(before: &Audited, event: &Event, fault: bool) -> Audited {
    let mut after = before.clone();
    match *event {
        Event::Claim => {
            let eligible = matches!(before.phase, Phase::Pending | Phase::Retry | Phase::Leased)
                && before.deadline == 0;
            // Production claim first exhausts expired fifth attempts, even
            // when the destination is disabled; selection then checks enabled.
            if eligible && before.attempts == 5 {
                after.phase = Phase::Exhausted;
            } else if eligible && before.enabled {
                after.token += 1;
                after.attempts += 1;
                after.lease_revision = before.destination_revision;
                after.deadline = 60;
                after.phase = Phase::Leased;
            }
        }
        Event::BeforeDeadline if before.deadline > 1 => after.deadline = 1,
        Event::Deadline => after.deadline = 0,
        Event::Supersede
            if before.revision == 0 && !(before.phase == Phase::Leased && before.deadline > 0) =>
        {
            // Abstract one immutable mapped-request replacement. Its old
            // token must not acknowledge the new pending request or claim.
            after.revision = 1;
            after.attempts = 0;
            after.phase = Phase::Pending;
            after.deadline = 0;
        }
        Event::Revoke => {
            after.enabled = false;
            if before.phase != Phase::Complete {
                after.phase = Phase::Cancelled;
                after.deadline = 0;
            }
        }
        Event::EditDestination if before.destination_revision == 0 => {
            after.destination_revision = 1;
            if before.phase != Phase::Complete {
                after.phase = Phase::Cancelled;
                after.deadline = 0;
            }
        }
        Event::Acknowledge { old, outcome } => {
            let token = if old {
                before.token.saturating_sub(1)
            } else {
                before.token
            };
            let evidence = LeaseEvidence {
                leased: before.phase == Phase::Leased,
                token_matches: token != 0 && token == before.token,
                unexpired: before.deadline > 0,
                destination_enabled: before.enabled,
                revision_matches: before.lease_revision == before.destination_revision,
            };
            let expected = before.phase == Phase::Leased
                && token != 0
                && token == before.token
                && before.deadline > 0
                && before.enabled
                && before.lease_revision == before.destination_revision;
            let mut actual = notification_delivery::lease_current(evidence);
            if fault
                && old
                && before.token > 1
                && before.phase == Phase::Leased
                && before.deadline > 0
                && before.enabled
            {
                actual = true;
            }
            after.violation |= actual != expected;
            if actual {
                let previous = u32::from(before.attempts.saturating_sub(1));
                let policy_outcome = match outcome {
                    0 => Outcome::Complete,
                    1 => Outcome::TemporaryFailure,
                    _ => Outcome::Repair,
                };
                let wanted = match outcome {
                    0 => State::Complete,
                    1 if before.attempts == 5 => State::Exhausted,
                    1 => State::Retry,
                    _ => State::Repair,
                };
                // Independent table, not the production exponential formula.
                let delay = [5_u16, 10, 20, 40, 80][usize::from(before.attempts.saturating_sub(1))];
                match notification_delivery::acknowledge(policy_outcome, previous, 0.0) {
                    Ok(decision) => {
                        after.violation |=
                            decision.state != wanted || decision.due_at != f64::from(delay);
                        after.phase = match decision.state {
                            State::Complete => Phase::Complete,
                            State::Retry => Phase::Retry,
                            State::Exhausted => Phase::Exhausted,
                            State::Repair => Phase::Repair,
                        };
                        // Only retry time affects subsequent admission.
                        after.deadline = if after.phase == Phase::Retry {
                            delay
                        } else {
                            0
                        };
                    }
                    Err(_) => after.violation = true,
                }
            }
        }
        _ => {}
    }
    let effects_valid = required_effects(before, event, &after);
    after.violation |= !effects_valid;
    after.violation |= after.attempts > 5
        || after.token > 10
        || (after.phase == Phase::Retry && after.attempts >= 5);
    after
}
impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-notification-lease".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/notification_delivery.rs")
                            .as_slice(),
                        include_bytes!("notification_lease.rs").as_slice(),
                        include_bytes!("../../../Cargo.lock").as_slice()
                    ]
                    .concat()
                ),
                self.fault
            ),
        }
    }
    fn initial_state(&self) -> Result<Audited, ModelError> {
        Ok(Audited::default())
    }
    fn step(
        &self,
        before: &Audited,
        event: &Event,
    ) -> Result<Transition<Audited, String>, ModelError> {
        let after = apply(before, event, self.fault);
        Ok(Transition::accepted(after, vec![format!("{event:?}")]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "notification.lease",
                "lease fencing, retry deadline or attempt exhaustion obligation failed",
            )
        } else {
            Check::passed("notification.lease")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        let mut inputs = vec![
            Event::Claim,
            Event::BeforeDeadline,
            Event::Deadline,
            Event::Supersede,
            Event::Revoke,
            Event::EditDestination,
        ];
        for old in [false, true] {
            for outcome in 0..3 {
                inputs.push(Event::Acknowledge { old, outcome });
            }
        }
        Ok(inputs)
    }
}
fn codec(v: &impl Serialize) -> Result<Vec<u8>, ModelError> {
    Ok(encode(&serde_json::to_value(v).map_err(|e| ModelError::new(e.to_string()))?).into_bytes())
}
impl ModelCodec for Adapter {
    fn encode_state(&self, v: &Audited) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
    fn decode_state(&self, v: &[u8]) -> Result<Audited, ModelError> {
        let state: Audited =
            serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))?;
        if state.attempts > 5
            || state.token > 10
            || state.token < state.attempts
            || state.revision > 1
            || state.destination_revision > 1
            || state.lease_revision > 1
            || (state.phase == Phase::Leased && state.attempts == 0)
            || ![0, 1, 5, 10, 20, 40, 60, 80].contains(&state.deadline)
        {
            return Err(ModelError::new(
                "notification state outside declared bounds",
            ));
        }
        Ok(state)
    }
    fn encode_input(&self, v: &Event) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
    fn decode_input(&self, v: &[u8]) -> Result<Event, ModelError> {
        let event: Event = serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))?;
        if matches!(event, Event::Acknowledge { outcome, .. } if outcome > 2) {
            return Err(ModelError::new(
                "notification outcome outside declared bounds",
            ));
        }
        Ok(event)
    }
    fn encode_output(&self, v: &String) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
}
pub fn check() -> Result<(), Box<dyn Error>> {
    let r = enumerate(&Adapter { fault: false }, SearchConfig::default())?;
    if r.failure.is_some()
        || r.termination != SearchTermination::GraphExhausted
        || r.skipped_checks != 0
    {
        return Err(format!(
            "notification lease check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "notification lease graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected notification lease fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("notification lease shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-notification-lease".into(),
        seed: None,
        parameters: vec![
            ("maximum-claims-per-revision".into(), "5".into()),
            ("maximum-revisions".into(), "2".into()),
            ("time".into(), "relative-deadline-boundaries".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("notification lease trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "notification lease injected fault found and shrunk: {} -> {} inputs",
        f.inputs.len(),
        reduced.minimized.inputs.len()
    );
    Ok(())
}
pub fn replay_trace(path: &str) -> Result<(), Box<dyn Error>> {
    let trace = Trace::read_from(File::open(path)?, &ReadLimits::default())?;
    let r = replay(
        &Adapter { fault: true },
        &trace,
        ReplayOptions {
            allow_build_mismatch: false,
        },
    )?;
    if r.outcome != ReplayOutcome::Exact || !r.failure_reproduced || !r.build_matches {
        return Err("notification lease fresh replay did not match".into());
    }
    println!(
        "notification lease fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(state: &mut Audited, event: Event) {
        *state = apply(state, &event, false);
        assert!(!state.violation);
    }
    fn acknowledge(state: &mut Audited, old: bool, outcome: u8) {
        step(state, Event::Acknowledge { old, outcome });
    }

    #[test]
    fn required_claim_and_replacement_effects_detect_omission() {
        let state = Audited::default();
        assert!(!required_effects(&state, &Event::Claim, &state));
        assert!(!required_effects(&state, &Event::Supersede, &state));
        let leased = apply(&state, &Event::Claim, false);
        assert!(required_effects(&leased, &Event::Claim, &leased));
        assert!(required_effects(&leased, &Event::Supersede, &leased));
        let mut wrong = leased.clone();
        wrong.token += 1;
        assert!(!required_effects(&leased, &Event::Claim, &wrong));
        let exhausted = Audited {
            attempts: 5,
            token: 5,
            phase: Phase::Leased,
            ..Audited::default()
        };
        assert!(!required_effects(&exhausted, &Event::Claim, &exhausted));
    }

    #[test]
    fn trace_codec_rejects_out_of_bound_attempts_and_outcomes() {
        let model = Adapter { fault: false };
        let state = Audited {
            attempts: 6,
            token: 6,
            ..Audited::default()
        };
        assert!(model.decode_state(&codec(&state).unwrap()).is_err());
        let event = Event::Acknowledge {
            old: false,
            outcome: 3,
        };
        assert!(model.decode_input(&codec(&event).unwrap()).is_err());
    }

    #[test]
    fn five_temporary_failures_obey_backoff_and_exhaust() {
        let mut state = Audited::default();
        for attempt in 1..=5 {
            step(&mut state, Event::Claim);
            assert_eq!(state.attempts, attempt);
            acknowledge(&mut state, false, 1);
            if attempt < 5 {
                assert_eq!(state.phase, Phase::Retry);
                let snapshot = state.clone();
                step(&mut state, Event::Claim);
                assert_eq!(state, snapshot);
                step(&mut state, Event::BeforeDeadline);
                step(&mut state, Event::Claim);
                assert_eq!(state.attempts, attempt);
                step(&mut state, Event::Deadline);
            }
        }
        assert_eq!(state.phase, Phase::Exhausted);
        let snapshot = state.clone();
        step(&mut state, Event::Claim);
        assert_eq!(state, snapshot);
    }

    #[test]
    fn expired_fifth_claim_exhausts_without_acknowledgement() {
        let mut state = Audited::default();
        for attempt in 1..=5 {
            step(&mut state, Event::Claim);
            assert_eq!(state.attempts, attempt);
            step(&mut state, Event::Deadline);
        }
        step(&mut state, Event::Claim);
        assert_eq!(state.phase, Phase::Exhausted);
    }

    #[test]
    fn replacement_and_reclaim_reject_old_acknowledgements() {
        let mut state = Audited::default();
        step(&mut state, Event::Claim);
        step(&mut state, Event::Supersede);
        assert_eq!(state.revision, 0);
        step(&mut state, Event::Deadline);
        step(&mut state, Event::Supersede);
        assert_eq!(state.revision, 1);
        acknowledge(&mut state, false, 0);
        assert_eq!(state.phase, Phase::Pending);
        step(&mut state, Event::Claim);
        let snapshot = state.clone();
        acknowledge(&mut state, true, 0);
        assert_eq!(state, snapshot);
        assert!(
            apply(
                &state,
                &Event::Acknowledge {
                    old: true,
                    outcome: 0
                },
                true,
            )
            .violation
        );
        acknowledge(&mut state, false, 0);
        assert_eq!(state.phase, Phase::Complete);
    }

    #[test]
    fn destination_edit_and_revocation_cancel_live_delivery() {
        for event in [Event::EditDestination, Event::Revoke] {
            let mut state = Audited::default();
            step(&mut state, Event::Claim);
            step(&mut state, event);
            acknowledge(&mut state, false, 0);
            assert_eq!(state.phase, Phase::Cancelled);
            step(&mut state, Event::Supersede);
            step(&mut state, Event::Claim);
            if state.enabled {
                assert_eq!(state.lease_revision, 1);
                acknowledge(&mut state, false, 0);
                assert_eq!(state.phase, Phase::Complete);
            } else {
                assert_eq!(state.phase, Phase::Pending);
                assert_eq!(state.attempts, 0);
            }
        }
    }
}
