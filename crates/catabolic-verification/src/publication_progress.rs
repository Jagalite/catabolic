// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded checks for generation-specific publication acknowledgement.
use catabolic_core::{
    encode,
    publication_progress::{self, Effect, Evidence},
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
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Event {
    observed: Option<i64>,
    current: i64,
    verified: i64,
    binding: bool,
    healthy: bool,
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
            name: "catabolic-publication-progress".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/publication_progress.rs")
                            .as_slice(),
                        include_bytes!("publication_progress.rs").as_slice(),
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
        let mut result = publication_progress::decide(Evidence {
            observed_generation: e.observed,
            current_generation: e.current,
            verified_generation: e.verified,
            binding_matches: e.binding,
            healthy: e.healthy,
        });
        if self.fault
            && e.healthy
            && e.binding
            && e.observed.is_some_and(|v| v < e.current)
            && e.current > e.verified
        {
            result = Effect::Verify {
                generation: e.current,
            };
        }
        let allowed =
            e.healthy && e.binding && e.observed == Some(e.current) && e.current > e.verified;
        let good = if allowed {
            result
                == Effect::Verify {
                    generation: e.current,
                }
        } else {
            result == Effect::RetainPending
        };
        Ok(Transition::accepted(
            Audited {
                violation: before.violation || !good,
            },
            vec![format!("{result:?}")],
        ))
    }
    fn check_state(&self, s: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.violation {
            Check::failed(
                "publication_progress.generation",
                "verification acknowledged a different or unhealthy generation",
            )
        } else {
            Check::passed("publication_progress.generation")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        let mut v = Vec::new();
        for observed in [None, Some(0), Some(1), Some(2)] {
            for current in 0..3 {
                for verified in 0..3 {
                    for binding in [false, true] {
                        for healthy in [false, true] {
                            v.push(Event {
                                observed,
                                current,
                                verified,
                                binding,
                                healthy,
                            });
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
            "publication progress check failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "publication progress graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected publication progress fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("publication progress shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-publication-progress".into(),
        seed: None,
        parameters: vec![("generations".into(), "0..2".into())],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("publication progress trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "publication progress injected fault found and shrunk: {} -> {} inputs",
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
        return Err("publication progress fresh replay did not match".into());
    }
    println!(
        "publication progress fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}
