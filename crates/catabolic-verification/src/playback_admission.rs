// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded admission and capacity obligations for playback.
use catabolic_core::{encode, playback_admission, sha256};
use serde::{Deserialize, Serialize};
use stateless::automatic::Auto;
use stateless::execution::{ReplayOptions, ReplayOutcome, record, replay};
use stateless::explore::{SearchConfig, SearchTermination, ShrinkConfig, enumerate, shrink};
use stateless::trace::{ReadLimits, RunConfig, Termination, Trace};
use stateless::{Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition};
use std::{error::Error, fs::File, path::Path};
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Audited {
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Event {
    global: i64,
    principal: i64,
    reserved: i64,
    available: u128,
    output: u64,
    floor: u64,
    preset: u8,
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
            name: "catabolic-playback-admission".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/playback_admission.rs").as_slice(),
                        include_bytes!("playback_admission.rs").as_slice(),
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
        let session = playback_admission::session_allowed(e.global, e.principal);
        let preset = ["h264-720p", "h264-1080p", "other"][usize::from(e.preset)];
        let allowed_preset = playback_admission::preset_supported(preset);
        let mut capacity =
            playback_admission::reservation_available(e.reserved, e.available, e.output, e.floor);
        if self.fault
            && e.reserved >= 0
            && e.available < e.reserved as u128 + u128::from(e.output) + u128::from(e.floor)
        {
            capacity = true;
        }
        let expected_capacity = e.reserved >= 0
            && e.available
                .checked_sub(e.reserved.max(0) as u128)
                .and_then(|v| v.checked_sub(u128::from(e.output)))
                .is_some_and(|v| v >= u128::from(e.floor));
        let good = session == (e.global < 100 && e.principal < 10)
            && allowed_preset == (e.preset < 2)
            && capacity == expected_capacity;
        Ok(Transition::accepted(
            Audited {
                violation: before.violation || !good,
            },
            vec![format!("{session}/{allowed_preset}/{capacity}")],
        ))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "playback.admission",
                "playback session or capacity admission violated its limits",
            )
        } else {
            Check::passed("playback.admission")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        let mut v = Vec::new();
        for global in [0, 99, 100] {
            for principal in [0, 9, 10] {
                for reserved in [-1, 0, 1] {
                    for available in 0..5 {
                        for output in 0..3 {
                            for floor in 0..3 {
                                for preset in 0..3 {
                                    v.push(Event {
                                        global,
                                        principal,
                                        reserved,
                                        available,
                                        output,
                                        floor,
                                        preset,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(v)
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
            "playback admission check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "playback admission graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected playback admission fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("playback admission shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-playback-admission".into(),
        seed: None,
        parameters: vec![
            ("session-limits".into(), "100/10".into()),
            ("capacity-values".into(), "0..4".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("playback admission trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "playback admission injected fault found and shrunk: {} -> {} inputs",
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
        return Err("playback admission fresh replay did not match".into());
    }
    println!(
        "playback admission fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}
