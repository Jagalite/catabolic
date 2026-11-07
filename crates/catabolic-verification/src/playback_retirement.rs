// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded playback demand, worker completion and cache retirement.
use catabolic_core::{encode, playback, playback_admission, sha256};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::execution::{ReplayOptions, ReplayOutcome, record, replay};
use stateless::explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink};
use stateless::trace::{ReadLimits, RunConfig, Termination, Trace};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use std::{error::Error, fs::File, path::Path};
// Two callers with explicit expiration, one physical worker/reservation, two
// cache files and a separate durable retirement acknowledgement. Each deletion
// edge represents one interrupted cleanup invocation; the next invocation must
// re-read demand. SQLite atomicity, the exclusive catalog writer and cache
// identity/type checks remain production-adapter obligations, not model proofs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    state: playback::State,
    expires: [u8; 2],
    now: u8,
    old: bool,
    directory: bool,
    files: u8,
    reserved: bool,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    AttachA,
    AttachB,
    EndA,
    EndB,
    Tick,
    Complete,
    InvalidOutput,
    LoseSource,
    RecoverWorker,
    Poll,
    Age,
    Retire,
    AcknowledgeRetirement,
}
struct Adapter {
    fault: bool,
}
fn active(s: &Audited) -> u32 {
    s.expires.iter().filter(|until| **until > s.now).count() as u32
}
fn state_name(state: playback::State) -> &'static str {
    match state {
        playback::State::Queued => "queued",
        playback::State::Running => "running",
        playback::State::Ready => "ready",
        playback::State::Failed => "failed",
        playback::State::Cancelled => "cancelled",
        playback::State::Stale => "stale",
    }
}
fn step_state(before: &Audited, event: &Event, fault: bool) -> Audited {
    let mut after = before.clone();
    let viewers = active(before);
    match event {
        Event::AttachA | Event::AttachB
            if before.directory
                && matches!(
                    before.state,
                    playback::State::Running | playback::State::Ready
                ) =>
        {
            after.expires[usize::from(matches!(event, Event::AttachB))] = before.now + 1;
        }
        Event::EndA | Event::EndB => after.expires[usize::from(matches!(event, Event::EndB))] = 0,
        Event::Tick if before.now < 3 => after.now += 1,
        Event::Age => after.old = true,
        Event::Complete
        | Event::InvalidOutput
        | Event::LoseSource
        | Event::RecoverWorker
        | Event::Poll => {
            let input = match event {
                Event::Complete => playback::Event::Complete { valid_output: true },
                Event::InvalidOutput => playback::Event::Complete {
                    valid_output: false,
                },
                Event::RecoverWorker => playback::Event::Recover,
                _ => playback::Event::Poll,
            };
            let result = playback::decide(
                before.state,
                playback::Evidence {
                    authorized_viewers: viewers,
                    source_current: !matches!(event, Event::LoseSource),
                    tools_current: true,
                    destination_current: true,
                    within_budget: true,
                },
                input,
            );
            let expected = if before.state != playback::State::Running {
                before.state
            } else if matches!(event, Event::RecoverWorker) {
                playback::State::Failed
            } else if viewers == 0 {
                playback::State::Cancelled
            } else if matches!(event, Event::LoseSource | Event::InvalidOutput) {
                playback::State::Failed
            } else if matches!(event, Event::Complete) {
                playback::State::Ready
            } else {
                playback::State::Running
            };
            after.violation |= result.state != expected;
            after.violation |= result.release_reservation != (expected != playback::State::Running);
            after.violation |= result.stop
                != (before.state == playback::State::Running
                    && expected != playback::State::Running);
            after.state = result.state;
            if result.release_reservation {
                after.reserved = false;
            }
        }
        Event::Retire | Event::AcknowledgeRetirement => {
            let allowed = playback_admission::cache_retirement_allowed(
                state_name(before.state),
                before.directory,
                viewers > 0 && !fault,
                if before.old { 0.0 } else { 4000.0 },
                4000.0,
            );
            let expected = before.state != playback::State::Running
                && (before.state != playback::State::Stale || before.directory)
                && viewers == 0
                && (before.state != playback::State::Ready || before.old);
            after.violation |= allowed != expected;
            if allowed {
                if matches!(event, Event::Retire) && before.directory {
                    // A crash can leave one file removed without a DB update.
                    // Repeating cleanup after reopen must remain idempotent.
                    after.files = before.files.saturating_sub(1);
                    after.directory = after.files != 0;
                } else if matches!(event, Event::AcknowledgeRetirement) && !before.directory {
                    after.state = playback::State::Stale;
                    after.reserved = false;
                }
            }
            // Independent progress obligations also reject an adapter that
            // accepts cleanup but silently omits deletion or acknowledgement.
            if expected && matches!(event, Event::Retire) && before.directory {
                after.violation |= after.files != before.files.saturating_sub(1)
                    || after.directory != (before.files > 1);
            }
            if expected && matches!(event, Event::AcknowledgeRetirement) && !before.directory {
                after.violation |= after.state != playback::State::Stale || after.reserved;
            }
        }
        _ => {}
    }
    after.violation |= after.reserved != (after.state == playback::State::Running);
    after.violation |= after.files < before.files && viewers > 0;
    after.violation |=
        after.state == playback::State::Stale && (after.directory || after.files != 0);
    after
}
impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-playback-retirement".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/playback_admission.rs").as_slice(),
                        include_bytes!("../../catabolic-core/src/playback.rs").as_slice(),
                        include_bytes!("playback_retirement.rs").as_slice(),
                        include_bytes!("../../../Cargo.lock").as_slice()
                    ]
                    .concat()
                ),
                self.fault
            ),
        }
    }
    fn initial_state(&self) -> Result<Audited, ModelError> {
        Ok(Audited {
            state: playback::State::Running,
            expires: [0; 2],
            now: 0,
            old: false,
            directory: true,
            files: 2,
            reserved: true,
            violation: false,
        })
    }
    fn step(
        &self,
        before: &Audited,
        event: &Event,
    ) -> Result<Transition<Audited, String>, ModelError> {
        Ok(Transition::accepted(
            step_state(before, event, self.fault),
            vec![format!("{event:?}")],
        ))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "playback.retirement",
                "demand, reservation or retirement progress obligation violated",
            )
        } else {
            Check::passed("playback.retirement")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::AttachA,
            Event::AttachB,
            Event::EndA,
            Event::EndB,
            Event::Tick,
            Event::Complete,
            Event::InvalidOutput,
            Event::LoseSource,
            Event::RecoverWorker,
            Event::Poll,
            Event::Age,
            Event::Retire,
            Event::AcknowledgeRetirement,
        ])
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
        if state.now > 3
            || state.files > 2
            || state.expires.iter().any(|expiry| *expiry > 4)
            || state.state == playback::State::Queued
        {
            return Err(ModelError::new(
                "playback retirement state exceeds documented bounds",
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
    let r = enumerate(&Adapter { fault: false }, SearchConfig::default())?;
    if r.failure.is_some()
        || r.termination != SearchTermination::GraphExhausted
        || r.skipped_checks != 0
    {
        return Err(format!(
            "playback retirement check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "playback retirement graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected playback retirement fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("playback retirement shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-playback-retirement".into(),
        seed: None,
        parameters: vec![
            ("sessions".into(), "2 with expiry".into()),
            ("clock".into(), "0..3".into()),
            ("cache-files".into(), "2".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("playback retirement trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "playback retirement injected fault found and shrunk: {} -> {} inputs",
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
        return Err("playback retirement fresh replay did not match".into());
    }
    println!(
        "playback retirement fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(events: &[Event], fault: bool) -> Audited {
        let mut state = Adapter { fault }.initial_state().unwrap();
        for event in events {
            state = step_state(&state, event, fault);
        }
        state
    }
    #[test]
    fn expiration_and_repeated_cleanup_release_exactly_one_reservation() {
        let state = run(
            &[
                Event::AttachA,
                Event::AttachB,
                Event::EndA,
                Event::Complete,
                Event::Tick,
                Event::Age,
                Event::Retire,
                Event::Retire,
                Event::AcknowledgeRetirement,
                Event::Retire,
                Event::AcknowledgeRetirement,
            ],
            false,
        );
        assert!(!state.violation);
        assert_eq!(state.state, playback::State::Stale);
        assert_eq!(state.files, 0);
        assert!(!state.reserved);
    }
    #[test]
    fn interruption_rechecks_new_demand_before_more_deletion() {
        let state = run(
            &[
                Event::AttachA,
                Event::Complete,
                Event::EndA,
                Event::Age,
                Event::Retire,
                Event::AttachB,
                Event::Retire,
            ],
            false,
        );
        assert!(!state.violation);
        assert_eq!(state.files, 1);
        assert!(
            run(
                &[Event::AttachA, Event::Complete, Event::Age, Event::Retire],
                true
            )
            .violation
        );
    }
}
