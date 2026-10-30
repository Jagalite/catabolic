// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded playback demand, worker completion and cache retirement.
use catabolic_core::{encode, playback_admission, sha256};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::execution::{ReplayOptions, ReplayOutcome, record, replay};
use stateless::explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink};
use stateless::trace::{ReadLimits, RunConfig, Termination, Trace};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use std::{error::Error, fs::File, path::Path};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    state: u8,
    sessions: [bool; 2],
    old: bool,
    directory: bool,
    reserved: bool,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    AttachA,
    AttachB,
    EndA,
    EndB,
    Complete,
    Age,
    Retire,
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
            name: "catabolic-playback-retirement".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/playback_admission.rs").as_slice(),
                        include_bytes!("playback_retirement.rs").as_slice(),
                        include_bytes!("../../../Cargo.lock").as_slice(),
                    ]
                    .concat()
                ),
                self.fault
            ),
        }
    }
    fn initial_state(&self) -> Result<Audited, ModelError> {
        Ok(Audited {
            state: 0,
            sessions: [false; 2],
            old: false,
            directory: true,
            reserved: true,
            violation: false,
        })
    }
    fn step(
        &self,
        before: &Audited,
        event: &Event,
    ) -> Result<Transition<Audited, String>, ModelError> {
        let mut after = before.clone();
        match event {
            Event::AttachA | Event::AttachB if before.state != 2 => {
                after.sessions[usize::from(matches!(event, Event::AttachB))] = true
            }
            Event::EndA | Event::EndB => {
                after.sessions[usize::from(matches!(event, Event::EndB))] = false
            }
            Event::Complete if before.state == 0 => {
                after.state = 1;
                after.reserved = false;
            }
            Event::Age => after.old = true,
            Event::Retire => {
                let active = before.sessions.iter().any(|v| *v);
                let phase = if before.state == 0 {
                    "running"
                } else if before.state == 1 {
                    "ready"
                } else {
                    "stale"
                };
                let allowed = playback_admission::cache_retirement_allowed(
                    phase,
                    before.directory,
                    active && !self.fault,
                    if before.old { 0.0 } else { 4000.0 },
                    4000.0,
                );
                let expected = before.state == 1 && before.old && !active;
                if allowed != expected {
                    after.violation = true;
                }
                if allowed {
                    after.state = 2;
                    after.directory = false;
                    after.reserved = false;
                }
            }
            _ => {}
        }
        if (after.state == 2 && after.sessions.iter().any(|v| *v))
            || (after.reserved != (after.state == 0))
        {
            after.violation = true;
        }
        Ok(Transition::accepted(after, vec![format!("{event:?}")]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "playback.retirement",
                "cache retired while demanded or worker active",
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
            Event::Complete,
            Event::Age,
            Event::Retire,
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
        parameters: vec![("live-session".into(), "boolean".into())],
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
