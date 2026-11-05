// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded acceptance, baseline and retry decision audit.
use catabolic_core::{
    encode,
    execution::{self, Effect, Input, Lease, Phase, Proof, State},
    processing_acceptance::{self, BaselineAction, SourceAction},
    sha256,
};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::execution::{ReplayOptions, ReplayOutcome, record, replay};
use stateless::explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink};
use stateless::trace::{ReadLimits, RunConfig, Termination, Trace};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use std::{error::Error, fs::File, path::Path};
// One immutable source snapshot, at most two attempts, one source revision
// change and five clock values. SQLite and filesystem effects are abstracted;
// committed/reopened acceptance tests qualify their production adapter separately.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    phase: u8, // queued, running, failed, complete, changed
    attempt: u8,
    now: u8,
    until: u8,
    retry_due: Option<u8>,
    source_changed: bool,
    facts: u8,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Start,
    Fail,
    Tick,
    RetryDue,
    ChangeSource,
    CompleteCurrent,
    CompleteOld,
}
struct Adapter {
    fault: bool,
}
fn phase(value: u8) -> Result<Phase, ModelError> {
    match value {
        0 => Ok(Phase::Queued),
        1 => Ok(Phase::Running),
        2 => Ok(Phase::Failed),
        3 => Ok(Phase::Complete),
        4 => Ok(Phase::Changed),
        _ => Err(ModelError::new("invalid bounded processing phase")),
    }
}
fn proof(attempt: u8) -> Proof {
    Proof {
        generation: i64::from(attempt),
        attempt: i64::from(attempt),
        token_digest: format!("token-{attempt}"),
        snapshot_digest: "original-source".into(),
        options_digest: "options".into(),
    }
}
fn execution_state(s: &Audited) -> Result<State, ModelError> {
    Ok(State {
        phase: phase(s.phase)?,
        attempts: i64::from(s.attempt),
        snapshot_digest: "original-source".into(),
        options_digest: "options".into(),
        claim: (s.attempt > 0).then(|| Lease {
            proof: proof(s.attempt),
            until: f64::from(s.until),
            worker_alive: true,
        }),
    })
}
impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-processing-acceptance".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/processing_acceptance.rs")
                            .as_slice(),
                        include_bytes!("../../catabolic-core/src/execution.rs").as_slice(),
                        include_bytes!("processing_acceptance.rs").as_slice(),
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
    fn step(&self, before: &Audited, e: &Event) -> Result<Transition<Audited, String>, ModelError> {
        let mut after = before.clone();
        let state = execution_state(before)?;
        let now = f64::from(before.now);
        match e {
            Event::Tick if before.now < 4 => after.now += 1,
            Event::ChangeSource => after.source_changed = true,
            Event::Start if before.attempt < 2 => {
                let result = execution::reduce(&state, &Input::Start { now, duration: 2.0 });
                let expected = matches!(before.phase, 0 | 1)
                    && (before.attempt == 0 || before.until <= before.now);
                if result.is_ok() != expected {
                    after.violation = true;
                }
                if expected {
                    after.violation |= result
                        != Ok(Effect::Acquire {
                            generation: i64::from(before.attempt + 1),
                            attempt: i64::from(before.attempt + 1),
                            until: now + 2.0,
                        });
                }
                if let Ok(Effect::Acquire {
                    generation,
                    attempt,
                    until,
                }) = result
                {
                    after.violation |=
                        generation != i64::from(before.attempt + 1) || attempt != generation;
                    after.phase = 1;
                    after.attempt = attempt as u8;
                    after.until = until as u8;
                }
                if expected {
                    after.violation |= after.phase != 1
                        || after.attempt != before.attempt + 1
                        || after.until != before.now + 2;
                }
            }
            Event::RetryDue => {
                let due = before.retry_due.is_some_and(|due| due <= before.now);
                if due {
                    let result = execution::reduce(
                        &state,
                        &Input::Retry {
                            pending_artifact: false,
                        },
                    );
                    after.violation |= result.is_ok() != (before.phase == 2);
                    if before.phase == 2 {
                        after.violation |= result != Ok(Effect::Requeue);
                    }
                    if result == Ok(Effect::Requeue) {
                        after.phase = 0;
                        after.retry_due = None;
                    }
                    if before.phase == 2 {
                        after.violation |= after.phase != 0 || after.retry_due.is_some();
                    }
                }
            }
            Event::Fail | Event::CompleteCurrent | Event::CompleteOld => {
                let attempt = if matches!(e, Event::CompleteOld) {
                    1
                } else {
                    before.attempt
                };
                let failed = matches!(e, Event::Fail);
                let reported = if failed {
                    Phase::Failed
                } else {
                    Phase::Complete
                };
                let validate =
                    processing_acceptance::source_action(reported, false) == SourceAction::Validate;
                let current =
                    serde_json::json!({"revision": if before.source_changed { 1 } else { 0 }});
                let source_matches = processing_acceptance::inventory_matches(
                    &serde_json::json!({"revision":0}),
                    &current,
                );
                let outcome = if validate && !source_matches && !self.fault {
                    Phase::Changed
                } else {
                    reported
                };
                let result = execution::reduce(
                    &state,
                    &Input::Finish {
                        now,
                        proof: proof(attempt),
                        outcome,
                    },
                );
                let expected = before.phase == 1
                    && attempt > 0
                    && attempt == before.attempt
                    && before.until > before.now;
                after.violation |= result.is_ok() != expected;
                let required_outcome = if failed {
                    Phase::Failed
                } else if before.source_changed {
                    Phase::Changed
                } else {
                    Phase::Complete
                };
                if expected {
                    after.violation |= result
                        != Ok(Effect::Finish {
                            outcome: required_outcome,
                        });
                }
                if let Ok(Effect::Finish { outcome }) = result {
                    let expected_phase = if failed {
                        2
                    } else if before.source_changed {
                        4
                    } else {
                        3
                    };
                    after.phase = match outcome {
                        Phase::Failed => 2,
                        Phase::Complete => 3,
                        Phase::Changed => 4,
                        _ => {
                            after.violation = true;
                            before.phase
                        }
                    };
                    after.violation |= after.phase != expected_phase;
                    after.until = 0;
                    if outcome == Phase::Complete {
                        after.facts += 1;
                    }
                    let retry = processing_acceptance::retry_decision(
                        outcome,
                        true,
                        i64::from(attempt),
                        1,
                        now,
                    );
                    after.violation |= retry.enqueue != failed;
                    after.retry_due = retry.enqueue.then_some(retry.due_at as u8);
                    if failed {
                        after.violation |= retry.due_at != now + f64::from(attempt);
                    }
                }
            }
            _ => {}
        }
        // Required adapter effects cannot be silently omitted after acceptance.
        if before.phase == 1 && before.until > before.now {
            let finishing = matches!(e, Event::Fail | Event::CompleteCurrent)
                || (matches!(e, Event::CompleteOld) && before.attempt == 1);
            if finishing {
                let failed = matches!(e, Event::Fail);
                let expected_phase = if failed {
                    2
                } else if before.source_changed {
                    4
                } else {
                    3
                };
                after.violation |= after.phase != expected_phase || after.until != 0;
                after.violation |= after.facts != before.facts + u8::from(expected_phase == 3);
            }
        }
        // Existing facts may describe the old revision after a later source
        // change; the invariant forbids NEW publication using stale evidence.
        after.violation |= after.facts > 1 || (after.facts > before.facts && before.source_changed);
        Ok(Transition::accepted(after, vec![format!("{e:?}")]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "processing.acceptance",
                "source, attempt, retry or publication obligation violated",
            )
        } else {
            Check::passed("processing.acceptance")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::Start,
            Event::Fail,
            Event::Tick,
            Event::RetryDue,
            Event::ChangeSource,
            Event::CompleteCurrent,
            Event::CompleteOld,
        ])
    }
}
// Preserve the original exhaustive policy boundary cases independently of the
// lifecycle's smaller attempt and time bounds.
fn check_policy_boundaries() -> Result<(), Box<dyn Error>> {
    for phase in [
        Phase::Complete,
        Phase::Failed,
        Phase::Timeout,
        Phase::Mismatch,
        Phase::Changed,
    ] {
        for invalid in [false, true] {
            for baseline in [None, Some("same"), Some("old")] {
                for requested in [false, true] {
                    for attempt in [1, 2, 10, 11] {
                        let source = processing_acceptance::source_action(phase, invalid);
                        let checksum = processing_acceptance::baseline_action(baseline, "same");
                        let expected = match baseline {
                            None => BaselineAction::Establish,
                            Some("same") => BaselineAction::Preserve,
                            _ => BaselineAction::Mismatch,
                        };
                        let retry = processing_acceptance::retry_decision(
                            phase, requested, attempt, 5, 100.0,
                        );
                        let retryable =
                            matches!(phase, Phase::Failed | Phase::Timeout) && requested;
                        let delay = 5u32
                            .saturating_mul(1u32 << attempt.saturating_sub(1).clamp(0, 12) as u32)
                            .min(3600);
                        if (source == SourceAction::Validate)
                            != (phase == Phase::Complete || invalid)
                            || checksum != expected
                            || retry.retryable != retryable
                            || retry.enqueue != (retryable && attempt <= 10)
                            || retry.due_at != 100.0 + f64::from(delay)
                        {
                            return Err("processing policy boundary mismatch".into());
                        }
                    }
                }
            }
        }
    }
    Ok(())
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
        phase(state.phase)?;
        if state.attempt > 2
            || state.now > 4
            || state.until > 6
            || state.facts > 1
            || state.retry_due.is_some_and(|due| due > 6)
        {
            return Err(ModelError::new(
                "processing state exceeds documented bounds",
            ));
        }
        Ok(state)
    }
    fn encode_input(&self, v: &Event) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
    fn decode_input(&self, v: &[u8]) -> Result<Event, ModelError> {
        serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
    }
    fn encode_output(&self, v: &String) -> Result<Vec<u8>, ModelError> {
        codec(v)
    }
}
pub fn check() -> Result<(), Box<dyn Error>> {
    check_policy_boundaries()?;
    let r = enumerate(&Adapter { fault: false }, SearchConfig::default())?;
    if r.failure.is_some()
        || r.termination != SearchTermination::GraphExhausted
        || r.skipped_checks != 0
    {
        return Err(format!(
            "processing acceptance check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "processing acceptance graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected processing fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("processing shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-processing-acceptance".into(),
        seed: None,
        parameters: vec![
            ("sources".into(), "original,changed".into()),
            ("clock".into(), "0..4".into()),
            ("attempts".into(), "0..2".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("processing trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "processing injected fault found and shrunk: {} -> {} inputs",
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
        return Err("processing fresh replay did not match".into());
    }
    println!("processing fresh-process replay exact, failed property reproduced, matching build");
    Ok(())
}
