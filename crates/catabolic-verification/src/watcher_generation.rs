// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded watcher generation and event retention sequences.
use catabolic_core::{
    encode, sha256,
    watcher_schedule::{self, CompletionEffect, CompletionEvidence},
};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::execution::{ReplayOptions, ReplayOutcome, record, replay};
use stateless::explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink};
use stateless::trace::{ReadLimits, RunConfig, Termination, Trace};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use std::{error::Error, fs::File, path::Path};
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    pending: u8,
    requested: u8,
    completed: u8,
    active: Option<u8>,
    event: bool,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Request,
    Observe,
    Claim,
    Complete,
    WrongGeneration,
}
struct Adapter {
    fault: bool,
}
impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-watcher-generation".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/watcher_schedule.rs").as_slice(),
                        include_bytes!("watcher_generation.rs").as_slice(),
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
        let mut after = before.clone();
        match event {
            Event::Request if before.pending < 3 => {
                after.pending += 1;
                after.requested = after.pending;
            }
            Event::Observe if before.pending < 3 => {
                after.pending += 1;
                after.event = true;
            }
            Event::Claim if before.active.is_none() && before.pending > before.completed => {
                after.active = Some(before.pending);
            }
            Event::Complete | Event::WrongGeneration if before.active.is_some() => {
                let generation = before.active.unwrap();
                let claimed = if matches!(event, Event::WrongGeneration) {
                    generation + 1
                } else {
                    generation
                };
                let mut effect = watcher_schedule::complete(CompletionEvidence {
                    run_generation: generation.into(),
                    claimed_generation: claimed.into(),
                    pending_generation: before.pending.into(),
                });
                if self.fault
                    && claimed == generation
                    && before.event
                    && before.pending > generation
                {
                    effect = CompletionEffect::Complete {
                        generation: generation.into(),
                        clear_events: true,
                    };
                }
                match effect {
                    CompletionEffect::Reject => {
                        if claimed == generation {
                            after.violation = true;
                        }
                    }
                    CompletionEffect::Complete {
                        generation: accepted,
                        clear_events,
                    } => {
                        if claimed != generation
                            || accepted != i64::from(generation)
                            || clear_events != (before.pending <= generation)
                        {
                            after.violation = true;
                        }
                        after.completed = accepted as u8;
                        after.active = None;
                        if clear_events {
                            after.event = false;
                        }
                    }
                }
            }
            _ => {}
        }
        if after.completed > after.pending || after.requested > after.pending {
            after.violation = true;
        }
        Ok(Transition::accepted(after, vec![format!("{event:?}")]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "watcher.generation",
                "completion lost newer work or accepted a mismatched generation",
            )
        } else {
            Check::passed("watcher.generation")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::Request,
            Event::Observe,
            Event::Claim,
            Event::Complete,
            Event::WrongGeneration,
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
        serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
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
            "watcher generation check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "watcher generation graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected watcher generation fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("watcher generation shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-watcher-generation".into(),
        seed: None,
        parameters: vec![("maximum-generation".into(), "3".into())],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("watcher generation trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "watcher generation injected fault found and shrunk: {} -> {} inputs",
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
        return Err("watcher generation fresh replay did not match".into());
    }
    println!(
        "watcher generation fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}
