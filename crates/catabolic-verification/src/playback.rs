// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Statelessness explores the same policy called by the real playback worker.
use catabolic_core::{
    encode,
    playback::{Decision, Event, Evidence, State, decide},
    sha256,
};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use stateless::{
    execution::{ReplayOptions, ReplayOutcome, record, replay},
    explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink},
    trace::{ReadLimits, RunConfig, Termination, Trace},
};
use std::{error::Error, fs::File, path::Path};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    state: State,
    violation: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    evidence: Evidence,
    event: Event,
}
struct Adapter {
    fault: bool,
}
impl Model for Adapter {
    type State = Audited;
    type Input = Input;
    type Output = Decision;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-playback-production".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/playback.rs").as_slice(),
                        include_bytes!("playback.rs").as_slice(),
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
            state: State::Queued,
            violation: None,
        })
    }
    fn step(
        &self,
        before: &Audited,
        input: &Input,
    ) -> Result<Transition<Audited, Decision>, ModelError> {
        let mut result = decide(before.state, input.evidence, input.event);
        if self.fault
            && before.state == State::Running
            && matches!(input.event, Event::Complete { .. })
        {
            result.state = State::Ready;
        }
        let active = matches!(before.state, State::Queued | State::Running);
        let valid = input.evidence.authorized_viewers > 0
            && input.evidence.source_current
            && input.evidence.tools_current
            && input.evidence.destination_current
            && input.evidence.within_budget;
        let mut violation = before.violation.clone();
        if active
            && result.state == State::Ready
            && !(before.state == State::Running
                && valid
                && input.event == Event::Complete { valid_output: true })
        {
            violation = Some("playback.invalid_completion".into());
        }
        if !active && result.state != before.state {
            violation = Some("playback.terminal_resurrection".into());
        }
        if before.state == State::Running
            && valid
            && input.event == Event::Poll
            && (result.stop || result.state != State::Running)
        {
            violation = Some("playback.sibling_cancellation".into());
        }
        if !matches!(result.state, State::Queued | State::Running) && !result.release_reservation {
            violation = Some("playback.reservation_leak".into());
        }
        Ok(Transition::accepted(
            Audited {
                state: result.state,
                violation,
            },
            vec![result],
        ))
    }
    fn check_state(&self, state: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok([
            "playback.invalid_completion",
            "playback.terminal_resurrection",
            "playback.sibling_cancellation",
            "playback.reservation_leak",
        ]
        .iter()
        .map(|id| {
            if state.violation.as_deref() == Some(id) {
                Check::failed(*id, "independent playback effect audit rejected transition")
            } else {
                Check::passed(*id)
            }
        })
        .collect())
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![];
        for viewers in 0..=2 {
            for mask in 0..16 {
                for event in [
                    Event::Start,
                    Event::Poll,
                    Event::Complete {
                        valid_output: false,
                    },
                    Event::Complete { valid_output: true },
                    Event::Recover,
                ] {
                    inputs.push(Input {
                        evidence: Evidence {
                            authorized_viewers: viewers,
                            source_current: mask & 1 != 0,
                            tools_current: mask & 2 != 0,
                            destination_current: mask & 4 != 0,
                            within_budget: mask & 8 != 0,
                        },
                        event,
                    });
                }
            }
        }
        Ok(inputs)
    }
}
fn codec(value: &impl Serialize) -> Result<Vec<u8>, ModelError> {
    Ok(
        encode(&serde_json::to_value(value).map_err(|e| ModelError::new(e.to_string()))?)
            .into_bytes(),
    )
}
impl ModelCodec for Adapter {
    fn encode_state(&self, value: &Audited) -> Result<Vec<u8>, ModelError> {
        codec(value)
    }
    fn decode_state(&self, value: &[u8]) -> Result<Audited, ModelError> {
        serde_json::from_slice(value).map_err(|e| ModelError::new(e.to_string()))
    }
    fn encode_input(&self, value: &Input) -> Result<Vec<u8>, ModelError> {
        codec(value)
    }
    fn decode_input(&self, value: &[u8]) -> Result<Input, ModelError> {
        serde_json::from_slice(value).map_err(|e| ModelError::new(e.to_string()))
    }
    fn encode_output(&self, value: &Decision) -> Result<Vec<u8>, ModelError> {
        codec(value)
    }
}
pub fn check() -> Result<(), Box<dyn Error>> {
    let report = enumerate(&Adapter { fault: false }, SearchConfig::default())?;
    if report.failure.is_some()
        || report.termination != SearchTermination::GraphExhausted
        || report.skipped_checks != 0
    {
        return Err(format!(
            "playback verification failed/incomplete: {:?}",
            report.termination
        )
        .into());
    }
    println!(
        "production playback graph exhausted: {} states, {} transitions, zero skipped checks",
        report.states, report.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let report = enumerate(&model, SearchConfig::default())?;
    let failure = report.failure.ok_or("injected playback fault not found")?;
    let result = shrink(&model, &failure, ShrinkConfig::default())?;
    if !result.validated_original {
        return Err("playback shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-playback-production".into(),
        seed: None,
        parameters: vec![
            ("viewers".into(), "0..2".into()),
            ("evidence-bits".into(), "4".into()),
            ("events".into(), "5".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &failure.inputs),
        ("minimized", &result.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("playback trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "production playback injected fault found and shrunk: {} -> {} inputs",
        failure.inputs.len(),
        result.minimized.inputs.len()
    );
    Ok(())
}
pub fn replay_trace(path: &str) -> Result<(), Box<dyn Error>> {
    let trace = Trace::read_from(File::open(path)?, &ReadLimits::default())?;
    let result = replay(
        &Adapter { fault: true },
        &trace,
        ReplayOptions {
            allow_build_mismatch: false,
        },
    )?;
    if result.outcome != ReplayOutcome::Exact || !result.failure_reproduced || !result.build_matches
    {
        return Err("production playback fresh replay did not match".into());
    }
    println!(
        "production playback fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}
