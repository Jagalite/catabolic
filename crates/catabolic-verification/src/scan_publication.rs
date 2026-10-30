// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Bounded ordered-evidence audit for the production scan publication fence.
use catabolic_core::{
    encode,
    scan_publication::{self, Evidence, Fence, ScopeEffect},
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
    stage: u8,
    violation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Evidence { stage: u8, valid: bool },
    Scope { complete: bool, verified: bool },
}
struct Adapter {
    fault: bool,
}
fn fence(stage: u8) -> Fence {
    [
        Fence::Catalog,
        Fence::Claim,
        Fence::Policy,
        Fence::Root,
        Fence::Ready,
    ][usize::from(stage)]
}
impl Model for Adapter {
    type State = Audited;
    type Input = Event;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catabolic-scan-publication".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}-fault={}",
                sha256(
                    [
                        include_bytes!("../../catabolic-core/src/scan_publication.rs").as_slice(),
                        include_bytes!("scan_publication.rs").as_slice(),
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
        let output = match *event {
            Event::Evidence { stage, valid } => {
                let evidence = match stage {
                    0 => Evidence::Catalog(valid),
                    1 => Evidence::Claim(valid),
                    2 => Evidence::Policy(valid),
                    _ => Evidence::Root(valid),
                };
                let result = scan_publication::advance(fence(before.stage), evidence);
                let expected = before.stage < 4 && before.stage == stage && valid;
                if result.is_ok() != expected {
                    after.violation = true;
                }
                if result.is_ok() {
                    after.stage += 1;
                } else {
                    // A rejected publication attempt exits; a retry starts with
                    // fresh catalog evidence rather than carrying a partial fence.
                    after.stage = 0;
                }
                format!("{result:?}")
            }
            Event::Scope { complete, verified } => {
                let mut result =
                    scan_publication::scope_effect(fence(before.stage), complete, verified);
                if self.fault && before.stage == 4 && complete && !verified {
                    result = Ok(ScopeEffect::PublishMissing);
                }
                let expected = if before.stage != 4 {
                    None
                } else if complete && verified {
                    Some(ScopeEffect::PublishMissing)
                } else {
                    Some(ScopeEffect::RecordOnly)
                };
                if result.ok() != expected {
                    after.violation = true;
                }
                format!("{result:?}")
            }
        };
        Ok(Transition::accepted(after, vec![output]))
    }
    fn check_state(&self, state: &Audited) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if state.violation {
            Check::failed(
                "scan_publication.evidence",
                "missing publication requires ordered fresh evidence and verified complete scope",
            )
        } else {
            Check::passed("scan_publication.evidence")
        }])
    }
}
impl Enumerate for Adapter {
    fn inputs(&self, _: &Audited) -> Result<Vec<Event>, ModelError> {
        let mut v = Vec::new();
        for stage in 0..4 {
            for valid in [false, true] {
                v.push(Event::Evidence { stage, valid });
            }
        }
        for complete in [false, true] {
            for verified in [false, true] {
                v.push(Event::Scope { complete, verified });
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
            "scan publication verification failed/incomplete: {:?}",
            r.termination
        )
        .into());
    }
    println!(
        "scan publication graph exhausted: {} states, {} transitions, zero skipped checks",
        r.states, r.transitions
    );
    Ok(())
}
pub fn find(directory: &str) -> Result<(), Box<dyn Error>> {
    let model = Auto::new(Adapter { fault: true });
    let f = enumerate(&model, SearchConfig::default())?
        .failure
        .ok_or("injected scan publication fault not found")?;
    let reduced = shrink(&model, &f, ShrinkConfig::default())?;
    if !reduced.validated_original {
        return Err("scan shrink did not validate original".into());
    }
    let config = RunConfig {
        strategy: "catabolic-scan-publication".into(),
        seed: None,
        parameters: vec![
            ("fence-stages".into(), "4".into()),
            ("scope-facts".into(), "2".into()),
        ],
    };
    std::fs::create_dir(directory)?;
    for (name, inputs) in [
        ("original", &f.inputs),
        ("minimized", &reduced.minimized.inputs),
    ] {
        let trace = record(&model, inputs.clone(), config.clone(), inputs.len())?;
        if trace.termination != Termination::PropertyFailed {
            return Err("scan trace failed to reproduce".into());
        }
        let mut file = File::create_new(Path::new(directory).join(format!("{name}.sttrace")))?;
        trace.write_to(&mut file)?;
        file.sync_all()?;
    }
    println!(
        "scan publication injected fault found and shrunk: {} -> {} inputs",
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
        return Err("scan fresh replay did not match".into());
    }
    println!(
        "scan publication fresh-process replay exact, failed property reproduced, matching build"
    );
    Ok(())
}
