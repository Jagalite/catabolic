// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded two-caller demand and physical reservation sequencing.
use catabolic_core::{
    encode,
    rendition::{self, Capacity, DemandPhase, Effect, Input},
    sha256,
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
    attached: [bool; 2],
    cancelled: [bool; 2],
    ready: [bool; 2],
    job: u8,
    reserved: bool,
    releases: u8,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    AdmitA,
    AdmitB,
    CancelA,
    CancelB,
    Start,
    Complete,
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
            name: "catabolic-shared-job".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/rendition.rs").as_slice(),
                        include_bytes!(
                            "../../catabolic-store/resources/migrations/022_http_access.sql"
                        )
                        .as_slice(),
                        include_bytes!("shared_job.rs").as_slice(),
                        include_bytes!("../../../Cargo.lock").as_slice(),
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
            Event::AdmitA | Event::AdmitB => {
                let n = usize::from(matches!(event, Event::AdmitB));
                if !before.attached[n] {
                    let capacity = Capacity {
                        principal_pending: 0,
                        global_pending: 0,
                        principal_limit: 10,
                        global_limit: 10,
                        available: 10,
                        reserved: 0,
                        output_budget: 1,
                        reserve_floor: 0,
                        job_reserved: before.reserved,
                    };
                    let phase = if before.job == 2 {
                        "complete"
                    } else if before.job == 1 {
                        "running"
                    } else {
                        "queued"
                    };
                    match rendition::reduce(&Input::Admit {
                        existing_body_matches: None,
                        job_phase: phase.into(),
                        capacity,
                    }) {
                        Ok(Effect::Attach { reserve, demand }) => {
                            after.attached[n] = true;
                            after.ready[n] = demand == DemandPhase::Ready;
                            if reserve {
                                after.reserved = true;
                            }
                        }
                        _ => after.violation = true,
                    }
                }
            }
            Event::CancelA | Event::CancelB => {
                let n = usize::from(matches!(event, Event::CancelB));
                if before.attached[n] && !before.ready[n] && !before.cancelled[n] {
                    if rendition::reduce(&Input::Cancel) == Ok(Effect::CancelDemand) {
                        after.cancelled[n] = true;
                    } else {
                        after.violation = true;
                    }
                }
            }
            Event::Start if before.job == 0 && before.attached.iter().any(|v| *v) => after.job = 1,
            Event::Complete if before.job != 2 && before.attached.iter().any(|v| *v) => {
                // Abstract the migration's transactional fanout and release.
                after.job = 2;
                for n in 0..2 {
                    if before.attached[n] && !before.cancelled[n] {
                        after.ready[n] = true;
                    }
                }
                if before.reserved && !self.fault {
                    after.reserved = false;
                    after.releases += 1;
                }
            }
            _ => {}
        }
        let any = after.attached.iter().any(|v| *v);
        if (any && after.job != 2 && !after.reserved)
            || (after.job == 2 && (after.reserved || after.releases != 1))
            || (0..2).any(|n| {
                after.ready[n] != (after.attached[n] && !after.cancelled[n] && after.job == 2)
            })
        {
            after.violation = true;
        }
        Ok(Transition::accepted(after, vec![format!("{event:?}")]))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "shared_job.accounting",
                "demand fanout or reservation obligation violated",
            )
        } else {
            Check::passed("shared_job.accounting")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        Ok(vec![
            Event::AdmitA,
            Event::AdmitB,
            Event::CancelA,
            Event::CancelB,
            Event::Start,
            Event::Complete,
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
        return Err(format!("shared job check failed/incomplete: {:?}", r.termination).into());
    }
    println!(
        "shared job graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected shared job fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("shared job shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-shared-job".into(),
        seed: None,
        parameters: vec![("reservation-release".into(), "boolean".into())],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("shared job trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "shared job injected fault found and shrunk: {} -> {} inputs",
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
        return Err("shared job fresh replay did not match".into());
    }
    println!("shared job fresh-process replay exact, failed property reproduced, matching build");
    Ok(())
}
